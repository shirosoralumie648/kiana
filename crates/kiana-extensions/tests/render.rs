//! 原生 UI 类的渲染字节（设计文档 §10.3 的夹具）。
//!
//! TS 参考：`packages/tui/src/utils.ts`、`packages/tui/src/components/{text,box}.ts`、
//! `packages/tui/src/tui.ts`（Container）、`packages/coding-agent/src/modes/interactive/
//! theme/dark.json`（经 `theme.ts` 的转换）。夹具文件在 `tests/fixtures/render/`：
//!
//! - `text_samples.json`：6 个样本在 `render(20)` / `truncateToWidth(18, "…")` /
//!   `visibleWidth` 上的逐字结果（设计文档 §10.3 列出的同一组值）；
//! - `ansi_layout.json`：`Text`/`Box`/`Container` 组合与 `Editor` 子类的 ANSI 字节；
//! - `theme_dark.json`：dark 主题全部 49 个 foreground token 的 `getFgAnsi` 字节。
//!   由 Node 直接跑 TS 参考生成（`getThemeByName("dark").getFgAnsi(token)`，token 表来自
//!   `theme-tokens.ts` 的 `slot === "foreground"` 项），不是从 Rust `kiana-theme` 反推的，
//!   因此该测试能发现 kiana-theme 与 TS 的偏差。

use std::rc::Rc;

use kiana_extensions::engine::{self, EngineConfig, JsEngine};
use serde_json::Value;

struct Fixture {
    engine: Rc<JsEngine>,
}

impl Fixture {
    fn new() -> Self {
        let engine = Rc::new(JsEngine::with_config(EngineConfig::new()).expect("create engine"));
        engine::install(&engine).expect("install prelude");
        kiana_extensions::ui::install_attached(&engine).expect("install ui adapter");
        Fixture { engine }
    }

    fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.engine
            .eval_script(source, "<render-test>")
            .expect("eval")
    }

    /// 求值成 JSON（`serde_json::Value` 没有 `FromJs`）。
    fn eval_json(&self, expression: &str) -> Value {
        let text: String = self.eval(&format!("JSON.stringify({expression})"));
        serde_json::from_str(&text).expect("parse json")
    }
}

/// 全局主题是进程级状态：主题相关测试串行执行，避免互相看到对方切换的主题。
static THEME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn theme_guard() -> std::sync::MutexGuard<'static, ()> {
    THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn fixture_json(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/render/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("read {path}: {error}");
    });
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

#[test]
fn text_samples_match_the_ts_reference() {
    let fixture = Fixture::new();
    let samples = fixture_json("text_samples.json");
    let samples = samples.as_array().expect("array");
    assert_eq!(samples.len(), 6);
    for sample in samples {
        let input = sample["input"].as_str().expect("input");
        let source = serde_json::to_string(input).expect("quote input");
        let actual: Value = fixture.eval_json(&format!(
            r#"(() => {{
                const text = {source};
                const truncated = __kiana_tui.truncateToWidth(text, 18, "…", false);
                return {{
                    visibleWidth: __kiana_tui.visibleWidth(text),
                    render: new __kiana_tui.Text(text, 0, 0).render(20),
                    truncate: truncated,
                    truncateWidth: __kiana_tui.visibleWidth(truncated),
                }};
            }})()"#
        ));
        assert_eq!(
            actual["visibleWidth"], sample["visibleWidth"],
            "visibleWidth of {input:?}"
        );
        assert_eq!(actual["render"], sample["render"], "render of {input:?}");
        assert_eq!(
            actual["truncate"], sample["truncate"],
            "truncateToWidth of {input:?}"
        );
        assert_eq!(
            actual["truncateWidth"], sample["truncateWidth"],
            "truncateToWidth width of {input:?}"
        );
    }
}

#[test]
fn text_box_container_bytes_match_the_fixture() {
    let fixture = Fixture::new();
    let expected = fixture_json("ansi_layout.json");
    let actual = fixture.eval_json(
        r#"(() => {
            const bg = (text) => "\x1b[48;2;20;30;40m" + text + "\x1b[49m";
            const text = new __kiana_tui.Text("\x1b[31mred\x1b[0m", 1, 0);
            const box = new __kiana_tui.Box(1, 1, bg);
            box.addChild(text);
            const container = new __kiana_tui.Container();
            container.addChild(box);
            container.addChild(new __kiana_tui.Spacer(1));
            container.addChild(new __kiana_tui.TruncatedText("truncated text here", 0, 0));
            return container.render(12);
        })()"#,
    );
    assert_eq!(actual, expected["container"]);
}

