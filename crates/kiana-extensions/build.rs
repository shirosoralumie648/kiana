//! Generates the embedded virtual-file table (design §3.6).
//!
//! TypeBox 1.3.27 is vendored under `vendor/typebox/` (only `build/**/*.mjs`; the `.d.mts`
//! declarations are not shipped). This script walks that directory, sorts the paths for a stable
//! table, and emits `static VIRTUAL_FILES: &[VirtualFile]` with `include_bytes!` entries — no
//! `rust-embed` dependency (design §3.6 asks for the same table shape).
//!
//! The generated table also carries the `js/virtual/*.js` module sources that back the kiana-*
//! namespaces, so a single [`VirtualFs`] serves both the embedded TypeBox package and the virtual
//! module shims. Module names are the QuickJS module names and must stay byte-identical to what
//! the resolver hands to the loader:
//!
//! * TypeBox files: `typebox/` + path below `vendor/typebox/` (`typebox/build/index.mjs`, …).
//! * Virtual shims: `kiana-virtual/<name>.js` (the canonical name each public key aliases to).
//!
//! [`VirtualFs`]: the type is defined by the loader-compile unit; this file only names it.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Vendored package root, relative to the crate manifest directory.
const VENDOR_DIR: &str = "vendor/typebox";
/// Public module namespace of the vendored package.
const PACKAGE_PREFIX: &str = "typebox";

/// `(public key, canonical module name)` pairs for the 22 virtual modules (design §3.3).
///
/// Several keys intentionally share one canonical name: the TypeScript map points them at the same
/// module object, and a single QuickJS module name is what preserves that identity in Rust.
const VIRTUAL_ALIASES: &[(&str, &str)] = &[
    ("typebox", "typebox/build/index.mjs"),
    ("typebox/compile", "typebox/build/compile/index.mjs"),
    ("typebox/value", "typebox/build/value/index.mjs"),
    ("@sinclair/typebox", "typebox/build/index.mjs"),
    (
        "@sinclair/typebox/compile",
        "typebox/build/compile/index.mjs",
    ),
    ("@sinclair/typebox/value", "typebox/build/value/index.mjs"),
    (
        "@earendil-works/kiana-agent-core",
        "kiana-virtual/kiana-agent-core.js",
    ),
    ("@earendil-works/kiana-tui", "kiana-virtual/kiana-tui.js"),
    ("@earendil-works/kiana-ai", "kiana-virtual/kiana-ai.js"),
    (
        "@earendil-works/kiana-ai/compat",
        "kiana-virtual/kiana-ai.js",
    ),
    (
        "@earendil-works/kiana-ai/models",
        "kiana-virtual/kiana-ai/models.js",
    ),
    (
        "@earendil-works/kiana-ai/oauth",
        "kiana-virtual/kiana-ai/oauth.js",
    ),
    (
        "@earendil-works/kiana-ai/providers/all",
        "kiana-virtual/kiana-ai/providers-all.js",
    ),
    (
        "@earendil-works/kiana-coding-agent",
        "kiana-virtual/kiana-coding-agent.js",
    ),
    (
        "@mariozechner/kiana-agent-core",
        "kiana-virtual/kiana-agent-core.js",
    ),
    ("@mariozechner/kiana-tui", "kiana-virtual/kiana-tui.js"),
    ("@mariozechner/kiana-ai", "kiana-virtual/kiana-ai.js"),
    ("@mariozechner/kiana-ai/compat", "kiana-virtual/kiana-ai.js"),
    (
        "@mariozechner/kiana-ai/models",
        "kiana-virtual/kiana-ai/models.js",
    ),
    (
        "@mariozechner/kiana-ai/oauth",
        "kiana-virtual/kiana-ai/oauth.js",
    ),
    (
        "@mariozechner/kiana-ai/providers/all",
        "kiana-virtual/kiana-ai/providers-all.js",
    ),
    (
        "@mariozechner/kiana-coding-agent",
        "kiana-virtual/kiana-coding-agent.js",
    ),
];

