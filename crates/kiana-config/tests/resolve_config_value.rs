//! `core/resolve-config-value.ts` 的移植测试。
//!
//! TS 源：`packages/coding-agent/test/resolve-config-value.test.ts`（8 个 `test(...)` 语句，
//! 其中 `test.each(["!exit 1", "!nonexistent-command-12345", "!printf ''"])` 参数化 3 例，
//! 运行时共 10 例）。每个测试上方的 `TS:` 注释保留原测试标题逐字。
//!
//! 测试通过 [`serial`] 串行化：TS 侧 vitest 在同一文件内顺序执行，而 Rust 默认并发；
//! 缓存与 `process.env` 都是进程级状态，并发会让计数/环境断言互相干扰。

use std::fs;
use std::sync::{Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_config::resolve_config_value::{
    ConfigEnv, ConfigValueError, clear_config_value_cache, get_config_value_env_var_name,
    get_config_value_env_var_names, get_missing_config_value_env_var_names,
    is_command_config_value, is_config_value_configured, resolve_config_value,
    resolve_config_value_or_throw, resolve_config_value_uncached,
    resolve_config_value_uncached_with_shell, resolve_headers, resolve_headers_or_throw,
    windows_default_shell_invocation,
};
use kiana_core::shell::{CommandTransport, Platform, ShellConfig};

static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 串行化本文件的全部测试（进程级缓存 + 进程环境变量）。
fn serial() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_env(name: &str, value: &str) {
    // SAFETY: 本文件所有测试都持有 `serial()` 的互斥锁，没有其它线程并发读写环境变量。
    unsafe { std::env::set_var(name, value) };
}

fn remove_env(name: &str) {
    // SAFETY: 同上。
    unsafe { std::env::remove_var(name) };
}

/// 生成 TS 测试里的计数命令（每次执行把计数文件 +1，并输出 value）。
fn counter_command(counter: &std::path::Path, exit_code: u8) -> String {
    let escaped = counter
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    format!(
        "!sh -c 'count=$(cat \"{escaped}\"); echo $((count + 1)) > \"{escaped}\"; {}'",
        if exit_code == 0 {
            "echo value".to_string()
        } else {
            format!("exit {exit_code}")
        }
    )
}

/// TS: "resolves literals, environment templates, and escapes"。
#[test]
fn resolves_literals_environment_templates_and_escapes() {
    let _serial = serial();
    clear_config_value_cache();

    set_env("TEST_CONFIG_LEFT", "left");
    set_env("TEST_CONFIG_RIGHT", "right");
    assert_eq!(
        resolve_config_value("literal-key", None).as_deref(),
        Some("literal-key")
    );
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_LEFT", None).as_deref(),
        Some("left")
    );
    assert_eq!(
        resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("left_right")
    );
    assert_eq!(
        resolve_config_value("$$TEST_CONFIG_LEFT", None).as_deref(),
        Some("$TEST_CONFIG_LEFT")
    );
    assert_eq!(
        resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("!literal-right")
    );

    remove_env("TEST_CONFIG_LEFT");
    remove_env("TEST_CONFIG_RIGHT");
    clear_config_value_cache();
}

/// TS: "uses credential-scoped environment before process.env"。
#[test]
fn uses_credential_scoped_environment_before_process_env() {
    let _serial = serial();
    clear_config_value_cache();

    set_env("TEST_CONFIG_SCOPED", "process");
    let mut env = ConfigEnv::new();
    env.insert("TEST_CONFIG_SCOPED".to_string(), "credential".to_string());
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_SCOPED", Some(&env)).as_deref(),
        Some("credential")
    );

    remove_env("TEST_CONFIG_SCOPED");
    clear_config_value_cache();
}

/// TS: "executes shell commands and trims their output"。
#[test]
fn executes_shell_commands_and_trims_their_output() {
    let _serial = serial();
    clear_config_value_cache();

    assert_eq!(
        resolve_config_value("!echo '  spaced-key  '", None).as_deref(),
        Some("spaced-key")
    );
    assert_eq!(
        resolve_config_value("!printf 'line1\\nline2'", None).as_deref(),
        Some("line1\nline2")
    );
    assert_eq!(
        resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
        Some("hello-world")
    );

    clear_config_value_cache();
}

