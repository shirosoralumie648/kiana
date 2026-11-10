//! W3C Trace Context（`traceparent` / `tracestate`）的解析、注入、提取与父子绑定。
//!
//! 规范：<https://www.w3.org/TR/trace-context/>。本模块只做纯字符串处理，不依赖后端：
//!
//! - [`TraceParent`] 是 `version-trace_id-parent_id-flags` 的结构化视图；
//! - [`TraceContext`] 额外携带可选的 `tracestate`；
//! - [`TraceContext::extract`] 从 [`TelemetryCarrier`] 读取**小写**键（混合大小写不参与，
//!   避免大小写重复竞争）；
//! - [`TraceContext::inject`] 写回同名小写键；
//! - [`TraceContext::child`] 生成同 trace id、新 span id 的子上下文——这是「父子 span
//!   绑定」的唯一来源。
//!
//! # 校验规则
//!
//! - `version = ff` 非法；`version = 00` 时整串必须恰好 55 字符；
//! - trace id 32 hex、span id 16 hex，全零非法；
//! - flags 2 hex（bit 0 = sampled，其它位保留原样）；
//! - `tracestate` 最多 32 条、每条 `key=value`，key 小写且字符集受限；不合法时整个
//!   tracestate 被丢弃（traceparent 仍然有效）。
//!
//! # 与 `TelemetryCarrier` 的关系
//!
//! `TelemetryCarrier` 是 `IndexMap<String, String>` 的别名（可序列化、跨进程传递的字符串
//! map）。本模块把 W3C 键写进去/读出来；同一 carrier 里可以同时存在后端私有键（内存实现
//! 的 `kiana-memory-*`），互不干扰。

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::index::TelemetryCarrier;

/// `traceparent` 的键名（小写，W3C 规定）。
pub const TRACEPARENT: &str = "traceparent";
/// `tracestate` 的键名（小写，W3C 规定）。
pub const TRACESTATE: &str = "tracestate";
/// 子进程环境变量名（大写，OpenTelemetry 约定）。
pub const TRACEPARENT_ENV: &str = "TRACEPARENT";
/// `tracestate` 的子进程环境变量名。
pub const TRACESTATE_ENV: &str = "TRACESTATE";

/// W3C sampled flag（bit 0）。
pub const TRACE_FLAG_SAMPLED: u8 = 0x01;

/// 16 字节 trace id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub [u8; 16]);

impl TraceId {
    /// 全零 id（非法；用于校验）。
    pub const ZERO: TraceId = TraceId([0; 16]);

    /// 随机生成一个非零 trace id。
    pub fn random() -> Self {
        loop {
            let id = TraceId(Uuid::new_v4().into_bytes());
            if id != TraceId::ZERO {
                return id;
            }
        }
    }

    /// 32 位小写 hex。
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }

    /// 从 32 位 hex 解析；长度/字符/全零非法。
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 32
            || !text
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        let id = TraceId(bytes);
        (id != TraceId::ZERO).then_some(id)
    }

    /// 是否全零。
    pub fn is_zero(self) -> bool {
        self == TraceId::ZERO
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// 8 字节 span id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(pub [u8; 8]);

impl SpanId {
    /// 全零 id（非法）。
    pub const ZERO: SpanId = SpanId([0; 8]);

    /// 随机生成一个非零 span id。
    pub fn random() -> Self {
        loop {
            let uuid = Uuid::new_v4().into_bytes();
            let id = SpanId(uuid[..8].try_into().expect("8 bytes"));
            if id != SpanId::ZERO {
                return id;
            }
        }
    }

    /// 16 位小写 hex。
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(16);
        for byte in self.0 {
            text.push_str(&format!("{byte:02x}"));
        }
        text
    }

    /// 从 16 位 hex 解析；长度/字符/全零非法。
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 16
            || !text
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        {
            return None;
        }
        let mut bytes = [0u8; 8];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        let id = SpanId(bytes);
        (id != SpanId::ZERO).then_some(id)
    }

    /// 是否全零。
    pub fn is_zero(self) -> bool {
        self == SpanId::ZERO
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// 解析后的 `traceparent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceParent {
    /// 协议版本（当前 0）。
    pub version: u8,
    /// trace id。
    pub trace_id: TraceId,
    /// 父 span id（发送方自己的 span）。
    pub span_id: SpanId,
    /// trace flags。
    pub flags: u8,
}

impl TraceParent {
    /// 用随机 id 创建根上下文。
    pub fn new_root() -> Self {
        TraceParent {
            version: 0,
            trace_id: TraceId::random(),
            span_id: SpanId::random(),
            flags: TRACE_FLAG_SAMPLED,
        }
    }

    /// 是否采样。
    pub fn is_sampled(&self) -> bool {
        self.flags & TRACE_FLAG_SAMPLED != 0
    }

    /// 同一 trace 下的子 span（新 span id，其余标志保持）。
    pub fn child(&self) -> Self {
        TraceParent {
            version: self.version,
            trace_id: self.trace_id,
            span_id: SpanId::random(),
            flags: self.flags,
        }
    }

