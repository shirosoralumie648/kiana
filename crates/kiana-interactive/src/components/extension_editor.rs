//! components/extension-editor.ts。
//!
//! 扩展 `ui.editor()` 的多行编辑器：`Ctrl+G`（`app.editor.external`）打开外部编辑器。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的 `TUI` 在这里用到 `stop()` / `start()` / `requestRender(true)`，因此接收共享句柄
//!   `Rc<RefCell<dyn Tui>>`（交互模式的 `InteractiveTuiReference` 即此形态）；内置
//!   `Editor` 需要 [`EditorTui`]（terminal + requestRender），由调用方一并传入。
//! * TS 的 `KeybindingsManager` 实例 → `Rc<KeybindingsManager>`（仅用于
//!   `app.editor.external`）；取消键仍走全局 `getKeybindings()`，与其余内置组件的按键解析保持一致。
//! * 异步外部编辑器任务用可注入的 [`EditorSpawn`] 投递（默认 `tokio::task::spawn_local`）。
//!
//! ## 被谁调用
//!
//! 两个宿主路径：扩展 `ui.editor()`（`interactive_mode/extension_ui` 的
//! `begin_extension_editor`，压入扩展对话框栈，提交/取消经 `Weak` 回写
//! `DialogValue::Text`；没有倒计时，宿主不为它注册 dispose 回调）和 `/bug` 流程
//! （`bug_report.rs` 的 `input`，挂到编辑器槽位作 overlay，回调经 `OverlayFinish`
//! 恢复编辑器并唤醒等待者）。
//!
//! ## 设计原因
//!
//! * 内部 `Editor` 实例被三方共享：容器树要通过代理（`EditorProxy`）渲染它，外部编辑器
//!   任务结束后要把编辑结果写回它的文本，组件自身在按键分发与焦点切换时也要访问它。
//!   所以 `Editor` 放进 `Rc<RefCell<>>`，容器树里挂的是一个只做转发的私有代理，而不是
//!   `Editor` 本身。
//! * 外部编辑器是异步子进程：TS 的 `handleOpenExternalEditor` 是 async 方法，第一个
//!   `await` 之前同步执行 `tui.stop()`。Rust 侧保持同一顺序——先同步 `stop()`，再把
//!   「调起编辑器 → 写回文本 → `start()` + 强制重绘」整个包成 future 投递出去。「投递」
//!   抽象成可注入的 [`EditorSpawn`]，测试才能拦截任务、手动推进到完成。
//!
//! ## 执行流程（`handle_input`）
//!
//! 1. 全局键位表命中 `tui.select.cancel`（Escape/Ctrl+C）→ 调 `on_cancel` 并返回；
//!    该判定先于外部编辑器，保证取消永远可用；
//! 2. 注入的 keybindings 命中 `app.editor.external`（默认 Ctrl+G）→ 停 TUI、投递任务。
//!    任务内 [`edit_in_external_editor`] 结果为 `Complete` 时把新文本写回编辑器；
//!    `Failed`（启动失败或非 0 退出）时保留原文本。无论成败都以 `start()` +
//!    `request_render(true)` 恢复 TUI（对应 TS 的 `finally`）；
//! 3. 其余按键转发给内部 `Editor`（多行编辑与自动补全都在那里实现）。
//!
//! ## 注意事项
//!
//! * 外部编辑器命令在构造时一次性解析：显式参数 > `$VISUAL` > `$EDITOR` > 平台默认
//!   （Windows `notepad`，其余 `nano`）；运行期不再读环境变量。空串与 `None` 等价
//!   （JS 的 `||` 语义）。交互模式宿主从 settings 读命令传入（覆盖环境变量）。
//! * `options.description` 只画在标题下方，不传给内部 `Editor`；`padding_x` 与
//!   `autocomplete_max_visible` 透传给 `EditorOptions`。
//! * 组件实现 [`Focusable`]：`set_focused` 会同步转发给内部 `Editor`，容器焦点遍历到
//!   本组件时内层编辑器状态保持一致。

use std::any::Any;
use std::rc::Rc;

