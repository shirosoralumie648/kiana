//! 移植 TS `test/mcp-extension.test.ts` 的 `describe("MCP config")`（8 个 it）+ 直接文件格式测试。
//!
//! 错误消息、条目顺序、默认值折叠与重写字节都按 TS 源锁定。

mod mcp_core_support;

use std::fs;

use kiana_builtin_extensions::mcp::config::{
    LoadMcpConfigOptions, McpExposure, McpServerConfigPatch, McpServerScope,
    UpdateMcpServerConfigOptions, add_mcp_server_config, load_mcp_config, remove_mcp_server_config,
    update_mcp_server_config,
};
use mcp_core_support::{
    Paths, config_json, entry_names, entry_scopes, entry_tuples, read_json, read_text, setup,
    validate, write_json,
};
use serde_json::json;

/// Config values are resolved at connect time, so the literal reference must survive loading.
const TOKEN_HEADER: &str = "Bearer ${TOKEN}";

fn load(
    paths: &Paths,
    project_trusted: bool,
) -> kiana_builtin_extensions::mcp::config::LoadedMcpConfig {
    load_mcp_config(paths.options(project_trusted))
}

/// `expect.stringContaining`。
fn contains(errors: &[String], needle: &str) -> bool {
    errors.iter().any(|error| error.contains(needle))
}

#[test]
fn merges_global_and_trusted_project_servers_and_validates_entries() {
    let paths = setup(
        &json!({
            "mcpServers": {
                "shared": { "command": "global-cmd" },
                "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": TOKEN_HEADER } },
                "off": { "command": "x", "enabled": false },
                "bad": { "args": ["no command"] },
                "legacy": { "type": "sse", "url": "https://example.com/sse" },
                "badUrl": { "url": "example.com/mcp" },
                "bad name": { "command": "x" }
            }
        }),
        &json!({ "mcpServers": { "shared": { "command": "project-cmd", "exposure": "direct" } } }),
    );

    let trusted = load(&paths, true);
    // Disabled servers are kept so /mcp can enable them again.
    assert_eq!(
        entry_tuples(&trusted.servers),
        vec![
            (
                "shared".to_string(),
                Some(McpServerScope::Project),
                json!({ "command": "project-cmd", "exposure": "direct" })
            ),
            (
                "remote".to_string(),
                Some(McpServerScope::Global),
                json!({ "url": "https://example.com/mcp", "headers": { "Authorization": TOKEN_HEADER } })
            ),
            (
                "off".to_string(),
                Some(McpServerScope::Global),
                json!({ "command": "x", "enabled": false })
            ),
        ]
    );
    assert_eq!(trusted.errors.len(), 4);
    assert!(trusted.errors[0].contains("server \"bad\" needs either \"command\""));
    assert!(trusted.errors[1].contains("legacy SSE transport is not supported"));
    assert!(trusted.errors[2].contains("server \"badUrl\": url must be an http or https URL"));
    assert!(trusted.errors[3].contains("invalid server name \"bad name\""));

    // Untrusted projects cannot add or override servers, since stdio servers run commands.
    let untrusted = load(&paths, false);
    let shared = untrusted
        .servers
        .iter()
        .find(|server| server.name == "shared")
        .expect("shared server");
    assert_eq!(
        config_json(&shared.config),
        json!({ "command": "global-cmd" })
    );
}

