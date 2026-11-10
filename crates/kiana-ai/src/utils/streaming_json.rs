//! `utils/streaming-json.ts`——增量、宽容的 JSON 解析。
//!
//! TS 源共 559 行，是本次移植里最难复刻的部分（`spec/ai-core.md` §2.2.26、§8.2 第 1 条）。
//! [`ToolCallArgumentsStream`] 被 `kiana-agent-core` 的 `proxy.rs` 直接使用，公开形状**已冻结**
//! （`ARCHITECTURE.md` §4.9）；本文件只填实现，并额外导出 TS 里也是公开面的
//! [`StreamingJsonParser`]、[`JsonChange`]、[`JsonPath`]。
//! Rust 侧另需 `PathSegment`（TS 的 `JsonPath` 元素是 `string | number`）以及
//! `ToolCallArgumentsFinish`/`ToolCallArgumentsReplace` 两个返回值类型。
//!
//! 规则摘要（逐条对应 TS 的模块注释）：
//! - `append()` 只读每个片段一次，总代价与文本长度线性相关；
//! - `snapshot()` 返回「迄今读到的文本」的值，快照是普通 JSON 值且**返回后永不改变**：
//!   每次新快照复制仍在「打开路径」上的容器；TS 共享已完成的容器与字符串，Rust 侧
//!   因为 `serde_json::Value` 没有可共享的子节点，改为 `deep_clone` 复制（代价差异见下文）；
//! - 未终止字符串显示已读到的字符；未终止转义被扣留；未终止容器显示已读到的成员/元素；
//!   键或值尚未开始的成员被省略；未终止字面量/数字在构成合法前缀时显示；
//! - malformed 文本宽容读取（保留原始控制字符、非法转义按字面文本、容忍缺失逗号/冒号、
//!   丢弃非法位置的值与未知单词、忽略根值之后的文本），**永不抛错**；
//! - `append_changes()`（TS 的 `append()`）同时返回本次片段的变更序列（`set`/`append`），
//!   嵌套超过 32 层的容器在关闭时作为整体一次 `set` 报告；
//! - 复制 open path 的成本用 credit 机制摊销：`snapshot()` 在 credit 不足时返回**旧快照**。
//!
//! # 与 TS 的两处被迫差异
//!
//! 1. **孤立代理项**：TS 的字符串是 UTF-16，`\ud83d` 可以单独存在；Rust 的 [`String`] 不行。
//!    解析器内部用 `Vec<u16>` 累积字符串，跨片段的代理对能正确合并，但一个**始终**没有配对的
//!    代理项在快照里显示为 U+FFFD（`ARCHITECTURE.md` §8 第 31/32 条）。
//! 2. **非有限数**：`Number("1e400")` 在 JS 里是 `Infinity`，`serde_json::Number` 存不下，
//!    因此快照里落为 `null`（与 `kiana-json` 的 `js_parse` 同一处理，§8 第 32 条）。
//!
//! # 数据结构与执行流程
//!
//! [`StreamingJsonParser`] 是一台单遍状态机，状态集中在四个字段上：
//!
//! - `stack: Vec<Frame>`：每个打开的 `{`/`[` 一层。`Frame::container` 是**工作副本**，
//!   字符边读边写入它，但它永远不会出现在快照或变更里；`Frame::phase` 记录对象成员读到
//!   哪一步（`Key`/`Colon`/`Value`/`Comma`），数组只会用到 `Value`/`Comma`。
//! - `string: Option<OpenString>`：当前未终止的字符串（键或值）。已读文本、未完成的转义、
//!   等待配对的高代理项都挂在这里。
//! - `scalar: String` + `scalar_slot`：当前未终止的数字/字面量 token 及其目标位置。
//! - `state: Mutex<SnapshotState>`：快照缓存、credit、dirty、revision。快照的公开签名是
//!   `&self`，因此可变状态放在锁里。
//!
//! 一次 `append_changes(text)` 的处理顺序：
//!
//! 1. 增加 credit、清空上一片段的变更、把打开字符串的 `change` 指针复位（片段边界处不再
//!    原地续写上一段的变更对象）；
//! 2. 循环消费 `text`：有打开字符串走 `read_string`，有未终止标量走 `read_scalar`，否则
//!    取一个字符交给 `structural`；
//! 3. `structural` 负责括号压栈/弹栈、字符串开启、标量开启，并通过 `value_slot` 判断当前
//!    位置是否接受值（非法位置返回 `None`，读入后丢弃）；
//! 4. 值完成时由 `place`/`place_existing` 写入工作副本并记账，变更通过 `report` 追加到
//!    `changes`。
//!
//! 快照是另一条独立流程：`snapshot_shared()` 先用 `path_cost` 与 credit 判断是否值得复制；
//! 可以复制时 `copy_open_path` 从**最内层**向上，为每个打开容器做一份副本，并把下一层的
//! 副本放进父副本；未终止字符串/标量只在最内层副本里落位。
//!
//! # 关键不变量
//!
//! - 快照不可变：返回的是 `Arc<Value>`，之后的 `append` 不会修改已返回的值。
//! - `path_cost` 恒等于所有打开容器的 `size + 1` 之和（`open_container`/`place`/
//!   `place_open` 增加，`close_container` 扣除这一层）。它是「复制 open path」的记账值。
//! - 变更里的值都是独立副本：容器在关闭时才 `deep_clone` 上报，字符串在 `Set`/`Append`
//!   里保存自己的 `String`，因此调用方可以长期持有变更而不受后续解析影响。
//! - `Frame::container` 从不外借：快照与变更拿到的是独立值（复制或移动），闭合后工作副本
//!   才移入父容器。
//!
//! # 代价模型与 TS 的差异
//!
//! credit 按「每次 append 一个常量 + 每个 UTF-16 code unit 一个常量」增长，
//! `snapshot_shared` 按 `path_cost` 扣除；credit 不足时返回旧快照，因此大容器打开期间预览
//! 会暂时落后于文本（TS 的 `streaming-json.test.ts` 断言了这一可观察行为）。
//!
//! TS 的快照只做**浅拷贝**：已完成的成员共享引用，复制一层容器的成本是它的成员数。本实现
//! 受 `serde_json::Value` 限制，`copy_container_skipping` 对每个已完成成员调用 `deep_clone`，
//! 因此一次快照的真实成本是 open path 上已完成成员的**总字节数**（外加未终止字符串已读到的
//! 文本），而 credit 仍只按成员数扣。成员很大且频繁取快照时，实际复制量可能明显高于 credit
//! 记账。需要 O(1) 句柄时用 [`StreamingJsonParser::snapshot_shared`]，不要在热路径反复调用
//! [`StreamingJsonParser::snapshot`]——后者每次都深拷贝整个值。
//!
//! # 并发与错误处理
//!
//! - `append_changes` 需要 `&mut self`，同一解析器上不会有两个写入者；`snapshot_shared`
//!   只借 `&self`，用 `Mutex` 串行化缓存访问。对 `snapshot_shared`、`mark_dirty` 与
//!   `append_changes` 的 credit 更新这几条路径，持锁范围仅限 `SnapshotState` 的读写与
//!   `copy_open_path`；`impl Clone for StreamingJsonParser` 则是例外：它在
//!   `self.state.lock()` 之后构造整个结构体，guard 存活到函数末尾，期间执行 `root`、逐帧
//!   `container` 与 `changes` 的深拷贝——克隆整份解析器状态期间一直持锁。锁中毒时用
//!   `into_inner()` 继续：锁里只有缓存与计数器，解析器的其余字段仍由 `&mut self` 独占。
//! - 解析没有错误返回值，也不会因为文本格式而 panic：非法输入只会表现为「值被丢弃 / 文本被
//!   忽略」。需要判断完整文本是否合法时，用 `ToolCallArgumentsStream::finish()`，或直接调用
//!   本模块的 `parse_tool_call_arguments`（`finish()` 只是它的薄包装；`StreamingJsonParser`
//!   本身没有 `finish()` 方法）。
//! - 深嵌套的 `Value` 大多由本文件显式迭代释放（`drop_json`/`drop_changes`），但并非全部：
//!   `Clone` 实现、`ToolCallArgumentsStream` 的隐式 `Drop`、`place`/`place_existing`
//!   （后者经 `set_slot`）覆盖已有对象成员或数组元素时被替换的旧值、以及 `place_existing`
//!   在 `slot` 为 `None` 时直接丢弃的传入值，仍走 `serde_json` 的递归路径。例如 `{"a":` +
//!   深嵌套数组 + `,"a":1}` 会在重复键覆盖处递归析构旧数组，深参数下需要调用方自行注意栈深度。
//!
//! # 测试与阅读顺序
//!
//! `crates/kiana-ai/tests/streaming_json.rs` 用 fixture（TS 逐片记录的快照与变更）与原生
//! 差分模糊测试覆盖本文件；深度与线性时间用例也在同一文件里。建议按
//! `ToolCallArgumentsStream` → `append_changes` 主循环 → `structural`/`value_slot` →
//! `place*` → `copy_open_path` 的顺序阅读，转义与数字辅助函数最后看。

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::types::JsonObject;

// ---------------------------------------------------------------------------
// 冻结的公开类型（形状不得改，见 ARCHITECTURE.md §4.9）
// ---------------------------------------------------------------------------

