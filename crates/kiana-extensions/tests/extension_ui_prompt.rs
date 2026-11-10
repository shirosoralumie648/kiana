//! `withUIPrompt` / `ui_prompt_start` / `ui_prompt_end`（`ca-extensions.md` §2.3.8，
//! `design-extension-runtime.md` §4.5）。
//!
//! 覆盖：单次 prompt 的延后事件、嵌套只发外层一对、空标题省略、取消（future 被丢弃）与同步
//! panic 仍然配对发出 `end`、以及默认调度器的“不在当前同步栈发出”语义。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{ExtensionEvent, UIPromptKind, UIPromptReason, UIPromptStartEventType};
use kiana_ext_api::ui::{CustomFactory, ExtensionUiContext};
use kiana_extensions::runner::noop_ui::no_op_ui_context;
use kiana_extensions::runner::ui_prompt::{
    ActivePrompt, DeferredScheduler, UiPromptContext, UiPromptHost, UiPromptScheduler,
    scheduler_from_fn, with_ui_prompt,
};

// ===========================================================================
// 测试替身
// ===========================================================================

/// 手动微任务队列：只有显式 `run_all()` 才会发出事件。
#[derive(Default)]
struct ManualScheduler {
    queue: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl UiPromptScheduler for ManualScheduler {
    fn schedule(&self, task: Box<dyn FnOnce() + Send + 'static>) {
        self.queue.lock().expect("manual scheduler").push(task);
    }
}

impl ManualScheduler {
    fn pending(&self) -> usize {
        self.queue.lock().expect("manual scheduler").len()
    }

    fn run_all(&self) {
        loop {
            let tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = {
                let mut queue = self.queue.lock().expect("manual scheduler");
                std::mem::take(&mut *queue)
            };
            if tasks.is_empty() {
                return;
            }
            for task in tasks {
                task();
            }
        }
    }
}

/// `run()` 同步 panic 的 prompt 构造器（永远不会返回 future）。
fn panicking_prompt<'a>() -> LocalBoxFuture<'a, ()> {
    panic!("synchronous boom")
}

/// 事件的可比较形态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Recorded {
    Start {
        kind: UIPromptKind,
        title: Option<String>,
    },
    End {
        kind: UIPromptKind,
        title: Option<String>,
    },
}

#[derive(Default)]
struct RecordingHost {
    events: Mutex<Vec<Recorded>>,
}

impl RecordingHost {
    fn events(&self) -> Vec<Recorded> {
        self.events.lock().expect("recording host").clone()
    }
}

impl UiPromptHost for RecordingHost {
    fn emit_ui_prompt(&self, event: ExtensionEvent) {
        let recorded = match event {
            ExtensionEvent::UIPromptStart(event) => {
                assert_eq!(event.event_type, UIPromptStartEventType::UIPromptStart);
                assert_eq!(event.reason, UIPromptReason::UiPrompt);
                Recorded::Start {
                    kind: event.kind,
                    title: event.title,
                }
            }
            ExtensionEvent::UIPromptEnd(event) => {
                assert_eq!(event.reason, UIPromptReason::UiPrompt);
                Recorded::End {
                    kind: event.kind,
                    title: event.title,
                }
            }
            _ => panic!("unexpected extension event"),
        };
        self.events.lock().expect("recording host").push(recorded);
    }
}

fn setup() -> (
    Arc<UiPromptContext>,
    Arc<RecordingHost>,
    Arc<ManualScheduler>,
) {
    let host = Arc::new(RecordingHost::default());
    let scheduler = Arc::new(ManualScheduler::default());
    let context =
        UiPromptContext::with_scheduler(no_op_ui_context(), host.clone(), scheduler.clone());
    (context, host, scheduler)
}

fn custom_factory() -> CustomFactory {
    Arc::new(|_tui, _theme, _keybindings, _done| Box::pin(std::future::pending()))
}

fn start(kind: UIPromptKind, title: Option<&str>) -> Recorded {
    Recorded::Start {
        kind,
        title: title.map(str::to_owned),
    }
}

fn end(kind: UIPromptKind, title: Option<&str>) -> Recorded {
    Recorded::End {
        kind,
        title: title.map(str::to_owned),
    }
}

