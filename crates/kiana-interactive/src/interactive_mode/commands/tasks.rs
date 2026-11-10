//! `/tasks` 面板的命令层（`tui-panels-design.md` §6）。
//!
//! 打开面板时异步拉取本会话的后台任务记录（`AgentSession::list_background_tasks`），
//! 再挂载 [`TasksDialog`]。取消、刷新都走 `spawn_local`：
//!
//! - `c` 只在 Running 状态由组件回调；命令层立即返回 `Ok(true)`（组件据此显示
//!   `Cancelled <id>` 并自动触发一次刷新），真正的 `cancel_background_task` 在本地任务里
//!   执行，失败时通过 [`TasksDialog::set_notice`] 回填错误；
//! - `r` 重新拉取快照，组件按 task id 保留选中行；
//! - 面板关闭后异步结果通过 `Weak`/`Rc` 槽位丢弃，不会操作已卸载的组件。
//!
//! manager 未启用（continuity 关闭）时快照是 [`TasksDialogSnapshot::disabled`]，面板显示
//! `Background tasks are not enabled for this session.`；拉取失败保留 `load_error`。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_session::AgentSession;

use crate::components::tasks_dialog::{
    TasksDialog, TasksDialogCallbacks, TasksDialogOptions, TasksDialogSnapshot, TasksNotice,
    load_tasks_snapshot,
};

use super::super::InteractiveMode;
use super::{SelectorCreation, SelectorFocus};

/// 对话框的共享槽位（构造后由刷新/取消回调回填）。
type DialogSlot = Rc<RefCell<Option<Rc<RefCell<TasksDialog>>>>>;

impl InteractiveMode {
    /// 打开 `/tasks` 面板：先异步取快照，再挂载选择器。
    pub(crate) fn show_tasks_selector(&mut self) {
        let session = self.session();
        let weak = self.downgrade();
        tokio::task::spawn_local(async move {
            let snapshot = load_snapshot(&session).await;
            if let Some(inner) = weak.upgrade() {
                InteractiveMode { inner }.mount_tasks_selector(snapshot);
            }
        });
    }

    /// 用已取好的快照挂载对话框。
    fn mount_tasks_selector(&mut self, snapshot: TasksDialogSnapshot) {
        let session = self.session();
        let weak = self.downgrade();
        self.show_selector(move |_mode, done| {
            let slot: DialogSlot = Rc::new(RefCell::new(None));

            let callbacks = TasksDialogCallbacks {
                on_cancel: {
                    let session = session.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    Box::new(move |record| {
                        // `Ok(true)` 让组件显示 Cancelled 并触发 on_refresh；取消结果
                        // 异步回填，失败时用 notice 覆盖成功文案。
                        let task_id = record.task_id.clone();
                        let session = session.clone();
                        let slot = Rc::clone(&slot);
                        let weak = weak.clone();
                        tokio::task::spawn_local(async move {
                            let result = session.cancel_background_task(&task_id).await;
                            let message = match result {
                                Ok(true) => None,
                                Ok(false) => Some(TasksNotice::info("Task is already finished.")),
                                Err(error) => Some(TasksNotice::error(error.to_string())),
                            };
                            let dialog = slot.borrow().clone();
                            if let Some(dialog) = dialog {
                                if let Some(message) = message {
                                    dialog.borrow_mut().set_notice(Some(message));
                                }
                                let snapshot = load_snapshot(&session).await;
                                dialog.borrow_mut().set_snapshot(snapshot);
                            }
                            if let Some(inner) = weak.upgrade() {
                                InteractiveMode { inner }.request_render();
                            }
                        });
                        Ok(true)
                    })
                },
                on_refresh: {
                    let session = session.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    Box::new(move || {
                        let session = session.clone();
                        let slot = Rc::clone(&slot);
                        let weak = weak.clone();
                        tokio::task::spawn_local(async move {
                            let snapshot = load_snapshot(&session).await;
                            let dialog = slot.borrow().clone();
                            if let Some(dialog) = dialog {
                                dialog.borrow_mut().set_snapshot(snapshot);
                            }
                            if let Some(inner) = weak.upgrade() {
                                InteractiveMode { inner }.request_render();
                            }
                        });
                    })
                },
                on_close: Box::new(move || done.finish()),
            };

            let dialog = Rc::new(RefCell::new(TasksDialog::new(TasksDialogOptions {
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

/// 拉取一份任务快照；区分“manager 未启用”与“读取失败”。
async fn load_snapshot(session: &AgentSession) -> TasksDialogSnapshot {
    if session.background_task_manager().is_none() {
        return TasksDialogSnapshot::disabled();
    }
    load_tasks_snapshot(|| {
        let session = session.clone();
        async move {
            session
                .list_background_tasks()
                .await
                .map(Some)
                .map_err(|error| error.to_string())
        }
    })
    .await
}
