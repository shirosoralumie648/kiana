//! `renderers/{read,write,edit,ls}.ts` 的移植测试。
//!
//! 断言组件渲染出的纯文本（去掉 ANSI、行尾留白）与逐字关键字/脚注，并检查需要
//! 样式区分的部分（错误色、`[invalid arg]`、edit 头部背景）。
//!
//! 组件复用差异：TS 用 `lastComponent` 原地 `setText`，Rust 渲染器每帧返回新组件；
//! 这里通过 `lastComponent` 传入上一帧组件来覆盖 write 的高亮缓存与 edit 的状态回放。

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::Duration;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::ansi::strip_ansi;
use kiana_ext_api::tool_types::{
    RenderState, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tools::renderers::edit::{EditPreview, EditRenderState, edit_renderers};
use kiana_tools::renderers::ls::ls_renderers;
use kiana_tools::renderers::read::read_renderers;
use kiana_tools::renderers::write::{WriteCallRenderComponent, write_renderers};
use kiana_tui::tui::Component;
use serde_json::json;
use tempfile::TempDir;

const WIDTH: usize = 300;

fn theme() -> Arc<Theme> {
    kiana_theme::theme::get_theme_by_name("dark").expect("bundled dark theme")
}

/// 初始化全局主题与键位表（`keyHint("app.tools.expand")` 需要）。
fn setup() -> Arc<Theme> {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        kiana_theme::theme::set_theme_instance(theme());
        let manager = kiana_tui::keybindings::KeybindingsManager::new(
            kiana_config::keybindings::keybindings().clone(),
            Default::default(),
        );
        kiana_tui::keybindings::set_keybindings(manager);
    });
    theme()
}

/// `keyText("app.tools.expand")`（当前平台）。
fn expand_key() -> String {
    kiana_ui_kit::keybinding_hints::key_text("app.tools.expand")
}

fn expand_hint() -> String {
    format!("{} to expand", expand_key())
}

/// 读取 renderer 会调用 `getReadmePath()`（读 `KIANA_PACKAGE_DIR`）；串行化这些测试。
fn read_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: 调用方持有 `read_lock()`，本测试二进制内该变量的读写已串行化。
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn result(text: &str, details: JsonValue, is_error: bool) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![text_block(text)],
        details,
        structured_content: None,
        usage: None,
        is_error: Some(is_error),
        terminate: None,
    }
}

struct ContextBuilder {
    args: JsonValue,
    cwd: std::path::PathBuf,
    expanded: bool,
    args_complete: bool,
    is_error: bool,
    output_pad: usize,
    invalidated: Arc<tokio::sync::Notify>,
}

impl ContextBuilder {
    fn new(args: JsonValue, cwd: &Path) -> Self {
        Self {
            args,
            cwd: cwd.to_path_buf(),
            expanded: false,
            args_complete: false,
            is_error: false,
            output_pad: 0,
            invalidated: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    fn args_complete(mut self, args_complete: bool) -> Self {
        self.args_complete = args_complete;
        self
    }

    fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }

    fn output_pad(mut self, output_pad: usize) -> Self {
        self.output_pad = output_pad;
        self
    }

    fn build(&self) -> ToolRenderContext {
        let invalidated = Arc::clone(&self.invalidated);
        ToolRenderContext {
            args: self.args.clone(),
            tool_call_id: "test-call".to_string(),
            invalidate: Arc::new(move || invalidated.notify_one()),
            last_component: None,
            state: new_state(),
            cwd: self.cwd.clone(),
            execution_started: true,
            args_complete: self.args_complete,
            is_partial: false,
            expanded: self.expanded,
            show_images: false,
            is_error: self.is_error,
            duration_ms: None,
            output_pad: self.output_pad,
        }
    }
}

fn new_state() -> RenderState {
    Arc::new(Mutex::new(JsonValue::Object(serde_json::Map::new())))
}

fn call_component(
    renderers: &ToolRenderers,
    args: &JsonValue,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    (renderers.render_call.as_ref().expect("renderCall"))(args, theme, context)
}

fn result_component(
    renderers: &ToolRenderers,
    result: &AgentToolResult<JsonValue>,
    expanded: bool,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    (renderers.render_result.as_ref().expect("renderResult"))(
        result,
        &ToolRenderResultOptions {
            expanded,
            is_partial: false,
        },
        theme,
        context,
    )
}

/// ANSI 之外的纯文本；每行去掉右侧留白（Text 会补满 width）。
fn plain(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| strip_ansi(line).trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_text(component: &mut Box<dyn Component>) -> String {
    plain(&component.render(WIDTH))
}

/// Box/Container 组件：去掉上下 padding 造成的空行。
fn render_block(component: &mut Box<dyn Component>) -> String {
    plain(&component.render(WIDTH))
        .trim_matches('\n')
        .to_string()
}

fn bg_prefix(theme: &Theme, token: ThemeBg) -> String {
    let wrapped = theme.bg(token, "");
    wrapped
        .strip_suffix("\x1b[49m")
        .unwrap_or(&wrapped)
        .to_string()
}

fn edit_state(state: &RenderState) -> EditRenderState {
    let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .get("editRender")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

// =============================================================================
// read
// =============================================================================

#[test]
fn read_call_uses_file_path_first_then_path() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();

    let context = ContextBuilder::new(
        json!({ "path": "a.txt", "file_path": "b.ts", "offset": 2, "limit": 3 }),
        temp.path(),
    )
    .build();
    let args = context.args.clone();
    let mut component = call_component(&renderers, &args, &theme, &context);
    assert_eq!(render_text(&mut component), "read b.ts:2-4");

    let context =
        ContextBuilder::new(json!({ "file_path": null, "path": "a.txt" }), temp.path()).build();
    let args = context.args.clone();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "read a.txt"
    );

    let context = ContextBuilder::new(json!({}), temp.path()).build();
    let args = context.args.clone();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "read ..."
    );

