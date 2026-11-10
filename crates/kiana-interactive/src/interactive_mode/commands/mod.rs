//! `interactive-mode.ts` 的命令分派与键位处理（§2.50.8 / §3.2）。
//!
//! 本文件实现 `setupKeyHandlers()` / `setupEditorSubmitHandler()`（提交分派表、bash 模式、
//! 非命令输入）、`showSelector` 基础设施、键盘动作处理（thinking/model 循环），以及各
//! `/command` 的公共辅助。
//!
//! # CONTRACT（`interactive_mode.rs` 调用；后续单元必须保留签名）
//!
//! ```text
//! pub(crate) fn setup_key_handlers(&mut self)
//! pub(crate) fn setup_editor_submit_handler(&mut self)
//! ```
//!
//! # 设计原因：为什么回调都要“延后一拍”
//!
//! 键盘/提交回调由编辑器组件在 `handle_input` 内部调用。此时编辑器自身的 `RefCell`
//! 正被代理借用，渲染器通常也在输入分发的借用栈上；回调若同步清空编辑器文本、改边框色
//! 或重入渲染器，会触发 `RefCell` 重入 panic。TS 的 `this` 没有借用检查，回调可以同步改
//! UI；Rust 用 `run_after_dispatch` 与 `tokio::task::spawn_local` 把动作推迟到当前分发
//! 返回之后，恢复同样的可重入语义。新增按键处理时必须复用这两个入口，不要同步调用会写
//! 编辑器/渲染器的方法。
//!
//! # 执行流程
//!
//! 1. `InteractiveMode::init()` 第 9 步安装两个处理器（在托管工具安装完成之后；
//!    见 `crates/kiana-interactive/src/interactive_mode.rs`）。
//! 2. 编辑器把 Escape、命名的 `app.*` 动作、`onChange`、`onSubmit`、Ctrl+V 变成回调。
//! 3. `onSubmit` → `dispatch_submit`：先按 JS `trim` 归一化，再按固定顺序匹配内置命令表，
//!    然后才是 `!`/`!!` bash 命令，最后按“压缩中 / 流式中 / 普通消息”分流。
//! 4. 多数同步 UI 动作（选择器、状态行）直接执行；`/scoped-models` 显式延后，异步
//!    处理器放进本地任务，避免阻塞输入分发。
//!
//! # 注意事项
//!
//! - 命令匹配顺序与参数切片长度是 TS 冻结契约（`spec/ca-interactive-mode.md` §3.2）。
//!   即使调整实现，也按表序保留，便于逐条对照 TS；新增命令应放在表中对应位置。
//! - 键位回调默认只捕获 `Weak<RefCell<InteractiveModeInner>>`，模式释放后升级失败即
//!   静默跳过；`dispatch_submit` 里少数分支会直接 clone `InteractiveMode`/`AgentSession`
//!   （例如 `/scoped-models` 与压缩、流式提交），这些任务不依赖模式存活。
//! - 选择器生命周期由 `active_selector_token`（身份）与 `active_selector_dispose`（清理）
//!   两个槽位表示；`show_selector`/`done()` 的先后顺序见函数注释，改动会影响选择器能否
//!   正确关闭。
//! - 本模块按命令域拆成子模块；`mod.rs` 只放共享辅助、分派表和键盘接线，具体命令实现
//!   在 `auth`/`bash_compact`/`export_import_share`/`mcp`/`misc`/`model`/`panels`/
//!   `permissions`/`session`/`settings`/`tasks`/`thinking` 里。
//! - `test_*` 方法为 `tests/` 集成测试而声明为 `pub` 并加 `#[doc(hidden)]`，只做转发或
//!   状态读取，不参与生产路径。

pub mod auth;
pub mod bash_compact;
pub mod export_import_share;
pub mod mcp;
pub mod misc;
pub mod model;
pub mod panels;
pub mod permissions;
pub mod session;
pub mod settings;
pub mod tasks;
pub mod thinking;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use kiana_ai::types::ModelThinkingLevel;
use kiana_json::js_trim;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::tui::Component;

use crate::tui_renderer::{SharedComponentProxy, component_address};

use super::{ActiveEditor, InteractiveMode, InteractiveModeInner, SelectorToken};

/// 把 UI 动作延后到当前输入分发结束之后执行。
///
/// 默认编辑器由 `DefaultEditorProxy`/`EditorComponentProxy` 转发：代理在 `handle_input`
/// 期间对编辑器的 `RefCell`（以及渲染器）持有可变借用，回调里同步改编辑器文本、
/// 边框色或重入渲染器都会 panic。TS 的 `this` 没有借用限制（`setupKeyHandlers` /
/// `setupEditorSubmitHandler` 里的回调都同步改 UI），因此这里用 `spawn_local` 恢复同样的
/// 可重入语义：动作在本次 `handle_input` 返回后的下一个本地任务里执行。
///
/// 前提是调用线程处于 tokio 的 `LocalSet` 上下文（主程序以 current-thread + `LocalSet`
/// 运行，见 `crates/kiana-coding-agent/src/main.rs`）。若模式已经释放，`weak.upgrade()`
/// 返回 `None`，动作被静默丢弃——回调不允许在关闭后继续操作 UI。
fn run_after_dispatch<F>(weak: &Weak<RefCell<InteractiveModeInner>>, action: F)
where
    F: FnOnce(&mut InteractiveMode) + 'static,
{
    let weak = Weak::clone(weak);
    tokio::task::spawn_local(async move {
        if let Some(inner) = weak.upgrade() {
            let mut mode = InteractiveMode { inner };
            action(&mut mode);
        }
    });
}