/// `ToolCallArgumentsStream`（`streaming-json.ts:452`）：一次流式工具调用的参数。
///
/// 语义（对应 TS 的文档注释）：
/// - `text`：到目前为止收到的**原始**参数文本；
/// - `append(delta)`：追加一段文本，返回 partial arguments 的**值副本**：快照对象身份变化时
///   返回新的值副本（内容可能相同——对象/数组快照按 `Arc` 身份比较，重建即算变化、`revision`
///   递增；原始值按 JS 严格相等比较，内容相同则不算变化），未变化时内容相等。需要按对象身份
///   判断「是否变了」请用 [`Self::partial`]（`Arc` + `revision`），不要对返回的 `JsonObject`
///   做身份比较（TS 返回同一个对象，Rust 的 `JsonObject` 是值类型）；
/// - `replace(text)`：当 `text` 以当前文本为前缀时返回缺失的后缀作为 `delta`（可能是空串），
///   否则**从空重来**（`delta: None`）；
/// - `finish()`：对完整文本做一次完整解析，格式错误时 `arguments` 为 `{}` 且
///   `arguments_error` 为解析错误文本。
///
/// 性能要求（回归 #9265）：520 KB 参数、16,000 个 `append` 必须 < 10 秒，即**线性时间**；
/// 参数文本永不挂在 content block 上。
///
/// 注意「线性时间」描述的是解析器本身：`append` 返回的 `JsonObject` 需要从快照逐成员搬移
/// （TS 直接把快照对象当作 `#arguments`），每次参数变化都是 O(当前参数总字节数)，见
/// `update_partial`。参数越大、变化越频繁，这部分复制越贵。
#[derive(Debug, Default, Clone)]
pub struct ToolCallArgumentsStream {
    /// `#text`
    text: String,
    /// `#parser`
    parser: StreamingJsonParser,
    /// `#snapshot`（共享句柄；TS 里是同一个对象引用）
    snapshot: Option<std::sync::Arc<serde_json::Value>>,
    /// `#arguments`（可读副本，`arguments()` 返回）。
    arguments: JsonObject,
    /// `#arguments` 的共享句柄：只在参数**变化**时替换，未变时 [`Self::partial`] 反复返回同一个
    /// `Arc`。`append`/`replace` 更新 `arguments` 后必须同时执行
    /// `arguments_handle = Arc::new(arguments.clone())` 并 `revision += 1`——这是复刻 TS
    /// `if (snapshot === this.#snapshot)` 身份语义的依据（`tests/streaming_json.rs` 用
    /// `Arc::ptr_eq` 锁定该行为）。
    arguments_handle: std::sync::Arc<JsonObject>,
    /// 参数每变化一次 +1。
    revision: u64,
}

/// `replace(text)` 的返回值。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallArgumentsReplace {
    /// 缺失的后缀；`None` 表示文本不连续、已从空重来。
    pub delta: Option<String>,
    /// 处理后的 partial arguments（值副本；需要身份判断时用 [`PartialArguments`]）。
    pub arguments: JsonObject,
}

/// `append`/`replace` 期间可见的 partial arguments 的版本证据。
///
/// TS 靠对象身份判断「是否变了」；Rust 侧同时提供 `Arc` 句柄与单调递增的 `revision`，
/// 让需要身份比较的调用方复刻 `if (snapshot === this.#snapshot)`。判定规则：`Arc` 指针相同
/// 即未变；指针变化时 `revision` 必增 1。
///
/// 注意派生的 `PartialEq` 比较的是 `Arc` 指向的**内容**（以及 `revision`），不是指针；
/// 判断「是否同一个句柄」必须用 `std::sync::Arc::ptr_eq`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PartialArguments {
    /// 当前 partial 参数；与上一次相比未变化时是同一个 `Arc`。
    pub arguments: std::sync::Arc<JsonObject>,
    /// 每次返回**新对象**时 +1；返回同一对象时不变。
    pub revision: u64,
}

impl ToolCallArgumentsStream {
    /// 新建（TS 的 `new ToolCallArgumentsStream()`）。
    pub fn new() -> Self {
        Self::default()
    }

    /// `get text`。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `append(delta)`：追加参数文本并返回 partial arguments。
    ///
    /// 文本无条件追加到 [`Self::text`]，再交给增量解析器；格式问题不会在这里报错。
    /// 返回的是 `arguments` 的**值副本**，需要身份判断用 [`Self::partial`]。每次调用都会
    /// 询问一次解析器快照，若 credit 不足会暂时返回旧参数（预览滞后，见模块注释）。
    pub fn append(&mut self, delta: &str) -> JsonObject {
        self.text.push_str(delta);
        self.parser.append(delta);
        self.update_partial()
    }

    /// `replace(text)`：用提供方的完整参数文本覆盖当前状态。
    ///
    /// - 若 `text` 以当前 [`Self::text`] 为前缀：只把缺失的后缀交给解析器，`delta` 为该后缀
    ///   （可能是空串），`arguments` 是追加后的 partial；
    /// - 否则：清空已收文本、丢弃旧解析器（连同它的 credit、快照缓存与 work 状态），从空重来，
    ///   `delta` 为 `None`。
    ///
    /// 前缀判断按字节（`str::strip_prefix`）。文本始终是合法 UTF-8，因此不会把多字节字符
    /// 拆开。解析错误同样不会在这里出现，最终结论看 [`Self::finish`]。
    pub fn replace(&mut self, text: &str) -> ToolCallArgumentsReplace {
        if let Some(delta) = text.strip_prefix(self.text.as_str()) {
            let delta = delta.to_string();
            let arguments = if delta.is_empty() {
                self.current_partial()
            } else {
                self.append(&delta)
            };
            return ToolCallArgumentsReplace {
                delta: Some(delta),
                arguments,
            };
        }
        self.text.clear();
        self.parser = StreamingJsonParser::new();
        ToolCallArgumentsReplace {
            delta: None,
            arguments: self.append(text),
        }
    }

    /// `finish()`：完整文本的一次完整解析。
    ///
    /// 不读取增量解析器，而是把 [`Self::text`] 整段交给 [`parse_tool_call_arguments`]。这是
    /// 唯一会报告格式错误的入口：空/纯空白文本返回空参数且无错误；根值不是对象或修复后
    /// 仍无法解析时，`arguments` 为空对象、`arguments_error` 为错误文本。
    pub fn finish(&self) -> ToolCallArgumentsFinish {
        parse_tool_call_arguments(&self.text)
    }

    /// 只读访问内部的增量解析器（`#parser`）：`snapshot()`/`append` 的语义由它承载。
    ///
    /// 返回的引用可以调用 [`StreamingJsonParser::snapshot_shared`]，但那会消耗 credit 并
    /// 更新解析器的快照缓存，可能让后续 partial 更新滞后。除调试与差分测试外不要在流式
    /// 过程中调用它。
    pub fn parser(&self) -> &StreamingJsonParser {
        &self.parser
    }

    /// 最近一次的 partial arguments（`#arguments`）。
    ///
    /// 与最近一次 [`Self::partial`] 的内容一致；每次参数变化时整体替换（见
    /// `arguments_handle`），调用方不应依赖跨调用的引用稳定。
    pub fn arguments(&self) -> &JsonObject {
        &self.arguments
    }

    /// 最近一次的**原始解析快照**（`#snapshot`），即 `arguments` 的未退化来源。
    ///
    /// TS 用它做对象身份比较（`if (snapshot === this.#snapshot) return`）；Rust 侧等价的公开面是
    /// [`Self::partial`] 的 `Arc` 句柄，这里只暴露快照本身供调试与测试断言。
    ///
    /// `None` 表示解析器还没有产出过快照（文本尚未形成值）。返回的引用借自内部 `Arc`，
    /// 与 `&self` 同生命周期；下一次 `append` 需要 `&mut self`，因此持有期间不会被替换。
    pub fn snapshot(&self) -> Option<&serde_json::Value> {
        self.snapshot.as_deref()
    }

    /// 带版本号的 partial 视图：把 TS 的对象身份比较显式化。
    ///
    /// 返回的 `Arc` 在参数未变化时与上一次**是同一个指针**（TS 的
    /// `snapshot === this.#snapshot` 语义）；`revision` 与之一致地只在变化时递增。判断
    /// 「是否有新 partial」时比较 `std::sync::Arc::ptr_eq` 或 `revision` 即可。
    /// `crates/kiana-ai/tests/streaming_json.rs` 用 `Arc::ptr_eq` 锁定了这些行为。
    ///
    /// 说明：当前 `kiana-agent-core::proxy` 直接把 [`Self::append`] 的返回值写进 content
    /// block，并没有使用这个视图；`partial()` 是 `ARCHITECTURE.md` §4.9 冻结的公开面，
    /// 供需要身份比较的调用方使用。
    pub fn partial(&self) -> PartialArguments {
        PartialArguments {
            arguments: std::sync::Arc::clone(&self.arguments_handle),
            revision: self.revision,
        }
    }

    /// `replace` 空 delta 的快速路径：直接返回缓存参数（不改 `revision`）。
    ///
    /// 对应 TS 的 `delta === "" ? this.#partial() : this.append(delta)` 分支。没有新文本
    /// 就不会产生新 credit，也不会改变 `path_cost`，因此这里不需要再询问解析器；返回的是
    /// `arguments` 的值副本。
    fn current_partial(&self) -> JsonObject {
        self.arguments.clone()
    }

    /// 读一次解析器快照；与上次**严格相等**（JS `===`）时直接返回缓存，否则重算 arguments。
    ///
    /// Rust 侧比 TS 多一次复制：TS 把快照对象直接当作 `#arguments`，而 `JsonObject`
    /// （`IndexMap`）与 `serde_json::Value` 是不同类型，这里用 `deep_clone` 逐成员搬移，
    /// 因此每次参数变化都是 O(当前参数总字节数)。返回值同样是值副本。
    fn update_partial(&mut self) -> JsonObject {
        // 每次都问解析器要快照，再按严格相等（`snapshots_strict_equal`）判断是否变了。
        // JS 的严格相等对对象/数组是引用比较，对原始值（undefined/null/布尔/字符串/数字）是
        // 值比较——`NaN !== NaN`，`-0 === 0`。
        let snapshot = self.parser.snapshot_shared();
        if snapshots_strict_equal(self.snapshot.as_ref(), snapshot.as_ref()) {
            return self.arguments.clone();
        }
        // Tool arguments 是对象；其它 partial 值显示为「还没有参数」。
        self.arguments = match snapshot.as_deref() {
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| (k.clone(), deep_clone(v)))
                .collect(),
            _ => JsonObject::new(),
        };
        self.snapshot = snapshot;
        // 句柄与 revision 必须一起更新：只换句柄不递增会让调用方以为没变，只递增不换句柄
        // 会让 `Arc::ptr_eq` 与版本号矛盾（见 `arguments_handle` 的说明）。
        self.arguments_handle = std::sync::Arc::new(self.arguments.clone());
        self.revision += 1;
        self.arguments.clone()
    }
}

