//! 对应 ca-extensions 的运行时部分：加载器、ExtensionRunner、事件派发、虚拟模块、JS 引擎。
//!
//! TS 源：packages/coding-agent/src/core/extensions/{index,loader,runner,wrapper,virtual-modules,jiti-loader,jiti-static-loader}.ts
//!
//! 运行时形态见 `spec/design-extension-runtime.md`：rquickjs 0.14.0 + oxc 0.155 + 单线程所有权。
//! 公开类型契约在 `kiana-ext-api`（`core/extensions/types.ts`），本 crate 不重复定义。
//! `core/extensions/index.ts` 的再导出面就是 [`index`]（crate 根的 `pub use index::*`）。
//!
//! # 与 `ARCHITECTURE.md` §5 映射的两处说明
//!
//! - `jiti-loader.ts` / `jiti-static-loader.ts` 在 §5 的文件树里没有独立的 `.rs` 文件：它们
//!   描述的是模块加载策略（原生 ESM、CJS、虚拟模块、TS 去类型），已折进
//!   [`loader`]（`loader/mod.rs` 的策略选择、缓存与事务）与 `loader::resolver`
//!   （Node/jiti 解析顺序）。这是策略落点，不是遗漏。
//! - `packages/coding-agent/src/extensions/**` 是四个**内置扩展**，归
//!   `kiana-builtin-extensions`（阶段 6）；本 crate 的 TS 源是
//!   `packages/coding-agent/src/core/extensions/*.ts`。任务简报里的目录提示不准确，
//!   `ARCHITECTURE.md` §5 的映射才是权威。
//!
//! # Feature 门控（`ARCHITECTURE.md` §8 第 4、27 条）
//!
//! `runtime` 是唯一会把 rquickjs/oxc 拉进来的特性；没有它时本 crate 退化成
//! `kiana-ext-api` 的再导出面（[`index`]），`cargo build -p kiana-extensions
//! --no-default-features` 必须保持绿色。`locale-data` 只影响 `loader::js_host` 的 Intl 实现，
//! 同样在运行时门控之内。`webassembly`（wasmtime）保持可选：启用后 `loader::js_host` 安装真实的
//! `WebAssembly.Module`/`Instance`/`instantiate`（见 `loader/wasm.rs` 的覆盖范围与限制）。
//!
//! # 装配（本 crate 唯一知道 §9.2 顺序的地方）
//!
//! [`ExtensionHost::new`] / [`Generation::new`] 是装配方（当前为 `kiana-coding-agent` 的
//! 进程入口）唯一需要调用的入口：调用方**不需要**知道各 shim/虚拟模块/API 的安装顺序。
//! 顺序见 [`Generation::with_config`]。
//!
//! 所有虚拟模块与 shim 的源码在第一个扩展被求值之前声明（TDZ）：装配只做声明与全局安装，
//! 扩展的求值发生在调用方随后调用的 `loader::load_extensions*`。
//!
//! 运行期模块（loader/runner/ui/virtual_modules/engine/project_trust/wrapper）全部
//! `#[cfg(feature = "runtime")]`；
//! [`index`] 无条件编译，因此 `--no-default-features` 下公开面仍然完整。
//!
//! # 这个 crate 解决什么问题
//!
//! `kiana-ext-api` 是扩展 API 的冻结契约（类型、`define_tool` 与 9 个 `is*` 守卫），
//! 本身不能执行任何东西。本 crate 提供运行期：用 rquickjs 建 JS 引擎、用 oxc 把扩展的
//! TypeScript 去类型、按 Node/jiti 的规则解析模块、注入 22 个虚拟模块键与 `node:*` shim、
//! 把扩展注册的工具/命令/handler 变成 Rust 回调，并派发 13 个 emitter 的事件。没有它，
//! 扩展只是一组登记不进去的数据，`kiana-session` 也没有可调用的 `ExtensionRunner`。
//!
//! # 在系统中的位置
//!
//! 上游（本 crate 依赖）：`kiana-ext-api`（公开类型契约）、`kiana-core`（主题、语法高亮、
//! slash command 与 source info）、`kiana-theme`、`kiana-tui`、`kiana-tools`。
//!
//! 下游（调用方）：
//!
//! - `kiana-coding-agent` 的进程入口在单线程 `LocalSet` 上创建 [`ExtensionHost`]
//!   （`src/main.rs`、`src/bin/kiana-rpc.rs`），并让它在整个进程期间存活；
//! - `kiana-resources` 的 `DefaultResourceLoader::reload` 调用
//!   [`loader::load_extensions_cached`]；
//! - `kiana-session` 构造 [`runner::ExtensionRunner`]、调用 `bind_core` / `set_ui_context`
//!   并派发事件；装配引擎不在它的职责内。
//!
//! # 核心概念与类型
//!
//! - [`engine::JsEngine`]：一个线程上的 QuickJS runtime + context。
//!   [`engine::EngineConfig`] 配置内存上限、栈上限、单次 `with_js` 的看门狗 deadline 与
//!   代际号（`generation`）。
//! - [`loader`]：扩展发现、TS 去类型、模块解析、`createExtensionAPI` 的事务
//!   （loading / active / failed）与工厂缓存。
//! - [`virtual_modules`]：`globalThis.__kiana_virtual` 的 22 个键与 typebox。
//! - [`runner`]：13 个 emitter 的 [`runner::ExtensionRunner`] 与 `ctx` 的 JS 物化。
//! - [`project_trust`]：会话创建之前的项目信任评估（自由函数，不需要 runner）。
//! - [`wrapper`]：把扩展注册的 `RegisteredTool` 适配成 agent loop 使用的 `AgentTool`。
//! - [`ui`]：原生 TUI 组件、`ctx.ui` 的异步桥与 `@earendil-works/kiana-tui` 的运行期面。
//! - [`Generation`]：全部层安装完成后的一代引擎；[`ExtensionHost`]：持有当前代、负责
//!   代际重载的宿主。
//!
//! # 一次典型调用
//!
//! 1. 进程入口创建 [`ExtensionHost::new`]（或直接用 [`Generation::new`]）：建引擎并按
//!    §9.2 的顺序安装全部层（本文件末尾的 `assembly`）；
//! 2. `kiana-resources` 调 [`loader::load_extensions_cached`]：扩展模块在引擎里求值，
//!    通过 `kiana.*` 注册工具、命令与 handler；
//! 3. `kiana-session` 用 [`runner::ExtensionRunner`] 构造 runner 并接到会话动作；事件
//!    派发与 UI 桥调用都发生在创建引擎的那条线程上；
//! 4. 需要整代替换时调用 [`ExtensionHost::reload`]：销毁旧代、装配新代
//!    （`EngineConfig::generation` +1），旧 `kiana`/`ctx` 句柄由上层 `invalidate` 后失效。
//!
//! # 边界与失败模式
//!
//! - **装配失败**：任一步 `install` 出错时 `Generation::with_config` 先撤销线程本地登记
//!   （`teardown`）再返回 `Err`，不会把半装的引擎留给线程
//!   （`tests/public_surface.rs` 的 `a_failed_generation_leaves_no_engine_or_loader_bound`）。
//! - **线程上没有引擎**：调用 `loader::load_extensions*` 时每个路径得到一条
//!   `Extension runtime engine is not installed` 的加载错误，而不是 panic（`loader/mod.rs`
//!   按路径构造 `LoadError` 并继续处理其余路径）。`kiana-coding-agent` 的入口会丢弃
//!   `ExtensionHost::new()` 的错误并继续启动，因此装配失败不会中止进程，而是转成逐路径
//!   的加载错误。
//! - **线程约束**：[`engine::JsEngine`] 与 [`Generation`] 都是 `!Send`，只能在创建线程上
//!   使用；[`runner`] 的线程本地扩展表在别的线程上访问会 panic。
//! - **重载失败**：`ExtensionHost::reload` 先销毁旧代再装配新代，低层错误发生时没有旧代
//!   可以回滚；调用方应把 `Err` 当作致命错误（[`ExtensionHost::generation`] 的 panic
//!   文案即为此状态）。
//!
//! # 阅读顺序
//!
//! [`index`]（对外再导出面）→ [`engine`]（引擎、限制与关闭顺序）→ [`loader`]（加载与
//! API 事务）→ [`virtual_modules`] → [`runner`] → [`ui`] → 本文件末尾的 `assembly`
//! （装配顺序与代际生命周期）。

