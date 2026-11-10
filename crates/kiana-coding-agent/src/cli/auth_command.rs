//! `cli/auth-command.ts`（126 行）：`kiana auth` 子命令的解析与校验。
//!
//! TS: `packages/coding-agent/src/cli/auth-command.ts`
//!
//! # 职责边界（两段式解析）
//! 本模块只做“识别与校验”，不解析通用选项、不访问模型运行时：
//! [`parse_auth_command`] 先剥离 `auth`、子命令名以及本子命令专属的开关
//! （`--json` / `--credentials` / `--no-refresh` / `--min-expiry`），把其余 token 按原
//! 顺序放进 [`AuthCommand::args`]；调用方（`app::run_auth_command`）再对它跑
//! `args::parse_args`，最后用 [`validate_auth_command_args`] 做跨字段校验。
//! 这样 auth 子命令复用了通用 flag 解析；代价是专属开关的位置无关（可出现在任意位置），
//! 重复出现也不报错（布尔项重复置位，`--min-expiry` 后者覆盖前者）。
//!
//! # 调用方与错误约定
//! `app::run_auth_command` 先用 [`is_auth_command_help`] 短路帮助，再处理
//! [`parse_auth_command`] 的返回值：`Ok(None)` 表示 argv 不是 auth 命令；`Err` 的
//! `Display` 就是 TS 的 `error.message`（stderr 的 `Error: ` 前缀与退出码由调用方加）。
//! 本模块的错误都是 [`AuthCommandError`]，不包含 IO/网络错误。
//!
//! # 逐字文案
//! usage、命令名与帮助文本是当前稳定契约（`tests/credential_print.rs` 有 golden 断言）；
//! 修改空格或标点会破坏 CLI 契约，改动需同步测试。

use kiana_ai::auth::types::AuthResult;

use crate::cli::args::Args;

/// `AuthCommandKind`：`"check" | "api_key" | "bearer_token"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthCommandKind {
    /// `kiana auth check`。
    #[default]
    Check,
    /// `kiana auth print-api-key`。
    ApiKey,
    /// `kiana auth print-bearer-token`。
    BearerToken,
}

impl AuthCommandKind {
    /// TS 的内部字面量（`"check"` / `"api_key"` / `"bearer_token"`）。
    ///
    /// 注意 `ApiKey` 返回 `"api_key"`，不是 CLI 拼写 `print-api-key`；用户可见的
    /// 子命令名见 [`get_auth_command_name`]。
    pub fn as_str(self) -> &'static str {
        match self {
            AuthCommandKind::Check => "check",
            AuthCommandKind::ApiKey => "api_key",
            AuthCommandKind::BearerToken => "bearer_token",
        }
    }
}

/// `AuthCommand`。
///
/// `args` 是**尚未解析**的剩余 token（含 `--provider` / `--model` 及其取值），不包含已被
/// 本模块消费的专属开关。`json` / `credentials` / `no_refresh` 只对
/// [`AuthCommandKind::Check`] 有意义，`min_expiry_ms` 只对
/// [`AuthCommandKind::BearerToken`] 有意义；[`parse_auth_command`] 已拒绝跨子命令使用。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthCommand {
    /// 子命令。
    pub kind: AuthCommandKind,
    /// 交给 `parseArgs` 的剩余参数。
    pub args: Vec<String>,
    /// `--json`（仅 check）。
    pub json: bool,
    /// `--credentials`（仅 check）。
    pub credentials: bool,
    /// `--no-refresh`（仅 check）。
    pub no_refresh: bool,
    /// `--min-expiry` 的毫秒数（仅 print-bearer-token）。
    pub min_expiry_ms: Option<i64>,
}

/// `AuthCommandError`：`Display` 就是 TS `error.message`。
///
/// 只承载参数/文案错误（没有 source）；调用方原样打印，除 `Error: ` 前缀外不再改写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCommandError {
    /// 逐字消息。
    pub message: String,
}

