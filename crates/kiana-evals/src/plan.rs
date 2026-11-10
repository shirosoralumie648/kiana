//! evals/src/plan.ts。
//!
//! 把 `vitest --discover` 输出的「被发现的评测用例」展开成可执行的评测任务矩阵。
//! 移植自 TS；错误文本、字段顺序与用例顺序是当前稳定契约（`expected-runs.json`
//! 与 `protocol.json` 的 sha256 依赖任务数组的序列化字节）。
//!
//! # 为什么独立成模块
//! `plan.ts` 在 TS 侧是纯函数层：不读写文件、不启动 Docker、不依赖进程状态，只做
//! 「校验 → 展开」。本模块保持同样的边界，因此可以在不启动容器的情况下直接用
//! `serde_json::Value` 测试；`cli` 也只在拿到 discovery 的 JSON 之后才进入这里。
//!
//! # 在调用链中的位置
//! 上游是 `cli::run_with`：它按 [`DOCUMENTATION_VARIANTS`] 的顺序对两个变体各执行一次
//! discovery，分别用 [`parse_discovered_cases`] 解析结果，确认两个变体发现的用例集合一致后，
//! 对第一个变体的用例调用 [`create_task_plan`]。
//! 展开出的任务数组（[`EvalTask`]）有两个消费者：`cli` 把它序列化进 `protocol.json`
//! 的 `tasks` 字段（参与摘要计算）和 `expected-runs.json`；随后按数组顺序逐条交给
//! `docker::run_task`，后者用任务字段生成容器目录名与测试名正则，不再重新校验。
//!
//! # 执行流程
//! 1. [`parse_discovered_cases`] 按数组顺序逐项检查：项必须是对象且 `name`/`file` 是
//!    字符串，`name` 必须恰好是两段以 `" > "` 分隔的非空（JS `trim()` 后）文本，且
//!    `(eval_set, case_id)` 在本次输入中首次出现。任一检查失败立即返回 [`PlanError`]，
//!    不会返回部分结果。
//! 2. [`create_task_plan`] 先校验 `model` 与 `runs_per_variant`，再按
//!    「用例（输入顺序）× 轮次 1..=N × 变体」三层循环生成任务；每轮的变体顺序交替。
//!
//! # 不变量与注意事项
//! - 任务数组的顺序属于外部契约：`protocol.json` 的 sha256 与 `expected-runs.json` 的
//!   字节都由序列化顺序决定，调整循环层次、字段声明顺序或变体顺序都会改变产物。
//! - 本模块不排序用例，只拒绝重复身份；输出保持 discovery 的原始顺序。
//! - 本模块的错误消息是 TS `TypeError.message` 的逐字复刻，`cli` 通过 `CliError::Plan`
//!   原样打印，属于可观察输出，不能顺手改写。

use std::collections::HashSet;

use kiana_ai::auth::oauth::js_trim;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 两个文档变体，顺序即 `DOCUMENTATION_VARIANTS` 的顺序。
///
/// TS: `plan.ts#DOCUMENTATION_VARIANTS`
///
/// 顺序是可观察的：`cli` 按此顺序执行两次 discovery，`docker::build_images` 按此顺序
/// 构建镜像，`tests/plan.rs` 也锁定了 `without_docs` 在前的顺序。
pub const DOCUMENTATION_VARIANTS: [DocumentationVariant; 2] = [
    DocumentationVariant::WithoutDocs,
    DocumentationVariant::WithDocs,
];

/// 文档变体：评测运行时是否加载 Kiana 文档。
///
/// TS: `plan.ts#DocumentationVariant`（`"without_docs" | "with_docs"`）
///
/// `serde` 使用 `snake_case` 序列化，与 TS 字面量及 [`DocumentationVariant::as_str`] 的
/// 返回值一致；报告 JSON 与任务身份哈希里的变体字符串因此不会漂移。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentationVariant {
    /// 不提供 Kiana 文档（对照组）。
    WithoutDocs,
    /// 提供 Kiana 文档（实验组）。
    WithDocs,
}

