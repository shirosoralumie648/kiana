//! TS: `test/session-info-modified-timestamp.test.ts`（1 例）。

mod common;

use common::*;
use kiana_session_core::session_manager::{FileEntry, SessionManager, entry_json_string};

fn create_session_file(path: &str) {
    let mut header = session_header("test-session", Some("/tmp"), None);
    header.timestamp = Some(kiana_ai::model_catalog::js_date_to_iso_string(0));
    std::fs::write(
        path,
        format!("{}\n", entry_json_string(&FileEntry::Header(header))),
    )
    .unwrap();

    // SessionManager 只有见到第一条消息后才开始持久化；补一条最小 assistant 条目。
    let mut manager = SessionManager::open(path, None, None).unwrap();
    manager.append_message(assistant_message("hi", 0)).unwrap();
}

/// TS: "uses last user/assistant message timestamp instead of file mtime"
#[tokio::test]
async fn uses_last_user_assistant_message_timestamp_instead_of_file_mtime() {
    let temp = temp_dir("kiana-session-modified-");
    let file_path = format!("{}/modified.jsonl", temp.path().to_string_lossy());
    create_session_file(&file_path);

    let before = std::fs::metadata(&file_path).unwrap();
    let before_mtime = before
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    // 保证文件 mtime 与消息时间戳即使粗粒度文件系统也能不同。
    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut manager = SessionManager::open(&file_path, None, None).unwrap();
    let message_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    manager
        .append_message(assistant_message("later", message_time))
        .unwrap();

    let session_dir = std::path::Path::new(&file_path)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let sessions = SessionManager::list("/tmp", Some(&session_dir), None, None)
        .await
        .unwrap();
    let session = sessions
        .iter()
        .find(|session| session.path == file_path)
        .expect("session info");
    assert_eq!(session.modified, message_time as f64);
    assert_ne!(session.modified as i64, before_mtime);
}

/// 回归（评审 #6）：`buildSessionInfo` 对畸形会话文件的处理规则（当前稳定契约）：
/// - 首个可解析行不是对象/不是 header（如 `[]`）→ 整个文件被拒绝；
/// - `session_info.name` 非字符串 → `name?.trim()` 抛错，整个会话被拒绝；
/// - 消息条目没有 `content` 键 → 不贡献 `modified`（回退到 header 时间戳）。
///
/// 期望值由 TS 参考实现生成（vitest probe `list`）。
#[tokio::test]
async fn build_session_info_matches_typescript_on_malformed_files() {
    let temp = temp_dir("kiana-session-info-malformed-");
    let dir = temp.path().to_string_lossy().into_owned();
    std::fs::write(
        format!("{dir}/a.jsonl"),
        "[]\n{\"type\":\"session\",\"id\":\"x\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/b.jsonl"),
        "{\"type\":\"session\",\"id\":\"y\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n{\"type\":\"session_info\",\"id\":\"n\",\"parentId\":null,\"timestamp\":\"2026-01-01T00:00:01.000Z\",\"name\":42}\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/c.jsonl"),
        "{\"type\":\"session\",\"id\":\"z\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n{\"type\":\"message\",\"id\":\"m\",\"parentId\":null,\"timestamp\":\"2026-01-02T00:00:00.000Z\",\"message\":{\"role\":\"user\",\"timestamp\":999}}\n",
    )
    .unwrap();

    let sessions = SessionManager::list("/tmp", Some(&dir), None, None)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1, "only c.jsonl is a readable session");
    let session = &sessions[0];
    assert_eq!(session.id, "z");
    assert_eq!(session.name, None);
    assert_eq!(session.modified, 1_767_225_600_000.0);
    assert_eq!(session.message_count, 1);
    assert_eq!(session.first_message, "(no messages)");
}