#[test]
fn lets_project_entries_override_enabled_and_exposure_of_global_servers() {
    let paths = setup(
        &json!({ "mcpServers": { "tools": { "command": "x", "env": { "TOKEN": "secret" } } } }),
        // An override cannot change the command, which would run with the global env.
        &json!({ "mcpServers": {
            "tools": { "enabled": false, "args": ["y"] },
            "missing": { "enabled": false }
        } }),
    );
    let project = paths.project();
    let loaded = load(&paths, true);
    assert_eq!(
        loaded
            .servers
            .iter()
            .map(|server| (
                server.name.clone(),
                server.override_.clone(),
                config_json(&server.config)
            ))
            .collect::<Vec<_>>(),
        vec![(
            "tools".to_string(),
            None,
            json!({ "command": "x", "env": { "TOKEN": "secret" } })
        )]
    );
    assert_eq!(loaded.errors.len(), 2);
    assert!(
        loaded.errors[0]
            .contains("server \"tools\": an override can only set enabled, exposure, toolExposure")
    );
    assert!(loaded.errors[1].contains(
        "server \"missing\" needs \"command\" or \"url\", or a global server to override"
    ));

    write_json(
        &project,
        &json!({ "mcpServers": { "tools": { "enabled": false } } }),
    );
    let reloaded = load(&paths, true);
    let tools = &reloaded.servers[0];
    assert_eq!(
        tools.override_.as_deref(),
        Some(project.to_string_lossy().as_ref())
    );
    assert_eq!(
        config_json(&tools.config),
        json!({ "command": "x", "env": { "TOKEN": "secret" }, "enabled": false })
    );

    // Overrides keep `enabled: true`, since it replaces the global value.
    update_mcp_server_config(
        &project,
        "tools",
        &McpServerConfigPatch {
            enabled: Some(true),
            exposure: None,
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect("update override");
    assert_eq!(
        read_json(&project)["mcpServers"]["tools"],
        json!({ "enabled": true })
    );
    assert_eq!(
        read_text(&project),
        "{\n  \"mcpServers\": {\n    \"tools\": {\n      \"enabled\": true\n    }\n  }\n}\n"
    );
}

// Regression: #11191.
#[test]
fn reads_and_edits_mcp_json_files_that_start_with_a_utf8_bom() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    let project = paths.project();
    fs::write(
        &global,
        "\u{FEFF}{\"mcpServers\":{\"demo\":{\"command\":\"echo\",\"enabled\":false}}}\n",
    )
    .expect("write global");
    fs::write(
        &project,
        "\u{FEFF}{\"mcpServers\":{\"local\":{\"command\":\"local-cmd\"}}}\n",
    )
    .expect("write project");

    let loaded = load(&paths, true);
    assert_eq!(loaded.errors, Vec::<String>::new());
    assert_eq!(
        entry_scopes(&loaded.servers),
        vec![
            ("demo".to_string(), Some(McpServerScope::Global)),
            ("local".to_string(), Some(McpServerScope::Project)),
        ]
    );

    let other = validate("other", &json!({ "command": "other-cmd" }));
    assert!(!add_mcp_server_config(&global, "other", &other).expect("add other"));
    assert_eq!(
        read_json(&global)["mcpServers"],
        json!({
            "demo": { "command": "echo", "enabled": false },
            "other": { "command": "other-cmd" }
        })
    );
    // BOM-free, two-space indent, exactly one trailing newline.
    assert_eq!(
        read_text(&global),
        "{\n  \"mcpServers\": {\n    \"demo\": {\n      \"command\": \"echo\",\n      \"enabled\": false\n    },\n    \"other\": {\n      \"command\": \"other-cmd\"\n    }\n  }\n}\n"
    );
}

// Regression: #10239.
#[test]
fn rejects_server_names_that_differ_only_in_dash_and_underscore() {
    let paths = setup(
        &json!({ "mcpServers": { "work-files": { "command": "a" }, "work_files": { "command": "b" } } }),
        &json!({}),
    );
    let loaded = load(&paths, false);
    assert_eq!(entry_names(&loaded.servers), vec!["work-files"]);
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].contains("server \"work_files\" conflicts with \"work-files\""));
}

#[test]
fn validates_exposure_and_reads_auto_enable_codemode_with_project_precedence() {
    let paths = setup(
        &json!({
            "autoEnableCodemode": false,
            "mcpServers": {
                "later": { "command": "x", "exposure": "deferred" },
                // `codemode-deferred` is an alias for `codemode`.
                "scripts": { "command": "x", "exposure": "codemode-deferred", "toolExposure": { "a": "codemode-deferred" } },
                "off": { "command": "x", "exposure": "hidden" },
                "wrong": { "command": "x", "exposure": "model-only" },
                "described": { "command": "x", "description": "Docs search" },
                "badDescription": { "command": "x", "description": 1 }
            }
        }),
        &json!({ "autoEnableCodemode": "yes", "mcpServers": {} }),
    );

    let untrusted = load(&paths, false);
    assert_eq!(untrusted.auto_enable_codemode, Some(false));
    assert_eq!(
        entry_names(&untrusted.servers),
        vec!["later", "scripts", "off", "described"]
    );
    assert_eq!(
        untrusted
            .servers
            .iter()
            .map(|server| server.config.base().exposure)
            .collect::<Vec<_>>(),
        vec![
            Some(McpExposure::Deferred),
            Some(McpExposure::Codemode),
            Some(McpExposure::Hidden),
            None
        ]
    );
    assert_eq!(
        untrusted.servers[1]
            .config
            .base()
            .tool_exposure
            .as_ref()
            .and_then(|overrides| overrides.get("a"))
            .copied(),
        Some(McpExposure::Codemode)
    );
    assert_eq!(
        untrusted.servers[3].config.base().description.as_deref(),
        Some("Docs search")
    );
    assert_eq!(untrusted.errors.len(), 2);
    assert!(untrusted.errors[0].contains("server \"wrong\": exposure must be one of"));
    assert!(
        untrusted.errors[1].contains("server \"badDescription\": description must be a string")
    );

    let trusted = load(&paths, true);
    assert_eq!(trusted.auto_enable_codemode, Some(false));
    assert!(contains(
        &trusted.errors,
        "autoEnableCodemode must be a boolean"
    ));
}

