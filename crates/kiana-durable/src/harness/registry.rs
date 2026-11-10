//! `harness/registry.ts`：应用拥有的扩展注册表与内建任务表。
//!
//! TS: `packages/durable/src/harness/registry.ts`。发布即冻结：每次 `install`/`uninstall` 构造
//! 一份新的不可变 [`RegistryState`]，替换当前快照后同步通知监听器（与 TS 的 `#publish` 一致：
//! 构造下一个状态时抛错则什么都不发布）。
//!
//! # 为什么需要快照
//! 调度器持有 `Arc<dyn RegistryReader>`，在解析 agent、检查 blocked 等路径上反复调用
//! `snapshot()`（`harness/scheduler.rs`）；`Harness::open` 也用同一份快照检查三个内建任务
//! 是否齐全。发布时整体替换 `Arc<RegistryState>`，已经发出去的快照内容不变，解析过程因此
//! 不会看到“装了一半”的扩展集合（`tests/harness_registry.rs` 断言旧快照仍指向旧扩展）。
//!
//! # 一次 install 的顺序
//! 1. [`validate_extension`] 校验**新扩展自身**：工具重名、section key 语法/保留/重复；
//! 2. 在旧列表上按名字原位替换，或把新扩展追加到末尾；
//! 3. [`RegistryState::new`] 重建任务表：内建任务在前，扩展任务按安装顺序；任务名与内建或
//!    其它扩展冲突时返回错误（因此替换同名扩展、接替它原来的任务名是合法的）；
//! 4. `publish` 在锁内换掉当前快照并复制监听器列表，释放锁后同步调用监听器。
//!
//! 校验失败时什么都不发布，旧快照与监听器都不动。`Registry::install`/`uninstall` 的冻结
//! trait 签名没有错误通道，失败以 `panic_any(BoxError)` 抛出（测试用 `catch_unwind` 取回）；
//! 监听器在锁外运行，因此监听器里可以再次访问 registry。同一 `Arc` 监听器重复注册是空操作。

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::IndexMap;

use crate::harness::agent::INSTRUCTIONS_KEY;
use crate::harness::compaction::COMPACTION_TASK;
use crate::harness::generation::GENERATION_TASK;
use crate::harness::tool::TOOL_TASK;
use crate::harness::types::{
    AnyTask, Extension, PromptSection, Registry, RegistryReader, RegistrySection, RegistrySnapshot,
    RegistryTool,
};
use crate::types::{BoxError, Unsubscribe};

/// 与 TS 的 `new Error(message)` 对应的普通错误。
fn js_error(message: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(message.into()))
}

/// section key 的合法形状（TS: `const SECTION_KEY = /^[a-z][a-z0-9_-]*$/`）。
///
/// 错误消息里 `SECTION_KEY` 用正则的 `toString()` 形式，即 [`SECTION_KEY_TEXT`]。
///
/// 这只是同一形状的常量表达（本文件不用它做校验）；实际校验由 [`is_valid_section_key`]
/// 逐字节完成，两者应对应：首字符只能是小写 ASCII 字母，其余只能是小写字母、数字、`_`、`-`。
pub const SECTION_KEY_PATTERN: &str = "^[a-z][a-z0-9_-]*$";

/// 正则的字面文本形式；校验扩展时的错误消息用它。
///
/// 错误消息形如 `Section key {JSON 引号包裹的 key} must match {SECTION_KEY_TEXT}`，改动这个
/// 常量会改变可观察的错误文本。
pub const SECTION_KEY_TEXT: &str = "/^[a-z][a-z0-9_-]*$/";

/// 每个 registry 都持有的内建任务定义；它们不是扩展，不能删除或替换。
///
/// 顺序即 `kiana.generation`、`kiana.tool`、`kiana.compaction`（决定 `Harness::open` 报错里的名字顺序）。
///
/// 这些 `AnyTask` 在 `LazyLock` 初始化时 clone 一次并长期复用；`RegistryState` 每次发布只是
/// 再 clone 进任务表，所以同一份内建定义的 `definition` `Arc` 指针在所有快照里相同
/// （调度器的 `same_task` 依赖指针身份）。任务表按插入顺序暴露：先内建，再按扩展安装顺序。
pub static BUILTIN_TASKS: LazyLock<Vec<AnyTask>> = LazyLock::new(|| {
    vec![
        GENERATION_TASK.clone(),
        TOOL_TASK.clone(),
        COMPACTION_TASK.clone(),
    ]
});

