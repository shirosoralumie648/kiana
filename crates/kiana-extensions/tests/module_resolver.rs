//! Module resolution tests (`spec/design-extension-runtime.md` §3.8).
//!
//! The TS reference has no dedicated resolver test: jiti's resolver is exercised indirectly by
//! `packages/coding-agent/test/extensions-discovery.test.ts` ("resolves dependencies from
//! extension's own node_modules"). These tests pin the Node/jiti order on a checked-in fixture
//! tree plus the cross-unit virtual registry contract.
#![cfg(feature = "runtime")]

use std::path::{Path, PathBuf};

use kiana_extensions::loader::resolver::{
    ModuleResolver, ResolutionKind, ResolveError, ResolverOptions, VirtualAlias, VirtualFile,
    VirtualFs, builtin_module_key, is_virtual_module_key,
};

static VIRTUAL_FILES: [VirtualFile; 3] = [
    VirtualFile {
        name: "typebox/build/index.mjs",
        source: b"export const entry = 1;",
    },
    VirtualFile {
        name: "typebox/build/_index.mjs",
        source: b"export const helpers = 1;",
    },
    VirtualFile {
        name: "typebox/build/compile/index.mjs",
        source: b"export const compile = 1;",
    },
];

static VIRTUAL_ALIASES: [VirtualAlias; 2] = [
    VirtualAlias {
        key: "typebox",
        target: "typebox/build/index.mjs",
    },
    VirtualAlias {
        key: "typebox/compile",
        target: "typebox/build/compile/index.mjs",
    },
];

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resolver/project")
}

fn entry_path() -> String {
    project()
        .join("ext/entry.ts")
        .to_string_lossy()
        .into_owned()
}

fn export_path(relative: &str) -> String {
    project().join(relative).to_string_lossy().into_owned()
}

fn resolver() -> ModuleResolver {
    ModuleResolver::new(ResolverOptions {
        extension_dir: Some(project().join("ext")),
        virtual_fs: VirtualFs::new(&VIRTUAL_FILES, &VIRTUAL_ALIASES),
    })
}

fn resolver_with_root(root: &Path) -> ModuleResolver {
    ModuleResolver::new(ResolverOptions {
        extension_dir: Some(root.to_path_buf()),
        virtual_fs: VirtualFs::empty(),
    })
}

fn resolve_import(resolver: &ModuleResolver, base: &str, specifier: &str) -> String {
    resolver
        .resolve_specifier(base, specifier, ResolutionKind::Import)
        .unwrap_or_else(|error| panic!("resolve {specifier} from {base} failed: {error}"))
}

fn resolve_require(resolver: &ModuleResolver, base: &str, specifier: &str) -> String {
    resolver
        .resolve_specifier(base, specifier, ResolutionKind::Require)
        .unwrap_or_else(|error| panic!("require {specifier} from {base} failed: {error}"))
}

#[test]
fn prefers_js_over_ts_for_extensionless_relative_imports() {
    // Measured jiti 2.7: `JITI_EXTENSIONS` is `.js,.mjs,.cjs,.ts,...`; with `local.ts` and
    // `local.js` present, jiti loads `local.js`.
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./local"),
        export_path("ext/local.js")
    );
}

#[test]
fn resolves_extensionless_js_files() {
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./only_js"),
        export_path("ext/only_js.js")
    );
}

#[test]
fn resolves_json_for_extensionless_relative_imports() {
    // jiti falls through to Node's CJS resolver, which probes `.json` for both import and require.
    let resolver = resolver();
    for kind in [ResolutionKind::Import, ResolutionKind::Require] {
        assert_eq!(
            resolver
                .resolve_specifier(&entry_path(), "./jsononly/data", kind)
                .unwrap(),
            export_path("ext/jsononly/data.json")
        );
    }
}

#[test]
fn resolves_directory_index_before_package_main() {
    // Measured jiti 2.7: a relative directory is probed for `index.*` before `package.json`
    // `main` (`./sub` → `index.js`, `./dir_main` → `index.ts` even though `main` exists).
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./sub"),
        export_path("ext/sub/index.js")
    );
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./dir_main"),
        export_path("ext/dir_main/index.ts")
    );
}

#[test]
fn relative_directories_fall_back_to_package_main() {
    // Without an index file, jiti's second pass (`nativeRequire.resolve`) uses `main`.
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./mainonly"),
        export_path("ext/mainonly/m.js")
    );
}