#[test]
fn validates_the_oauth_callback_url_scope_and_client_name() {
    let paths = setup(
        &json!({
            "mcpServers": {
                "ok": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://localhost:8080/callback", "scope": "a b" } },
                "ipv6": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://[::1]/cb", "callbackPort": 9000 } },
                "same": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://127.0.0.1:2/cb", "callbackPort": 2 } },
                "remote": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "https://example.com/callback" } },
                "both": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://127.0.0.1:1/cb", "callbackPort": 2 } },
                "scope": { "url": "https://a.example/mcp", "oauth": { "scope": ["a"] } },
                "named": { "url": "https://a.example/mcp", "oauth": { "clientName": "Claude Code" } },
                "unnamed": { "url": "https://a.example/mcp", "oauth": { "clientName": " " } },
                "metadata": { "url": "https://a.example/mcp", "oauth": { "authServerMetadataUrl": "https://idp.example/m" } },
                "plainMetadata": { "url": "https://a.example/mcp", "oauth": { "authServerMetadataUrl": "http://idp.example/m" } },
                // #10302
                "cimd": { "url": "https://a.example/mcp", "oauth": { "clientRegistration": "cimd", "callbackUrl": "http://localhost/callback" } },
                "badRegistration": { "url": "https://a.example/mcp", "oauth": { "clientRegistration": "auto" } },
                "cimdClient": { "url": "https://a.example/mcp", "oauth": { "clientRegistration": "cimd", "clientId": "x" } },
                "cimdPath": { "url": "https://a.example/mcp", "oauth": { "clientRegistration": "cimd", "callbackUrl": "http://127.0.0.1/cb" } }
            }
        }),
        &json!({}),
    );
    let loaded = load(&paths, false);
    assert_eq!(
        entry_names(&loaded.servers),
        vec!["ok", "ipv6", "same", "named", "metadata", "cimd"]
    );
    assert_eq!(loaded.errors.len(), 8);
    let expected = [
        "server \"remote\": oauth.callbackUrl must be an http URI on localhost",
        "server \"both\": oauth.callbackUrl and oauth.callbackPort name different ports",
        "server \"scope\": oauth.scope must be a string",
        "server \"unnamed\": oauth.clientName must be a non-empty string",
        "server \"plainMetadata\": oauth.authServerMetadataUrl must be an https URL",
        "server \"badRegistration\": oauth.clientRegistration must be \"dcr\" or \"cimd\"",
        "server \"cimdClient\": oauth.clientRegistration \"cimd\" cannot be combined",
        "server \"cimdPath\": oauth.clientRegistration \"cimd\" requires oauth.callbackUrl",
    ];
    for (index, needle) in expected.iter().enumerate() {
        assert!(
            loaded.errors[index].contains(needle),
            "error {index}: {} does not contain {needle}",
            loaded.errors[index]
        );
    }
}

