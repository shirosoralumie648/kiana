//! `packages/tui/test/autocomplete.test.ts`（41 个用例：4 个 `it` + 37 个 `test()`）。
//!
//! TS 源：`packages/tui/test/autocomplete.test.ts`（测试函数文档保留用例标题以便对照）。
//!
//! ## fd 用例的跳过方式
//!
//! TS 用 `describe("fd @ file suggestions", { skip: !isFdInstalled })` 整组跳过 24 个用例；
//! Rust 侧等价做法是**运行时探测** `which fd`（[`resolve_fd_path`]），缺失时打印提示并
//! 直接返回（不断言），存在时执行全部断言。这样在 CI 装上 fd 后同一批用例自动生效。
//!
//! ## 临时目录
//!
//! TS 用 `mkdtempSync(tmpdir(), "kiana-autocomplete-")`；`tempfile` crate 与工作区里 wasmtime 的
//! 版本要求冲突（`tempfile =3.20.0` vs `^3.27.0`），因此这里手写 [`TempDir`]。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_tui::autocomplete::{
    AutocompleteItem, AutocompleteProvider, CombinedAutocompleteProvider, CommandEntry,
    GetSuggestionsOptions, SlashCommand, resolve_fd_path,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 测试基础设施（对应 TS 的 setupFolder / resolveFdPath / getSuggestions）
// ---------------------------------------------------------------------------

/// 临时目录（`mkdtempSync` 的等价物）；`Drop` 时递归删除。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{prefix}{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `setupFolder(baseDir, {dirs, files})`。
fn setup_folder(base_dir: &Path, dirs: &[&str], files: &[(&str, &str)]) {
    for dir in dirs {
        std::fs::create_dir_all(base_dir.join(dir)).expect("mkdir");
    }
    for (file_path, contents) in files {
        let full_path = base_dir.join(file_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(&full_path, contents).expect("write file");
    }
}

/// fd 是否可用（`resolveFdPath()` 的真假）。
fn fd_path() -> Option<String> {
    resolve_fd_path()
}

/// `getSuggestions(provider, lines, cursorLine, cursorCol, force)`。
async fn get_suggestions(
    provider: &mut CombinedAutocompleteProvider,
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
    force: bool,
) -> Option<kiana_tui::autocomplete::AutocompleteSuggestions> {
    let options = GetSuggestionsOptions::new(CancellationToken::new(), force);
    provider
        .get_suggestions(lines, cursor_line, cursor_col, &options)
        .await
}

/// 便捷版本：单行文本 + 光标在 `char` 数处（TS 的 `cursorCol` 是 UTF-16；测试里的字符串
/// 除 emoji 外都是 BMP，这里用 [`utf16_len`] 换算）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn values(items: &[AutocompleteItem]) -> Vec<String> {
    items.iter().map(|item| item.value.clone()).collect()
}

/// 空命令表的 provider（TS 里大量 `new CombinedAutocompleteProvider([], baseDir)`）。
fn provider(base_dir: &str) -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::new(Vec::<CommandEntry>::new(), base_dir, None)
}

fn provider_with_fd(base_dir: &str, fd: &str) -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::new(Vec::<CommandEntry>::new(), base_dir, Some(fd.to_string()))
}

fn ignored_without_fd(test: &str) -> Option<String> {
    match fd_path() {
        Some(fd) => Some(fd),
        None => {
            eprintln!("skipping `{test}`: fd is not installed (TS skips this describe block)");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// describe("extractPathPrefix") — 4 个 it
// ---------------------------------------------------------------------------

/// `extracts / from 'hey /' when forced`
#[tokio::test]
async fn extracts_slash_from_hey_slash_when_forced() {
    let mut provider = provider("/tmp");
    let result = get_suggestions(&mut provider, &["hey /".to_string()], 0, 5, true).await;
    let result = result.expect("Should return suggestions for root directory");
    assert_eq!(result.prefix, "/", "Prefix should be '/'");
}

/// `extracts /A from '/A' when forced`
#[tokio::test]
async fn extracts_slash_a_from_slash_a_when_forced() {
    let mut provider = provider("/tmp");
    let result = get_suggestions(&mut provider, &["/A".to_string()], 0, 2, true).await;
    // `/A` 在 `/tmp` 下通常没有匹配项；TS 只断言“若返回则 prefix 为 /A”。
    if let Some(result) = result {
        assert_eq!(result.prefix, "/A", "Prefix should be '/A'");
    }
}

/// `does not trigger for slash commands`
#[tokio::test]
async fn does_not_trigger_for_slash_commands() {
    let mut provider = provider("/tmp");
    let result = get_suggestions(&mut provider, &["/model".to_string()], 0, 6, true).await;
    assert!(result.is_none(), "Should not trigger for slash commands");
}

/// `triggers for absolute paths after slash command argument`
#[tokio::test]
async fn triggers_for_absolute_paths_after_slash_command_argument() {
    let mut provider = provider("/tmp");
    let result = get_suggestions(&mut provider, &["/command /".to_string()], 0, 10, true).await;
    let result = result.expect("Should trigger for absolute paths in command arguments");
    assert_eq!(result.prefix, "/", "Prefix should be '/'");
}

// ---------------------------------------------------------------------------
// describe("fd @ file suggestions") — 24 个 test()，整组 `{ skip: !isFdInstalled }`
// ---------------------------------------------------------------------------

/// 每个 fd 用例共用的“根目录 / cwd / outside”三段结构（对应 TS 的 beforeEach/afterEach）。
struct FdFixture {
    root: TempDir,
    base_dir: PathBuf,
    outside_dir: PathBuf,
    fd: String,
}

impl FdFixture {
    fn new(test_name: &str) -> Option<Self> {
        let fd = ignored_without_fd(test_name)?;
        let root = TempDir::new("kiana-autocomplete-root-");
        let base_dir = root.join("cwd");
        let outside_dir = root.join("outside");
        std::fs::create_dir_all(&base_dir).expect("mkdir cwd");
        std::fs::create_dir_all(&outside_dir).expect("mkdir outside");
        Some(Self {
            root,
            base_dir,
            outside_dir,
            fd,
        })
    }

    fn base(&self) -> String {
        self.base_dir.to_string_lossy().into_owned()
    }

    fn provider(&self) -> CombinedAutocompleteProvider {
        provider_with_fd(&self.base(), &self.fd)
    }
}

/// `returns all files and folders for empty @ query`
#[tokio::test]
async fn returns_all_files_and_folders_for_empty_at_query() {
    let Some(fixture) = FdFixture::new("returns all files and folders for empty @ query") else {
        return;
    };
    setup_folder(&fixture.base_dir, &["src"], &[("README.md", "readme")]);

    let mut provider = fixture.provider();
    let line = "@";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, 1, false).await;
    let mut values: Vec<String> = values(&result.expect("suggestions").items);
    values.sort();
    let mut expected = vec!["@README.md".to_string(), "@src/".to_string()];
    expected.sort();
    assert_eq!(values, expected);
}

/// `recognizes @ after CJK punctuation without consuming the preceding text`
#[tokio::test]
async fn recognizes_at_after_cjk_punctuation_without_consuming_preceding_text() {
    let Some(fixture) =
        FdFixture::new("recognizes @ after CJK punctuation without consuming the preceding text")
    else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("README.md", "readme")]);
    let mut provider = fixture.provider();

    let mut prefixes: Vec<String> = vec!["查看，".to_string(), "\u{3000}".to_string()];
    prefixes.extend(
        "，．：；！？（）［］｛｝“”‘’…—。、「」『』《》【】"
            .chars()
            .map(|ch| ch.to_string()),
    );

    for before in prefixes {
        for force in [false, true] {
            let line = format!("{before}@REA");
            let col = utf16_len(&line);
            let result =
                get_suggestions(&mut provider, std::slice::from_ref(&line), 0, col, force).await;
            let result = result.unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, "@REA");
            assert_eq!(values(&result.items), vec!["@README.md".to_string()]);

            let applied = provider.apply_completion(
                std::slice::from_ref(&line),
                0,
                col,
                &result.items[0],
                &result.prefix,
            );
            assert_eq!(applied.lines[0], format!("{before}@README.md "));
            assert_eq!(applied.cursor_col, utf16_len(&applied.lines[0]));
        }
    }
}