#[test]
fn editor_subclass_super_render_and_border_color() {
    let fixture = Fixture::new();
    let expected = fixture_json("ansi_layout.json");
    let actual = fixture.eval_json(
        r#"(() => {
            const theme = {
                borderColor: (text) => "\x1b[35m" + text + "\x1b[0m",
                selectList: {
                    selectedText: (t) => t,
                    selectedPrefix: (t) => t,
                    description: (t) => t,
                    scrollInfo: (t) => t,
                    noMatch: (t) => t,
                },
            };
            const base = new __kiana_tui.Editor(null, theme, { paddingX: 0 });
            base.setText("hi");
            const baseLines = base.render(10);
            class Override extends __kiana_tui.Editor {
                renderTopBorder(width, hidden) {
                    return "TOP(" + width + "," + hidden + ")[" + this.borderColor("x") + "]";
                }
                renderBottomBorder() { return "BOTTOM"; }
            }
            const over = new Override(null, theme, {});
            over.setText("yo");
            return {
                base: baseLines,
                override: over.render(10),
                border: typeof base.borderColor,
            };
        })()"#,
    );
    assert_eq!(actual, expected["editor"]);
    // 基类边框经 `borderColor` 着色（§5.4）；覆写的两行替换了哨兵行。
    let base = actual["base"].as_array().expect("base");
    assert_eq!(base[0], "\u{1b}[35m──────────\u{1b}[0m");
    let overridden = actual["override"].as_array().expect("override");
    assert_eq!(overridden[0], "TOP(10,0)[\u{1b}[35mx\u{1b}[0m]");
    assert_eq!(overridden[2], "BOTTOM");
}

#[test]
fn theme_tokens_match_the_dark_fixture_and_the_rust_theme() {
    let _guard = theme_guard();
    let fixture = Fixture::new();
    let expected = fixture_json("theme_dark.json");
    let expected = expected.as_object().expect("object");
    let theme = kiana_theme::theme::get_theme_by_name("dark").expect("dark theme");
    assert_eq!(expected.len(), 49);
    for color in kiana_theme::ThemeColor::ALL {
        let token = color.as_str();
        let want = expected
            .get(token)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("missing fixture token {token}"));
        assert_eq!(want, theme.get_fg_ansi(color), "rust vs fixture: {token}");
        let actual: String = fixture.eval(&format!(
            "globalThis.__kiana_tui_theme = globalThis[Symbol.for(\"@earendil-works/kiana-coding-agent:theme\")]; globalThis.__kiana_tui_theme.getFgAnsi({token:?})"
        ));
        assert_eq!(actual, want, "js vs fixture: {token}");
        let styled: String = fixture.eval(&format!(
            "globalThis.__kiana_tui_theme.fg({token:?}, \"x\")"
        ));
        assert_eq!(styled, theme.fg(color, "x"), "fg: {token}");
    }
}

#[test]
fn theme_symbol_aliases_share_the_native_instance() {
    let _guard = theme_guard();
    let fixture = Fixture::new();
    let same: bool = fixture.eval(
        r#"(() => {
            const current = globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")];
            const legacy = globalThis[Symbol.for("@mariozechner/kiana-coding-agent:theme")];
            return current !== undefined && current === legacy;
        })()"#,
    );
    assert!(same);
}

#[test]
fn container_renders_children_in_order() {
    let fixture = Fixture::new();
    let lines: Vec<String> = fixture.eval(
        r#"(() => {
            const container = new __kiana_tui.Container();
            container.addChild(new __kiana_tui.Text("one", 0, 0));
            container.addChild(new __kiana_tui.Text("two", 0, 0));
            return container.render(6);
        })()"#,
    );
    assert_eq!(
        lines,
        vec!["one   ".to_owned(), "two   ".to_owned()],
        "Container concatenates child lines"
    );
}

#[test]
fn cursor_markers_and_key_constants_match_the_ts_source() {
    let fixture = Fixture::new();
    let marker: String = fixture.eval("globalThis.__kiana_tui.CURSOR_MARKER");
    let start: String = fixture.eval("globalThis.__kiana_tui.FAKE_CURSOR_START");
    let end: String = fixture.eval("globalThis.__kiana_tui.FAKE_CURSOR_END");
    assert_eq!(marker, "\u{1b}_kiana:c\u{7}");
    assert_eq!(start, "\u{1b}_kiana:fc\u{7}");
    assert_eq!(end, "\u{1b}_kiana:/fc\u{7}");
    let rendered: String = fixture.eval("__kiana_tui.renderFakeCursor('x')");
    assert_eq!(rendered, format!("{start}x{end}"));

    let escape: String = fixture.eval("__kiana_tui.Key.escape");
    assert_eq!(escape, "escape");
    let ctrl: String = fixture.eval("__kiana_tui.Key.ctrlShiftAlt('x')");
    assert_eq!(ctrl, "ctrl+shift+alt+x");
    let parsed: String = fixture.eval("String(__kiana_tui.parseKey('\u{1b}'))");
    assert_eq!(parsed, "escape");
    let unknown: String = fixture.eval("String(__kiana_tui.parseKey('not-a-key'))");
    assert_eq!(unknown, "undefined");
    let matches: bool = fixture.eval("__kiana_tui.matchesKey('\u{1b}', 'escape')");
    assert!(matches);
    let release: bool = fixture.eval("__kiana_tui.isKeyRelease('\u{1b}[2:3~')");
    assert!(release);
}

