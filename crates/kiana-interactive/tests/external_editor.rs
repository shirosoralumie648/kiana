//! `external-editor.test.ts` 的 Rust 移植。
//!
//! 用 `fake-external-editor.mjs`（逐字复制）作为外部编辑器，通过 node 启动，以便捕获
//! 启动参数并断言临时目录前缀、权限、文件名、内容清理与失败路径。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use kiana_interactive::external_editor::{
    ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor,
};
use serde_json::Value;
use tempfile::Builder;

struct Capture {
    file_path: String,
    content: String,
    entries: Vec<String>,
    directory_mode: u64,
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-external-editor.mjs")
}

fn node_executable() -> String {
    static NODE: OnceLock<String> = OnceLock::new();
    NODE.get_or_init(|| std::env::var("NODE").unwrap_or_else(|_| "node".to_string()))
        .clone()
}

fn run_external_editor(flag: Option<&str>) -> (ExternalEditorResult, Capture) {
    let test_directory = Builder::new()
        .prefix("kiana-external-editor-test-")
        .tempdir()
        .expect("temp dir");
    let capture_path = test_directory.path().join("capture.json");
    let mut command = format!(
        "{} {} {}",
        node_executable(),
        fixture_path().display(),
        capture_path.display()
    );
    if let Some(flag) = flag {
        command.push(' ');
        command.push_str(flag);
    }
    // tokio::process 需要 reactor：TS 调用方总在事件循环里，Rust 测试自建 current_thread runtime。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime
        .block_on(edit_in_external_editor(ExternalEditorOptions {
            command,
            content: "original".to_string(),
        }))
        .expect("external editor result");
    let raw = std::fs::read_to_string(&capture_path).expect("capture file");
    let parsed: Value = serde_json::from_str(&raw).expect("capture json");
    let capture = Capture {
        file_path: parsed["filePath"].as_str().expect("filePath").to_string(),
        content: parsed["content"].as_str().expect("content").to_string(),
        entries: parsed["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry.as_str().expect("entry").to_string())
            .collect(),
        directory_mode: parsed["directoryMode"].as_u64().expect("directoryMode"),
    };
    (result, capture)
}

#[test]
fn edits_a_prompt_inside_a_private_temporary_directory() {
    let (result, capture) = run_external_editor(None);
    let file_path = PathBuf::from(&capture.file_path);
    let directory = file_path.parent().expect("directory");

    assert_eq!(
        result,
        ExternalEditorResult::Complete {
            content: "edited".to_string()
        }
    );
    assert_eq!(
        directory.parent().expect("tmpdir"),
        std::env::temp_dir().as_path()
    );
    let directory_name = directory
        .file_name()
        .expect("directory name")
        .to_string_lossy();
    assert!(
        directory_name.starts_with("kiana-editor-") && directory_name.len() > "kiana-editor-".len(),
        "unexpected directory name: {directory_name}"
    );
    assert_eq!(
        file_path.file_name().expect("file name").to_string_lossy(),
        "prompt.md"
    );
    assert_eq!(capture.entries, vec!["prompt.md".to_string()]);
    assert_eq!(capture.content, "original");
    if !cfg!(windows) {
        assert_eq!(
            capture.directory_mode & 0o077,
            0,
            "mode={:o}",
            capture.directory_mode
        );
    }
    assert!(!directory.exists());
}

#[test]
fn keeps_the_original_content_when_the_editor_exits_unsuccessfully() {
    let (result, capture) = run_external_editor(Some("--fail"));

    assert_eq!(result, ExternalEditorResult::Failed);
    assert!(
        !Path::new(&capture.file_path)
            .parent()
            .expect("directory")
            .exists()
    );
}

#[test]
fn returns_empty_content_when_the_editor_clears_the_prompt() {
    let (result, _capture) = run_external_editor(Some("--empty"));

    assert_eq!(
        result,
        ExternalEditorResult::Complete {
            content: String::new()
        }
    );
}