impl DocumentationVariant {
    /// TS 的字符串字面量值（`JSON.stringify` / 模板字符串的输出）。
    ///
    /// 同时充当多个外部标识：`docker build --target` 的目标名、discovery 输出目录
    /// `<artifact>/discovery/<variant>`、报告里的 `<artifact>/<variant>/...` 路径段。
    /// 因此这些字符串是冻结契约，不能改名。
    pub fn as_str(self) -> &'static str {
        match self {
            DocumentationVariant::WithoutDocs => "without_docs",
            DocumentationVariant::WithDocs => "with_docs",
        }
    }
}

impl std::fmt::Display for DocumentationVariant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `vitest --discover` 返回的单个评测用例身份。
///
/// TS: `plan.ts#DiscoveredEvalCase`（字段顺序 `file, fullName, evalSet, caseId`）
///
/// `serde` 以 camelCase 输出字段名，声明顺序就是 `JSON.stringify` 的键序；
/// `tests/plan.rs` 用逐字节字符串断言锁定该顺序。`file` 保留 discovery 的原值，
/// `cli` 在比较完两个变体之后才去掉容器内的包前缀。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredEvalCase {
    /// 发现该用例的评测文件路径。
    pub file: String,
    /// Vitest 的完整用例名（`<eval set> > <case>`）。
    pub full_name: String,
    /// `full_name` 中 ` > ` 之前的部分（未 trim）。
    pub eval_set: String,
    /// `full_name` 中 ` > ` 之后的部分（未 trim）。
    pub case_id: String,
}

/// 一个 `(用例, 变体, 模型, run)` 组合；`createTaskPlan` 的输出。
///
/// TS: `plan.ts#EvalTask`（字段顺序 `file, fullName, evalSet, caseId, variant, model, runNumber`；
/// 展开顺序决定 `expected-runs.json` 的键序）
///
/// 下游直接消费这些字段：`docker::run_task` 用
/// `(eval_set, case_id, variant, model, run_number)` 求任务目录名，用
/// `"{eval_set} {case_id}"` 作为 vitest 的测试名，不会再次校验取值。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalTask {
    /// 发现该用例的评测文件路径。
    pub file: String,
    /// Vitest 的完整用例名。
    pub full_name: String,
    /// 评测集名。
    pub eval_set: String,
    /// 用例名。
    pub case_id: String,
    /// 文档变体。
    pub variant: DocumentationVariant,
    /// `<provider>/<model>`。
    pub model: String,
    /// 从 1 开始的重复序号。
    pub run_number: u64,
}

/// `parseDiscoveredCases` / `createTaskPlan` 的错误（TS 侧全部是 `TypeError`）。
///
/// 消息文本是当前稳定契约；上层 CLI 只打印该文本（改动需同步测试）。
///
/// 只保留文本而不设结构化变体，是因为 TS 侧全部抛 `TypeError` 且调用方只读取
/// `message`；`cli::CliError` 通过 `#[from]` 把它原样包装为 `Plan` 变体，`Display`
/// 输出仍是这段文本。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);

impl PlanError {
    fn new(message: impl Into<String>) -> Self {
        PlanError(message.into())
    }
}

