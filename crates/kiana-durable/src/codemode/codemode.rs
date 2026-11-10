//! codemode/codemode.ts：在 QuickJS 沙箱里跑模型写的 JavaScript，脚本挺过重启。
//!
//! 问题：脚本是内存里的 VM 状态。重启时若脚本在
//!
//! ```js
//! const results = await Promise.all(paths.map((path) => tools.read({ path })));
//! ```
//!
//! 里等最后几个读取，脚本与它做过的每次读取都会丢。
//!
//! 办法：确定性重放（kiana-codemode 的 `CodemodeSandboxBase.start()`）。VM 只在宿主递结果时前进；
//! 给定相同的 start 数据、时钟、seed 与同一顺序的结果，它做出相同的调用与参数。工具因此可安全
//! 重放：重启后 `execute()` 再跑一遍，用存下的 start 数据重新启动脚本，按当初得到结果的顺序把
//! 结果递给它——结果来自每次投递前写下的 journal。嵌套工具调用按 key（`c<VM 调用 id>`）挂回
//! 自己的任务，除非自己的 replay 策略要求重跑，否则永不跑第二遍；结果来自 `NestedCallDoc`。
//! 之后脚本继续实时运行。
//!
//! 文档（按 code mode 调用的任务作用域，随任务退休）：
//! - `kiana.codemode.run`：start 数据（VM 启动前写一次）与结局（调用决定它时与 store 写入在同一
//!   事务里写一次，之后的重跑直接返回它，不再起 VM）。
//! - `kiana.codemode.journal`：每投递一个结果一条，按投递顺序：VM 调用 ID、时钟值、VM 处理完后
//!   发出的调用数（确认后）、以及 global 的结局（没有别处存它）。
//!
//! 重跑会拒绝重放（不接新嵌套调用；早前接纳的嵌套调用仍可自行恢复，并被调用的清理中止）：
//! 本模块版本或 VM 指纹变了、deadline 过了、或脚本分叉（发出与 journal 不同的调用/顺序）。
//! 每种都以错误结果结束调用。
//!
//! # 在系统中的位置
//! 上层把 `create_codemode_extension()` 返回的扩展注册进 harness 的 registry：模型看到
//! `codemode` 工具与列出可用工具的 `codemode` 提示段；`create_codemode_tool()` 也可以单独注册。
//! 脚本里的 `tools.*` 调用经 `ToolExecutionApi::execute_tool()` 回到 harness，成为 key 为
//! `c<调用 id>` 的嵌套工具任务；本模块的文档写入都通过 `api.commit()`。
//!
//! # 执行流程（首次运行与重跑共用同一份文档）
//! 一次 code mode 调用可能被执行多次（例如进程重启后恢复）。每次尝试都从 `execute()` 开始，
//! 按 `kiana.codemode.run` 文档里的状态分流：
//!
//! 1. 解析脚本首行的 `// @options: {...}`，得到纯脚本以及 `max_output_tokens`/`timeout_ms`。
//! 2. 在一次提交里读 `run` 文档：已有 `result` 就直接返回它，不再启动 VM；没有 `start` 则用
//!    当前可调用工具、命名空间、全局函数与 store 初值写下 start 数据（首次运行）。
//! 3. 读 `journal`。有条目就重放：VM 重新执行脚本，每发出一个调用，`on_call()` 按调用 ID
//!    找到当初的记录，把记录的结果交还给它。已结算的嵌套调用按 key `c<调用 id>` 复用结果；
//!    未结算的由嵌套工具自己的 replay 策略决定（`Safe` 重跑，`Unsafe` 以 `interrupted` 结算）。
//! 4. 重放追平 journal 后转入实时：实时调用先等自己的发出计数被提交，再真正执行；结果按
//!    完成顺序追加进 journal，随后才投递给 VM。
//! 5. 脚本给出结局后，`complete()` 用一次提交同时写下 `result` 与脚本的 `store()` 写入；
//!    之后任何尝试都直接返回这个结局。
//!
//! # 状态与不变量
//! - journal 是只追加的写前日志：第 N 条投递对应结果号 `seq = N`，条目记下 VM 调用 `id`、
//!   投递时的 VM 时钟 `at`、VM 处理完后的累计调用数（确认后才补）以及 global 的调用与结局。
//! - 调用 id 由 VM 按发出顺序分配（1、2、……；tool 与 global 共用一个序列）。journal 的顺序
//!   是投递顺序，不是调用 id 顺序——并发调用谁先完成谁先入账。
//! - 实时调用只在 `call.id <= durable_issued` 时启动：它的“已发出”必须先落盘。这样重启后
//!   重放一定能看到这个调用，不会少投一个结果。
//! - `run`/`journal` 是任务作用域文档，随任务退休；`kiana.codemode.store` 是会话作用域、
//!   可 rewind、fork 按 fork 点取值的文档。
//!
//! # 并发模型
//! 脚本跑在 kiana-codemode 的沙箱里；VM 的 worker 线程有自己的单线程运行时。那条线程是
//! detach 的（终止只请求退出、不 join），脚本结束后可能仍在收尾。`on_started`/`on_call`/
//! `on_idle` 从那条线程同步回调，回调里只做短促的状态修改；需要 `await` 的工作（嵌套工具执行、
//! 文档提交、结果投递）都通过 `spawn()` 交给宿主运行时——spawn 到 worker 的运行时会在它退出时
//! 被丢掉。
//! 回调与任务共享的状态在 `Mutex<CallState>` 里，临界区内没有 `await`。
//! `Gate`/`Ready` 两个一次性闸门保证：指纹记录完成、run 句柄存好之前，任何调用都不会被投递
//! 或启动。
//!
//! # 失败模式
//! 重跑时若发现模块版本与 start 数据不符、沙箱指纹变了、或脚本分叉，则不重放，直接用一个
//! 错误结果结束调用（`decide()` 把它写进 `run` 文档）。deadline 在任何一次尝试开始时都可能
//! 已过，同样以错误结果结束。存储提交失败则本次调用以该 `BoxError` 结束，不写结果，也不投递
//! 未落盘的结果。宿主主动关闭沙箱（不是本调用中止）时同样不存结局：本次尝试结束，留给下一次
//! 重放。
//!
//! # 阅读顺序
//! `execute()` → `run_script()` → `on_call()` → `deliver_replayed()` → `pump()` →
//! `on_idle()` → `complete()`。`outcome_of()`/`text_of()` 决定脚本看到的值，
//! `format_result()`（在 `output.rs`）决定模型看到的内容，二者都不碰持久化。

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use async_trait::async_trait;

use kiana_ai::types::{
    ConstrainedSampling, ConstrainedSamplingConfig, GrammarVariants, TextContent, TextKind,
    UserContentBlock,
};
use kiana_chord::context::{AbortReason, AbortSignal, Context};
use kiana_codemode::declarations::to_codemode_identifier;
use kiana_codemode::runtime::execution::StartError;
use kiana_codemode::source::{CODEMODE_SOURCE_GRAMMAR, parse_codemode_source};
use kiana_codemode::types::{
    BoxFuture as CodemodeBoxFuture, CodemodeCallTarget, CodemodeDeclaredGlobal,
    CodemodeDeclaredTool, CodemodeDuring, CodemodeHostCall, CodemodeOutcome, CodemodeOutcomeErr,
    CodemodeOutcomeOk, CodemodeResult, CodemodeResultOk, CodemodeRun, CodemodeRunOptions,
    CodemodeStoreWrites,
};
use kiana_json::js_object::deep_equal;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use super::describe::{
    CodemodeNamespace, DEFAULT_CODEMODE_INLINE_BUDGET, codemode_description, codemode_section,
    namespace_of, sample_of,
};
use super::models::{
    CATALOG_GLOBALS, CODEMODE_INTERNAL_PREFIX, GENERATE_IMAGES_TOOL, MAX_CONCURRENT_MODEL_CALLS,
    catalog_call, create_model_tools, model_calls,
};
use super::output::{
    CallSummary, DEFAULT_MAX_OUTPUT_TOKENS, FormatResultOptions, Limits, OutputData, OutputFile,
    OutputFileResult, format_result,
};
use crate::env::{ExecutionEnv, FileContent};
use crate::harness::define::{define_extension, define_tool, section};
use crate::harness::tool::{NESTED_CALL_DOC, ToolTaskInput};
use crate::harness::types::{
    Extension, NestedToolExecutionResult, NestedToolOptions, OutputLimits, PromptSection,
    ReplayPolicy, ToolCaller, ToolExecuteFn, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration,
};
use crate::search::{SearchDocument, rank_documents};
use crate::types::{
    BoxError, CommitOptions, DocDefinition, DocTarget, DocToken, DocumentFork, DocumentHistory,
    DocumentSemantics, JsonObject, ScopeKind, SessionChange, Tx, define_doc,
};

/// 本模块里所有"把存下的结果变成脚本看到的东西"的部分的版本：结果映射、全局函数、start 数据。
/// 这些变化时把它加一，升级前启动的调用就不会被另一份代码重放。
pub const CODEMODE_MODULE_VERSION: i64 = 2;

/// 工具名。
pub const CODEMODE_TOOL_NAME: &str = "codemode";

/// `skip_serializing_if` 谓词：字段值为 `false` 时不写进 JSON（与 TS 里缺省的布尔标志等价）。
fn is_false(value: &bool) -> bool {
    !*value
}

/// JS `JSON.stringify` 的数字写法。
///
/// 存进文档的 `f64` 都是 JS 的 number：整数值写成 `1760000000000`，不是 serde_json 的
/// `1760000000000.0`。超出 JS 安全整数区间的值保留 serde 的写法（TS 会写成指数形式，本模块
/// 的字段——时钟、seed、token 预算——不会到那里）。
///
/// 本文件把 `Start` 的 `startedAt`/`seed`/`deadline`/`maxOutputTokens`、journal 的 `at` 与
/// details 的 `durationMs` 都走这里；改动会让历史文档的字节表示与 TS 写下的不一致。测试
/// `stores_document_numbers_the_way_the_ts_writes_them` 锁定了 `startedAt`/`seed`/
/// `maxOutputTokens`、journal 的 `at` 与 details 的 `durationMs` 的整数写法；`deadline` 也走
/// 同一 helper，但该测试的脚本没有 `@options`（deadline 为 `None`），并未断言它的字节写法。
mod js_number {
    use serde::{Serialize, Serializer};

    struct JsNumber(f64);

    impl Serialize for JsNumber {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let value = self.0;
            if value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
                serializer.serialize_i64(value as i64)
            } else {
                serializer.serialize_f64(value)
            }
        }
    }

    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        JsNumber(*value).serialize(serializer)
    }

    pub fn serialize_option<S: Serializer>(
        value: &Option<f64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => JsNumber(*value).serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    /// 手工构造文档条目时的同一写法。
    pub fn json(value: f64) -> serde_json::Value {
        if value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
            serde_json::Value::from(value as i64)
        } else {
            serde_json::Value::from(value)
        }
    }
}

/// 脚本看到的一个工具：`tools.<jsName>`。
///
/// `structured`：工具声明了 `structuredOutputSchema`，所以带值的错误结果也以该值 resolve。
/// `hidden`：本扩展的内部工具（`kiana.codemode.*`），脚本只能通过 `models.classify()` 之类的全局
/// 函数到达；永不出现在 `tools`、`ALL_TOOLS` 或工具搜索里。
///
/// 这些条目连同 `Start` 一起存进文档；重跑时脚本看到的是首次运行时的声明，而不是当前
/// registry 的样子（`run_script()` 只用 start 数据构造沙箱选项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredTool {
    /// 工具名。
    pub name: String,
    /// 脚本里的标识符。
    pub js_name: String,
    /// 工具样例（描述加声明）。
    pub description: String,
    /// 声明了输出 schema。
    #[serde(default, skip_serializing_if = "is_false")]
    pub structured: bool,
    /// 本扩展的内部工具。
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

/// start 数据里的一个命名空间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartNamespace {
    /// 名字。
    pub name: String,
    /// 说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 额外指示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// 成员工具名（原始名）。
    pub tools: Vec<String>,
}

/// start 数据里的一个全局函数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartGlobal {
    /// 名字。
    pub name: String,
    /// `execute` 收到参数数组。
    pub spread: bool,
}

/// 一次调用开始时的数据；重跑从它开始起 VM。
///
/// 它是重建沙箱的唯一配置快照：时钟起点、随机种子、工具/全局声明、`max_output_tokens` 与
/// store 初值都在首次运行时冻结并写进 `kiana.codemode.run`；重放的另一半输入是 `journal`
/// （历史结果与投递顺序）。沙箱声明与 store 初值取自这里，而不是当前 registry/store。
/// 但实际实现仍取自当前环境：工具调用经 `call_tool()` 的 `ToolExecutionApi::execute_tool()`
/// 落成嵌套任务，由 harness 按当前 `agent.callable` 解析；全局调用经 `call_global()` 在当前的
/// `CodemodeToolOptions::globals` 中查找（发现类/目录类全局由本模块内部回答）。工具被移除或
/// 改实现时，未记录/未结算的调用会失败或返回不同结果，脚本可能因此走到另一条路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Start {
    /// 模块版本。
    pub version: i64,
    /// 起始时钟（epoch 毫秒）。
    #[serde(serialize_with = "js_number::serialize")]
    pub started_at: f64,
    /// 随机种子。
    #[serde(serialize_with = "js_number::serialize")]
    pub seed: f64,
    /// VM 指纹，首次运行在任何调用被接纳之前记录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// 脚本设了 `timeout_ms` 时的 `startedAt + timeout_ms`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub deadline: Option<f64>,
    /// 脚本看到的工具。
    pub tools: Vec<DeclaredTool>,
    /// 工具所属的命名空间，`describeNamespace()`/`searchTools()` 看到的样子。
    pub namespaces: Vec<StartNamespace>,
    /// 全局函数。
    pub globals: Vec<StartGlobal>,
    /// 脚本 options 行里的 `max_output_tokens`，或默认值。
    #[serde(serialize_with = "js_number::serialize")]
    pub max_output_tokens: f64,
    /// `store()` 的初值。
    pub store: JsonObject,
}

