//! `utils/sleep.ts`——可中止的 sleep。
//!
//! 先 `signal.throwIfAborted()`（同步抛出），再 `setTimeout` + `once` abort 监听：
//! abort 时 `clearTimeout` 并 **reject `signal.reason` 原值**（不是包装成 `AbortError`）。
//! 正常到期时移除监听并 resolve。
//!
//! TS: `utils/sleep.ts#sleep`
//!
//! # 被谁调用
//! - `auth/oauth/kimi_coding.rs` 的 `refresh_token`：刷新失败后的重试退避（1s/2s/4s）里调用。
//! - `auth/oauth/github_copilot.rs` 的 `fetch_with_rate_limit_retry`：在 429 限流重试的
//!   退避中调用（只覆盖 `/models` 与 `/models/{id}/policy`）；OAuth 刷新路径
//!   `refresh_github_copilot_token` 给它传 `max_retries: 0`，不经过这里。
//! `utils/retry.rs` 的退避**不**复用这里的 `sleep`：那边需要忽略取消原因、返回一个
//! 轻量的内部错误（见 `sleep_with_abort`），与 TS 的 `RetrySleepAbortError` 对应。
//!
//! # 执行流程
//! 1. 先同步检查 `scope.is_aborted()`；已中止则立即返回 `Err`，不创建定时器。
//! 2. 否则用 `tokio::time::sleep` 与 `token.cancelled()` 做 `select!`：谁先就绪谁决定结果。
//! 3. 中止时的错误由 `scope.reason()` 还原，`name`/`message` 原样保留（例如
//!    `TimeoutError` / `timed out`）；没有 reason 时用回退文案 `The operation was aborted`。
//!
//! # 运行前提
//! 必须运行在启用了时间驱动的 tokio 运行时内。`tokio::time::sleep` 在没有 runtime 或
//! 没有 timer 的上下文里会 panic（tokio 的既定行为），本函数不做检查也不返回错误。

use crate::error::AiError;
use crate::utils::abort::AbortScope;

/// 等待 `ms` 毫秒；`scope` 中止时以 `signal.reason`（缺省 `AbortError`）失败。
///
/// # 参数与返回
/// - `ms`：等待时长（毫秒）；`0` 表示一次零时长等待。
/// - `scope`：取消来源。调用前已取消、或等待期间被取消，都会返回 `Err`。
/// - `Ok(())`：计时正常到期。
/// - `Err(AiError)`：`kind` 是 `AiErrorKind::Aborted`；`name` / `message` 取自 `AbortScope`
///   记录的 reason（所以 `TimeoutError` 之类的名字会保留下来）。
///
/// # 与 TS 的对应
/// TS 在 abort 时 `reject(signal.reason)`（原值，不包一层 `AbortError`）。这里把 reason 的
/// `name`/`message` 搬进 `AiError`，因此 `Display`（等于 `message`）与 TS 的 `error.message`
/// 一致；`abort_error` 就是这段转换。
///
/// # 取消的终态性
/// 取消一旦发生就不会恢复：同一 `scope` 上的后续调用会在开头的检查里立即失败。
///
/// TS: `utils/sleep.ts#sleep`
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub async fn sleep(ms: u64, scope: &AbortScope) -> Result<(), AiError> {
    if scope.is_aborted() {
        return Err(abort_error(scope));
    }
    let token = scope.token();
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => Ok(()),
        _ = token.cancelled() => Err(abort_error(scope)),
    }
}

/// 把 `AbortScope` 记录的 reason 还原成 [`AiError`]。
///
/// 有两个不同来源的缺省文案，别混淆：
/// - `AbortScope::abort()` 会先写入 reason，message 是 `"This operation was aborted"`；
/// - 没有 reason 的 scope（例如从裸 `CancellationToken` 转换来的）走这里的回退分支，
///   message 是 TS `abortReason()` 使用的 `"The operation was aborted"`。
///
/// 有 reason 时 `name`/`message` 原样取自它（`name` 可以是 `"TimeoutError"` 等）；
/// 走回退分支时 `name` 是 `"AbortError"`。两种情况下 `kind` 都是 `AiErrorKind::Aborted`。
fn abort_error(scope: &AbortScope) -> AiError {
    scope
        .reason()
        .map(|reason| reason.to_error())
        .unwrap_or_else(|| AiError::aborted("The operation was aborted"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortReason;

    #[tokio::test]
    async fn resolves_after_the_delay() {
        let scope = AbortScope::new();
        assert!(sleep(1, &scope).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_with_the_signal_reason() {
        let scope = AbortScope::new();
        scope.abort_with(AbortReason {
            name: "TimeoutError".into(),
            message: "timed out".into(),
        });
        let error = sleep(10_000, &scope).await.unwrap_err();
        assert_eq!(error.name, "TimeoutError");
        assert_eq!(error.message, "timed out");
    }

    #[tokio::test]
    async fn rejects_when_aborted_while_sleeping() {
        let scope = AbortScope::new();
        let token = scope.token();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            token.cancel();
        });
        let error = sleep(10_000, &scope).await.unwrap_err();
        assert!(error.is_aborted());
        assert_eq!(error.message, "The operation was aborted");
    }
}