#[test]
fn resolves_per_tool_exposure_from_exact_names_then_patterns_in_order() {
    let paths = setup(
        &json!({
            "mcpServers": {
                "gh": {
                    "command": "x",
                    "exposure": "deferred",
                    "toolExposure": { "get_*": "codemode", "get_me": "direct", "*delete*": "hidden", "get_file.*": "direct" }
                },
                "bad": { "command": "x", "toolExposure": { "a": "visible" } }
            }
        }),
        &json!({}),
    );
    let loaded = load(&paths, false);
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].contains("server \"bad\": toolExposure \"a\" must be one of"));
    let config = &loaded.servers[0].config;
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(config, "get_me"),
        McpExposure::Direct
    );
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(config, "get_issue"),
        McpExposure::Codemode
    );
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(config, "get_delete_hint"),
        McpExposure::Codemode
    );
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(config, "delete_repo"),
        McpExposure::Hidden
    );
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(config, "list_issues"),
        McpExposure::Deferred
    );
    // Only `*` is special.
    let file = validate(
        "file",
        &json!({ "command": "x", "toolExposure": { "get_file.*": "direct" } }),
    );
    assert_eq!(
        kiana_core::mcp_servers::get_mcp_tool_exposure(&file, "get_file_x"),
        McpExposure::Codemode
    );
}

#[test]
fn validates_provider_auth_and_accepts_it_only_in_the_global_mcp_json() {
    let paths = setup(
        &json!({
            "mcpServers": {
                "radius": { "url": "https://radius.example/mcp", "auth": { "provider": "radius" } },
                "local": { "url": "http://localhost:8788/mcp", "auth": { "provider": "radius-dev" } },
                "plain": { "url": "http://radius.example/mcp", "auth": { "provider": "radius" } },
                "empty": { "url": "https://radius.example/mcp", "auth": { "provider": "" } }
            }
        }),
        &json!({ "mcpServers": { "radius": { "url": "https://evil.example/mcp", "auth": { "provider": "radius" } } } }),
    );
    let loaded = load(&paths, true);
    // The project entry cannot replace the global one: it would send the credential to its own URL.
    assert_eq!(
        entry_tuples(&loaded.servers)
            .into_iter()
            .map(|(name, scope, config)| (name, scope, config["url"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                "radius".to_string(),
                Some(McpServerScope::Global),
                json!("https://radius.example/mcp")
            ),
            (
                "local".to_string(),
                Some(McpServerScope::Global),
                json!("http://localhost:8788/mcp")
            ),
        ]
    );
    assert_eq!(loaded.errors.len(), 3);
    assert!(loaded.errors[0].contains("server \"plain\": auth requires an https URL"));
    assert!(loaded.errors[1].contains("server \"empty\": auth.provider must be a provider name"));
    assert!(
        loaded.errors[2].contains("server \"radius\": auth is only allowed in the global mcp.json")
    );
}

#[test]
fn project_config_is_present_even_when_the_project_file_is_absent() {
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    fs::create_dir_all(&agent_dir).expect("agent dir");
    let cwd = root.path().join("project");
    fs::create_dir_all(&cwd).expect("project dir");
    fs::write(agent_dir.join("mcp.json"), "{\"mcpServers\":{}}").expect("write global");

    let options = LoadMcpConfigOptions {
        agent_dir: agent_dir.clone(),
        cwd: cwd.clone(),
        project_trusted: true,
    };
    let loaded = load_mcp_config(options);
    assert_eq!(
        loaded.project_config,
        Some(
            cwd.join(".kiana")
                .join("mcp.json")
                .to_string_lossy()
                .into_owned()
        )
    );
    assert!(!cwd.join(".kiana").join("mcp.json").exists());

    let untrusted = load_mcp_config(LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: false,
    });
    assert_eq!(untrusted.project_config, None);
}

#[test]
fn rewrites_with_detected_indentation_and_preserves_unknown_keys() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::write(
        &global,
        "{\n\t\"keep\": true,\n\t\"mcpServers\": {\n\t\t\"demo\": { \"command\": \"echo\", \"custom\": 1 }\n\t}\n}\n",
    )
    .expect("write tab-indented file");

    let other = validate("other", &json!({ "command": "other-cmd" }));
    add_mcp_server_config(&global, "other", &other).expect("add");
    assert_eq!(
        read_text(&global),
        "{\n\t\"keep\": true,\n\t\"mcpServers\": {\n\t\t\"demo\": {\n\t\t\t\"command\": \"echo\",\n\t\t\t\"custom\": 1\n\t\t},\n\t\t\"other\": {\n\t\t\t\"command\": \"other-cmd\"\n\t\t}\n\t}\n}\n"
    );
}

