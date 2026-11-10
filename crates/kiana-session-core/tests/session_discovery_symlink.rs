//! TS: `test/suite/regressions/7497-session-discovery-symlink.test.ts`（3 例）。

mod common;

use std::sync::{Mutex, MutexGuard, OnceLock};

use common::*;
use kiana_session_core::session_manager::{FileEntry, SessionManager, entry_json_string};

/// 这些用例通过 `KIANA_CODING_AGENT_DIR` 全局环境变量切换 sessions 目录；同一测试二进制的用例
/// 并行执行，必须串行化。
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

struct DiscoveryFixture {
    _temp: tempfile::TempDir,
    temp_dir: String,
    sessions_dir: String,
}

impl DiscoveryFixture {
    fn new() -> Self {
        let temp = temp_dir("kiana-session-discovery-");
        let temp_dir = temp.path().to_string_lossy().into_owned();
        let agent_dir = format!("{temp_dir}/agent");
        let sessions_dir = format!("{agent_dir}/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        // SAFETY: 本测试二进制的这些用例由 `env_lock` 串行化，且运行期没有其它线程读取该变量。
        unsafe {
            std::env::set_var(kiana_core::config::ENV_AGENT_DIR, &agent_dir);
        }
        Self {
            _temp: temp,
            temp_dir,
            sessions_dir,
        }
    }

    fn write_session(&self, dir: &str, id: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let header = session_header(id, Some(&format!("{}/project", self.temp_dir)), None);
        std::fs::write(
            format!("{dir}/{id}.jsonl"),
            format!("{}\n", entry_json_string(&FileEntry::Header(header))),
        )
        .unwrap();
    }
}

impl Drop for DiscoveryFixture {
    fn drop(&mut self) {
        // SAFETY: 与 `new` 相同，`env_lock` 保证没有并发访问。
        unsafe {
            std::env::remove_var(kiana_core::config::ENV_AGENT_DIR);
        }
    }
}

/// TS: "discovers a session through a directory link and preserves the alias path"
#[test]
fn discovers_a_session_through_a_directory_link_and_preserves_the_alias_path() {
    let _guard = env_lock();
    let fixture = DiscoveryFixture::new();
    let target_dir = format!("{}/linked-sessions", fixture.temp_dir);
    fixture.write_session(&target_dir, "linked");
    let alias_dir = format!("{}/--linked--", fixture.sessions_dir);
    std::os::unix::fs::symlink(&target_dir, &alias_dir).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sessions = runtime
        .block_on(SessionManager::list_all(None, None))
        .unwrap();

    let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, ["linked"]);
    assert_eq!(sessions[0].path, format!("{alias_dir}/linked.jsonl"));
}

/// TS: "ignores a broken directory link without hiding valid sessions"
#[test]
fn ignores_a_broken_directory_link_without_hiding_valid_sessions() {
    let _guard = env_lock();
    let fixture = DiscoveryFixture::new();
    fixture.write_session(&format!("{}/--regular--", fixture.sessions_dir), "regular");
    let target_dir = format!("{}/removed-sessions", fixture.temp_dir);
    std::fs::create_dir_all(&target_dir).unwrap();
    std::os::unix::fs::symlink(&target_dir, format!("{}/--broken--", fixture.sessions_dir))
        .unwrap();
    std::fs::remove_dir_all(&target_dir).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sessions = runtime
        .block_on(SessionManager::list_all(None, None))
        .unwrap();

    let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, ["regular"]);
}

/// TS: "ignores links to files"
#[test]
fn ignores_links_to_files() {
    let _guard = env_lock();
    let fixture = DiscoveryFixture::new();
    fixture.write_session(&format!("{}/--regular--", fixture.sessions_dir), "regular");
    let target_file = format!("{}/not-a-directory", fixture.temp_dir);
    std::fs::write(&target_file, "").unwrap();
    std::os::unix::fs::symlink(&target_file, format!("{}/--file--", fixture.sessions_dir)).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sessions = runtime
        .block_on(SessionManager::list_all(None, None))
        .unwrap();

    let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, ["regular"]);
}
