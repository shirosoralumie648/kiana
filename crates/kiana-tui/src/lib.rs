//! 对应 packages/tui：tui-core + tui-text + tui-components + terminal 合为一个 crate。
//! TS 源：packages/tui/src/**
//!
//! Kiana 的终端 UI 基础层：组件抽象、布局、文本宽度/ANSI 处理、按键解码、颜色与终端能力
//! 探测，以及主屏/备用屏两个渲染器。它不认识“对话”“工具调用”等业务概念；任何由
//! [`tui::Component`] 组成的树都可以交给它画到终端并接收输入。
//!
//! # 这个 crate 解决什么问题
//!
//! TypeScript 参照实现的 `packages/tui` 是一个包，内部再分 tui-core / tui-text /
//! tui-components / tui-terminal 等单元（另有 tui-latex、tui-autocomplete）。
//! `ARCHITECTURE.md` §1 的映射规则是“一个 TS 包对应
//! 一个 crate”（唯一例外是 `packages/coding-agent`），所以 Rust 侧只有一个 `kiana-tui`，
//! 下游只要一条依赖边即可拿到整套终端 UI。模块与 TS 文件的逐行对应见 `ARCHITECTURE.md`
//! §5 的 `kiana-tui` 表；单元规范是 `spec/tui-core.md`、`spec/tui-text.md`、
//! `spec/tui-components.md`。
//!
//! 这些单元在 Rust 侧同属一个 crate、没有各自的编译边界：`text.rs` 与
//! `components/mod.rs` 都在模块文档里声明“本 crate 内不得出现第二份实现”。加功能前先确认
//! 现有实现位于哪个模块，避免同一算法写出两份、行为逐渐分叉。
//!
//! # 在系统中的位置
//!
//! - 调用方：`kiana-interactive` 的 `tui_renderer` 负责构造并驱动两个渲染器；
//!   `kiana-theme`（主题 token）、`kiana-tools`（diff 着色）、`kiana-ui-kit`、
//!   `kiana-ext-api` / `kiana-extensions`（扩展 UI）使用这里的颜色与组件类型。
//!   `ARCHITECTURE.md` §2.2 的依赖边里，有 14 个 crate 直接依赖 `kiana-tui`。
//! - 依赖（本 crate 依赖的 crate）：仓库内只有 `kiana-json`（JS 数字/键序/字符串语义）；
//!   其余依赖是外部 crate。
//!   本 crate 在仓库依赖图里是叶子：仓库内除 `kiana-json` 外没有其它 kiana 依赖。
//! - 对应 TS：`packages/tui/src/**`。对外符号总表在 [`index`]（对应 `index.ts`）。除重命名
//!   导出外（例如 `Box` 是 `components::box_component::BoxComponent` 的别名，只能写
//!   `kiana_tui::Box` 或完整模块路径 `kiana_tui::components::box_component::BoxComponent`），
//!   在那里再导出的名字既可写 `kiana_tui::X`，也可写 `kiana_tui::模块::X`。
//!
//! # 核心概念与类型
//!
//! - [`tui::Component`]：对象安全的渲染单元。`render(width)` 返回若干行（可含 ANSI），
//!   键盘/鼠标输入分别走 `handle_input` / `handle_mouse`。它**不要求 `Send`**：整个 TUI
//!   在单线程上运行（`tui.rs` 模块文档“三处有意偏差”第 3 条、`ARCHITECTURE.md` §4.2）。
//!   需要从别的线程送输入时，用通道把数据送回 UI 线程。
//! - 没有自己的事件循环：输入到达、定时器到期与渲染节拍都由宿主循环驱动
//!   （`Terminal::poll_*`、`Tui::render_now`）；`ProcessTerminal` 会为 stdin 启动一个后台
//!   读取线程（见 `terminal.rs` 的“线程模型”）。
//! - [`tui::Tui`]：在 `Component` 之上增加 `start` / `stop` / `render_now`、顶层子组件、
//!   焦点与 overlay。两个实现：[`main_screen::TuiMainScreen`]（写主屏与滚动缓冲区，按行
//!   差分）和 [`alt_screen::TuiAltScreen`]（备用屏全屏，经 [`layout`] 做布局）。
//! - [`tui::TuiBase`]：两个渲染器共同**持有**的状态（overlay 栈、焦点状态机、输入队列、
//!   渲染调度）。它不在 [`index`] 的再导出列表里，路径是 `kiana_tui::tui::TuiBase`。
//! - [`terminal_colors::Terminal`]：终端抽象（写字节、尺寸、光标、标题/进度、轮询输入与
//!   定时器）。宿主或测试实现它就能脱离真实 TTY；真实实现是
//!   [`terminal::ProcessTerminal`]。
//! - [`terminal_colors::Color`] / [`terminal_colors::TextStyle`] /
//!   [`terminal_colors::TerminalColors`]：跨 crate 的颜色与样式契约；它们派生 serde，
//!   字段名就是 JSON 形状，改字段会影响下游。
//! - [`text`]：终端显示宽度、ANSI/OSC 扫描、换行、截断与按列切片的唯一实现。
//!
//! # 一次典型调用
//!
//! 下面的链路取自 `kiana-interactive/src/tui_renderer.rs` 的 `create_interactive_tui`
//! 与 `pump_events`：
//!
//! 1. 准备终端：宿主可以注入 `Box<dyn Terminal>`；未注入时用 `ProcessTerminal::new()`
//!    （写真实 stdout，`start()` 时启动独立的 stdin 读取线程）。
//! 2. 选渲染器：`TuiMode::Regular` 用 `TuiMainScreen::with_options(...)`；
//!    `TuiMode::Fullscreen` 用 `TuiAltScreen::new(..., TuiAltScreenOptions { ... })`。
//! 3. `Tui::start()`：交给 `Terminal::start()` 进入 raw mode、开启括号粘贴并发起键盘协议
//!    协商（`ProcessTerminal` 的行为），随后隐藏光标、查询单元格尺寸并请求首帧。
//! 4. 宿主循环每轮：`Terminal::poll_input` / `poll_timers` / `poll_resize` → 渲染器的
//!    `pump_pending_input`（把排队输入分发给组件）→ 渲染器判定该出一帧时：
//!    Regular 走 `TuiMainScreen::flush_pending_render`，其内部消费渲染决策后直接调用
//!    `do_render()`，不经过 `render_now`；
//!    Fullscreen 走 `TuiAltScreen::take_render_decision_now()`，返回 `true` 后再调用
//!    `Tui::render_now(false)`。
//! 5. 退出：`Tui::stop(options)` 取消渲染定时器、显示光标、`Terminal::stop()` 恢复终端
//!    模式。
//!
//! # 边界与失败模式
//!
//! - **没有统一的错误类型**，多数公开 API 在签名上不可失败：`Component::render` 返回
//!   `Vec<String>`，`Terminal::write` 与 `Tui::render_now` 返回 `()`。渲染期的问题无法以
//!   `Result` 的形式交给宿主。
//! - 与 TS 同步 `throw` 等价的位置是 `panic!`：主屏渲染遇到超宽行、以及渲染期的文件系统
//!   写入失败，都会 panic（见 `main_screen.rs` 模块文档的“已知偏差”）。
//! - [`terminal::ProcessTerminal`] 忽略所有输出错误（`write_raw` 丢弃 `io::Result`），
//!   终端或管道断开不会被上报，相关方法也不返回 `Result`（见该类型的“约束”）。
//! - 少数可恢复路径有专属错误类型：`components::scroll_view::ScrollViewError`（构造时
//!   `axis` 非法，或对单子项容器的 `add_child` / `remove_child` / `clear` 被拒；`Display`
//!   文案固定（当前稳定契约，改动需同步测试））与 `native::ClipboardError`（剪贴板传输失败）。
//! - **平台层按 feature 编译**：默认 `default = []`；Linux / macOS / Windows 后端分别由
//!   `x11` / `darwin` / `win32` 打开（`native` 本身是空 feature）。未开启时
//!   `native::get_native_clipboard()` 返回 `None`，调用方应按“没有剪贴板后端”处理。
//!
//! # 阅读顺序
//!
//! 1. [`tui`]：`Component` / `Focusable` / `Tui` 三个 trait 与 `TuiBase`，先弄清组件如何
//!    被挂载、输入如何到达组件、一帧由谁调度。
//! 2. [`terminal_colors`] 与 [`terminal`]：`Terminal` trait 和真实终端实现，理解输入/输出
//!    边界。
//! 3. [`text`]：显示宽度与 ANSI 语义；后续所有排版都建立在它上面。
//! 4. [`layout_node`] 与 [`layout`]：备用屏的空间分配与命中测试。
//! 5. [`components`]：基础组件（输入框、编辑器、栈、列表、滚动视图、Markdown 等）。
//! 6. [`main_screen`] 与 [`alt_screen`]：两种渲染器各自完整的一帧流程（差分输出、overlay
//!    合成、鼠标路由、选择与搜索）。
//! 7. 按需查阅：`keys` / `stdin` / `keybindings`（输入解码）、`colors` / `oklab` /
//!    `terminal_image` / `native`（颜色、图片、平台能力）、`markdown` / `latex` /
//!    `autocomplete` / `fuzzy`（组件依赖的子系统）。
//! 8. [`index`]：对外符号总表。新增公开符号时，除了在所属模块写 `pub`，还必须在这里补一条
//!    `pub use`，否则 crate 根看不到它。
//!
//! `src/` 下还有两个**私有**子模块不在下面的 `pub mod` 列表里：`text.rs` 用
//! `#[path = "text_tables.rs"] mod text_tables;` 与 `#[path = "word_dict.rs"] mod word_dict;`
//! 声明它们；字符宽度表与 CJK/泰/老/高棉/缅词典引擎只由 [`text`] 使用。
//!
//! # 模块索引
//!
//! 顺序与下面的 `pub mod` 声明一致（字母序，不是 TS 的分组顺序）；每个模块的详细文档写在
//! 它自己文件顶部的 `//!` 里，本表只做一句话定位。
//!
//! | 模块 | 职责（对应 TS） |
//! | --- | --- |
//! | [`alt_screen`] | 备用屏渲染器：布局、overlay、选择、搜索、图片（`tui-alt-screen.ts`） |
//! | [`alt_screen_search`] | 备用屏全文搜索：索引、匹配、搜索框（`alt-screen-search.ts`） |
//! | [`autocomplete`] | 补全 provider：`@` 文件、斜杠命令、路径（`autocomplete.ts`） |
//! | [`colors`] | 颜色解析、混色与 ANSI 序列拼接（`colors.ts`） |
//! | [`components`] | 基础组件：box/input/editor/栈/列表/滚动/图片/Markdown（`components/**`） |
//! | [`editor_component`] | 可替换编辑器的契约 `EditorComponent`（`editor-component.ts`） |
//! | [`flash`] | 备用屏顶部的瞬时消息栈（`components/alt-screen-flash.ts`） |
//! | [`fuzzy`] | 模糊匹配与过滤（`fuzzy.ts`） |
//! | [`image`] | TS `image.ts` 的再导出面（剪贴板；图片组件在 `components::image`） |
//! | [`index`] | 对外符号总表（`index.ts`） |
//! | [`keybindings`] | 全局键位注册表（`keybindings.ts`） |
//! | [`keys`] | 按键序列解析：legacy 与 Kitty 协议（`keys.ts`、`native-modifiers.ts`） |
//! | [`latex`] | LaTeX 渲染（`latex.ts`） |
//! | [`layout`] | 备用屏布局引擎：测量、分配、绘制、命中测试（`layout.ts`） |
//! | [`layout_node`] | 布局协议与滚动节点（`layout-node.ts`） |
//! | [`main_screen`] | 主屏/滚动缓冲区渲染器，按行差分（`tui-main-screen.ts`） |
//! | [`markdown`] | marked 等价的 lexer/renderer（`marked`、`components/markdown.ts`） |
//! | [`native`] | 原生平台层：剪贴板、修饰键、VT 输入（`native/**`，feature 门控） |
//! | [`oklab`] | Oklab / OKHSL 数值转换（`oklab.ts`） |
//! | [`program_status`] | OSC 7501 程序状态编码（`program-status.ts`） |
//! | [`stdin`] | 输入分片缓冲，拼出完整转义序列（`stdin-buffer.ts`） |
//! | [`terminal`] | `ProcessTerminal`：真实终端实现（`terminal.ts`） |
//! | [`terminal_colors`] | `Terminal` trait 与颜色/样式类型（`terminal-colors.ts` + `colors.ts` 类型） |
//! | [`terminal_image`] | 图片能力探测、Kitty/iTerm2 编码、OSC 8 链接（`terminal-image.ts`） |
//! | [`text`] | 宽度、ANSI、换行、截断、按词移动、kill ring/撤销（`utils.ts` 等） |
//! | [`tui`] | 组件协议、鼠标、overlay、`TuiBase`（`tui.ts`） |
//! | [`wheel`] | 滚轮事件换算成行数（`wheel-scroll.ts`） |

