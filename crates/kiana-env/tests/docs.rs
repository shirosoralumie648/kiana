//! 随 crate 保留的上游文档（spec §2.2.17）：`docs/protocol.md`（151 行）、`docs/semantics.md`（68 行）
//! 与包根 `README.md`（66 行）、`CHANGELOG.md`（26 行）都是 `packages/env/` 的逐字副本。
//!
//! 断言字节数与 FNV-1a 64 指纹，防止编辑或重新生成时悄悄改写文本：线协议与执行语义以这两份文档为权威，
//! 而 README/CHANGELOG 记录的是随 npm 包发布的版本（ARCHITECTURE.md §8.25 不再发 npm，但文本仍逐字保留）。

/// 路径、内容、字节数、FNV-1a 64 指纹、行数（行数与 spec §2.2.17 引用的数字一致）。
const FILES: [(&str, &str, usize, u64, usize); 4] = [
    (
        "docs/protocol.md",
        include_str!("../docs/protocol.md"),
        10_579,
        0x45696e94453c9eed,
        151,
    ),
    (
        "docs/semantics.md",
        include_str!("../docs/semantics.md"),
        5_247,
        0xd9b28e26f9992a0b,
        68,
    ),
    (
        "README.md",
        include_str!("../README.md"),
        4_556,
        0x7ec3a3ab657a0031,
        66,
    ),
    (
        "CHANGELOG.md",
        include_str!("../CHANGELOG.md"),
        2_383,
        0x7472541b25f2e9f7,
        26,
    ),
];

/// FNV-1a, 64 bit: the same fingerprint the codemode prelude test uses.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[test]
fn keeps_the_upstream_documents_verbatim() {
    for (name, text, length, hash, _) in FILES {
        assert_eq!(text.len(), length, "{name} changed length");
        assert_eq!(fnv1a(text.as_bytes()), hash, "{name} changed contents");
        assert!(text.ends_with('\n'), "{name} lost its final newline");
    }
}

#[test]
fn keeps_the_documented_line_counts() {
    for (name, text, _, _, lines) in FILES {
        assert_eq!(text.lines().count(), lines, "{name} line count");
    }
    assert_eq!(
        FILES[0].1.lines().next(),
        Some("# kiana-env wire protocol"),
        "protocol.md heading"
    );
    assert_eq!(
        FILES[1].1.lines().next(),
        Some("# Semantics"),
        "semantics.md heading"
    );
    assert_eq!(
        FILES[3].1.lines().next(),
        Some("# Changelog"),
        "CHANGELOG.md heading"
    );
}

#[test]
fn documents_every_operation_the_daemon_dispatches() {
    // The op table in docs/protocol.md is the contract kiana-env-client reads; every op it names must appear.
    let protocol = FILES[0].1;
    for op in [
        "hello",
        "lstat",
        "realpath",
        "write",
        "writeChunk",
        "truncate",
        "fsync",
        "rename",
        "mkdir",
        "rm",
        "mkdtemp",
        "open",
        "pread",
        "fstat",
        "scanLines",
        "opendir",
        "readdir",
        "listFiles",
        "close",
        "exec",
        "watch",
        "pty",
        "ptyWrite",
        "ptyResize",
        "ptyForeground",
    ] {
        assert!(
            protocol.contains(op),
            "docs/protocol.md does not mention {op}"
        );
    }
}
