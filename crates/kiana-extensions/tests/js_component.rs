//! `JsComponent`：JS 组件对象 → `kiana_tui::Component`（设计文档 §2.9）。
//!
//! 断言四条契约：
//! 1. 每次调用都经 `This` 传入对象（JS 子类的 `super` 能看到 `this`）；
//! 2. `render/handleInput/handleMouse/invalidate/wantsKeyRelease/handlesInput` 的转发；
//! 3. 渲染失败返回上一次成功的行（首次失败 `[]`），同一错误只上报一次；
//! 4. 鼠标事件与结果按 TS 形状（camelCase JSON）序列化。

use kiana_extensions::engine::{self, EngineConfig, JsEngine};
use kiana_extensions::ui::adapter::{self, JsComponent};
use kiana_tui::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};
use std::rc::Rc;

fn engine() -> Rc<JsEngine> {
    let engine = Rc::new(JsEngine::with_config(EngineConfig::new()).expect("create engine"));
    engine::install(&engine).expect("install prelude");
    kiana_extensions::ui::install_attached(&engine).expect("install ui adapter");
    engine
}

/// 求值一个 JS 对象表达式并把它存成组件引用。
fn component(engine: &Rc<JsEngine>, source: &str) -> JsComponent {
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(source)?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval component");
    JsComponent::from_ref(engine, reference)
}

#[test]
fn render_is_dispatched_with_this() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            prefix: "js:",
            render(width) { return [this.prefix + width]; },
            handleInput(data) { this.last = data; },
        })"#,
    );
    assert_eq!(component.render(17), vec!["js:17".to_owned()]);
    // render 宽度按 JS 数值语义取整（负数/非有限 → 0）。
    assert_eq!(component.render(0), vec!["js:0".to_owned()]);
}

#[test]
fn handle_input_uses_the_same_object_and_can_read_state() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            seen: [],
            render(width) { return [String(this.seen.length) + ":" + String(width)]; },
            handleInput(data) { this.seen.push(data); },
        })"#,
    );
    component.handle_input("a");
    component.handle_input("b");
    assert_eq!(component.render(4), vec!["2:4".to_owned()]);
}

#[test]
fn handle_input_and_wants_key_release_are_optional() {
    let engine = engine();
    let mut display_only = component(&engine, r#"({ render() { return ["x"]; } })"#);
    assert!(!display_only.handles_input());
    assert!(!display_only.wants_key_release());
    display_only.handle_input("ignored");

    let interactive = component(
        &engine,
        r#"({ render() { return ["y"]; }, wantsKeyRelease: true, handleInput() {} })"#,
    );
    assert!(interactive.handles_input());
    assert!(interactive.wants_key_release());
}

#[test]
fn render_failure_returns_the_last_successful_lines() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            calls: 0,
            render() {
                this.calls += 1;
                if (this.calls > 1) throw new Error("boom");
                return ["ok"];
            },
        })"#,
    );
    assert_eq!(component.render(10), vec!["ok".to_owned()]);
    // 首次失败后返回上一次成功的行（不是 []）。
    assert_eq!(component.render(10), vec!["ok".to_owned()]);
    // 同一错误只上报一次。
    let errors = engine.take_errors();
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert!(errors[0].message.contains("boom"), "{:?}", errors[0]);
}

#[test]
fn render_failure_before_any_success_returns_no_lines() {
    let engine = engine();
    let mut component = component(&engine, r#"({ render() { throw new Error("early"); } })"#);
    assert!(component.render(10).is_empty());
    assert_eq!(engine.take_errors().len(), 1);
}

#[test]
fn invalidate_is_forwarded() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            invalidations: 0,
            render() { return [String(this.invalidations)]; },
            invalidate() { this.invalidations += 1; },
        })"#,
    );
    assert_eq!(component.render(3), vec!["0".to_owned()]);
    component.invalidate();
    assert_eq!(component.render(3), vec!["1".to_owned()]);
}

#[test]
fn handle_mouse_parses_the_result() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            render() { return ["m"]; },
            handleMouse() { return { handled: true, focus: true, capture: false }; },
        })"#,
    );
    let event = TuiMouseEvent {
        event_type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 2,
        y: 3,
        screen_x: 12,
        screen_y: 13,
        width: 40,
        height: 20,
        shift: true,
        alt: false,
        ctrl: true,
        wheel_delta: None,
        click_count: Some(2),
    };
    let result = component.handle_mouse(&event).expect("result");
    assert_eq!(result.handled, Some(true));
    assert_eq!(result.focus, Some(true));
    assert_eq!(result.capture, Some(false));
}

