//! `registerMcpServer` 的校验、所有权与命名空间冲突测试。
//!
//! 移植 `packages/coding-agent/test/mcp-extension.test.ts` 的 `describe("MCP config")`
//! 中与单条配置校验相关的用例（`loadMcpConfig` 的合并/信任逻辑属于 telemetry-mcp），
//! 以及 `registerMcpServer` 的所有权与 `mcp__` 命名空间规则（§4.4 / §8.6 第 14 条）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_core::mcp_servers::{McpExposure, McpServerConfig, McpServerRegistry};
use kiana_extensions::runner::mcp_registration::{
    McpRegistrationError, prepare_mcp_server, register_mcp_server,
};
use serde_json::json;

const EXPOSURES: &str = "\"codemode\", \"deferred\", \"direct\", \"hidden\"";

fn new_registry() -> McpServerRegistry {
    McpServerRegistry::new()
}

// ---------------------------------------------------------------------------
// 成功路径
// ---------------------------------------------------------------------------

#[test]
fn registers_a_valid_stdio_server_with_the_extension_path() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/mcp.ts",
        "demo",
        &json!({ "command": "echo", "args": ["hi"], "exposure": "direct" }),
    )
    .expect("valid config");

    let server = registry.get("demo").expect("registered");
    assert_eq!(server.name, "demo");
    assert_eq!(server.extension_path, "/extensions/mcp.ts");
    match &server.config {
        McpServerConfig::Stdio(config) => {
            assert_eq!(config.command, "echo");
            assert_eq!(config.args.as_deref(), Some(&["hi".to_string()][..]));
            assert_eq!(config.base.exposure, Some(McpExposure::Direct));
        }
        McpServerConfig::Http(_) => panic!("expected stdio config"),
    }
}

#[test]
fn registers_a_valid_http_server_and_resolves_exposure_aliases() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/mcp.ts",
        "docs",
        &json!({
            "url": "https://mcp.example.com/jira",
            "exposure": "codemode-deferred",
            "toolExposure": { "a": "codemode-deferred", "b": "hidden" },
        }),
    )
    .expect("valid config");

    let server = registry.get("docs").expect("registered");
    match &server.config {
        McpServerConfig::Http(config) => {
            assert_eq!(config.url, "https://mcp.example.com/jira");
            assert_eq!(config.base.exposure, Some(McpExposure::Codemode));
            let tool_exposure = config.base.tool_exposure.as_ref().expect("toolExposure");
            assert_eq!(tool_exposure["a"], McpExposure::Codemode);
            assert_eq!(tool_exposure["b"], McpExposure::Hidden);
        }
        McpServerConfig::Stdio(_) => panic!("expected http config"),
    }
}

#[test]
fn the_stored_config_is_an_independent_copy() {
    let registry = new_registry();
    let mut config = json!({ "command": "echo", "args": ["hi"] });
    register_mcp_server(&registry, "/extensions/mcp.ts", "demo", &config).expect("valid config");

    // 扩展后续修改自己的配置对象（TS 的 structuredClone 语义要求注册项不受影响）。
    config["args"] = json!(["changed"]);
    config["command"] = json!("changed");
    let server = registry.get("demo").expect("registered");
    match &server.config {
        McpServerConfig::Stdio(config) => {
            assert_eq!(config.command, "echo");
            assert_eq!(config.args.as_deref(), Some(&["hi".to_string()][..]));
        }
        McpServerConfig::Http(_) => panic!("expected stdio config"),
    }
}

