//! 移植 `packages/coding-agent/test/tool-search.test.ts`（9 个 `it`）。

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::types::JsonValue;
use kiana_builtin_extensions::codemode::tool::{
    CodemodeDescriptionOptions, create_codemode_description,
};
use kiana_builtin_extensions::tool_search::tool::{
    Bm25Ranker, ToolRanker, ToolSearchDocument, create_tool_search_document, tokenize,
};
use kiana_ext_api::tool_types::ToolNamespace;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

struct NoopExecute;

impl AgentToolExecute for NoopExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async {
            Ok(AgentToolResult {
                content: Vec::new(),
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn tool(name: &str, description: &str, properties: Value) -> AgentTool {
    let mut parameters = Map::new();
    parameters.insert("type".to_string(), Value::String("object".to_string()));
    parameters.insert("properties".to_string(), properties);
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        parameters: Value::Object(parameters),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(NoopExecute),
    }
}

fn empty_properties() -> Value {
    Value::Object(Map::new())
}

#[test]
fn splits_camel_case_and_snake_case_drops_stop_words_and_folds_plurals() {
    assert_eq!(
        tokenize("listIssues for the GitHub_repo"),
        vec!["list", "issue", "git", "hub", "repo"]
    );
    assert_eq!(
        tokenize("searches queries HTTPServer"),
        vec!["search", "query", "http", "server"]
    );
}

fn ranker_tools() -> Vec<AgentTool> {
    vec![
        tool(
            "mcp__github__list_issues",
            "List issues in a repository.",
            json!({"state": {"type": "string", "description": "open or closed"}}),
        ),
        tool(
            "mcp__github__create_pull_request",
            "Open a pull request.",
            empty_properties(),
        ),
        tool(
            "mcp__linear__search_issues",
            "Search Linear issues by text.",
            empty_properties(),
        ),
        tool(
            "mcp__docs__search",
            "Search the documentation.",
            empty_properties(),
        ),
    ]
}

fn documents() -> Vec<ToolSearchDocument> {
    ranker_tools()
        .iter()
        .map(|tool| create_tool_search_document(tool, None))
        .collect()
}

#[test]
fn ranks_by_term_relevance_and_respects_the_limit() {
    let ranker = Bm25Ranker::new();
    let documents = documents();
    assert_eq!(
        ranker
            .rank("issue", &documents, 8.0)
            .iter()
            .map(|matched| matched.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__linear__search_issues", "mcp__github__list_issues"]
    );
    assert_eq!(
        ranker.rank("pull requests", &documents, 8.0)[0].name,
        "mcp__github__create_pull_request"
    );
    assert_eq!(ranker.rank("search", &documents, 1.0).len(), 1);
    // 属性名与描述都可搜索。
    assert_eq!(
        ranker
            .rank("closed", &documents, 8.0)
            .iter()
            .map(|matched| matched.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__github__list_issues"]
    );
}

#[test]
fn returns_nothing_for_unknown_or_empty_queries() {
    let ranker = Bm25Ranker::new();
    let documents = documents();
    assert!(ranker.rank("kubernetes", &documents, 8.0).is_empty());
    assert!(ranker.rank("the", &documents, 8.0).is_empty());
    // 已知的 v1 限制：没有同义词，因此 "tickets" 找不到 "issues"。
    assert!(ranker.rank("tickets", &documents, 8.0).is_empty());
}

#[test]
fn includes_the_namespace_in_the_search_text() {
    let document = create_tool_search_document(
        &tool("mcp__x__run", "Run it.", empty_properties()),
        Some(&ToolNamespace {
            name: "mcp__x".to_string(),
            description: Some("Kubernetes cluster tools".to_string()),
            ..ToolNamespace::default()
        }),
    );
    let matches = Bm25Ranker::new().rank("kubernetes", &[document], 8.0);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "mcp__x__run");
    assert!(matches[0].score > 0.0);
}

fn catalog_tools() -> Vec<AgentTool> {
    let plain = tool("read_notes", "Read notes.", empty_properties());
    let github: Vec<AgentTool> = ["a", "b", "c"]
        .iter()
        .map(|suffix| {
            tool(
                &format!("mcp__github__{suffix}"),
                &format!("GitHub {suffix}."),
                empty_properties(),
            )
        })
        .collect();
    let docs = vec![
        tool("mcp__docs__search", "Search docs.", empty_properties()),
        tool("mcp__docs__long", &"Long ".repeat(200), empty_properties()),
    ];
    let mut all = vec![plain];
    all.extend(github);
    all.extend(docs);
    all
}

fn catalog_namespaces(tools: &[AgentTool]) -> IndexMap<String, ToolNamespace> {
    let mut namespaces = IndexMap::new();
    for tool in tools {
        if tool.name.starts_with("mcp__github") {
            namespaces.insert(
                tool.name.clone(),
                ToolNamespace {
                    name: "mcp__github".to_string(),
                    description: Some("GitHub server".to_string()),
                    ..ToolNamespace::default()
                },
            );
        } else if tool.name.starts_with("mcp__docs") {
            namespaces.insert(
                tool.name.clone(),
                ToolNamespace {
                    name: "mcp__docs".to_string(),
                    ..ToolNamespace::default()
                },
            );
        }
    }
    namespaces
}

#[test]
fn lists_everything_without_a_budget() {
    let all = catalog_tools();
    let description = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces: Some(catalog_namespaces(&all)),
            ..CodemodeDescriptionOptions::default()
        },
    );
    assert!(description.contains("Nested tools:"));
    assert!(description.contains("## mcp__github\nGitHub server"));
    assert!(description.contains("## mcp__docs\n\n### `mcp__docs"));
    // 搜索提示始终在，因此后来出现的工具不会改变它。
    assert!(description.contains("find unlisted tools, such as MCP tools"));
    // #10555：查找 helper 是异步的；没有 `await` 时脚本会把 promise 序列化成 {}。
    assert!(description.contains("`await searchTools(query, { limit?, namespace? })`"));
    assert!(description.contains("`await describeTool(name)`"));
    assert!(description.contains("`await describeNamespace(name)`"));
}

#[test]
fn fills_the_budget_round_robin_cheapest_first_and_says_what_is_missing() {
    // 每个小 section 约 42 token：每组先一个，然后再一个。
    let all = catalog_tools();
    let description = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces: Some(catalog_namespaces(&all)),
            inline_budget: Some(170.0),
            ..CodemodeDescriptionOptions::default()
        },
    );
    assert!(description.contains("### `read_notes`"));
    assert!(description.contains("## mcp__docs (some tools not listed)"));
    assert!(description.contains("### `mcp__docs__search`"));
    assert!(!description.contains("### `mcp__docs__long`"));
    assert!(description.contains("## mcp__github (some tools not listed)"));
    assert!(description.contains("find unlisted tools, such as MCP tools"));
    // 确定性：同样输入得到同样描述。
    assert_eq!(
        create_codemode_description(
            &all,
            &CodemodeDescriptionOptions {
                namespaces: Some(catalog_namespaces(&all)),
                inline_budget: Some(170.0),
                ..CodemodeDescriptionOptions::default()
            },
        ),
        description
    );
}

