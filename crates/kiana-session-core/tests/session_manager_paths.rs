//! 会话目录编码（`getDefaultSessionDirPath` / `getDefaultSessionDir`）。
//!
//! TS: `test/sdk-session-manager.test.ts`（"uses agentDir for the default persisted session
//! path"）与 `session-manager.ts` §2.2.7。期望值由 TS 参考实现生成（vitest/node probe）。

mod common;

use common::*;
use kiana_session_core::session_manager::{get_default_session_dir, get_default_session_dir_path};

/// `safePath = --<cwd 去掉开头分隔符、把 / \ : 换成 ->--`，落在 `<agentDir>/sessions/` 下。
#[test]
fn encodes_the_cwd_into_the_default_session_dir() {
    let temp = temp_dir("kiana-session-dir-");
    let agent = temp.path().to_string_lossy().into_owned();

    assert_eq!(
        get_default_session_dir_path("/home/u/proj", &agent),
        format!("{agent}/sessions/--home-u-proj--")
    );
    // 开头分隔符只去掉一个；内部的 `:` 换成 `-`；末尾的 `/` 被 resolve 去掉。
    assert_eq!(
        get_default_session_dir_path("/tmp/a:b/c/", &agent),
        format!("{agent}/sessions/--tmp-a-b-c--")
    );
    // agentDir 末尾的 `/` 同样被 resolve 去掉。
    assert_eq!(
        get_default_session_dir_path("/home/u/proj", &format!("{agent}/")),
        format!("{agent}/sessions/--home-u-proj--")
    );
}

/// 目录不存在时递归创建并返回路径；已存在时原样返回。
#[test]
fn creates_the_default_session_dir_when_missing() {
    let temp = temp_dir("kiana-session-dir-create-");
    let agent = temp.path().to_string_lossy().into_owned();
    let expected = format!("{agent}/sessions/--home-u-proj--");

    let created = get_default_session_dir("/home/u/proj", Some(&agent)).unwrap();
    assert_eq!(created, expected);
    assert!(std::path::Path::new(&created).is_dir());

    // 第二次调用（目录已存在）不报错。
    assert_eq!(
        get_default_session_dir("/home/u/proj", Some(&agent)).unwrap(),
        expected
    );
}

/// TS: "uses agentDir for the default persisted session path"（`createAgentSession` 的
/// `agentDir` 最终经 `ENV_AGENT_DIR` / `getAgentDir()` 进入 `SessionManager.create`）。
#[test]
fn create_uses_the_env_agent_dir_for_the_default_session_dir() {
    use kiana_session_core::session_manager::SessionManager;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    let _guard = env_lock();
    let temp = temp_dir("kiana-session-dir-env-");
    let agent = format!("{}/agent", temp.path().to_string_lossy());
    let previous = std::env::var(kiana_core::config::ENV_AGENT_DIR).ok();
    // SAFETY: 本测试二进制中只有该用例读取该变量，且由 `env_lock` 串行化。
    unsafe {
        std::env::set_var(kiana_core::config::ENV_AGENT_DIR, &agent);
    }

    let session = SessionManager::create("/home/u/proj", None, None).unwrap();
    let expected_dir = format!("{agent}/sessions/--home-u-proj--");
    assert_eq!(session.get_session_dir(), expected_dir);
    assert!(std::path::Path::new(&expected_dir).is_dir());
    let file = session.get_session_file().unwrap().to_string();
    assert!(
        file.starts_with(&format!("{expected_dir}/")),
        "unexpected session file {file}"
    );

    // SAFETY: 与 set 相同。
    unsafe {
        match previous {
            Some(value) => std::env::set_var(kiana_core::config::ENV_AGENT_DIR, value),
            None => std::env::remove_var(kiana_core::config::ENV_AGENT_DIR),
        }
    }
}
