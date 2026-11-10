//! resource-loader 集成测试的共享装配。
//!
//! 加载真正的 `.ts` 扩展需要 `kiana-extensions` 的 JS 引擎按
//! `spec/design-extension-runtime.md` §9.2 的顺序安装（与 `kiana-extensions` 自己的
//! `tests/fixtures/extensions/support.rs` 相同）。引擎是线程本地的 `Rc`，因此每个测试
//! 在自己的线程上创建并持有它；测试结束即销毁。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::rc::Rc;

use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::loader::js_host;
use kiana_extensions::virtual_modules;

/// 装配好的 JS 引擎（loader + api.js + 虚拟模块 + Node 全局）。
pub struct LoaderHarness {
    engine: Rc<JsEngine>,
}

impl LoaderHarness {
    /// 建引擎并按 §9.2 的顺序安装。
    pub fn new() -> Self {
        let engine = Rc::new(JsEngine::new().expect("create engine"));
        js_host::install_intl(&engine).expect("install intl");
        engine::install(&engine).expect("install prelude");
        js_host::install(&engine).expect("install node globals");
        virtual_modules::register_module_sources(&engine).expect("register virtual sources");
        loader::install(Rc::clone(&engine)).expect("install extension loader + api.js");
        match virtual_modules::install(&engine) {
            Ok(()) => {}
            Err(error) => {
                let caught =
                    engine.with_js(|ctx| engine::take_exception(&ctx, engine::JsErrorKind::Host));
                panic!(
                    "install virtual modules: {error:?}: {}: {}\n{}",
                    caught.name,
                    caught.message,
                    caught.stack.unwrap_or_default()
                );
            }
        }
        Self { engine }
    }
}

impl Default for LoaderHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个临时根目录，带 `agent/` 与 `project/` 两个子目录（对应 TS 测试的 `agentDir` / `cwd`）。
pub struct TestDirs {
    pub temp: tempfile::TempDir,
    pub agent_dir: PathBuf,
    pub cwd: PathBuf,
}

impl TestDirs {
    pub fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("resource-loader-test-")
            .tempdir()
            .expect("create temp dir");
        let agent_dir = temp.path().join("agent");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&agent_dir).expect("create agent dir");
        std::fs::create_dir_all(&cwd).expect("create project dir");
        Self {
            temp,
            agent_dir,
            cwd,
        }
    }

    pub fn agent(&self, relative: &str) -> PathBuf {
        self.agent_dir.join(relative)
    }

    pub fn project(&self, relative: &str) -> PathBuf {
        self.cwd.join(relative)
    }

    pub fn temp_path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }
}

impl Default for TestDirs {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 `dir` 下写文件（自动建父目录）。
pub fn write_file(dir: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(&path, contents).expect("write fixture file");
    path
}

/// 一个最小的扩展源：无行为。
pub const EMPTY_EXTENSION: &str = "export default function() {}\n";

/// 注册一个命令的扩展源。
pub fn command_extension(name: &str, description: &str) -> String {
    format!(
        "export default function(kiana) {{\n  kiana.registerCommand({name:?}, {{\n    description: {description:?},\n    handler: async () => {{}},\n  }});\n}}\n"
    )
}

/// 注册一个工具的扩展源（typebox schema）。
pub fn tool_extension(tool_name: &str) -> String {
    format!(
        r#"
import {{ Type }} from "typebox";
export default function(kiana) {{
    kiana.registerTool({{
        name: "{tool_name}",
        label: "{tool_name}",
        description: "Test tool",
        parameters: Type.Object({{}}),
        execute: async () => ({{ content: [{{ type: "text", text: "ok" }}] }}),
    }});
}}
"#
    )
}

/// 构造一个注册单个命令的 Rust 扩展工厂。
pub fn command_factory(
    name: &'static str,
    description: &'static str,
) -> kiana_ext_api::types::ExtensionFactory {
    std::sync::Arc::new(move |kiana| {
        let name = name.to_string();
        let description = description.to_string();
        Box::pin(async move {
            kiana.register_command(
                &name,
                kiana_ext_api::types::RegisterCommandOptions {
                    description: Some(description),
                    get_argument_completions: None,
                    handler: std::sync::Arc::new(|_, _| Box::pin(async {})),
                },
            );
        })
    })
}

/// 构造一个记录「被加载过」的内置扩展工厂。
pub fn recording_factory(
    name: &'static str,
    loaded: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> kiana_ext_api::types::ExtensionFactory {
    std::sync::Arc::new(move |_kiana| {
        let loaded = std::sync::Arc::clone(&loaded);
        Box::pin(async move {
            loaded.lock().expect("lock").push(name.to_string());
        })
    })
}

/// 便捷构造 [`kiana_resources::loader::DefaultResourceLoader`]。
pub fn new_loader(
    dirs: &TestDirs,
    mutate: impl FnOnce(&mut kiana_resources::loader::DefaultResourceLoaderOptions),
) -> kiana_resources::loader::DefaultResourceLoader {
    let mut options = kiana_resources::loader::DefaultResourceLoaderOptions {
        cwd: dirs.cwd.to_string_lossy().into_owned(),
        agent_dir: dirs.agent_dir.to_string_lossy().into_owned(),
        ..Default::default()
    };
    mutate(&mut options);
    kiana_resources::loader::DefaultResourceLoader::new(options)
}
