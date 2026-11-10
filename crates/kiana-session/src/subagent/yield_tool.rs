//! `yield` 工具：按 JSON Schema 校验子代理的最终结果，并支持 label 增量装配。
//!
//! 子代理可能分多次产出结果：先给 `{"label":"findings","value":[...]}`，再给
//! `{"label":"summary","value":"..."}`。本模块把多次提交装配成一个输出对象：
//!
//! - **数组 append**：同一 label 连续提交数组时拼接；
//! - **标量 replace**：标量/对象后提交者覆盖先前的值；
//! - **类型变化**在 `Strict` 模式下报错（防止“把数组变成标量”这种静默截断），
//!   `Permissive` 模式下覆盖并记录 warning。
//!
//! 校验使用 `kiana_ai::utils::validation`（与工具参数校验同一套实现），因此 JSON
//! Schema 语义（type/required/properties/anyOf/pattern 等）与 agent loop 一致。失败时
//! 有限重试：`max_retries` 默认 2，超过后返回 `exhausted=true`；调用方应把子代理终态
//! 置为 `failed`，而不是把不完整结果当成功。

use std::collections::BTreeMap;

use indexmap::IndexMap;
use kiana_ai::types::JsonValue;
use kiana_ai::utils::validation::{
    collect_json_schema_errors, collect_json_schema_errors_with_root, format_validation_path,
};
use serde::{Deserialize, Serialize};

/// 校验模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum YieldValidationMode {
    /// 严格：schema 或类型不匹配都拒绝。
    #[default]
    Strict,
    /// 宽松：schema 错误记录 warning 后接受；类型变化覆盖旧值。
    Permissive,
}

/// yield 契约。
#[derive(Debug, Clone)]
pub struct YieldContract {
    /// 最终输出对象的 JSON Schema。
    pub output_schema: JsonValue,
    /// 每个 label 的 schema；为空表示不逐 label 校验。
    pub label_schemas: IndexMap<String, JsonValue>,
    /// 校验模式。
    pub mode: YieldValidationMode,
    /// 失败后的最大重试次数（有限值，默认 2）。
    pub max_retries: u32,
}

impl YieldContract {
    /// 只校验最终输出。
    pub fn new(output_schema: JsonValue) -> Self {
        Self {
            output_schema,
            label_schemas: IndexMap::new(),
            mode: YieldValidationMode::Strict,
            max_retries: 2,
        }
    }

    /// 设置 label schema。
    pub fn with_label_schema(mut self, label: impl Into<String>, schema: JsonValue) -> Self {
        self.label_schemas.insert(label.into(), schema);
        self
    }

    /// 设置校验模式。
    pub fn with_mode(mut self, mode: YieldValidationMode) -> Self {
        self.mode = mode;
        self
    }

    /// 设置最大重试次数。
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }
}

/// 一次提交的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct YieldOutcome {
    /// 装配后的输出。
    pub output: JsonValue,
    /// 宽松模式下的 warning（严格模式为空）。
    pub warnings: Vec<String>,
    /// 本次结果经历的失败次数（成功后的累计失败尝试）。
    pub attempts: u32,
}

/// yield 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YieldError {
    /// 结果不符合 schema；`exhausted` 为真时调用方应终止子代理。
    Invalid {
        /// 字段级错误。
        errors: Vec<String>,
        /// 已尝试次数。
        attempts: u32,
        /// 是否已经超过重试上限。
        exhausted: bool,
    },
    /// 提交了未知 label。
    UnknownLabel {
        /// label。
        label: String,
    },
    /// payload 形状不对。
    Malformed {
        /// 说明。
        message: String,
    },
}

impl YieldError {
    /// 是否还可以重试。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Invalid {
                exhausted: false,
                ..
            }
        )
    }
}

impl std::fmt::Display for YieldError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid {
                errors,
                attempts,
                exhausted,
            } => write!(
                formatter,
                "yield output failed schema validation after {attempts} attempt(s){}: {}",
                if *exhausted { " (exhausted)" } else { "" },
                errors.join("; ")
            ),
            Self::UnknownLabel { label } => write!(formatter, "unknown yield label `{label}`"),
            Self::Malformed { message } => write!(formatter, "malformed yield payload: {message}"),
        }
    }
}

impl std::error::Error for YieldError {}

/// 增量装配器。
#[derive(Debug)]
pub struct YieldAssembler {
    contract: YieldContract,
    values: IndexMap<String, JsonValue>,
    attempts: u32,
    warnings: Vec<String>,
    /// 模型是否调用过 `yield`（哪怕 payload 为空或校验失败）。runner 用它区分
    /// “从未提交”与“提交了空/不完整结果”：前者回退到 assistant 文本，后者必须在
    /// 结束时跑 `finish()` 并判失败。
    attempted: bool,
    /// 是否至少有一次提交被接受。畸形 payload 从未被接受时，runner 仍可回退到
    /// assistant 文本（否则模型接着给出的正常回答会被丢弃）。
    accepted: bool,
}

impl YieldAssembler {
    /// 新建。
    pub fn new(contract: YieldContract) -> Self {
        Self {
            contract,
            values: IndexMap::new(),
            attempts: 0,
            warnings: Vec::new(),
            attempted: false,
            accepted: false,
        }
    }

