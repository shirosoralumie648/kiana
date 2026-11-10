//! 共享 JS 语义 JSON 层。
//!
//! 三个 crate 边界上的约定都收在这里，避免每个 crate 各写一份、各自偏离：
//! - [`js_num`](mod@js_num)：ECMAScript 数字格式化（`1e+21`、`1e-7`、`-0`、`NaN`/`Infinity`）。
//! - [`js_string`](mod@js_string)：`String(v)` / truthiness / `typeof` / `String.prototype.trim` / JSON 字符串转义。
//! - [`js_object`](mod@js_object)：JS 对象键序（整数键优先）与 `JSON.stringify` 等价输出。
//! - [`js_parse`](mod@js_parse)：`JSON.parse` 与带修复/流式解析的分层入口。
//!
//! 使用纪律（见 `ARCHITECTURE.md` §4.3）：
//! 1. 任何会写到磁盘、发到网络或进入模型上下文的 JSON 都走 [`js_object::stringify`]，
//!    不直接用 `serde_json::to_string`，因为后者既不对数字做 JS 格式化，也不重排整数键。
//! 2. `serde_json` 在本工作区开启 `preserve_order`（`IndexMap` 后端），与 TS 对象字面量
//!    的插入序一一对应；`sort_js_keys` 只用于模拟 JS 引擎自身的键序规则。
//! 3. 数字在 Rust 里的内存表示可以是整数类型，但一旦跨到 JSON 文本就必须经过
//!    [`js_num::format_js_number`]；而从 `i64`/`u64` 结构体字段构造出的 `Value` 在写出前
//!    要先过 [`normalize_numbers`]（`|v| > 2^53` 的整数舍入为 JS 的 f64 值）。
//!
//! # 在系统中的位置
//!
//! 依赖是单向的：本 crate 只依赖 `serde` / `serde_json` / `ryu` / `indexmap` /
//! `thiserror` / `num-bigint` / `num-traits`，不依赖任何其他 `kiana-*` crate，因此位于
//! 工作区依赖图的最底层，任何层都可以引用它而不会形成循环依赖。反向看，工作区中的
//! 大多数 crate 直接依赖它：例如 `kiana-durable` 的 JSONL/SQLite 持久化用它写出每条
//! 记录、`kiana-ai` 用它解析模型响应与工具参数、`kiana-tools` 用它格式化工具输出里的
//! 数字与字符串。
//!
//! 本 crate 只处理 `serde_json::Value`（工作区统一的 JSON 值类型，不在本 crate 定义）：
//! 全部函数都是纯函数——不持有全局状态、不做 IO、不启动线程、没有 `unsafe`。
//! 本文件本身不含逻辑，只声明四个模块并用 `pub use` 把常用入口汇总到 crate 根
//! （`kiana_json::stringify` 与 `kiana_json::js_object::stringify` 是同一个函数）。
//!
//! # 设计原因：为什么不直接包一层 serde_json
//!
//! `serde_json` 与 V8 的差异集中在四处：数字的文本形式（最短往返表示与 `Number::toString`
//! 的指数、负零规则不同，具体对照见 [`js_num`](mod@js_num) 的模块文档）、对象键的枚举序
//! （`preserve_order` 只保证插入序）、错误消息文本、以及 `recursion_limit`（128 层）。
//! 这些差异会体现在写入磁盘的会话文件、发给模型的工具结果和错误日志里，所以解析器与
//! 写出器都按 V8 的行为手写，`serde_json` 只作为 `Value` 容器使用；其 `preserve_order`
//! feature 用来保住 TS 对象字面量的插入序。
//!
//! # 一次典型调用
//!
//! 1. 拿到文本（会话文件的一行、模型响应片段、工具参数）后：严格场景调用 [`parse`]，
//!    可能不完整的流式场景调用 [`parse_streaming`]，得到 `serde_json::Value`。
//! 2. 如果该值由 Rust 结构体的 `i64`/`u64` 字段直接构造，写出前先过 [`normalize_numbers`]
//!    （见上文使用纪律第 3 条）。
//! 3. 需要文本时用 [`js_object::stringify`] / [`js_object::stringify_pretty`] 写出；
//!    需要深比较（数字按 f64 值比较、对象按键名匹配、忽略插入序）时用 [`deep_equal`]。
//!    不要用 `Value` 的 `==` 代替它：`serde_json::Number` 的相等按内部变体
//!    （`PosInt`/`NegInt`/`Float`）比较，因此 `serde_json::from_str("1")` 与
//!    `serde_json::from_str("1.0")`、`from_str("-0")` 与 `from_str("0")` 在 `Value`
//!    下都不相等；而 JS 的深比较是数值语义（`1 === 1.0`、`-0 === 0`）。
//!    注意本 crate 的 [`parse`] 会把整数值字面量（含 `1.0`、`1e2`）统一存成整数表示，
//!    故 `parse("1") == parse("1.0")`（`-0` 例外，仍存为浮点，`parse("-0") != parse("0")`）；
//!    [`deep_equal`] 仍按 f64 数值语义比较。
//!
//! # 边界与失败模式
//!
//! - [`parse`] / [`parse_with_repair`] 以 [`ParseError`] 返回失败；`message` 与 V8 的
//!   `SyntaxError.message` 逐字对齐，由 `tests/parse_v8.rs`、`tests/api_golden.rs` 的
//!   fixture 锁定。
//! - [`parse_streaming`] 不返回 `Result`：最坏情况回退为空对象 `{}`，不会把“解析失败”
//!   暴露给调用方；返回的 `bool` 只区分「严格/修复解析成功」与「partial 回退（含空输入）」，
//!   是 TS 不可观察的补充信息（见 [`js_parse`](mod@js_parse)）。
//! - 两处与 V8 的**被迫偏差**（见 `ARCHITECTURE.md` §8 第 31、32 条）：
//!   `1e400` 这类溢出数字落为 `Value::Null`（`serde_json::Number` 存不下 `Infinity`）；
//!   孤立 UTF-16 代理项被替换为 U+FFFD。对**解析结果**再调用
//!   `String()`/`typeof`/truthiness 时，个别值因此与 TS 不同（见
//!   [`js_string`](mod@js_string) 与 [`js_parse`](mod@js_parse)）。
//! - 深层嵌套：本 crate 的解析、键序重排、写出、深比较都用显式栈，不受 `serde_json`
//!   `recursion_limit` 的限制；唯一残留的深度上限来自 `serde_json::Value` 自身的递归
//!   `Drop`（外部类型的行为，见 [`js_object`](mod@js_object)）。
//!
//! # 阅读顺序
//!
//! 1. [`js_num`](mod@js_num)：数字格式化，被其余模块复用；
//! 2. [`js_string`](mod@js_string)：`String(v)`/truthiness/typeof，依赖 js_num；
//! 3. [`js_object`](mod@js_object)：键序与写出，依赖前两者；
//! 4. [`js_parse`](mod@js_parse)：解析与宽松回退，依赖 js_object 的键序重排；其子模块
//!    `js_parse::partial` 是 `partial-json` 的移植。
//!
//! 每个模块先读模块级文档，再看公开函数；`tests/` 下的 fixture 测试（`api_golden.rs`、
//! `parse_v8.rs`、`parse_streaming.rs` 等）是对应行为的验收基准。根部 `pub use` 只重导出
//! 常用项；`js_parse::repair`、`js_parse::partial` 等仍通过模块路径公开，未在根部重导出。

pub mod js_num;
pub mod js_object;
pub mod js_parse;
pub mod js_string;

pub use js_num::{format_js_number, is_js_integer, parse_js_number, to_radix_string};
pub use js_object::{deep_equal, sort_js_keys, stringify, stringify_pretty};
pub use js_parse::{ParseError, normalize_numbers, parse, parse_streaming, parse_with_repair};
pub use js_string::{JsType, is_truthy, js_quote, js_strict_equal, js_string, js_trim, js_typeof};
