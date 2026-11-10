//! `extensions-discovery.test.ts`（31 个用例）的 Rust 移植。
//!
//! 每个用例的标题沿用移植源 `extensions-discovery.test.ts` 的命名（便于对照）；加载走真实的 `discoverAndLoadExtensions` 流水线
//! （`loader.ts` 的发现规则 + `rquickjs` 模块 loader + `js/api.js` 的 `kiana` 对象）。
//!
//! 与 TS 的一处已记录差异：`does not infer package ownership from ancestor manifests`
//! 在 TS 里解析到祖先 `node_modules` 的物理包；Rust 设计（`design-extension-runtime.md`
//! §3.8 第 1 步）让 22 个虚拟键永远优先，因此扩展拿到宿主模块，`physicalDependency`
//! 为 `undefined`。本测试按 Rust 架构断言（无错误、无警告、成功加载），并在交付报告中单列。
#![cfg(feature = "runtime")]

#[path = "fixtures/extensions/support.rs"]
mod support;

use std::path::{Path, PathBuf};

use kiana_ext_api::types::LoadExtensionsResult;
use kiana_extensions::loader::{discover_and_load_extensions, load_extensions};
use support::{COMMAND_EXTENSION, LoaderHarness, temp_root, tool_extension, write_file};

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

async fn discover(
    _harness: &LoaderHarness,
    root: &Path,
    configured: &[String],
) -> LoadExtensionsResult {
    discover_and_load_extensions(
        configured,
        &path_string(root),
        Some(&path_string(root)),
        None,
    )
    .await
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_direct_ts_files_in_extensions() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let extensions = root.path().join("extensions");
    write_file(&extensions, "foo.ts", COMMAND_EXTENSION);
    write_file(&extensions, "bar.ts", COMMAND_EXTENSION);

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 2);
    let mut names: Vec<String> = result
        .extensions
        .iter()
        .map(|extension| basename(&extension.path))
        .collect();
    names.sort();
    assert_eq!(names, vec!["bar.ts".to_string(), "foo.ts".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn loads_the_coding_agent_entrypoint_without_rewriting_kiana_ai_provider_subpaths() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/coding-agent-import.ts",
        r#"
import { getAgentDir } from "@earendil-works/kiana-coding-agent";
void getAgentDir;
export default function(kiana) {
    kiana.registerCommand("test", { handler: async () => {} });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_infer_package_ownership_from_ancestor_manifests() {
    // Regression for #9863（Rust 侧断言见文件头注释）。
    let harness = LoaderHarness::new();
    let root = temp_root();
    let dependency_dir = root
        .path()
        .join("node_modules")
        .join("@earendil-works")
        .join("kiana-coding-agent");
    std::fs::create_dir_all(&dependency_dir).expect("create dependency dir");
    write_file(
        root.path(),
        "package.json",
        &serde_json::json!({
            "name": "application",
            "type": "module",
            "dependencies": { "@earendil-works/kiana-coding-agent": "1.0.0" },
        })
        .to_string(),
    );
    write_file(
        &dependency_dir,
        "package.json",
        &serde_json::json!({
            "name": "@earendil-works/kiana-coding-agent",
            "type": "module",
            "exports": "./index.js",
        })
        .to_string(),
    );
    write_file(
        &dependency_dir,
        "index.js",
        "export const physicalDependency = true;",
    );
    write_file(
        root.path(),
        "extensions/compiled-esm-extension.js",
        r#"
import * as host from "@earendil-works/kiana-coding-agent";
export default function(kiana) {
    // 虚拟键（宿主模块）胜出：`getAgentDir` 存在。
    if (typeof host.getAgentDir === "function") kiana.registerCommand("host-module", { handler: async () => {} });
    // 祖先 `node_modules` 的物理包**没有**被选中：它的导出是 undefined。
    if (host.physicalDependency === true) kiana.registerCommand("physical-dependency", { handler: async () => {} });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    // Rust 架构（`design-extension-runtime.md` §3.8 第 1 步）让虚拟键优先，祖先
    // `node_modules` 的物理包被宿主模块遮蔽；TS 会解析到物理包。两条断言分别锁定
    // “宿主模块可用”与“物理包未被选中”，任一方向回归都会失败。
    assert!(
        result.extensions[0].commands.contains_key("host-module"),
        "virtual module must shadow the ancestor package"
    );
    assert!(
        !result.extensions[0]
            .commands
            .contains_key("physical-dependency"),
        "ancestor node_modules package must not be resolved"
    );
    assert_eq!(result.warnings, Some(Vec::new()));
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_the_type_only_kiana_ai_oauth_compatibility_barrel_resolvable() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/oauth-import.ts",
        r#"
import * as oauth from "@earendil-works/kiana-ai/oauth";
void oauth;
export default function(kiana) {
    kiana.registerCommand("test", { handler: async () => {} });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_direct_js_files_in_extensions() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(root.path(), "extensions/foo.js", COMMAND_EXTENSION);

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(basename(&result.extensions[0].path), "foo.js");
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_subdirectory_with_index_ts() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-extension/index.ts",
        COMMAND_EXTENSION,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("my-extension"));
    assert!(result.extensions[0].path.contains("index.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_subdirectory_with_index_js() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-extension/index.js",
        COMMAND_EXTENSION,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.js"));
}

#[tokio::test(flavor = "current_thread")]
async fn prefers_index_ts_over_index_js() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-extension/index.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-extension/index.js",
        COMMAND_EXTENSION,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_subdirectory_with_package_json_kiana_field() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-package/src/main.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-package/package.json",
        &serde_json::json!({
            "name": "my-package",
            "kiana": { "extensions": ["./src/main.ts"] },
        })
        .to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("src"));
    assert!(result.extensions[0].path.contains("main.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_package_json_kiana_extension_entries_with_leading_tilde_package_relative() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let direct = write_file(
        root.path(),
        "extensions/tilde-package/~entry.ts",
        COMMAND_EXTENSION,
    );
    let slash = write_file(
        root.path(),
        "extensions/tilde-package/~/entry.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/tilde-package/package.json",
        &serde_json::json!({
            "name": "tilde-package",
            "kiana": { "extensions": ["~entry.ts", "~/entry.ts"] },
        })
        .to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    let mut paths: Vec<String> = result
        .extensions
        .iter()
        .map(|extension| extension.path.clone())
        .collect();
    paths.sort();
    let mut expected = vec![path_string(&direct), path_string(&slash)];
    expected.sort();
    assert_eq!(paths, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn package_json_can_declare_multiple_extensions() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-package/ext1.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-package/ext2.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-package/package.json",
        &serde_json::json!({
            "name": "my-package",
            "kiana": { "extensions": ["./ext1.ts", "./ext2.ts"] },
        })
        .to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn package_json_with_kiana_field_takes_precedence_over_index_ts() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-package/index.ts",
        &tool_extension("from-index"),
    );
    write_file(
        root.path(),
        "extensions/my-package/custom.ts",
        &tool_extension("from-custom"),
    );
    write_file(
        root.path(),
        "extensions/my-package/package.json",
        &serde_json::json!({
            "name": "my-package",
            "kiana": { "extensions": ["./custom.ts"] },
        })
        .to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("custom.ts"));
    assert!(result.extensions[0].tools.contains_key("from-custom"));
    assert!(!result.extensions[0].tools.contains_key("from-index"));
}

#[tokio::test(flavor = "current_thread")]
async fn ignores_package_json_without_kiana_field_falls_back_to_index_ts() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-package/index.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-package/package.json",
        &serde_json::json!({ "name": "my-package", "version": "1.0.0" }).to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn ignores_subdirectory_without_index_or_package_json() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/not-an-extension/helper.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/not-an-extension/utils.ts",
        COMMAND_EXTENSION,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_recurse_beyond_one_level() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/container/nested/index.ts",
        COMMAND_EXTENSION,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn handles_mixed_direct_files_and_subdirectories() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(root.path(), "extensions/direct.ts", COMMAND_EXTENSION);
    write_file(
        root.path(),
        "extensions/with-index/index.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/with-manifest/entry.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/with-manifest/package.json",
        &serde_json::json!({ "kiana": { "extensions": ["./entry.ts"] } }).to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn skips_non_existent_paths_declared_in_package_json() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/my-package/exists.ts",
        COMMAND_EXTENSION,
    );
    write_file(
        root.path(),
        "extensions/my-package/package.json",
        &serde_json::json!({
            "kiana": { "extensions": ["./exists.ts", "./missing.ts"] },
        })
        .to_string(),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("exists.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn loads_extensions_and_registers_commands() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(root.path(), "extensions/with-command.ts", COMMAND_EXTENSION);

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].commands.contains_key("test"));
}

#[tokio::test(flavor = "current_thread")]
async fn loads_extensions_and_registers_tools() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/with-tool.ts",
        &tool_extension("my-tool"),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].tools.contains_key("my-tool"));
}

#[tokio::test(flavor = "current_thread")]
async fn reports_errors_for_invalid_extension_code() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/invalid.ts",
        "this is not valid typescript export",
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].path.contains("invalid.ts"));
    assert_eq!(result.extensions.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn handles_explicitly_configured_paths() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let custom = write_file(root.path(), "custom-location/my-ext.ts", COMMAND_EXTENSION);

    let result = discover(&harness, root.path(), &[path_string(&custom)]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("my-ext.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_dependencies_from_extension_s_own_node_modules() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extensions/with-deps");

    let result = discover(&harness, root.path(), &[path_string(&fixture)]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("with-deps"));
    assert!(result.extensions[0].tools.contains_key("parse_duration"));
}

#[tokio::test(flavor = "current_thread")]
async fn registers_message_and_entry_renderers() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/with-renderer.ts",
        r#"
export default function(kiana) {
    kiana.registerMarkdownTransformer((markdown) => {
        return markdown;
    });
    kiana.registerMessageRenderer("my-custom-type", (message, options, theme) => {
        return null; // Use default rendering
    });
    kiana.registerEntryRenderer("my-entry-type", (entry, options, theme) => {
        return null;
    });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].markdown_transformer.is_some());
    assert!(
        result.extensions[0]
            .message_renderers
            .contains_key("my-custom-type")
    );
    assert!(
        result.extensions[0]
            .entry_renderers
            .as_ref()
            .is_some_and(|renderers| renderers.contains_key("my-entry-type"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reports_error_when_extension_throws_during_initialization() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/throws.ts",
        r#"
export default function(kiana) {
    throw new Error("Initialization failed!");
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].error.contains("Initialization failed!"));
    assert_eq!(result.extensions.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn reports_error_when_extension_has_no_default_export() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/no-default.ts",
        r#"
export function notDefault(kiana) {
    kiana.registerCommand("test", { handler: async () => {} });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0]
            .error
            .contains("does not export a valid factory function")
    );
    assert_eq!(result.extensions.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn allows_multiple_extensions_to_register_different_tools() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/tool-a.ts",
        &tool_extension("tool-a"),
    );
    write_file(
        root.path(),
        "extensions/tool-b.ts",
        &tool_extension("tool-b"),
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 2);
    let mut names: Vec<String> = result
        .extensions
        .iter()
        .flat_map(|extension| extension.tools.keys().cloned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["tool-a".to_string(), "tool-b".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn loads_extension_with_event_handlers() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/with-handlers.ts",
        r#"
export default function(kiana) {
    kiana.on("agent_start", async () => {});
    kiana.on("tool_call", async (event) => undefined);
    kiana.on("agent_end", async () => {});
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].handlers.contains_key("agent_start"));
    assert!(result.extensions[0].handlers.contains_key("tool_call"));
    assert!(result.extensions[0].handlers.contains_key("agent_end"));
}

#[tokio::test(flavor = "current_thread")]
async fn loads_extension_with_shortcuts() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/with-shortcut.ts",
        r#"
export default function(kiana) {
    kiana.registerShortcut("ctrl+t", {
        description: "Test shortcut",
        handler: async (ctx) => {},
    });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].shortcuts.contains_key("ctrl+t"));
}

#[tokio::test(flavor = "current_thread")]
async fn loads_extension_with_flags() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/with-flag.ts",
        r#"
export default function(kiana) {
    kiana.registerFlag("my-flag", {
        description: "My custom flag",
        handler: async (value) => {},
    });
}
"#,
    );

    let result = discover(&harness, root.path(), &[]).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].flags.contains_key("my-flag"));
}

#[tokio::test(flavor = "current_thread")]
async fn load_extensions_only_loads_explicit_paths_without_discovery() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/discovered.ts",
        &tool_extension("discovered"),
    );
    let explicit = write_file(root.path(), "explicit.ts", &tool_extension("explicit"));

    let result = load_extensions(
        &[path_string(&explicit)],
        &path_string(root.path()),
        None,
        None,
    )
    .await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].tools.contains_key("explicit"));
    assert!(!result.extensions[0].tools.contains_key("discovered"));
}

#[tokio::test(flavor = "current_thread")]
async fn load_extensions_with_no_paths_loads_nothing() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    write_file(root.path(), "extensions/discovered.ts", COMMAND_EXTENSION);

    let result = load_extensions(&[], &path_string(root.path()), None, None).await;

    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.extensions.len(), 0);
}