    /// 当前已装配的 label。
    pub fn values(&self) -> &IndexMap<String, JsonValue> {
        &self.values
    }

    /// 模型是否调用过 `yield`。
    pub fn attempted(&self) -> bool {
        self.attempted
    }

    /// 是否至少有一次提交被接受（装配状态被写入）。
    pub fn accepted(&self) -> bool {
        self.accepted
    }

    /// 失败次数是否已超过 `max_retries`。
    ///
    /// runner 用它区分「模型调用过 yield 但每次都校验失败」与「从未调用」：前者
    /// 必须走 `finish()` 判定失败，不能静默回退到 assistant 文本。
    pub fn attempts_exhausted(&self) -> bool {
        self.attempts > self.contract.max_retries
    }

    /// 提交一次 payload。
    ///
    /// 支持的形状：
    /// - `{ "label": "summary", "value": ... }`；
    /// - `{ "labels": { "summary": ..., "findings": [...] } }`；
    /// - 直接 `{ "summary": ..., "findings": [...] }`（对象里没有 `label`/`labels`
    ///   时按 label map 解释）。
    ///
    /// 契约本身声明了 `label`/`labels` 输出字段时，这两个键不当作信封标记，直接
    /// 按 label map 解释（见 [`contract_declares_label`]）。
    pub fn submit(&mut self, payload: &JsonValue) -> Result<YieldOutcome, YieldError> {
        // 即便 payload 畸形或校验失败，也算“模型尝试过 yield”：runner 的最终
        // `finish()` 不能把它当成“从未提交”而静默回退。
        self.attempted = true;
        // 畸形 payload 同样消耗一次重试预算：否则模型连续提交畸形/未知 label 后，
        // runner 会看到 attempts=0，把“尝试过 yield 但从未成功”误判成“从未调用过
        // yield”并静默回退到 assistant 文本。
        let assignments = match parse_payload(payload, &self.contract) {
            Ok(assignments) => assignments,
            Err(error) => {
                self.attempts = self.attempts.saturating_add(1);
                return Err(error);
            }
        };
        // 先在副本上装配：校验失败时不能把半成品提交进 `values`，否则重试会在
        // 上一次的错误内容上继续合并（数组重复 append、旧字段残留）。
        let mut values = self.values.clone();
        let mut warnings = self.warnings.clone();
        let mut label_errors = Vec::new();

        for (label, value) in assignments {
            if !self.contract.label_schemas.is_empty()
                && !self.contract.label_schemas.contains_key(&label)
            {
                self.attempts = self.attempts.saturating_add(1);
                return Err(YieldError::UnknownLabel { label });
            }
            // 输出 schema 明确禁止额外字段时，未声明的 label 不会出现在最终对象里，
            // 静默接受会让 `finish()` 报一条与模型提交无关的 schema 错误。
            if self.contract.label_schemas.is_empty()
                && rejects_unknown_properties(&self.contract.output_schema)
                && !root_declares_property(&self.contract.output_schema, &label)
            {
                self.attempts = self.attempts.saturating_add(1);
                return Err(YieldError::UnknownLabel { label });
            }
            let previous = values.get(&label).cloned();
            let mut type_errors = Vec::new();
            let merged = merge_value(previous.as_ref(), value, &mut type_errors);
            match self.contract.mode {
                YieldValidationMode::Strict => label_errors.extend(type_errors),
                YieldValidationMode::Permissive => {
                    warnings.extend(
                        type_errors
                            .into_iter()
                            .map(|error| format!("{error} (replaced in permissive mode)")),
                    );
                }
            }
            if let Some(schema) = self.contract.label_schemas.get(&label) {
                let errors = schema_errors(schema, &merged);
                if !errors.is_empty() {
                    label_errors.extend(
                        errors
                            .into_iter()
                            .map(|error| format!("{label}.{}", format_validation_path(&error))),
                    );
                }
            } else if let Some(schema) =
                output_property_schema(&self.contract.output_schema, &label)
            {
                // 输出 schema 还没必齐时不做整体校验，但**已提交的字段**必须先符合各自
                // 的子 schema：否则错误类型的值会被写进装配状态，Strict 的“禁止类型
                // 变化”会让后续正确的提交永远修不回来。子 schema 里的相对 `$ref`
                // 要用输出契约根解析（`#/$defs/...` 通常只定义在根上）。
                let errors = schema_errors_with_root(&self.contract.output_schema, schema, &merged);
                if !errors.is_empty() {
                    label_errors.extend(
                        errors
                            .into_iter()
                            .map(|error| format!("{label}.{}", format_validation_path(&error))),
                    );
                }
            }
            values.insert(label, merged);
        }

        let output = assembled_output_from(&values, &self.contract.output_schema);
        let mut errors = label_errors;
        // 增量装配允许「先给 findings、后给 summary」；在 required 字段还没到齐时
        // 不把“缺字段”当成错误。`finish()` 才做最终裁决。
        if schema_is_complete(&self.contract.output_schema, &output) {
            errors.extend(
                schema_errors(&self.contract.output_schema, &output)
                    .into_iter()
                    .map(|error| format_validation_path(&error)),
            );
        }
        self.finish_with_values(errors, output, values, warnings)
    }

