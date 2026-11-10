//! `session-share.test.ts` 的移植。
//!
//! TS 用 vitest mock `node:child_process`；Rust 版在 PATH 前面放一个假的 `gh` 脚本
//! （同样的命令、同样的参数、同样的 stdin/文件交互），从而保留真实的 `tokio::process`
//! 调用路径。环境变量是进程级的，因此本文件的测试用 [`ENV_LOCK`] 串行化。

use std::cell::RefCell;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kiana_interactive::session_share::{SessionShareContext, share_session};
use kiana_theme::init_theme;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Tui;
use serde_json::json;

use crate::common::{
    FakeTui, FauxSessionOptions, faux_session, lock, run_local, shared, user_msg, yield_tasks,
};

mod common;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

/// 假 `gh`：`auth status` 成功；`gist create` 记录参数与上传文件并回显递增的 gist URL。
struct FakeGh {
    /// 每次 `gist create` 追加写入的上传日志。
    log: PathBuf,
    /// 每次 `gist create` 的完整参数（每个参数一行，调用之间以 `---8<---` 分隔）。
    args_log: PathBuf,
    previous_path: Option<std::ffi::OsString>,
    previous_api_base: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for FakeGh {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
            match &self.previous_api_base {
                Some(value) => std::env::set_var("KIANA_API_BASE_URL", value),
                None => std::env::remove_var("KIANA_API_BASE_URL"),
            }
        }
    }
}

impl FakeGh {
    fn install() -> Self {
        Self::install_with(false)
    }

    /// `gh auth status` 成功后删除自身：下一次 `spawn("gh", …)` 得到 ENOENT。
    fn install_removing_itself_after_auth_status() -> Self {
        Self::install_with(true)
    }

    fn install_with(remove_after_auth_status: bool) -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("fake gh dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let log = dir.path().join("uploads.log");
        let args_log = dir.path().join("args.log");
        let counter = dir.path().join("counter");
        let remove = if remove_after_auth_status {
            "  rm -f \"$0\"\n"
        } else {
            ""
        };
        // 删除模式把进程 PATH 限制为假 gh 目录（脚本内部自带系统 PATH），这样删除后
        // 不存在真实的 `gh`，下一次 spawn 必然 ENOENT。
        let path_prologue = if remove_after_auth_status {
            "export PATH=/usr/bin:/bin\n"
        } else {
            ""
        };
        let script = format!(
            r#"#!/bin/sh
{path_prologue}if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
{remove}  exit 0
fi
if [ "$1" = "gist" ]; then
  printf '%s\n' "$@" >> "{args_log}"
  printf -- '---8<---\n' >> "{args_log}"
  for last in "$@"; do :; done
  sleep 0.1
  cat "$last" >> "{log}"
  printf '\n---8<---\n' >> "{log}"
  count=$(cat "{counter}" 2>/dev/null || echo 0)
  count=$((count + 1))
  echo "$count" > "{counter}"
  echo "https://gist.github.com/test/gist-$count"
  exit 0
fi
exit 1
"#,
            remove = remove,
            path_prologue = path_prologue,
            log = log.display(),
            args_log = args_log.display(),
            counter = counter.display(),
        );
        let gh = bin.join("gh");
        {
            let mut file = std::fs::File::create(&gh).expect("write fake gh");
            file.write_all(script.as_bytes()).expect("script");
        }
        let mut permissions = std::fs::metadata(&gh).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).expect("chmod");

        let previous_path = std::env::var_os("PATH");
        let new_path = if remove_after_auth_status {
            bin.into_os_string()
        } else {
            match &previous_path {
                Some(path) => {
                    let mut value = bin.into_os_string();
                    value.push(":");
                    value.push(path);
                    value
                }
                None => bin.into_os_string(),
            }
        };
        unsafe { std::env::set_var("PATH", new_path) };
        // 测试覆盖旧默认值：分享查看器由 `KIANA_API_BASE_URL` 派生。
        let previous_api_base = std::env::var_os("KIANA_API_BASE_URL");
        unsafe { std::env::set_var("KIANA_API_BASE_URL", "https://kiana.dev") };
        FakeGh {
            log,
            args_log,
            previous_path,
            previous_api_base,
            _dir: dir,
            _guard: guard,
        }
    }

    fn uploads(&self) -> Vec<String> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.split("\n---8<---\n")
            .map(str::to_string)
            .filter(|part| !part.is_empty())
            .collect()
    }

    /// 每次 `gist create` 的参数列表（按调用顺序）。
    fn gist_args(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.args_log)
            .unwrap_or_default()
            .split("---8<---\n")
            .filter(|part| !part.is_empty())
            .map(|part| {
                part.trim_end_matches('\n')
                    .split('\n')
                    .map(str::to_string)
                    .collect()
            })
            .collect()
    }
}