#[test]
fn relative_directories_ignore_the_module_field() {
    // Measured jiti 2.7: `module` is a bundler-only field; a directory that only declares it is
    // MODULE_NOT_FOUND for both `import` and `require`.
    let resolver = resolver();
    for kind in [ResolutionKind::Import, ResolutionKind::Require] {
        let error = resolver
            .resolve_specifier(&entry_path(), "./modonly", kind)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Error loading module './modonly': No such file or directory (os error 2)"
        );
    }
}

#[test]
fn substitutes_typescript_files_for_js_specifiers() {
    // jiti 2.7 (tryNative false) maps `./mod.js` → `mod.ts`, `./mod.mjs` → `mod.mts`,
    // `./mod.cjs` → `mod.cts` when the literal file is absent.
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./nodenext/mod.js"),
        export_path("ext/nodenext/mod.ts")
    );
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./nodenext/mod.mjs"),
        export_path("ext/nodenext/mod.mts")
    );
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "./nodenext/mod.cjs"),
        export_path("ext/nodenext/mod.cts")
    );
}

#[test]
fn relative_directories_do_not_use_the_exports_field() {
    // Node only applies `exports` to package-name resolution, not to relative directory imports;
    // measured with jiti 2.7 (`jiti.import("./onlyexports")` → "Cannot find module"). The design
    // §3.8 wording lists `exports` for directories, but the TS reference wins.
    let resolver = resolver();
    for kind in [ResolutionKind::Import, ResolutionKind::Require] {
        let error = resolver
            .resolve_specifier(&entry_path(), "./dir_exports", kind)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Error loading module './dir_exports': No such file or directory (os error 2)"
        );
    }
}

#[test]
fn resolves_from_the_importer_directory_upward() {
    let resolver = resolver();
    // `ext/node_modules` is closer than `project/node_modules` (design §3.6).
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "closest-pkg"),
        export_path("ext/node_modules/closest-pkg/index.js")
    );
    // no local copy: walk up to the project's node_modules.
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "main-pkg"),
        export_path("node_modules/main-pkg/lib/main.js")
    );
}

#[test]
fn honours_exports_conditions_and_subpaths() {
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "exports-pkg"),
        export_path("node_modules/exports-pkg/dist/import.mjs")
    );
    assert_eq!(
        resolve_require(&resolver, &entry_path(), "exports-pkg"),
        export_path("node_modules/exports-pkg/dist/require.cjs")
    );
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "exports-pkg/sub"),
        export_path("node_modules/exports-pkg/sub/feature.js")
    );
}

#[test]
fn ignores_the_module_field_for_both_modes() {
    // Measured jiti 2.7: package `module` is never consulted; both import and require use `main`.
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "module-pkg"),
        export_path("node_modules/module-pkg/cjs.js")
    );
    assert_eq!(
        resolve_require(&resolver, &entry_path(), "module-pkg"),
        export_path("node_modules/module-pkg/cjs.js")
    );
}

#[test]
fn resolves_package_index_and_scoped_packages() {
    let resolver = resolver();
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "index-pkg"),
        export_path("node_modules/index-pkg/index.js")
    );
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "@scope/pkg"),
        export_path("node_modules/@scope/pkg/index.js")
    );
}

#[test]
fn resolves_package_imports_field() {
    let resolver = resolver();
    let importer = project()
        .join("pkg-imports/main.js")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        resolve_require(&resolver, &importer, "#internal"),
        export_path("pkg-imports/src/internal.js")
    );
}

#[test]
fn resolves_tsconfig_paths_only_when_the_extension_dir_has_tsconfig() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resolver/tsproject");
    let with_tsconfig = resolver_with_root(&root);
    let importer = root.join("entry.ts").to_string_lossy().into_owned();
    assert_eq!(
        resolve_import(&with_tsconfig, &importer, "@lib/util"),
        root.join("src/util.ts").to_string_lossy().into_owned()
    );

    let without_tsconfig = ModuleResolver::new(ResolverOptions {
        extension_dir: None,
        virtual_fs: VirtualFs::empty(),
    });
    let error = without_tsconfig
        .resolve_specifier(&importer, "@lib/util", ResolutionKind::Import)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error loading module '@lib/util': No such file or directory (os error 2)"
    );
}