    /// 结束装配并做最终 schema 校验。
    ///
    /// 与 [`Self::submit`] 不同，这里即使 `required` 字段缺失也会报错（除非
    /// `Permissive`）。失败会占用重试次数并在超过 `max_retries` 后返回
    /// `exhausted=true`。
    pub fn finish(&mut self) -> Result<YieldOutcome, YieldError> {
        let output = assembled_output_from(&self.values, &self.contract.output_schema);
        let errors = schema_errors(&self.contract.output_schema, &output)
            .into_iter()
            .map(|error| format_validation_path(&error))
            .collect::<Vec<_>>();
        let values = self.values.clone();
        let warnings = self.warnings.clone();
        self.finish_with_values(errors, output, values, warnings)
    }

    /// 校验一次候选装配结果；只有成功时才把它提交为新的装配状态。
    fn finish_with_values(
        &mut self,
        errors: Vec<String>,
        output: JsonValue,
        values: IndexMap<String, JsonValue>,
        mut warnings: Vec<String>,
    ) -> Result<YieldOutcome, YieldError> {
        if !errors.is_empty() {
            self.attempts = self.attempts.saturating_add(1);
            match self.contract.mode {
                YieldValidationMode::Strict => {
                    let exhausted = self.attempts > self.contract.max_retries;
                    // 失败不提交 values/warnings：重试从上一次成功状态开始。
                    return Err(YieldError::Invalid {
                        errors,
                        attempts: self.attempts,
                        exhausted,
                    });
                }
                YieldValidationMode::Permissive => {
                    warnings.extend(
                        errors
                            .iter()
                            .map(|error| format!("yield schema warning: {error}")),
                    );
                }
            }
        } else {
            self.attempts = 0;
        }

        self.values = values;
        self.warnings = warnings;
        self.accepted = true;
        Ok(YieldOutcome {
            output,
            warnings: self.warnings.clone(),
            attempts: self.attempts,
        })
    }

    /// 当前装配输出（尚未通过 schema 校验也可能返回）。
    pub fn output(&self) -> JsonValue {
        assembled_output_from(&self.values, &self.contract.output_schema)
    }
}

/// 把已装配的 label 组合成输出对象。
///
/// 只有一个 `result`/`output` label 且契约没有把它声明成对象属性时，直接返回该值，
/// 避免给调用方多包一层；其余情况返回 label map。契约本身是对象（或把该名字声明
/// 为属性/required）时不能拆包，否则合法的对象输出永远无法通过校验。
fn assembled_output_from(values: &IndexMap<String, JsonValue>, schema: &JsonValue) -> JsonValue {
    if values.len() == 1 {
        for label in ["result", "output"] {
            if let Some(value) = values.get(label)
                && !root_declares_property(schema, label)
                && !root_has_object_contract(schema)
            {
                return value.clone();
            }
        }
    }
    JsonValue::Object(
        values
            .iter()
            .map(|(label, value)| (label.clone(), value.clone()))
            .collect(),
    )
}

/// schema 的根对象是否声明了 `name`（properties 或 required）。
fn root_declares_property(schema: &JsonValue, name: &str) -> bool {
    let in_properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|properties| properties.contains_key(name));
    let in_required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)));
    in_properties || in_required
}

/// schema 是否要求根值必须是对象。
fn root_is_object(schema: &JsonValue) -> bool {
    schema.get("type").and_then(JsonValue::as_str) == Some("object")
}

/// schema 是否声明了对象形状（`type: object`、`properties`、`required` 或组合器）。
///
/// 只检查 `type` 会漏掉省略 `type` 但声明了属性的契约（JSON Schema 允许），
/// 这种契约下把单 label 拆包会让合法输出永远无法通过校验。
fn root_has_object_contract(schema: &JsonValue) -> bool {
    root_is_object(schema)
        || schema.get("properties").is_some()
        || schema.get("required").is_some()
        || root_has_combinator(schema)
}

/// 根 schema 是否禁止未声明的属性（`additionalProperties: false`）。
fn rejects_unknown_properties(schema: &JsonValue) -> bool {
    schema
        .get("additionalProperties")
        .and_then(JsonValue::as_bool)
        == Some(false)
}

/// 根 schema 是否用组合器（`allOf`/`anyOf`/`oneOf`/`$ref`）声明契约。
///
/// 这些写法的对象要求无法从顶层 `type`/`properties` 直接看出：单 label 拆包会
/// 让组合器 schema 校验一个非对象值。无法确定时保持包装（fail closed）。
fn root_has_combinator(schema: &JsonValue) -> bool {
    ["allOf", "anyOf", "oneOf", "$ref"]
        .iter()
        .any(|key| schema.get(*key).is_some())
}

/// 根 schema 里 `label` 属性的子 schema（没有声明时返回 `None`）。
///
/// 不要求 `type: object`：JSON Schema 允许只写 `properties`，这种契约同样要为
/// 已提交字段做增量校验。
fn output_property_schema<'a>(schema: &'a JsonValue, label: &str) -> Option<&'a JsonValue> {
    schema
        .get("properties")
        .and_then(JsonValue::as_object)?
        .get(label)
}

