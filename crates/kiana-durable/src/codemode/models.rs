//! codemode/models.ts：code mode 脚本的 `models.*`。
//!
//! 目录函数是宿主全局函数，像任何 global 一样入 journal。`models.classify()` 与
//! `models.generateImages()` 是付费效果，所以各自跑成一个内部工具（`callers: ["tools"]`、
//! 保留名、对脚本的工具列表隐藏）：带自己 task、live 行、abort、用量记账与 replay 策略的
//! 嵌套调用。code mode 工具把脚本对该 global 的调用改写成对该内部工具的调用。
//!
//! # 为什么目录函数与付费调用分开
//! 三个目录函数不发起模型推理调用（`getModelsOfType`/`getModelOfType` 只读宿主目录；
//! `getAvailableOfType` 还会解析 auth，可能等待 I/O），因此由 `codemode.rs::call_global()`
//! 直接调用 [`catalog_call`] 回答，结果连同调用名与参数写进 journal，重放时不再访问宿主
//! 目录；它们不产生嵌套 task，也不占 [`MAX_CONCURRENT_MODEL_CALLS`] 的槽位。
//! `classify()`/`generateImages()` 会产生真实 API 调用与用量，所以包装成内部工具，交给
//! Harness 的嵌套调用机制去获得独立 task、调用行、取消传播与按工具名记账。
//!
//! # 调用链
//! 1. 脚本按 spread 方式调用 `models.classify(model, context)`。
//! 2. `codemode.rs::as_tool_call()` 用 [`model_call`] 把 global 改写成工具调用，参数数组的
//!    前两项变成 `{ model, context }`，多余参数被丢弃。
//! 3. `call_tool()` 为内部工具取并发槽位（[`MAX_CONCURRENT_MODEL_CALLS`]），再执行
//!    [`create_model_tools`] 注册的闭包。
//! 4. 闭包用 [`resolve_model`] 在宿主目录里解析 provider/id，用
//!    [`check_classifier_context`]/[`check_images_context`] 预检参数形状，然后调用
//!    kiana-ai 的 `Models::classify`/`generate_images`。
//! 5. 结果整体序列化成 `ToolExecutionResult.structured_output`；`stopReason` 不是 `"stop"`
//!    时 `is_error = Some(true)`，但注册项声明了 `structured_output_schema`，脚本的
//!    promise 仍会以该对象 resolve（脚本自己检查 `stopReason`/`errorMessage`），与 coding
//!    agent 的处理一致（测试
//!    `resolves_a_model_call_that_failed_with_its_result_as_the_coding_agent_does` 锁定）。
//!
//! # 安全边界
//! 脚本不能传 baseUrl 或 headers：它只能给 provider + id，[`resolve_model`] 只在宿主的
//! 模型目录里查找；目录条目经 [`to_model_info`] 返回前会删掉 `headers`（配置的头可能带
//! 凭证，测试 `reads_the_model_catalog_without_credentials_in_headers` 锁定）。
//!
//! # 失败模式
//! - 参数形状错误在发请求前变成 `BoxError`，脚本的 promise 以该消息 reject（测试
//!   `rejects_a_bad_classifier_context_with_the_expected_shape_before_any_request`）；
//! - provider 调用本身不返回 `Err`：kiana-ai 的 `classify`/`generate_images` 把失败折叠进
//!   `stopReason`（`"error"`/`"aborted"`）随结果对象 resolve；闭包内能导致 promise reject
//!   的只有模型解析、上下文预检或结果序列化（`serde_json::to_value`）失败；
//! - provider 返回 `stopReason: "error"` 不是 `Err`：它随结果对象 resolve，由脚本判断；
//! - 取消：请求收到外层 abort token 与原因（`ai_abort_reason` 负责映射），由 kiana-ai 的
//!   请求层处理；脚本结束时仍在等槽位的调用不会发起请求（见 `codemode.rs`）。

use std::sync::{Arc, LazyLock};

use kiana_ai::models::{Models, ModelsClassifierOptions, ModelsImagesOptions};
use kiana_ai::types::{
    ClassifierContext, ClassifierStopReason, ImagesContext, ImagesStopReason, Model, ModelType,
    TextContent, TextKind, Usage, UserContentBlock,
};
use kiana_chord::context::Context;
use serde_json::{Value as JsonValue, json};

