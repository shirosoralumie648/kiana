//! 验收检查：用代码判定 acceptance criteria，不可判定一律 `UNVERIFIED`。
//!
//! 关键约束：
//!
//! - **runtime-stamped receipt**：每条执行记录由 [`ExecutionRecord::record`] 在运行时
//!   计算 `args_sha256` / `output_sha256` / `seq`；判定前用 [`ExecutionRecord::verify`]
//!   重新计算并比对，防止“后来补写的凭证”被当作执行证据。
//! - `tests_passed:<command>` 必须锚定到**真实执行**：存在匹配的 shell 执行记录、
//!   receipt 校验通过、命令结构匹配（`&&`/`;`/`||` 分段后按 token 比较可执行文件与
//!   参数；`echo` 里提到命令不算；额外 flag 必须可证明不改变测试选择）、且输出包含
//!   测试摘要形状。没有匹配执行 → `UNVERIFIED`，绝不静默通过。
//! - 文件检查有界读取（50,000 字节）；超限、符号链接、目录、解析失败都降级为
//!   `UNVERIFIED`/`checked=false`，不做无界读取。
//! - `acceptance` 只做判定，不执行任何修复；失败/未知不会阻断 agent 主循环。
//!
//! 参考设计：
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/subagents/acceptance_checks.py`
//!   的 leaf 家族（`file:` / `file_written:` / `tests_passed:`）、`_TEST_FAIL_SHAPE_RE` /
//!   `_TEST_PASS_SHAPE_RE`、`checked`/`holds` 词汇分层与 UNVERIFIED 语义；
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/agents/middlewares/tool_receipt.py`
//!   的 `args_sha256` / `output_sha256` 运行时凭证；
//! - `reference_project/hermes-agent/tools/checkpoint_manager.py` 的有界读取与
//!   “文件不存在不等于内容为空”的区分。

use std::fs;
use std::path::{Component, Path, PathBuf};

use kiana_ai::types::JsonValue;

use crate::subagent::result::{ReceiptStatus, ToolReceipt, sha256_hex, sha256_json};

/// 文件检查的最大读取字节数。
pub const ACCEPTANCE_READ_CAP_BYTES: u64 = 50_000;

/// 验收结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatus {
    /// 所有 leaf 都可判定且成立。
    Passed,
    /// 至少一个可判定 leaf 不成立。
    Failed,
    /// 存在不可判定的 leaf（或没有 criterion）。
    Unverified,
}

impl AcceptanceStatus {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
        }
    }
}

/// criterion 家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceFamily {
    /// `file:<path> exists`
    FileExists,
    /// `file:<path> non-empty`
    FileNonEmpty,
    /// `file:<path> json-valid`
    FileJsonValid,
    /// `file:<path> csv-columns:["id","name"]`
    FileCsvColumns,
    /// `file_written:<path>`
    FileWritten,
    /// `tests_passed:<command>`
    TestsPassed,
    /// 无法用代码判定。
    Undecidable,
}

impl AcceptanceFamily {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FileExists => "file_exists",
            Self::FileNonEmpty => "file_non_empty",
            Self::FileJsonValid => "file_json_valid",
            Self::FileCsvColumns => "file_csv_columns",
            Self::FileWritten => "file_written",
            Self::TestsPassed => "tests_passed",
            Self::Undecidable => "undecidable",
        }
    }
}

/// 单条 criterion 的判定。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceLeaf {
    /// 原始 criterion（截断到 512 字符）。
    pub criterion: String,
    /// 家族。
    pub family: AcceptanceFamily,
    /// 是否执行了确定性检查。
    pub checked: bool,
    /// `checked && 条件成立`。
    pub holds: bool,
    /// 有界证据说明。
    pub detail: String,
}

/// 验收报告。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceReport {
    /// 每条 criterion 的判定。
    pub leaves: Vec<AcceptanceLeaf>,
    /// 聚合结论。
    pub status: AcceptanceStatus,
    /// 没有确定性检查的 criterion。
    pub unchecked: Vec<String>,
}

impl AcceptanceReport {
    /// 是否全部可判定且成立。
    pub fn all_hold(&self) -> bool {
        self.status == AcceptanceStatus::Passed
    }
}

/// 运行时执行记录。
///
/// 只有通过 [`Self::record`] 构造（运行时计算哈希）且 [`Self::verify`] 通过的记录
/// 才能作为 `tests_passed` 的证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    /// 工具名。
    pub tool: String,
    /// 工具参数（运行时捕获）。
    pub arguments: JsonValue,
    /// 工具输出（运行时捕获，尽量保留完整输出而不是 UI 截断版本）。
    pub output: String,
    /// 运行时凭证。
    pub receipt: ToolReceipt,
}

impl ExecutionRecord {
    /// 运行时打戳。
    pub fn record(
        tool: impl Into<String>,
        arguments: JsonValue,
        output: impl Into<String>,
        status: ReceiptStatus,
        seq: u64,
    ) -> Self {
        let tool = tool.into();
        let output = output.into();
        let receipt = ToolReceipt {
            tool: tool.clone(),
            args_sha256: sha256_json(&arguments),
            output_sha256: sha256_hex(output.as_bytes()),
            status,
            seq,
        };
        Self {
            tool,
            arguments,
            output,
            receipt,
        }
    }

    /// 重新计算并比对凭证（防止持久化后被改写）。
    pub fn verify(&self) -> bool {
        if self.receipt.tool != self.tool || self.receipt.seq == 0 || self.receipt.tool.is_empty() {
            return false;
        }
        if !is_sha256(&self.receipt.args_sha256) || !is_sha256(&self.receipt.output_sha256) {
            return false;
        }
        self.receipt.args_sha256 == sha256_json(&self.arguments)
            && self.receipt.output_sha256 == sha256_hex(self.output.as_bytes())
    }

    /// 从参数里取命令字符串。
    pub fn command(&self) -> Option<String> {
        if let Some(command) = self
            .arguments
            .get("command")
            .or_else(|| self.arguments.get("cmd"))
            .and_then(JsonValue::as_str)
        {
            return Some(command.to_string());
        }
        if let Some(argv) = self.arguments.get("argv").and_then(JsonValue::as_array) {
            let parts: Vec<&str> = argv.iter().filter_map(JsonValue::as_str).collect();
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
        None
    }
}

/// 验收上下文。
#[derive(Debug, Clone)]
pub struct AcceptanceContext<'a> {
    /// 文件检查的根目录（所有相对路径都相对它解析）。
    pub workdir: PathBuf,
    /// 运行时执行记录。
    pub executions: &'a [ExecutionRecord],
}

impl<'a> AcceptanceContext<'a> {
    /// 新建。
    pub fn new(workdir: impl Into<PathBuf>, executions: &'a [ExecutionRecord]) -> Self {
        Self {
            workdir: workdir.into(),
            executions,
        }
    }
}

/// 判定一组 criteria。
pub fn evaluate_acceptance(
    criteria: &[String],
    context: &AcceptanceContext<'_>,
) -> AcceptanceReport {
    let mut leaves = Vec::new();
    for criterion in criteria {
        let criterion = criterion.trim();
        if criterion.is_empty() {
            continue;
        }
        leaves.push(evaluate_leaf(criterion, context));
    }
    let unchecked: Vec<String> = leaves
        .iter()
        .filter(|leaf| !leaf.checked)
        .map(|leaf| leaf.criterion.clone())
        .collect();
    let status = if leaves.is_empty() {
        AcceptanceStatus::Unverified
    } else if leaves.iter().any(|leaf| leaf.checked && !leaf.holds) {
        AcceptanceStatus::Failed
    } else if leaves.iter().any(|leaf| !leaf.checked) {
        AcceptanceStatus::Unverified
    } else {
        AcceptanceStatus::Passed
    };
    AcceptanceReport {
        leaves,
        status,
        unchecked,
    }
}

