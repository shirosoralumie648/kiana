//! `test/chat-support.ts` 的 Rust 等价物：faux provider + 可变的 Harness 设置。
//!
//! TS 的 `ChatSetup` 在 close/reopen 之间存活，像一个宿主进程自己的对象；这里同样把
//! provider、models、registry、reports、settings 与时钟放在可共享的夹具里。

#![allow(dead_code)]

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kiana_ai::models::{Models, MutableModels, create_models};
use kiana_ai::providers::faux::{FauxProviderHandle, FauxResponseStep, faux_provider_with};
use kiana_chord::context::Context;
use kiana_durable::harness::harness::{Harness, HarnessImpl};
use kiana_durable::harness::registry::{RegistryImpl, create_registry_impl};
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationCreateOptions, EnvBuilder,
    HarnessOptions, HarnessSettings, ModelRef, ModelsProvider, RegistryReader, RootOptions,
    SettingsProvider, ToolRegistration,
};
use kiana_durable::types::{BoxError, EntryRecord, ReportInfo, Storage};

/// 测试上下文（TS: session-support 的 `context`）。
pub fn context() -> kiana_chord::context::ContextRef {
    Arc::clone(&kiana_chord::context::BACKGROUND_CONTEXT)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在 close/reopen 之间存活的 models、registry、报告与设置。
pub struct ChatSetup {
    pub faux: FauxProviderHandle,
    pub models: Arc<dyn MutableModels>,
    pub registry: Arc<RegistryImpl>,
    pub reports: Arc<Mutex<Vec<(String, ReportInfo)>>>,
    /// 活的 Harness 设置（测试可在决定之间改字段）。
    pub settings: Arc<Mutex<HarnessSettings>>,
    now: Arc<Mutex<Arc<dyn Fn() -> i64 + Send + Sync>>>,
    pub identity: Option<kiana_ai::types::ClientIdentity>,
}

impl ChatSetup {
    pub fn new() -> Self {
        Self::with_faux_options(Default::default())
    }

    /// TS `chatSetup(options)`：用给定的 faux 选项（tokensPerSecond / tokenSize）建夹具。
    pub fn with_faux_options(
        options: kiana_ai::providers::faux::RegisterFauxProviderOptions,
    ) -> Self {
        let faux = faux_provider_with(options);
        let models = create_models(Default::default());
        models.set_provider(Arc::clone(&faux.provider));
        ChatSetup {
            faux,
            models,
            registry: create_registry_impl(),
            reports: Arc::new(Mutex::new(Vec::new())),
            settings: Arc::new(Mutex::new(HarnessSettings::default())),
            now: Arc::new(Mutex::new(Arc::new(default_now))),
            identity: None,
        }
    }

    /// 当前时钟（epoch 毫秒）。
    pub fn now(&self) -> i64 {
        (lock(&self.now))()
    }

    /// 设置时钟。
    pub fn set_now(&self, now: Arc<dyn Fn() -> i64 + Send + Sync>) {
        *lock(&self.now) = now;
    }

    /// 报告的错误消息。
    pub fn report_messages(&self) -> Vec<String> {
        lock(&self.reports)
            .iter()
            .map(|(message, _)| message.clone())
            .collect()
    }

    /// 报告的 `ReportInfo`。
    pub fn report_infos(&self) -> Vec<ReportInfo> {
        lock(&self.reports)
            .iter()
            .map(|(_, info)| info.clone())
            .collect()
    }
}

impl Default for ChatSetup {
    fn default() -> Self {
        Self::new()
    }
}

fn default_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `openChat()` 的选项。
#[derive(Clone, Default)]
pub struct OpenChatOptions {
    pub env: Option<EnvBuilder>,
}

/// 打开 Harness 并返回它的根（首次创建时配置 faux 模型）。
pub async fn open_chat(
    storage: Arc<dyn Storage>,
    setup: &ChatSetup,
) -> (Arc<HarnessImpl>, Arc<dyn ConversationTrait>) {
    open_chat_with(storage, setup, OpenChatOptions::default()).await
}

pub async fn open_chat_with(
    storage: Arc<dyn Storage>,
    setup: &ChatSetup,
    options: OpenChatOptions,
) -> (Arc<HarnessImpl>, Arc<dyn ConversationTrait>) {
    let models: Arc<dyn Models> = setup.models.clone();
    let settings = Arc::clone(&setup.settings);
    let reports = Arc::clone(&setup.reports);
    let now_clock = Arc::clone(&setup.now);
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || (lock(&now_clock))());
    let registry: Arc<dyn RegistryReader> = Arc::clone(&setup.registry) as Arc<dyn RegistryReader>;
    let harness = Harness::open(
        storage,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: setup.identity.clone(),
            registry,
            settings: Some(SettingsProvider::Dynamic(Arc::new(move || {
                Some(lock(&settings).clone())
            }))),
            env: options.env,
            conversation_created: None,
            now: Some(now),
            set_timer: None,
            on_wake: None,
            on_report: Some(Arc::new(
                move |error: &(dyn std::error::Error + Send + Sync), info: &ReportInfo| {
                    lock(&reports).push((error.to_string(), info.clone()));
                },
            )),
            read_only: None,
        },
        &*context(),
    )
    .await
    .expect("the harness opens");
    let root = harness
        .root(
            &*context(),
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await
        .expect("the root");
    (harness, root)
}