impl AuthCommandError {
    /// 以消息构造。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AuthCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AuthCommandError {}

/// `AUTH_COMMAND_USAGE`。
///
/// 返回逐字 usage 行（`{app}` 展开为 `APP_NAME`），用于错误后的
/// `Use ... or <usage>.` 提示；`tests/credential_print.rs` 断言了 check 的字符串。
pub fn get_auth_command_usage(kind: AuthCommandKind) -> String {
    let app = kiana_core::config::APP_NAME;
    match kind {
        AuthCommandKind::Check => format!(
            "{app} auth check --provider <provider> [--json] [--credentials] [--no-refresh]"
        ),
        AuthCommandKind::ApiKey => {
            format!("{app} auth print-api-key --provider <provider> [--model <model>]")
        }
        AuthCommandKind::BearerToken => format!(
            "{app} auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]"
        ),
    }
}

/// `getAuthCommandName(kind)`。
///
/// 返回用户可见的子命令名（`auth check` / `auth print-api-key` /
/// `auth print-bearer-token`），用于 `Unknown option ... for "<name>".` 错误。
pub fn get_auth_command_name(kind: AuthCommandKind) -> &'static str {
    match kind {
        AuthCommandKind::Check => "auth check",
        AuthCommandKind::ApiKey => "auth print-api-key",
        AuthCommandKind::BearerToken => "auth print-bearer-token",
    }
}

/// `isAuthCommandHelp(args)`。
///
/// 判定比名字更宽：第一个参数是 `auth`，且满足任一条件即为真——
/// 没有第二个参数、第二个参数是 `help`、或任意位置出现 `--help` / `-h`。
/// 因此 `kiana auth`、`kiana auth --help`、`kiana auth check --help` 都打印帮助；
/// 即使 `--help` 出现在取值位置（如 `--provider --help`）也会命中。
/// 调用方必须在 [`parse_auth_command`] 之前调用它（否则裸 `auth` 会被判为未知子命令）。
pub fn is_auth_command_help(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "auth")
        && (args.get(1).is_none()
            || args.get(1).is_some_and(|arg| arg == "help")
            || args.iter().any(|arg| arg == "--help")
            || args.iter().any(|arg| arg == "-h"))
}

/// `printAuthCommandHelp()` 的纯文本（`console.log` 会追加末尾换行，这里已包含）。
///
/// 返回字节即用户可见输出；`tests/credential_print.rs::auth_command_help_text_matches_ts`
/// 逐字比对（含末尾换行）。
pub fn auth_command_help_text() -> String {
    let app = kiana_core::config::APP_NAME;
    format!(
        "Usage:
  {app} auth print-api-key [--provider <provider>] [--model <model>]
  {app} auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]
  {app} auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]

Auth commands require at least one of --provider or --model. Checks refresh expired OAuth credentials by default; --no-refresh prevents this. --credentials emits the credential, or includes it in JSON output.
"
    )
}

/// `printAuthCommandHelp()`：单次 `console.log`（文本已含末尾换行）。
pub fn print_auth_command_help(output: &crate::cli::setup::Output) {
    output.write_stdout(&auth_command_help_text());
}

