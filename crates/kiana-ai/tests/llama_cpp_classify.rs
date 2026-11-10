//! `test/llama-cpp-classify.test.ts` 的 Rust 移植。
//!
//! TS 的 `fakeServer` 是一个按路径分派的假 `fetch`；Rust 侧用同一形状的
//! [`MockHttpClient`] handler（`/tokenize`、`/apply-template`、`/completion`）。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ai::api::llama_cpp_classify::{
    answer_from_probabilities, classify, label_probabilities, llama_server_root, peak_confidence,
    render_question,
};
use kiana_ai::http::HttpRequest;
use kiana_ai::models::ClassifierOptions;
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierStopReason, Model, StreamOptions,
};
use serde_json::{Value, json};
use url::Url;

use mock_http::{MockHttpClient, MockResponse, classifier_model, context, questions};

static SERVER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A fresh server URL per test: label tokens are cached per server and model.
fn model() -> Model {
    let index = SERVER_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    classifier_model(
        "llama-cpp-classify",
        "llama.cpp",
        "qwen",
        &format!("http://llama-{index}.test:8080/v1"),
    )
}

/// Token IDs: one per character, the character code.
fn char_tokens(content: &str) -> Vec<f64> {
    content.chars().map(|ch| ch as u32 as f64).collect()
}

type NextFn = Arc<dyn Fn(&str, i64) -> Value + Send + Sync>;
type TemplateFn = Arc<dyn Fn(&[Value]) -> String + Send + Sync>;
type TokenizeFn = Arc<dyn Fn(&str) -> Vec<f64> + Send + Sync>;

#[derive(Clone, Default)]
struct FakeServerOptions {
    /// Log-probabilities of the next token by token text, in rank order.
    next: Option<NextFn>,
    template: Option<TemplateFn>,
    tokenize: Option<TokenizeFn>,
    /// 每次成功请求前的等待（让并发调用真正重叠）。
    delay: Option<std::time::Duration>,
}

/// `fakeServer(options)`：按路径分派的假服务器。
fn fake_server(options: FakeServerOptions) -> Arc<MockHttpClient> {
    let default_tokenize: TokenizeFn = Arc::new(|content: &str| char_tokens(content));
    let tokenize = options.tokenize.clone().unwrap_or(default_tokenize);
    let next = options.next.clone();
    let template = options.template.clone();
    let delay = options.delay;
    let handler = move |request: &HttpRequest| {
        let url = Url::parse(&request.url).expect("request URL");
        let body: Value =
            serde_json::from_slice(request.body.as_deref().unwrap_or_default()).expect("json");
        match url.path() {
            "/tokenize" => {
                let content = body["content"].as_str().expect("content");
                MockResponse::json(json!({"tokens": tokenize(content)}))
            }
            "/apply-template" => {
                let messages = body["messages"].as_array().expect("messages").clone();
                let prompt = match &template {
                    Some(template) => template(&messages),
                    None => {
                        let mut prompt = String::new();
                        for message in &messages {
                            prompt.push_str(&format!(
                                "<|{}|>\n{}\n",
                                message["role"].as_str().unwrap_or_default(),
                                message["content"].as_str().unwrap_or_default()
                            ));
                        }
                        prompt.push_str("<|assistant|>\n");
                        prompt
                    }
                };
                MockResponse::json(json!({"prompt": prompt}))
            }
            "/completion" => {
                let prompt = body["prompt"].as_str().expect("prompt");
                let depth = body["n_probs"].as_i64().expect("n_probs");
                let ranked = match &next {
                    Some(next) => next(prompt, depth),
                    None => json!({"A": -0.1, "B": -2.5}),
                };
                let top_logprobs: Vec<Value> = ranked
                    .as_object()
                    .expect("ranked object")
                    .iter()
                    .map(|(token, logprob)| {
                        json!({
                            "id": token.chars().next().map(|ch| ch as u32).unwrap_or(0),
                            "token": token,
                            "bytes": [],
                            "logprob": logprob
                        })
                    })
                    .collect();
                MockResponse::json(json!({
                    "content": "A",
                    "completion_probabilities": [{"id": 65, "token": "A", "top_logprobs": top_logprobs}]
                }))
            }
            _ => MockResponse::text(404, "not found"),
        }
    };
    match delay {
        Some(delay) => MockHttpClient::new_delayed(handler, delay),
        None => MockHttpClient::new(handler),
    }
}