#[test]
fn registration_notifies_the_change_listener() {
    let registry = new_registry();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    registry.set_change_listener(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));

    register_mcp_server(
        &registry,
        "/extensions/mcp.ts",
        "demo",
        &json!({ "command": "echo" }),
    )
    .expect("valid config");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // 失败路径不写注册表，也不触发监听。
    assert!(
        register_mcp_server(
            &registry,
            "/extensions/mcp.ts",
            "bad name",
            &json!({ "command": "x" })
        )
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 校验错误（移植 MCP config 组的同款输入）
// ---------------------------------------------------------------------------

fn invalid_error(name: &str, config: serde_json::Value) -> McpRegistrationError {
    let registry = new_registry();
    register_mcp_server(&registry, "/extensions/conflict.ts", name, &config)
        .expect_err("config must be rejected")
}

fn invalid_message(name: &str, config: serde_json::Value) -> String {
    invalid_error(name, config).to_string()
}

#[test]
fn invalid_server_names_get_the_extension_prefix() {
    let message = invalid_message("bad name", json!({ "command": "x" }));
    assert_eq!(
        message,
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         invalid server name \"bad name\" (use letters, digits, \"_\" and \"-\")"
    );
}

#[test]
fn missing_transport_is_rejected() {
    assert_eq!(
        invalid_message("bad", json!({ "args": ["no command"] })),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"bad\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    );
}

#[test]
fn legacy_sse_transport_is_rejected() {
    assert_eq!(
        invalid_message(
            "legacy",
            json!({ "type": "sse", "url": "https://example.com/sse" })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"legacy\": legacy SSE transport is not supported; use the streamable HTTP URL"
    );
}

#[test]
fn non_http_urls_are_rejected() {
    assert_eq!(
        invalid_message("badUrl", json!({ "url": "example.com/mcp" })),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"badUrl\": url must be an http or https URL"
    );
}

#[test]
fn unknown_exposures_and_bad_descriptions_are_rejected() {
    assert_eq!(
        invalid_message("wrong", json!({ "command": "x", "exposure": "model-only" })),
        format!(
            "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
             server \"wrong\": exposure must be one of {EXPOSURES}"
        )
    );
    assert_eq!(
        invalid_message(
            "badDescription",
            json!({ "command": "x", "description": 1 })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"badDescription\": description must be a string"
    );
}

#[test]
fn bad_tool_exposure_values_are_rejected() {
    assert_eq!(
        invalid_message(
            "bad",
            json!({ "command": "x", "toolExposure": { "a": "visible" } })
        ),
        format!(
            "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
             server \"bad\": toolExposure \"a\" must be one of {EXPOSURES}"
        )
    );
    assert_eq!(
        invalid_message(
            "bad",
            json!({ "command": "x", "toolExposure": ["not", "a", "map"] })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"bad\": toolExposure must map tool names to exposures"
    );
}

#[test]
fn provider_auth_needs_https_and_a_provider_name() {
    assert_eq!(
        invalid_message(
            "plain",
            json!({ "url": "http://radius.example/mcp", "auth": { "provider": "radius" } })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"plain\": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]"
    );
    assert_eq!(
        invalid_message(
            "empty",
            json!({ "url": "https://radius.example/mcp", "auth": { "provider": "" } })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"empty\": auth.provider must be a provider name"
    );
}

#[test]
fn oauth_callback_errors_are_prefixed_like_ts() {
    // 与 MCP config 组的 OAuth 用例同款输入。
    assert_eq!(
        invalid_message(
            "remote",
            json!({
                "url": "https://a.example/mcp",
                "oauth": { "callbackUrl": "https://example.com/callback" },
            })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"remote\": oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] \
         without query or fragment"
    );
    assert_eq!(
        invalid_message(
            "unnamed",
            json!({ "url": "https://a.example/mcp", "oauth": { "clientName": " " } })
        ),
        "Invalid MCP server registered by extension \"/extensions/conflict.ts\": \
         server \"unnamed\": oauth.clientName must be a non-empty string"
    );
}

// ---------------------------------------------------------------------------
// 所有权与命名空间
// ---------------------------------------------------------------------------

#[test]
fn a_server_owned_by_another_extension_is_rejected() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "a" }),
    )
    .expect("first registration");

    let error = register_mcp_server(
        &registry,
        "/extensions/b.ts",
        "shared",
        &json!({ "command": "b" }),
    )
    .expect_err("second owner must be rejected");
    assert_eq!(
        error.to_string(),
        "MCP server \"shared\" is already registered by extension \"/extensions/a.ts\""
    );
    // 原配置保持不变。
    match &registry.get("shared").expect("still registered").config {
        McpServerConfig::Stdio(config) => assert_eq!(config.command, "a"),
        McpServerConfig::Http(_) => panic!("expected stdio config"),
    }
}

#[test]
fn the_same_extension_may_replace_its_own_server() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "before" }),
    )
    .expect("first registration");
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "after" }),
    )
    .expect("same owner may replace");

    assert_eq!(registry.list().len(), 1);
    match &registry.get("shared").expect("registered").config {
        McpServerConfig::Stdio(config) => assert_eq!(config.command, "after"),
        McpServerConfig::Http(_) => panic!("expected stdio config"),
    }
}

#[test]
fn names_differing_only_in_dash_and_underscore_conflict() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "work-files",
        &json!({ "command": "a" }),
    )
    .expect("first registration");

    let error = register_mcp_server(
        &registry,
        "/extensions/b.ts",
        "work_files",
        &json!({ "command": "b" }),
    )
    .expect_err("namespace clash must be rejected");
    assert_eq!(
        error.to_string(),
        "MCP server \"work_files\" conflicts with registered server \"work-files\""
    );
    assert!(registry.get("work_files").is_none());

    // 反向顺序同样成立。
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "work_files",
        &json!({ "command": "a" }),
    )
    .expect("first registration");
    let error = register_mcp_server(
        &registry,
        "/extensions/b.ts",
        "work-files",
        &json!({ "command": "b" }),
    )
    .expect_err("namespace clash must be rejected");
    assert_eq!(
        error.to_string(),
        "MCP server \"work-files\" conflicts with registered server \"work_files\""
    );
}