/// Virtual shim files served from `js/virtual/` (module name, path relative to the crate).
///
/// The `kiana-virtual/helpers/*.js` modules are installation-internal (they build the namespace
/// seeds), but they must live in this table too: `virtual_modules::install` imports them through
/// the runtime's `Resolver`/`Loader`, exactly like any other module, and QuickJS normalizes every
/// import specifier through the resolver before consulting the module cache (review fix: without
/// these entries the real `ModuleResolver` rejects the helper names and `install()` fails).
const SHIM_FILES: &[(&str, &str)] = &[
    (
        "kiana-virtual/kiana-agent-core.js",
        "js/virtual/kiana-agent-core.js",
    ),
    ("kiana-virtual/kiana-tui.js", "js/virtual/kiana-tui.js"),
    ("kiana-virtual/kiana-ai.js", "js/virtual/kiana-ai.js"),
    (
        "kiana-virtual/kiana-ai/models.js",
        "js/virtual/kiana-ai/models.js",
    ),
    (
        "kiana-virtual/kiana-ai/oauth.js",
        "js/virtual/kiana-ai/oauth.js",
    ),
    (
        "kiana-virtual/kiana-ai/providers-all.js",
        "js/virtual/kiana-ai/providers-all.js",
    ),
    (
        "kiana-virtual/kiana-coding-agent.js",
        "js/virtual/kiana-coding-agent.js",
    ),
    // Installation-internal namespace builders (not public virtual keys).
    ("kiana-virtual/helpers/bridge.js", "js/virtual/bridge.js"),
    (
        "kiana-virtual/helpers/kiana-ai.js",
        "js/virtual/kiana-ai-helpers.js",
    ),
    (
        "kiana-virtual/helpers/kiana-tui.js",
        "js/virtual/kiana-tui-helpers.js",
    ),
    (
        "kiana-virtual/helpers/kiana-coding-agent.js",
        "js/virtual/kiana-coding-agent-helpers.js",
    ),
];

fn main() {
    println!("cargo:rerun-if-changed={VENDOR_DIR}");
    println!("cargo:rerun-if-changed=js/virtual");
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let vendor_root = manifest_dir.join(VENDOR_DIR);

    let mut typebox_files = Vec::new();
    walk_mjs(&vendor_root, &vendor_root, &mut typebox_files);
    typebox_files.sort();

    let mut out = String::new();
    out.push_str("// @generated by build.rs — do not edit.\n");
    out.push_str("// Vendored TypeBox 1.3.27 + js/virtual shims (design-extension-runtime.md §3.3/§3.6).\n\n");
    out.push_str("pub static VIRTUAL_FILES: &[VirtualFile] = &[\n");
    for relative in &typebox_files {
        let module_name = format!("{PACKAGE_PREFIX}/{}", relative.replace('\\', "/"));
        let abs = vendor_root.join(relative);
        let _ = writeln!(
            out,
            "    VirtualFile {{ name: {module_name:?}, source: include_bytes!({:?}) }},",
            abs.to_string_lossy()
        );
        println!("cargo:rerun-if-changed={}", abs.display());
    }
    for (module_name, source_path) in SHIM_FILES {
        let abs = manifest_dir.join(source_path);
        let _ = writeln!(
            out,
            "    VirtualFile {{ name: {module_name:?}, source: include_bytes!({:?}) }},",
            abs.to_string_lossy()
        );
        println!("cargo:rerun-if-changed={}", abs.display());
    }
    out.push_str("];\n\n");

    out.push_str("pub static VIRTUAL_ALIASES: &[VirtualAlias] = &[\n");
    for (key, target) in VIRTUAL_ALIASES {
        let _ = writeln!(
            out,
            "    VirtualAlias {{ key: {key:?}, target: {target:?} }},"
        );
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"))
        .join("virtual_files.rs");
    fs::write(&dest, out).expect("write generated virtual file table");
}

/// Collect every `*.mjs` below `dir` as a path relative to `root`.
fn walk_mjs(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", dir.display()),
    };
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file type");
        if file_type.is_dir() {
            walk_mjs(&path, root, out);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "mjs") {
            let relative = path
                .strip_prefix(root)
                .expect("walked path is below the vendor root")
                .to_string_lossy()
                .into_owned();
            out.push(relative);
        }
    }
}

// force rebuild marker