/// `kiana.codemode.run` 文档的内容。
///
/// 三个字段都遵循“缺失才写入”：`start` 只在首次运行时写，`outputDir` 只在第一次解析输出目录
/// 时写，`result` 只在结局第一次决定时写。这样并发的两次提交不会互相覆盖，先落盘者胜。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunState {
    /// start 数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Start>,
    /// 结果文件（完整输出、图片）所在目录；只创建一次，重跑写回同样的文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    /// 结局，只决定一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonObject>,
}

/// `kiana.codemode.run`：start 数据与结局。
///
/// 任务作用域：任务退休时整份文档消失，所以永不 checkpoint（`tests/codemode_checkpoints.rs`
/// 断言 `checkpoint_when` 恒为 false）。`execute()` 在启动 VM 之前先读这里的 `result`，
/// 重试因此不会重复跑已经决定结局的脚本。
pub static CODEMODE_RUN_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition::new(
        "kiana.codemode.run",
        1,
        DocumentSemantics::TASK,
    ))
});

/// journal 里 global 调用的记录。
///
/// 全局函数可能有副作用（计数、写外部状态），所以重放时直接回放这里存下的结局，不再调用
/// 宿主实现（`tests/codemode.rs` 的 `journals_globals_with_their_results_and_hands_a_replay_the_recorded_ones`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalGlobal {
    /// 名字。
    pub name: String,
    /// 参数；`undefined` 省略，与显式 `null` 不同。
    ///
    /// 读回时 `"args": null` 必须是 `Some(Null)`：serde 的 `Option` 会把它当成缺失，重放便认为
    /// 脚本这次改成了无参数调用。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "kiana_codemode::types::deserialize_optional_value"
    )]
    pub args: Option<JsonValue>,
    /// 结局。
    pub outcome: CodemodeOutcome,
}

/// journal 的一条。
///
/// 条目按投递顺序追加；结果号是“位置 + 1”。`at` 是投递时使用的 VM 时钟读数，重放必须用同一个
/// 值投递，脚本里的 `Date.now()` 才会与首次运行一致（`keeps_the_scripts_clock_and_randomness_across_a_restart`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    /// VM 的调用 ID。它的结果是本条的位置 + 1（`seq`）。
    pub id: u64,
    /// VM 处理本结果时的 VM 时钟。
    #[serde(serialize_with = "js_number::serialize")]
    pub at: f64,
    /// VM 处理本结果后发出的调用数；确认前没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_after: Option<u64>,
    /// global 的调用与结局；工具的结果是它嵌套调用的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<JournalGlobal>,
}

/// `kiana.codemode.journal` 文档的内容。
///
/// `firstIssued` 记录脚本首次运行（任何结果之前）发出的调用数，是重放核对分叉的基准；
/// 每条 `issuedAfter` 对应一个结果号。两者都由 `pump()` 在提交里补齐，从不覆盖已有值。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalState {
    /// 条目。
    pub entries: Vec<JournalEntry>,
    /// 脚本首次运行（任何结果之前）发出的调用数；确认前没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_issued: Option<u64>,
}

/// `kiana.codemode.journal`：每次投递一条结果。
///
/// 初始内容是 `{"entries": []}`。正常路径只追加条目并补写 `issuedAfter`，不重排、不删除。
pub static CODEMODE_JOURNAL_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new("kiana.codemode.journal", 1, DocumentSemantics::TASK).with_initial(
            |_| {
                let mut value = JsonObject::new();
                value.insert("entries".to_owned(), JsonValue::Array(Vec::new()));
                value
            },
        ),
    )
});

/// 脚本用 `store()` 保留、用 `load()` 读取的东西，按会话。可 rewind，fork 时按 fork 点拷贝，
/// 所以 fork 看到的是它那条条目提交时的值。
///
/// `checkpoint_when` 在累计 64 个 delta 后让下一次变化存成完整 base；任务作用域的 run/journal
/// 文档则永不 checkpoint（`tests/codemode_checkpoints.rs` 覆盖）。
pub static CODEMODE_STORE_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.codemode.store",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert(
                "values".to_owned(),
                JsonValue::Object(serde_json::Map::new()),
            );
            value
        })
        .with_checkpoint_when(|_value, _ops, info| info.deltas_since_base >= 64),
    )
});

/// 脚本做过的一次工具调用，按 code mode 调用 details 的样子。
///
/// `args`/`error` 是预览：分别裁到 200/500 个 UTF-16 码元，避免 details 随结果无限变大。
/// details 只在结果里写一次；脚本运行期间的 UI 读的是 live 嵌套工具表（见
/// `CodemodeToolDetails`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeNestedCall {
    /// VM 给调用的编号；嵌套调用 ID 是 `<callId>/c<id>`。
    pub id: u64,
    /// 工具名。
    pub name: String,
    /// 参数的紧凑 JSON，裁剪过。
    pub args: String,
    /// 状态。
    pub status: NestedCallStatus,
    /// 耗时。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "js_number::serialize_option"
    )]
    pub duration_ms: Option<f64>,
    /// 错误文本，裁剪过。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 嵌套调用的状态：`running | ok | error | cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedCallStatus {
    /// 运行中。
    Running,
    /// 正常返回。
    Ok,
    /// 错误。
    Error,
    /// 脚本结束时仍未完成。
    Cancelled,
}

impl NestedCallStatus {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            NestedCallStatus::Running => "running",
            NestedCallStatus::Ok => "ok",
            NestedCallStatus::Error => "error",
            NestedCallStatus::Cancelled => "cancelled",
        }
    }
}

/// code mode 调用的 details，放在结果里：它做过的工具调用，按顺序，带状态。对全局函数的调用
/// 不列出。脚本运行期间 UI 从 `kiana.live.nestedTools` 看嵌套调用，所以这份列表只随结果写一次。
///
/// `fullOutputPath` 由 `complete()` 在完整输出被写进临时文件时补上；`details()` 生成这份结构时
/// 还不知道要不要写文件，所以那里恒为 `None`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeToolDetails {
    /// 调用。
    pub calls: Vec<CodemodeNestedCall>,
    /// 输出被裁到 token 预算时，完整文本所在的临时文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

// details 会随工具结果进会话记录，所以参数与错误只保留预览；长度按 UTF-16 码元计，与 TS 的
// `String#slice` 一致（见 `cut`）。
const ARGS_PREVIEW_CHARS: usize = 200;
const ERROR_PREVIEW_CHARS: usize = 500;

/// TS 的 `cut`：超过 `max` 时保留前 `max - 3` 个 UTF-16 码元再加 `...`。
///
/// 按 UTF-16 码元而不是字节/`char` 计数，是为了与 TS 的 `slice` 对齐（例如 emoji 占两个码元）。
/// `max < 3` 时 `saturating_sub` 会截成 0 个码元，只留下 `...`。
fn cut(text: &str, max: usize) -> String {
    if kiana_chord::delta::index::utf16_len(text) > max {
        format!(
            "{}...",
            kiana_chord::delta::index::owned_slice(text, 0, Some(max.saturating_sub(3)))
        )
    } else {
        text.to_owned()
    }
}

/// 一个脚本作为顶层全局函数调用的函数；它的结果入 journal。
///
/// 调用与结局由本模块记进 journal；重放时，已经记进 journal 的调用直接回答记录值，不再执行
/// 一次。注意只有结果已提交的调用才有这个保证：在结果提交前进程崩溃的 global，重放时会再执行
/// 一次（journal 里没有它，按新调用处理）。脚本能调用的全局名来自 start 数据；执行时
/// `call_global()` 在当前的 `CodemodeToolOptions::globals` 里查找实现。发现类与目录类全局由
/// 本模块内部回答，不走这个 trait。
///
/// `execute` 返回 `None` 表示 JS 的 `undefined`，`Some(Value::Null)` 是 `null`。
pub trait CodemodeGlobal: Send + Sync {
    /// 全局名。
    fn name(&self) -> &str;
    /// `execute` 收到全部调用参数的数组，而不是第一个。
    fn spread(&self) -> bool {
        false
    }
    /// 执行；`args` 是脚本给的 JSON 值（`None` 即 `undefined`）。
    fn execute<'a>(
        &'a self,
        args: Option<JsonValue>,
        api: Arc<dyn ToolExecutionApi>,
        context: Arc<dyn Context>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsonValue>, BoxError>> + Send + 'a>>;
}

/// 能起确定性 run 的沙箱（TS 的 `CodemodeSandboxBase`）。
///
/// 这里抽成 trait 是为了让宿主注入不同实现（`tests/codemode.rs` 的 `CountingSandbox` 包装真实
/// 沙箱并计数启动次数），同时把 `CodemodeSandboxBase`（执行核心）与 `CodemodeSandbox`
/// （worker 线程里的 QuickJS 实现）适配成同一种 trait 对象。`start()` 同步返回句柄；结果与回调
/// 都通过 `CodemodeRun` 交互。
pub trait CodemodeSandbox: Send + Sync {
    /// 启动一次 run。
    fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, StartError>;
}

impl CodemodeSandbox for kiana_codemode::CodemodeSandboxBase {
    fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, StartError> {
        kiana_codemode::CodemodeSandboxBase::start(self, code, options)
    }
}

impl CodemodeSandbox for kiana_codemode::CodemodeSandbox {
    fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, StartError> {
        kiana_codemode::CodemodeSandbox::start(self, code, options)
    }
}

/// code mode 工具的输入：`{ code }`。
///
/// 只有 `code` 一个字符串字段；`create_codemode_tool` 读不到时用空串（随后
/// `parse_codemode_source` 报“需要非空源码”的错误）。
pub type CodemodeToolInput = JsonValue;

/// code mode 工具的选项。
///
/// 构造后被 `Arc` 共享给每次执行：`create_codemode_tool` 与 `create_codemode_extension` 各持一份
/// 克隆，因此同一个沙箱实例会被同一扩展的所有调用复用。
#[derive(Clone)]
pub struct CodemodeToolOptions {
    /// 跑脚本的沙箱：Node 上是 `CodemodeSandbox`，Cloudflare 上是 `RemoteCodemodeSandbox`。
    pub sandbox: Arc<dyn CodemodeSandbox>,
    /// 按名字前缀分组的工具，例如 MCP 服务器，供 `codemode` 段与工具搜索使用。
    pub namespaces: Option<Vec<CodemodeNamespace>>,
    /// `codemode` 段里工具声明可用的估算 token 数。默认 3000。
    pub inline_budget: Option<f64>,
    /// 给脚本 `models.*`：模型目录、classifier 与图片生成。默认 true。
    pub models: Option<bool>,
    /// 宿主函数，脚本作为全局函数调用。
    pub globals: Option<Vec<Arc<dyn CodemodeGlobal>>>,
}

impl CodemodeToolOptions {
    /// 只给沙箱。
    pub fn new(sandbox: Arc<dyn CodemodeSandbox>) -> Self {
        CodemodeToolOptions {
            sandbox,
            namespaces: None,
            inline_budget: None,
            models: None,
            globals: None,
        }
    }

    /// `models` 是否开启（缺省 true）。
    pub fn models_enabled(&self) -> bool {
        self.models != Some(false)
    }
}

/// 受脚本自己的 token 预算约束的输出；Harness 的上限只拦失控。
///
/// 同一组值既传给 `format_result()`，也写进工具的 `output_limits`，避免两处上限不一致。
const OUTPUT_LIMITS: Limits = Limits {
    max_bytes: 4 * 1024 * 1024,
    max_lines: 100_000,
};

/// 找工具的全局函数，由 start 数据回答，所以重放看到的与首次运行一致。
const DISCOVERY_GLOBALS: [&str; 3] = ["searchTools", "describeTool", "describeNamespace"];
/// `searchTools(query)` 不给 `limit`（或给 `null`）时返回的条数。
const DEFAULT_SEARCH_LIMIT: u64 = 8;

/// 空 JSON 对象；调用参数缺省或为 `null` 时的统一归一值（与 TS 的 `{}` 一致）。
fn empty_object() -> JsonValue {
    JsonValue::Object(serde_json::Map::new())
}

/// 把错误消息包成 `is_error` 的工具结果：本次 code mode 调用因此以 `Ok` 返回给 harness（模型
/// 看到一条错误结果），而不是以 `Err` 结束（写下结局的提交失败时仍以 `BoxError` 结束）。它不会
/// 投递给脚本：它只用于 `decide()`/`diverge()` 构造宿主侧的结局（版本不符、deadline 已过、
/// 指纹变化、分叉）；这些路径里 VM 要么还没启动，要么已经结束或被中止。
fn error_result(message: impl Into<String>) -> ToolExecutionResult {
    ToolExecutionResult {
        output: Some(vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: message.into(),
            text_signature: None,
        })]),
        is_error: Some(true),
        ..Default::default()
    }
}

/// 工具结果映射：带 schema 的 resolve 其值，也 resolve 带该值的错误结果；不带 schema 的
/// resolve 其文本（忽略图片）。其他错误以文本 reject。
///
/// 错误消息的优先级：不带 schema 的工具先用结果文本（可为空串），为空时退回诊断里的 error
/// 消息，仍为空则用 `Tool <name> failed`；带 schema 的工具没有结果文本，直接从诊断开始。
/// `properties.structuredOutput` 只出现在不带 schema 的错误结果里（总是附带文本，可能为空串）。
/// 带 schema 且有值的错误结果在早退分支直接 resolve 为 `Ok`；没有值时 `properties` 为 `None`，
/// 脚本不能通过 `error.structuredOutput` 读到它。
fn outcome_of(name: &str, structured: bool, result: &NestedToolExecutionResult) -> CodemodeOutcome {
    let value: Option<JsonValue> = if structured {
        result.structured_output.clone()
    } else {
        Some(JsonValue::String(text_of(
            result.structured_output.as_ref(),
        )))
    };
    if !result.is_error || (structured && value.is_some()) {
        return CodemodeOutcome::Ok(CodemodeOutcomeOk { ok: true, value });
    }
    let message = if structured {
        String::new()
    } else {
        value
            .as_ref()
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == crate::harness::types::DiagnosticSeverity::Error
        })
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let message = if !message.is_empty() {
        message
    } else if !diagnostics.is_empty() {
        diagnostics
    } else {
        format!("Tool {name} failed")
    };
    CodemodeOutcome::Err(CodemodeOutcomeErr {
        ok: false,
        message,
        properties: value.map(|value| {
            let mut properties = indexmap::IndexMap::new();
            properties.insert("structuredOutput".to_owned(), value);
            properties
        }),
    })
}