use kiana_config::keybindings::KeybindingsManager;
use kiana_theme::ThemeColor;
use kiana_tui::components::editor::{Editor, EditorOptions, EditorTheme, EditorTui};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{
    Component, Container, Focusable, Tui, TuiMouseEvent, TuiMouseEventResult, TuiStopOptions,
};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_hint;

use crate::external_editor::{
    ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor,
};

/// 异步任务投递（TS 里是 `void this.handleOpenExternalEditor()` 的微任务）。
///
/// 接收一个已 pin 的 `Future<Output = ()>` 并安排执行；生产环境用
/// `tokio::task::spawn_local`（交互模式跑在 UI 线程的 `LocalSet` 上，future 不是
/// `Send`），测试注入替身把 future 存起来手动 await。`Rc` 而非函数指针，因为替身需要
/// 捕获可变状态。
pub type EditorSpawn = Rc<dyn Fn(std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>)>;

/// `ExtensionEditorOptions extends EditorOptions { description?: string }`。
#[derive(Debug, Clone, Default)]
pub struct ExtensionEditorOptions {
    /// 编辑器左边距（`EditorOptions.paddingX`）。
    pub padding_x: Option<f64>,
    /// 自动补全可见项上限（`EditorOptions.autocompleteMaxVisible`）。
    pub autocomplete_max_visible: Option<f64>,
    /// 标题下方的说明（**不**传给 `Editor`）。
    pub description: Option<String>,
}

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn default_spawn() -> EditorSpawn {
    Rc::new(|future| {
        tokio::task::spawn_local(future);
    })
}

/// 取第一个非空字符串（JS 的 `a || b || c` 对空串为假）。
fn first_non_empty(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values.into_iter().flatten().find(|value| !value.is_empty())
}

/// 共享 [`Editor`] 的容器代理。
///
/// `Editor` 是三方共享的可变状态（见模块文档），不能直接放进 `Container`（容器按值
/// 持有子项）。这个零字段代理把 `render` / `handle_mouse` / 焦点查询全部借道
/// `RefCell` 转发过去；`handles_input` 为 false，按键由外层组件统一分发。
struct EditorProxy(Rc<std::cell::RefCell<Editor>>);

impl Component for EditorProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for EditorProxy {
    fn focused(&self) -> bool {
        self.0.borrow().focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.0.borrow_mut().set_focused(focused);
    }
}

/// 扩展编辑器（TS `class ExtensionEditorComponent extends Container implements Focusable`）。
///
/// 静态部分（边框、标题、说明、按键提示）构造时一次性装入 [`Container`]；只有
/// `Editor` 行是共享可变的。生命周期由宿主管理：交互模式的编辑器对话框提交/取消后
/// 整个组件被丢弃，`/bug` 流程用完后恢复原编辑器。
pub struct ExtensionEditorComponent {
    container: Container,
    editor: Rc<std::cell::RefCell<Editor>>,
    on_cancel: Box<dyn FnMut()>,
    tui: Rc<std::cell::RefCell<dyn Tui>>,
    keybindings: Rc<KeybindingsManager>,
    external_editor_command: String,
    focused: bool,
    spawn: EditorSpawn,
}

