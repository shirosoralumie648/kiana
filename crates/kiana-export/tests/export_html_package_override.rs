//! KIANA_PACKAGE_DIR regression: export-html reads the package selected at runtime.
//! TS source: core/export-html/index.ts#generateHtml and config.ts#getExportTemplateDir.
//! Embedded assets are copied only to construct isolated test packages; the test
//! output is compared with the overridden disk assets, not a Rust-generated golden.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Env {
    previous: Option<std::ffi::OsString>,
}

impl Drop for Env {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("KIANA_PACKAGE_DIR", value) },
            None => unsafe { std::env::remove_var("KIANA_PACKAGE_DIR") },
        }
    }
}

fn package_dir(root: &Path) -> PathBuf {
    let target = root.join("src/core/export-html");
    std::fs::create_dir_all(target.join("vendor")).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets");
    for name in ["template.html", "template.css", "template.js"] {
        std::fs::copy(source.join(name), target.join(name)).unwrap();
    }
    for name in [
        "marked.min.js",
        "marked-cjk-friendly.min.js",
        "highlight.min.js",
    ] {
        std::fs::copy(
            source.join("vendor").join(name),
            target.join("vendor").join(name),
        )
        .unwrap();
    }
    root.to_path_buf()
}

#[test]
fn package_override_controls_assets_and_missing_asset_errors() {
    let _lock: MutexGuard<'static, ()> = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let package = package_dir(temp.path());
    unsafe { std::env::set_var("KIANA_PACKAGE_DIR", &package) };
    let _env = Env {
        previous: std::env::var_os("KIANA_PACKAGE_DIR"),
    };
    std::fs::write(
        package.join("src/core/export-html/template.html"),
        "<title>TS package override</title>{{CSS}}{{JS}}{{SESSION_DATA}}{{MARKED_JS}}{{MARKED_CJK_FRIENDLY_JS}}{{HIGHLIGHT_JS}}",
    )
    .unwrap();
    let data = kiana_export::SessionData {
        header: serde_json::json!({"type":"session"}),
        entries: vec![],
        leaf_id: None,
        system_prompt: None,
        tools: None,
        rendered_tools: None,
    };
    let html = kiana_export::generate_html(&data, Some("dark")).unwrap();
    assert!(html.starts_with("<title>TS package override</title>"));

    std::fs::remove_file(package.join("src/core/export-html/vendor/highlight.min.js")).unwrap();
    let error = kiana_export::generate_html(&data, Some("dark")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ENOENT: no such file or directory, open '")
    );
    assert!(error.to_string().contains("highlight.min.js"));
}
