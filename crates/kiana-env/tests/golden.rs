//! 对拍测试：`tests/fixtures/protocol.json` 由参照实现（`/home/ctyun/code/kiana_rev/kiana/packages/env/daemon`
//! 的 `kiana-env`）生成——同一串请求的逐条回复，字段名与消息文本逐字记录。
//!
//! 夹具由 `/home/ctyun/kiana-work/scratch/kiana-env-daemon/gen-fixture.mjs` 生成（`node gen-fixture.mjs
//! <参照二进制> crates/kiana-env/tests/fixtures/protocol.json`），对机器相关字段做了归一化：`<root>` 临时目录、
//! `<root-name>` 根目录自己的名字、`<random>` 的 mkdtemp 目录名、`<uuid>` 的溢出文件名；`info` 只留
//! name/kind/size（其余是文件系统的 mtime/dev/ino）。
//!
//! **不要**把夹具与原始字节比较：参照 daemon 的 serde_json 未开 `preserve_order`（键按字典序），
//! 本工作区开了 `preserve_order`（键按插入序），客户端解析 JSON，因此两者都合法。

mod common;

use common::{Daemon, EVENT, TempDir};
use serde_json::{Value, json};
use std::time::Duration;

const FIXTURE: &str = include_str!("fixtures/protocol.json");

