//! evals/src/cli.ts。
//!
//! 文档增益评测的入口：解析 CLI/env、构建两个 Docker 镜像、发现用例、规划任务、逐个跑
//! 容器、读取 Vitest JSON、汇总报告、写 `.eval/<runId>/` 下的产物。
//!
//! TS 里这段逻辑是模块顶层直接执行的（没有导出）；Rust 侧收进 [`run`] / [`run_with`]，
//! 由 `main.rs` 建 tokio 运行时后调用。
//!
//! 当前设计约束（改动需同步测试）：
//! - 参数解析的顺序决定报错文本，是当前稳定契约（`.docs.eval.ts` 检查最先，其次 `-t` /
//!   `--testNamePattern`，再次 `--testNamePattern=`，最后 runner 选项）；
//! - `Number(configuredRuns)` 用 ECMAScript `ToNumber` 语义（`0x`/`0o`/`0b`、`Infinity`、
//!   十进制指数；`Number.isSafeInteger` + `>= 1` 校验）；
//! - 文件列表按 JS 默认 `sort()`（UTF-16 码元序）排序；
//! - 产物 JSON 用 serde 结构体的声明顺序，数字用 JS 语义格式；
//! - 控制台输出顺序：每个任务一行、报告文本、`Artifacts:`；退出码仅由 blocked pair 决定。
//!
//! # 这个文件在系统中的位置
//!
//! 调用方：`src/main.rs` 建好 tokio 运行时后调用 [`run`]；集成测试
//! `crates/kiana-evals/tests/docker.rs` 用 [`run_with`] 注入临时包根、参数与环境闭包。
//! 本文件只做**编排**与产物落盘，具体工作委托给三个兄弟模块：[`crate::docker`] 负责镜像、
//! 容器与文件系统错误文本，[`crate::plan`] 负责解析发现结果与展开任务矩阵，
//! [`crate::report`] 负责读取 `vitest.json`、构造观测与汇总比较报告。
//!
//! # 一次运行的主要阶段（顺序即副作用的顺序）
//!
//! 1. `parse_eval_cli` 解析参数与环境，得到 provider/model 与重复次数；
//! 2. 选文件：显式请求优先，否则默认 glob；归一到包内相对路径、按 UTF-16 码元序排序，
//!    并逐个校验 `.docs.eval.ts` 后缀；
//! 3. 生成 `runId` 与 `.eval/<runId>` 路径，先校验 `auth.json` 里有该 provider 的凭据，
//!    再递归创建权限 0o700 的产物目录；
//! 4. 构建两个变体镜像；两个 image id 相同直接失败；
//! 5. 对两个变体各跑一次用例发现，用 V8 兼容的 JSON 解析读回结果，再比较两边身份一致；
//! 6. 以第一个变体的用例展开任务矩阵，计算协议摘要，写 `protocol.json` / `expected-runs.json`；
//! 7. 逐个任务跑容器；每跑完一个就全量重写 `observations.jsonl`；
//! 8. 汇总比较报告，写 `report.json` / `report.txt`，打印报告与产物目录，按 blocked pair
//!    决定退出码。
//!
//! # 关键约束与失败模式
//!
//! - 参数与文件校验都发生在任何 docker 调用、目录创建之前：这些错误的文本与出现时机被
//!   测试锁定（例如「没有任何 eval 文件」必须先于鉴权与 docker 失败）。
//! - `.eval/<runId>` 在镜像构建之前创建，且代码里没有任何清理逻辑：后续步骤失败会在磁盘上
//!   留下一个空的产物目录；同一包根多次运行会累积多个 `<runId>` 目录。
//! - 全流程串行：两个变体的发现、每个任务的容器运行都是顺序 `await`，没有并发任务。
//! - `protocolDigest` 是**紧凑**（无空格）序列化的 sha256，而写盘的 `protocol.json` 是
//!   pretty 文本；字段名、字段顺序以及 `files`/`cases`/`tasks` 的顺序都会改变摘要。
//! - 本文件写的五个产物文件（`protocol.json`、`expected-runs.json`、`observations.jsonl`、
//!   `report.json`、`report.txt`）都以恰好一个 `\n` 结尾，集成测试按字节比较。
//! - 任务失败（用例不通过、没有 `vitest.json`）只产生 `errored` 观测，不会让本模块返回
//!   `Err`；只有参数、文件 I/O、docker 启动或发现失败等基础设施问题才以 [`CliError`] 中止。

use std::path::Path;

use kiana_ai::auth::oauth::js_trim;
use kiana_ai::model_catalog::model_data::code_unit_compare;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::docker::{self, BuiltImages};
use crate::plan::{self, DOCUMENTATION_VARIANTS, DiscoveredEvalCase, EvalTask};
use crate::report::{self, EvalObservation, EvalRunIdentity};

/// 环境变量查询函数（`NodeJS.ProcessEnv` 的只读视图）。
///
/// 返回 `None` 表示变量未设置；返回 `Some("")` 表示设置为空串（JS 里是 falsy）。
///
/// [`run`] 把它绑定到进程环境，[`run_with`] 允许调用方注入（集成测试里就是一个查表的
/// 闭包）。闭包可能被查询多次（`KIANA_PROVIDER`、`KIANA_MODEL`、
/// `KIANA_EVAL_RUNS_PER_VARIANT`、`KIANA_CODING_AGENT_DIR`），必须可重复调用。
/// 本文件里 provider/model/重复次数都会再过一遍 trim：设置为纯空白等价于未设置。
pub type EnvironmentLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `RUNNER_OPTIONS = new Set(["--provider", "--model", "--runs-per-variant"])`。
const RUNNER_OPTIONS: [&str; 3] = ["--provider", "--model", "--runs-per-variant"];
/// `FILTER_OPTIONS = new Set(["-t", "--testNamePattern"])`。
const FILTER_OPTIONS: [&str; 2] = ["-t", "--testNamePattern"];
/// `--testNamePattern=` 前缀。
const TEST_NAME_PATTERN_PREFIX: &str = "--testNamePattern=";
/// 容器内 `packages/evals` 的绝对路径前缀。
const CONTAINER_PACKAGE_PREFIX: &str = "/repo/packages/evals/";

/// `parseEvalCli` 的结果。
///
/// TS: `cli.ts#EvalCliOptions`（内部类型）
///
/// 经过 `parse_eval_cli` 后满足：`provider` / `model` 要么同时为 `Some`、要么同时为 `None`，
/// 且都已 trim；`runs_per_variant` 是已校验的正安全整数（仍用 `f64` 保存，以对齐 JS 的
/// `number`）。`requested_files` 是**原始参数**，还没做包内路径归一；`discovery_args`
/// 保持用户给出的顺序，原样透传给 vitest。
#[derive(Clone, Debug, PartialEq)]
pub struct EvalCliOptions {
    /// 选中的 provider（trim 后非空）。
    pub provider: Option<String>,
    /// 选中的 model（trim 后非空）。
    pub model: Option<String>,
    /// 每个变体的重复次数（JS number，已校验为正安全整数）。
    pub runs_per_variant: f64,
    /// 显式请求的评测文件（原始参数）。
    pub requested_files: Vec<String>,
    /// 透传给 vitest 的过滤参数。
    pub discovery_args: Vec<String>,
}

