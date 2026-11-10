//! auth/oauth/device-code.ts——RFC 8628 设备码轮询。
//!
//! 常量与边界逐字（spec/ai-auth.md §2.7）：
//! - 无 `expiresInSeconds` → deadline 为 `+∞`；
//! - `intervalSeconds` 缺失 → 5s；`Math.floor` 后至少 1s；
//! - `slow_down`：服务端给了有限且 > 0 的 `intervalSeconds` 就用它（至少 1s），否则 +5s；
//! - 睡眠用 `min(intervalMs, remainingMs)`，睡眠前重算 remaining，≤0 直接 break；
//! - 超时文案取决于是否出现过 `slow_down`。
//!
//! `abortableSleep` 的中止错误是**普通 `Error`**（`name === "Error"`），不是 `AbortError`。
//!
//! # 为什么需要这个模块
//! 六个设备码流程（GitHub Copilot、Kimi Coding、xAI、Meta、OpenAI Codex、Radius）的轮询
//! 时间线完全相同：固定间隔、`slow_down` 退避、deadline、可取消睡眠。把这段逻辑集中到本
//! 模块后，各流程只需提供一个“执行一次 HTTP 轮询”的 `poll` 闭包，RFC 8628 的边界因此只
//! 实现一遍、只维护一份测试。
//!
//! # 一次轮询的执行流程
//! 1. 由 `expires_in_seconds` 算 deadline（缺省 `+∞`），由 `interval_seconds` 算初始间隔
//!    （缺省 5 秒，最小 1 秒）。
//! 2. `wait_before_first_poll` 为真且尚未到期时，先睡一个不超过剩余时间的间隔。
//! 3. 循环（每轮开头检查 deadline 与取消）：调用一次 `poll`；`Complete`/`Failed` 立即结束，
//!    `SlowDown` 调整间隔，`Pending` 保持间隔；随后重算剩余时间并睡
//!    `min(interval, remaining)`。
//! 4. 循环退出时抛超时错误，文案取决于是否出现过 `slow_down`。
//!
//! # 注意事项
//! - `poll` 的签名不接收 signal：本模块只在“睡眠”和“每轮开始”观察取消。如果单次轮询
//!   内部有长请求，闭包必须自己监听同一个 `signal`。
//! - 时钟是可注入的 [`AuthClock`]；睡眠仍用 tokio 计时器，测试用 `start_paused` 同时推进
//!   两者（`tests/oauth_device_code.rs`）。
//! - JS 的 `Math.max`/`Math.min` 在任一操作数为 NaN 时返回 NaN，Rust 的 `f64::max/min`
//!   不是这个语义；下面的 `js_max`/`js_min` 专门保留 JS 行为。
//!
//! TS: `src/auth/oauth/device-code.ts`

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::types::AuthError;
use crate::utils::abort::AbortScope;

/// `CANCEL_MESSAGE`。
const CANCEL_MESSAGE: &str = "Login cancelled";
/// `TIMEOUT_MESSAGE`。
const TIMEOUT_MESSAGE: &str = "Device flow timed out";
/// `SLOW_DOWN_TIMEOUT_MESSAGE`。
const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
/// `MINIMUM_INTERVAL_MS`。
const MINIMUM_INTERVAL_MS: f64 = 1000.0;
/// RFC 8628 section 3.2：服务端省略 `interval` 时客户端必须用 5 秒。
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// RFC 8628 section 3.5：`slow_down` 表示轮询间隔必须增加 5 秒。
const SLOW_DOWN_INTERVAL_INCREMENT_MS: f64 = 5000.0;

/// 一次轮询的结果。
///
/// `Pending`/`SlowDown` 只影响后续轮询；`Failed`/`Complete` 是终态。`SlowDown` 携带的
/// `intervalSeconds` 若为 `Some`，表示服务端在响应里给了新的最小间隔。
///
/// TS: `OAuthDeviceCodeIncompletePollResult | { status: "complete"; value: T }`
#[derive(Debug, Clone, PartialEq)]
pub enum OAuthDeviceCodePollResult<T> {
    /// `{ status: "pending" }`：本次没有结果，按当前间隔继续轮询。
    Pending,
    /// `{ status: "slow_down"; intervalSeconds? }`。
    ///
    /// 服务端要求降速；`interval_seconds` 是它给出的新最小间隔（秒），缺省时按 RFC 8628
    /// 增加 5 秒。
    SlowDown {
        /// 服务端给出的新最小间隔（秒）。
        interval_seconds: Option<f64>,
    },
    /// `{ status: "failed"; message }`：终止轮询，`message` 原样成为错误消息。
    Failed {
        /// 失败消息（原样抛出）。
        message: String,
    },
    /// `{ status: "complete"; value: T }`：终止轮询并返回该值。
    Complete {
        /// 完成值。
        value: T,
    },
}