/// TS 的 `snapshot === this.#snapshot`：`undefined` 与任何值都不相等（两边都 `undefined` 时相等），
/// 原始值按 JS 严格相等比较，对象/数组按引用（[`std::sync::Arc::ptr_eq`]）比较。
fn snapshots_strict_equal(
    previous: Option<&std::sync::Arc<Value>>,
    next: Option<&std::sync::Arc<Value>>,
) -> bool {
    match (previous, next) {
        (None, None) => true,
        (Some(previous), Some(next)) => match (previous.as_ref(), next.as_ref()) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(previous), Value::Bool(next)) => previous == next,
            (Value::String(previous), Value::String(next)) => previous == next,
            (Value::Number(previous), Value::Number(next)) => {
                match (previous.as_f64(), next.as_f64()) {
                    // `NaN !== NaN`；`-0 === 0`（f64 的 `==` 正好如此）。
                    (Some(previous), Some(next)) => previous == next,
                    _ => false,
                }
            }
            // 对象/数组：只有同一个快照对象才是 `===`。
            _ => std::sync::Arc::ptr_eq(previous, next),
        },
        _ => false,
    }
}

/// `finish()` 的返回值：`Pick<ToolCall, "arguments" | "argumentsError">`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallArgumentsFinish {
    /// 完整文本解析出的参数对象；解析失败时为空对象（与 `arguments_error` 同时出现）。
    pub arguments: JsonObject,
    /// `argumentsError`：完整文本不是合法 JSON 对象时的错误文本，逐字来自
    /// `parseToolCallArguments`。
    pub arguments_error: Option<String>,
}

/// `parseToolCallArguments(text)`：完整文本 → `{arguments, argumentsError}`。
///
/// 空/纯空白文本返回空参数且 `arguments_error` 为 `None`；根值不是对象（数组、`null`、
/// 标量）或修复后仍无法解析时，`arguments` 为空对象，`arguments_error` 是
/// `json_parse::parse_tool_call_arguments` 生成的文本（错误消息 + `"\n\nReceived arguments:\n"`
/// + 原文）。注意不要与 [`crate::utils::json_parse::parse_tool_call_arguments`] 混淆：那个
/// 入口接收 `Option<&str>` 并返回 `ParsedToolCallArguments`，本函数只是它在本模块的
/// 公开薄包装（TS 的 `streaming-json.ts` 也从 `json-parse.ts` 引入同名函数）。
pub fn parse_tool_call_arguments(text: &str) -> ToolCallArgumentsFinish {
    let parsed = crate::utils::json_parse::parse_tool_call_arguments(Some(text));
    ToolCallArgumentsFinish {
        arguments: parsed.arguments,
        arguments_error: parsed.arguments_error,
    }
}

// ---------------------------------------------------------------------------
// 变更类型（TS 里同样导出）
// ---------------------------------------------------------------------------

/// 路径的一段：对象成员键或数组下标。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSegment {
    /// 对象成员键。
    Key(String),
    /// 数组下标。
    Index(usize),
}

/// 从根值出发的路径（成员键与数组下标）。
pub type JsonPath = Vec<PathSegment>;

/// 一段文本对值造成的变更；见模块注释。
#[derive(Debug, Clone, PartialEq)]
pub enum JsonChange {
    /// 键、数组元素或**已完成的值**出现（新字符串以首段文本 set 一次；容器以空容器 set）。
    Set {
        /// 变更位置。
        path: JsonPath,
        /// 新值。
        value: Value,
    },
    /// 片段**扩展了**一个打开的字符串。
    Append {
        /// 变更位置。
        path: JsonPath,
        /// 追加的文本。
        text: String,
    },
}

/// 值在父容器中的位置；非法位置为 `None`（读入但丢弃）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    /// 根值，没有父容器。
    Root,
    /// 父对象的成员键。
    Member(String),
    /// 父数组的下标。
    Element(usize),
}

/// 一个打开的对象或数组。
#[derive(Debug)]
struct Frame {
    /// 工作容器（永不直接交给快照；快照拿副本）。
    container: Value,
    /// 它在父容器中的位置。
    slot: Option<Slot>,
    /// 它所在的位置路径（当内部变更被报告时）。
    path: Option<JsonPath>,
    /// 内部变更是否被报告；为假时整个值在关闭时报告一次。
    reports_inside: bool,
    /// 成员或元素数（复制成本）。
    size: usize,
    /// 对象：最近读到的键，成员值放在这里。
    key: Option<String>,
    /// 对象成员的读取阶段；数组只使用 `Value`/`Comma`。
    phase: Phase,
}

/// 对象成员的读取阶段（数组只使用 `Value`/`Comma`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 期待下一个成员键。
    Key,
    /// 键已读完，等待冒号；缺失冒号时 `value_slot` 也接受值。
    Colon,
    /// 期待成员值或数组元素。
    Value,
    /// 值已结束，等待逗号（或闭合符）。
    Comma,
}

/// 字符串是键还是值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringTarget {
    /// 对象成员键：只写入 `Frame::key`，不产生值变更。
    Key,
    /// 值字符串：进入快照，并产生 `Set`/`Append` 变更。
    Value,
}

/// 一个未终止的字符串。
#[derive(Debug)]
struct OpenString {
    target: StringTarget,
    slot: Option<Slot>,
    /// 已读到的文本。跨片段的代理对按 `from_utf16_lossy` 的规则合并：未配对的高代理项先
    /// 停在 `pending_high`，等下一个 code unit 决定是配对还是替换成 U+FFFD。
    text: String,
    /// 尚未配对的高代理项；快照/结束时按 `from_utf16_lossy` 显示为 U+FFFD。
    pending_high: Option<u16>,
    /// 已读到的转义：`""` 表示无，否则是一个反斜杠加最多五个字符。
    escape: String,
    /// 值字符串的位置（当变更被报告时）。
    path: Option<JsonPath>,
    /// 这个字符串正在扩展的变更在 `changes` 里的下标（当它仍是本片段最后一条变更时）。
    change: Option<usize>,
}

/// 未终止标量（数字/字面量）在快照里的待定值。
#[derive(Debug)]
struct Pending {
    /// 这个未终止标量应放入的位置（根或父容器的槽位）。
    slot: Slot,
    /// 按当前 token 算出的可见值（字面量前缀或数字）。
    value: Value,
}

/// 快照相关的可变状态（`snapshot()` 的冻结签名是 `&self`，因此用 `Mutex` 提供内部可变性）。
///
/// 缓存的快照用 `Arc` 保存：TS 返回的是同一个对象引用，Rust 侧的公开签名要求返回值，
/// 因此只有真正要交出所有权时才复制。[`ToolCallArgumentsStream`] 与
/// `tests/streaming_json.rs` 用 [`StreamingJsonParser::snapshot_shared`] 拿 O(1) 的句柄
/// （这也是 200k 层嵌套用例不必在每次快照后再复制一整棵树的前提）。
#[derive(Debug, Default)]
struct SnapshotState {
    /// 自上次快照以来是否有文本可能改变值；为假时 `snapshot_shared` 直接返回缓存。
    dirty: bool,
    /// 最近一次成功生成的快照。因 credit 不足，它可能落后于已读文本。
    snapshot: Option<std::sync::Arc<Value>>,
    /// 剩余复制 credit；`append_changes` 增加，`snapshot_shared` 按 `path_cost` 扣除。
    credit: u64,
    /// 最近一次重建快照时取的全局序号（见 `SNAPSHOT_SEQ`）。
    revision: u64,
}

impl Drop for SnapshotState {
    fn drop(&mut self) {
        // `Value` 的 `Drop` 是递归的（外部类型，深值会爆栈）；这里显式拆开。
        // 若快照仍被外部 `Arc` 共享，`try_unwrap` 会失败，释放就落到最后一个持有者
        // （可能是调用方），本 impl 无法代为迭代释放。
        if let Some(arc) = self.snapshot.take()
            && let Ok(value) = std::sync::Arc::try_unwrap(arc)
        {
            drop_json(value);
        }
    }
}

/// 迭代式深复制（`Value::clone` 是递归的，深嵌套会爆栈；见 `kiana_json::js_object` 的模块注释）。
///
/// 快照复制与变更上报都依赖它；返回的新值与入参不共享任何节点，因此调用方可以安全地长期
/// 持有。
fn deep_clone(value: &Value) -> Value {
    enum Task<'a> {
        Visit(&'a Value),
        FinishArray(usize),
        FinishObject(Vec<String>),
    }
    let mut tasks = vec![Task::Visit(value)];
    let mut done: Vec<Value> = Vec::new();
    // 显式栈：Visit 展开子节点（逆序入栈），Finish* 按子节点数从 done 尾部取回并重建容器。
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(value) => match value {
                Value::Array(items) => {
                    tasks.push(Task::FinishArray(items.len()));
                    for item in items.iter().rev() {
                        tasks.push(Task::Visit(item));
                    }
                }
                Value::Object(members) => {
                    tasks.push(Task::FinishObject(members.keys().cloned().collect()));
                    for (_, item) in members.iter().rev() {
                        tasks.push(Task::Visit(item));
                    }
                }
                other => done.push(other.clone()),
            },
            Task::FinishArray(len) => {
                let start = done.len() - len;
                let items: Vec<Value> = done.drain(start..).collect();
                done.push(Value::Array(items));
            }
            Task::FinishObject(keys) => {
                let start = done.len() - keys.len();
                let items: Vec<Value> = done.drain(start..).collect();
                let mut members = Map::with_capacity(keys.len());
                for (key, item) in keys.into_iter().zip(items) {
                    members.insert(key, item);
                }
                done.push(Value::Object(members));
            }
        }
    }
    done.pop().expect("one value")
}

/// 迭代式释放（`Value::Drop` 是递归的）。
fn drop_json(value: Value) {
    let mut work = vec![value];
    while let Some(value) = work.pop() {
        match value {
            Value::Array(items) => work.extend(items),
            Value::Object(members) => work.extend(members.into_values()),
            _ => {}
        }
    }
}