/// Completion log-probabilities for bool (Yes/No) and letter labels.
fn answer_by_prompt(prompt: &str, _depth: i64) -> Value {
    if prompt.contains("Answer Yes or No.") {
        json!({"Y": -0.05, "N": -3})
    } else if prompt.contains("Answer with one level number.") {
        json!({"2": -0.2, "1": -1.8, "0": -4})
    } else {
        json!({"B": -0.3, "A": -1.5, "C": -3})
    }
}

fn classifier_context() -> ClassifierContext {
    context(
        json!({"message": "Help! My payouts have been failing for 3 days."}),
        questions(vec![
            (
                "team",
                ClassifierQuestion::Choice {
                    instructions: "Which team should handle this?".into(),
                    criteria: [
                        ("billing".to_string(), "Payments and refunds".to_string()),
                        ("technical".to_string(), "Bugs and outages".to_string()),
                        ("sales".to_string(), String::new()),
                    ]
                    .into_iter()
                    .collect(),
                },
            ),
            (
                "urgent",
                ClassifierQuestion::Bool {
                    instructions: "Does this convey urgency?".into(),
                    criteria: BoolQuestionCriteria {
                        r#true: "The user needs help soon".into(),
                        r#false: "No time pressure".into(),
                    },
                },
            ),
            (
                "severity",
                ClassifierQuestion::Score {
                    instructions: "How severe is this?".into(),
                    criteria: vec!["low".into(), "medium".into(), "high".into()],
                },
            ),
        ]),
    )
}