/// `handleMouse` 收到的是 TS 形状的事件对象（camelCase/下划线字段名逐字）。
#[test]
fn handle_mouse_event_payload_uses_ts_field_names() {
    let engine = engine();
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"({
                    render() { return []; },
                    handleMouse(event) {
                        globalThis.__mouse = event;
                        return undefined;
                    },
                })"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval");
    let mut component = JsComponent::from_ref(&engine, reference);
    let event = TuiMouseEvent {
        event_type: TuiMouseEventType::Wheel,
        button: TuiMouseButton::None,
        x: 0,
        y: 7,
        screen_x: 0,
        screen_y: 7,
        width: 80,
        height: 24,
        shift: false,
        alt: true,
        ctrl: false,
        wheel_delta: Some(-3.0),
        click_count: None,
    };
    assert!(component.handle_mouse(&event).is_none());
    let json: String = engine
        .eval_script("JSON.stringify(globalThis.__mouse)", "<test>")
        .expect("read event");
    let seen: serde_json::Value = serde_json::from_str(&json).expect("parse event");
    assert_eq!(seen["type"], "wheel");
    assert_eq!(seen["button"], "none");
    assert_eq!(seen["x"].as_f64(), Some(0.0));
    assert_eq!(seen["y"].as_f64(), Some(7.0));
    assert_eq!(seen["screenX"].as_f64(), Some(0.0));
    assert_eq!(seen["screenY"].as_f64(), Some(7.0));
    assert_eq!(seen["width"].as_f64(), Some(80.0));
    assert_eq!(seen["height"].as_f64(), Some(24.0));
    assert_eq!(seen["shift"], false);
    assert_eq!(seen["alt"], true);
    assert_eq!(seen["ctrl"], false);
    assert_eq!(seen["wheelDelta"].as_f64(), Some(-3.0));
    assert!(seen.get("clickCount").is_none());
}

/// 同一个错误只上报一次，但每次渲染仍走 JS。
#[test]
fn repeated_failures_are_deduplicated_but_render_is_still_called() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            count: 0,
            render() {
                this.count += 1;
                globalThis.__renderCount = this.count;
                throw new Error("same");
            },
        })"#,
    );
    for _ in 0..3 {
        assert!(component.render(5).is_empty());
    }
    let count: f64 = engine
        .eval_script("globalThis.__renderCount", "<test>")
        .expect("read count");
    assert_eq!(count, 3.0);
    assert_eq!(engine.take_errors().len(), 1);
}

/// `dispose` 只由所有者显式调用（`ui.custom` 关闭对话框 / widget 被替换）；普通 Drop
/// **不**调用 `dispose()`（TS `Box.clear`/`removeChild`、overlay `hide` 都不调用，
/// `box.ts:43-46`、`tui.ts:770-790`）。
#[test]
fn drop_releases_the_reference_without_calling_dispose() {
    let engine = engine();
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"({
                    render() { return []; },
                    dispose() { globalThis.__disposed = (globalThis.__disposed || 0) + 1; },
                })"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval");
    {
        let _component = JsComponent::from_ref(&engine, reference);
    }
    // Drop 在 JS 事务之外只登记；下一次原生方法调用时清理引用。
    engine
        .eval_script::<()>("new __kiana_tui.Text('x').render(1);", "<flush>")
        .expect("flush");
    let disposed: f64 = engine
        .eval_script("globalThis.__disposed ?? 0", "<test>")
        .expect("read disposed");
    assert_eq!(disposed, 0.0, "Drop must not call dispose()");
    // 引用表项已释放。
    let still_there: bool = engine
        .eval_script(
            &format!("({reference} in globalThis.__kiana_refs)"),
            "<test>",
        )
        .expect("read refs");
    assert!(!still_there, "the __kiana_refs entry must be released");
}

/// 所有者显式 `dispose()`：调用一次 JS `dispose` 并释放引用。
#[test]
fn dispose_calls_dispose_once() {
    let engine = engine();
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"({
                    render() { return []; },
                    dispose() { globalThis.__disposed = (globalThis.__disposed || 0) + 1; },
                })"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval");
    let mut component = JsComponent::from_ref(&engine, reference);
    component.dispose();
    component.dispose();
    // 事务外的 dispose 登记到下一个事务；原生方法调用会清理。
    engine
        .eval_script::<()>("new __kiana_tui.Text('x').render(1);", "<flush>")
        .expect("flush");
    let disposed: f64 = engine
        .eval_script("globalThis.__disposed ?? 0", "<test>")
        .expect("read disposed");
    assert_eq!(disposed, 1.0, "dispose must run exactly once");
}

// ===========================================================================
// 回归：评审发现的偏差
// ===========================================================================