/// 迭代释放一组变更里的值（`Value` 的 `Drop` 是递归的）。
fn drop_changes(changes: Vec<JsonChange>) {
    for change in changes {
        if let JsonChange::Set { value, .. } = change {
            drop_json(value);
        }
    }
}

/// 替换缓存的快照；旧的若已无人共享就迭代释放。
fn replace_snapshot(slot: &mut Option<std::sync::Arc<Value>>, new: Option<std::sync::Arc<Value>>) {
    if let Some(old) = slot.take()
        && let Ok(value) = std::sync::Arc::try_unwrap(old)
    {
        drop_json(value);
    }
    *slot = new;
}

/// 全局快照版本号：每次重建快照就取一个新值，保证不同解析器之间也不会撞号。
/// 只用于区分代际，不参与同步，因此 `Relaxed` 足够。
static SNAPSHOT_SEQ: AtomicU64 = AtomicU64::new(1);

// 变更报告的深度上限：超过它的容器整体报告。
// 判定用 `path.len()`（从根开始的段数）：达到 32 段的容器不再报告内部变更，关闭时整体上报一次。
const MAX_REPORTED_DEPTH: usize = 32;
// 每次 append 与每个追加字符赚取的复制 credit；下限保证小文档的每次快照都是新的。
const CREDIT_PER_APPEND: u64 = 256;
const CREDIT_PER_CHARACTER: u64 = 8;
// credit 的长度单位是 UTF-16 code unit（见 `append_changes` 的 `units`），与 JS 的
// `text.length` 一致；若按 UTF-8 字节计，含非 ASCII 字符的片段会与 TS 拿到不同的 credit。

// 未终止数字超过这个长度就不显示，保证 `snapshot()` 便宜。
// 长度按 UTF-16 code unit 计（`pending_scalar`），与 TS 的 `token.length` 对齐。
const MAX_PENDING_SCALAR_LENGTH: usize = 64;

/// 完整数字 token 的语法（TS 源：`NUMBER` 常量）；编译结果进程内复用。
///
/// 只匹配合法 JSON 数字：前导零、`+1`、`.5`、`1.`、残缺指数都不匹配，这类 token 会被
/// `scalar_value` 丢弃。
fn number_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$").expect("valid regex")
    })
}

/// 匹配「指数已开始但没有数字」的 token（如 `1.5e+`），捕获组 1 是尾数。
///
/// `pending_scalar_value` 用它把残缺指数忽略掉，与 TS 的
/// `NUMBER_WITHOUT_EXPONENT_DIGITS` 行为一致：未终止数字在指数未成形时显示尾数的值。
fn number_without_exponent_digits_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$").expect("valid regex")
    })
}

/// JS `Number(token)` 到 `serde_json::Value`；非有限数只能退化为 `null`（§8 第 32 条）。
///
/// JS 只有 f64 一个数字类型，`1`、`1.0`、`1e2` 是同一个值；serde 却区分整数与浮点表示，
/// 且两者的 `PartialEq` 不相等。因此整数值（2^53 以内、且不是 `-0`）按整数存，与
/// `kiana_json::js_parse` 的 `integer_number` 一致。
fn number_value(token: &str) -> Option<Value> {
    let value: f64 = token.parse().ok()?;
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        // `-0` 保留 f64 以留下符号位。
        if value != 0.0 || !value.is_sign_negative() {
            // 2^53 界限保证 `as i64`/`as u64` 往返不丢精度；负数走 i64，其余走 u64。
            let number = if value < 0.0 {
                serde_json::Number::from(value as i64)
            } else {
                serde_json::Number::from(value as u64)
            };
            return Some(Value::Number(number));
        }
    }
    Some(match serde_json::Number::from_f64(value) {
        Some(number) => Value::Number(number),
        None => Value::Null,
    })
}

/// `scalarValue(token)`：三个字面量或合法 JSON 数字；其它一律 `None`（丢弃）。
fn scalar_value(token: &str) -> Option<Value> {
    match token {
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "null" => return Some(Value::Null),
        _ => {}
    }
    if number_regex().is_match(token) {
        return number_value(token);
    }
    None
}

/// `pendingScalarValue(token)`：未终止标量显示的值。
///
/// 先看字面量前缀（`t`→`true` 等），再看完整数字，最后处理「指数已开始但没有数字」的
/// 残缺数字（回退到尾数）；三者都不匹配时返回 `None`，该 token 在结束前不显示。
fn pending_scalar_value(token: &str) -> Option<Value> {
    for (literal, value) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if literal.starts_with(token) {
            return Some(value);
        }
    }
    if number_regex().is_match(token) {
        return number_value(token);
    }
    let mantissa = number_without_exponent_digits_regex().captures(token)?;
    number_value(mantissa.get(1)?.as_str())
}

/// 分隔符：标量在此结束。
/// 引号也算分隔符：`123"abc"` 会在引号处结束数字，随后引号开启一个字符串。
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"'
    )
}