/// `parseAuthCommand(args)`。
///
/// `args[0] != "auth"` 返回 `Ok(None)`；未知子命令或非法选项返回错误。
///
/// 子命令由 `args[1]` 决定（缺省也落到未知子命令错误，调用方通常先被
/// [`is_auth_command_help`] 拦截裸 `auth`）。扫描从 `args[2]` 开始：
/// - `--min-expiry` 只允许 `print-bearer-token`，缺值或格式不符即错误；
/// - `--json` / `--credentials` / `--no-refresh` 只允许 `check`；
/// - 其余 token（包括 `--provider` / `--model` 及其取值、以及真正的未知选项）按原顺序
///   进入 [`AuthCommand::args`]，留给后续 `parse_args` 与 [`validate_auth_command_args`]。
///
/// 被消费的专属开关不会出现在 `args` 里，因此不会再被 `parse_args` 当作未知 flag。
/// 所有错误消息都是最终用户可见文本。
pub fn parse_auth_command(args: &[String]) -> Result<Option<AuthCommand>, AuthCommandError> {
    if args.first().map(String::as_str) != Some("auth") {
        return Ok(None);
    }

    let kind = match args.get(1).map(String::as_str) {
        Some("check") => AuthCommandKind::Check,
        Some("print-api-key") => AuthCommandKind::ApiKey,
        Some("print-bearer-token") => AuthCommandKind::BearerToken,
        _ => {
            let value = args.get(1).map(String::as_str).unwrap_or("");
            return Err(AuthCommandError::new(format!(
                "Unknown auth command \"{value}\". Use \"{0} auth print-api-key\", \"{0} auth print-bearer-token\", or \"{0} auth check\".",
                kiana_core::config::APP_NAME
            )));
        }
    };

    let mut command_args: Vec<String> = Vec::new();
    let mut json = false;
    let mut credentials = false;
    let mut no_refresh = false;
    let mut min_expiry_ms: Option<i64> = None;

    let mut index = 2usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--min-expiry" {
            if kind != AuthCommandKind::BearerToken {
                return Err(AuthCommandError::new(
                    "--min-expiry is only supported by print-bearer-token",
                ));
            }
            index += 1;
            let value = args.get(index).map(String::as_str);
            let parsed = value.and_then(parse_min_expiry);
            let Some(parsed) = parsed else {
                return Err(AuthCommandError::new(
                    "--min-expiry must use a duration such as 30m or 1h",
                ));
            };
            min_expiry_ms = Some(parsed);
            index += 1;
            continue;
        }
        if arg == "--json" || arg == "--credentials" || arg == "--no-refresh" {
            if kind != AuthCommandKind::Check {
                return Err(AuthCommandError::new(format!(
                    "{arg} is only supported by auth check"
                )));
            }
            match arg {
                "--json" => json = true,
                "--credentials" => credentials = true,
                _ => no_refresh = true,
            }
            index += 1;
            continue;
        }
        command_args.push(args[index].clone());
        index += 1;
    }

    Ok(Some(AuthCommand {
        kind,
        args: command_args,
        json,
        credentials,
        no_refresh,
        min_expiry_ms,
    }))
}

/// `/^(\d+)(ms|s|m|h)$/iu` + `Number(amount) * multiplier`。
///
/// 正则的 `i` 只影响**匹配**：`match[2]` 保留输入的原始大小写，而 TS 后续的
/// `unit === "ms" | "s" | "m"` 比较是区分大小写的，因此 `30M`/`30S`/`30MS` 都落到
/// 默认分支（3_600_000）——`30M` 是 30 小时而不是 30 分钟。
///
/// `Number(digits)` 是 IEEE-754 双精度（正确舍入，超范围得到 `Infinity`），不是 `parseInt`。
/// 结果最终写入 `min_oauth_validity_ms: Option<i64>`（kiana-ai 的冻结类型），因此把超出 i64
/// 的数值饱和到边界：它只作为“剩余有效期下限”参与比较，饱和值与 JS 的 3.6e30/Infinity
/// 给出相同的刷新决定。
///
/// 返回 `None` 仅表示格式不匹配（含缺值/空串），由 [`parse_auth_command`] 转成用户可见错误。
fn parse_min_expiry(value: &str) -> Option<i64> {
    static DURATION_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new("(?i)^([0-9]+)(ms|s|m|h)$").expect("valid duration regex")
    });
    let captures = DURATION_RE.captures(value)?;
    // Rust 的 f64 解析与 JS `Number()` 一样做正确舍入；超长数字串得到 Infinity。
    let amount = captures
        .get(1)?
        .as_str()
        .parse::<f64>()
        .unwrap_or(f64::INFINITY);
    // 逐字复刻 TS：大小写敏感的 `unit === ...` 比较，默认（含 `h`/`H` 与任何大写单位）为小时。
    let multiplier = match captures.get(2)?.as_str() {
        "ms" => 1.0,
        "s" => 1_000.0,
        "m" => 60_000.0,
        _ => 3_600_000.0,
    };
    Some(js_number_to_i64(amount * multiplier))
}

/// JS Number → i64（向零截断 + 饱和）；非有限值与越界值取 i64 边界。
fn js_number_to_i64(value: f64) -> i64 {
    if value.is_nan() {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        value as i64
    }
}

/// `validateAuthCommandArgs(args, kind)` 的返回值（`{provider?, model?}`）。
///
/// 两个字段都已用 JS `trim()` 去首尾空白，空白串归一为 `None`；
/// [`validate_auth_command_args`] 保证至少一个为 `Some`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthCommandTarget {
    /// 去掉首尾空白后的 provider。
    pub provider: Option<String>,
    /// 去掉首尾空白后的 model。
    pub model: Option<String>,
}