/// `recognizes @ after opening wrappers like ( and backticks`
#[tokio::test]
async fn recognizes_at_after_opening_wrappers_like_paren_and_backticks() {
    let Some(fixture) = FdFixture::new("recognizes @ after opening wrappers like ( and backticks")
    else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("README.md", "readme")]);
    let mut provider = fixture.provider();

    for before in ["(", "see (", "[", "`", "<", "{"] {
        let line = format!("{before}@REA");
        let col = utf16_len(&line);
        let result =
            get_suggestions(&mut provider, std::slice::from_ref(&line), 0, col, false).await;
        let result = result.unwrap_or_else(|| panic!("{line}"));
        assert_eq!(result.prefix, "@REA");
        let applied = provider.apply_completion(
            std::slice::from_ref(&line),
            0,
            col,
            &result.items[0],
            &result.prefix,
        );
        assert_eq!(applied.lines[0], format!("{before}@README.md "));
    }

    let embedded = "foo(@REA";
    assert!(
        get_suggestions(
            &mut provider,
            &[embedded.to_string()],
            0,
            utf16_len(embedded),
            false
        )
        .await
        .is_none()
    );
}

/// `preserves CJK characters and embedded @ in attachment paths`
#[tokio::test]
async fn preserves_cjk_characters_and_embedded_at_in_attachment_paths() {
    let Some(fixture) =
        FdFixture::new("preserves CJK characters and embedded @ in attachment paths")
    else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[("文档/说明.md", "text"), ("文档@备份/说明.md", "backup")],
    );
    let mut provider = fixture.provider();

    for before in ["", "查看，"] {
        for directory in ["文档", "文档@备份"] {
            let prefix = format!("@{directory}/说");
            let line = format!("{before}{prefix}");
            let result = get_suggestions(
                &mut provider,
                std::slice::from_ref(&line),
                0,
                utf16_len(&line),
                false,
            )
            .await;
            let result = result.unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, prefix);
            assert_eq!(values(&result.items), vec![format!("@{directory}/说明.md")]);
        }
    }
}

/// `completes quoted CJK attachments after prose without losing path segments or quotes`
#[tokio::test]
async fn completes_quoted_cjk_attachments_after_prose_without_losing_path_segments_or_quotes() {
    let Some(fixture) = FdFixture::new(
        "completes quoted CJK attachments after prose without losing path segments or quotes",
    ) else {
        return;
    };
    let mut provider = fixture.provider();

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("我的{separator}文档");
        setup_folder(
            &fixture.base_dir,
            &[],
            &[
                (&format!("{directory}/说明.md"), "text"),
                ("文档/说明.md", "not the quoted path"),
            ],
        );
        let line = format!("查看：@\"{directory}/说\"后文");
        // TS 的 `line.indexOf('"后文')` 是 UTF-16 索引；Rust 的 `find` 是字节索引，需换算。
        let cursor_col = utf16_len(&line[..line.find("\"后文").expect("cursor")]);
        let result = get_suggestions(
            &mut provider,
            std::slice::from_ref(&line),
            0,
            cursor_col,
            false,
        )
        .await;
        let result = result.expect("suggestions");
        assert_eq!(result.prefix, format!("@\"{directory}/说"));
        assert_eq!(
            values(&result.items),
            vec![format!("@\"{directory}/说明.md\"")]
        );

        let applied = provider.apply_completion(
            std::slice::from_ref(&line),
            0,
            cursor_col,
            &result.items[0],
            &result.prefix,
        );
        assert_eq!(
            applied.lines[0],
            format!("查看：@\"{directory}/说明.md\" 后文")
        );
        assert_eq!(
            applied.cursor_col,
            utf16_len(&format!("查看：@\"{directory}/说明.md\" "))
        );
    }
}

/// `does not interpret email addresses or @ after ASCII or CJK letters as attachment prefixes`
#[tokio::test]
async fn does_not_interpret_email_addresses_or_at_after_ascii_or_cjk_letters() {
    let Some(fixture) = FdFixture::new(
        "does not interpret email addresses or @ after ASCII or CJK letters as attachment prefixes",
    ) else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[("README.md", "readme"), ("example.com", "text")],
    );
    let mut provider = fixture.provider();

    for before in [
        "user",
        "查看",
        "あ",
        "カ",
        "한",
        "ㄅ",
        "𠮷",
        "か\u{3099}",
        "禰\u{e0100}",
        "々",
        "Ａ",
    ] {
        for name in ["REA", "example.com"] {
            let line = format!("{before}@{name}");
            assert!(
                get_suggestions(
                    &mut provider,
                    std::slice::from_ref(&line),
                    0,
                    utf16_len(&line),
                    false
                )
                .await
                .is_none(),
                "{line}"
            );
        }
    }
}

/// `matches file with extension in query`
#[tokio::test]
async fn matches_file_with_extension_in_query() {
    let Some(fixture) = FdFixture::new("matches file with extension in query") else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("file.txt", "content")]);
    let mut provider = fixture.provider();

    let line = "@file.txt";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert!(values(&result.items).contains(&"@file.txt".to_string()));
}

/// `matches regex metacharacters in queries literally`（#10972）
#[tokio::test]
async fn matches_regex_metacharacters_in_queries_literally() {
    let Some(fixture) = FdFixture::new("matches regex metacharacters in queries literally") else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[
            ("pages/[id].tsx", "x"),
            ("pages/index.tsx", "x"),
            ("pages/(group).ts", "x"),
        ],
    );
    let mut provider = fixture.provider();

    for (line, expected) in [
        ("@pages/[", "@pages/[id].tsx"),
        ("@pages/(", "@pages/(group).ts"),
        ("@[id", "@pages/[id].tsx"),
    ] {
        let result = get_suggestions(
            &mut provider,
            &[line.to_string()],
            0,
            utf16_len(line),
            false,
        )
        .await
        .expect("suggestions");
        assert_eq!(values(&result.items), vec![expected.to_string()], "{line}");
    }
}

/// `filters are case insensitive`
#[tokio::test]
async fn filters_are_case_insensitive() {
    let Some(fixture) = FdFixture::new("filters are case insensitive") else {
        return;
    };
    setup_folder(&fixture.base_dir, &["src"], &[("README.md", "readme")]);
    let mut provider = fixture.provider();

    let line = "@re";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let mut values = values(&result.items);
    values.sort();
    assert_eq!(values, vec!["@README.md".to_string()]);
}

/// `ranks directories before files`
#[tokio::test]
async fn ranks_directories_before_files() {
    let Some(fixture) = FdFixture::new("ranks directories before files") else {
        return;
    };
    setup_folder(&fixture.base_dir, &["src"], &[("src.txt", "text")]);
    let mut provider = fixture.provider();

    let line = "@src";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(result.items[0].value, "@src/");
    assert!(values(&result.items).contains(&"@src.txt".to_string()));
}