fn is_hex_digit(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

/// 把一个值放进容器副本的指定位置。
///
/// 数组下标小于长度时覆盖，否则追加（快照复制时打开的元素总在末尾）；对象按键插入，
/// 已存在的键保留第一次插入的位置（`IndexMap` 语义，对应 JS 重复键保位）。
fn set_slot(container: &mut Value, slot: &Slot, value: Value) {
    match slot {
        Slot::Element(index) => {
            if let Value::Array(elements) = container {
                if *index < elements.len() {
                    elements[*index] = value;
                } else {
                    elements.push(value);
                }
            }
        }
        Slot::Member(key) => {
            if let Value::Object(members) = container {
                members.insert(key.clone(), value);
            }
        }
        Slot::Root => {}
    }
}

// ---------------------------------------------------------------------------
// StreamingJsonParser
// ---------------------------------------------------------------------------

/// 增量、宽容的 JSON 解析器；规则与成本见模块注释。
///
/// 状态机逐方法对应 TS 的 `StreamingJsonParser`（`streaming-json.ts`）：外层的
/// `append`/`snapshot` 是公开面，其余 `read_*`/`place_*`/`value_*` 是内部步骤。
#[derive(Debug)]
pub struct StreamingJsonParser {
    /// 已接收的原始文本（与 TS 的 `ToolCallArgumentsStream#text` 不同，这里保留一份便于调试；
    /// 解析本身是流式的，不依赖它）。
    text: String,
    /// 已完成的根值；标量在 `end_scalar` 写入，容器在闭合时移入。
    root: Option<Value>,
    /// 根值已结束：之后的文本全部忽略（TS 的「根值之后的文本被忽略」）。
    done: bool,
    /// 打开容器的栈，栈顶是最内层；每层的 `container` 是工作副本。
    stack: Vec<Frame>,
    /// 当前未终止的字符串（键或值）；为 `None` 时字符串外的字符才交给 `structural`。
    string: Option<OpenString>,
    /// 当前未终止的标量 token（数字或字面量）。
    scalar: String,
    scalar_slot: Option<Slot>,
    /// open path 上 `size + 1` 之和：一次新快照要复制的量。
    path_cost: u64,
    /// 快照缓存与 credit；`snapshot_shared` 只借 `&self`，所以用 `Mutex` 提供内部可变性。
    state: Mutex<SnapshotState>,
    /// 本片段产生的变更。
    changes: Vec<JsonChange>,
}

impl Default for StreamingJsonParser {
    fn default() -> Self {
        StreamingJsonParser {
            text: String::new(),
            root: None,
            done: false,
            stack: Vec::new(),
            string: None,
            scalar: String::new(),
            scalar_slot: None,
            path_cost: 0,
            state: Mutex::new(SnapshotState::default()),
            changes: Vec::new(),
        }
    }
}

impl Drop for StreamingJsonParser {
    fn drop(&mut self) {
        // `Value` 的 `Drop` 是递归的：深嵌套的值会爆栈，这里拆开逐层释放。
        if let Some(root) = self.root.take() {
            drop_json(root);
        }
        for frame in self.stack.drain(..) {
            drop_json(frame.container);
        }
        drop_changes(std::mem::take(&mut self.changes));
    }
}

// 注意：这里直接 `Value::clone`/`JsonChange::clone`，二者都是递归实现；与上面的 `Drop`
// （`impl Drop for StreamingJsonParser`）不同，深嵌套文档在 Clone 时仍可能栈溢出。复制深层
// 解析器状态前需自行评估栈深度。
impl Clone for StreamingJsonParser {
    fn clone(&self) -> Self {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        StreamingJsonParser {
            text: self.text.clone(),
            root: self.root.clone(),
            done: self.done,
            stack: self
                .stack
                .iter()
                .map(|frame| Frame {
                    container: frame.container.clone(),
                    slot: frame.slot.clone(),
                    path: frame.path.clone(),
                    reports_inside: frame.reports_inside,
                    size: frame.size,
                    key: frame.key.clone(),
                    phase: frame.phase,
                })
                .collect(),
            string: self.string.as_ref().map(|open| OpenString {
                target: open.target,
                slot: open.slot.clone(),
                text: open.text.clone(),
                pending_high: open.pending_high,
                escape: open.escape.clone(),
                path: open.path.clone(),
                change: open.change,
            }),
            scalar: self.scalar.clone(),
            scalar_slot: self.scalar_slot.clone(),
            path_cost: self.path_cost,
            state: Mutex::new(SnapshotState {
                dirty: state.dirty,
                snapshot: state.snapshot.clone(),
                credit: state.credit,
                revision: state.revision,
            }),
            changes: self.changes.clone(),
        }
    }
}

impl StreamingJsonParser {
    /// `new StreamingJsonParser()`。
    pub fn new() -> Self {
        Self::default()
    }

    /// `append(text)`：读入下一段文本（变更序列被丢弃；需要变更时用
    /// [`Self::append_changes`]）。
    ///
    /// 代价与 `text` 长度线性相关（不含快照复制）。没有错误返回值：格式问题按宽容规则
    /// 读入或丢弃，完整文本的合法性要到 `parse_tool_call_arguments` 才能判定。
    pub fn append(&mut self, text: &str) {
        let changes = self.append_changes(text);
        drop_changes(changes);
    }

    /// `append(text)`：读入下一段文本，按顺序返回它造成的变更。
    ///
    /// TS 的 `append()` 就是这个签名（返回 `readonly JsonChange[]`）；Rust 的冻结签名不接受
    /// 返回值，因此变更序列单独暴露在这个方法上。
    pub fn append_changes(&mut self, text: &str) -> Vec<JsonChange> {
        self.text.push_str(text);
        // credit 按 UTF-16 code unit 计（JS 的 `text.length`）；`saturating_add` 防御极端长
        // 的流（总量只影响快照频率，不会溢出）。
        let units = text.encode_utf16().count() as u64;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.credit = state
                .credit
                .saturating_add(CREDIT_PER_APPEND + CREDIT_PER_CHARACTER * units);
        }
        // 上一切片的变更可能含深值：迭代释放，避免递归 `Drop`（`Value` 的外部限制）。
        drop_changes(std::mem::take(&mut self.changes));
        // 片段边界处不再续写上一段的变更对象：打开字符串必须在自己的片段里重新 `report`。
        if let Some(open) = self.string.as_mut() {
            open.change = None;
        }
        // 主循环是三态分派：打开字符串优先，其次是未终止标量，否则一次消费一个字符。
        // `index` 始终停在字符边界上（见 `read_string` 的字节扫描说明）。
        let mut index = 0usize;
        while index < text.len() && !self.done {
            if let Some(open) = self.string.take() {
                index = self.read_string(open, text, index);
            } else if !self.scalar.is_empty() {
                index = self.read_scalar(text, index);
            } else {
                let ch = text[index..]
                    .chars()
                    .next()
                    .expect("index is a char boundary");
                index += ch.len_utf8();
                self.structural(ch);
            }
        }
        // 返回本片段的变更：`take` 把带容量的 `Vec` 整体交给调用方，字段被替换为 `Vec::new()`
        // （容量 0）；容量随返回值转移、不在解析器内复用，下一片段仍需重新分配。
        std::mem::take(&mut self.changes)
    }

    /// `snapshot()`：迄今读到的文本的值；未开始时为 `None`。
    ///
    /// 返回的值是普通 JSON 值，**返回后永不改变**。文本自上次快照以来没有改变、或复制 open
    /// path 的 credit 不足时，返回**上一个**快照。
    ///
    /// 每次调用都深拷贝整个值（O(当前参数总字节数)）；高频读取或只需要句柄时用
    /// [`Self::snapshot_shared`]。
    pub fn snapshot(&self) -> Option<Value> {
        self.snapshot_shared().map(|value| deep_clone(&value))
    }

    /// `snapshot()` 的共享句柄版本：内容与 [`Self::snapshot`] 相同，但复用缓存里同一个 `Arc`
    /// 时不复制（TS 返回的就是同一个对象引用）。[`ToolCallArgumentsStream`] 用它判断快照是否
    /// 变化；需要 O(1) 句柄的调用方也应使用它，而不是反复 `snapshot()` 深拷贝。
    ///
    /// 返回 `None` 表示还没有值。credit 不足时返回旧句柄并保持 `dirty`，等后续 `append`
    /// 补充 credit 后再生成新快照（这是 TS 中可观察的「预览滞后」）。
    pub fn snapshot_shared(&self) -> Option<std::sync::Arc<Value>> {
        let pending = self.pending_scalar();
        let cost = self.path_cost + u64::from(pending.is_some());
        // 扣费与重建都在同一把锁内，保证并发调用者看到一致的缓存与 revision。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.dirty {
            // 文本自上次快照以来没有改变值：直接复用同一个句柄。
            return state.snapshot.clone();
        }
        if cost > state.credit {
            // credit 不足：保持 dirty，返回旧快照，等后续 append 再补 credit。
            return state.snapshot.clone();
        }
        state.credit -= cost;
        state.dirty = false;
        let snapshot = self.copy_open_path(pending).map(std::sync::Arc::new);
        // 旧快照若已无人共享，由 replace_snapshot 迭代释放（Value 的 Drop 是递归的）。
        replace_snapshot(&mut state.snapshot, snapshot.clone());
        state.revision = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        snapshot
    }

    /// 最近一次新快照的版本号（`0` 表示还没有生成过快照）。
    ///
    /// 每次重建快照（`dirty` 且 credit 足够）都会从全局计数器取一个新值，即使该次
    /// `copy_open_path` 返回 `None`；它只作代际标识，不参与同步。当前仓库内没有生产调用方
    /// （`ToolCallArgumentsStream` 不依赖它），保留给调试与断言。
    /// [`ToolCallArgumentsStream`] 的变化判断按 TS 的 `snapshot === this.#snapshot` 语义
    /// （原始值按值比较、对象/数组按 `Arc` 身份比较），不依赖这个版本号。
    pub fn snapshot_revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision
    }

    /// `get text`：已接收的原始文本。
    pub fn text(&self) -> &str {
        &self.text
    }

    // --- 内部步骤 ---------------------------------------------------------

    /// 标记「文本可能改变了值」；真正生成快照与扣 credit 留给 `snapshot_shared`。
    ///
    /// 用 `&self` 只是因为 `dirty` 由 `Mutex` 保护、写入不需要 `&mut self`；当前所有调用方
    /// （`read_scalar`/`structural`/`note_string_text`/`open_container`/`close_container`/
    /// `place`/`place_open`）本身都持有 `&mut self`。锁只覆盖一个 bool 的写入。
    fn mark_dirty(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).dirty = true;
    }

    /// 读字符串字符，直到字符串结束或文本读完；返回下一个字节下标。
    ///
    /// 按字节扫描是安全的：引号 `0x22` 与反斜杠 `0x5c` 都是 ASCII，不会出现在 UTF-8
    /// 多字节序列的中间，因此 `run_start..index` 切片总落在字符边界上。`run_start` 是
    /// 「尚未追加的普通文本起点」，遇到转义或引号时先把这段文本追加掉。
    fn read_string(&mut self, mut open: OpenString, text: &str, start: usize) -> usize {
        let bytes = text.as_bytes();
        let mut run_start = start;
        let mut index = start;
        while index < bytes.len() {
            let byte = bytes[index];
            if !open.escape.is_empty() {
                if self.continue_escape(&mut open, byte) {
                    run_start = index + 1;
                    index += 1;
                    continue;
                }
                // 不属于转义：按普通字符串字符在下面读入。
                // `continue_escape` 返回 false 表示当前字节要重读，`run_start` 回到它。
                run_start = index;
            }
            if byte == b'"' {
                // 引号结束字符串：先吐出待追加的普通文本，再收尾。
                if index > run_start {
                    self.append_string(&mut open, &text[run_start..index]);
                }
                self.end_string(open);
                return index + 1;
            }
            if byte == b'\\' {
                // 反斜杠开启转义：同样先追加前一段普通文本；escape 状态可跨片段保留。
                if index > run_start {
                    self.append_string(&mut open, &text[run_start..index]);
                }
                open.escape = "\\".to_string();
                run_start = index + 1;
            }
            index += 1;
        }
        // 片段结束仍未闭合：保存 OpenString，下一次 append 从转义的中间继续。
        if text.len() > run_start {
            self.append_string(&mut open, &text[run_start..]);
        }
        self.string = Some(open);
        text.len()
    }

    /// 读数字或字面量字符，直到下一个分隔符；返回分隔符的下标。
    ///
    /// 只在 `self.scalar` 非空时被调用（主循环的分派条件）。若片段第一个字节就是分隔符，
    /// `end == start`：这里先 `end_scalar()` 结束上一个 token，再原样返回下标，让主循环把
    /// 该分隔符当结构字符处理。
    fn read_scalar(&mut self, text: &str, start: usize) -> usize {
        let bytes = text.as_bytes();
        let mut end = start;
        while end < bytes.len() && !is_delimiter(bytes[end]) {
            end += 1;
        }
        if end > start {
            self.scalar.push_str(&text[start..end]);
            if self.scalar_slot.is_some() {
                self.mark_dirty();
            }
        }
        if end < bytes.len() {
            self.end_scalar();
        }
        end
    }

    /// 处理字符串外的一个字符。
    ///
    /// 刻意不校验括号类型：`}` 也能关闭数组（流式宽松解析，不因类型不匹配提前失败）。空白被忽略；未知字符一律当作
    /// 标量开头，非法位置的标量随后在 `value_slot` 得到 `None` 并被丢弃。
    ///
    /// `:` 只在对象且 phase 为 `Colon` 时推进到 `Value`；`,` 把对象置回 `Key`、数组置回
    /// `Value`。缺失逗号/冒号由 `value_slot` 与 `start_string` 另行容忍。
    fn structural(&mut self, ch: char) {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {}
            '"' => self.start_string(),
            '{' => self.open_container(Value::Object(Map::new())),
            '[' => self.open_container(Value::Array(Vec::new())),
            '}' | ']' => self.close_container(),
            ':' => {
                if let Some(frame) = self.stack.last_mut()
                    && !matches!(frame.container, Value::Array(_))
                    && frame.phase == Phase::Colon
                {
                    frame.phase = Phase::Value;
                }
            }
            ',' => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.phase = if matches!(frame.container, Value::Array(_)) {
                        Phase::Value
                    } else {
                        Phase::Key
                    };
                }
            }
            _ => {
                // 未知字符一律开启标量：数字、true/false/null 的首字母都走这里；
                // 非法位置的标量在 `value_slot` 得到 `None`，结束后被丢弃。
                self.scalar = ch.to_string();
                self.scalar_slot = self.value_slot();
                if self.scalar_slot.is_some() {
                    self.mark_dirty();
                }
            }
        }
    }

    /// 从现在开始的一个值放在哪里；该位置不接受值时返回 `None`。
    ///
    /// - 根：只有 `root` 为空时接受，根值之后的位置一律丢弃；
    /// - 数组：永远接受下一个元素，下标取工作数组的当前长度（打开的最后一个元素不在工作
    ///   数组里，关闭时才移入，因此不会重复计数）；
    /// - 对象：phase 为 `Value` 或 `Colon` 且已有键时接受（缺失冒号也被容忍），否则丢弃。
    fn value_slot(&self) -> Option<Slot> {
        match self.stack.last() {
            None => {
                if self.root.is_none() {
                    Some(Slot::Root)
                } else {
                    None
                }
            }
            Some(frame) => {
                if matches!(frame.container, Value::Array(_)) {
                    let index = match &frame.container {
                        Value::Array(elements) => elements.len(),
                        _ => unreachable!(),
                    };
                    return Some(Slot::Element(index));
                }
                // 缺失冒号被容忍：`{"a" 1}` 读作 `{"a": 1}`。
                if (frame.phase == Phase::Value || frame.phase == Phase::Colon)
                    && frame.key.is_some()
                {
                    frame.key.clone().map(Slot::Member)
                } else {
                    None
                }
            }
        }
    }

    /// 打开一个字符串：判断它处在键位还是值位，再建立 `OpenString`。
    ///
    /// 键位只对对象成立（phase 为 `Key` 或 `Comma`），因此缺失逗号的 `{"a":1 "b":2}`
    /// 会把第二个字符串当键。值位若处于会被报告的位置（`reported_path` 返回 `Some`），会立即
    /// `place_open` 记账并上报一条空串的 `Set`，后续文本通过 `note_string_text` 原地扩展这条
    /// 变更；父容器位于 32 层或更深时 `reported_path` 为 `None`，这里不产生变更，等外层容器
    /// 关闭时整体上报；位置非法时 `place_open` 也直接返回，文本被丢弃。
    fn start_string(&mut self) {
        let key_position = match self.stack.last() {
            // 缺失逗号被容忍：`{"a": 1 "b": 2}` 读第二个成员。
            Some(frame) => {
                !matches!(frame.container, Value::Array(_))
                    && (frame.phase == Phase::Key || frame.phase == Phase::Comma)
            }
            None => false,
        };
        if key_position {
            self.string = Some(OpenString {
                target: StringTarget::Key,
                slot: None,
                text: String::new(),
                pending_high: None,
                escape: String::new(),
                path: None,
                change: None,
            });
            return;
        }
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        self.string = Some(OpenString {
            target: StringTarget::Value,
            slot: slot.clone(),
            text: String::new(),
            pending_high: None,
            escape: String::new(),
            path: path.clone(),
            change: None,
        });
        // 值字符串立即可见为空串（这里只记账；真实文本在结束或快照时写入）。
        self.place_open(slot.as_ref());
        if let Some(path) = path {
            let index = self.report(JsonChange::Set {
                path,
                value: Value::String(String::new()),
            });
            if let Some(open) = self.string.as_mut() {
                open.change = Some(index);
            }
        }
    }

    /// 值在 `slot` 处的位置路径（当该层报告内部变更时）。
    ///
    /// 根值返回空路径。父容器 `reports_inside` 为假时返回 `None`，有两种成因：路径长度达到
    /// `MAX_REPORTED_DEPTH`，或容器本身位于非法位置（`path` 为 `None`）。前者会在该容器关闭
    /// 时整体上报一次；后者永远不会被单独上报——`close_container` 要求 `frame.path.is_some()`
    /// 才上报，因此该值的变化直接消失。
    fn reported_path(&self, slot: Option<&Slot>) -> Option<JsonPath> {
        let slot = slot?;
        if matches!(slot, Slot::Root) {
            return Some(Vec::new());
        }
        let frame = self.stack.last()?;
        if !frame.reports_inside {
            return None;
        }
        let mut path = frame.path.clone()?;
        path.push(match slot {
            Slot::Member(key) => PathSegment::Key(key.clone()),
            Slot::Element(index) => PathSegment::Index(*index),
            Slot::Root => unreachable!(),
        });
        Some(path)
    }

    /// 追加一条变更并返回它在 `changes` 里的下标。
    ///
    /// 下标只在**本片段**内有效：`append_changes` 开头会清空 `changes`，打开字符串跨片段
    /// 继续时 `open.change` 会被复位并重新 `report`。
    fn report(&mut self, change: JsonChange) -> usize {
        self.changes.push(change);
        self.changes.len() - 1
    }

    /// 用 `byte` 继续一个转义；`false` 表示它不属于转义、需要按普通字符重读。
    ///
    /// `open.escape` 只有两种形态：`"\\"`（刚读到反斜杠）与 `"\\u" + 至多四个十六进制位`。
    /// 非法转义不报错：把已读的反斜杠或 `\uXX` 原样写回文本，让该字节按普通字符重读，
    /// 与 TS 的 `repairJson` 容错一致。
    fn continue_escape(&mut self, open: &mut OpenString, byte: u8) -> bool {
        if open.escape == "\\" {
            // 简单转义表：JSON 的八个单字符转义。
            let decoded = match byte {
                b'"' => Some('"'),
                b'\\' => Some('\\'),
                b'/' => Some('/'),
                b'b' => Some('\u{8}'),
                b'f' => Some('\u{c}'),
                b'n' => Some('\n'),
                b'r' => Some('\r'),
                b't' => Some('\t'),
                _ => None,
            };
            if let Some(decoded) = decoded {
                open.escape.clear();
                self.append_string(open, &decoded.to_string());
                return true;
            }
            if byte == b'u' {
                open.escape = "\\u".to_string();
                return true;
            }
            // 非法转义：反斜杠保留为文本，字符按普通字符重读（与 repairJson 一致）。
            open.escape.clear();
            self.append_string(open, "\\");
            return false;
        }
        // 形态 `\uXXXX`：每读一个十六进制位就推进；满六字符（`\u` + 4 位）时合成 code unit。
        if is_hex_digit(byte) {
            open.escape.push(byte as char);
            if open.escape.len() < 6 {
                return true;
            }
            let digits = u32::from_str_radix(&open.escape[2..], 16).expect("hex digits");
            open.escape.clear();
            let unit = (digits & 0xffff) as u16;
            self.append_units(open, &[unit]);
            return true;
        }
        // `\u` 后面不是四位十六进制：保留为文本，字符按普通字符重读。
        let literal = std::mem::take(&mut open.escape);
        self.append_string(open, &literal);
        false
    }

    /// 追加一段已解码的字符串文本（来自原文的普通字符或简单转义）。
    ///
    /// `&str` 不可能携带孤立代理项，因此若此前有等待配对的高代理项，它此时确定是孤儿：
    /// 按 `from_utf16_lossy` 的规则先补一个 U+FFFD，并把 replacement 记入变更流，保证
    /// 快照文本与 `Set`/`Append` 文本一致。
    fn append_string(&mut self, open: &mut OpenString, text: &str) {
        // `&str` 里没有孤立代理项，追加进去的字符都是完整标量值：先前的 pending_high 已是孤儿。
        if open.pending_high.take().is_some() {
            open.text.push('\u{fffd}');
            let replacement = "\u{fffd}".to_string();
            self.note_string_text(open, &replacement);
        }
        open.text.push_str(text);
        self.note_string_text(open, text);
    }

    /// 追加一个 UTF-16 code unit（来自 `\uXXXX`），按 `from_utf16_lossy` 的规则配对。
    ///
    /// 高代理项先停在 `pending_high`，等下一个 unit 决定配对还是替换；这使跨片段的
    /// `\uD83D` + `\uDE00` 也能合并成同一个字符。未配对的高/低代理项都输出 U+FFFD，
    /// 因为 Rust 的 `String` 无法表示孤立代理项（模块注释的「被迫差异」第 1 条）。
    fn append_units(&mut self, open: &mut OpenString, units: &[u16]) {
        let mut appended = String::new();
        for &unit in units {
            match open.pending_high.take() {
                Some(high) => {
                    if (0xdc00..=0xdfff).contains(&unit) {
                        let code = 0x1_0000u32
                            + ((u32::from(high) - 0xd800) << 10)
                            + (u32::from(unit) - 0xdc00);
                        appended.push(char::from_u32(code).expect("valid scalar value"));
                    } else {
                        appended.push('\u{fffd}');
                        if (0xd800..=0xdbff).contains(&unit) {
                            open.pending_high = Some(unit);
                        } else if (0xdc00..=0xdfff).contains(&unit) {
                            appended.push('\u{fffd}');
                        } else {
                            appended.push(char::from_u32(u32::from(unit)).expect("BMP scalar"));
                        }
                    }
                }
                None => {
                    if (0xd800..=0xdbff).contains(&unit) {
                        open.pending_high = Some(unit);
                    } else if (0xdc00..=0xdfff).contains(&unit) {
                        appended.push('\u{fffd}');
                    } else {
                        appended.push(char::from_u32(u32::from(unit)).expect("BMP scalar"));
                    }
                }
            }
        }
        if !appended.is_empty() {
            open.text.push_str(&appended);
            self.note_string_text(open, &appended);
        }
        let _ = std::mem::take(&mut appended);
    }

    /// 把新读到的文本登记进快照记账与变更序列（TS 的 `appendString` 后半段）。
    ///
    /// 键字符串不产生变更（提前返回）；值字符串在有效槽位上每次追加都会 `mark_dirty`，因为
    /// 快照文本变了。`slot` 为 `None` 时提前返回：文本被丢弃，也不标脏。
    /// 只有当 `open.change` 仍是本片段**最后一条**变更时才原地扩展它；中间插入过其它变更
    /// （例如嵌套容器关闭）时另起一条 `Append`。`open.change` 的下标只在本片段有效。
    fn note_string_text(&mut self, open: &mut OpenString, text: &str) {
        if open.target != StringTarget::Value {
            return;
        }
        if open.slot.is_none() {
            return;
        }
        self.mark_dirty();
        let Some(path) = open.path.clone() else {
            return;
        };
        let _ = &path;
        // 只有当这个字符串的变更仍是本片段最后一条时才就地扩展；否则另起一条 append。
        if let Some(index) = open.change
            && index + 1 == self.changes.len()
        {
            match &mut self.changes[index] {
                JsonChange::Set { value, .. } => {
                    let mut current = match value {
                        Value::String(current) => std::mem::take(current),
                        _ => String::new(),
                    };
                    current.push_str(text);
                    *value = Value::String(current);
                }
                JsonChange::Append { text: current, .. } => {
                    current.push_str(text);
                }
            }
            return;
        }
        let index = self.report(JsonChange::Append {
            path,
            text: text.to_string(),
        });
        open.change = Some(index);
    }

    /// 结束一个字符串：键写入 `Frame::key` 并进入 `Colon`；值落位并结束该值。
    ///
    /// `OpenString` 被消费（`self.string` 已先清空）。未配对的高代理项在落位前补 U+FFFD；
    /// 键字符串没有父容器时（根上的键）直接丢弃（没有合法落点）。
    fn end_string(&mut self, open: OpenString) {
        self.string = None;
        match open.target {
            StringTarget::Key => {
                let Some(frame) = self.stack.last_mut() else {
                    return;
                };
                let mut text = open.text;
                if open.pending_high.is_some() {
                    text.push('\u{fffd}');
                }
                frame.key = Some(text);
                frame.phase = Phase::Colon;
            }
            StringTarget::Value => {
                if open.slot.is_some() {
                    let mut text = open.text;
                    if open.pending_high.is_some() {
                        text.push('\u{fffd}');
                    }
                    self.place_existing(open.slot.as_ref(), Value::String(text));
                }
                self.value_ended();
            }
        }
    }

    /// 结束一个标量：合法 token 在槽位有效时写入工作副本；只有 `reported_path` 返回 `Some`
    /// 时才上报 `Set`，深层位置的变更等外层容器关闭时整体上报。非法位置（`slot` 为 `None`）
    /// 既不写入也不上报；非法 token（未知单词、坏数字）被丢弃。
    ///
    /// 无论 token 是否合法都要调用 `value_ended`：否则父容器不会进入 `Comma` 阶段，根值也
    /// 不会置 `done`，后续文本会被错误地读进同一个位置。
    fn end_scalar(&mut self) {
        let token = std::mem::take(&mut self.scalar);
        let slot = self.scalar_slot.take();
        if let Some(value) = scalar_value(&token) {
            self.place(slot.as_ref(), value.clone());
            if let Some(path) = self.reported_path(slot.as_ref()) {
                self.report(JsonChange::Set { path, value });
            }
        }
        self.value_ended();
    }

    /// 文本末尾未终止标量的可见值。
    ///
    /// 位置非法、token 为空、或 token 超过 `MAX_PENDING_SCALAR_LENGTH`（UTF-16 code unit）
    /// 时返回 `None`。长度上限保证未终止的超长数字不会让每次快照都变贵。
    fn pending_scalar(&self) -> Option<Pending> {
        let slot = self.scalar_slot.as_ref()?;
        let token = self.scalar.as_str();
        if token.is_empty() {
            return None;
        }
        if token.encode_utf16().count() > MAX_PENDING_SCALAR_LENGTH {
            return None;
        }
        let value = pending_scalar_value(token)?;
        Some(Pending {
            slot: slot.clone(),
            value,
        })
    }

    /// 打开一个新容器：记录它在父容器中的位置，压入 `Frame`；只有 `path` 存在时才上报空容器
    /// 的 `Set`。更深（父容器路径长度已达 `MAX_REPORTED_DEPTH`，即本容器路径 33 段及以上）
    /// 或非法位置的容器不会在这里上报：前者由外层容器关闭时整体上报，后者被直接丢弃。
    ///
    /// `phase` 按**新容器**的类型决定（对象从 `Key` 开始，数组从 `Value` 开始）。容器的实际
    /// 内容不会写进父容器的工作副本，而是在关闭时由 `place_existing` 移入（快照则由
    /// `copy_open_path` 逐层拼装）。`path_cost += 1` 是这一层的复制成本。
    fn open_container(&mut self, container: Value) {
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        self.place_open(slot.as_ref());
        // 数组的初始阶段是 `value`、对象是 `key`（按**新**容器的类型判断）。
        let phase = if matches!(container, Value::Array(_)) {
            Phase::Value
        } else {
            Phase::Key
        };
        let empty = if matches!(container, Value::Array(_)) {
            Value::Array(Vec::new())
        } else {
            Value::Object(Map::new())
        };
        self.stack.push(Frame {
            container,
            slot,
            path: path.clone(),
            reports_inside: path
                .as_ref()
                .is_some_and(|path| path.len() < MAX_REPORTED_DEPTH),
            size: 0,
            key: None,
            phase,
        });
        self.path_cost += 1;
        self.mark_dirty();
        // 新容器：工作容器持续变化，报告出去的变更对象永不变化。
        if let Some(path) = path {
            self.report(JsonChange::Set { path, value: empty });
        }
    }

    /// 关闭最内层容器：弹出 `Frame`，把它的工作容器移入父容器。
    ///
    /// 若该层深到不报告内部变更（路径长度达到 `MAX_REPORTED_DEPTH`），关闭时把整个值作为
    /// 一条 `Set` 上报。游离的 `}`/`]`（栈为空）被忽略；`}` 也能关闭数组（宽松解析），
    /// 不校验类型匹配。`path_cost` 用饱和减法扣除，避免记账异常时下溢。
    fn close_container(&mut self) {
        let Some(frame) = self.stack.pop() else {
            // 游离的闭合符被忽略。
            return;
        };
        self.path_cost = self.path_cost.saturating_sub(frame.size as u64 + 1);
        self.mark_dirty();
        if frame.path.is_some() && !frame.reports_inside {
            // 深到不报告内部的层：值在关闭时已最终，克隆一份交给变更对象。
            let path = frame.path.clone().expect("checked");
            self.report(JsonChange::Set {
                path,
                value: deep_clone(&frame.container),
            });
        }
        self.place_existing(frame.slot.as_ref(), frame.container);
        self.value_ended();
    }

    /// 一个值结束：根值之后置 `done`，否则父容器进入 `comma`。
    ///
    /// 只有 `root` 已存在时才置 `done`：非法位置的根值被丢弃后，后续文本仍有机会成为根值
    /// （TS 的「根值之后的文本被忽略」以「根值存在」为前提）。
    fn value_ended(&mut self) {
        match self.stack.last_mut() {
            None => {
                if self.root.is_some() {
                    self.done = true;
                }
            }
            Some(frame) => frame.phase = Phase::Comma,
        }
    }

    /// 把值放入 `slot`（做 `size`/`path_cost` 记账并标记 dirty）。非法位置什么也不做。
    ///
    /// 数组元素只有在下标等于当前长度时才算「新增」；对象成员只有键不存在时才算新增。
    /// 覆盖已存在的成员不增加计数，但 `IndexMap::insert` 保留首次插入位置（JS 的重复键
    /// 保位语义）。这里的 `path_cost` 增量必须与 `close_container` 的扣减成对。
    fn place(&mut self, slot: Option<&Slot>, value: Value) {
        let Some(slot) = slot else {
            return;
        };
        self.mark_dirty();
        match slot {
            Slot::Root => self.root = Some(value),
            Slot::Element(index) => {
                let index = *index;
                let mut grew = false;
                if let Some(frame) = self.stack.last_mut()
                    && let Value::Array(elements) = &mut frame.container
                {
                    if index == elements.len() {
                        frame.size += 1;
                        grew = true;
                    }
                    if index < elements.len() {
                        elements[index] = value;
                    } else {
                        elements.push(value);
                    }
                }
                if grew {
                    self.path_cost += 1;
                }
            }
            Slot::Member(key) => {
                let mut grew = false;
                if let Some(frame) = self.stack.last_mut()
                    && let Value::Object(members) = &mut frame.container
                {
                    if !members.contains_key(key) {
                        frame.size += 1;
                        grew = true;
                    }
                    members.insert(key.clone(), value);
                }
                if grew {
                    self.path_cost += 1;
                }
            }
        }
    }

    /// 只记 `size`/`path_cost` 并标记 dirty，不写入值：打开的容器/字符串在快照或关闭时才落位。
    ///
    /// 这是 Rust 与 TS 的结构性差异：TS 在打开时就把容器对象 `#place` 进父容器（靠引用
    /// 共享看到后续变化），Rust 不能把工作副本借给父容器，因此这里只记账；真正的写入由
    /// `place_existing`（关闭时）或 `copy_open_path`（快照时）完成。
    fn place_open(&mut self, slot: Option<&Slot>) {
        let Some(slot) = slot else {
            return;
        };
        self.mark_dirty();
        match slot {
            Slot::Root => {}
            Slot::Element(index) => {
                let index = *index;
                let mut grew = false;
                if let Some(frame) = self.stack.last_mut()
                    && let Value::Array(elements) = &mut frame.container
                    && index == elements.len()
                {
                    frame.size += 1;
                    grew = true;
                }
                if grew {
                    self.path_cost += 1;
                }
            }
            Slot::Member(key) => {
                let mut grew = false;
                if let Some(frame) = self.stack.last_mut()
                    && let Value::Object(members) = &mut frame.container
                    && !members.contains_key(key)
                {
                    frame.size += 1;
                    grew = true;
                }
                if grew {
                    self.path_cost += 1;
                }
            }
        }
    }

    /// 写入值但不记账（打开时已经记过）。
    ///
    /// 用于字符串结束与容器关闭。对于新增的非根成员/元素，`place_open` 已在打开时增加
    /// `size`/`path_cost` 并标记 dirty，这里只把完成的值移入父容器，因此不再重复记账与
    /// `mark_dirty`。`Slot::Root` 与重复键覆盖在 `place_open` 中没有计数增量（前者是空分支，
    /// 后者因键已存在），这里同样不做增减。
    fn place_existing(&mut self, slot: Option<&Slot>, value: Value) {
        let Some(slot) = slot else {
            return;
        };
        match slot {
            Slot::Root => self.root = Some(value),
            _ => {
                if let Some(frame) = self.stack.last_mut() {
                    set_slot(&mut frame.container, slot, value);
                }
            }
        }
    }

    /// 新快照：从最深一层开始复制打开的容器，每层放入其打开子项的副本。
    ///
    /// 工作容器里**不包含**当前打开的子项（`place_open` 只记账），因此每层只需复制已完成的
    /// 成员，再把下一层的副本放进本层副本。与 TS 的浅拷贝不同，这里的已完成成员用
    /// `deep_clone` 深复制，真实成本是它们的总字节数（credit 只按成员数扣，见模块注释）；
    /// 自底向上的拼装保证深嵌套时不会递归。
    fn copy_open_path(&self, pending: Option<Pending>) -> Option<Value> {
        // 未终止的值字符串与未终止标量一样，只在最内层副本里落位。
        let string_value: Option<(Slot, Value)> = match &self.string {
            Some(open) if open.target == StringTarget::Value => open.slot.clone().map(|slot| {
                let mut text = open.text.clone();
                if open.pending_high.is_some() {
                    text.push('\u{fffd}');
                }
                (slot, Value::String(text))
            }),
            _ => None,
        };
        if self.stack.is_empty() {
            // 没有打开的容器：根值已经是最终值；未终止的根标量/根字符串优先于它。
            if let Some(pending) = pending
                && matches!(pending.slot, Slot::Root)
            {
                return Some(pending.value);
            }
            if let Some((Slot::Root, value)) = string_value {
                return Some(value);
            }
            return self.root.as_ref().map(deep_clone);
        }
        let mut child: Option<Value> = None;
        let mut child_slot: Option<Slot> = None;
        // 自底向上：内层副本直接**移动**进父层副本（TS 里是同一个引用；这里所有权唯一，
        // 因此不必再深拷贝一遍）。每个容器仍要为它的已完成成员付出复制成本，详见模块注释。
        let mut pending = pending;
        let mut string_value = string_value;
        for depth in (0..self.stack.len()).rev() {
            let frame = &self.stack[depth];
            // 打开子项的位置（本层最深的那个槽）。注意：工作副本里不含打开子项，下面不会
            // 真的跳过任何槽；这个值当前只用于记录位置（`let _ = &open_slot;`），真正放入
            // 打开子项由后面的 `set_slot` 完成。
            let open_slot = if depth == self.stack.len() - 1 {
                match (
                    pending.as_ref().map(|p| &p.slot),
                    string_value.as_ref().map(|(slot, _)| slot),
                ) {
                    (Some(slot), _) => Some(slot.clone()),
                    (None, Some(slot)) => Some(slot.clone()),
                    (None, None) => None,
                }
            } else {
                child_slot.clone()
            };
            // 本层待放入的子项（打开子项或下一层副本）。
            let insert = depth == self.stack.len() - 1 || child.is_some();
            // 打开的子树从不写进工作容器（`place_open` 只记账），因此复制不需要跳过任何槽；
            // 跳过反而会把**已存在的同名成员**挪到末尾（重复键必须保留首次位置）。
            let _ = &open_slot;
            let mut copy = copy_container_skipping(&frame.container, None, insert);
            if depth == self.stack.len() - 1 {
                if let Some(pending) = pending.take() {
                    set_slot(&mut copy, &pending.slot, pending.value);
                }
                if let Some((slot, value)) = string_value.take() {
                    set_slot(&mut copy, &slot, value);
                }
            }
            if let (Some(child_value), Some(slot)) = (child.take(), &child_slot) {
                set_slot(&mut copy, slot, child_value);
            }
            child = Some(copy);
            child_slot = frame.slot.clone();
        }
        match child_slot {
            // 最外层打开容器位于根槽：child 就是整棵快照。
            Some(Slot::Root) => child,
            // 最外层位于非法位置（被丢弃）：没有可交付的新值，退回已完成的根值。
            _ => self.root.as_ref().map(deep_clone),
        }
    }
}