use crate::harness::define::define_tool;
use crate::harness::types::{ToolCaller, ToolExecuteFn, ToolExecutionResult, ToolRegistration};
use crate::types::BoxError;

/// code mode 内部工具的保留前缀；它们从不出现在脚本或模型的列表里。
///
/// 执行器用它做两件事：把 start 数据里的内部工具标成 hidden（沙箱不声明它们，
/// `ALL_TOOLS` 与工具搜索也看不到），以及把脚本对 `models.*` 的调用识别为需要并发槽位的
/// 付费调用。调用行里显示的名字是去掉该前缀后的形式（例如 `models.classify`）。
pub const CODEMODE_INTERNAL_PREFIX: &str = "kiana.codemode.";
/// `models.classify()` 的内部工具名。
///
/// 名字带保留前缀，且注册为 `callers: [Tools]`：模型不能直接选中它，脚本只能通过
/// `models.classify()` 全局函数到达它。
pub const CLASSIFY_TOOL: &str = "kiana.codemode.models.classify";
/// `models.generateImages()` 的内部工具名。
///
/// 与 [`CLASSIFY_TOOL`] 相同，模型与脚本的工具列表都看不到它。
pub const GENERATE_IMAGES_TOOL: &str = "kiana.codemode.models.generateImages";

/// 脚本调用的 global，按内部工具索引。
///
/// 表的顺序就是 `codemode.rs` 构造 start 数据时注册这些 globals 的顺序；`as_tool_call()`
/// 通过 [`model_call`] 做同一个映射。两张表必须与 [`create_model_tools`] 保持一致：
/// 改写出的内部工具名最终由那里注册的闭包执行。
pub fn model_calls() -> &'static [(&'static str, &'static str)] {
    &[
        ("models.classify", CLASSIFY_TOOL),
        ("models.generateImages", GENERATE_IMAGES_TOOL),
    ]
}

/// 脚本调用的 global 对应的内部工具名。
///
/// 只做精确匹配（大小写敏感）；不在 [`model_calls`] 里的名字返回 `None`。
/// `codemode.rs` 用它决定一个 global 调用是否要改写成内部工具调用。
pub fn model_call(name: &str) -> Option<&'static str> {
    model_calls()
        .iter()
        .find(|(global, _)| *global == name)
        .map(|(_, tool)| *tool)
}

/// 目录函数：`models` 命名空间的宿主全局函数。
///
/// 它们不经过内部工具，而是由 `codemode.rs::call_global()` 直接调用 [`catalog_call`]；
/// 结果随后写入 journal，因此重放不会再次查询宿主目录，也不占
/// [`MAX_CONCURRENT_MODEL_CALLS`] 的槽位。
pub const CATALOG_GLOBALS: [&str; 3] = [
    "models.getModelsOfType",
    "models.getAvailableOfType",
    "models.getModelOfType",
];

/// 一个脚本最多同时进行的 `models.classify()`/`generateImages()` 调用数；其余等待。
///
/// 计数在 `codemode.rs` 的 `CallState` 里（每次 code mode 调用一份），由 `model_slot()`/
/// `release_model_slot()` 维护；等待者按先进先出唤醒。目录函数不占槽位。脚本结束时仍在
/// 等槽位的调用会被拒绝，不会发起付费请求。
pub const MAX_CONCURRENT_MODEL_CALLS: usize = 4;

/// 模型类型集合（顺序即 `new Set([...])` 的顺序）。
///
/// `to_model_type()` 与 `resolve_model()` 都按这个顺序遍历；后者用它决定错误消息里报告
/// 哪个“实际类型”。
pub const MODEL_TYPES: [ModelType; 3] = [ModelType::Chat, ModelType::Image, ModelType::Classifier];

/// 模型类型的字面量。
///
/// 返回值同时用于脚本可见的 `type` 参数、错误消息与 [`resolve_model`] 的提示；改动字面量
/// 会改变脚本 API。
pub fn model_type_str(model_type: ModelType) -> &'static str {
    match model_type {
        ModelType::Chat => "chat",
        ModelType::Image => "image",
        ModelType::Classifier => "classifier",
    }
}

