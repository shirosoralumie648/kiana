//! `core/source-info.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! TS: `packages/coding-agent/src/core/source-info.ts`、`core/package-manager.ts#PathMetadata`。

use kiana_core::source_info::{
    BUILTIN_PATH_PREFIX, PathMetadata, SourceInfo, SourceOrigin, SourceScope,
    SyntheticSourceInfoOptions, create_source_info, create_synthetic_source_info,
    get_synthetic_path_source, is_synthetic_path,
};
use serde_json::json;

#[test]
fn builtin_prefix_is_the_ts_literal() {
    assert_eq!(BUILTIN_PATH_PREFIX, "builtin:");
}

#[test]
fn get_synthetic_path_source_matches_the_ts_branches() {
    // builtin:<name>
    assert_eq!(
        get_synthetic_path_source("builtin:read").as_deref(),
        Some("builtin")
    );
    assert_eq!(
        get_synthetic_path_source("builtin:").as_deref(),
        Some("builtin")
    );
    // <prefix:name> with the first colon-separated segment; empty segment falls back to temporary.
    assert_eq!(
        get_synthetic_path_source("<inline:foo>").as_deref(),
        Some("inline")
    );
    assert_eq!(
        get_synthetic_path_source("<inline:1>").as_deref(),
        Some("inline")
    );
    assert_eq!(
        get_synthetic_path_source("<>").as_deref(),
        Some("temporary")
    );
    assert_eq!(
        get_synthetic_path_source("<:x>").as_deref(),
        Some("temporary")
    );
    assert_eq!(get_synthetic_path_source("<a:b:c>").as_deref(), Some("a"));
    // Not closed => undefined, even though is_synthetic_path is true.
    assert_eq!(get_synthetic_path_source("<inline"), None);
    assert_eq!(get_synthetic_path_source("builtin"), None);
    assert_eq!(get_synthetic_path_source("/tmp/file.ts"), None);
    assert_eq!(get_synthetic_path_source(""), None);
}

#[test]
fn is_synthetic_path_only_requires_the_leading_character() {
    assert!(is_synthetic_path("builtin:read"));
    assert!(is_synthetic_path("builtin:"));
    assert!(is_synthetic_path("<inline:foo>"));
    assert!(is_synthetic_path("<"));
    assert!(!is_synthetic_path(""));
    assert!(!is_synthetic_path("/tmp/<x>"));
    assert!(!is_synthetic_path("builtin"));
}

#[test]
fn create_source_info_copies_every_field() {
    let metadata = PathMetadata {
        source: "npm:foo".into(),
        scope: SourceScope::User,
        origin: SourceOrigin::Package,
        base_dir: Some("/base".into()),
        package_root: Some("/pkg".into()),
    };
    let info = create_source_info("/base/x.ts", &metadata);
    assert_eq!(
        info,
        SourceInfo {
            path: "/base/x.ts".into(),
            source: "npm:foo".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some("/base".into()),
        }
    );
}

#[test]
fn create_synthetic_source_info_defaults_scope_and_origin() {
    let info = create_synthetic_source_info(
        "builtin:read",
        SyntheticSourceInfoOptions {
            source: "builtin".into(),
            ..SyntheticSourceInfoOptions::default()
        },
    );
    assert_eq!(info.scope, SourceScope::Temporary);
    assert_eq!(info.origin, SourceOrigin::TopLevel);
    assert_eq!(info.base_dir, None);

    let overridden = create_synthetic_source_info(
        "<inline:1>",
        SyntheticSourceInfoOptions {
            source: "inline".into(),
            scope: Some(SourceScope::Project),
            origin: Some(SourceOrigin::Package),
            base_dir: Some("/b".into()),
        },
    );
    assert_eq!(overridden.scope, SourceScope::Project);
    assert_eq!(overridden.origin, SourceOrigin::Package);
    assert_eq!(overridden.base_dir.as_deref(), Some("/b"));
}

#[test]
fn source_info_serializes_camel_case_and_skips_absent_base_dir() {
    let info = SourceInfo {
        path: "builtin:read".into(),
        source: "builtin".into(),
        scope: SourceScope::Temporary,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    };
    let json = serde_json::to_value(&info).expect("serialize");
    assert_eq!(
        json,
        json!({
            "path": "builtin:read",
            "source": "builtin",
            "scope": "temporary",
            "origin": "top-level"
        })
    );
    // Key order follows the TS interface declaration order.
    assert_eq!(
        serde_json::to_string(&info).expect("serialize"),
        r#"{"path":"builtin:read","source":"builtin","scope":"temporary","origin":"top-level"}"#
    );

    let with_base = SourceInfo {
        base_dir: Some("/x".into()),
        ..info
    };
    assert_eq!(
        serde_json::to_string(&with_base).expect("serialize"),
        r#"{"path":"builtin:read","source":"builtin","scope":"temporary","origin":"top-level","baseDir":"/x"}"#
    );
}

#[test]
fn scope_and_origin_use_the_ts_literals() {
    for (scope, literal) in [
        (SourceScope::User, "user"),
        (SourceScope::Project, "project"),
        (SourceScope::Temporary, "temporary"),
    ] {
        assert_eq!(
            serde_json::to_value(scope).expect("serialize"),
            json!(literal)
        );
    }
    assert_eq!(
        serde_json::to_value(SourceOrigin::Package).expect("serialize"),
        json!("package")
    );
    assert_eq!(
        serde_json::to_value(SourceOrigin::TopLevel).expect("serialize"),
        json!("top-level")
    );
}

#[test]
fn path_metadata_serializes_package_root() {
    let metadata = PathMetadata {
        source: "local".into(),
        scope: SourceScope::Project,
        origin: SourceOrigin::TopLevel,
        base_dir: Some("/base".into()),
        package_root: Some("/root".into()),
    };
    assert_eq!(
        serde_json::to_string(&metadata).expect("serialize"),
        r#"{"source":"local","scope":"project","origin":"top-level","baseDir":"/base","packageRoot":"/root"}"#
    );
}