fn evaluate_leaf(criterion: &str, context: &AcceptanceContext<'_>) -> AcceptanceLeaf {
    let bounded = bound_detail(criterion, 512);
    let mut leaf = AcceptanceLeaf {
        criterion: bounded,
        family: AcceptanceFamily::Undecidable,
        checked: false,
        holds: false,
        detail: String::new(),
    };

    if let Some(rest) = strip_prefix_ci(criterion, "file_written:") {
        leaf.family = AcceptanceFamily::FileWritten;
        evaluate_file(rest.trim(), FileMode::Written, context, &mut leaf);
        return leaf;
    }
    if let Some(rest) = strip_prefix_ci(criterion, "file:") {
        let rest = rest.trim();
        if let Some(path) = strip_suffix_word_ci(rest, "exists") {
            leaf.family = AcceptanceFamily::FileExists;
            evaluate_file(path.trim(), FileMode::Exists, context, &mut leaf);
            return leaf;
        }
        if let Some(path) = strip_suffix_word_ci(rest, "non-empty") {
            leaf.family = AcceptanceFamily::FileNonEmpty;
            evaluate_file(path.trim(), FileMode::NonEmpty, context, &mut leaf);
            return leaf;
        }
        if let Some(path) = strip_suffix_word_ci(rest, "json-valid") {
            leaf.family = AcceptanceFamily::FileJsonValid;
            evaluate_file(path.trim(), FileMode::JsonValid, context, &mut leaf);
            return leaf;
        }
        if let Some(index) = rest.to_ascii_lowercase().rfind(" csv-columns:") {
            let path = rest[..index].trim();
            let columns = &rest[index + " csv-columns:".len()..];
            leaf.family = AcceptanceFamily::FileCsvColumns;
            evaluate_file(
                path,
                FileMode::CsvColumns(columns.trim()),
                context,
                &mut leaf,
            );
            return leaf;
        }
        leaf.detail = "unrecognized file criterion".to_string();
        return leaf;
    }
    if let Some(command) = strip_prefix_ci(criterion, "tests_passed:") {
        leaf.family = AcceptanceFamily::TestsPassed;
        evaluate_tests_passed(command.trim(), context, &mut leaf);
        return leaf;
    }
    leaf.family = AcceptanceFamily::Undecidable;
    leaf.detail = "no deterministic check for this criterion".to_string();
    leaf
}

enum FileMode<'a> {
    Exists,
    NonEmpty,
    Written,
    JsonValid,
    CsvColumns(&'a str),
}

fn evaluate_file(
    path: &str,
    mode: FileMode<'_>,
    context: &AcceptanceContext<'_>,
    leaf: &mut AcceptanceLeaf,
) {
    let Some(resolved) = resolve_scoped_path(&context.workdir, path) else {
        leaf.detail = format!("path is outside the workdir or not resolvable: {path}");
        return;
    };
    // 打开一次，之后所有判断都基于同一个 fd：先 stat 再按路径读取会被“符号链接
    // 换成 /dev/zero / FIFO”击败（无界读取或阻塞单线程 LocalSet）。
    let mut file = match open_for_check(&resolved) {
        Ok(file) => file,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                leaf.checked = true;
                leaf.holds = false;
                leaf.detail = format!("file does not exist: {path}");
                return;
            }
            #[cfg(unix)]
            if error.raw_os_error() == Some(libc::ELOOP) {
                leaf.detail = format!("refusing to follow symlink: {path}");
                return;
            }
            leaf.detail = format!("could not open file: {path}");
            return;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            leaf.detail = format!("could not stat file: {path}");
            return;
        }
    };
    if metadata.is_dir() {
        // 参照实现的 size probe 对非普通文件返回 None，结论降级为 UNVERIFIED；
        // 目录不是「文件不存在」，也不是「检查已执行且不成立」。
        leaf.detail = format!("path is a directory, not a file: {path}");
        return;
    }
    if !metadata.is_file() {
        leaf.detail = format!("path is not a regular file: {path}");
        return;
    }
    let size = metadata.len();
    match mode {
        FileMode::Exists => {
            leaf.checked = true;
            leaf.holds = true;
            leaf.detail = format!("file exists ({size} bytes)");
        }
        FileMode::NonEmpty => {
            leaf.checked = true;
            leaf.holds = size > 0;
            leaf.detail = if size > 0 {
                format!("file is non-empty ({size} bytes)")
            } else {
                "file is empty".to_string()
            };
        }
        FileMode::Written => {
            // file_written 要求存在 + 可回读，而不只是元数据存在。零字节文件同样
            // 要打开并回读一次：参照实现把「0 字节回读成功」视为 holds=true。
            match std::io::Read::read(&mut file, &mut [0u8; 1]) {
                Ok(_) => {
                    leaf.checked = true;
                    leaf.holds = true;
                    leaf.detail = format!("file exists and is readable ({size} bytes)");
                }
                Err(_) => {
                    leaf.detail = "file exists but read-back failed".to_string();
                }
            }
        }
        FileMode::JsonValid => {
            if size > ACCEPTANCE_READ_CAP_BYTES {
                leaf.detail = format!(
                    "file is {size} bytes, above the {} byte read cap",
                    ACCEPTANCE_READ_CAP_BYTES
                );
                return;
            }
            let Some(bytes) = read_bounded(&mut file) else {
                leaf.detail = "could not read file".to_string();
                return;
            };
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    leaf.checked = true;
                    leaf.holds = false;
                    leaf.detail = "invalid UTF-8 JSON syntax".to_string();
                    return;
                }
            };
            // kiana-json 复刻 V8 的接受范围（1e400、任意深度、孤立代理项）；但它的
            // 文档级特例不是 JSON 文档，要显式拒绝。
            if matches!(
                text.trim(),
                "undefined" | "Infinity" | "NaN" | "[object Object]"
            ) {
                leaf.checked = true;
                leaf.holds = false;
                leaf.detail = "invalid JSON syntax".to_string();
                return;
            }
            leaf.checked = true;
            match kiana_json::parse(&text) {
                Ok(_) => {
                    leaf.holds = true;
                    leaf.detail = "valid JSON".to_string();
                }
                Err(error) => {
                    leaf.holds = false;
                    leaf.detail =
                        format!("invalid JSON: {}", bound_detail(&error.to_string(), 160));
                }
            }
        }
        FileMode::CsvColumns(spec) => {
            // 只验证第一条 CSV 记录：数据行里出现非法 UTF-8 与判定无关（参照实现用
            // surrogateescape 解码完整前缀，只对 header 做 UTF-8 校验）。因此不再
            // 要求整个文件小于读取上限，而是读取 `cap+1` 字节的有界前缀。
            let Some(required) = parse_columns_spec(spec) else {
                leaf.detail =
                    "csv-columns requires a nonempty JSON array of unique nonempty column names"
                        .to_string();
                return;
            };
            let mut bytes = Vec::new();
            let mut limited = std::io::Read::take(&mut file, ACCEPTANCE_READ_CAP_BYTES + 1);
            if std::io::Read::read_to_end(&mut limited, &mut bytes).is_err() {
                leaf.detail = "bounded CSV read failed".to_string();
                return;
            }
            let expected_prefix = size.min(ACCEPTANCE_READ_CAP_BYTES + 1);
            if bytes.len() as u64 != expected_prefix {
                leaf.detail = "complete CSV prefix unavailable within the read cap".to_string();
                return;
            }
            let complete = bytes.len() as u64 == size;
            let (columns, consumed, malformed, incomplete) =
                parse_csv_header_record(&bytes, complete);
            let header_bytes = consumed;
            if !malformed && !complete && incomplete {
                leaf.detail = "CSV header incomplete within the read cap".to_string();
                return;
            }
            if header_bytes > ACCEPTANCE_READ_CAP_BYTES {
                leaf.detail =
                    format!("CSV header exceeds the {ACCEPTANCE_READ_CAP_BYTES}-byte read budget");
                return;
            }
            if malformed {
                leaf.checked = true;
                leaf.detail = "invalid CSV header".to_string();
                return;
            }
            if !complete
                && !bytes[..consumed as usize].ends_with(b"\n")
                && !bytes[..consumed as usize].ends_with(b"\r")
            {
                leaf.detail = "CSV header incomplete within the read cap".to_string();
                return;
            }
            let Some(columns) = columns else {
                leaf.checked = true;
                leaf.detail = "CSV header is not UTF-8".to_string();
                return;
            };
            leaf.checked = true;
            let header_set: std::collections::BTreeSet<&String> = columns.iter().collect();
            let missing: Vec<&String> = required
                .iter()
                .filter(|column| !header_set.contains(column))
                .collect();
            if columns.is_empty()
                || columns.iter().any(|column| column.is_empty())
                || header_set.len() != columns.len()
            {
                leaf.detail =
                    "CSV header is empty or contains empty or duplicate column names".to_string();
            } else if !missing.is_empty() {
                leaf.detail =
                    format!("CSV header {columns:?} is missing required columns {missing:?}");
            } else {
                leaf.holds = true;
                leaf.detail =
                    "CSV header contains required columns; data rows not validated".to_string();
            }
        }
    }
}