/// `MODELS_DESCRIPTION`：`models` 命名空间的说明。
///
/// `LazyLock` 只在第一次使用时拼接；文本里插入了 [`MAX_CONCURRENT_MODEL_CALLS`] 的值。
/// 这是直接展示给模型的提示文本，改动会改变模型看到的 `models` 说明。
pub static MODELS_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
    [
        "`models` (all functions return promises):",
        "- `models.getModelsOfType(type, provider?)`, `models.getAvailableOfType(type, provider?)` (configured auth only), `models.getModelOfType(type, provider, id)`: the catalog; `type` is \"chat\", \"image\", or \"classifier\".",
        "- `models.classify(model, { state: { ... }, images?: [{ type: \"image\", data, mimeType }], questions: { <id>: { type: \"choice\", instructions, criteria: { <label>: <meaning> } } | { type: \"score\", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: \"bool\", instructions, criteria: { true: <meaning>, false: <meaning> } } } })` resolves to `{ answers: { <id>: ... }, stopReason, errorMessage?, usage? }`.",
        "- `models.generateImages(model, { input: [{ type: \"text\", text: <prompt> }, ...optional { type: \"image\", data, mimeType }] })` resolves to `{ output: [image blocks], stopReason, errorMessage?, usage? }`; show each image with `image(block)`.",
        &format!("At most {MAX_CONCURRENT_MODEL_CALLS} classify/generateImages calls run at once; more wait."),
    ]
    .join("\n")
});

/// 只把 JSON 对象算作 record；`null`、数组与标量都不算。
fn is_record(value: Option<&JsonValue>) -> bool {
    matches!(value, Some(JsonValue::Object(_)))
}

/// JS 的对象键序：整数索引键升序在前，其余按插入序。
///
/// 借助 `kiana_json` 的 `sort_js_keys` 拿到顺序（它返回重排后的副本，因此这一步会克隆
/// 整个值）；非对象返回空列表。
fn js_keys(value: &JsonValue) -> Vec<String> {
    match kiana_json::js_object::sort_js_keys(value) {
        JsonValue::Object(map) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// 错误消息里的一个脚本值：`undefined`、`a string`、`an array`，或它的键（`{ prompt }`）。
///
/// 输入是 `Option<&JsonValue>`：`None` 对应 JS 的 `undefined`，与 `Some(Null)` 不同。
/// 对象最多列出前 6 个键（多余用 `, ...`），键序按 JS 规则（整数索引键升序在前，其余
/// 插入序）；空数组与非空数组分别显示为 `an empty array` 与 `an array`。用于把脚本
/// 拼错的参数以它熟悉的形状回显到错误消息里。
pub fn describe_value(value: Option<&JsonValue>) -> String {
    let Some(value) = value else {
        return "undefined".to_owned();
    };
    match value {
        JsonValue::Null => "null".to_owned(),
        JsonValue::Array(items) => {
            if items.is_empty() {
                "an empty array".to_owned()
            } else {
                "an array".to_owned()
            }
        }
        JsonValue::Object(_) => {
            let keys = js_keys(value);
            if keys.is_empty() {
                "{}".to_owned()
            } else {
                let head: Vec<String> = keys.iter().take(6).cloned().collect();
                let suffix = if keys.len() > 6 { ", ..." } else { "" };
                format!("{{ {}{suffix} }}", head.join(", "))
            }
        }
        JsonValue::String(_) => "a string".to_owned(),
        JsonValue::Bool(_) => "a boolean".to_owned(),
        JsonValue::Number(_) => "a number".to_owned(),
    }
}

/// `an image`、`a classifier`。
///
/// 只看首字符是否为 `a/e/i/o/u`，不处理英语的例外发音；当前调用点只传
/// `chat`/`image`/`classifier`。
pub fn with_article(word: &str) -> String {
    let first = word.chars().next();
    let article = match first {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    };
    format!("{article} {word}")
}

/// `JSON.stringify(value)`（`undefined` 用 `"undefined"`）。
///
/// `None` 表示 JS 的 `undefined`（不是 `null`）；其余值用紧凑 JSON，字符串会带引号。
/// 错误消息用它把脚本传入的原值回显出来。
pub fn stringify_value(value: Option<&JsonValue>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(value) => kiana_json::js_object::stringify(value),
    }
}

/// 把脚本参数解析成模型类型：只接受 `"chat"`/`"image"`/`"classifier"` 三个精确字符串。
///
/// 其它任何值（缺失、`null`、数字、大小写不符）都返回错误，错误消息用
/// [`stringify_value`] 回显原值并列出合法取值。
fn to_model_type(value: Option<&JsonValue>) -> Result<ModelType, BoxError> {
    if let Some(JsonValue::String(text)) = value {
        for model_type in MODEL_TYPES {
            if model_type_str(model_type) == text {
                return Ok(model_type);
            }
        }
    }
    Err(format!(
        "Unknown model type {}. Use \"chat\", \"image\", or \"classifier\".",
        stringify_value(value)
    )
    .into())
}

/// 可选的 provider 参数：缺失与 `null` 都表示“所有 provider”；非字符串返回错误。
fn to_provider(value: Option<&JsonValue>) -> Result<Option<String>, BoxError> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(provider)) => Ok(Some(provider.clone())),
        Some(_) => Err("provider must be a string".into()),
    }
}

