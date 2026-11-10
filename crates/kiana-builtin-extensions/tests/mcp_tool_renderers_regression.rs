//! `test/suite/regressions/10285-mcp-tool-renderers.test.ts` 的第一个 `it`。
//!
//! 第二个 `it`（HTML 导出）需要 `createHarness()`/`AgentSession.exportToHtml()` 与
//! `kiana-export`，属于 `crates/kiana-session/tests` 的下游测试。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use kiana_builtin_extensions::mcp::config::LoadedMcpConfig;
use kiana_builtin_extensions::mcp::index::{McpExtensionOptions, create_mcp_extension};
use kiana_core::ansi::strip_ansi;
use kiana_ext_api::tool_types::{ToolRenderContext, ToolRenderers};
use kiana_ext_api::types::ExtensionApi;
use kiana_theme::theme::{Theme, get_theme_by_name, init_theme};
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Component;
use serde_json::json;

#[path = "mcp_extension_support/mod.rs"]
mod mcp_extension_support;

use mcp_extension_support::FakeExtensionApi;

fn theme() -> Arc<Theme> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
    get_theme_by_name("dark").expect("builtin dark theme")
}

fn render_context() -> ToolRenderContext {
    ToolRenderContext {
        args: json!({}),
        tool_call_id: "call-1".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(serde_json::Value::Null)),
        cwd: PathBuf::from("/"),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        is_error: false,
        duration_ms: None,
        output_pad: 0,
    }
}

#[test]
#[allow(clippy::arc_with_non_send_sync)] // 假 ExtensionApi 故意是单线程的（ExtensionApi 本身 !Send）。
fn renders_calls_to_mcp_tools_that_are_not_registered() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        let api = Arc::new(FakeExtensionApi::new());
        let factory = create_mcp_extension(McpExtensionOptions {
            load_config: Some(Arc::new(|_ctx| LoadedMcpConfig {
                servers: Vec::new(),
                auto_enable_codemode: None,
                errors: Vec::new(),
                project_config: None,
            })),
            ..McpExtensionOptions::default()
        });
        factory(api.clone() as Arc<dyn ExtensionApi>).await;
        let resolver = api
            .tool_renderers
            .borrow()
            .first()
            .cloned()
            .expect("MCP tool renderer registered");

        let next_none = || None::<ToolRenderers>;
        let renderers = resolver("mcp__my_docs__search", &next_none)
            .expect("fallback renderers for an unregistered MCP tool");
        let context = render_context();
        let call = renderers.render_call.as_ref().expect("renderCall");
        let mut component = call(&json!({ "query": "kiana" }), &theme(), &context);
        let text = strip_ansi(&component.render(100).join("\n"));
        assert!(
            text.contains("my_docs/search query=\"kiana\""),
            "unexpected render output: {text}"
        );

        assert!(resolver("not_mcp", &next_none).is_none());

        // 已注册工具保留自己的渲染器：next() 优先。
        let sentinel = ToolRenderers {
            render_shell: None,
            render_call: Some(Arc::new(|_args, _theme, _context| {
                Box::new(Text::new("sentinel")) as Box<dyn Component>
            })),
            render_result: None,
        };
        let sentinel_for_next = sentinel.clone();
        let next_some = move || Some(sentinel_for_next.clone());
        let resolved = resolver("read", &next_some).expect("next() wins");
        assert!(Arc::ptr_eq(
            resolved.render_call.as_ref().expect("sentinel renderCall"),
            sentinel.render_call.as_ref().expect("sentinel renderCall"),
        ));
    }));
}