#[test]
fn leaves_deferred_tools_and_their_namespaces_out_entirely() {
    let all = catalog_tools();
    let deferred: std::collections::HashSet<String> = all
        .iter()
        .filter(|tool| tool.name.starts_with("mcp__github"))
        .map(|tool| tool.name.clone())
        .collect();
    let description = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces: Some(catalog_namespaces(&all)),
            deferred: Some(deferred),
            ..CodemodeDescriptionOptions::default()
        },
    );
    assert!(!description.contains("mcp__github"));
    let without_github: Vec<AgentTool> = all
        .iter()
        .filter(|tool| !tool.name.starts_with("mcp__github"))
        .cloned()
        .collect();
    // deferred 工具（例如之后连接的服务器）不改变描述。
    assert_eq!(
        description,
        create_codemode_description(
            &without_github,
            &CodemodeDescriptionOptions {
                namespaces: Some(catalog_namespaces(&without_github)),
                ..CodemodeDescriptionOptions::default()
            },
        )
    );
}

#[test]
fn leaves_namespace_instructions_out() {
    let github: Vec<AgentTool> = ["a", "b", "c"]
        .iter()
        .map(|suffix| {
            tool(
                &format!("mcp__github__{suffix}"),
                &format!("GitHub {suffix}."),
                empty_properties(),
            )
        })
        .collect();
    let mut namespaces = IndexMap::new();
    for tool in &github {
        namespaces.insert(
            tool.name.clone(),
            ToolNamespace {
                name: "mcp__github".to_string(),
                instructions: Some("Long usage guide.".to_string()),
                ..ToolNamespace::default()
            },
        );
    }
    let description = create_codemode_description(
        &github,
        &CodemodeDescriptionOptions {
            namespaces: Some(namespaces),
            ..CodemodeDescriptionOptions::default()
        },
    );
    assert!(description.contains("## mcp__github\n\n### `mcp__github__a`"));
    assert!(!description.contains("Long usage guide."));
}