/// 给脚本的目录条目。`headers` 被去掉，因为配置的头可以带凭证。
///
/// 先把 `kiana_ai::types::Model` 序列化成 JSON，再从对象里删除顶层 `headers`；其余字段
/// （provider、baseUrl、type 等）原样交给脚本。序列化失败时返回 `null`（不是错误）。
pub fn to_model_info(model: &Model) -> JsonValue {
    let mut info = serde_json::to_value(model).unwrap_or(JsonValue::Null);
    if let JsonValue::Object(map) = &mut info {
        map.shift_remove("headers");
    }
    info
}

/// 目录函数：脚本以 `args`（参数列表）调用 `name` 时解析出的值。
///
/// `args` 是 spread 调用的参数数组；不是数组时按空列表处理。三个位置依次是 `type`、
/// `provider`、`id`，多余的参数被忽略：
/// - `models.getModelsOfType`：返回目录里该类型的全部模型（provider 可选）；
/// - `models.getAvailableOfType`：先解析 auth，只返回已配置凭证的 provider 的模型；它可能
///   等待 I/O，并接收调用方的 abort signal；
/// - `models.getModelOfType`：要求 provider 与 id 都是字符串，找不到时返回 `None`
///   （脚本看到 `undefined`），不是错误。
///
/// 返回 `Some(Array)` 时脚本拿到数组，`None` 时拿到 `undefined`。所有返回都经过
/// [`to_model_info`]，因此不会泄露 `headers`。类型或参数形状错误返回 `BoxError`，由
/// `call_global()` 转成脚本的 promise 拒绝。
pub async fn catalog_call(
    name: &str,
    args: Option<&JsonValue>,
    models: &dyn Models,
    context: &dyn Context,
) -> Result<Option<JsonValue>, BoxError> {
    let empty: Vec<JsonValue> = Vec::new();
    let list: &[JsonValue] = match args {
        Some(JsonValue::Array(items)) => items,
        _ => &empty,
    };
    let type_value = list.first();
    let provider_value = list.get(1);
    let id_value = list.get(2);
    if name == "models.getModelsOfType" {
        let model_type = to_model_type(type_value)?;
        let provider = to_provider(provider_value)?;
        return Ok(Some(JsonValue::Array(
            models
                .get_models_of_type(model_type, provider.as_deref())
                .iter()
                .map(to_model_info)
                .collect(),
        )));
    }
    if name == "models.getAvailableOfType" {
        let model_type = to_model_type(type_value)?;
        let provider = to_provider(provider_value)?;
        let available = models
            .get_available_of_type(
                model_type,
                provider.as_deref(),
                kiana_ai::auth::types::AuthOperationOptions {
                    signal: context.abort_signal().map(|signal| {
                        kiana_ai::utils::abort::AbortScope::from(signal.token().clone())
                    }),
                },
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        return Ok(Some(JsonValue::Array(
            available.iter().map(to_model_info).collect(),
        )));
    }
    if !matches!(provider_value, Some(JsonValue::String(_)))
        || !matches!(id_value, Some(JsonValue::String(_)))
    {
        let described = list
            .iter()
            .map(|value| describe_value(Some(value)))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "models.getModelOfType(type, provider, id) expects three strings, got ({described}). The provider and the id are separate arguments, for example models.getModelOfType(\"classifier\", \"typesafe\", \"jev-latest\")."
        )
        .into());
    }
    let model_type = to_model_type(type_value)?;
    let provider = provider_value
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let id = id_value.and_then(JsonValue::as_str).unwrap_or_default();
    Ok(models
        .get_model_of_type(model_type, provider, id)
        .map(|model| to_model_info(&model)))
}