/// TS: "returns undefined when command resolution fails: !exit 1"。
#[test]
fn returns_undefined_when_command_resolution_fails_exit_1() {
    let _serial = serial();
    clear_config_value_cache();
    assert_eq!(resolve_config_value("!exit 1", None), None);
    clear_config_value_cache();
}

/// TS: "returns undefined when command resolution fails: !nonexistent-command-12345"。
#[test]
fn returns_undefined_when_command_resolution_fails_nonexistent_command() {
    let _serial = serial();
    clear_config_value_cache();
    assert_eq!(
        resolve_config_value("!nonexistent-command-12345", None),
        None
    );
    clear_config_value_cache();
}

/// TS: "returns undefined when command resolution fails: !printf ''"。
#[test]
fn returns_undefined_when_command_resolution_fails_empty_output() {
    let _serial = serial();
    clear_config_value_cache();
    assert_eq!(resolve_config_value("!printf ''", None), None);
    clear_config_value_cache();
}

/// TS: "caches successful and failed commands until explicitly cleared"。
#[test]
fn caches_successful_and_failed_commands_until_explicitly_cleared() {
    let _serial = serial();
    clear_config_value_cache();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let counter = temp_dir.path().join("counter");
    fs::write(&counter, "0").expect("write counter");

    let success = counter_command(&counter, 0);
    assert_eq!(
        resolve_config_value(&success, None).as_deref(),
        Some("value")
    );
    assert_eq!(
        resolve_config_value(&success, None).as_deref(),
        Some("value")
    );
    assert_eq!(
        fs::read_to_string(&counter).expect("read counter").trim(),
        "1"
    );

    clear_config_value_cache();
    assert_eq!(
        resolve_config_value(&success, None).as_deref(),
        Some("value")
    );
    assert_eq!(
        fs::read_to_string(&counter).expect("read counter").trim(),
        "2"
    );

    let failure = counter_command(&counter, 1);
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(
        fs::read_to_string(&counter).expect("read counter").trim(),
        "3"
    );

    clear_config_value_cache();
}

/// TS: "does not cache environment values"。
#[test]
fn does_not_cache_environment_values() {
    let _serial = serial();
    clear_config_value_cache();

    set_env("TEST_CONFIG_DYNAMIC", "first");
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("first")
    );
    set_env("TEST_CONFIG_DYNAMIC", "second");
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("second")
    );

    remove_env("TEST_CONFIG_DYNAMIC");
    clear_config_value_cache();
}

/// TS: "uncached resolution executes a command on every call"。
#[test]
fn uncached_resolution_executes_a_command_on_every_call() {
    let _serial = serial();
    clear_config_value_cache();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let counter = temp_dir.path().join("uncached-counter");
    fs::write(&counter, "0").expect("write counter");

    let command = counter_command(&counter, 0);
    assert_eq!(
        resolve_config_value_uncached(&command, None).as_deref(),
        Some("value")
    );
    assert_eq!(
        resolve_config_value_uncached(&command, None).as_deref(),
        Some("value")
    );
    assert_eq!(
        fs::read_to_string(&counter).expect("read counter").trim(),
        "2"
    );

    clear_config_value_cache();
}

/// TS: "uses stdin when the configured Windows shell requires it"。
#[test]
fn uses_stdin_when_the_configured_windows_shell_requires_it() {
    if cfg!(windows) {
        // TS 测试在真实 win32 上直接 return。
        return;
    }
    let _serial = serial();
    clear_config_value_cache();

    let shell = if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "bash"
    };
    let shell_config = ShellConfig {
        shell: shell.to_string(),
        args: vec!["-s".to_string()],
        command_transport: Some(CommandTransport::Stdin),
    };
    let expansion = "${name}";
    let command = format!("!name='World'; echo \"Hello, {expansion}!\"");

    assert_eq!(
        resolve_config_value_uncached_with_shell(&command, None, Platform::Win32, Ok(shell_config))
            .as_deref(),
        Some("Hello, World!")
    );

    clear_config_value_cache();
}

// ---------------------------------------------------------------------------
// 以下为补充的规范测试（§2.11.3/§2.11.4 要求逐字一致）。
// ---------------------------------------------------------------------------