/// Maps multi-character labels to single tokens, as a real vocabulary would.
fn word_tokens(content: &str) -> Vec<f64> {
    fn push_part(tokens: &mut Vec<f64>, part: &str) {
        match part {
            "Yes" => tokens.push(89.0),
            "No" => tokens.push(78.0),
            other => tokens.extend(char_tokens(other)),
        }
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in content.chars() {
        if ch == '\n' {
            if !current.is_empty() {
                push_part(&mut tokens, &current);
                current.clear();
            }
            tokens.push(u32::from('\n') as f64);
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        push_part(&mut tokens, current.as_str());
    }
    tokens
}

fn options(client: &Arc<MockHttpClient>) -> ClassifierOptions {
    StreamOptions {
        api_key: Some("local".into()),
        fetch: client.fetch(),
        ..StreamOptions::default()
    }
}

fn completion_depths(client: &MockHttpClient) -> Vec<i64> {
    client
        .requests()
        .into_iter()
        .filter(|request| request.request.url.ends_with("/completion"))
        .map(|request| request.json()["n_probs"].as_i64().expect("n_probs"))
        .collect()
}

#[tokio::test]
async fn answers_choice_bool_and_score_questions_from_label_log_probabilities() {
    let server = fake_server(FakeServerOptions {
        next: Some(Arc::new(answer_by_prompt)),
        tokenize: Some(Arc::new(word_tokens)),
        template: None,
        delay: None,
    });
    let classifier_model = model();

    let result = classify(
        &classifier_model,
        &classifier_context(),
        Some(&options(&server)),
    )
    .await;

    assert!(result.error_message.is_none());
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let choice = label_probabilities(&[-1.5, -0.3, -3.0], 1.0);
    assert_eq!(
        result.answers["team"],
        ClassifierAnswer::Choice {
            choice: "technical".into(),
            probabilities: [
                ("billing".to_string(), choice[0]),
                ("technical".to_string(), choice[1]),
                ("sales".to_string(), choice[2])
            ]
            .into_iter()
            .collect(),
            confidence: peak_confidence(&choice),
        }
    );
    assert_eq!(
        result.answers["urgent"],
        ClassifierAnswer::Bool {
            probability: label_probabilities(&[-0.05, -3.0], 1.0)[0]
        }
    );
    let levels = label_probabilities(&[-4.0, -1.8, -0.2], 1.0);
    assert_eq!(
        result.answers["severity"],
        ClassifierAnswer::Score {
            score: levels[1] + 2.0 * levels[2],
            confidence: peak_confidence(&levels),
        }
    );

    let root = llama_server_root(&classifier_model.base_url);
    for request in server.requests() {
        assert!(request.request.url.starts_with(&format!("{root}/")));
        assert_eq!(request.json()["model"], json!("qwen"));
        assert_eq!(request.header("authorization"), Some("Bearer local"));
    }
    let completion = server
        .requests()
        .into_iter()
        .find(|request| request.request.url.ends_with("/completion"))
        .expect("completion");
    let body = completion.json();
    assert_eq!(body["n_predict"], json!(1));
    assert_eq!(body["n_probs"], json!(256));
    assert_eq!(body["post_sampling_probs"], json!(false));
    assert_eq!(body["cache_prompt"], json!(true));
    let template = server
        .requests()
        .into_iter()
        .find(|request| request.request.url.ends_with("/apply-template"))
        .expect("apply-template");
    assert_eq!(
        template.json()["chat_template_kwargs"],
        json!({"enable_thinking": false})
    );
}

#[test]
fn repeats_the_state_around_all_questions_and_ends_with_this_questions_labels() {
    let rendered = render_question(&classifier_context(), "team").expect("rendered");
    let state = "State:\n{\n \"message\": \"Help! My payouts have been failing for 3 days.\"\n}";
    assert_eq!(rendered.labels, vec!["A", "B", "C"]);
    assert_eq!(rendered.keys, vec!["billing", "technical", "sales"]);
    assert_eq!(
        rendered.content,
        [
            state,
            "",
            "Task: answer each of the following questions about the state.",
            "",
            "Question: Which team should handle this?",
            "",
            "Options:",
            "- billing: Payments and refunds",
            "- technical: Bugs and outages",
            "- sales",
            "",
            "Question: Does this convey urgency?",
            "",
            "Yes means: The user needs help soon",
            "No means: No time pressure",
            "",
            "Question: How severe is this?",
            "",
            "Levels:",
            "0. low",
            "1. medium",
            "2. high",
            "",
            state,
            "",
            "Question: Which team should handle this?",
            "",
            "Options:",
            "A. billing: Payments and refunds",
            "B. technical: Bugs and outages",
            "C. sales",
            "",
            "Answer with one letter.",
        ]
        .join("\n")
    );
}

#[test]
fn shares_everything_before_the_final_question_across_the_questions_of_a_request() {
    let context = classifier_context();
    let prefix = |id: &str| {
        let content = render_question(&context, id).expect("rendered").content;
        content[..content.rfind("Question:").expect("question")].to_string()
    };
    assert_eq!(prefix("urgent"), prefix("team"));
    assert_eq!(prefix("severity"), prefix("team"));
    assert!(
        render_question(&context, "urgent")
            .expect("rendered")
            .content
            .ends_with("No means: No time pressure\n\nAnswer Yes or No.")
    );
    assert!(
        render_question(&context, "severity")
            .expect("rendered")
            .content
            .ends_with("2. high\n\nAnswer with one level number.")
    );
}

#[tokio::test]
async fn divides_label_log_probabilities_by_the_temperature() {
    let server = fake_server(FakeServerOptions::default());
    let classifier_context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick one".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );
    let mut request_options = options(&server);
    request_options.temperature = Some(2.0);

    let result = classify(&model(), &classifier_context, Some(&request_options)).await;

    let expected = label_probabilities(&[-0.1 / 2.0, -2.5 / 2.0], 1.0);
    match &result.answers["pick"] {
        ClassifierAnswer::Choice { probabilities, .. } => {
            assert!((probabilities["a"] - expected[0]).abs() < 1e-15);
            assert!((probabilities["b"] - expected[1]).abs() < 1e-15);
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    assert_eq!(label_probabilities(&[-0.1, -2.5], 2.0), expected);
}

#[tokio::test]
async fn rejects_non_positive_temperatures_before_sending_requests() {
    let server = fake_server(FakeServerOptions::default());
    let mut request_options = options(&server);
    request_options.temperature = Some(0.0);

    let result = classify(&model(), &classifier_context(), Some(&request_options)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Temperature must be a positive number, got 0"))
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn retries_deeper_readouts_when_a_label_is_missing_and_fails_without_inventing_zeros() {
    let pick_context = || {
        context(
            json!({}),
            questions(vec![(
                "pick",
                ClassifierQuestion::Choice {
                    instructions: "Pick one".into(),
                    criteria: [
                        ("a".to_string(), String::new()),
                        ("b".to_string(), String::new()),
                    ]
                    .into_iter()
                    .collect(),
                },
            )]),
        )
    };
    let deep = fake_server(FakeServerOptions {
        next: Some(Arc::new(|_prompt: &str, depth: i64| {
            if depth < 4096 {
                json!({"A": -0.1})
            } else {
                json!({"A": -0.1, "B": -9})
            }
        })),
        ..FakeServerOptions::default()
    });
    let recovered = classify(&model(), &pick_context(), Some(&options(&deep))).await;
    assert_eq!(recovered.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(completion_depths(&deep), vec![256, 4096]);

    let never = fake_server(FakeServerOptions {
        next: Some(Arc::new(|_prompt: &str, _depth: i64| json!({"A": -0.1}))),
        ..FakeServerOptions::default()
    });
    let failed = classify(&model(), &pick_context(), Some(&options(&never))).await;
    assert_eq!(failed.stop_reason, ClassifierStopReason::Error);
    assert!(failed.answers.is_empty());
    assert!(failed.error_message.as_deref().is_some_and(|message| {
        message.contains("did not rank labels B for pick within the top 32768 tokens")
    }));
    assert_eq!(completion_depths(&never), vec![256, 4096, 32768]);
}

#[tokio::test]
async fn closes_a_reasoning_block_the_template_leaves_open() {
    let server = fake_server(FakeServerOptions {
        template: Some(Arc::new(|_| "<|assistant|>\n<think>".to_string())),
        ..FakeServerOptions::default()
    });
    let classifier_context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );

    classify(&model(), &classifier_context, Some(&options(&server))).await;

    let completion = server
        .requests()
        .into_iter()
        .find(|request| request.request.url.ends_with("/completion"))
        .expect("completion");
    assert_eq!(
        completion.json()["prompt"],
        json!("<|assistant|>\n<think></think>")
    );
}

#[tokio::test]
async fn reads_labels_in_reply_position_and_rejects_labels_that_are_not_one_token() {
    // A tokenizer that merges a newline with a following letter falls back to the label alone.
    let merging = fake_server(FakeServerOptions {
        tokenize: Some(Arc::new(|content: &str| {
            if content.starts_with('\n') && content.chars().count() > 1 {
                vec![1000.0]
            } else {
                char_tokens(content)
            }
        })),
        ..FakeServerOptions::default()
    });
    let pick_context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );
    let merged = classify(&model(), &pick_context, Some(&options(&merging))).await;
    assert_eq!(merged.stop_reason, ClassifierStopReason::Stop);

    // The default fake tokenizer splits "Yes" into three tokens.
    let split = fake_server(FakeServerOptions::default());
    let bool_context = context(
        json!({}),
        questions(vec![(
            "ok",
            ClassifierQuestion::Bool {
                instructions: "OK?".into(),
                criteria: BoolQuestionCriteria {
                    r#true: String::new(),
                    r#false: String::new(),
                },
            },
        )]),
    );
    let result = classify(&model(), &bool_context, Some(&options(&split))).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Label \"Yes\" is not a single token for qwen"))
    );
}

#[tokio::test]
async fn caches_label_tokens_per_server_and_model() {
    let classifier_model = model();
    let server = fake_server(FakeServerOptions::default());
    let pick_context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );
    classify(&classifier_model, &pick_context, Some(&options(&server))).await;
    let first_tokenizations = server
        .requests()
        .iter()
        .filter(|request| request.request.url.ends_with("/tokenize"))
        .count();
    classify(&classifier_model, &pick_context, Some(&options(&server))).await;

    assert!(first_tokenizations > 0);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.request.url.ends_with("/tokenize"))
            .count(),
        first_tokenizations
    );
}

