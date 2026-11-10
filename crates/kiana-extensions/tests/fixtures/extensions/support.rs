//! loader-facade 测试的共享装配（`tests/fixtures/extensions/` 下的模块文件，不单独成为测试目标）。
//!
//! 装配顺序对齐 `spec/design-extension-runtime.md` §9.2：
//! `install_intl` → `engine::install`（prelude）→ `js_host::install`（Node 全局）→
//! `loader::install`（模块 loader + `js/api.js`）→ `virtual_modules::install`。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::rc::Rc;

use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::loader::js_host;
use kiana_extensions::virtual_modules;

/// 装配好的引擎（loader + api.js + 虚拟模块 + Node 全局）。
pub struct LoaderHarness {
    pub engine: Rc<JsEngine>,
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

/// 建一个唯一的临时根目录（调用方持有 `TempDir` 以在测试结束时清理）。
pub fn temp_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("kiana-ext-test-")
        .tempdir()
        .expect("create temp dir")
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

/// 基础扩展源：注册命令 `test`。
pub const COMMAND_EXTENSION: &str = r#"
export default function(kiana) {
    kiana.registerCommand("test", { handler: async () => {} });
}
"#;

/// 注册工具的扩展源（typebox 的 `Type.Object({})`）。
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
