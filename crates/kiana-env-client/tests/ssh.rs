//! 对应 `packages/env/test/ssh.test.ts`：本机临时 `sshd` 上的 8 个用例。
//!
//! 与 TS 的差异：
//! - 整套 `describe` 在 `sshd` 不可用时跳过（TS 的 `describe.skipIf(sshd === undefined)`）；Rust
//!   没有共享的 `beforeAll`，这里由 [`setup`] 建一次夹具，然后按 TS 的顺序逐个运行用例（每个
//!   用例 60 s 超时），失败消息带上用例名。
//! - `freePort` 用 `TcpListener::bind("127.0.0.1:0")`；`waitForPort` 为 100×50 ms。
//! - `getOrThrow` 用 [`kiana_durable::env::get_or_throw`]（`Err` 时把原错误作为 panic payload）。
//!
//! TS: packages/env/test/ssh.test.ts

#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_chord::context::Context;
use kiana_durable::env::{
    ExecutionError, ExecutionErrorCode, FileContent, FileKind, FileListEntry, FileSystem,
    ListFilesOptions, Shell, ShellCommand, ShellExecOptions, ShellOutputInfo,
};
use kiana_env_client::ssh::{ConnectedSsh, SshFailure};
use kiana_env_client::{
    Connection, HostKeyChangedError, LocalExecutionEnv, LocalExecutionEnvOptions,
    RemoteExecutionEnv, RemoteExecutionEnvOptions, SshConnectOptions, SshTarget, accept_host_key,
    connect_ssh, forget_host_key, scan_host_key, ssh_connection,
};
use sha2::Digest;

// ---------------------------------------------------------------------------
// 环境探测
// ---------------------------------------------------------------------------

fn which(program: &str) -> Option<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

fn sshd_path() -> Option<String> {
    which("sshd").or_else(|| {
        if Path::new("/usr/sbin/sshd").exists() {
            Some("/usr/sbin/sshd".to_owned())
        } else {
            None
        }
    })
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("the bound address").port()
}

async fn wait_for_port(port: u16) {
    for _attempt in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("sshd did not start");
}