/// `CustomEditor`（`custom-editor.ts`）：actionHandlers/onEscape/onCtrlD/onPasteImage/
/// onExtensionShortcut 的判定顺序与 `super.handleInput` 的落点。
#[test]
fn custom_editor_handle_input_order() {
    let fixture = Fixture::new();
    let result: Value = fixture.eval_json(
        r#"(() => {
            const theme = { borderColor: (text) => text, selectList: {} };
            const matches = (data, action) => {
                if (action === "app.clipboard.pasteImage") return data === "paste";
                if (action === "app.interrupt") return data === "esc";
                if (action === "app.exit") return data === "ctrl+d";
                if (action === "app.model.next") return data === "next";
                return false;
            };
            const calls = [];
            const editor = new __kiana_tui.CustomEditor(null, theme, { matches }, {
                paddingX: 0,
                embedWorkingStatus: true,
            });
            editor.onPasteImage = () => calls.push("paste");
            editor.onEscape = () => calls.push("escape");
            editor.onCtrlD = () => calls.push("exit");
            editor.onAction("app.interrupt", () => calls.push("interrupt-action"));
            editor.onAction("app.model.next", () => calls.push("model-next"));
            editor.onExtensionShortcut = (data) => {
                if (data === "ext") { calls.push("extension"); return true; }
                return false;
            };

            editor.handleInput("ext");
            editor.handleInput("paste");
            editor.handleInput("esc");
            editor.handleInput("ctrl+d");
            editor.handleInput("next");
            editor.handleInput("a");
            return {
                calls,
                text: editor.getText(),
                embedWorkingStatus: editor.embedWorkingStatus,
                actionHandlers: editor.actionHandlers instanceof Map,
            };
        })()"#,
    );
    assert_eq!(
        result["calls"],
        serde_json::json!(["extension", "paste", "escape", "exit", "model-next"])
    );
    // 未命中的按键落到 `super.handleInput`（原生 Editor 插入文本）。
    assert_eq!(result["text"], "a");
    assert_eq!(result["embedWorkingStatus"], true);
    assert_eq!(result["actionHandlers"], true);
}

/// `CustomEditor.renderTopBorder` 的 working-status 逻辑（`custom-editor.ts:38-79`）。
#[test]
fn custom_editor_working_status_border() {
    let fixture = Fixture::new();
    let result: Value = fixture.eval_json(
        r#"(() => {
            const theme = { borderColor: (text) => text, selectList: {} };
            const editor = new __kiana_tui.CustomEditor(null, theme, { matches: () => false }, {
                paddingX: 0,
                embedWorkingStatus: true,
            });
            editor.setWorkingStatusIndicator({
                renderInBorder(width) { return "XX"; },
                renderSpinnerInBorder(width) { return "S"; },
            });
            return {
                withoutOverflow: editor.renderTopBorder(20, 0),
                withOverflow: editor.renderTopBorder(20, 3),
                plain: new __kiana_tui.CustomEditor(null, theme, { matches: () => false })
                    .renderTopBorder(10, 0),
            };
        })()"#,
    );
    assert_eq!(result["withoutOverflow"], "── XX ──────────────");
    // 溢出标签放不下 → 退回 spinner；状态宽度 1，剩余横线 15 条。
    assert_eq!(result["withOverflow"], "── S ───────────────");
    // 未开启 embedWorkingStatus → 基类边框（10 条横线）。
    assert_eq!(result["plain"], "──────────");
}

// ===========================================================================
// 回归：评审发现的偏差（原生类包装层）
// ===========================================================================

/// `Box.removeChild` 按对象身份移除子组件（`tui.ts:370-377`），且不调用 `dispose()`。
#[test]
fn box_remove_child_removes_the_rendered_child() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            globalThis.__boxDisposed = false;
            const box = new __kiana_tui.Box(0, 0);
            const child = {
                render(width) { return ["x"]; },
                dispose() { globalThis.__boxDisposed = true; },
            };
            box.addChild(child);
            const before = box.render(3);
            box.removeChild(child);
            const after = box.render(3);
            return {
                before,
                after,
                disposed: globalThis.__boxDisposed,
                unknownRemove: (box.removeChild({ render() { return []; } }), "ok"),
            };
        })()"#,
    );
    assert_eq!(result["before"], serde_json::json!(["x  "]));
    assert!(
        result["after"].as_array().expect("array").is_empty(),
        "the child must be gone: {}",
        result["after"]
    );
    assert_eq!(result["disposed"], false, "removeChild must not dispose");
    assert_eq!(result["unknownRemove"], "ok");
}