/// `validateAuthCommandArgs(args, kind)`。
///
/// 输入是**已经过 `parse_args`** 的 [`Args`]（本模块只剥离专属开关，通用选项由调用方
/// 解析）。校验顺序固定，错误消息即用户可见文本：
/// 1. 取 `provider` / `model`，按 JS `trim()` 去空白，空串归一为 `None`；
/// 2. `unknown_flags` 非空 → 报第一个未知选项（`IndexMap` 的插入顺序）：
///    `Unknown option --<name> for "<命令名>".`；
/// 3. 出现 `--api-key`、位置消息或 `@file` → `Auth commands only accept --provider and --model`；
/// 4. `check` 与打印类子命令都要求 provider/model 至少一个，但错误文案不同。
///
/// 注意：已知但与本命令无关的通用选项（如 `--session`、`--thinking`）不会报错，因为
/// `parse_args` 已把它们收进具名字段——这与 TS 的实现一致，属于被接受的行为。
pub fn validate_auth_command_args(
    args: &Args,
    kind: AuthCommandKind,
) -> Result<AuthCommandTarget, AuthCommandError> {
    let provider = args
        .provider
        .as_deref()
        .map(kiana_tui::text::trim_js)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string);
    let model = args
        .model
        .as_deref()
        .map(kiana_tui::text::trim_js)
        .filter(|model| !model.is_empty())
        .map(str::to_string);

    if let Some(option) = args.unknown_flags.keys().next() {
        return Err(AuthCommandError::new(format!(
            "Unknown option --{option} for \"{}\".",
            get_auth_command_name(kind)
        )));
    }
    if args.api_key.is_some() || !args.messages.is_empty() || !args.file_args.is_empty() {
        return Err(AuthCommandError::new(
            "Auth commands only accept --provider and --model",
        ));
    }
    if kind == AuthCommandKind::Check {
        if provider.is_none() && model.is_none() {
            return Err(AuthCommandError::new(
                "Auth checks require --provider <provider> or --model <model>",
            ));
        }
        return Ok(AuthCommandTarget { provider, model });
    }
    if provider.is_none() && model.is_none() {
        return Err(AuthCommandError::new(
            "Credential printing requires --provider <provider> or --model <model>",
        ));
    }
    Ok(AuthCommandTarget { provider, model })
}

/// `getAuthCredential(auth)`：`apiKey` 优先，否则从 `Authorization` 头提取 Bearer token。
///
/// 细节：
/// - `api_key` 非空时直接返回它（空串视为未提供，继续看 headers）；
/// - header 名用 `to_lowercase()` 做大小写不敏感匹配，只取**第一个**名称匹配的项；
///   若该项值为 `None`，直接返回 `None`，不会再尝试后面的同名 header；
/// - Bearer 提取用与 JS `/^Bearer\s+(.+)$/iu` 等价的正则（见下方注释），
///   匹配失败或捕获组为空都返回 `None`。
pub fn get_auth_credential(auth: Option<&AuthResult>) -> Option<String> {
    let auth = auth?;
    if let Some(api_key) = auth.auth.api_key.as_ref()
        && !api_key.is_empty()
    {
        return Some(api_key.clone());
    }
    let authorization = auth.auth.headers.as_ref().and_then(|headers| {
        headers
            .iter()
            .find(|(name, _)| name.to_lowercase() == "authorization")
            .and_then(|(_, value)| value.clone())
    })?;
    static BEARER_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        // 逐字复刻 JS `/^Bearer\s+(.+)$/iu`：
        // - `\s` = ECMAScript WhiteSpace + LineTerminator（含 U+FEFF，**不含** U+0085；
        //   Rust 的 `\s` 反过来含 U+0085 不含 U+FEFF，因此显式列出集合）；
        // - `.` 不匹配 \n、\r、U+2028、U+2029；
        // - 未开启 multiline 时 `$` 只匹配字符串末尾（不匹配末尾换行之前）。
        regex::Regex::new(
            "(?i)^Bearer[\\t\\n\\x0B\\x0C\\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]+([^\\n\\r\u{2028}\u{2029}]+)$",
        )
        .expect("valid bearer regex")
    });
    BEARER_RE
        .captures(&authorization)
        .and_then(|captures| captures.get(1).map(|value| value.as_str().to_string()))
}
