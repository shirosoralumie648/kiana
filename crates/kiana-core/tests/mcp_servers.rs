//! `core/mcp-servers.ts` 的移植测试。
//!
//! 来源：
//! - `packages/coding-agent/test/mcp-extension.test.ts`：只取 `validateMcpServerConfig` /
//!   `getMcpToolExposure` 的断言（其余断言属于 `kiana-extensions` / `kiana-builtin-extensions`），
//!   测试名沿用 TS 的 `it` 标题。
//! - `tests/fixtures/mcp_validation.json`：由 TS 参考实现直接生成的对拍向量（101 条校验用例 +
//!   exposure 用例），错误串与成功时的配置对象逐字比较。
//! - 注册表 / 工具名匹配 / 常量：TS 无测试，按 `ARCHITECTURE.md` §4.8 第 9 条新写。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use kiana_core::mcp_servers::{
    LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL, McpExposure, McpServerConfig,
    McpServerConfigBase, McpServerRegistry, McpStdioServerConfig, READ_MCP_RESOURCE_TOOL,
    RegisteredMcpServer, create_tool_name_matcher, get_mcp_tool_exposure, is_loopback_redirect_uri,
    is_mcp_tool_name, mcp_namespace, validate_mcp_server_config,
};
use serde::Deserialize;
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/mcp_validation.json");

#[derive(Deserialize)]
struct Fixture {
    validate: Vec<ValidateCase>,
    exposure: Vec<ExposureCase>,
}

#[derive(Deserialize)]
struct ValidateCase {
    case: String,
    server: String,
    input: Value,
    ok: Option<Value>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ExposureCase {
    case: String,
    input: Value,
    tools: serde_json::Map<String, Value>,
}

/// serde_json 区分 `1`（整数）与 `1.0`（浮点），JS 不区分。配置里的 `timeout` 是 f64，
/// 因此把整数值的浮点归一化后再比较。
fn normalize_numbers(value: Value) -> Value {
    match value {
        Value::Number(number) => {
            if let Some(float) = number.as_f64()
                && float.fract() == 0.0
                && float.abs() <= 9_007_199_254_740_992.0
            {
                return Value::Number((float as i64).into());
            }
            Value::Number(number)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_numbers).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, normalize_numbers(value)))
                .collect(),
        ),
        other => other,
    }
}

#[test]
fn matches_the_ts_golden_fixture_for_validation() {
    let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture");
    assert!(fixture.validate.len() > 90);
    for case in fixture.validate {
        let result = validate_mcp_server_config(&case.server, &case.input);
        match (case.ok, case.error) {
            (Some(expected), None) => {
                let config = result
                    .unwrap_or_else(|error| panic!("case {}: unexpected error {error}", case.case));
                let actual = normalize_numbers(serde_json::to_value(&config).expect("serialize"));
                let expected = normalize_numbers(expected);
                assert_eq!(actual, expected, "case {}: config mismatch", case.case);
                // The fixture's `ok` object is the TS `JSON.stringify` output, so the byte
                // order must match as well (the returned shallow copy keeps the input order).
                assert_eq!(
                    kiana_json::js_object::stringify(&actual),
                    kiana_json::js_object::stringify(&expected),
                    "case {}: key order",
                    case.case
                );
            }
            (None, Some(expected_error)) => {
                let error = result.err().unwrap_or_else(|| {
                    panic!("case {}: expected error {expected_error}", case.case)
                });
                assert_eq!(error, expected_error, "case {}: error text", case.case);
            }
            _ => panic!(
                "case {}: fixture must have exactly one of ok/error",
                case.case
            ),
        }
    }
}

#[test]
fn matches_the_ts_golden_fixture_for_tool_exposure() {
    let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture");
    assert!(!fixture.exposure.is_empty());
    for case in fixture.exposure {
        let config = validate_mcp_server_config("s", &case.input)
            .unwrap_or_else(|error| panic!("case {}: {error}", case.case));
        for (tool, expected) in case.tools {
            let expected = expected.as_str().expect("exposure string");
            assert_eq!(
                get_mcp_tool_exposure(&config, &tool).as_str(),
                expected,
                "case {}: tool {tool}",
                case.case
            );
        }
    }
}