/// `returns nested file paths`
#[tokio::test]
async fn returns_nested_file_paths() {
    let Some(fixture) = FdFixture::new("returns nested file paths") else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("src/index.ts", "export {};\n")]);
    let mut provider = fixture.provider();

    let line = "@index";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert!(values(&result.items).contains(&"@src/index.ts".to_string()));
}

/// `matches deeply nested paths`
#[tokio::test]
async fn matches_deeply_nested_paths() {
    let Some(fixture) = FdFixture::new("matches deeply nested paths") else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[
            ("packages/tui/src/autocomplete.ts", "export {};"),
            ("packages/ai/src/autocomplete.ts", "export {};"),
        ],
    );
    let mut provider = fixture.provider();

    let line = "@tui/src/auto";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert!(values.contains(&"@packages/tui/src/autocomplete.ts".to_string()));
    assert!(!values.contains(&"@packages/ai/src/autocomplete.ts".to_string()));
}

/// `matches directory in middle of path with --full-path`
#[tokio::test]
async fn matches_directory_in_middle_of_path_with_full_path() {
    let Some(fixture) = FdFixture::new("matches directory in middle of path with --full-path")
    else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[
            ("src/components/Button.tsx", "export {};"),
            ("src/utils/helpers.ts", "export {};"),
        ],
    );
    let mut provider = fixture.provider();

    let line = "@components/";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert!(values.contains(&"@src/components/Button.tsx".to_string()));
    assert!(!values.contains(&"@src/utils/helpers.ts".to_string()));
}

/// `scopes fuzzy search to relative directories and searches recursively`
#[tokio::test]
async fn scopes_fuzzy_search_to_relative_directories_and_searches_recursively() {
    let Some(fixture) =
        FdFixture::new("scopes fuzzy search to relative directories and searches recursively")
    else {
        return;
    };
    setup_folder(
        &fixture.outside_dir,
        &[],
        &[
            ("nested/alpha.ts", "export {};"),
            ("nested/deeper/also-alpha.ts", "export {};"),
            ("nested/deeper/zzz.ts", "export {};"),
        ],
    );
    let mut provider = fixture.provider();

    let line = "@../outside/a";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert!(values.contains(&"@../outside/nested/alpha.ts".to_string()));
    assert!(values.contains(&"@../outside/nested/deeper/also-alpha.ts".to_string()));
    assert!(!values.contains(&"@../outside/nested/deeper/zzz.ts".to_string()));
}

/// `ranks shallower same-score @ matches before deeper matches`
#[tokio::test]
async fn ranks_shallower_same_score_at_matches_before_deeper_matches() {
    let Some(fixture) =
        FdFixture::new("ranks shallower same-score @ matches before deeper matches")
    else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[
            "scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile",
            "scope/projects",
        ],
        &[],
    );
    let mut provider = fixture.provider();

    let line = "@scope/pro";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert_eq!(values[0], "@scope/projects/");
    assert!(
        values.contains(
            &"@scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile/".to_string()
        )
    );
}

/// `includes scoped direct children when recursive @ matches are flooded`
#[tokio::test]
async fn includes_scoped_direct_children_when_recursive_at_matches_are_flooded() {
    let Some(fixture) =
        FdFixture::new("includes scoped direct children when recursive @ matches are flooded")
    else {
        return;
    };
    let flooded: Vec<String> = (0..250)
        .map(|index| {
            format!(
                "scope/a{:03}/venv/lib/python3.12/site-packages/pkg/core/profile",
                index + 1
            )
        })
        .collect();
    let mut dirs: Vec<&str> = vec!["scope/projects"];
    dirs.extend(flooded.iter().map(String::as_str));
    setup_folder(&fixture.base_dir, &dirs, &[]);
    let mut provider = fixture.provider();

    let line = "@scope/pro";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert_eq!(values[0], "@scope/projects/");
    assert!(
        values.iter().any(|value| value.contains("/profile/")),
        "Should keep deep fuzzy matches after direct children"
    );
}

/// `quotes paths containing whitespace or CJK punctuation for @ suggestions`
#[tokio::test]
async fn quotes_paths_containing_whitespace_or_cjk_punctuation_for_at_suggestions() {
    let Some(fixture) =
        FdFixture::new("quotes paths containing whitespace or CJK punctuation for @ suggestions")
    else {
        return;
    };
    let mut provider = fixture.provider();

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("my{separator}folder");
        setup_folder(
            &fixture.base_dir,
            &[],
            &[(&format!("{directory}/test.txt"), "content")],
        );
        let line = "@my";
        let result = get_suggestions(
            &mut provider,
            &[line.to_string()],
            0,
            utf16_len(line),
            false,
        )
        .await
        .expect("suggestions");
        let expected = format!("@\"{directory}/\"");
        let item = result
            .items
            .iter()
            .find(|entry| entry.value == expected)
            .unwrap_or_else(|| panic!("{directory}"));

        let applied = provider.apply_completion(
            &[line.to_string()],
            0,
            utf16_len(line),
            item,
            &result.prefix,
        );
        let continued =
            get_suggestions(&mut provider, &applied.lines, 0, applied.cursor_col, false)
                .await
                .expect("continued suggestions");
        assert_eq!(continued.prefix, format!("@\"{directory}/"));
        assert!(values(&continued.items).contains(&format!("@\"{directory}/test.txt\"")));
    }
}

/// `includes hidden paths but excludes .git`
#[tokio::test]
async fn includes_hidden_paths_but_excludes_git() {
    let Some(fixture) = FdFixture::new("includes hidden paths but excludes .git") else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[".kiana", ".github", ".git"],
        &[
            (".kiana/config.json", "{}"),
            (".github/workflows/ci.yml", "name: ci"),
            (".git/config", "[core]"),
        ],
    );
    let mut provider = fixture.provider();

    let line = "@";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, 1, false)
        .await
        .expect("suggestions");
    let values = values(&result.items);
    assert!(values.contains(&"@.kiana/".to_string()));
    assert!(values.contains(&"@.github/".to_string()));
    assert!(
        !values
            .iter()
            .any(|value| value == "@.git" || value.starts_with("@.git/"))
    );
}

/// `follows symlinked directories for fuzzy @ search`
#[tokio::test]
async fn follows_symlinked_directories_for_fuzzy_at_search() {
    let Some(fixture) = FdFixture::new("follows symlinked directories for fuzzy @ search") else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("dir/some_file.txt", "real")]);
    setup_folder(&fixture.outside_dir, &[], &[("some_file.txt", "symlinked")]);
    std::os::unix::fs::symlink("../outside", fixture.base_dir.join("symlinked_dir"))
        .expect("symlink");

    let mut provider = fixture.provider();
    let line = "@some";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    let values = values(&result.items);
    assert!(values.contains(&"@dir/some_file.txt".to_string()));
    assert!(values.contains(&"@symlinked_dir/some_file.txt".to_string()));
}

/// `returns symlinked directories when matching their name`
#[tokio::test]
async fn returns_symlinked_directories_when_matching_their_name() {
    let Some(fixture) = FdFixture::new("returns symlinked directories when matching their name")
    else {
        return;
    };
    setup_folder(
        &fixture.outside_dir,
        &[],
        &[("nested/file.txt", "symlinked")],
    );
    std::os::unix::fs::symlink("../outside", fixture.base_dir.join("symlinked_dir"))
        .expect("symlink");

    let mut provider = fixture.provider();
    let line = "@symlinked";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert!(values(&result.items).contains(&"@symlinked_dir/".to_string()));
}