/// 原始条目，最旧在前。
pub async fn all_entries(
    conversation: &Arc<dyn ConversationTrait>,
    context: &dyn Context,
) -> Vec<EntryRecord> {
    let page = conversation
        .entries(Default::default(), 1000, None, context)
        .await
        .expect("entries");
    let mut items = page.items;
    items.reverse();
    items
}

/// 消息的第一段文本。
pub fn text_of(message: Option<&kiana_ai::types::Message>) -> Option<String> {
    let message = message?;
    match message {
        kiana_ai::types::Message::System(_) => None,
        kiana_ai::types::Message::User(user) => Some(match &user.content {
            kiana_ai::types::UserContent::Text(text) => text.clone(),
            kiana_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect(),
        }),
        kiana_ai::types::Message::Assistant(assistant) => {
            assistant.content.iter().find_map(|block| match block {
                kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
        }
        kiana_ai::types::Message::ToolResult(result) => {
            result.content.iter().find_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
        }
    }
}

/// 真实时间里轮询 `check` 直到成立（跨节流窗口与计时器）。
pub async fn wait_for<F, Fut>(mut check: F, timeout: Duration) -> Result<(), BoxError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if check().await {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(Box::new(std::io::Error::other("Condition was not reached")));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 只在 `timeout` 内等 `check` 成立。
pub async fn eventually<F, Fut>(mut check: F) -> Result<(), BoxError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return Ok(());
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(0)).await;
    }
    Err(Box::new(std::io::Error::other("Condition was not reached")))
}

/// 等待 watch 投递队列排空（watch 回调在提交之后由投递任务异步执行）。
///
/// 旧实现 sleep 固定 3ms，慢机器/CI 负载下会偶发漏批次；现在等待 `NativeWatch` 的
/// 待投递队列为空且投递任务退出（`AgentEventStream::wait_until_idle`），与调度延迟无关。
pub async fn drained(stream: &kiana_durable::harness::events::AgentEventStream) {
    // 先让出一轮，确保调用点在 drained 之前刚触发的提交有机会把帧排入 watch。
    tokio::task::yield_now().await;
    stream.wait_until_idle().await;
}

/// 等待任意 `WatchHandle` 的异步投递队列排空（对话/文档 watch）。
pub async fn drained_watch<T: Send + Sync + 'static>(
    watch: &dyn kiana_durable::types::WatchHandle<T>,
) {
    tokio::task::yield_now().await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !watch.is_idle() {
        if std::time::Instant::now() > deadline {
            panic!("watch did not drain within 10s");
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// 已安装的这些名字的工具，作为 `configure()` 的输入。
pub fn tools_named(setup: &ChatSetup, names: &[&str]) -> Vec<ToolRegistration> {
    let installed = setup.registry.snapshot().tools();
    names
        .iter()
        .map(|name| {
            installed
                .iter()
                .find(|entry| entry.tool.name() == *name)
                .unwrap_or_else(|| panic!("Tool {name} is not installed"))
                .tool
                .clone()
        })
        .collect()
}

/// 常用创建选项。
pub fn ownerless(agent: Option<AgentChange>) -> ConversationCreateOptions {
    ConversationCreateOptions {
        ownership: kiana_durable::types::ConversationOwnership::Ownerless,
        agent,
        init: None,
    }
}

/// 一个永不回答的响应（TS 的 `unanswered()`）：工厂被调用时通知 `reached`，然后等请求取消。
pub struct Unanswered {
    reached: Arc<tokio::sync::Notify>,
    pub step: FauxResponseStep,
}

impl Unanswered {
    /// 等到模型请求真的到达工厂。
    pub async fn reached(&self) {
        self.reached.notified().await;
    }
}

/// TS: `unanswered()`。
pub fn unanswered() -> Unanswered {
    let reached = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&reached);
    let step =
        FauxResponseStep::factory(move |input: kiana_ai::providers::faux::FauxResponseInput| {
            let reached = Arc::clone(&signal);
            async move {
                reached.notify_one();
                match input.options.and_then(|options| options.signal) {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
                Err(kiana_ai::utils::diagnostics::ThrownValue::from_text(
                    "cancelled",
                ))
            }
        });
    Unanswered { reached, step }
}

/// 测试用的 `FauxResponseStep`：立即回答一段文本。
pub fn answer(text: &str) -> FauxResponseStep {
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        kiana_ai::providers::faux::faux_text(text.to_owned()),
        Default::default(),
    ))
}