#[test]
fn a_same_name_replacement_does_not_trip_the_namespace_check() {
    // namespace 检查只比较 `server.name !== name` 的条目。
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "work-files",
        &json!({ "command": "a" }),
    )
    .expect("first registration");
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "work-files",
        &json!({ "command": "b" }),
    )
    .expect("same name, same owner");
    assert_eq!(registry.list().len(), 1);
}

#[test]
fn prepare_defers_the_registry_write_until_the_factory_commits() {
    // TS 的 `applyRuntimeChange` 在扩展加载期（state === "loading"）只把 `mcpServers.register`
    // 压入**当前工厂**的 pending 队列；校验/所有权检查仍对当时的注册表求值。
    let registry = new_registry();
    let server = prepare_mcp_server(
        &registry,
        "/extensions/a.ts",
        "demo",
        &json!({ "command": "echo" }),
    )
    .expect("valid config");
    assert_eq!(server.name, "demo");
    assert_eq!(server.extension_path, "/extensions/a.ts");
    assert!(registry.get("demo").is_none(), "prepare 本身不写注册表");

    // 同一个工厂内的排队项之间互不可见（TS 的 owner/namespace 检查只读注册表），
    // 因此 `my-server` 与 `my_server` 在提交前不会触发命名空间冲突；commit 时按序
    // 调用 `register`，两者都能落库（`register` 本身不做检查）。
    prepare_mcp_server(
        &registry,
        "/extensions/a.ts",
        "my-server",
        &json!({ "command": "a" }),
    )
    .expect("first pending registration");
    prepare_mcp_server(
        &registry,
        "/extensions/a.ts",
        "my_server",
        &json!({ "command": "b" }),
    )
    .expect("pending registrations do not clash yet");

    registry.register(server);
    assert!(registry.get("demo").is_some());
}

#[test]
fn a_later_extension_sees_the_previous_extensions_committed_server() {
    // TS 的 `initializeExtension` 在每个工厂返回后立刻 `load.commit()`
    // （loader.ts:626-631），`loadExtensionsInternal` 顺序加载扩展（loader.ts:694-701）：
    // pending 队列的边界是单个工厂，后一个扩展能看到前一个扩展已提交的 server。
    let registry = new_registry();
    let prepared = prepare_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "a" }),
    )
    .expect("factory A queues the registration");
    registry.register(prepared); // factory A commits before factory B runs.

    let error = prepare_mcp_server(
        &registry,
        "/extensions/b.ts",
        "shared",
        &json!({ "command": "b" }),
    )
    .expect_err("a committed owner is visible to the next extension");
    assert_eq!(
        error.to_string(),
        "MCP server \"shared\" is already registered by extension \"/extensions/a.ts\""
    );
}

#[test]
fn eager_registration_is_not_a_factory_queue() {
    // `register_mcp_server` 对应 TS `applyRuntimeChange` 在 state === "active" 时的立即
    // 路径。若在工厂内用它替代 pending 队列，同一工厂的 `my-server`/`my_server` 会触发
    // TS 加载期不会出现的命名空间冲突——工厂内必须用 `prepare_mcp_server` + commit。
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "my-server",
        &json!({ "command": "a" }),
    )
    .expect("immediate registration");
    let error = register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "my_server",
        &json!({ "command": "b" }),
    )
    .expect_err("eager write exposes the namespace sibling");
    assert_eq!(
        error.to_string(),
        "MCP server \"my_server\" conflicts with registered server \"my-server\""
    );
}

#[test]
fn prepare_still_rejects_names_owned_by_a_committed_server() {
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "a" }),
    )
    .expect("first registration");
    assert!(matches!(
        prepare_mcp_server(
            &registry,
            "/extensions/b.ts",
            "shared",
            &json!({ "command": "b" })
        ),
        Err(McpRegistrationError::AlreadyRegistered { .. })
    ));
}

#[test]
fn validation_runs_before_the_owner_check() {
    // TS 先 validate 再查 owner：非法配置即使名字被占用也报校验错误。
    let registry = new_registry();
    register_mcp_server(
        &registry,
        "/extensions/a.ts",
        "shared",
        &json!({ "command": "a" }),
    )
    .expect("first registration");
    let error = register_mcp_server(
        &registry,
        "/extensions/b.ts",
        "shared",
        &json!({ "args": ["no transport"] }),
    )
    .expect_err("invalid config");
    assert!(matches!(
        error,
        McpRegistrationError::Invalid {
            ref extension_path,
            ..
        } if extension_path == "/extensions/b.ts"
    ));
}