pub mod index;

#[cfg(feature = "runtime")]
pub mod engine;
#[cfg(feature = "runtime")]
pub mod loader;
#[cfg(feature = "runtime")]
pub mod project_trust;
#[cfg(feature = "runtime")]
pub mod runner;
#[cfg(feature = "runtime")]
pub mod ui;
#[cfg(feature = "runtime")]
pub mod virtual_modules;
#[cfg(feature = "runtime")]
pub mod wrapper;

/// 把 [`index`] 的再导出面提到 crate 根：`kiana_extensions::X` 与
/// `kiana_extensions::index::X` 是同一项（`tests/public_surface.rs` 对两个路径做同类型断言）。
///
/// `core/extensions/index.ts` 是纯再导出；crate 根与 `kiana_extensions::index` 两个路径都可用。
pub use index::*;

#[cfg(feature = "runtime")]
mod assembly {
    //! 装配器：本 crate 公开的两个装配类型 `Generation` / `ExtensionHost` 的实现。
    //!
    //! 为什么是私有的内联模块：设计文档 §9.2 的安装顺序必须只有一处维护，调用方只应拿到
    //! “已经装好的一代”，不能自行拼装。模块私有、两个类型在文件末尾 `pub use` 到 crate
    //! 根，既隐藏全部 `install` 细节，又保留 `kiana_extensions::Generation` 这个路径。
    //!
    //! 除构造与重载外，本模块还放三类只服务于装配期的实现：
    //! `@earendil-works/kiana-tui` 的三个 coding-agent helper（`KIANA_TUI_COMPAT_JS` 与
    //! `key_hint`）、`highlightCode` 的文本实现（`highlight_code_text` /
    //! `highlight_theme_for`），以及整代销毁路径（`GenerationInner::drop` → `teardown`）。

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;

    use rquickjs::function::Opt;
    use rquickjs::{Array, Ctx, Function, Object, Value};

    use crate::engine::{EngineConfig, EngineResult, JsEngine};
    use crate::{engine, loader, runner, ui, virtual_modules};