fn stdio(input: Value) -> McpServerConfig {
    validate_mcp_server_config("s", &input).expect("valid stdio config")
}

#[test]
fn merges_global_and_trusted_project_servers_and_validates_entries() {
    // The subset of mcp-extension.test.ts "merges global and trusted project servers and
    // validates entries" that belongs to validateMcpServerConfig.
    assert!(validate_mcp_server_config("shared", &json!({"command": "global-cmd"})).is_ok());
    let remote = validate_mcp_server_config(
        "remote",
        &json!({"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}}),
    )
    .expect("remote");
    match remote {
        McpServerConfig::Http(config) => {
            assert_eq!(
                config.headers.as_ref().expect("headers")["Authorization"],
                "Bearer ${TOKEN}"
            );
        }
        McpServerConfig::Stdio(_) => panic!("expected an HTTP config"),
    }
    assert!(validate_mcp_server_config("off", &json!({"command": "x", "enabled": false})).is_ok());

    assert_eq!(
        validate_mcp_server_config("bad", &json!({"args": ["no command"]})).unwrap_err(),
        "server \"bad\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    );
    assert_eq!(
        validate_mcp_server_config(
            "legacy",
            &json!({"type": "sse", "url": "https://example.com/sse"})
        )
        .unwrap_err(),
        "server \"legacy\": legacy SSE transport is not supported; use the streamable HTTP URL"
    );
    assert_eq!(
        validate_mcp_server_config("badUrl", &json!({"url": "example.com/mcp"})).unwrap_err(),
        "server \"badUrl\": url must be an http or https URL"
    );
    assert_eq!(
        validate_mcp_server_config("bad name", &json!({"command": "x"})).unwrap_err(),
        "invalid server name \"bad name\" (use letters, digits, \"_\" and \"-\")"
    );
}

#[test]
fn validates_exposure_and_reads_autoenablecodemode_with_project_precedence() {
    // The validateMcpServerConfig subset of the TS test with the same name.
    let scripts = stdio(json!({
        "command": "x",
        "exposure": "codemode-deferred",
        "toolExposure": {"a": "codemode-deferred"}
    }));
    assert_eq!(scripts.base().exposure, Some(McpExposure::Codemode));
    assert_eq!(
        scripts.base().tool_exposure.as_ref().expect("toolExposure")["a"],
        McpExposure::Codemode
    );
    assert_eq!(
        validate_mcp_server_config("wrong", &json!({"command": "x", "exposure": "model-only"}))
            .unwrap_err(),
        "server \"wrong\": exposure must be one of \"codemode\", \"deferred\", \"direct\", \"hidden\""
    );
    assert_eq!(
        validate_mcp_server_config("badDescription", &json!({"command": "x", "description": 1}))
            .unwrap_err(),
        "server \"badDescription\": description must be a string"
    );
    let described = stdio(json!({"command": "x", "description": "Docs search"}));
    assert_eq!(described.base().description.as_deref(), Some("Docs search"));
}

#[test]
fn resolves_per_tool_exposure_from_exact_names_then_patterns_in_order() {
    let config = stdio(json!({
        "command": "x",
        "exposure": "deferred",
        "toolExposure": {"get_*": "codemode", "get_me": "direct", "*delete*": "hidden", "get_file.*": "direct"}
    }));
    assert_eq!(get_mcp_tool_exposure(&config, "get_me").as_str(), "direct");
    assert_eq!(
        get_mcp_tool_exposure(&config, "get_issue").as_str(),
        "codemode"
    );
    assert_eq!(
        get_mcp_tool_exposure(&config, "get_delete_hint").as_str(),
        "codemode"
    );
    assert_eq!(
        get_mcp_tool_exposure(&config, "delete_repo").as_str(),
        "hidden"
    );
    assert_eq!(
        get_mcp_tool_exposure(&config, "list_issues").as_str(),
        "deferred"
    );
    // Only `*` is special.
    let only_star = stdio(json!({"command": "x", "toolExposure": {"get_file.*": "direct"}}));
    assert_eq!(
        get_mcp_tool_exposure(&only_star, "get_file_x").as_str(),
        "codemode"
    );
    assert_eq!(
        get_mcp_tool_exposure(&only_star, "get_file.x").as_str(),
        "direct"
    );
    assert_eq!(
        validate_mcp_server_config(
            "bad",
            &json!({"command": "x", "toolExposure": {"a": "visible"}})
        )
        .unwrap_err(),
        "server \"bad\": toolExposure \"a\" must be one of \"codemode\", \"deferred\", \"direct\", \"hidden\""
    );
}

#[test]
fn validates_the_oauth_callback_url_scope_and_client_name() {
    // Ported from the TS test with the same name; the surrounding loadMcpConfig merge is covered
    // by kiana-extensions. Server names and inputs match the TS fixture.
    let ok = |input: Value| validate_mcp_server_config("s", &input);
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"callbackUrl": "http://localhost:8080/callback", "scope": "a b"}}))
            .is_ok()
    );
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"callbackUrl": "http://[::1]/cb", "callbackPort": 9000}}))
            .is_ok()
    );
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"callbackUrl": "http://127.0.0.1:2/cb", "callbackPort": 2}}))
            .is_ok()
    );

    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"callbackUrl": "https://example.com/callback"}}))
            .unwrap_err(),
        "server \"s\": oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"callbackUrl": "http://127.0.0.1:1/cb", "callbackPort": 2}}))
            .unwrap_err(),
        "server \"s\": oauth.callbackUrl and oauth.callbackPort name different ports"
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"scope": ["a"]}})).unwrap_err(),
        "server \"s\": oauth.scope must be a string"
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientName": " "}})).unwrap_err(),
        "server \"s\": oauth.clientName must be a non-empty string"
    );
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientName": "Claude Code"}})).is_ok()
    );
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"authServerMetadataUrl": "https://idp.example/m"}}))
            .is_ok()
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"authServerMetadataUrl": "http://idp.example/m"}}))
            .unwrap_err(),
        "server \"s\": oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
    );
    assert!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientRegistration": "cimd", "callbackUrl": "http://localhost/callback"}}))
            .is_ok()
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientRegistration": "auto"}}))
            .unwrap_err(),
        "server \"s\": oauth.clientRegistration must be \"dcr\" or \"cimd\""
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientRegistration": "cimd", "clientId": "x"}}))
            .unwrap_err(),
        "server \"s\": oauth.clientRegistration \"cimd\" cannot be combined with oauth.clientId or oauth.clientName"
    );
    assert_eq!(
        ok(json!({"url": "https://a.example/mcp", "oauth": {"clientRegistration": "cimd", "callbackUrl": "http://127.0.0.1/cb"}}))
            .unwrap_err(),
        "server \"s\": oauth.clientRegistration \"cimd\" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback"
    );
}