/// 不带 schema 的工具折叠后的文本：字符串、一张图片，或文本与图片块的数组。
///
/// 数组只保留 `type == "text"` 的块并用换行连接；单独的图片块是对象，不是字符串也不是数组，
/// 返回空串——脚本拿不到图片数据。
fn text_of(value: Option<&JsonValue>) -> String {
    match value {
        Some(JsonValue::String(text)) => text.clone(),
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                JsonValue::Object(map)
                    if map.get("type").and_then(JsonValue::as_str) == Some("text") =>
                {
                    map.get("text")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// 结构化的 JSON 相等；键序无关。`undefined` 只与 `undefined` 相等（`null` 是一个值）。
///
/// 重放校验用它比较调用参数：脚本把 `f(null)` 改成 `f()` 时必须判为不同（serde 的 `Option`
/// 恰好能表达这个区别）。数字按 f64 比较（`1` 与 `1.0` 相等），与 JS 的 `===` 一致。
fn same_json(left: Option<&JsonValue>, right: Option<&JsonValue>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => deep_equal(left, right),
        _ => false,
    }
}

/// `QueueItem` 的克隆载体（journal 提交需要跨 `await` 持有）。
fn queue_item_clone(item: &QueueItem) -> QueueItem {
    QueueItem {
        id: item.id,
        at: item.at,
        outcome: item.outcome.clone(),
        global: item.global.clone(),
    }
}

/// `describe(call)`：`tools.name(args)` 或 `name(args)`。
///
/// 只用于分叉错误消息（例如 `call 3 is tools.x({"a":1}), but was a call to y`），参数用紧凑
/// JSON；`undefined` 参数显示为空括号。
fn describe_call(call: &CodemodeHostCall) -> String {
    let prefix = if call.target == CodemodeCallTarget::Tool {
        "tools."
    } else {
        ""
    };
    let args = match &call.args {
        Some(args) => kiana_json::js_object::stringify(args),
        None => String::new(),
    };
    format!("{prefix}{}({args})", call.name)
}

/// 一次 code mode 工具执行：首次运行，或重启后重放 journal 的重跑。
///
/// 生命周期：`execute()` 准备 start 数据 → `run_script()` 启动沙箱 → 沙箱回调驱动重放/实时 →
/// 结局由 `complete()` 或 `decide()` 提交。`Arc<Self>` 会被 VM 回调与任意数量的宿主任务共享，
/// 所以字段要么是不可变配置，要么自带同步（`Mutex`/原子量/`OnceCell`）。
struct CodemodeCall {
    options: Arc<CodemodeToolOptions>,
    api: Arc<dyn ToolExecutionApi>,
    context: Arc<dyn Context>,
    /// 宿主运行时。VM 的 worker 线程有自己的单线程运行时，且是 detach 的（不 join，脚本结束后
    /// 可能仍在收尾）；沙箱回调（`onStarted`/`onCall`/`onIdle`）从那条线程进来，必须把工作
    /// 交回宿主运行时，否则任务会随 worker 的运行时一起被丢掉，嵌套调用永远不落定。
    runtime: tokio::runtime::Handle,
    /// `AbortController` 的对应物。
    controller: AbortSignal,
    /// 指纹检查完才打开；之前不投递、不接纳任何调用。
    gate: Gate,
    /// run 句柄已存好（回调可能先于 `sandbox.start()` 返回就触发）。
    ready: Ready,
    run: Mutex<Option<CodemodeRun>>,
    state: Mutex<CallState>,
    /// 结果文件目录，只解析一次（包括失败）。
    output_dir: tokio::sync::OnceCell<Result<String, String>>,
}

/// 指纹闸门：打开后所有等待者立刻通过。
///
/// 沙箱会在脚本运行前回调 `on_started` 交出指纹，此时不能投递任何结果、也不能启动任何调用；
/// 等待者在这里挂起，`open()` 只把标志置位一次并广播。`notify_waiters()` 对“已创建但还没
/// poll”的 `Notified` 也保证送达（tokio 1.54 的 `Notify` 语义），所以先复查标志再等待不会
/// 丢唤醒。
struct Gate {
    open: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Gate {
    fn new() -> Self {
        Gate {
            open: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn open(&self) {
        if !self.open.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) {
        if self.open.load(Ordering::SeqCst) {
            return;
        }
        // 先建 `notified()` 再复查：即使置位发生在两次检查之间，`notify_waiters()` 也会让这个
        // 已创建（尚未 poll）的 future 立刻完成。
        let notified = self.notify.notified();
        if self.open.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// run 句柄闸门（见 [`CodemodeCall::ready`]）。
///
/// 沙箱的 `on_started` 回调可能在 `sandbox.start()` 返回之前触发；`run` 句柄要等返回后才存进
/// `CodemodeCall`。等待者（`check_fingerprint`、`pump`、`release`）用这个闸门等到句柄可用。
struct Ready {
    set: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Ready {
    fn new() -> Self {
        Ready {
            set: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn set(&self) {
        if !self.set.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) {
        if self.set.load(Ordering::SeqCst) {
            return;
        }
        // 与 `Gate::wait()` 相同的“先建 future 再复查”模式。
        let notified = self.notify.notified();
        if self.set.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// 一个已完成的实时调用，等 journal 提交。
///
/// `pump()` 先把它连同 `acks` 提交进 journal，成功后才投递给 VM（`tests/codemode.rs` 的
/// `delivers_a_result_only_after_its_journal_commit` 锁定这个顺序）。`at` 是完成时的宿主时钟，
/// 写入 journal 后成为重放时使用的 VM 时钟读数。
#[derive(Clone)]
struct QueueItem {
    id: u64,
    at: f64,
    outcome: CodemodeOutcome,
    global: Option<JournalGlobal>,
}

/// 一次执行的可变状态；回调与异步任务都改它，所以放在互斥量里。
///
/// 字段可以分成四组读：
/// - 重放：`start`、`replayed`、`journal_length`、`next_replayed`、`replaying`、`rerun`、
///   `first_issued`、`durable_issued`、`replay_index`、`ready`、`held`。`replaying` 表示还在
///   逐条投递 journal；`ready` 是“嵌套结果已到、可以投递”的重放调用；`held` 是 journal 里没有、
///   要等重放结束才能启动的调用。
/// - 实时：`released`（首批调用已放行）、`waiting`（等调用数提交）、`queue`（已完成、等
///   journal 提交）、`acks`（待写入 `firstIssued`/`issuedAfter` 的调用数）、`committing`
///   （同一时刻只允许一次提交在飞）。
/// - 结束：`ended`（已经有决定的工具结果）、`failure`（存储或内部错误）。
/// - 展示与配额：`model_calls`/`model_waiters`（模型调用并发上限）、`generated_images`、
///   `rows`（details 的行）。
///
/// 不变量：`durable_issued` 与 `issued` 单调不减；`journal_length` 等于已提交进 journal 的条目数
/// （重放开始时是读到的 journal 长度）；`next_replayed` 只在真正投递下一条时前进（投递失败会置
/// `failure`，重放不再继续）。
struct CallState {
    start: Option<Start>,
    /// 本次启动时 journal 的内容；重放只投递这些。
    replayed: Vec<JournalEntry>,
    /// journal 里的条目数（重放 + 实时）；下一次投递是 `length + 1`。
    journal_length: usize,
    /// 已投递的重放条目数。
    next_replayed: usize,
    replaying: bool,
    /// 早前一次尝试跑过本调用：它的嵌套调用 key 可能已存在。
    rerun: bool,
    /// 实时调用是否可以启动；之前一律 held（见 `release()`）。
    released: bool,
    /// 早前一次尝试在脚本首次运行后记录的调用数。
    first_issued: Option<u64>,
    /// 已知已提交的最大调用数：不超过它的调用可以启动。
    durable_issued: u64,
    /// 等自己的调用数被提交的实时调用。
    waiting: Vec<CodemodeHostCall>,
    /// 每个重放调用 ID 的 journal 下标。
    replay_index: HashMap<u64, usize>,
    /// VM 到此刻发出的调用数；它的调用 ID 是 1、2、……
    issued: u64,
    /// 重放：journal 里调用的结果，按 journal 顺序等待投递。
    ready: HashMap<u64, CodemodeOutcome>,
    /// 重放：journal 没有的调用，重放结束后才被接纳。
    held: Vec<CodemodeHostCall>,
    /// 实时：已完成、等 journal 提交的调用。
    queue: Vec<QueueItem>,
    /// 要记录的已确认调用数，按结果号（0：脚本首次运行）。
    acks: BTreeMap<u64, u64>,
    committing: bool,
    /// 调用已决定脚本以外的结局（分叉、不匹配）或失败时置位。
    ended: Option<ToolExecutionResult>,
    failure: Option<BoxError>,
    /// 在飞的 `models.classify()`/`generateImages()` 调用，以及等槽位的。
    model_calls: usize,
    model_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
    /// `models.generateImages()` 返回的图片数，用来发现脚本从没展示它们。
    generated_images: usize,
    /// 脚本的工具调用，供 `details` 使用，按 VM 调用 ID。
    rows: BTreeMap<u64, CodemodeNestedCall>,
}

impl CallState {
    fn empty() -> Self {
        CallState {
            start: None,
            replayed: Vec::new(),
            journal_length: 0,
            next_replayed: 0,
            replaying: false,
            rerun: false,
            released: false,
            first_issued: None,
            durable_issued: 0,
            waiting: Vec::new(),
            replay_index: HashMap::new(),
            issued: 0,
            ready: HashMap::new(),
            held: Vec::new(),
            queue: Vec::new(),
            acks: BTreeMap::new(),
            committing: false,
            ended: None,
            failure: None,
            model_calls: 0,
            model_waiters: Vec::new(),
            generated_images: 0,
            rows: BTreeMap::new(),
        }
    }
}

/// 取互斥量；锁中毒（持锁线程 panic）时继续用内层值，而不是把 panic 传播给每个回调。
///
/// 本文件里临界区都很短（只改 `CallState` 或读 `run` 句柄），且没有跨 `await` 持锁。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一次提交回调返回值的擦除 future。
type ValueFuture<'t, T> = Pin<Box<dyn Future<Output = Result<T, BoxError>> + Send + 't>>;

/// 把 `async (tx) => value` 的提交回调包成 [`SessionChange`]。
struct FnChange<F> {
    run: F,
}

#[async_trait]
impl<F, T> SessionChange for FnChange<F>
where
    F: for<'t> Fn(&'t dyn Tx) -> ValueFuture<'t, T> + Send + Sync,
    T: Send + 'static,
{
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let value = (self.run)(tx).await?;
        Ok(Box::new(value))
    }
}

/// 在 `api.commit()` 里跑一个闭包并取回它的值（TS 的 `api.commit(async (tx) => ...)`）。
///
/// `FnChange` 把“async 闭包”适配成 `SessionChange`：提交把返回值装箱成 `Any` 带回，这里再
/// downcast 成 `T`。类型对不上说明实现违约，转成错误而不是 panic。
async fn commit_value<F, T>(
    api: &dyn ToolExecutionApi,
    context: &dyn Context,
    run: F,
) -> Result<T, BoxError>
where
    F: for<'t> Fn(&'t dyn Tx) -> ValueFuture<'t, T> + Send + Sync,
    T: Send + 'static,
{
    let change = FnChange { run };
    let boxed = api
        .commit(&change, context, CommitOptions::default())
        .await?;
    match boxed.downcast::<T>() {
        Ok(value) => Ok(*value),
        Err(_) => Err("commit() did not return its value".into()),
    }
}

/// 从文档里取一个字段的引用；需要跨 `await` 持有或修改时由调用方 `clone()`。
fn field<'a>(value: &'a JsonObject, key: &str) -> Option<&'a JsonValue> {
    value.get(key)
}

/// JSON 值 → `JsonObject`（`Object.entries` 的视图）；非对象返回 `None`，对象会拷贝键值。
fn as_object(value: &JsonValue) -> Option<JsonObject> {
    match value {
        JsonValue::Object(map) => Some(
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// `JsonObject` → JSON 值对象；与 `as_object` 互逆。
fn object_value(object: JsonObject) -> JsonValue {
    JsonValue::Object(object.into_iter().collect())
}

impl CodemodeCall {
    fn new(
        options: Arc<CodemodeToolOptions>,
        api: Arc<dyn ToolExecutionApi>,
        context: Arc<dyn Context>,
    ) -> Self {
        CodemodeCall {
            options,
            api,
            context,
            runtime: tokio::runtime::Handle::current(),
            controller: AbortSignal::new(),
            gate: Gate::new(),
            ready: Ready::new(),
            run: Mutex::new(None),
            state: Mutex::new(CallState::empty()),
            output_dir: tokio::sync::OnceCell::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, CallState> {
        lock(&self.state)
    }

    /// 在宿主运行时上起一个任务（见 [`CodemodeCall::runtime`]）。
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future);
    }

    /// run 是否已有结局。`CodemodeRun::finished` 在结果决定时置位，早于 `result` future 完成，
    /// 所以这里能比 `result` 更早挡住后续回调。
    fn run_finished(&self) -> bool {
        lock(&self.run).as_ref().is_some_and(CodemodeRun::finished)
    }

    /// 什么都不该再发生：run 有结局，或调用已决定/失败。
    ///
    /// 与 `run_finished()` 不完全等价：`ended`/`failure` 是宿主侧的决定，可能早于 VM 停下；
    /// 两者的并集才是“别再接纳新工作”。
    fn stopped(&self) -> bool {
        let state_stopped = {
            let state = self.state();
            state.ended.is_some() || state.failure.is_some()
        };
        state_stopped || self.run_finished()
    }

    /// 记录第一个失败并中止 VM。错误本身由 `run_script()` 在结果 future 返回后取回并传播，
    /// 所以调用方看到的是根因，而不是中止导致的 `Aborted`。
    fn fail(&self, error: BoxError) {
        let mut state = self.state();
        if state.failure.is_some() {
            return;
        }
        let message = error.to_string();
        state.failure = Some(error);
        drop(state);
        self.controller
            .abort(Some(AbortReason::error(std::io::Error::other(message))));
    }

    /// 取 start 数据；`execute()` 在启动沙箱之前就把它放进 `state`，所以回调里调用时必定存在。
    fn start_data(&self) -> Start {
        self.state()
            .start
            .clone()
            .expect("the call has its start data")
    }

    /// 首次运行或重跑的入口：解析源码与 start 数据，决定这次是新跑还是重放，再启动沙箱。
    ///
    /// 阶段：
    /// 1. 解析 `// @options:` 行，得到去掉 options 行的脚本与 `max_output_tokens`/`timeout_ms`。
    /// 2. 从 `api.agent()` 取当前可调用工具，构造 start 数据；准备一个提交闭包：已有 `result`
    ///    直接返回，否则在没有 `start` 时写下它；同一次提交里读出 journal 的条目与 `firstIssued`。
    /// 3. 把阶段 2 读到的 journal 与 `firstIssued` 装进 `CallState`，初始化重放位置与
    ///    `durable_issued`。
    /// 4. 版本与 deadline 检查；订阅外层 abort 信号后调用 `run_script()`。
    ///
    /// 错误：解析失败、提交失败以 `BoxError` 返回；重放不兼容以“已决定的错误结果”返回（不是 Err）。
    async fn execute(self: &Arc<Self>, source: &str) -> Result<ToolExecutionResult, BoxError> {
        // 阶段 1：拆出脚本与首行 options。空源码/非法 options 在这里直接失败。
        let parsed = parse_codemode_source(source).map_err(|error| Box::new(error) as BoxError)?;
        let code = parsed.code;
        let source_options = parsed.options;
        // 阶段 2：用当前 registry 快照构造 start 数据。`declared` 含内部工具（`hidden` 标出，
        // 沙箱声明时过滤掉；`call_tool` 还要用它查结构化标志），`callable` 去掉内部工具（只用于
        // 分命名空间）。
        let agent = self.api.agent(&*self.context).await?;
        let declared: Vec<DeclaredTool> = agent
            .callable
            .iter()
            .map(|tool| DeclaredTool {
                name: tool.tool.name.clone(),
                js_name: to_codemode_identifier(&tool.tool.name),
                description: sample_of(tool),
                structured: tool.structured_output_schema.is_some(),
                hidden: tool.tool.name.starts_with(CODEMODE_INTERNAL_PREFIX),
            })
            .collect();
        let callable: Vec<&ToolRegistration> = agent
            .callable
            .iter()
            .filter(|tool| !tool.tool.name.starts_with(CODEMODE_INTERNAL_PREFIX))
            .collect();
        let models = self.options.models_enabled();
        let mut globals: Vec<StartGlobal> = DISCOVERY_GLOBALS
            .iter()
            .map(|name| StartGlobal {
                name: (*name).to_owned(),
                spread: true,
            })
            .collect();
        if models {
            for name in CATALOG_GLOBALS {
                globals.push(StartGlobal {
                    name: name.to_owned(),
                    spread: true,
                });
            }
            for (name, _) in model_calls() {
                globals.push(StartGlobal {
                    name: (*name).to_owned(),
                    spread: true,
                });
            }
        }
        for global in self.options.globals.iter().flatten() {
            globals.push(StartGlobal {
                name: global.name().to_owned(),
                spread: global.spread(),
            });
        }
        let namespaces_options = self.options.namespaces.clone().unwrap_or_default();
        let mut namespaces: Vec<StartNamespace> = Vec::new();
        for tool in &callable {
            let Some(namespace) = namespace_of(&tool.tool.name, &namespaces_options) else {
                continue;
            };
            match namespaces
                .iter_mut()
                .find(|candidate| candidate.name == namespace.name)
            {
                Some(entry) => entry.tools.push(tool.tool.name.clone()),
                None => namespaces.push(StartNamespace {
                    name: namespace.name.clone(),
                    description: namespace.description.clone(),
                    instructions: namespace.instructions.clone(),
                    tools: vec![tool.tool.name.clone()],
                }),
            }
        }
        let now = self.api.now();
        let seed = random_u32() as f64;
        let max_output_tokens = source_options
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        let timeout = source_options.timeout_ms;
        let task_id = self.api.task_id();
        let conversation_id = self.api.conversation_id();
        let api = Arc::clone(&self.api);
        let context = Arc::clone(&self.context);
        let start_tools = declared.clone();
        let start_namespaces = namespaces;
        let start_globals = globals;
        // 阶段 2（续）：一次提交里读出“已有结局还是 start 数据”，顺便在首次运行时冻结 start。
        let found = commit_value(api.as_ref(), context.as_ref(), move |tx| {
            let tools = start_tools.clone();
            let namespaces = start_namespaces.clone();
            let globals = start_globals.clone();
            Box::pin(async move {
                let mut run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                // 结局只决定一次：已有结果的调用直接返回，不再启动 VM（重试幂等的来源）。
                if let Some(result) = field(run.value(), "result").cloned() {
                    let result: ToolExecutionResult = serde_json::from_value(result)?;
                    return Ok(Found::Done(result));
                }
                // 有 start 就是重跑；没有则把当前 store 快照、工具/全局清单与时钟/种子冻结进去。
                let rerun = field(run.value(), "start").is_some();
                if !rerun {
                    let store = tx
                        .doc(&DocTarget::conversation(
                            &CODEMODE_STORE_DOC,
                            conversation_id,
                        ))
                        .await?;
                    let values = field(store.value(), "values")
                        .and_then(as_object)
                        .unwrap_or_default();
                    let start = Start {
                        version: CODEMODE_MODULE_VERSION,
                        started_at: now as f64,
                        seed,
                        fingerprint: None,
                        deadline: timeout.map(|timeout| now as f64 + timeout),
                        tools,
                        namespaces,
                        globals,
                        max_output_tokens,
                        store: values,
                    };
                    run.value_mut()
                        .insert("start".to_owned(), serde_json::to_value(&start)?);
                }
                let start_value = field(run.value(), "start")
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                let start: Start = serde_json::from_value(start_value)?;
                // journal 与 start 在同一次提交里读，避免两者取自不同的提交时点。
                let journal = tx
                    .doc(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task_id))
                    .await?;
                let entries: Vec<JournalEntry> = match field(journal.value(), "entries") {
                    Some(JsonValue::Array(entries)) => {
                        serde_json::from_value(JsonValue::Array(entries.clone()))?
                    }
                    _ => Vec::new(),
                };
                let first_issued =
                    field(journal.value(), "firstIssued").and_then(JsonValue::as_u64);
                Ok(Found::Start {
                    start,
                    journal: entries,
                    first_issued,
                    rerun,
                })
            })
        })
        .await?;

        let (start, journal, first_issued, rerun) = match found {
            Found::Done(result) => return Ok(result),
            Found::Start {
                start,
                journal,
                first_issued,
                rerun,
            } => (start, journal, first_issued, rerun),
        };
        // 阶段 3：把 journal 装进内存状态。`durable_issued` 取 firstIssued 与所有 issuedAfter 的
        // 最大值——早前尝试确认过的调用数都算已落盘，重放结束后这些调用可以立即启动。
        {
            let mut state = self.state();
            let journal_length = journal.len();
            let replay_index = journal
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.id, index))
                .collect();
            let mut durable_issued = first_issued.unwrap_or(0);
            for entry in &journal {
                durable_issued = durable_issued.max(entry.issued_after.unwrap_or(0));
            }
            state.start = Some(start.clone());
            state.replayed = journal;
            state.journal_length = journal_length;
            state.next_replayed = 0;
            state.replaying = journal_length > 0;
            state.rerun = rerun;
            state.released = false;
            state.first_issued = first_issued;
            state.durable_issued = durable_issued;
            state.replay_index = replay_index;
        }

        // 阶段 4：重放兼容性检查。两者都以“已决定的错误结果”结束（调用本身成功），
        // 不会起 VM。
        if start.version != CODEMODE_MODULE_VERSION {
            return self
                .decide(error_result(format!(
                    "The script cannot resume: it started under code mode version {}, this is {}.",
                    start.version, CODEMODE_MODULE_VERSION
                )))
                .await;
        }
        let remaining = start
            .deadline
            .map(|deadline| deadline - self.api.now() as f64);
        if remaining.is_some_and(|remaining| remaining <= 0.0) {
            return self
                .decide(error_result(
                    "Script timed out: its deadline passed while it was interrupted.",
                ))
                .await;
        }

        let outer_signal = self.context.abort_signal();
        // 订阅外层调用的取消，转发给本调用的 controller（VM 的 signal）。订阅只在本次尝试
        // 有效，run_script 返回后即 drop（AbortSubscription 的 drop 会注销回调）。
        let subscription = outer_signal.as_ref().map(|signal| {
            let controller = self.controller.clone();
            signal.on_abort(move |reason| controller.abort(Some(reason)))
        });
        // 订阅之前可能已经中止：不补这一步，VM 会照常跑完。
        if let Some(signal) = &outer_signal
            && signal.is_aborted()
        {
            self.controller.abort(signal.reason());
        }
        let outcome = self.run_script(&code, &start, remaining).await;
        drop(subscription);
        outcome
    }

    /// 启动沙箱并等待结果，然后按 TS 的后处理决定本次调用的结果。
    ///
    /// 声明给 VM 的工具过滤掉 `hidden` 的内部工具（它们只能经 `models.*` 全局到达）；globals
    /// 原样声明。三个回调都从 VM 的 worker 线程同步进来，这里只把实际工作 `spawn` 到宿主运行时。
    ///
    /// 结果分类（按顺序）：
    /// 1. `state.failure`：内部或存储错误，优先传播；
    /// 2. `state.ended`：分叉或指纹检查已经决定了结局；
    /// 3. 外层已中止：返回 abort 错误；
    /// 4. 沙箱被宿主关闭、并非本调用中止：不存结局，返回错误留给下一次尝试重放；
    /// 5. 重跑在早前尝试继续过的地方结束：判分叉；
    /// 6. 其余交给 `complete()`。
    async fn run_script(
        self: &Arc<Self>,
        code: &str,
        start: &Start,
        remaining: Option<f64>,
    ) -> Result<ToolExecutionResult, BoxError> {
        // 隐藏的内部工具不出现在脚本的 `tools` 里（只能通过模型类全局调用）。
        let declared_tools: Vec<CodemodeDeclaredTool> = start
            .tools
            .iter()
            .filter(|tool| !tool.hidden)
            .map(|tool| CodemodeDeclaredTool {
                name: tool.name.clone(),
                js_name: Some(tool.js_name.clone()),
                description: Some(tool.description.clone()),
            })
            .collect();
        let declared_globals: Vec<CodemodeDeclaredGlobal> = start
            .globals
            .iter()
            .map(|global| CodemodeDeclaredGlobal {
                name: global.name.clone(),
                spread: Some(global.spread),
            })
            .collect();
        let on_started = {
            let this = Arc::clone(self);
            Arc::new(move |fingerprint: String| {
                let this = Arc::clone(&this);
                let task = Arc::clone(&this);
                this.spawn(async move { task.check_fingerprint(fingerprint).await });
            })
        };
        let on_call = {
            let this = Arc::clone(self);
            Arc::new(move |call: CodemodeHostCall| {
                this.on_call(call);
            })
        };
        let on_idle = {
            let this = Arc::clone(self);
            Arc::new(move |seq: u64, issued: u64| {
                this.on_idle(seq, issued);
            })
        };
        let mut run = self.options.sandbox.start(
            code,
            CodemodeRunOptions {
                started_at: start.started_at,
                seed: start.seed,
                tools: Some(declared_tools),
                globals: Some(declared_globals),
                store: Some(start.store.clone()),
                signal: Some(self.controller.token().clone()),
                // 重跑只剩余量：deadline 是首次运行按 startedAt + timeout_ms 记下的绝对时刻。
                timeout_ms: remaining,
                on_started: Some(on_started),
                on_call: Some(on_call),
                on_idle: Some(on_idle),
            },
        )?;
        // `run` 要存进 `self.run` 供回调投递，但它的 `result` future 只能 await 一次；把一个
        // 立即完成的占位 future 换进去，真正的 future 留在本地 await。占位结果永远不会被观察：
        // 其他任务只通过 `finished()` 与 `deliver()` 使用这个句柄。
        let placeholder: CodemodeBoxFuture<'static, CodemodeResult> = Box::pin(async {
            CodemodeResult::Ok(CodemodeResultOk::new(
                None,
                Vec::new(),
                Vec::new(),
                CodemodeStoreWrites::default(),
                None,
            ))
        });
        let result_future = std::mem::replace(&mut run.result, placeholder);
        *lock(&self.run) = Some(run);
        self.ready.set();
        let result = result_future.await;
        if let Some(failure) = self.state().failure.take() {
            return Err(failure);
        }
        if let Some(ended) = self.state().ended.clone() {
            return Ok(ended);
        }
        if let Some(signal) = self.context.abort_signal()
            && signal.is_aborted()
        {
            return Err(Box::new(kiana_chord::context::abort_error(&signal)));
        }
        // 被中止，但不是本调用中止的：宿主关掉了沙箱（例如在关 Harness 前关闭）。
        // 这对脚本什么都没决定：不存任何结果，结束本次调用，留给下次尝试重放。
        if let CodemodeResult::Err(error) = &result
            && error.error.kind == kiana_codemode::types::CodemodeErrorKind::Aborted
            && !self.controller.is_aborted()
        {
            if let Some(signal) = self.context.abort_signal() {
                if !signal.is_aborted() {
                    signal.token().cancelled().await;
                }
                if signal.is_aborted() {
                    return Err(Box::new(kiana_chord::context::abort_error(&signal)));
                }
            }
            return Err("The code mode sandbox was closed while the script ran".into());
        }
        // 重放在实时运行继续之处结束，就是分叉（设计 §4.3）。
        //
        // `during` 是脚本结束时所处的结果号与当时累计发出的调用数。重跑在“早前尝试还没走到的
        // 位置”结束属于正常重放；在早前尝试继续过的位置结束说明这次少走了。继续过的证据有两种：
        // 该位置已经确认过调用数（`issuedAfter`，或 seq 0 的 `firstIssued`/非空 journal），或者
        // journal 后面还记着编号超过 `during.issued` 的调用。`||` 把两种情况并在一起。
        let when = result.during();
        if let Some(during) = when {
            let (replayed, first_issued) = {
                let state = self.state();
                (state.replayed.clone(), state.first_issued)
            };
            if self.state().rerun && during.seq as usize <= replayed.len() {
                let continued = if during.seq == 0 {
                    !replayed.is_empty() || first_issued.is_some()
                } else {
                    replayed[during.seq as usize - 1].issued_after.is_some()
                } || replayed
                    .iter()
                    .skip(during.seq as usize)
                    .any(|entry| entry.id > during.issued);
                if continued {
                    return self
                        .decide(error_result(format!(
                            "The script took a different path when resumed after a restart: it ended after result {}, where it went on before.",
                            during.seq
                        )))
                        .await;
                }
            }
        }
        self.complete(result).await
    }

    /// 存下调用决定的结局，若 VM 在跑就结束它；返回该结局。
    ///
    /// 文档里的 `result` 只写一次（先到者胜），返回值则是本次构造的结果（带当前 details）。
    async fn decide(&self, decided: ToolExecutionResult) -> Result<ToolExecutionResult, BoxError> {
        let result = ToolExecutionResult {
            details: Some(serde_json::to_value(self.details())?),
            ..decided
        };
        let task_id = self.api.task_id();
        let stored = result.clone();
        let api = Arc::clone(&self.api);
        let context = Arc::clone(&self.context);
        commit_value(api.as_ref(), context.as_ref(), move |tx| {
            let stored = stored.clone();
            Box::pin(async move {
                let mut run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                if field(run.value(), "result").is_none() {
                    run.value_mut()
                        .insert("result".to_owned(), serde_json::to_value(&stored)?);
                }
                Ok(())
            })
        })
        .await?;
        Ok(result)
    }

    /// 以 `message` 结束调用：先存结局，再用脚本接不住的错误停掉 VM。
    ///
    /// 分叉是宿主侧的单向决定：`ended` 一置位，后续回调都会被 `stopped()` 挡掉。`diverge()` 的
    /// 调用点都在回调与后台任务里（`on_call()`、`on_idle()`、`release()`、`launch()`）。落盘与
    /// 中止在后台任务里完成；此时 `run_script()` 正等沙箱的 `result` future，VM 停止后它从
    /// `state.ended` 取到结局。
    fn diverge(self: &Arc<Self>, message: String) {
        {
            let mut state = self.state();
            if state.ended.is_some() || state.failure.is_some() {
                return;
            }
            let result = error_result(format!(
                "The script took a different path when resumed after a restart: {message}"
            ));
            state.ended = Some(result);
        }
        let this = Arc::clone(self);
        let abort_message = message;
        self.spawn(async move {
            let ended = this.state().ended.clone();
            let Some(ended) = ended else { return };
            match this.decide(ended).await {
                Ok(_) => this
                    .controller
                    .abort(Some(AbortReason::error(std::io::Error::other(
                        abort_message,
                    )))),
                Err(error) => this.fail(error),
            }
        });
    }

    /// 脚本自己的结局，与它的 store 写入在同一次提交里存下。
    ///
    /// 提交顺序：`result` 缺失才写入（先到者胜）；只有成功结果才带 `storeWrites`，先按键删除再
    /// 写入新值。失败结果不动 store。
    ///
    /// `details` 只供 UI/会话记录（`kiana-ai` 的 `ToolResultMessage.details` 文档写明它不属于
    /// 模型输入）：模型看到的是 `formatted.content`。失败时它的正文已含由 `details.calls` 转成的
    /// 调用摘要（`format_result()` 接收 `CallSummary`）；输出被裁剪时正文末尾还含
    /// `[Full output: <path> ...]`（由 `output.rs` 在裁剪时拼接）。`fullOutputPath` 只写进
    /// `details`，不进入模型内容。
    async fn complete(
        self: &Arc<Self>,
        result: CodemodeResult,
    ) -> Result<ToolExecutionResult, BoxError> {
        let details = self.details();
        let calls: Vec<CallSummary<'_>> = details
            .calls
            .iter()
            .map(|call| CallSummary {
                name: &call.name,
                status: call.status.as_str(),
            })
            .collect();
        let start = self.start_data();
        let generated_images = self.state().generated_images;
        // 格式化用 start 数据里的 token 预算（重跑沿用首次运行的值）与从 `startedAt` 起算的
        // 墙钟；`generated_images` 用来提示脚本从没展示过 `models.generateImages()` 的图片。
        let formatted = format_result(
            &result,
            FormatResultOptions {
                generated_images,
                wall_ms: self.api.now() as f64 - start.started_at,
                max_output_tokens: start.max_output_tokens,
                calls: &calls,
                limits: OUTPUT_LIMITS,
                write: self.output_files(),
            },
        )
        .await?;
        let mut details_value = serde_json::to_value(&details)?;
        if let Some(path) = &formatted.full_output_path
            && let JsonValue::Object(map) = &mut details_value
        {
            map.insert("fullOutputPath".to_owned(), JsonValue::String(path.clone()));
        }
        let tool = ToolExecutionResult {
            output: Some(formatted.content),
            is_error: Some(!result.result_ok()),
            details: Some(details_value),
            ..Default::default()
        };
        let writes = match &result {
            CodemodeResult::Ok(ok) => Some(ok.store_writes.clone()),
            CodemodeResult::Err(_) => None,
        };
        let task_id = self.api.task_id();
        let conversation_id = self.api.conversation_id();
        let commit_tool = tool.clone();
        let api = Arc::clone(&self.api);
        let context = Arc::clone(&self.context);
        commit_value(api.as_ref(), context.as_ref(), move |tx| {
            let tool = commit_tool.clone();
            let writes = writes.clone();
            Box::pin(async move {
                let mut run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                if let Some(existing) = field(run.value(), "result").cloned() {
                    let existing: ToolExecutionResult = serde_json::from_value(existing)?;
                    return Ok(existing);
                }
                run.value_mut()
                    .insert("result".to_owned(), serde_json::to_value(&tool)?);
                if let Some(writes) = writes
                    && (!writes.set.is_empty() || !writes.delete.is_empty())
                {
                    // store 写入与结果同事务：先删（`delete` 是存成 `undefined` 的 key），再按
                    // `set` 写入。提交成功时两者一起对后续调用可见。
                    let mut store = tx
                        .doc(&DocTarget::conversation(
                            &CODEMODE_STORE_DOC,
                            conversation_id,
                        ))
                        .await?;
                    let mut values = field(store.value(), "values")
                        .and_then(as_object)
                        .unwrap_or_default();
                    for key in &writes.delete {
                        values.shift_remove(key);
                    }
                    for (key, value) in &writes.set {
                        values.insert(key.clone(), value.clone());
                    }
                    store
                        .value_mut()
                        .insert("values".to_owned(), object_value(values));
                }
                Ok(tool)
            })
        })
        .await
    }

    /// 把结果文件的写入落到 run 的输出目录里的固定名字上，重跑会覆盖它们。
    ///
    /// 环境缺失时每个文件都得到 `no execution environment`；写失败时把 `FileError::message`
    /// 原样交给 `format_result`，由它拼进图片/完整输出项的提示文本。
    fn output_files(self: &Arc<Self>) -> OutputFile {
        let this = Arc::clone(self);
        Arc::new(move |name: String, data: OutputData| {
            let this = Arc::clone(&this);
            Box::pin(async move {
                let Some(env) = this.api.env() else {
                    return OutputFileResult::Error("no execution environment".to_owned());
                };
                let directory = match this.output_dir(env.as_ref()).await {
                    Ok(directory) => directory,
                    Err(error) => return OutputFileResult::Error(error.to_string()),
                };
                let path = format!("{directory}/{name}");
                let content = match data {
                    OutputData::Text(text) => FileContent::Text(text),
                    OutputData::Bytes(bytes) => FileContent::Bytes(bytes),
                };
                match env.write_file(&path, content, &*this.context).await {
                    Ok(()) => OutputFileResult::Path(path),
                    Err(error) => OutputFileResult::Error(error.message),
                }
            }) as super::output::OutputFuture
        })
    }

    /// run 的输出目录：已记录的，否则新建一个临时目录并在写任何文件之前记录。
    ///
    /// 解析结果连同错误只算一次：TS 在闭包里缓存 `outputDir()` 的 promise（`directory ??= ...`），
    /// 它拒绝后同一结果的每个文件都报同一条错误，环境也不会再被问一次。
    async fn output_dir(&self, env: &dyn ExecutionEnv) -> Result<String, BoxError> {
        match self
            .output_dir
            .get_or_init(|| async {
                self.resolve_output_dir(env)
                    .await
                    .map_err(|error| error.to_string())
            })
            .await
        {
            Ok(directory) => Ok(directory.clone()),
            Err(message) => Err(message.clone().into()),
        }
    }

    /// 读记录的目录，或新建并记录它。
    ///
    /// 先读 `run` 文档的 `outputDir`；没有就 `create_temp_dir("kiana-codemode-")`，再写回文档。
    /// 写入只在字段缺失时发生，并返回“文档里实际的值”——并发/重跑时先记录者胜，后建出的目录
    /// 会被丢弃。
    ///
    /// 注意：创建目录与记录不是原子的。创建成功但记录失败时，本次解析返回 `Err`，`output_dir()`
    /// 的 `OnceCell` 会缓存这条错误，所以这次不会写任何文件；刚创建的目录成为孤儿，下一次尝试
    /// （新的 `CodemodeCall`）会再建一个目录。
    async fn resolve_output_dir(&self, env: &dyn ExecutionEnv) -> Result<String, BoxError> {
        let task_id = self.api.task_id();
        let api = Arc::clone(&self.api);
        let context = Arc::clone(&self.context);
        let recorded: Option<String> = commit_value(api.as_ref(), context.as_ref(), move |tx| {
            Box::pin(async move {
                let run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                Ok(field(run.value(), "outputDir")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned))
            })
        })
        .await?;
        if let Some(recorded) = recorded {
            return Ok(recorded);
        }
        let created = env
            .create_temp_dir(Some("kiana-codemode-"), context.as_ref())
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        let created_for_commit = created.clone();
        commit_value(api.as_ref(), context.as_ref(), move |tx| {
            let created = created_for_commit.clone();
            Box::pin(async move {
                let mut run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                if field(run.value(), "outputDir").is_none() {
                    run.value_mut()
                        .insert("outputDir".to_owned(), JsonValue::String(created.clone()));
                }
                Ok(field(run.value(), "outputDir")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(&created)
                    .to_owned())
            })
        })
        .await
    }

    /// 指纹检查：首次运行时把沙箱指纹写进 start 数据（只写一次），重跑时与记录值比较。
    ///
    /// 指纹由沙箱在脚本运行前给出（`on_started`），此时 run 句柄可能还没存好，所以先等 `ready`。
    /// 不一致时把错误结果落盘并中止 VM；一致（或首次记录成功）后打开 `gate`，让重放/实时调用
    /// 继续。首次运行随后 `release()`；有 journal 的重跑由 `deliver_replayed()` 开始。
    async fn check_fingerprint(self: &Arc<Self>, fingerprint: String) {
        self.ready.wait().await;
        let recorded = self.start_data().fingerprint;
        match recorded {
            Some(recorded) => {
                if recorded != fingerprint {
                    let result = error_result(format!(
                        "The script cannot resume: the sandbox changed since it started ({recorded} -> {fingerprint})."
                    ));
                    {
                        let mut state = self.state();
                        if state.ended.is_none() {
                            state.ended = Some(result.clone());
                        }
                    }
                    if let Err(error) = self.decide(result).await {
                        self.fail(error);
                        return;
                    }
                    self.controller
                        .abort(Some(AbortReason::error(std::io::Error::other(
                            "Sandbox fingerprint changed",
                        ))));
                    return;
                }
            }
            None => {
                let task_id = self.api.task_id();
                let fingerprint_for_commit = fingerprint.clone();
                let api = Arc::clone(&self.api);
                let context = Arc::clone(&self.context);
                let committed = commit_value(api.as_ref(), context.as_ref(), move |tx| {
                    let fingerprint = fingerprint_for_commit.clone();
                    Box::pin(async move {
                        let mut run = tx.doc(&DocTarget::task(&CODEMODE_RUN_DOC, task_id)).await?;
                        if let Some(JsonValue::Object(start)) = run.value_mut().get_mut("start") {
                            start.insert("fingerprint".to_owned(), JsonValue::String(fingerprint));
                        }
                        // `start` 对象缺失时这次写入是空操作（正常路径不会发生：execute() 在
                        // 启动沙箱之前已经写好 start）。
                        Ok(())
                    })
                })
                .await;
                if let Err(error) = committed {
                    self.fail(error);
                    return;
                }
            }
        }
        self.gate.open();
        // 门开之前什么都没投递：现在开始。没有 journal 的重跑在脚本首次确认时才释放调用。
        let (replaying, rerun) = {
            let state = self.state();
            (state.replaying, state.rerun)
        };
        if replaying {
            if let Err(error) = self.deliver_replayed().await {
                self.fail(error);
            }
        } else if !rerun {
            self.release().await;
        }
    }

    /// 脚本发出一次调用（`onCall` 回调）：登记 details 行、记录最大调用 id，然后按当前阶段处理。
    ///
    /// - 重放中且 journal 里有这个调用：核对调用形态后，把记录的结果投回去（global 直接投，
    ///   tool 还要经由嵌套调用取回结果）。
    /// - 其他情况：已放行就进 `waiting` 并尝试启动，未放行就先 `held`。
    ///
    /// 整个函数没有 `await`：它在 VM 的 worker 线程上同步执行，所有异步工作都 `spawn` 出去。
    fn on_call(self: &Arc<Self>, made: CodemodeHostCall) {
        if self.stopped() {
            return;
        }
        // `models.classify()`/`generateImages()` 在 start 数据里是 global，但执行时改写成对内部
        // 工具的调用：这样它们有嵌套任务、live 行与用量记账。
        let call = as_tool_call(made);
        let index = {
            let mut state = self.state();
            if call.target == CodemodeCallTarget::Tool {
                started(&mut state, &call);
            }
            // 记录已经见到的最大调用 id；global 也占调用 id，但不进 details 的行。
            state.issued = state.issued.max(call.id);
            if state.replaying {
                state.replay_index.get(&call.id).copied()
            } else {
                None
            }
            .map(|index| {
                let entry = state.replayed[index].clone();
                (index, entry)
            })
        };
        let Some((_index, entry)) = index else {
            // journal 里没有这个调用（实时阶段的正常情况，或重放阶段的新调用）：等到调用数被
            // 提交后才能启动，避免重启后漏投。
            let (released, stopped) = {
                let mut state = self.state();
                let stopped = state.ended.is_some() || state.failure.is_some();
                if stopped {
                    (false, true)
                } else if state.released {
                    state.waiting.push(call);
                    (true, false)
                } else {
                    state.held.push(call);
                    (false, false)
                }
            };
            if stopped {
                return;
            }
            if released {
                self.launch_covered();
            }
            return;
        };
        if let Some(global) = entry.global {
            // journal 里这个调用是 global：目标、名字、参数必须与记录完全一致才能直接投记录值，
            // 不再执行宿主函数；不一致即为分叉。
            if call.target != CodemodeCallTarget::Global
                || call.name != global.name
                || !same_json(call.args.as_ref(), global.args.as_ref())
            {
                self.diverge(format!(
                    "call {} is {}, but was a call to {}",
                    call.id,
                    describe_call(&call),
                    global.name
                ));
                return;
            }
            self.state().ready.insert(call.id, global.outcome);
            let this = Arc::clone(self);
            self.spawn(async move {
                this.gate.wait().await;
                if let Err(error) = this.deliver_replayed().await {
                    this.fail(error);
                }
            });
            return;
        }
        if call.target != CodemodeCallTarget::Tool {
            self.diverge(format!(
                "call {} is {}, but was a tool call",
                call.id,
                describe_call(&call)
            ));
            return;
        }
        // journal 里这个调用是 tool：结果经 `call_tool` 从嵌套调用取（key `c<id>` 命中早前
        // 尝试留下的任务/结果，是否重跑由嵌套工具自己的 replay 策略决定）。
        let this = Arc::clone(self);
        self.spawn(async move {
            this.gate.wait().await;
            match this.call_tool(&call).await {
                Ok(outcome) => {
                    this.state().ready.insert(call.id, outcome);
                    if let Err(error) = this.deliver_replayed().await {
                        this.fail(error);
                    }
                }
                Err(error) => {
                    if this.stopped() {
                        return;
                    }
                    if this
                        .context
                        .abort_signal()
                        .is_some_and(|signal| signal.is_aborted())
                    {
                        this.fail(error);
                    } else {
                        // key 指向另一个工具或别的参数。
                        let message =
                            format!("call {} is {}: {}", call.id, describe_call(&call), error);
                        this.diverge(message);
                    }
                }
            }
        });
    }

    /// 重放：按 journal 顺序投递已记的结果，每个都在 VM 发出它的调用之后。
    ///
    /// 返回 `Ok(())` 只表示“现在没有可投递的”（等下一次 `on_call`/`on_idle` 再驱动），不代表
    /// 重放结束；`Err` 来自 `CodemodeRun::deliver` 的校验（时钟必须有限、结果号必须是只用一次的
    /// 正整数、调用必须正在等待结果）。
    async fn deliver_replayed(&self) -> Result<(), BoxError> {
        self.ready.wait().await;
        loop {
            // 读、删、加一与投递在同一把锁里：并发的投递者不会看到同一个 `next_replayed`，
            // 投递顺序也因此与 journal 顺序一致（TS 的同步循环没有 await，自然如此）。
            let delivered = {
                let mut state = self.state();
                if state.next_replayed >= state.replayed.len() {
                    return Ok(());
                }
                if state.ended.is_some() || state.failure.is_some() || self.run_finished() {
                    return Ok(());
                }
                let entry = state.replayed[state.next_replayed].clone();
                // 只投 VM 已经发出的调用：否则就是脚本少了调用，交给 on_idle 的分叉检查处理。
                if entry.id > state.issued {
                    return Ok(());
                }
                // 结果还没到（嵌套工具在跑）或 global 还没登记，就等它。
                let Some(outcome) = state.ready.remove(&entry.id) else {
                    return Ok(());
                };
                state.next_replayed += 1;
                let seq = state.next_replayed as f64;
                self.deliver(seq, entry.at, entry.id, outcome)
            };
            delivered?;
        }
    }

    /// 把一条结果交给 VM。`seq` 是 journal 位置（结果号），`at` 是重放要用的 VM 时钟。
    ///
    /// run 句柄尚未存好时静默成功；调用方都先等 `ready`，正常路径不会走到这个分支。
    fn deliver(
        &self,
        seq: f64,
        at: f64,
        id: u64,
        outcome: CodemodeOutcome,
    ) -> Result<(), BoxError> {
        let run = lock(&self.run);
        match run.as_ref() {
            Some(run) => run
                .deliver(seq, at, id, outcome)
                .map_err(|message| -> BoxError { message.into() }),
            None => Ok(()),
        }
    }

    /// VM 处理完结果 `seq` 后再次等待（`seq == 0` 是脚本首次运行后）。
    ///
    /// 做三件事：把累计调用数 `issued` 记进 `acks`，由 `pump()` 落盘；重跑时核对早前尝试记录
    /// 过的调用数，不一致就是分叉；重放阶段在追平 journal 后切换到实时。
    fn on_idle(self: &Arc<Self>, seq: u64, issued: u64) {
        if self.stopped() {
            return;
        }
        let (replaying, rerun, journal_length) = {
            let state = self.state();
            (state.replaying, state.rerun, state.journal_length)
        };
        // 早前尝试记录的调用数必须匹配；缺失的现在记录。
        if rerun
            && seq as usize <= {
                let state = self.state();
                state.replayed.len()
            }
        {
            let recorded = {
                let state = self.state();
                if seq == 0 {
                    state.first_issued
                } else {
                    state.replayed[seq as usize - 1].issued_after
                }
            };
            if let Some(recorded) = recorded
                && recorded != issued
            {
                let message = if seq == 0 {
                    format!("its first run made {issued} calls, where it had made {recorded}")
                } else {
                    format!(
                        "after result {seq} it had made {issued} calls, where it had made {recorded}"
                    )
                };
                self.diverge(message);
                return;
            }
        }
        {
            let mut state = self.state();
            state.acks.insert(seq, issued);
        }
        if !replaying {
            // 没有 journal 的重跑：早前尝试可能接纳的每个调用现在都做过了。
            if seq == 0 && rerun {
                let this = Arc::clone(self);
                self.spawn(async move { this.release().await });
            }
            // 脚本处理完所有已投递结果时立刻提交：它发出的调用在等它，而等待长调用的脚本正是
            // 重启最常找到的位置。否则随下一次提交；VM 会自己追上，不等任何东西。
            if seq as usize == journal_length {
                let this = Arc::clone(self);
                self.spawn(async move { this.pump().await });
            }
            return;
        }
        let (next_replayed, next_entry, stopped) = {
            let state = self.state();
            (
                state.next_replayed,
                state.replayed.get(seq as usize).cloned(),
                state.ended.is_some() || state.failure.is_some(),
            )
        };
        if stopped || self.run_finished() {
            return;
        }
        // 追上了：VM 处理完所有已投递结果并在等待。
        if seq as usize != next_replayed {
            return;
        }
        if let Some(next) = next_entry {
            if next.id > issued {
                self.diverge(format!(
                    "it waits without making call {}, which it made before",
                    next.id
                ));
            }
            return;
        }
        // 最后一个重放结果已确认：继续实时，journal 没有的调用在调用数提交后接纳。
        self.state().replaying = false;
        let this = Arc::clone(self);
        let this2 = Arc::clone(self);
        self.spawn(async move { this.pump().await });
        self.spawn(async move { this2.release().await });
    }

    /// 让 held 的调用启动，以及之后的实时调用，各自在调用数提交后。重跑时早前尝试可能以这些
    /// key 接纳过别的调用：重放释放时（最后一个 journal 结果后的 idle，或无 journal 时的首次
    /// 运行后）它的计数与每个记录值都对上，于是它已做出早前尝试可能接纳的每个调用。所有 held
    /// 调用先全部检查，无论目标；分叉时一个都不启动。此后发出的调用都是新的。
    ///
    /// 可重复调用：已释放、已结束或 run 已结束就直接返回。
    async fn release(self: &Arc<Self>) {
        self.gate.wait().await;
        self.ready.wait().await;
        let (stopped, released) = {
            let state = self.state();
            (
                state.ended.is_some() || state.failure.is_some(),
                state.released,
            )
        };
        if stopped || released || self.run_finished() {
            return;
        }
        let calls = {
            let mut state = self.state();
            std::mem::take(&mut state.held)
        };
        let rerun = self.state().rerun;
        if rerun {
            // 全有或全无：先检查完所有 held 调用，再决定启动任何一个（分叉时一个都不启动）。
            for call in &calls {
                let other = match self.earlier(call).await {
                    Ok(other) => other,
                    Err(error) => {
                        self.fail(error);
                        return;
                    }
                };
                if self.stopped() {
                    return;
                }
                if let Some(other) = other {
                    self.diverge(format!(
                        "call {} is {}, but was {other}",
                        call.id,
                        describe_call(call)
                    ));
                    return;
                }
            }
        }
        {
            let mut state = self.state();
            state.released = true;
            state.waiting.extend(calls);
            let held = std::mem::take(&mut state.held);
            state.waiting.extend(held);
        }
        self.launch_covered();
    }

    /// 启动等待中调用数已提交的那些调用。
    ///
    /// 覆盖条件是 `call.id <= durable_issued`；不满足的留在 `waiting`，等下一次 `acks` 提交后
    /// 的 `launch_covered()` 再试。
    fn launch_covered(self: &Arc<Self>) {
        let (released, stopped) = {
            let state = self.state();
            (
                state.released,
                state.ended.is_some() || state.failure.is_some(),
            )
        };
        if !released || stopped || self.run_finished() {
            return;
        }
        let covered = {
            let mut state = self.state();
            let durable_issued = state.durable_issued;
            let mut covered: Vec<CodemodeHostCall> = Vec::new();
            let mut waiting: Vec<CodemodeHostCall> = Vec::new();
            for call in std::mem::take(&mut state.waiting) {
                if call.id <= durable_issued {
                    covered.push(call);
                } else {
                    waiting.push(call);
                }
            }
            state.waiting = waiting;
            covered
        };
        for call in covered {
            self.launch(call);
        }
    }

    /// 早前一次尝试在本调用的 key 下做过的调用，当它是另一个时：另一个目标、工具或参数。
    ///
    /// key 是嵌套调用文档（`kiana.tool.nested-call`）的 `c<调用 id>`：先读出它记录的 `taskId`，
    /// 再取任务输入里的调用名与参数。任何一步缺失或解析失败都按“没有早前调用”处理（返回
    /// `Ok(None)`），不据此判分叉；只有确实读到另一个调用时才返回描述文本。
    async fn earlier(&self, call: &CodemodeHostCall) -> Result<Option<String>, BoxError> {
        let target = DocTarget::task_family(
            &NESTED_CALL_DOC,
            self.api.task_id(),
            format!("c{}", call.id),
            JsonValue::Null,
        );
        let Some(state) = self.api.snapshot(&target, &*self.context).await? else {
            return Ok(None);
        };
        let Some(task_id) = field(&state, "taskId").and_then(JsonValue::as_i64) else {
            return Ok(None);
        };
        let Some(record) = self
            .api
            .get_task(crate::types::id_from_number(task_id), &*self.context)
            .await?
        else {
            return Ok(None);
        };
        let Ok(input) = serde_json::from_value::<ToolTaskInput>(record.input.clone()) else {
            return Ok(None);
        };
        let ToolTaskInput::Nested { call: made, .. } = input else {
            return Ok(None);
        };
        // TS 的 `call.args ?? {}`：`null` 与 `undefined` 都按空对象比较；`call_tool` 也把它们
        // 归一成 `{}` 交给嵌套调用，所以嵌套任务记下的参数是 `{}`。
        let call_args = match call.args.as_ref() {
            Some(JsonValue::Null) | None => empty_object(),
            Some(args) => args.clone(),
        };
        let made_args = object_value(made.arguments.clone());
        if call.target == CodemodeCallTarget::Tool
            && made.name == call.name
            && same_json(Some(&made_args), Some(&call_args))
        {
            return Ok(None);
        }
        Ok(Some(format!(
            "tools.{}({})",
            made.name,
            kiana_json::js_object::stringify(&object_value(made.arguments))
        )))
    }

    /// 跑一个实时调用并 journal 它的结果。
    ///
    /// 工具调用走 `call_tool()`，global 走 `call_global()`；global 的结果还要连同调用名与参数
    /// 包成 `JournalGlobal`，这样重放可以不经宿主直接回答。完成的结果带完成时刻进 `queue`，
    /// 由 `pump()` 提交后再投递。
    fn launch(self: &Arc<Self>, call: CodemodeHostCall) {
        if self.stopped() {
            return;
        }
        let this = Arc::clone(self);
        self.spawn(async move {
            let done: Result<(CodemodeOutcome, Option<JournalGlobal>), BoxError> =
                if call.target == CodemodeCallTarget::Tool {
                    this.call_tool(&call).await.map(|outcome| (outcome, None))
                } else {
                    this.call_global(&call).await.map(|outcome| {
                        let global = JournalGlobal {
                            name: call.name.clone(),
                            args: call.args.clone(),
                            outcome: outcome.clone(),
                        };
                        (outcome, Some(global))
                    })
                };
            match done {
                Ok((outcome, global)) => {
                    // 结局已决定之后到达的结果无处可去，调用的 API 也已结束。
                    if this.stopped() {
                        return;
                    }
                    let at = this.api.now() as f64;
                    this.state().queue.push(QueueItem {
                        id: call.id,
                        at,
                        outcome,
                        global,
                    });
                    this.pump().await;
                }
                Err(error) => {
                    if this.stopped() {
                        return;
                    }
                    if this
                        .context
                        .abort_signal()
                        .is_some_and(|signal| signal.is_aborted())
                    {
                        this.fail(error);
                    } else {
                        // 早前尝试以另一个工具或别的参数接纳过的 key：本次尝试走了另一条路。
                        let message =
                            format!("call {} is {}: {}", call.id, describe_call(&call), error);
                        this.diverge(message);
                    }
                }
            }
        });
    }

    /// 执行一次脚本的工具调用，并映射成脚本看到的结果。
    ///
    /// 参数不是对象时不执行工具，直接构造嵌套任务会存下的 `invalid_arguments` 校验结果（与
    /// TS 把原值交给嵌套校验的效果一致）。
    /// 内部模型工具先取并发槽位（最多 `MAX_CONCURRENT_MODEL_CALLS` 个）；脚本在等待期间结束就
    /// 退还槽位并报错，不再发起付费调用。
    /// 嵌套调用的 key 是 `c<调用 id>`：同一 key 在早前尝试里的任务/结果会被 harness 复用（是否
    /// 重跑由嵌套工具自己的 replay 策略决定）。
    async fn call_tool(&self, call: &CodemodeHostCall) -> Result<CodemodeOutcome, BoxError> {
        // 按脚本给的样子传下去；参数不是对象时由校验报告（TS 的 `(call.args ?? {}) as JsonObject`）。
        let raw_args = match &call.args {
            None | Some(JsonValue::Null) => empty_object(),
            Some(args) => args.clone(),
        };
        let args = match raw_args {
            JsonValue::Object(args) => args,
            other => {
                // TS 把非对象参数原样交给嵌套工具，嵌套任务的参数校验以 `invalid_arguments`
                // 结束：脚本的 promise 用那条校验消息拒绝，本次调用不因此分叉。`executeTool()`
                // 的冻结签名只接受对象，所以这里直接构造嵌套任务会存下的那个结果。
                let message = format!(
                    "Validation failed for tool \"{}\":\n  - root: must be object\n\nReceived arguments:\n{}",
                    call.name,
                    kiana_json::js_object::stringify_pretty(&other, 2)
                );
                let result = NestedToolExecutionResult {
                    task_id: crate::types::id_from_number(0),
                    structured_output: None,
                    is_error: true,
                    details: None,
                    diagnostics: vec![crate::harness::types::ToolDiagnostic {
                        severity: crate::harness::types::DiagnosticSeverity::Error,
                        code: Some("invalid_arguments".to_owned()),
                        message,
                    }],
                    usage: None,
                    duration_ms: None,
                };
                let structured = self
                    .start_data()
                    .tools
                    .iter()
                    .find(|tool| tool.name == call.name)
                    .is_some_and(|tool| tool.structured);
                let outcome = outcome_of(&call.name, structured, &result);
                self.finish_row(call.id, &outcome, &result);
                return Ok(outcome);
            }
        };
        let model = call.name.starts_with(CODEMODE_INTERNAL_PREFIX);
        if model {
            self.model_slot().await;
            // 等待期间脚本可能已结束：结束的脚本不再接纳新的付费调用。
            if self.stopped() {
                self.release_model_slot();
                return Err("The script has ended".into());
            }
        }
        let result = Arc::clone(&self.api)
            .execute_tool(
                &call.name,
                args.into_iter().collect(),
                &*self.context,
                NestedToolOptions {
                    // 与 `earlier()` 查询的 key 相同：重跑会命中早前尝试留下的嵌套任务。
                    key: Some(format!("c{}", call.id)),
                    progress: None,
                },
            )
            .await;
        if model {
            self.release_model_slot();
        }
        let result = result?;
        if call.name == GENERATE_IMAGES_TOOL {
            self.state().generated_images += count_images(result.structured_output.as_ref());
        }
        let structured = self
            .start_data()
            .tools
            .iter()
            .find(|tool| tool.name == call.name)
            .is_some_and(|tool| tool.structured);
        let outcome = outcome_of(&call.name, structured, &result);
        self.finish_row(call.id, &outcome, &result);
        Ok(outcome)
    }

    /// 一次工具调用结束：行得到状态、耗时与错误。
    ///
    /// 状态只看 `result.is_error`；错误文本优先取 outcome 的消息，成功结果则退回诊断里的
    /// error 消息。文本裁到 `ERROR_PREVIEW_CHARS` 个 UTF-16 码元。
    fn finish_row(&self, id: u64, outcome: &CodemodeOutcome, result: &NestedToolExecutionResult) {
        let mut state = self.state();
        let Some(row) = state.rows.get_mut(&id) else {
            return;
        };
        row.status = if result.is_error {
            NestedCallStatus::Error
        } else {
            NestedCallStatus::Ok
        };
        if let Some(duration) = result.duration_ms {
            row.duration_ms = Some(duration);
        }
        let errors = result
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.severity == crate::harness::types::DiagnosticSeverity::Error
            })
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let error = match outcome {
            CodemodeOutcome::Err(error) => error.message.clone(),
            CodemodeOutcome::Ok(_) => errors,
        };
        if result.is_error && !error.is_empty() {
            row.error = Some(cut(&error, ERROR_PREVIEW_CHARS));
        }
    }

    /// 行按调用顺序；脚本结束时仍在跑的调用是 cancelled。
    ///
    /// `rows` 是 `BTreeMap<调用 id, _>`，所以顺序就是脚本发出调用的顺序。`details()` 在决定结局
    /// 时调用，此时还在 `Running` 的行确实会被调用清理中止。
    fn details(&self) -> CodemodeToolDetails {
        let state = self.state();
        let calls = state
            .rows
            .values()
            .map(|row| {
                if row.status == NestedCallStatus::Running {
                    CodemodeNestedCall {
                        status: NestedCallStatus::Cancelled,
                        ..row.clone()
                    }
                } else {
                    row.clone()
                }
            })
            .collect();
        CodemodeToolDetails {
            calls,
            full_output_path: None,
        }
    }

    /// 执行一个全局函数调用。
    ///
    /// 发现类（`searchTools`/`describeTool`/`describeNamespace`）与目录类（`models.get*`）由本
    /// 模块直接回答，保证重放一致；其余交给 `CodemodeToolOptions::globals`。
    /// 未知名字返回一个错误 outcome（脚本 promise 拒绝），不是整个调用失败。宿主错误同样变成
    /// 错误 outcome；只有外层已中止时才向上传播。
    async fn call_global(&self, call: &CodemodeHostCall) -> Result<CodemodeOutcome, BoxError> {
        let discovery = DISCOVERY_GLOBALS.contains(&call.name.as_str());
        let catalog = CATALOG_GLOBALS.contains(&call.name.as_str());
        let global = if discovery || catalog {
            None
        } else {
            self.options
                .globals
                .iter()
                .flatten()
                .find(|candidate| candidate.name() == call.name)
                .cloned()
        };
        if !discovery && !catalog && global.is_none() {
            return Ok(CodemodeOutcome::Err(CodemodeOutcomeErr {
                ok: false,
                message: format!("Unknown global {}", call.name),
                properties: None,
            }));
        }
        let value: Result<Option<JsonValue>, BoxError> = if discovery {
            self.discover(call)
        } else if catalog {
            catalog_call(
                &call.name,
                call.args.as_ref(),
                &*self.api.models(),
                &*self.context,
            )
            .await
        } else {
            global
                .expect("the global is resolved")
                .execute(
                    call.args.clone(),
                    Arc::clone(&self.api),
                    Arc::clone(&self.context),
                )
                .await
        };
        match value {
            Ok(value) => Ok(CodemodeOutcome::Ok(CodemodeOutcomeOk { ok: true, value })),
            Err(error) => {
                if self
                    .context
                    .abort_signal()
                    .is_some_and(|signal| signal.is_aborted())
                {
                    return Err(error);
                }
                Ok(CodemodeOutcome::Err(CodemodeOutcomeErr {
                    ok: false,
                    message: crate::types::errors::error_message(error.as_ref()),
                    properties: None,
                }))
            }
        }
    }

    /// 取一个模型调用槽位：有空位就占用；满 `MAX_CONCURRENT_MODEL_CALLS` 个就挂到等待队列
    /// 尾部，被 `release_model_slot()` 唤醒后重试。等待者按先进先出唤醒。
    async fn model_slot(&self) {
        loop {
            let waiter = {
                let mut state = self.state();
                if state.model_calls < MAX_CONCURRENT_MODEL_CALLS {
                    state.model_calls += 1;
                    return;
                }
                let (sender, receiver) = tokio::sync::oneshot::channel();
                state.model_waiters.push(sender);
                receiver
            };
            let _ = waiter.await;
        }
    }

    /// 归还槽位并唤醒队首等待者。计数用 `saturating_sub`，重复调用不会把它减成负数。
    fn release_model_slot(&self) {
        let mut state = self.state();
        state.model_calls = state.model_calls.saturating_sub(1);
        if !state.model_waiters.is_empty() {
            let sender = state.model_waiters.remove(0);
            let _ = sender.send(());
        }
    }

    /// `searchTools()`、`describeTool()`、`describeNamespace()`：由 start 数据回答。
    ///
    /// 参数按 spread global 的约定放在一个数组里（`args[0]` 是查询或名字）。全部信息来自 start
    /// 数据，不读当前 registry：重放时脚本看到与首次运行相同的工具目录。
    /// 返回 `Ok(None)` 表示脚本显式要 `undefined`（找不到的工具/命名空间）；参数错误返回
    /// `Err`，由 `call_global` 转成脚本里的 `Error`。
    fn discover(&self, call: &CodemodeHostCall) -> Result<Option<JsonValue>, BoxError> {
        // 发现类全局按 spread 声明，脚本的实参作为整个数组传进来；缺省时用空切片。
        let empty: Vec<JsonValue> = Vec::new();
        let args: &[JsonValue] = match &call.args {
            Some(JsonValue::Array(items)) => items,
            _ => &empty,
        };
        let start = self.start_data();
        let tools: Vec<&DeclaredTool> = start.tools.iter().filter(|tool| !tool.hidden).collect();
        let namespace_of_tool = |name: &str| -> Option<&StartNamespace> {
            start
                .namespaces
                .iter()
                .find(|namespace| namespace.tools.iter().any(|tool| tool == name))
        };
        let matches_namespace = |namespace: &StartNamespace, query: &str| -> bool {
            namespace.name == query
                || to_codemode_identifier(&namespace.name) == to_codemode_identifier(query)
        };
        if call.name == "describeTool" {
            // 原工具名与脚本标识符都认；找不到返回 undefined（`Ok(None)`），不是错误。
            let name = args.first();
            let Some(JsonValue::String(name)) = name else {
                return Err("describeTool() expects a tool name".into());
            };
            return Ok(tools
                .iter()
                .find(|tool| &tool.name == name || &tool.js_name == name)
                .map(|tool| JsonValue::String(tool.description.clone())));
        }
        if call.name == "describeNamespace" {
            let name = args.first();
            let Some(JsonValue::String(name)) = name else {
                return Err("describeNamespace() expects a namespace name".into());
            };
            let Some(namespace) = start
                .namespaces
                .iter()
                .find(|candidate| matches_namespace(candidate, name))
            else {
                return Ok(None);
            };
            let mut value = JsonObject::new();
            value.insert("name".to_owned(), JsonValue::String(namespace.name.clone()));
            if let Some(description) = &namespace.description {
                value.insert(
                    "description".to_owned(),
                    JsonValue::String(description.clone()),
                );
            }
            if let Some(instructions) = &namespace.instructions {
                value.insert(
                    "instructions".to_owned(),
                    JsonValue::String(instructions.clone()),
                );
            }
            value.insert(
                "tools".to_owned(),
                JsonValue::Array(
                    namespace
                        .tools
                        .iter()
                        .map(|tool| JsonValue::String(to_codemode_identifier(tool)))
                        .collect(),
                ),
            );
            return Ok(Some(object_value(value)));
        }
        // searchTools：把工具名、下划线换空格后的名字、描述与所属命名空间拼成检索文本，
        // 交给 BM25 排序；返回条目的 name 是脚本标识符（js_name）。
        let query = args.first();
        let Some(JsonValue::String(query)) = query else {
            return Err("searchTools() expects a query string".into());
        };
        let options = args.get(1);
        // TS 的 `options?.limit ?? DEFAULT_SEARCH_LIMIT`：显式 `null` 与缺省一样回退。
        let limit = options
            .and_then(|options| options.get("limit"))
            .filter(|limit| !limit.is_null())
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_SEARCH_LIMIT));
        let Some(limit) = limit
            .as_f64()
            .filter(|limit| kiana_json::js_num::is_js_integer(*limit) && *limit > 0.0)
        else {
            return Err("searchTools() limit must be a positive integer".into());
        };
        let namespace = options.and_then(|options| options.get("namespace"));
        if let Some(namespace) = namespace
            && !namespace.is_null()
            && !namespace.is_string()
        {
            return Err("searchTools() namespace must be a string".into());
        }
        let namespace = namespace.and_then(JsonValue::as_str);
        let mut documents: Vec<SearchDocument<'_>> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for tool in &tools {
            let own = namespace_of_tool(&tool.name);
            if let Some(namespace) = namespace
                && !own.is_some_and(|own| matches_namespace(own, namespace))
            {
                continue;
            }
            let text = [
                tool.name.clone(),
                tool.name.replace('_', " "),
                tool.description.clone(),
                own.map(|own| own.name.clone()).unwrap_or_default(),
                own.and_then(|own| own.description.clone())
                    .unwrap_or_default(),
            ]
            .join(" ");
            texts.push(text);
            documents.push(SearchDocument {
                name: &tool.name,
                text: "",
            });
        }
        // 文档的文本借用 `texts`；先构造好再统一指向。
        for (index, text) in texts.iter().enumerate() {
            documents[index].text = text;
        }
        let ranked = rank_documents(query, &documents, limit as usize);
        let mut result = Vec::with_capacity(ranked.len());
        for name in ranked {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .expect("ranked names come from the tools");
            let mut entry = JsonObject::new();
            entry.insert("name".to_owned(), JsonValue::String(tool.js_name.clone()));
            entry.insert(
                "description".to_owned(),
                JsonValue::String(tool.description.clone()),
            );
            result.push(object_value(entry));
        }
        Ok(Some(JsonValue::Array(result)))
    }

    /// 在一次提交里 journal 已完成的调用与已确认的调用数，然后投递这些调用并启动计数覆盖的调用；
    /// 同时最多一次提交在飞，期间到达的进入下一批。
    ///
    /// 提交内容：`acks` 里 seq 0 写 `firstIssued`，seq >= 1 写对应条目的 `issuedAfter`（都只在
    /// 缺失时写），然后按 `queue` 的顺序追加新条目。提交成功后更新 `durable_issued`、递增
    /// `journal_length`，再按同一顺序投递——VM 看到的结果号与 journal 顺序严格一致。
    async fn pump(self: &Arc<Self>) {
        self.ready.wait().await;
        loop {
            // 只有一批提交在飞：`committing` 期间到达的调用留在 queue/acks，等本批走完。
            let (batch, acks, stopped) = {
                let mut state = self.state();
                let stopped = state.ended.is_some() || state.failure.is_some();
                if state.committing || stopped {
                    return;
                }
                if state.queue.is_empty() && state.acks.is_empty() {
                    return;
                }
                state.committing = true;
                let batch = std::mem::take(&mut state.queue);
                let acks: Vec<(u64, u64)> = state.acks.iter().map(|(k, v)| (*k, *v)).collect();
                state.acks.clear();
                (batch, acks, stopped)
            };
            let _ = stopped;
            let task_id = self.api.task_id();
            let api = Arc::clone(&self.api);
            let context = Arc::clone(&self.context);
            let batch_for_commit: Vec<QueueItem> = batch.iter().map(queue_item_clone).collect();
            let acks_for_commit = acks.clone();
            let committed = commit_value(api.as_ref(), context.as_ref(), move |tx| {
                let batch = batch_for_commit.clone();
                let acks = acks_for_commit.clone();
                Box::pin(async move {
                    let mut journal = tx
                        .doc(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task_id))
                        .await?;
                    let draft = journal.value_mut();
                    if draft.get("entries").is_none() {
                        draft.insert("entries".to_owned(), JsonValue::Array(Vec::new()));
                    }
                    for (seq, issued) in &acks {
                        if *seq == 0 && draft.get("firstIssued").is_none() {
                            draft.insert("firstIssued".to_owned(), json!(issued));
                        }
                    }
                    let entries = draft.get_mut("entries").and_then(JsonValue::as_array_mut);
                    if let Some(entries) = entries {
                        // 结果号 seq 对应条目下标 seq-1；只在缺失时补，不覆盖早前尝试记下的值。
                        for (seq, issued) in &acks {
                            if *seq == 0 {
                                continue;
                            }
                            if let Some(JsonValue::Object(entry)) =
                                entries.get_mut(*seq as usize - 1)
                                && entry.get("issuedAfter").is_none()
                            {
                                entry.insert("issuedAfter".to_owned(), json!(issued));
                            }
                        }
                        // 批内顺序就是完成顺序，也是随后的投递顺序。
                        for item in &batch {
                            let mut entry = JsonObject::new();
                            entry.insert("id".to_owned(), json!(item.id));
                            entry.insert("at".to_owned(), js_number::json(item.at));
                            if let Some(global) = &item.global {
                                entry.insert("global".to_owned(), serde_json::to_value(global)?);
                            }
                            entries.push(object_value(entry));
                        }
                    }
                    Ok(())
                })
            })
            .await;
            if let Err(error) = committed {
                {
                    let mut state = self.state();
                    state.committing = false;
                }
                self.fail(error);
                return;
            }
            let delivers: Vec<(u64, f64, u64, CodemodeOutcome)> = {
                let mut state = self.state();
                // 提交成功之后才更新：这些调用数已经落盘，覆盖的调用可以启动。
                for (_, issued) in &acks {
                    state.durable_issued = state.durable_issued.max(*issued);
                }
                let mut delivers = Vec::with_capacity(batch.len());
                for item in &batch {
                    state.journal_length += 1;
                    delivers.push((
                        state.journal_length as u64,
                        item.at,
                        item.id,
                        item.outcome.clone(),
                    ));
                }
                delivers
            };
            // `committing` 直到这批结果投递完、它们覆盖的调用启动完才清掉（TS 的
            // codemode.ts:1000-1007）：投递会引出新的调用，其中很快完成的会在 `launch_covered()`
            // 之后触发另一次 `pump()`；它若在这里就提交下一批并投递更大的结果号，VM 看到的投递
            // 顺序就不再是 journal 顺序。
            for (seq, at, id, outcome) in delivers {
                if self.run_finished() {
                    continue;
                }
                if let Err(error) = self.deliver(seq as f64, at, id, outcome) {
                    self.fail(error);
                    return;
                }
            }
            self.launch_covered();
            self.state().committing = false;
        }
    }
}

