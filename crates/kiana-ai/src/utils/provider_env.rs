//! `utils/provider-env.ts`——provider 环境变量取值。
//!
//! 顺序：scoped `env` → 进程环境 → Bun 沙箱回退（`/proc/self/environ`）→ 缺失。
//! 用 `||`，因此**空字符串视为缺失**（TS 源码用 `||`，不是 `??`）。
//!
//! Bun 沙箱回退只在「进程 env 为空」时生效（`Object.keys(process.env).length === 0`），
//! 解析一次后缓存（含失败的空缓存）。Rust 侧只保留 Linux 的 `/proc/self/environ` 路径。
//!
//! TS: `utils/provider-env.ts#getProviderEnvValue`
//!
//! # 为什么需要这个模块
//! provider 适配器在解析凭据、区域、端点等配置时，既要允许调用方用请求级的
//! [`ProviderEnv`] 临时覆盖，又要回落到宿主进程的环境变量。把「三级查找」集中在一处，
//! 各家适配器才不会各自实现出不同的优先级与空值语义。
//!
//! # 执行流程
//! 1. 传入了 `env` 且 `env[name]` 存在且非空：直接返回该值（克隆一份 `String`）。
//! 2. 否则读进程环境；这一步每次调用都会重新查，运行期 `set_var`/`remove_var` 的结果
//!    对后续调用可见。
//! 3. 前两步都拿不到非空值时才考虑 `/proc/self/environ` 回退。只有「进程环境里一个
//!    变量都没有」这种异常情形才会真正去读文件，其余情况立即得到 `None`。
//!
//! # 谁在调用
//! `api/anthropic_messages.rs`、`api/bedrock_converse_stream.rs`、`api/azure_openai_config.rs`、
//! `api/google_vertex.rs`、`api/google_shared.rs`、`api/openai_completions.rs` 等适配器在解析
//! 凭据（API key / Google ADC / AWS 配置）、云区域与项目 ID、缓存策略等配置时调用。
//!
//! # 重要约束与失败模式
//! - 空字符串与「键不存在」等价：返回值永远不会是 `Some("")`（TS 用 `||` 而不是 `??`）。
//! - `std::env::var` 遇到非 UTF-8 的值返回 `Err`，本实现把它当作缺失；TS 的
//!   `process.env` 则总能给出字符串。需要逐字节保真的调用方要自行处理这个差异。
//! - `/proc/self/environ` 的解析结果在首次使用时写入进程级缓存，之后**不再刷新**；
//!   读到空表（非 Linux、读取失败或文件本身为空）也会被缓存，不会在下次调用重试。
//! - 缓存的 `Mutex` 在一次 `get_or_insert_with`（内部含同步文件读取）期间保持持有；
//!   锁被毒化时通过 `into_inner` 恢复，不会因此 panic。

use std::sync::Mutex;

use crate::types::ProviderEnv;

/// 缓存：`None` 表示尚未读取；`Some(map)` 是 `/proc/self/environ` 的解析结果（可能为空）。
///
/// 只在 `get_bun_sandbox_env_value` 里读写，且整段临界区都是同步代码（读文件 + 解析），
/// 不含 `await`。一旦被写成 `Some(_)` 就再也不会被替换，所以运行期环境变化不会反映到这里。
static PROC_ENV_CACHE: Mutex<Option<ProviderEnv>> = Mutex::new(None);

/// 读取 `/proc/self/environ`（按 `\0` 拆分，第一个 `=` 之前是键且必须非空）。
///
/// 解析细节（与 TS 的 `readFileSync(path, "utf-8")` + `split("\0")` 对齐）：
/// - 每一段只在**第一个** `=` 处切开，因此值里允许再出现 `=`（测试 `B=x=y`）。
/// - `idx == 0`（即 `=VALUE`）或整段没有 `=` 的条目被跳过，它们不是合法的 `KEY=VALUE`。
/// - 字节流用 `String::from_utf8_lossy` 解码：非法 UTF-8 序列会变成 U+FFFD，而不是导致失败。
///
/// 读文件失败（不存在、权限不足、非 Linux）时返回空表，调用方无法区分「读失败」与
/// 「文件为空」——两者都表现为「这个键查不到」。
fn read_proc_env() -> ProviderEnv {
    let mut map = ProviderEnv::new();
    let Ok(data) = std::fs::read("/proc/self/environ") else {
        return map;
    };
    let text = String::from_utf8_lossy(&data);
    for entry in text.split('\0') {
        if let Some(idx) = entry.find('=')
            && idx > 0
        {
            map.insert(entry[..idx].to_string(), entry[idx + 1..].to_string());
        }
    }
    map
}