/// classifier 上下文在错误里展示的形状。
///
/// 与 [`MODELS_DESCRIPTION`] 里 `models.classify()` 那一行的 context 参数形状一致；错误
/// 消息把它原样附在后面。
const CLASSIFIER_CONTEXT_SHAPE: &str = "{ state: { ... }, images?: [{ type: \"image\", data: <base64>, mimeType }], questions: { <id>: { type: \"choice\", instructions, criteria: { <label>: <meaning> } } | { type: \"score\", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: \"bool\", instructions, criteria: { true: <meaning>, false: <meaning> } } } }";

/// 非空且每一项都是字符串；空列表不算合法 criteria。
fn is_strings(values: &[&JsonValue]) -> bool {
    !values.is_empty() && values.iter().all(|value| value.is_string())
}

/// 图片块的形状：`type == "image"` 且 `data`/`mimeType` 都是字符串；多余字段不看。
fn is_image_block(value: &JsonValue) -> bool {
    match value {
        JsonValue::Object(map) => {
            map.get("type").and_then(JsonValue::as_str) == Some("image")
                && map.get("data").is_some_and(JsonValue::is_string)
                && map.get("mimeType").is_some_and(JsonValue::is_string)
        }
        _ => false,
    }
}

/// 检查脚本的 classifier 上下文，让错误在发请求之前就以预期形状失败。
///
/// 校验是逐层的形状检查，不是完整 schema：
/// - `context` 必须是对象，`context.state` 必须是对象；
/// - `images` 可选，若给出必须是图片块数组（`type: "image"`，`data` 与 `mimeType` 都是
///   字符串）；
/// - `questions` 必须是非空对象；每个问题必须有字符串 `instructions`，以及三种形态之一
///   的 `criteria`：`choice` 为非空对象且值全是字符串、`score` 为非空字符串数组、`bool`
///   必须同时有字符串 `true` 与 `false`。
///
/// 多余字段不拒绝（`ClassifierContext` 没有 `deny_unknown_fields`）。检查通过后再
/// `serde_json::from_value` 成强类型；那一步仍可能因字段细节失败，错误直接返回。这样
/// 脚本常见的拼写错误（例如只给 `{ prompt }`）得到带期望形状的消息，且不会发出请求
/// （测试 `rejects_a_bad_classifier_context_with_the_expected_shape_before_any_request`
/// 断言 provider 的调用次数为 0）。
pub fn check_classifier_context(
    context: Option<&JsonValue>,
) -> Result<ClassifierContext, BoxError> {
    let fail = |problem: String| -> BoxError {
        format!("models.classify() {problem}. Expected context: {CLASSIFIER_CONTEXT_SHAPE}.").into()
    };
    let Some(JsonValue::Object(map)) = context else {
        return Err(fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    if !is_record(map.get("state")) {
        return Err(fail(format!(
            "context.state must be an object, got {}",
            describe_value(map.get("state"))
        )));
    }
    if let Some(images) = map.get("images") {
        let Some(images) = images.as_array() else {
            return Err(fail(format!(
                "context.images must be an array, got {}",
                describe_value(Some(images))
            )));
        };
        for (index, image) in images.iter().enumerate() {
            if !is_image_block(image) {
                return Err(fail(format!(
                    "context.images[{index}] must be an image block, got {}",
                    describe_value(Some(image))
                )));
            }
        }
    }
    let Some(questions) = map.get("questions") else {
        return Err(fail(format!(
            "context.questions must map question IDs to questions, got {}",
            describe_value(None)
        )));
    };
    let JsonValue::Object(questions) = questions else {
        return Err(fail(format!(
            "context.questions must map question IDs to questions, got {}",
            describe_value(Some(questions))
        )));
    };
    if questions.is_empty() {
        return Err(fail(format!(
            "context.questions must map question IDs to questions, got {}",
            describe_value(Some(&JsonValue::Object(questions.clone())))
        )));
    }
    for (id, question) in questions {
        let at = format!("context.questions.{id}");
        let JsonValue::Object(question) = question else {
            return Err(fail(format!(
                "{at} must be a question object, got {}",
                describe_value(Some(question))
            )));
        };
        if !question
            .get("instructions")
            .is_some_and(JsonValue::is_string)
        {
            return Err(fail(format!("{at}.instructions must be a string")));
        }
        let criteria = question.get("criteria");
        let question_type = question.get("type").and_then(JsonValue::as_str);
        match question_type {
            Some("choice") => {
                let valid = match criteria {
                    Some(JsonValue::Object(criteria)) => {
                        let values: Vec<&JsonValue> = criteria.values().collect();
                        is_strings(&values)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"choice\" question, so criteria must map each label to its meaning"
                    )));
                }
            }
            Some("score") => {
                let valid = match criteria {
                    Some(JsonValue::Array(criteria)) => {
                        let values: Vec<&JsonValue> = criteria.iter().collect();
                        is_strings(&values)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"score\" question, so criteria must list the levels as strings, lowest first"
                    )));
                }
            }
            Some("bool") => {
                let valid = match criteria {
                    Some(JsonValue::Object(criteria)) => {
                        criteria.get("true").is_some_and(JsonValue::is_string)
                            && criteria.get("false").is_some_and(JsonValue::is_string)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"bool\" question, so criteria must be {{ true: string, false: string }}"
                    )));
                }
            }
            _ => {
                return Err(fail(format!(
                    "{at}.type must be \"choice\", \"score\", or \"bool\", got {}",
                    stringify_value(question.get("type"))
                )));
            }
        }
    }
    let checked: ClassifierContext = serde_json::from_value(JsonValue::Object(map.clone()))?;
    Ok(checked)
}