    /// 55 字符的标准文本（version 恒为 00）。
    pub fn to_header(self) -> String {
        format!(
            "{:02x}-{}-{}-{:02x}",
            self.version,
            self.trace_id.to_hex(),
            self.span_id.to_hex(),
            self.flags
        )
    }
}

impl fmt::Display for TraceParent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_header())
    }
}

impl FromStr for TraceParent {
    type Err = TraceContextError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let mut parts = text.split('-');
        let version = parts
            .next()
            .filter(|part| part.len() == 2)
            .and_then(|part| u8::from_str_radix(part, 16).ok())
            .ok_or(TraceContextError::Malformed)?;
        if version == 0xff {
            return Err(TraceContextError::Malformed);
        }
        let trace_id = parts
            .next()
            .and_then(TraceId::from_hex)
            .ok_or(TraceContextError::Malformed)?;
        let span_id = parts
            .next()
            .and_then(SpanId::from_hex)
            .ok_or(TraceContextError::Malformed)?;
        let flags = parts
            .next()
            .filter(|part| part.len() == 2)
            .and_then(|part| u8::from_str_radix(part, 16).ok())
            .ok_or(TraceContextError::Malformed)?;
        let rest = parts.collect::<Vec<_>>();
        if version == 0 {
            if !rest.is_empty() {
                return Err(TraceContextError::Malformed);
            }
        } else if rest.is_empty() {
            // 高版本允许额外字段，但至少要有一个；没有也接受（宽松）。
        }
        Ok(TraceParent {
            version,
            trace_id,
            span_id,
            flags,
        })
    }
}

/// trace context 解析错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceContextError {
    /// traceparent 形状非法。
    Malformed,
}

impl fmt::Display for TraceContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("malformed traceparent")
    }
}

impl std::error::Error for TraceContextError {}

/// `traceparent` + 可选 `tracestate`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// traceparent 结构。
    pub traceparent: TraceParent,
    /// 原始 tracestate；校验失败时为 `None`。
    pub tracestate: Option<String>,
}

impl TraceContext {
    /// 用随机 id 创建根上下文。
    pub fn new_root() -> Self {
        TraceContext {
            traceparent: TraceParent::new_root(),
            tracestate: None,
        }
    }

    /// 同 trace、新 span id 的子上下文；tracestate 原样继承。
    pub fn child(&self) -> Self {
        TraceContext {
            traceparent: self.traceparent.child(),
            tracestate: self.tracestate.clone(),
        }
    }

    /// 带 tracestate 的 builder；非法 tracestate 会被丢弃。
    pub fn with_tracestate(mut self, tracestate: impl Into<String>) -> Self {
        let tracestate = tracestate.into();
        self.tracestate = validate_tracestate(&tracestate).map(|_| tracestate);
        self
    }

    /// 是否采样。
    pub fn is_sampled(&self) -> bool {
        self.traceparent.is_sampled()
    }

    /// 从 carrier 提取。只读小写键；`traceparent` 缺失或非法时返回 `None`。
    pub fn extract(carrier: &TelemetryCarrier) -> Option<Self> {
        let traceparent = carrier.get(TRACEPARENT)?.parse::<TraceParent>().ok()?;
        let tracestate = carrier
            .get(TRACESTATE)
            .filter(|value| validate_tracestate(value).is_some())
            .cloned();
        Some(TraceContext {
            traceparent,
            tracestate,
        })
    }

    /// 写入 carrier（小写键）。tracestate 为空/非法时不写该键。
    pub fn inject(&self, carrier: &mut TelemetryCarrier) {
        carrier.insert(TRACEPARENT.to_string(), self.traceparent.to_header());
        match self
            .tracestate
            .as_deref()
            .filter(|value| validate_tracestate(value).is_some())
        {
            Some(tracestate) => {
                carrier.insert(TRACESTATE.to_string(), tracestate.to_string());
            }
            None => {
                carrier.shift_remove(TRACESTATE);
            }
        }
    }

    /// 子进程环境变量对（`TRACEPARENT` 必有，`TRACESTATE` 可选）。
    pub fn to_env_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![(TRACEPARENT_ENV, self.traceparent.to_header())];
        if let Some(tracestate) = self
            .tracestate
            .as_deref()
            .filter(|value| validate_tracestate(value).is_some())
        {
            pairs.push((TRACESTATE_ENV, tracestate.to_string()));
        }
        pairs
    }

    /// 从环境变量值解析（`traceparent` 必填）。
    pub fn from_env_values(traceparent: Option<&str>, tracestate: Option<&str>) -> Option<Self> {
        let traceparent = traceparent?.parse::<TraceParent>().ok()?;
        let tracestate = tracestate
            .filter(|value| validate_tracestate(value).is_some())
            .map(ToString::to_string);
        Some(TraceContext {
            traceparent,
            tracestate,
        })
    }
}