#[test]
fn lists_only_namespaces_with_a_zero_budget() {
    let all = catalog_tools();
    let description = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces: Some(catalog_namespaces(&all)),
            inline_budget: Some(0.0),
            ..CodemodeDescriptionOptions::default()
        },
    );
    assert!(description.contains("## mcp__docs (tools not listed)"));
    assert!(!description.contains("codemode tool declaration:"));
}

/// `tests/fixtures/codemode_descriptions.json` 由 TS 参考实现生成（与 `tool-search.test.ts`
/// 相同的输入，另外加了 `outputSchema`/guideline 的样例）。docs 路径因安装位置不同而不同，
/// 比较前统一替换成 `<docs>`。
#[test]
fn matches_the_typescript_golden_codemode_descriptions() {
    let golden: Value = serde_json::from_str(include_str!("fixtures/codemode_descriptions.json"))
        .expect("valid golden fixture");
    let normalize = |text: &str| -> String {
        static DOCS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"[^\n]*docs/codemode\.md").expect("valid docs regex")
        });
        DOCS.replace_all(text, "<docs>").into_owned()
    };

    assert_eq!(
        normalize(&create_codemode_description(
            &[],
            &CodemodeDescriptionOptions::default()
        )),
        normalize(golden["empty"].as_str().unwrap())
    );

    let all = catalog_tools();
    let namespaces = catalog_namespaces(&all);
    assert_eq!(
        normalize(&create_codemode_description(
            &all,
            &CodemodeDescriptionOptions {
                namespaces: Some(namespaces.clone()),
                ..CodemodeDescriptionOptions::default()
            },
        )),
        normalize(golden["all"].as_str().unwrap())
    );
    assert_eq!(
        normalize(&create_codemode_description(
            &all,
            &CodemodeDescriptionOptions {
                namespaces: Some(namespaces.clone()),
                inline_budget: Some(170.0),
                ..CodemodeDescriptionOptions::default()
            },
        )),
        normalize(golden["budget170"].as_str().unwrap())
    );
    assert_eq!(
        normalize(&create_codemode_description(
            &all,
            &CodemodeDescriptionOptions {
                namespaces: Some(namespaces),
                inline_budget: Some(0.0),
                ..CodemodeDescriptionOptions::default()
            },
        )),
        normalize(golden["budget0"].as_str().unwrap())
    );

    let echo = tool(
        "echo",
        "Echo text back.\n\nSecond paragraph.",
        json!({"text": {"type": "string", "description": "Text to echo"}}),
    );
    let stats = {
        let mut tool = tool("stats", "Return structured stats", empty_properties());
        tool.output_schema = Some(json!({
            "type": "object",
            "required": ["files", "names"],
            "properties": {
                "files": {"type": "number"},
                "names": {"type": "array", "items": {"type": "string"}}
            }
        }));
        tool
    };
    let mut guidelines = IndexMap::new();
    guidelines.insert("echo".to_string(), vec!["Use it.".to_string()]);
    assert_eq!(
        normalize(&create_codemode_description(
            &[echo, stats],
            &CodemodeDescriptionOptions {
                guidelines: Some(guidelines),
                models: true,
                ..CodemodeDescriptionOptions::default()
            },
        )),
        normalize(golden["withDeclaration"].as_str().unwrap())
    );
}

/// `tests/fixtures/codemode_loadout.json` 由 TS 参考实现生成。
struct FakeLoadout {
    declared: Vec<AgentTool>,
    callable: Vec<AgentTool>,
    namespaces: IndexMap<String, ToolNamespace>,
    guidelines: IndexMap<String, Vec<String>>,
}