#[test]
fn validates_provider_auth_and_accepts_it_only_in_the_global_mcp_json() {
    // The validateMcpServerConfig subset: the "only in the global mcp.json" rule is enforced by
    // extensions/mcp/config.ts, not by this module.
    assert!(
        validate_mcp_server_config(
            "radius",
            &json!({"url": "https://radius.example/mcp", "auth": {"provider": "radius"}})
        )
        .is_ok()
    );
    assert!(
        validate_mcp_server_config(
            "local",
            &json!({"url": "http://localhost:8788/mcp", "auth": {"provider": "radius-dev"}})
        )
        .is_ok()
    );
    assert_eq!(
        validate_mcp_server_config(
            "plain",
            &json!({"url": "http://radius.example/mcp", "auth": {"provider": "radius"}})
        )
        .unwrap_err(),
        "server \"plain\": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]"
    );
    assert_eq!(
        validate_mcp_server_config(
            "empty",
            &json!({"url": "https://radius.example/mcp", "auth": {"provider": ""}})
        )
        .unwrap_err(),
        "server \"empty\": auth.provider must be a provider name"
    );
}

#[test]
fn rejects_server_names_that_differ_only_in_dash_and_underscore() {
    // Regression #10239: the namespace collapses `-` to `_`, so these two names clash.
    assert_eq!(mcp_namespace("work-files"), mcp_namespace("work_files"));
    assert_eq!(mcp_namespace("work-files"), "mcp__work_files");
    assert!(validate_mcp_server_config("work-files", &json!({"command": "a"})).is_ok());
    assert!(validate_mcp_server_config("work_files", &json!({"command": "b"})).is_ok());
}