    /// `@earendil-works/kiana-tui` 的虚拟 namespace 额外共享的、coding-agent 的三个主题/提示
    /// helper（`packages/coding-agent/src/index.ts:479-481` 导出；`virtual_modules.rs` 在
    /// kiana-tui 桥里把它们标记为 `shared(…, "kiana-tui")`）。
    ///
    /// ui-adapter 的 `__kiana_tui` 尚未提供它们；没有这三个成员时 `summarize.ts`（getMarkdownTheme）、
    /// `tools.ts`（getSettingsListTheme）与 `debug-provider.ts`（keyHint）在运行期抛
    /// `kiana: native bridge member "kiana-tui.<name>" is not installed`。这里按 TS 的实现
    /// 用实时主题（`Symbol.for("@earendil-works/kiana-coding-agent:theme")`）构造；`keyHint` 的
    /// 键位查询由 Rust 侧 `kiana_tui::keybindings::get_keybindings()` 提供（见 [`key_hint`]）。
    /// `highlightCode` 走宿主函数 [`highlight_code_lines`]（TS 的 `cli-highlight` 分词在
    /// Rust 侧的逐字移植是 `kiana-theme` 的 `highlight_code`）。
    const KIANA_TUI_COMPAT_JS: &str = r#"
        (function (highlightCode) {
            const theme = () =>
                globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")];
            return {
                getMarkdownTheme() {
                    const t = theme();
                    const fg = (token, text) => t.fg(token, text);
                    return {
                        heading: (text) => fg("mdHeading", text),
                        link: (text) => fg("mdLink", text),
                        linkUrl: (text) => fg("mdLinkUrl", text),
                        code: (text) => fg("mdCode", text),
                        codeBlock: (text) => fg("mdCodeBlock", text),
                        codeBlockBorder: (text) => fg("mdCodeBlockBorder", text),
                        quote: (text) => fg("mdQuote", text),
                        quoteBorder: (text) => fg("mdQuoteBorder", text),
                        hr: (text) => fg("mdHr", text),
                        listBullet: (text) => fg("mdListBullet", text),
                        bold: (text) => t.bold(text),
                        italic: (text) => t.italic(text),
                        underline: (text) => t.underline(text),
                        strikethrough: (text) => t.strikethrough(text),
                        highlightCode: highlightCode,
                    };
                },
                getSettingsListTheme() {
                    const t = theme();
                    return {
                        label: (text, selected) => selected ? t.fg("accent", text) : text,
                        value: (text, selected) =>
                            selected ? t.fg("accent", text) : t.fg("muted", text),
                        description: (text) => t.fg("dim", text),
                        cursor: t.fg("accent", "→ "),
                        hint: (text) => t.fg("dim", text),
                    };
                },
            };
        })
    "#;

    thread_local! {
        /// `getCliHighlightTheme` 的缓存：按 `Theme` 实例身份（TS 相同）。
        static CLI_HIGHLIGHT_THEME: RefCell<
            Option<(Arc<kiana_theme::Theme>, kiana_core::syntax_highlight::HighlightTheme)>,
        > = const { RefCell::new(None) };
    }

