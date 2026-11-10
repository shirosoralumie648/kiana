//! utils/text.ts。
//!
//! 逐字移植 `packages/coding-agent/src/utils/text.ts`（9 行）：
//!
//! - `split_bom`：以 `﻿` 开头 → `{bom: "﻿", text: 去掉首字符}`；
//!   否则 `{bom: "", text: 原样}`。
//! - `strip_bom`：`split_bom(content).text`。
//!
//! TS 的 `string.startsWith("﻿")` / `slice(1)` 都按 UTF-16 code unit 工作，
//! 但 U+FEFF 是单个 BMP code unit，Rust 的 `str::strip_prefix('\u{FEFF}')` 与
//! `&s[3..]`（U+FEFF 的 UTF-8 编码是 `EF BB BF`）语义完全一致，不需要 UTF-16 适配。
//!
//! ## 使用场景与约束
//!
//! 从磁盘读取的文本（主题 JSON、配置文件、迁移数据、prompt/context 文件）可能以 BOM
//! 开头；在交给 `serde_json` 或 JS 语义的解析器之前先调用 [`strip_bom`]，否则 BOM 会被
//! 当作正文首字符导致解析失败。调用方分布在 `kiana-theme`、`kiana-resources`、
//! `kiana-config`、`kiana-coding-agent` 等 crate。
//!
//! 两个函数都会产生新分配的 `String` 数据（即使输入没有 BOM 也会复制一遍）：`split_bom`
//! 放进 `BomSplit::bom` / `BomSplit::text`，`strip_bom` 直接返回 `String`；
//! 需要零拷贝时调用方可以自行 `strip_prefix`。
//!
//! 只去掉**开头一个** BOM；文本中间的 U+FEFF 原样保留（`tests/text.rs` 断言）。
//! 空串输入得到空 `bom` 与空 `text`；只有 BOM 的输入得到 `bom == "\u{FEFF}"`、`text == ""`。

/// `splitBom(content)` 的返回值：`bom` 是 `"﻿"` 或空串，`text` 是去掉 BOM 的正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BomSplit {
    /// 检测到的 BOM：`"\u{FEFF}"`（U+FEFF，UTF-8 编码为 `EF BB BF`）或空串。
    pub bom: String,
    /// 去掉开头一个 BOM 后的正文；输入没有 BOM 时与输入内容相同。
    pub text: String,
}

/// 拆出开头的一个 UTF-8 BOM（U+FEFF）。
///
/// 返回值总是拥有数据：`bom` 为 `"\u{FEFF}"` 或 `""`，`text` 为剩余部分（可能为空）。
/// 只检查第一个字符，不扫描全文；非 UTF-8 输入不适用（参数已经是 `&str`）。
pub fn split_bom(content: &str) -> BomSplit {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => BomSplit {
            bom: "\u{FEFF}".to_string(),
            text: rest.to_string(),
        },
        None => BomSplit {
            bom: String::new(),
            text: content.to_string(),
        },
    }
}

/// 去掉开头的一个 UTF-8 BOM（U+FEFF）。
///
/// 等价于 `split_bom(content).text`，BOM 本身被丢弃；输入无 BOM 时原样复制返回。
pub fn strip_bom(content: &str) -> String {
    split_bom(content).text
}