/// `Editor` 的 JS 子类持有 `this.tui`（`editor.ts:307、375`；rainbow-editor.ts:56 用它
/// `this.tui.requestRender()`）。
#[test]
fn editor_wrapper_exposes_the_tui() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            globalThis.__renderCalls = 0;
            const tui = {
                terminal: { rows: 30 },
                requestRender() { globalThis.__renderCalls += 1; },
            };
            const theme = { borderColor: (text) => text, selectList: {} };
            const editor = new __kiana_tui.Editor(tui, theme, { paddingX: 0 });
            const same = editor.tui === tui;
            editor.tui.requestRender();
            class Rainbow extends __kiana_tui.Editor {
                constructor(tui, theme) { super(tui, theme, {}); }
                render(width) {
                    this.tui.requestRender();
                    return super.render(width);
                }
            }
            const rainbow = new Rainbow(tui, theme);
            rainbow.render(10);
            return {
                same,
                calls: globalThis.__renderCalls,
                rows: editor.tui.terminal.rows,
            };
        })()"#,
    );
    assert_eq!(result["same"], true);
    // 显式一次 + 子类 render 一次。
    assert_eq!(result["calls"], 2);
    assert_eq!(result["rows"], 30);
}

/// `Markdown` 的 `transform(markdown, availableWidth)` 与主题的 `highlightCode(code, lang)`
/// / `codeBlockIndent`（`markdown.ts:222-251`）。
#[test]
fn markdown_transform_and_highlight_code_are_bridged() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const identity = (text) => text;
            const theme = {
                heading: identity, link: identity, linkUrl: identity, code: identity,
                codeBlock: identity, codeBlockBorder: identity, quote: identity,
                quoteBorder: identity, hr: identity, listBullet: identity, bold: identity,
                italic: identity, strikethrough: identity, underline: identity,
                highlightCode(code, lang) {
                    return code.split("\n").map((line) => "HL(" + String(lang) + "):" + line);
                },
                codeBlockIndent: "> ",
            };
            const markdown = new __kiana_tui.Markdown(
                "```js\nlet x = 1;\n```",
                0,
                0,
                theme,
                undefined,
                {
                    transform(text, width) {
                        return text.replace("let x", "let y") + "\nwidth=" + width;
                    },
                },
            );
            return { lines: markdown.render(20) };
        })()"#,
    );
    let lines: Vec<String> = result["lines"]
        .as_array()
        .expect("array")
        .iter()
        .map(|line| line.as_str().unwrap_or_default().to_owned())
        .collect();
    let trimmed: Vec<&str> = lines.iter().map(|line| line.trim_end()).collect();
    assert!(trimmed.contains(&"> HL(js):let y = 1;"), "{lines:?}");
    assert!(!trimmed.contains(&"> HL(js):let x = 1;"));
    assert!(trimmed.contains(&"width=20"), "{lines:?}");
    // 只有显式传 `codeBlockIndent` 时才用 "> "。
    let default_indent = fixture.eval_json(
        r#"(() => {
            const identity = (text) => text;
            const theme = {
                heading: identity, link: identity, linkUrl: identity, code: identity,
                codeBlock: identity, codeBlockBorder: identity, quote: identity,
                quoteBorder: identity, hr: identity, listBullet: identity, bold: identity,
                italic: identity, strikethrough: identity, underline: identity,
                highlightCode: (code) => code.split("\n"),
            };
            const markdown = new __kiana_tui.Markdown("```js\nlet z = 2;\n```", 0, 0, theme);
            return markdown.render(20);
        })()"#,
    );
    let lines: Vec<String> = default_indent
        .as_array()
        .expect("array")
        .iter()
        .map(|line| line.as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        lines.iter().any(|line| line.trim_end() == "  let z = 2;"),
        "{lines:?}"
    );
}

/// `ScrollView`：省略 `scrollbar` 默认 `"hidden"`（`scroll-view.ts:52`）；
/// `updateLayout(h, v, requestRender)` 的第三个参数被保存并用于滚动的重绘请求
/// （`scroll-view.ts:189-192`）。
#[test]
fn scroll_view_defaults_and_update_layout_request_render() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const child = {
                render() { return Array.from({ length: 20 }, (_, i) => "line" + i); }
            };
            globalThis.__scrollRenders = 0;
            const hidden = new __kiana_tui.ScrollView(child, {});
            hidden.updateLayout(20, 5, () => { globalThis.__scrollRenders += 1; });
            hidden.setScrollbarActive(true);
            hidden.scrollBy(1);
            const hiddenScrollbar = hidden.scrollbar;

            const always = new __kiana_tui.ScrollView(child, { scrollbar: "always" });
            always.updateLayout(20, 5, () => {});
            always.setScrollbarActive(true);
            const auto = new __kiana_tui.ScrollView(child, { scrollbar: "auto" });
            return {
                hiddenScrollbar,
                alwaysScrollbar: always.scrollbar,
                autoScrollbar: auto.scrollbar,
                explicitHidden: new __kiana_tui.ScrollView(child, { scrollbar: "hidden" }).scrollbar,
                renders: globalThis.__scrollRenders,
            };
        })()"#,
    );
    // 省略 `scrollbar`（`{}`）→ `"hidden"`（`scroll-view.ts:52`）。
    assert_eq!(result["hiddenScrollbar"], "hidden");
    assert_eq!(result["explicitHidden"], "hidden");
    assert_eq!(result["alwaysScrollbar"], "always");
    assert_eq!(result["autoScrollbar"], "auto");
    // `scrollBy` 的变化经 updateLayout 的第三个参数请求重绘。
    assert!(result["renders"].as_f64().unwrap_or(0.0) > 0.0, "{result}");
}