#[test]
fn is_loopback_redirect_uri_matches_the_whatwg_rules() {
    assert!(is_loopback_redirect_uri("http://localhost/cb"));
    assert!(is_loopback_redirect_uri("http://localhost:8080/callback"));
    assert!(is_loopback_redirect_uri("http://127.0.0.1/cb"));
    assert!(is_loopback_redirect_uri("http://[::1]/cb"));
    assert!(is_loopback_redirect_uri("http://[0:0:0:0:0:0:0:1]/cb"));
    // `?` / `#` alone are normalized to empty query/fragment, which TS accepts.
    assert!(is_loopback_redirect_uri("http://localhost/cb?"));
    assert!(is_loopback_redirect_uri("http://localhost/cb#"));
    // Non-empty query or fragment.
    assert!(!is_loopback_redirect_uri("http://localhost/cb?x=1"));
    assert!(!is_loopback_redirect_uri("http://localhost/cb#x"));
    // Only http:.
    assert!(!is_loopback_redirect_uri("https://localhost/cb"));
    // Only loopback hosts.
    assert!(!is_loopback_redirect_uri("http://example.com/cb"));
    assert!(!is_loopback_redirect_uri("http://localhost.example.com/cb"));
    // Unparseable.
    assert!(!is_loopback_redirect_uri("not a url"));
    assert!(!is_loopback_redirect_uri(""));
}

#[test]
fn is_mcp_tool_name_and_constants() {
    assert_eq!(LIST_MCP_RESOURCES_TOOL, "list_mcp_resources");
    assert_eq!(
        LIST_MCP_RESOURCE_TEMPLATES_TOOL,
        "list_mcp_resource_templates"
    );
    assert_eq!(READ_MCP_RESOURCE_TOOL, "read_mcp_resource");
    assert!(is_mcp_tool_name("mcp__docs__search"));
    assert!(is_mcp_tool_name("mcp__"));
    for tool in [
        LIST_MCP_RESOURCES_TOOL,
        LIST_MCP_RESOURCE_TEMPLATES_TOOL,
        READ_MCP_RESOURCE_TOOL,
    ] {
        assert!(is_mcp_tool_name(tool));
    }
    assert!(!is_mcp_tool_name("read"));
    assert!(!is_mcp_tool_name("list_mcp_resources_extra"));
    assert!(!is_mcp_tool_name("mcp_docs"));
}

#[test]
fn create_tool_name_matcher_treats_only_star_as_special() {
    let matcher = create_tool_name_matcher(&["exact", "get_*", "a.b", "*delete*", "x?y"]);
    assert!(matcher("exact"));
    assert!(matcher("get_issue"));
    assert!(!matcher("get"));
    assert!(matcher("a.b"));
    assert!(!matcher("axb"));
    assert!(matcher("x?y"));
    assert!(!matcher("xzy"));
    assert!(matcher("delete_repo"));
    assert!(matcher("predeletepost"));
    assert!(matcher("deleted"));
    assert!(!matcher("remove"));
    assert!(!matcher(""));

    let empty = create_tool_name_matcher::<String>(&[]);
    assert!(!empty("anything"));

    let owned: Vec<String> = vec!["mcp__*".to_string()];
    let matcher = create_tool_name_matcher(&owned);
    assert!(matcher("mcp__docs__search"));
}