/// 首次运行返回的发现结果。
enum Found {
    /// 已决定的结局。
    Done(ToolExecutionResult),
    /// start 数据与 journal。
    Start {
        start: Start,
        journal: Vec<JournalEntry>,
        first_issued: Option<u64>,
        rerun: bool,
    },
}

/// 脚本对 `models.classify()` 或 `models.generateImages()` 的调用，作为它内部工具的调用。
///
/// 脚本按 spread 方式调用，参数数组的前两项分别成为内部工具的 `model` 与 `context`，多余项被
/// 忽略。改写后 target 变成 Tool，于是它会走 `call_tool()`：有嵌套任务、live 行与用量记账。
fn as_tool_call(call: CodemodeHostCall) -> CodemodeHostCall {
    let tool = if call.target == CodemodeCallTarget::Global {
        super::models::model_call(&call.name)
    } else {
        None
    };
    let Some(tool) = tool else {
        return call;
    };
    let empty: Vec<JsonValue> = Vec::new();
    let list: &[JsonValue] = match &call.args {
        Some(JsonValue::Array(items)) => items,
        _ => &empty,
    };
    let mut args = JsonObject::new();
    args.insert(
        "model".to_owned(),
        list.first().cloned().unwrap_or(JsonValue::Null),
    );
    args.insert(
        "context".to_owned(),
        list.get(1).cloned().unwrap_or(JsonValue::Null),
    );
    CodemodeHostCall {
        id: call.id,
        target: CodemodeCallTarget::Tool,
        name: tool.to_owned(),
        args: Some(object_value(args)),
    }
}