/// `returns symlinked files without requiring type l`
#[tokio::test]
async fn returns_symlinked_files_without_requiring_type_l() {
    let Some(fixture) = FdFixture::new("returns symlinked files without requiring type l") else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("original.txt", "content")]);
    let link_path = fixture.base_dir.join("link.txt");
    std::os::unix::fs::symlink("original.txt", link_path).expect("symlink");

    let mut provider = fixture.provider();
    let line = "@link";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert!(values(&result.items).contains(&"@link.txt".to_string()));
}

/// `returns the same @ suggestions when the cwd path contains the query`
#[tokio::test]
async fn returns_the_same_at_suggestions_when_the_cwd_path_contains_the_query() {
    let Some(fixture) =
        FdFixture::new("returns the same @ suggestions when the cwd path contains the query")
    else {
        return;
    };
    let normal_base_dir = fixture.root.join("cwd-normal");
    let query_in_path_base_dir = fixture.root.join("cwd-plan-repro");
    std::fs::create_dir_all(&normal_base_dir).expect("mkdir");
    std::fs::create_dir_all(&query_in_path_base_dir).expect("mkdir");

    for base in [&normal_base_dir, &query_in_path_base_dir] {
        setup_folder(
            base,
            &["packages/coding-agent/examples/extensions/plan-mode"],
            &[
                (
                    "packages/coding-agent/examples/extensions/plan-mode/README.md",
                    "readme",
                ),
                ("packages/tui/docs/plan.md", "plan"),
            ],
        );
    }

    let query = "@plan";
    let mut normal_provider = provider_with_fd(&normal_base_dir.to_string_lossy(), &fixture.fd);
    let mut query_provider =
        provider_with_fd(&query_in_path_base_dir.to_string_lossy(), &fixture.fd);

    let normal_result = get_suggestions(
        &mut normal_provider,
        &[query.to_string()],
        0,
        utf16_len(query),
        false,
    )
    .await;
    let query_result = get_suggestions(
        &mut query_provider,
        &[query.to_string()],
        0,
        utf16_len(query),
        false,
    )
    .await;

    let normalize = |result: Option<kiana_tui::autocomplete::AutocompleteSuggestions>| {
        let mut entries: Vec<String> = result
            .map(|suggestions| {
                suggestions
                    .items
                    .iter()
                    .map(|item| {
                        format!(
                            "{} :: {}",
                            item.label,
                            item.description.clone().unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort();
        entries
    };

    assert_eq!(normalize(query_result), normalize(normal_result.clone()));
    let normal = normalize(normal_result);
    assert!(normal.contains(
        &"plan-mode/ :: packages/coding-agent/examples/extensions/plan-mode".to_string()
    ));
    assert!(normal.contains(&"plan.md :: packages/tui/docs/plan.md".to_string()));
}

/// `continues autocomplete inside quoted @ paths`
#[tokio::test]
async fn continues_autocomplete_inside_quoted_at_paths() {
    let Some(fixture) = FdFixture::new("continues autocomplete inside quoted @ paths") else {
        return;
    };
    setup_folder(
        &fixture.base_dir,
        &[],
        &[
            ("my folder/test.txt", "content"),
            ("my folder/other.txt", "content"),
        ],
    );
    let mut provider = fixture.provider();

    let line = "@\"my folder/\"";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line) - 1,
        false,
    )
    .await
    .expect("Should return suggestions for quoted folder path");
    let values = values(&result.items);
    assert!(values.contains(&"@\"my folder/test.txt\"".to_string()));
    assert!(values.contains(&"@\"my folder/other.txt\"".to_string()));
}

/// `applies quoted @ completion without duplicating closing quote`
#[tokio::test]
async fn applies_quoted_at_completion_without_duplicating_closing_quote() {
    let Some(fixture) =
        FdFixture::new("applies quoted @ completion without duplicating closing quote")
    else {
        return;
    };
    setup_folder(&fixture.base_dir, &[], &[("my folder/test.txt", "content")]);
    let mut provider = fixture.provider();

    let line = "@\"my folder/te\"";
    let cursor_col = utf16_len(line) - 1;
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, cursor_col, false)
        .await
        .expect("Should return suggestions for quoted @ path");
    let item = result
        .items
        .iter()
        .find(|entry| entry.value == "@\"my folder/test.txt\"")
        .expect("Should find test.txt suggestion");

    let applied =
        provider.apply_completion(&[line.to_string()], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "@\"my folder/test.txt\" ");
}

// ---------------------------------------------------------------------------
// describe("dot-slash path completion") — 9 个 test()，不依赖 fd
// ---------------------------------------------------------------------------

/// `completes Chinese path prefixes after whitespace or CJK punctuation on Tab`
#[tokio::test]
async fn completes_chinese_path_prefixes_after_whitespace_or_cjk_punctuation_on_tab() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(
        base.path(),
        &[],
        &[("说明.md", "file"), ("文档/说明.md", "nested file")],
    );
    let mut provider = provider(&base.str());

    let mut completions: Vec<(String, String)> = vec![
        ("说".to_string(), "说明.md".to_string()),
        ("文".to_string(), "文档/".to_string()),
        ("文档/说".to_string(), "文档/说明.md".to_string()),
        ("./文档/说".to_string(), "./文档/说明.md".to_string()),
    ];
    if !cfg!(windows) {
        completions.push((
            format!("{}/文档/说", base.str()),
            format!("{}/文档/说明.md", base.str()),
        ));
    }

    for separator in " \t\u{3000}\u{00A0}，：；。！？（「《".chars() {
        for (prefix, value) in &completions {
            let before = format!("查看𠮷{separator}");
            let line = format!("{before}{prefix} 后文");
            let cursor_col = utf16_len(&before) + utf16_len(prefix);
            let result = get_suggestions(
                &mut provider,
                std::slice::from_ref(&line),
                0,
                cursor_col,
                true,
            )
            .await;
            let result = result.unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, *prefix);
            assert_eq!(values(&result.items), vec![value.clone()]);

            let applied = provider.apply_completion(
                std::slice::from_ref(&line),
                0,
                cursor_col,
                &result.items[0],
                &result.prefix,
            );
            assert_eq!(applied.lines[0], format!("{before}{value} 后文"));
            assert_eq!(applied.cursor_col, utf16_len(&before) + utf16_len(value));
        }
    }
}

/// `treats unquoted separators as boundaries even when a matching literal path exists`
#[tokio::test]
async fn treats_unquoted_separators_as_boundaries_even_when_a_matching_literal_path_exists() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("归档/说明.md", "other")]);
    let mut provider = provider(&base.str());

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("资料{separator}归档");
        setup_folder(
            base.path(),
            &[],
            &[(&format!("{directory}/说明.md"), "archive")],
        );

        for marker in ["", "@"] {
            let line = format!("{marker}{directory}/说");
            let result = get_suggestions(
                &mut provider,
                std::slice::from_ref(&line),
                0,
                utf16_len(&line),
                true,
            )
            .await
            .unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, "归档/说");
            assert_eq!(values(&result.items), vec!["归档/说明.md".to_string()]);
        }

        let quoted = format!("查看，\"{directory}/说\"后文");
        let cursor_col = utf16_len(&quoted[..quoted.find("\"后文").expect("cursor")]);
        let result = get_suggestions(
            &mut provider,
            std::slice::from_ref(&quoted),
            0,
            cursor_col,
            true,
        )
        .await
        .expect("suggestions");
        assert_eq!(result.prefix, format!("\"{directory}/说"));
        assert_eq!(
            values(&result.items),
            vec![format!("\"{directory}/说明.md\"")]
        );

        let applied = provider.apply_completion(
            std::slice::from_ref(&quoted),
            0,
            cursor_col,
            &result.items[0],
            &result.prefix,
        );
        assert_eq!(
            applied.lines[0],
            format!("查看，\"{directory}/说明.md\"后文")
        );

        let missing = format!("查看，\"不存在{separator}归档/说");
        assert!(
            get_suggestions(
                &mut provider,
                std::slice::from_ref(&missing),
                0,
                utf16_len(&missing),
                true
            )
            .await
            .is_none()
        );
    }
}