/// 把夹具里的 `<root>` 换成实际临时目录。
fn fill(value: &Value, root: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace("<root>", root)),
        Value::Array(items) => Value::Array(items.iter().map(|item| fill(item, root)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), fill(value, root)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 反向归一化：实际输出里出现的临时目录、随机名与 pid。
fn normalize(value: &Value, root: &str) -> Value {
    let root_name = root.rsplit('/').next().unwrap_or(root);
    let normalize_text = |text: &str| -> String {
        let mut text = text.replace(root, "<root>");
        text = text.replace(root_name, "<root-name>");
        let mut result = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let rest = &text[index..];
            if let Some(prefix) = ["prefix-", "tmp-"]
                .into_iter()
                .find(|prefix| rest.starts_with(prefix))
            {
                let after = &rest[prefix.len()..];
                let random: String = after
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                if random.len() == 6 {
                    result.push_str(prefix);
                    result.push_str("<random>");
                    index += prefix.len() + 6;
                    continue;
                }
                let _ = prefix;
            }
            if let Some(after) = rest.strip_prefix("kiana-output-") {
                let mut length = 0;
                for character in after.chars() {
                    if character.is_ascii_hexdigit() || character == '-' {
                        length += character.len_utf8();
                    } else {
                        break;
                    }
                }
                if length == 36 {
                    result.push_str("kiana-output-<uuid>");
                    index += "kiana-output-".len() + 36;
                    continue;
                }
            }
            let character = rest.chars().next().expect("a character");
            result.push(character);
            index += character.len_utf8();
        }
        result
    };
    match value {
        Value::String(text) => Value::String(normalize_text(text)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| normalize(item, root)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize(value, root)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `info`/`stat` 里只有 name/kind/size 是跨机器稳定的。
fn trim_info(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.contains_key("kind") && map.contains_key("name") => json!({
            "name": map["name"],
            "kind": map["kind"],
            "size": map["size"],
        }),
        other => other.clone(),
    }
}

#[test]
fn replays_the_reference_daemon_session() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture JSON");
    let dir = TempDir::new("golden");
    // The same tree the fixture generator made before sending its requests.
    dir.mkdir("dir");
    dir.write("a.txt", b"alpha\nbeta\n");
    dir.write("dir/b.txt", b"b\n");
    let root = dir.str();
    // The daemon's temp directories (spill files) are created in TMPDIR; keep the fixture independent of the
    // machine's by only checking the shape of `spillPath`.
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();

    let requests = fixture["requests"].as_array().expect("requests");
    let replies = fixture["replies"].as_array().expect("replies");
    assert_eq!(requests.len(), replies.len());

    for (request, expected) in requests.iter().zip(replies) {
        let id = request["id"].as_u64().expect("id") as u32;
        let op = request["op"].as_str().expect("op");
        let mut json = fill(&request["json"], &root);
        common::object(&mut json).insert("op".into(), json!(op));
        let payload = match &request["payload"] {
            Value::String(text) => text.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        daemon.send_frame(common::REQUEST, id, &json, &payload);
        // Collect events until the reply; `exec` output is compared as text per stream.
        let (events, reply) = daemon.reply_with_events(id);
        let mut stdout = String::new();
        let mut stderr = String::new();
        for event in &events {
            assert_eq!(event.kind, EVENT);
            match event.json["stream"].as_str() {
                Some("stdout") => stdout.push_str(&String::from_utf8_lossy(&event.payload)),
                Some("stderr") => stderr.push_str(&String::from_utf8_lossy(&event.payload)),
                _ => {}
            }
        }
        let expected_kind = if expected.get("error").is_some() {
            common::ERROR
        } else {
            common::RESULT
        };
        assert_eq!(
            reply.kind, expected_kind,
            "request {id} ({op}): reply {:?}, expected {expected}",
            reply.json
        );
        if expected_kind == common::ERROR {
            let actual = normalize(&reply.json, &root);
            assert_eq!(
                actual, expected["error"],
                "request {id} ({op}) error differs"
            );
        } else {
            let mut actual = normalize(&reply.json, &root);
            let is_info = actual
                .as_object()
                .is_some_and(|map| map.contains_key("kind") && map.contains_key("name"));
            if is_info {
                actual = trim_info(&actual);
            }
            if let Value::Object(map) = &mut actual {
                // `info`/`stat` carry the file system's timestamps and identities; only name/kind/size are stable.
                for key in ["info", "stat"] {
                    if let Some(entry) = map.get(key).cloned() {
                        map.insert(key.into(), trim_info(&entry));
                    }
                }
            }
            if let Value::Object(map) = &mut actual
                && let Some(spill) = map.remove("spillPath")
            {
                let spill = spill.as_str().unwrap().to_string();
                assert!(spill.contains("/tmp-"), "spill path {spill}");
                assert!(spill.ends_with(".log"), "spill path {spill}");
                assert_eq!(
                    expected.get("spillPath"),
                    Some(&json!(true)),
                    "request {id} ({op}) has a spill file the reference did not"
                );
            } else {
                assert!(
                    expected.get("spillPath").is_none(),
                    "request {id} ({op}) lost its spill file"
                );
            }
            let mut wanted = expected["result"].clone();
            if op == "hello" {
                // The version is the npm package version (`KIANA_ENV_VERSION` when injected, else the
                // `NPM_VERSION` literal in build.rs; tests/version.rs pins it to package.json).
                assert!(actual["version"].is_string());
                assert_eq!(actual["protocol"], wanted["protocol"]);
                for key in ["os", "arch", "separator", "driveCwds", "features"] {
                    assert_eq!(actual[key], wanted[key], "hello.{key}");
                }
                assert!(actual["home"].is_string(), "hello.home");
                assert!(actual["cwd"].is_string(), "hello.cwd");
                assert!(actual["tmpdir"].is_string(), "hello.tmpdir");
                assert!(actual["pid"].is_u64(), "hello.pid");
                for key in ["hasHome", "hasCwd", "hasTmpdir", "hasPid"] {
                    assert_eq!(wanted[key], json!(true), "fixture hello.{key}");
                }
                continue;
            }
            if let Value::Object(map) = &mut wanted
                && map.contains_key("kind")
                && map.contains_key("name")
            {
                wanted = trim_info(&wanted);
            }
            assert_eq!(actual, wanted, "request {id} ({op}) result differs");
            if let Some(text) = expected.get("payload").and_then(Value::as_str) {
                assert_eq!(
                    normalize(&json!(String::from_utf8_lossy(&reply.payload)), &root),
                    *text,
                    "request {id} ({op}) payload differs"
                );
            } else {
                assert!(
                    reply.payload.is_empty(),
                    "request {id} ({op}) has a payload"
                );
            }
            if let Some(text) = expected.get("stdout").and_then(Value::as_str) {
                assert_eq!(
                    normalize(&json!(stdout), &root),
                    *text,
                    "request {id} stdout"
                );
            }
            if let Some(text) = expected.get("stderr").and_then(Value::as_str) {
                assert_eq!(
                    normalize(&json!(stderr), &root),
                    *text,
                    "request {id} stderr"
                );
            }
        }
    }
    daemon.close_stdin();
    let status = daemon.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0));
}