/// `models.generateImages()` 结果里的图片块数。
///
/// 结果形状是 `{ output: [...] }`；只数 `type == "image"` 的项，其他形状返回 0。用来发现脚本
/// 从没把返回的图片展示出来。
fn count_images(value: Option<&JsonValue>) -> usize {
    let Some(JsonValue::Object(map)) = value else {
        return 0;
    };
    match map.get("output") {
        Some(JsonValue::Array(output)) => output
            .iter()
            .filter(|item| {
                matches!(item, JsonValue::Object(map) if map.get("type").and_then(JsonValue::as_str) == Some("image"))
            })
            .count(),
        _ => 0,
    }
}

/// 脚本发出工具调用时它的行立刻出现，运行时也如此；等待启动时也是。
///
/// 内部工具名去掉 `kiana.codemode.` 前缀后显示（例如 `models.classify`）；参数预览裁到
/// `ARGS_PREVIEW_CHARS` 个 UTF-16 码元。
fn started(state: &mut CallState, call: &CodemodeHostCall) {
    if state.rows.contains_key(&call.id) {
        return;
    }
    let args = match &call.args {
        None => String::new(),
        Some(args) => kiana_json::js_object::stringify(args),
    };
    let name = call
        .name
        .strip_prefix(CODEMODE_INTERNAL_PREFIX)
        .unwrap_or(&call.name)
        .to_owned();
    state.rows.insert(
        call.id,
        CodemodeNestedCall {
            id: call.id,
            name,
            args: cut(&args, ARGS_PREVIEW_CHARS),
            status: NestedCallStatus::Running,
            duration_ms: None,
            error: None,
        },
    );
}

