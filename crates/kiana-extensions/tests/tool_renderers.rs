//! 渲染器解析测试：tool / message / entry / markdown 四类解析链。
//!
//! 移植 `packages/coding-agent/test/extensions-runner.test.ts` 的
//! `describe("message and entry renderers")` 与
//! “resolves tool renderers in extension load order, each able to defer to the next”
//! （`test/suite/regressions/10285-mcp-tool-renderers.test.ts` 的解析部分）。

use std::cell::Cell;
use std::sync::Arc;

use indexmap::IndexMap;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::tool_types::RenderShell;
use kiana_ext_api::{
    EntryRenderer, Extension, MarkdownTransformContext, MarkdownTransformer, MessageRenderer,
    ToolRendererResolver, ToolRenderers,
};
use kiana_extensions::runner::renderers::{
    get_entry_renderer, get_markdown_transformers, get_message_renderer, resolve_tool_renderers,
};

fn base_extension(path: &str) -> Extension {
    Extension {
        path: path.to_string(),
        resolved_path: path.to_string(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: path.to_string(),
            source: "local".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        handlers: IndexMap::new(),
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

fn renderers_with_shell(render_shell: RenderShell) -> ToolRenderers {
    ToolRenderers {
        render_shell: Some(render_shell),
        render_call: None,
        render_result: None,
    }
}

fn renderers_with_call() -> ToolRenderers {
    ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_args, _theme, _context| {
            panic!("render call is never invoked in this test")
        })),
        render_result: None,
    }
}

fn extension_with_resolvers(path: &str, resolvers: Vec<ToolRendererResolver>) -> Extension {
    let mut ext = base_extension(path);
    ext.tool_renderers = Some(resolvers);
    ext
}

// TS: "resolves tool renderers in extension load order, each able to defer to the next"
#[test]
fn resolves_tool_renderers_in_extension_load_order_each_able_to_defer_to_the_next() {
    let first: ToolRendererResolver = Arc::new(|tool_name, next| {
        if tool_name == "a" {
            Some(renderers_with_call())
        } else {
            next()
        }
    });
    let second: ToolRendererResolver = Arc::new(|_tool_name, next| {
        next().or_else(|| Some(renderers_with_shell(RenderShell::SelfRender)))
    });
    let extensions = [
        extension_with_resolvers("/extensions/first.ts", vec![first]),
        extension_with_resolvers("/extensions/second.ts", vec![second]),
    ];

    // "a"：第一个 resolver 短路（不调用 next），第二个与 base 都不参与。
    let resolved = resolve_tool_renderers(&extensions, "a", &|| None).expect("renderers");
    assert!(resolved.render_call.is_some());
    assert_eq!(resolved.render_shell, None);
    assert!(resolved.render_result.is_none());

    // "b"：两者都委托给 next；第二个用 `next() ?? { renderShell: "self" }` 兜底。
    let resolved = resolve_tool_renderers(&extensions, "b", &|| None).expect("renderers");
    assert_eq!(resolved.render_shell, Some(RenderShell::SelfRender));
    assert!(resolved.render_call.is_none());

    // "b" 且 base 有渲染器：next() 一直落到 base，第二个不再兜底。
    let base_renderers = renderers_with_call();
    let expected = base_renderers.render_call.clone();
    let resolved = resolve_tool_renderers(&extensions, "b", &|| Some(base_renderers.clone()))
        .expect("renderers");
    assert_eq!(
        resolved.render_shell, None,
        "resolver 的兜底只在 next() 为 undefined 时生效"
    );
    assert!(Arc::ptr_eq(
        resolved.render_call.as_ref().expect("render call"),
        expected.as_ref().expect("render call")
    ));
}

#[test]
fn next_is_lazy_so_base_is_not_called_when_a_resolver_short_circuits() {
    let calls = Cell::new(0);
    let resolver: ToolRendererResolver =
        Arc::new(|_tool_name, _next| Some(renderers_with_shell(RenderShell::SelfRender)));
    let extensions = [extension_with_resolvers(
        "/extensions/short-circuit.ts",
        vec![resolver],
    )];

    let resolved = resolve_tool_renderers(&extensions, "any", &|| {
        calls.set(calls.get() + 1);
        Some(renderers_with_call())
    });
    assert_eq!(
        resolved.expect("renderers").render_shell,
        Some(RenderShell::SelfRender)
    );
    assert_eq!(calls.get(), 0, "resolver 未调用 next()，base 不应被求值");
}