/// 轮询回调的返回 future。
///
/// `Send + 'static` 让流程可以在多线程运行时里跨 `await` 持有它（例如放进 `tokio::spawn`
/// 的任务）；闭包每次被调用都应创建新的 future，且同一时刻只会有一个在运行。
pub type OAuthDeviceCodePollFuture<T> =
    Pin<Box<dyn Future<Output = Result<OAuthDeviceCodePollResult<T>, AuthError>> + Send + 'static>>;

/// 轮询选项。
///
/// TS: `src/auth/oauth/device-code.ts#OAuthDeviceCodePollOptions`
pub struct OAuthDeviceCodePollOptions<T> {
    /// 轮询间隔（秒）；缺省 5。非正数经 `floor` 后仍落到 1 秒下限（NaN 的走法见模块文档）。
    pub interval_seconds: Option<f64>,
    /// 有效期（秒）；缺省无限。deadline 从流程开始的墙钟起算；`NaN` 会让循环一次都不执行
    /// 并直接以超时错误结束（与 TS 的 `Date.now() < NaN` 一致）。
    pub expires_in_seconds: Option<f64>,
    /// 是否在首次轮询前先等待一个间隔（GitHub Copilot 一类流程使用）。
    pub wait_before_first_poll: bool,
    /// 单次轮询；顺序调用，不会并发。返回 `Err` 会直接终止整个流程。
    pub poll: Arc<dyn Fn() -> OAuthDeviceCodePollFuture<T> + Send + Sync>,
    /// 取消信号；只影响睡眠与每轮开始前的检查（见模块文档的注意事项）。
    pub signal: AbortScope,
}

/// 可注入的墙钟（`Date.now()`）；测试用假时钟推进时间线。
///
/// TS 直接用 `Date.now()`；spec/ai-auth.md §6.3 要求 Rust 侧可注入。其他 OAuth 流程也复用
/// 这个 trait（例如 Anthropic 用它计算 token 的绝对过期时间），因此实现只应返回墙钟毫秒，
/// 不要承担单调计时职责：系统时间被调整时结果会跟着跳。
pub trait AuthClock: Send + Sync {
    /// 当前墙钟毫秒（`Date.now()`）。
    fn now_ms(&self) -> f64;
}

/// 默认墙钟。
///
/// 系统时间早于 `UNIX_EPOCH` 时返回 `0.0`（内部 `Result` 的错误分支），不会 panic。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemAuthClock;

impl AuthClock for SystemAuthClock {
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as f64)
            .unwrap_or(0.0)
    }
}

/// `abortableSleep(ms, signal, cancelMessage)`。
///
/// 中止时抛 `new Error(cancelMessage)`（普通 Error，name 为 `"Error"`）。
///
/// `ms` 是“已经取整的毫秒数”，这里再按 Node `setTimeout` 的规则钳制：`0` 或大于
/// `2^31 - 1` 都变成 1ms（见私有函数 `set_timeout_duration`）。进入时 signal 已中止则
/// 不等待，立即返回错误；等待期间只观察取消 token，不读取 `signal.reason()`，所以错误
/// 固定是 `cancel_message`。
///
/// TS: `src/auth/oauth/device-code.ts#abortableSleep`
pub async fn abortable_sleep(
    ms: u64,
    signal: &AbortScope,
    cancel_message: &str,
) -> Result<(), AuthError> {
    if signal.is_aborted() {
        return Err(AuthError::new(cancel_message));
    }
    let token = signal.token();
    tokio::select! {
        _ = tokio::time::sleep(set_timeout_duration(ms)) => Ok(()),
        _ = token.cancelled() => Err(AuthError::new(cancel_message)),
    }
}