/// 生成脚本的随机 seed：取 UUID v4 的前四个字节按小端解释成 `u32`。
///
/// 范围是 `0..=0xffffffff`，满足 `CodemodeRunOptions.seed` 的取值要求；同一个 seed 会让 VM 的
/// `Math.random()` 序列可重放。
fn random_u32() -> u32 {
    let bytes = uuid::Uuid::new_v4();
    let bytes = bytes.as_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// `CodemodeResult` 的读取辅助。
///
/// `CodemodeResult` 定义在 kiana-codemode，本文件只关心“成功与否”和“结束时的 during”两件事；
/// 用扩展 trait 把两个变体的匹配收在一处。
trait CodemodeResultExt {
    /// 是否成功。
    fn result_ok(&self) -> bool;
    /// 结束时的 `during`。
    fn during(&self) -> Option<CodemodeDuring>;
}

impl CodemodeResultExt for CodemodeResult {
    fn result_ok(&self) -> bool {
        matches!(self, CodemodeResult::Ok(_))
    }

    fn during(&self) -> Option<CodemodeDuring> {
        match self {
            CodemodeResult::Ok(result) => result.during,
            CodemodeResult::Err(result) => result.during,
        }
    }
}

/// code mode 工具。脚本以 `tools.<name>(args)` 调用会话的可调用工具。
///
/// 注册项的要点：
/// - `replay: Safe`：工具任务被中断时 Harness 可以重跑 `execute()`，run/journal 文档让这次
///   重跑变成重放，已经完成的嵌套调用不会重复执行。
/// - `callers: [Model]`：只提供给模型；`agent.callable` 不含它，所以脚本不能递归调用 code mode。
/// - `output_limits` 与 `format_result()` 共用 `OUTPUT_LIMITS`，Harness 只做最后兜底。
/// - `constrained_sampling` 让支持的模型直接生成原始 JS，而不是 JSON 转义后的字符串。
pub fn create_codemode_tool(options: CodemodeToolOptions) -> ToolRegistration {
    let options = Arc::new(options);
    let models = options.models_enabled();
    let execute: ToolExecuteFn = Arc::new(move |args, api, context| {
        let options = Arc::clone(&options);
        Box::pin(async move {
            // 缺字段或类型不对时按空串处理，由 `parse_codemode_source` 给出明确错误。
            let code = args
                .get("code")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned();
            let call = Arc::new(CodemodeCall::new(options, api, context));
            call.execute(&code).await
        })
    });
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: CODEMODE_TOOL_NAME.to_owned(),
            description: codemode_description(models),
            parameters: json!({
                "type": "object",
                "required": ["code"],
                "properties": {
                    "code": { "type": "string", "description": "Raw JavaScript source." }
                }
            }),
            // 有能力的模型把脚本写成原始文本而不是 JSON 转义字符串；kiana-ai 只对它们应用。
            constrained_sampling: Some(ConstrainedSampling::Config(
                ConstrainedSamplingConfig::Grammar {
                    variants: GrammarVariants {
                        openai_lark: Some(CODEMODE_SOURCE_GRAMMAR.to_owned()),
                        openai_regex: None,
                    },
                },
            )),
        },
        replay: Some(ReplayPolicy::Safe),
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: Some(vec![ToolCaller::Model]),
        deferred: false,
        output_limits: Some(OutputLimits {
            max_bytes: Some(OUTPUT_LIMITS.max_bytes as u64),
            max_lines: Some(OUTPUT_LIMITS.max_lines as u64),
            retain: None,
        }),
        execute: Some(execute),
    })
}