/// `String.prototype.trimStart()`（ECMAScript 的 `WhiteSpace` + `LineTerminator`）。
///
/// 与 [`kiana_json::js_string::js_trim`] 同属 JS 语义：Rust 的 `str::trim_start` 不剥掉
/// U+FEFF（BOM/ZWNBSP）等码点，而 JS 会；`setupKeyHandlers` 的 bash 模式判定
/// （`text.trimStart().startsWith("!")`）因此必须用这里。
///
/// 返回原字符串的子切片（不分配内存），生命周期跟随 `text`；全部为空白时返回空串。
pub(crate) fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

/// ECMAScript `WhiteSpace` + `LineTerminator`（与 `kiana_json::js_string` 的集合一致）。
///
/// 与 Rust `char::is_whitespace` 的差异：本集合包含 U+FEFF（BOM/ZWNBSP），不包含
/// U+0085（NEL）；两侧判定必须用同一集合，否则 `trimStart` 的结果会与 TS 不一致。
pub(crate) fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `dispose` 槽：`done()` 与 `disposeActiveSelector()` 共享（只执行一次）。
type DisposeSlot = Rc<RefCell<Option<Box<dyn FnMut()>>>>;

/// `showSelector` 的 `done()` 回调（可克隆，交给选择器组件的多个回调）。
///
/// 克隆共享同一个 `Rc` 闭包。底层闭包由 `InteractiveMode::show_selector` 构造：先执行
/// dispose（即使令牌已过期），再校验选择器令牌，因此重复调用或“迟到的 done”不会关闭
/// 之后挂载的新选择器。
#[derive(Clone)]
pub(crate) struct SelectorDone(Rc<dyn Fn()>);

impl SelectorDone {
    /// 调用一次 `done()`。
    ///
    /// 可能被选择器的多个回调调用；令牌校验保证只有当前活动选择器会被关闭，重复调用
    /// 不会影响之后挂载的新选择器。
    pub(crate) fn finish(&self) {
        (self.0)();
    }

    /// 无操作的 `done`（测试接缝）。测试直接调用 `navigate_tree_from_selector` 时没有
    /// 真正的选择器可关闭，用 `noop` 代替组件传入的 `done`。
    pub(crate) fn noop() -> Self {
        SelectorDone(Rc::new(|| {}))
    }
}

/// 选择器显示后要聚焦的目标。
pub(crate) enum SelectorFocus {
    /// 聚焦选择器组件自身（TS `focus: selector`）。
    Component,
    /// 聚焦顶层组件的第 N 个直接子组件（TS `selector.getMessageList()`：组件本身
    /// `handlesInput` 为 false，输入必须直达内部列表）。
    ///
    /// N 通过 `for_each_container_child` 计数（会穿过共享句柄代理，得到稳定的直接
    /// 子组件下标）；越界时 `show_selector` 不改变焦点。
    Child(usize),
    /// 聚焦一个已经算好的内部组件地址（TS `selector.getSettingsList()`）。
    Address(usize),
}

/// `showSelector(create)` 的返回值（TS `{ component, focus, dispose? }`）。
pub(crate) struct SelectorCreation {
    /// 选择器组件（共享句柄：宿主与回调都要访问）。
    ///
    /// `show_selector` 会把它经 `SharedComponentProxy` 挂进 `editor_container`，替换当前
    /// 编辑器；容器因此持有组件引用。
    pub component: SharedComponent,
    /// 焦点目标。
    pub focus: SelectorFocus,
    /// `dispose?()`。
    ///
    /// 由 `show_selector` 保存到共享槽位；`done()` 或下一个选择器挂载时执行一次
    /// （`Option::take` 保证不重复）。用于释放选择器自己的监听器/临时资源。
    pub dispose: Option<Box<dyn FnMut()>>,
}

/// 把具体组件装箱成 [`SharedComponent`]（保持同一份堆分配）。
pub(crate) fn shared_component<T: Component + 'static>(component: T) -> SharedComponent {
    Rc::new(RefCell::new(component))
}

/// `Date.now()`（毫秒）。
///
/// 返回 `f64` 以对齐 TS 的 `number`；`handle_escape` 的 500 ms 双击判定依赖它。
pub(crate) fn now_millis() -> f64 {
    kiana_ai::model_catalog::now_epoch_millis() as f64
}