fn username() -> String {
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
    {
        return user;
    }
    let output = Command::new("id")
        .arg("-un")
        .output()
        .expect("`id -un` runs");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn keygen(file: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(file)
        .status()
        .expect("ssh-keygen runs");
    assert!(status.success(), "ssh-keygen for {}", file.display());
}

/// 部署的 daemon 以内容命名。
fn deployed_name(daemon: &str) -> String {
    let bytes = std::fs::read(daemon).expect("the daemon binary is readable");
    format!(
        "kiana-env-{}",
        &hex::encode(sha2::Sha256::digest(&bytes))[..32]
    )
}

// ---------------------------------------------------------------------------
// 夹具：本机一次性 sshd（自己的 host key、客户端 key 与 home）
// ---------------------------------------------------------------------------

struct Fixture {
    root: tempfile::TempDir,
    home: String,
    target: SshTarget,
    host_key_pub: String,
    daemon: String,
    server: Child,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

async fn setup() -> Option<Fixture> {
    let sshd = sshd_path()?;
    let daemon = common::daemon_path();
    which("ssh-keygen")?;
    let root = tempfile::Builder::new()
        .prefix("kiana-env-ssh-")
        .tempdir()
        .expect("create a temporary directory");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    keygen(&root.path().join("host_key"));
    keygen(&root.path().join("client_key"));
    std::fs::copy(
        root.path().join("client_key.pub"),
        root.path().join("authorized_keys"),
    )
    .expect("authorized_keys");
    let port = free_port();
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nStrictModes no\nSetEnv HOME={}\n",
        root.path().join("host_key").display(),
        root.path().join("authorized_keys").display(),
        root.path().join("sshd.pid").display(),
        home.display(),
    );
    std::fs::write(root.path().join("sshd_config"), config).expect("sshd_config");
    std::fs::write(root.path().join("ssh_config"), "").expect("ssh_config");
    let server = Command::new(&sshd)
        .arg("-D")
        .arg("-e")
        .arg("-f")
        .arg(root.path().join("sshd_config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start sshd");
    wait_for_port(port).await;
    let target = SshTarget {
        host: "127.0.0.1".to_owned(),
        user: Some(username()),
        port: Some(u32::from(port)),
        identity_file: Some(
            root.path()
                .join("client_key")
                .to_string_lossy()
                .into_owned(),
        ),
        known_hosts_file: root
            .path()
            .join("known_hosts")
            .to_string_lossy()
            .into_owned(),
        host_key_alias: "kiana-env-test".to_owned(),
        ssh: None,
        config_file: Some(
            root.path()
                .join("ssh_config")
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let host_key_pub = root
        .path()
        .join("host_key.pub")
        .to_string_lossy()
        .into_owned();
    Some(Fixture {
        root,
        home: home.to_string_lossy().into_owned(),
        target,
        host_key_pub,
        daemon,
        server,
    })
}

fn connect_options(fixture: &Fixture, binary: &str) -> SshConnectOptions {
    let mut options = SshConnectOptions::new(fixture.target.clone());
    options.binary = Some(binary.to_owned());
    options
}

async fn connect(fixture: &Fixture) -> ConnectedSsh {
    match connect_ssh(connect_options(fixture, &fixture.daemon)).await {
        Ok(connected) => connected,
        Err(error) => panic!("connectSsh failed: {error}"),
    }
}

async fn exec_capture(
    env: &RemoteExecutionEnv,
    argv: Vec<String>,
) -> Result<(i32, String), ExecutionError> {
    let output = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&output);
    let options = ShellExecOptions {
        on_output: Some(Arc::new(
            move |text: &str, _context: &dyn Context, _info: &ShellOutputInfo| {
                sink.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push_str(text);
            },
        )),
        ..ShellExecOptions::default()
    };
    let result = env
        .exec(ShellCommand::Argv(argv), Some(&options), common::context())
        .await?;
    let text = output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok((result.exit_code, text))
}

async fn case<F>(name: &str, run: F)
where
    F: std::future::Future<Output = ()>,
{
    match tokio::time::timeout(Duration::from_secs(60), common::titled(name, run)).await {
        Ok(()) => {}
        Err(_) => panic!("SSH bootstrap case `{name}` timed out after 60s"),
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// `refuses an untrusted host, then deploys and runs the daemon once its key is accepted`
async fn refuses_an_untrusted_host_then_deploys_and_runs_the_daemon_once_its_key_is_accepted(
    fixture: &Fixture,
) {
    let name =
        "refuses an untrusted host, then deploys and runs the daemon once its key is accepted";
    match connect_ssh(connect_options(fixture, &fixture.daemon)).await {
        Err(SshFailure::HostKeyUnknown(_)) => {}
        Err(error) => panic!("{name}: expected HostKeyUnknownError, got {error}"),
        Ok(_) => panic!("{name}: an untrusted host must be refused"),
    }
    let (lines, fingerprints) = match scan_host_key(&fixture.target).await {
        Ok(scanned) => scanned,
        Err(error) => panic!("{name}: scanHostKey failed: {error}"),
    };
    let listed = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(&fixture.host_key_pub)
        .output()
        .expect("ssh-keygen -lf runs");
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let expected = listed
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    assert!(
        fingerprints.join("\n").contains(&expected),
        "{name}: the fingerprints contain {expected}"
    );
    if let Err(error) = accept_host_key(&fixture.target, &lines).await {
        panic!("{name}: acceptHostKey failed: {error}");
    }
    let tools = Path::new(&fixture.home).join(".kiana/mobile/tools");
    std::fs::create_dir_all(&tools).expect("mkdir tools");
    std::fs::write(
        tools.join("kiana-env-0123456789abcdef0123456789abcdef"),
        "old",
    )
    .expect("write an old daemon");
    let connected = connect(fixture).await;
    let connection = Arc::clone(&connected.connection);
    assert_eq!(connected.remote.home, fixture.home, "{name}: remote.home");
    let deployed = tools.join(deployed_name(&fixture.daemon));
    assert_eq!(
        std::fs::read(&deployed).expect("the deployed daemon"),
        std::fs::read(&fixture.daemon).expect("the daemon"),
        "{name}: the deployed bytes"
    );
    let mut names: Vec<String> = std::fs::read_dir(&tools)
        .expect("list the tools directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("kiana-env-"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![deployed_name(&fixture.daemon)],
        "{name}: other daemons are removed"
    );
    let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(&connection),
        id: "kiana-env:test".to_owned(),
        cwd: fixture.home.clone(),
        shell_path: None,
        shell_env: None,
        watch: None,
    });
    if let Err(error) = env
        .write_file(
            "over-ssh.txt",
            FileContent::Text("hello".to_owned()),
            common::context(),
        )
        .await
    {
        panic!("{name}: writeFile failed: {}", error.message);
    }
    match env.read_text_file("over-ssh.txt", common::context()).await {
        Ok(text) => assert_eq!(text, "hello", "{name}: readTextFile"),
        Err(error) => panic!("{name}: readTextFile failed: {}", error.message),
    }
    let (code, output) = match exec_capture(
        &env,
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf \"%s\" \"$HOME\"".to_owned(),
        ],
    )
    .await
    {
        Ok(result) => result,
        Err(error) => panic!("{name}: exec failed: {}", error.message),
    };
    assert_eq!(code, 0, "{name}: the exit code");
    assert_eq!(output, fixture.home, "{name}: $HOME");
    connection.close();
}

/// `reuses a verified daemon and replaces a tampered one`
async fn reuses_a_verified_daemon_and_replaces_a_tampered_one(fixture: &Fixture) {
    let name = "reuses a verified daemon and replaces a tampered one";
    let file = Path::new(&fixture.home)
        .join(".kiana/mobile/tools")
        .join(deployed_name(&fixture.daemon));
    let before = std::fs::metadata(&file)
        .expect("the deployed daemon")
        .modified()
        .expect("its mtime");
    connect(fixture).await.connection.close();
    let after = std::fs::metadata(&file)
        .expect("the deployed daemon")
        .modified()
        .expect("its mtime");
    assert_eq!(after, before, "{name}: a verified daemon is reused");

    std::fs::write(&file, "tampered").expect("tamper with the daemon");
    let connected = connect(fixture).await;
    let connection = Arc::clone(&connected.connection);
    assert_eq!(
        std::fs::read(&file).expect("the deployed daemon"),
        std::fs::read(&fixture.daemon).expect("the daemon"),
        "{name}: the tampered daemon is replaced"
    );
    match connection.info().await {
        Ok(info) => assert_eq!(info.home, fixture.home, "{name}: info.home"),
        Err(error) => panic!("{name}: info failed: {error}"),
    }
    connection.close();
}

/// `verifies the daemon again before starting it after a lost connection`
async fn verifies_the_daemon_again_before_starting_it_after_a_lost_connection(fixture: &Fixture) {
    let name = "verifies the daemon again before starting it after a lost connection";
    let connected = connect(fixture).await;
    let connection = Arc::clone(&connected.connection);
    let file = Path::new(&fixture.home)
        .join(".kiana/mobile/tools")
        .join(deployed_name(&fixture.daemon));
    let pid = match connection.info().await {
        Ok(info) => info.pid,
        Err(error) => panic!("{name}: info failed: {error}"),
    };
    let status = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill -9 runs");
    assert!(status.success(), "{name}: kill -9");
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Linux refuses to write a running program's file; the daemon is dead by now.
    std::fs::write(&file, "tampered").expect("tamper with the daemon");
    let info = match connection.info().await {
        Ok(info) => info,
        Err(error) => panic!("{name}: the restarted daemon failed: {error}"),
    };
    assert_ne!(info.pid, pid, "{name}: a new daemon runs");
    assert_eq!(
        std::fs::read(&file).expect("the deployed daemon"),
        std::fs::read(&fixture.daemon).expect("the daemon"),
        "{name}: the verified binary is redeployed"
    );
    connection.close();
}

/// `lists files over SSH as the local environment lists them`
async fn lists_files_over_ssh_as_the_local_environment_lists_them(fixture: &Fixture) {
    let name = "lists files over SSH as the local environment lists them";
    let tree = Path::new(&fixture.home).join("tree");
    for file in [
        "a/b.txt",
        "a/c/d.txt",
        ".hidden",
        "out/x.js",
        "skip/y.txt",
        ".git/HEAD",
    ] {
        let path = tree.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, "").expect("write a file");
    }
    std::fs::create_dir_all(tree.join("empty")).expect("mkdir empty");
    std::fs::write(tree.join(".gitignore"), "out/\n").expect(".gitignore");
    let connected = connect(fixture).await;
    let connection = Arc::clone(&connected.connection);
    let local_connection = Arc::new(Connection::new(vec![fixture.daemon.clone()]));
    let options = ListFilesOptions {
        exclude: Some(vec!["skip".to_owned()]),
        ..ListFilesOptions::default()
    };
    let remote = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(&connection),
        id: "kiana-env:test".to_owned(),
        cwd: fixture.home.clone(),
        shell_path: None,
        shell_env: None,
        watch: None,
    });
    let listing = match remote
        .list_files("tree", Some(&options), common::context())
        .await
    {
        Ok(listing) => listing,
        Err(error) => panic!("{name}: listFiles failed: {}", error.message),
    };
    let expected: Vec<FileListEntry> = [
        (".gitignore", FileKind::File),
        (".hidden", FileKind::File),
        ("a", FileKind::Directory),
        ("a/b.txt", FileKind::File),
        ("a/c", FileKind::Directory),
        ("a/c/d.txt", FileKind::File),
        ("empty", FileKind::Directory),
    ]
    .into_iter()
    .map(|(path, kind)| FileListEntry {
        path: path.to_owned(),
        kind,
    })
    .collect();
    assert_eq!(listing.entries, expected, "{name}: the remote listing");
    assert!(!listing.truncated, "{name}: the listing is complete");
    let local = LocalExecutionEnv::new(LocalExecutionEnvOptions {
        connection: Arc::clone(&local_connection),
        cwd: fixture.home.clone(),
        shell_path: None,
        shell_env: None,
        watch: None,
    });
    match local
        .list_files("tree", Some(&options), common::context())
        .await
    {
        Ok(listing) => assert_eq!(listing.entries, expected, "{name}: the local listing"),
        Err(error) => panic!("{name}: the local listFiles failed: {}", error.message),
    }
    connection.close();
    local_connection.close();
}