/// `handles an empty prefix after whitespace or CJK punctuation consistently`
#[tokio::test]
async fn handles_an_empty_prefix_after_whitespace_or_cjk_punctuation_consistently() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("说明.md", "text")]);
    let mut provider = provider(&base.str());

    for separator in [" ", "\t", "\u{3000}", "，", "。"] {
        for force in [false, true] {
            let line = format!("查看{separator}");
            let result = get_suggestions(
                &mut provider,
                std::slice::from_ref(&line),
                0,
                utf16_len(&line),
                force,
            )
            .await
            .unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, "");
            assert_eq!(values(&result.items), vec!["说明.md".to_string()]);
        }
    }
    assert!(
        get_suggestions(&mut provider, &[String::new()], 0, 0, false)
            .await
            .is_none()
    );
}

/// `completes paths after opening wrappers like ( [ { < and backticks`
#[tokio::test]
async fn completes_paths_after_opening_wrappers_like_brackets_and_backticks() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("src/main.ts", "x")]);
    let mut provider = provider(&base.str());

    for wrapper in ["(", "[", "{", "<", "`", "((", "(`"] {
        for prefix in ["src/ma", "./src/ma"] {
            let before = format!("see {wrapper}");
            let line = format!("{before}{prefix}");
            let result = get_suggestions(
                &mut provider,
                std::slice::from_ref(&line),
                0,
                utf16_len(&line),
                true,
            )
            .await
            .unwrap_or_else(|| panic!("{line}"));
            assert_eq!(result.prefix, prefix);
            let value = prefix.replace("src/ma", "src/main.ts");
            assert_eq!(values(&result.items), vec![value.clone()]);

            let applied = provider.apply_completion(
                std::slice::from_ref(&line),
                0,
                utf16_len(&line),
                &result.items[0],
                &result.prefix,
            );
            assert_eq!(applied.lines[0], format!("{before}{value}"));
        }
    }
}

/// `completes quoted paths after opening wrappers`
#[tokio::test]
async fn completes_quoted_paths_after_opening_wrappers() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("my dir/main.ts", "x")]);
    let mut provider = provider(&base.str());

    let line = "see (\"my dir/ma";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, utf16_len(line), true)
        .await
        .expect("suggestions");
    assert_eq!(result.prefix, "\"my dir/ma");
    assert_eq!(
        values(&result.items),
        vec!["\"my dir/main.ts\"".to_string()]
    );
}

/// `keeps wrappers that are closed inside the path`
#[tokio::test]
async fn keeps_wrappers_that_are_closed_inside_the_path() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(
        base.path(),
        &[],
        &[("[slug]/page.tsx", "x"), ("(group)/layout.tsx", "x")],
    );
    let mut provider = provider(&base.str());

    for (prefix, value) in [
        ("[slug]/pa", "[slug]/page.tsx"),
        ("(group)/la", "(group)/layout.tsx"),
        ("./[slug]/pa", "./[slug]/page.tsx"),
    ] {
        let line = format!("see {prefix}");
        let result = get_suggestions(
            &mut provider,
            std::slice::from_ref(&line),
            0,
            utf16_len(&line),
            true,
        )
        .await
        .unwrap_or_else(|| panic!("{line}"));
        assert_eq!(result.prefix, prefix);
        assert_eq!(values(&result.items), vec![value.to_string()]);
    }
}

/// `preserves CJK characters in unprefixed Tab completions`
#[tokio::test]
async fn preserves_cjk_characters_in_unprefixed_tab_completions() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("文档/说明.md", "text")]);
    let mut provider = provider(&base.str());

    let line = "文档/说";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, utf16_len(line), true)
        .await
        .expect("suggestions");
    assert_eq!(result.prefix, line);
    assert_eq!(values(&result.items), vec!["文档/说明.md".to_string()]);
}

/// `preserves ./ prefix when completing paths`
#[tokio::test]
async fn preserves_dot_slash_prefix_when_completing_paths() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(
        base.path(),
        &[],
        &[("update.sh", "#!/bin/bash"), ("utils.ts", "export {};")],
    );
    let mut provider = provider(&base.str());

    let line = "./up";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, utf16_len(line), true)
        .await
        .expect("suggestions");
    assert!(
        values(&result.items).contains(&"./update.sh".to_string()),
        "Expected ./update.sh in {:?}",
        values(&result.items)
    );
}

/// `preserves ./ prefix for directory completions`
#[tokio::test]
async fn preserves_dot_slash_prefix_for_directory_completions() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &["src"], &[("src/index.ts", "export {};")]);
    let mut provider = provider(&base.str());

    let line = "./sr";
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, utf16_len(line), true)
        .await
        .expect("suggestions");
    assert!(
        values(&result.items).contains(&"./src/".to_string()),
        "Expected ./src/ in {:?}",
        values(&result.items)
    );
}

// ---------------------------------------------------------------------------
// describe("quoted path completion") — 4 个 test()，不依赖 fd
// ---------------------------------------------------------------------------

/// `quotes paths containing whitespace or CJK punctuation for direct completion`
#[tokio::test]
async fn quotes_paths_containing_whitespace_or_cjk_punctuation_for_direct_completion() {
    let base = TempDir::new("kiana-autocomplete-");
    let mut provider = provider(&base.str());

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("my{separator}folder");
        setup_folder(
            base.path(),
            &[],
            &[(&format!("{directory}/test.txt"), "content")],
        );

        let line = "my";
        let result = get_suggestions(&mut provider, &[line.to_string()], 0, utf16_len(line), true)
            .await
            .expect("suggestions");
        let expected = format!("\"{directory}/\"");
        let item = result
            .items
            .iter()
            .find(|entry| entry.value == expected)
            .unwrap_or_else(|| panic!("{directory}"));

        let applied = provider.apply_completion(
            &[line.to_string()],
            0,
            utf16_len(line),
            item,
            &result.prefix,
        );
        let continued = get_suggestions(&mut provider, &applied.lines, 0, applied.cursor_col, true)
            .await
            .expect("continued suggestions");
        assert_eq!(continued.prefix, format!("\"{directory}/"));
        assert_eq!(
            values(&continued.items),
            vec![format!("\"{directory}/test.txt\"")]
        );
    }
}

/// `keeps quoted directories before files`
#[tokio::test]
async fn keeps_quoted_directories_before_files() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(
        base.path(),
        &["z folder", "z，folder"],
        &[("a.txt", "text")],
    );
    let mut provider = provider(&base.str());

    let result = get_suggestions(&mut provider, &[String::new()], 0, 0, true)
        .await
        .expect("suggestions");
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.label.ends_with('/'))
            .collect::<Vec<_>>(),
        vec![true, true, false]
    );
}

