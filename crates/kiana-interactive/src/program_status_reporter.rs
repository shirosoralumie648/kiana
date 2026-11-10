//! modes/interactive/program-status-reporter.ts。
//!
//! 把交互模式状态上报给终端（OSC 7501）：agent 运行与压缩期间 `working`，对话框等待用户
//! 时 `blocked`，结束后 `done` / `error` / `idle`。消息只包含会话名、对话框标题和错误的
//! 首行；prompt 与 assistant 输出永不上报。
//!
//! # 为什么需要这个模块
//! 交互模式本身关注渲染与输入，而终端状态栏（OSC 7501）只需要一个很窄的事实：agent
//! 正在运行、正在等用户、还是已经结束。把这段判定集中在这里，事件转发路径和命令/对话框
//! 代码只需调用 `handle_event` / `set_blocked`，不必各自拼装状态，也就不会出现两处实现
//! 对“当前应该显示什么”给出不同答案。
//!
//! # 边界：什么会离开本模块
//! 一次上报只包含 `state`、可选的 `kind`、可选的一行 `message`，以及固定的 `app`。
//! `message` 只有三个来源：
//! - `working` / `done`：当前会话名；
//! - `blocked`：对话框标题；
//! - `error`：错误文本的首行。
//!
//! prompt、assistant 输出与工具结果从不进入本模块；`handle_event` 只读取 `MessageEnd`
//! 消息的 `role` / `stop_reason` / `error_message`。`kind` 只在 `state == blocked` 时写出。
//!
//! # 调用方
//! - `InteractiveMode` 持有本类型。`subscribe_to_agent` 收到的事件经
//!   `InteractiveMode::handle_event` 转发到这里的 `handle_event`
//!   （`interactive_mode/events.rs`）。
//! - 扩展对话框以 source `"extension-dialog"`、登录流程以 `"login"` 调用 `set_blocked`。
//! - `rebind_current_session` 在切换会话前调用 `reset`。
//!
//! # 状态模型
//! 上报内容不是某个字段的直读，而是 `current_status` 按固定优先级合成：
//! 1. `blocked` 列表非空 → 取最后打开的对话框（`blocked`）；
//! 2. `compacting` → `working` + `"Compacting context"`；
//! 3. `run_active` → `working`；否则 → `resting_status`。
//!    其中 `working` / `done` 两态的 `message` 会被替换为当前会话名。
//!
//! `run_active` / `compacting` / `blocked` 是输入，`run_result` / `resting_status` 是记忆：
//! run 期间的最新结果只写进 `run_result`，到 `AgentSettled` 才落到 `resting_status`。
//!
//! # 去重与上报
//! `report` 用与 TS `JSON.stringify` 相同键序的字符串做去重键；键不变就不调用
//! `Terminal::set_program_status`。因此“处理了一个事件”不等于“发生了一次终端写入”。
//!
//! # 注意事项
//! - `reset` 只清 run 相关状态，不清 `blocked` 列表；仓库内的调用方分别在结束时用
//!   `set_blocked(source, None)` 清自己的槽。
//! - `last_report` 不随 `reset` 清空，所以重置后的状态若与上次完全一致会被去重吞掉。
//! - `get_terminal` 每次 `report` 都重新调用，终端的替换不需要通知本类型。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_ai::types::StopReason;
use kiana_core::config::APP_NAME;
use kiana_session::agent_session::AgentSessionEvent;
use kiana_tui::program_status::state_name;
use kiana_tui::terminal_colors::{ProgramStatus, ProgramStatusKind, ProgramStatusState, Terminal};
use kiana_tui::text::trim_js;
use serde_json::{Map, Value};

/// `BlockedStatus = { kind: NonNullable<ProgramStatus["kind"]>; message: string }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedStatus {
    /// 对话框类型。
    pub kind: ProgramStatusKind,
    /// 对话框标题（作为 `blocked` 的 message）。
    pub message: String,
}