#[test]
fn add_creates_missing_files_and_reports_replacement() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("nested").join("agent").join("mcp.json");
    let demo = validate("demo", &json!({ "command": "echo" }));
    assert!(!add_mcp_server_config(&path, "demo", &demo).expect("create"));
    assert_eq!(
        read_json(&path),
        json!({ "mcpServers": { "demo": { "command": "echo" } } })
    );
    let other = validate("demo", &json!({ "command": "other" }));
    assert!(add_mcp_server_config(&path, "demo", &other).expect("replace"));
    assert_eq!(
        read_json(&path)["mcpServers"]["demo"],
        json!({ "command": "other" })
    );
}

#[test]
fn remove_does_not_create_files_and_reports_removal() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("mcp.json");
    assert!(!remove_mcp_server_config(&path, "demo").expect("missing file"));
    assert!(!path.exists());

    let paths = setup(
        &json!({ "mcpServers": { "demo": { "command": "echo" }, "keep": { "command": "x" } } }),
        &json!({}),
    );
    let global = paths.global();
    let before = read_text(&global);
    assert!(!remove_mcp_server_config(&global, "absent").expect("absent entry"));
    assert_eq!(
        read_text(&global),
        before,
        "no write when nothing is removed"
    );

    assert!(remove_mcp_server_config(&global, "demo").expect("remove"));
    assert_eq!(
        read_json(&global)["mcpServers"],
        json!({ "keep": { "command": "x" } })
    );
}

#[test]
fn update_removes_default_values_only_on_non_override_entries() {
    let paths = setup(
        &json!({ "mcpServers": { "demo": { "command": "x" } } }),
        &json!({}),
    );
    let global = paths.global();
    update_mcp_server_config(
        &global,
        "demo",
        &McpServerConfigPatch {
            enabled: Some(true),
            exposure: Some(McpExposure::Codemode),
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect("fold defaults");
    assert_eq!(
        read_json(&global)["mcpServers"]["demo"],
        json!({ "command": "x" })
    );

    update_mcp_server_config(
        &global,
        "demo",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: Some(McpExposure::Hidden),
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect("set values");
    assert_eq!(
        read_json(&global)["mcpServers"]["demo"],
        json!({ "command": "x", "enabled": false, "exposure": "hidden" })
    );

    let error = update_mcp_server_config(
        &global,
        "absent",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect_err("missing server");
    assert_eq!(
        error.to_string(),
        format!(
            "{} does not define MCP server \"absent\"",
            global.to_string_lossy()
        )
    );
}

#[test]
fn update_with_override_adds_a_missing_entry_as_an_override() {
    let paths = setup(&json!({}), &json!({}));
    let project = paths.project();
    update_mcp_server_config(
        &project,
        "demo",
        &McpServerConfigPatch {
            enabled: Some(true),
            exposure: Some(McpExposure::Codemode),
        },
        UpdateMcpServerConfigOptions { override_: true },
    )
    .expect("add override");
    assert_eq!(
        read_json(&project)["mcpServers"]["demo"],
        json!({ "enabled": true, "exposure": "codemode" })
    );
}

#[test]
fn reports_invalid_json_and_invalid_shapes() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::write(&global, "{oops").expect("write invalid");
    let loaded = load(&paths, false);
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].starts_with(&global.to_string_lossy().to_string()));
    assert!(loaded.errors[0].contains("JSON"));

    let demo = validate("demo", &json!({ "command": "echo" }));
    let error = add_mcp_server_config(&global, "demo", &demo).expect_err("invalid json");
    assert!(error.to_string().contains("JSON"));

    fs::write(&global, "[]").expect("write array");
    let loaded = load(&paths, false);
    assert!(loaded.errors[0].contains("expected an object with an \"mcpServers\" object"));
    let error = add_mcp_server_config(&global, "demo", &demo).expect_err("array root");
    assert!(
        error
            .to_string()
            .contains("expected an object with an \"mcpServers\" object")
    );
}

#[test]
fn update_requires_the_file_to_define_an_object_entry() {
    let paths = setup(&json!({ "mcpServers": { "demo": 5 } }), &json!({}));
    let global = paths.global();
    let error = update_mcp_server_config(
        &global,
        "demo",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect_err("non-object entry");
    assert!(error.to_string().contains(&format!(
        "{} does not define MCP server \"demo\"",
        global.to_string_lossy()
    )));
}

#[test]
fn keeps_exactly_one_trailing_newline() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::write(&global, "{\"mcpServers\":{}}\n\n\n").expect("write");
    let demo = validate("demo", &json!({ "command": "echo" }));
    add_mcp_server_config(&global, "demo", &demo).expect("add");
    let text = read_text(&global);
    assert!(text.ends_with("}\n"));
    assert!(!text.ends_with("\n\n"));
}

#[test]
fn truncates_the_detected_indent_to_ten_utf16_units_like_json_stringify() {
    // JS 的字符串 space 只取前 10 个字符：12 个空格的缩进被截成 10 个。
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::write(
        &global,
        "{\n            \"mcpServers\": {\n                        \"a\": {\"command\":\"x\"}\n            }\n}\n",
    )
    .expect("write");
    let other = validate("b", &json!({ "command": "other-cmd" }));
    add_mcp_server_config(&global, "b", &other).expect("add");
    assert_eq!(
        read_text(&global),
        "{\n          \"mcpServers\": {\n                    \"a\": {\n                              \"command\": \"x\"\n                    },\n                    \"b\": {\n                              \"command\": \"other-cmd\"\n                    }\n          }\n}\n"
    );
}

#[test]
fn serializes_js_numbers_in_preserved_unknown_keys() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::write(
        &global,
        "{\"mcpServers\":{\"keep\":{\"command\":\"echo\",\"timeout\":60.0,\"custom\":1e21}}}",
    )
    .expect("write");
    let demo = validate("demo", &json!({ "command": "other" }));
    add_mcp_server_config(&global, "demo", &demo).expect("add");
    let text = read_text(&global);
    assert!(text.contains("\"custom\": 1e+21"), "{text}");
    assert!(!text.contains("60.0"), "{text}");
}

