//! 无业务依赖的共用渲染小件：供 kiana-tools 渲染器与 kiana-interactive 复用。
//!
//! TS 源：packages/coding-agent/src/modes/interactive/components/{keybinding-hints,visual-truncate,diff,dynamic-border,themed-text,countdown-timer,kiana-logo,bordered-loader,auth-url}.ts
//!
//! 各模块与 TS 文件的对应关系见模块文档。与 TS 的跨语言差异（TUI 回调代替 `&mut Tui`、
//! `Arc<Theme>` 快照代替类实例、`Timer` 注入代替 `setInterval`）都在对应模块的文档里说明。
//!
//! 快速入口：`use kiana_ui_kit::*;`（crate 根再导出 [`index`] 的全部公开项，与 `kiana-tui` /
//! `kiana-theme` 的约定一致）。
//!
//! # 这个 crate 解决什么问题
//! 它收集 9 个没有业务含义、却要在多处复用的终端渲染小件：边框、加载框、键位提示、
//! 行级 diff 着色、按可视行数折叠、主题化文本、倒计时、登录 URL、logo。若没有独立 crate，
//! `kiana-tools` 的 renderer 与 `kiana-interactive` 会各自实现一遍，同一段 TS 语义
//! （换行、着色、截断、键位文案）迟早出现两套行为。
//!
//! # 在系统中的位置
//! 依赖边只向下：本 crate 依赖 `kiana-tui`（`Component` trait、终端能力）与 `kiana-theme`
//! （颜色 token），另用 `kiana-core` 的剪贴板实现；向上被 `kiana-tools`、
//! `kiana-builtin-extensions`、`kiana-interactive` 直接依赖（见各 crate 的 `Cargo.toml`）。
//! 按 `ARCHITECTURE.md` 的分层表，本 crate 在 L7、`kiana-tools` 在 L8、`kiana-interactive`
//! 在 L12，因此给本 crate 增加对任何上层 crate 的依赖都会违反该分层。
//!
//! # 核心概念与类型
//! 可视组件实现 `kiana_tui::tui::Component`，由宿主在重绘时按视口宽度调用
//! `render(width) -> Vec<String>`（每行可含 ANSI）。本 crate 的公开组件只有五个：
//! `DynamicBorder`、`ThemedText`、`VisualLinePreview`、`AuthUrlComponent`、`BorderedLoader`。
//! 其余模块提供函数（`render_diff`、`truncate_to_visual_lines`、`key_hint`、
//! `kiana_logo_lines` 等）或非组件句柄：`CountdownTimer` 不实现 `Component`，只持有定时器
//! 与回调，由宿主组件持有并驱动。
//!
//! `index` 是再导出 barrel，crate 根再用 `pub use crate::index::*;` 把同一批名字提到根部；
//! 因此 `kiana_ui_kit::DynamicBorder` 与 `kiana_ui_kit::dynamic_border::DynamicBorder`
//! 两种路径都可用，集成测试 `tests/index.rs` 同时锁定这两种写法。
//!
//! # 一次典型调用
//! 本 crate 不驱动渲染循环，只提供可挂载的组件与可拼装的字符串：
//! 1. 工具渲染器侧：`kiana-tools` 的 renderer 在生成结果时调用 `render_diff`
//!    （`renderers/edit.rs`），或把 `key_hint("app.tools.expand", "to expand")` 拼进折叠提示
//!    （`renderers/find.rs`）。
//! 2. 交互界面侧：`kiana-interactive` 的对话框把 `DynamicBorder::default()` 存为字段
//!    （如 `components/model_selector.rs`），在会话/重载等界面把 `ThemedText` 加为子项
//!    （`interactive_mode/session.rs`、`interactive_mode/reload.rs`）；`VisualLinePreview`
//!    则由 `kiana-tools`（`renderers/bash.rs`）与 `kiana-builtin-extensions`
//!    （`mcp/tools.rs`、`codemode/renderer.rs`）用作折叠子项，宿主重绘时
//!    调用 `render(width)` 逐层转发。
//! 3. 主题切换或需要全量重绘时，宿主调用 `Component::invalidate()`：`ThemedText` 与
//!    `VisualLinePreview` 借此丢弃缓存，下次 `render` 按新主题重建；`DynamicBorder` 的默认
//!    颜色闭包每次 `render` 都现读全局主题，无需失效。
//!
//! # 边界与失败模式
//! * 主题是全局状态。使用全局主题着色的路径（`DynamicBorder::default()` 的渲染、
//!   `key_hint`/`raw_key_hint`、`render_diff`、`kiana_logo_lines`/`kiana_wordmark` 等）在主题
//!   未初始化时会 panic，各处统一是 `expect("theme must be initialized before rendering ...")`，
//!   复现 TS 在主题未初始化时抛异常的语义。`BorderedLoader` 另行接收 `Arc<Theme>`，
//!   全局主题与快照两种语义见其模块文档。
//! * 键位文案不是常量：`key_text`/`key_display_text`/`key_hint` 在调用时从 `kiana_tui` 的
//!   全局键位表解析，同一段代码在不同宿主或测试线程里可能得到不同文本（测试需先用
//!   `set_keybindings` 注入键位表）。
//! * 本 crate 自身不读写文件、不做网络请求、不启动进程或异步任务（`std::fs` 只出现在
//!   `diff.rs` 的测试里）；唯一改变外部状态的是 `AuthUrlComponent::copy` 的剪贴板调用，
//!   且它把失败转成提示行，函数本身不返回 `Result`。
//! * `render_diff` 与 `truncate_to_visual_lines` 同步执行且不返回 `Result`。前者在主题
//!   未初始化时 panic；内部移植的空白整理函数在前后缀不变量被破坏时也会
//!   `panic!(... "this is a bug")`；含 `\r`（CRLF 输入）的行按 context 原样输出。
//!   后者的边界（空文本、`max_visual_lines == 0`）按 TS 语义处理，逐条见对应模块文档。
//!
//! # 阅读顺序
//! `index.rs`（导出面）→ `dynamic_border.rs`（最小的一类组件：无缓存、无状态，先建立
//! `Component` 直觉）→ `themed_text.rs` / `visual_truncate.rs`（`invalidate` 与缓存的配合）
//! → `keybinding_hints.rs`（全局键位表与 UTF-16 大小写细节）→ `bordered_loader.rs` /
//! `countdown_timer.rs`（宿主注入定时器与取消）→ `diff.rs`（最大的移植：私有 `diffWords`
//! 与逐字正则）→ `auth_url.rs` / `kiana_logo.rs`（余下的平台相关小件）。
pub mod auth_url;
pub mod bordered_loader;
pub mod countdown_timer;
pub mod diff;
pub mod dynamic_border;
pub mod index;
pub mod keybinding_hints;
pub mod kiana_logo;
pub mod themed_text;
pub mod visual_truncate;

// --- crate 根导出面（对应 TS barrel `components/index.ts` 中属于本 crate 的部分） ---
pub use crate::index::*;