/// evals CLI 的错误。`Display` 等于 TS 的 `error.message`。
///
/// `src/main.rs` 不区分具体变体：它把 `Display` 打到 stderr 并以 1 退出。因此各变体的文本
/// （包括 docker 侧复刻的 Node `SystemError` 前缀，如 `EEXIST`/`ENOTDIR`/`EACCES`）都是
/// 可观察接口，不能随手改写。
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// 与 TS `new Error(message)` 对应的文本。
    #[error("{0}")]
    Message(String),
    /// 文件系统操作失败。
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// `JSON.parse` 失败。
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    /// `parseDiscoveredCases` / `createTaskPlan` 失败。
    #[error("{0}")]
    Plan(#[from] plan::PlanError),
    /// docker 执行失败。
    #[error("{0}")]
    Docker(#[from] docker::DockerError),
    /// `readTaskObservation` 的持久化 I/O 失败。
    #[error("{0}")]
    Report(#[from] report::ReportError),
}

impl CliError {
    fn message(text: impl Into<String>) -> Self {
        CliError::Message(text.into())
    }
}

/// `String.prototype.trim() || undefined`：trim 后为空即视为未设置。
///
/// `js_trim` 用的是 ECMAScript 的空白集（含 BOM），因此 `" "`、`"\t"`、`"\u{feff}"` 都算空。
fn trimmed_option(value: Option<String>) -> Option<String> {
    value
        .map(|value| js_trim(&value).to_string())
        .filter(|value| !value.is_empty())
}

/// `parseEvalCli(args, environment)`。
///
/// TS: `cli.ts#parseEvalCli`（内部函数）
///
/// 逐参扫描，`index` 手工前进：每一步要么消费 1 个参数、要么消费 2 个，或者直接报错。
/// 五条规则的判定顺序是当前稳定契约；其中第 1 条（`.docs.eval.ts` 后缀）必须最先判定，否则
/// `--runs-per-variant=2.docs.eval.ts` 会被当成选项值。其余分支覆盖的参数集合互不重叠，
/// 但仍保持同样次序，以便错误文本稳定（属于用户可见契约）。除通过 `environment` 闭包读环境外
/// 不接触外部状态，也不访问文件系统。
///
/// 所有错误都是 [`CliError::Message`]，文本即 TS 的 `error.message`。
fn parse_eval_cli(
    args: &[String],
    environment: EnvironmentLookup<'_>,
) -> Result<EvalCliOptions, CliError> {
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut runs_text: Option<String> = None;
    let mut cli_selected_model = false;
    let mut requested_files: Vec<String> = Vec::new();
    let mut discovery_args: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];

        // 1. 以 `.docs.eval.ts` 结尾的参数永远视为请求文件（最先判定）。
        //    因为先于选项解析，`--runs-per-variant=2.docs.eval.ts` 这类参数也会被当成文件名。
        if argument.ends_with(".docs.eval.ts") {
            requested_files.push(argument.clone());
            index += 1;
            continue;
        }

        // 2. `-t` / `--testNamePattern`：取下一个参数，falsy（缺失或空串）报错。
        //    只有空串算 falsy；`" "` 是 JS 真值，会原样进入 discovery_args。
        if FILTER_OPTIONS.contains(&argument.as_str()) {
            let value = args.get(index + 1).filter(|value| !value.is_empty());
            let Some(value) = value else {
                return Err(CliError::message(format!("Missing value for {argument}.")));
            };
            discovery_args.push(argument.clone());
            discovery_args.push(value.clone());
            index += 2;
            continue;
        }

        // 3. `--testNamePattern=<value>`：空值报错，否则原样透传。
        //    只按前缀切分一次，值里可以继续包含 `=`。
        if let Some(value) = argument.strip_prefix(TEST_NAME_PATTERN_PREFIX) {
            if value.is_empty() {
                return Err(CliError::message(
                    "Missing value for --testNamePattern.".to_string(),
                ));
            }
            discovery_args.push(argument.clone());
            index += 1;
            continue;
        }

        // 4. runner 选项（`--name value` 或 `--name=value`）。
        //    `name` 只按第一个 `=` 切分；`=` 形式的值可以以 `-` 开头（`--provider=-x`），
        //    而分开形式的下一个参数若以 `-` 开头会被判为漏值（例如 `--provider --model m`
        //    在 `--provider` 处报错，而不是把 `--model` 当作 provider 的值）。
        let equals = argument.find('=');
        let name = match equals {
            Some(position) => &argument[..position],
            None => argument.as_str(),
        };
        if RUNNER_OPTIONS.contains(&name) {
            let value = match equals {
                Some(position) => Some(&argument[position + 1..]),
                None => args.get(index + 1).map(String::as_str),
            };
            let value = match value {
                Some(value) if !value.is_empty() => value,
                _ => return Err(CliError::message(format!("Missing value for {name}."))),
            };
            if equals.is_none() && value.starts_with('-') {
                return Err(CliError::message(format!("Missing value for {name}.")));
            }
            match name {
                "--provider" => provider = Some(value.to_string()),
                "--model" => model = Some(value.to_string()),
                _ => runs_text = Some(value.to_string()),
            }
            if name != "--runs-per-variant" {
                cli_selected_model = true;
            }
            index += if equals.is_none() { 2 } else { 1 };
            continue;
        }

        // 5. 其他参数一律不支持。
        return Err(CliError::message(format!(
            "Unsupported eval argument: {argument}"
        )));
    }

    // 后处理：先 trim，再决定 provider/model 的来源。`cli_selected_model` 只要出现过
    // `--provider` 或 `--model` 就为真：此时整体使用 CLI 值，缺一半直接报错，
    // **不会**回退到环境变量。
    let provider = trimmed_option(provider);
    let model = trimmed_option(model);
    let (provider, model) = if cli_selected_model {
        match (provider, model) {
            (Some(provider), Some(model)) => (Some(provider), Some(model)),
            _ => {
                return Err(CliError::message(
                    "CLI model selection requires both --provider and --model.".to_string(),
                ));
            }
        }
    } else {
        let env_provider = trimmed_option(environment("KIANA_PROVIDER"));
        let env_model = trimmed_option(environment("KIANA_MODEL"));
        if env_provider.is_some() != env_model.is_some() {
            return Err(CliError::message(
                "Set both KIANA_PROVIDER and KIANA_MODEL, or neither.".to_string(),
            ));
        }
        (env_provider, env_model)
    };

    // 重复次数：CLI 值优先于环境变量；即使 CLI 值 trim 后为空也仍然“占位”，
    // 此时回退到默认值 1，而不是环境变量里的数字。
    let configured_runs = match runs_text {
        Some(text) => Some(text),
        None => environment("KIANA_EVAL_RUNS_PER_VARIANT"),
    };
    let configured_runs = configured_runs.map(|text| js_trim(&text).to_string());
    let runs_per_variant = match configured_runs.as_deref().filter(|text| !text.is_empty()) {
        Some(text) => js_number(text),
        None => 1.0,
    };
    // 与 `Number.isSafeInteger` + `>= 1` 对齐：NaN、Infinity、小数、0、负数都会被拒绝。
    if !is_safe_integer(runs_per_variant) || runs_per_variant < 1.0 {
        return Err(CliError::message(
            "Runs per variant must be a positive integer.".to_string(),
        ));
    }

    Ok(EvalCliOptions {
        provider,
        model,
        runs_per_variant,
        requested_files,
        discovery_args,
    })
}

