//! `frame.rs`：长度前缀帧，`u32 length | u8 type | u32 id | u32 jsonLength | json | payload`（docs/protocol.md）。
//!
//! TS 源：packages/env/daemon/src/frame.rs（原 daemon 本身就是 Rust；逐字移植，签名不变）。
//! 线协议的权威文本见 `crates/kiana-env/docs/protocol.md`。
//! Length-prefixed frames: `u32 length | u8 type | u32 id | u32 jsonLength | json | payload` (docs/protocol.md).
//!
//! # 为什么需要这个文件
//! 守护进程与客户端之间的 stdin/stdout 是连续的字节流，没有消息边界。所有通信都切成“长度前缀 +
//! 小 JSON 头 + 原始 payload”的帧：JSON 放控制字段与小型结果，文件内容和命令输出直接作为原始字节
//! 传输，既不膨胀（无需 base64），也不会被 JSON 转义规则改变内容。
//!
//! # 帧布局
//! ```text
//! u32 length | u8 type | u32 id | u32 jsonLength | json (jsonLength 字节) | payload
//! ```
//! 所有整数都是大端序（网络字节序）。`length` 是它自身之后所有字节的数量，即
//! `9 + jsonLength + payload.len()`；`type` 的取值与方向见本文件的常量和 docs/protocol.md。
//!
//! # 读端的结束与容错
//! - 长度前缀尚未读满就遇到 EOF（0..=3 字节）：视为干净结束，返回 `None`；
//!   长度前缀完整但 body 不足则是 `UnexpectedEof` 错误，连接断开。
//! - JSON 解析失败不终结连接：帧以 `Value::Null` 交给上层，由请求分发处回复 `EINVAL` “invalid JSON”；
//!   `jsonLength == 0` 是合法的空对象 `{}`。
//!
//! # 写端约束
//! - 帧体（`length` 所计数的部分，不含 4 字节前缀）超过 `MAX_FRAME` 时返回错误，绝不截断发送；
//! - 每帧写完都 `flush`：写线程外层是 1 MiB 的 `BufWriter`（见 main.rs），不 flush 会让小帧滞留在缓冲区；
//! - `MAX_PAYLOAD` 是提供给调用方（`pread`、`listFiles`）的预留量，本文件不单独校验它。

use serde_json::Value;
use std::io::{self, Read, Write};

/// 客户端发往守护进程的请求（type 1）；`json.op` 指明操作名。
pub const REQUEST: u8 = 1;
/// 请求成功的结果（type 2，守护进程 → 客户端）；`id` 对应请求，发出后该请求结束。
pub const RESULT: u8 = 2;
/// 请求失败（type 3，守护进程 → 客户端）；JSON 为 `{ code, message, ... }`，同样结束请求。
pub const ERROR: u8 = 3;
/// 运行中请求的进度事件（type 4，守护进程 → 客户端）；用于 `exec` / `pty` 输出与 `watch` 变更，
/// 不结束请求。
pub const EVENT: u8 = 4;
/// 客户端取消某个请求（type 5）；`id` 是被取消的请求。
pub const CANCEL: u8 = 5;
/// 双向存活探测（type 6）；接收方忽略内容。
pub const PING: u8 = 6;

/// Largest frame either side accepts.
///
/// 即 `length` 字段允许的最大值（16 MiB），也是收发两端共同的硬上限。
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Largest payload a response carries, leaving room for its JSON.
///
/// 等于 `MAX_FRAME - 64 KiB`：给 9 字节帧头与结果 JSON 共预留 64 KiB，
/// 使“上限大小的 payload + 适量 JSON”仍能装进一帧。
pub const MAX_PAYLOAD: usize = MAX_FRAME - 64 * 1024;

/// 一帧的完整内容：类型、请求 id、JSON 头与原始 payload。
///
/// `json` 在读端解析失败时是 `Value::Null`（见 `read_frame`），空 JSON 则是 `{}`；
/// `id` 为 0 表示不属于任何请求（如 ping）。
pub struct Frame {
    /// 帧类型，取值为 `REQUEST`…`PING`。
    pub kind: u8,
    /// 请求 id：结果、错误与事件回填对应请求的 id；ping 用 0。
    pub id: u32,
    /// JSON 头；读端解析失败时为 `Value::Null`，长度 0 时为 `{}`。
    pub json: Value,
    /// 原始载荷字节（文件内容、命令输出等），不经过 JSON 编码。
    pub payload: Vec<u8>,
}

