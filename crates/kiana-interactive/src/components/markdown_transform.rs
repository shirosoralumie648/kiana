//! components/markdown-transform.ts。
//!
//! 把一组扩展提供的 `MarkdownTransformer` 串成 `Markdown` 的 `transform` 回调。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/markdown-transform.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`create_markdown_transform`]：生成 `Markdown` 的 transform 回调。
//!
//! ## 为什么需要这一层
//!
//! 多个扩展（包括 JS 扩展桥）提供的 transformer 必须共享同一套渲染上下文，并按注册
//! 顺序串行执行。`Markdown::render` 每次按宽度重算都会重新调用返回的 transform 回调，
//! 因此上下文必须每次新建，不能把一个 transformer 的修改泄漏给后面的 transformer。
//!
//! ## 执行流程
//!
//! 1. [`create_markdown_transform`] 捕获消息类型、流式标志、transformer 列表以及可选的
//!    `message_id` / `timestamp`，返回 `TransformFn`；
//! 2. 渲染时构造新的 `MarkdownTransformContext`，带上本次的 `available_width`；
//! 3. `apply_markdown_transformers` 把每个 transformer 的输出作为下一个的输入，返回最终
//!    文本。
//!
//! ## 异常语义（对应 TS 的 try/catch）
//!
//! TS 在每个 transformer 外层 `try/catch`，抛异常时保留该步之前的文本并继续。Rust 的
//! transformer 是不可失败的闭包（扩展桥会把 JS 异常转成返回值），所以这里只处理 Rust
//! transformer 真 panic 的情况。两个 thread-local 深度计数器与宿主 panic hook 协作：
//!
//! * [`catch_unwind_silent`] / [`catch_unwind_guarded`] 在调用期间递增各自的计数；
//! * 本文件安装的静默 hook 只在静默计数大于 0 时跳过默认报告；
//! * 宿主 hook（`interactive_mode/lifecycle.rs`）用 `panics_are_guarded` 判断 panic 是否
//!   已被捕获，是则交回上一个 hook，不当作未捕获异常处理。
//!
//! 两个 hook 都查询同一组线程局部标记、未命中时转调 `previous`，因此安装先后不影响受保
//! 护区间的输出行为。注意 hook 是进程级且只安装一次，安装后不会卸载。

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, UnwindSafe, catch_unwind};
use std::sync::{Arc, Once};

use kiana_ext_api::types::{MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer};
use kiana_tui::components::markdown::TransformFn;

// 用深度计数而不是布尔值，嵌套保护区退出时不会把外层保护误清掉。两个计数器分开，
// 因为静默 hook 只关心静默区间，而宿主 hook 关心两者。
thread_local! {
    /// 当前线程处于 [`catch_unwind_silent`] 保护中的嵌套深度。
    static SILENCED_PANICS: Cell<usize> = const { Cell::new(0) };
    /// 当前线程处于任意 [`catch_unwind_guarded`] 保护中的嵌套深度。
    static GUARDED_PANICS: Cell<usize> = const { Cell::new(0) };
}

/// 当前线程的 panic 是否会被调用方 `catch_unwind` 捕获（不是 Node 的
/// `uncaughtException`）。宿主 panic hook 用它避免把 `try/catch` 语义的受保护调用当成崩溃。
///
/// 任一种受保护深度大于 0 就返回 `true`：静默与保留输出的两种保护区都算“已捕获”。
/// 只反映**当前线程**，因为计数器是 `thread_local`。
pub(crate) fn panics_are_guarded() -> bool {
    SILENCED_PANICS.with(|depth| depth.get() > 0) || GUARDED_PANICS.with(|depth| depth.get() > 0)
}

/// 标记"panic 会被捕获"的 `catch_unwind`：保留默认 panic 输出，只让宿主 panic hook 跳过
/// 未捕获异常处理。
///
/// 与 [`catch_unwind_silent`] 的区别：**不**安装静默 hook、**不**抑制 stderr 输出，适合
/// “panic 是预期内的能力探测”场景（例如在没有 `LocalSet` 时 `spawn_local` 会 panic，
/// 调用方只想据此降级，但仍希望看到默认报告）。返回值与 `std::panic::catch_unwind` 相同。
pub fn catch_unwind_guarded<F, R>(f: F) -> std::thread::Result<R>
where
    F: FnOnce() -> R + UnwindSafe,
{
    GUARDED_PANICS.with(|depth| depth.set(depth.get() + 1));
    let result = catch_unwind(f);
    GUARDED_PANICS.with(|depth| depth.set(depth.get() - 1));
    result
}

static INSTALL_SILENT_HOOK: Once = Once::new();

/// 安装一次全局 panic hook：仅当**当前线程**位于 [`catch_unwind_silent`] 内时吞掉
/// `thread ... panicked at ...` 的默认输出。
///
/// TS 用 `try/catch` 静默吞掉 transformer / 渲染器抛出的异常（不打印任何东西）。Rust 的
/// `catch_unwind` 在解开前仍会走默认 panic hook，把报告写到 stderr，污染 TUI；这里保留
/// 原来的 hook，只在受保护线程的受保护区间内跳过输出，其它线程/其它 panic 行为不变。
fn install_silent_hook() {
    INSTALL_SILENT_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let silenced = SILENCED_PANICS.with(|depth| depth.get() > 0);
            if !silenced {
                previous(info);
            }
        }));
    });
}

