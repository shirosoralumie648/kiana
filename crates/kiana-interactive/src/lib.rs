//! 对应 ca-interactive-mode + ca-interactive-components。
//! TS 源：packages/coding-agent/src/modes/interactive/**
//!
//! 本 crate 没有单一的 TS 对应文件（TS 的 `modes/interactive/` 目录里没有 `index.ts`）。
//! crate 根的公开面按 `ARCHITECTURE.md` §5.1/§5.5 与两个 TS barrel 组装：
//!
//! - [`components::index`]（= `components/index.ts`，37 行 barrel）的全部再导出；
//! - [`index`]（= `modes/index.ts` 的交互侧：`InteractiveMode` / `InteractiveModeOptions`）；
//! - [`interactive_mode`] 的入口函数 `format_crash_extension_hint` / `format_resume_command`
//!   （`interactive-mode.ts` 的公开符号，不是 `modes/index.ts` 的成员）；
//! - [`tui_renderer`] 的组合根 `create_interactive_tui` / `create_interactive_tui_reference`
//!   及其选项/句柄类型（由 `interactive-mode.ts:206` 再导出，见 §5.1/§5.5）。
//!
//! 各子模块仍可用完整路径访问（例如
//! `kiana_interactive::components::footer::FooterComponent`）。crate 根与 TS 一样只做再导出，
//! 不新增实现；本 crate 内不得出现第二份实现。
//!
//! # 这个 crate 解决什么问题
//!
//! 交互模式是 CLI 的默认前台形态：终端里逐条渲染用户/助手消息、工具执行、选择器等
//! 48 个组件，并驱动编辑器输入、会话持久化与模型调用。TS 把这些代码放在
//! `packages/coding-agent/src/modes/interactive/`（约 19,342 行，含 245.7 KB 的
//! `interactive-mode.ts`）；Rust 侧按 `ARCHITECTURE.md` §3.7 拆成独立 crate，避免
//! 多名实现者并发改同一个超大 crate 的其余部分。
//!
//! # 在系统中的位置
//!
//! 上游：唯一调用方是 `kiana-coding-agent`（见该 crate 的 `Cargo.toml`）——
//! `src/app.rs` 在 `AppMode::Interactive` 分支构造并运行 [`InteractiveMode`]；
//! `src/cli/` 的启动界面、会话选择器与配置选择器直接使用本 crate 的具体组件；
//! `src/sdk_exports.rs` 把本 crate 的公开面再次转出成 SDK 入口。
//!
//! 下游（本 crate 依赖的层）：`kiana-tui` / `kiana-ui-kit`（组件框架与共用小件）、
//! `kiana-session` / `kiana-session-core`（会话运行时）、`kiana-ai` / `kiana-agent-core`
//! （模型调用）、`kiana-config` / `kiana-theme` / `kiana-core` 等支撑层。完整清单见
//! `ARCHITECTURE.md` §2：本 crate 位于 L12，只被 L13 的 `kiana-coding-agent` 依赖。
//!
//! # 一次典型调用
//!
//! ```text
//! kiana-coding-agent::app（AppMode::Interactive）
//!   1. InteractiveMode::new(runtime, InteractiveModeOptions { .. })
//!      —— options 携带启动告警、初始消息/图片、tui_mode 等启动上下文；
//!   2. interactive_mode.run()
//!      —— 先 init()（构造渲染器、挂载组件），再进入
//!         get_user_input() -> session.prompt() 的循环（无退出分支）；
//!   3. 退出不由 run() 返回：关停/崩溃路径经 lifecycle 的 shutdown() 调
//!      std::process::exit 直接结束进程。
//! ```
//!
//! `create_interactive_tui` / `create_interactive_tui_reference`（`tui_renderer` 模块）
//! 是 `run()` 之外的第二个入口：按 `tui_mode` 组装主屏/全屏渲染器并返回可共享句柄，
//! 交互模式内部在 init/切换渲染器时使用。
//!
//! # 边界与失败模式
//!
//! - 进程退出模型：本 crate 的关停路径直接调用 `std::process::exit`（见
//!   `interactive_mode/lifecycle.rs` 的模块文档），不展开栈、不运行 `Drop`；因此
//!   `InteractiveMode` 不能指望析构函数做清理，终端恢复由显式路径完成。
//! - 渲染器代理：`InteractiveTuiReference::children()` 是文档化的不可用路径（会
//!   panic），需要取出子项时改用 `take_children`，原因见 `tui_renderer.rs` 模块文档。
//! - 离线环境：`KIANA_OFFLINE` 下模型目录刷新与包更新检查被跳过
//!   （见 `run()` 的文档注释）。
//!
//! # 阅读顺序
//!
//! 1. 本文件（公开面 == TS 两个 barrel + `interactive-mode.ts` 的公开符号）；
//! 2. `index.rs` / `components/index.rs`（两份 barrel 的逐名对照表）；
//! 3. `interactive_mode.rs`（核心状态与 `run()` 循环，子模块按功能拆分）；
//! 4. `tui_renderer.rs`（渲染器组合根与 TS Proxy 的表示差异）；
//! 5. `components/`（按需查阅单个组件；多数文件头标注了对应的 TS 源文件）。

// --- 子模块 ---
pub mod assets;
pub mod bug_report;
pub mod chat_viewport;
pub mod components;
pub mod external_editor;
pub mod index;
pub mod interactive_mode;
pub mod model_catalog_refresh;
pub mod model_search;
pub mod program_status_reporter;
pub mod render_core;
pub mod session_share;
pub mod tui_renderer;

// --- 公开面 ---
// `components/index.ts` 的全量再导出（含 kiana-ui-kit 的 9 个共用小件）。
pub use components::index::*;
// `modes/index.ts` 的交互侧。glob 里的两个名字与下面的显式再导出是同一项（显式优先），
// glob 本身因此不引入任何新名字；保留它是为了与 TS 的再导出面逐名对应，并让本模块
// 将来新增的名字自动出现在 crate 根。
#[allow(unused_imports)]
pub use index::*;
pub use interactive_mode::{
    InteractiveMode, InteractiveModeOptions, format_crash_extension_hint, format_resume_command,
};
pub use tui_renderer::{
    InteractiveRenderer, InteractiveTuiOptions, InteractiveTuiReference, create_interactive_tui,
    create_interactive_tui_reference,
};