/// 评审回归：项目 `auth` 检查看校验后对象上的 `url` 键（`"url" in config && config.auth`），
/// 不是类型化的 HTTP 变体。stdio 条目保留 `url` 时同样被拒。
#[test]
fn rejects_project_auth_for_any_entry_that_keeps_a_url_key() {
    let paths = setup(
        &json!({}),
        &json!({ "mcpServers": {
            "stdioWithUrl": {
                "type": "stdio",
                "command": "x",
                "url": "https://e.com/mcp",
                "auth": { "provider": "p" }
            }
        } }),
    );
    let loaded = load(&paths, true);
    assert!(loaded.servers.is_empty());
    assert_eq!(
        loaded.errors,
        vec![format!(
            "{}: server \"stdioWithUrl\": auth is only allowed in the global mcp.json",
            paths.project().to_string_lossy()
        )]
    );

    // `"url" in config` 只看键是否存在：值不是字符串也一样。
    let paths = setup(
        &json!({}),
        &json!({ "mcpServers": {
            "numberUrl": { "command": "x", "url": 5, "auth": { "provider": "p" } }
        } }),
    );
    let loaded = load(&paths, true);
    assert!(loaded.servers.is_empty());
    assert_eq!(
        loaded.errors,
        vec![format!(
            "{}: server \"numberUrl\": auth is only allowed in the global mcp.json",
            paths.project().to_string_lossy()
        )]
    );

    // 假值的 `auth`（0/false/""/null）不触发。
    let paths = setup(
        &json!({}),
        &json!({ "mcpServers": {
            "falsy": { "command": "x", "url": 5, "auth": 0 },
            "empty": { "command": "x", "url": 5, "auth": "" }
        } }),
    );
    let loaded = load(&paths, true);
    assert_eq!(
        entry_names(&loaded.servers),
        vec!["falsy".to_string(), "empty".to_string()]
    );
    assert!(loaded.errors.is_empty());
}