#[test]
fn exposes_use_the_ts_literals() {
    for (exposure, literal) in [
        (McpExposure::Codemode, "codemode"),
        (McpExposure::Deferred, "deferred"),
        (McpExposure::Direct, "direct"),
        (McpExposure::Hidden, "hidden"),
    ] {
        assert_eq!(exposure.as_str(), literal);
        assert_eq!(exposure.to_string(), literal);
        assert_eq!(McpExposure::parse(literal), Some(exposure));
        assert_eq!(
            serde_json::to_value(exposure).expect("serialize"),
            json!(literal)
        );
    }
    assert_eq!(McpExposure::parse("codemode-deferred"), None);
    assert_eq!(McpExposure::parse("Codemode"), None);
}

#[test]
fn configs_serialize_in_the_ts_input_order_and_keep_the_type_string() {
    // `JSON.stringify(validateMcpServerConfig("s", <input>))` from the TS reference: the
    // returned shallow copy keeps the caller's key order (spec ca-builtin-extensions.md §2.2.7).
    let config = stdio(json!({
        "command": "x",
        "exposure": "direct",
        "description": "d",
        "toolExposure": {"a": "deferred"},
        "enabled": false,
        "type": "stdio",
        "args": ["a"],
        "env": {"A": "1"},
        "cwd": "/tmp"
    }));
    assert_eq!(
        serde_json::to_string(&config).expect("serialize"),
        concat!(
            r#"{"command":"x","exposure":"direct","description":"d","#,
            r#""toolExposure":{"a":"deferred"},"enabled":false,"type":"stdio","#,
            r#""args":["a"],"env":{"A":"1"},"cwd":"/tmp"}"#
        )
    );

    let streamable = validate_mcp_server_config(
        "s",
        &json!({"type": "streamable-http", "url": "https://example.com/mcp"}),
    )
    .expect("valid http");
    assert_eq!(
        serde_json::to_string(&streamable).expect("serialize"),
        r#"{"type":"streamable-http","url":"https://example.com/mcp"}"#
    );

    let plain = validate_mcp_server_config("s", &json!({"url": "https://example.com/mcp"}))
        .expect("valid http");
    assert_eq!(
        serde_json::to_string(&plain).expect("serialize"),
        r#"{"url":"https://example.com/mcp"}"#
    );
}

