//! 移植自 `packages/durable/test/examples/20-late-join.ts`：迟到的加入者。客户端在一次运行
//! 已经进行时挂上，先拿到当前状态（会话视图或 snapshot 事件），然后只拿此后的变化。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, FauxTokenSize, RegisterFauxProviderOptions,
    faux_assistant_message, faux_provider_with, faux_text, faux_tool_call,
};
use kiana_ai::types::{AssistantContentBlock, JsonObject, StopReason};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::Op;
use kiana_durable::harness::define::{define_extension, define_tool};
use kiana_durable::harness::events::{AgentEvent, MessageChange, ToolOutputUpdate};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::live::{LiveState, SlotStatus};
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, ConversationView, Extension, HarnessOptions, ModelRef, ModelsProvider, Registry,
    RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind, ToolExecuteFn,
    ToolExecutionResult, ToolOutput, ToolRegistration, UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{BoxError, JsonValue, Session, WatchListener};
use std::time::Duration;

/// TS 的 `pause(ms)`。
async fn pause(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// 每 100 ms 打印一行的工具。
fn count_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(|_arguments: JsonValue, api, _context| {
        Box::pin(async move {
            for n in 1..=10 {
                api.output(ToolOutput::Text(format!("{n}\n")), None);
                pause(100).await;
            }
            Ok(ToolExecutionResult::default())
        })
    });
    let mut tool = ToolRegistration::new(kiana_ai::types::Tool {
        name: "count".to_owned(),
        description: "Counts to ten".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    tool.execute = Some(execute);
    define_tool(tool)
}

struct Count {
    tools: Vec<ToolRegistration>,
}

impl Extension for Count {
    fn name(&self) -> &str {
        "count"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }
}

/// 结构视图的监听器：运行中的槽的输出每帧都打印。
struct ViewPrinter;

#[async_trait]
impl WatchListener<ConversationView> for ViewPrinter {
    async fn call(&self, value: ConversationView, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        let live = value
            .docs
            .get("kiana.live")
            .and_then(LiveState::from_document)
            .unwrap_or_default();
        let slot = live.tools.as_ref().and_then(|tools| tools.first());
        if slot.is_some_and(|slot| slot.status == SlotStatus::Running) {
            let output = slot
                .and_then(|slot| slot.output.clone())
                .unwrap_or_default();
            println!("view output now: {output:?}");
        }
    }
}

/// 按 UTF-16 单元切片（TS 的 `String.prototype.slice`）。
fn utf16_slice(text: &str, start: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().skip(start).collect();
    String::from_utf16_lossy(&units)
}

fn event_type(event: &AgentEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 事件监听器：应用工具输出增量与消息文本增量。
struct EventPrinter {
    output: Arc<Mutex<String>>,
}

#[async_trait]
impl WatchListener<Vec<AgentEvent>> for EventPrinter {
    async fn call(&self, value: Vec<AgentEvent>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        for event in &value {
            match event {
                AgentEvent::ToolExecutionUpdate {
                    output: Some(update),
                    ..
                } => {
                    let mut output = self.output.lock().unwrap();
                    match update {
                        ToolOutputUpdate::Set { set } => *output = set.clone(),
                        ToolOutputUpdate::Window { trim_start, append } => {
                            let start = trim_start.unwrap_or(0);
                            let trimmed = utf16_slice(&output, start);
                            *output = format!("{trimmed}{}", append.clone().unwrap_or_default());
                        }
                    }
                    println!("event output now: {:?}", *output);
                }
                AgentEvent::MessageUpdate { changes, .. } => {
                    let deltas: String = changes
                        .iter()
                        .filter_map(|change| match change {
                            MessageChange::TextDelta { delta, .. } => Some(delta.clone()),
                            _ => None,
                        })
                        .collect();
                    println!("event text delta: {deltas:?}");
                }
                other => println!("event: {}", event_type(other)),
            }
        }
    }
}

fn faux_model() -> ModelRef {
    ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    let registry = create_registry_impl();
    registry.install(define_extension(Count {
        tools: vec![count_tool()],
    }));
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        tokens_per_second: Some(40.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            AssistantContentBlock::ToolCall(faux_tool_call("count", JsonObject::new())),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )),
        FauxResponseStep::Message(faux_assistant_message(
            vec![AssistantContentBlock::Text(faux_text(
                "Counted to ten, and this answer streams slowly.",
            ))],
            Default::default(),
        )),
    ]);
    let models: Arc<dyn Models> = {
        let models = create_models(Default::default());
        models.set_provider(Arc::clone(&faux.provider));
        models
    };
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        context,
    )
    .await?;
    let root = harness
        .root(
            context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(faux_model())),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await?;

    let submission = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("Count to ten, then tell me.".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    // 工具进行到一半时加入。
    pause(500).await;

    // 结构客户端：视图持有已提交的转录与文档，包括运行中工具的输出。
    let view = root.watch(context).await?;
    {
        let value = view.value();
        let live = value
            .docs
            .get("kiana.live")
            .and_then(LiveState::from_document)
            .unwrap_or_default();
        println!(
            "view entries: {:?}",
            value
                .entries
                .iter()
                .map(|entry| entry.kind.clone())
                .collect::<Vec<_>>()
        );
        let slot = live.tools.as_ref().and_then(|tools| tools.first());
        println!(
            "view tool slot: {} {}",
            slot.map(|slot| slot.status.as_str()).unwrap_or("none"),
            serde_json::to_string(&slot.and_then(|slot| slot.output.clone()))?
        );
    }
    view.start(Arc::new(ViewPrinter));

    // 事件客户端：snapshot 事件带着同样的状态；之后的事件在它之上应用。
    let stream = harness.watch_events(root.id(), context).await?;
    println!(
        "snapshot tools: {:?}",
        stream
            .snapshot
            .tools
            .iter()
            .map(|slot| format!("{} {}", slot.name, slot.status.as_str()))
            .collect::<Vec<_>>()
    );
    let output = Arc::new(Mutex::new(
        stream
            .snapshot
            .tools
            .first()
            .and_then(|slot| slot.output.clone())
            .unwrap_or_default(),
    ));
    stream.start(Arc::new(EventPrinter {
        output: Arc::clone(&output),
    }));

    submission.wait(context).await?;
    harness.wait_for_idle(context).await?;
    pause(0).await;
    stream.stop().await;
    view.stop().await;
    harness.close(context).await;
    Ok(())
}