/// `BorderedLoader` / `DynamicBorder` 的组合面（`bordered-loader.ts`、`dynamic-border.ts`）。
#[test]
fn bordered_loader_and_dynamic_border_compose() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const theme = { fg: (token, text) => "[" + token + "]" + text };
            const tui = { requestRender() {} };
            globalThis.__aborts = 0;
            const loader = new __kiana_tui.BorderedLoader(tui, theme, "Working...", {});
            loader.onAbort = () => { globalThis.__aborts += 1; };
            loader.handleInput("\u001b");
            const lines = loader.render(60);
            const border = new __kiana_tui.DynamicBorder((text) => "<" + text + ">");
            const borderLines = border.render(5);
            const plain = new __kiana_tui.DynamicBorder();
            const plainLines = plain.render(5);
            return {
                aborts: globalThis.__aborts,
                lines,
                borderLines,
                plainWidth: __kiana_tui.visibleWidth(plainLines[0]),
                plainHasAnsi: plainLines[0] !== "─".repeat(5),
            };
        })()"#,
    );
    assert_eq!(result["aborts"], 1, "onAbort must fire on the cancel key");
    assert_eq!(result["borderLines"], serde_json::json!(["<─────>"]));
    assert_eq!(result["plainWidth"], 5);
    assert_eq!(
        result["plainHasAnsi"], true,
        "default border uses theme.fg(border)"
    );
    let lines: Vec<String> = result["lines"]
        .as_array()
        .expect("array")
        .iter()
        .map(|line| line.as_str().unwrap_or_default().to_owned())
        .collect();
    // 上下边框来自 DynamicBorder（theme.fg("border")），中间是渲染后的 loader。
    assert!(
        lines[0].contains("[border]") && lines[0].contains("────"),
        "{lines:?}"
    );
    assert!(lines.last().unwrap().contains("[border]"));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("[accent]") && line.contains("[muted]Working...")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("cancel")),
        "{lines:?}"
    );

    // 非布尔 truthiness：`cancellable: 0` 是 false（TS `??` + truthiness），不显示取消提示。
    let falsy = fixture.eval_json(
        r#"(() => {
            const theme = { fg: (token, text) => text };
            const tui = { requestRender() {} };
            const falsyLoader = new __kiana_tui.BorderedLoader(tui, theme, "Working...", { cancellable: 0 });
            globalThis.__aborts2 = 0;
            falsyLoader.onAbort = () => { globalThis.__aborts2 += 1; };
            falsyLoader.handleInput("\u001b");
            return { lines: falsyLoader.render(60), aborts: globalThis.__aborts2 };
        })()"#,
    );
    assert_eq!(falsy["aborts"], 0, "cancellable: 0 must be falsy");
    let falsy_lines = falsy["lines"].as_array().expect("array");
    assert!(
        !falsy_lines
            .iter()
            .any(|line| line.as_str().unwrap_or_default().contains("cancel")),
        "{falsy_lines:?}"
    );
}

/// `CancellableLoader.onAbort` 在取消键上触发（`cancellable-loader.ts:29-35`）。
#[test]
fn cancellable_loader_on_abort_fires() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            globalThis.__loaderAborts = 0;
            globalThis.__loaderRenders = 0;
            const tui = { requestRender() { globalThis.__loaderRenders += 1; } };
            const loader = new __kiana_tui.CancellableLoader(tui, (t) => t, (t) => t, "Working...");
            loader.onAbort = () => { globalThis.__loaderAborts += 1; };
            const before = loader.aborted;
            loader.handleInput("\u001b");
            loader.handleInput("\u001b");
            return {
                before,
                aborted: loader.aborted,
                aborts: globalThis.__loaderAborts,
                hasAbortSignal: loader.signal instanceof AbortSignal,
            };
        })()"#,
    );
    assert_eq!(result["before"], false);
    assert_eq!(result["aborted"], true, "the token must be cancelled");
    assert_eq!(result["aborts"], 2, "onAbort must fire on every cancel key");
    assert_eq!(result["hasAbortSignal"], true);
}

/// `Input` / `SelectList` / `SettingsList` 的回调以组件为接收者（TS `this.onSubmit?.(…)`）。
#[test]
fn component_callbacks_run_with_the_component_as_this() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const input = new __kiana_tui.Input({ prompt: "" });
            input.onSubmit = function (text) { this.setValue("submitted:" + text); };
            input.handleInput("\r");
            const inputValue = input.getValue();

            const list = new __kiana_tui.SelectList(
                [{ value: "a", label: "A" }, { value: "b", label: "B" }],
                5,
                {},
            );
            list.onSelect = function (item) { this.__selected = item.value; };
            list.handleInput("\r");

            const settings = new __kiana_tui.SettingsList(
                [{ id: "mode", label: "Mode", currentValue: "a", values: ["a", "b"] }],
                5,
                {},
                function (id, value) { this.__changed = id + "=" + value; },
                function () { this.__cancelled = true; },
                {},
            );
            settings.handleInput("\r");
            return {
                inputValue,
                selected: list.__selected,
                changed: settings.__changed,
                cancelled: settings.__cancelled,
            };
        })()"#,
    );
    assert_eq!(result["inputValue"], "submitted:");
    assert_eq!(result["selected"], "a");
    assert_eq!(result["changed"], "mode=b");
    assert!(result["cancelled"].is_null() || result["cancelled"] == false);
}

