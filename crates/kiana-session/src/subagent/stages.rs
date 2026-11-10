//! Phase 2：声明式 stage 流水线的运行时支持。
//!
//! `AgentDefinition.stages` 声明一条**顺序**流水线；每个 stage 由 `owner`（缺省为
//! 声明它的 agent）执行，消费/产出固定的四类 artifact。本模块只提供与执行解耦的
//! 纯函数：
//!
//! - artifact 的 JSON Schema（复用 `yield_tool` 的校验实现）；
//! - stage 的 yield 契约与提示词构造；
//! - 产出收集与 schema 校验；
//! - `review.json` 的命名空间、写入与严格回读（缺失 / 非法 / 陈旧一律 reject）。
//!
//! 执行器在 `AgentSession` 的 Work 生命周期里：staged Work 不允许 resume、要求
//! `isolation: shared`，stage 之间通过 artifact 传递上下文。TS 参照实现没有这套
//! 机制，字段与错误文案以本模块与 ADR 为准。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use kiana_ai::types::JsonValue;
use kiana_ai::utils::validation::{collect_json_schema_errors, format_validation_path};

use crate::agent_definition::{AgentStage, StageArtifact};
use crate::subagent::work::{RunId, WorkId};
use crate::subagent::yield_tool::{YieldContract, YieldValidationMode};

/// `review.json` 的 schema 版本。
pub const REVIEW_RECORD_VERSION: u32 = 1;

/// artifact 的固定 JSON Schema。
pub fn artifact_schema(artifact: StageArtifact) -> JsonValue {
    match artifact {
        // 计划必须有至少一步；`notes` 可选，方便 owner 补充约束。
        StageArtifact::Plan => serde_json::json!({
            "type": "object",
            "required": ["steps"],
            "properties": {
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "string"}
                },
                "notes": {"type": "string"}
            }
        }),
        // 改动文件用相对路径；runner 不负责验证路径是否存在（隔离语义由 Work 决定）。
        StageArtifact::ChangedFiles => serde_json::json!({
            "type": "array",
            "items": {"type": "string"}
        }),
        // 测试证据只记录命令与结论，不复制完整输出（输出仍是工具凭证的一部分）。
        StageArtifact::TestEvidence => serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "required": ["command", "status"],
                "properties": {
                    "command": {"type": "string"},
                    "status": {"type": "string", "enum": ["passed", "failed", "skipped"]},
                    "summary": {"type": "string"}
                }
            }
        }),
        // 评审发现：severity/summary 必填，file/line 可选。
        StageArtifact::ReviewFindings => serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "required": ["severity", "summary"],
                "properties": {
                    "severity": {"type": "string", "enum": ["info", "warning", "error"]},
                    "summary": {"type": "string"},
                    "file": {"type": "string"},
                    "line": {"type": "integer"}
                }
            }
        }),
    }
}

/// 构造一个 stage 的 yield 契约：每个 `produces` 是一个必填 label。
pub fn stage_yield_contract(stage: &AgentStage) -> YieldContract {
    let mut label_schemas = IndexMap::new();
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for artifact in &stage.produces {
        let schema = artifact_schema(*artifact);
        label_schemas.insert(artifact.as_str().to_string(), schema.clone());
        properties.insert(artifact.as_str().to_string(), schema);
        required.push(JsonValue::String(artifact.as_str().to_string()));
    }
    YieldContract {
        output_schema: serde_json::json!({
            "type": "object",
            "required": required,
            "properties": JsonValue::Object(properties)
        }),
        label_schemas,
        mode: YieldValidationMode::Strict,
        max_retries: 2,
    }
}

/// 构造 stage 的提示词：原始任务 + 阶段说明 + 已产出 artifact（紧凑、确定序）。
pub fn build_stage_prompt(
    base_prompt: &str,
    stage: &AgentStage,
    index: usize,
    total: usize,
    artifacts: &BTreeMap<StageArtifact, JsonValue>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(base_prompt.trim_end());
    prompt.push_str(&format!(
        "\n\n## Workflow stage {}/{}: {}\n\n",
        index + 1,
        total,
        stage.name
    ));
    prompt.push_str(
        "You are running one stage of a sequential workflow. \
         Submit this stage's artifacts with the `yield` tool.\n\n",
    );
    prompt.push_str(
        "Artifacts to produce (all are required in a single `yield` output; each must \
         match its schema):\n",
    );
    for artifact in &stage.produces {
        // 只给名字时模型容易提交缺字段/枚举值错误的对象，stage 随后被判失败。
        // 把与校验完全相同的 schema 渲染进提示词，required/enum/类型都是模型可见的。
        prompt.push_str(&format!(
            "- `{}`: {}\n",
            artifact.as_str(),
            kiana_json::stringify(&kiana_json::normalize_numbers(artifact_schema(*artifact)))
        ));
    }
    prompt.push_str("\nInputs from earlier stages:\n");
    if stage.consumes.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for artifact in &stage.consumes {
            let value = artifacts.get(artifact).cloned().unwrap_or(JsonValue::Null);
            prompt.push_str(&format!(
                "- `{}`: {}\n",
                artifact.as_str(),
                kiana_json::stringify(&kiana_json::normalize_numbers(value))
            ));
        }
    }
    prompt
}