    // 非字符串 path → [invalid arg]（错误色）。
    let context = ContextBuilder::new(json!({ "path": 42 }), temp.path()).build();
    let args = context.args.clone();
    let mut component = call_component(&renderers, &args, &theme, &context);
    let raw = component.render(WIDTH).join("\n");
    assert!(raw.contains(&theme.fg(ThemeColor::Error, "[invalid arg]")));
    assert_eq!(render_text(&mut component), "read [invalid arg]");

    // 非字符串 offset 让行号范围整体消失（TS 的 formatReadLineRange）。
    let context =
        ContextBuilder::new(json!({ "path": "a.txt", "offset": "abc" }), temp.path()).build();
    let args = context.args.clone();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "read a.txt"
    );
}

#[test]
fn read_call_ignores_output_pad_and_expands_compacts() {
    let theme = setup();
    let _guard = read_lock();
    let temp = TempDir::new().expect("temp");
    let skill = temp.path().join("skills").join("review").join("SKILL.md");
    fs::create_dir_all(skill.parent().expect("parent")).expect("mkdir");
    fs::write(&skill, "instructions").expect("write");
    let renderers = read_renderers();

    let args = json!({ "path": skill });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .output_pad(5)
        .build();
    let text = render_text(&mut call_component(&renderers, &args, &theme, &context));
    assert_eq!(
        text,
        format!("[skill] review ({})", expand_hint()),
        "outputPad 不作用于非 self-render 的 Text"
    );

    // offset/limit 的警示色范围拼在 label 后、提示前。
    let args = json!({ "path": skill, "offset": 1, "limit": 5 });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        format!("[skill] review:1-5 ({})", expand_hint())
    );

    // 展开后不做紧凑分类。
    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        format!("read {}:1-5", skill.display())
    );
}

#[test]
fn read_call_compact_docs_and_resource_classes() {
    let theme = setup();
    let _guard = read_lock();
    let temp = TempDir::new().expect("temp");
    let package = temp.path().join("pkg");
    fs::create_dir_all(package.join("docs")).expect("mkdir docs");
    fs::write(package.join("README.md"), "readme").expect("write");
    let guide = package.join("docs").join("guide.md");
    fs::write(&guide, "guide").expect("write");
    let _env = EnvGuard::set("KIANA_PACKAGE_DIR", &package);
    let renderers = read_renderers();

    let args = json!({ "path": guide });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        format!("read docs docs/guide.md ({})", expand_hint())
    );

    // package 根目录本身不是 docs 分类（relative 为空）。
    let args = json!({ "path": package });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        format!("read {}", package.display())
    );

    // 资源文件名 → read resource + 相对 cwd 的路径（posix 分隔符）。
    let agent_file = temp.path().join("CLAUDE.md");
    fs::write(&agent_file, "rules").expect("write");
    let args = json!({ "path": agent_file });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        format!("read resource CLAUDE.md ({})", expand_hint())
    );
}