impl Frame {
    /// 构造不带 payload 的帧（结果、错误、ping 等控制帧）。
    pub fn new(kind: u8, id: u32, json: Value) -> Frame {
        Frame {
            kind,
            id,
            json,
            payload: Vec::new(),
        }
    }

    /// 构造带原始 payload 的帧（结果数据或输出事件）。
    pub fn with_payload(kind: u8, id: u32, json: Value, payload: Vec<u8>) -> Frame {
        Frame {
            kind,
            id,
            json,
            payload,
        }
    }
}

/// 本文件主动构造的帧格式错误使用 `InvalidData`：`length`/`jsonLength` 越界、JSON 无法序列化、
/// 帧过大。`read_exact` 的 `UnexpectedEof` 与底层 `Read`/`Write` 的其它 I/O 错误不经过这里，原样传播。
///
/// 读端错误会沿 `read_frame` 的 `?` 传播到 `serve_frames`，最终结束连接；
/// 写端错误由 `Output::run` 视为发送失败，关闭输出并停止写线程。
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Read one frame; `None` at a clean end of input.
///
/// 只有在长度前缀还没读满（0..=3 字节后 EOF）时才返回 `Ok(None)`，守护进程把它当作客户端正常断开。
/// `Err` 有三种来源：本文件构造的帧格式错误（`length` 不在 `9..=MAX_FRAME`、`jsonLength` 超出本帧，
/// 返回 `InvalidData`）、body 提前结束（`read_exact` 的 `UnexpectedEof`），以及底层 `Read` 自身的
/// I/O 错误（原样传播）。
///
/// JSON 内容不合法不算帧错误：它变成 `Value::Null` 交给上层，以便仍能按 `id` 回复错误帧。
/// body 按 `length` 一次性分配，而 `length` 已先被限制在 16 MiB，因此每个坏帧最多造成一次
/// 16 MiB 分配（读取结束后即释放）。
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut length = [0u8; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length) as usize;
    if !(9..=MAX_FRAME).contains(&length) {
        return Err(invalid("frame length out of range"));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    let kind = body[0];
    let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
    let json_length = u32::from_be_bytes([body[5], body[6], body[7], body[8]]) as usize;
    if 9 + json_length > length {
        return Err(invalid("JSON length out of range"));
    }
    // Unparseable JSON leaves the framing intact: the frame arrives with `Value::Null` and gets an error reply.
    let json = if json_length == 0 {
        Value::Object(Default::default())
    } else {
        serde_json::from_slice(&body[9..9 + json_length]).unwrap_or(Value::Null)
    };
    let payload = body[9 + json_length..].to_vec();
    Ok(Some(Frame {
        kind,
        id,
        json,
        payload,
    }))
}