/// 从一次 stage 运行的 yield 输出里取出并校验该 stage 的全部 `produces`。
pub fn collect_stage_artifacts(
    stage: &AgentStage,
    output: Option<&JsonValue>,
) -> Result<BTreeMap<StageArtifact, JsonValue>, String> {
    let mut collected = BTreeMap::new();
    for artifact in &stage.produces {
        let value = output
            .and_then(|output| output.get(artifact.as_str()))
            .ok_or_else(|| {
                format!(
                    "stage `{}` did not yield required artifact `{}`",
                    stage.name,
                    artifact.as_str()
                )
            })?;
        let errors = collect_json_schema_errors(&artifact_schema(*artifact), value);
        if !errors.is_empty() {
            let details = errors
                .iter()
                .map(format_validation_path)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "stage `{}` artifact `{}` is invalid: {details}",
                stage.name,
                artifact.as_str()
            ));
        }
        collected.insert(*artifact, value.clone());
    }
    Ok(collected)
}

/// 从 stage 输出里取出 `review_findings`（仅当该 stage 声明产出它）。
///
/// 评审是审计产物：stage 在 yield 之后即使自身失败/取消/超时，已经产生的 findings
/// 也必须能持久化，因此这里不要求 stage 到达 Completed，也不检查其它 artifact。
/// 未产出时返回 `None`；产出但 schema 非法时返回错误，由调用方 reject。
pub fn review_findings_from_output(
    stage: &AgentStage,
    output: Option<&JsonValue>,
) -> Result<Option<JsonValue>, String> {
    if !stage.produces.contains(&StageArtifact::ReviewFindings) {
        return Ok(None);
    }
    let Some(value) = output.and_then(|output| output.get(StageArtifact::ReviewFindings.as_str()))
    else {
        return Ok(None);
    };
    let errors = collect_json_schema_errors(&artifact_schema(StageArtifact::ReviewFindings), value);
    if !errors.is_empty() {
        let details = errors
            .iter()
            .map(format_validation_path)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "stage `{}` artifact `review_findings` is invalid: {details}",
            stage.name
        ));
    }
    Ok(Some(value.clone()))
}

/// 把 artifact map 变成稳定的 JSON 对象（按固定 artifact 顺序）。
pub fn artifacts_to_json(artifacts: &BTreeMap<StageArtifact, JsonValue>) -> JsonValue {
    let mut object = serde_json::Map::new();
    for artifact in StageArtifact::ALL {
        if let Some(value) = artifacts.get(&artifact) {
            object.insert(artifact.as_str().to_string(), value.clone());
        }
    }
    JsonValue::Object(object)
}

/// `review.json` 路径：
/// `<cwd>/.kiana/reviews/<session-key>/<workId>/<run-key>/<stage-key>/review.json`。
///
/// 按 `workId` 命名空间隔离，并额外带上父会话键：workId 只在单个会话内唯一，
/// 同一仓库里的两个会话都会从 1 开始编号。`runId` 也进入命名空间：没有 durable
/// 账本时 workId 来自进程级计数器，同一会话在新进程里会再次从 1 开始，只按
/// workId 命名会让新的评审覆盖旧评审（以及共用的 `review.json.tmp`）。stage 名
/// 同样必需：`validate_stages` 允许多个 stage 产出 `review_findings`，只按
/// `(workId, runId)` 命名会让后一个 stage 覆盖前一个的 findings。
pub fn review_path(
    cwd: &Path,
    parent_session_id: &str,
    work_id: WorkId,
    run_id: &RunId,
    stage: &str,
) -> PathBuf {
    review_components(parent_session_id, work_id, run_id, stage)
        .iter()
        .fold(cwd.to_path_buf(), |path, component| path.join(component))
        .join("review.json")
}