/// `Number.isSafeInteger(value)`。
///
/// `2^53 - 1` 是 JS 能精确表示的最大整数；超过它之后相邻 f64 的间隔大于 1，重复次数会失真。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// JS 的 `Number(string)`（ECMAScript `ToNumber` 的字符串分支）。
///
/// 与 `crates/kiana-json/src/js_num.rs#parse_js_number` 同语义（本文件自带实现，未直接调用
/// `kiana_json::parse_js_number`）：trim 集见 [`js_trim`]；`0x`/`0o`/`0b` 只接受无符号形式；
/// `Infinity` 精确拼写；十进制走 `StrDecimalLiteral`；其余为 `NaN`；`-0` 保留符号。
///
/// 边界：
/// - 进制前缀要求 `0` 后至少还有两个字节，因此裸 `"0x"` 会落到十进制分支并得到 `NaN`；
/// - 进制前缀在剥离正负号**之前**识别，所以 `"+0x10"` / `"-0x10"` 也是 `NaN`（与 JS 相同）；
/// - 十进制部分只做形状校验（[`is_decimal_literal`]），数值转换交给 Rust 的 `f64` 解析；
///   `-0` 由 `0.0 * -1.0` 保留符号位，溢出（如 `1e400`）得到无穷大。
fn js_number(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }

    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let base: u32 = match bytes[1] {
            b'x' | b'X' => 16,
            b'b' | b'B' => 2,
            b'o' | b'O' => 8,
            _ => 0,
        };
        if base != 0 {
            // 精确整数累加后一次性转为十进制字符串，再由 f64 解析器正确舍入（与 V8 一致）。
            let mut value = num_bigint::BigUint::from(0u8);
            let big_base = num_bigint::BigUint::from(base);
            for &byte in &bytes[2..] {
                let Some(digit) = (byte as char).to_digit(base) else {
                    return f64::NAN;
                };
                value = value * &big_base + num_bigint::BigUint::from(digit);
            }
            return value
                .to_str_radix(10)
                .parse::<f64>()
                .unwrap_or(f64::INFINITY);
        }
    }

    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }

    let (sign, magnitude) = if let Some(rest) = text.strip_prefix('+') {
        (1.0, rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1.0, rest)
    } else {
        (1.0, text)
    };
    if !is_decimal_literal(magnitude) {
        return f64::NAN;
    }
    magnitude
        .parse::<f64>()
        .map(|value| value * sign)
        .unwrap_or(f64::NAN)
}

/// `StrDecimalLiteral`（不含符号）：`Digits . Digits? ExponentPart? | . Digits ExponentPart?
/// | Digits ExponentPart?`。
///
/// 只检查形状、不做数值转换：`"5."`、`".5"`、`"5.e2"` 合法，`"1e"`、`"1_000"`、`""` 不合法。
/// 调用方已剥离符号；指数部分必须至少有一位数字。
fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut saw_integer = false;
    let mut saw_fraction = false;

    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        saw_integer = true;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            saw_fraction = true;
        }
    }
    if !saw_integer && !saw_fraction {
        return false;
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    index == bytes.len()
}

/// `` `${new Date().toISOString().replaceAll(":", "-")}_${randomUUID()}` ``。
///
/// 毫秒精度时间戳 + UUID v4，只用于目录命名；没有唯一性检查，也没有冲突处理。
fn artifact_run_id() -> String {
    let timestamp = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
        .replace(':', "-");
    format!("{timestamp}_{}", uuid::Uuid::new_v4())
}

/// `containerPath(path)`：把包内路径归一成相对 `packageRoot` 的容器路径。
///
/// TS: `cli.ts#containerPath`（内部函数）
///
/// `resolve`/`relative` 都是**词法**操作，不解析符号链接；结果里的 `\` 换成 `/`，
/// 以 `..` 开头（即包外）时报 `Eval file must be inside {packageRoot}: {path}`。
///
/// 返回的是相对 `package_root` 的路径（容器内看到的 `/repo/packages/evals/...` 前缀由
/// docker 侧拼装），因此结果可以为空串——当 `path` 恰好解析到包根本身时（例如 `"."`）。
/// 包外判断是对**文本**做 `starts_with("..")`，所以 `..foo/bar.ts` 这种名字也会被拒绝。
fn container_path(package_root: &Path, path: &str) -> Result<String, CliError> {
    let absolute = docker::js_resolve(package_root, path);
    let package_relative = docker::js_relative(package_root, &absolute);
    let relative_text = docker::path_text(&package_relative);
    if relative_text.starts_with("..") {
        return Err(CliError::message(format!(
            "Eval file must be inside {}: {path}",
            docker::path_text(package_root)
        )));
    }
    Ok(relative_text.replace('\\', "/"))
}

/// `normalizeDiscoveredFile(path)`：去掉容器内 `packages/evals` 前缀。
///
/// TS: `cli.ts#normalizeDiscoveredFile`（内部函数）
///
/// 前缀是字面量 `/repo/packages/evals/`（带结尾斜杠）；只做一次 `strip_prefix`，不解析
/// `..`、不做大小写处理。前缀本身匹配后得到空串，剩余部分为空也不额外报错。
fn normalize_discovered_file(path: &str) -> Result<String, CliError> {
    match path.strip_prefix(CONTAINER_PACKAGE_PREFIX) {
        Some(relative) => Ok(relative.to_string()),
        None => Err(CliError::message(format!(
            "Discovered eval path is outside the container package: {path}"
        ))),
    }
}

/// 两个变体的发现结果必须一致，比较 `JSON.stringify([fullName, normalizedFile])` 的排序数组。
///
/// TS: `cli.ts#compareDiscovery`（内部函数）
///
/// 两边的顺序可以不同（各自先排序），但（`fullName`, 归一文件）列表必须逐项相等；这里比较
/// 的是 `Vec` 而不是集合，因此条数也必须一致。
fn compare_discovery(
    left: &[DiscoveredEvalCase],
    right: &[DiscoveredEvalCase],
) -> Result<(), CliError> {
    let left = discovery_identities(left)?;
    let right = discovery_identities(right)?;
    if left != right {
        return Err(CliError::message(
            "Documentation variants discovered different eval cases.".to_string(),
        ));
    }
    Ok(())
}

/// `compareDiscovery` 里的 `identities(cases)`。
///
/// 每项比较的是 `[fullName, file]` 的 JSON 文本（对应 TS 的 `JSON.stringify`），因此字段
/// 边界不依赖任何分隔符；排序用 `code_unit_compare`（UTF-16 码元序，等同 JS 默认 `sort()`）。
/// 文件归一可能失败，因此返回 `Result`。
fn discovery_identities(cases: &[DiscoveredEvalCase]) -> Result<Vec<String>, CliError> {
    let mut identities = Vec::with_capacity(cases.len());
    for eval_case in cases {
        let file = normalize_discovered_file(&eval_case.file)?;
        identities.push(serde_json::to_string(&[
            eval_case.full_name.as_str(),
            file.as_str(),
        ])?);
    }
    identities.sort_by(|left, right| code_unit_compare(left, right));
    Ok(identities)
}

