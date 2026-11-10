//! 对应 modes/interactive/theme：Theme 类、system 主题、主题 JSON 校验。
//!
//! TS 源：packages/coding-agent/src/modes/interactive/theme/*.ts（5 个文件 / 2,261 行）。
//!
//! 注意：`theme.ts` 的 `highlightCode` 依赖 `utils/syntax-highlight.ts` 与 `utils/fs-watch.ts`，
//! 而这两个文件的实现在 **`kiana-core`**（`kiana_core::syntax_highlight` / `kiana_core::fs_watch`，
//! 见 ARCHITECTURE.md §5 的 `kiana-core` 模块树）。`kiana-core` 是 L3、`kiana-theme` 是 L6，依赖方向
//! 只能是 `kiana-theme -> kiana-core`，因此本 crate **不得**复制这两份实现，只调用
//! `kiana_core::syntax_highlight::{highlight, supports_language}` 与
//! `kiana_core::fs_watch::{close_watcher, watch_with_error_handler}`。
//!
//! 本文件只声明模块树；实现阶段按模块注释里的 TS 路径逐文件移植。
//!
//! # 这个 crate 解决什么问题
//!
//! 交互式 TUI 不写死颜色：组件按 token 名（`accent`、`mdCodeBlock`、`borderMuted` 等）
//! 向当前主题要 ANSI 字符串。本 crate 提供这条链路的全部环节：token 表与 slot 约束、
//! 主题 JSON 的解析与校验、内置 `dark`/`light` 资源、按终端上报颜色生成 `system` 主题、
//! 主题的加载与进程级全局实例，以及把「设置项 + 终端颜色/明暗变化」接到重绘上的
//! [`InteractiveThemeController`]。
//!
//! 没有它，每个渲染组件都要自行解析颜色字符串、处理变量引用与 token fallback，`system`
//! 主题也没有统一入口；而校验错误是主题作者直接看到的用户文案，当前措辞是稳定契约，改动需同步测试。
//!
//! # 在系统中的位置
//!
//! 上游（源码直接使用）：`kiana-tui`（`Color`、`TerminalColors`、
//! `TerminalColorScheme` 与 `MarkdownTheme`/`EditorTheme`/`SelectListTheme`/
//! `SettingsListTheme` 等渲染主题结构）、`kiana-core`（配置目录、语法高亮、文件监听）、
//! `kiana-json`（`JSON.parse` 的逐字等价物）。
//!
//! 下游：`kiana-coding-agent` 是进程入口，`app.rs` 安装主题 JSON 校验器并调用
//! [`init_theme`]，`cli/startup_ui.rs` 负责从磁盘加载并登记主题；`kiana-ui-kit` 的
//! 组件通过 [`THEME`] 取色，`kiana-tools` 的渲染器接收 [`Theme`] 参数；`kiana-extensions`
//! 把 [`Theme`] 桥接成 JS 对象；`kiana-export` 用主题颜色做 HTML 导出；`kiana-resources` 用
//! [`load_theme_from_path`] 加载资源目录里的主题；`kiana-session` 用
//! [`get_theme_by_name`] 校验设置里的主题名；`kiana-interactive` 驱动
//! [`InteractiveThemeController`]。以上依赖关系见各 crate 的 `Cargo.toml` 与调用点。
//!
//! # 核心概念与类型
//!
//! - [`Theme`]：一个已加载的主题。构造时把全部 token 解析成具体颜色并预计算 ANSI
//!   序列，渲染热路径只做查表与拼接。`fg`/`bg` 在主题缺少某个 token 时退化为终端默认
//!   色（只在绕过校验器构造出不完整主题时出现）；需要 TS 的
//!   `Unknown theme color: {token}` 报错时用 `try_fg`/`try_bg`。
//! - [`ThemeToken`] / [`ThemeColor`] / [`ThemeBg`]：56 个 token 与它们的 slot（前景/
//!   背景）。颜色槽不能混用；声明顺序是冻结的，是 [`ThemeToken::ALL`]/[`THEME_TOKENS`]
//!   的下标基准，也是对外发布 JSON Schema 的属性顺序。它**不**决定主题文件键序（内置
//!   文件只是大体一致，校验也不要求），也不保证 [`Theme::colors`] 与导出 CSS 的顺序——
//!   后两者沿用主题文档 `colors` 的相对顺序，且构造时全部前景 token 排在背景 token 之前。
//! - [`ThemeJson`]：主题文档的视图；[`validate_theme_json`] 是按 typebox 1.3.27 语义
//!   手写的校验器，错误文案是当前稳定契约。校验器通过 [`set_theme_json_validator`] 安装，
//!   是可选的。
//! - 全局单例：[`current_theme`]/[`init_theme`]/[`set_theme`]/[`set_theme_instance`]
//!   维护进程级的当前主题；[`THEME`]（类型 [`ThemeProxy`]）与 [`theme()`] 是 TS `theme`
//!   Proxy 的对应物，本身不读取全局状态；通过它们的**方法**访问主题时，若尚未
//!   `init_theme`/`set_theme`，返回 `Err`，消息为
//!   `Theme not initialized. Call initTheme() first.`。
//! - `system` 主题：[`generate_system_theme_colors`] 把终端上报的前景/背景与 16 色调色板
//!   映射成全部 token（见 `system_theme` 模块）。
//! - [`InteractiveThemeController`] + [`ThemeHost`]：把设置里的主题与终端实际颜色绑定，
//!   `system` 主题在终端回色前渲染为灰阶。
//!
//! # 一次典型调用
//!
//! 1. 入口（`kiana-coding-agent`）：`set_theme_json_validator(Box::new(validate_theme_json))`
//!    安装生产校验器；交互式启动时先 `set_registered_themes` 登记磁盘主题、
//!    `mark_terminal_colors_pending()` 标记终端颜色未知，再用 `resolve_theme_setting`
//!    由设置与终端明暗得到主题名，调用 [`init_theme`]。
//! 2. 加载：`system` 按当前终端颜色直接生成；其他名字先查已登记主题，未命中时由
//!    `load_theme_json` 依次尝试内置 `dark`/`light`、已登记主题的源文件与
//!    `kiana_core::config::get_custom_themes_dir()` 下的 `<name>.json`。成功得到的
//!    [`Theme`] 被写入全局单例。
//! 3. 渲染：组件经 [`THEME`] 或 [`current_theme`] 取主题，调用 `fg`/`bg`/`style`（以及
//!    `get_markdown_theme` 等工厂）得到 ANSI 字符串。
//! 4. 交互式会话：`InteractiveThemeController::apply_from_settings()` 同步发起终端颜色
//!    查询并应用已就绪的结果；终端迟到的颜色回复与明暗变化会更新全局主题，明暗变化
//!    触发的重新查询在下一次调用 `wait_for_terminal_colors()` 时发起。
//!
//! # 边界与失败模式
//!
//! - 全局状态是进程级的：当前主题、当前名、已登记主题、JSON 校验器、终端颜色与明暗
//!   都在 static 中。共享同一进程的多个测试必须串行访问（`tests/common/mod.rs` 用一把
//!   全局锁并调用 `reset_theme_state()`）。
//! - 加载失败不是 `Result`：[`init_theme`] 静默回退到 `system`；[`set_theme`] 回退后返回
//!   [`ThemeResult`]（`success=false` 与错误文案），由调用方决定是否提示。未知名字的
//!   普通查询用 [`get_theme_by_name`]，它把任何错误折叠成 `None`。
//! - [`ThemeError`] 的 `Display` 是稳定的错误文案（不含包装前缀），改动需同步测试；
//!   可直接拼进面向用户的文案；错误只有消息，没有结构化的类别或错误码字段。
//! - 校验器可选：未安装 [`ThemeJsonValidator`] 时，文档按 TS 的宽松语义处理（变量解析
//!   推迟到创建主题/导出，损坏但未被引用的变量不影响加载）。内置 `dark`/`light` 本身
//!   也不经过校验器。
//! - 热重载只在 `init_theme(..., true)` / `set_theme(..., true)` 时启动，且仅当当前主题
//!   是 `kiana_core::config::get_custom_themes_dir()` 下存在同名 `.json` 的自定义主题
//!   （内置 `dark`/`light` 与 `system` 会被跳过）。去抖 100 ms，过期定时器靠
//!   generation 与当前主题名复查丢弃；[`stop_theme_watcher`] 停止。文件监听本身由
//!   `kiana_core::fs_watch` 提供，
//!   本 crate 不重复实现。
//! - `highlight_code` 不返回错误：语言为空或 `supports_language` 不识别时只给每行上
//!   `mdCodeBlock` 色（全局主题未初始化时原样返回该行），未知语言在
//!   `kiana_core::syntax_highlight` 内回退 plaintext。
//!
//! # 阅读顺序
//!
//! 1. `theme_tokens`：56 个 token、slot 与 fallback（主题文件的词汇表）；
//! 2. `theme_schema`：文档结构、校验器与错误文本，以及颜色变量的解析时机；
//! 3. `assets` 与 `system_theme`：内置主题、由终端颜色生成 `system` 主题的数值规则；
//! 4. `theme`：Theme 类、加载、全局单例、TUI 主题工厂与 `highlightCode`；
//! 5. `theme_controller`：交互式会话中的主题设置与终端颜色绑定；
//! 6. `index`：再导出面；crate 根的 `pub use crate::index::*` 让下游可以写
//!    `kiana_theme::Theme`，模块路径也保留。
pub mod assets;
pub mod index;
pub mod system_theme;
pub mod theme;
pub mod theme_controller;
pub mod theme_schema;
pub mod theme_tokens;

// --- crate 根导出面（对应 TS 的 `theme.ts` / `theme-controller.ts` 导出） ---
//
// 与 `kiana-tui` 的 `lib.rs` 同一约定：crate 根再导出 [`index`]，下游可以写
// `kiana_theme::Theme` 而不是 `kiana_theme::theme::Theme`。模块路径仍然可用。
pub use crate::index::*;
