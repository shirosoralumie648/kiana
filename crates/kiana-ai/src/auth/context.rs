//! auth/context.ts。
//!
//! 默认 auth context：env 变量来自 `process.env`（浏览器里恒 `undefined`），
//! 文件存在性用 `node:fs`（浏览器里恒 `false`）。
//!
//! 精确语义（spec/ai-auth.md §2.2）：
//! - `env(name)`：仅当值为字符串且 `value.trim().length > 0` 时返回**原值**（不 trim）；
//!   `trim` 是 JS 的 Unicode 空白（含 BOM），因此用 [`kiana_json::js_string::js_trim`]。
//! - `fileExists(path)`：前导 `~` 用 `os.homedir() + path.slice(1)` 展开（字符串拼接，不是
//!   `path.join`，所以 `~x` → `<home>x`）；任何错误 → `false`。
//!
//! # 为什么需要这一层
//! 鉴权的第一手信息来自「宿主环境」：环境变量与文件（`~/.aws/credentials`、ADC 文件等）。
//! 不同宿主访问它们的方式不同（Node 的 `process.env` / `node:fs`；测试或浏览器等宿主可以
//! 换成自己的实现），所以 `types` 里的 `AuthContext` 把这两件事抽象成 async trait，本文件
//! 提供唯一的生产默认实现。`providers/*` 与 `auth/resolve.rs` 只依赖 trait，不直接读进程
//! 环境。
//!
//! # 执行流程
//! - `env`：`std::env::var_os` 取原始 `OsString` → `to_string_lossy` 转字符串 →
//!   用 `js_trim` 判空 → 非空则返回**原值**。实现是同步的，包在 `async_trait` 里没有实际
//!   等待点。
//! - `file_exists`：先做 `~` 展开（拿不到 home 目录即 `false`），再用 `tokio::fs::metadata`
//!   探测；成功即 `true`，任何 IO 错误一律 `false`。
//!
//! # 注意事项
//! - 环境变量名含 `=` 或 NUL 时 `std::env::var_os` 返回 `None`，按「未设置」处理。
//! - `to_string_lossy` 会把非 UTF-8 字节替换成 U+FFFD，再参与 `js_trim` 与返回；Node 对
//!   本机环境变量的解码细节未在仓库内验证。
//! - TS 的浏览器分支（`env` 恒 `undefined`、`fileExists` 恒 `false`）在 Rust 侧没有对应
//!   代码：默认实现总是读真实进程环境。需要模拟浏览器/受限环境时注入自定义 `AuthContext`，
//!   不要调用本模块的默认实现。
//! - `file_exists` 跟随符号链接（`metadata` 的语义），目录也算存在；断链、权限不足等
//!   情况都是 `false`。
//!
//! TS: `src/auth/context.ts`

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::types::AuthContext;

/// 默认 [`AuthContext`] 实现（等价 TS 的 `defaultProviderAuthContext()` 返回值）。
///
/// 零大小、无状态：`Clone`/`Copy` 只是复制一个标记，不复制任何环境快照——每次调用
/// `env`/`file_exists` 都重新读取当时的进程环境与文件系统。因此同一个实例在配置变化后
/// 立刻看到新值，也意味着本类型不适合当作「配置快照」缓存。
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultProviderAuthContext;

/// `os.homedir()`。
///
/// 返回 `None` 表示当前环境无法确定 home 目录；此时任何前导 `~` 的路径都按「不存在」处理。
fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// 前导 `~` 展开：`os.homedir() + path.slice(1)`（字符串拼接）。
///
/// 没有 home 目录时返回 `None`（TS 的 `os.homedir()` 抛错 → `fileExists` 的 catch → `false`）。
///
/// 只有以 `~` 开头的路径才需要 home 目录；不以 `~` 开头的路径即使 `home` 为 `None` 也原样
/// 返回。`~` 单独出现时 `rest` 是空串，结果是 home 目录本身。
fn expand_tilde(path: &str, home: Option<PathBuf>) -> Option<String> {
    match path.strip_prefix('~') {
        None => Some(path.to_string()),
        Some(rest) => home.map(|home| format!("{}{}", home.display(), rest)),
    }
}