impl InteractiveMode {
    /// TS `private showSelector(create)`：把选择器挂在编辑器槽位。
    ///
    /// `create` 同时拿到 `&mut self`（构造 config 需要读取会话/设置）和 `done` 回调
    /// （对应 TS 闭包捕获的 `done`）。
    ///
    /// # 生命周期与顺序
    ///
    /// 1. 新建选择器令牌，并把 `done()` 与 dispose 闭包共享到同一个 `Option` 槽，
    ///    保证 dispose 至多执行一次。
    /// 2. 调用 `create` 构造组件；此时旧选择器仍挂在槽位里。
    /// 3. `dispose_active_selector()` 清理旧选择器，然后安装新令牌。
    /// 4. 清空 `editor_container` 并挂上 `SharedComponentProxy`（容器持有组件引用）。
    /// 5. 按 `SelectorFocus` 解析焦点地址并 `set_focus_to_address`，最后请求渲染。
    ///
    /// `done()` 先执行 dispose（即使令牌已过期），再确认自己仍是活动选择器；只有仍活动时
    /// 才清空两个槽位，并延后一个任务恢复编辑器槽。这样选择器在自身 `handle_input` 里
    /// 调用 `done()` 不会在分发过程中拆掉自己；若延迟期间已有新选择器挂载，则放弃恢复，
    /// 避免覆盖新选择器。
    ///
    /// # 边界
    ///
    /// - `create` 返回的 `SelectorFocus::Child(index)` 越界时地址为 `None`，焦点保持原样。
    /// - 模式在 `done()` 执行前被释放（`weak.upgrade()` 失败）时静默返回；dispose 已执行。
    pub(crate) fn show_selector<F>(&mut self, create: F)
    where
        F: FnOnce(&mut InteractiveMode, SelectorDone) -> SelectorCreation,
    {
        let token = Rc::new(SelectorToken);
        let dispose_slot: DisposeSlot = Rc::new(RefCell::new(None));
        let done = {
            let weak = self.downgrade();
            let token = Rc::clone(&token);
            let dispose_slot = Rc::clone(&dispose_slot);
            SelectorDone(Rc::new(move || {
                // TS `done()`：先 dispose（即使令牌已过期），再检查令牌。
                let dispose = dispose_slot.borrow_mut().take();
                if let Some(mut dispose) = dispose {
                    dispose();
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mode = InteractiveMode { inner };
                let is_active = mode
                    .inner
                    .borrow()
                    .active_selector_token
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &token));
                if !is_active {
                    return;
                }
                {
                    let mut inner = mode.inner.borrow_mut();
                    inner.active_selector_token = None;
                    inner.active_selector_dispose = None;
                }
                // UI 变更延后一个任务：`done()` 通常由选择器自身的 `handleInput`
                // 调用栈触发，立即 clear 编辑器槽位会 drop 正在分发的代理对象
                // （Rust 里是悬垂引用）。若这段时间里已有新选择器挂载，则不再覆盖它。
                let mut mode = mode;
                tokio::task::spawn_local(async move {
                    if mode.inner.borrow().active_selector_token.is_some() {
                        return;
                    }
                    mode.restore_editor_into_slot();
                    mode.request_render();
                });
            }))
        };

        // 先构造新选择器（`create` 可能读取当前会话/设置），再卸载旧选择器；新的
        // dispose 回调在构造后立刻放进共享槽，供 `done()`/`dispose_active_selector()` 复用。
        let creation = create(self, done);
        *dispose_slot.borrow_mut() = creation.dispose;

        self.dispose_active_selector();
        {
            let mut inner = self.inner.borrow_mut();
            inner.active_selector_token = Some(Rc::clone(&token));
            let slot = Rc::clone(&dispose_slot);
            inner.active_selector_dispose = Some(Box::new(move || {
                let dispose = slot.borrow_mut().take();
                if let Some(mut dispose) = dispose {
                    dispose();
                }
            }));
        }