/// `focusTarget` 是组件对象（TS `tui.ts:81-104`）：不能因为 serde 反序列化失败把整个
/// `handled`/`focus` 结果丢掉。对象形式的 `focusTarget` 解释为“本次接收者”。
#[test]
fn mouse_result_with_a_component_focus_target_is_not_dropped() {
    let engine = engine();
    let mut component = component(
        &engine,
        r#"({
            render() { return ["m"]; },
            handleMouse(event) {
                return { handled: true, focus: true, focusTarget: this.child, render: false };
            },
            child: { render() { return []; } },
        })"#,
    );
    let event = TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x: 1,
        y: 1,
        screen_x: 1,
        screen_y: 1,
        width: 20,
        height: 10,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    };
    let result = component
        .handle_mouse(&event)
        .expect("the result must survive");
    assert_eq!(result.handled, Some(true));
    assert_eq!(result.focus, Some(true));
    assert_eq!(result.render, Some(false));
    assert_eq!(
        result.focus_target,
        Some(kiana_tui::tui::MouseFocusTarget::SelfComponent)
    );
}

/// `JsEditorComponent` 把 `EditorComponent` 的可选方法转发给 JS 编辑器
/// （`interactive-mode.ts:2832-2854、2656`）。
#[test]
fn editor_component_forwards_optional_methods() {
    use std::future::Future;
    use std::pin::Pin;

    use kiana_extensions::ui::NativeAutocompleteProvider;
    use kiana_tui::autocomplete::{
        ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
        GetSuggestionsOptions,
    };
    use kiana_tui::editor_component::EditorComponent;

    struct NoopProvider;

    impl AutocompleteProvider for NoopProvider {
        fn get_suggestions<'a>(
            &'a mut self,
            _lines: &'a [String],
            _cursor_line: usize,
            _cursor_col: usize,
            _options: &'a GetSuggestionsOptions,
        ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
            Box::pin(async { None })
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> ApplyCompletionResult {
            ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            }
        }
    }

    let engine = engine();
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"globalThis.__editor = {
                    calls: [],
                    render() { return []; },
                    getText() { return "plain"; },
                    getExpandedText() { return "expanded"; },
                    setText() {},
                    addToHistory(text) { this.calls.push("history:" + text); },
                    insertTextAtCursor(text) { this.calls.push("insert:" + text); },
                    setPaddingX(padding) { this.calls.push("padding:" + padding); },
                    setAutocompleteMaxVisible(max) { this.calls.push("max:" + max); },
                    setAutocompleteProvider(provider) {
                        this.calls.push(
                            "provider:" + typeof provider.getSuggestions + ":" +
                            typeof provider.applyCompletion,
                        );
                    },
                    dispose() {},
                };"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval editor");
    let mut component =
        kiana_extensions::ui::adapter::JsEditorComponent::from_ref(&engine, reference);
    component.add_to_history("h");
    component.insert_text_at_cursor("i");
    assert_eq!(component.get_expanded_text(), "expanded");
    component.set_padding_x(3);
    component.set_autocomplete_max_visible(7);
    component
        .set_autocomplete_provider(Box::new(NativeAutocompleteProvider(Box::new(NoopProvider))));
    let calls: Vec<String> = engine
        .eval_script("globalThis.__editor.calls", "<test>")
        .expect("read calls");
    assert_eq!(
        calls,
        vec![
            "history:h".to_owned(),
            "insert:i".to_owned(),
            "padding:3".to_owned(),
            "max:7".to_owned(),
            "provider:function:function".to_owned(),
        ]
    );

    // 没有 `getExpandedText` 时回退到 `getText`（TS `getExpandedText?.() ?? getText()`）。
    let fallback = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"({ render() { return []; }, getText() { return "collapsed"; }, setText() {} })"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval fallback");
    let fallback = kiana_extensions::ui::adapter::JsEditorComponent::from_ref(&engine, fallback);
    assert_eq!(fallback.get_expanded_text(), "collapsed");
}

/// 所有者组件（widget/footer/header/custom）Drop 时调用 `dispose()`。
#[test]
fn owned_component_disposes_on_drop() {
    use kiana_extensions::ui::adapter::OwnedJsComponent;
    let engine = engine();
    let reference = engine
        .with_js(|ctx| {
            let value: rquickjs::Value = ctx.eval(
                r#"({
                    render() { return []; },
                    dispose() { globalThis.__ownedDisposed = (globalThis.__ownedDisposed || 0) + 1; },
                })"#,
            )?;
            adapter::store_ref(&ctx, value)
        })
        .expect("eval");
    {
        let _component = OwnedJsComponent::new(JsComponent::from_ref(&engine, reference));
    }
    // Drop 在 JS 事务之外只登记；下一次原生方法调用时清理。
    engine
        .eval_script::<()>("new __kiana_tui.Text('x').render(1);", "<flush>")
        .expect("flush");
    let disposed: f64 = engine
        .eval_script("globalThis.__ownedDisposed ?? 0", "<test>")
        .expect("read disposed");
    assert_eq!(disposed, 1.0);
}