/// `globSync("evals/**/*.docs.eval.ts", { cwd: packageRoot })`（Node 默认 `follow: false`、
/// `dot: false`）。
///
/// Node 的 `fs.globSync` 语义：
/// - `**` 只递归**真实目录**，不进入符号链接目录（首层的字面量 `evals` 会被正常打开，
///   即使它本身是指向目录的符号链接）；
/// - 以 `.` 开头的条目既不匹配也不递归；
/// - 读不了的目录被静默跳过（`globSync` 不抛错）；
/// - 尾部 `*` 不做类型过滤：普通文件、目录、符号链接（含悬空链接）只要名字匹配都算。
///
/// 返回顺序取决于 `read_dir` 的目录顺序（随平台/文件系统而变），因此本函数**不保证**
/// 有序；[`run_with`] 会统一按码元序排序后再使用。`evals` 不存在或不可读时返回空列表。
fn list_default_eval_files(package_root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk_default_eval_files(&package_root.join("evals"), &mut files);
    files
}

/// [`list_default_eval_files`] 的递归部分。
fn walk_default_eval_files(directory: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        // Node 的 globSync 对不可读目录静默跳过。
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        // `dot: false`：`.` 开头的条目既不匹配也不递归。
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if name.ends_with(".docs.eval.ts") {
            files.push(docker::path_text(&path));
        }
        // `file_type` 不跟随符号链接（`**` 不进入链接目录）；类型读取失败时按非目录处理。
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            walk_default_eval_files(&path, files);
        }
    }
}

/// `fs.promises.readFile(path, "utf8")`：`open` 失败按 `open`、读取失败按 `read` 格式化。
///
/// 返回原始字节；调用方用 `String::from_utf8_lossy` 解码成文本，对应 TS 以 `"utf8"`
/// 读取得到的字符串。
async fn read_file_node(path: &Path) -> Result<Vec<u8>, CliError> {
    use tokio::io::AsyncReadExt as _;
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        CliError::message(docker::node_system_error("open", Some(path), &error))
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await.map_err(|error| {
        CliError::message(docker::node_system_error("read", Some(path), &error))
    })?;
    Ok(bytes)
}

/// `fs.promises.writeFile(path, text)`：`open` 失败带路径、`write` 失败不带路径（libuv 约定）。
///
/// `File::create` 等价于 Node `writeFile` 的默认 `flag: "w"`：不存在则创建、存在则截断。
async fn write_file_node(path: &Path, contents: &str) -> Result<(), CliError> {
    use tokio::io::AsyncWriteExt as _;
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        CliError::message(docker::node_system_error("open", Some(path), &error))
    })?;
    file.write_all(contents.as_bytes())
        .await
        .map_err(|error| CliError::message(docker::node_system_error("write", None, &error)))?;
    Ok(())
}

/// `protocol.json` 里的单个用例。
///
/// 只保留 `(evalSet, caseId, file)` 三个字段：TS 的 `cases.map(({evalSet, caseId, file}) => ...)`
/// 刻意丢掉了 `fullName`，所以它也**不在**协议里。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolCase {
    /// 评测集名。
    eval_set: String,
    /// 用例名。
    case_id: String,
    /// 相对 `packages/evals` 的文件路径。
    file: String,
}

/// 参与 `protocolDigest` 计算的协议对象（字段顺序即 `JSON.stringify` 的键序）。
///
/// TS: `cli.ts` 的 `protocol` 字面量
///
/// 字段顺序就是序列化后的键序，而摘要取的是紧凑序列化的 sha256，所以字段名与顺序属于协议
/// 的一部分，改动会让所有 run 的摘要变化。`runs_per_variant` 从 `f64` 强转为 `u64`：
/// `parse_eval_cli` 已保证它是 ≥ 1 的安全整数。`images` 的内部顺序固定为 `without_docs`、
/// `with_docs`（由 `BuiltImages` 的字段顺序决定）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvalProtocol {
    /// schema 版本（恒为 1）。
    schema_version: u32,
    /// `<provider>/<model>`。
    model: String,
    /// 每个变体的重复次数。
    runs_per_variant: u64,
    /// 两个变体的镜像。
    images: BuiltImages,
    /// 请求/发现的评测文件（相对包路径，已排序）。
    files: Vec<String>,
    /// 用例身份。
    cases: Vec<ProtocolCase>,
    /// 任务矩阵。
    tasks: Vec<EvalTask>,
}

/// 落盘的 `protocol.json`：协议对象 + 末尾的 `protocolDigest`。
///
/// `#[serde(flatten)]` 让协议字段先写、`protocolDigest` 最后写。注意写盘用的是 pretty 文本
/// （2 空格缩进 + 结尾换行），而摘要算的是 compact 文本，两者不是同一份字节。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolArtifact<'a> {
    /// 协议对象（扁平展开，键序排在最前）。
    #[serde(flatten)]
    protocol: &'a EvalProtocol,
    /// 紧凑协议文本的 sha256 十六进制。
    protocol_digest: String,
}

/// evals CLI 入口：执行原模块的顶层逻辑。
///
/// 返回进程退出码：`report.blockedPairs` 非空时为 1，否则 0（`process.exitCode` 语义）。
///
/// 参数来自 `std::env::args_os().skip(1)`、环境来自进程环境，二者都经 `to_string_lossy`
/// 转成字符串（非 UTF-8 字节被替换成 U+FFFD，而不是报错）。副作用：构建镜像、运行容器、
/// 在 `<package_root>/.eval/<runId>/` 下写产物，并按顺序把进度行、报告文本与
/// `Artifacts: <dir>` 打印到 stdout；`Err` 由 `main.rs` 打印到 stderr 并以 1 退出。
pub async fn run() -> Result<i32, CliError> {
    // TS 侧 `process.argv.slice(2)` 跳过 node 与脚本路径；Rust 二进制只有 argv[0]
    // （可执行文件本身），所以等价区间是 argv[1..]。
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    // 原生 task suite：`kiana-evals run --suite ...`。其余参数保持旧 docs A/B 行为。
    if args.first().map(String::as_str) == Some("run") {
        return crate::native::run_cli(&args[1..])
            .await
            .map_err(|error| CliError::message(error.to_string()));
    }
    let environment =
        |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
    run_with(args, &docker::package_root(), &environment).await
}

