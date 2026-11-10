//! `/mcp` 面板的命令层（`tui-panels-design.md` §5）。
//!
//! 与 `/permissions`、`/tasks` 不同，`/mcp` 在 TS 参照实现里由内建扩展注册；本模块
//! 只负责**在 TUI 中输入精确的 `/mcp` 时**打开新的 [`McpDialog`]。带参数的
//! `/mcp login`、`/mcp logout`、`/mcp reconnect` 不在这里匹配，继续落到
//! `session.prompt` 由扩展处理（保持既有行为与输出）。
//!
//! # 数据源
//!
//! 面板只读 `mcp.json`（[`McpDialogSnapshot::load`]），不调用 `kiana mcp list --json`，
//! 也不建立任何连接。启用/禁用通过 [`write_mcp_server_enabled`] 写回条目来源文件；
//! 运行期状态（connected/needs-auth/failed）由扩展的连接管理器持有，当前面板显示
//! `configured`。这是本集成的已知差距，等扩展侧提供稳定状态快照后由命令层回填。
//!
//! # 交互与借用
//!
//! `r` 刷新与启停写回都在 `spawn_local` 里完成：回调位于组件的 `handle_input` 借用
//! 栈上，不能同步重借对话框。登录/登出需要扩展的 OAuth 流程与 UI 通道，本面板只返回
//! 明确错误提示用户使用扩展命令，不伪造成功。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use kiana_builtin_extensions::mcp::config::LoadMcpConfigOptions;

use crate::components::mcp_dialog::{
    McpDialog, McpDialogCallbacks, McpDialogOptions, McpDialogSnapshot, write_mcp_server_enabled,
};

use super::super::{InteractiveMode, InteractiveModeInner};
use super::{SelectorCreation, SelectorFocus};

/// 对话框的共享槽位（刷新回调在构造前创建）。
type DialogSlot = Rc<RefCell<Option<Rc<RefCell<McpDialog>>>>>;

impl InteractiveMode {
    /// 打开 `/mcp` 面板（精确命令；子命令仍由扩展处理）。
    pub(crate) fn show_mcp_selector(&mut self) {
        let session = self.session();
        let settings = self.settings_manager();
        let cwd = PathBuf::from(session.session_manager().get_cwd());
        let options = LoadMcpConfigOptions {
            agent_dir: kiana_core::config::get_agent_dir(),
            cwd,
            project_trusted: settings.is_project_trusted(),
        };
        let snapshot = McpDialogSnapshot::load(options.clone());
        let weak = self.downgrade();

        self.show_selector(move |_mode, done| {
            let slot: DialogSlot = Rc::new(RefCell::new(None));

            let callbacks = McpDialogCallbacks {
                on_toggle: {
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let options = options.clone();
                    Box::new(move |row, enabled| {
                        write_mcp_server_enabled(row, enabled)?;
                        schedule_refresh(&slot, &options, &weak);
                        Ok(())
                    })
                },
                on_login: Box::new(|row| {
                    Err(format!(
                        "Sign-in for \"{}\" is not available from this panel; use the extension /mcp command.",
                        row.name
                    ))
                }),
                on_logout: Box::new(|row| {
                    Err(format!(
                        "Sign-out for \"{}\" is not available from this panel; use the extension /mcp command.",
                        row.name
                    ))
                }),
                on_refresh: {
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let options = options.clone();
                    Box::new(move || schedule_refresh(&slot, &options, &weak))
                },
                on_close: Box::new(move || done.finish()),
            };

            let dialog = Rc::new(RefCell::new(McpDialog::new(McpDialogOptions {
                snapshot,
                callbacks,
            })));
            *slot.borrow_mut() = Some(Rc::clone(&dialog));
            SelectorCreation {
                component: kiana_tui::layout_node::shared(Rc::clone(&dialog)),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }
}

/// 重新读取 `mcp.json` 并回填面板（延迟到本次输入分发之后）。
fn schedule_refresh(
    slot: &DialogSlot,
    options: &LoadMcpConfigOptions,
    weak: &Weak<RefCell<InteractiveModeInner>>,
) {
    let slot = Rc::clone(slot);
    let options = options.clone();
    let weak = weak.clone();
    tokio::task::spawn_local(async move {
        let dialog = slot.borrow().clone();
        if let Some(dialog) = dialog {
            let snapshot = McpDialogSnapshot::load(options);
            dialog.borrow_mut().set_snapshot(snapshot);
        }
        if let Some(inner) = weak.upgrade() {
            InteractiveMode { inner }.request_render();
        }
    });
}