/// 校验 `vitest --discover` 的 JSON 输出并派生稳定的用例身份。
///
/// TS: `plan.ts#parseDiscoveredCases`
///
/// - 非数组 → `Discovered eval cases must be an array.`
/// - 项不是对象、或 `name`/`file` 不是字符串 → `Discovered eval case is invalid.`
/// - `name` 不是恰好两段以 `" > "` 分隔的非空（`trim()` 后）文本 →
///   `Documentation eval must use "<eval set> > <case>": {name}`
/// - `(evalSet, caseId)` 重复 → `Duplicate eval case identity: {name}`
///
/// 其它行为：
/// - 每项只检查 `name` / `file` 两个字段，记录里的额外字段被忽略。
/// - 分隔符是精确的三字符 `" > "`；出现两处及以上就不满足「恰好两段」。
/// - 逐项遍历，首个非法项立即返回错误，输入顺序决定报出哪条错误。
/// - 重复身份用**未 trim** 的两段比较，所以 `"a > b"` 与 `" a > b "` 是两个不同用例。
/// - 返回顺序与输入数组一致，`full_name` 原样保留。
///
/// 存下来的 `eval_set`/`case_id` 是**未 trim** 的原值。
pub fn parse_discovered_cases(value: &Value) -> Result<Vec<DiscoveredEvalCase>, PlanError> {
    let Value::Array(items) = value else {
        return Err(PlanError::new("Discovered eval cases must be an array."));
    };

    // TS 用 `JSON.stringify([evalSet, caseId])` 做去重键。该编码对字符串二元组是单射，
    // 因此直接用元组本身去重与逐字比较 JSON 文本等价。
    // 键借用 `value` 内部的 `&str`，生命周期覆盖整个函数，无需复制字符串。
    let mut identities: HashSet<(&str, &str)> = HashSet::new();
    let mut cases = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(record) = item else {
            return Err(PlanError::new("Discovered eval case is invalid."));
        };
        let (Some(Value::String(name)), Some(Value::String(file))) =
            (record.get("name"), record.get("file"))
        else {
            return Err(PlanError::new("Discovered eval case is invalid."));
        };

        // TS 侧用 `const [evalSet, caseId, ...extra] = name.split(" > ")` 并要求
        // `extra.length === 0`；这里用「split 后恰好两段」表达同一条件。
        let parts: Vec<&str> = name.split(" > ").collect();
        let valid =
            parts.len() == 2 && !js_trim(parts[0]).is_empty() && !js_trim(parts[1]).is_empty();
        if !valid {
            return Err(PlanError::new(format!(
                "Documentation eval must use \"<eval set> > <case>\": {name}"
            )));
        }
        let (eval_set, case_id) = (parts[0], parts[1]);
        if !identities.insert((eval_set, case_id)) {
            return Err(PlanError::new(format!(
                "Duplicate eval case identity: {name}"
            )));
        }
        cases.push(DiscoveredEvalCase {
            file: file.clone(),
            full_name: name.clone(),
            eval_set: eval_set.to_string(),
            case_id: case_id.to_string(),
        });
    }
    Ok(cases)
}