/// 缓存的主题引用跟随全局主题切换（TS 的模块 `Proxy`，`theme.ts:648-663`）。
#[test]
fn cached_theme_references_follow_theme_switches() {
    let _guard = theme_guard();
    let fixture = Fixture::new();
    let original = kiana_theme::theme::current_theme()
        .unwrap_or_else(kiana_extensions::ui::current_global_theme);
    let dark = kiana_theme::theme::get_theme_by_name("dark").expect("dark");
    let light = kiana_theme::theme::get_theme_by_name("light").expect("light");
    kiana_theme::theme::set_theme_instance(std::sync::Arc::clone(&original));

    let before: String = fixture.eval(
        r#"(() => {
            globalThis.__cachedTheme = globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")];
            return __cachedTheme.getFgAnsi("accent");
        })()"#,
    );
    assert_eq!(
        before,
        original.get_fg_ansi(kiana_theme::ThemeColor::Accent),
        "the live instance must reflect the current theme"
    );

    kiana_theme::theme::set_theme_instance(std::sync::Arc::clone(&light));
    let after: String = fixture.eval("globalThis.__cachedTheme.getFgAnsi('accent')");
    let styled: String = fixture.eval("globalThis.__cachedTheme.fg('accent', 'x')");
    let restore = std::sync::Arc::clone(&dark);
    kiana_theme::theme::set_theme_instance(restore);
    assert_eq!(
        after,
        light.get_fg_ansi(kiana_theme::ThemeColor::Accent),
        "a cached reference must follow setTheme"
    );
    assert_eq!(styled, light.fg(kiana_theme::ThemeColor::Accent, "x"));
    assert_ne!(before, after, "dark and light accents must differ");
}

/// `Image` 包装：`getImageId()` 与 `render` 的兜底占位行。
#[test]
fn image_wrapper_exposes_the_image_id_and_renders() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const theme = { fallbackColor: (text) => "[" + text + "]" };
            const image = new __kiana_tui.Image(
                "aGVsbG8=",
                "image/png",
                theme,
                { maxWidthCells: 10, maxHeightCells: 5 },
                { widthPx: 100, heightPx: 50 },
            );
            const withoutId = image.getImageId();
            const withId = new __kiana_tui.Image("aGVsbG8=", "image/png", theme, { imageId: 7 });
            const lines = image.render(20);
            return {
                withoutId: String(withoutId),
                withId: withId.getImageId(),
                lineCount: lines.length,
                allStrings: lines.every((line) => typeof line === "string"),
            };
        })()"#,
    );
    assert_eq!(result["withoutId"], "undefined");
    assert_eq!(result["withId"], 7);
    assert!(result["lineCount"].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(result["allStrings"], true);
}

// ===========================================================================
// 回归：第二轮评审发现的偏差
// ===========================================================================

/// `VStack`/`HStack` 是 TS 组合类（`components/v-stack.ts`、`components/h-stack.ts`）：
/// `HStack` 横向合成（`compositeTuiLine`），`VStack` 生效 `gap`/`visible`/尺寸分配。
/// 夹具由 Node 直接跑 TS 参考生成（`stacks.json`）。
#[test]
fn stacks_match_the_ts_fixture() {
    let fixture = Fixture::new();
    let expected = fixture_json("stacks.json");
    let actual = fixture.eval_json(
        r#"(() => {
            const cases = {};
            cases.hstack_basic = new __kiana_tui.HStack([
                new __kiana_tui.Text("ab", 0, 0),
                new __kiana_tui.Text("cd", 0, 0),
            ]).render(20);
            cases.hstack_align_center = new __kiana_tui.HStack([
                new __kiana_tui.Text("a\nb\nc", 0, 0),
                new __kiana_tui.Text("x", 0, 0),
            ], { align: "center" }).render(20);
            cases.hstack_align_end = new __kiana_tui.HStack([
                new __kiana_tui.Text("a\nb\nc", 0, 0),
                new __kiana_tui.Text("x", 0, 0),
            ], { align: "end" }).render(20);
            cases.hstack_gap = new __kiana_tui.HStack([
                new __kiana_tui.Text("ab", 0, 0),
                new __kiana_tui.Text("cd", 0, 0),
            ], { gap: 2 }).render(20);
            cases.vstack_gap = new __kiana_tui.VStack([
                new __kiana_tui.Text("a", 0, 0),
                new __kiana_tui.Text("b", 0, 0),
            ], { gap: 2 }).render(5);
            cases.vstack_basic = new __kiana_tui.VStack([
                new __kiana_tui.Text("a", 0, 0),
                new __kiana_tui.Text("b", 0, 0),
            ]).render(5);
            cases.vstack_visible = new __kiana_tui.VStack([
                { component: new __kiana_tui.Text("hidden", 0, 0), visible: () => false },
                new __kiana_tui.Text("shown", 0, 0),
            ]).render(10);
            cases.hstack_grow = new __kiana_tui.HStack([
                { component: new __kiana_tui.Text("ab", 0, 0), grow: 1 },
                new __kiana_tui.Text("cd", 0, 0),
            ], { gap: 1 }).render(12);
            cases.vstack_empty = new __kiana_tui.VStack([]).render(10);
            cases.hstack_empty = new __kiana_tui.HStack([]).render(10);
            return cases;
        })()"#,
    );
    for key in [
        "hstack_basic",
        "hstack_align_center",
        "hstack_align_end",
        "hstack_gap",
        "vstack_gap",
        "vstack_basic",
        "vstack_visible",
        "hstack_grow",
        "vstack_empty",
        "hstack_empty",
    ] {
        assert_eq!(actual[key], expected[key], "case {key}");
    }
}