#[test]
fn timeout_serializes_with_js_number_formatting() {
    // Expected strings generated with the TS reference:
    //   JSON.stringify(validateMcpServerConfig("s", { command: "x", timeout: v }))
    for (value, expected) in [
        (60.0_f64, "60"),
        (60.5, "60.5"),
        (0.1, "0.1"),
        (1e20, "100000000000000000000"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (1e21, "1e+21"),
        (123456789012345680000.0, "123456789012345680000"),
        (1.25e300, "1.25e+300"),
        (9007199254740993.0, "9007199254740992"),
        (2.5, "2.5"),
        (3600.0, "3600"),
        (0.25, "0.25"),
    ] {
        let config = validate_mcp_server_config("s", &json!({"command": "x", "timeout": value}))
            .expect("valid stdio");
        assert_eq!(
            serde_json::to_string(&config).expect("serialize"),
            format!(r#"{{"command":"x","timeout":{expected}}}"#),
            "timeout {value:?}"
        );
        // `Serialize` embeds a `JSON.stringify` blob (RawValue), so `to_string_pretty` does not
        // re-indent it. The mcp.json writer takes `to_value` + kiana_json's pretty writer instead
        // (spec ca-builtin-extensions.md §2.2.7).
        let value = serde_json::to_value(&config).expect("to_value");
        assert!(
            kiana_json::js_object::stringify_pretty(&value, 2)
                .contains(&format!(r#""timeout": {expected}"#)),
            "pretty timeout {value:?}"
        );
    }

    // A config without a timeout has no key at all (TS `undefined`).
    let config = validate_mcp_server_config("s", &json!({"command": "x"})).expect("valid stdio");
    assert_eq!(
        serde_json::to_string(&config).expect("serialize"),
        r#"{"command":"x"}"#
    );

    // The HTTP variant carries the same base fields.
    let config = validate_mcp_server_config(
        "s",
        &json!({"url": "https://example.com/mcp", "timeout": 1e20}),
    )
    .expect("valid http");
    assert_eq!(
        serde_json::to_string(&config).expect("serialize"),
        r#"{"url":"https://example.com/mcp","timeout":100000000000000000000}"#
    );
}

#[test]
fn serialize_preserves_the_ts_input_key_order_and_unknown_keys() {
    // Regression: `JSON.stringify(validateMcpServerConfig("s", <input>))` from the TS reference.
    // TS returns the resolved shallow copy `{...input}`, so keys stay in the caller's insertion
    // order (with JS integer-index keys first) and unknown keys keep their position. The
    // `kiana mcp add` path writes the validated config back with `JSON.stringify`, so this order
    // is observable in mcp.json (spec ca-builtin-extensions.md §2.2.7).
    for (input, expected) in [
        (
            json!({"command": "x", "exposure": "direct", "description": "d"}),
            r#"{"command":"x","exposure":"direct","description":"d"}"#,
        ),
        (
            json!({"description": "d", "command": "x", "exposure": "direct"}),
            r#"{"description":"d","command":"x","exposure":"direct"}"#,
        ),
        // Unknown keys stay in place; the exposure alias is resolved in place.
        (
            json!({"x-custom": 1, "command": "x", "a-b": {"z": 1, "a": 2}, "exposure": "codemode-deferred"}),
            r#"{"x-custom":1,"command":"x","a-b":{"z":1,"a":2},"exposure":"codemode"}"#,
        ),
        // `url` is a string, so the entry validates as HTTP and `command` remains an unknown key
        // in its original position.
        (
            json!({"command": "x", "url": "https://nope"}),
            r#"{"command":"x","url":"https://nope"}"#,
        ),
        // JS puts integer-index keys first in ascending order.
        (
            json!({"2": "b", "1": "a", "command": "x", "10": "c"}),
            r#"{"1":"a","2":"b","10":"c","command":"x"}"#,
        ),
        // Nested objects keep their own insertion order (oauth, headers).
        (
            json!({"url": "https://a/mcp", "oauth": {"scope": "s", "clientId": "c"}, "headers": {"B": "2", "A": "1"}}),
            r#"{"url":"https://a/mcp","oauth":{"scope":"s","clientId":"c"},"headers":{"B":"2","A":"1"}}"#,
        ),
        (
            json!({"command": "x", "toolExposure": {"b": "direct", "1": "hidden", "a": "codemode"}}),
            r#"{"command":"x","toolExposure":{"1":"hidden","b":"direct","a":"codemode"}}"#,
        ),
        // Nested oauth objects keep unknown keys in place, with integer-index keys first.
        (
            json!({"url": "https://a/mcp", "oauth": {"x-extra": "e", "clientId": "c", "1": "one"}}),
            r#"{"url":"https://a/mcp","oauth":{"1":"one","x-extra":"e","clientId":"c"}}"#,
        ),
        // Unknown numeric values follow JS number formatting too.
        (
            json!({"command": "x", "big": 1e21, "whole": 1.0, "neg": -0.0, "nested": {"z": 1e-7, "a": 2.5}}),
            r#"{"command":"x","big":1e+21,"whole":1,"neg":0,"nested":{"z":1e-7,"a":2.5}}"#,
        ),
        (
            json!({"command": "x", "env": {"2": "b", "1": "a", "Z": "z"}}),
            r#"{"command":"x","env":{"1":"a","2":"b","Z":"z"}}"#,
        ),
        // The `kiana mcp add` object shapes: transport fields first, then exposure/description.
        (
            json!({"command": "npx", "args": ["-y", "x"], "env": {"A": "1"}, "cwd": "/tmp", "exposure": "direct", "description": "D"}),
            r#"{"command":"npx","args":["-y","x"],"env":{"A":"1"},"cwd":"/tmp","exposure":"direct","description":"D"}"#,
        ),
        (
            json!({"url": "https://a/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}, "oauth": {"clientId": "c"}, "exposure": "deferred", "description": "D"}),
            r#"{"url":"https://a/mcp","headers":{"Authorization":"Bearer ${TOKEN}"},"oauth":{"clientId":"c"},"exposure":"deferred","description":"D"}"#,
        ),
    ] {
        let config = validate_mcp_server_config("s", &input).expect("valid config");
        assert_eq!(
            serde_json::to_string(&config).expect("serialize"),
            expected,
            "input {input}"
        );
        // Consumers that materialize a Value first (the mcp.json writer) see the same order.
        let value = serde_json::to_value(&config).expect("to_value");
        assert_eq!(
            kiana_json::js_object::stringify(&value),
            expected,
            "to_value input {input}"
        );
    }
}

#[test]
fn config_equality_ignores_the_recorded_input_key_order() {
    let first = validate_mcp_server_config(
        "s",
        &json!({"command": "x", "exposure": "direct", "description": "d"}),
    )
    .expect("valid");
    let second = validate_mcp_server_config(
        "s",
        &json!({"description": "d", "exposure": "direct", "command": "x"}),
    )
    .expect("valid");
    assert_eq!(first, second);
}

#[test]
fn programmatic_configs_serialize_in_declaration_order_with_extras_last() {
    // No TS counterpart: configs built without `validateMcpServerConfig` have no recorded input
    // order, so the declaration order is used (see the module docs).
    let config = McpStdioServerConfig {
        base: McpServerConfigBase {
            description: Some("d".to_string()),
            timeout: Some(60.0),
            ..Default::default()
        },
        command: "x".to_string(),
        extra: IndexMap::from([("z".to_string(), json!(1))]),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&McpServerConfig::Stdio(config)).expect("serialize"),
        r#"{"description":"d","timeout":60,"command":"x","z":1}"#
    );
}

#[test]
fn tool_patterns_use_the_js_dot_which_excludes_line_terminators() {
    // TS `toolPatternRegExp` joins parts with `.*`; JS `.` (no `s` flag) does not match `\n`,
    // `\r`, U+2028 or U+2029. Verified against the TS reference.
    let matcher = create_tool_name_matcher(&["*"]);
    assert!(matcher("ab"));
    assert!(matcher(""));
    assert!(matcher("a.b"));
    assert!(!matcher("a\nb"));
    assert!(!matcher("a\rb"));
    assert!(!matcher("a\u{2028}b"));
    assert!(!matcher("a\u{2029}b"));
    assert!(!matcher("a\r\nb"));

    let matcher = create_tool_name_matcher(&["a*b"]);
    assert!(matcher("a?b"));
    assert!(matcher("axb"));
    assert!(!matcher("a\rb"));

    // The same class is used by `toolExposure` pattern overrides: in TS the pattern does not
    // match, so the server exposure (`codemode`) wins over the `direct` override.
    let config = validate_mcp_server_config(
        "s",
        &json!({"command": "x", "toolExposure": {"*": "direct"}}),
    )
    .expect("valid stdio");
    assert_eq!(get_mcp_tool_exposure(&config, "ab"), McpExposure::Direct);
    assert_eq!(
        get_mcp_tool_exposure(&config, "a\rb"),
        McpExposure::Codemode
    );
    assert_eq!(
        get_mcp_tool_exposure(&config, "a\u{2028}b"),
        McpExposure::Codemode
    );
}

#[test]
fn null_is_not_undefined_for_optional_fields() {
    // TS checks `field !== undefined`, and JSON null is not undefined: every optional field
    // rejects an explicit null with its normal type error.
    for (input, error) in [
        (
            json!({"command": "x", "exposure": null}),
            "server \"s\": exposure must be one of \"codemode\", \"deferred\", \"direct\", \"hidden\"",
        ),
        (
            json!({"command": "x", "toolExposure": null}),
            "server \"s\": toolExposure must map tool names to exposures",
        ),
        (
            json!({"command": "x", "enabled": null}),
            "server \"s\": enabled must be a boolean",
        ),
        (
            json!({"command": "x", "description": null}),
            "server \"s\": description must be a string",
        ),
        (
            json!({"command": "x", "timeout": null}),
            "server \"s\": timeout must be a positive number of seconds",
        ),
        (
            json!({"command": "x", "args": null}),
            "server \"s\": args must be an array of strings",
        ),
        (
            json!({"command": "x", "env": null}),
            "server \"s\": env must map names to strings",
        ),
        (
            json!({"command": "x", "cwd": null}),
            "server \"s\": cwd must be a string",
        ),
        (
            json!({"url": "https://example.com/mcp", "headers": null}),
            "server \"s\": headers must map names to strings",
        ),
        (
            json!({"url": "https://example.com/mcp", "oauth": null}),
            "server \"s\": oauth must be an object",
        ),
        (
            json!({"url": "https://example.com/mcp", "auth": null}),
            "server \"s\": auth.provider must be a provider name",
        ),
    ] {
        assert_eq!(validate_mcp_server_config("s", &input).unwrap_err(), error);
    }
}

fn registered(name: &str, command: &str, extension_path: &str) -> RegisteredMcpServer {
    RegisteredMcpServer {
        name: name.to_string(),
        config: stdio(json!({"command": command})),
        extension_path: extension_path.to_string(),
    }
}

fn names(registry: &McpServerRegistry) -> Vec<String> {
    registry
        .list()
        .into_iter()
        .map(|server| server.name)
        .collect()
}

#[test]
fn registry_keeps_registration_order_when_replacing() {
    let registry = McpServerRegistry::default();
    registry.register(registered("a", "ca", "/ext/one"));
    registry.register(registered("b", "cb", "/ext/one"));
    registry.register(registered("c", "cc", "/ext/one"));
    assert_eq!(names(&registry), ["a", "b", "c"]);

    registry.register(registered("b", "replaced", "/ext/two"));
    assert_eq!(names(&registry), ["a", "b", "c"]);
    match registry.get("b").expect("b").config {
        McpServerConfig::Stdio(config) => assert_eq!(config.command, "replaced"),
        McpServerConfig::Http(_) => panic!("expected stdio"),
    }
}

#[test]
fn registry_fires_the_change_listener_on_every_register_and_successful_unregister() {
    let registry = McpServerRegistry::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let listener_calls = Arc::clone(&calls);
    registry.set_change_listener(Some(Arc::new(move || {
        listener_calls.fetch_add(1, Ordering::SeqCst);
    })));

    registry.register(registered("a", "ca", "/ext/one"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Replacing an entry fires even when the value is unchanged.
    registry.register(registered("a", "ca", "/ext/one"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Wrong owner: neither removes nor fires.
    registry.unregister("a", "/ext/other");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(names(&registry), ["a"]);

    // Missing name: no-op.
    registry.unregister("missing", "/ext/one");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Matching owner removes and fires.
    registry.unregister("a", "/ext/one");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(names(&registry).is_empty());

    // A cleared listener is not called.
    registry.set_change_listener(None);
    registry.register(registered("b", "cb", "/ext/one"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn registry_list_and_get_return_deep_copies() {
    let registry = McpServerRegistry::default();
    registry.register(registered("a", "ca", "/ext/one"));

    let mut listed = registry.list();
    assert_eq!(listed.len(), 1);
    listed[0].config.base_mut().enabled = Some(false);
    listed[0].name = "renamed".into();
    assert_eq!(names(&registry), ["a"]);
    assert_eq!(registry.get("a").expect("a").config.base().enabled, None);

    let mut got = registry.get("a").expect("a");
    got.config.base_mut().timeout = Some(1.0);
    assert_eq!(registry.get("a").expect("a").config.base().timeout, None);
    assert!(registry.get("missing").is_none());
}