// ===========================================================================
// 基本语义
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn emits_one_deferred_start_end_pair_for_a_single_prompt() {
    let (context, host, scheduler) = setup();

    let future = context.select("Pick a model", &["a".to_owned()], None);
    assert_eq!(context.depth(), 1);
    assert_eq!(
        context.active_prompt(),
        Some(ActivePrompt {
            kind: UIPromptKind::Select,
            title: Some("Pick a model".to_owned()),
        })
    );
    // 延后到微任务：调用点还没有任何事件。
    assert!(host.events().is_empty());
    assert_eq!(scheduler.pending(), 1);

    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![start(UIPromptKind::Select, Some("Pick a model"))]
    );

    assert_eq!(future.await, None);
    assert_eq!(context.depth(), 0);
    assert_eq!(context.active_prompt(), None);
    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Select, Some("Pick a model")),
            end(UIPromptKind::Select, Some("Pick a model")),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_aborted_signal_still_emits_the_start_end_pair() {
    let (context, host, scheduler) = setup();
    let signal = tokio_util::sync::CancellationToken::new();
    signal.cancel();
    let options = || kiana_ext_api::ui::ExtensionUIDialogOptions {
        signal: Some(signal.clone()),
        timeout: None,
    };

    // TS: `withUIPrompt` 先发 `ui_prompt_start`，mode 实现看到已取消的信号立即完成（不进入
    // 交互），随后 `finish` 发 `ui_prompt_end`。
    assert_eq!(
        context
            .select("Pick", &["a".to_owned()], Some(options()))
            .await,
        None
    );
    assert!(!context.confirm("Sure?", "yes", Some(options())).await);
    assert_eq!(context.input("Name", None, Some(options())).await, None);
    assert_eq!(context.depth(), 0);
    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Select, Some("Pick")),
            end(UIPromptKind::Select, Some("Pick")),
            start(UIPromptKind::Confirm, Some("Sure?")),
            end(UIPromptKind::Confirm, Some("Sure?")),
            start(UIPromptKind::Input, Some("Name")),
            end(UIPromptKind::Input, Some("Name")),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn emits_the_kind_for_all_five_blocking_prompts_and_omits_empty_titles() {
    let (context, host, scheduler) = setup();

    assert_eq!(context.select("", &[], None).await, None);
    assert!(!context.confirm("", "message", None).await);
    assert_eq!(context.input("", None, None).await, None);
    assert_eq!(context.editor("", None).await, None);
    let _ = context.custom(custom_factory(), None).await;
    scheduler.run_all();

    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Select, None),
            end(UIPromptKind::Select, None),
            start(UIPromptKind::Confirm, None),
            end(UIPromptKind::Confirm, None),
            start(UIPromptKind::Input, None),
            end(UIPromptKind::Input, None),
            start(UIPromptKind::Editor, None),
            end(UIPromptKind::Editor, None),
            start(UIPromptKind::Custom, None),
            end(UIPromptKind::Custom, None),
        ]
    );
    assert_eq!(scheduler.pending(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_non_empty_titles_on_the_events() {
    let (context, host, scheduler) = setup();

    assert!(!context.confirm("Delete file?", "Sure?", None).await);
    assert_eq!(context.editor("Edit notes", Some("prefill")).await, None);
    scheduler.run_all();

    let events = host.events();
    assert_eq!(
        events,
        vec![
            start(UIPromptKind::Confirm, Some("Delete file?")),
            end(UIPromptKind::Confirm, Some("Delete file?")),
            start(UIPromptKind::Editor, Some("Edit notes")),
            end(UIPromptKind::Editor, Some("Edit notes")),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn nesting_emits_only_the_outer_pair() {
    let (context, host, scheduler) = setup();

    // 外层 custom 对话框正在显示（深度 1）。
    let custom = context.custom(custom_factory(), None);
    assert_eq!(context.depth(), 1);

    // 组件内部再开一个 confirm：深度 2，不额外发事件。
    let nested = context.confirm("Nested", "message", None);
    assert_eq!(context.depth(), 2);
    assert!(!nested.await);
    assert_eq!(context.depth(), 1);
    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![start(UIPromptKind::Custom, None)],
        "the nested prompt must not emit its own pair"
    );

    let _ = custom.await;
    assert_eq!(context.depth(), 0);
    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Custom, None),
            end(UIPromptKind::Custom, None),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn emits_the_end_when_the_prompt_future_is_dropped() {
    let (context, host, scheduler) = setup();

    {
        let future = context.input("Cancelled", None, None);
        assert_eq!(context.depth(), 1);
        drop(future);
    }
    assert_eq!(context.depth(), 0);
    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Input, Some("Cancelled")),
            end(UIPromptKind::Input, Some("Cancelled")),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn still_emits_the_end_when_the_prompt_panics_synchronously() {
    let (context, host, scheduler) = setup();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // `run()` 同步 panic：`with_prompt` 在展开前归零深度并发出 `end`。
        drop(context.with_prompt::<()>(UIPromptKind::Confirm, Some("Boom"), panicking_prompt));
    }));
    assert!(result.is_err());
    assert_eq!(context.depth(), 0);
    assert_eq!(context.active_prompt(), None);

    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Confirm, Some("Boom")),
            end(UIPromptKind::Confirm, Some("Boom")),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn with_ui_prompt_emits_a_pair_around_the_future() {
    let (context, host, scheduler) = setup();

    let value = with_ui_prompt(&context, UIPromptKind::Input, Some("Name"), || {
        Box::pin(async { Some("value".to_owned()) })
    })
    .await;
    assert_eq!(value, Some("value".to_owned()));
    assert_eq!(context.depth(), 0);

    scheduler.run_all();
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Input, Some("Name")),
            end(UIPromptKind::Input, Some("Name")),
        ]
    );
}