/// 契约是否把 `name` 当作输出字段（而不是装配信封的标记字段）。
///
/// `label`/`labels` 是信封标记，但如果输出 schema（或其 label schema）本身就
/// 声明了同名字段，模型提交的 `{"label": ...}` / `{"labels": ...}` 是输出对象
/// 本身，不能再按信封拆解，否则契约字段会被静默改写或报 malformed。
fn contract_declares_label(contract: &YieldContract, name: &str) -> bool {
    if contract.label_schemas.contains_key(name) {
        return true;
    }
    let schema = &contract.output_schema;
    let in_properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|properties| properties.contains_key(name));
    let in_required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)));
    in_properties || in_required
}

fn parse_payload(
    payload: &JsonValue,
    contract: &YieldContract,
) -> Result<Vec<(String, JsonValue)>, YieldError> {
    let object = payload.as_object().ok_or_else(|| YieldError::Malformed {
        message: "yield payload must be an object".to_string(),
    })?;
    // 契约声明了同名字段时优先按直接 label map 解释。
    if let Some(labels) = object.get("labels")
        && !contract_declares_label(contract, "labels")
    {
        let labels = labels.as_object().ok_or_else(|| YieldError::Malformed {
            message: "`labels` must be an object".to_string(),
        })?;
        return Ok(labels
            .iter()
            .map(|(label, value)| (label.clone(), value.clone()))
            .collect());
    }
    if let Some(label) = object.get("label")
        && !contract_declares_label(contract, "label")
    {
        let label = label.as_str().ok_or_else(|| YieldError::Malformed {
            message: "`label` must be a string".to_string(),
        })?;
        let value = object
            .get("value")
            .cloned()
            .ok_or_else(|| YieldError::Malformed {
                message: "`label` form requires a `value` field".to_string(),
            })?;
        if label.trim().is_empty() {
            return Err(YieldError::Malformed {
                message: "`label` cannot be empty".to_string(),
            });
        }
        return Ok(vec![(label.to_string(), value)]);
    }
    Ok(object
        .iter()
        .map(|(label, value)| (label.clone(), value.clone()))
        .collect())
}

fn merge_value(
    previous: Option<&JsonValue>,
    value: JsonValue,
    type_errors: &mut Vec<String>,
) -> JsonValue {
    match (previous, &value) {
        (Some(JsonValue::Array(existing)), JsonValue::Array(new_items)) => {
            let mut merged = existing.clone();
            merged.extend(new_items.iter().cloned());
            JsonValue::Array(merged)
        }
        (Some(existing), new_value) if same_kind(existing, new_value) => new_value.clone(),
        (Some(existing), new_value) => {
            // 类型变化：数组 ↔ 标量/对象。严格模式把它当错误，宽松模式覆盖。
            type_errors.push(format!(
                "yield label changed type from {} to {}",
                kind_name(existing),
                kind_name(new_value)
            ));
            new_value.clone()
        }
        (None, new_value) => new_value.clone(),
    }
}

fn same_kind(left: &JsonValue, right: &JsonValue) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

fn kind_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn schema_errors(
    schema: &JsonValue,
    value: &JsonValue,
) -> Vec<kiana_ai::utils::validation::ValidationError> {
    if schema.is_null() {
        return Vec::new();
    }
    collect_json_schema_errors(schema, value)
}

/// 用输出契约根解析子 schema 的相对 `$ref` 后再校验。
fn schema_errors_with_root(
    root: &JsonValue,
    schema: &JsonValue,
    value: &JsonValue,
) -> Vec<kiana_ai::utils::validation::ValidationError> {
    if schema.is_null() {
        return Vec::new();
    }
    collect_json_schema_errors_with_root(root, schema, value)
}

/// output 是否已经包含 schema 要求的全部属性。
///
/// 用于区分「增量装配中的半成品」与「提交了完整结果但字段不合法」：前者不报错，
/// 后者按正常校验处理。`allOf` 里的 `required` 同样生效；`$ref`/`anyOf`/`oneOf`
/// 让 required 集合不可知，按“不完整”处理（跳过整体校验，留给 `finish()`）。
fn schema_is_complete(schema: &JsonValue, output: &JsonValue) -> bool {
    let Some(required) = schema_required_properties(schema) else {
        return false;
    };
    let Some(object) = output.as_object() else {
        return true;
    };
    required
        .iter()
        .all(|property| object.contains_key(property.as_str()))
}

/// 顶层与 `allOf` 递归收集的 `required` 属性名；不可知时返回 `None`。
fn schema_required_properties(schema: &JsonValue) -> Option<Vec<String>> {
    if !schema.is_object() {
        return Some(Vec::new());
    }
    if ["$ref", "anyOf", "oneOf"]
        .iter()
        .any(|key| schema.get(*key).is_some())
    {
        return None;
    }
    let mut required = Vec::new();
    if let Some(properties) = schema.get("required").and_then(JsonValue::as_array) {
        required.extend(
            properties
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_string),
        );
    }
    if let Some(all_of) = schema.get("allOf").and_then(JsonValue::as_array) {
        for item in all_of {
            required.extend(schema_required_properties(item)?);
        }
    }
    Some(required)
}