#[test]
fn reports_environment_variable_names_in_order() {
    let _serial = serial();
    assert_eq!(
        get_config_value_env_var_name("$FOO"),
        Some("FOO".to_string())
    );
    assert_eq!(
        get_config_value_env_var_name("${FOO}"),
        Some("FOO".to_string())
    );
    assert_eq!(get_config_value_env_var_name("$FOO$BAR"), None);
    assert_eq!(get_config_value_env_var_name("literal"), None);
    assert_eq!(get_config_value_env_var_name("!echo $FOO"), None);
    assert_eq!(
        get_config_value_env_var_names("$B/${A}_$B"),
        vec!["B".to_string(), "A".to_string()]
    );
    assert_eq!(
        get_config_value_env_var_names("!echo $A"),
        Vec::<String>::new()
    );
    assert!(is_command_config_value("!echo hi"));
    assert!(!is_command_config_value("echo hi"));
    assert!(is_config_value_configured("literal", None));
    assert!(is_config_value_configured("!exit 1", None));
}

#[test]
fn parses_template_escapes_and_literal_fallbacks() {
    let _serial = serial();
    clear_config_value_cache();
    assert_eq!(resolve_config_value("a$$b", None).as_deref(), Some("a$b"));
    assert_eq!(resolve_config_value("a$!b", None).as_deref(), Some("a!b"));
    assert_eq!(resolve_config_value("$", None).as_deref(), Some("$"));
    assert_eq!(resolve_config_value("a$", None).as_deref(), Some("a$"));
    assert_eq!(resolve_config_value("${}", None).as_deref(), Some("${}"));
    assert_eq!(
        resolve_config_value("${INVALID-NAME}", None).as_deref(),
        Some("${INVALID-NAME}")
    );
    assert_eq!(
        resolve_config_value("${UNCLOSED", None).as_deref(),
        Some("${UNCLOSED")
    );
    assert_eq!(resolve_config_value("$$$$", None).as_deref(), Some("$$"));
    assert_eq!(resolve_config_value("", None).as_deref(), Some(""));
}

#[test]
fn reports_missing_environment_variables() {
    let _serial = serial();
    clear_config_value_cache();
    remove_env("KIANA_TEST_MISSING_A");
    remove_env("KIANA_TEST_MISSING_B");
    assert_eq!(
        get_missing_config_value_env_var_names("$KIANA_TEST_MISSING_A", None),
        vec!["KIANA_TEST_MISSING_A".to_string()]
    );
    let mut env = ConfigEnv::new();
    env.insert("KIANA_TEST_MISSING_A".to_string(), String::new());
    // 空串视为缺失（TS 的 `||`）。
    assert_eq!(
        get_missing_config_value_env_var_names("$KIANA_TEST_MISSING_A", Some(&env)),
        vec!["KIANA_TEST_MISSING_A".to_string()]
    );
    assert!(!is_config_value_configured("$KIANA_TEST_MISSING_A", None));
}

#[test]
fn gives_verbatim_resolution_errors() {
    let _serial = serial();
    clear_config_value_cache();
    remove_env("KIANA_TEST_MISSING_A");
    remove_env("KIANA_TEST_MISSING_B");

    let error = resolve_config_value_or_throw("!exit 1", "API key for provider \"x\"", None)
        .expect_err("command must fail");
    assert_eq!(
        error.to_string(),
        "Failed to resolve API key for provider \"x\" from shell command: exit 1"
    );

    let error =
        resolve_config_value_or_throw("$KIANA_TEST_MISSING_A", "API key for provider \"x\"", None)
            .expect_err("env var must be missing");
    assert_eq!(
        error.to_string(),
        "Failed to resolve API key for provider \"x\" from environment variable: KIANA_TEST_MISSING_A"
    );

    let error = resolve_config_value_or_throw(
        "${KIANA_TEST_MISSING_A}-${KIANA_TEST_MISSING_B}",
        "provider \"x\" header \"Authorization\"",
        None,
    )
    .expect_err("env vars must be missing");
    assert_eq!(
        error.to_string(),
        "Failed to resolve provider \"x\" header \"Authorization\" from environment variables: KIANA_TEST_MISSING_A, KIANA_TEST_MISSING_B"
    );
    assert_eq!(
        error,
        ConfigValueError::EnvironmentVariables {
            description: "provider \"x\" header \"Authorization\"".to_string(),
            names: vec![
                "KIANA_TEST_MISSING_A".to_string(),
                "KIANA_TEST_MISSING_B".to_string()
            ],
        }
    );

    let error = resolve_config_value_or_throw("$KIANA_TEST_MISSING_A", "API key", None)
        .expect_err("env var must be missing");
    assert!(matches!(
        error,
        ConfigValueError::EnvironmentVariable { .. }
    ));

    assert_eq!(
        resolve_config_value_or_throw("literal", "API key", None).expect("literal resolves"),
        "literal"
    );
    clear_config_value_cache();
}

