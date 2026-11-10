//! 移植 `packages/ai/test/model-catalog-types.test.ts`。
//!
//! TS 侧该文件主要做**类型级**断言（`expectTypeOf`，在 Rust 里由 `flatten_*_catalog` 的返回类型
//! 本身承担），运行时断言只有两类：
//! 1. `flatten*Catalog` 按 `model.type` 过滤、按 `model.id` 作键，跨 api 组时后者覆盖前者；
//! 2. `github-copilot` 的所有 `gpt-*` 模型走 `openai-responses`。
//!
//! Rust 侧的目录数据是 vendored 的 `crates/kiana-ai/data/*.json`（`MODELS` 的等价物），
//! 因此第 2 点直接对生成数据断言。

mod model_data_support;

use kiana_ai::model_catalog::{
    flatten_chat_model_catalog, flatten_classifier_model_catalog, flatten_image_model_catalog,
};
use model_data_support::{assert_js_eq, data_groups, read_data};
use serde_json::{Value, json};

#[test]
fn derives_model_api_id_and_provider_literals_from_grouped_model_data() {
    // `XAI_MODELS["grok-4.5"|"grok-4.6"|"grok-4.7"|"grok-4.3"].api === "openai-responses"`。
    let xai = flatten_chat_model_catalog(&data_groups("xai.json"));
    for id in ["grok-4.5", "grok-4.6", "grok-4.7", "grok-4.3"] {
        let model = xai.get(id).unwrap_or_else(|| panic!("xai/{id}"));
        assert_eq!(
            model.get("api").and_then(Value::as_str),
            Some("openai-responses"),
            "{id}"
        );
        assert_eq!(model.get("id").and_then(Value::as_str), Some(id), "{id}");
        assert_eq!(
            model.get("provider").and_then(Value::as_str),
            Some("xai"),
            "{id}"
        );
    }
}

#[test]
fn routes_github_copilot_grok_4_5_through_the_responses_api() {
    let copilot = flatten_chat_model_catalog(&data_groups("github-copilot.json"));
    let model = copilot.get("grok-4.5").expect("github-copilot/grok-4.5");
    assert_eq!(
        model.get("api").and_then(Value::as_str),
        Some("openai-responses")
    );
}

#[test]
fn routes_all_github_copilot_gpt_models_through_the_responses_api() {
    let copilot = flatten_chat_model_catalog(&data_groups("github-copilot.json"));
    let gpt_models: Vec<&Value> = copilot
        .iter()
        .filter(|(key, _)| key.starts_with("gpt-"))
        .map(|(_, model)| model)
        .collect();
    assert!(!gpt_models.is_empty(), "expected at least one gpt-* model");
    for model in &gpt_models {
        assert_eq!(
            model.get("api").and_then(Value::as_str),
            Some("openai-responses")
        );
    }
    // `expectTypeOf(GITHUB_COPILOT_MODELS["gpt-6-astra"].api).toEqualTypeOf<"openai-responses">()`
    assert_eq!(
        copilot["gpt-6-astra"].get("api").and_then(Value::as_str),
        Some("openai-responses")
    );
    for id in ["gpt-6-sol", "gpt-6-luna"] {
        let model = &copilot[id];
        assert_eq!(
            model.get("api").and_then(Value::as_str),
            Some("openai-responses"),
            "{id}"
        );
        // `toMatchObject({ api, contextWindow: 1000000, maxTokens: 128000, thinkingLevelMap: {off:"none", max:"max"} })`
        assert_js_eq(
            &model["contextWindow"],
            &json!(1000000),
            &format!("{id} contextWindow"),
        );
        assert_js_eq(
            &model["maxTokens"],
            &json!(128000),
            &format!("{id} maxTokens"),
        );
        let thinking = &model["thinkingLevelMap"];
        assert_js_eq(&thinking["off"], &json!("none"), &format!("{id} off"));
        assert_js_eq(&thinking["max"], &json!("max"), &format!("{id} max"));
    }
}

// ---------------------------------------------------------------------------
// `flatten*Catalog` 的运行时语义（`src/model-catalog.ts`；spec 2.1.10）
// ---------------------------------------------------------------------------

/// 把 `(api, "type:id", type)` 三元组装配成 `ModelGroups`（同一 api 的条目按给出顺序合并）。
fn groups(entries: &[(&str, &str, &str)]) -> kiana_ai::model_catalog::ModelGroups<Value> {
    let mut groups: kiana_ai::model_catalog::ModelGroups<Value> =
        kiana_ai::model_catalog::ModelGroups::new();
    for (api, key, model_type) in entries {
        let id = key.rsplit_once(':').map(|(_, id)| id).unwrap_or(key);
        let group = groups.entry((*api).to_string()).or_default();
        group.insert(
            (*key).to_string(),
            json!({ "id": id, "type": model_type, "api": api }),
        );
    }
    groups
}