/// Bun 沙箱回退：仅当进程环境变量为空（Bun 单文件二进制在 Linux 沙箱里的已知缺陷）时生效。
///
/// 返回 `None` 有两层含义：一是进程环境非空、根本不启用回退；二是回退表里没有这个键。
/// 判定「进程 env 是否为空」只看是否存在任意一项，因此只要进程里还有别的变量，本回退就
/// 整体关闭——即使请求的键恰好不在进程环境里。回退表里的空字符串由调用方
/// [`get_provider_env_value`] 过滤，本函数原样返回。
fn get_bun_sandbox_env_value(name: &str) -> Option<String> {
    // 进程 env 非空就不启用回退（与 TS 的 `Object.keys(process.env).length > 0` 一致）。
    if std::env::vars_os().next().is_some() {
        return None;
    }
    let mut guard = PROC_ENV_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(read_proc_env);
    cache.get(name).cloned()
}

/// `getProviderEnvValue(name, env?)`：scoped 覆盖 → 进程 env → Bun 沙箱回退 → `None`。
///
/// # 参数与返回
/// - `name`：环境变量名，原样精确匹配（是否大小写敏感取决于操作系统）。
/// - `env`：可选的请求级覆盖表；`None` 等价于空表，不会报错。
/// - `Some(value)` 保证 `value` 非空；键缺失或值为空串一律得到 `None`。
///
/// # 注意
/// 进程环境分支每次调用都重新读取，而 `/proc/self/environ` 分支只在进程环境整体为空时
/// 才可达，且结果被永久缓存。因此在普通进程里第 3 步是死路径，环境变量的后续变化只能
/// 通过第 1、2 步观察到。
///
/// TS: `utils/provider-env.ts#getProviderEnvValue`
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(value) = env.and_then(|env| env.get(name))
        && !value.is_empty()
    {
        return Some(value.clone());
    }
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Some(value);
    }
    get_bun_sandbox_env_value(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_env_wins_and_empty_is_missing() {
        let mut env = ProviderEnv::new();
        env.insert("KIANA_AI_TEST_ENV_KEY".into(), "scoped".into());
        assert_eq!(
            get_provider_env_value("KIANA_AI_TEST_ENV_KEY", Some(&env)),
            Some("scoped".to_string())
        );
        env.insert("KIANA_AI_TEST_ENV_KEY".into(), String::new());
        // 空串视为缺失：回落到进程 env（该变量不存在）。
        assert_eq!(
            get_provider_env_value("KIANA_AI_TEST_ENV_KEY", Some(&env)),
            None
        );
    }

    #[test]
    fn falls_back_to_process_env() {
        // SAFETY: 测试进程内唯一的写者（该变量名不与其它测试共享）。
        unsafe { std::env::set_var("KIANA_AI_TEST_PROCESS_ENV", "process") };
        assert_eq!(
            get_provider_env_value("KIANA_AI_TEST_PROCESS_ENV", None),
            Some("process".to_string())
        );
        unsafe { std::env::remove_var("KIANA_AI_TEST_PROCESS_ENV") };
        assert_eq!(
            get_provider_env_value("KIANA_AI_TEST_PROCESS_ENV", None),
            None
        );
    }

    #[test]
    fn scoped_env_beats_process_env() {
        unsafe { std::env::set_var("KIANA_AI_TEST_OVERRIDE_ENV", "process") };
        let mut env = ProviderEnv::new();
        env.insert("KIANA_AI_TEST_OVERRIDE_ENV".into(), "scoped".into());
        assert_eq!(
            get_provider_env_value("KIANA_AI_TEST_OVERRIDE_ENV", Some(&env)),
            Some("scoped".to_string())
        );
        unsafe { std::env::remove_var("KIANA_AI_TEST_OVERRIDE_ENV") };
    }

    #[test]
    fn parses_proc_environ_format() {
        // 只验证解析规则本身（不依赖真实 /proc 内容）。
        let mut map = ProviderEnv::new();
        for entry in "A=1\0EMPTY=\0=BROKEN\0B=x=y".split('\0') {
            if let Some(idx) = entry.find('=')
                && idx > 0
            {
                map.insert(entry[..idx].to_string(), entry[idx + 1..].to_string());
            }
        }
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
        assert!(map.get("BROKEN").is_none());
        // 值里允许出现 `=`。
        assert_eq!(map.get("B").map(String::as_str), Some("x=y"));
    }
}
