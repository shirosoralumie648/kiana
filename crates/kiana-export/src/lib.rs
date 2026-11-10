//! 对应 core/export-html：单文件自包含 HTML 导出（模板/CSS/vendored JS 原样嵌入）。
//!
//! TS 源：packages/coding-agent/src/core/export-html/*（含 template.html/css/js 与 vendor/）
//!
//! # 公开面
//!
//! - [`ansi_to_html`](fn@ansi_to_html)：ANSI SGR → 内联样式 HTML；
//! - [`tool_renderer`]：自定义工具的 TUI 渲染器 → HTML；
//! - [`index`]：颜色推导、主题变量、占位符单次替换、base64 会话数据、两个导出入口；
//! - [`assets`]：逐字节内嵌的模板与 vendored 资源。
//!
//! 导出的 HTML 与 TS 版本逐字节相同：模板/JS/CSS 内嵌同一份文件，会话数据按 JS
//! `JSON.stringify` 语义序列化后 base64。`template.js`（浏览器端 viewer）不在 Rust 侧
//! 重写。
//!
//! # 这个 crate 解决什么问题
//!
//! 把一个会话渲染成单个 HTML 文件：模板 DOM、内联 CSS、浏览器端 viewer 脚本与
//! vendored 的 Markdown/高亮库全部写进同一份 HTML，会话数据按 JS `JSON.stringify`
//! 语义序列化后以 base64 放进 `<script id="session-data" type="application/json">`。
//! 模板里没有 `<link>`/`<script src>` 一类外部引用，产物不依赖独立的 `.css`/`.js`
//! 兄弟文件。没有这个 crate，`kiana-session` 的 HTML 导出 API 与 `kiana` CLI 的
//! `--export` 都无法产出这种单文件 HTML。
//!
//! TS 实现在运行时从 `core/export-html/` 包目录读取模板与资源；Rust 单二进制没有
//! 可靠的包目录，因此默认改用 `include_str!` 内嵌同一份文件（见 [`assets`]）。只有
//! 设置了环境变量 `KIANA_PACKAGE_DIR` 时，[`generate_html`] 才改为从磁盘读取，并
//! 按 Node 的文件系统文案报错。
//!
//! # 在系统中的位置
//!
//! 上游依赖（见 `Cargo.toml`）：
//!
//! - `kiana-session-core`：`SessionManager` 负责打开/读取会话，导出的是它的**原始
//!   JSON**（不是 typed 视图）；
//! - `kiana-json`：JS 语义的 JSON 序列化与字符串引号；
//! - `kiana-core`：`APP_NAME`、路径规范化与导出模板目录；
//! - `kiana-theme`：主题 token、解析后的颜色与导出专用颜色；
//! - `kiana-ext-api` / `kiana-tui`：工具渲染器调用的渲染回调与 `Component`；
//! - `kiana-ai`：`JsonValue` 与消息内容块类型。
//!
//! 下游调用方：
//!
//! - `kiana-session` 的 `AgentSession::export_to_html`：传入实时会话、`systemPrompt`/
//!   `tools` 与自定义工具渲染器，并把 [`ExportError`] 的文案转成 `AgentSessionError`；
//! - `kiana-coding-agent` 的 `--export` 分支：调用 [`export_from_file`]，成功打印
//!   `Exported to: ...`，失败打印 `Error: ...` 并以退出码 1 结束；
//! - 本 crate 的集成测试 `tests/`（资产校验、golden、XSS、CJK 等）。
//!
//! # 核心概念
//!
//! - [`SessionData`]：写进 HTML 的完整载荷，字段顺序即 JSON 键序；
//! - [`ExportOptions`]：输出路径、主题名与可选的工具渲染器（`None` 表示不预渲染）；
//! - [`ExportSessionState`]：调用方适配出的 `systemPrompt`/`tools` 子集，避免本 crate
//!   依赖 `kiana-agent-core`；
//! - [`ToolHtmlRenderer`]：把工具调用/结果的 TUI 渲染转成 HTML；默认渲染器
//!   `DefaultToolHtmlRenderer` 会吞掉扩展渲染回调的 panic，`render_call`/`render_result`
//!   各自降级为“没有自定义渲染”；自定义实现需自行保证不 panic，其 panic 会向调用方传播；
//! - [`assets`]：逐字节内嵌的模板与 vendored 资源；导出需与 golden 基线逐字节一致，故必须保留原始字节。
//!
//! # 一次典型调用
//!
//! [`export_session_to_html`]：
//!
//! 1. 取 `SessionManager` 关联的会话文件路径；没有关联（内存会话）直接报
//!    [`ExportError::CannotExportInMemorySession`]，路径非空但文件已不存在报
//!    [`ExportError::NothingToExport`]；
//! 2. 读取原始 `header`/`entries` JSON；若传了工具渲染器，按条目顺序预渲染自定义
//!    工具，结果按 `toolCallId` 的首次赋值顺序存进 map；
//! 3. 计算 `leafId`，组装 [`SessionData`]；
//! 4. [`generate_html`]：选资产 → 生成主题变量与派生颜色 → JS 语义序列化 + base64 →
//!    按固定顺序对 CSS/HTML 各占位符做**首次**替换；
//! 5. 写文件，返回 `normalizePath` 之后的路径（相对路径不会被 resolve 成绝对路径）。
//!
//! [`export_from_file`] 复用同一段生成逻辑，但先 `resolvePath` 输入路径、用
//! `SessionManager::open` 打开会话，且不消费 `systemPrompt`/`tools`；
//! `ExportOptions::tool_renderer` 即使提供也不生效。
//!
//! # 边界与失败模式
//!
//! - 错误统一为 [`ExportError`]：前三个变体的文案是当前稳定契约（改动需同步测试）。`Theme` 透传
//!   `kiana-theme` 的消息；`Session` 对非 fs 错误透传下层消息，fs 错误经
//!   `node_session_read_error`/`node_fs_error` 按 Node 文案重建（如 `EISDIR … , read`、
//!   `EACCES … , open`），预渲染时还承载 `js_null_deref_error` 合成的 V8 `TypeError`
//!   文案；`Io` 按 Node `fs` 文案重建。调用方直接消费 `Display` 文本
//!   （CLI 原样打印，`kiana-session` 用它构造 `AgentSessionError`），因此错误文案是
//!   兼容性契约，不能按 Rust 习惯改写。
//! - [`generate_html`] 在 `KIANA_PACKAGE_DIR` 已设置时从磁盘读六个资产文件；缺失或
//!   读失败返回 [`ExportError::Io`]，不会静默回退到内嵌副本。
//! - 导出的逐字节一致性依赖三件事：资产不变；每个占位符恰好替换一次且顺序固定
//!   （replacement 中的 `$&`/`$$` 等按 JS GetSubstitution 语义解释）；会话数据按
//!   JS `JSON.stringify` 的键序与数字格式写出。这三处共同决定输出字节；改动资产、
//!   替换语义或序列化后必须重跑 golden 核对。替换顺序并非必然改变字节：base64
//!   字母表不含 `{`，先替换 `{{SESSION_DATA}}` 也是安全的（见
//!   spec/ca-tools-compaction-export.md 2.22.4），固定顺序主要是便于审计。
//! - 会话条目里模型可控的 JSON 可能极深，载荷序列化是**迭代**实现的，避免
//!   `serde_json::to_value` 式的递归遍历导致栈溢出。
//! - 工具渲染器由扩展提供，可能 panic：默认渲染器 `DefaultToolHtmlRenderer` 边界上用
//!   `catch_unwind` 吞掉并抑制 panic 输出，`render_call`/`render_result` 各自降级为
//!   “没有自定义渲染”，单个渲染器失败不会中断整个导出。另外，只有在提供工具渲染器
//!   执行预渲染时（`pre_render_custom_tools` 读取 `msg.role`/`block.type`），会话数据
//!   本身畸形（如 `message` 为 `null`）才会按 TS 的 `TypeError` 文案中止导出；无渲染器
//!   的导出只做原样序列化，`message: null` 会正常导出。
//!
//! # 阅读顺序
//!
//! [`index`]（导出主流程、主题变量、占位符替换与序列化）→ [`mod@ansi_to_html`]（ANSI
//! SGR → 内联样式）→ [`tool_renderer`]（TUI 渲染器 → HTML）→ [`assets`]（内嵌资源）。
//! 测试在 `tests/`：`assets.rs` 校验资产大小/SHA-256/占位符数量，`export_html_golden.rs`
//! 对比 TS 参考产物的 SHA-256 与 base64 载荷。

pub mod ansi_to_html;
pub mod assets;
pub mod index;
pub mod tool_renderer;

pub use ansi_to_html::{ansi_lines_to_html, ansi_to_html};
pub use index::{
    ExportError, ExportOptions, ExportSessionState, ExportToolInfo, RenderedToolHtml, SessionData,
    export_from_file, export_session_to_html, generate_html,
};
pub use tool_renderer::{
    DefaultToolHtmlRenderer, GetToolRenderersFn, RenderedToolResult, ToolHtmlRenderer,
    ToolHtmlRendererDeps, create_tool_html_renderer,
};