/// `starts the daemon through the login shell only when asked`
async fn starts_the_daemon_through_the_login_shell_only_when_asked(fixture: &Fixture) {
    let name = "starts the daemon through the login shell only when asked";
    // The login shell is the account's shell: sh and bash read .profile, zsh (the macOS default)
    // reads .zprofile.
    std::fs::write(
        Path::new(&fixture.home).join(".profile"),
        "export KIANA_ENV_LOGIN=yes\n",
    )
    .expect(".profile");
    std::fs::write(
        Path::new(&fixture.home).join(".zprofile"),
        "export KIANA_ENV_LOGIN=yes\n",
    )
    .expect(".zprofile");
    let login = |login_shell: bool| {
        let target = fixture.target.clone();
        let daemon = fixture.daemon.clone();
        let home = fixture.home.clone();
        async move {
            let mut options = SshConnectOptions::new(target);
            options.binary = Some(daemon);
            options.login_shell = Some(login_shell);
            let connected = match connect_ssh(options).await {
                Ok(connected) => connected,
                Err(error) => panic!("connectSsh failed: {error}"),
            };
            let connection = Arc::clone(&connected.connection);
            let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
                connection: Arc::clone(&connection),
                id: "kiana-env:test".to_owned(),
                cwd: home,
                shell_path: None,
                shell_env: None,
                watch: None,
            });
            let output = match exec_capture(
                &env,
                vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "printf \"%s\" \"$KIANA_ENV_LOGIN\"".to_owned(),
                ],
            )
            .await
            {
                Ok((_code, output)) => output,
                Err(error) => panic!("exec failed: {}", error.message),
            };
            connection.close();
            output
        }
    };
    assert_eq!(login(false).await, "", "{name}: without the login shell");
    assert_eq!(login(true).await, "yes", "{name}: with the login shell");
}