/// `review.json` 所在的目录组件（相对 cwd）；写入与路径推导共用同一份定义。
fn review_components(
    parent_session_id: &str,
    work_id: WorkId,
    run_id: &RunId,
    stage: &str,
) -> Vec<String> {
    vec![
        ".kiana".to_string(),
        "reviews".to_string(),
        identity_path_key(parent_session_id),
        work_id.0.to_string(),
        identity_path_key(&run_id.0),
        identity_path_key(stage),
    ]
}

/// 身份命名空间键：可读前缀 + 稳定哈希。
///
/// 只做字符替换会让 `a/b` 与 `a:b` 落到同一个目录，共享固定的
/// `review.json.tmp` 并互相误判陈旧；这里附加 FNV-1a 哈希并限制前缀长度。
fn identity_path_key(identity: &str) -> String {
    let mut sanitized = identity
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.len() > 64 {
        sanitized.truncate(64);
    }
    if sanitized.is_empty() {
        sanitized.push_str("session");
    }
    // FNV-1a：稳定、无额外依赖，仅用于目录名去重（与 durable_store 的会话目录一致）。
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in identity.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3_u64);
    }
    format!("{sanitized}-{hash:016x}")
}

/// 逐级拒绝 symlink 目录组件。
///
/// 克隆仓库可以把 `.kiana` 提交成指向工作区外的 symlink；若沿用它写入，
/// `review.json` 会落到权限引擎与 workspace 边界之外。
fn ensure_plain_directory_chain(cwd: &Path, components: &[String]) -> Result<(), String> {
    let mut current = cwd.to_path_buf();
    for component in components {
        current = current.join(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "review.json is invalid: {} is a symlink; refusing to write outside the workspace",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "review.json is invalid: {} is not a directory",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "review.json is invalid: cannot inspect {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

/// 目标文件或临时文件不能是 symlink（`fs::write` 会跟随 symlink 写到目标）。
fn ensure_not_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "review.json is invalid: {} is a symlink",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "review.json is invalid: cannot inspect {}: {error}",
            path.display()
        )),
    }
}

/// 写临时文件：最终组件不能是符号链接。
///
/// 先 `ensure_not_symlink` 再 `write` 之间存在 TOCTOU；Unix 上用 `O_NOFOLLOW`
/// 打开，链接会直接返回 `ELOOP`，而不是把内容写到链接目标。
fn write_temporary(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    ensure_not_symlink(path).map_err(std::io::Error::other)?;
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

/// 写入并严格回读 `review.json`；返回路径。
///
/// 回读不是形式主义：宿主/其它进程可能已经留下同名的陈旧文件，写入失败或内容被
/// 替换时只有回读能发现。缺失、非法、陈旧（workId/runId/stage 不匹配，或写入时间
/// 早于本次运行）都返回 `Err`，调用方必须把 stage 判为 reject。
#[allow(clippy::too_many_arguments)]
pub fn persist_review(
    cwd: &Path,
    parent_session_id: &str,
    work_id: WorkId,
    run_id: &RunId,
    stage: &AgentStage,
    findings: &JsonValue,
    started_at_ms: u64,
    now_ms: u64,
) -> Result<PathBuf, String> {
    let components = review_components(parent_session_id, work_id, run_id, &stage.name);
    ensure_plain_directory_chain(cwd, &components)?;
    let directory = components
        .iter()
        .fold(cwd.to_path_buf(), |path, component| path.join(component));
    let path = directory.join("review.json");
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "review.json is invalid: cannot create {}: {error}",
            directory.display()
        )
    })?;
    let mut record = serde_json::Map::new();
    record.insert(
        "version".to_string(),
        JsonValue::from(REVIEW_RECORD_VERSION),
    );
    // 身份字段写成十进制字符串：与 durable Work 账本一致，避免 > 2^53 的 WorkId
    // 在 JS 数字语义下舍入。
    record.insert(
        "workId".to_string(),
        JsonValue::String(work_id.0.to_string()),
    );
    record.insert("runId".to_string(), JsonValue::String(run_id.0.clone()));
    record.insert("stage".to_string(), JsonValue::String(stage.name.clone()));
    record.insert("writtenAtMs".to_string(), JsonValue::from(now_ms));
    record.insert("findings".to_string(), findings.clone());
    let text = kiana_json::stringify(&kiana_json::normalize_numbers(JsonValue::Object(record)));
    let temporary = path.with_extension("json.tmp");
    ensure_not_symlink(&temporary)?;
    ensure_not_symlink(&path)?;
    write_temporary(&temporary, &text).map_err(|error| {
        format!(
            "review.json is missing: cannot write {}: {error}",
            path.display()
        )
    })?;
    // 检查与 rename 之间仍可能有人替换目标；rename 本身只替换目录项、不跟随
    // 符号链接，写入内容也已经通过 `O_NOFOLLOW` 落在真实文件上。这里再复核一次，
    // 把“目标是链接”变成明确的 reject，而不是悄悄替换链接。
    ensure_not_symlink(&path)?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "review.json is missing: cannot replace {}: {error}",
            path.display()
        )
    })?;
    load_review(&path, work_id, run_id, stage, started_at_ms)?;
    Ok(path)
}