/// 评审回归：`mcp.json` 编辑函数按 JS 普通对象语义查找/写入/删除（Object.prototype）。
#[test]
fn follows_object_prototype_semantics_for_server_names() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    let config = validate("x", &json!({ "command": "x" }));

    // `target["__proto__"] = config` 改的是原型（不是自有键）：条目丢失，但 `replaced` 为 true。
    assert!(add_mcp_server_config(&global, "__proto__", &config).expect("add __proto__"));
    assert_eq!(read_text(&global), "{\n  \"mcpServers\": {}\n}\n");

    // 其它继承属性是 `undefined` 之外的函数：`replaced` 为 true，赋值创建自有键。
    assert!(add_mcp_server_config(&global, "constructor", &config).expect("add constructor"));
    assert_eq!(
        read_text(&global),
        "{\n  \"mcpServers\": {\n    \"constructor\": {\n      \"command\": \"x\"\n    }\n  }\n}\n"
    );

    // 自有键按普通删除处理。
    assert!(remove_mcp_server_config(&global, "constructor").expect("remove constructor"));
    assert_eq!(read_text(&global), "{\n  \"mcpServers\": {}\n}\n");

    // 只有继承属性时 `delete` 是空操作，但 TS 仍返回 true 并重写文件。
    fs::write(
        &global,
        "{\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"x\"\n    }\n  }\n}\n",
    )
    .expect("write");
    let before = read_text(&global);
    assert!(remove_mcp_server_config(&global, "toString").expect("remove toString"));
    assert_eq!(read_text(&global), before);

    // 没有自有 `__proto__` 时它是 `Object.prototype`（记录）：更新成功、重写文件，但不改条目。
    update_mcp_server_config(
        &global,
        "__proto__",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect("update __proto__");
    assert_eq!(read_text(&global), before);

    // 其余继承属性不是记录 → 与 TS 相同的报错。
    let error = update_mcp_server_config(
        &global,
        "toString",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions::default(),
    )
    .expect_err("toString is not a record");
    assert_eq!(
        error.to_string(),
        format!(
            "{} does not define MCP server \"toString\"",
            global.to_string_lossy()
        )
    );

    // 带 override 时：mcpServers 存在则继承属性仍不是缺失 → 报错；文件缺失时才是缺失。
    let error = update_mcp_server_config(
        &global,
        "constructor",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions { override_: true },
    )
    .expect_err("inherited constructor");
    assert_eq!(
        error.to_string(),
        format!(
            "{} does not define MCP server \"constructor\"",
            global.to_string_lossy()
        )
    );

    let fresh = paths.agent_dir.join("fresh.json");
    update_mcp_server_config(
        &fresh,
        "constructor",
        &McpServerConfigPatch {
            enabled: Some(false),
            exposure: None,
        },
        UpdateMcpServerConfigOptions { override_: true },
    )
    .expect("override on a missing file");
    assert_eq!(
        read_text(&fresh),
        "{\n  \"mcpServers\": {\n    \"constructor\": {\n      \"enabled\": false\n    }\n  }\n}\n"
    );
}

/// 评审回归：文件 IO 错误用 Node 的 errno 文案（`readFileSync`/`mkdirSync`/`writeFileSync`）。
#[test]
fn reports_file_errors_with_node_messages() {
    let paths = setup(&json!({}), &json!({}));
    let global = paths.global();
    fs::remove_file(&global).expect("remove mcp.json");
    fs::create_dir(&global).expect("make mcp.json a directory");
    // `readFileSync` 在打开后读取时失败：EISDIR 且不带路径。
    let loaded = load(&paths, false);
    assert_eq!(
        loaded.errors,
        vec![format!(
            "{}: EISDIR: illegal operation on a directory, read",
            global.to_string_lossy()
        )]
    );
    assert!(entry_names(&loaded.servers).is_empty());

    // `mkdirSync(dirname(path), { recursive: true })` 的 ENOTDIR / EEXIST。
    let blocker = paths.agent_dir.join("blocker");
    fs::write(&blocker, "x").expect("write blocker");
    let error = add_mcp_server_config(
        &blocker.join("sub").join("mcp.json"),
        "demo",
        &validate("demo", &json!({ "command": "x" })),
    )
    .expect_err("parent is a file");
    assert_eq!(
        error.to_string(),
        format!(
            "ENOTDIR: not a directory, mkdir '{}'",
            blocker.join("sub").to_string_lossy()
        )
    );

    let error = add_mcp_server_config(
        &blocker.join("mcp.json"),
        "demo",
        &validate("demo", &json!({ "command": "x" })),
    )
    .expect_err("parent is a file");
    assert_eq!(
        error.to_string(),
        format!(
            "EEXIST: file already exists, mkdir '{}'",
            blocker.to_string_lossy()
        )
    );
}