    /// `getMarkdownTheme().highlightCode(code, lang?)`（`theme.ts:1038-1057`）。
    ///
    /// TS 用 `cli-highlight`（highlight.js）按语言着色；Rust 侧同一实现是
    /// `kiana_core::syntax_highlight`（`utils/syntax-highlight.ts` 的逐字移植）。主题取
    /// [`ui::current_global_theme`]——与 `getMarkdownTheme` 其余成员读的是同一个实例，
    /// 因此全局主题尚未 `initTheme` 时也一致（回退内置 `dark`）。语言缺失 / 无效时**逐行**
    /// 套 `mdCodeBlock`（TS 的 `validLang` 分支）。
    fn highlight_code_lines<'js>(
        ctx: Ctx<'js>,
        code: String,
        lang: Opt<String>,
    ) -> rquickjs::Result<Array<'js>> {
        let lines = highlight_code_text(&code, lang.0.as_deref());
        let array = Array::new(ctx.clone())?;
        for (index, line) in lines.iter().enumerate() {
            array.set(index, rquickjs::String::from_str(ctx.clone(), line)?)?;
        }
        Ok(array)
    }

    /// `highlightCode` 的文本实现（数组构造只做 JS 物化）。
    fn highlight_code_text(code: &str, lang: Option<&str>) -> Vec<String> {
        let theme = ui::current_global_theme();
        let valid_lang = lang
            .filter(|language| !language.is_empty())
            .filter(|language| kiana_core::syntax_highlight::supports_language(language));
        let Some(language) = valid_lang else {
            // `split('\n')` 与 TS 的 `split("\n")` 一样保留尾部空行（`"a\n"` →
            // `["a", ""]`）；`public_surface.rs` 的 python golden 用例断言的输出里就有
            // 末尾空行。
            return code
                .split('\n')
                .map(|line| theme.fg(kiana_theme::ThemeColor::MdCodeBlock, line))
                .collect();
        };
        // 显式语言 + `ignore_illegals: true`：与 `kiana-theme` 的 `highlight_code` 采用
        // 同一调用形式（`kiana-core::syntax_highlight` 的文件文档列出的两个调用方），
        // 不走 `highlight_auto` 自动检测。
        let options = kiana_core::syntax_highlight::HighlightOptions {
            language: Some(language.to_owned()),
            ignore_illegals: true,
            language_subset: None,
            theme: highlight_theme_for(&theme),
        };
        kiana_core::syntax_highlight::highlight(code, &options)
            .split('\n')
            .map(str::to_owned)
            .collect()
    }

    /// `buildCliHighlightTheme(t)`：highlight.js scope → 主题 token / 文本样式。
    fn build_highlight_theme(
        theme: &Arc<kiana_theme::Theme>,
    ) -> kiana_core::syntax_highlight::HighlightTheme {
        // 键是 highlight.js 的 scope 名（不含 `hljs-` 前缀）；kiana-core 的查找规则是
        // 精确匹配 → `前缀.` → `前缀-` 回退，整条 span 栈都未命中时使用 `default` 键。
        // 本表不插 `default`，因此未覆盖的 scope 原样输出。
        use kiana_core::syntax_highlight::{HighlightFormatter, HighlightTheme};

        // 两个小工厂的区别：`color` 按主题 token 给文本着色（前景色），`style` 调用
        // `Theme` 的文本样式方法（bold/italic/underline），后者不改变前景色。两者都把
        // `Arc<Theme>` 克隆进 formatter，因此生成的 `HighlightTheme` 与主题实例同寿命；
        // 主题实例被替换后必须重建（由 `highlight_theme_for` 的指针缓存负责）。
        fn color(
            map: &mut HighlightTheme,
            theme: &Arc<kiana_theme::Theme>,
            scope: &str,
            token: kiana_theme::ThemeColor,
        ) {
            let theme = Arc::clone(theme);
            let formatter: HighlightFormatter = Arc::new(move |text: &str| theme.fg(token, text));
            map.insert(scope.to_owned(), formatter);
        }

        fn style(
            map: &mut HighlightTheme,
            theme: &Arc<kiana_theme::Theme>,
            scope: &str,
            apply: fn(&kiana_theme::Theme, &str) -> String,
        ) {
            let theme = Arc::clone(theme);
            let formatter: HighlightFormatter = Arc::new(move |text: &str| apply(&theme, text));
            map.insert(scope.to_owned(), formatter);
        }

        let mut map = HighlightTheme::new();
        color(
            &mut map,
            theme,
            "keyword",
            kiana_theme::ThemeColor::SyntaxKeyword,
        );
        color(
            &mut map,
            theme,
            "built_in",
            kiana_theme::ThemeColor::SyntaxType,
        );
        color(
            &mut map,
            theme,
            "literal",
            kiana_theme::ThemeColor::SyntaxNumber,
        );
        color(
            &mut map,
            theme,
            "number",
            kiana_theme::ThemeColor::SyntaxNumber,
        );
        color(
            &mut map,
            theme,
            "regexp",
            kiana_theme::ThemeColor::SyntaxString,
        );
        color(
            &mut map,
            theme,
            "string",
            kiana_theme::ThemeColor::SyntaxString,
        );
        color(&mut map, theme, "subst", kiana_theme::ThemeColor::Text);
        color(
            &mut map,
            theme,
            "comment",
            kiana_theme::ThemeColor::SyntaxComment,
        );
        color(
            &mut map,
            theme,
            "doctag",
            kiana_theme::ThemeColor::SyntaxComment,
        );
        color(&mut map, theme, "meta", kiana_theme::ThemeColor::Muted);
        color(
            &mut map,
            theme,
            "function",
            kiana_theme::ThemeColor::SyntaxFunction,
        );
        color(
            &mut map,
            theme,
            "title",
            kiana_theme::ThemeColor::SyntaxFunction,
        );
        color(
            &mut map,
            theme,
            "class",
            kiana_theme::ThemeColor::SyntaxType,
        );
        color(&mut map, theme, "type", kiana_theme::ThemeColor::SyntaxType);
        color(
            &mut map,
            theme,
            "tag",
            kiana_theme::ThemeColor::SyntaxPunctuation,
        );
        color(
            &mut map,
            theme,
            "name",
            kiana_theme::ThemeColor::SyntaxKeyword,
        );
        color(
            &mut map,
            theme,
            "attr",
            kiana_theme::ThemeColor::SyntaxVariable,
        );
        color(
            &mut map,
            theme,
            "variable",
            kiana_theme::ThemeColor::SyntaxVariable,
        );
        color(
            &mut map,
            theme,
            "params",
            kiana_theme::ThemeColor::SyntaxVariable,
        );
        color(
            &mut map,
            theme,
            "operator",
            kiana_theme::ThemeColor::SyntaxOperator,
        );
        color(
            &mut map,
            theme,
            "punctuation",
            kiana_theme::ThemeColor::SyntaxPunctuation,
        );
        style(&mut map, theme, "emphasis", kiana_theme::Theme::italic);
        style(&mut map, theme, "strong", kiana_theme::Theme::bold);
        style(&mut map, theme, "link", kiana_theme::Theme::underline);
        color(
            &mut map,
            theme,
            "addition",
            kiana_theme::ThemeColor::ToolDiffAdded,
        );
        color(
            &mut map,
            theme,
            "deletion",
            kiana_theme::ThemeColor::ToolDiffRemoved,
        );
        map
    }

    /// `getCliHighlightTheme(t)`：按 `Theme` 实例身份缓存。
    fn highlight_theme_for(
        theme: &Arc<kiana_theme::Theme>,
    ) -> kiana_core::syntax_highlight::HighlightTheme {
        // 缓存键是 `Arc` 指针身份：`initTheme` 会重设全局主题（已注册主题按
        // `kiana_theme::theme::load_theme` 复用同一个 `Arc`），`setThemeInstance` 直接
        // 换入调用方实例；指针变了才重建 formatter，同一实例重复调用只克隆 `HashMap`。
        CLI_HIGHLIGHT_THEME.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((cached, highlight_theme)) = cache.as_ref()
                && Arc::ptr_eq(cached, theme)
            {
                return highlight_theme.clone();
            }
            let built = build_highlight_theme(theme);
            *cache = Some((Arc::clone(theme), built.clone()));
            built
        })
    }

    /// `keybinding-hints.ts#keyHint`：把 keybinding id 解析成按键串，再拼上描述。
    ///
    /// `keybinding` 是扩展传入的动作 id（如 `"tui.select.confirm"`），查询走
    /// `kiana_tui::keybindings::get_keybindings()`；同一动作有多个和弦时先以 `/` 连接、
    /// 再由 [`format_key_text`] 拆开重排。未知 id 的 `get_keys` 返回空表，此时输出只有
    /// 描述前的空格（不是错误）。
    fn key_hint(keybinding: String, description: String) -> String {
        // `keybinding-hints.ts#keyText` + `#keyHint`。
        let keys = kiana_tui::keybindings::get_keybindings().get_keys(&keybinding);
        let key_text = format_key_text(&keys.join("/"), false);
        let theme = crate::ui::current_global_theme();
        // 主题缺少 `dim` / `muted` 的 ANSI 映射时 `fg_named` 返回 `Err`，退回未着色文本，
        // 因此 hint 在任何主题下都仍包含键位与描述。
        let left = theme
            .fg_named("dim", &key_text)
            .unwrap_or_else(|_| key_text.clone());
        let hint = format!(" {description}");
        let right = theme
            .fg_named("muted", &hint)
            .unwrap_or_else(|_| hint.clone());
        format!("{left}{right}")
    }

    /// `keybinding-hints.ts#formatKeyText`：把 `ctrl+shift+k` 形态的键位串转成显示文本。
    ///
    /// 有两个分隔层级：`/` 分隔同一动作的多个和弦，`+` 分隔单个和弦内的按键，输出按同一
    /// 层级重新拼接。macOS 上 `alt` 显示为 `option`（大小写不敏感）；`capitalize` 为真时
    /// 只把每个按键名的首字符大写（`ctrl` → `Ctrl`）。`keyHint` 传 `false`，沿用 TS 的
    /// 小写键位提示。空片段（如空串或末尾多一个分隔符）原样保留为空串。
    fn format_key_text(key: &str, capitalize: bool) -> String {
        key.split('/')
            .map(|chord| {
                chord
                    .split('+')
                    .map(|part| {
                        // macOS 的用户习惯把 Alt 键叫 Option；比较不区分大小写，
                        // 因为键位表里可能写成 `alt` 或 `Alt`。
                        let display =
                            if cfg!(target_os = "macos") && part.eq_ignore_ascii_case("alt") {
                                "option"
                            } else {
                                part
                            };
                        if capitalize {
                            let mut chars = display.chars();
                            match chars.next() {
                                Some(first) => {
                                    first.to_uppercase().collect::<String>() + chars.as_str()
                                }
                                None => String::new(),
                            }
                        } else {
                            display.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 一代的共享主体：引擎 + 是否已把引擎交出去（[`Generation::into_engine`]）。
    ///
    /// `Generation` 可克隆（多个持有者共享同一个 `Rc` 代），而线程本地的
    /// 引擎登记（loader / dispatch 的 `Weak`）与 UI 槽（`Rc`）都指向本代；如果每个克隆
    /// 析构时都拆线程本地状态，仍在使用的克隆会被拆掉。因此析构放在**最后一个克隆**释放的
    /// [`GenerationInner`] 上（设计文档 §2.8 的整代销毁）。
    struct GenerationInner {
        engine: Rc<JsEngine>,
        /// [`Generation::into_engine`] 把引擎交给调用方后不再拆除本代的安装状态。
        detached: Cell<bool>,
    }

    impl Drop for GenerationInner {
        fn drop(&mut self) {
            if self.detached.get() {
                return;
            }
            teardown(&self.engine);
        }
    }

    /// 拆除一个引擎的线程本地登记（幂等；只对仍是“当前”的登记生效）。
    ///
    /// 对应设计文档 §2.8：引擎的 `Drop` 负责 `Persistent` 的逆序清理。loader 与 dispatch
    /// 的引擎登记是 `Weak`（`loader::install` / `runner::install_dispatch`），不延长引擎
    /// 生命周期：最后一条强引用释放时引擎（连同 JS 堆）随即销毁，登记由下一次查询惰性
    /// 剔除。UI 槽（ui-adapter）持有 `Rc`，必须显式注销（否则引擎活到线程退出）。
    /// 装配失败与正常析构走同一条路径。
    fn teardown(engine: &Rc<JsEngine>) {
        ui::detach(engine);
    }

    /// 把 `@earendil-works/kiana-tui` 的虚拟 namespace 接到 ui-adapter 发布的运行期面
    /// （设计文档 §5.1/§5.4、§9.2 第 10 步）。
    ///
    /// `virtual_modules` 的种子对象把该 namespace 的原生成员做成动态转发：每次使用都读
    /// `globalThis.__kiana_native["kiana-tui"]`（`bridge.js#lookup`）。ui-adapter 的
    /// `ui::install` 把完整运行期面放在 `globalThis.__kiana_tui`（JS 组合类 + 原生类 + 文本
    /// 工具），本步把它登记为原生桥。没有这一步，任何 `Key`/`CustomEditor`/`Text` 的使用
    /// 都会抛 `kiana: native bridge member "kiana-tui.<name>" is not installed`。
    ///
    /// 与 `VIRTUAL_MODULES["@earendil-works/kiana-tui"] = bundledKianaTui` 的映射对应：模块
    /// namespace 直接就是包的真实导出面，宿主只需提供这份运行期面。
    fn publish_kiana_tui_bridge(engine: &JsEngine) -> EngineResult<()> {
        // builder 在本次 `with_js` 里同步执行，返回的对象被写进
        // `__kiana_native["kiana-tui"]`；之后扩展每次取用该 namespace 的成员都走这个桥。
        // 注意 `set_native_namespace` 是**整组替换** `__kiana_native["kiana-tui"]`，
        // 而不是逐成员合并；这里把读到的 `__kiana_tui` 原对象补齐后返回，所以先前由
        // ui-adapter 发布的成员不会丢。若换成只返回新增成员的对象，其余成员会被抹掉。
        virtual_modules::set_native_namespace(engine, "kiana-tui", |ctx| {
            let tui: Object<'_> = ctx.globals().get("__kiana_tui")?;
            // 三个 coding-agent helper 只在 ui-adapter 尚未提供时补上（幂等）。
            let compat_factory: Function<'_> = ctx.eval(KIANA_TUI_COMPAT_JS)?;
            let highlight = Function::new(ctx.clone(), highlight_code_lines)?;
            let compat: Object<'_> = compat_factory.call((highlight,))?;
            for name in ["getMarkdownTheme", "getSettingsListTheme"] {
                let present = matches!(tui.get::<_, Option<Value<'_>>>(name), Ok(Some(_)));
                if !present {
                    let value: Value<'_> = compat.get(name)?;
                    tui.set(name, value)?;
                }
            }
            if !matches!(tui.get::<_, Option<Value<'_>>>("keyHint"), Ok(Some(_))) {
                tui.set("keyHint", Function::new(ctx.clone(), key_hint)?)?;
            }
            Ok(tui)
        })
    }

    /// 装配好的一“代”扩展 JS 运行时。
    ///
    /// 一代 = 一个 `JsEngine` + 按 `spec/design-extension-runtime.md` §9.2 顺序安装的全部
    /// shim、虚拟模块、扩展 API、上下文、派发与 UI 原生层。`/reload` 或会话替换等价于
    /// 销毁旧代、建立新代（§2.8）：[`ExtensionHost::reload`] 就是这么做的。最后一个克隆
    /// 释放时 `GenerationInner` 的 `Drop` 撤销全部线程本地登记，引擎随之销毁。
    ///
    /// 引擎是 `!Send`（rquickjs 的 `Context`/`Persistent` 都属于创建线程），因此本类型只能在
    /// 创建它的线程上使用；进程入口的单线程 `LocalSet`（`kiana-coding-agent/src/main.rs`）
    /// 正是这个模型（§2.0）。
    #[derive(Clone)]
    pub struct Generation {
        inner: Rc<GenerationInner>,
    }

    impl std::fmt::Debug for Generation {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("Generation")
                .field("generation", &self.inner.engine.config().generation)
                .field("ready", &self.is_ready())
                .finish()
        }
    }

    impl Generation {
        /// 用默认配置（512 MiB / 4 MiB / 100 ms / generation 0）装配一代。
        pub fn new() -> EngineResult<Self> {
            Self::with_config(EngineConfig::default())
        }

        /// 按 §9.2 的严格顺序装配一代。
        ///
        /// 1. [`JsEngine::with_config`]：§9.2 第 1–2 步（Runtime/Context、内存与栈上限、
        ///    中断看门狗、未处理拒绝跟踪器，并注入 `__kiana_host` 的 Promise/定时器原语）；
        /// 2. `loader::js_host::install_intl`：§9.2 第 3 步，**必须最早**——任何模块的顶层
        ///    `Intl`/`toLocale*` 引用都要在这之后；
        /// 3. [`engine::install`]：§9.2 第 4 步，`js/prelude_core.js`（`__kiana` 协议、定时器、
        ///    AbortSignal）；
        /// 4. `loader::js_host::install`：§9.2 第 5 步，Node 全局与 `node:*` shim；
        /// 5. [`loader::install`]：§9.2 第 5–6 步的 loader 侧（模块 loader + 虚拟模块源登记）
        ///    与第 7 步的 `js/api.js`。虚拟模块求值需要 loader 已就位，因此本步在下一步之前；
        ///    `js/api.js` 的顶层不 import 任何虚拟模块（只定义 `makeApi`），提前求值没有
        ///    可观察差异（见交付报告 deviations）；
        /// 6. [`virtual_modules::install`]：§9.2 第 5–6 步，22 个键 + typebox（求值一次，
        ///    身份共享）；
        /// 7. [`runner::context::install`]：§9.2 第 7 步，`js/context.js` 与 `kiana_ctx`
        ///    宿主原语；
        /// 8. [`runner::install_dispatch`]：§9.2 第 7 步，`js/dispatch.js` 与
        ///    `__kiana_dispatch_host`；
        /// 9. [`ui::install`]：§9.2 第 7 步之后的 UI 层，`js/ui/{tui,ui}.js` +
        ///    原生组件/主题/桥；
        /// 10. `virtual_modules::set_native_namespace("kiana-tui", __kiana_tui)`：把上一步发布的
        ///     运行期面接到 `@earendil-works/kiana-tui` 虚拟 namespace 的原生桥上
        ///     （TS 的 `VIRTUAL_MODULES` 直接指向打包的 kiana-tui 模块）。
        ///
        /// 第 5 步的 [`loader::install`] 还会把本引擎登记为线程的“当前引擎”（UI 工厂闭包
        /// 与线程本地的 loader 查询靠它）。同一线程上已有另一个存活宿主时，新引擎成为
        /// 当前引擎，但不会清空它登记的扩展/订阅——那些条目属于仍在运行的上一代
        /// （见 [`loader::install`] 的文档与 `tests/public_surface.rs` 的多宿主用例）。
        ///
        /// 任何一步失败都返回引擎错误；已经建好的引擎随返回值丢弃，按 §2.8 的析构顺序清理，
        /// 不留半装的线程本地状态。
        pub fn with_config(config: EngineConfig) -> EngineResult<Self> {
            // `/reload` 的第 2 步（`resource-loader.ts:544`）：新代不能复用旧代的工厂。
            // 缓存值持有旧引擎持久槽里的回调 id，而 id 序号是线程全局的——新引擎上取回
            // 只会得到 `Extension callback is no longer registered`。必须在
            // `loader::install` 把新引擎登记为当前引擎**之前**清空：缓存项的 `Drop` 依赖
            // 当前引擎来释放持久槽，早清才指向旧引擎（TS 的 GC 等价物）。
            loader::factory_cache::clear_extension_cache();

            // 1. 引擎本身：Runtime/Context + limits + `__kiana_host`，尚未装任何模块。
            let engine = Rc::new(JsEngine::with_config(config)?);

            let installed = (|| -> EngineResult<()> {
                // 2. Intl 必须最先安装。
                loader::js_host::install_intl(&engine)?;
                // 3. prelude_core（`__kiana` 协议；依赖已注入的 `__kiana_host`）。
                engine::install(&engine)?;
                // 4. Node 全局与 `node:*` shim。
                loader::js_host::install(&engine)?;
                // 5. 模块 loader + `js/api.js`（虚拟模块的前置条件）。
                loader::install(Rc::clone(&engine))?;
                // 6. 22 个虚拟模块 + typebox。
                virtual_modules::install(&engine)?;
                // 7. `ctx` 与 `js/context.js`。
                runner::context::install(&engine)?;
                // 8. 13 个 emitter 与 `js/dispatch.js`。
                runner::install_dispatch(Rc::clone(&engine))?;
                // 9. UI 适配层（原生 TUI 组件 + `ctx.ui` 桥）。`install_attached` 同时把引擎
                //    登记到 `ui::adapter::JsEngineSlot`：UI 工厂闭包是 `Send + Sync`、不能捕获
                //    `Rc<JsEngine>`，只能在被调用时按线程查当前引擎（设计 §5.6）。
                ui::install_attached(&engine)?;
                // 10. kiana-tui 虚拟 namespace 的原生桥。
                publish_kiana_tui_bridge(&engine)?;
                Ok(())
            })();
            if let Err(error) = installed {
                // 半装的代不能把引擎留在任何线程本地槽里（它们持有 `Rc`，会让引擎活过
                // 返回值）；`teardown` 只清仍指向本引擎的登记。
                teardown(&engine);
                return Err(error);
            }

            Ok(Self {
                inner: Rc::new(GenerationInner {
                    engine,
                    detached: Cell::new(false),
                }),
            })
        }

        /// 引擎本体（按设计文档 §2.0 的单线程模型，只允许在创建线程上使用）。
        pub fn engine(&self) -> &Rc<JsEngine> {
            &self.inner.engine
        }

        /// 取出引擎，保留这一代的全部安装状态。
        ///
        /// 交出后本代（及其克隆）析构时不再撤销线程本地登记：调用方拿到的是仍处于
        /// “已安装”状态的引擎，等价于把这一代的所有权移交给它。
        ///
        /// 注意：跳过 `teardown` 也意味着 UI 线程槽（`ui::adapter::JsEngineSlot`）仍持有
        /// 引擎的 `Rc`（`ui::install_attached` 登记）。调用方若希望最后一个句柄释放时引擎
        /// 就被销毁，需要自己调用 [`ui::detach`]；否则引擎会留在线程槽里直到线程退出。
        pub fn into_engine(self) -> Rc<JsEngine> {
            self.inner.detached.set(true);
            Rc::clone(&self.inner.engine)
        }

        /// 自检：§9.2 的每一层都已安装（装配器与测试用）。
        ///
        /// 只读全局标记，不触发任何 JS 求值之外的副作用。它证明的是“装配完成”，**不**证明
        /// 任何扩展已加载。内部走两次
        /// [`engine::JsEngine::with_js`]，因此不能在另一个 `with_js` 事务内调用
        /// （嵌套会因 `Context` 的 `RefCell` 已借出而 panic，见 [`engine`] 的模块文档）。
        pub fn is_ready(&self) -> bool {
            let engine = &self.inner.engine;
            // 检查顺序与装配顺序一致：每个全局由对应 install 步骤写入（`__kiana_api` 来自
            // loader、`__kiana_virtual` 来自 virtual_modules、`__kiana_ui_native` 来自 ui）。
            let globals_ready = engine.with_js(|ctx| -> bool {
                let has = |name: &str| {
                    matches!(ctx.globals().get::<_, Option<Object>>(name), Ok(Some(_)))
                };
                has("__kiana")
                    && has("__kiana_api")
                    && has("__kiana_virtual")
                    && has("__kiana_ctx")
                    && has("__kiana_dispatch")
                    && has("__kiana_ui_native")
                    && has("__kiana_tui_native")
            });
            // 原生桥：`__kiana_native["kiana-tui"]` 已发布（第 10 步）。
            let bridge_ready = engine.with_js(|ctx| -> bool {
                let Ok(bridge) = ctx
                    .globals()
                    .get::<_, Option<Object>>(virtual_modules::NATIVE_BRIDGE_GLOBAL)
                else {
                    return false;
                };
                let Some(bridge) = bridge else {
                    return false;
                };
                let Ok(Some(tui)) = bridge.get::<_, Option<Object>>("kiana-tui") else {
                    return false;
                };
                // 不只看对象存在：至少一个原生成员必须真的可调用。
                matches!(tui.get::<_, Option<Function>>("Text"), Ok(Some(_)))
                    && matches!(tui.get::<_, Option<Function>>("matchesKey"), Ok(Some(_)))
            });
            // 最后两项用 host kind 表证明 runner 的 context 与 ui 的异步桥都注册在本引擎上
            // （`register_host_kind` 写的是 `EngineShared` 里的表，按引擎隔离）。
            globals_ready
                && bridge_ready
                && loader::is_installed(engine)
                && virtual_modules::is_installed(engine)
                && engine.has_host_kind(runner::context::HOST_KIND)
                && engine.has_host_kind(ui::HOST_KIND)
        }
    }

    /// 扩展宿主：持有当前代，负责代际重载。
    ///
    /// 这是本 crate 面向装配方的唯一入口。`ExtensionHost::new()` 之后，调用方拿
    /// [`ExtensionHost::engine`] 去调用 `loader::load_extensions*` / `runner::ExtensionRunner`，
    /// **不要**自己再调用任何 `install`：顺序只在这里维护。
    ///
    /// 宿主与它持有的引擎都是 `!Send`：必须在创建引擎的线程上创建、使用并销毁。宿主析构
    /// （最后一个 [`Generation`] 克隆释放）时整代被拆毁，线程本地登记随之撤销（§2.8）。
    #[derive(Debug)]
    pub struct ExtensionHost {
        config: EngineConfig,
        /// 只在 [`ExtensionHost::reload`] 装配新代的瞬间是 `None`（`&mut self` 期间外界
        /// 无法观察；装配失败后保持 `None`，见该方法的错误约定）。
        generation: Option<Generation>,
    }

    impl ExtensionHost {
        /// 默认配置装配（512 MiB / 4 MiB / 100 ms / generation 0，见 [`Generation::new`]）。
        pub fn new() -> EngineResult<Self> {
            Self::with_config(EngineConfig::default())
        }

        /// 指定配置装配（内存/栈/看门狗/代际号）；配置会被保存，供随后的
        /// [`ExtensionHost::reload`] 以此为基生成下一代。
        pub fn with_config(config: EngineConfig) -> EngineResult<Self> {
            let generation = Generation::with_config(config.clone())?;
            Ok(Self {
                config,
                generation: Some(generation),
            })
        }

        /// 当前代。
        ///
        /// 只在 [`ExtensionHost::reload`] 失败后缺失（此时调用方已按约定放弃该宿主）。
        pub fn generation(&self) -> &Generation {
            self.generation
                .as_ref()
                .expect("ExtensionHost has no generation: the last reload failed")
        }

        /// 当前代的引擎。
        pub fn engine(&self) -> &Rc<JsEngine> {
            self.generation().engine()
        }

        /// 当前配置（下一次 [`ExtensionHost::reload`] 以此为基）。
        pub fn config(&self) -> &EngineConfig {
            &self.config
        }

        /// 销毁旧代、按同一配置建立新代（`EngineConfig::generation` +1）。
        ///
        /// 供需要整代替换的调用方使用（当前仓库的 `/reload` 走 `kiana-session` 的
        /// `AgentSession::reload` → `kiana-resources` 的 `DefaultResourceLoader::reload`，
        /// 不经过本方法）：旧代的 `kiana`/`ctx` 句柄必须失效（`invalidate` 由上层在
        /// 调用本方法前触发；`api.js` 对旧 `kiana` 的调用会抛 stale 文案，§4.6）。整代替换的
        /// 顺序与 TS 的 `ResourceLoader.reload()` 一致：先销毁旧代（引擎与线程本地登记），
        /// 再装配新代。这样新代的 `loader::install` 看到线程上已无存活引擎，会清空上一代
        /// 残留的扩展登记（同线程上另一个仍存活的宿主则不受影响）；工厂缓存由
        /// [`Generation::with_config`] 在安装新引擎之前清空（TS `resource-loader.ts:544`
        /// 的 `clearExtensionCache()`）。析构顺序由 [`JsEngine`] 的 `Drop` 保证（§2.8）。
        ///
        /// 低层装配失败时（内存/引擎错误），旧代已经销毁且没有可回滚的状态；调用方应把
        /// `Err` 当作致命错误，不要再使用该宿主（TS 的 reload 失败是扩展级错误列表，
        /// 不涉及重建 JS 引擎，因此没有对应恢复语义）。
        pub fn reload(&mut self) -> EngineResult<()> {
            let config = self.config.next_generation();
            // 先拆旧代：`GenerationInner` 的析构撤销 UI 槽登记，loader/dispatch 的 `Weak`
            // 登记随最后一条强引用释放而失效；引擎的 `Drop` 完成 `Persistent` 的逆序清理
            // （§2.8）。上层应在此之前对旧 ctx 调用 `invalidate`，让 stale 文案先生效
            // （本方法只负责资源）。
            drop(self.generation.take());
            let generation = Generation::with_config(config.clone())?;
            self.generation = Some(generation);
            self.config = config;
            Ok(())
        }
    }
}

/// 装配器只从这里对外暴露：[`Generation`] 与 [`ExtensionHost`] 的 `install` 细节留在私有
/// `assembly` 模块内，调用方只能拿到“已装好的一代”，不能自行拼装。
#[cfg(feature = "runtime")]
pub use assembly::{ExtensionHost, Generation};