#[test]
fn flattens_each_model_type_by_id() {
    let grouped = groups(&[
        ("openai-completions", "chat:model-a", "chat"),
        ("openai-completions", "image:model-a", "image"),
        ("openai-completions", "classifier:model-c", "classifier"),
    ]);
    let chat = flatten_chat_model_catalog(&grouped);
    assert_eq!(chat.keys().cloned().collect::<Vec<_>>(), vec!["model-a"]);
    let images = flatten_image_model_catalog(&grouped);
    assert_eq!(images.keys().cloned().collect::<Vec<_>>(), vec!["model-a"]);
    let classifiers = flatten_classifier_model_catalog(&grouped);
    assert_eq!(
        classifiers.keys().cloned().collect::<Vec<_>>(),
        vec!["model-c"]
    );
    // chat 与 image 同 id 各自成表（互不覆盖）。
    assert_eq!(
        chat["model-a"].get("type").and_then(Value::as_str),
        Some("chat")
    );
    assert_eq!(
        images["model-a"].get("type").and_then(Value::as_str),
        Some("image")
    );
}

#[test]
fn later_api_groups_override_earlier_ones_but_keep_the_first_position() {
    // `Object.fromEntries`：后出现的同 id 覆盖值，位置保持首次出现处。
    let grouped = groups(&[
        ("anthropic-messages", "chat:dup", "chat"),
        ("openai-completions", "chat:first", "chat"),
        ("openai-completions", "chat:dup", "chat"),
    ]);
    let chat = flatten_chat_model_catalog(&grouped);
    assert_eq!(
        chat.keys().cloned().collect::<Vec<_>>(),
        vec!["dup", "first"]
    );
    assert_eq!(
        chat["dup"].get("api").and_then(Value::as_str),
        Some("openai-completions"),
        "later api group wins"
    );
}

#[test]
fn vendored_catalog_covers_every_provider_shard() {
    // `MODELS` 的键顺序（spec 2.3.1）在 vendored 数据里是逐文件独立的；这里只确认每个
    // provider 的分片都能摊平出至少一个 chat 模型（`typesafe` 只有 classifier）。
    let providers = read_data(".manifest.json");
    let files = providers
        .get("files")
        .and_then(Value::as_object)
        .expect("manifest files");
    let mut chats = 0;
    for name in files.keys() {
        if name == ".manifest.json" {
            continue;
        }
        let chat = flatten_chat_model_catalog(&data_groups(name));
        chats += chat.len();
    }
    assert!(
        chats > 1000,
        "expected >1000 vendored chat models, got {chats}"
    );
}

// ---------------------------------------------------------------------------
// `scripts/model-catalog-protocol.ts`（spec 2.4.10；TS 测试 `scripts/model-catalog-protocol.test.ts`）
// ---------------------------------------------------------------------------

mod catalog_protocol {
    use kiana_ai::model_catalog::protocol::{
        ModelCatalogArtifact, ModelCatalogIndex, ModelCatalogIndexEntry,
        ModelCatalogRepresentation, ModelCatalogRequest, compare_model_catalog_kiana_versions,
        get_model_catalog_artifact_key, get_model_catalog_provider_key, parse_model_catalog_index,
        parse_model_catalog_representation, parse_model_catalog_request, select_model_catalog,
    };

    fn revision(letter: char) -> String {
        format!("sha256-{}", letter.to_string().repeat(64))
    }

    fn index() -> ModelCatalogIndex {
        ModelCatalogIndex {
            schema_version: 1,
            default_revision: revision('b'),
            catalogs: vec![
                ModelCatalogIndexEntry {
                    minimum_kiana_version: "0.80.7".to_string(),
                    revision: revision('a'),
                },
                ModelCatalogIndexEntry {
                    minimum_kiana_version: "0.85.0".to_string(),
                    revision: revision('b'),
                },
            ],
        }
    }

    fn catalog(
        kiana_version: Option<&str>,
        representation: ModelCatalogRepresentation,
    ) -> ModelCatalogRequest {
        ModelCatalogRequest::Catalog {
            kiana_version: kiana_version.map(str::to_string),
            representation,
        }
    }

    fn invalid(error: &str) -> ModelCatalogRequest {
        ModelCatalogRequest::Invalid {
            error: error.to_string(),
        }
    }

    fn redirect(location: &str) -> ModelCatalogRequest {
        ModelCatalogRequest::Redirect {
            location: location.to_string(),
        }
    }