#[test]
fn virtual_keys_include_exact_subpaths_and_aliases() {
    assert!(is_virtual_module_key(
        "@earendil-works/kiana-ai/providers/all"
    ));
    assert!(is_virtual_module_key("@mariozechner/kiana-coding-agent"));
    assert!(!is_virtual_module_key("@earendil-works/kiana-ai/providers"));

    let resolver = resolver();
    let base = entry_path();
    // Alias keys resolve to the embedded file named in the table.
    assert_eq!(
        resolve_import(&resolver, &base, "typebox"),
        "typebox/build/index.mjs"
    );
    assert_eq!(
        resolve_import(&resolver, &base, "typebox/compile"),
        "typebox/build/compile/index.mjs"
    );
    // Plain virtual modules keep their key as the module name.
    assert_eq!(
        resolve_import(&resolver, &base, "@earendil-works/kiana-ai/models"),
        "@earendil-works/kiana-ai/models"
    );
    // Relative imports inside the virtual namespace stay in the table.
    assert_eq!(
        resolve_import(&resolver, "typebox/build/index.mjs", "./_index.mjs"),
        "typebox/build/_index.mjs"
    );
    // ...and fail when they point outside it.
    let error = resolver
        .resolve_specifier(
            "typebox/build/index.mjs",
            "./missing.mjs",
            ResolutionKind::Import,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error loading module './missing.mjs': No such file or directory (os error 2)"
    );
}

#[test]
fn node_builtins_map_to_shim_keys() {
    assert_eq!(builtin_module_key("path").as_deref(), Some("node:path"));
    assert_eq!(builtin_module_key("fs").as_deref(), Some("node:fs"));
    assert_eq!(
        builtin_module_key("fs/promises").as_deref(),
        Some("node:fs/promises")
    );
    assert_eq!(
        builtin_module_key("node:child_process").as_deref(),
        Some("node:child_process")
    );
    assert!(builtin_module_key("ms").is_none());

    let resolver = resolver();
    assert_eq!(resolve_import(&resolver, &entry_path(), "os"), "node:os");
    assert_eq!(
        resolve_import(&resolver, &entry_path(), "node:zlib"),
        "node:zlib"
    );
}

#[test]
fn failure_text_matches_the_rquickjs_loader_wording() {
    let resolver = resolver();
    let error = resolver
        .resolve_specifier(&entry_path(), "./missing", ResolutionKind::Import)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error loading module './missing': No such file or directory (os error 2)"
    );

    let error: ResolveError = resolver
        .resolve_specifier(&entry_path(), "not-a-real-package", ResolutionKind::Import)
        .unwrap_err();
    assert_eq!(error.specifier(), "not-a-real-package");
    assert_eq!(error.message(), "No such file or directory (os error 2)");
}

#[test]
fn resolves_file_urls_like_node() {
    // Review finding 7: jiti runs `fileURLToPath` before resolving, so `file:` URLs are paths,
    // not bare package names.
    let resolver = resolver();
    let target = export_path("ext/local.js");
    let url = format!("file://{target}");
    assert_eq!(resolve_import(&resolver, &entry_path(), &url), target);
    assert_eq!(
        resolve_import(
            &resolver,
            &entry_path(),
            &format!("file://localhost{target}")
        ),
        target
    );
    // Percent escapes are decoded (`%6c` is `l`).
    let encoded = url.replacen("/local.js", "/%6cocal.js", 1);
    assert_eq!(resolve_import(&resolver, &entry_path(), &encoded), target);
    // Extensionless file URLs use the same probe order.
    assert_eq!(
        resolve_import(
            &resolver,
            &entry_path(),
            &format!("file://{}/ext/only_js", project().display())
        ),
        export_path("ext/only_js.js")
    );
    // A `file:` URL importer resolves relative specifiers against its own directory (Node ESM).
    assert_eq!(
        resolve_import(&resolver, &format!("file://{}", entry_path()), "./local"),
        target
    );
    // A missing file URL fails with the loader's wording, not a bare-package error.
    let error = resolver
        .resolve_specifier(
            &entry_path(),
            "file:///definitely/not/here.ts",
            ResolutionKind::Import,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error loading module 'file:///definitely/not/here.ts': No such file or directory (os error 2)"
    );
    // Remote hosts and encoded separators are invalid on this platform.
    assert!(
        resolver
            .resolve_specifier(
                &entry_path(),
                "file://example.com/tmp/x.js",
                ResolutionKind::Import
            )
            .is_err()
    );
    assert!(
        resolver
            .resolve_specifier(
                &entry_path(),
                "file:///tmp/a%2Fb.js",
                ResolutionKind::Import
            )
            .is_err()
    );
}