/// 打开用于检查的文件：不跟随最终组件的符号链接，且不会因为 FIFO 等特殊文件
/// 阻塞打开。
fn open_for_check(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// 从已打开的 fd 读取最多 `ACCEPTANCE_READ_CAP_BYTES` 字节；超过上限返回 `None`
/// （调用方按 UNVERIFIED 处理）。读取量由 `take` 硬限制，不依赖 stat 的大小。
fn read_bounded(file: &mut fs::File) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(file, ACCEPTANCE_READ_CAP_BYTES + 1);
    std::io::Read::read_to_end(&mut limited, &mut bytes).ok()?;
    (bytes.len() as u64 <= ACCEPTANCE_READ_CAP_BYTES).then_some(bytes)
}

fn evaluate_tests_passed(
    command: &str,
    context: &AcceptanceContext<'_>,
    leaf: &mut AcceptanceLeaf,
) {
    if command.is_empty() {
        leaf.detail = "tests_passed requires a command".to_string();
        return;
    }
    // 只看最新一次“文本命中”的执行：先匹配命令，再要求 receipt 校验通过。
    //
    // 参照实现把“文本命中但控制流/归属无法证明”也算作一次匹配（`unprovable`），
    // 而且最新的匹配是决定性的——后面一次无法证明的重跑必须把更早的通过覆盖成
    // UNVERIFIED，而不是让旧证据继续生效。
    let mut latest: Option<(CommandMatch, &ExecutionRecord)> = None;
    for execution in context.executions {
        if !is_shell_tool(&execution.tool) || !execution.verify() {
            continue;
        }
        let Some(executed) = execution.command() else {
            continue;
        };
        let outcome = commands_match(
            command,
            &executed,
            execution.receipt.status == ReceiptStatus::Succeeded,
        );
        if outcome == CommandMatch::NoMatch {
            continue;
        }
        if latest.map(|(_, current)| execution.receipt.seq >= current.receipt.seq) != Some(false) {
            latest = Some((outcome, execution));
        }
    }
    let Some((outcome, execution)) = latest else {
        leaf.detail = "no matching recorded shell execution with a valid receipt".to_string();
        return;
    };
    if outcome == CommandMatch::Unprovable {
        leaf.detail = "matching segment cannot be proven to have executed".to_string();
        return;
    }
    match execution.receipt.status {
        ReceiptStatus::Succeeded => {}
        other => {
            leaf.checked = true;
            leaf.holds = false;
            leaf.detail = format!(
                "latest matching execution recorded status={}",
                other.as_str()
            );
            return;
        }
    }
    // 记录的输出尾巴必须属于匹配段：末段重定向或前面有非静默段时，通过/失败
    // 形状都可能来自别处，不能作为测试证据。
    if let Some(reason) = output_attribution(
        execution.command().as_deref().unwrap_or_default(),
        &context.workdir,
    ) {
        leaf.detail = reason;
        return;
    }
    match summarize_test_output(&execution.output) {
        TestSummary::Passed => {
            leaf.checked = true;
            leaf.holds = true;
            leaf.detail = "recorded output carries a passing test summary".to_string();
        }
        TestSummary::Failed => {
            leaf.checked = true;
            leaf.holds = false;
            leaf.detail = "recorded output carries a failing test summary".to_string();
        }
        TestSummary::Unverified => {
            leaf.detail =
                "matching execution recorded no recognizable test-summary shape".to_string();
        }
    }
}