/// `SECTION_KEY.test(key)`：`^[a-z][a-z0-9_-]*$`（ASCII 语义）。
///
/// 空串、非 ASCII 字母，以及首字符为数字/`_`/`-` 的 key 都返回 false。逐字节扫描只接受
/// ASCII，因此非 ASCII 字符（包括全角字母）一律不合法。
pub fn is_valid_section_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
    })
}

/// 不可变的已发布 registry 状态。
///
/// 三个字段在构造后不再变化，快照的 `Arc` 因此可以安全地跨 `await` 持有：
/// - `extensions` 保持安装顺序；`install` 保证同名扩展只有一个并原位替换；
/// - `by_name` 与 `extensions` 指向同一批 `Arc`（输入名字唯一时），只用于按名查找；
/// - `tasks` 的插入顺序是“三个内建任务 + 扩展任务（按扩展安装顺序、扩展内声明顺序）”，
///   `tasks()` 按这个顺序返回。
///
/// `new` 是唯一的构造口：它会拒绝与已存在任务重名的扩展任务，但**不**检查扩展之间的
/// 工具/section 重名；单个扩展内部的重名/非法 key 由 [`validate_extension`] 检查，跨扩展
/// 同名工具/分段由 `resolve_agent` 合并时后者覆盖前者（`harness/agent.rs`）。
pub struct RegistryState {
    extensions: Vec<Arc<dyn Extension>>,
    by_name: IndexMap<String, Arc<dyn Extension>>,
    tasks: IndexMap<String, AnyTask>,
}

impl RegistryState {
    /// 由扩展列表构造；任务重名时返回错误（与 TS 的构造器一样）。
    ///
    /// 同名扩展在 `extensions` 里出现多次时（只有直接调用本函数才可能，`RegistryImpl`
    /// 不会这样传），`by_name` 保留最后一个，而 `extensions` 全部保留；因此正常使用应传入
    /// 名字唯一的列表。
    pub fn new(extensions: Vec<Arc<dyn Extension>>) -> Result<Self, BoxError> {
        let mut by_name: IndexMap<String, Arc<dyn Extension>> = IndexMap::new();
        for extension in &extensions {
            by_name.insert(extension.name().to_owned(), Arc::clone(extension));
        }
        let mut tasks: IndexMap<String, AnyTask> = IndexMap::new();
        for task in BUILTIN_TASKS.iter() {
            tasks.insert(task.definition.name.clone(), task.clone());
        }
        for extension in &extensions {
            for task in extension.tasks() {
                let name = task.definition.name.clone();
                if tasks.contains_key(&name) {
                    return Err(js_error(format!(
                        "Task {name} of extension {} is already installed",
                        extension.name()
                    )));
                }
                tasks.insert(name, task);
            }
        }
        Ok(RegistryState {
            extensions,
            by_name,
            tasks,
        })
    }
}

impl RegistrySnapshot for RegistryState {
    fn installed(&self) -> Vec<Arc<dyn Extension>> {
        self.extensions.clone()
    }

    fn extension(&self, name: &str) -> Option<Arc<dyn Extension>> {
        self.by_name.get(name).cloned()
    }

    fn tools(&self) -> Vec<RegistryTool> {
        let mut tools = Vec::new();
        for extension in &self.extensions {
            for tool in extension.tools() {
                tools.push(RegistryTool {
                    extension: Arc::clone(extension),
                    tool,
                });
            }
        }
        tools
    }

    fn sections(&self) -> Vec<RegistrySection> {
        let mut sections = Vec::new();
        for extension in &self.extensions {
            for section in extension.sections() {
                sections.push(RegistrySection {
                    extension: Arc::clone(extension),
                    section,
                });
            }
        }
        sections
    }

    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.values().cloned().collect()
    }

    fn task(&self, name: &str) -> Option<AnyTask> {
        self.tasks.get(name).cloned()
    }
}

/// 应用拥有的扩展注册表（TS: `RegistryImpl`）。
///
/// 内部只有一个 `Mutex<RegistryInner>`：`snapshot()` 取锁克隆当前 `Arc` 后立刻释放；
/// 发布在锁内只做“换 `current` + 复制监听器列表”。监听器在锁外调用，因此监听器里可以安全
/// 地再次 `install`/`snapshot`（不会自锁）；并发发布时，各次发布仍会按自己复制到的列表
/// 逐个调用监听器，而监听器看到的是调用那一刻的最新快照。
pub struct RegistryImpl {
    inner: Arc<Mutex<RegistryInner>>,
}