    #[test]
    fn builds_revision_scoped_storage_keys() {
        let legacy = revision('a');
        assert_eq!(
            get_model_catalog_artifact_key(&legacy, ModelCatalogArtifact::Models),
            format!("models/v1/revisions/{legacy}/models.json")
        );
        assert_eq!(
            get_model_catalog_provider_key(
                &legacy,
                "openrouter",
                ModelCatalogRepresentation::Legacy
            ),
            format!("models/v1/revisions/{legacy}/providers/openrouter.json")
        );
        assert_eq!(
            get_model_catalog_provider_key(
                &legacy,
                "openrouter",
                ModelCatalogRepresentation::Typed
            ),
            format!("models/v1/revisions/{legacy}/providers/openrouter.all.json")
        );
    }

    #[test]
    fn orders_kiana_versions_with_semver_precedence() {
        assert!(compare_model_catalog_kiana_versions("0.85.0", "0.84.4").expect("valid") > 0.0);
        assert!(
            compare_model_catalog_kiana_versions("0.85.0-rc.1", "0.85.0").expect("valid") < 0.0
        );
        assert!(
            compare_model_catalog_kiana_versions("0.85.0-rc.2", "0.85.0-rc.10").expect("valid")
                < 0.0
        );
        assert_eq!(
            compare_model_catalog_kiana_versions("v1.0.0", "1.0.0+build").expect("valid"),
            0.0
        );
        let error = compare_model_catalog_kiana_versions("0.85", "0.85.0").expect_err("invalid");
        assert!(
            error.to_string().contains("Invalid Kiana version"),
            "{error}"
        );
    }

    #[test]
    fn selects_the_newest_catalog_a_kiana_version_supports() {
        // #9099：0.85.0 之前的已发布客户端不能用混合 API 目录。
        let index = index();
        let selected = |version: Option<&str>| {
            select_model_catalog(&index, version)
                .expect("query succeeds")
                .map(|entry| entry.revision)
        };
        assert_eq!(
            selected(Some("0.84.4")).as_deref(),
            Some(revision('a').as_str())
        );
        assert_eq!(
            selected(Some("0.85.0-rc.1")).as_deref(),
            Some(revision('a').as_str())
        );
        assert_eq!(
            selected(Some("0.85.0")).as_deref(),
            Some(revision('b').as_str())
        );
        assert_eq!(
            selected(Some("1.0.0")).as_deref(),
            Some(revision('b').as_str())
        );
        assert_eq!(selected(Some("0.80.6")), None);
        assert_eq!(selected(None).as_deref(), Some(revision('b').as_str()));
    }

    #[test]
    fn redirects_kiana_user_agents_to_an_explicit_catalog_version() {
        assert_eq!(
            parse_model_catalog_request(
                "https://kiana.dev/api/models/providers/openrouter?types=chat%2Cimage",
                Some("kiana/0.84.4 (linux; node/v22.0.0; x64)"),
            )
            .expect("valid url"),
            redirect(
                "https://kiana.dev/api/models/providers/openrouter?types=chat%2Cimage&kiana-version=0.84.4"
            )
        );
        assert_eq!(
            parse_model_catalog_request("https://kiana.dev/api/models", Some("kiana/0.84.4"))
                .expect("valid url"),
            redirect("https://kiana.dev/api/models?kiana-version=0.84.4")
        );
    }

    #[test]
    fn serves_explicit_and_unversioned_catalog_requests_without_redirecting() {
        assert_eq!(
            parse_model_catalog_request(
                "https://kiana.dev/api/models?kiana-version=0.83.0",
                Some("kiana/0.85.1 (linux; node/v22.0.0; x64)"),
            )
            .expect("valid url"),
            catalog(Some("0.83.0"), ModelCatalogRepresentation::Legacy)
        );
        assert_eq!(
            parse_model_catalog_request(
                "https://kiana.dev/api/models?types=chat",
                Some("curl/8.0.0")
            )
            .expect("valid url"),
            catalog(None, ModelCatalogRepresentation::Typed)
        );
        assert_eq!(
            parse_model_catalog_request("https://kiana.dev/api/models", Some("kiana/latest"))
                .expect("valid url"),
            catalog(None, ModelCatalogRepresentation::Legacy)
        );
    }

    #[test]
    fn rejects_invalid_catalog_request_parameters() {
        assert_eq!(
            parse_model_catalog_request("https://kiana.dev/api/models?kiana-version=latest", None)
                .expect("valid url"),
            invalid("Invalid Kiana version.")
        );
        assert_eq!(
            parse_model_catalog_request(
                "https://kiana.dev/api/models?types=",
                Some("kiana/0.84.4")
            )
            .expect("valid url"),
            invalid("Invalid model types.")
        );
        assert_eq!(
            parse_model_catalog_representation(Some("chat,-image")),
            None
        );
        assert_eq!(
            parse_model_catalog_representation(None),
            Some(ModelCatalogRepresentation::Legacy)
        );
    }