#[tokio::test]
async fn validates_option_counts_before_sending_requests() {
    let server = fake_server(FakeServerOptions::default());
    let criteria: indexmap::IndexMap<String, String> = (0..63)
        .map(|index| (format!("option{index}"), String::new()))
        .collect();
    let too_many = classify(
        &model(),
        &context(
            json!({}),
            questions(vec![(
                "pick",
                ClassifierQuestion::Choice {
                    instructions: "Pick".into(),
                    criteria,
                },
            )]),
        ),
        Some(&options(&server)),
    )
    .await;
    let too_few = classify(
        &model(),
        &context(
            json!({}),
            questions(vec![(
                "rate",
                ClassifierQuestion::Score {
                    instructions: "Rate".into(),
                    criteria: vec!["only".into()],
                },
            )]),
        ),
        Some(&options(&server)),
    )
    .await;

    assert!(too_many.error_message.as_deref().is_some_and(|message| {
        message.contains("A choice question needs 2 to 62 options, got 63")
    }));
    assert!(
        too_few
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("A score question needs 2 to 10 levels, got 1"))
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn passes_completion_payloads_and_responses_through_the_request_hooks() {
    let server = fake_server(FakeServerOptions::default());
    let payloads: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let statuses: Arc<Mutex<Vec<u16>>> = Arc::new(Mutex::new(Vec::new()));
    let payloads_for_hook = Arc::clone(&payloads);
    let statuses_for_hook = Arc::clone(&statuses);
    let mut request_options = options(&server);
    request_options.on_payload = Some(Arc::new(move |payload: Value, _model: &Model| {
        payloads_for_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(payload.clone());
        let mut object = payload.as_object().cloned().unwrap_or_default();
        object.insert("id_slot".to_string(), json!(1));
        Some(Value::Object(object))
    }));
    request_options.on_response = Some(Arc::new(move |response, _model: &Model| {
        statuses_for_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(response.status);
    }));

    let context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );
    classify(&model(), &context, Some(&request_options)).await;

    let payloads = payloads.lock().unwrap().clone();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["n_predict"], json!(1));
    assert_eq!(*statuses.lock().unwrap(), vec![200]);
    let completion = server
        .requests()
        .into_iter()
        .find(|request| request.request.url.ends_with("/completion"))
        .expect("completion");
    assert_eq!(completion.json()["id_slot"], json!(1));
}