/// 检查脚本的图片上下文，让 `{ prompt }` 之类的错误以预期形状失败。
///
/// `context.input` 必须是非空数组；每一项要么是 `{ type: "text", text: <string> }`，
/// 要么是图片块（`type: "image"`，`data` 与 `mimeType` 都是字符串）。多余字段不拒绝。
/// 通过后 `serde_json::from_value` 成 `ImagesContext`，仍可能返回 serde 错误。
pub fn check_images_context(context: Option<&JsonValue>) -> Result<ImagesContext, BoxError> {
    let fail = |problem: String| -> BoxError {
        format!(
            "models.generateImages() {problem}. Expected context: {{ input: [{{ type: \"text\", text: <prompt> }}, ...optional {{ type: \"image\", data: <base64>, mimeType }} references] }}."
        )
        .into()
    };
    let Some(JsonValue::Object(map)) = context else {
        return Err(fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    let Some(input) = map.get("input") else {
        return Err(fail(format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(None)
        )));
    };
    let JsonValue::Array(blocks) = input else {
        return Err(fail(format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(Some(input))
        )));
    };
    if blocks.is_empty() {
        return Err(fail(format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(Some(input))
        )));
    }
    for (index, block) in blocks.iter().enumerate() {
        let valid = match block {
            JsonValue::Object(map) => match map.get("type").and_then(JsonValue::as_str) {
                Some("text") => map.get("text").is_some_and(JsonValue::is_string),
                Some("image") => is_image_block(block),
                _ => false,
            },
            _ => false,
        };
        if !valid {
            return Err(fail(format!(
                "context.input[{index}] must be a text or image block, got {}",
                describe_value(Some(block))
            )));
        }
    }
    let checked: ImagesContext = serde_json::from_value(JsonValue::Object(map.clone()))?;
    Ok(checked)
}