    #[test]
    fn validates_stored_indexes_and_drops_publication_metadata() {
        let index = index();
        let with_metadata = serde_json::json!({
            "schemaVersion": 1,
            "defaultRevision": index.default_revision.clone(),
            "catalogs": index.catalogs.iter().map(|entry| serde_json::json!({
                "minimumKianaVersion": entry.minimum_kiana_version,
                "revision": entry.revision,
                "sourceCommit": "abc",
                "modelCount": 1,
            })).collect::<Vec<_>>(),
        });
        assert_eq!(
            parse_model_catalog_index(&with_metadata).expect("valid index"),
            index
        );

        let mut wrong_schema = with_metadata.clone();
        wrong_schema["schemaVersion"] = serde_json::json!(2);
        let error = parse_model_catalog_index(&wrong_schema).expect_err("schema version");
        assert!(error.to_string().contains("index is invalid"), "{error}");

        let mut wrong_default = with_metadata.clone();
        wrong_default["defaultRevision"] = serde_json::json!(revision('c'));
        let error = parse_model_catalog_index(&wrong_default).expect_err("default revision");
        assert!(error.to_string().contains("invalid"), "{error}");

        let mut wrong_minimum = with_metadata;
        wrong_minimum["catalogs"] = serde_json::json!([
            { "minimumKianaVersion": "latest", "revision": revision('a') }
        ]);
        let error = parse_model_catalog_index(&wrong_minimum).expect_err("minimum version");
        assert!(error.to_string().contains("invalid"), "{error}");
    }
}

// ---------------------------------------------------------------------------
// `KIANA_USER_AGENT_RE` 与 V8 `Date.parse` 的逐字节回归
//
// 这两个模式都是手写移植的风险点：UA 正则要按 JS 的 `\s` 语义与 `i` 标志工作，
// `Date.parse` 要复刻 V8 `DateParser` 的全部接受/拒绝分支。语料由 Node 26 直接产出
// （`parseModelCatalogRequest` / `Date.parse`，`TZ=UTC`），见 `tests/fixtures/catalog-tooling/`。
// ---------------------------------------------------------------------------

mod user_agent_and_date_parsing {
    use kiana_ai::model_catalog::protocol::{ModelCatalogRequest, parse_model_catalog_request};
    use kiana_ai::model_catalog::{js_date_parse, js_date_parse_with_offset};
    use serde_json::Value;