/// `try/catch` 的 Rust 等价物：捕获 panic，且不打印 panic hook 的默认报告。
///
/// 语义与 TS 的 `try { ... } catch { ... }` 一致：调用方的错误分支继续执行，stderr
/// 保持干净。`catch_unwind` 的 `Err` 载荷原样返回。
///
/// 首次调用会安装进程级静默 hook；它只在当前线程的静默区间内抑制输出，其它线程与区间
/// 之外的 panic 仍走原 hook。计数在 `catch_unwind` 返回后递减，嵌套调用按深度叠加。
pub fn catch_unwind_silent<F, R>(f: F) -> std::thread::Result<R>
where
    F: FnOnce() -> R + UnwindSafe,
{
    install_silent_hook();
    SILENCED_PANICS.with(|depth| depth.set(depth.get() + 1));
    let result = catch_unwind(f);
    SILENCED_PANICS.with(|depth| depth.set(depth.get() - 1));
    result
}

/// `createMarkdownTransform(messageType, isStreaming, transformers, messageId?, timestamp?)`。
///
/// 返回的函数在每次 `Markdown::render` 按宽度重算时被调用。`context` 每次**新建**，
/// 因此 transformer 修改上下文不会影响其它 transformer。
///
/// `transformers` 按给定顺序串联（前一个的输出是后一个的输入）；`message_id` /
/// `timestamp` 在每次渲染时克隆进上下文，实时消息通常为 `None`。返回的 [`TransformFn`]
/// 是 `Send + Sync` 的 `Arc` 闭包，可以被复制给多个 `Markdown` 实例共享。
pub fn create_markdown_transform(
    message_type: MarkdownMessageType,
    is_streaming: bool,
    transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
) -> TransformFn {
    Arc::new(move |markdown: &str, available_width: usize| {
        let context = MarkdownTransformContext {
            message_type,
            is_streaming,
            available_width,
            message_id: message_id.clone(),
            timestamp: timestamp.clone(),
        };
        apply_markdown_transformers(markdown, &context, &transformers)
    })
}

/// `applyMarkdownTransformers`：按注册顺序逐个应用。
///
/// TS 用 `try/catch` 吞掉 transformer 抛出的异常并继续；Rust 的 transformer 是不可失败的
/// 闭包，扩展桥把 JS 异常转成原样返回，因此这里的 [`catch_unwind_silent`] 只覆盖宿主
/// transformer 真崩了的情况（保持 TS 的「链继续」语义，测试用 `panic!` 复现抛异常）。
/// 注意必须用静默版本：TS 的 `catch` 不打印 panic 报告。
///
/// 单个 transformer panic 时只丢弃这一步，`transformed` 保持上一步的结果，循环继续。
/// `AssertUnwindSafe` 表示调用方接受这一约定：panic 后只保留局部字符串 `transformed`，
/// 不依赖 transformer 内部状态的一致性；持有内部可变状态的 transformer 需自行保证
/// 跨 panic 仍可用。
fn apply_markdown_transformers(
    markdown: &str,
    context: &MarkdownTransformContext,
    transformers: &[MarkdownTransformer],
) -> String {
    // 空列表直接返回原文；每一步失败（panic）等价于“该 transformer 不存在”。
    let mut transformed = markdown.to_string();
    for transformer in transformers {
        let result = catch_unwind_silent(AssertUnwindSafe(|| transformer(&transformed, context)));
        if let Ok(next) = result {
            transformed = next;
        }
    }
    transformed
}

// 三个测试分别锁定：注册顺序串联、单个 transformer panic 后链继续（对应 TS 的
// try/catch 语义）、上下文字段完整透传。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_transformers_in_registration_order() {
        let transformers: Vec<MarkdownTransformer> = vec![
            Arc::new(|markdown: &str, _| format!("a:{markdown}")),
            Arc::new(|markdown: &str, _| format!("b:{markdown}")),
        ];
        let transform = create_markdown_transform(
            MarkdownMessageType::Assistant,
            false,
            transformers,
            None,
            None,
        );
        assert_eq!(transform("x", 10), "b:a:x");
    }

    #[test]
    fn keeps_going_after_a_panicking_transformer() {
        let transformers: Vec<MarkdownTransformer> = vec![
            Arc::new(|markdown: &str, _| markdown.replace("still", "remains")),
            Arc::new(|_: &str, _| panic!("broken transformer")),
            Arc::new(|markdown: &str, _| format!("{markdown} after error")),
        ];
        let transform = create_markdown_transform(
            MarkdownMessageType::Assistant,
            false,
            transformers,
            None,
            None,
        );
        assert_eq!(
            transform("still visible", 10),
            "remains visible after error"
        );
    }

    #[test]
    fn passes_the_full_context() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let slot = Arc::clone(&seen);
        let transformers: Vec<MarkdownTransformer> = vec![Arc::new(move |markdown: &str, ctx| {
            *slot.lock().unwrap() = Some(ctx.clone());
            markdown.to_string()
        })];
        let transform = create_markdown_transform(
            MarkdownMessageType::User,
            false,
            transformers,
            Some("id-1".to_string()),
            Some("2025-01-01T00:00:00.000Z".to_string()),
        );
        let _ = transform("x", 7);
        assert_eq!(seen.lock().unwrap().clone(), Some(context_with(7)));
    }

    fn context_with(width: usize) -> MarkdownTransformContext {
        MarkdownTransformContext {
            message_type: MarkdownMessageType::User,
            is_streaming: false,
            available_width: width,
            message_id: Some("id-1".to_string()),
            timestamp: Some("2025-01-01T00:00:00.000Z".to_string()),
        }
    }
}