/// 校验 tracestate：最多 32 条、`key=value`、key 小写且字符集受限、总长度 <= 512。
pub fn validate_tracestate(value: &str) -> Option<()> {
    if value.is_empty() || value.len() > 512 {
        return None;
    }
    let entries: Vec<&str> = value.split(',').collect();
    if entries.len() > 32 {
        return None;
    }
    for entry in entries {
        let entry = entry.trim();
        let (key, value) = entry.split_once('=')?;
        if key.is_empty() || value.is_empty() {
            return None;
        }
        let mut key_chars = key.chars();
        let first = key_chars.next()?;
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return None;
        }
        if !key_chars.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || "_-*/".contains(character)
        }) {
            return None;
        }
        if !value
            .chars()
            .all(|character| character.is_ascii_graphic() && character != ',' && character != '=')
        {
            return None;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_ID: &str = "00f067aa0ba902b7";
    const PARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn parses_and_formats_the_w3c_sample() {
        let parsed: TraceParent = PARENT.parse().expect("parse");
        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.trace_id.to_hex(), TRACE_ID);
        assert_eq!(parsed.span_id.to_hex(), SPAN_ID);
        assert!(parsed.is_sampled());
        assert_eq!(parsed.to_header(), PARENT);
    }

    #[test]
    fn rejects_invalid_traceparents() {
        for value in [
            "not-a-traceparent",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "",
        ] {
            assert!(value.parse::<TraceParent>().is_err(), "{value}");
        }
    }

    #[test]
    fn injects_and_extracts_lowercase_keys_only() {
        let context =
            TraceContext::from_env_values(Some(PARENT), Some("vendor=value")).expect("ctx");
        let mut carrier = TelemetryCarrier::new();
        context.inject(&mut carrier);
        assert_eq!(carrier.get(TRACEPARENT).map(String::as_str), Some(PARENT));
        assert_eq!(
            carrier.get(TRACESTATE).map(String::as_str),
            Some("vendor=value")
        );

        let extracted = TraceContext::extract(&carrier).expect("extract");
        assert_eq!(extracted, context);

        // 大小写不同的键不会被读取。
        let mut mixed = TelemetryCarrier::new();
        mixed.insert("TraceParent".into(), PARENT.into());
        assert!(TraceContext::extract(&mixed).is_none());
    }

    #[test]
    fn rejects_missing_or_invalid_traceparent() {
        let mut carrier = TelemetryCarrier::new();
        assert!(TraceContext::extract(&carrier).is_none());
        carrier.insert(TRACEPARENT.into(), "not-a-traceparent".into());
        assert!(TraceContext::extract(&carrier).is_none());
        carrier.insert(TRACEPARENT.into(), PARENT.into());
        carrier.insert(TRACESTATE.into(), "UPPER=case".into());
        let context = TraceContext::extract(&carrier).expect("traceparent still valid");
        assert_eq!(context.tracestate, None, "invalid tracestate is dropped");
    }

    #[test]
    fn child_shares_the_trace_id_and_gets_a_new_span_id() {
        let parent: TraceParent = PARENT.parse().expect("parse");
        let child = parent.child();
        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.flags, parent.flags);
        assert_eq!(child.version, parent.version);

        let context =
            TraceContext::from_env_values(Some(PARENT), Some("vendor=value")).expect("ctx");
        let child = context.child();
        assert_eq!(child.traceparent.trace_id, context.traceparent.trace_id);
        assert_eq!(child.tracestate.as_deref(), Some("vendor=value"));
    }

    #[test]
    fn env_pairs_follow_the_uppercase_otel_names() {
        let context =
            TraceContext::from_env_values(Some(PARENT), Some("vendor=value")).expect("ctx");
        assert_eq!(
            context.to_env_pairs(),
            vec![
                (TRACEPARENT_ENV, PARENT.to_string()),
                (TRACESTATE_ENV, "vendor=value".to_string())
            ]
        );
        let child = context.child();
        let pairs = child.to_env_pairs();
        assert_eq!(pairs[0].0, "TRACEPARENT");
        assert_eq!(pairs[0].1.split('-').nth(1).unwrap(), TRACE_ID);
    }

    #[test]
    fn tracestate_validation_is_conservative() {
        assert!(validate_tracestate("vendor=value,other=1").is_some());
        assert!(validate_tracestate("").is_none());
        assert!(validate_tracestate("Vendor=value").is_none());
        assert!(validate_tracestate("vendor=").is_none());
        assert!(validate_tracestate("vendor=has,comma").is_none());
        assert!(validate_tracestate(&"a=1,".repeat(33)).is_none());
        assert!(validate_tracestate(&format!("vendor={}", "x".repeat(600))).is_none());
    }

    #[test]
    fn random_ids_are_non_zero_and_hex_shaped() {
        for _ in 0..32 {
            let trace = TraceId::random();
            let span = SpanId::random();
            assert!(!trace.is_zero());
            assert!(!span.is_zero());
            assert_eq!(trace.to_hex().len(), 32);
            assert_eq!(span.to_hex().len(), 16);
            assert_eq!(TraceId::from_hex(&trace.to_hex()), Some(trace));
            assert_eq!(SpanId::from_hex(&span.to_hex()), Some(span));
        }
    }
}
