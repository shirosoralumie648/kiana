//! `utils/deprecation.ts` 的移植测试（TS 无对应测试；按 §4.8 第 9 条覆盖公开 API）。
//!
//! TS 用 `console.warn` 输出；Rust 按 §4.4 走 `tracing::warn!`。这里安装一个捕获
//! `tracing` 事件的最小 subscriber，验证“同一条消息只输出一次”与 `clear` 之后重新输出。
//! 进程级集合是全局状态，测试用一把锁串行。

use std::sync::{Arc, Mutex};

use kiana_core::changelog::{clear_deprecation_warnings_for_tests, warn_deprecation};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Default)]
struct CaptureSubscriber {
    messages: Arc<Mutex<Vec<String>>>,
}

struct MessageVisitor<'a>(&'a mut Vec<String>);

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.push(format!("{value:?}"));
        }
    }
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut messages = self.messages.lock().expect("capture messages");
        let mut visitor = MessageVisitor(&mut messages);
        event.record(&mut visitor);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

fn capture(action: impl FnOnce()) -> Vec<String> {
    let subscriber = CaptureSubscriber::default();
    let messages = Arc::clone(&subscriber.messages);
    tracing::subscriber::with_default(subscriber, action);
    messages.lock().expect("captured").clone()
}

#[test]
fn warns_only_once_per_message() {
    let _guard = lock();
    clear_deprecation_warnings_for_tests();

    let first = capture(|| {
        warn_deprecation("legacy setting");
        warn_deprecation("legacy setting");
    });
    assert_eq!(
        first,
        vec!["Deprecation warning: legacy setting".to_string()]
    );

    let second = capture(|| warn_deprecation("legacy setting"));
    assert!(second.is_empty(), "duplicate warning must be suppressed");
}

#[test]
fn clear_allows_the_same_message_again() {
    let _guard = lock();
    clear_deprecation_warnings_for_tests();

    let count = |action: &dyn Fn()| capture(action).len();
    assert_eq!(
        count(&|| warn_deprecation("same")),
        1,
        "first warning is emitted"
    );
    clear_deprecation_warnings_for_tests();
    assert_eq!(count(&|| warn_deprecation("same")), 1);
}

#[test]
fn distinct_messages_are_independent() {
    let _guard = lock();
    clear_deprecation_warnings_for_tests();

    let messages = capture(|| {
        warn_deprecation("one");
        warn_deprecation("two");
        warn_deprecation("one");
    });
    assert_eq!(
        messages,
        vec![
            "Deprecation warning: one".to_string(),
            "Deprecation warning: two".to_string(),
        ]
    );
    clear_deprecation_warnings_for_tests();
}