#[test]
fn resolves_headers_and_drops_empty_values() {
    let _serial = serial();
    clear_config_value_cache();
    set_env("TEST_HEADER_VALUE", "from-env");

    let mut headers = IndexMap::new();
    headers.insert("A".to_string(), "literal".to_string());
    headers.insert("B".to_string(), "!printf ''".to_string());
    headers.insert("C".to_string(), "$TEST_HEADER_VALUE".to_string());
    headers.insert("D".to_string(), "$!escaped".to_string());

    let resolved = resolve_headers(Some(&headers), None).expect("headers resolve");
    // B 的空 stdout 被丢弃；`$!` 是转义（不是命令），D 保留为 "!escaped"。
    assert_eq!(resolved.len(), 3);
    let keys: Vec<&str> = resolved.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["A", "C", "D"]);
    assert_eq!(resolved["A"], "literal");
    assert_eq!(resolved["C"], "from-env");
    assert_eq!(resolved["D"], "!escaped");

    assert_eq!(resolve_headers(None, None), None);
    assert_eq!(resolve_headers(Some(&IndexMap::new()), None), None);

    remove_env("TEST_HEADER_VALUE");
    clear_config_value_cache();
}

#[test]
fn resolves_headers_or_throw_with_header_descriptions() {
    let _serial = serial();
    clear_config_value_cache();
    remove_env("KIANA_TEST_MISSING_HEADER");

    let mut headers = IndexMap::new();
    headers.insert(
        "Authorization".to_string(),
        "$KIANA_TEST_MISSING_HEADER".to_string(),
    );

    let error = resolve_headers_or_throw(Some(&headers), "provider \"x\"", None)
        .expect_err("header env var must be missing");
    assert_eq!(
        error.to_string(),
        "Failed to resolve provider \"x\" header \"Authorization\" from environment variable: KIANA_TEST_MISSING_HEADER"
    );

    let mut resolved_headers = IndexMap::new();
    resolved_headers.insert("X-Empty".to_string(), "literal-empty-check".to_string());
    let resolved = resolve_headers_or_throw(Some(&resolved_headers), "provider \"x\"", None)
        .expect("headers resolve");
    assert_eq!(
        resolved.as_ref().map(|map| map["X-Empty"].as_str()),
        Some("literal-empty-check")
    );

    assert_eq!(
        resolve_headers_or_throw(None, "provider \"x\"", None),
        Ok(None)
    );
    assert_eq!(
        resolve_headers_or_throw(Some(&IndexMap::new()), "provider \"x\"", None),
        Ok(None)
    );

    clear_config_value_cache();
}