impl FakeLoadout {
    fn exposure(&self, name: &str) -> kiana_ext_api::tool_types::ToolExposure {
        if name == "codemode" {
            kiana_ext_api::tool_types::ToolExposure::ModelOnly
        } else if name.starts_with("mcp__docs") {
            kiana_ext_api::tool_types::ToolExposure::Deferred
        } else {
            kiana_ext_api::tool_types::ToolExposure::Direct
        }
    }
}

impl kiana_ext_api::tool_types::ToolLoadout for FakeLoadout {
    fn declared(&self) -> &[AgentTool] {
        &self.declared
    }

    fn callable(&self) -> &[AgentTool] {
        &self.callable
    }

    fn registered(&self) -> &[AgentTool] {
        &self.callable
    }

    fn get_exposure(&self, name: &str) -> kiana_ext_api::tool_types::ToolExposure {
        self.exposure(name)
    }

    fn get_namespace(&self, name: &str) -> Option<&ToolNamespace> {
        self.namespaces.get(name)
    }

    fn get_prompt_guidelines(&self, name: &str) -> &[String] {
        self.guidelines.get(name).map_or(&[], Vec::as_slice)
    }
}

#[test]
fn matches_the_typescript_golden_codemode_loadout() {
    use kiana_builtin_extensions::codemode::tool::{
        CodemodeToolOptions, create_codemode_tool_definition, prepare_codemode_loadout,
    };
    use kiana_config::settings::CodemodeMode;

    let golden: Value = serde_json::from_str(include_str!("fixtures/codemode_loadout.json"))
        .expect("valid golden fixture");
    fn normalize_json(value: &Value) -> Value {
        static DOCS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"[^\n]*docs/codemode\.md").expect("valid docs regex")
        });
        match value {
            Value::String(text) => Value::String(DOCS.replace_all(text, "<docs>").into_owned()),
            Value::Array(array) => Value::Array(array.iter().map(normalize_json).collect()),
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), normalize_json(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    let read = tool("read", "Read a file.", json!({"path": {"type": "string"}}));
    let echo = tool(
        "echo",
        "Echo text back.\n\nSecond paragraph.",
        json!({"text": {"type": "string", "description": "Text to echo"}}),
    );
    let mut stats = tool("stats", "Return structured stats", empty_properties());
    stats.output_schema = Some(json!({
        "type": "object",
        "required": ["files", "names"],
        "properties": {
            "files": {"type": "number"},
            "names": {"type": "array", "items": {"type": "string"}}
        }
    }));
    let codemode = tool(
        "codemode",
        "old description",
        json!({"code": {"type": "string"}}),
    );
    let docs = tool("mcp__docs__search", "Search docs.", empty_properties());
    let mut namespaces = IndexMap::new();
    namespaces.insert(
        "mcp__docs__search".to_string(),
        ToolNamespace {
            name: "mcp__docs".to_string(),
            description: Some("Docs server".to_string()),
            ..ToolNamespace::default()
        },
    );
    let mut guidelines = IndexMap::new();
    guidelines.insert("echo".to_string(), vec!["Use it.".to_string()]);
    let loadout = FakeLoadout {
        declared: vec![read.clone(), echo.clone(), stats.clone(), codemode.clone()],
        callable: vec![read, echo, stats, codemode, docs],
        namespaces,
        guidelines,
    };

    let on = prepare_codemode_loadout(
        &loadout,
        &CodemodeToolOptions {
            models: Some(true),
            ..CodemodeToolOptions::default()
        },
    );
    assert_eq!(
        normalize_json(&serde_json::to_value(&on).unwrap()),
        normalize_json(&golden["on"]),
        "on mode"
    );

    let only = prepare_codemode_loadout(
        &loadout,
        &CodemodeToolOptions {
            models: Some(true),
            get_mode: Some(std::sync::Arc::new(|| CodemodeMode::Only)),
            ..CodemodeToolOptions::default()
        },
    );
    assert_eq!(
        normalize_json(&serde_json::to_value(&only).unwrap()),
        normalize_json(&golden["only"]),
        "only mode"
    );

    let default_budget = prepare_codemode_loadout(&loadout, &CodemodeToolOptions::default());
    // `noBudget` 来自未设置 `models`、`getInlineBudget: () => undefined`，即回落到默认预算。
    assert_eq!(
        normalize_json(&serde_json::to_value(&default_budget).unwrap()),
        normalize_json(&golden["noBudget"]),
        "default inline budget"
    );
    // 工具定义本身只是把同样的函数装上。
    let definition = create_codemode_tool_definition(CodemodeToolOptions::default());
    let changes = (definition.prepare_loadout.as_ref().unwrap())(&loadout).unwrap();
    assert!(changes.descriptions.is_some());
}