fn settings(destination: &str) -> kiana_config::settings::schema::Settings {
    serde_json::from_value(json!({ "sessionShareDestination": destination })).expect("settings")
}

/// 取出 HTML 里 `<script id="session-data">` 的 base64 内容。
fn session_data(html: &str) -> &str {
    let Some(index) = html.find("id=\"session-data\"") else {
        return "";
    };
    let rest = &html[index..];
    let Some(open) = rest.find('>') else {
        return "";
    };
    let start = index + open + 1;
    let Some(end) = html[start..].find("</script>") else {
        return "";
    };
    html[start..start + end].trim()
}

struct Harness {
    statuses: Rc<RefCell<Vec<String>>>,
    errors: Rc<RefCell<Vec<String>>>,
    context: SessionShareContext,
}

fn harness(
    session: kiana_session::agent_session::AgentSession,
    editor: kiana_tui::layout_node::SharedComponent,
) -> Harness {
    let statuses: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let errors: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let ui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(FakeTui::default()));
    let editor_container = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    let context = SessionShareContext {
        session,
        ui,
        editor_container,
        editor,
        show_status: {
            let statuses = Rc::clone(&statuses);
            Rc::new(move |message: &str| statuses.borrow_mut().push(message.to_string()))
        },
        show_error: {
            let errors = Rc::clone(&errors);
            Rc::new(move |message: &str| errors.borrow_mut().push(message.to_string()))
        },
    };
    Harness {
        statuses,
        errors,
        context,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_concurrent_session_exports_isolated() {
    theme();
    let gh = FakeGh::install();

    run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            persist_session: true,
            ..Default::default()
        })
        .await;
        fixture
            .session
            .with_session_manager_mut(|manager| manager.append_message(user_msg("first export")))
            .expect("append message");

        let editor_a = shared(Text::new("editor a"));
        let a = harness(fixture.session.clone(), editor_a);
        let b_statuses = Rc::new(RefCell::new(Vec::new()));
        let b_errors: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        let share_a = tokio::task::spawn_local(share_session(a.context));
        // 让 A 走到 gist 上传的 await（导出与 HTML 写入已同步完成）。
        yield_tasks(20).await;

        fixture
            .session
            .with_session_manager_mut(|manager| manager.append_message(user_msg("second export")))
            .expect("append message");
        let ui_b: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(FakeTui::default()));
        let editor_container_b = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
        let b = SessionShareContext {
            session: fixture.session.clone(),
            ui: ui_b,
            editor_container: editor_container_b,
            editor: shared(Text::new("editor b")),
            show_status: {
                let statuses = Rc::clone(&b_statuses);
                Rc::new(move |message: &str| statuses.borrow_mut().push(message.to_string()))
            },
            show_error: {
                let errors = Rc::clone(&b_errors);
                Rc::new(move |message: &str| errors.borrow_mut().push(message.to_string()))
            },
        };
        let share_b = tokio::task::spawn_local(share_session(b));
        share_a.await.expect("share a").expect("share a succeeds");
        share_b.await.expect("share b").expect("share b succeeds");

        assert!(
            a.errors.borrow().is_empty(),
            "errors A: {:?}",
            a.errors.borrow()
        );
        assert!(
            b_errors.borrow().is_empty(),
            "errors B: {:?}",
            b_errors.borrow()
        );
        assert_eq!(a.statuses.borrow().len(), 1);
        assert_eq!(b_statuses.borrow().len(), 1);
        assert!(a.statuses.borrow()[0].contains("https://kiana.dev/session/#gist-1"));
        assert!(b_statuses.borrow()[0].contains("https://kiana.dev/session/#gist-2"));

        let uploads = gh.uploads();
        assert_eq!(uploads.len(), 2, "each share uploads its own export");
        // session 数据以 base64 嵌在 HTML 里；比较两个 payload 即可验证"每次分享导出的是
        // 自己那一刻的会话快照"（第二次分享多了一条消息）。
        let payloads: Vec<&str> = uploads.iter().map(|html| session_data(html)).collect();
        assert!(!payloads[0].is_empty() && !payloads[1].is_empty());
        assert_ne!(payloads[0], payloads[1]);
        let (small, large) = if payloads[0].len() < payloads[1].len() {
            (payloads[0], payloads[1])
        } else {
            (payloads[1], payloads[0])
        };
        assert!(
            large.len() > small.len(),
            "the second export contains the appended message"
        );
        assert_eq!(a.statuses.borrow().len(), 1);
        assert_eq!(b_statuses.borrow().len(), 1);
        assert!(a.statuses.borrow()[0].contains("https://kiana.dev/session/#gist-"));
        assert!(b_statuses.borrow()[0].contains("https://kiana.dev/session/#gist-"));
        assert_ne!(a.statuses.borrow()[0], b_statuses.borrow()[0]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn forces_gist_sharing_without_checking_radius_authentication() {
    theme();
    let gh = FakeGh::install();
    let credential_reads: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            settings: settings("gist"),
            persist_session: true,
            credential_reads: Some(std::sync::Arc::clone(&credential_reads)),
            ..Default::default()
        })
        .await;
        fixture
            .session
            .with_session_manager_mut(|manager| manager.append_message(user_msg("hello")))
            .expect("append message");
        // 与 TS 测试同形：Radius provider 存在（认证已配置），但 gist 目标必须完全不碰它。
        assert!(
            fixture.model_runtime.get_provider("radius").is_some(),
            "fixture must expose a radius provider for this regression"
        );
        // 先让创建会话时排队的后台模型刷新跑完（它们会读取全部 provider 的凭证）。
        yield_tasks(200).await;
        lock(&credential_reads).clear();
        yield_tasks(10).await;
        let harness = harness(fixture.session.clone(), shared(Text::new("editor")));
        share_session(harness.context)
            .await
            .expect("share succeeds");

        assert!(
            harness.errors.borrow().is_empty(),
            "errors: {:?}",
            harness.errors.borrow()
        );
        assert_eq!(gh.uploads().len(), 1);
        let statuses = harness.statuses.borrow();
        assert_eq!(statuses.len(), 1);
        assert!(
            statuses[0].contains("https://kiana.dev/session/#gist-1"),
            "status: {}",
            statuses[0]
        );
        assert!(statuses[0].contains("Gist: "));
        // #10870：`getProvider("radius")` / `getAuth("radius")` 都不得发生。
        let reads = lock(&credential_reads);
        assert!(
            !reads.iter().any(|provider| provider == "radius"),
            "radius credentials must not be read, got {reads:?}"
        );
        drop(reads);
        // spawn 的完整参数逐字匹配 `gh gist create --public=false <html>`。
        let calls = gh.gist_args();
        assert_eq!(calls.len(), 1, "calls: {calls:?}");
        assert_eq!(calls[0].len(), 4, "args: {:?}", calls[0]);
        assert_eq!(calls[0][0], "gist");
        assert_eq!(calls[0][1], "create");
        assert_eq!(calls[0][2], "--public=false");
        assert!(
            calls[0][3].ends_with("/session.html"),
            "upload path: {}",
            calls[0][3]
        );
    })
    .await;
}