#[tokio::test]
async fn reports_server_errors_and_cancellation() {
    let client = MockHttpClient::always(MockResponse::json_status(
        400,
        json!({"error": {"message": "context overflow"}}),
    ));
    let mut request_options = options(&client);
    request_options.max_retries = Some(0);
    let failing = classify(&model(), &classifier_context(), Some(&request_options)).await;
    assert_eq!(failing.stop_reason, ClassifierStopReason::Error);
    let message = failing.error_message.expect("message");
    assert!(message.contains("llama.cpp error (400)"), "{message}");
    assert!(message.contains("context overflow"), "{message}");

    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let aborted = classify(
        &model(),
        &classifier_context(),
        Some(&StreamOptions {
            api_key: Some("local".into()),
            fetch: client.fetch(),
            signal: Some(token),
            ..StreamOptions::default()
        }),
    )
    .await;
    assert_eq!(aborted.stop_reason, ClassifierStopReason::Aborted);
}

#[tokio::test]
async fn rejects_models_for_other_classifier_apis() {
    let server = fake_server(FakeServerOptions::default());
    let other = Model {
        api: "typesafe-system-one".into(),
        ..model()
    };

    let result = classify(&other, &classifier_context(), Some(&options(&server))).await;

    assert!(result.error_message.as_deref().is_some_and(|message| {
        message.contains("Unsupported classifier API: typesafe-system-one")
    }));
    assert!(server.requests().is_empty());
}

/// 纯函数（TS 里的 `derives the server root from OpenAI-compatible base URLs` 与
/// `computes TypeSafe's confidence and expected scores`）在模块单元测试里覆盖；这里再锁定一次
/// 导出面，防止重命名。
#[test]
fn exports_the_pure_helpers() {
    assert_eq!(
        llama_server_root("http://127.0.0.1:8080/v1/"),
        "http://127.0.0.1:8080"
    );
    assert!((peak_confidence(&[0.89, 0.06, 0.05]) - 0.835).abs() < 1e-12);
    assert_eq!(
        answer_from_probabilities(
            &ClassifierQuestion::Score {
                instructions: String::new(),
                criteria: vec!["a".into(), "b".into(), "c".into()],
            },
            &["0".into(), "1".into(), "2".into()],
            &[0.2, 0.3, 0.5],
        ),
        ClassifierAnswer::Score {
            score: 1.3,
            confidence: peak_confidence(&[0.2, 0.3, 0.5]),
        }
    );
}

#[tokio::test]
async fn shares_in_flight_label_token_lookups_between_concurrent_calls() {
    // TS 的 `labelTokenCache` 缓存的是 in-flight Promise：两个并发 classify 对相同标签只发
    // 一次 `/tokenize` 请求（成功的值随后也留在缓存里）。
    let classifier_model = model();
    let server = fake_server(FakeServerOptions {
        delay: Some(std::time::Duration::from_millis(10)),
        ..FakeServerOptions::default()
    });
    let pick_context = context(
        json!({}),
        questions(vec![(
            "pick",
            ClassifierQuestion::Choice {
                instructions: "Pick".into(),
                criteria: [
                    ("a".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );

    let first_options = options(&server);
    let second_options = options(&server);
    let (first, second) = tokio::join!(
        classify(&classifier_model, &pick_context, Some(&first_options)),
        classify(&classifier_model, &pick_context, Some(&second_options)),
    );

    assert_eq!(first.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(second.stop_reason, ClassifierStopReason::Stop);
    let tokenize_requests = server
        .requests()
        .iter()
        .filter(|request| request.request.url.ends_with("/tokenize"))
        .count();
    // 两个标签 × (`"\n"`、`"\n<label>"`) = 4 次；没有 in-flight 去重时是 8 次。
    assert_eq!(tokenize_requests, 4);
}
