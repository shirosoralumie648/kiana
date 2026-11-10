//! `packages/durable/test/harness-partial-changes.test.ts` 的移植。
//!
//! TS 用 chord 的 `track()` 提交并检查 prepared ops；Rust 侧这里直接在 `serde_json::Value`
//! 上提交（`PartialChanges::commit` 只依赖 `assign_json` 的语义），但 `PartialCommit` 保留了
//! 同样的计划形状（文本是 `AssignPlan::Append`，参数是 `JsonChange::Append`），所以
//! “appends growing argument and text strings instead of reassigning them” 与 TS 一样检查每次
//! 提交的 op 形状，而不只是终态。

use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ToolCall, ToolCallKind,
};
use kiana_ai::utils::streaming_json::{JsonChange, PathSegment, ToolCallArgumentsStream};
use kiana_durable::harness::json::{AssignPlan, PlanKey};
use kiana_durable::harness::partial_changes::{PartialChanges, PartialCommit};
use serde_json::{Value, json};

/// 一个流式响应：partial 消息、事件收集器与参数流（TS 的 `setup()`）。
struct Stream {
    partial: Value,
    changes: PartialChanges,
    args: ToolCallArgumentsStream,
    generation: Value,
    /// 每次成功提交的 `PartialCommit`（TS 的 prepared ops）。
    ops: Vec<PartialCommit>,
}

fn partial_message() -> Box<AssistantMessage> {
    Box::new(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![],
        ..Default::default()
    })
}

impl Stream {
    fn new() -> Self {
        Stream {
            partial: json!({ "content": [{ "type": "text", "text": "" }] }),
            changes: PartialChanges::new(),
            args: ToolCallArgumentsStream::new(),
            generation: json!({ "attempt": 1 }),
            ops: Vec::new(),
        }
    }

    fn text(&mut self, delta: &str) {
        let text = self.partial["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        self.partial["content"][0]["text"] = Value::String(format!("{text}{delta}"));
        self.changes.observe(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: delta.to_owned(),
            partial: partial_message(),
        });
    }

    fn argument(&mut self, delta: &str) {
        if self.partial["content"].get(1).is_none() {
            self.partial["content"]
                .as_array_mut()
                .expect("content")
                .push(json!({ "type": "toolCall", "id": "c1", "name": "write", "arguments": {} }));
            self.changes.observe(&AssistantMessageEvent::ToolCallStart {
                content_index: 1,
                partial: partial_message(),
            });
        }
        let arguments = self.args.append(delta);
        self.partial["content"][1]["arguments"] =
            serde_json::to_value(arguments).expect("arguments");
        self.changes.observe(&AssistantMessageEvent::ToolCallDelta {
            content_index: 1,
            delta: delta.to_owned(),
            partial: partial_message(),
        });
    }

    /// provider 结束两个内容块。
    fn end_blocks(&mut self) {
        let text = self.partial["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        self.changes.observe(&AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: text,
            partial: partial_message(),
        });
        self.changes.observe(&AssistantMessageEvent::ToolCallEnd {
            content_index: 1,
            tool_call: ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "c1".to_owned(),
                name: "write".to_owned(),
                arguments: Default::default(),
                ..Default::default()
            },
            partial: partial_message(),
        });
    }

    /// 一次 partial 提交；`fail` 模拟被拒绝的提交（`restore()` 后丢弃）。
    fn commit(&mut self, fail: bool) {
        let mut message = self.partial.clone();
        let taken = self.changes.take(&mut message);
        if fail {
            self.changes.restore(&taken);
            return;
        }
        self.ops.push(taken.clone());
        self.changes
            .commit(&mut self.generation, &mut message, &taken);
    }

    fn committed(&self) -> (Option<Value>, Option<Value>) {
        let content = &self.generation["message"]["content"];
        (
            content.get(0).map(|block| block["text"].clone()),
            content.get(1).map(|block| block["arguments"].clone()),
        )
    }
}