/// label 增量装配的纯函数版本（不维护 schema），供调用方做轻量合并。
pub fn merge_labels(
    existing: &mut BTreeMap<String, JsonValue>,
    incoming: impl IntoIterator<Item = (String, JsonValue)>,
) {
    for (label, value) in incoming {
        let appending = matches!(existing.get(&label), Some(JsonValue::Array(_)))
            && matches!(&value, JsonValue::Array(_));
        if appending {
            if let (Some(JsonValue::Array(current)), JsonValue::Array(mut items)) =
                (existing.get_mut(&label), value)
            {
                current.append(&mut items);
            }
        } else {
            existing.insert(label, value);
        }
    }
}

/// 构造一个可直接注册到子代理工具表的 `yield` 工具。
///
/// 工具的输入是 [`YieldAssembler::submit`] 接受的三种 payload 形状；成功时把装配后的
/// 输出同时放进文本内容、`details` 与 `structuredContent`，失败时返回
/// [`kiana_agent_core::types::AgentToolError`]（agent loop 会把它转成 failed tool
/// result）。每次工具调用都会复用同一个装配器，因此 label 增量跨调用生效。
pub fn create_yield_tool(contract: YieldContract) -> kiana_agent_core::types::AgentTool {
    create_yield_tool_with_state(contract).0
}