/// PATH 恢复守卫（panic 时也恢复）。
struct PathGuard(Option<std::ffi::OsString>);

impl Drop for PathGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "PATH 是进程级环境变量：整个用例（含 await）必须独占，避免与 FakeGh 用例竞争"
)]
async fn reports_missing_radius_authentication_when_radius_is_forced() {
    theme();
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // PATH 里没有 `gh`：如果实现错误地走到 gist 分支，错误信息会不同。
    let empty_dir = tempfile::tempdir().expect("empty dir");
    let _path_guard = PathGuard(std::env::var_os("PATH"));
    unsafe { std::env::set_var("PATH", empty_dir.path()) };

    let (errors, statuses) = run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            settings: settings("radius"),
            ..Default::default()
        })
        .await;
        let harness = harness(fixture.session.clone(), shared(Text::new("editor")));
        share_session(harness.context)
            .await
            .expect("share succeeds");
        (
            harness.errors.borrow().clone(),
            harness.statuses.borrow().clone(),
        )
    })
    .await;

    assert_eq!(
        errors,
        vec!["Radius authentication is not configured. Run '/login radius' first.".to_string()]
    );
    assert!(statuses.is_empty(), "statuses: {statuses:?}");
}

/// 回归（review finding）：Node 的 `spawnSync` 对缺失的二进制**不抛异常**，而是返回
/// `{status: null, error: ENOENT}`，因此 TS 走 `authResult.status !== 0` 分支，报告
/// "GitHub CLI is not logged in..."（不是 "not installed"）。
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "PATH 是进程级环境变量：整个用例（含 await）必须独占，避免与 FakeGh 用例竞争"
)]
async fn reports_missing_gh_as_not_logged_in() {
    theme();
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // PATH 里没有 `gh`：`Command::output()` 返回 ENOENT。
    let empty_dir = tempfile::tempdir().expect("empty dir");
    let _path_guard = PathGuard(std::env::var_os("PATH"));
    unsafe { std::env::set_var("PATH", empty_dir.path()) };

    let (errors, statuses) = run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            settings: settings("gist"),
            ..Default::default()
        })
        .await;
        let harness = harness(fixture.session.clone(), shared(Text::new("editor")));
        share_session(harness.context)
            .await
            .expect("share succeeds");
        (
            harness.errors.borrow().clone(),
            harness.statuses.borrow().clone(),
        )
    })
    .await;

    assert_eq!(
        errors,
        vec!["GitHub CLI is not logged in. Run 'gh auth login' first.".to_string()]
    );
    assert!(statuses.is_empty(), "statuses: {statuses:?}");
}