/// `continues completion inside quoted paths`
#[tokio::test]
async fn continues_completion_inside_quoted_paths() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(
        base.path(),
        &[],
        &[
            ("my folder/test.txt", "content"),
            ("my folder/other.txt", "content"),
        ],
    );
    let mut provider = provider(&base.str());

    let line = "\"my folder/\"";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line) - 1,
        true,
    )
    .await
    .expect("Should return suggestions for quoted folder path");
    let values = values(&result.items);
    assert!(values.contains(&"\"my folder/test.txt\"".to_string()));
    assert!(values.contains(&"\"my folder/other.txt\"".to_string()));
}

/// `applies quoted completion without duplicating closing quote`
#[tokio::test]
async fn applies_quoted_completion_without_duplicating_closing_quote() {
    let base = TempDir::new("kiana-autocomplete-");
    setup_folder(base.path(), &[], &[("my folder/test.txt", "content")]);
    let mut provider = provider(&base.str());

    let line = "\"my folder/te\"";
    let cursor_col = utf16_len(line) - 1;
    let result = get_suggestions(&mut provider, &[line.to_string()], 0, cursor_col, true)
        .await
        .expect("Should return suggestions for quoted path");
    let item = result
        .items
        .iter()
        .find(|entry| entry.value == "\"my folder/test.txt\"")
        .expect("Should find test.txt suggestion");

    let applied =
        provider.apply_completion(&[line.to_string()], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "\"my folder/test.txt\"");
}

// ---------------------------------------------------------------------------
// shouldTriggerFileCompletion（spec §2.10.10 的实测表）
// ---------------------------------------------------------------------------

#[test]
fn should_trigger_file_completion_matches_ts() {
    let provider = provider("/tmp");
    for (line, expected) in [
        ("/mod", false),
        ("/model x", true),
        ("hi", true),
        ("  /mod", false),
    ] {
        assert_eq!(
            provider.should_trigger_file_completion_impl(&[line.to_string()], 0, utf16_len(line)),
            expected,
            "{line}"
        );
    }
}

// ---------------------------------------------------------------------------
// 斜杠命令补全的形状（spec §2.10.11 的实测表）
// ---------------------------------------------------------------------------

fn command_fixture() -> Vec<CommandEntry> {
    vec![
        CommandEntry::Slash(
            SlashCommand::new("model")
                .with_description("Select the active model")
                .with_argument_hint("<name>"),
        ),
        CommandEntry::Slash(SlashCommand::new("skill:research-idea").with_description("Refine")),
        CommandEntry::Slash(SlashCommand::new("noDesc")),
        CommandEntry::Item(AutocompleteItem::new("valOnly", "valOnly")),
    ]
}

#[tokio::test]
async fn slash_command_suggestions_match_ts_shape() {
    let mut provider = CombinedAutocompleteProvider::new(command_fixture(), "/tmp", None);
    let result = get_suggestions(&mut provider, &["/".to_string()], 0, 1, false)
        .await
        .expect("suggestions");
    assert_eq!(result.prefix, "/");
    let items = result.items;
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].value, "model");
    assert_eq!(items[0].label, "model");
    assert_eq!(
        items[0].description.as_deref(),
        Some("<name> \u{2014} Select the active model")
    );
    assert_eq!(items[1].value, "skill:research-idea");
    assert_eq!(items[1].description.as_deref(), Some("Refine"));
    // 空描述的项**不出现在对象里**（TS 的 `...(item.description && {description})`）。
    assert_eq!(items[2].description, None);
    assert_eq!(items[3].value, "valOnly");
    assert_eq!(items[3].description, None);
}