/// 与 [`create_yield_tool`] 相同，同时返回共享的装配器。
///
/// 子代理 runner 在模型循环结束后读取装配结果作为终态输出；工具本身仍按每次
/// `submit` 的增量结果回复模型。
pub fn create_yield_tool_with_state(
    contract: YieldContract,
) -> (
    kiana_agent_core::types::AgentTool,
    std::sync::Arc<std::sync::Mutex<YieldAssembler>>,
) {
    use std::sync::{Arc, Mutex};

    use kiana_agent_core::types::{
        AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
        BoxFuture,
    };
    use kiana_ai::types::{TextContent, TextKind, UserContentBlock};

    struct YieldExecute {
        assembler: Arc<Mutex<YieldAssembler>>,
    }

    impl AgentToolExecute for YieldExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async move {
                let mut assembler = self
                    .assembler
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match assembler.submit(&params) {
                    Ok(outcome) => {
                        // 模型可见的 JSON 文本必须走 kiana-json：数字格式与键序要复刻
                        // `JSON.stringify`（1e21、整数样键排序），不能直接用 serde_json。
                        let text = kiana_json::stringify(&kiana_json::normalize_numbers(
                            outcome.output.clone(),
                        ));
                        Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent {
                                kind_text: TextKind::Text,
                                text,
                                text_signature: None,
                            })],
                            details: outcome.output.clone(),
                            structured_content: Some(outcome.output),
                            usage: None,
                            is_error: None,
                            terminate: None,
                        })
                    }
                    Err(error) => Err(AgentToolError {
                        message: error.to_string(),
                        name: Some("YieldError".to_string()),
                        stack: None,
                    }),
                }
            })
        }
    }

    let output_schema = contract.output_schema.clone();
    let assembler = Arc::new(Mutex::new(YieldAssembler::new(contract)));
    let tool = AgentTool {
        name: "yield".to_string(),
        label: "Yield".to_string(),
        description: "Submit the final structured result for this subagent. Labels may be submitted incrementally; arrays append and scalars replace.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": true
        }),
        constrained_sampling: None,
        output_schema: Some(output_schema),
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(YieldExecute {
            assembler: Arc::clone(&assembler),
        }),
    };
    (tool, assembler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strict_contract() -> YieldContract {
        YieldContract::new(json!({
            "type": "object",
            "required": ["summary", "findings"],
            "properties": {
                "summary": {"type": "string"},
                "findings": {"type": "array", "items": {"type": "string"}}
            }
        }))
    }

    #[test]
    fn arrays_append_and_scalars_replace() {
        let mut assembler = YieldAssembler::new(
            YieldContract::new(json!({"type": "object"}))
                .with_mode(YieldValidationMode::Permissive),
        );
        assembler
            .submit(&json!({"labels": {"findings": ["a"], "summary": "first"}}))
            .expect("first");
        let outcome = assembler
            .submit(&json!({"labels": {"findings": ["b"], "summary": "second"}}))
            .expect("second");
        assert_eq!(outcome.output["findings"], json!(["a", "b"]));
        assert_eq!(outcome.output["summary"], json!("second"));
    }

    #[test]
    fn label_form_is_supported() {
        let mut assembler = YieldAssembler::new(strict_contract());
        assembler
            .submit(&json!({"label": "findings", "value": ["x"]}))
            .expect("label form");
        let outcome = assembler
            .submit(&json!({"label": "summary", "value": "done"}))
            .expect("label form");
        assert_eq!(outcome.output["findings"], json!(["x"]));
        assert_eq!(outcome.output["summary"], json!("done"));
    }

    #[test]
    fn output_projection_matches_finish_for_a_single_result_label() {
        // runner 的取消/失败路径用 `output()`，必须与成功路径的 `finish()` 同形。
        let mut assembler = YieldAssembler::new(
            YieldContract::new(json!({"type": "string"}))
                .with_mode(YieldValidationMode::Permissive),
        );
        assembler
            .submit(&json!({"label": "result", "value": "done"}))
            .expect("submit");
        assert_eq!(assembler.output(), json!("done"));
        assert_eq!(assembler.finish().expect("finish").output, json!("done"));
    }

    #[test]
    fn object_contract_keeps_a_single_result_label_wrapped() {
        // 契约的根是对象且声明了 `result` 属性：不能把单 label 拆成标量。
        let contract = YieldContract::new(json!({
            "type": "object",
            "required": ["result"],
            "properties": {"result": {"type": "string"}}
        }));
        let mut assembler = YieldAssembler::new(contract);
        let outcome = assembler
            .submit(&json!({"labels": {"result": "done"}}))
            .expect("object output");
        assert_eq!(outcome.output, json!({"result": "done"}));
        assembler.finish().expect("complete object output");
    }

    #[test]
    fn trivial_object_contract_does_not_unwrap_a_single_result_label() {
        // `{"type":"object"}` 要求对象；拆成标量会让合法输出永远校验失败。
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({"type": "object"})));
        let outcome = assembler
            .submit(&json!({"label": "result", "value": "done"}))
            .expect("object output");
        assert_eq!(outcome.output, json!({"result": "done"}));
        assert!(assembler.finish().is_ok());
    }

    #[test]
    fn schema_declared_label_field_is_output_not_envelope() {
        // 契约本身有 `label`/`value` 字段：模型提交的直接输出不能被拆成信封。
        let contract = YieldContract::new(json!({
            "type": "object",
            "required": ["label", "value"],
            "properties": {"label": {"type": "string"}, "value": {"type": "string"}}
        }));
        let mut assembler = YieldAssembler::new(contract);
        let outcome = assembler
            .submit(&json!({"label": "kind", "value": "record"}))
            .expect("direct output");
        assert_eq!(outcome.output, json!({"label": "kind", "value": "record"}));
        assembler.finish().expect("complete output");
    }

    #[test]
    fn schema_declared_labels_field_is_output_not_envelope() {
        // `labels` 不是对象；若仍按信封解析会报 malformed。
        let contract = YieldContract::new(json!({
            "type": "object",
            "required": ["labels"],
            "properties": {"labels": {"type": "array", "items": {"type": "string"}}}
        }));
        let mut assembler = YieldAssembler::new(contract);
        let outcome = assembler
            .submit(&json!({"labels": ["a", "b"]}))
            .expect("direct output");
        assert_eq!(outcome.output, json!({"labels": ["a", "b"]}));
    }

    #[test]
    fn label_schema_named_like_the_envelope_still_accepts_envelopes() {
        // label schema 声明了 `label`：它是契约字段名，信封合并后输出键仍是
        // `label`，但提交形状 `{"labels": {...}}` 里的 `labels` 不是契约字段。
        let contract = YieldContract::new(json!({"type": "object"}))
            .with_label_schema("label", json!({"type": "string"}));
        let mut assembler = YieldAssembler::new(contract);
        let outcome = assembler
            .submit(&json!({"labels": {"label": "done"}}))
            .expect("envelope form");
        assert_eq!(outcome.output, json!({"label": "done"}));
    }

    #[test]
    fn invalid_incremental_values_are_not_committed_before_the_contract_is_complete() {
        let mut assembler = YieldAssembler::new(strict_contract());
        // findings 还没到齐（整体校验被跳过），但 summary 的类型错误必须当场拒绝，
        // 不能先写进装配状态。
        let error = assembler
            .submit(&json!({"labels": {"summary": 42}}))
            .unwrap_err();
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
        // 没有残留的错误类型值：正确类型补齐后仍能成功。
        let outcome = assembler
            .submit(&json!({"labels": {"summary": "done", "findings": ["a"]}}))
            .expect("repairing the value must succeed");
        assert_eq!(
            outcome.output,
            json!({"summary": "done", "findings": ["a"]})
        );
        assembler.finish().expect("complete output");
    }

    #[test]
    fn combinator_root_schemas_keep_a_single_label_wrapped() {
        // allOf 里的对象契约不能从顶层 type/properties 看出：单 label 不能被拆成
        // 内部值，否则组合器 schema 校验的是一个非对象。
        let contract = YieldContract::new(json!({
            "allOf": [{
                "type": "object",
                "required": ["result"],
                "properties": {
                    "result": {
                        "type": "object",
                        "required": ["x"],
                        "properties": {"x": {"type": "string"}}
                    }
                }
            }]
        }));
        let mut assembler = YieldAssembler::new(contract);
        let outcome = assembler
            .submit(&json!({"label": "result", "value": {"x": "y"}}))
            .expect("allOf object output");
        assert_eq!(outcome.output, json!({"result": {"x": "y"}}));
        assembler.finish().expect("complete allOf output");
    }

    #[test]
    fn strict_mode_rejects_missing_required_fields_at_finish() {
        let mut assembler = YieldAssembler::new(strict_contract());
        assembler
            .submit(&json!({"labels": {"summary": "only summary"}}))
            .expect("partial assembly is accepted");
        let error = assembler.finish().unwrap_err();
        match error {
            YieldError::Invalid {
                errors,
                attempts,
                exhausted,
            } => {
                assert_eq!(attempts, 1);
                assert!(!exhausted);
                assert!(
                    errors.iter().any(|error| error.contains("findings")),
                    "{errors:?}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn strict_retries_are_finite_and_then_exhaust() {
        let mut assembler = YieldAssembler::new(strict_contract().with_max_retries(2));
        assembler
            .submit(&json!({"labels": {"summary": "only summary"}}))
            .expect("partial");
        for attempt in 1..=2 {
            let error = assembler.finish().unwrap_err();
            assert!(error.is_retryable(), "attempt {attempt}");
        }
        let error = assembler.finish().unwrap_err();
        assert!(!error.is_retryable(), "third attempt exhausts");
    }

    #[test]
    fn permissive_mode_accepts_schema_errors_with_warnings() {
        let mut assembler =
            YieldAssembler::new(strict_contract().with_mode(YieldValidationMode::Permissive));
        assembler
            .submit(&json!({"labels": {"summary": "only summary"}}))
            .expect("partial");
        let outcome = assembler.finish().expect("permissive accepts");
        assert!(!outcome.warnings.is_empty());
    }

    #[test]
    fn unknown_label_is_rejected_when_label_schemas_are_declared() {
        let mut assembler = YieldAssembler::new(
            YieldContract::new(json!({"type": "object"}))
                .with_label_schema("summary", json!({"type": "string"})),
        );
        let error = assembler
            .submit(&json!({"labels": {"other": "x"}}))
            .unwrap_err();
        assert!(matches!(error, YieldError::UnknownLabel { .. }));
    }

    #[test]
    fn single_result_label_is_unwrapped() {
        let mut assembler = YieldAssembler::new(
            YieldContract::new(json!({"type": "string"}))
                .with_mode(YieldValidationMode::Permissive),
        );
        let outcome = assembler
            .submit(&json!({"label": "result", "value": "done"}))
            .expect("submit");
        assert_eq!(outcome.output, json!("done"));
    }

    #[test]
    fn strict_mode_rejects_label_type_changes() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({"type": "object"})));
        assembler
            .submit(&json!({"labels": {"findings": ["a"]}}))
            .expect("array first");
        let error = assembler
            .submit(&json!({"labels": {"findings": "not an array"}}))
            .unwrap_err();
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn malformed_payloads_error() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({"type": "object"})));
        assert!(matches!(
            assembler.submit(&json!("not an object")),
            Err(YieldError::Malformed { .. })
        ));
        assert!(matches!(
            assembler.submit(&json!({"label": "x"})),
            Err(YieldError::Malformed { .. })
        ));
    }

    #[tokio::test]
    async fn yield_tool_executes_incrementally_and_reports_errors() {
        let tool = create_yield_tool(strict_contract());
        let first = tool
            .execute
            .execute("call-1", json!({"labels": {"findings": ["a"]}}), None, None)
            .await
            .expect("partial yield accepted");
        assert!(first.structured_content.is_some());
        let second = tool
            .execute
            .execute(
                "call-2",
                json!({"labels": {"findings": ["b"], "summary": "done"}}),
                None,
                None,
            )
            .await
            .expect("complete yield accepted");
        let output = second.structured_content.expect("structured output");
        assert_eq!(output["findings"], json!(["a", "b"]));
        assert_eq!(output["summary"], json!("done"));

        let error = tool
            .execute
            .execute("call-3", json!("not an object"), None, None)
            .await
            .unwrap_err();
        assert!(error.message.contains("malformed"), "{}", error.message);
    }

    #[test]
    fn failed_submission_does_not_mutate_assembler_state() {
        let contract = YieldContract::new(json!({
            "type": "object",
            "required": ["summary"],
            "properties": {"summary": {"type": "string"}}
        }))
        .with_max_retries(5);
        let mut assembler = YieldAssembler::new(contract);
        assembler
            .submit(&json!({"labels": {"summary": "ok"}}))
            .expect("first");
        // summary 类型错误：这次提交失败，错误内容不能被保存下来参与后续合并。
        let error = assembler
            .submit(&json!({"labels": {"summary": 5}}))
            .unwrap_err();
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
        let outcome = assembler
            .submit(&json!({"labels": {"summary": "real"}}))
            .expect("retry after failure");
        assert_eq!(outcome.output["summary"], json!("real"));
    }

    #[test]
    fn finish_rejects_an_incomplete_assembly() {
        let mut assembler = YieldAssembler::new(strict_contract());
        assembler
            .submit(&json!({"labels": {"findings": ["a"]}}))
            .expect("partial");
        let error = assembler.finish().expect_err("missing summary");
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn yield_tool_text_uses_js_json_formatting() {
        let tool = create_yield_tool(YieldContract::new(json!({"type": "object"})));
        let result = tool
            .execute
            .execute(
                "call-1",
                json!({"x": 1e21_f64, "10": "ten", "2": "two"}),
                None,
                None,
            )
            .await
            .expect("yield");
        let text = match &result.content[0] {
            kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
            other => panic!("unexpected content: {other:?}"),
        };
        assert!(text.contains("1e+21"), "{text}");
        let two = text.find("\"2\"").expect("numeric key 2");
        let ten = text.find("\"10\"").expect("numeric key 10");
        assert!(two < ten, "integer-like keys must sort numerically: {text}");
    }

    #[test]
    fn property_schemas_validate_without_an_explicit_object_type() {
        // JSON Schema 允许省略 `type: object`，只看 `properties`；半成品阶段
        // （findings 未提交）也要按子 schema 校验已提交字段。
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({
            "properties": {
                "summary": {"type": "string"},
                "findings": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["summary", "findings"]
        })));
        let error = assembler
            .submit(&json!({"label": "summary", "value": 5}))
            .expect_err("wrong type must be rejected even before the contract is complete");
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn properties_keep_a_single_result_label_wrapped_without_type_object() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({
            "properties": {"other": {"type": "string"}}
        })));
        let outcome = assembler
            .submit(&json!({"label": "result", "value": "done"}))
            .expect("submit");
        assert_eq!(outcome.output, json!({"result": "done"}));
    }

    #[test]
    fn all_of_required_properties_make_incremental_submissions_incomplete() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({
            "allOf": [
                {"required": ["summary"]},
                {"required": ["findings"]}
            ],
            "properties": {
                "summary": {"type": "string"},
                "findings": {"type": "array", "items": {"type": "string"}}
            }
        })));
        // findings 还没到：半成品提交不能被当成“完整但不合法”。
        assembler
            .submit(&json!({"label": "summary", "value": "done"}))
            .expect("partial submission");
        let error = assembler.finish().expect_err("findings is still missing");
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn combinator_roots_skip_whole_schema_validation_until_finish() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({
            "anyOf": [
                {"required": ["a"]},
                {"required": ["b"]}
            ],
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"}
            }
        })));
        assembler
            .submit(&json!({"label": "a", "value": "done"}))
            .expect("unknown completeness must not turn a partial submit into an error");
        assembler.finish().expect("anyOf branch satisfied");
    }

    #[test]
    fn property_schemas_resolve_refs_against_the_output_root() {
        let contract = || {
            YieldContract::new(json!({
                "type": "object",
                "$defs": {"Finding": {"type": "string"}},
                "properties": {"finding": {"$ref": "#/$defs/Finding"}},
                "required": ["finding"]
            }))
        };
        let mut assembler = YieldAssembler::new(contract());
        let outcome = assembler
            .submit(&json!({"label": "finding", "value": "ok"}))
            .expect("a root-relative $ref must resolve during incremental validation");
        assert_eq!(outcome.output, json!({"finding": "ok"}));

        let mut assembler = YieldAssembler::new(contract());
        let error = assembler
            .submit(&json!({"label": "finding", "value": 5}))
            .expect_err("the resolved $ref must still enforce its type");
        assert!(matches!(error, YieldError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn additional_properties_false_rejects_undeclared_labels() {
        let mut assembler = YieldAssembler::new(YieldContract::new(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"]
        })));
        let error = assembler
            .submit(&json!({"label": "bogus", "value": 1}))
            .expect_err("undeclared label cannot appear in the output");
        assert!(
            matches!(error, YieldError::UnknownLabel { .. }),
            "{error:?}"
        );
        assembler
            .submit(&json!({"label": "summary", "value": "ok"}))
            .expect("declared label is accepted");
    }

    #[test]
    fn malformed_and_unknown_label_submissions_consume_retries() {
        let mut assembler = YieldAssembler::new(
            YieldContract::new(json!({"type": "object"}))
                .with_label_schema("summary", json!({"type": "string"}))
                .with_max_retries(1),
        );
        assert!(!assembler.attempts_exhausted());
        assert!(matches!(
            assembler.submit(&json!({"label": 5})).unwrap_err(),
            YieldError::Malformed { .. }
        ));
        assert!(!assembler.attempts_exhausted());
        assert!(matches!(
            assembler
                .submit(&json!({"label": "bogus", "value": 1}))
                .unwrap_err(),
            YieldError::UnknownLabel { .. }
        ));
        assert!(
            assembler.attempts_exhausted(),
            "malformed/unknown-label attempts must not leave the retry budget untouched"
        );
    }

    #[test]
    fn attempts_exhausted_tracks_strict_validation_failures() {
        let mut assembler = YieldAssembler::new(strict_contract().with_max_retries(1));
        assert!(!assembler.attempts_exhausted());
        let first = assembler
            .submit(&json!({"label": "summary", "value": 1}))
            .expect_err("wrong type");
        assert!(matches!(
            first,
            YieldError::Invalid {
                exhausted: false,
                ..
            }
        ));
        assert!(!assembler.attempts_exhausted());
        let second = assembler
            .submit(&json!({"label": "summary", "value": 2}))
            .expect_err("wrong type again");
        assert!(matches!(
            second,
            YieldError::Invalid {
                exhausted: true,
                ..
            }
        ));
        assert!(assembler.attempts_exhausted());
    }
}