/// 进程级环境变量守卫（panic 时也恢复）。
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// TS 的 `fs.mkdtempSync` 位于 try/finally 之外：TMPDIR 不可用时 `shareSession` reject，
/// 且不会显示 shareSession 自己的 "Failed to export session" 文案。
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "TMPDIR 是进程级环境变量：整个用例必须独占，避免与其它用例竞争"
)]
async fn rejects_when_the_temp_directory_cannot_be_created() {
    theme();
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 先建好夹具（它自己会创建临时目录），再制造不可用的 TMPDIR。
    let fixture = faux_session(FauxSessionOptions {
        settings: settings("gist"),
        persist_session: true,
        ..Default::default()
    })
    .await;
    let parent = tempfile::tempdir().expect("parent");
    let missing = parent.path().join("missing");
    let _tmpdir = EnvGuard::set("TMPDIR", missing.as_os_str());

    let harness = harness(fixture.session.clone(), shared(Text::new("editor")));
    let error = share_session(harness.context)
        .await
        .expect_err("mkdtemp failure rejects shareSession");

    assert_eq!(
        error.message,
        format!(
            "ENOENT: no such file or directory, mkdtemp '{}/kiana-share-XXXXXX'",
            missing.display()
        )
    );
    assert!(
        harness.errors.borrow().is_empty(),
        "errors: {:?}",
        harness.errors.borrow()
    );
    assert!(harness.statuses.borrow().is_empty());
}

/// `gh auth status` 成功后二进制消失：Node 的 spawn 错误文案是 `spawn gh ENOENT`。
#[tokio::test(flavor = "current_thread")]
async fn reports_node_style_spawn_errors_for_the_gist_upload() {
    theme();
    let _gh = FakeGh::install_removing_itself_after_auth_status();

    run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            settings: settings("gist"),
            persist_session: true,
            ..Default::default()
        })
        .await;
        fixture
            .session
            .with_session_manager_mut(|manager| manager.append_message(user_msg("hello")))
            .expect("append message");
        let harness = harness(fixture.session.clone(), shared(Text::new("editor")));
        share_session(harness.context)
            .await
            .expect("share completes");
        assert_eq!(
            harness.errors.borrow().clone(),
            vec!["Failed to create gist: spawn gh ENOENT".to_string()]
        );
        assert!(harness.statuses.borrow().is_empty());
    })
    .await;
}