/// `Container.clear` 只是清空数组（`box.ts:43-46`），**不**调用子组件的 `dispose()`。
#[test]
fn box_clear_does_not_dispose_children() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            globalThis.__cleared = 0;
            const box = new __kiana_tui.Box(0, 0);
            box.addChild({ render() { return ["x"]; }, dispose() { globalThis.__cleared += 1; } });
            box.clear();
            return { lines: box.render(4), disposed: globalThis.__cleared };
        })()"#,
    );
    assert!(result["lines"].as_array().expect("array").is_empty());
    assert_eq!(result["disposed"], 0, "clear must not dispose");
}

/// `DynamicBorder` 的默认颜色在渲染时读取当前主题（`dynamic-border.ts:11-14` 的模块
/// `Proxy` 语义），`setTheme` 之后立即生效。
#[test]
fn dynamic_border_default_color_follows_theme_switches() {
    let _guard = theme_guard();
    let fixture = Fixture::new();
    let original = kiana_theme::theme::current_theme()
        .unwrap_or_else(kiana_extensions::ui::current_global_theme);
    let dark = kiana_theme::theme::get_theme_by_name("dark").expect("dark");
    let light = kiana_theme::theme::get_theme_by_name("light").expect("light");

    kiana_theme::theme::set_theme_instance(std::sync::Arc::clone(&dark));
    let before: String = fixture.eval("new __kiana_tui.DynamicBorder().render(5)[0]");
    kiana_theme::theme::set_theme_instance(std::sync::Arc::clone(&light));
    let after: String = fixture.eval("new __kiana_tui.DynamicBorder().render(5)[0]");
    kiana_theme::theme::set_theme_instance(std::sync::Arc::clone(&original));

    let border = "─".repeat(5);
    assert_eq!(before, dark.fg(kiana_theme::ThemeColor::Border, &border));
    assert_eq!(
        after,
        light.fg(kiana_theme::ThemeColor::Border, &border),
        "the default color must read the live theme at render time"
    );
    assert_ne!(before, after, "dark and light borders must differ");
}

/// `ScrollView` 的公开面（`scroll-view.ts:60-90、200-216`）：getter、只读字段、任意字符串
/// 选项值（不做校验）与三个「永远抛错」的容器方法。
#[test]
fn scroll_view_exposes_the_full_ts_surface() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const child = { render() { return ["x"]; } };
            const view = new __kiana_tui.ScrollView(child, {
                follow: "bogus",
                overscroll: "bogus",
                scrollbar: "bogus",
            });
            view.updateLayout(20, 5, () => {});
            const throwMessage = (call) => {
                try { call(); return "no error"; } catch (error) { return error.message; }
            };
            return {
                followEnd: view.followEnd,
                primary: view.primary,
                overscroll: view.overscroll,
                scrollbar: view.scrollbar,
                scrollTop: view.scrollTop,
                viewportHeight: view.viewportHeight,
                isScrollbarVisible: view.isScrollbarVisible,
                isScrollbarActive: view.isScrollbarActive,
                addChild: throwMessage(() => view.addChild({ render() { return []; } })),
                removeChild: throwMessage(() => view.removeChild(child)),
                clear: throwMessage(() => view.clear()),
            };
        })()"#,
    );
    assert_eq!(result["followEnd"], false);
    assert_eq!(result["primary"], false);
    assert_eq!(result["overscroll"], "bogus");
    assert_eq!(result["scrollbar"], "bogus");
    assert_eq!(result["scrollTop"], 0.0);
    assert_eq!(result["viewportHeight"], 5.0);
    assert_eq!(result["isScrollbarVisible"], false);
    assert_eq!(result["isScrollbarActive"], false);
    assert_eq!(result["addChild"], "ScrollView has exactly one child");
    assert_eq!(result["removeChild"], "ScrollView child cannot be removed");
    assert_eq!(result["clear"], "ScrollView child cannot be cleared");
}

