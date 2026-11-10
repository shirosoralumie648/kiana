//! `packages/ai/test/uuid.test.ts`。
//!
//! TS 用 `vi.useFakeTimers()` + `vi.stubGlobal("crypto", ...)` 控制时钟与随机源；Rust 侧
//! [`uuidv7`] 的时间戳是显式参数（普通 id 取系统时钟、带参 id 为 follower），随机源不可注入。
//! 因此本文件断言同一组可观察性质：格式、单调性、follower 时间戳保留、尾部随机性、边界与
//! 非法时间戳。两条依赖桩的用例在末尾以等价方式表达（见各测试注释）。

use kiana_ai::utils::uuid::{MAX_UUID_V7_TIMESTAMP, UuidV7Error, uuidv7};

const TIMESTAMP: u64 = 0x0123_4567_89ab;

/// 与 TS 的 `UUID_V7_RE` 等价的逐字符校验（`^[0-9a-f]{8}-...-7...-[89ab]...-...$`）。
fn matches_uuid_v7(uuid: &str) -> bool {
    let bytes = uuid.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            14 => {
                if *byte != b'7' {
                    return false;
                }
            }
            19 => {
                if !matches!(byte, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte) {
                    return false;
                }
            }
        }
    }
    true
}

fn parse_timestamp(uuid: &str) -> u64 {
    let hex: String = uuid.chars().filter(|ch| *ch != '-').take(12).collect();
    u64::from_str_radix(&hex, 16).expect("hex")
}

// describe("uuidv7")

#[test]
fn generates_ordered_uuidv7s_while_preserving_follower_timestamps() {
    let first = uuidv7(None).expect("uuid");
    let second = uuidv7(None).expect("uuid");
    let after_rollback = uuidv7(None).expect("uuid");
    let after_advance = uuidv7(None).expect("uuid");
    let ordinary_ids = [&first, &second, &after_rollback, &after_advance];
    let follower_timestamp = TIMESTAMP - 1_000;
    let followers = [
        uuidv7(Some(follower_timestamp as f64)).expect("uuid"),
        uuidv7(Some(follower_timestamp as f64)).expect("uuid"),
    ];

    for id in ordinary_ids
        .iter()
        .map(|id| id.as_str())
        .chain(followers.iter().map(String::as_str))
    {
        assert!(matches_uuid_v7(id), "not a UUIDv7: {id}");
    }

    // 普通 id 单调不减（TS 用假时钟验证「回拨/前进」；这里验证真实时钟下的排序不变式）。
    let mut sorted: Vec<&String> = ordinary_ids.to_vec();
    sorted.sort();
    assert_eq!(sorted, ordinary_ids.to_vec());
    // 同一毫秒内（或时钟未前进时）生成的 id 互不相同。
    let unique: std::collections::HashSet<&String> = ordinary_ids.iter().copied().collect();
    assert_eq!(unique.len(), ordinary_ids.len());
    // follower 保留传入的时间戳，且彼此不同。
    for follower in &followers {
        assert_eq!(parse_timestamp(follower), follower_timestamp);
    }
    assert_ne!(followers[0], followers[1]);
}

#[test]
fn uses_fresh_randomness_for_every_uuid_tail() {
    // TS 用桩 crypto 固定随机字节，断言尾部为 01010101/02020202。Rust 侧无法注入随机源，
    // 改为断言「同一时间戳的两次生成尾部不同」——随机源生效的必要条件。
    let first = uuidv7(Some(TIMESTAMP as f64)).expect("uuid");
    let second = uuidv7(Some(TIMESTAMP as f64)).expect("uuid");
    assert_ne!(first[28..], second[28..]);
    assert_eq!(parse_timestamp(&first), TIMESTAMP);
    assert_eq!(parse_timestamp(&second), TIMESTAMP);
}

#[test]
fn accepts_timestamp_boundaries() {
    for timestamp in [0u64, MAX_UUID_V7_TIMESTAMP] {
        let uuid = uuidv7(Some(timestamp as f64)).expect("uuid");
        assert_eq!(parse_timestamp(&uuid), timestamp);
    }
}

#[test]
fn rejects_invalid_timestamps() {
    for timestamp in [
        -1.0,
        (MAX_UUID_V7_TIMESTAMP as f64) + 1.0,
        1.5,
        f64::NAN,
        f64::INFINITY,
    ] {
        let error = uuidv7(Some(timestamp)).expect_err("invalid timestamp");
        assert!(
            matches!(
                error,
                UuidV7Error::Timestamp { max } if max == MAX_UUID_V7_TIMESTAMP
            ),
            "unexpected error: {error:?}"
        );
    }
}