/// 严格读取 `review.json`，返回 findings；缺失 / 非法 / 陈旧都报错。
pub fn load_review(
    path: &Path,
    work_id: WorkId,
    run_id: &RunId,
    stage: &AgentStage,
    started_at_ms: u64,
) -> Result<JsonValue, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| format!("review.json is missing at {}", path.display()))?;
    let value =
        kiana_json::parse(&text).map_err(|error| format!("review.json is invalid: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "review.json is invalid: expected an object".to_string())?;
    let stored_work_id = match object.get("workId") {
        Some(JsonValue::String(text)) => text.parse::<i64>().ok(),
        Some(JsonValue::Number(number)) => number.as_i64(),
        _ => None,
    };
    if stored_work_id != Some(work_id.0) {
        return Err(format!(
            "review.json is stale: workId {:?} does not match {}",
            object.get("workId"),
            work_id.0
        ));
    }
    let stored_run_id = object.get("runId").and_then(JsonValue::as_str);
    if stored_run_id != Some(run_id.0.as_str()) {
        return Err(format!(
            "review.json is stale: runId {stored_run_id:?} does not match {}",
            run_id.0
        ));
    }
    let stored_stage = object.get("stage").and_then(JsonValue::as_str);
    if stored_stage != Some(stage.name.as_str()) {
        return Err(format!(
            "review.json is stale: stage {stored_stage:?} does not match `{}`",
            stage.name
        ));
    }
    let written_at_ms = object
        .get("writtenAtMs")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "review.json is invalid: missing `writtenAtMs`".to_string())?;
    if written_at_ms < started_at_ms {
        return Err(format!(
            "review.json is stale: writtenAtMs {written_at_ms} is older than the run start {started_at_ms}"
        ));
    }
    let findings = object
        .get("findings")
        .ok_or_else(|| "review.json is invalid: missing `findings`".to_string())?;
    let errors =
        collect_json_schema_errors(&artifact_schema(StageArtifact::ReviewFindings), findings);
    if !errors.is_empty() {
        let details = errors
            .iter()
            .map(format_validation_path)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("review.json is invalid: findings {details}"));
    }
    Ok(findings.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_definition::AgentStageBudget;

    fn stage(name: &str, consumes: &[StageArtifact], produces: &[StageArtifact]) -> AgentStage {
        AgentStage {
            name: name.to_string(),
            owner: None,
            consumes: consumes.to_vec(),
            produces: produces.to_vec(),
            exit_criteria: Vec::new(),
            budget: AgentStageBudget::default(),
        }
    }

    #[test]
    fn artifact_schemas_accept_and_reject_the_expected_shapes() {
        let valid = [
            (
                StageArtifact::Plan,
                serde_json::json!({"steps": ["read", "edit"]}),
            ),
            (
                StageArtifact::ChangedFiles,
                serde_json::json!(["src/lib.rs"]),
            ),
            (
                StageArtifact::TestEvidence,
                serde_json::json!([{"command": "cargo test", "status": "passed"}]),
            ),
            (
                StageArtifact::ReviewFindings,
                serde_json::json!([{"severity": "warning", "summary": "missing test"}]),
            ),
        ];
        for (artifact, value) in valid {
            assert!(
                collect_json_schema_errors(&artifact_schema(artifact), &value).is_empty(),
                "{} must accept {value}",
                artifact.as_str()
            );
        }

        let invalid = [
            (StageArtifact::Plan, serde_json::json!({"steps": []})),
            (StageArtifact::ChangedFiles, serde_json::json!("src/lib.rs")),
            (
                StageArtifact::TestEvidence,
                serde_json::json!([{"command": "cargo test", "status": "maybe"}]),
            ),
            (
                StageArtifact::ReviewFindings,
                serde_json::json!([{"severity": "fatal", "summary": "boom"}]),
            ),
        ];
        for (artifact, value) in invalid {
            assert!(
                !collect_json_schema_errors(&artifact_schema(artifact), &value).is_empty(),
                "{} must reject {value}",
                artifact.as_str()
            );
        }
    }

    #[test]
    fn yield_contract_requires_every_produced_artifact() {
        let contract = stage_yield_contract(&stage(
            "implement",
            &[StageArtifact::Plan],
            &[StageArtifact::ChangedFiles, StageArtifact::TestEvidence],
        ));
        assert_eq!(contract.label_schemas.len(), 2);
        assert_eq!(
            contract.output_schema["required"],
            serde_json::json!(["changed_files", "test_evidence"])
        );
    }

    #[test]
    fn collect_reports_missing_and_invalid_artifacts() {
        let stage = stage("verify", &[], &[StageArtifact::ReviewFindings]);
        let error = collect_stage_artifacts(&stage, None).expect_err("missing output");
        assert!(error.contains("did not yield required artifact `review_findings`"));
        let output = serde_json::json!({"review_findings": [{"severity": "bad"}]});
        let error = collect_stage_artifacts(&stage, Some(&output)).expect_err("invalid findings");
        assert!(
            error.contains("artifact `review_findings` is invalid"),
            "{error}"
        );
        let output = serde_json::json!({"review_findings": []});
        assert_eq!(
            collect_stage_artifacts(&stage, Some(&output))
                .expect("empty findings are valid")
                .len(),
            1
        );
    }

    #[test]
    fn stage_prompt_lists_inputs_deterministically() {
        let stage = stage(
            "implement",
            &[StageArtifact::Plan, StageArtifact::TestEvidence],
            &[StageArtifact::ChangedFiles],
        );
        let mut artifacts = BTreeMap::new();
        artifacts.insert(StageArtifact::Plan, serde_json::json!({"steps": ["a"]}));
        artifacts.insert(StageArtifact::TestEvidence, serde_json::json!([]));
        let prompt = build_stage_prompt("do the work", &stage, 1, 3, &artifacts);
        assert!(prompt.starts_with("do the work"));
        assert!(prompt.contains("## Workflow stage 2/3: implement"));
        assert!(prompt.contains("- `plan`: {\"steps\":[\"a\"]}"));
        assert!(prompt.contains("- `test_evidence`: []"));
        assert!(
            !prompt.contains("`changed_files`: []"),
            "inputs only list consumed artifacts"
        );
    }

    /// 回归（round-17 finding #6）：提示词必须暴露与校验相同的 required/enum，
    /// 否则模型只能猜 artifact 形状，stage 因不完整对象被判失败。
    #[test]
    fn stage_prompt_renders_the_produced_artifact_schemas() {
        let stage = stage("verify", &[], &[StageArtifact::TestEvidence]);
        let prompt = build_stage_prompt("verify it", &stage, 0, 1, &BTreeMap::new());
        assert!(
            prompt.contains("- `test_evidence`: {\"type\":\"array\""),
            "{prompt}"
        );
        assert!(
            prompt.contains("\"required\":[\"command\",\"status\"]"),
            "required fields must be visible: {prompt}"
        );
        assert!(
            prompt.contains("\"enum\":[\"passed\",\"failed\",\"skipped\"]"),
            "enum values must be visible: {prompt}"
        );
        assert!(
            prompt.contains("all are required in a single `yield` output"),
            "{prompt}"
        );
    }

    #[test]
    fn review_round_trip_rejects_missing_invalid_and_stale_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let stage = stage("verify", &[], &[StageArtifact::ReviewFindings]);
        let findings = serde_json::json!([{"severity": "info", "summary": "ok"}]);
        let path = persist_review(
            temp.path(),
            "session/with:chars",
            WorkId(7),
            &RunId::new("run-7"),
            &stage,
            &findings,
            100,
            200,
        )
        .expect("persist");
        assert!(path.ends_with("review.json"));
        let path_text = path.to_string_lossy();
        assert!(
            path_text.contains("session_with_chars"),
            "session ids are sanitized: {path_text}"
        );
        assert!(
            path_text.contains("7") && path_text.contains("reviews"),
            "reviews are namespaced by work id: {path_text}"
        );
        assert_eq!(
            load_review(&path, WorkId(7), &RunId::new("run-7"), &stage, 100).expect("load"),
            findings
        );

        let other_run = review_path(
            temp.path(),
            "session/with:chars",
            WorkId(7),
            &RunId::new("run-8"),
            "verify",
        );
        assert_ne!(
            path, other_run,
            "the same work resumed in another run must not reuse the review file"
        );

        let missing = path.with_file_name("absent.json");
        let error = load_review(&missing, WorkId(7), &RunId::new("run-7"), &stage, 100)
            .expect_err("missing");
        assert!(error.contains("is missing"), "{error}");

        std::fs::write(&path, "not json").expect("write invalid");
        let error =
            load_review(&path, WorkId(7), &RunId::new("run-7"), &stage, 100).expect_err("invalid");
        assert!(error.contains("is invalid"), "{error}");

        persist_review(
            temp.path(),
            "session/with:chars",
            WorkId(7),
            &RunId::new("run-7"),
            &stage,
            &findings,
            100,
            200,
        )
        .expect("rewrite");
        let error = load_review(&path, WorkId(7), &RunId::new("run-8"), &stage, 100)
            .expect_err("stale run");
        assert!(error.contains("is stale"), "{error}");
        let error = load_review(&path, WorkId(7), &RunId::new("run-7"), &stage, 201)
            .expect_err("stale start");
        assert!(error.contains("is stale"), "{error}");
    }

    /// round-18 finding #6：多个 stage 都产出 `review_findings` 时不能互相覆盖。
    #[test]
    fn reviews_from_two_stages_do_not_overwrite_each_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_stage = stage("verify", &[], &[StageArtifact::ReviewFindings]);
        let second_stage = stage("re-review", &[], &[StageArtifact::ReviewFindings]);
        let first_findings = serde_json::json!([{"severity": "info", "summary": "first"}]);
        let second_findings = serde_json::json!([{"severity": "error", "summary": "second"}]);
        let first = persist_review(
            temp.path(),
            "session",
            WorkId(3),
            &RunId::new("run-3"),
            &first_stage,
            &first_findings,
            100,
            200,
        )
        .expect("first review");
        let second = persist_review(
            temp.path(),
            "session",
            WorkId(3),
            &RunId::new("run-3"),
            &second_stage,
            &second_findings,
            100,
            300,
        )
        .expect("second review");
        assert_ne!(first, second, "each stage needs its own review namespace");
        assert_eq!(
            load_review(&first, WorkId(3), &RunId::new("run-3"), &first_stage, 100)
                .expect("first survives"),
            first_findings
        );
        assert_eq!(
            load_review(&second, WorkId(3), &RunId::new("run-3"), &second_stage, 100)
                .expect("second survives"),
            second_findings
        );
    }

    #[test]
    fn session_keys_are_hashed_and_bounded() {
        let cwd = Path::new("/tmp/workspace");
        let slash = review_path(cwd, "a/b", WorkId(1), &RunId::new("run-a"), "verify");
        let colon = review_path(cwd, "a:b", WorkId(1), &RunId::new("run-a"), "verify");
        assert_ne!(
            slash, colon,
            "distinct session ids must not share a reviews namespace"
        );
        let key = identity_path_key(&"x".repeat(200));
        assert!(key.len() <= 64 + 1 + 16, "{key}");
    }

    /// 回归（round-17 finding #7）：临时文件写入不能跟随符号链接。`O_NOFOLLOW`
    /// 让打开本身失败，链接目标保持原样（检查与使用之间没有可利用的窗口）。
    #[cfg(unix)]
    #[test]
    fn write_temporary_does_not_follow_a_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, "untouched").expect("outside");
        let temporary = temp.path().join("review.json.tmp");
        std::os::unix::fs::symlink(&outside, &temporary).expect("symlink");
        assert!(
            write_temporary(&temporary, "replacement").is_err(),
            "a symlinked temp file must not be opened"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).expect("outside"),
            "untouched",
            "the symlink target must not be written through"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persist_review_rejects_symlinked_state_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, temp.path().join(".kiana")).expect("symlink");
        let review_stage = stage("verify", &[], &[StageArtifact::ReviewFindings]);
        let error = persist_review(
            temp.path(),
            "session",
            WorkId(1),
            &RunId::new("run-1"),
            &review_stage,
            &serde_json::json!([]),
            100,
            200,
        )
        .expect_err("a symlinked .kiana directory must be rejected");
        assert!(error.contains("symlink"), "{error}");
        assert!(
            !outside.join("reviews").exists(),
            "review.json must never be written outside the workspace"
        );
    }
}