/// `process.env[name]` 的过滤：字符串且 trim 后非空才返回原值。
///
/// `js_trim` 而不是 `str::trim`：JS 的 `String.prototype.trim` 把 BOM（U+FEFF）等也当空白，
/// 两者认定的空白字符集合不同，对同一输入可能得出不同的「是否已配置」结论。返回值保留原始
/// 首尾空白。
fn filter_env_value(value: Option<std::ffi::OsString>) -> Option<String> {
    let value = value?;
    let value = value.to_string_lossy().into_owned();
    if kiana_json::js_string::js_trim(&value).is_empty() {
        None
    } else {
        Some(value)
    }
}

#[async_trait]
impl AuthContext for DefaultProviderAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        filter_env_value(std::env::var_os(name))
    }

    async fn file_exists(&self, path: &str) -> bool {
        // 先展开 `~`；失败（拿不到 home 目录）时不触碰文件系统，直接按不存在处理。
        let Some(resolved) = expand_tilde(path, home_dir()) else {
            return false;
        };
        // 用 `metadata` 而不是 `access`：只查询元数据，不要求目标可读/可写；成功（含目录、
        // 跟随符号链接后的目标）即存在，其余任何错误都落到 `false`。
        tokio::fs::metadata(&resolved).await.is_ok()
    }
}

/// `defaultProviderAuthContext()`：默认 auth context。
///
/// 每次调用都新建一个零大小的 `DefaultProviderAuthContext` 并装箱成 `Arc<dyn AuthContext>`；
/// 实例之间不共享状态，返回多个实例与返回同一个实例在行为上等价。`CreateModelsOptions`
/// 未显式提供 `auth_context` 时使用它。
///
/// TS: `src/auth/context.ts#defaultProviderAuthContext`
pub fn default_provider_auth_context() -> Arc<dyn AuthContext> {
    Arc::new(DefaultProviderAuthContext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_must_be_non_blank_strings() {
        assert_eq!(filter_env_value(None), None);
        assert_eq!(filter_env_value(Some("".into())), None);
        assert_eq!(filter_env_value(Some("   \t\n".into())), None);
        // 只含 BOM 也算空白（JS 的 trim）。
        assert_eq!(filter_env_value(Some("\u{feff}".into())), None);
        // 原值返回，不去除首尾空白。
        assert_eq!(
            filter_env_value(Some(" key ".into())),
            Some(" key ".to_string())
        );
    }

    #[test]
    fn tilde_expansion_concatenates_like_node() {
        let home = PathBuf::from("/home/tester");
        assert_eq!(
            expand_tilde("~", Some(home.clone())).as_deref(),
            Some("/home/tester")
        );
        assert_eq!(
            expand_tilde("~/x", Some(home.clone())).as_deref(),
            Some("/home/tester/x")
        );
        // `~x` → `<home>x`（不是 `<home>/x`）。
        assert_eq!(
            expand_tilde("~x", Some(home)).as_deref(),
            Some("/home/testerx")
        );
        assert_eq!(
            expand_tilde("x~", Some(PathBuf::from("/h"))).as_deref(),
            Some("x~")
        );
        // 没有 home → None（TS 抛错后 catch 成 false）。
        assert_eq!(expand_tilde("~/x", None), None);
    }

    #[tokio::test]
    async fn reports_existing_and_missing_files() {
        let context = DefaultProviderAuthContext;
        let mut path = std::env::temp_dir();
        path.push(format!("kiana-ai-auth-context-{}", std::process::id()));
        std::fs::write(&path, b"x").expect("write temp file");
        assert!(context.file_exists(&path.to_string_lossy()).await);
        let missing = format!("{}.missing", path.display());
        assert!(!context.file_exists(&missing).await);
        assert!(!context.file_exists("~").await || home_dir().is_some());
        let _ = std::fs::remove_file(&path);
    }
}