/// 复制容器，但跳过 `skip` 指向的那个槽（它由下一层的副本取代）。
///
/// 当前唯一调用方 `copy_open_path` 总是传 `skip = None`：工作副本里本来就不含打开的子项，
/// 没有需要跳过的槽。`insert` 只影响容量预留（+1），不改变结果语义。
///
/// 用 `deep_clone`（显式栈）而不是 `Value::clone`：完成的成员可能有任意深度。
/// 这里用普通循环而不是 iterator adapter：快照复制是热路径，debug 构建下 adapter 的开销
/// 能差一个数量级。
fn copy_container_skipping(container: &Value, skip: Option<&Slot>, insert: bool) -> Value {
    let extra = usize::from(insert);
    match container {
        Value::Array(elements) => {
            let skipped = match skip {
                Some(Slot::Element(index)) => Some(*index),
                _ => None,
            };
            let mut items: Vec<Value> = Vec::with_capacity(elements.len() + extra);
            for (index, element) in elements.iter().enumerate() {
                if skipped == Some(index) {
                    continue;
                }
                items.push(deep_clone(element));
            }
            Value::Array(items)
        }
        Value::Object(members) => {
            let skipped = match skip {
                Some(Slot::Member(key)) => Some(key),
                _ => None,
            };
            let mut copy = Map::with_capacity(members.len() + extra);
            for (key, value) in members {
                if skipped.is_some_and(|skipped| skipped == key) {
                    continue;
                }
                copy.insert(key.clone(), deep_clone(value));
            }
            Value::Object(copy)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(parser: &mut StreamingJsonParser, text: &str) -> Vec<JsonChange> {
        let changes = parser.append_changes(text);
        changes
            .into_iter()
            .map(|change| match change {
                JsonChange::Set { path, value } => JsonChange::Set { path, value },
                JsonChange::Append { path, text } => JsonChange::Append { path, text },
            })
            .collect()
    }

    #[test]
    fn reports_a_number_that_ends_and_a_string_that_starts_in_order() {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            changes(&mut parser, r#"{"n":1"#),
            vec![JsonChange::Set {
                path: vec![],
                value: serde_json::json!({})
            }]
        );
        assert_eq!(
            changes(&mut parser, r#"2,"s":"ab"#),
            vec![
                JsonChange::Set {
                    path: vec![PathSegment::Key("n".into())],
                    value: serde_json::json!(12)
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("s".into())],
                    value: serde_json::json!("ab")
                }
            ]
        );
        assert_eq!(
            changes(&mut parser, r#"c","t":tru"#),
            vec![JsonChange::Append {
                path: vec![PathSegment::Key("s".into())],
                text: "c".into()
            }]
        );
        assert_eq!(
            changes(&mut parser, "e}"),
            vec![JsonChange::Set {
                path: vec![PathSegment::Key("t".into())],
                value: serde_json::json!(true)
            }]
        );
        assert_eq!(changes(&mut parser, r#" {"ignored":1}"#), vec![]);
    }

    #[test]
    fn reports_nested_containers_then_their_members() {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            changes(&mut parser, r#"{"a":[1,{"b":["x""#),
            vec![
                JsonChange::Set {
                    path: vec![],
                    value: serde_json::json!({})
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("a".into())],
                    value: serde_json::json!([])
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("a".into()), PathSegment::Index(0)],
                    value: serde_json::json!(1)
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("a".into()), PathSegment::Index(1)],
                    value: serde_json::json!({})
                },
                JsonChange::Set {
                    path: vec![
                        PathSegment::Key("a".into()),
                        PathSegment::Index(1),
                        PathSegment::Key("b".into())
                    ],
                    value: serde_json::json!([])
                },
                JsonChange::Set {
                    path: vec![
                        PathSegment::Key("a".into()),
                        PathSegment::Index(1),
                        PathSegment::Key("b".into()),
                        PathSegment::Index(0)
                    ],
                    value: serde_json::json!("x")
                },
            ]
        );
        assert_eq!(
            changes(&mut parser, r#","y"#),
            vec![JsonChange::Set {
                path: vec![
                    PathSegment::Key("a".into()),
                    PathSegment::Index(1),
                    PathSegment::Key("b".into()),
                    PathSegment::Index(1)
                ],
                value: serde_json::json!("y")
            }]
        );
        assert_eq!(
            changes(&mut parser, r#"z"]}],"a":"again"}"#),
            vec![
                JsonChange::Append {
                    path: vec![
                        PathSegment::Key("a".into()),
                        PathSegment::Index(1),
                        PathSegment::Key("b".into()),
                        PathSegment::Index(1)
                    ],
                    text: "z".into()
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("a".into())],
                    value: serde_json::json!("again")
                }
            ]
        );
    }

    #[test]
    fn holds_split_escapes_back() {
        let mut parser = StreamingJsonParser::new();
        parser.append(r#"{"a":"x"#);
        assert_eq!(changes(&mut parser, "\\u00"), vec![]);
        assert_eq!(
            changes(&mut parser, "e9"),
            vec![JsonChange::Append {
                path: vec![PathSegment::Key("a".into())],
                text: "é".into()
            }]
        );
    }

    #[test]
    fn reports_a_deep_container_once_when_it_closes() {
        let depth = 40;
        let text = format!("{}\"deep\"{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
        let mut parser = StreamingJsonParser::new();
        let changes = parser.append_changes(&text);
        assert_eq!(changes.len(), 34);
        let last = changes.last().expect("changes");
        match last {
            JsonChange::Set { path, value } => {
                assert_eq!(path.len(), 32);
                let expected: Value =
                    serde_json::from_str(&text[32 * 5..text.len() - 32]).expect("json");
                assert_eq!(value, &expected);
            }
            other => panic!("expected set, got {other:?}"),
        }
    }

    #[test]
    fn reports_nothing_for_invalid_positions_and_text_after_the_root() {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            changes(&mut parser, r#"{"a":1 2, "b" "x"}"#),
            vec![
                JsonChange::Set {
                    path: vec![],
                    value: serde_json::json!({})
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("a".into())],
                    value: serde_json::json!(1)
                },
                JsonChange::Set {
                    path: vec![PathSegment::Key("b".into())],
                    value: serde_json::json!("x")
                },
            ]
        );
        assert_eq!(changes(&mut parser, r#"{"c":3}"#), vec![]);
    }
}