// --- 子模块 ---
// 模块职责见文件头的「模块索引」；详细文档在各模块自己的文件顶部（//!）。
// 不要在这些 `pub mod` 前加外层 `///`：外层文档会与文件内 `//!` 合并到父模块作用域解析
// 链接，使 `//!` 里指向本模块条目的短链接失效（docs/COMMENT_STYLE.md §4.2）。
pub mod alt_screen;
pub mod alt_screen_search;
pub mod autocomplete;
pub mod colors;
pub mod components;
pub mod editor_component;
pub mod flash;
pub mod fuzzy;
pub mod image;
pub mod index;
pub mod keybindings;
pub mod keys;
pub mod latex;
pub mod layout;
pub mod layout_node;
pub mod main_screen;
pub mod markdown;
pub mod native;
pub mod oklab;
pub mod program_status;
pub mod stdin;
pub mod terminal;
pub mod terminal_colors;
pub mod terminal_image;
pub mod text;
pub mod tui;
pub mod wheel;

// --- crate 根导出面（对应 `index.ts`） ---
//
// TS 的 `@earendil-works/kiana-tui` 入口就是 `src/index.ts`；Rust 侧对应 crate 根，因此把
// [`index`] 的再导出抬到根上，下游（`kiana-interactive` / `kiana-ui-kit` / `kiana-ext-api`）可以写
// `kiana_tui::Component` 而不是 `kiana_tui::tui::Component`（ARCHITECTURE §4.10：跨 crate 接口
// 一律 `pub` 且在 `lib.rs` 再导出）。模块路径仍然可用（`kiana_tui::tui::…`）。
pub use crate::index::*;