/// 对一段输出做公开的测试摘要分类（便于调用方/测试复用同一套形状判断）。
pub fn classify_test_output(output: &str) -> AcceptanceStatus {
    match summarize_test_output(output) {
        TestSummary::Passed => AcceptanceStatus::Passed,
        TestSummary::Failed => AcceptanceStatus::Failed,
        TestSummary::Unverified => AcceptanceStatus::Unverified,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestSummary {
    Passed,
    Failed,
    Unverified,
}

/// 识别测试摘要形状（参照实现的 `summarize_test_output`）。
///
/// 失败形状优先；Node 的 TAP 测试点先被剔除，只有完整的 counters 块才算 Node 证据；
/// Go 的 `ok` 行要求没有 `--- SKIP` 独占（或至少有一个 `--- PASS`）；unittest 要求
/// 实际执行数大于 skipped/expected failures；JVM 只认计数行，构建 banner 不算。
fn summarize_test_output(output: &str) -> TestSummary {
    let text = strip_node_test_points(output);
    let build_summary = build_test_summary_holds(&text);
    let node_summary = node_test_summary_outcome(&text);
    // 完整的 Node counters 拥有这一族输出：只保留可识别的兄弟 runner 摘要，
    // 防止 TAP 点或程序日志里的 `ok`/`passed` 混入证据。
    let text = if node_summary.is_some() {
        sibling_summary_text(&text)
    } else {
        text
    };
    if contains_fail_shape(&text)
        || build_summary == Some(false)
        || node_summary == Some(NodeOutcome::Failed)
    {
        return TestSummary::Failed;
    }
    // Cargo 会为单元/集成/doctest 等 target 分别输出 summary。一个空 target 的
    // `0 passed` 不能否决另一个真正跑过测试的 target；只有完整、成功的兄弟
    // summary 才能豁免空 target（参照实现的 `_without_empty_cargo_targets`）。
    let summary_text = without_empty_cargo_targets(&without_go_zero_test_lines(&text));
    let go_ok =
        contains_go_pass_shape(&summary_text) && go_package_summary_is_evidence(&summary_text);
    let passing = contains_pass_shape(&summary_text)
        || go_ok
        || unittest_test_summary_holds(&summary_text)
        || build_summary == Some(true)
        || node_summary == Some(NodeOutcome::Passed);
    if passing && !contains_zero_test_shape(&summary_text) {
        TestSummary::Passed
    } else {
        TestSummary::Unverified
    }
}

/// Node 完整 counters 的判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeOutcome {
    Passed,
    Failed,
    Unverified,
}

/// 剔除 ANSI 颜色与 Node 的 TAP/spec 测试点行。
///
/// 单个测试点（`ok 1 - name`、`not ok 2`）不是完整的运行摘要；`# pass 1` 之类的
/// 计数行保留给 [`node_test_summary_outcome`] 判断。
fn strip_node_test_points(output: &str) -> String {
    let ansi = regex::Regex::new("\x1b\\[[0-9;:]*m").expect("ansi regex");
    let text = ansi.replace_all(output, "").into_owned();
    let points = regex::Regex::new(concat!(
        r"(?m)^[ \t]*(?:#[ \t]+Subtest:[^\r\n]*",
        r"|(?:not )?ok[ \t]+[0-9]+(?:[ \t]+[-#][^\r\n]*|[ \t]*)",
        r"|[✔✖﹣▶⚠][ \t]+[^\r\n]*)\r?$"
    ))
    .expect("node point regex");
    points.replace_all(&text, "").into_owned()
}

/// 从完整、自洽的 Node counters 块读出判决。
///
/// Rust 的 `regex` 不支持反向引用，因此对 `#` 与 `ℹ` 两种前缀各编译一条
/// 完整块正则（块内前缀必须一致）。
fn node_test_summary_outcome(text: &str) -> Option<NodeOutcome> {
    let mut positive = false;
    let mut complete = false;
    for prefix in ["#", "ℹ"] {
        let pattern = format!(
            concat!(
                r"(?m)^{p}[ \t]+tests ([0-9]{{1,9}})\r?\n",
                r"{p}[ \t]+suites [0-9]{{1,9}}\r?\n",
                r"{p}[ \t]+pass ([0-9]{{1,9}})\r?\n",
                r"{p}[ \t]+fail ([0-9]{{1,9}})\r?\n",
                r"{p}[ \t]+cancelled ([0-9]{{1,9}})\r?\n",
                r"{p}[ \t]+skipped ([0-9]{{1,9}})\r?\n",
                r"{p}[ \t]+todo ([0-9]{{1,9}})[ \t]*\r?$"
            ),
            p = regex::escape(prefix)
        );
        let Ok(regex) = regex::Regex::new(&pattern) else {
            continue;
        };
        for captures in regex.captures_iter(text) {
            let value = |index: usize| {
                captures
                    .get(index)
                    .and_then(|matched| matched.as_str().parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let (total, passed, failed, cancelled, skipped, todo) =
                (value(1), value(2), value(3), value(4), value(5), value(6));
            if total != passed + failed + cancelled + skipped + todo {
                continue;
            }
            complete = true;
            if failed > 0 || cancelled > 0 {
                return Some(NodeOutcome::Failed);
            }
            positive = positive || passed > 0;
        }
    }
    if positive {
        Some(NodeOutcome::Passed)
    } else if complete {
        Some(NodeOutcome::Unverified)
    } else {
        None
    }
}

/// Node 拥有输出时保留的兄弟 runner 摘要行（pytest/cargo/go/mocha/unittest）。
fn sibling_summary_text(text: &str) -> String {
    let sibling = regex::Regex::new(concat!(
        r"(?m)^[ \t=]*(?:[0-9]+ (?:passed|failed|errors?|warnings?|skipped|deselected|xfailed|xpassed)(?:, )?)+ in [0-9]+(?:\.[0-9]+)?s",
        r"(?: \((?:[0-9]+ days?, )?[0-9]+:[0-5][0-9]:[0-5][0-9]\))?[ \t=]*\r?$",
        r"|^test result: (?:ok|FAILED)\.[^\r\n]*$",
        r"|^(?:ok|FAIL)[ \t]+[^\r\n]+$",
        r"|^[ \t]*--- (?:PASS|FAIL|SKIP):[^\r\n]*$",
        r"|^Ran [0-9]+ tests? in [0-9]+(?:\.[0-9]+)?s\r?\n\r?\n(?:OK|FAILED)[^\r\n]*$"
    ))
    .expect("sibling regex");
    let maven = maven_summary_regex();
    let gradle = gradle_summary_regex();
    let mut lines: Vec<String> = Vec::new();
    for captures in sibling.captures_iter(text) {
        if let Some(matched) = captures.get(0) {
            lines.push(matched.as_str().to_string());
        }
    }
    for regex in [maven, gradle] {
        for captures in regex.captures_iter(text) {
            if let Some(matched) = captures.get(0) {
                lines.push(matched.as_str().to_string());
            }
        }
    }
    lines.join("\n")
}

fn maven_summary_regex() -> regex::Regex {
    regex::Regex::new(concat!(
        r"(?m)^[ \t]*(?:\[(?:INFO|WARNING|ERROR)\][ \t]*)?Tests run:[ \t]*([0-9]+),[ \t]*",
        r"Failures:[ \t]*([0-9]+),[ \t]*Errors:[ \t]*([0-9]+),[ \t]*",
        r"Skipped:[ \t]*([0-9]+)(?:,[ \t]*Time elapsed:[^\r\n]*)?[ \t]*\r?$"
    ))
    .expect("maven summary regex")
}

fn gradle_summary_regex() -> regex::Regex {
    regex::Regex::new(concat!(
        r"(?m)^[ \t]*([0-9]+) tests? completed(?:,[ \t]*([0-9]+) failed)?",
        r"(?:,[ \t]*([0-9]+) skipped)?[ \t]*\r?$"
    ))
    .expect("gradle summary regex")
}

/// JVM 计数行证据：`None` 表示没有可执行测试的证明，`Some(false)` 表示有失败。
///
/// 构建成功 banner 永远不参与（参照实现的 `_build_test_summary_holds`）。
fn build_test_summary_holds(text: &str) -> Option<bool> {
    let mut positive = false;
    for captures in maven_summary_regex().captures_iter(text) {
        // 只看捕获组：group 0 是整行，长度恒大于 9，把它算进来会让所有
        // Maven 摘要都被跳过（参照实现检查的是 named groups）。
        if captures
            .iter()
            .skip(1)
            .flatten()
            .any(|matched| matched.as_str().len() > 9)
        {
            continue;
        }
        let value = |index: usize| {
            captures
                .get(index)
                .and_then(|matched| matched.as_str().parse::<u64>().ok())
                .unwrap_or(0)
        };
        let (total, failed, errors, skipped) = (value(1), value(2), value(3), value(4));
        if failed + errors + skipped > total {
            continue;
        }
        if failed > 0 || errors > 0 {
            return Some(false);
        }
        positive = positive || total > skipped;
    }
    for captures in gradle_summary_regex().captures_iter(text) {
        if captures
            .iter()
            .skip(1)
            .flatten()
            .any(|matched| matched.as_str().len() > 9)
        {
            continue;
        }
        let value = |index: usize| {
            captures
                .get(index)
                .and_then(|matched| matched.as_str().parse::<u64>().ok())
                .unwrap_or(0)
        };
        let (total, failed, skipped) = (value(1), value(2), value(3));
        if failed + skipped > total {
            continue;
        }
        if failed > 0 {
            return Some(false);
        }
        positive = positive || total > skipped;
    }
    positive.then_some(true)
}

/// Go 的包级通过行必须由 `--- PASS` 用例支撑（全 skip 的包同样打印 `ok`）。
fn contains_go_pass_shape(text: &str) -> bool {
    regex::Regex::new(r"(?m)^ok[ \t]+\S")
        .map(|regex| regex.is_match(text))
        .unwrap_or(false)
}

fn go_package_summary_is_evidence(text: &str) -> bool {
    let Ok(results) = regex::Regex::new(r"(?m)^[ \t]*--- (PASS|FAIL|SKIP):") else {
        return false;
    };
    let outcomes: Vec<&str> = results
        .captures_iter(text)
        .filter_map(|captures| captures.get(1).map(|matched| matched.as_str()))
        .collect();
    outcomes.is_empty() || outcomes.contains(&"PASS")
}

fn without_go_zero_test_lines(text: &str) -> String {
    regex::Regex::new(
        r"(?m)^(?:ok|\?)[ \t]+\S+[^\r\n]*\[(?:no test files|no tests to run)\][ \t]*\r?$",
    )
    .map(|regex| regex.replace_all(text, "").into_owned())
    .unwrap_or_else(|_| text.to_string())
}

/// unittest 的完整 summary：`Ran N tests` + `OK`，且执行数必须大于
/// skipped/expected failures（全 skip 的套件同样打印 OK）。
fn unittest_test_summary_holds(text: &str) -> bool {
    let Ok(regex) = regex::Regex::new(concat!(
        r"(?m)^Ran (0|[1-9][0-9]{0,8}) tests? in [0-9]+(?:\.[0-9]+)?s\r?\n\r?\n",
        r"OK(?: \(([^\r\n()]*)\))?[ \t]*\r?$"
    )) else {
        return false;
    };
    for captures in regex.captures_iter(text) {
        let total = captures
            .get(1)
            .and_then(|matched| matched.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        let Some(counts) = unittest_counter_sum(
            captures
                .get(2)
                .map(|matched| matched.as_str())
                .unwrap_or(""),
        ) else {
            continue;
        };
        if total > counts {
            return true;
        }
    }
    false
}

/// `OK (skipped=1, expected failures=0)` 的计数和；任何未知/重复/越界字段都无效。
fn unittest_counter_sum(counts: &str) -> Option<u64> {
    if counts.is_empty() {
        return Some(0);
    }
    let counter = regex::Regex::new(r"^(?:0|[1-9][0-9]{0,8})$").ok()?;
    let mut seen = std::collections::BTreeSet::new();
    let mut sum = 0u64;
    for field in counts.split(", ") {
        let (name, value) = field.split_once('=')?;
        if !matches!(name, "skipped" | "expected failures") || !seen.insert(name) {
            return None;
        }
        if !counter.is_match(value) {
            return None;
        }
        sum = sum.saturating_add(value.parse::<u64>().ok()?);
    }
    Some(sum)
}

/// 去掉被非空兄弟 target 证明为“空 target”的 Cargo summary 行。
///
/// 只有完整形态（counters 齐全且 finished in …s）的成功 summary 才参与豁免；
/// 裁剪过的输出不能抹掉 `0 passed` 证据。
fn without_empty_cargo_targets(output: &str) -> String {
    let pattern = concat!(
        r"(?im)^[ \t]*test result: ok\.[ \t]+([0-9]+) passed;[ \t]+0 failed;",
        r"[ \t]+[0-9]+ ignored;[ \t]+[0-9]+ measured;[ \t]+[0-9]+ filtered out;",
        r"[ \t]+finished in [0-9]+(?:\.[0-9]+)?s[ \t]*\r?$"
    );
    let Ok(regex) = regex::Regex::new(pattern) else {
        return output.to_string();
    };
    let mut has_nonzero = false;
    for captures in regex.captures_iter(output) {
        if captures.get(1).map(|matched| matched.as_str()) != Some("0") {
            has_nonzero = true;
            break;
        }
    }
    if !has_nonzero {
        return output.to_string();
    }
    regex
        .replace_all(output, |captures: &regex::Captures<'_>| {
            if captures.get(1).map(|matched| matched.as_str()) == Some("0") {
                String::new()
            } else {
                captures
                    .get(0)
                    .map(|matched| matched.as_str().to_string())
                    .unwrap_or_default()
            }
        })
        .into_owned()
}

fn contains_fail_shape(output: &str) -> bool {
    static PATTERNS: &[&str] = &[
        r"(?i)\b[1-9]\d*[ \t]+failed\b",
        r"(?i)\b[1-9]\d*[ \t]+errors?\b",
        r"(?m)^FAILED\b",
        r"(?m)^ERROR\s+\S",
        r"test result: FAILED",
        r"(?m)^FAIL\s+\S",
        r"BUILD FAILURE",
    ];
    PATTERNS.iter().any(|pattern| {
        regex::Regex::new(pattern)
            .map(|regex| regex.is_match(output))
            .unwrap_or(false)
    })
}

/// pytest/jest 的计数通过形状；Go 的 `ok` 行单独由
/// [`contains_go_pass_shape`] 判断，TAP 测试点已在进入这里前剔除。
fn contains_pass_shape(output: &str) -> bool {
    static PATTERNS: &[&str] = &[r"(?i)\b[1-9]\d*\s+passed\b", r"(?i)\ball tests passed\b"];
    PATTERNS.iter().any(|pattern| {
        regex::Regex::new(pattern)
            .map(|regex| regex.is_match(output))
            .unwrap_or(false)
    })
}

fn contains_zero_test_shape(output: &str) -> bool {
    static PATTERNS: &[&str] = &[
        r"(?i)\b0\s+passed\b",
        r"(?i)\[no test files\]",
        r"(?i)\[no tests to run\]",
        r"(?i)\bRan 0 tests\b",
    ];
    PATTERNS.iter().any(|pattern| {
        regex::Regex::new(pattern)
            .map(|regex| regex.is_match(output))
            .unwrap_or(false)
    })
}

pub(crate) fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "bash"
            | "shell"
            | "run_command"
            | "exec_command"
            | "unified_exec"
            | "terminal"
            | "powershell"
            | "pwsh"
    )
}

/// 一次命令结构匹配的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandMatch {
    /// 文本命中且控制流可证明。
    Match,
    /// 文本命中，但执行/状态归属无法证明（用于把更早的通过降级为 UNVERIFIED）。
    Unprovable,
    /// 文本不命中。
    NoMatch,
}

