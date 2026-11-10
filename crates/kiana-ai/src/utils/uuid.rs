//! `utils/uuid.ts`——时间有序的 UUIDv7。
//!
//! TS 用模块级可变状态（`lastOrdinaryTimestamp`、`sequence`）保证同一毫秒内生成的 id
//! 单调：无参调用取 `max(now, lastOrdinaryTimestamp)` 并回写；带参调用按原值生成且**不**更新
//! `lastOrdinaryTimestamp`（follower id），但仍会推进共享的 `sequence`。Rust 用 `Mutex`
//! 串行化（`spec/ai-core.md` §2.2.6）。
//!
//! 字节布局（RFC 9562）：48 位大端毫秒时间戳 + 版本 7 + 41 位序列 + variant 10。
//!
//! TS: `utils/uuid.ts#uuidv7`
//!
//! # 状态机与并发
//!
//! 全局状态只有两项：`last_ordinary_timestamp`（普通调用见过的时间戳高点）与 `sequence`
//! （所有调用共享的计数器）。对它们读取与更新必须在同一临界区内完成，才能保证「同一毫秒内
//! id 不重复且单调」，因此 [`uuidv7`] 在**同一把锁**里完成「读时间戳 → 取随机字节 →
//! 推进序列」。拆成多把锁会让两个线程拿到相同的 `(timestamp, sequence)` 组合。临界区包含
//! `getrandom::fill` 调用（可能进入系统调用），所以随机源慢时会串行化所有调用者；这与 TS
//! 的单线程模块状态语义一致。
//!
//! - 普通调用（`None`）：`effective = max(now_ms, last_ordinary_timestamp)` 并写回；
//!   系统时钟回拨时时间戳停在旧高点，靠 `sequence` 递增继续保证不重复。
//! - follower 调用（`Some(ts)`）：时间戳原样使用，**不**更新 `last_ordinary_timestamp`，
//!   但仍会消耗并推进全局 `sequence`。因此两个 follower 彼此不同，follower 的字典序却
//!   可能小于此前的普通 id（时间戳更早）。
//! - `sequence` 在首次调用时由 5 个随机字节初始化（实际只用 40 位），此后每次 `+1`；
//!   当当前值 `seq` 已是 `MAX_SEQUENCE`（41 位全 1）时返回
//!   [`UuidV7Error::SequenceExhausted`]。因此 `seq == MAX_SEQUENCE - 1` 的那次调用会成功
//!   返回 `MAX_SEQUENCE` 并把状态写回它——实际可用到的最后一个值是 `MAX_SEQUENCE`，
//!   再下一次调用才失败；失败后不会回绕或重新播种。
//!
//! # 失败模式
//!
//! - 显式时间戳非有限、带小数或越界 → [`UuidV7Error::Timestamp`]，错误文本中的上限是
//!   十进制 `281474976710655`（与 TS 模板字符串一致）。
//! - 操作系统随机源失败时 `getrandom::fill(...).expect(...)` 直接 **panic**：错误枚举没有
//!   随机源变体，调用方无法通过 `Result` 恢复。
//! - 持锁线程 panic 会使 `Mutex` 中毒；这里用 `into_inner()` 继续使用内部状态。状态只是
//!   两个整数，没有需要修复的跨字段不变量。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// `MAX_UUID_V7_TIMESTAMP = 0xffffffffffff`（48 位）。
///
/// 也是 [`UuidV7Error::Timestamp`] 携带的上限：错误消息按十进制渲染这个值。
pub const MAX_UUID_V7_TIMESTAMP: u64 = 0xffff_ffff_ffff;

/// 序列上限 `(1n << 41n) - 1n`。
const MAX_SEQUENCE: u64 = (1u64 << 41) - 1;

/// [`uuidv7`] 的错误（TS 的两种 `RangeError`）。
///
/// `Display` 文本是稳定的错误文案（上限用十进制），因为上层可能直接展示或按文本匹配
/// 错误。没有覆盖「随机源失败」——那种情况直接 panic，见模块文档。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UuidV7Error {
    /// 时间戳不是 `0..=2^48-1` 的整数。
    #[error("UUIDv7 timestamp must be an integer between 0 and {max}")]
    Timestamp {
        /// 允许的最大时间戳（十进制渲染，与 TS 的模板字符串一致）。
        max: u64,
    },
    /// 序列耗尽。
    ///
    /// 永久性失败：`sequence` 停在 `MAX_SEQUENCE`，之后每次调用都返回该错误，即使时钟
    /// 继续前进也不会恢复（TS 行为相同）。
    #[error("UUIDv7 generator sequence exhausted")]
    SequenceExhausted,
}

#[derive(Debug)]
struct State {
    last_ordinary_timestamp: i64,
    sequence: Option<u64>,
}