        // 编辑器槽位只容纳一个子项：先清空（旧选择器的清理已由上面的
        // `dispose_active_selector()` 完成，这里只是 drop 挂载的代理），再挂新选择器的
        // 代理；容器通过代理持有组件引用。
        let container = Rc::clone(&self.inner.borrow().editor_container);
        {
            let mut container = container.borrow_mut();
            container.clear();
            container.add_child(Box::new(SharedComponentProxy::new(Rc::clone(
                &creation.component,
            ))));
        }
        let address = match creation.focus {
            SelectorFocus::Component => container
                .borrow()
                .children
                .last()
                .map(|child| component_address(child.as_ref())),
            SelectorFocus::Child(index) => {
                // `for_each_container_child` 会穿过代理层，枚举选择器自己的直接子组件；
                // 回调返回 `false` 提前终止。越界时 `address` 保持 `None`。
                let component = Rc::clone(&creation.component);
                let component = component.borrow();
                let mut address = None;
                let mut current = 0usize;
                component.for_each_container_child(&mut |child| {
                    if current == index {
                        address = Some(component_address(child));
                        false
                    } else {
                        current += 1;
                        true
                    }
                });
                address
            }
            SelectorFocus::Address(address) => Some(address),
        };
        if let Some(address) = address {
            self.inner.borrow_mut().ui.set_focus_to_address(address);
        }
        self.request_render();
    }

    /// `dispatch_submit` 的测试接缝。
    ///
    /// 直接调用分派函数，绕过编辑器的提交路径：编辑器不会自动清空文本、也不会展开
    /// 粘贴标记；命令分支自己负责清空编辑器，普通消息分支则不会。
    #[doc(hidden)]
    pub fn test_dispatch_submit(&mut self, text: &str) {
        self.dispatch_submit(text);
    }

    /// `getPathCommandArgument` 的测试接缝。
    #[doc(hidden)]
    pub fn test_path_command_argument(text: &str, command: &str) -> Option<String> {
        export_import_share::get_path_command_argument(text, command)
    }

    /// 最近一次 `showStatus` 的文本（测试断言用）。
    #[doc(hidden)]
    pub fn test_last_status_message(&self) -> String {
        self.inner.borrow().last_status_message.clone()
    }

    /// Anthropic 订阅警告是否已展示。
    #[doc(hidden)]
    pub fn test_anthropic_warning_shown(&self) -> bool {
        self.inner.borrow().anthropic_subscription_warning_shown
    }

    /// 当前是否处于 bash 模式。
    #[doc(hidden)]
    pub fn test_is_bash_mode(&self) -> bool {
        self.inner.borrow().is_bash_mode
    }

    /// 当前编辑器槽位内容的渲染文本（测试断言用）。
    #[doc(hidden)]
    pub fn test_selector_render_text(&self) -> String {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        let mut container = container.borrow_mut();
        match container.children.last_mut() {
            Some(child) => child.render(80).join("\n"),
            None => String::new(),
        }
    }

    /// 当前活动选择器是否存在且已挂载。
    #[doc(hidden)]
    pub fn test_has_active_selector(&self) -> bool {
        self.inner.borrow().active_selector_token.is_some()
    }

    /// 当前活动编辑器槽位里的组件地址（测试接缝）。
    #[doc(hidden)]
    pub fn test_selector_child_address(&self) -> Option<usize> {
        self.inner
            .borrow()
            .editor_container
            .borrow()
            .children
            .last()
            .map(|child| component_address(child.as_ref()))
    }

    /// `setupKeyHandlers()`：在默认编辑器上安装 Escape、`app.*` 动作、`onChange`、粘贴回调。
    ///
    /// 由 `init()` 在托管工具安装完成后调用；`init()` 开头检查 `is_initialized`，正常流程
    /// 只会安装一次。所有回调只捕获 `Weak`，模式释放后自动失效。会写编辑器/渲染器的同步
    /// 动作必须经 `run_after_dispatch` 延后（原因见模块文档）；两个例外：
    ///
    /// - `onCtrlD` 直接调用 `handle_ctrl_d`：后者只 `spawn_local` 关闭任务，不写编辑器；
    /// - `onDebug` 直接调用 `handle_debug_command`：后者自己检测渲染器借用，冲突时延后
    ///   重试（见 `commands/misc.rs`）。
    ///
    /// 处理器装在**默认编辑器**上。扩展替换编辑器时，只有当新编辑器本身仍是
    /// `CustomEditor`（TS 的鸭子判定），`set_custom_editor_component` 才会调用
    /// `copy_handlers_from` 把这些回调/动作表复制过去（见 `components/custom_editor.rs`
    /// 与 `extension_ui/mod.rs`）。
    pub(crate) fn setup_key_handlers(&mut self) {
        let editor = self.default_editor();

        // onEscape：优先级链（流式 → bash 运行中 → bash 模式 → 空编辑器的双击 Escape）。
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_on_escape(Some(Box::new(move || {
                run_after_dispatch(&weak, |mode| mode.handle_escape());
            })));
        }

        // 应用动作处理器（`AppKeybinding` 名称来自 `kiana-config/src/keybindings`）。
        // 同步动作统一走 `run_after_dispatch`；异步动作（模型循环、外部编辑器、复制、
        // follow-up、新会话）直接 spawn 本地任务。`onCtrlD` 是同步例外，见函数文档。
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.clear",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.handle_ctrl_c());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_on_ctrl_d(Some(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_ctrl_d();
                }
            })));
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.suspend",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.handle_ctrl_z());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.thinking.cycle",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.cycle_thinking_level());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.model.cycleForward",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.cycle_model("forward").await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.model.cycleBackward",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.cycle_model("backward").await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.model.select",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.show_model_selector(None));
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.tools.expand",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.toggle_tool_output_expansion());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.thinking.toggle",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.toggle_thinking_block_visibility());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.editor.external",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.handle_open_external_editor().await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.message.copy",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.handle_copy_command(CopyOptions {
                                flash_confirmation: true,
                                prefer_selection: true,
                            })
                            .await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.message.followUp",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.handle_follow_up().await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.message.dequeue",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.handle_dequeue());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.session.new",
                Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.handle_clear_command().await;
                        });
                    }
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.session.tree",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.show_tree_selector(None));
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.session.fork",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.show_user_message_selector());
                }),
            );
        }
        {
            let weak = self.downgrade();
            editor.borrow_mut().set_action_handler(
                "app.session.resume",
                Box::new(move || {
                    run_after_dispatch(&weak, |mode| mode.show_session_selector());
                }),
            );
        }

        // 全局调试处理器（不依赖焦点）。
        {
            let weak = self.downgrade();
            self.inner
                .borrow()
                .renderer
                .borrow_mut()
                .set_on_debug(Some(Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        InteractiveMode { inner }.handle_debug_command();
                    }
                })));
        }

        // bash 模式跟着编辑器文本变化。
        {
            let weak = self.downgrade();
            editor
                .borrow_mut()
                .set_on_change(Some(Box::new(move |text: &str| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let is_bash_mode = js_trim_start(text).starts_with('!');
                    // `setText()` 常在 `inner.borrow()` 调用栈里触发（宿主批量改文本），
                    // 此时无法取可变借用：等借用释放后按编辑器当前文本重新对齐。
                    //
                    // 快路径：借用成功，立即更新 `is_bash_mode`；只有边框色写回延后。
                    // 慢路径：连同状态更新一起延后，并重新读取编辑器当前文本（延迟期间
                    // 文本可能又变了，所以不复用回调参数 `text`）。
                    let changed = match inner.try_borrow_mut() {
                        Ok(mut inner) => {
                            std::mem::replace(&mut inner.is_bash_mode, is_bash_mode) != is_bash_mode
                        }
                        Err(_) => {
                            let weak = Rc::downgrade(&inner);
                            tokio::task::spawn_local(async move {
                                if let Some(inner) = weak.upgrade() {
                                    let mut mode = InteractiveMode { inner };
                                    let current =
                                        js_trim_start(&mode.inner.borrow().editor.get_text())
                                            .starts_with('!');
                                    let changed = {
                                        let mut inner = mode.inner.borrow_mut();
                                        std::mem::replace(&mut inner.is_bash_mode, current)
                                            != current
                                    };
                                    if changed {
                                        mode.update_editor_border_color();
                                    }
                                }
                            });
                            return;
                        }
                    };
                    if changed {
                        // 边框色写回编辑器（`setBorderColor`）：`onChange` 由编辑器自身的
                        // `handle_input`/`setText` 触发，此时编辑器 RefCell 仍被代理借用，
                        // 同步写回会 panic；延后一个任务即可（TS 没有借用限制）。
                        let weak = Rc::downgrade(&inner);
                        tokio::task::spawn_local(async move {
                            if let Some(inner) = weak.upgrade() {
                                InteractiveMode { inner }.update_editor_border_color();
                            }
                        });
                    }
                })));
        }

        // Ctrl+V：文件路径 → 图片 → 纯文本。
        {
            let weak = self.downgrade();
            editor
                .borrow_mut()
                .set_on_paste_image(Some(Box::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        tokio::task::spawn_local(async move {
                            mode.handle_clipboard_paste().await;
                        });
                    }
                })));
        }
    }

    /// `onEscape` 的优先级链（§2.50.8）。
    ///
    /// 只处理第一个命中的分支；优先级从高到低依次为：
    ///
    /// 1. 流式中：把 steering/follow-up 队列并回编辑器并中止当前回合
    ///    （`restore_queued_messages_to_editor(true, ...)` 会 `spawn_local` 调 `abort`）。
    /// 2. bash 正在运行：只中止 bash，不退出 bash 模式。
    /// 3. bash 模式（`is_bash_mode` 为真，由 `onChange` 与 bash 分派共同维护）：清空文本、
    ///    退出模式、更新边框色。
    /// 4. 编辑器为空（JS `trim` 语义）：按 `doubleEscapeAction` 判定双击。两次 Escape
    ///    间隔严格小于 500 ms 才触发；触发后把时间戳归零，避免第三次立即再次触发。
    ///    `Tree` 打开树选择器，`Fork`（`_` 分支）打开用户消息选择器，`None` 不做任何事。
    fn handle_escape(&mut self) {
        if self.session().is_streaming() {
            self.restore_queued_messages_to_editor(true, None);
        } else if self.session().is_bash_running() {
            self.session().abort_bash();
        } else if self.inner.borrow().is_bash_mode {
            self.inner.borrow().editor.set_text("");
            self.inner.borrow_mut().is_bash_mode = false;
            self.update_editor_border_color();
        } else if js_trim(&self.inner.borrow().editor.get_text()).is_empty() {
            let action = self.settings_manager().get_double_escape_action();
            if action != kiana_config::settings::schema::DoubleEscapeAction::None {
                let now = now_millis();
                let last_escape_time = self.inner.borrow().last_escape_time;
                if now - last_escape_time < 500.0 {
                    match action {
                        kiana_config::settings::schema::DoubleEscapeAction::Tree => {
                            self.show_tree_selector(None);
                        }
                        _ => self.show_user_message_selector(),
                    }
                    self.inner.borrow_mut().last_escape_time = 0.0;
                } else {
                    self.inner.borrow_mut().last_escape_time = now;
                }
            }
        }
    }

    /// `cycleThinkingLevel()`：在当前模型可用的 thinking 级别里循环到下一个。
    ///
    /// `AgentSession::cycle_thinking_level` 在模型不支持 thinking 或没有可用级别时返回
    /// `None`；此时只显示提示，不改 footer/边框。成功时先失效 footer、更新边框色
    /// （边框随 thinking level 变化），再显示 `Thinking level: <level>`。
    pub(crate) fn cycle_thinking_level(&mut self) {
        let new_level = self.session().cycle_thinking_level(Default::default());
        match new_level {
            None => self.show_status("Current model does not support thinking"),
            Some(new_level) => {
                self.inner.borrow().footer.borrow_mut().invalidate();
                self.update_editor_border_color();
                self.show_status(&format!("Thinking level: {}", level_str(new_level)));
            }
        }
    }

    /// `cycleModel(direction)`：在当前可用模型列表里循环切换。
    ///
    /// `direction` 来自按键处理器（`"forward"` / `"backward"`），原样交给
    /// `AgentSession::cycle_model`；成功时依次：失效 footer、更新编辑器边框色（边框随
    /// thinking level 变化）、显示 `Switched to <name>`，只有模型声明 `reasoning` 且级别
    /// 不是 `Off` 时才附加 ` (thinking: <level>)`。名称优先用 `model.name`，为空时退回
    /// `model.id`。最后按新模型检查 Anthropic 订阅认证警告（可能再显示一条警告）。
    ///
    /// # 结果分支
    ///
    /// - `Err`：`show_error`，展示会话层错误的 `Display` 文本。
    /// - `Ok(None)`：没有可切换的模型。scope 为空说 `Only one model available`，非空说
    ///   `Only one model in scope`（scope 里过滤后只剩当前一个时也走这里）。
    /// - `Ok(Some)`：切换成功，见上。
    pub(crate) async fn cycle_model(&mut self, direction: &str) {
        let result = self
            .session()
            .cycle_model(direction, Default::default())
            .await;
        match result {
            Err(error) => self.show_error(&error.to_string()),
            Ok(None) => {
                let message = if self.session().scoped_models().is_empty() {
                    "Only one model available"
                } else {
                    "Only one model in scope"
                };
                self.show_status(message);
            }
            Ok(Some(result)) => {
                self.inner.borrow().footer.borrow_mut().invalidate();
                self.update_editor_border_color();
                let thinking_str = if result.model.reasoning == Some(true)
                    && result.thinking_level != ModelThinkingLevel::Off
                {
                    format!(" (thinking: {})", level_str(result.thinking_level))
                } else {
                    String::new()
                };
                let name = if result.model.name.is_empty() {
                    result.model.id.clone()
                } else {
                    result.model.name.clone()
                };
                self.show_status(&format!("Switched to {name}{thinking_str}"));
                let model = result.model.clone();
                self.maybe_warn_about_anthropic_subscription_auth_for(Some(model))
                    .await;
            }
        }
    }

    /// 当前编辑器（共享句柄），供 `/share`、`/bug` 恢复编辑器槽位。
    ///
    /// 每次调用都新建一个代理（`DefaultEditorProxy` 或 `EditorComponentProxy`），它和
    /// 槽位里已挂载的代理是两个对象，但都转发到同一底层编辑器。调用方把返回的句柄交给
    /// 子系统挂到 `editor_container`；结束后由 `restore_editor_into_slot()` 重建标准代理
    /// 并重新设置焦点（旧代理地址不能继续使用）。
    pub(crate) fn current_editor_shared(&self) -> SharedComponent {
        let inner = self.inner.borrow();
        match &inner.editor {
            ActiveEditor::Default { handle, .. } => {
                Rc::new(RefCell::new(super::DefaultEditorProxy(Rc::clone(handle))))
                    as SharedComponent
            }
            ActiveEditor::Custom { handle, .. } => {
                Rc::new(RefCell::new(super::EditorComponentProxy(Rc::clone(handle))))
                    as SharedComponent
            }
        }
    }

    /// `showExtensionEditor(title)`：`/tree` 的自定义摘要指令输入。
    ///
    /// TS 的 `showExtensionEditor` 走扩展 UI 上下文的 `editor` 请求；这里复用同一个上下文
    /// 实现（`extension_ui::ExtensionUiContext::editor`），避免依赖对话框栈的私有 token。
    ///
    /// 返回 `Some(text)` 表示用户提交（文本可能为空）；`None` 表示用户取消或响应通道
    /// 关闭（模式已释放时由 `resolve_responder_default` 按默认值收尾）。该调用会等待用户
    /// 操作，期间编辑器槽位被对话框占用；调用方必须处于 `LocalSet` 上下文。
    pub(crate) async fn show_extension_editor(&mut self, title: &str) -> Option<String> {
        let context = self.create_extension_ui_context();
        context.editor(title, None).await
    }

    /// `setupEditorSubmitHandler()`：把默认编辑器的提交回调接到 `dispatch_submit`。
    ///
    /// 回调参数 `&str` 指向提交时构造的临时字符串，只在本次回调内有效；且
    /// `run_after_dispatch` 的 future 要求 `'static`，因此先复制成 `String` 再移交。
    /// 真实提交路径上，编辑器在调用 `on_submit` 之前已经清空文本、
    /// 展开粘贴标记并按 JS 空白 trim（`kiana-tui` 的 `submit_value`）；本处理器只负责
    /// 调度，不重复做这些事。
    pub(crate) fn setup_editor_submit_handler(&mut self) {
        let weak = self.downgrade();
        self.default_editor()
            .borrow_mut()
            .set_on_submit(Some(Box::new(move |text: &str| {
                let text = text.to_string();
                run_after_dispatch(&weak, move |mode| mode.dispatch_submit(&text));
            })));
    }

    /// `onSubmit` 的分派表（§3.2）。
    ///
    /// 输入先按 JS `trim`（包含 U+FEFF 等码点）归一化；空串直接返回。分派顺序是冻结
    /// 契约：内置命令逐条精确/前缀匹配并 `return`，未命中的才进入 bash、压缩、流式、
    /// 普通消息分支。新增命令必须放在表中对应位置（`spec/ca-interactive-mode.md` §3.2）。
    ///
    /// 命令分支都会清空编辑器，但时机不同：`/model`、`/thinking`、`/scoped-models` 等在
    /// 调用处理器前清空；`/settings` 在挂载选择器后清空；`/export`、`/import`、`/share`、
    /// `/copy` 在异步处理器返回后清空。这是 TS 调用顺序的复刻，不要“统一”。真实提交路径
    /// 上编辑器已经清空并 trim；直接调用 `test_dispatch_submit` 时则依赖这些分支自己清空。
    ///
    /// # 参数与边界
    ///
    /// - 参数切片用 `slice_chars`（UTF-16 code unit）。以 `Option<String>` 传参的命令
    ///   （`/model`、`/thinking`、`/login`、`/compact`）：精确形式得到 `None`，
    ///   `"<cmd> "` 前缀形式即使参数为空也得到 `Some("")`；`/bug` 会把空串再转回 `None`。
    /// - bash 已在运行时只显示警告并把原文放回编辑器，不执行新命令。
    /// - 压缩/流式分支里的 `session.prompt` 错误被 `let _` 丢弃；普通消息放入
    ///   `pending_user_inputs` 或交给 `on_input_callback`。
    fn dispatch_submit(&mut self, text: &str) {
        let text = js_trim(text);
        if text.is_empty() {
            return;
        }

        // 阶段 1：内置命令表。顺序与 TS 的 `onSubmit` 分派表逐条对齐（§3.2）；
        // 每个分支处理完立即 `return`，不会落入后面的 bash/消息分支。
        if text == "/settings" {
            self.show_settings_selector();
            self.clear_editor();
            return;
        }
        if text == "/scoped-models" {
            self.clear_editor();
            let mut mode = self.clone();
            tokio::task::spawn_local(async move {
                mode.show_models_selector();
            });
            return;
        }
        if text == "/model" || text.starts_with("/model ") {
            let search_term = if text.starts_with("/model ") {
                Some(js_trim(&slice_chars(text, 7)).to_string())
            } else {
                None
            };
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_model_command(search_term).await;
                }
            });
            return;
        }
        if text == "/thinking" || text.starts_with("/thinking ") {
            let search_term = if text.starts_with("/thinking ") {
                Some(js_trim(&slice_chars(text, 10)).to_string())
            } else {
                None
            };
            self.clear_editor();
            self.handle_thinking_command(search_term);
            return;
        }
        if text == "/export" || text.starts_with("/export ") {
            let weak = self.downgrade();
            let text = text.to_string();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_export_command(&text).await;
                    mode.clear_editor();
                }
            });
            return;
        }
        if text == "/import" || text.starts_with("/import ") {
            let weak = self.downgrade();
            let text = text.to_string();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_import_command(&text).await;
                    mode.clear_editor();
                }
            });
            return;
        }
        if text == "/share" {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_share_command().await;
                    mode.clear_editor();
                }
            });
            return;
        }
        if text == "/bug" || text.starts_with("/bug ") {
            // `text.slice("/bug".length)`：精确 `/bug` 切片为空串（`hint = None`）；
            // `"/bug "` 前缀形式保留其后的参数（JS `slice` 按 UTF-16 code unit 计数）。
            let hint = js_trim(&slice_chars(text, "/bug".len())).to_string();
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_bug_command(if hint.is_empty() { None } else { Some(hint) })
                        .await;
                }
            });
            return;
        }
        if text == "/copy" {
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_copy_command(CopyOptions::default()).await;
                    mode.clear_editor();
                }
            });
            return;
        }
        if text == "/name" || text.starts_with("/name ") {
            self.handle_name_command(text);
            self.clear_editor();
            return;
        }
        if text == "/session" {
            self.handle_session_command();
            self.clear_editor();
            return;
        }
        if text == "/changelog" {
            self.handle_changelog_command();
            self.clear_editor();
            return;
        }
        if text == "/hotkeys" {
            self.handle_hotkeys_command();
            self.clear_editor();
            return;
        }
        if text == "/fork" {
            self.show_user_message_selector();
            self.clear_editor();
            return;
        }
        if text == "/clone" {
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_clone_command().await;
                }
            });
            return;
        }
        if text == "/tree" {
            self.show_tree_selector(None);
            self.clear_editor();
            return;
        }
        if text == "/trust" {
            self.show_trust_selector();
            self.clear_editor();
            return;
        }
        if text == "/login" || text.starts_with("/login ") {
            let provider_ref = if text.starts_with("/login ") {
                Some(js_trim(&slice_chars(text, 7)).to_string())
            } else {
                None
            };
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }
                        .handle_login_command(provider_ref)
                        .await;
                }
            });
            return;
        }
        if text == "/logout" {
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }
                        .show_oauth_selector("logout")
                        .await;
                }
            });
            return;
        }
        if text == "/new" {
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_clear_command().await;
                }
            });
            return;
        }
        if text == "/compact" || text.starts_with("/compact ") {
            let custom_instructions = if text.starts_with("/compact ") {
                Some(js_trim(&slice_chars(text, 9)).to_string())
            } else {
                None
            };
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }
                        .handle_compact_command(custom_instructions)
                        .await;
                }
            });
            return;
        }
        if text == "/reload" {
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.handle_reload_command().await;
                }
            });
            return;
        }
        if text == "/debug" {
            self.handle_debug_command();
            self.clear_editor();
            return;
        }
        if text == "/arminsayshi" {
            self.handle_armin_says_hi();
            self.clear_editor();
            return;
        }
        if text == "/dementedelves" {
            self.handle_demented_delves();
            self.clear_editor();
            return;
        }
        if text == "/resume" {
            self.show_session_selector();
            self.clear_editor();
            return;
        }
        if text == "/quit" {
            self.clear_editor();
            let weak = self.downgrade();
            tokio::task::spawn_local(async move {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.shutdown(false).await;
                }
            });
            return;
        }

        // 阶段 1.5：面板命令（`/permissions`、`/mcp`、`/tasks`）。名字与 24 个内置命令、
        // 扩展命令（`mcp`/`llama`）无交集；放在内置链之后、bash 之前，保持冻结表的
        // 匹配顺序不变（设计文档 §9.1）。
        if self.try_dispatch_panel_command(text) {
            self.clear_editor();
            return;
        }

        // 阶段 2：bash 命令。`!` 普通执行，`!!` 额外把 `exclude_from_context` 传给会话。
        // `!!` 从第 2 个 UTF-16 单元切、`!` 从第 1 个切；命令文本为空时忽略，继续走
        // 后面的消息分支。
        if text.starts_with('!') {
            let is_excluded = text.starts_with("!!");
            let command = if is_excluded {
                js_trim(&slice_chars(text, 2)).to_string()
            } else {
                js_trim(&slice_chars(text, 1)).to_string()
            };
            if !command.is_empty() {
                if self.session().is_bash_running() {
                    self.show_warning(
                        "A bash command is already running. Press Esc to cancel it first.",
                    );
                    self.inner.borrow().editor.set_text(text);
                    return;
                }
                self.inner.borrow().editor.add_to_history(text);
                let weak = self.downgrade();
                tokio::task::spawn_local(async move {
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        mode.handle_bash_command(command, is_excluded).await;
                        mode.inner.borrow_mut().is_bash_mode = false;
                        mode.update_editor_border_color();
                    }
                });
                return;
            }
        }

        // 阶段 3：压缩进行中。扩展命令绕过队列立即 `prompt`（TS 语义），普通文本以 steer
        // 模式入队；压缩结束后由 `flush_compaction_queue` 取走并提交。两条路径都先把原文
        // 加入编辑器历史并清空编辑器。
        if self.session().is_compacting() {
            if self.is_extension_command(text) {
                self.inner.borrow().editor.add_to_history(text);
                self.inner.borrow().editor.set_text("");
                let session = self.session();
                let text = text.to_string();
                tokio::task::spawn_local(async move {
                    let _ = session.prompt(text, Default::default()).await;
                });
            } else {
                self.queue_compaction_message(text, super::CompactionQueueMode::Steer);
            }
            return;
        }

        // 阶段 4：流式中。以 `StreamingBehavior::Steer` 提交给当前回合；`prompt` 完成后
        // 刷新 pending 显示并请求渲染。`prompt` 的错误被 `let _` 丢弃，不向用户显示。
        if self.session().is_streaming() {
            self.inner.borrow().editor.add_to_history(text);
            self.inner.borrow().editor.set_text("");
            let session = self.session();
            let weak = self.downgrade();
            let text = text.to_string();
            tokio::task::spawn_local(async move {
                let _ = session
                    .prompt(
                        text,
                        kiana_session::agent_session::PromptOptions {
                            streaming_behavior: Some(
                                kiana_ext_api::events::StreamingBehavior::Steer,
                            ),
                            ..Default::default()
                        },
                    )
                    .await;
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.update_pending_messages_display();
                    mode.request_render();
                }
            });
            return;
        }

        // 阶段 5：普通消息。先把 `pending_bash_components` 里的 bash 执行组件从 pending
        // 容器移入聊天容器，再交给等待中的 `getUserInput()` 回调；没有等待者时入队，
        // 由下一次 `get_user_input` 取走。最后把文本加入编辑器历史（真实提交路径上
        // 编辑器已在调用回调前清空）。
        self.flush_pending_bash_components();
        let callback = self.inner.borrow_mut().on_input_callback.take();
        if let Some(mut callback) = callback {
            callback(text);
        } else {
            self.inner
                .borrow_mut()
                .pending_user_inputs
                .push(text.to_string());
        }
        self.inner.borrow().editor.add_to_history(text);
    }
}