/// 命令结构匹配：整条相等，或按 shell 操作符分段后**最后一段**等于 criterion，
/// 且控制流能证明该段真的带着记录的 exit status 执行过。
///
/// 不把 `echo "cargo test"` 里的子串当作匹配；`echo ok || cargo test` 里的
/// `cargo test` 也不会被当成执行证据（`||` 只有在记录为失败时才可能执行右侧）。
/// 文本命中但无法归因时返回 [`CommandMatch::Unprovable`]，由调用方降级为
/// UNVERIFIED（参照实现的 `_commands_match` 三态语义）。
fn commands_match(criterion: &str, executed: &str, executed_success: bool) -> CommandMatch {
    let criterion = criterion.trim();
    let executed = executed.trim();
    if criterion.is_empty() || executed.is_empty() {
        return CommandMatch::NoMatch;
    }
    let (criterion_segments, criterion_gaps) = split_shell_with_ops(criterion);
    // 复合 criterion 的跨段控制流语义无法安全对齐，只接受单段 criterion
    // （尾随 `;` 无害，其它操作符如后台 `&` 无法归因）。
    if criterion_segments.len() != 1
        || criterion_gaps
            .iter()
            .any(|operators| operators.iter().any(|operator| operator != ";"))
    {
        return CommandMatch::NoMatch;
    }
    // 用解析后的段取 token：`pytest tests;`/`pytest tests # note` 不能把 `;` 或
    // 注释当成参数（参照实现同样先做 shell 解析）。绝不能走原始字符串相等的
    // 快捷路径：`$VAR` 展开、尾部 `&`、重定向都要经过 segment/归属检查。
    let expected = shell_tokens(&criterion_segments[0]);
    if expected.is_empty() {
        return CommandMatch::NoMatch;
    }
    let (segments, gaps) = split_shell_with_ops(executed);
    let Some(last) = segments.len().checked_sub(1) else {
        return CommandMatch::NoMatch;
    };
    let mut saw_unprovable = false;
    for (index, segment) in segments.iter().enumerate() {
        match segment_matches(&expected, &shell_tokens(segment)) {
            CommandMatch::NoMatch => continue,
            CommandMatch::Unprovable => {
                // 文本上相关但参数/选项改变了选择：即使别的窗口命中，整体也只能是
                // UNVERIFIED（参照实现的 `saw_unprovable`）。
                saw_unprovable = true;
                continue;
            }
            CommandMatch::Match => {}
        }
        // 整条命令的 exit status 属于最后执行的段；匹配段不在最后就无法归因。
        if index != last {
            saw_unprovable = true;
            continue;
        }
        // 首尾除 `;` 以外的操作符（后台 `&`、悬空 `&&`/`||`）都会让状态归属不明确。
        if gaps
            .first()
            .is_some_and(|operators| operators.iter().any(|operator| operator != ";"))
            || gaps
                .get(index + 1)
                .is_some_and(|operators| operators.iter().any(|operator| operator != ";"))
        {
            saw_unprovable = true;
            continue;
        }
        if !gaps.iter().take(index + 1).skip(1).all(|operators| {
            operators
                .iter()
                .all(|operator| operator_provably_executes(operator, executed_success))
        }) {
            saw_unprovable = true;
            continue;
        }
        // 前面的段改写了匹配段运行时的 shell 状态（PATH/CI/...），状态不可见。
        if segments[..index]
            .iter()
            .any(|segment| segment_pollutes_state(segment))
        {
            saw_unprovable = true;
            continue;
        }
        return CommandMatch::Match;
    }
    if saw_unprovable {
        CommandMatch::Unprovable
    } else {
        CommandMatch::NoMatch
    }
}

