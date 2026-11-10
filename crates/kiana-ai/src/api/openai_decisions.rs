//! api/openai-decisions.ts。
//!
//! OpenAI 的 Decisions API：`POST /v1/decisions`，body 为 `{ model, input, questions }`。
//! <https://developers.openai.com/api/docs/guides/decisions>
//!
//! state 以 JSON 文本发送。带图片时 input 变成一条 user 消息：state 作为 `input_text`，
//! 后面跟 `input_image` data URL。问题映射到 Decisions 类型：`choice` → `choice`、
//! `score` → `score`、`bool` → `predicate`。predicate 没有 criteria 字段，因此 true/false
//! 的含义追加到 instructions。
//!
//! 只有 OpenAI API key 可用：Sign in with ChatGPT 的 token 在这条路由上会被拒绝。
//!
//! TS: `packages/ai/src/api/openai-decisions.ts`。
//!
//! # 这个文件解决什么问题
//! 把 Kiana 的通用分类契约（[`ClassifierContext`] 加一组 [`ClassifierQuestion`]）翻译成
//! Decisions API 的线上 JSON 形状，再把响应翻译回 [`ClassifierResult`]。上层只认识
//! [`crate::models::ProviderClassifier`]，不需要知道 `input_text`/`input_image` 的拼法或
//! `predicate` 与 `bool` 的命名差异。
//!
//! # 一次请求的执行流程
//! 1. [`classify`] 先构造一个 `stop_reason = Stop` 的结果骨架（`timestamp` 取当前毫秒），
//!    再调用私有的 `run`。任何错误只改写骨架的 `answers`/`usage`/`stop_reason`/
//!    `error_message`（`usage` 在解析答案之前就已写入），因此调用方总能拿到结构完整的
//!    结果。
//! 2. `run` 校验 `model.api == "openai-decisions"`，把问题按 JS 键序转成 `questions`
//!    数组，把 `state`（无图片时是 JSON 文本，有图片时是一条 user 消息）与 `model.id`
//!    一起 POST 到 `{base_url}/decisions`。
//! 3. 响应必须是 JSON 对象。先解析 `usage`（被拒绝或格式错误的回答同样已经计费），
//!    再按问题 ID 逐个解析答案；任何一步失败都会清空全部答案。
//!
//! # 失败模式与注意事项
//! - 超过 128 张图片在发请求前就失败；有图片时 `input_text` 在前，图片按传入顺序在后。
//! - 504 不重试：网关超时通常意味着输入超过了时间上限，重发同一输入只会再次超时。
//! - `choice` 题的 `probabilities` 会按 JS 对象键序重排（整数索引键升序在前，其余键保持
//!   插入序），这是 TS `Object.fromEntries` 的可观察行为，测试会锁定它。
//! - 答案按 `name` 匹配问题 ID，与服务器返回顺序无关；重复 `name` 以最后一次为准。
//! - 问题 ID 允许 `__proto__`、`constructor` 这类在 JS 里原型敏感的字符串；Rust 的 map
//!   没有原型链，天然按普通键处理（测试 `preserves_prototype_sensitive_question_ids_in_answers`
//!   锁定）。
//! - 只有 OpenAI API key 可用；OAuth（Sign in with ChatGPT）凭证在
//!   `providers/openai.rs` 的模型过滤里就被排除，本文件不承担鉴权判断。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;

use crate::models::{ClassifierOptions, ProviderClassifier};
use crate::types::{
    ClassifierAnswer, ClassifierContext, ClassifierQuestion, ClassifierResult,
    ClassifierStopReason, Model,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};

use super::classifier_shared::{
    ClassifierHttpError, is_record, js_entries, json_object_value, now_unix_ms,
    parse_classifier_usage, post_classifier_request, required_number, url_with_base,
};

const LABEL: &str = "OpenAI Decisions";

/// The endpoint accepts at most this many image parts per request.
const MAX_IMAGES: usize = 128;

/// Cloudflare in front of api.openai.com answers 504 with an HTML page when a request runs
/// longer than about five seconds; retrying the same input runs into it again, so 504 is not
/// retried.
const NO_RETRY_STATUSES: [u16; 1] = [504];