/// 当前模型的 thinking level 转字符串（TS 的模板 `${level}`）。
///
/// 输出直接进入状态行与模型选择器，拼写/大小写是用户可见契约，不要改用 `Debug`
/// 输出；每个 `ModelThinkingLevel` 变体都必须有映射。
pub(crate) fn level_str(level: ModelThinkingLevel) -> String {
    match level {
        ModelThinkingLevel::Off => "off".to_string(),
        ModelThinkingLevel::Minimal => "minimal".to_string(),
        ModelThinkingLevel::Low => "low".to_string(),
        ModelThinkingLevel::Medium => "medium".to_string(),
        ModelThinkingLevel::High => "high".to_string(),
        ModelThinkingLevel::Xhigh => "xhigh".to_string(),
        ModelThinkingLevel::Max => "max".to_string(),
    }
}

/// JS `text.slice(start)`（按 UTF-16 code unit；代理对中间切开产生 U+FFFD）。
///
/// `start` 是 UTF-16 单元下标（不是字节下标，也不是 `char` 下标）；`start` 越界返回
/// 空串。命令分派里的 7/10/4/9 等硬编码长度是当前稳定契约（spec §3.2），修改分支时
/// 必须同步修改这些数字。
pub(crate) fn slice_chars(text: &str, start: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if start >= units.len() {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..])
}

/// `/copy` 的选项（TS `{ flashConfirmation?, preferSelection? }`）。
///
/// `Default`（两个 `false`）用于 `/copy` 命令；按键处理器 `app.message.copy` 显式传
/// `flash_confirmation: true, prefer_selection: true`。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CopyOptions {
    /// `flashConfirmation`。
    ///
    /// 复制成功且处于全屏模式时用 `flash("Copied!")` 显示瞬时提示；否则走
    /// `show_status`（见 `export_import_share::handle_copy_command`）。
    pub flash_confirmation: bool,
    /// `preferSelection`。
    ///
    /// 优先复制当前选择：仅当全屏模式、copy-on-select 关闭且存在活动选择时成立；
    /// 其余情况回退到复制最后一条 agent 文本。
    pub prefer_selection: bool,
}