/// 运行时展开（`$VAR`/`$( )`/反引号）：展开后的参数不可见，命中也不能证明。
const EXPANSION_CHARS: &[char] = &['$', '`'];
/// glob 元字符：额外参数里的 glob 可能展开成隐藏的选项/目标。
const GLOB_CHARS: &[char] = &['*', '?', '['];
/// 取值的选项（独立形式）：其后的 token 是选项值，不是 positional 目标。
const VALUE_TAKING_OPTIONS: &[&str] = &[
    "-k",
    "-m",
    "-c",
    "-p",
    "-n",
    "-r",
    "--maxfail",
    "--junitxml",
    "--basetemp",
    "--cov",
    "--cov-report",
    "--durations-min",
    "--capture",
    "--tb",
    "--color",
    "--dist",
    "--ignore",
    "--ignore-glob",
    "--deselect",
    "--exclude",
    "--exclude-glob",
    "--skip",
    "--skip-file",
];
/// 排除语义的选项：其值命名的是“没有运行”的目标，不能作为匹配证据。
const NEGATING_OPTIONS: &[&str] = &[
    "--ignore",
    "--ignore-glob",
    "--deselect",
    "--exclude",
    "--exclude-glob",
    "--skip",
    "--skip-file",
];

fn has_expansion(token: &str) -> bool {
    token.contains(EXPANSION_CHARS)
}

fn has_glob(token: &str) -> bool {
    token.contains(GLOB_CHARS)
}

fn has_parent_component(value: &str) -> bool {
    value.split(['/', '\\']).any(|component| component == "..")
}

fn is_pathlike(token: &str) -> bool {
    if token.contains(['/', '\\']) {
        return true;
    }
    regex::Regex::new(r"\.(?:py|jsx?|tsx?|go|rs|java|rb|php)$")
        .is_ok_and(|regex| regex.is_match(token))
}

fn effective_env_assignments(tokens: &[String]) -> std::collections::BTreeMap<&str, &str> {
    let (assignments, _) = strip_env_assignments(tokens);
    let mut effective = std::collections::BTreeMap::new();
    for token in assignments {
        if let Some((name, value)) = token.split_once('=') {
            // 重复赋值是 last-wins：`CI=0 CI=1` 与 `CI=1 CI=0` 的 token 集合相同但
            // 运行环境相反。
            effective.insert(name, value);
        }
    }
    effective
}

/// 逐字归一化可执行文件路径（只做词法处理；`..` 由调用方先行拒绝）。
fn normalize_executable(token: &str) -> String {
    let slash = token.replace('\\', "/");
    let absolute = slash.starts_with('/');
    let joined = slash
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// criterion 里的 positional 参数（可能命名测试选择）。
///
/// 未知取值选项后跟 path-like token 时返回 `None`（fail closed）：该 token 可能是
/// 选项值（`--rootdir /tmp/project`），不能当作 selection 证明。
fn criterion_positional_args(expected: &[String]) -> Option<Vec<&str>> {
    let tokens = &expected[1..];
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token.starts_with('-') {
            if VALUE_TAKING_OPTIONS.contains(&token) {
                index += 2;
                continue;
            }
            if !token.contains('=')
                && let Some(following) = tokens.get(index + 1)
                && !following.starts_with('-')
                && is_pathlike(following)
            {
                return None;
            }
            index += 1;
            continue;
        }
        positionals.push(token);
        index += 1;
    }
    Some(positionals)
}

fn criterion_scopes_selection(expected: &[String]) -> bool {
    criterion_positional_args(expected)
        .is_some_and(|positionals| positionals.iter().any(|token| is_pathlike(token)))
}

fn negated_positions(tokens: &[String]) -> (Vec<usize>, Vec<usize>) {
    let mut options = Vec::new();
    let mut values = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if NEGATING_OPTIONS.contains(&token.as_str()) {
            options.push(index);
            if index + 1 < tokens.len() {
                values.push(index + 1);
            }
        } else if NEGATING_OPTIONS
            .iter()
            .any(|option| token.starts_with(&format!("{option}=")))
        {
            options.push(index);
            values.push(index);
        }
    }
    (options, values)
}

fn negated_value(token: &str) -> String {
    for option in NEGATING_OPTIONS {
        if let Some(value) = token.strip_prefix(&format!("{option}=")) {
            return value.to_string();
        }
    }
    token.to_string()
}

/// 额外 flag 是否可证明不改变“哪些测试会运行”（verbosity/输出/并行/覆盖率等）。
fn safe_extra_flag(token: &str) -> bool {
    let pattern = concat!(
        r"^(-[vqxsl]+|-r\S*|-n\d*",
        r"|--verbose|--quiet|--capture=\S+|--tb=\S+|--color=\S+",
        r"|--durations(=\S+)?|--durations-min=\S+|--disable-warnings",
        r"|--junitxml=\S+|--basetemp=\S+|--dist=\S+",
        r"|--cov(=\S*)?|--cov-report=\S+",
        r"|--strict-markers|--strict-config|--exitfirst|--showlocals|--maxfail=\d+",
        r"|--no-header|--no-summary)$"
    );
    regex::Regex::new(pattern).is_ok_and(|regex| regex.is_match(token))
}