/// 锁保护的可变状态：当前快照、监听器表与监听器 id 分配器。
///
/// `next_listener_id` 单调递增、不复用；`listeners` 按注册顺序保存，发布时按该顺序调用。
struct RegistryInner {
    current: Arc<RegistryState>,
    listeners: Vec<RegistryListener>,
    next_listener_id: u64,
}

/// 一个注册的监听器；`id` 让注销按注册身份移除（TS 的 `Set.delete(listener)`）。
///
/// `subscribe` 用 `Arc::ptr_eq` 判重：同一个 `Arc` 重复注册不会插入第二项，返回的注销闭包
/// 持有的仍是第一次分配的 `id`。
struct RegistryListener {
    id: u64,
    listener: Arc<dyn Fn() + Send + Sync>,
}

impl RegistryImpl {
    /// 新建，只含内建任务。
    pub fn new() -> Self {
        RegistryImpl {
            inner: Arc::new(Mutex::new(RegistryInner {
                current: Arc::new(
                    RegistryState::new(Vec::new()).expect("the built-in tasks are unique"),
                ),
                listeners: Vec::new(),
                next_listener_id: 0,
            })),
        }
    }

    /// 取锁；中毒时继续使用内部状态。
    ///
    /// 临界区内只有 `Arc` 替换、`Vec` 追加/按 id 删除与计数器自增，没有需要跨字段修复的
    /// 不变量；监听器在锁外调用，因此监听器 panic 也不会污染这把锁。
    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 构造并校验下一个状态（任务重名时抛错），然后同步发布。
    ///
    /// 顺序很重要：`RegistryState::new` 在取锁之前完成，所以校验失败时 `current` 与监听器都
    /// 不会变动（测试 `validates_the_registry_as_it_would_be_after_an_install` 断言旧快照
    /// 不变且监听计数为 0）。锁内只做两件事：换掉 `current`、克隆监听器列表；随后在锁外
    /// 逐个调用，避免监听器回调再进 registry 时自锁。
    fn publish(&self, extensions: Vec<Arc<dyn Extension>>) -> Result<(), BoxError> {
        let state = RegistryState::new(extensions)?;
        let listeners = {
            let mut inner = self.lock();
            inner.current = Arc::new(state);
            inner
                .listeners
                .iter()
                .map(|entry| Arc::clone(&entry.listener))
                .collect::<Vec<_>>()
        };
        // TS 无 try/catch：监听器抛错会向上传播给调用者。
        for listener in listeners {
            listener();
        }
        Ok(())
    }
}

impl Default for RegistryImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryReader for RegistryImpl {
    /// 当前快照；克隆 `Arc` 后立即释放锁，调用方可以长期持有且内容不变。
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::clone(&self.lock().current) as Arc<dyn RegistrySnapshot>
    }

    /// 注册发布监听器，返回按 id 移除的注销闭包。
    ///
    /// 同一 `Arc` 重复注册是空操作（返回第一次的 `id`）。注销闭包只持有 `Weak`，registry
    /// 已释放时什么也不做。
    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = {
            let mut state = self.lock();
            // TS 用 `Set`：同一监听器重复注册是空操作（引用相等按 `Arc::ptr_eq` 判定）。
            if let Some(existing) = state
                .listeners
                .iter()
                .find(|entry| Arc::ptr_eq(&entry.listener, &listener))
            {
                existing.id
            } else {
                state.next_listener_id += 1;
                let id = state.next_listener_id;
                state.listeners.push(RegistryListener {
                    id,
                    listener: Arc::clone(&listener),
                });
                id
            }
        };
        let weak = Arc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                let mut state = inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(position) = state.listeners.iter().position(|entry| entry.id == id) {
                    state.listeners.remove(position);
                }
            }
        })
    }
}

impl Registry for RegistryImpl {
    /// 安装扩展，或按名字原位替换同名扩展。
    ///
    /// 先 [`validate_extension`] 检查这一个扩展内部的重名/key，再由 `publish` 重建任务表检查
    /// 任务名冲突。同名替换会丢弃旧扩展的全部工具/分段/任务，因此接替它原来的任务名是合法的
    /// （测试 `validates_the_registry_as_it_would_be_after_an_install` 的最后一例）。
    ///
    /// # Panics
    /// 校验或发布失败时以 `panic_any(BoxError)` 抛出；trait 方法没有 `Result` 通道。
    fn install(&self, extension: Arc<dyn Extension>) {
        let result = (|| {
            validate_extension(extension.as_ref())?;
            let current = self.lock().current.installed();
            let index = current
                .iter()
                .position(|installed| installed.name() == extension.name());
            let next = match index {
                None => {
                    let mut next = current;
                    next.push(Arc::clone(&extension));
                    next
                }
                Some(index) => {
                    let mut next = current;
                    next[index] = Arc::clone(&extension);
                    next
                }
            };
            self.publish(next)
        })();
        if let Err(error) = result {
            panic_js(error);
        }
    }