/// `firstLine(text)`：`text?.split(/\r?\n/, 1)[0]?.trim() || "Error"`。
fn first_line(text: Option<&str>) -> String {
    let Some(text) = text else {
        return "Error".to_string();
    };
    // `/\r?\n/` 只在 `\n`（可带前导 `\r`）处切分；孤立的 `\r` 不是分隔符。
    let first = text.split('\n').next().unwrap_or("");
    let trimmed = trim_js(first);
    if trimmed.is_empty() {
        "Error".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 终端 getter（TS `() => Terminal`）。
///
/// 每次 `report` 都会调用一次；getter 每次重新解析当前渲染器持有的终端
/// （`switchTuiMode` 原地替换的是渲染器，同一个终端经 `SharedTerminal` 交给新渲染器），
/// 因此渲染器或终端的变化无需通知本类型。
/// 回调没有 `Send` / `Sync` 约束：上报器在 UI 的本地任务里使用（事件由 `spawn_local`
/// 循环投递，见 `interactive_mode/events.rs`）。
pub type GetTerminal = Box<dyn Fn() -> Rc<RefCell<Box<dyn Terminal>>>>;

/// 会话名 getter（TS `() => string | undefined`）。
///
/// 只在合成 `working` / `done` 状态时调用；返回 `None` 时该状态没有 `message` 字段。
pub type GetSessionName = Box<dyn Fn() -> Option<String>>;

/// 交互模式状态上报器（TS `class ProgramStatusReporter`）。
///
/// 负责把 agent 事件、压缩状态与对话框阻塞合成成一个终端程序状态，并在内容变化时
/// 通过 [`GetTerminal`] 交给终端写出。它不持有事件订阅：调用方必须显式把
/// `AgentSessionEvent` 送进 `handle_event`。
///
/// 内部状态分三类：
/// - 瞬时状态：`run_active`、`compacting`，以及按打开顺序排列的 `blocked` 列表；
/// - 记忆：`run_result`（当前 run 的最新结果）、`resting_status`（没有 run 时的状态）；
/// - 去重：`last_report`（上次上报的 JSON 键）。
pub struct ProgramStatusReporter {
    get_terminal: GetTerminal,
    get_session_name: GetSessionName,
    run_active: bool,
    compacting: bool,
    /// 当前 run 的结果，settle 时上报。
    run_result: ProgramStatus,
    /// 没有 run 时的静止状态。
    resting_status: ProgramStatus,
    /// 按打开顺序排列的对话框（最新的在末尾）。
    blocked: Vec<(String, BlockedStatus)>,
    last_report: Option<String>,
}

impl ProgramStatusReporter {
    /// TS `constructor(getTerminal, getSessionName)`。
    ///
    /// 初始状态是“没有 run、未压缩、没有对话框”：合成结果为 `idle`。构造函数不上报，
    /// 第一次状态写出发生在调用方显式调用 `report` 或第一个被处理的事件之后。
    pub fn new(get_terminal: GetTerminal, get_session_name: GetSessionName) -> Self {
        ProgramStatusReporter {
            get_terminal,
            get_session_name,
            run_active: false,
            compacting: false,
            run_result: ProgramStatus {
                state: ProgramStatusState::Done,
                app: None,
                kind: None,
                message: None,
            },
            resting_status: ProgramStatus {
                state: ProgramStatusState::Idle,
                app: None,
                kind: None,
                message: None,
            },
            blocked: Vec::new(),
            last_report: None,
        }
    }

    /// `handleEvent(event)`。
    ///
    /// 只处理与状态栏相关的变体：`AgentStart`、assistant 的 `MessageEnd`、
    /// `CompactionStart` / `CompactionEnd`、`AgentSettled`，以及用来刷新会话名的
    /// `SessionInfoChanged`。其它事件直接返回，不产生上报。
    ///
    /// 每个被处理的事件结束时都会调用 `report`（内容未变时内部去重），因此调用方无需
    /// 在事件之后再次上报。`MessageEnd` 若消息不是 assistant 角色会提前返回，连
    /// `report` 也跳过（spec §2.11 明确标注“不 report”）。
    pub fn handle_event(&mut self, event: &AgentSessionEvent) {
        match event {
            AgentSessionEvent::AgentStart => {
                // 新一轮 run：丢弃上一轮的结果，避免旧 error 被本次 AgentSettled 带出。
                self.run_active = true;
                self.run_result = plain_status(ProgramStatusState::Done);
            }
            AgentSessionEvent::MessageEnd { message } => {
                // 最新响应决定结果：重试后的成功响应会覆盖此前的错误。
                if message.role() != "assistant" {
                    return;
                }
                self.run_result = match message.as_assistant() {
                    Some(assistant) if assistant.stop_reason == StopReason::Error => {
                        ProgramStatus {
                            state: ProgramStatusState::Error,
                            app: None,
                            kind: None,
                            message: Some(first_line(assistant.error_message.as_deref())),
                        }
                    }
                    // 自定义消息没有 `stopReason`，按成功处理。
                    _ => plain_status(ProgramStatusState::Done),
                };
            }
            AgentSessionEvent::CompactionStart { .. } => {
                // 压缩期间由 `current_status` 优先显示 "Compacting context"，与 run 是否
                // 活动无关；这里只置标志。
                self.compacting = true;
            }
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                error_message,
                ..
            } => {
                self.compacting = false;
                // JS 的 `if (event.errorMessage)`：空串是假值，不进入错误分支。
                let error_message = error_message.as_deref().filter(|text| !text.is_empty());
                // 压缩结果落到哪一“记忆”取决于是否处于 run 中：
                // - run 中：压缩是 run 的一部分，结果写进 `run_result`，由后续 `AgentSettled`
                //   定稿；若之后还有 assistant 响应，它也会覆盖这里的结果。
                // - 没有 run：只有手动压缩会改变 `resting_status`（成功 done / 失败 error）；
                //   自动压缩被中止则回到 idle，自动压缩失败则保持原静止状态不变。
                if self.run_active {
                    // 失败的恢复压缩会结束 run，除非后续响应成功。
                    if *aborted {
                        self.run_result = plain_status(ProgramStatusState::Idle);
                    } else if let Some(error_message) = error_message {
                        self.run_result = ProgramStatus {
                            state: ProgramStatusState::Error,
                            app: None,
                            kind: None,
                            message: Some(first_line(Some(error_message))),
                        };
                    }
                } else if *aborted {
                    self.resting_status = plain_status(ProgramStatusState::Idle);
                } else if *reason == kiana_ext_api::events::CompactionReason::Manual {
                    self.resting_status = match error_message {
                        Some(error_message) => ProgramStatus {
                            state: ProgramStatusState::Error,
                            app: None,
                            kind: None,
                            message: Some(first_line(Some(error_message))),
                        },
                        None => plain_status(ProgramStatusState::Done),
                    };
                }
            }
            AgentSessionEvent::AgentSettled { aborted } => {
                self.run_active = false;
                // 定稿：非中止时把 run 期间记录的最终结果转为静止状态；中止一律 idle，
                // 即使本轮此前已有成功响应（测试 `reports_only_the_outcome_of_the_run`）。
                self.resting_status = if *aborted {
                    plain_status(ProgramStatusState::Idle)
                } else {
                    self.run_result.clone()
                };
            }
            // 会话名是 working/done 消息的一部分，因此需要重新上报。
            AgentSessionEvent::SessionInfoChanged { .. } => {}
            _ => return,
        }
        self.report();
    }

    /// `setBlocked(source, status)`：为对话框上报 `blocked`，用 `None` 清除。
    /// 重新打开同一个 source 会替换它（并移到插入序末尾）。
    ///
    /// `source` 是调用方定义的槽位标识（仓库内使用 `"extension-dialog"` 与 `"login"`），
    /// 只用于去重，不参与上报内容。所有已登记的槽都会排队保留，但 `current_status`
    /// 只显示最后一个；因此后打开的对话框覆盖先打开的，先打开的关闭后会露出前一个。
    /// 对不存在的 source 传 `None` 是空操作（仍会走一次去重上报）。
    pub fn set_blocked(&mut self, source: &str, status: Option<BlockedStatus>) {
        // 先删同 source 的旧项，再在末尾插入：等价于“替换并置顶”。
        self.blocked.retain(|(existing, _)| existing != source);
        if let Some(status) = status {
            self.blocked.push((source.to_string(), status));
        }
        self.report();
    }

    /// `reset()`：忘记上一个会话的 run（例如切换会话后）。
    ///
    /// 只重置 run 相关字段（`run_active`、`compacting`、`run_result`、`resting_status`），
    /// 并立即上报。`blocked` 列表和 `last_report` 都保留：前者由各对话框调用方自行清除，
    /// 后者意味着重置后的状态若与上次相同不会产生新的终端写入。这两点与 TS 实现一致
    /// （`spec/ca-interactive-mode.md` §2.11 的 `reset()` 也只复位上述四个字段）。
    pub fn reset(&mut self) {
        self.run_active = false;
        self.compacting = false;
        self.run_result = plain_status(ProgramStatusState::Done);
        self.resting_status = plain_status(ProgramStatusState::Idle);
        self.report();
    }

    /// `report()`：内容未变时不上报（去重键就是 `JSON.stringify(status)`）。
    ///
    /// 流程：合成当前状态 → 注入 `app` → 序列化成 TS 键序的 JSON → 与 `last_report`
    /// 比较。相同则直接返回（连终端 getter 都不调用）；不同则先记住新键，再通过终端写出。
    /// 去重只比较内容，不比较时间；同一状态在一次会话中最多写一次，直到它发生变化。
    pub fn report(&mut self) {
        // TS `{ ...this.currentStatus(), app: APP_NAME }`：`app` 始终最后写入。
        let mut status = self.current_status();
        status.app = Some(APP_NAME.to_string());
        let key = status_json(&status);
        // 去重键是字段顺序固定的 JSON（含 `app`）；只要内容相同，键就相同。
        if self.last_report.as_deref() == Some(key.as_str()) {
            return;
        }
        self.last_report = Some(key);
        let terminal = (self.get_terminal)();
        terminal.borrow_mut().set_program_status(&status);
    }

    /// `currentStatus()`。
    ///
    /// 把三组输入合成为一条上报状态，优先级固定为：对话框阻塞 > 压缩 > run/静止状态。
    /// 返回的状态 `app` 为空，由 `report` 填入；调用期间会读取一次会话名 getter
    /// （仅当最终状态是 `working` / `done`）。
    fn current_status(&self) -> ProgramStatus {
        // 1) 对话框优先：只显示最后打开且仍未关闭的那个。
        if let Some((_, blocked)) = self.blocked.last() {
            return ProgramStatus {
                state: ProgramStatusState::Blocked,
                app: None,
                kind: Some(blocked.kind),
                message: Some(blocked.message.clone()),
            };
        }
        // 2) 压缩其次：此时不显示会话名，避免“Working / Compacting context”两种文案互相覆盖。
        if self.compacting {
            return ProgramStatus {
                state: ProgramStatusState::Working,
                app: None,
                kind: None,
                message: Some("Compacting context".to_string()),
            };
        }
        // 3) run 中固定为 working；没有 run 时使用上次 AgentSettled 定稿的静止状态。
        let status = if self.run_active {
            plain_status(ProgramStatusState::Working)
        } else {
            self.resting_status.clone()
        };
        // working / done 的 message 是当前会话名；error 等其它状态保留自己已有的 message。
        if matches!(
            status.state,
            ProgramStatusState::Working | ProgramStatusState::Done
        ) {
            return ProgramStatus {
                state: status.state,
                app: None,
                kind: None,
                message: (self.get_session_name)(),
            };
        }
        status
    }
}

fn plain_status(state: ProgramStatusState) -> ProgramStatus {
    ProgramStatus {
        state,
        app: None,
        kind: None,
        message: None,
    }
}

/// `JSON.stringify({ ...status, app })` 的键序。
///
/// 不能直接序列化 [`ProgramStatus`]：Rust 结构体的字段顺序是
/// `state, app, kind, message`，而 TS 的展开顺序是 `state, kind?, message?, app`
/// （`blocked` 来自 `{ state: "blocked", ...blocked }`）。
fn status_json(status: &ProgramStatus) -> String {
    let mut map = Map::new();
    map.insert(
        "state".to_string(),
        Value::String(state_name(status.state).to_string()),
    );
    // kind 只在 blocked 时进入键序；其它状态即使带 kind 也不写出（与 `ProgramStatus`
    // 的编码不变量一致）。
    if status.state == ProgramStatusState::Blocked
        && let Some(kind) = status.kind
    {
        map.insert(
            "kind".to_string(),
            Value::String(kiana_tui::program_status::kind_name(kind).to_string()),
        );
    }
    if let Some(message) = &status.message {
        map.insert("message".to_string(), Value::String(message.clone()));
    }
    map.insert("app".to_string(), Value::String(APP_NAME.to_string()));
    Value::Object(map).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        state: ProgramStatusState,
        kind: Option<ProgramStatusKind>,
        message: Option<&str>,
    ) -> ProgramStatus {
        ProgramStatus {
            state,
            app: Some("kiana".to_string()),
            kind,
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn status_json_uses_ts_key_order() {
        // `{ ...currentStatus(), app }`：state, kind?, message?, app。
        assert_eq!(
            status_json(&status(
                ProgramStatusState::Blocked,
                Some(ProgramStatusKind::Auth),
                Some("Log in")
            )),
            r#"{"state":"blocked","kind":"auth","message":"Log in","app":"kiana"}"#
        );
        assert_eq!(
            status_json(&status(ProgramStatusState::Working, None, None)),
            r#"{"state":"working","app":"kiana"}"#
        );
        assert_eq!(
            status_json(&status(ProgramStatusState::Done, None, Some("Fix login"))),
            r#"{"state":"done","message":"Fix login","app":"kiana"}"#
        );
        assert_eq!(
            status_json(&status(ProgramStatusState::Error, None, Some("No model"))),
            r#"{"state":"error","message":"No model","app":"kiana"}"#
        );
        assert_eq!(
            status_json(&status(ProgramStatusState::Idle, None, None)),
            r#"{"state":"idle","app":"kiana"}"#
        );

        // 不能直接用 `ProgramStatus` 的 serde 顺序（state, app, kind, message）。
        assert_eq!(
            serde_json::to_string(&status(
                ProgramStatusState::Blocked,
                Some(ProgramStatusKind::Auth),
                Some("Log in")
            ))
            .unwrap(),
            r#"{"state":"blocked","app":"kiana","kind":"auth","message":"Log in"}"#
        );
    }

    #[test]
    fn first_line_splits_on_r_optional_n_and_trims() {
        assert_eq!(first_line(None), "Error");
        assert_eq!(first_line(Some("")), "Error");
        assert_eq!(first_line(Some("   ")), "Error");
        assert_eq!(first_line(Some("\nsecond")), "Error");
        assert_eq!(first_line(Some("first\nsecond")), "first");
        assert_eq!(first_line(Some("first\r\nsecond")), "first");
        assert_eq!(first_line(Some("  padded  \nnext")), "padded");
        // 孤立的 `\r` 不是分隔符。
        assert_eq!(first_line(Some("a\rb\nc")), "a\rb");
    }
}