/// `connects lazily and reports a failed start as the error of the operation`
async fn connects_lazily_and_reports_a_failed_start_as_the_error_of_the_operation(
    fixture: &Fixture,
) {
    let name = "connects lazily and reports a failed start as the error of the operation";
    let lazy_target = SshTarget {
        known_hosts_file: fixture
            .root
            .path()
            .join("lazy_known_hosts")
            .to_string_lossy()
            .into_owned(),
        ..fixture.target.clone()
    };
    let mut options = SshConnectOptions::new(lazy_target.clone());
    options.binary = Some(fixture.daemon.clone());
    let ssh = ssh_connection(options);
    assert!(
        ssh.remote().is_none(),
        "{name}: nothing is known before the first start"
    );
    let connection = Arc::clone(&ssh.connection);
    let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(&connection),
        id: "kiana-env:test".to_owned(),
        cwd: fixture.home.clone(),
        shell_path: None,
        shell_env: None,
        watch: None,
    });
    // Nothing is trusted yet: each operation fails with the reason and tries again.
    match env.read_text_file("missing.txt", common::context()).await {
        Ok(_) => panic!("{name}: an untrusted host must fail"),
        Err(error) => assert!(
            error.message.contains("not trusted"),
            "{name}: the failure names the host key: {}",
            error.message
        ),
    }
    match env
        .exec(
            ShellCommand::Argv(vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()]),
            None,
            common::context(),
        )
        .await
    {
        Ok(result) => panic!(
            "{name}: an untrusted host must fail, got exit {}",
            result.exit_code
        ),
        Err(error) => assert_eq!(
            error.code,
            ExecutionErrorCode::SpawnError,
            "{name}: {}",
            error.message
        ),
    }
    let (lines, _fingerprints) = match scan_host_key(&lazy_target).await {
        Ok(scanned) => scanned,
        Err(error) => panic!("{name}: scanHostKey failed: {error}"),
    };
    if let Err(error) = accept_host_key(&lazy_target, &lines).await {
        panic!("{name}: acceptHostKey failed: {error}");
    }
    match env
        .exec(
            ShellCommand::Argv(vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()]),
            None,
            common::context(),
        )
        .await
    {
        Ok(result) => assert_eq!(result.exit_code, 0, "{name}: the accepted host runs"),
        Err(error) => panic!("{name}: the accepted host must run: {}", error.message),
    }
    let remote = ssh
        .remote()
        .unwrap_or_else(|| panic!("{name}: remote() is known"));
    assert_eq!(remote.home, fixture.home, "{name}: remote().home");
    connection.close();
}