/// 只按 provider 与 id 解析脚本的模型，脚本自带的 baseUrl 或 headers 永远拿不到凭证，
/// 错误消息说明该怎么做。
///
/// `name` 只用于错误消息里的函数名；`model` 期望 `{ provider, id }` 形状。
/// - provider/id 缺失或不是字符串：报“第一个参数应是哪种模型”，并在参数为
///   `undefined`/`null` 时提示 `getModelOfType()` 对未知 provider/id 返回 `undefined`；
/// - 目录里有：返回克隆的 `kiana_ai::types::Model`；
/// - 目录里没有：若同一 provider/id 在其它类型下存在，错误里报告实际类型；否则报告未知。
///
/// 所有错误路径都附带 `models.getAvailableOfType("<type>")` 的提示。模型只来自宿主目录，
/// 因此脚本无法注入 baseUrl/headers。
pub fn resolve_model(
    name: &str,
    model_type: ModelType,
    model: Option<&JsonValue>,
    models: &dyn Models,
) -> Result<Model, BoxError> {
    let type_name = model_type_str(model_type);
    let list_hint = format!(
        "List the {type_name} models you can use with models.getAvailableOfType(\"{type_name}\")."
    );
    let provider = model
        .and_then(|model| model.get("provider"))
        .and_then(JsonValue::as_str);
    let id = model
        .and_then(|model| model.get("id"))
        .and_then(JsonValue::as_str);
    let (Some(provider), Some(id)) = (provider, id) else {
        let undefined_hint = if model.is_none() || model == Some(&JsonValue::Null) {
            " models.getModelOfType() returns undefined for an unknown provider or id."
        } else {
            ""
        };
        return Err(format!(
            "{name}() expects {} model as its first argument, got {}.{undefined_hint} {list_hint}",
            with_article(type_name),
            describe_value(model)
        )
        .into());
    };
    if let Some(resolved) = models.get_model_of_type(model_type, provider, id) {
        return Ok(resolved);
    }
    let actual_type = MODEL_TYPES.iter().copied().find(|other| {
        *other != model_type && models.get_model_of_type(*other, provider, id).is_some()
    });
    match actual_type {
        Some(actual_type) => Err(format!(
            "\"{provider}/{id}\" is {} model, not {} model. {list_hint}",
            with_article(model_type_str(actual_type)),
            with_article(type_name)
        )
        .into()),
        None => Err(format!("Unknown {type_name} model \"{provider}/{id}\". {list_hint}").into()),
    }
}

/// `kiana_chord` 的 abort 原因 → kiana-ai 的渲染形态（名字 + 消息）。
///
/// - `Error(e)` → name `"Error"`，message 为 `e.to_string()`；
/// - `DomException { name, message }` 原样传递；
/// - `Value(v)` → name `"AbortError"`，message 为 JS `String(v)` 的写法（对象会变成
///   `[object Object]`，数组按 join 展开），不是 JSON 序列化。
fn ai_abort_reason(
    reason: kiana_chord::context::AbortReason,
) -> kiana_ai::utils::abort::AbortReason {
    match reason {
        kiana_chord::context::AbortReason::Error(error) => kiana_ai::utils::abort::AbortReason {
            name: "Error".to_owned(),
            message: error.to_string(),
        },
        kiana_chord::context::AbortReason::DomException { name, message } => {
            kiana_ai::utils::abort::AbortReason { name, message }
        }
        kiana_chord::context::AbortReason::Value(value) => kiana_ai::utils::abort::AbortReason {
            name: "AbortError".to_owned(),
            message: kiana_json::js_string::js_string(Some(&value)),
        },
    }
}

/// 一个文本内容块：固定 `type: "text"`，不带 `text_signature`。
fn text_block(text: String) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text,
        text_signature: None,
    })
}

/// 一次模型调用的结果，按工具返回：整个结果给脚本；用量由 Harness 记账。
///
/// `stop_reason` 用 TS 字面量（`"stop"`/`"error"`/`"aborted"`）。只有 `"stop"` 算成功：
/// 文本是 `"<label> done"`；否则文本带上错误消息，且 `is_error = Some(true)`。无论成功
/// 失败，`structured_output` 都是完整结果对象的 JSON——配合注册项的结构化标记，脚本的
/// promise 仍会 resolve。`usage` 原样交给 Harness，按内部工具名累计。
fn model_result(
    label: &str,
    stop_reason: &str,
    error_message: Option<&str>,
    usage: Option<Usage>,
    value: JsonValue,
) -> ToolExecutionResult {
    let ok = stop_reason == "stop";
    let text = if ok {
        format!("{label} done")
    } else {
        format!("{label} {stop_reason}: {}", error_message.unwrap_or(""))
    };
    ToolExecutionResult {
        output: Some(vec![text_block(text)]),
        structured_output: Some(value),
        is_error: if ok { None } else { Some(true) },
        details: None,
        diagnostics: None,
        usage,
        control: None,
    }
}