/// [`run`] 的可注入版本：参数、包根与环境由调用方提供（测试用）。
///
/// `package_root` 只影响 CLI 侧的文件发现、容器路径与产物目录；`docker::build_images`
/// 仍使用编译期的 `CARGO_MANIFEST_DIR`（与 TS 的模块级 `packageRoot` 一致），因此测试里
/// 假 docker 看到的 `--file`/构建上下文始终指向真实 crate 与工作区。
///
/// # 产物与失败语义
///
/// 产物目录下会写 `protocol.json`、`expected-runs.json`、`observations.jsonl`、
/// `report.json`、`report.txt`；`discovery/<variant>/` 与 `tasks/<sha256>/` 由
/// [`crate::docker`] 的调用创建。返回 `Ok(0)` / `Ok(1)` 表示整条流水线跑完（1 仅当存在
/// blocked pair），返回 `Err` 表示参数、文件 I/O、docker 启动或发现失败等基础设施错误；
/// 单个任务的用例失败不会变成 `Err`。
///
/// 本函数不清理已有的 `.eval` 目录，也不检查 `runId` 是否与旧目录重名。
#[doc(hidden)]
pub async fn run_with(
    args: Vec<String>,
    package_root: &Path,
    environment: EnvironmentLookup<'_>,
) -> Result<i32, CliError> {
    let cli = parse_eval_cli(&args, environment)?;

    // 阶段 1：provider/model 必须成对。`parse_eval_cli` 只保证「全用 CLI」或「全用 env」，
    // 允许两者整体缺失，因此这里还要再兜一次底。
    let (Some(provider), Some(model)) = (cli.provider.clone(), cli.model.clone()) else {
        return Err(CliError::message(
            "Set KIANA_PROVIDER and KIANA_MODEL, or pass --provider and --model.".to_string(),
        ));
    };
    let model_identity = format!("{provider}/{model}");

    // 阶段 2：确定文件列表。显式请求按用户给出的原始字符串处理，否则走默认发现；
    // 两种来源都要经 `container_path` 归一成包内相对路径。
    let mut files: Vec<String> = if cli.requested_files.is_empty() {
        list_default_eval_files(package_root)
            .iter()
            .map(|path| container_path(package_root, path))
            .collect::<Result<_, _>>()?
    } else {
        cli.requested_files
            .iter()
            .map(|path| container_path(package_root, path))
            .collect::<Result<_, _>>()?
    };
    // 与 TS 的 `files.sort()` 一致：UTF-16 码元序，不是字节序。
    files.sort_by(|left, right| code_unit_compare(left, right));
    if files.is_empty() {
        return Err(CliError::message(
            "No documentation eval files were selected.".to_string(),
        ));
    }
    for file in &files {
        if !file.ends_with(".docs.eval.ts") {
            return Err(CliError::message(format!(
                "Documentation runner cannot execute non-doc eval: {file}"
            )));
        }
    }

    // 阶段 3：产物目录与凭据。顺序与 TS 顶层执行一致：生成 runId → 校验 auth.json → 建目录。
    let run_id = artifact_run_id();
    let artifact_directory = docker::js_resolve(package_root, &format!(".eval/{run_id}"));
    let coding_agent_dir = environment("KIANA_CODING_AGENT_DIR");
    let auth_path = docker::require_eval_auth_file_with(&provider, coding_agent_dir.as_deref())?;
    // `await mkdir(artifactDirectory, { recursive: true, mode: 0o700 })`（错误文本按 Node 的
    // `SystemError` 格式化）。
    docker::create_dir_all_mode(&artifact_directory, 0o700)
        .map_err(|error| CliError::message(error.to_string()))?;

    // 阶段 4：构建两个变体镜像。构建失败会在磁盘上留下上面刚建好的空产物目录
    // （本模块没有清理逻辑）。
    let images = docker::build_images()?;
    // 两个变体的镜像 id 相同意味着对照组与实验组跑的是同一份运行时，直接失败。
    if images.with_docs.id == images.without_docs.id {
        return Err(CliError::message(
            "Documentation variants resolved to the same image.".to_string(),
        ));
    }
    let context = docker::create_docker_context(
        images,
        artifact_directory.clone(),
        auth_path,
        provider,
        model,
        cli.runs_per_variant,
    );

    // 阶段 5：两个变体各发现一次，顺序固定为 `DOCUMENTATION_VARIANTS`（`without_docs`、
    // `with_docs`）。两份结果随后由 `compare_discovery` 比较，不一致会直接失败。
    let mut discoveries: Vec<Vec<DiscoveredEvalCase>> = Vec::with_capacity(2);
    for variant in DOCUMENTATION_VARIANTS {
        let path = docker::discover_cases(&context, variant, &files, &cli.discovery_args)?;
        let bytes = read_file_node(&path).await?;
        let text = String::from_utf8_lossy(&bytes);
        // `JSON.parse(...)`：失败时 V8 的 `SyntaxError.message` 会作为模块顶层错误打印，
        // 因此用 kiana-json 的 V8 兼容解析器而不是 serde_json 的错误文本。
        let value = kiana_json::parse(&text).map_err(|error| CliError::message(error.message))?;
        discoveries.push(plan::parse_discovered_cases(&value)?);
    }
    // `DOCUMENTATION_VARIANTS` 恒为两项，`discoveries` 的长度因此也是 2。若以后改这个常量，
    // 下面的 `unreachable!` 会 panic，`Vec::with_capacity(2)` 也需要同步修改。
    let (first, second) = match discoveries.as_slice() {
        [first, second] => (first, second),
        _ => unreachable!("discoveries always has one entry per documentation variant"),
    };
    compare_discovery(first, second)?;

    // 协议/任务里的用例文件必须是包内相对路径；两个变体已确认一致，因此直接取第一个。
    let mut cases: Vec<DiscoveredEvalCase> = Vec::with_capacity(first.len());
    for eval_case in first {
        let mut normalized = eval_case.clone();
        normalized.file = normalize_discovered_file(&eval_case.file)?;
        cases.push(normalized);
    }
    if cases.is_empty() {
        return Err(CliError::message(
            "No documentation eval cases matched the selection.".to_string(),
        ));
    }
    // 阶段 6：把用例展开成「用例 × run × 变体」的任务矩阵。奇数轮与偶数轮的变体顺序相反
    // （见 `plan::create_task_plan`），以抵消顺序带来的偏差。
    let tasks = plan::create_task_plan(&cases, &model_identity, cli.runs_per_variant)?;

    // 协议对象只保留 (evalSet, caseId, file)，`fullName` 被刻意丢弃。
    let protocol = EvalProtocol {
        schema_version: 1,
        model: model_identity,
        runs_per_variant: cli.runs_per_variant as u64,
        images: context.images.clone(),
        files: files.clone(),
        cases: cases
            .iter()
            .map(|eval_case| ProtocolCase {
                eval_set: eval_case.eval_set.clone(),
                case_id: eval_case.case_id.clone(),
                file: eval_case.file.clone(),
            })
            .collect(),
        tasks: tasks.clone(),
    };
    // 先对 compact 文本取摘要，再把 digest 追加到 pretty 产物里；digest 的输入不包含
    // digest 自身，因此可以独立复算。
    let protocol_text = serde_json::to_string(&protocol)?;
    let protocol_digest = hex::encode(Sha256::digest(protocol_text.as_bytes()));
    let protocol_artifact = ProtocolArtifact {
        protocol: &protocol,
        protocol_digest: protocol_digest.clone(),
    };
    write_file_node(
        &artifact_directory.join("protocol.json"),
        &format!("{}\n", serde_json::to_string_pretty(&protocol_artifact)?),
    )
    .await?;
    write_file_node(
        &artifact_directory.join("expected-runs.json"),
        &format!("{}\n", serde_json::to_string_pretty(&tasks)?),
    )
    .await?;

    // 阶段 7：任务严格串行。`run_task` 只在 docker 无法启动等基础设施错误时返回 `Err`；
    // 容器退出码非零不算错误，是否产出 `vitest.json` 才决定观测是 errored 还是交给
    // report 层解析。
    let mut observations: Vec<EvalObservation> = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        println!(
            "\n[{}/{}] {} > {} | {} | {} | run {}",
            index + 1,
            tasks.len(),
            task.eval_set,
            task.case_id,
            task.variant.as_str(),
            task.model,
            task.run_number
        );
        let report_path = docker::run_task(&context, task)?;
        let observation = match report_path {
            Some(path) => report::read_task_observation(task, &path, &artifact_directory).await?,
            None => report::errored_observation(task),
        };
        observations.push(observation);
        let lines: Vec<String> = observations
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        // TS 每次都**全量重写** observations.jsonl（不是追加）。
        // 于是文件内容始终与内存里的 `observations` 一一对应；中途失败时已完成的观测也已落盘。
        write_file_node(
            &artifact_directory.join("observations.jsonl"),
            &format!("{}\n", lines.join("\n")),
        )
        .await?;
    }

    // 阶段 8：期望运行来自任务矩阵（与 tasks 同序），观测按任务顺序累积；汇总算法全部在
    // report 模块，本文件只负责写盘与打印。
    let expected_runs: Vec<EvalRunIdentity> = tasks.iter().map(EvalRunIdentity::from).collect();
    let report =
        report::summarize_eval_observations(&protocol_digest, &expected_runs, &observations);
    let report_text = report::format_eval_comparison_report(&report);
    write_file_node(
        &artifact_directory.join("report.json"),
        &format!("{}\n", serde_json::to_string_pretty(&report)?),
    )
    .await?;
    write_file_node(
        &artifact_directory.join("report.txt"),
        &format!("{report_text}\n"),
    )
    .await?;

    println!("\n{report_text}");
    println!("\nArtifacts: {}", docker::path_text(&artifact_directory));
    // 退出码只看是否存在 blocked pair；lift/flags 等比较结果不影响退出码。
    Ok(if report.blocked_pairs.is_empty() {
        0
    } else {
        1
    })
}