/// `Number.isSafeInteger(runsPerVariant)` 的等价判断（TS 用 JS number 表达）。
///
/// 与 JS 语义对齐：`NaN`、`±Infinity`、带小数部分、或绝对值超过 2^53-1 都返回 `false`。
/// `-0.0` 会通过本函数（小数部分为 0），由调用方的 `< 1.0` 检查拒绝。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// 把用例展开成 `runsPerVariant` 轮、每轮两个变体的任务数组。
///
/// TS: `plan.ts#createTaskPlan`
///
/// 校验顺序固定：先 `model` 后 `runs_per_variant`，两者都不合法时返回的是 model 错误。
/// `model` 只要求含 `/` 且不以 `/` 开头或结尾，因此 `"a/b/c"` 这类多斜杠形式也会通过。
///
/// 奇数轮变体顺序是 `[without_docs, with_docs]`，偶数轮是 `[with_docs, without_docs]`
/// （抵消缓存/顺序偏差）。`runs_per_variant` 用 `f64` 表达 TS 的 `number`：必须是安全整数且
/// `>= 1`（`Number.isSafeInteger` 语义）。
///
/// 输出顺序为用例输入顺序 → `run_number` 升序 → 该轮的变体顺序，长度恒为
/// `cases.len() * runs * 2`；`cli` 按这个顺序写入 `expected-runs.json` 并顺序执行任务。
///
/// 错误：
/// - `model` 不含 `/`、或以 `/` 开头/结尾 →
///   `Model identity must contain a provider and model.`
/// - `runs_per_variant` 非安全整数（含 `NaN`、`±Infinity`、小数）或 `< 1` →
///   `Runs per variant must be a positive integer.`
pub fn create_task_plan(
    cases: &[DiscoveredEvalCase],
    model: &str,
    runs_per_variant: f64,
) -> Result<Vec<EvalTask>, PlanError> {
    if !model.contains('/') || model.starts_with('/') || model.ends_with('/') {
        return Err(PlanError::new(
            "Model identity must contain a provider and model.",
        ));
    }
    if !is_safe_integer(runs_per_variant) || runs_per_variant < 1.0 {
        return Err(PlanError::new(
            "Runs per variant must be a positive integer.",
        ));
    }
    // 已通过安全整数与 `>= 1` 校验：该范围内 f64 → u64 是精确转换，不会截断或回绕。
    let runs = runs_per_variant as u64;

    // 按「用例数 × 轮数 × 2 个变体」预留容量；最终任务数与该乘积一致。
    let mut tasks = Vec::with_capacity(cases.len() * runs as usize * 2);
    // 循环层次直接决定产物顺序（用例 → 轮次 → 变体），下游按数组顺序序列化与执行。
    for eval_case in cases {
        for run_number in 1..=runs {
            let variants: [DocumentationVariant; 2] = if run_number % 2 == 1 {
                [
                    DocumentationVariant::WithoutDocs,
                    DocumentationVariant::WithDocs,
                ]
            } else {
                [
                    DocumentationVariant::WithDocs,
                    DocumentationVariant::WithoutDocs,
                ]
            };
            for variant in variants {
                tasks.push(EvalTask {
                    file: eval_case.file.clone(),
                    full_name: eval_case.full_name.clone(),
                    eval_set: eval_case.eval_set.clone(),
                    case_id: eval_case.case_id.clone(),
                    variant,
                    model: model.to_string(),
                    run_number,
                });
            }
        }
    }
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_non_arrays_and_invalid_records() {
        assert_eq!(
            parse_discovered_cases(&json!(null))
                .unwrap_err()
                .to_string(),
            "Discovered eval cases must be an array."
        );
        for value in [
            json!([null]),
            json!([[]]),
            json!(["x"]),
            json!([{}]),
            json!([{"name": 1, "file": "f"}]),
            json!([{"name": "a > b"}]),
        ] {
            assert_eq!(
                parse_discovered_cases(&value).unwrap_err().to_string(),
                "Discovered eval case is invalid."
            );
        }
    }

    #[test]
    fn rejects_names_with_the_wrong_shape() {
        let cases = |name: &str| json!([{ "name": name, "file": "f" }]);
        for name in ["a", "a > b > c", " > b", "a > ", "   > b"] {
            let error = parse_discovered_cases(&cases(name))
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!("Documentation eval must use \"<eval set> > <case>\": {name}")
            );
        }
        // `trim()` 使用 JS 语义（含 BOM）。
        let error = parse_discovered_cases(&cases("\u{feff} > b"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("<eval set> > <case>"));
    }

    #[test]
    fn keeps_untrimmed_parts_and_rejects_unsafe_run_counts() {
        let cases = parse_discovered_cases(&json!([{ "name": " a  >  b ", "file": "f" }])).unwrap();
        assert_eq!(cases[0].eval_set, " a ");
        assert_eq!(cases[0].case_id, " b ");
        for value in [
            f64::NAN,
            f64::INFINITY,
            1.5,
            0.0,
            -1.0,
            9_007_199_254_740_992.0,
        ] {
            assert_eq!(
                create_task_plan(&cases, "p/m", value)
                    .unwrap_err()
                    .to_string(),
                "Runs per variant must be a positive integer."
            );
        }
    }
}
