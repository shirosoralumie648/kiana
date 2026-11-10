//! delta 目录。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 共享形状（`Seg` / `Path` / `Op`）
//!
//! [`crate::types::StateEvent`] 的 `ops` 成员是 `readonly Op[]`（spec/chord-rest.md §2.27），所以
//! 操作语言的核心类型在本模块里只有这一份定义。`apply`/`apply_immutable`/`encoder`/`decoder`/
//! `diff`/`tracker` 等由 delta 单元在下面的子模块里实现（Rust 形状见 spec/chord-rest.md §8.2）；
//! 本文件只给出被跨文件引用的那三个类型。`Op` 的变体与 tuple 编码是 wire 稳定契约：
//!
//! ```text
//! ["r", value]                        Replace(JsonValue)
//! ["s", path, value]                  Set(Path, JsonValue)
//! ["d", path]                         Delete(Path)
//! ["a", path, text]                   Append(Path, String)
//! ["t", path, count]                  Truncate(Path, usize)   count = 保留的 UTF-16 单元数
//! ["p", path, index, remove, items]   Splice { path, index, remove, items }
//! ["m", path, permutation]            Permute(Path, Vec<usize>)  new[i] = old[permutation[i]]
//! ```
//!
//! 已解码的 op 只有这 7 种；`WireOp` 额外允许 `["#", id, path]` 与省略路径的短形式，那些是
//! codec 的语法，不属于 [`Op`]。
//!
//! ## 设计原因
//!
//! 操作语言把“状态怎么变”与“状态怎么传输”分开：`Op` 是解码后的强类型形式；wire 形式
//! （JSON 元组、路径 interning、`#` 路径定义）是 codec 的语法，只在 `index` 子模块里处理。
//! 三个共享类型集中定义在本模块，让 `apply`、`diff`、`pending`、`encoder`/`decoder` 共用
//! 同一份定义，避免出现第二套互不兼容的 `Op`。
//!
//! ## 一次典型调用
//!
//! 1. 收到 wire op 批次后，先用 [`crate::delta::index::decode`] 处理 `#` 路径定义、数字 id
//!    引用与省略路径的短形式并做校验，得到 [`Op`]；跨批次复用 id 表时改用配套的
//!    [`crate::delta::index::Decoder::decode`]。只有单条、路径内联的 JSON 元组才用
//!    [`crate::delta::index::parse_op`]：它把 `items[0]` 当 verb，不认 `#`，也拒绝省略
//!    路径的短形式。
//! 2. [`crate::delta::index::apply`]（校验版）或 `apply_unvalidated`（trusted 版）按顺序
//!    应用 op；`Replace` 直接替换根值，其余 op 都基于前一条的结果。
//! 3. 慢消费者用 [`crate::delta::pending::PendingOps`] 在两次 `take` 之间折叠叶 op。
//! 4. 修订之间用 [`crate::delta::diff::diff_revisions`] 生成 op。`tracker` 负责从 draft
//!    变更生成 op：kiana-chord 内的 delta tracker 尚未移植（见 `tracker` 模块说明）；
//!    `kiana-durable` 的 `session::transaction` 另有值级的 `Tracker`/`Change`/`DocDraft`，
//!    其 `Change::prepare` 用 `diff_revisions` 从快照差异得到 op，但没有 overlay/Proxy、
//!    增量 op-log，也不保证与 TS tracker 的 op 序列逐字节相同。
//!
//! ## 注意事项
//!
//! - op 序列**顺序敏感**：同一条路径上的后一条 op 看到前一条 op 的结果。
//! - `s`/`d`/`a`/`t` 的路径不能为空；`p`/`m` 可以为空，因为它们作用于被跟踪值本身
//!   （被跟踪值可以就是数组）。
//! - [`Seg::Index`] 由解码器保证非负。校验版 [`crate::delta::index::apply`] 会在
//!   `assert_valid_op` 里拒绝手动构造的负下标；trusted 版 `apply_unvalidated` 跳过这层
//!   校验，对象父容器会把负下标经 `seg_key` 变成普通键 `"-1"` 正常读/写/删，只有数组
//!   父容器仍报 `UnsafePathError`，因此这条不变量不由 trusted 路径保证。
//! - 本文件只定义类型；子模块里 `draft` 与 `tracker` 目前只有模块说明，没有实现
//!   （见各自文件）。

// --- 子模块 ---
pub mod apply_immutable_trusted;
pub mod diff;
pub mod draft;
pub mod index;
pub mod pending;
pub mod revision_validator;
pub mod tracker;

use serde_json::Value;

/// 路径段：对象键（`String`）或数组下标（`i64`，恒为非负整数）。
///
/// TS: `delta/index.ts#Seg`（`string | number`）。数字段在 JSON 里是数字，在对象上是键——`apply`
/// 用父容器是数组还是对象来判别（见 spec §2.27 的 `s`/`d` 规则）。
///
/// 解码路径时负数、小数与非有限数字都会被拒绝（`index::number_to_seg`）；超过 `i64` 的
/// 非负整数保留为 `Seg::Key`，对象写入按普通属性键处理，但数组父容器会按“非数字段”报
/// `UnsafePathError`。手动构造的 `Seg::Index` 若为负，校验版
/// [`crate::delta::index::apply`]（`assert_valid_op`）会拒绝；trusted 版
/// `apply_unvalidated` 跳过这层校验，对象父容器会把该段当作普通键 `"-1"` 读/写/删，
/// 只有数组父容器报 `UnsafePathError`。因此“索引恒非负”是解码器与校验版的不变量，
/// trusted 路径不能保证它。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Seg {
    /// 对象键。
    Key(String),
    /// 数组下标（非负）。
    Index(i64),
}

/// TS: `delta/index.ts#Path`（`readonly Seg[]`）。非空约束由 `assertValidOp` 在运行期检查
/// （`"path is empty"`），不是类型约束。
pub type Path = Vec<Seg>;

/// 已解码的操作（TS: `delta/index.ts#Op`）。
///
/// `r` 是**唯一**替换整个值的操作；`s`/`d`/`a`/`t` 不能指向根（`p` 与 `m` 可以，因为被跟踪的值
/// 本身可以是数组）。
///
/// 一批 op 按切片顺序逐条应用，后一条看到的是前一条执行后的状态；`r` 之后的 op 作用于
/// 替换后的值。校验版应用器在应用前对每条 op 调用 `assert_valid_op`，trusted 版跳过这一步。
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `["r", value]`。
    Replace(Value),
    /// `["s", path, value]`。
    Set(Path, Value),
    /// `["d", path]`。
    Delete(Path),
    /// `["a", path, text]`。
    Append(Path, String),
    /// `["t", path, count]`：把路径上的字符串截到前 `count` 个 UTF-16 单元。
    Truncate(Path, usize),
    /// `["p", path, index, remove, items]`。
    Splice {
        /// 被 splice 的数组所在路径。
        path: Path,
        /// 起始下标。
        index: usize,
        /// 删除个数。
        remove: usize,
        /// 插入项。
        items: Vec<Value>,
    },
    /// `["m", path, permutation]`：`new[i] = old[permutation[i]]`。
    Permute(Path, Vec<usize>),
}