static STATE: Mutex<State> = Mutex::new(State {
    last_ordinary_timestamp: -1,
    sequence: None,
});

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// 当前 Unix 毫秒（`Date.now()`）。
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 生成一个时间有序的 UUIDv7。
///
/// - `timestamp_ms == None`：用当前时间，并保证不小于上次生成的普通 id 的时间戳；
/// - `Some(ts)`：按给定时间戳生成 follower id，不更新 `last_ordinary_timestamp`
///   ——但仍会推进全局 `sequence`，因此连续的 follower 彼此不同。
///
/// `Some(ts)` 的 `ts` 必须是 `0..=MAX_UUID_V7_TIMESTAMP` 之间的整数（`Number.isInteger`）。
///
/// # 参数
///
/// 时间戳用 `f64` 承载 TS 的 `number`：`NaN`、`±∞`、带小数、负数或超过
/// [`MAX_UUID_V7_TIMESTAMP`] 的值都返回 [`UuidV7Error::Timestamp`]。
///
/// # 返回
///
/// 小写十六进制 8-4-4-4-12，版本位固定 7、variant 位固定 `10`。普通调用产生的 id 按
/// `(timestamp, sequence)` 递增，而时间戳占最高 48 位，所以字符串字典序与生成顺序一致；
/// follower 的时间戳可能更小，不参与这个顺序保证。
///
/// # 失败
///
/// [`UuidV7Error::SequenceExhausted`] 之后生成器永久失败；操作系统随机源故障是 panic
/// （见模块文档）。`Timestamp` 需要调用方改用合法时间戳后再试，`SequenceExhausted` 无法
/// 通过重试恢复。
///
/// TS: `utils/uuid.ts#uuidv7`
pub fn uuidv7(timestamp_ms: Option<f64>) -> Result<String, UuidV7Error> {
    // 先校验显式时间戳再进锁：非法输入直接返回错误，不消耗全局序列号。
    let requested = match timestamp_ms {
        Some(value) => {
            if !value.is_finite()
                || value.fract() != 0.0
                || value < 0.0
                || value > MAX_UUID_V7_TIMESTAMP as f64
            {
                return Err(UuidV7Error::Timestamp {
                    max: MAX_UUID_V7_TIMESTAMP,
                });
            }
            value as i64
        }
        None => now_ms(),
    };

    // TS 的顺序：先算 effectiveTimestamp 并回写全局状态，再取 16 随机字节、用 bytes[1..5]
    // 组成 sequence（或自增），最后把时间戳写回 bytes[0..6]（覆盖掉那 6 个随机字节）。
    let mut bytes = [0u8; 16];
    let (effective_timestamp, sequence) = with_state(|state| -> Result<(u64, u64), UuidV7Error> {
        let effective = match timestamp_ms {
            None => {
                let effective = requested.max(state.last_ordinary_timestamp);
                state.last_ordinary_timestamp = effective;
                effective
            }
            Some(_) => requested,
        };
        getrandom::fill(&mut bytes).expect("OS randomness is available");
        let sequence = match state.sequence {
            None => {
                let seq = (u64::from(bytes[1]) << 32)
                    | (u64::from(bytes[2]) << 24)
                    | (u64::from(bytes[3]) << 16)
                    | (u64::from(bytes[4]) << 8)
                    | u64::from(bytes[5]);
                state.sequence = Some(seq);
                seq
            }
            Some(seq) => {
                if seq == MAX_SEQUENCE {
                    return Err(UuidV7Error::SequenceExhausted);
                }
                state.sequence = Some(seq + 1);
                seq + 1
            }
        };
        Ok((effective.max(0) as u64, sequence))
    })?;

    // 时间戳按大端写回 bytes[0..6]，覆盖前面那 6 个随机字节：时间戳占据 UUID 的最高 48 位，
    // 因此普通 id 的字典序等于时间顺序。
    for index in (0..6).rev() {
        bytes[index] = ((effective_timestamp >> ((5 - index) * 8)) & 0xff) as u8;
    }
    // 41 位序列拆进 bytes[6..12]：bytes[6] 高 4 位是版本 7，bytes[8] 高 2 位是 variant
    // `10`；bytes[11] 的最低位保留随机值（TS 的 `& 0x01`），其余位来自序列。
    bytes[6] = 0x70 | ((sequence >> 37) & 0x0f) as u8;
    bytes[7] = ((sequence >> 29) & 0xff) as u8;
    bytes[8] = 0x80 | ((sequence >> 23) & 0x3f) as u8;
    bytes[9] = ((sequence >> 15) & 0xff) as u8;
    bytes[10] = ((sequence >> 7) & 0xff) as u8;
    bytes[11] = (((sequence & 0x7f) << 1) as u8) | (bytes[11] & 0x01);

    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..].concat()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp_of(uuid: &str) -> u64 {
        u64::from_str_radix(&uuid.replace('-', "")[..12], 16).expect("hex")
    }

    #[test]
    fn preserves_follower_timestamps_and_orders_ordinary_ids() {
        let first = uuidv7(None).expect("uuid");
        let second = uuidv7(None).expect("uuid");
        let followers = [
            uuidv7(Some(1234.0)).expect("uuid"),
            uuidv7(Some(1234.0)).expect("uuid"),
        ];
        assert_eq!(timestamp_of(&followers[0]), 1234);
        assert_eq!(timestamp_of(&followers[1]), 1234);
        assert_ne!(followers[0], followers[1]);
        assert!(first <= second);
    }

    #[test]
    fn rejects_invalid_timestamps() {
        assert_eq!(
            uuidv7(Some(-1.0)).unwrap_err(),
            UuidV7Error::Timestamp {
                max: MAX_UUID_V7_TIMESTAMP
            }
        );
        assert!(uuidv7(Some(1.5)).is_err());
        assert!(uuidv7(Some(f64::NAN)).is_err());
        assert!(uuidv7(Some(f64::INFINITY)).is_err());
        // 2^48 超上限。
        assert!(uuidv7(Some(281_474_976_710_656.0)).is_err());
    }
}