fn selection_path_parts(value: &str) -> (String, Option<String>) {
    match value.split_once("::") {
        Some((path, nodeid)) => (path.to_string(), Some(nodeid.to_string())),
        None => (value.to_string(), None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionPathKind {
    Relative,
    Absolute,
    /// 驱动器/UNC 等 Windows 拼写：没有文件系统出处时一律 fail closed。
    Windows,
}

fn selection_path_kind(value: &str) -> SelectionPathKind {
    let (path, _) = selection_path_parts(value);
    let slash = path.replace('\\', "/");
    let bytes = slash.as_bytes();
    if slash.starts_with("//") {
        return SelectionPathKind::Windows;
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return SelectionPathKind::Windows;
    }
    if slash.starts_with('/') {
        SelectionPathKind::Absolute
    } else {
        SelectionPathKind::Relative
    }
}

fn normalize_selection_path(value: &str) -> Option<String> {
    let (path, _) = selection_path_parts(value);
    let slash = path.replace('\\', "/");
    let absolute = slash.starts_with('/');
    let mut components = Vec::new();
    for component in slash.split('/') {
        match component {
            "" | "." => continue,
            ".." => return None,
            other => components.push(other),
        }
    }
    let joined = components.join("/");
    if absolute {
        Some(format!("/{joined}"))
    } else if joined.is_empty() {
        Some(".".to_string())
    } else {
        Some(joined)
    }
}

fn nodeids_overlap(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => {
            a == b || a.starts_with(&format!("{b}::")) || b.starts_with(&format!("{a}::"))
        }
    }
}

/// 被排除的值是否与 criterion 命中目标重叠（相等、父子路径或 nodeid 前缀）。
///
/// 重叠意味着 criterion 的一部分选择根本没有运行；无关的排除（`--ignore
/// tests/slow` 对 `pytest tests/unit`）不影响匹配。
fn negation_overlaps(criterion_token: &str, negated: &str) -> bool {
    let a_kind = selection_path_kind(criterion_token);
    let b_kind = selection_path_kind(negated);
    if a_kind == SelectionPathKind::Windows
        || b_kind == SelectionPathKind::Windows
        || a_kind != b_kind
    {
        // 不同路径家族（相对 vs 绝对、POSIX vs Windows）在缺少 cwd/文件系统出处时
        // 可能别名，fail closed。
        return true;
    }
    let (a_path, a_nodeid) = selection_path_parts(criterion_token);
    let (b_path, b_nodeid) = selection_path_parts(negated);
    let Some(a) = normalize_selection_path(&a_path) else {
        return true;
    };
    let Some(b) = normalize_selection_path(&b_path) else {
        return true;
    };
    if a.is_empty() || b.is_empty() || a == "." || b == "." {
        return false;
    }
    if a == b {
        return nodeids_overlap(a_nodeid.as_deref(), b_nodeid.as_deref());
    }
    let a_prefix = format!("{}/", a.trim_end_matches('/'));
    let b_prefix = format!("{}/", b.trim_end_matches('/'));
    a.starts_with(&b_prefix) || b.starts_with(&a_prefix)
}

/// 单段命令匹配（参照实现的 `_segment_matches`，POSIX 语义）。
///
/// 可执行文件按“criterion 是裸名时接受任意路径拼写、显式路径要求同路径”比较；
/// criterion 参数必须按顺序出现在执行参数里；额外参数只有在可证明不改变选择时
/// 才算证据（`-q`/`--cov` 等安全 flag，或 criterion 本身用 path-like positional
/// 限定过范围时的额外目标）；`--ignore`/`--deselect` 等排除选项与命中目标重叠时
/// 判 UNVERIFIED。
fn segment_matches(expected: &[String], actual: &[String]) -> CommandMatch {
    if effective_env_assignments(expected) != effective_env_assignments(actual) {
        return CommandMatch::Unprovable;
    }
    let expected = strip_env_assignments(expected).1;
    let actual = strip_env_assignments(actual).1;
    if expected.is_empty() || actual.is_empty() {
        return CommandMatch::NoMatch;
    }
    if expected
        .iter()
        .chain(actual.iter())
        .any(|token| has_expansion(token))
    {
        return CommandMatch::Unprovable;
    }
    if expected[0] != actual[0] {
        if has_parent_component(&expected[0]) || has_parent_component(&actual[0]) {
            return CommandMatch::Unprovable;
        }
        if expected[0].contains(['/', '\\']) {
            // 显式路径的 criterion 只认同一归一化路径的执行（裸名会走 PATH）。
            if !actual[0].contains(['/', '\\'])
                || normalize_executable(&expected[0]) != normalize_executable(&actual[0])
            {
                return CommandMatch::NoMatch;
            }
        } else if executable_basename(&expected[0]) != executable_basename(&actual[0]) {
            return CommandMatch::NoMatch;
        }
    }
    let (option_positions, negated) = negated_positions(actual);
    let mut consumed = vec![0usize];
    let mut index = 1;
    for token in &expected[1..] {
        let mut found = false;
        while index < actual.len() {
            if &actual[index] == token && !negated.contains(&index) {
                consumed.push(index);
                index += 1;
                found = true;
                break;
            }
            index += 1;
        }
        if !found {
            return CommandMatch::NoMatch;
        }
    }
    if !negated.is_empty() && !criterion_scopes_selection(expected) {
        // 裸 criterion 代表 runner 的默认选择；任何排除选项都会缩小它，而 criterion
        // 没有 positional 目标可供下面的重叠检查捕获。
        return CommandMatch::Unprovable;
    }
    let negated_values: Vec<String> = negated
        .iter()
        .map(|position| negated_value(&actual[*position]))
        .collect();
    for value in &negated_values {
        if has_expansion(value) || has_glob(value) || has_parent_component(value) {
            return CommandMatch::Unprovable;
        }
    }
    if !negated_values.is_empty() {
        for position in &consumed {
            if *position != 0 && has_parent_component(&actual[*position]) {
                return CommandMatch::Unprovable;
            }
        }
        for position in &consumed {
            if *position == 0 {
                continue;
            }
            if negated_values
                .iter()
                .any(|value| negation_overlaps(&actual[*position], value))
            {
                return CommandMatch::Unprovable;
            }
        }
    }
    for (position, token) in actual.iter().enumerate() {
        if consumed.contains(&position)
            || negated.contains(&position)
            || option_positions.contains(&position)
        {
            continue;
        }
        if has_expansion(token) || has_glob(token) {
            return CommandMatch::Unprovable;
        }
        if token.starts_with('-') {
            if !safe_extra_flag(token) {
                return CommandMatch::Unprovable;
            }
        } else if !criterion_scopes_selection(expected) {
            // 裸 criterion 下的额外 positional 会缩窄 runner 的默认选择
            // （`python -m unittest pkg.OneTest`）。
            return CommandMatch::Unprovable;
        }
    }
    CommandMatch::Match
}

/// 记录的输出尾巴是否能归因给匹配段（参照实现的 `_output_attribution`）。
///
/// 匹配段总是命令的最后一段；以下两种情况让尾巴里的测试摘要不再可信：
/// 末段重定向了输出；或者前面存在无法证明静默的段（非单参数、范围内的 `cd`
/// 都算不静默），它可能打印出摘要形状。
fn output_attribution(executed: &str, workdir: &Path) -> Option<String> {
    let (segments, _gaps) = split_shell_with_ops(executed);
    let (last, preceding) = segments.split_last()?;
    if shell_tokens(last)
        .iter()
        .any(|token| token.contains(['<', '>']))
    {
        return Some(
            "matched segment redirects its output; the recorded tail is not test evidence"
                .to_string(),
        );
    }
    if !preceding
        .iter()
        .all(|segment| is_silent_segment(segment, workdir))
    {
        return Some("recorded output is not attributable to the matched segment".to_string());
    }
    None
}

/// 把单个段切成 POSIX 词（去引号、处理反斜杠转义）。
///
/// 只用于状态污染/静默判断；匹配本身仍比较段原文，避免语义判断改变既有匹配。
fn shell_tokens(segment: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    for character in segment.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else if character == '\\' && active == '"' {
                escaped = true;
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                started = true;
            }
            '\\' => {
                escaped = true;
                started = true;
            }
            character if character.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `NAME=value` 形式的 shell 变量赋值。
fn is_env_assignment(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// 去掉开头的 `NAME=value` 赋值，返回 (赋值前缀, 其余词)。
fn strip_env_assignments(tokens: &[String]) -> (&[String], &[String]) {
    let index = tokens
        .iter()
        .take_while(|token| is_env_assignment(token))
        .count();
    tokens.split_at(index)
}

fn executable_basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

/// 前缀段是否改写了匹配段运行时能看到的 shell 状态。
///
/// 任何 `NAME=value` 赋值前缀，或带参数的 `export`/`unset`，都让后面的命令
/// 在不可见的环境里运行（参照实现的 `_segment_pollutes_state`）。
fn segment_pollutes_state(segment: &str) -> bool {
    let tokens = shell_tokens(segment);
    let (assignments, rest) = strip_env_assignments(&tokens);
    if !assignments.is_empty() {
        return true;
    }
    match rest.first() {
        Some(first) if matches!(executable_basename(first), "export" | "unset") => rest.len() > 1,
        _ => false,
    }
}

/// 与参照实现的 `_cd_target_in_scope`：目标要么是不含 `..` 的相对路径，要么是
/// workdir 之内的绝对路径。`~`、`-` 与运行时展开都不算可证明。
fn cd_target_in_scope(target: &str, workdir: &Path) -> bool {
    if target.is_empty() || target == "-" || target.starts_with('~') {
        return false;
    }
    let slash_path = target.replace('\\', "/");
    if slash_path.split('/').any(|part| part == "..") {
        return false;
    }
    let candidate = Path::new(&slash_path);
    if !candidate.is_absolute() {
        return true;
    }
    let Ok(candidate) = candidate.canonicalize() else {
        return false;
    };
    let Ok(root) = workdir.canonicalize() else {
        return false;
    };
    candidate == root || candidate.starts_with(&root)
}

/// 前缀段是否可证明不产生输出（参照实现的 `_is_silent_segment`）。
///
/// 只有单参数的 `cd`（目标无摘要形状、无展开、在范围内）可能静默；其它可执行
/// 文件都可能打印任意文本，包括伪造的测试摘要。
fn is_silent_segment(segment: &str, workdir: &Path) -> bool {
    let tokens = shell_tokens(segment);
    let (_, rest) = strip_env_assignments(&tokens);
    let Some(executable) = rest.first() else {
        return true;
    };
    if executable_basename(executable) != "cd" {
        return false;
    }
    let args = &rest[1..];
    if args.len() != 1 {
        return false;
    }
    let target = &args[0];
    if contains_fail_shape(target) || contains_pass_shape(target) {
        return false;
    }
    if target.contains(['$', '`', '*', '?', '[']) {
        return false;
    }
    cd_target_in_scope(target, workdir)
}

/// `;`/`|` 之前/之间无条件执行；`&&` 依赖成功，`||` 依赖失败。
fn operator_provably_executes(operator: &str, executed_success: bool) -> bool {
    match operator {
        ";" | "|" => true,
        "&&" => executed_success,
        "||" => !executed_success,
        _ => false,
    }
}

/// 把命令切成段，并保留首/段间/尾的操作符：`gaps.len() == segments.len() + 1`。
///
/// 注释与转义按 POSIX shell 处理：未加引号的 `#`（位于词首）到行尾是注释，
/// 注释里的 `;`/`&&` 不执行任何东西；反斜杠转义下一个字符（单引号内除外），
/// 所以 `\;` 是普通参数而不是分隔符。两者都不能被当作执行证据。
fn split_shell_with_ops(command: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut segments: Vec<String> = Vec::new();
    let mut gaps: Vec<Vec<String>> = vec![Vec::new()];
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_comment = false;
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        if in_comment {
            if character == '\n' {
                in_comment = false;
                flush_shell_segment(&mut segments, &mut gaps, &mut current);
                gaps.last_mut().expect("gap exists").push(";".to_string());
            }
            continue;
        }
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if let Some(active) = quote {
            current.push(character);
            if active == '\'' {
                if character == '\'' {
                    quote = None;
                }
            } else if character == '"' {
                quote = None;
            } else if character == '\\' {
                escaped = true;
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                current.push(character);
            }
            '\\' => {
                current.push(character);
                escaped = true;
            }
            '#' if current.chars().last().is_none_or(char::is_whitespace) => {
                in_comment = true;
            }
            ';' | '\n' => {
                flush_shell_segment(&mut segments, &mut gaps, &mut current);
                gaps.last_mut().expect("gap exists").push(";".to_string());
            }
            '&' | '|' => {
                let mut operator = character.to_string();
                if chars.peek() == Some(&character) {
                    chars.next();
                    operator.push(character);
                }
                flush_shell_segment(&mut segments, &mut gaps, &mut current);
                gaps.last_mut().expect("gap exists").push(operator);
            }
            _ => current.push(character),
        }
    }
    flush_shell_segment(&mut segments, &mut gaps, &mut current);
    (segments, gaps)
}

fn flush_shell_segment(
    segments: &mut Vec<String>,
    gaps: &mut Vec<Vec<String>>,
    current: &mut String,
) {
    let segment = current.trim().to_string();
    current.clear();
    if segment.is_empty() {
        return;
    }
    segments.push(segment);
    gaps.push(Vec::new());
}

fn resolve_scoped_path(workdir: &Path, path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workdir.join(candidate)
    };
    // 词法归一化，拒绝 `..` 穿越。
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::RootDir => normalized.push(Path::new("/")),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) => return None,
        }
    }
    let workdir_normalized = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let parent = normalized.parent()?;
    let parent_canonical = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(_) => {
            // 不存在的父目录：用词法结果继续（后续 stat 会给出 NotFound）。
            return normalized
                .starts_with(&workdir_normalized)
                .then_some(normalized);
        }
    };
    if !parent_canonical.starts_with(&workdir_normalized) {
        return None;
    }
    Some(parent_canonical.join(normalized.file_name().unwrap_or_default()))
}