/// `predicateInstructions(question)`。
///
/// 把 true/false 的语义并入 `instructions`。Decisions 的 `predicate` 类型没有 criteria
/// 字段，因此布尔题的两个分支说明只能写成两行自然语言追加到 instructions 后面，形如：
/// ```text
/// <原 instructions>\n\nTrue means: <criteria.true>\nFalse means: <criteria.false>
/// ```
/// 某一侧的说明为空串时省略该行；两侧都为空时原样返回 instructions（不留下多余空行）。
/// 非 Bool 问题返回空串——只有 `wire_question` 的 predicate 分支会调用它。
fn predicate_instructions(question: &ClassifierQuestion) -> String {
    let ClassifierQuestion::Bool {
        instructions,
        criteria,
    } = question
    else {
        return String::new();
    };
    let mut meanings = Vec::new();
    if !criteria.r#true.is_empty() {
        meanings.push(format!("True means: {}", criteria.r#true));
    }
    if !criteria.r#false.is_empty() {
        meanings.push(format!("False means: {}", criteria.r#false));
    }
    if meanings.is_empty() {
        instructions.clone()
    } else {
        format!("{instructions}\n\n{}", meanings.join("\n"))
    }
}

/// `wireQuestion(name, question)`。
///
/// 把一个问题翻译成 Decisions 的 `DecisionQuestion`。`name` 就是问题 ID
/// （`context.questions` 的键），响应里的答案靠它回配。三种形态：
/// - `choice`：`criteria` 的键是选项值、值是可选的说明；空说明会省略 `description` 键
///   （对应 TS 的 `description ? {value, description} : {value}`，不会发送空串）。
/// - `score`：`criteria` 的每个 label 写成 `{ "label": ... }`，线上没有 value 字段。
/// - `bool`：线上类型叫 `predicate`，没有 criteria 字段，true/false 的含义通过
///   `predicate_instructions` 并入 `instructions`。
///
/// choice 的选项顺序由 `js_entries` 决定（整数索引键升序在前），与 TS
/// `Object.entries(criteria)` 一致；问题在数组里的顺序则由 `run` 决定。
fn wire_question(name: &str, question: &ClassifierQuestion) -> Value {
    let mut map = serde_json::Map::new();
    match question {
        ClassifierQuestion::Choice {
            instructions,
            criteria,
        } => {
            map.insert("type".to_string(), Value::String("choice".to_string()));
            map.insert("name".to_string(), Value::String(name.to_string()));
            map.insert(
                "instructions".to_string(),
                Value::String(instructions.clone()),
            );
            let choices = js_entries(criteria)
                .into_iter()
                .map(|(value, description)| {
                    let mut choice = serde_json::Map::new();
                    choice.insert("value".to_string(), Value::String(value.clone()));
                    // 空 description 省略该键（`description ? {value, description} : {value}`）。
                    if !description.is_empty() {
                        choice.insert(
                            "description".to_string(),
                            Value::String(description.clone()),
                        );
                    }
                    Value::Object(choice)
                })
                .collect();
            map.insert("choices".to_string(), Value::Array(choices));
        }
        ClassifierQuestion::Score {
            instructions,
            criteria,
        } => {
            map.insert("type".to_string(), Value::String("score".to_string()));
            map.insert("name".to_string(), Value::String(name.to_string()));
            map.insert(
                "instructions".to_string(),
                Value::String(instructions.clone()),
            );
            let levels = criteria
                .iter()
                .map(|label| {
                    let mut level = serde_json::Map::new();
                    level.insert("label".to_string(), Value::String(label.clone()));
                    Value::Object(level)
                })
                .collect();
            map.insert("levels".to_string(), Value::Array(levels));
        }
        ClassifierQuestion::Bool { .. } => {
            map.insert("type".to_string(), Value::String("predicate".to_string()));
            map.insert("name".to_string(), Value::String(name.to_string()));
            map.insert(
                "instructions".to_string(),
                Value::String(predicate_instructions(question)),
            );
        }
    }
    Value::Object(map)
}

/// `wireInput(context)`：无图片时是 state 的 JSON 文本，否则是一条 user 消息。
///
/// 无图片时返回 `state` 的 JSON 字符串；有图片时返回只含一条 user 消息的数组，内容顺序
/// 固定为「1 个 `input_text` + N 个 `input_image`」。图片以 `data:<mime>;base64,<data>`
/// 内联，请求体大小随图片线性增长。
///
/// 错误：图片超过 `MAX_IMAGES` 时返回普通 [`ClassifierHttpError`]（不发请求），消息为
/// `OpenAI Decisions accepts at most 128 images, got <n>`；base URL 非法等错误不在这里，
/// 而在随后的 `url_with_base` 里暴露。
fn wire_input(context: &ClassifierContext) -> Result<Value, ClassifierHttpError> {
    let state = kiana_json::js_object::stringify(&json_object_value(&context.state));
    let images = context.images.as_deref().unwrap_or(&[]);
    if images.is_empty() {
        return Ok(Value::String(state));
    }
    if images.len() > MAX_IMAGES {
        return Err(ClassifierHttpError::new(format!(
            "{LABEL} accepts at most {MAX_IMAGES} images, got {}",
            images.len()
        )));
    }
    let mut content = Vec::with_capacity(images.len() + 1);
    let mut text_part = serde_json::Map::new();
    text_part.insert("type".to_string(), Value::String("input_text".to_string()));
    text_part.insert("text".to_string(), Value::String(state));
    content.push(Value::Object(text_part));
    for image in images {
        let mut part = serde_json::Map::new();
        part.insert("type".to_string(), Value::String("input_image".to_string()));
        part.insert(
            "image_url".to_string(),
            Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
        );
        content.push(Value::Object(part));
    }
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert("content".to_string(), Value::Array(content));
    Ok(Value::Array(vec![Value::Object(message)]))
}

/// `choiceProbabilities(value, id)`：`[{value, probability}]` → `Record<string, number>`。
///
/// 校验是「全有或全无」：任一条目不是对象、`value` 不是字符串、或 `probability` 不是
/// 有限数字，整次分类就失败，不会跳过坏条目。`value` 重复时后者覆盖前者，但键的插入
/// 位置保留第一次出现的位置（与 TS 对象赋值一致）。
///
/// 最后按 JS 对象键序重排：数组索引键（`"0"`、`"2"`、`"10"`）按数值升序排在最前，
/// 其余键保持插入序；这是 `Object.fromEntries` 的可观察结果，测试
/// `orders_choice_probability_keys_like_object_from_entries` 用 Node 的黄金值锁定。
fn choice_probabilities(
    value: &Value,
    id: &str,
) -> Result<IndexMap<String, f64>, ClassifierHttpError> {
    let invalid =
        || ClassifierHttpError::new(format!("{LABEL} returned invalid probabilities for {id}"));
    let Value::Array(entries) = value else {
        return Err(invalid());
    };
    let mut probabilities = IndexMap::with_capacity(entries.len());
    for entry in entries {
        let Some(object) = entry.as_object() else {
            return Err(invalid());
        };
        let Some(value) = object.get("value").and_then(Value::as_str) else {
            return Err(invalid());
        };
        let probability = required_number(
            LABEL,
            object.get("probability").unwrap_or(&Value::Null),
            &format!("probability for {id}.{value}"),
        )?;
        probabilities.insert(value.to_string(), probability);
    }
    // `Object.fromEntries` 建出的是普通对象：数组索引键按数值升序排在最前，其余键保持插入序
    // （`{"10":..,"2":..,"b":..}` 枚举为 `["2","10","b"]`）。
    Ok(js_entries(&probabilities)
        .into_iter()
        .map(|(value, probability)| (value.clone(), *probability))
        .collect())
}

/// `parseAnswer(id, question, answer)`。
///
/// 按问题类型校验并转换单个答案。先无条件检查 `answer.type == "refusal"`：无论问题是
/// 什么类型，拒答都让整次分类失败（错误文本 `OpenAI Decisions refused to answer <id>`）。
/// 其余分支要求 `type` 与问题类型严格对应（`choice`/`score`/`predicate`），再读取各自
/// 必填的字符串或数值字段。
///
/// 缺字段、类型不符、数值非有限都会返回普通 [`ClassifierHttpError`]；本函数不做部分
/// 容错——调用方要么拿到全部答案，要么一个都没有。
fn parse_answer(
    id: &str,
    question: &ClassifierQuestion,
    answer: &serde_json::Map<String, Value>,
) -> Result<ClassifierAnswer, ClassifierHttpError> {
    let answer_type = answer.get("type").and_then(Value::as_str);
    if answer_type == Some("refusal") {
        return Err(ClassifierHttpError::new(format!(
            "{LABEL} refused to answer {id}"
        )));
    }
    match question {
        ClassifierQuestion::Choice { .. } => {
            let Some(choice) = answer.get("choice").and_then(Value::as_str) else {
                return Err(ClassifierHttpError::new(format!(
                    "{LABEL} did not return a choice answer for {id}"
                )));
            };
            if answer_type != Some("choice") {
                return Err(ClassifierHttpError::new(format!(
                    "{LABEL} did not return a choice answer for {id}"
                )));
            }
            let probabilities =
                choice_probabilities(answer.get("probabilities").unwrap_or(&Value::Null), id)?;
            let confidence = required_number(
                LABEL,
                answer.get("confidence").unwrap_or(&Value::Null),
                &format!("confidence for {id}"),
            )?;
            Ok(ClassifierAnswer::Choice {
                choice: choice.to_string(),
                probabilities,
                confidence,
            })
        }
        ClassifierQuestion::Score { .. } => {
            if answer_type != Some("score") {
                return Err(ClassifierHttpError::new(format!(
                    "{LABEL} did not return a score answer for {id}"
                )));
            }
            let score = required_number(
                LABEL,
                answer.get("score").unwrap_or(&Value::Null),
                &format!("score for {id}"),
            )?;
            let confidence = required_number(
                LABEL,
                answer.get("confidence").unwrap_or(&Value::Null),
                &format!("confidence for {id}"),
            )?;
            Ok(ClassifierAnswer::Score { score, confidence })
        }
        ClassifierQuestion::Bool { .. } => {
            if answer_type != Some("predicate") {
                return Err(ClassifierHttpError::new(format!(
                    "{LABEL} did not return a predicate answer for {id}"
                )));
            }
            let probability = required_number(
                LABEL,
                answer.get("probability").unwrap_or(&Value::Null),
                &format!("probability for {id}"),
            )?;
            Ok(ClassifierAnswer::Bool { probability })
        }
    }
}

/// `parseAnswers(value, context)`：答案按 name 匹配，输出顺序 = 问题顺序。
///
/// 第一步只收集「是对象且 `name` 是字符串」的答案到 `by_name`：无法识别的数组元素被
/// 静默跳过，重复 `name` 后者覆盖前者（与 TS 的 `byName.set(answer.name, answer)` 覆盖
/// 语义一致）。
/// 第二步按 `context.questions` 的 JS 键序遍历并逐个查找；任一问题缺答案就返回
/// `OpenAI Decisions did not return an answer for <id>`。因此成功结果的键序恒等于问题
/// 顺序，而不是服务器返回的数组顺序（测试会打乱响应顺序来验证）。
fn parse_answers(
    value: &Value,
    context: &ClassifierContext,
) -> Result<IndexMap<String, ClassifierAnswer>, ClassifierHttpError> {
    let Value::Array(entries) = value else {
        return Err(ClassifierHttpError::new(format!(
            "{LABEL} returned an unexpected response"
        )));
    };
    let mut by_name: IndexMap<String, &serde_json::Map<String, Value>> = IndexMap::new();
    for answer in entries {
        let Some(object) = answer.as_object() else {
            continue;
        };
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            continue;
        };
        by_name.insert(name.to_string(), object);
    }
    let mut answers = IndexMap::with_capacity(context.questions.len());
    for (id, question) in js_entries(&context.questions) {
        let Some(answer) = by_name.get(id) else {
            return Err(ClassifierHttpError::new(format!(
                "{LABEL} did not return an answer for {id}"
            )));
        };
        answers.insert(id.clone(), parse_answer(id, question, answer)?);
    }
    Ok(answers)
}

/// `errorMessage(error)`：504 的网关超时有专属说明（且不打印 HTML 页面）。
///
/// Cloudflare 超时返回的是整页 HTML，走通用正文提取会让整页 HTML 参与拼接，但最多保留
/// `MAX_PROVIDER_ERROR_BODY_CHARS`（4000 个 UTF-16 码元，超出部分附截断说明）；这里对
/// 504 返回固定文案，并提示「约 60 万 token 以上的输入」这一已观察到的触发条件。其它
/// 错误交给 `format_provider_error`（`normalize_provider_error` 之后的正文截断与格式化）。
fn error_message(error: &ClassifierHttpError) -> String {
    if error.status == Some(504) {
        return format!(
            "{LABEL} error (504): the request timed out at the gateway. Very large inputs \
             (above roughly 600K tokens) currently exceed its time limit."
        );
    }
    format_provider_error(
        &normalize_provider_error(&error.to_provider_error_value()),
        Some(&format!("{LABEL} error")),
    )
}

/// 发送一次 Decisions 请求并把结果写进 `output`。
///
/// `output` 由 [`classify`] 预先填好固定字段，本函数只负责补 `usage` 与 `answers`；
/// 失败时可能已经写入 `usage`（计费已发生），`answers` 仍由调用方清空。
///
/// 错误来源（按触发位置）：API 名不符 → `Unsupported classifier API`；图片超限 →
/// `accepts at most 128 images`；URL 非法 → `Invalid URL`；缺 key →
/// `No API key for provider`；HTTP/超时/解析失败由 `post_classifier_request` 转成
/// [`ClassifierHttpError`]；响应不是对象或缺答案 → 本函数的普通错误。全部错误都由
/// [`classify`] 转成结果里的 `stop_reason`/`error_message`。
async fn run(
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
    output: &mut ClassifierResult,
) -> Result<(), ClassifierHttpError> {
    if model.api != "openai-decisions" {
        return Err(ClassifierHttpError::unsupported_api(&model.api));
    }
    let questions = js_entries(&context.questions)
        .into_iter()
        .map(|(id, question)| wire_question(id, question))
        .collect();
    // 问题数组的键序 = JS 对象键序（整数索引键升序在前）；wire_question 只负责单个问题。
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("input".to_string(), wire_input(context)?);
    body.insert("questions".to_string(), Value::Array(questions));
    let response = post_classifier_request(
        LABEL,
        url_with_base(&model.base_url, "decisions")?,
        model,
        Value::Object(body),
        options,
        &NO_RETRY_STATUSES,
        None,
    )
    .await?;
    // 与 TS 的 `isRecord(body)` 对应：先判形状，非对象响应（例如网关返回的裸数组或
    // HTML 被解析成标量）在解析 usage/answers 之前就报 unexpected response。
    if !is_record(&response) {
        return Err(ClassifierHttpError::new(format!(
            "{LABEL} returned an unexpected response"
        )));
    }
    // Set before parsing answers: a request with malformed or refused answers was still billed.
    let usage = parse_classifier_usage(response.get("usage").unwrap_or(&Value::Null), model);
    if let Some(usage) = usage {
        output.usage = Some(usage);
    }
    output.answers = parse_answers(response.get("answers").unwrap_or(&Value::Null), context)?;
    Ok(())
}

/// Classification through OpenAI's Decisions API.
///
/// 参数：`model` 必须是 `api == "openai-decisions"` 的 classifier 模型（否则结果是
/// `Unsupported classifier API: <api>`，不发送请求）；`context` 携带状态、可选图片与
/// 问题定义；`options` 是 [`ClassifierOptions`]（即 [`crate::types::StreamOptions`] 的
/// 别名），其中 `api_key` 必填。
///
/// 返回：签名不是 `Result`，所有可预期失败都编码在 [`ClassifierResult`] 里。成功时
/// `stop_reason = Stop`、`answers` 按问题顺序排列；失败时 `answers` 为空，
/// `stop_reason = Error`，若 `options.signal` 已取消则 `stop_reason = Aborted`
/// （两种情况的错误文本都写入 `error_message`）。只要响应里的 usage 可解析就会保留，
/// 因为被拒绝或格式错误的回答也已经计费。
///
/// [`crate::models::ProviderClassifier`] 的实现只是转发到本函数，便于测试直接调用。
pub async fn classify(
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: now_unix_ms(),
    };
    if let Err(error) = run(model, context, options, &mut output).await {
        output.answers = IndexMap::new();
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
        output.stop_reason = if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        };
        output.error_message = Some(error_message(&error));
    }
    output
}

/// `openAIDecisionsApi()`。
///
/// 构造注册到 `openai` provider 的 Decisions 分类器实现。返回 trait object，由
/// `providers/openai.rs` 以 `"openai-decisions"` 为键放进 `Provider` 的分类器表
/// （该 provider 的 classifier 模型只对有 API key 的凭证列出）。实现无状态，实际工作
/// 全部委托给 [`classify`]。
pub fn openai_decisions_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(OpenAiDecisionsApi)
}

struct OpenAiDecisionsApi;

#[async_trait]
impl ProviderClassifier for OpenAiDecisionsApi {
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        classify(model, context, options).await
    }
}