/// 由 TS 参考实现生成的黄金向量（`tests/fixtures/resolve_config_value_golden.json`）。
///
/// 生成方式：`node --experimental-strip-types` 直接运行
/// `packages/coding-agent/src/core/resolve-config-value.ts`，对每个输入调用
/// `resolveConfigValue` / `getConfigValueEnvVarName(s)` / `getMissingConfigValueEnvVarNames` /
/// `isCommandConfigValue` / `isConfigValueConfigured`（scoped env 固定为
/// `{A:"1",B:"2",_:"3",LONG_NAME:"long",EMPTY:""}`），JSON 化后逐项比对。
#[test]
fn matches_ts_golden_vectors_for_template_parsing() {
    let _serial = serial();
    clear_config_value_cache();

    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/resolve_config_value_golden.json"))
            .expect("golden fixture");
    let env = golden_env();

    for case in fixture["cases"].as_array().expect("cases") {
        let config = case["c"].as_str().expect("c");
        let expected_value = case["value"].as_str();
        assert_eq!(
            resolve_config_value(config, Some(&env)).as_deref(),
            expected_value,
            "resolveConfigValue({config:?})"
        );
        assert_eq!(
            get_config_value_env_var_name(config).as_deref(),
            case["envName"].as_str(),
            "getConfigValueEnvVarName({config:?})"
        );
        let expected_names: Vec<String> = case["envNames"]
            .as_array()
            .expect("envNames")
            .iter()
            .map(|name| name.as_str().expect("name").to_string())
            .collect();
        assert_eq!(
            get_config_value_env_var_names(config),
            expected_names,
            "getConfigValueEnvVarNames({config:?})"
        );
        let expected_missing: Vec<String> = case["missing"]
            .as_array()
            .expect("missing")
            .iter()
            .map(|name| name.as_str().expect("name").to_string())
            .collect();
        assert_eq!(
            get_missing_config_value_env_var_names(config, Some(&env)),
            expected_missing,
            "getMissingConfigValueEnvVarNames({config:?})"
        );
        assert_eq!(
            is_command_config_value(config),
            case["isCommand"].as_bool().expect("isCommand"),
            "isCommandConfigValue({config:?})"
        );
        assert_eq!(
            is_config_value_configured(config, Some(&env)),
            case["configured"].as_bool().expect("configured"),
            "isConfigValueConfigured({config:?})"
        );
    }

    // `env?.[name] || process.env[name] || undefined` 的回退序列（空串视为缺失）。
    let expected_fallback: Vec<Option<String>> = fixture["fallback"]
        .as_array()
        .expect("fallback")
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect();
    set_env("TEST_PROBE", "from-process");
    let empty_scoped = ConfigEnv::from([("TEST_PROBE".to_string(), String::new())]);
    assert_eq!(
        resolve_config_value("$TEST_PROBE", Some(&empty_scoped)),
        expected_fallback[0]
    );
    let scoped = ConfigEnv::from([("TEST_PROBE".to_string(), "scoped".to_string())]);
    assert_eq!(
        resolve_config_value("$TEST_PROBE", Some(&scoped)),
        expected_fallback[1]
    );
    let other = ConfigEnv::from([("OTHER".to_string(), "x".to_string())]);
    assert_eq!(
        resolve_config_value("$TEST_PROBE", Some(&other)),
        expected_fallback[2]
    );
    set_env("TEST_PROBE", "");
    assert_eq!(
        resolve_config_value("$TEST_PROBE", None),
        expected_fallback[3]
    );
    set_env("TEST_PROBE", "  padded  ");
    assert_eq!(
        resolve_config_value("$TEST_PROBE", None),
        expected_fallback[4]
    );
    remove_env("TEST_PROBE");

    clear_config_value_cache();
}

fn golden_env() -> ConfigEnv {
    ConfigEnv::from([
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
        ("_".to_string(), "3".to_string()),
        ("LONG_NAME".to_string(), "long".to_string()),
        ("EMPTY".to_string(), String::new()),
    ])
}

/// `String.prototype.trim()` 的空白集合（§8.3 第 9 条）：U+0085 不是 JS 空白，
/// 而 NBSP/FEFF/Zs/行分隔符是。期望值由 TS 参考实现生成（真实字符而非 `\u` 转义）。
#[test]
fn trims_stdout_with_javascript_whitespace_semantics() {
    let _serial = serial();
    clear_config_value_cache();
    let ch = |code: u32| char::from_u32(code).expect("valid char");
    let cases = [
        (
            format!("!printf '  {} x {} '", ch(0x00a0), ch(0xfeff)),
            Some("x".to_string()),
        ),
        (
            format!("!printf '{}x{}'", ch(0x0085), ch(0x0085)),
            Some(format!("{}x{}", ch(0x0085), ch(0x0085))),
        ),
        (
            format!("!printf '{}a{}'", ch(0x2028), ch(0x2029)),
            Some("a".to_string()),
        ),
        (
            format!("!printf 'a{}b'", ch(0x2028)),
            Some(format!("a{}b", ch(0x2028))),
        ),
        (
            format!("!printf '{}{}x'", ch(0x000b), ch(0x000c)),
            Some("x".to_string()),
        ),
        (
            format!("!printf '{}x{}'", ch(0x3000), ch(0x3000)),
            Some("x".to_string()),
        ),
    ];
    for (command, expected) in cases {
        assert_eq!(
            resolve_config_value_uncached(&command, None),
            expected,
            "command {command:?}"
        );
    }
    clear_config_value_cache();
}

// ---------------------------------------------------------------------------
// 回归测试：Node 子进程默认值与 Windows 默认 shell 的参数构造。
// ---------------------------------------------------------------------------