// ===========================================================================
// 调度器
// ===========================================================================

#[test]
fn scheduler_from_fn_delegates_to_the_closure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_scheduler = Arc::clone(&calls);
    let scheduler = scheduler_from_fn(move |task| {
        task();
        calls_in_scheduler.fetch_add(1, Ordering::SeqCst);
    });
    scheduler.schedule(Box::new(|| {}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn deferred_scheduler_never_runs_on_the_calling_stack_without_a_tokio_runtime() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_task = Arc::clone(&ran);
    DeferredScheduler::default()
        .schedule(Box::new(move || ran_in_task.store(true, Ordering::SeqCst)));
    // 设计文档 §2.5：事件绝不在调用方的同步栈上发出；没有 tokio 运行时也不能退化为内联。
    assert!(!ran.load(Ordering::SeqCst));
    for _ in 0..200 {
        if ran.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn deferred_scheduler_keeps_working_after_a_task_panics() {
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let scheduler = DeferredScheduler::default();

    let first = Arc::clone(&events);
    scheduler.schedule(Box::new(move || {
        first.lock().expect("events").push("first");
    }));
    scheduler.schedule(Box::new(|| panic!("task boom")));
    let second = Arc::clone(&events);
    scheduler.schedule(Box::new(move || {
        second.lock().expect("events").push("second");
    }));

    for _ in 0..200 {
        if events.lock().expect("events").len() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(*events.lock().expect("events"), vec!["first", "second"]);

    // panic 之后新事件仍然会被调度（`flush_pending` 已复位）。
    let after = Arc::clone(&events);
    scheduler.schedule(Box::new(move || {
        after.lock().expect("events").push("after");
    }));
    for _ in 0..200 {
        if events.lock().expect("events").len() == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        *events.lock().expect("events"),
        vec!["first", "second", "after"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_default_scheduler_survives_a_panicking_task() {
    let scheduler = DeferredScheduler::default();
    scheduler.schedule(Box::new(|| panic!("scheduler boom")));
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_task = Arc::clone(&ran);
    scheduler.schedule(Box::new(move || ran_in_task.store(true, Ordering::SeqCst)));
    for _ in 0..200 {
        if ran.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread")]
async fn the_default_scheduler_never_emits_on_the_current_synchronous_stack() {
    let host = Arc::new(RecordingHost::default());
    let context = UiPromptContext::new(no_op_ui_context(), host.clone());

    let future = context.select("Deferred", &[], None);
    // 当前同步栈上没有任何事件。
    assert!(host.events().is_empty());
    assert_eq!(future.await, None);
    assert_eq!(context.depth(), 0);

    for _ in 0..50 {
        if host.events().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(
        host.events(),
        vec![
            start(UIPromptKind::Select, Some("Deferred")),
            end(UIPromptKind::Select, Some("Deferred")),
        ]
    );
}