#[test]
fn read_result_is_hidden_when_collapsed_and_not_an_error() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();
    let args = json!({ "path": "a.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let result = result("line1\nline2", JsonValue::Null, false);
    let mut component = result_component(&renderers, &result, false, &theme, &context);
    assert_eq!(render_text(&mut component), "");
}

#[test]
fn read_result_expanded_keeps_leading_newline_and_trims_trailing_blanks() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();
    let args = json!({ "path": "a.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .build();
    let tool_result = result("line1\nline2\n\n", JsonValue::Null, false);
    let mut component = result_component(&renderers, &tool_result, true, &theme, &context);
    assert_eq!(render_text(&mut component), "\nline1\nline2");

    // 错误在折叠时也显示，且不走语法高亮。
    let args = json!({ "path": "a.ts" });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .with_error(true)
        .build();
    let tool_result = result("ENOENT: missing", JsonValue::Null, true);
    let mut component = result_component(&renderers, &tool_result, false, &theme, &context);
    assert_eq!(render_text(&mut component), "\nENOENT: missing");
}

#[test]
fn read_result_previews_ten_lines_for_collapsed_errors() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();
    let args = json!({ "path": "a.txt" });
    let text = (1..=15)
        .map(|index| format!("line{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tool_result = result(&text, JsonValue::Null, true);

    // `formatReadResult`：折叠视图只对错误渲染，并且 `maxLines = 10`。
    let context = ContextBuilder::new(args.clone(), temp.path())
        .with_error(true)
        .build();
    let mut collapsed = result_component(&renderers, &tool_result, false, &theme, &context);
    let output = render_text(&mut collapsed);
    assert!(output.starts_with("\nline1\n"));
    assert!(output.ends_with(&format!("\n... (5 more lines, {})", expand_hint())));
    assert!(!output.contains("line11"));

    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .with_error(true)
        .build();
    let mut expanded = result_component(&renderers, &tool_result, true, &theme, &context);
    let output = render_text(&mut expanded);
    assert!(output.contains("line11"));
    assert!(!output.contains("more lines"));
}

fn truncation_details(
    truncated_by: JsonValue,
    max_lines: u64,
    max_bytes: u64,
    output_lines: u64,
    total_lines: u64,
    first_line_exceeds_limit: bool,
) -> JsonValue {
    json!({
        "truncation": {
            "content": "x",
            "truncated": true,
            "truncatedBy": truncated_by,
            "totalLines": total_lines,
            "totalBytes": 100,
            "outputLines": output_lines,
            "outputBytes": 10,
            "lastLinePartial": false,
            "firstLineExceedsLimit": first_line_exceeds_limit,
            "maxLines": max_lines,
            "maxBytes": max_bytes,
        }
    })
}

#[test]
fn read_result_truncation_footers_are_verbatim() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();
    let args = json!({ "path": "a.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .build();

    let cases = [
        (
            truncation_details(json!("bytes"), 2000, 51200, 1, 2, true),
            "\n[First line exceeds 50.0KB limit]",
        ),
        (
            truncation_details(json!("lines"), 2000, 51200, 10, 2500, false),
            "\n[Truncated: showing 10 of 2500 lines (2000 line limit)]",
        ),
        (
            truncation_details(json!("bytes"), 2000, 51200, 50, 500, false),
            "\n[Truncated: 50 lines shown (50.0KB limit)]",
        ),
    ];
    for (details, expected) in cases {
        let result = result("line1", details, false);
        let mut component = result_component(&renderers, &result, true, &theme, &context);
        let output = render_text(&mut component);
        assert!(output.ends_with(expected), "{output:?}");
    }

    // 未截断的 details 不产生脚注。
    let details = json!({ "truncation": {
        "content": "x", "truncated": false, "truncatedBy": JsonValue::Null,
        "totalLines": 1, "totalBytes": 1, "outputLines": 1, "outputBytes": 1,
        "lastLinePartial": false, "firstLineExceedsLimit": false, "maxLines": 2000, "maxBytes": 51200
    }});
    let result = result("line1", details, false);
    let mut component = result_component(&renderers, &result, true, &theme, &context);
    assert_eq!(render_text(&mut component), "\nline1");
}

// =============================================================================
// write
// =============================================================================

#[test]
fn write_call_previews_content_verbatim() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = write_renderers();

    let args = json!({ "path": "x.txt", "content": "line1\nline2" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "write x.txt\n\nline1\nline2"
    );

    // 尾部空行被裁掉；空内容只有标题行。
    let args = json!({ "path": "x.txt", "content": "a\n\n\n" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "write x.txt\n\na"
    );
    let args = json!({ "path": "x.txt", "content": "" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "write x.txt"
    );

    // file_path 优先；null content 视为空。
    let args = json!({ "path": "a.txt", "file_path": "b.txt", "content": JsonValue::Null });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "write b.txt"
    );

    // 展开时显示全部行。
    let content = (1..=12)
        .map(|index| format!("line{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let args = json!({ "path": "x.txt", "content": content });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .build();
    let output = render_text(&mut call_component(&renderers, &args, &theme, &context));
    assert!(output.contains("line12"));
}

#[test]
fn write_call_invalid_content_is_an_error() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = write_renderers();
    let args = json!({ "path": "x.txt", "content": 42 });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let mut component = call_component(&renderers, &args, &theme, &context);
    let raw = component.render(WIDTH).join("\n");
    assert!(raw.contains(&theme.fg(ThemeColor::Error, "[invalid content arg - expected string]")));
    assert_eq!(
        render_text(&mut component),
        "write x.txt\n\n[invalid content arg - expected string]"
    );
}

#[test]
fn write_call_previews_ten_of_many_lines_with_total() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = write_renderers();
    let content = (1..=12)
        .map(|index| format!("line{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let args = json!({ "path": "x.txt", "content": content });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let output = render_text(&mut call_component(&renderers, &args, &theme, &context));
    assert!(output.contains("line10"));
    assert!(!output.contains("line11"));
    assert!(output.ends_with(&format!(
        "\n... (2 more lines, 12 total, {})",
        expand_hint()
    )));
}

#[test]
fn write_partial_updates_reuse_the_highlight_cache() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = write_renderers();

    let args = json!({ "path": "src/main.ts", "content": "fn ma" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let mut first = call_component(&renderers, &args, &theme, &context);
    let _ = first.render(WIDTH);

    let args = json!({ "path": "src/main.ts", "content": "fn main() {\n  println!(\"hi\");\n}\n" });
    let mut context = ContextBuilder::new(args.clone(), temp.path()).build();
    context.last_component = Some(first);
    let mut incremental = call_component(&renderers, &args, &theme, &context);
    let incremental_cache = incremental
        .as_any()
        .and_then(|any| any.downcast_ref::<WriteCallRenderComponent>())
        .expect("write call component")
        .cache
        .clone()
        .expect("cache");
    assert_eq!(
        incremental_cache.raw_content,
        "fn main() {\n  println!(\"hi\");\n}\n"
    );
    let incremental_text = render_text(&mut incremental);

    // 参数完整时一次性重建；输出必须与增量缓存完全一致。
    let context = ContextBuilder::new(args.clone(), temp.path())
        .args_complete(true)
        .build();
    let mut full = call_component(&renderers, &args, &theme, &context);
    assert_eq!(incremental_text, render_text(&mut full));
    assert!(incremental_text.contains("fn main()"));
}

#[test]
fn write_result_only_renders_errors() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = write_renderers();
    let args = json!({ "path": "x.txt", "content": "x" });

    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let ok = result("Successfully wrote to x.txt", JsonValue::Null, false);
    let mut component = result_component(&renderers, &ok, false, &theme, &context);
    assert_eq!(render_text(&mut component), "");

    let context = ContextBuilder::new(args.clone(), temp.path())
        .with_error(true)
        .build();
    let error = result("EACCES: permission denied", JsonValue::Null, true);
    let mut component = result_component(&renderers, &error, false, &theme, &context);
    assert_eq!(render_text(&mut component), "\nEACCES: permission denied");

    // 空文本的错误结果什么都不渲染。
    let empty = result("", JsonValue::Null, true);
    let mut component = result_component(&renderers, &empty, false, &theme, &context);
    assert_eq!(render_text(&mut component), "");
}

// =============================================================================
// ls
// =============================================================================

#[test]
fn ls_call_renders_path_and_limit() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = ls_renderers();

    let cases: Vec<(JsonValue, &str)> = vec![
        (json!({}), "ls ."),
        (json!({ "path": "" }), "ls ."),
        (json!({ "path": "src" }), "ls src"),
        (json!({ "path": "src", "limit": 3 }), "ls src (limit 3)"),
        (json!({ "limit": 2.5 }), "ls . (limit 2.5)"),
        (json!({ "limit": 1e21 }), "ls . (limit 1e+21)"),
        (json!({ "limit": "abc" }), "ls . (limit abc)"),
        (json!({ "limit": JsonValue::Null }), "ls . (limit null)"),
        (json!({ "limit": true }), "ls . (limit true)"),
    ];
    for (args, expected) in cases {
        let context = ContextBuilder::new(args.clone(), temp.path()).build();
        assert_eq!(
            render_text(&mut call_component(&renderers, &args, &theme, &context)),
            expected,
            "{args}"
        );
    }

    // 非字符串 path → [invalid arg]。
    let args = json!({ "path": 42 });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_text(&mut call_component(&renderers, &args, &theme, &context)),
        "ls [invalid arg]"
    );
}

#[test]
fn ls_result_previews_and_warns() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = ls_renderers();
    let args = json!({});

    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let ok = result("a\nb", JsonValue::Null, false);
    assert_eq!(
        render_text(&mut result_component(
            &renderers, &ok, false, &theme, &context
        )),
        "\na\nb"
    );

    // 空输出（含只有空白）什么都不渲染。
    let empty = result("   ", JsonValue::Null, false);
    assert_eq!(
        render_text(&mut result_component(
            &renderers, &empty, false, &theme, &context
        )),
        ""
    );

    // 20 行预览。
    let text = (1..=25)
        .map(|index| format!("entry{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let long = result(&text, JsonValue::Null, false);
    let output = render_text(&mut result_component(
        &renderers, &long, false, &theme, &context,
    ));
    assert!(output.contains("entry20"));
    assert!(!output.contains("entry21"));
    assert!(output.ends_with(&format!("\n... (5 more lines, {})", expand_hint())));
    let output = render_text(&mut result_component(
        &renderers, &long, true, &theme, &context,
    ));
    assert!(output.contains("entry25"));

    // 通知（条目数 / 字节）；0 在 JS 里是 falsy。
    let entry_limit = result("a", json!({ "entryLimitReached": 500 }), false);
    assert_eq!(
        render_text(&mut result_component(
            &renderers,
            &entry_limit,
            false,
            &theme,
            &context
        )),
        "\na\n[Truncated: 500 entries limit]"
    );
    let zero = result("a", json!({ "entryLimitReached": 0 }), false);
    assert_eq!(
        render_text(&mut result_component(
            &renderers, &zero, false, &theme, &context
        )),
        "\na"
    );

    // TS 用 JS 的 `String.prototype.trim()`：去掉 U+FEFF，保留 U+0085。
    let js_trim = result("\u{feff}alpha\nbeta\u{85}", JsonValue::Null, false);
    let raw = result_component(&renderers, &js_trim, false, &theme, &context).render(WIDTH);
    let joined = raw.join("\n");
    assert!(!joined.contains('\u{feff}'), "{joined:?}");
    assert!(joined.contains('\u{85}'), "{joined:?}");
    let truncation = result(
        "a",
        json!({ "truncation": {
            "content": "a", "truncated": true, "truncatedBy": "bytes",
            "totalLines": 1, "totalBytes": 60000, "outputLines": 1, "outputBytes": 51200,
            "lastLinePartial": false, "firstLineExceedsLimit": false,
            "maxLines": 2000, "maxBytes": 51200
        }}),
        false,
    );
    assert_eq!(
        render_text(&mut result_component(
            &renderers,
            &truncation,
            false,
            &theme,
            &context
        )),
        "\na\n[Truncated: 50.0KB limit]"
    );
    let both = result(
        "a",
        json!({
            "entryLimitReached": 500,
            "truncation": {
                "content": "a", "truncated": true, "truncatedBy": "bytes",
                "totalLines": 1, "totalBytes": 60000, "outputLines": 1, "outputBytes": 51200,
                "lastLinePartial": false, "firstLineExceedsLimit": false,
                "maxLines": 2000, "maxBytes": 51200
            }
        }),
        false,
    );
    assert_eq!(
        render_text(&mut result_component(
            &renderers, &both, false, &theme, &context
        )),
        "\na\n[Truncated: 500 entries limit, 50.0KB limit]"
    );
}

// =============================================================================
// edit
// =============================================================================

fn edit_args(path: &Path) -> JsonValue {
    json!({
        "path": path.to_string_lossy(),
        "edits": [{ "oldText": "world", "newText": "there" }],
    })
}

/// TS 的 `getEditHeaderBg(..., theme)` 用**传入**的 theme；Rust 曾误用进程级全局主题。
#[test]
fn edit_call_header_background_comes_from_the_theme_argument() {
    let installed = setup();
    let light = kiana_theme::theme::get_theme_by_name("light").expect("bundled light theme");
    let dark_pending = bg_prefix(&installed, ThemeBg::ToolPendingBg);
    let light_pending = bg_prefix(&light, ThemeBg::ToolPendingBg);
    assert_ne!(
        dark_pending, light_pending,
        "the two bundled themes must differ for this test to be meaningful"
    );

    let temp = TempDir::new().expect("temp");
    let renderers = edit_renderers();
    let args = edit_args(&temp.path().join("preview.txt"));
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let mut component = call_component(&renderers, &args, &light, &context);
    let raw = component.render(WIDTH);
    assert!(
        raw.iter().all(|line| line.starts_with(&light_pending)),
        "header must use the passed theme, not the global one: {raw:?}"
    );
    assert!(raw.iter().all(|line| !line.starts_with(&dark_pending)));
}

#[test]
fn edit_call_header_respects_output_pad() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = edit_renderers();

    let args = json!({});
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_block(&mut call_component(&renderers, &args, &theme, &context)),
        "edit ..."
    );

    let args = json!({ "path": "file.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .output_pad(0)
        .build();
    assert_eq!(
        render_block(&mut call_component(&renderers, &args, &theme, &context)),
        "edit file.txt"
    );

    let context = ContextBuilder::new(args.clone(), temp.path())
        .output_pad(3)
        .build();
    assert_eq!(
        render_block(&mut call_component(&renderers, &args, &theme, &context)),
        "   edit file.txt"
    );

    // file_path 优先（`file_path ?? path`）。
    let args = json!({ "path": "a.txt", "file_path": "b.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    assert_eq!(
        render_block(&mut call_component(&renderers, &args, &theme, &context)),
        "edit b.txt"
    );
}

#[tokio::test]
async fn edit_call_previews_the_diff_and_switches_the_header_background() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("preview.txt");
    fs::write(&file, "hello world\n").expect("write");
    let args = edit_args(&file);
    let renderers = edit_renderers();

    let notified = Arc::new(tokio::sync::Notify::new());
    let target = Arc::clone(&notified);
    let mut context = ContextBuilder::new(args.clone(), temp.path())
        .args_complete(true)
        .build();
    context.invalidate = Arc::new(move || target.notify_one());
    let state = Arc::clone(&context.state);

    let mut component = call_component(&renderers, &args, &theme, &context);
    let raw_before = component.render(WIDTH);
    let pending = bg_prefix(&theme, ThemeBg::ToolPendingBg);
    assert!(
        raw_before.iter().all(|line| line.starts_with(&pending)),
        "header should use the pending background before the preview settles"
    );
    tokio::time::timeout(Duration::from_secs(5), notified.notified())
        .await
        .expect("preview invalidated");

    let state_after = edit_state(&state);
    assert!(
        state_after
            .preview
            .as_ref()
            .is_some_and(|preview| matches!(preview, EditPreview::Result(_)))
    );
    assert!(!state_after.preview_pending);

    let mut component = call_component(&renderers, &args, &theme, &context);
    let raw_after = component.render(WIDTH);
    let success = bg_prefix(&theme, ThemeBg::ToolSuccessBg);
    assert!(raw_after.iter().all(|line| line.starts_with(&success)));
    let output = plain(&raw_after);
    assert!(output.contains("edit "), "{output}");
    assert!(output.contains("hello world"), "{output}");
    assert!(output.contains("hello there"), "{output}");
}

#[tokio::test]
async fn edit_call_previews_errors_with_the_error_background() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let missing = temp.path().join("missing.txt");
    let args = edit_args(&missing);
    let renderers = edit_renderers();

    let notified = Arc::new(tokio::sync::Notify::new());
    let target = Arc::clone(&notified);
    let mut context = ContextBuilder::new(args.clone(), temp.path())
        .args_complete(true)
        .build();
    context.invalidate = Arc::new(move || target.notify_one());
    let state = Arc::clone(&context.state);
    let mut component = call_component(&renderers, &args, &theme, &context);
    let _ = component.render(WIDTH);
    tokio::time::timeout(Duration::from_secs(5), notified.notified())
        .await
        .expect("preview invalidated");

    let state_after = edit_state(&state);
    let preview_error = match state_after.preview.as_ref().expect("preview") {
        EditPreview::Error(error) => error.error.clone(),
        EditPreview::Result(_) => panic!("expected an error preview"),
    };
    assert!(preview_error.contains("Could not edit file:"));
    assert!(preview_error.contains("ENOENT"));

    let mut component = call_component(&renderers, &args, &theme, &context);
    let raw = component.render(WIDTH);
    let error_bg = bg_prefix(&theme, ThemeBg::ToolErrorBg);
    assert!(raw.iter().all(|line| line.starts_with(&error_bg)));
    let output = plain(&raw);
    assert!(output.contains("Could not edit file:"), "{output}");
    assert!(output.contains("ENOENT"), "{output}");

    // 结果里的同一条错误不再重复渲染（上下文带着同一个 state 与 isError=true）。
    let error_result = result(&preview_error, JsonValue::Null, true);
    let mut result_context = ContextBuilder::new(args.clone(), temp.path())
        .with_error(true)
        .build();
    result_context.state = Arc::clone(&context.state);
    let mut result_view =
        result_component(&renderers, &error_result, false, &theme, &result_context);
    assert_eq!(render_text(&mut result_view), "");

    // 不同的错误则正常渲染。
    let other_error = result("different failure", JsonValue::Null, true);
    let mut result_view =
        result_component(&renderers, &other_error, false, &theme, &result_context);
    assert_eq!(render_text(&mut result_view), "\ndifferent failure");
}

#[tokio::test]
async fn edit_call_does_not_spawn_a_preview_before_the_arguments_are_complete() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("partial.txt");
    fs::write(&file, "hello world\n").expect("write");
    let args = edit_args(&file);
    let renderers = edit_renderers();

    let context = ContextBuilder::new(args.clone(), temp.path())
        .args_complete(false)
        .build();
    let state = Arc::clone(&context.state);
    let mut component = call_component(&renderers, &args, &theme, &context);
    let _ = component.render(WIDTH);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let edit_state = edit_state(&state);
    assert!(edit_state.preview.is_none());
    assert!(!edit_state.preview_pending);
    assert!(edit_state.call_rendered);
}

#[tokio::test]
async fn edit_result_updates_the_preview_and_renders_the_diff_through_the_call_component() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("result.txt");
    fs::write(&file, "hello world\n").expect("write");
    let args = edit_args(&file);
    let renderers = edit_renderers();
    let diff = " 1 hello world\n-1 hello world\n+1 hello there";

    // 调用先渲染（参数未完整 → 不触发异步预演），结果再落定。
    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let state = Arc::clone(&context.state);
    let _ = call_component(&renderers, &args, &theme, &context);

    let details = json!({ "diff": diff, "patch": "--- a\n+++ b\n", "firstChangedLine": 1 });
    let ok = result("Successfully replaced 1 block(s)", details, false);
    let mut result_view = result_component(&renderers, &ok, false, &theme, &context);
    assert_eq!(
        render_text(&mut result_view),
        "",
        "结果与已同步的预览相同 → diff 只在调用组件里显示一次"
    );

    let state_after = edit_state(&state);
    match state_after.preview.as_ref().expect("preview set by result") {
        EditPreview::Result(result) => {
            assert_eq!(result.diff, diff);
            assert_eq!(result.first_changed_line, Some(1));
        }
        EditPreview::Error(_) => panic!("expected a result preview"),
    }

    let mut call = call_component(&renderers, &args, &theme, &context);
    let raw = call.render(WIDTH);
    assert!(
        raw.iter()
            .all(|line| line.starts_with(&bg_prefix(&theme, ThemeBg::ToolSuccessBg)))
    );
    let output = plain(&raw);
    assert!(output.contains("hello there"), "{output}");
}

#[test]
fn edit_result_without_a_call_component_renders_the_diff_in_the_result() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("replay.txt");
    let args = edit_args(&file);
    let renderers = edit_renderers();
    let diff = " 1 a\n-2 b\n+2 B\n 3 c";
    let details = json!({ "diff": diff, "patch": "", "firstChangedLine": 2 });
    let ok = result("Successfully replaced 1 block(s)", details, false);

    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let state = Arc::clone(&context.state);
    let mut component = result_component(&renderers, &ok, false, &theme, &context);
    let output = render_text(&mut component);
    assert!(output.contains("-2 b"), "{output}");
    assert!(output.contains("+2 B"), "{output}");

    // TS 在没有 callComponent 时不会创建状态。
    let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(guard.get("editRender").is_none());
}

/// TS: `if (resultDiff && resultDiff !== previewDiff)` —— 空字符串是 falsy。
/// 回放一个 `details.diff === ""` 的（伪造）结果时不能渲染空白的 diff 行。
#[test]
fn edit_result_without_a_call_component_and_empty_diff_renders_nothing() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let args = edit_args(&temp.path().join("empty-diff.txt"));
    let renderers = edit_renderers();
    let details = json!({ "diff": "", "patch": "", "firstChangedLine": 1 });
    let ok = result("Successfully replaced 1 block(s)", details, false);

    let context = ContextBuilder::new(args.clone(), temp.path()).build();
    let mut component = result_component(&renderers, &ok, false, &theme, &context);
    assert_eq!(render_text(&mut component), "");
}

#[test]
fn edit_result_error_uses_the_error_color_and_output_pad() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let args = edit_args(&temp.path().join("error.txt"));
    let renderers = edit_renderers();
    let error = result(
        "Could not find the exact text in file",
        JsonValue::Null,
        true,
    );

    let context = ContextBuilder::new(args.clone(), temp.path())
        .with_error(true)
        .output_pad(2)
        .build();
    let mut component = result_component(&renderers, &error, false, &theme, &context);
    let raw = component.render(WIDTH).join("\n");
    assert!(raw.contains(&theme.fg(ThemeColor::Error, "Could not find the exact text in file")));
    assert_eq!(
        render_block(&mut component),
        "  Could not find the exact text in file"
    );

    // 非错误结果即使带 details 也不重复错误。
    let empty = result("", JsonValue::Null, true);
    let mut component = result_component(&renderers, &empty, false, &theme, &context);
    assert_eq!(render_text(&mut component), "");
}