#[test]
fn resolvers_from_the_same_extension_keep_their_registration_order() {
    let first: ToolRendererResolver =
        Arc::new(|tool_name, _next| (tool_name == "x").then(renderers_with_call));
    let second: ToolRendererResolver = Arc::new(|tool_name, _next| {
        (tool_name == "x").then(|| renderers_with_shell(RenderShell::Default))
    });
    let extensions = [extension_with_resolvers(
        "/extensions/two-resolvers.ts",
        vec![first, second],
    )];

    // 同一扩展的 resolver 按注册顺序运行：第一个命中即短路。
    let resolved = resolve_tool_renderers(&extensions, "x", &|| None).expect("renderers");
    assert!(resolved.render_call.is_some());
    assert_eq!(resolved.render_shell, None);
}

#[test]
fn load_order_decides_between_extensions_that_both_answer() {
    let first: ToolRendererResolver =
        Arc::new(|_tool_name, _next| Some(renderers_with_shell(RenderShell::SelfRender)));
    let second: ToolRendererResolver =
        Arc::new(|_tool_name, _next| Some(renderers_with_shell(RenderShell::Default)));
    let extensions = [
        extension_with_resolvers("/extensions/first.ts", vec![first]),
        extension_with_resolvers("/extensions/second.ts", vec![second]),
    ];

    let resolved = resolve_tool_renderers(&extensions, "any", &|| None).expect("renderers");
    assert_eq!(resolved.render_shell, Some(RenderShell::SelfRender));
}

#[test]
fn an_unhandled_tool_falls_back_to_base_or_none() {
    let resolver: ToolRendererResolver = Arc::new(|_tool_name, next| next());
    let extensions = [extension_with_resolvers(
        "/extensions/defer.ts",
        vec![resolver],
    )];

    assert!(resolve_tool_renderers(&extensions, "missing", &|| None).is_none());

    let calls = Cell::new(0);
    let base = || {
        calls.set(calls.get() + 1);
        Some(renderers_with_shell(RenderShell::Default))
    };
    let resolved = resolve_tool_renderers(&extensions, "missing", &base).expect("base renderers");
    assert_eq!(resolved.render_shell, Some(RenderShell::Default));
    assert_eq!(calls.get(), 1);
}

#[test]
fn no_extensions_means_base_is_used_directly() {
    let base = || Some(renderers_with_shell(RenderShell::Default));
    let resolved = resolve_tool_renderers(&[], "any", &base).expect("base renderers");
    assert_eq!(resolved.render_shell, Some(RenderShell::Default));
    assert!(resolve_tool_renderers(&[], "any", &|| None).is_none());
}

/// Issue #10285（解析部分）：MCP 扩展的 resolver 让未注册的 `mcp__*` 工具也有渲染器，
/// 已注册工具仍用 `base`（`getToolDefinition`）返回的渲染器。
#[test]
fn mcp_style_resolvers_handle_unregistered_tools_and_leave_registered_ones_to_base() {
    let mcp: ToolRendererResolver = Arc::new(|tool_name, next| {
        if tool_name.starts_with("mcp__") {
            Some(renderers_with_shell(RenderShell::SelfRender))
        } else {
            next()
        }
    });
    let extensions = [extension_with_resolvers("/extensions/mcp.ts", vec![mcp])];

    // `base` = `getToolDefinition(toolName)`：只有已注册的工具（`read`）有渲染器。
    let registered = renderers_with_call();
    let expected = registered.render_call.clone();
    let resolve = |tool_name: &str| {
        let base = || {
            if tool_name == "read" {
                Some(registered.clone())
            } else {
                None
            }
        };
        resolve_tool_renderers(&extensions, tool_name, &base)
    };

    let call = resolve("mcp__my_docs__search").expect("MCP 渲染器");
    assert!(
        call.render_call.is_none(),
        "MCP 工具用 resolver 提供的渲染器，而不是 base 的"
    );
    assert_eq!(call.render_shell, Some(RenderShell::SelfRender));

    assert!(
        resolve("not_mcp").is_none(),
        "非 MCP 且未注册的工具没有渲染器"
    );

    let read = resolve("read").expect("已注册工具");
    assert!(Arc::ptr_eq(
        read.render_call.as_ref().expect("render call"),
        expected.as_ref().expect("render call")
    ));
}

// ---------------------------------------------------------------------------
// message / entry / markdown
// ---------------------------------------------------------------------------