/// 写出一帧并 flush：4 字节长度前缀 + type/id/jsonLength 头 + JSON + payload。
///
/// 错误：JSON 序列化失败（正常 `Value` 不会发生）或帧体长度（`9 + JSON + payload`）超过 `MAX_FRAME`
/// 时返回 `InvalidData`，底层写入错误原样返回。写入中途失败时已发出的字节无法回滚，连接应视为不可用
/// （`Output::run` 收到错误后会关闭输出并停止写线程）。
pub fn write_frame(output: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let json = serde_json::to_vec(&frame.json).map_err(|_| invalid("unserializable JSON"))?;
    let length = 9 + json.len() + frame.payload.len();
    if length > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(length as u32).to_be_bytes());
    header.push(frame.kind);
    header.extend_from_slice(&frame.id.to_be_bytes());
    header.extend_from_slice(&(json.len() as u32).to_be_bytes());
    output.write_all(&header)?;
    output.write_all(&json)?;
    output.write_all(&frame.payload)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_json_and_payload() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &Frame::with_payload(RESULT, 7, json!({ "a": 1 }), vec![0, 255, 10]),
        )
        .unwrap();
        let frame = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!((frame.kind, frame.id), (RESULT, 7));
        assert_eq!(frame.json, json!({ "a": 1 }));
        assert_eq!(frame.payload, vec![0, 255, 10]);
        assert!(read_frame(&mut &[][..]).unwrap().is_none());
    }

    /// A frame as the bytes of its header plus body, for the malformed-input cases.
    fn raw(kind: u8, id: u32, json_length: u32, body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.push(kind);
        frame.extend_from_slice(&id.to_be_bytes());
        frame.extend_from_slice(&json_length.to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn a_partial_header_is_a_clean_end() {
        // `read_exact` reports UnexpectedEof after 1..3 bytes too, so the daemon stops without an error.
        for bytes in [&[][..], &[0], &[0, 0], &[0, 0, 0]] {
            assert!(read_frame(&mut &*bytes).unwrap().is_none(), "{bytes:?}");
        }
    }

    #[test]
    fn out_of_range_lengths_are_errors() {
        // Below the 9-byte header, and above MAX_FRAME.
        for length in [0u32, 8, MAX_FRAME as u32 + 1] {
            let error = match read_frame(&mut &length.to_be_bytes()[..]) {
                Err(error) => error,
                Ok(_) => panic!("length {length} must be refused"),
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "frame length out of range");
        }
    }

    #[test]
    fn a_short_body_is_an_error_not_an_end() {
        // The length header promises more bytes than arrive: EOF after the header is an error.
        let mut bytes = 20u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&[RESULT, 0, 0, 0, 1, 0, 0, 0, 0]);
        let error = match read_frame(&mut &bytes[..]) {
            Err(error) => error,
            Ok(_) => panic!("a short body must be an error"),
        };
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_json_length_past_the_frame_is_an_error() {
        // A 9-byte body whose JSON length says 5 bytes, leaving no room for them.
        let error = match read_frame(&mut &raw(REQUEST, 1, 5, &[1, 0, 0, 0, 1, 5, 0, 0, 0])[..]) {
            Err(error) => error,
            Ok(_) => panic!("a JSON length past the frame must be refused"),
        };
        assert_eq!(error.to_string(), "JSON length out of range");
    }

    #[test]
    fn unparseable_json_becomes_null_and_zero_length_is_an_empty_object() {
        // Framing stays intact; the handler turns Null into EINVAL "invalid JSON".
        let mut bytes = raw(REQUEST, 1, 2, b"{x{x");
        bytes[0..4].copy_from_slice(&11u32.to_be_bytes());
        let frame = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert_eq!(frame.json, Value::Null);
        assert_eq!(frame.id, 1);

        let frame = read_frame(&mut &raw(REQUEST, 2, 0, &[1, 0, 0, 0, 2, 0, 0, 0, 0])[..])
            .unwrap()
            .unwrap();
        assert_eq!(frame.json, json!({}));
    }

    #[test]
    fn a_frame_too_large_to_write_is_refused_but_the_limit_is_inclusive() {
        let error = write_frame(
            &mut Vec::new(),
            &Frame::with_payload(RESULT, 1, json!({}), vec![0u8; MAX_FRAME]),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "frame too large");
        // A payload that fills the frame exactly is written.
        let payload = vec![0u8; MAX_FRAME - 9 - 2];
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Frame::with_payload(RESULT, 1, json!({}), payload.clone()),
        )
        .unwrap();
        assert_eq!(bytes.len(), 4 + MAX_FRAME);
        let frame = read_frame(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(frame.payload.len(), payload.len());
    }

    #[test]
    fn write_frame_flushes_and_encodes_big_endian_headers() {
        struct Flushing(Vec<u8>, bool);
        impl Write for Flushing {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.1 = true;
                Ok(())
            }
        }
        let mut sink = Flushing(Vec::new(), false);
        write_frame(&mut sink, &Frame::new(PING, 0x01020304, json!({}))).unwrap();
        assert!(sink.1, "write_frame flushes");
        let bytes = sink.0;
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            11
        );
        assert_eq!(bytes[4], PING);
        assert_eq!(
            u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            0x01020304
        );
        assert_eq!(
            u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]),
            2
        );
        assert_eq!(&bytes[13..], b"{}");
    }

    #[test]
    fn frame_constants_match_the_protocol_document() {
        assert_eq!(
            (REQUEST, RESULT, ERROR, EVENT, CANCEL, PING),
            (1, 2, 3, 4, 5, 6)
        );
        assert_eq!(MAX_FRAME, 16 * 1024 * 1024);
        assert_eq!(MAX_PAYLOAD, MAX_FRAME - 64 * 1024);
    }
}