impl ExtensionEditorComponent {
    /// TS `constructor(tui, keybindings, title, prefill, onSubmit, onCancel, options?, externalEditorCommand?)`。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tui: Rc<std::cell::RefCell<dyn Tui>>,
        editor_tui: EditorTui,
        keybindings: Rc<KeybindingsManager>,
        title: &str,
        prefill: Option<&str>,
        on_submit: impl FnMut(&str) + 'static,
        on_cancel: impl FnMut() + 'static,
        options: ExtensionEditorOptions,
        external_editor_command: Option<String>,
    ) -> Self {
        let external_editor_command = first_non_empty([
            external_editor_command,
            std::env::var("VISUAL").ok(),
            std::env::var("EDITOR").ok(),
        ])
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "nano".to_string()
            }
        });

        let editor_theme: EditorTheme = kiana_theme::get_editor_theme();
        let editor_options = EditorOptions {
            padding_x: options.padding_x,
            autocomplete_max_visible: options.autocomplete_max_visible,
        };
        let mut editor = Editor::new(editor_tui, editor_theme, editor_options);
        if let Some(prefill) = prefill
            && !prefill.is_empty()
        {
            editor.set_text(prefill);
        }
        editor.on_submit = Some(Box::new(on_submit));
        let editor = Rc::new(std::cell::RefCell::new(editor));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Accent, title),
            1,
            0,
        )));
        if let Some(description) = options
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
        {
            container.add_child(Box::new(Spacer::new()));
            container.add_child(Box::new(Text::with_padding(
                fg(ThemeColor::Text, description),
                1,
                0,
            )));
        }
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(EditorProxy(Rc::clone(&editor))));
        container.add_child(Box::new(Spacer::new()));
        let hint = key_hint("tui.select.confirm", "submit")
            + "  "
            + &key_hint("tui.input.newLine", "newline")
            + "  "
            + &key_hint("tui.select.cancel", "cancel")
            + &format!("  {}", key_hint("app.editor.external", "external editor"));
        container.add_child(Box::new(Text::with_padding(hint, 1, 0)));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        ExtensionEditorComponent {
            container,
            editor,
            on_cancel: Box::new(on_cancel),
            tui,
            keybindings,
            external_editor_command,
            focused: false,
            spawn: default_spawn(),
        }
    }

    /// 注入异步任务投递（测试接缝；默认 `spawn_local`）。
    ///
    /// 也可以在运行期替换，但生产代码从不调用；构造后立即注入是测试的固定用法
    /// （见 `tests/components_extension_dialogs.rs` 的 `extension_editor_ctrl_g_*`）。
    pub fn set_spawn(&mut self, spawn: EditorSpawn) {
        self.spawn = spawn;
    }

    /// 当前编辑器文本（`editor.getText()`）。
    pub fn editor_text(&self) -> String {
        self.editor.borrow().get_text()
    }

    /// 当前外部编辑器命令（`$VISUAL` / `$EDITOR` 解析结果）。
    pub fn external_editor_command(&self) -> &str {
        &self.external_editor_command
    }

    fn handle_open_external_editor(&mut self) {
        let content = self.editor.borrow().get_text();
        // TS 的 `handleOpenExternalEditor` 在第一个 `await` 之前同步执行 `this.tui.stop()`；
        // 必须在投递异步任务前停掉渲染循环。测试断言 stop 计数在 handle_input 返回前
        // 已经 +1（`extension_editor_ctrl_g_runs_the_external_editor_between_stop_and_start`）。
        self.tui.borrow_mut().stop(TuiStopOptions::default());
        let tui = Rc::clone(&self.tui);
        let editor = Rc::clone(&self.editor);
        let command = self.external_editor_command.clone();

        let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> =
            Box::pin(async move {
                let result =
                    edit_in_external_editor(ExternalEditorOptions { command, content }).await;
                if let Ok(ExternalEditorResult::Complete { content }) = result {
                    editor.borrow_mut().set_text(&content);
                }
                // TS 的 `finally`：无论成功/失败/取消都恢复 TUI 并强制重绘。`Err`（临时
                // 文件 IO 失败）与 `Failed`（编辑器退出码非 0）都保持编辑器原文本。
                tui.borrow_mut().start();
                tui.borrow_mut().request_render(true);
            });
        (self.spawn)(future);
    }
}

impl Component for ExtensionEditorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        // Escape or Ctrl+C to cancel（先于外部编辑器判定）。
        if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
            return;
        }

        // External editor (app keybinding)：用注入的 keybindings，而不是全局表。
        if self.keybindings.matches(key_data, "app.editor.external") {
            self.handle_open_external_editor();
            return;
        }

        self.editor.borrow_mut().handle_input(key_data);
    }

    fn handles_input(&self) -> bool {
        true
    }

    /// TS `Container.handleMouse`：按行路由到 `Editor`（点击定位光标/自动补全列表）。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

impl Focusable for ExtensionEditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.editor.borrow_mut().set_focused(focused);
    }
}