#[cfg(test)]
mod tests {
    // 本模块测的是本文件的纯函数与本地文件系统函数（参数解析、JS Number、路径归一、
    // 默认发现、Node 错误文本）。需要假 docker 的端到端流程在
    // `crates/kiana-evals/tests/docker.rs`。
    use super::*;

    fn environment_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn parse(values: &[&str], pairs: &[(&str, &str)]) -> Result<EvalCliOptions, CliError> {
        parse_eval_cli(&args(values), &environment_of(pairs))
    }

    fn parse_error(values: &[&str], pairs: &[(&str, &str)]) -> String {
        parse(values, pairs).unwrap_err().to_string()
    }

    /// TS: `parseEvalCli` 的第 1 条规则（`.docs.eval.ts` 最先判定）。
    #[test]
    fn docs_eval_suffix_wins_over_every_other_rule() {
        let options = parse(
            &["--runs-per-variant=2.docs.eval.ts", "evals/a.docs.eval.ts"],
            &[("KIANA_PROVIDER", "p"), ("KIANA_MODEL", "m")],
        )
        .unwrap();
        assert_eq!(
            options.requested_files,
            vec![
                "--runs-per-variant=2.docs.eval.ts".to_string(),
                "evals/a.docs.eval.ts".to_string()
            ]
        );
        assert!(options.discovery_args.is_empty());
        assert_eq!(options.runs_per_variant, 1.0);
    }

    /// TS: `parseEvalCli` 的第 2 条规则（filter 选项）。
    #[test]
    fn filter_options_require_a_truthy_value() {
        assert_eq!(parse_error(&["-t"], &[]), "Missing value for -t.");
        assert_eq!(parse_error(&["-t", ""], &[]), "Missing value for -t.");
        assert_eq!(
            parse_error(&["--testNamePattern"], &[]),
            "Missing value for --testNamePattern."
        );
        let options = parse(&["-t", "case one", "--testNamePattern", "x"], &[]).unwrap();
        assert_eq!(
            options.discovery_args,
            vec!["-t", "case one", "--testNamePattern", "x"]
        );
    }

    /// TS: `parseEvalCli` 的第 3 条规则（`--testNamePattern=`）。
    #[test]
    fn test_name_pattern_prefix_requires_a_value() {
        assert_eq!(
            parse_error(&["--testNamePattern="], &[]),
            "Missing value for --testNamePattern."
        );
        let options = parse(&["--testNamePattern=a=b"], &[]).unwrap();
        assert_eq!(options.discovery_args, vec!["--testNamePattern=a=b"]);
    }

    /// TS: `parseEvalCli` 的第 4 条规则（runner 选项）。
    #[test]
    fn runner_options_accept_separate_and_equals_forms() {
        let options = parse(
            &[
                "--provider",
                "fixture",
                "--model=model",
                "--runs-per-variant",
                "3",
            ],
            &[],
        )
        .unwrap();
        assert_eq!(options.provider.as_deref(), Some("fixture"));
        assert_eq!(options.model.as_deref(), Some("model"));
        assert_eq!(options.runs_per_variant, 3.0);

        let options = parse(&["--provider=-dash", "--model=m"], &[]).unwrap();
        assert_eq!(options.provider.as_deref(), Some("-dash"));

        assert_eq!(
            parse_error(&["--provider", "-dash", "--model", "m"], &[]),
            "Missing value for --provider."
        );
        assert_eq!(
            parse_error(&["--provider"], &[]),
            "Missing value for --provider."
        );
        assert_eq!(
            parse_error(&["--provider="], &[]),
            "Missing value for --provider."
        );
        assert_eq!(
            parse_error(&["--runs-per-variant"], &[]),
            "Missing value for --runs-per-variant."
        );
    }

    /// TS: `parseEvalCli` 的第 5 条规则（未知参数）。
    #[test]
    fn unknown_arguments_are_rejected_verbatim() {
        assert_eq!(
            parse_error(&["--verbose"], &[]),
            "Unsupported eval argument: --verbose"
        );
        assert_eq!(
            parse_error(&["stray"], &[]),
            "Unsupported eval argument: stray"
        );
    }

    /// TS: `parseEvalCli` 的后处理（CLI 选择必须成对）。
    #[test]
    fn cli_model_selection_requires_both_options() {
        assert_eq!(
            parse_error(&["--provider", "p"], &[("KIANA_MODEL", "m")]),
            "CLI model selection requires both --provider and --model."
        );
        assert_eq!(
            parse_error(&["--model", "m"], &[("KIANA_PROVIDER", "p")]),
            "CLI model selection requires both --provider and --model."
        );
        // `--provider "   "` trim 后为空 → 视为未提供。
        assert_eq!(
            parse_error(&["--provider", "   ", "--model", "m"], &[]),
            "CLI model selection requires both --provider and --model."
        );
    }

    /// TS: `parseEvalCli` 的后处理（env 选择必须成对）。
    #[test]
    fn environment_model_selection_requires_both_variables() {
        assert_eq!(
            parse_error(&[], &[("KIANA_PROVIDER", "p")]),
            "Set both KIANA_PROVIDER and KIANA_MODEL, or neither."
        );
        assert_eq!(
            parse_error(&[], &[("KIANA_MODEL", "m")]),
            "Set both KIANA_PROVIDER and KIANA_MODEL, or neither."
        );
        let options = parse(&[], &[("KIANA_PROVIDER", " p "), ("KIANA_MODEL", "m")]).unwrap();
        assert_eq!(options.provider.as_deref(), Some("p"));
        assert_eq!(options.model.as_deref(), Some("m"));
        // 两边都是空白 → 都视为未设置。
        let options = parse(&[], &[("KIANA_PROVIDER", " "), ("KIANA_MODEL", "\t")]).unwrap();
        assert_eq!(options.provider, None);
        assert_eq!(options.model, None);
        // CLI 选项优先于 env。
        let options = parse(
            &["--provider", "cli-p", "--model", "cli-m"],
            &[("KIANA_PROVIDER", "env-p"), ("KIANA_MODEL", "env-m")],
        )
        .unwrap();
        assert_eq!(options.provider.as_deref(), Some("cli-p"));
        assert_eq!(options.model.as_deref(), Some("cli-m"));
    }