fn message_renderer() -> MessageRenderer {
    Arc::new(|_message, _options, _theme| None)
}

fn entry_renderer() -> EntryRenderer {
    Arc::new(|_entry, _options, _theme| None)
}

// TS: "gets Markdown transformers in extension load order"
#[test]
fn gets_markdown_transformers_in_extension_load_order() {
    let first: MarkdownTransformer =
        Arc::new(|markdown: &str, _context: &MarkdownTransformContext| {
            format!("first({markdown})")
        });
    let second: MarkdownTransformer =
        Arc::new(|markdown: &str, _context: &MarkdownTransformContext| {
            format!("second({markdown})")
        });
    let mut a = base_extension("/extensions/markdown-renderer-a.ts");
    a.markdown_transformer = Some(first.clone());
    let mut b = base_extension("/extensions/markdown-renderer-b.ts");
    b.markdown_transformer = Some(second.clone());

    let transformers = get_markdown_transformers(&[a, b]);
    assert_eq!(transformers.len(), 2);
    assert!(Arc::ptr_eq(&transformers[0], &first));
    assert!(Arc::ptr_eq(&transformers[1], &second));

    // 调用方按顺序链式应用。
    let context = MarkdownTransformContext::default();
    let mut markdown = "x".to_string();
    for transformer in &transformers {
        markdown = transformer(&markdown, &context);
    }
    assert_eq!(markdown, "second(first(x))");
}

#[test]
fn extensions_without_a_markdown_transformer_are_skipped() {
    let mut with = base_extension("/extensions/with.ts");
    with.markdown_transformer = Some(Arc::new(
        |markdown: &str, _context: &MarkdownTransformContext| markdown.to_string(),
    ));
    let without = base_extension("/extensions/without.ts");
    assert_eq!(get_markdown_transformers(&[without, with]).len(), 1);
    assert!(get_markdown_transformers(&[]).is_empty());
}

// TS: "gets message renderer by type"
#[test]
fn gets_message_renderer_by_type() {
    let renderer = message_renderer();
    let mut ext = base_extension("/extensions/renderer.ts");
    ext.message_renderers
        .insert("my-type".to_string(), renderer.clone());

    let found = get_message_renderer(&[ext], "my-type").expect("renderer");
    assert!(Arc::ptr_eq(&found, &renderer));
    assert!(get_message_renderer(&[], "my-type").is_none());
}

#[test]
fn the_first_extension_with_a_message_renderer_wins() {
    let first = message_renderer();
    let second = message_renderer();
    assert!(!Arc::ptr_eq(&first, &second));

    let mut a = base_extension("/extensions/a.ts");
    a.message_renderers
        .insert("my-type".to_string(), first.clone());
    let mut b = base_extension("/extensions/b.ts");
    b.message_renderers
        .insert("my-type".to_string(), second.clone());

    let found = get_message_renderer(&[a, b], "my-type").expect("renderer");
    assert!(Arc::ptr_eq(&found, &first));
    assert!(get_message_renderer(&[base_extension("/extensions/empty.ts")], "my-type").is_none());
}

// TS: "gets entry renderer by type"
#[test]
fn gets_entry_renderer_by_type() {
    let renderer = entry_renderer();
    let mut ext = base_extension("/extensions/entry-renderer.ts");
    let mut renderers = IndexMap::new();
    renderers.insert("my-entry".to_string(), renderer.clone());
    ext.entry_renderers = Some(renderers);

    let found = get_entry_renderer(&[ext], "my-entry").expect("renderer");
    assert!(Arc::ptr_eq(&found, &renderer));
    // `entryRenderers` 不存在时同样返回 None。
    assert!(get_entry_renderer(&[base_extension("/extensions/none.ts")], "my-entry").is_none());
    assert!(get_entry_renderer(&[], "my-entry").is_none());
}

#[test]
fn the_first_extension_with_an_entry_renderer_wins() {
    let first = entry_renderer();
    let second = entry_renderer();

    let mut a = base_extension("/extensions/a.ts");
    a.entry_renderers = Some(IndexMap::from([("my-entry".to_string(), first.clone())]));
    let mut b = base_extension("/extensions/b.ts");
    b.entry_renderers = Some(IndexMap::from([("my-entry".to_string(), second)]));

    let found = get_entry_renderer(&[a, b], "my-entry").expect("renderer");
    assert!(Arc::ptr_eq(&found, &first));
}