fn public_key(root: &Path, name: &str) -> String {
    let file = root.join(name);
    keygen(&file);
    let text = std::fs::read_to_string(root.join(format!("{name}.pub"))).expect("the public key");
    text.split_whitespace()
        .take(2)
        .collect::<Vec<&str>>()
        .join(" ")
}

/// `accepts only host keys for the alias and never replaces a trusted key silently`
async fn accepts_only_host_keys_for_the_alias_and_never_replaces_a_trusted_key_silently(
    fixture: &Fixture,
) {
    let name = "accepts only host keys for the alias and never replaces a trusted key silently";
    let known_hosts = fixture
        .root
        .path()
        .join("accept_known_hosts")
        .to_string_lossy()
        .into_owned();
    let scoped = SshTarget {
        known_hosts_file: known_hosts.clone(),
        ..fixture.target.clone()
    };
    let first = public_key(fixture.root.path(), "accept_first");
    let second = public_key(fixture.root.path(), "accept_second");
    if accept_host_key(&scoped, &[format!("other-alias {first}")])
        .await
        .is_ok()
    {
        panic!("{name}: a key for another alias must be refused");
    }
    if accept_host_key(
        &scoped,
        &[format!("@cert-authority kiana-env-test {first}")],
    )
    .await
    .is_ok()
    {
        panic!("{name}: a certificate authority line must be refused");
    }
    let accepted_line = vec![format!("kiana-env-test {first}")];
    let (a, b) = tokio::join!(
        accept_host_key(&scoped, &accepted_line),
        accept_host_key(&scoped, &accepted_line),
    );
    if let Err(error) = a {
        panic!("{name}: the first concurrent accept failed: {error}");
    }
    if let Err(error) = b {
        panic!("{name}: the second concurrent accept failed: {error}");
    }
    assert_eq!(
        std::fs::read_to_string(&known_hosts).expect("known_hosts"),
        format!("kiana-env-test {first}\n"),
        "{name}: the accepted key is written once"
    );
    match accept_host_key(&scoped, &[format!("kiana-env-test {second}")]).await {
        Err(SshFailure::HostKeyChanged(HostKeyChangedError { .. })) => {}
        Err(error) => panic!("{name}: expected HostKeyChangedError, got {error}"),
        Ok(()) => panic!("{name}: a different trusted key must not be replaced"),
    }
    if let Err(error) = forget_host_key(&scoped).await {
        panic!("{name}: forgetHostKey failed: {error}");
    }
    if let Err(error) = accept_host_key(&scoped, &[format!("kiana-env-test {second}")]).await {
        panic!("{name}: accepting a new key after forget failed: {error}");
    }
    assert_eq!(
        std::fs::read_to_string(&known_hosts).expect("known_hosts"),
        format!("kiana-env-test {second}\n"),
        "{name}: the new key replaces the forgotten one"
    );
}