    /// 按名字卸载扩展，其余扩展保持原有顺序。
    ///
    /// 名字不存在时直接返回：不发布、不通知监听器。删除后 `RegistryState::new` 仍会重建任务
    /// 表；发布失败同样以 panic 形式抛出（见 `panic_js`）。
    fn uninstall(&self, name: &str) {
        let current = self.lock().current.installed();
        if !current.iter().any(|installed| installed.name() == name) {
            return;
        }
        let next: Vec<Arc<dyn Extension>> = current
            .into_iter()
            .filter(|installed| installed.name() != name)
            .collect();
        if let Err(error) = self.publish(next) {
            panic_js(error);
        }
    }
}

/// TS 的 `Registry.install`/`uninstall` 返回 `void`，错误以异常形式冒出。Rust 的冻结 trait
/// 签名没有错误通道，因此这里以同样的错误对象 panic（调用方可用 `catch_unwind` 取回）。
fn panic_js(error: BoxError) -> ! {
    std::panic::panic_any(error)
}

/// 一个扩展内工具名唯一、section key 合法且非保留，且同一扩展的分段 key 不重复。
///
/// TS: `validateExtension`
///
/// 检查范围是**单个扩展**：
/// - 工具名在本扩展内不能重复（不同扩展之间的同名工具是允许的：合并顺序中靠后的扩展
///   覆盖靠前的，默认是安装顺序，宿主 settings/agent 选择可重排）；
/// - section key 必须匹配 [`is_valid_section_key`]，不能是保留的 `instructions`
///   （`INSTRUCTIONS_KEY`），同一扩展内也不能重复；
/// - 任务名冲突**不在**这里检查：`RegistryState::new` 才把它与内建任务及其他扩展的任务合并
///   比较。
///
/// 错误文本会被 `install` 原样放进 panic payload；测试按子串匹配（`"two tools named"`、
/// `"must match"`、`"reserved"`、`"already installed"`）。
pub fn validate_extension(extension: &dyn Extension) -> Result<(), BoxError> {
    let mut tools: IndexMap<String, ()> = IndexMap::new();
    for tool in extension.tools() {
        let name = tool.name().to_owned();
        if tools.contains_key(&name) {
            return Err(js_error(format!(
                "Extension {} has two tools named {name}",
                extension.name()
            )));
        }
        tools.insert(name, ());
    }
    let mut sections: IndexMap<String, ()> = IndexMap::new();
    for section in extension.sections() {
        let key = section.key().to_owned();
        let quoted = serde_json::to_string(&serde_json::Value::String(key.clone()))
            .unwrap_or_else(|_| format!("\"{key}\""));
        if !is_valid_section_key(&key) {
            return Err(Box::new(crate::session::SessionTypeError::new(format!(
                "Section key {quoted} must match {SECTION_KEY_TEXT}"
            ))) as BoxError);
        }
        if key == INSTRUCTIONS_KEY {
            return Err(js_error(format!(
                "Section key {key} is reserved for the agent's instructions"
            )));
        }
        if sections.contains_key(&key) {
            return Err(js_error(format!(
                "Extension {} has two sections with key {key}",
                extension.name()
            )));
        }
        sections.insert(key, ());
    }
    Ok(())
}

/// 创建一个应用拥有的、只含内建任务的 registry。
///
/// 返回 trait object（`Arc<dyn Registry>`）；`HarnessOptions.registry` 只要求
/// `Arc<dyn RegistryReader>`，宿主可以用这个或自己的实现。
///
/// TS: `createRegistry()`
pub fn create_registry() -> Arc<dyn Registry> {
    Arc::new(RegistryImpl::new())
}

/// 与 `createRegistry<AppTool>()` 对应的具体句柄（测试需要读回快照/安装）。
///
/// 返回值保留 `RegistryImpl` 的具体类型，所以可以同时调用 `install`/`uninstall` 与
/// `snapshot`；示例和测试用它驱动 registry 热重载。
pub fn create_registry_impl() -> Arc<RegistryImpl> {
    Arc::new(RegistryImpl::new())
}

// 让 `PromptSection` 的导入在文档构建中可见（`Extension::sections()` 的返回元素）。
const _: fn(&dyn Extension) -> Vec<Arc<dyn PromptSection>> = |extension| extension.sections();
