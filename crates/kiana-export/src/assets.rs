//! template.html/template.css/template.js 与 vendor/{marked,highlight.js}（include_str!）。
//!
//! TS 侧 `generateHtml` 从磁盘读取 `core/export-html/` 的模板与 vendored 资源
//! （`getExportTemplateDir()`）。Rust 单二进制没有可靠的包目录，因此按
//! `ARCHITECTURE.md` §5 直接用 `include_str!` 内嵌同一份文件；导出结果的字节与
//! TS 读取磁盘时完全相同。
//!
//! 这些文件必须**逐字节**保持一致（SHA-256 见 `tests/assets.rs`）；不要在这里
//! 改写模板/CSS/JS。
//!
//! # 为什么用 `include_str!` 而不是运行时读文件
//!
//! TS 版从包目录读模板；Rust 单二进制发布时不一定有可用的包目录。默认情况下
//! [`crate::index::generate_html`] 直接使用这些常量（字节与 TS 读取磁盘文件时相同）；
//! 只有设置了 `KIANA_PACKAGE_DIR` 时才改为读取该目录下的同名文件。`include_str!` 在
//! 编译期嵌入内容，所以替换 `src/assets/` 里的文件后必须重新编译，并同步更新
//! `tests/assets.rs` 中的大小与 SHA-256 期望值。
//!
//! # 替换约束
//!
//! - `template.html` 恰有 6 个占位符：`{{CSS}}`、`{{JS}}`、`{{SESSION_DATA}}`、
//!   `{{MARKED_JS}}`、`{{MARKED_CJK_FRIENDLY_JS}}`、`{{HIGHLIGHT_JS}}`；
//! - `template.css` 恰有 4 个：`{{THEME_VARS}}`、`{{BODY_BG}}`、`{{CONTAINER_BG}}`、
//!   `{{INFO_BG}}`；
//! - `template.js` 与三个 vendor 文件不得出现 `{{`，否则后续替换会误伤其中的文本；
//! - `template.js` 里有 1 处 `$$`，`highlight.min.js` 里有 `$$` 与 `$&`，会参与 JS
//!   `String.replace` 的 GetSubstitution 语义（`index` 的替换函数复刻了该行为）。
//!
//! 以上约束都由 `tests/assets.rs` 断言；修改资产前先读该测试。

// 模板与样式表。
/// `template.html`（2,271 字节）：页面骨架，含 6 个 `{{...}}` 占位符。
pub const TEMPLATE_HTML: &str = include_str!("assets/template.html");
/// `template.css`（23,131 字节）：含 4 个主题变量占位符。
pub const TEMPLATE_CSS: &str = include_str!("assets/template.css");
/// `template.js`（86,667 字节，浏览器端 viewer 脚本）：Rust 侧不执行；插入模板时按
/// GetSubstitution 语义替换（其中的 `$$` 会变成 `$`，见 `index::js_replace_first`）。
pub const TEMPLATE_JS: &str = include_str!("assets/template.js");

// vendored 库。
/// `vendor/marked.min.js`（44,640 字节，marked v18.0.11）。
/// 浏览器端 Markdown 解析器。
pub const MARKED_MIN_JS: &str = include_str!("assets/vendor/marked.min.js");
/// `vendor/marked-cjk-friendly.min.js`（4,754 字节，v0.1.2）：marked 的 CJK 排版插件。
pub const MARKED_CJK_FRIENDLY_MIN_JS: &str =
    include_str!("assets/vendor/marked-cjk-friendly.min.js");
/// `vendor/highlight.min.js`（121,727 字节，highlight.js v11.9.0）。
/// 浏览器端代码高亮库。
pub const HIGHLIGHT_MIN_JS: &str = include_str!("assets/vendor/highlight.min.js");
