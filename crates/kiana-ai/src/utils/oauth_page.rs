//! `utils/oauth-page.ts`——OAuth 回调页的 HTML。
//!
//! TS: `packages/ai/src/utils/oauth-page.ts`。
//!
//! 页面必须**逐字节**一致（浏览器渲染、`oauth-callback-server` 的测试与用户可见文本都依赖它），
//! 因此模板被原样嵌入；只有 `title`/`heading`/`message`/`details` 与 logo SVG 是插值。
//! HTML 转义顺序与 TS 的 `escapeHtml` 相同：`&` → `<` → `>` → `"` → `'`。
//!
//! # 为什么逐字节锁定
//!
//! 页面由本地 OAuth 回调服务器直接作为 HTTP 响应体返回，浏览器渲染结果与用户可见文案
//! 都是契约；`tests/oauth_page.rs` 用 `tests/fixtures/utils/oauth_page.json`（由 TS 参照
//! 实现生成）做**整串相等**断言。任何缩进、换行、属性顺序或实体写法（例如把 `&#39;`
//! 改成 `&#x27;`）都会让测试失败。修改模板必须同步重新生成 fixture，不能只改这一侧。
//!
//! # 模板拼接顺序
//!
//! `render_page` 按固定顺序拼接：`PAGE_BEFORE_TITLE` → title → `PAGE_AFTER_TITLE` →
//! `LOGO_SVG` → `PAGE_AFTER_LOGO` → heading → `PAGE_AFTER_HEADING` → message →
//! `PAGE_AFTER_MESSAGE` →（可选）details 块 → `PAGE_AFTER_DETAILS`。拆分点与 TS 模板的
//! `${...}` 插值位置一一对应；details 为空时不输出空块（TS 的三元表达式同样不输出）。
//!
//! # 转义与安全边界
//!
//! [`escape_html`] 只覆盖 `& < > " '` 五个字符，且顺序固定（先 `&`，否则后续替换产生的
//! `&` 会被二次转义）。模板只把这些插值放进文本节点，没有属性插值；但它**不是**富文本
//! 过滤器：不清理控制字符、不限制长度。调用方仍应只传入错误消息等短文本。页面不加载
//! 外部脚本/样式，logo 是内联 SVG，因此响应不依赖第三方资源。
//!
//! # 调用方
//!
//! `auth::oauth::callback_server`、`auth::oauth::openai_chatgpt` 与
//! `kiana-builtin-extensions::mcp::oauth`。HTTP 状态码由调用方决定（如 400/404），本模块
//! 只产出响应体。

/// `LOGO_SVG`。
///
/// 内联 SVG（`aria-hidden="true"`，三个色块），不引用外部资源。它被原样插入 `<div
/// class="logo">`；与模板其余部分一样必须逐字节保持，改动需同步 fixture。
const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#F09082" d="M165.29 165.29H517.36V400H400V282.65H165.29Z"/><path fill="#4D9ABF" d="M165.29 282.65H282.65V400H400V517.36H282.65V634.72H165.29Z"/><path fill="#F1BE58" d="M517.36 400H634.72V634.72H517.36Z"/></svg>"##;

/// 模板在 `${title}` 之前的片段。
const PAGE_BEFORE_TITLE: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>"##;
/// `${title}` 与 `${LOGO_SVG}` 之间。
const PAGE_AFTER_TITLE: &str = r##"</title>
  <style>
    :root {
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }
    * { box-sizing: border-box; }
    html { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }
    main {
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }
    .logo {
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }
    p {
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }
    .details {
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }
  </style>
</head>
<body>
  <main>
    <div class="logo">"##;
/// `${LOGO_SVG}` 与 `${heading}` 之间。
const PAGE_AFTER_LOGO: &str = r##"</div>
    <h1>"##;
/// `${heading}` 与 `${message}` 之间。
const PAGE_AFTER_HEADING: &str = r##"</h1>
    <p>"##;
/// `${message}` 与 details 条件块之间。
const PAGE_AFTER_MESSAGE: &str = r##"</p>
    "##;
/// details 条件块之后。
const PAGE_AFTER_DETAILS: &str = r##"
  </main>