/// 扩展的静态部分：工具与提示段在创建时确定，之后每次返回克隆。
struct CodemodeExtension {
    tools: Vec<ToolRegistration>,
    sections: Vec<Arc<dyn PromptSection>>,
}

impl Extension for CodemodeExtension {
    fn name(&self) -> &str {
        "codemode"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }

    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
}

/// `codemode` 扩展：code mode 工具，以及列出脚本可调用工具的 `codemode` 提示段。
///
/// `models` 打开时还会注册 `models.classify()`/`models.generateImages()` 两个内部工具
/// （`callers: [Tools]`，名字带保留前缀，不出现在脚本的 `tools` 里）。
/// 提示段每次渲染都读 `input.agent.callable`，排除内部工具与 deferred 工具：延迟工具改用
/// `searchTools()` 查找，coding agent 也不把它们排进声明。
pub fn create_codemode_extension(options: CodemodeToolOptions) -> Arc<dyn Extension> {
    let models = options.models_enabled();
    let mut tools = vec![create_codemode_tool(options.clone())];
    if models {
        tools.extend(create_model_tools());
    }
    let namespaces = options.namespaces.clone().unwrap_or_default();
    let budget = options
        .inline_budget
        .unwrap_or(DEFAULT_CODEMODE_INLINE_BUDGET);
    let render: crate::harness::define::SectionRender = Arc::new(move |input, _context| {
        // 延迟工具用 `searchTools()` 找，coding agent 也把它们排除在外。
        let callable: Vec<ToolRegistration> = input
            .agent
            .callable
            .iter()
            .filter(|tool| !tool.tool.name.starts_with(CODEMODE_INTERNAL_PREFIX) && !tool.deferred)
            .cloned()
            .collect();
        let namespaces = namespaces.clone();
        Box::pin(async move { Ok(codemode_section(&callable, &namespaces, budget)) })
    });
    let sections = vec![section("codemode", render, None)];
    define_extension(CodemodeExtension { tools, sections })
}
