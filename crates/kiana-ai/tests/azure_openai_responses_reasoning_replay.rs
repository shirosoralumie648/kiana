//! `azure-openai-responses-reasoning-replay.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::{
    convert_responses_messages, process_responses_stream,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{AssistantMessage, Model};
use serde_json::{Value, json};

fn create_model() -> Model {
    Model {
        id: "gpt-5-mini".into(),
        name: "GPT-5 Mini".into(),
        api: "azure-openai-responses".into(),
        provider: "azure".into(),
        base_url: "https://example.invalid".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

async fn run(done_item: Value, completed_item: Value) -> AssistantMessage {
    let model = create_model();
    let mut output = mock_http::responses_output(&model);
    let stream = AssistantMessageEventStream::new();
    process_responses_stream(
        mock_http::event_stream(vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "sequence_number": 0,
                "item": { "type": "reasoning", "id": done_item["id"], "summary": [] },
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "sequence_number": 1,
                "item": done_item,
            }),
            json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": { "id": "resp_test", "status": "completed", "output": [completed_item] },
            }),
        ]),
        &mut output,
        &stream,
        &model,
        None,
    )
    .await
    .expect("process");
    output
}

fn replayed_reasoning(output: &AssistantMessage) -> Value {
    let context = mock_http::context(vec![
        mock_http::user("first"),
        kiana_ai::types::Message::Assistant(output.clone()),
        mock_http::user("follow-up"),
    ]);
    let input =
        convert_responses_messages(&create_model(), &context, &["azure"], None).expect("convert");
    input
        .into_iter()
        .find(|item| item["type"] == json!("reasoning"))
        .expect("reasoning item")
}

#[tokio::test]
async fn preserves_existing_encrypted_content_from_output_item_done() {
    let done_item = json!({ "type": "reasoning", "id": "rs_done", "summary": [], "encrypted_content": "from-output-item-done" });
    let completed_item = json!({ "type": "reasoning", "id": "rs_done", "summary": [], "encrypted_content": "from-response-completed" });
    let output = run(done_item, completed_item).await;
    let replayed = replayed_reasoning(&output);
    assert_eq!(replayed["type"], json!("reasoning"));
    assert_eq!(replayed["id"], json!("rs_done"));
    assert_eq!(
        replayed["encrypted_content"],
        json!("from-output-item-done")
    );
}

#[tokio::test]
async fn fills_encrypted_content_when_output_item_done_omitted_it() {
    let done_item = json!({ "type": "reasoning", "id": "rs_missing", "summary": [] });
    let completed_item = json!({ "type": "reasoning", "id": "rs_missing", "summary": [], "encrypted_content": "from-response-completed" });
    let output = run(done_item, completed_item).await;
    let replayed = replayed_reasoning(&output);
    assert_eq!(replayed["type"], json!("reasoning"));
    assert_eq!(replayed["id"], json!("rs_missing"));
    assert_eq!(
        replayed["encrypted_content"],
        json!("from-response-completed")
    );
}