#[test]
fn resolves_data_urls_to_themselves() {
    // Node ESM resolves `data:` URLs to the URL itself; the loader decodes the payload. jiti
    // supports them in the async `jiti.import` path.
    let resolver = resolver();
    let url = "data:text/javascript,export default 41";
    assert_eq!(resolve_import(&resolver, &entry_path(), url), url);
}

#[test]
fn cache_buster_on_the_base_is_ignored_for_resolution() {
    let resolver = resolver();
    let versioned = format!("{}?v=3-7", entry_path());
    assert_eq!(
        resolve_import(&resolver, &versioned, "./local"),
        export_path("ext/local.js")
    );
    // Relative dependencies keep their real path: only entries get a `?v=` name (design §3.4).
    assert!(!resolve_import(&resolver, &versioned, "./sub").contains('?'));
}

#[cfg(unix)]
#[test]
fn node_modules_symlinks_resolve_to_the_real_path() {
    use std::os::unix::fs::symlink;

    let temp = std::env::temp_dir().join(format!("kiana-resolver-symlink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    let real = temp.join("real-pkg");
    std::fs::create_dir_all(&real).expect("create real package");
    std::fs::write(
        real.join("package.json"),
        "{\"name\":\"linked-pkg\",\"main\":\"./main.js\"}",
    )
    .expect("write manifest");
    std::fs::write(real.join("main.js"), "module.exports = 1;").expect("write main");
    let app = temp.join("app");
    std::fs::create_dir_all(app.join("node_modules")).expect("create node_modules");
    symlink(&real, app.join("node_modules/linked-pkg")).expect("create symlink");

    let resolver = resolver_with_root(&app);
    let base = app.join("entry.ts").to_string_lossy().into_owned();
    let resolved = resolve_import(&resolver, &base, "linked-pkg");
    assert_eq!(resolved, real.join("main.js").to_string_lossy());

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn quickjs_surfaces_the_resolve_error_as_a_loading_error() {
    use rquickjs::loader::{ImportAttributes, Loader};
    use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Module, Runtime};

    struct NopLoader;
    impl Loader for NopLoader {
        fn load<'js>(
            &mut self,
            _ctx: &Ctx<'js>,
            name: &str,
            _attributes: Option<ImportAttributes<'js>>,
        ) -> rquickjs::Result<Module<'js>> {
            Err(rquickjs::Error::new_loading_message(name, "unreachable"))
        }
    }

    let rt = Runtime::new().expect("runtime");
    rt.set_loader(resolver(), NopLoader);
    let ctx = Context::full(&rt).expect("context");
    let message = ctx.with(|ctx| {
        let result: Result<_, CaughtError> = Module::evaluate(
            ctx.clone(),
            "entry",
            "import value from 'not-a-real-package'; export default value;",
        )
        .catch(&ctx);
        match result.expect_err("import must fail") {
            CaughtError::Exception(exception) => exception.message().expect("exception message"),
            other => panic!("expected a JS exception, got {other:?}"),
        }
    });
    // TS: jiti/Node 的 `Cannot find module '<spec>'` + Require stack（调用方是加载器）。
    assert_eq!(
        message,
        format!(
            "Cannot find module 'not-a-real-package'\nRequire stack:\n- {}",
            kiana_extensions::loader::loader_impl::loader_require_stack_path()
        )
    );
}

#[test]
fn resolver_implements_the_rquickjs_resolver_trait() {
    fn assert_impl<T: rquickjs::loader::Resolver>() {}
    assert_impl::<ModuleResolver>();
}

#[test]
fn empty_base_falls_back_to_the_extension_directory() {
    let resolver = resolver();
    // `<inline>` style module names are not file paths; bare packages still resolve from the
    // extension directory (loader.ts resolves extension entries relative to cwd anyway).
    assert_eq!(
        resolve_import(&resolver, "<inline>", "index-pkg"),
        export_path("node_modules/index-pkg/index.js")
    );
}