    /// TS: `parseEvalCli` 的后处理（runs per variant）。
    #[test]
    fn runs_per_variant_uses_js_number_conversion() {
        assert_eq!(parse(&[], &[]).unwrap().runs_per_variant, 1.0);
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "3")])
                .unwrap()
                .runs_per_variant,
            3.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", " 7 ")])
                .unwrap()
                .runs_per_variant,
            7.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "0x10")])
                .unwrap()
                .runs_per_variant,
            16.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "0o17")])
                .unwrap()
                .runs_per_variant,
            15.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "0b101")])
                .unwrap()
                .runs_per_variant,
            5.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "1e2")])
                .unwrap()
                .runs_per_variant,
            100.0
        );
        assert_eq!(
            parse(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", "\u{feff}2")])
                .unwrap()
                .runs_per_variant,
            2.0
        );
        for invalid in ["0", "-1", "1.5", "Infinity", "1e", "0x", "12abc", "NaN"] {
            assert_eq!(
                parse_error(&[], &[("KIANA_EVAL_RUNS_PER_VARIANT", invalid)]),
                "Runs per variant must be a positive integer.",
                "{invalid}"
            );
        }
        // CLI 值优先于 env；trim 后为空（这里是单个空格）则回退到默认值 1。
        assert_eq!(
            parse(
                &["--runs-per-variant", " "],
                &[("KIANA_EVAL_RUNS_PER_VARIANT", "9")]
            )
            .unwrap()
            .runs_per_variant,
            1.0
        );
        assert_eq!(
            parse(
                &["--runs-per-variant", "4"],
                &[("KIANA_EVAL_RUNS_PER_VARIANT", "9")]
            )
            .unwrap()
            .runs_per_variant,
            4.0
        );
    }

    /// TS: `containerPath`（词法 resolve/relative + `\` → `/`）。
    #[test]
    fn container_path_normalizes_relative_and_absolute_paths() {
        let root = docker::package_root();
        assert_eq!(
            container_path(&root, "evals/smoke.eval.ts").unwrap(),
            "evals/smoke.eval.ts"
        );
        assert_eq!(
            container_path(&root, "evals/./sub/../a.docs.eval.ts").unwrap(),
            "evals/a.docs.eval.ts"
        );
        let absolute = docker::path_text(&root.join("evals").join("a.docs.eval.ts"));
        assert_eq!(
            container_path(&root, &absolute).unwrap(),
            "evals/a.docs.eval.ts"
        );
        assert_eq!(container_path(&root, ".").unwrap(), "");
        assert_eq!(
            container_path(&root, "a\\b.docs.eval.ts").unwrap(),
            "a/b.docs.eval.ts"
        );
    }

    /// TS: `containerPath` 拒绝包外路径。
    #[test]
    fn container_path_rejects_paths_outside_the_package() {
        let root = docker::package_root();
        let error = container_path(&root, "../outside.ts").unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval file must be inside {}: ../outside.ts",
                docker::path_text(&root)
            )
        );
        // `..foo` 也以 `..` 开头（TS 用字符串前缀判定）。
        let error = container_path(&root, "..foo/bar.ts").unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "Eval file must be inside {}: ..foo/bar.ts",
                docker::path_text(&root)
            )
        );
    }

    /// TS: `normalizeDiscoveredFile`
    #[test]
    fn normalize_discovered_file_strips_the_container_prefix() {
        assert_eq!(
            normalize_discovered_file("/repo/packages/evals/evals/a.docs.eval.ts").unwrap(),
            "evals/a.docs.eval.ts"
        );
        assert_eq!(
            normalize_discovered_file("/repo/packages/evals/").unwrap(),
            ""
        );
        assert_eq!(
            normalize_discovered_file("/repo/packages/other/a.ts")
                .unwrap_err()
                .to_string(),
            "Discovered eval path is outside the container package: /repo/packages/other/a.ts"
        );
        assert_eq!(
            normalize_discovered_file("evals/a.ts")
                .unwrap_err()
                .to_string(),
            "Discovered eval path is outside the container package: evals/a.ts"
        );
    }

    fn discovered(full_name: &str, file: &str) -> DiscoveredEvalCase {
        DiscoveredEvalCase {
            file: file.to_string(),
            full_name: full_name.to_string(),
            eval_set: String::new(),
            case_id: String::new(),
        }
    }

    /// TS: `compareDiscovery`
    #[test]
    fn compare_discovery_ignores_order_but_not_identity() {
        let left = vec![
            discovered("A > one", "/repo/packages/evals/evals/a.ts"),
            discovered("B > two", "/repo/packages/evals/evals/b.ts"),
        ];
        let right = vec![
            discovered("B > two", "/repo/packages/evals/evals/b.ts"),
            discovered("A > one", "/repo/packages/evals/evals/a.ts"),
        ];
        compare_discovery(&left, &right).unwrap();
        let different = vec![
            discovered("B > two", "/repo/packages/evals/evals/b.ts"),
            discovered("A > three", "/repo/packages/evals/evals/a.ts"),
        ];
        assert_eq!(
            compare_discovery(&left, &different)
                .unwrap_err()
                .to_string(),
            "Documentation variants discovered different eval cases."
        );
        let different_file = vec![
            discovered("A > one", "/repo/packages/evals/evals/other.ts"),
            discovered("B > two", "/repo/packages/evals/evals/b.ts"),
        ];
        assert_eq!(
            compare_discovery(&left, &different_file)
                .unwrap_err()
                .to_string(),
            "Documentation variants discovered different eval cases."
        );
        // 容器前缀缺失时先报前缀错误。
        let invalid = vec![discovered("A > one", "evals/a.ts")];
        assert_eq!(
            compare_discovery(&invalid, &invalid)
                .unwrap_err()
                .to_string(),
            "Discovered eval path is outside the container package: evals/a.ts"
        );
    }

    /// JS 默认 `sort()` 是 UTF-16 码元序（与 `str::cmp` 在非 ASCII 上不同）。
    #[test]
    fn default_eval_files_include_every_docs_eval_asset() {
        let root = docker::package_root();
        let mut files = list_default_eval_files(&root);
        files.sort_by(|left, right| code_unit_compare(left, right));
        // `**/` 必须匹配零层目录（`evals/*.docs.eval.ts`），且只匹配 `.docs.eval.ts`。
        let expected: Vec<String> = [
            "custom-provider.docs.eval.ts",
            "extensions.docs.eval.ts",
            "models.docs.eval.ts",
            "openai-provider.docs.eval.ts",
            "tui.docs.eval.ts",
        ]
        .iter()
        .map(|name| docker::path_text(&root.join("evals").join(name)))
        .collect();
        assert_eq!(files, expected);
        assert!(
            !files.iter().any(|file| file.ends_with("smoke.eval.ts")),
            "host-only evals must not match the docs glob"
        );
    }

    /// Node `globSync` 的 `follow: false`：不进入符号链接目录（含循环链接）。
    #[cfg(unix)]
    #[test]
    fn default_eval_files_do_not_follow_symlinked_directories() {
        let root = tempfile::tempdir().unwrap();
        let evals = root.path().join("evals");
        std::fs::create_dir_all(evals.join("real")).unwrap();
        std::fs::write(evals.join("top.docs.eval.ts"), "").unwrap();
        std::fs::write(evals.join("real").join("inner.docs.eval.ts"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("linked.docs.eval.ts"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), evals.join("linkdir")).unwrap();
        // 循环链接：跟随会无限递归或产生重复的嵌套匹配。
        std::os::unix::fs::symlink(&evals, evals.join("loop")).unwrap();
        // 名字匹配的符号链接（含悬空）会被 glob 当作条目返回。
        std::os::unix::fs::symlink("../top.docs.eval.ts", evals.join("linkfile.docs.eval.ts"))
            .unwrap();
        std::os::unix::fs::symlink("nowhere", evals.join("broken.docs.eval.ts")).unwrap();

        let mut files: Vec<String> = list_default_eval_files(root.path())
            .iter()
            .map(|path| container_path(root.path(), path).unwrap())
            .collect();
        files.sort_by(|left, right| code_unit_compare(left, right));
        assert_eq!(
            files,
            [
                "evals/broken.docs.eval.ts",
                "evals/linkfile.docs.eval.ts",
                "evals/real/inner.docs.eval.ts",
                "evals/top.docs.eval.ts",
            ]
        );
    }

    /// Node `globSync` 跳过不可读目录，而不是让整个 CLI 失败。
    #[cfg(unix)]
    #[test]
    fn default_eval_files_skip_unreadable_directories() {
        use std::os::unix::fs::PermissionsExt as _;
        // root 会忽略权限位。
        // SAFETY: `geteuid` 没有前置条件。
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let evals = root.path().join("evals");
        std::fs::create_dir_all(evals.join("closed")).unwrap();
        std::fs::write(evals.join("open.docs.eval.ts"), "").unwrap();
        std::fs::write(evals.join("closed").join("hidden.docs.eval.ts"), "").unwrap();
        let closed = evals.join("closed");
        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&closed, permissions).unwrap();

        let files = list_default_eval_files(root.path());
        // 恢复权限，让 tempdir 清理成功。
        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&closed, permissions).unwrap();

        assert_eq!(
            files,
            vec![docker::path_text(&evals.join("open.docs.eval.ts"))]
        );
    }

    /// TS `readFile` 的错误文本是 Node 的 `SystemError`（`open` / `read` + 路径）。
    #[tokio::test]
    async fn read_file_node_reports_node_system_errors() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.json");
        assert_eq!(
            read_file_node(&missing).await.unwrap_err().to_string(),
            format!(
                "ENOENT: no such file or directory, open '{}'",
                docker::path_text(&missing)
            )
        );
        // 目录：Node 能打开但读取时报 `EISDIR`（fs.promises 的 readFile 带路径）。
        assert_eq!(
            read_file_node(directory.path())
                .await
                .unwrap_err()
                .to_string(),
            format!(
                "EISDIR: illegal operation on a directory, read '{}'",
                docker::path_text(directory.path())
            )
        );
    }

    /// TS `writeFile` 的 `open` 失败带路径、`write` 失败不带路径。
    #[tokio::test]
    async fn write_file_node_reports_node_system_errors() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("missing").join("out.json");
        assert_eq!(
            write_file_node(&nested, "x").await.unwrap_err().to_string(),
            format!(
                "ENOENT: no such file or directory, open '{}'",
                docker::path_text(&nested)
            )
        );
        // 目标是目录：Node 在 open 时报 EISDIR（带路径）。
        assert_eq!(
            write_file_node(directory.path(), "x")
                .await
                .unwrap_err()
                .to_string(),
            format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                docker::path_text(directory.path())
            )
        );
        // 只读父目录：open 报 EACCES（带路径）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // SAFETY: `geteuid` 没有前置条件。
            if unsafe { libc::geteuid() } != 0 {
                let readonly = directory.path().join("readonly");
                std::fs::create_dir(&readonly).unwrap();
                let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
                permissions.set_mode(0o555);
                std::fs::set_permissions(&readonly, permissions).unwrap();
                let target = readonly.join("out.json");
                let message = write_file_node(&target, "x").await.unwrap_err().to_string();
                let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
                permissions.set_mode(0o700);
                std::fs::set_permissions(&readonly, permissions).unwrap();
                assert_eq!(
                    message,
                    format!(
                        "EACCES: permission denied, open '{}'",
                        docker::path_text(&target)
                    )
                );
            }
        }
    }

    /// TS: `artifactRunId`：ISO 时间（`:` → `-`）加 `_` 加小写连字符 UUID。
    #[test]
    fn artifact_run_id_has_the_timestamp_and_uuid_shape() {
        let id = artifact_run_id();
        let (timestamp, uuid) = id.split_once('_').unwrap();
        assert!(timestamp.ends_with('Z'), "{timestamp}");
        assert!(!timestamp.contains(':'), "{timestamp}");
        assert_eq!(timestamp.len(), "2026-10-26T12-34-56.789Z".len());
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.matches('-').count(), 4);
        assert_eq!(uuid.to_lowercase(), uuid);
    }

    #[test]
    fn js_number_matches_ecmascript_to_number() {
        assert!(js_number("").is_sign_positive());
        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_number("3"), 3.0);
        assert_eq!(js_number(" 3 "), 3.0);
        assert_eq!(js_number("+3"), 3.0);
        assert_eq!(js_number("-3"), -3.0);
        assert_eq!(js_number("3.5"), 3.5);
        assert_eq!(js_number(".5"), 0.5);
        assert_eq!(js_number("5."), 5.0);
        assert_eq!(js_number("5.e2"), 500.0);
        assert_eq!(js_number("1e-7"), 1e-7);
        assert_eq!(js_number("0x10"), 16.0);
        assert_eq!(js_number("0X10"), 16.0);
        assert_eq!(js_number("0o10"), 8.0);
        assert_eq!(js_number("0b10"), 2.0);
        assert_eq!(js_number("Infinity"), f64::INFINITY);
        assert_eq!(js_number("+Infinity"), f64::INFINITY);
        assert_eq!(js_number("-Infinity"), f64::NEG_INFINITY);
        assert!(js_number("1e400").is_infinite());
        assert!(js_number("-0").is_sign_negative());
        for invalid in [
            "", "  ", "1e", "1e+", "0x", "0b2", "1_000", "nan", "--1", "+-1",
        ] {
            let value = js_number(invalid);
            if invalid.trim().is_empty() {
                assert_eq!(value, 0.0);
            } else {
                assert!(value.is_nan(), "{invalid}");
            }
        }
        // 十六进制大数按精确整数一次舍入。
        assert_eq!(
            js_number("0xc6C03A14c6c8a41691E9CCEaeec18c"),
            0xc6c03a14c6c8a41691e9cceaeec18c_u128 as f64
        );
    }
}