// =============================================================================
// 渲染器接线
// =============================================================================

#[test]
fn tool_definitions_wire_the_renderers() {
    let read = kiana_tools::read::create_read_tool_definition("/", None);
    assert!(read.render_call.is_some());
    assert!(read.render_result.is_some());
    assert_eq!(read.render_shell, None);

    let write = kiana_tools::write::create_write_tool_definition("/", None);
    assert!(write.render_call.is_some());
    assert!(write.render_result.is_some());

    let ls = kiana_tools::ls::create_ls_tool_definition("/", None);
    assert!(ls.render_call.is_some());
    assert!(ls.render_result.is_some());

    let edit = kiana_tools::edit::create_edit_tool_definition("/", None);
    assert_eq!(
        edit.render_shell,
        Some(kiana_ext_api::tool_types::RenderShell::SelfRender)
    );
    assert!(edit.render_call.is_some());
    assert!(edit.render_result.is_some());
}

// =============================================================================
// 宽松 details（TS 属性访问语义）
// =============================================================================

#[test]
fn read_renderer_tolerates_partial_details_like_ts() {
    // 回归：旧版本/扩展写入的 details 可能缺少 firstLineExceedsLimit 等字段；TS 逐个
    // 属性读取，截断脚注照常渲染（`maxLines ?? DEFAULT_MAX_LINES`）。
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = read_renderers();
    let args = json!({ "path": "a.txt" });
    let context = ContextBuilder::new(args.clone(), temp.path())
        .expanded(true)
        .build();

    let details = json!({ "truncation": {
        "truncated": true,
        "truncatedBy": "lines",
        "totalLines": 2500,
        "outputLines": 10
        // 缺 maxLines -> DEFAULT_MAX_LINES。
    }});
    let result = result("line1", details, false);
    let mut component = result_component(&renderers, &result, true, &theme, &context);
    let output = render_text(&mut component);
    assert!(
        output.ends_with("\n[Truncated: showing 10 of 2500 lines (2000 line limit)]"),
        "{output:?}"
    );
}

#[test]
fn ls_renderer_tolerates_partial_details_like_ts() {
    let theme = setup();
    let temp = TempDir::new().expect("temp");
    let renderers = ls_renderers();
    let context = ContextBuilder::new(json!({}), temp.path()).build();

    let details = json!({
        "entryLimitReached": 500,
        // 缺 maxBytes -> DEFAULT_MAX_BYTES。
        "truncation": { "truncated": true }
    });
    let result = result("a\nb", details, false);
    let mut component = result_component(&renderers, &result, false, &theme, &context);
    let output = render_text(&mut component);
    assert_eq!(
        output,
        "\na\nb\n[Truncated: 500 entries limit, 50.0KB limit]"
    );
}