/// `Math.max`（任一 NaN → NaN，与 Rust 的 `f64::max` 不同）。
fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.max(right)
    }
}

/// `setTimeout(ms)` 的时长转换：`!(after >= 1 && after <= TIMEOUT_MAX)` → 1ms。
///
/// Node 的 `TIMEOUT_MAX` 是 `2^31 - 1`（spec: lib/internal/timers.js），**不是** `u32::MAX`；
/// 超过上限（含 `Infinity`）时 Node 取 1ms。
const TIMEOUT_MAX_MS: f64 = 2_147_483_647.0;

fn timeout_ms(ms: f64) -> u64 {
    if ms.is_nan() || !(1.0..=TIMEOUT_MAX_MS).contains(&ms) {
        1
    } else {
        ms as u64
    }
}

/// 已取整毫秒的 `setTimeout` 时长钳制（`abortable_sleep` 直接接收 `u64`）。
///
/// 公开入口可能拿到 `0`（表示尽快触发）或 `u64::MAX`；零时长会忙等，超大时长在计时器里
/// 会回绕，因此两者都规范为可观察且安全的最小值 1ms。
fn set_timeout_duration(ms: u64) -> Duration {
    if ms == 0 || ms > TIMEOUT_MAX_MS as u64 {
        Duration::from_millis(1)
    } else {
        Duration::from_millis(ms)
    }
}

/// `Math.min`（任一 NaN → NaN，与 Rust 的 `f64::min` 不同：后者返回非 NaN 操作数）。
fn js_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.min(right)
    }
}

/// 先按 `setTimeout` 语义把 f64 毫秒转成整数（并钳制），再走可取消睡眠。
async fn abortable_sleep_ms(
    ms: f64,
    signal: &AbortScope,
    cancel_message: &str,
) -> Result<(), AuthError> {
    abortable_sleep(timeout_ms(ms), signal, cancel_message).await
}

/// `pollOAuthDeviceCodeFlow(options)`。
///
/// 使用系统时钟的便捷入口；确定性测试应调用
/// [`poll_oauth_device_code_flow_with_clock`] 注入假时钟。
///
/// TS: `src/auth/oauth/device-code.ts#pollOAuthDeviceCodeFlow`
pub async fn poll_oauth_device_code_flow<T>(
    options: OAuthDeviceCodePollOptions<T>,
) -> Result<T, AuthError> {
    poll_oauth_device_code_flow_with_clock(options, &SystemAuthClock).await
}