/// `models.classify()` 与 `models.generateImages()` 背后的内部工具。
///
/// 两个注册项的相同点：
/// - `callers: [Tools]`：只允许嵌套工具调用，模型不能直接选中它们；
/// - 名字带 [`CODEMODE_INTERNAL_PREFIX`]：脚本的 `tools`、`ALL_TOOLS` 与工具搜索都看不到；
/// - `structured_output_schema: Some({})`：这里只当作“结果有结构化输出”的标记，让失败
///   结果也能随对象 resolve；真正的输出形状由 `model_result` 填入；
/// - 参数 schema 是通用的 `{ model, context }`（内部调用已由 `as_tool_call()` 归一）。
///
/// 每个执行闭包先解析模型、预检上下文，再发起一次 provider 调用。
pub fn create_model_tools() -> Vec<ToolRegistration> {
    let parameters = json!({
        "type": "object",
        "required": ["model", "context"],
        "properties": { "model": {}, "context": {} }
    });
    let classify: ToolExecuteFn = Arc::new(move |args, api, context| {
        Box::pin(async move {
            let model = resolve_model(
                "models.classify",
                ModelType::Classifier,
                args.get("model"),
                &*api.models(),
            )?;
            let checked = check_classifier_context(args.get("context"))?;
            let result = api
                .models()
                .classify(
                    &model,
                    &checked,
                    ModelsClassifierOptions {
                        base: kiana_ai::types::StreamOptions {
                            telemetry: Some(context.telemetry()),
                            signal: context.abort_signal().map(|signal| signal.token().clone()),
                            ..Default::default()
                        },
                        signal_reason: context
                            .abort_signal()
                            .and_then(|signal| signal.reason())
                            .map(ai_abort_reason),
                        ..Default::default()
                    },
                )
                .await;
            let value = serde_json::to_value(&result)?;
            let label = format!("classify {}/{}", model.provider, model.id);
            Ok(model_result(
                &label,
                stop_reason_text(result.stop_reason),
                result.error_message.as_deref(),
                result.usage.clone(),
                value,
            ))
        })
    });
    let generate_images: ToolExecuteFn = Arc::new(move |args, api, context| {
        Box::pin(async move {
            let model = resolve_model(
                "models.generateImages",
                ModelType::Image,
                args.get("model"),
                &*api.models(),
            )?;
            let checked = check_images_context(args.get("context"))?;
            let result = api
                .models()
                .generate_images(
                    &model,
                    &checked,
                    ModelsImagesOptions {
                        base: kiana_ai::types::StreamOptions {
                            telemetry: Some(context.telemetry()),
                            signal: context.abort_signal().map(|signal| signal.token().clone()),
                            ..Default::default()
                        },
                        signal_reason: context
                            .abort_signal()
                            .and_then(|signal| signal.reason())
                            .map(ai_abort_reason),
                        ..Default::default()
                    },
                )
                .await;
            let value = serde_json::to_value(&result)?;
            let label = format!("generateImages {}/{}", model.provider, model.id);
            Ok(model_result(
                &label,
                images_stop_reason_text(result.stop_reason),
                result.error_message.as_deref(),
                result.usage.clone(),
                value,
            ))
        })
    });
    vec![
        define_tool(ToolRegistration {
            tool: kiana_ai::Tool {
                name: CLASSIFY_TOOL.to_owned(),
                description: "models.classify() of code mode scripts.".to_owned(),
                parameters: parameters.clone(),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: Some(json!({})),
            callers: Some(vec![ToolCaller::Tools]),
            deferred: false,
            output_limits: None,
            execute: Some(classify),
        }),
        define_tool(ToolRegistration {
            tool: kiana_ai::Tool {
                name: GENERATE_IMAGES_TOOL.to_owned(),
                description: "models.generateImages() of code mode scripts.".to_owned(),
                parameters: parameters.clone(),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: Some(json!({})),
            callers: Some(vec![ToolCaller::Tools]),
            deferred: false,
            output_limits: None,
            execute: Some(generate_images),
        }),
    ]
}
/// provider 的 classifier 停止原因 → 脚本可见的字面量。
fn stop_reason_text(stop_reason: ClassifierStopReason) -> &'static str {
    match stop_reason {
        ClassifierStopReason::Stop => "stop",
        ClassifierStopReason::Error => "error",
        ClassifierStopReason::Aborted => "aborted",
    }
}

/// provider 的图片生成停止原因 → 脚本可见的字面量。
fn images_stop_reason_text(stop_reason: ImagesStopReason) -> &'static str {
    match stop_reason {
        ImagesStopReason::Stop => "stop",
        ImagesStopReason::Error => "error",
        ImagesStopReason::Aborted => "aborted",
    }
}