fn parse_columns_spec(spec: &str) -> Option<Vec<String>> {
    let value: JsonValue = serde_json::from_str(spec).ok()?;
    let array = value.as_array()?;
    let columns: Vec<String> = array
        .iter()
        .filter_map(JsonValue::as_str)
        .map(str::to_string)
        .collect();
    if columns.len() != array.len() || columns.is_empty() {
        return None;
    }
    let unique: std::collections::BTreeSet<&String> = columns.iter().collect();
    (columns.iter().all(|column| !column.is_empty()) && unique.len() == columns.len())
        .then_some(columns)
}

/// 解析第一条 CSV 记录（参照实现的 `_check_csv_header`）。
///
/// 返回 `(header 字段, 消费的字节数, malformed, incomplete)`：
/// - `header` 为 `None` 表示 header 含非法 UTF-8（`checked=true, holds=false`）；
/// - `malformed` 是已消费 header 里的 CSV 语法错误；
/// - `incomplete` 是截断（读取窗口在引号或记录中间结束）。
///
/// 只消费第一条记录，数据行里的任意字节都不参与判定。
fn parse_csv_header_record(bytes: &[u8], complete: bool) -> (Option<Vec<String>>, u64, bool, bool) {
    let start = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    let mut fields: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut index = start;
    let mut consumed = bytes.len();
    let mut malformed = false;
    let mut incomplete = false;
    let mut ended = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    current.push(b'"');
                    index += 2;
                } else {
                    in_quotes = false;
                    after_quote = true;
                    index += 1;
                }
            } else {
                current.push(byte);
                index += 1;
            }
            continue;
        }
        if after_quote {
            match byte {
                b',' => {
                    fields.push(std::mem::take(&mut current));
                    after_quote = false;
                    index += 1;
                }
                b'\r' => {
                    consumed = index + 1;
                    if bytes.get(index + 1) == Some(&b'\n') {
                        consumed += 1;
                    }
                    ended = true;
                    break;
                }
                b'\n' => {
                    consumed = index + 1;
                    ended = true;
                    break;
                }
                _ => {
                    // `"a"b`：关闭引号后只允许分隔符/行尾。
                    consumed = index;
                    malformed = true;
                    break;
                }
            }
            continue;
        }
        match byte {
            b'"' if current.is_empty() => {
                in_quotes = true;
                index += 1;
            }
            b',' => {
                fields.push(std::mem::take(&mut current));
                index += 1;
            }
            b'\r' => {
                consumed = index + 1;
                if bytes.get(index + 1) == Some(&b'\n') {
                    consumed += 1;
                }
                ended = true;
                break;
            }
            b'\n' => {
                consumed = index + 1;
                ended = true;
                break;
            }
            _ => {
                current.push(byte);
                index += 1;
            }
        }
    }
    if !ended && !malformed && in_quotes {
        if complete {
            malformed = true;
        } else {
            incomplete = true;
        }
    }
    if malformed || incomplete {
        return (None, consumed as u64, malformed, incomplete);
    }
    fields.push(current);
    // 空行：Python csv.reader 返回 []（reader 跳过空记录）。
    if fields.len() == 1 && fields[0].is_empty() {
        fields.clear();
    }
    let header_bytes = &bytes[start..consumed.min(bytes.len())];
    if std::str::from_utf8(header_bytes).is_err() {
        return (None, consumed as u64, false, false);
    }
    let columns = fields
        .iter()
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect::<Vec<_>>();
    (Some(columns), consumed as u64, false, false)
}

fn strip_prefix_ci<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        value.get(prefix.len()..)
    } else {
        None
    }
}

fn strip_suffix_word_ci<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let split = value.len().checked_sub(suffix.len())?;
    let tail = value.get(split..)?;
    if !tail.eq_ignore_ascii_case(suffix) {
        return None;
    }
    let prefix = value.get(..split)?;
    // 要求后缀前至少一个空白，避免把文件名 `fooexists` 误判成 `foo exists`。
    if prefix.chars().last().is_some_and(char::is_whitespace) {
        Some(prefix)
    } else {
        None
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bound_detail(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut bounded: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    bounded.push_str("...");
    bounded
}