/// TS 的 `text.slice(index, index + size)`：按 UTF-16 code unit 切分。
///
/// Rust 的 `&str` 不能表示半个代理对，所以切点落在代理对中间时把整个字符留在前一段
/// （TS 的 slice 会切出孤立代理；拼接后两者对解析器等价）。
fn pieces(text: &str, size: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut units = 0usize;
    for character in text.chars() {
        let length = character.len_utf16();
        if units > 0 && units + length > size {
            result.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(character);
        units += length;
        if units >= size {
            result.push(std::mem::take(&mut current));
            units = 0;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// `commit.plan` 里 `content[index][name]` 的写入计划。
fn planned<'a>(plan: &'a AssignPlan, index: usize, name: &str) -> Option<&'a AssignPlan> {
    let AssignPlan::Fields(content) = plan else {
        return None;
    };
    let AssignPlan::Fields(block) = content.get(&PlanKey::Name("content".to_owned()))? else {
        return None;
    };
    let AssignPlan::Fields(fields) = block.get(&PlanKey::Index(index))? else {
        return None;
    };
    fields.get(&PlanKey::Name(name.to_owned()))
}

fn arguments() -> Value {
    json!({
        "path": "src/a.ts",
        "n": 1234,
        "ok": true,
        "content": "line \"one\"\n\tline \\ two é 😀\n",
        "edits": [
            { "oldText": "a", "newText": "b\nc" },
            { "oldText": "", "newText": "x".repeat(50) },
        ],
        "nested": { "deep": { "list": [1, [2, "three"], { "four": null }] } },
    })
}

#[test]
fn commits_tool_call_arguments_equal_to_the_final_parse_at_every_piece_size_and_commit_rhythm() {
    let text = serde_json::to_string(&arguments()).expect("json");
    for size in [1, 2, 3, 5, 7, 16] {
        for every in [1, 3, 10] {
            let mut stream = Stream::new();
            for (index, piece) in pieces(&text, size).iter().enumerate() {
                stream.argument(piece);
                if index % every == every - 1 {
                    stream.commit(false);
                }
            }
            stream.commit(false);
            assert_eq!(
                stream.committed().1,
                Some(arguments()),
                "size {size}, every {every}"
            );
        }
    }
}

#[test]
fn handles_a_number_ending_and_a_string_starting_in_one_piece_and_escapes_split_across_pieces() {
    let mut stream = Stream::new();
    for piece in [
        "{\"n\":1",
        "2,\"s\":\"a",
        "b\\",
        "u00e9\\\"",
        "q\\u",
        "00",
        "41",
        "\",\"t\":tr",
        "ue,",
        "\"u\":\"\\ud83d",
        "\\ude00\"}",
    ] {
        stream.argument(piece);
        stream.commit(false);
    }
    assert_eq!(
        stream.committed().1,
        Some(json!({ "n": 12, "s": "abé\"qA", "t": true, "u": "😀" }))
    );
}

#[test]
fn appends_growing_argument_and_text_strings_instead_of_reassigning_them() {
    // 与 TS 一样检查每次提交的 op 形状：文本是 append，参数成员也是 append。
    let mut stream = Stream::new();
    stream.text("Hello");
    stream.argument("{\"path\":\"a.ts\",\"content\":\"x");
    stream.commit(false);
    for _ in 0..20 {
        stream.text(" more");
        stream.argument("yz");
        stream.commit(false);
    }
    for (index, commit) in stream.ops.iter().enumerate().skip(1) {
        assert_eq!(
            planned(&commit.plan, 0, "text"),
            Some(&AssignPlan::Append {
                text: " more".to_owned()
            }),
            "commit {index}"
        );
        assert_eq!(commit.tool_calls.len(), 1, "commit {index}");
        assert_eq!(
            commit.tool_calls[0].1.changes,
            vec![JsonChange::Append {
                path: vec![PathSegment::Key("content".to_owned())],
                text: "yz".to_owned(),
            }],
            "commit {index}"
        );
    }
    assert_eq!(
        stream.committed(),
        (
            Some(Value::String(format!("Hello{}", " more".repeat(20)))),
            Some(json!({ "path": "a.ts", "content": format!("x{}", "yz".repeat(20)) })),
        )
    );
}

#[test]
fn applies_the_changes_of_a_failed_commit_in_the_next_one() {
    let mut stream = Stream::new();
    stream.argument("{\"a\":\"x");
    stream.commit(false);
    stream.argument("y\",\"b\":[1");
    stream.commit(true);
    stream.argument(",2],\"c\":\"z\"}");
    stream.commit(false);
    assert_eq!(
        stream.committed().1,
        Some(json!({ "a": "xy", "b": [1, 2], "c": "z" }))
    );
}

#[test]
fn re_applies_a_failed_commits_changes_even_when_it_would_have_added_the_call() {
    let mut stream = Stream::new();
    stream.commit(false);
    stream.argument("{\"a\":\"x");
    stream.commit(true);
    stream.argument("y\"");
    stream.commit(false);
    assert_eq!(stream.committed().1, Some(json!({ "a": "xy" })));
}

#[test]
fn falls_back_to_the_snapshot_when_the_committed_message_lost_a_call_whose_changes_were_committed()
{
    let mut stream = Stream::new();
    stream.argument("{\"a\":\"x");
    stream.commit(false);
    // 另一个写者重置了 generation 的 partial。
    stream.generation = json!({ "attempt": 1 });
    stream.argument("y\"");
    stream.commit(false);
    assert_eq!(stream.committed().1, Some(json!({ "a": "xy" })));
    stream.argument(",\"b\":2}");
    stream.commit(false);
    assert_eq!(stream.committed().1, Some(json!({ "a": "xy", "b": 2 })));
}

#[test]
fn commits_the_final_arguments_after_the_call_ends_and_drops_text_appends_when_a_block_ends() {
    let mut stream = Stream::new();
    stream.text("draft");
    stream.argument("{\"a\":\"x");
    stream.commit(false);
    // provider 可能在结束时替换块文本与参数；最后一次提交写出它们结束时的值。
    stream.text(" more");
    stream.partial["content"][0]["text"] = json!("replaced");
    stream.partial["content"][1] =
        json!({ "type": "toolCall", "id": "c1", "name": "write", "arguments": { "a": "final" } });
    stream.end_blocks();
    stream.commit(false);
    assert_eq!(
        stream.committed(),
        (Some(json!("replaced")), Some(json!({ "a": "final" })))
    );
}