</body>
</html>"##;

/// `escapeHtml(value)`。
///
/// 按 `&` → `<` → `>` → `"` → `'` 的顺序做**单次**替换，返回新 `String`（不修改入参）：
/// `&` 必须最先处理，否则替换 `<` 等字符时新产生的 `&` 会被再次转义。实体写法是当前稳定
/// 契约（单引号用 `&#39;`，不要写成 `&#x27;`，改动需同步测试）。不处理反引号、控制字符或
/// 非 ASCII 字符。
pub fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `renderPage(options)`。
///
/// 把四个插值字段分别转义后按常量片段拼接；`details` 为 `None` 或空串时不输出
/// `<div class="details">` 块（TS 的 `options.details ? ... : ""` 对 `""` 为假）。仅含
/// 空白的 details 在 JS 中是真值，会被渲染——这里的 `!details.is_empty()` 与之一致。
/// `with_capacity` 的 `512` 只是给插值预留空间，不是长度上限，也不影响输出字节。
fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let title = escape_html(title);
    let heading = escape_html(heading);
    let message = escape_html(message);
    let details = details
        .filter(|details| !details.is_empty())
        .map(escape_html);

    let mut page = String::with_capacity(PAGE_BEFORE_TITLE.len() + PAGE_AFTER_DETAILS.len() + 512);
    page.push_str(PAGE_BEFORE_TITLE);
    page.push_str(&title);
    page.push_str(PAGE_AFTER_TITLE);
    page.push_str(LOGO_SVG);
    page.push_str(PAGE_AFTER_LOGO);
    page.push_str(&heading);
    page.push_str(PAGE_AFTER_HEADING);
    page.push_str(&message);
    page.push_str(PAGE_AFTER_MESSAGE);
    if let Some(details) = details {
        page.push_str(r#"<div class="details">"#);
        page.push_str(&details);
        page.push_str("</div>");
    }
    page.push_str(PAGE_AFTER_DETAILS);
    page
}

/// `oauthSuccessHtml(message)`。
///
/// 成功页：`title` 与 `heading` 都固定为 `"Authentication successful"`，不包含 details
/// 块。`message` 会先转义，因此调用方可以传入任意文案（包括带 `<` 的文本）而不会注入
/// 标记。
pub fn oauth_success_html(message: &str) -> String {
    render_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

/// `oauthErrorHtml(message, details?)`。
///
/// 失败页：`title` 与 `heading` 都固定为 `"Authentication failed"`。`details` 为 `None`
/// 或空串时不渲染 details 块；非空（包括只有空白）时渲染成等宽字体的 `<div>`，并保留
/// 换行（模板的 `white-space: pre-wrap`）。`message` 与 `details` 都会转义。
pub fn oauth_error_html(message: &str, details: Option<&str>) -> String {
    render_page(
        "Authentication failed",
        "Authentication failed",
        message,
        details,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_success_page() {
        let html = oauth_success_html("You can close this window.");
        assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
        assert!(html.contains("<title>Authentication successful</title>"));
        assert!(html.contains("<h1>Authentication successful</h1>"));
        assert!(html.contains("<p>You can close this window.</p>"));
        assert!(!html.contains("class=\"details\""));
        assert!(html.ends_with("</html>"));
        assert!(html.contains("viewBox=\"0 0 800 800\""));
    }

    #[test]
    fn renders_the_error_page_with_details() {
        let html = oauth_error_html("Sign in failed.", Some("state mismatch"));
        assert!(html.contains("<title>Authentication failed</title>"));
        assert!(html.contains(r#"<div class="details">state mismatch</div>"#));
    }

    #[test]
    fn escapes_html_in_every_field() {
        let html = oauth_error_html("<bad>", Some("a & b \"c\" 'd'"));
        assert!(html.contains("<p>&lt;bad&gt;</p>"));
        assert!(html.contains("a &amp; b &quot;c&quot; &#39;d&#39;"));
    }

    #[test]
    fn omits_an_empty_details_block() {
        let html = oauth_error_html("x", Some(""));
        assert!(!html.contains("class=\"details\""));
    }
}