    fn read_fixture(name: &str) -> Vec<Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/catalog-tooling")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn kind_and_location(
        request: Result<ModelCatalogRequest, kiana_ai::model_catalog::CatalogError>,
    ) -> (&'static str, Option<String>) {
        match request {
            Ok(ModelCatalogRequest::Catalog { .. }) => ("catalog", None),
            Ok(ModelCatalogRequest::Redirect { location }) => ("redirect", Some(location)),
            Ok(ModelCatalogRequest::Invalid { .. }) => ("invalid", None),
            Err(error) => ("error", Some(error.message)),
        }
    }

    #[test]
    fn matches_the_recorded_user_agent_corpus() {
        let cases = read_fixture("user-agent.json");
        assert!(
            cases.len() >= 100,
            "corpus unexpectedly small: {}",
            cases.len()
        );
        let mut mismatches: Vec<String> = Vec::new();
        for case in &cases {
            let input = case["input"].as_str();
            let expected_kind = case["kind"].as_str().expect("kind");
            let expected_location = case["location"].as_str();
            let (kind, location) = kind_and_location(parse_model_catalog_request(
                "https://kiana.dev/api/models",
                input,
            ));
            let same = kind == expected_kind
                && match (location.as_deref(), expected_location) {
                    (Some(actual), Some(expected)) => actual == expected,
                    (None, None) => true,
                    _ => false,
                };
            if !same {
                mismatches.push(format!(
                    "{input:?}: node=({expected_kind},{expected_location:?}) rust=({kind},{location:?})"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} user-agent mismatches (first 20):\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// 非 ASCII 的 UA 不得 panic（旧实现按字节切 `&user_agent[..3]`，多字节首字符会落在字符中间）。
    #[test]
    fn never_panics_on_non_ascii_user_agents() {
        for input in [
            "éé/1.2.3",
            "ππ/1.2.3",
            "😀/1.2.3",
            "\t;é\u{00a0}",
            "\u{feff}kiana/1.2.3",
            "p\u{212a}/1.2.3",
            "kiana/1.2.3 (é)",
            "kiana/1.2.3 (a;\u{00e9})",
        ] {
            let _ = parse_model_catalog_request("https://kiana.dev/api/models", Some(input));
        }
    }

    /// `(a;)`、`(a;;b)`、`(a;;)` 都不匹配 `KIANA_USER_AGENT_RE`（每组都要有非空内容），
    /// 因此不会产生 Redirect。
    #[test]
    fn rejects_empty_user_agent_segments() {
        for input in [
            "kiana/1.2.3 (a;)",
            "kiana/1.2.3 (a;;b)",
            "kiana/1.2.3 (a;;)",
        ] {
            assert_eq!(
                kind_and_location(parse_model_catalog_request(
                    "https://kiana.dev/api/models",
                    Some(input)
                )),
                ("catalog", None),
                "{input}"
            );
        }
        // 合法形态仍然 Redirect。第三段 `[^()]+` 允许分号，所以 `(a; b; c; d)` 也匹配。
        for input in ["kiana/1.2.3 (a; b; c)", "kiana/1.2.3 (a; b; c; d)"] {
            assert_eq!(
                kind_and_location(parse_model_catalog_request(
                    "https://kiana.dev/api/models",
                    Some(input)
                )),
                (
                    "redirect",
                    Some("https://kiana.dev/api/models?kiana-version=1.2.3".to_string())
                ),
                "{input}"
            );
        }
    }

    /// JS 的 `\s` 含 U+FEFF、不含 U+0085；`i` 只对 ASCII `kiana/` 生效。
    #[test]
    fn follows_javascript_whitespace_and_ascii_case_folding() {
        for input in ["KIANA/1.2.3", "Kiana/1.2.3", "kIaNa/1.2.3"] {
            assert_eq!(
                kind_and_location(parse_model_catalog_request(
                    "https://kiana.dev/api/models",
                    Some(input)
                )),
                (
                    "redirect",
                    Some("https://kiana.dev/api/models?kiana-version=1.2.3".to_string())
                ),
                "{input}"
            );
        }
        // U+FEFF 是 JS 空白，可以出现在 `;` 之后。
        assert_eq!(
            kind_and_location(parse_model_catalog_request(
                "https://kiana.dev/api/models",
                Some("kiana/1.2.3 (a;\u{feff}b)")
            )),
            (
                "redirect",
                Some("https://kiana.dev/api/models?kiana-version=1.2.3".to_string())
            )
        );
        // `k\u{212a}ana`（开尔文符号）不是 ASCII `k`，前缀不折叠它。
        assert_eq!(
            kind_and_location(parse_model_catalog_request(
                "https://kiana.dev/api/models",
                Some("k\u{212a}ana/1.2.3")
            )),
            ("catalog", None)
        );
    }

    /// `parseKianaVersion` 用 JS `String.prototype.trim`（含 U+FEFF、不含 U+0085）。
    #[test]
    fn trims_kiana_versions_with_javascript_whitespace() {
        use kiana_ai::model_catalog::protocol::parse_kiana_version;
        assert!(
            parse_kiana_version("\u{feff}1.2.3").is_some(),
            "U+FEFF is JS whitespace"
        );
        assert!(
            parse_kiana_version("\u{3000}1.2.3\u{2028}").is_some(),
            "Zs and line separators"
        );
        assert!(
            parse_kiana_version("\u{85}1.2.3").is_none(),
            "U+0085 is not JS whitespace"
        );
        let parsed = parse_kiana_version(" v1.2.3-rc.1+build ").expect("valid");
        assert_eq!(parsed, (1, 2, 3, "rc.1".to_string()));
        // `Number.isSafeInteger` 的上限。
        assert!(parse_kiana_version("1.2.9007199254740992").is_none());
        assert!(parse_kiana_version("9007199254740991.0.0").is_some());
    }

    /// `new URL(url)` 失败时抛 `TypeError: Invalid URL`（消息不含输入）。
    #[test]
    fn reports_invalid_urls_like_the_url_constructor() {
        let error = parse_model_catalog_request("not a url", None).expect_err("invalid url");
        assert_eq!(error.message, "Invalid URL");
    }

    #[test]
    fn matches_the_recorded_date_parse_corpus() {
        let cases = read_fixture("date-parse.json");
        assert!(
            cases.len() >= 200,
            "corpus unexpectedly small: {}",
            cases.len()
        );
        let mut mismatches: Vec<String> = Vec::new();
        for case in &cases {
            let input = case["input"].as_str().expect("input string");
            let expected = case["expected"].as_f64();
            // 语料由 `TZ=UTC` 的 Node 生成；无时区的传统格式在那里等同 UTC。
            let actual = js_date_parse_with_offset(input, None);
            let same = match (expected, actual) {
                (None, None) => true,
                (Some(expected), Some(actual)) => expected == actual,
                _ => false,
            };
            if !same {
                mismatches.push(format!("{input:?}: node={expected:?} rust={actual:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} date-parse mismatches (first 40):\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// 多字节首字符不得 panic，并且要给出与 V8 一致的结果。
    #[test]
    fn never_panics_on_multibyte_month_names() {
        for input in ["éé/1", "漢字/1/2", "é/1/2", "π/1/2", "😀/1/2", "１２３/1/2"] {
            let _ = js_date_parse(input);
        }
        // V8 的 `ReadWord` 把 `é` 读成 3 字节前缀再补 0，落进关键词表之外，
        // 于是 legacy 解析器把剩下的 `1/2` 当作 `month/day`：`é/1/2` == 2001-01-02
        // （传统格式无时区时按本地时区解释；语料按 `TZ=UTC` 固定）。
        assert_eq!(
            js_date_parse_with_offset("é/1/2", None),
            Some(978_393_600_000.0)
        );
    }

    /// ES5 日期时间与 `TimeClip` 边界的精确毫秒值。
    #[test]
    fn parses_es5_forms_and_clips_at_the_time_range() {
        assert_eq!(
            js_date_parse("2026-10-26T11:07:41.777Z"),
            Some(1_793_012_861_777.0)
        );
        assert_eq!(
            js_date_parse("2026-10-26T11:07:41.777+05:30"),
            Some(1_793_012_861_777.0 - 330.0 * 60_000.0)
        );
        // 只有日期时按 UTC 解释。
        assert_eq!(js_date_parse("2026-10-26"), Some(1_792_972_800_000.0));
        // `24:00` 只在分秒毫秒全为 0 时合法。
        assert_eq!(
            js_date_parse("2026-10-26T24:00:00Z"),
            Some(1_793_059_200_000.0)
        );
        assert_eq!(js_date_parse("2026-10-26T24:00:01Z"), None);
        assert_eq!(js_date_parse("+275760-09-13T00:00:00Z"), Some(8.64e15));
        assert_eq!(js_date_parse("+275760-09-13T00:00:01Z"), None);
    }

    /// V8 的空白判定含 U+FEFF 与 Unicode Zs，不含 U+0085。
    #[test]
    fn uses_v8_whitespace() {
        // 前导空白会让 V8 走传统解析器（无时区 → 本地时间），所以用固定偏移比较
        // 两种解析路径在 TZ=UTC 下的一致性。
        assert_eq!(
            js_date_parse_with_offset("\u{feff}2026-01-01", None),
            js_date_parse_with_offset("2026-01-01", None)
        );
        assert_eq!(
            js_date_parse_with_offset("\u{3000}2026-01-01", None),
            js_date_parse_with_offset("2026-01-01", None)
        );
        assert_eq!(js_date_parse_with_offset("\u{85}2026-01-01", None), None);
    }

    /// 无时区的墙上时间按进程本地偏移换算；符号必须与 V8 一致（东八区 → 减 8 小时）。
    #[test]
    fn interprets_zoneless_wall_time_in_local_timezone() {
        let parsed =
            js_date_parse("\u{feff}2026-01-01").expect("traditional parser accepts prefix");
        let utc = js_date_parse_with_offset("\u{feff}2026-01-01", None).expect("utc");
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = 2026 - 1900;
        tm.tm_mon = 0;
        tm.tm_mday = 1;
        tm.tm_isdst = -1;
        let expected = unsafe { libc::mktime(&mut tm) } as f64 * 1000.0;
        assert_eq!(parsed, expected);
        let offset = utc - parsed;
        assert!(
            (-14.0 * 3_600_000.0..=14.0 * 3_600_000.0).contains(&offset),
            "offset {offset} out of range"
        );
        if std::env::var("TZ").as_deref() == Ok("Asia/Shanghai") {
            assert_eq!(offset, 8.0 * 3_600_000.0);
        }
    }
}

// ---------------------------------------------------------------------------
// 审查修复的回归测试：ICU 排序、toFixed、id 强制转换、超大 prerelease、
// 无时区 Date.parse、生成器保留上游 JSON 值。
// ---------------------------------------------------------------------------

mod icu_collation {
    use std::cmp::Ordering;

    use kiana_ai::model_catalog::locale_compare;

    #[test]
    fn interleaves_latin_accented_letters_with_ascii() {
        // Node 26: `"é".localeCompare("f") === -1`、`"Ö".localeCompare("z") === -1`。
        assert_eq!(locale_compare("é", "f"), Ordering::Less);
        assert_eq!(locale_compare("Ö", "z"), Ordering::Less);
        // 基础字母相同、变音符不同：先比较一次权重，再比较二次权重。
        assert_eq!(locale_compare("e", "é"), Ordering::Less);
        assert_eq!(locale_compare("É", "e"), Ordering::Greater);
    }
}

mod fixed_notation {
    use kiana_ai::model_catalog::{js_to_fixed, round_cost};

    #[test]
    fn keeps_the_negative_sign_when_rounding_to_zero() {
        assert_eq!(js_to_fixed(-1e-7, 6), "-0.000000");
        let rounded = round_cost(-1e-7);
        assert_eq!(rounded, 0.0);
        assert!(rounded.is_sign_negative(), "Number('-0.000000') is -0");
        // `(-0).toFixed(6) === "0.000000"`。
        assert_eq!(js_to_fixed(-0.0, 6), "0.000000");
    }

    #[test]
    fn formats_infinities_like_number_prototype_to_fixed() {
        assert_eq!(js_to_fixed(f64::NEG_INFINITY, 6), "-Infinity");
        assert_eq!(js_to_fixed(f64::INFINITY, 6), "Infinity");
        assert_eq!(round_cost(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }
}

mod flatten_id_coercion {
    use kiana_ai::model_catalog::{ModelGroups, flatten_chat_model_catalog};
    use serde_json::{Value, json};

    #[test]
    fn stringifies_any_id_like_javascript() {
        let mut groups: ModelGroups<Value> = ModelGroups::new();
        let group = groups.entry("openai-completions".to_string()).or_default();
        // `Object.fromEntries([[5, ...], [undefined, ...], ["a", ...]])` 的键。
        group.insert("chat:a".to_string(), json!({ "type": "chat", "id": "a" }));
        group.insert("chat:5".to_string(), json!({ "type": "chat", "id": 5 }));
        group.insert("chat:missing".to_string(), json!({ "type": "chat" }));
        let chat = flatten_chat_model_catalog(&groups);
        // `Object.keys(Object.fromEntries([[...,"a"],[...,5],[...,undefined]]))`：
        // 数组下标键按数值升序排最前，其余键保持插入顺序。
        assert_eq!(
            chat.keys().cloned().collect::<Vec<_>>(),
            vec!["5".to_string(), "a".to_string(), "undefined".to_string()]
        );
    }
}

mod protocol_numeric_edges {
    use kiana_ai::model_catalog::protocol::{
        ModelCatalogIndex, ModelCatalogIndexEntry, compare_model_catalog_kiana_versions,
        select_model_catalog,
    };

    fn revision(letter: char) -> String {
        format!("sha256-{}", letter.to_string().repeat(64))
    }

    #[test]
    fn returns_the_exact_numeric_difference_for_prerelease_numbers() {
        assert_eq!(
            compare_model_catalog_kiana_versions("1.2.3-3", "1.2.3-0").expect("valid"),
            3.0
        );
    }

    #[test]
    fn huge_numeric_prereleases_compare_as_nan_and_are_skipped() {
        let ones = "1".repeat(400);
        let twos = "2".repeat(400);
        // `Number(400 digits)` 是 Infinity，`Infinity - Infinity` 是 NaN。
        assert!(
            compare_model_catalog_kiana_versions(
                &format!("1.2.3-{ones}"),
                &format!("1.2.3-{twos}")
            )
            .expect("valid")
            .is_nan()
        );
        let index = ModelCatalogIndex {
            schema_version: 1,
            default_revision: revision('b'),
            catalogs: vec![ModelCatalogIndexEntry {
                minimum_kiana_version: format!("1.2.3-{twos}"),
                revision: revision('a'),
            }],
        };
        let selected = select_model_catalog(&index, Some(&format!("1.2.3-{ones}"))).expect("query");
        assert!(selected.is_none(), "NaN comparison skips the entry");
    }
}

mod zoneless_date_parsing {
    use kiana_ai::model_catalog::js_date_parse_with_offset;

    const HOUR: i32 = 3_600_000;

    #[test]
    fn interprets_wall_time_with_the_local_offset() {
        // TZ=+08:00：2020-1-1 00:00 本地 = 2019-12-31T16:00Z。
        assert_eq!(
            js_date_parse_with_offset("2020-1-1", Some(8 * HOUR)),
            Some(1_577_808_000_000.0)
        );
    }

    #[test]
    fn applies_time_clip_after_the_local_offset() {
        // 本地 08:00 (+08:00) 正好是 UTC 的 TimeClip 上界。
        assert_eq!(
            js_date_parse_with_offset("+275760-09-13T08:00:00", Some(8 * HOUR)),
            Some(8.64e15)
        );
        assert_eq!(
            js_date_parse_with_offset("+275760-09-13T08:00:01", Some(8 * HOUR)),
            None
        );
        // 本地 00:00 (+08:00) 已低于下界。
        assert_eq!(
            js_date_parse_with_offset("-271821-04-20T00:00:00", Some(8 * HOUR)),
            None
        );
        // 同一墙上时间按 UTC 解释则恰好等于下界。
        assert_eq!(
            js_date_parse_with_offset("-271821-04-20T00:00:00", None),
            Some(-8.64e15)
        );
    }
}

mod generator_value_preservation {
    use kiana_ai::model_catalog::generator::{
        GeneratorInputs, GeneratorState, LoadOptions, catalog_const_name,
        classifier_catalog_const_name, get_models_dev_cost, image_catalog_const_name,
        load_models_dev_data,
    };
    use serde_json::{Value, json};

    #[test]
    fn keeps_non_number_cost_values_for_later_validation() {
        let cost = get_models_dev_cost(Some(&json!({ "input": "0.5" })));
        assert_eq!(
            cost,
            json!({ "input": "0.5", "output": 0, "cacheRead": 0, "cacheWrite": 0 })
        );
    }

    #[test]
    fn keeps_null_tier_sizes() {
        let cost = get_models_dev_cost(Some(&json!({
            "input": 1,
            "tiers": [{ "tier": { "type": "context", "size": null }, "input": 2 }],
        })));
        assert_eq!(
            cost,
            json!({
                "input": 1,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "tiers": [{
                    "inputTokensAbove": null,
                    "input": 2,
                    "output": 0,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                }],
            })
        );
    }

    #[test]
    fn keeps_string_limits_in_models_dev_models() {
        let inputs = GeneratorInputs {
            models_dev: Some(json!({
                "openai": {
                    "models": {
                        "string-limit": {
                            "id": "string-limit",
                            "name": "String Limit",
                            "tool_call": true,
                            "limit": { "context": "8192", "output": 100 },
                        },
                    },
                },
            })),
            ..Default::default()
        };
        let mut state = GeneratorState::default();
        let models = load_models_dev_data(
            &inputs,
            &LoadOptions {
                strict: false,
                nvidia_nim: None,
            },
            &mut state,
        )
        .expect("load");
        let model = models
            .iter()
            .find(|model| model.get("id").and_then(Value::as_str) == Some("string-limit"))
            .expect("string-limit model");
        assert_eq!(model.get("contextWindow"), Some(&json!("8192")));
        assert_eq!(model.get("maxTokens"), Some(&json!(100)));
    }

    fn model_with_cost<'a>(models: &'a [Value], id: &str) -> &'a Value {
        models
            .iter()
            .find(|model| model.get("id").and_then(Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("missing model {id}"))
    }

    #[test]
    fn provider_specific_cost_objects_keep_upstream_value_types() {
        // Vertex / Kimi / Mistral / Moonshot 的 TS 对象字面量用 `m.cost?.input || 0`，
        // 非 number 值会原样进入生成结果。
        let inputs = GeneratorInputs {
            models_dev: Some(json!({
                "google-vertex": { "models": { "gemini-test": {
                    "tool_call": true,
                    "cost": { "input": "0.5" },
                    "limit": { "context": 1000 },
                } } },
                "kimi-code-plan-global": { "models": { "kimi-for-coding": {
                    "tool_call": true,
                    "cost": { "input": "0.5" },
                } } },
                "mistral": { "models": { "mistral-test": {
                    "tool_call": true,
                    "cost": { "input": "0.5" },
                } } },
                "moonshotai": { "models": { "moonshot-test": {
                    "tool_call": true,
                    "cost": { "input": "0.5" },
                } } },
            })),
            ..Default::default()
        };
        let mut state = GeneratorState::default();
        let models = load_models_dev_data(
            &inputs,
            &LoadOptions {
                strict: false,
                nvidia_nim: None,
            },
            &mut state,
        )
        .expect("load");
        for (id, api) in [
            ("gemini-test", "google-vertex"),
            ("kimi-for-coding", "openai-completions"),
            ("mistral-test", "mistral-conversations"),
            ("moonshot-test", "openai-completions"),
        ] {
            let model = model_with_cost(&models, id);
            assert_eq!(model.get("api").and_then(Value::as_str), Some(api), "{id}");
            assert_eq!(model["cost"]["input"], json!("0.5"), "{id} cost.input");
        }
        // Mistral 的 cacheRead 回退使用 `m.cost.input * 0.1`（ToNumber -> roundCost）。
        assert_eq!(
            model_with_cost(&models, "mistral-test")["cost"]["cacheRead"],
            json!(0.05)
        );
    }

    #[test]
    fn collapses_each_non_alphanumeric_run_to_one_underscore() {
        assert_eq!(catalog_const_name("foo--bar"), "FOO_BAR_MODELS");
        assert_eq!(catalog_const_name("--x--"), "_X__MODELS");
        assert_eq!(catalog_const_name("a_-b"), "A_B_MODELS");
        assert_eq!(image_catalog_const_name("foo--bar"), "FOO_BAR_IMAGE_MODELS");
        assert_eq!(
            classifier_catalog_const_name("foo--bar"),
            "FOO_BAR_CLASSIFIER_MODELS"
        );
    }
}