/// `Loader`/`CancellableLoader` 继承 `Text`：`setPaddingX` 真的改变输出，`CancellableLoader`
/// 还有 `setText`（`loader.ts:16`、`cancellable-loader.ts:12`）。
#[test]
fn loader_subclasses_expose_inherited_text_members() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const tui = { requestRender() {} };
            const identity = (text) => text;
            const loader = new __kiana_tui.Loader(tui, identity, identity, "msg");
            loader.setIndicator({ frames: [] });
            const before = loader.render(20)[1];
            loader.setPaddingX(0);
            const after = loader.render(20)[1];

            const cancellable = new __kiana_tui.CancellableLoader(tui, identity, identity, "msg");
            cancellable.setIndicator({ frames: [] });
            cancellable.setPaddingX(0);
            cancellable.setText("changed");
            const text = cancellable.render(20)[1];
            return { before, after, text };
        })()"#,
    );
    let before = result["before"].as_str().expect("string");
    let after = result["after"].as_str().expect("string");
    assert!(before.starts_with(" msg"), "{before:?}");
    assert!(after.starts_with("msg"), "{after:?}");
    assert_ne!(before, after, "setPaddingX must change the output");
    assert!(
        result["text"]
            .as_str()
            .unwrap_or_default()
            .starts_with("changed"),
        "{:?}",
        result["text"]
    );
}

/// `BorderedLoader` 继承 `Container`：`children`/`removeChild`/`clear`/`handleMouse` 存在
/// 且按对象身份操作（`bordered-loader.ts:7`）。
#[test]
fn bordered_loader_inherits_container_members() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
          try {
            const theme = { fg: (token, text) => text };
            const tui = { requestRender() {} };
            const loader = new __kiana_tui.BorderedLoader(tui, theme, "Working", {});
            const extra = { render() { return ["extra"]; } };
            loader.addChild(extra);
            const afterAdd = loader.children.length;
            const sameObject = loader.children[0] === extra;
            const rendered = loader.render(20);
            const mouse = loader.handleMouse({
                type: "press", button: "left", x: 1, y: 1,
                screenX: 1, screenY: 1, width: 20, height: 20,
                shift: false, alt: false, ctrl: false,
            });
            loader.removeChild(extra);
            const afterRemove = loader.children.length;
            loader.addChild({ render() { return ["again"]; } });
            loader.clear();
            const afterClear = loader.children.length;
            return {
                afterAdd, sameObject, renderedHasExtra: rendered.includes("extra"),
                mouseIsUndefined: mouse === undefined,
                afterRemove, afterClear,
            };
          } catch (error) {
            return { error: String(error && error.stack || error) };
          }
        })()"#,
    );
    assert!(result.get("error").is_none(), "js error: {result}");
    assert_eq!(result["afterAdd"], 1.0);
    assert_eq!(result["sameObject"], true);
    assert_eq!(result["renderedHasExtra"], true);
    assert_eq!(result["mouseIsUndefined"], true);
    assert_eq!(result["afterRemove"], 0.0);
    assert_eq!(result["afterClear"], 0.0);
}

/// `SelectList` 的回调与 `getSelectedItem()` 返回扩展传入的**原始对象**（对象身份与额外
/// 字段都保留，`select-list.ts:130-139、246-272`）。
#[test]
fn select_list_preserves_item_identity_and_extra_fields() {
    let fixture = Fixture::new();
    let result = fixture.eval_json(
        r#"(() => {
            const items = [
                { value: "a", label: "A", extra: 42 },
                { value: "b", label: "B" },
            ];
            const seen = { selected: [] };
            const list = new __kiana_tui.SelectList(items, 5, {}, {
                truncatePrimary(context) {
                    if (context.item === items[0]) {
                        seen.truncateSame = true;
                        seen.truncateExtra = context.item.extra;
                    }
                    return context.text;
                },
            });
            list.onSelect = (item) => { seen.selected.push(item); };
            list.onSelectionChange = (item) => { seen.changeSame = item === items[1]; };
            list.render(40);
            const selected = list.getSelectedItem();
            seen.getSame = selected === items[0];
            seen.getExtra = selected.extra;
            list.handleInput("\u001b[B");   // down -> b
            list.handleInput("\r");         // select b
            list.setSelectedIndex(0);
            list.handleInput("\r");         // select a
            seen.selectFirstSame = seen.selected[0] === items[1];
            seen.selectSecondSame = seen.selected[1] === items[0];
            seen.selectSecondKeys = Object.keys(seen.selected[1]).join(",");
            seen.selectSecondExtra = seen.selected[1].extra;
            return seen;
        })()"#,
    );
    assert_eq!(result["truncateSame"], true);
    assert_eq!(result["truncateExtra"], 42.0);
    assert_eq!(result["getSame"], true);
    assert_eq!(result["getExtra"], 42.0);
    assert_eq!(
        result["selectFirstSame"], true,
        "onSelect must receive the original object: {result}"
    );
    assert_eq!(result["selectSecondSame"], true);
    assert_eq!(result["selectSecondExtra"], 42.0);
    assert_eq!(result["selectSecondKeys"], "value,label,extra");
    assert_eq!(result["changeSame"], true);
}