/// 回归（评审 round 2 #1）：`extractTextContent` 与 TS 一样不做类型校验——
/// `content: null`、对象、数组里的 `null` 块都会抛 TypeError，`buildSessionInfo` 因此把整个
/// 会话视为不可读（不出现在 `list` 结果里）；`block.text` 缺失/非字符串按 JS
/// `join(" ")` 渲染（`undefined`/`null` → 空串，其它 → `String(value)`）。
///
/// 期望值由 TS 参考实现生成（vitest probe，`SessionManager.list(cwd, dir)`）。
#[tokio::test]
async fn build_session_info_matches_typescript_for_unvalidated_message_content() {
    let temp = temp_dir("kiana-session-info-content-");
    let dir = temp.path().to_string_lossy().into_owned();
    let header = |id: &str| {
        format!(
            r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}}"#
        )
    };
    let msg = |id: &str, message: &str| {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{message}}}"#
        )
    };
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("a.jsonl", vec![header("a"), msg("a1", r#"{"role":"user","content":null,"timestamp":1}"#)]),
        ("b.jsonl", vec![header("b"), msg("b1", r#"{"role":"user","content":{"weird":true},"timestamp":1}"#)]),
        ("c.jsonl", vec![header("c"), msg("c1", "null")]),
        ("d.jsonl", vec![header("d"), msg("d1", r#"{"role":"user","content":[null,{"type":"text","text":"x"}],"timestamp":1}"#)]),
        ("e.jsonl", vec![header("e"), msg("e1", r#"{"role":"user","content":[{"type":"text"},{"type":"text","text":"x"}],"timestamp":1}"#)]),
        ("f.jsonl", vec![header("f"), msg("f1", r#"{"role":"assistant","content":null,"timestamp":1}"#)]),
        ("g.jsonl", vec![header("g"), r#"{"type":"session_info","id":"g1","parentId":null,"timestamp":"2025-01-01T00:00:02.000Z","name":5}"#.to_string()]),
        ("h.jsonl", vec![header("h"), msg("h1", r#"{"role":"user","content":"hi","timestamp":1}"#)]),
        ("i.jsonl", vec![header("i"), msg("i1", r#""oops""#)]),
        ("j.jsonl", vec![header("j"), msg("j1", r#"{"role":"user","content":"hi"}"#)]),
        ("k.jsonl", vec![header("k"), msg("k1", r#"{"role":"system","content":"sys"}"#), msg("k2", r#"{"role":"user","content":"hi","timestamp":1}"#)]),
        ("l.jsonl", vec![header("l"), r#"{"type":"thinking_level_change","id":"l1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","thinkingLevel":5}"#.to_string()]),
        ("m.jsonl", vec![header("m"), msg("m1", r#"{"role":"user","content":[{"type":"text","text":5},{"type":"text","text":null},{"type":"text","text":{"a":1}},{"type":"image","data":"x"}],"timestamp":1}"#)]),
        ("n.jsonl", vec![header("n"), msg("n1", r#"{"role":"user","content":[5,"str",true,{"type":"text","text":"y"}],"timestamp":1}"#)]),
    ];
    for (name, lines) in cases {
        std::fs::write(format!("{dir}/{name}"), lines.join("\n") + "\n").unwrap();
    }

    let sessions = SessionManager::list("/tmp", Some(&dir), None, None)
        .await
        .unwrap();
    let summary: Vec<(String, String, String, usize)> = sessions
        .iter()
        .map(|session| {
            (
                session.id.clone(),
                session.first_message.clone(),
                session.all_messages_text.clone(),
                session.message_count,
            )
        })
        .collect();
    // 预期行为：a/b/c/d/f/g 被丢弃；i（message "oops"）保留但没有文本。
    assert_eq!(
        summary,
        vec![
            ("j".to_string(), "hi".to_string(), "hi".to_string(), 1),
            (
                "l".to_string(),
                "(no messages)".to_string(),
                String::new(),
                0
            ),
            (
                "i".to_string(),
                "(no messages)".to_string(),
                String::new(),
                1
            ),
            ("n".to_string(), "y".to_string(), "y".to_string(), 1),
            (
                "m".to_string(),
                "5  [object Object]".to_string(),
                "5  [object Object]".to_string(),
                1
            ),
            ("k".to_string(), "hi".to_string(), "hi".to_string(), 2),
            ("h".to_string(), "hi".to_string(), "hi".to_string(), 1),
            ("e".to_string(), " x".to_string(), " x".to_string(), 1),
        ]
    );
}

/// 回归（评审 round 2 #8）：`created` 使用 `new Date(header.timestamp)` 的强制转换
/// （`null` → 0、布尔 → 0/1、数字 → 毫秒并 TimeClip 截断、无效字符串/缺省 → `NaN`）。
///
/// 期望值由 TS 参考实现生成（vitest probe）。
#[tokio::test]
async fn build_session_info_uses_js_date_coercion_for_created() {
    let temp = temp_dir("kiana-session-created-");
    let dir = temp.path().to_string_lossy().into_owned();
    let cases: [(&str, &str); 4] = [
        ("null", "null"),
        ("true", "true"),
        ("num", "1234.9"),
        ("bad", "\"not-a-date\""),
    ];
    for (name, timestamp) in cases {
        std::fs::write(
            format!("{dir}/{name}.jsonl"),
            format!(
                r#"{{"type":"session","version":3,"id":"{name}","timestamp":{timestamp},"cwd":"/tmp"}}
{{"type":"message","id":"m","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{{"role":"user","content":"hi","timestamp":1}}}}
"#
            ),
        )
        .unwrap();
    }
    let sessions = SessionManager::list("/tmp", Some(&dir), None, None)
        .await
        .unwrap();
    let created = |id: &str| {
        sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.created)
            .expect("session")
    };
    assert_eq!(created("null"), 0.0);
    assert_eq!(created("true"), 1.0);
    assert_eq!(created("num"), 1234.0);
    assert!(created("bad").is_nan());
}

/// 回归（评审 round 2 #8）：TS 的 `buildSessionInfo` 原样返回 `header.id` /
/// `header.parentSession`（不校验类型）。Rust 的 `SessionInfo` 用 `String` 承载，非字符串值按
/// JS `String(value)` 转换（`5` → `"5"`、`7` → `"7"`；`null`/缺省保持 `None`）。
#[tokio::test]
async fn build_session_info_stringifies_raw_id_and_parent_session() {
    let temp = temp_dir("kiana-session-raw-header-");
    let dir = temp.path().to_string_lossy().into_owned();
    std::fs::write(
        format!("{dir}/raw.jsonl"),
        concat!(
            r#"{"type":"session","version":3,"id":5,"timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp","parentSession":7}"#,
            "\n",
            r#"{"type":"message","id":"m","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#,
            "\n",
        ),
    )
    .unwrap();
    let sessions = SessionManager::list("/tmp", Some(&dir), None, None)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "5");
    assert_eq!(sessions[0].parent_session_path.as_deref(), Some("7"));
}