struct FakeTools {
    all: Mutex<Vec<kiana_ext_api::types::ToolInfo>>,
    active: Mutex<Vec<String>>,
    set_calls: Mutex<Vec<Vec<String>>>,
}

impl FakeTools {
    fn new(all: Vec<kiana_ext_api::types::ToolInfo>, active: Vec<String>) -> Self {
        FakeTools {
            all: Mutex::new(all),
            active: Mutex::new(active),
            set_calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl kiana_builtin_extensions::tool_search::tool::ToolSearchTools for FakeTools {
    async fn get_all_tools(&self) -> Vec<kiana_ext_api::types::ToolInfo> {
        self.all.lock().unwrap().clone()
    }

    async fn get_active_tools(&self) -> Vec<String> {
        self.active.lock().unwrap().clone()
    }

    async fn set_active_tools(&self, tool_names: Vec<String>) {
        self.set_calls.lock().unwrap().push(tool_names.clone());
        *self.active.lock().unwrap() = tool_names;
    }
}

fn tool_info(
    name: &str,
    description: &str,
    exposure: kiana_ext_api::tool_types::ToolExposure,
) -> kiana_ext_api::types::ToolInfo {
    kiana_ext_api::types::ToolInfo {
        name: name.to_string(),
        description: description.to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        prompt_guidelines: None,
        reaches_tools: None,
        exposure,
        namespace: None,
        annotations: None,
        source_info: kiana_core::source_info::create_synthetic_source_info(
            "builtin:test",
            kiana_core::source_info::SyntheticSourceInfoOptions {
                source: "builtin".to_string(),
                ..Default::default()
            },
        ),
    }
}

async fn execute_search(
    definition: &kiana_ext_api::tool_types::ToolDefinition,
    params: Value,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute(
            "call",
            params,
            None,
            None,
            kiana_tools::definition::missing_tool_context(),
        )
        .await
}

#[tokio::test]
async fn rejects_invalid_argument_types_exactly_like_typescript() {
    use kiana_builtin_extensions::tool_search::tool::{
        ToolSearchToolOptions, create_tool_search_tool_definition,
    };

    let definition = create_tool_search_tool_definition(ToolSearchToolOptions::default());

    // `limit ?? 8` 之后是 `Number.isInteger(max) || max <= 0`：字符串/布尔/数组都失败。
    for limit in [json!("2"), json!(true), json!([2]), json!({"value": 2})] {
        let error = execute_search(&definition, json!({"query": "docs", "limit": limit}))
            .await
            .expect_err("non-number limit must fail");
        assert_eq!(error.message, "limit must be a positive integer");
        assert_eq!(error.name, None);
    }

    // 缺失/非字符串 query：TS 在 `query.trim()` 处抛 TypeError（V8 文案逐字）。
    let error = execute_search(&definition, json!({}))
        .await
        .expect_err("missing query must fail");
    assert_eq!(
        error.message,
        "Cannot read properties of undefined (reading 'trim')"
    );
    assert_eq!(error.name.as_deref(), Some("TypeError"));

    let error = execute_search(&definition, json!({"query": null}))
        .await
        .expect_err("null query must fail");
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'trim')"
    );
    assert_eq!(error.name.as_deref(), Some("TypeError"));

    for query in [
        json!(5),
        json!(false),
        json!(["docs"]),
        json!({"trim": "x"}),
    ] {
        let error = execute_search(&definition, json!({"query": query}))
            .await
            .expect_err("non-string query must fail");
        assert_eq!(error.message, "query.trim is not a function");
        assert_eq!(error.name.as_deref(), Some("TypeError"));
    }

    // 参数本身是 null（函数参数解构）或原始值（ToObject 后 query 仍是 undefined）。
    let error = execute_search(&definition, Value::Null)
        .await
        .expect_err("null params must fail");
    assert_eq!(
        error.message,
        "Cannot destructure property 'query' of 'object null' as it is null."
    );
    assert_eq!(error.name.as_deref(), Some("TypeError"));
    for params in [json!("docs"), json!(5), json!(["docs"])] {
        let error = execute_search(&definition, params)
            .await
            .expect_err("primitive params must fail");
        assert_eq!(
            error.message,
            "Cannot read properties of undefined (reading 'trim')"
        );
        assert_eq!(error.name.as_deref(), Some("TypeError"));
    }

    // query 校验先于 limit 校验（TS 的顺序）。
    let error = execute_search(&definition, json!({"query": "  ", "limit": "2"}))
        .await
        .expect_err("empty query must fail before the limit");
    assert_eq!(error.message, "query must not be empty");

    // `limit: null` 是 nullish，回落到默认值 8（与缺省一致），不是 0。
    let result = execute_search(&definition, json!({"query": "docs", "limit": null}))
        .await
        .unwrap();
    match &result.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => {
            assert_eq!(text.text, "No matching tools found.")
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn validates_tool_search_arguments_and_loads_matches() {
    use kiana_builtin_extensions::tool_search::tool::{
        ToolSearchToolOptions, create_tool_search_tool_definition,
    };
    use kiana_ext_api::tool_types::ToolExposure;

    let empty_definition = create_tool_search_tool_definition(ToolSearchToolOptions::default());
    let error = execute_search(&empty_definition, json!({"query": "  "}))
        .await
        .expect_err("empty query must fail");
    assert_eq!(error.message, "query must not be empty");
    let error = execute_search(&empty_definition, json!({"query": "docs", "limit": 0}))
        .await
        .expect_err("zero limit must fail");
    assert_eq!(error.message, "limit must be a positive integer");
    let error = execute_search(&empty_definition, json!({"query": "docs", "limit": 1.5}))
        .await
        .expect_err("fractional limit must fail");
    assert_eq!(error.message, "limit must be a positive integer");
    let result = execute_search(&empty_definition, json!({"query": "docs"}))
        .await
        .unwrap();
    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => {
            assert_eq!(text.text, "No matching tools found.")
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(result.details["loaded"], json!([]));

    let tools = Arc::new(FakeTools::new(
        vec![
            tool_info("read", "Read a file.", ToolExposure::Direct),
            tool_info(
                "mcp__docs__search",
                "Search the docs.\nSecond line.",
                ToolExposure::Deferred,
            ),
            tool_info(
                "mcp__docs__read",
                "Read a doc page.",
                ToolExposure::Deferred,
            ),
        ],
        vec!["read".to_string()],
    ));
    let definition = create_tool_search_tool_definition(ToolSearchToolOptions {
        tools: Some(tools.clone()),
    });
    let result = execute_search(&definition, json!({"query": "docs"}))
        .await
        .unwrap();
    match &result.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => assert_eq!(
            text.text,
            "Loaded 2 tools. They are available from your next call:\n- mcp__docs__read: Read a doc page.\n- mcp__docs__search: Search the docs."
        ),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        result.details["loaded"],
        json!(["mcp__docs__read", "mcp__docs__search"])
    );
    assert_eq!(
        *tools.set_calls.lock().unwrap(),
        vec![vec![
            "read".to_string(),
            "mcp__docs__read".to_string(),
            "mcp__docs__search".to_string()
        ]]
    );

    // 单数形式与描述的第一行。
    let tools = Arc::new(FakeTools::new(
        vec![tool_info(
            "mcp__docs__read",
            "Read a doc page.\nIgnored second line.",
            ToolExposure::Deferred,
        )],
        Vec::new(),
    ));
    let definition =
        create_tool_search_tool_definition(ToolSearchToolOptions { tools: Some(tools) });
    let result = execute_search(&definition, json!({"query": "page"}))
        .await
        .unwrap();
    match &result.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => assert_eq!(
            text.text,
            "Loaded 1 tool. They are available from your next call:\n- mcp__docs__read: Read a doc page."
        ),
        other => panic!("unexpected {other:?}"),
    }
}