/// Node `spawnSync`/`execSync` 的默认 `maxBuffer` 是 1 MiB：stdout 超过即 ENOBUFS，
/// TS 的 catch/错误分支 → `undefined`。边界由 node 26.12.1 实测：
/// 1 048 576 字节成功，1 048 577 字节失败。
#[cfg(unix)]
#[test]
fn returns_undefined_when_output_exceeds_the_node_max_buffer() {
    let _serial = serial();
    clear_config_value_cache();

    let at_limit = resolve_config_value_uncached("!head -c 1048576 /dev/zero | tr '\\0' a", None);
    assert_eq!(
        at_limit.as_deref().map(str::len),
        Some(1_048_576),
        "stdout exactly at maxBuffer must resolve"
    );

    let over_limit = resolve_config_value_uncached("!head -c 1048577 /dev/zero | tr '\\0' a", None);
    assert_eq!(
        over_limit, None,
        "stdout above maxBuffer must resolve to undefined (ENOBUFS)"
    );

    // 无界输出必须在 maxBuffer 处终止（而不是等满 10 s 超时并耗尽内存）。
    let started = std::time::Instant::now();
    assert_eq!(resolve_config_value_uncached("!yes", None), None);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "unbounded output must be killed at maxBuffer, took {:?}",
        started.elapsed()
    );

    clear_config_value_cache();
}

/// `maxBuffer` 同样作用于 configured shell 的 stdin 命令通道（`bash -s`）。
#[cfg(unix)]
#[test]
fn applies_max_buffer_to_the_stdin_command_transport() {
    let _serial = serial();
    clear_config_value_cache();

    let shell = if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "bash"
    };
    let shell_config = ShellConfig {
        shell: shell.to_string(),
        args: vec!["-s".to_string()],
        command_transport: Some(CommandTransport::Stdin),
    };
    assert_eq!(
        resolve_config_value_uncached_with_shell(
            "!head -c 1048577 /dev/zero | tr '\\0' a",
            None,
            Platform::Win32,
            Ok(shell_config),
        ),
        None
    );

    clear_config_value_cache();
}

/// Node `normalizeSpawnArguments()` 的 win32 shell 分支：
/// `comspec || "cmd.exe"`；`/^(?:.*\\)?cmd(?:\.exe)?$/i` 匹配时用
/// `['/d','/s','/c', '"command"']` + `windowsVerbatimArguments`，否则 `['-c', command]`。
#[test]
fn builds_windows_default_shell_invocations_like_node() {
    // 缺省 ComSpec → cmd.exe + 引号包裹 + verbatim。
    let (shell, args, verbatim) = windows_default_shell_invocation(None, "echo hi");
    assert_eq!(shell, "cmd.exe");
    assert_eq!(args, ["/d", "/s", "/c", "\"echo hi\""]);
    assert!(verbatim);

    // 绝对路径与大小写不敏感的 cmd.exe。
    let comspec = "C:\\Windows\\System32\\CMD.EXE";
    let (shell, args, verbatim) = windows_default_shell_invocation(Some(comspec), "echo hi");
    assert_eq!(shell, comspec);
    assert_eq!(args, ["/d", "/s", "/c", "\"echo hi\""]);
    assert!(verbatim);

    // 无扩展名的 `cmd` 也匹配。
    for name in ["cmd", "C:\\tools\\cmd", "c:\\foo\\Cmd.exe"] {
        let (_, args, verbatim) = windows_default_shell_invocation(Some(name), "x");
        assert!(verbatim, "{name} must be treated as cmd.exe");
        assert_eq!(args[3], "\"x\"");
    }

    // 非 cmd.exe 的 ComSpec（PowerShell）→ ['-c', command]。
    let comspec = "C:\\Program Files\\PowerShell\\7\\pwsh.exe";
    let (shell, args, verbatim) = windows_default_shell_invocation(Some(comspec), "echo hi");
    assert_eq!(shell, comspec);
    assert_eq!(args, ["-c", "echo hi"]);
    assert!(!verbatim);

    // Node 的正则只认反斜杠：正斜杠路径不匹配 cmd.exe。
    let (_, args, verbatim) =
        windows_default_shell_invocation(Some("C:/Windows/System32/cmd.exe"), "x");
    assert!(!verbatim);
    assert_eq!(args, ["-c", "x"]);
}