#[tokio::test]
async fn slash_command_argument_without_completions_returns_null() {
    let mut provider = CombinedAutocompleteProvider::new(command_fixture(), "/tmp", None);
    assert!(
        get_suggestions(&mut provider, &["/model x".to_string()], 0, 8, false)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn argument_hint_only_becomes_description_when_there_is_no_description() {
    let commands = vec![CommandEntry::Slash(
        SlashCommand::new("cmd").with_argument_hint("<x>"),
    )];
    let mut provider = CombinedAutocompleteProvider::new(commands, "/tmp", None);
    let result = get_suggestions(&mut provider, &["/".to_string()], 0, 1, false)
        .await
        .expect("suggestions");
    assert_eq!(result.items[0].description.as_deref(), Some("<x>"));
}

/// `applyCompletion` 的怪异点（spec §2.10.5 实测）：无前导空白时不命中斜杠分支。
#[test]
fn apply_completion_of_slash_prefix_without_leading_space_falls_through() {
    let provider = provider("/tmp");
    let item = AutocompleteItem::new("model", "model");
    let applied = provider.apply_completion_impl(&["/m".to_string()], 0, 2, &item, "/");
    assert_eq!(applied.lines[0], "/model");
    assert_eq!(applied.cursor_col, 6);

    let applied = provider.apply_completion_impl(&[" /".to_string()], 0, 2, &item, "/");
    assert_eq!(applied.lines[0], " /model ");
    assert_eq!(applied.cursor_col, 8);
}

// ---------------------------------------------------------------------------
// 命令参数与 applyCompletion 的边界（spec §2.10.4/§2.10.5 的逐字实测）
// ---------------------------------------------------------------------------

/// 参数文本保留第二个空格后的全部内容（`"/model  a b"` → `" a b"`）。
#[tokio::test]
async fn argument_completion_receives_text_after_the_first_space() {
    let command = SlashCommand::new("model").with_argument_completions(|prefix: &str| {
        Some(vec![AutocompleteItem::new(
            format!("arg:{prefix:?}"),
            prefix,
        )])
    });
    let mut provider =
        CombinedAutocompleteProvider::new(vec![CommandEntry::Slash(command)], "/tmp", None);

    let line = "/model  a b";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(result.prefix, " a b");
    assert_eq!(result.items[0].value, "arg:\" a b\"");

    // 空参数同样会调用 `getArgumentCompletions`（`"/model "` 的 argumentText 是 ""）。
    let line = "/model ";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(result.prefix, "");
    assert_eq!(result.items[0].value, "arg:\"\"");
}

/// `argumentHint: ""` 与 `description: ""` 都是 falsy，最终 `description` 缺席。
#[tokio::test]
async fn empty_argument_hint_and_description_are_both_absent() {
    let commands = vec![CommandEntry::Slash(
        SlashCommand::new("cmd2")
            .with_argument_hint("")
            .with_description(""),
    )];
    let mut provider = CombinedAutocompleteProvider::new(commands, "/tmp", None);
    let result = get_suggestions(&mut provider, &["/cmd2".to_string()], 0, 5, false)
        .await
        .expect("suggestions");
    assert_eq!(result.items[0].description, None);
}

/// 斜杠命令的匹配大小写不敏感（`fuzzyFilter` 走 `toLowerCase`）。
#[tokio::test]
async fn slash_command_matching_is_case_insensitive() {
    let commands = vec![CommandEntry::Slash(SlashCommand::new("model"))];
    let mut provider = CombinedAutocompleteProvider::new(commands, "/tmp", None);
    let result = get_suggestions(&mut provider, &["/MODEL".to_string()], 0, 6, false)
        .await
        .expect("suggestions");
    assert_eq!(result.items[0].value, "model");
}

/// `force` 时跳过整个斜杠分支（即使前缀看起来像命令）。
#[tokio::test]
async fn force_skips_the_slash_command_branch() {
    let commands = vec![CommandEntry::Slash(SlashCommand::new("model"))];
    let mut provider = CombinedAutocompleteProvider::new(commands, "/tmp", None);
    // `"/model"` 作为路径前缀本身不含 `/`（去掉行首 `/` 后没有分隔符），也不是 `.`/`~/`
    // 开头，所以自然不触发；force 时 extractPathPrefix 直接返回 "model"。
    let result = get_suggestions(&mut provider, &["/model".to_string()], 0, 6, true).await;
    if let Some(result) = result {
        assert_eq!(result.prefix, "model");
    }
}

/// `applyCompletion` 对带引号的目录 / 文件项的引号与光标处理（spec §2.10.5）。
#[test]
fn apply_completion_quote_handling_matches_ts() {
    let provider = provider("/tmp");

    // `@` 分支：目录项不补空格；光标落在值的长度处。
    let item = AutocompleteItem::new("@\"a b/\"", "a b/");
    let applied = provider.apply_completion_impl(&["@x".to_string()], 0, 2, &item, "@x");
    assert_eq!(applied.lines[0], "@\"a b/\"");
    assert_eq!(applied.cursor_col, 6);

    // 已经处在引号内：item 以引号结尾且 label 是文件 → 补一个空格，光标在闭引号之后。
    let item = AutocompleteItem::new("@\"a b/c d\"", "c d");
    let applied = provider.apply_completion_impl(&["@\"a b/".to_string()], 0, 6, &item, "@\"a b/");
    assert_eq!(applied.lines[0], "@\"a b/c d\" ");
    assert_eq!(applied.cursor_col, 11);

    // 普通路径：`beforePrefix` 用的是 `cursorCol - prefix.length`。
    let item = AutocompleteItem::new("X", "X");
    let applied = provider.apply_completion_impl(&["foo bar".to_string()], 0, 3, &item, "o b");
    assert_eq!(applied.lines[0], "X bar");
    assert_eq!(applied.cursor_col, 1);
}

// ---------------------------------------------------------------------------
// 评审修复的回归用例（reviewer findings）
// ---------------------------------------------------------------------------

/// 用一个 `fd` 替身脚本跑通依赖 fd 的 `@` 补全（CI 里没有 fd 时唯一可行的办法）。
///
/// 替身支持 `--base-directory` / `--max-depth` / `--full-path` / 末尾正则，输出格式与 fd
/// 一致（目录带尾部 `/`）。它只在**本测试文件内部**决定是否可用：真的 fd 存在时优先用
/// 真的（`resolve_fd_path`），否则写一个临时 Python 脚本。
struct FakeFd {
    _dir: TempDir,
    path: PathBuf,
}

impl FakeFd {
    fn new() -> Option<Self> {
        let dir = TempDir::new("kiana-autocomplete-fakefd-");
        let path = dir.join("fd.py");
        let script = r#"#!/usr/bin/env python3
import os, re, sys

def main():
    args = sys.argv[1:]
    base = "."
    maxdepth = None
    full_path = False
    query = None
    i = 0
    while i < len(args):
        a = args[i]
        if a in ("--base-directory", "-c"):
            base = args[i + 1]; i += 2; continue
        if a in ("--max-depth", "-d"):
            maxdepth = int(args[i + 1]); i += 2; continue
        if a in ("--max-results", "--type", "-t", "--exclude", "-E"):
            i += 2; continue
        if a in ("--follow", "-L", "--hidden", "-H", "--full-path", "-p"):
            full_path = full_path or a in ("--full-path", "-p")
            i += 1; continue
        query = a; i += 1
    pattern = re.compile(query) if query else None
    results = []
    for root, dirs, files in os.walk(base, followlinks=True):
        rel_root = os.path.relpath(root, base)
        if rel_root == ".":
            rel_root = ""
        entries = [(d, True) for d in dirs] + [(f, False) for f in files]
        for name, is_dir in entries:
            rel = os.path.join(rel_root, name) if rel_root else name
            if rel == ".git" or rel.startswith(".git" + os.sep) or ("/.git/" in rel) or rel.startswith(".git/"):
                continue
            depth = rel.count(os.sep) + 1
            if maxdepth is not None and depth > maxdepth:
                continue
            target = rel if full_path else name
            if pattern is not None and not pattern.search(target):
                continue
            results.append(rel + os.sep if is_dir else rel)
    for r in results:
        print(r)

main()
"#;
        std::fs::write(&path, script).expect("write fake fd");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).expect("stat").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }
        // python3 是 CI 的既有依赖（测试基建）；缺失时返回 None 让用例跳过。
        let python = std::process::Command::new("python3")
            .arg("--version")
            .output();
        if python
            .map(|output| !output.status.success())
            .unwrap_or(true)
        {
            return None;
        }
        Some(Self { _dir: dir, path })
    }

    fn as_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// 评审 #1（critical）：`resolveScopedFuzzyQuery` 曾用 `str::rfind('/')` 的 **UTF-8 字节**
/// 偏移去切 UTF-16 字符串，CJK 目录名会让 scoped 基路径与 fuzzy query 都切错。
///
/// TS 行为（`@文档/说`，base 下含 `文档/说/inside1.txt`、`文档/文档/说明.txt`）：返回
/// `@文档/说/`（目录，label `说/`）与 `@文档/说明.txt`；不含 `@文档/说inside1.txt`。
#[tokio::test]
async fn scoped_fuzzy_query_uses_utf16_last_slash_index() {
    let Some(fake) = FakeFd::new() else {
        eprintln!("skipping scoped_fuzzy_query_uses_utf16_last_slash_index: python3 unavailable");
        return;
    };
    let root = TempDir::new("kiana-autocomplete-cjk-scope-");
    let base_dir = root.join("cwd");
    setup_folder(
        &base_dir,
        &[],
        &[
            ("文档/说/inside1.txt", "x"),
            ("文档/说/inside2.txt", "x"),
            ("文档/说明.txt", "x"),
            ("其他/文档/说.md", "x"),
            ("其他/文档/说明.txt", "x"),
        ],
    );
    let mut provider = provider_with_fd(&base_dir.to_string_lossy(), &fake.as_str());

    let line = "@文档/说";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(result.prefix, line);
    let items = values(&result.items);
    assert_eq!(
        items,
        vec!["@文档/说/".to_string(), "@文档/说明.txt".to_string(),],
        "CJK scoped completion must slice at the UTF-16 last slash"
    );
    assert_eq!(result.items[0].label, "说/");
    assert_eq!(result.items[0].description.as_deref(), Some("文档/说"));
    assert_eq!(result.items[1].label, "说明.txt");
    assert_eq!(
        result.items[1].description.as_deref(),
        Some("文档/说明.txt")
    );

    // 目录不存在时必须返回空（TS 的 `resolveScopedFuzzyQuery` 返回 null 后回退 basePath，
    // 但 `@a/文档/说` 的 basePath 是 cwd，`a/文档/` 不存在 → 回退到无 scope 的搜索；
    // 这里断言不会把 CJK 前缀切错成“存在”的路径）。
    let line = "@其他/文档/说";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(
        values(&result.items),
        vec![
            "@其他/文档/说.md".to_string(),
            "@其他/文档/说明.txt".to_string(),
        ]
    );
}

/// 评审 #1 的星平面（astral）变体：emoji 目录名让 `rfind('/')` 的字节偏移 = UTF-16 + 2。
#[tokio::test]
async fn scoped_fuzzy_query_survives_astral_directory_names() {
    let Some(fake) = FakeFd::new() else {
        return;
    };
    let root = TempDir::new("kiana-autocomplete-astral-scope-");
    let base_dir = root.join("cwd");
    setup_folder(&base_dir, &[], &[("😀目录/说/x.txt", "x")]);
    let mut provider = provider_with_fd(&base_dir.to_string_lossy(), &fake.as_str());

    let line = "@😀目录/说";
    let result = get_suggestions(
        &mut provider,
        &[line.to_string()],
        0,
        utf16_len(line),
        false,
    )
    .await
    .expect("suggestions");
    assert_eq!(values(&result.items), vec!["@😀目录/说/".to_string()]);
}

/// 评审 #2（major）：`localeCompare` 必须跟随 locale 数据，不能写死
/// `alternate_handling: NonIgnorable`。`th-TH` 下 Node 把 `!` 与 `"` 视为相等。
#[tokio::test]
async fn file_completion_order_follows_locale_collation() {
    let root = TempDir::new("kiana-autocomplete-collation-");
    let base_dir = root.join("cwd");
    setup_folder(
        &base_dir,
        &[],
        &[
            ("a.txt", "x"),
            ("_x.txt", "x"),
            ("-d.txt", "x"),
            (":t.txt", "x"),
        ],
    );
    let provider = provider(&base_dir.to_string_lossy());
    let suggestions = provider.get_file_suggestions("");
    let labels: Vec<&str> = suggestions.iter().map(|item| item.label.as_str()).collect();

    // 无论进程 locale 是什么，排序必须与「按同一 collator 排序」一致（而不是字节序）。
    let mut expected = labels.clone();
    expected.sort_by(|a, b| kiana_tui::autocomplete::locale_compare(a, b));
    assert_eq!(labels, expected);

    // 直接对比 `locale_compare_with`（th-TH 下标点相等是 NonIgnorable 回归的判别点）。
    use std::cmp::Ordering;
    assert_eq!(
        kiana_tui::autocomplete::locale_compare_with("th_TH.UTF-8", "!", "\""),
        Ordering::Equal
    );
    assert_eq!(
        kiana_tui::autocomplete::locale_compare_with("en_US.UTF-8", "!", "\""),
        Ordering::Less
    );
}

/// 评审 #3（major）：LC_ALL / LC_MESSAGES / LANG 里**第一个存在的**键胜出（空值也算），
/// 且值不 trim。空 LC_ALL + LANG=ja_JP → `und`（kana 不折叠）。
#[test]
fn locale_env_priority_matches_node() {
    use std::ffi::OsString;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());

    struct Restore(Vec<(&'static str, Option<OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }
    let keys = ["LC_ALL", "LC_MESSAGES", "LANG"];
    let saved: Vec<(&'static str, Option<OsString>)> = keys
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    let _restore = Restore(saved);

    let set = |lc_all: Option<&str>, lc_messages: Option<&str>, lang: Option<&str>| {
        for (key, value) in [
            ("LC_ALL", lc_all),
            ("LC_MESSAGES", lc_messages),
            ("LANG", lang),
        ] {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    };

    // 空 LC_ALL 胜出 → `und`（kana 不折叠），而不是 ja-JP（kana 折叠）。
    set(Some(""), None, Some("ja_JP.UTF-8"));
    assert_eq!(
        kiana_tui::autocomplete::resolve_process_locale(),
        "und",
        "an empty LC_ALL must win over LANG"
    );
    assert_eq!(
        kiana_tui::autocomplete::locale_compare_with("", "あ", "ア"),
        std::cmp::Ordering::Less
    );

    // 值不 trim：前导空格让语言段失效（Node：`und-JP`）。
    set(Some(" ja_JP.UTF-8 "), None, None);
    assert_eq!(kiana_tui::autocomplete::resolve_process_locale(), "und-JP");

    // @modifier 保留为变体（Node：`de-DE-x-lvariant-euro`）。
    set(Some("de_DE.UTF-8@euro"), None, None);
    assert_eq!(
        kiana_tui::autocomplete::resolve_process_locale(),
        "de-DE-x-lvariant-euro"
    );

    // 三个都未设置 → en-US。
    set(None, None, None);
    assert_eq!(kiana_tui::autocomplete::resolve_process_locale(), "en-US");
}

// ---------------------------------------------------------------------------
// 评审修复（findings：UTF-16 代理对切片 / 非法默认 locale）
// ---------------------------------------------------------------------------

/// 评审修复（minor）：`cursorCol` 落在代理对中间时，TS 的两次 slice 拼接后仍是原字符
/// （`beforePrefix + item.value + afterCursor` 在 UTF-16 码元上拼回）。
///
/// TS 实测：`applyCompletion(["{\\u0061😀<"], 0, 3, {value:"", label:"x"}, "")` 的
/// `lines[0]` 仍是 `"{a😀<"`（emoji 占码元 2、3，光标 3 在两半之间）。
#[test]
fn apply_completion_preserves_character_split_inside_surrogate_pair() {
    let provider = provider("/tmp");
    let line = "{\u{61}\u{1F600}<";
    assert_eq!(line.encode_utf16().count(), 5);

    let item = AutocompleteItem::new("", "x");
    let applied = provider.apply_completion_impl(&[line.to_string()], 0, 3, &item, "");
    assert_eq!(applied.lines[0], line);
    assert_eq!(applied.cursor_line, 0);
    assert_eq!(applied.cursor_col, 3);

    // 若拼接后代理仍然落单（这里 prefix 长度 1 让切点错开半对），Rust 的 `String` 无法
    // 表示落单代理，按 ARCHITECTURE §4.6 的约定写成 U+FFFD（TS 会保留落单代理）。
    let applied = provider.apply_completion_impl(&[line.to_string()], 0, 3, &item, "a");
    assert_eq!(applied.lines[0], "{\u{61}\u{FFFD}<");
    assert_eq!(applied.cursor_col, 2);
}

/// 评审修复（minor）：默认 locale 被 Node/ICU 拒绝（`RangeError: Internal error. Icu
/// error.`）时，TS 的 `getFileSuggestions` 在排序处抛错并被 try/catch 吞掉 → `[]`，
/// `getSuggestions` 返回 null。
///
/// `process_locale_is_valid` 是进程级缓存，必须在设置了 `LC_ALL` 的子进程里断言。
#[tokio::test]
async fn invalid_process_locale_yields_no_file_suggestions() {
    const CHILD_ENV: &str = "KIANA_TUI_AUTOCOMPLETE_INVALID_LOCALE_CHILD";

    if std::env::var_os(CHILD_ENV).is_some() {
        assert!(
            !kiana_tui::autocomplete::process_locale_is_valid(),
            "LC_ALL=en_US_abcd_abcd must be rejected like Node's Intl"
        );
        let dir = TempDir::new("kiana-autocomplete-invalid-locale-");
        setup_folder(dir.path(), &["src"], &[("src/main.rs", "")]);
        let mut provider = provider(&dir.str());
        // 有匹配的 `src/`，正常情况下 getSuggestions 返回 Some；排序抛错后应为 None。
        let result = get_suggestions(&mut provider, &["sr".to_string()], 0, 2, false).await;
        assert!(
            result.is_none(),
            "invalid default locale must yield no suggestions"
        );
        return;
    }

    let exe = std::env::current_exe().expect("current test executable");
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "invalid_process_locale_yields_no_file_suggestions",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("LC_ALL", "en_US_abcd_abcd")
        .status()
        .expect("spawn child test process");
    assert!(status.success(), "child test process failed");
}