/// 带可注入时钟的 [`poll_oauth_device_code_flow`]。
///
/// 轮询直到 `Complete`、`Failed`、超时或取消。`clock` 只用于读取墙钟；睡眠仍走 tokio
/// 计时器（测试里 `start_paused` 会同时推进两者）。
///
/// 返回：`Complete` 的值。错误：`Failed` 的 `message` 原样成为 [`AuthError`]；`poll` 自身
/// 返回的错误直接上抛；取消 → `"Login cancelled"`；超时 → 两种超时文案之一。
pub async fn poll_oauth_device_code_flow_with_clock<T>(
    options: OAuthDeviceCodePollOptions<T>,
    clock: &dyn AuthClock,
) -> Result<T, AuthError> {
    // deadline 用墙钟毫秒表示；`None` 表示调用方没有给有效期（+∞，只能靠取消或 poll 结果
    // 结束）。`Some(NaN)` 会得到 NaN，循环条件立即为假 → 直接走超时错误（与 TS 的
    // `Date.now() < NaN` 一致）。
    let deadline = match options.expires_in_seconds {
        Some(seconds) => clock.now_ms() + seconds * 1000.0,
        None => f64::INFINITY,
    };
    // 初始间隔对齐 TS：`intervalSeconds ?? 5` → 乘 1000 → `Math.floor` → `Math.max(1000, ·)`。
    // 如果传入 NaN，`floor` 与 `js_max` 都保留 NaN；后续 `js_min(NaN, remaining)` 仍是 NaN，
    // 由 `timeout_ms` 按 Node `setTimeout` 规则钳成 1ms（测试已锁定）。
    let mut interval_ms = js_max(
        MINIMUM_INTERVAL_MS,
        (options
            .interval_seconds
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS))
        .mul_add(1000.0, 0.0)
        .floor(),
    );

    let mut slow_down_responses = 0u32;
    // 可选的首轮等待：只在还有剩余时间时睡，且睡眠不超过剩余时间。
    if options.wait_before_first_poll {
        let remaining_ms = deadline - clock.now_ms();
        if remaining_ms > 0.0 {
            abortable_sleep_ms(
                js_min(interval_ms, remaining_ms),
                &options.signal,
                CANCEL_MESSAGE,
            )
            .await?;
        }
    }

    // 每轮开头检查 deadline：一旦到期就不再发起新的 poll；超时文案在循环外统一选择。
    while clock.now_ms() < deadline {
        // 取消检查在 poll 之前：已取消时不再发一次网络请求（测试要求 0 次 poll）。
        // poll 执行期间没有额外的取消保护，见模块文档。
        if options.signal.is_aborted() {
            return Err(AuthError::new(CANCEL_MESSAGE));
        }

        // 每轮恰好调用一次 poll；它返回 `Result` 是为了传输/解析错误，业务状态在枚举里。
        let result = (options.poll)().await?;
        match result {
            OAuthDeviceCodePollResult::Complete { value } => return Ok(value),
            OAuthDeviceCodePollResult::Failed { message } => return Err(AuthError::new(message)),
            OAuthDeviceCodePollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // 服务端给了新最小间隔就用它（GitHub 在 `interval` 里报告新值）；
                // 否则按 RFC 8628 §3.5 增加 5 秒。
                interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        js_max(MINIMUM_INTERVAL_MS, (seconds * 1000.0).floor())
                    }
                    _ => js_max(
                        MINIMUM_INTERVAL_MS,
                        interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS,
                    ),
                };
            }
            OAuthDeviceCodePollResult::Pending => {}
        }

        // 睡眠前重算剩余时间：已到期就直接跳出，不再等待；否则等待长度不超过剩余时间。
        let remaining_ms = deadline - clock.now_ms();
        if remaining_ms <= 0.0 {
            break;
        }

        abortable_sleep_ms(
            js_min(interval_ms, remaining_ms),
            &options.signal,
            CANCEL_MESSAGE,
        )
        .await?;
    }

    // 走到这里说明 deadline 已过（或 `expires_in_seconds` 一开始就让条件为假）。文案与 TS
    // 一致：出现过 slow_down 时给出长版提示（WSL/VM 时钟漂移的常见成因）。
    Err(AuthError::new(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_timeout_matches_node_timeout_max() {
        assert_eq!(timeout_ms(5.0), 5);
        assert_eq!(timeout_ms(1000.9), 1000);
        assert_eq!(timeout_ms(0.0), 1);
        assert_eq!(timeout_ms(-5.0), 1);
        assert_eq!(timeout_ms(f64::NAN), 1);
        assert_eq!(timeout_ms(f64::INFINITY), 1);
        // Node 的 TIMEOUT_MAX 是 2^31 - 1，不是 u32::MAX。
        assert_eq!(timeout_ms(2_147_483_647.0), 2_147_483_647);
        assert_eq!(timeout_ms(2_147_483_648.0), 1);
        assert_eq!(timeout_ms(3e9), 1);
        assert_eq!(timeout_ms(f64::from(u32::MAX)), 1);
    }

    #[test]
    fn abortable_sleep_durations_match_node() {
        assert_eq!(set_timeout_duration(0), Duration::from_millis(1));
        assert_eq!(set_timeout_duration(1), Duration::from_millis(1));
        assert_eq!(set_timeout_duration(1000), Duration::from_millis(1000));
        assert_eq!(
            set_timeout_duration(2_147_483_647),
            Duration::from_millis(2_147_483_647)
        );
        assert_eq!(
            set_timeout_duration(2_147_483_648),
            Duration::from_millis(1)
        );
        assert_eq!(set_timeout_duration(u64::MAX), Duration::from_millis(1));
    }

    #[test]
    fn js_min_propagates_nan() {
        assert!(js_min(f64::NAN, 30_000.0).is_nan());
        assert!(js_min(30_000.0, f64::NAN).is_nan());
        assert_eq!(js_min(1.0, 2.0), 1.0);
        assert_eq!(js_min(2.0, 1.0), 1.0);
        assert_eq!(js_min(f64::INFINITY, 3000.0), 3000.0);
    }
}