/// `refuses a changed host key`
async fn refuses_a_changed_host_key(fixture: &Fixture) {
    let name = "refuses a changed host key";
    let other = public_key(fixture.root.path(), "other_key");
    let known_hosts = fixture
        .root
        .path()
        .join("changed_known_hosts")
        .to_string_lossy()
        .into_owned();
    std::fs::write(&known_hosts, format!("kiana-env-test {other}\n")).expect("known_hosts");
    let target = SshTarget {
        known_hosts_file: known_hosts,
        ..fixture.target.clone()
    };
    let mut options = SshConnectOptions::new(target);
    options.binary = Some(fixture.daemon.clone());
    match connect_ssh(options).await {
        Err(SshFailure::HostKeyChanged(HostKeyChangedError { .. })) => {}
        Err(error) => panic!("{name}: expected HostKeyChangedError, got {error}"),
        Ok(_) => panic!("{name}: a changed host key must be refused"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_bootstrap() {
    let Some(fixture) = setup().await else {
        eprintln!("skipping: sshd is not installed");
        return;
    };
    case(
        "refuses an untrusted host, then deploys and runs the daemon once its key is accepted",
        refuses_an_untrusted_host_then_deploys_and_runs_the_daemon_once_its_key_is_accepted(
            &fixture,
        ),
    )
    .await;
    case(
        "reuses a verified daemon and replaces a tampered one",
        reuses_a_verified_daemon_and_replaces_a_tampered_one(&fixture),
    )
    .await;
    case(
        "verifies the daemon again before starting it after a lost connection",
        verifies_the_daemon_again_before_starting_it_after_a_lost_connection(&fixture),
    )
    .await;
    case(
        "lists files over SSH as the local environment lists them",
        lists_files_over_ssh_as_the_local_environment_lists_them(&fixture),
    )
    .await;
    case(
        "starts the daemon through the login shell only when asked",
        starts_the_daemon_through_the_login_shell_only_when_asked(&fixture),
    )
    .await;
    case(
        "connects lazily and reports a failed start as the error of the operation",
        connects_lazily_and_reports_a_failed_start_as_the_error_of_the_operation(&fixture),
    )
    .await;
    case(
        "accepts only host keys for the alias and never replaces a trusted key silently",
        accepts_only_host_keys_for_the_alias_and_never_replaces_a_trusted_key_silently(&fixture),
    )
    .await;
    case(
        "refuses a changed host key",
        refuses_a_changed_host_key(&fixture),
    )
    .await;
}
