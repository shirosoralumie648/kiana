//! core/export-html/tool-renderer.ts。
//!
//! HTML 导出里的自定义工具渲染器：调用工具的 TUI `renderCall` / `renderResult`，
//! 把组件输出的 ANSI 行转成 HTML。
//!
//! 渲染宽度固定为 100（不随导出设备变化）；`isPartial: false`、`showImages: false`、
//! `outputPad: 1`、`durationMs: None`（因此计时行走“本地时钟”路径，预渲染的时长文本
//! 不可信，固定该路径以保证输出稳定）。
//!
//! TS 在两个方法外各包一层 `try/catch`（任何异常 → `undefined`，调用方回退到结构化
//! 渲染）。Rust 的渲染器不返回 `Result`，这里用 [`std::panic::catch_unwind`] 在边界上
//! 复刻该语义：一个 panic 的扩展渲染器不会中断导出。
//!
//! # 在导出流程中的位置
//!
//! `index` 的导出入口把 [`ToolHtmlRenderer`] 放进 [`crate::index::ExportOptions`]；
//! `pre_render_custom_tools` 按会话条目顺序调用 [`ToolHtmlRenderer::render_call`] 与
//! [`ToolHtmlRenderer::render_result`]，把 HTML 写进 `renderedTools`。本模块提供默认
//! 实现：按工具名查出 `renderCall`/`renderResult`，以固定宽度渲染成 ANSI 行，再交给
//! [`mod@crate::ansi_to_html`] 转 HTML。`kiana-session` 的 `AgentSession::export_to_html`
//! 负责组装依赖（扩展解析器、当前主题与 cwd）。
//!
//! # 每个 toolCallId 的四份缓存
//!
//! | 映射 | 内容 | 写入时机 |
//! |---|---|---|
//! | `rendered_call_components` | 调用视图组件 | `render_call` |
//! | `rendered_result_components` | 折叠/展开视图组件（同一槽位，后写覆盖） | `render_result` |
//! | `rendered_states` | call 与 result 共享的 JSON 状态 | 首次创建上下文时 |
//! | `rendered_args` | 工具调用参数 | `render_call`（没有调用过时为 `null`） |
//!
//! 缓存以 `toolCallId` 为键、在一次导出内持续存在，所以同一工具调用的折叠与展开两次
//! 渲染看到同一个 `state`（测试 `render_context_fields_match_ts` 断言 result 渲染仍能
//! 读到 call 阶段写入的状态，`state_calls == 1`）。
//!
//! # 固定的渲染参数
//!
//! 宽度默认 100；上下文恒为 `execution_started=true`、`args_complete=true`、
//! `show_images=false`、`duration_ms=None`、`output_pad=1`，`invalidate` 是空操作。
//! `expanded`/`is_partial`/`is_error` 随调用变化。导出的是静态文件，没有实时执行状态，
//! 这些值固定不变，否则渲染器内部按标志分支的输出会不同，导出结果就不可复现。
//!
//! # 失败模式
//!
//! 渲染器（包括组件自身的 `render`）panic 时，方法返回 `None`，等价 TS 的
//! `catch { return undefined }`：导出继续，该工具没有预渲染 HTML，模板按结构化视图
//! 回退。panic 输出被线程局部的 hook 抑制，共享状态的 poison 会被清除。
//!
//! `renderShell` 字段不参与 HTML 导出：这里只调用 `renderCall`/`renderResult`。

use std::cell::Cell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};

use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_ext_api::AgentToolResult;
use kiana_ext_api::tool_types::{
    RenderState, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_tui::tui::Component;

use crate::ansi_to_html::{ansi_lines_to_html, match_sgr_at};

/// 按工具名解析渲染器的回调（TS `(name) => ToolRenderers | undefined`）。
///
/// 不要求 `Send`/`Sync`：解析过程可能捕获扩展运行器（单线程 `LocalSet`）。
///
/// 返回 `None` 表示该工具没有可用的渲染器集合；即使集合存在，也可能只有
/// `render_call` 或只有 `render_result`，两个方法各自判断。
pub type GetToolRenderersFn = Arc<dyn Fn(&str) -> Option<ToolRenderers>>;

thread_local! {
    /// 当前线程是否处于「吞掉渲染器 panic 输出」的区间。TS 的 `catch {}` 不打印任何
    /// 东西，而 `catch_unwind` 默认仍会通过 panic hook 往 stderr 写
    /// `thread 'main' panicked ...`。
    static SUPPRESS_PANIC_OUTPUT: Cell<bool> = const { Cell::new(false) };
}

static PANIC_HOOK_INSTALLED: Once = Once::new();

/// 安装一个可抑制输出的 panic hook（只装一次，链到既有 hook）。
///
/// 抑制标记是线程局部的：导出渲染器在其它线程 panic 时仍走原 hook。全局 hook 被替换
/// 只发生一次；若之后有别的库再次 `set_hook`，本行为会退化为默认输出（与 TS 的偏差
/// 只会出现在那种情况下）。
fn install_quiet_panic_hook() {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SUPPRESS_PANIC_OUTPUT.with(Cell::get) {
                return;
            }
            previous(info);
        }));
    });
}

/// `catch_unwind` + 静默 panic 输出：TS 渲染器异常时 `catch {}` 不留任何输出。
///
/// 闭包借用了 `&mut self`（缓存、组件与状态），不满足 `UnwindSafe`，因此用
/// `AssertUnwindSafe` 包装。借用不会越过本函数边界；panic 后调用方会清除状态
/// `Mutex` 的 poison，缓存里最多缺少本次组件或停留在上一阶段，不会留下悬垂引用。
fn catch_renderer_panic<T>(render: impl FnOnce() -> T) -> Option<T> {
    install_quiet_panic_hook();
    SUPPRESS_PANIC_OUTPUT.with(|flag| flag.set(true));
    let outcome = catch_unwind(AssertUnwindSafe(render));
    SUPPRESS_PANIC_OUTPUT.with(|flag| flag.set(false));
    outcome.ok()
}

/// 自定义工具 HTML 渲染器的依赖。
///
/// TS: `tool-renderer.ts#ToolHtmlRendererDeps`。这里不做任何校验：回调在每次
/// `render_call`/`render_result` 时按工具名调用；`width=None` 表示使用默认 100。
pub struct ToolHtmlRendererDeps {
    /// 按工具名取得渲染器集合（扩展解析结果或已注册工具）。
    pub get_tool_renderers: GetToolRenderersFn,
    /// 渲染使用的主题。
    ///
    /// TS 直接持有 `Theme` 对象；Rust 的 [`Theme`] 不可 `Clone`，全局主题以
    /// `Arc<Theme>` 保存（`kiana_theme::current_theme()`），因此这里用 `Arc`。
    pub theme: Arc<Theme>,
    /// 渲染上下文的 cwd。
    pub cwd: String,
    /// 渲染宽度；`None` 表示使用默认值 `100`。
    pub width: Option<usize>,
}

/// 一个工具结果的两种渲染视图。
///
/// TS: `{ collapsed?: string; expanded?: string }`。`expanded` 总是存在（可能是空串），
/// `collapsed` 仅在非空且与 `expanded` 不同时存在。
///
/// 两个字段都是完整的 HTML 片段（若干 `<div class="ansi-line">` 的拼接），由 `index`
/// 序列化进 `renderedTools` 的 JSON 载荷；`None` 的键不会出现在载荷里。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedToolResult {
    /// 折叠视图 HTML（省略条件见类型注释）。
    pub collapsed: Option<String>,
    /// 展开视图 HTML（可能为空串）。
    pub expanded: String,
}

/// 把工具调用/结果渲染为 HTML。
///
/// TS: `tool-renderer.ts#ToolHtmlRenderer`。方法需要 `&mut self`，因为渲染器按
/// `toolCallId` 缓存组件、参数与渲染状态。
///
/// # 实现约定
///
/// - 以 `tool_call_id` 区分同一次导出里的多次工具调用；参数、状态与组件要保留到
///   导出结束，因为结果渲染可能晚于调用渲染。
/// - 返回 `None` 表示“该工具没有自定义渲染器”，不是错误：调用方按结构化视图回退。
/// - 实现必须自行吞掉渲染期间的异常（TS 版是 `try/catch`），trait 不返回 `Result`。
/// - 调用视图返回 `Some("")` 时，`index` 会把它当作假值丢弃（与 TS 的 `if (callHtml)`
///   一致）；结果视图的 `expanded` 允许为空串。
pub trait ToolHtmlRenderer {
    /// 渲染工具调用。工具没有自定义渲染器时返回 `None`。
    ///
    /// `args` 是会话条目里的原始工具参数（缺省时调用方传 `null`）；返回完整的 HTML
    /// 片段。渲染状态按 `tool_call_id` 缓存，稍后的 `render_result` 会复用。
    fn render_call(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        args: &JsonValue,
    ) -> Option<String>;

    /// 渲染工具结果（折叠 + 展开）。工具没有自定义渲染器时返回 `None`。
    ///
    /// `result` 是内容块（未知块类型在 `index` 转成接口类型时已被过滤），`details`
    /// 是原始 `details`（缺省为 `null`），`is_error` 来自会话条目 `isError` 的真值判断。
    fn render_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[UserContentBlock],
        details: &JsonValue,
        is_error: bool,
    ) -> Option<RenderedToolResult>;
}

/// [`create_tool_html_renderer`] 返回的具体渲染器。
///
/// 四个按 `toolCallId` 索引的映射与 TS 的 `renderedCallComponents` /
/// `renderedResultComponents` / `renderedStates` / `renderedArgs` 一一对应。
///
/// 内部持有 `Box<dyn Component>` 与未要求 `Send + Sync` 的解析回调，因此整个渲染器
/// 不是 `Send`/`Sync`：应在单线程内创建并完成一次导出。缓存在实例内累积且没有重置
/// 接口，不同会话应使用新的实例（否则相同 `toolCallId` 可能命中上一次的组件）。
pub struct DefaultToolHtmlRenderer {
    get_tool_renderers: GetToolRenderersFn,
    theme: Arc<Theme>,
    cwd: String,
    width: Option<usize>,
    rendered_call_components: HashMap<String, Box<dyn Component>>,
    rendered_result_components: HashMap<String, Box<dyn Component>>,
    rendered_states: HashMap<String, RenderState>,
    rendered_args: HashMap<String, JsonValue>,
}

impl DefaultToolHtmlRenderer {
    fn width(&self) -> usize {
        self.width.unwrap_or(100)
    }

    /// TS `getState(id)`：首次访问时创建 `{}`。
    ///
    /// 返回 `Arc` 克隆，调用方与映射共享同一把锁：同一次工具调用的 call 与 result
    /// （以及折叠/展开两次渲染）读写的是同一份状态。
    fn get_state(&mut self, tool_call_id: &str) -> RenderState {
        if let Some(state) = self.rendered_states.get(tool_call_id) {
            return state.clone();
        }
        let state: RenderState = Arc::new(Mutex::new(JsonValue::Object(serde_json::Map::new())));
        self.rendered_states
            .insert(tool_call_id.to_string(), state.clone());
        state
    }

    /// 清掉渲染器 panic 后留在共享状态 Mutex 上的 poison 标记。
    ///
    /// 渲染器持锁时 panic 会让 `Mutex` 进入 poisoned 状态，后续 `.lock().unwrap()`
    /// 会再次 panic；TS 的普通状态对象没有这个概念，同一 toolCallId 的后续渲染必须
    /// 继续可用。
    ///
    /// 只清除“曾 panic”的标记，不修改受保护的数据（`Mutex::clear_poison` 的语义），
    /// 因此 panic 前已写入的 `JsonValue` 字段仍然保留。
    fn clear_state_poison(&self, tool_call_id: &str) {
        if let Some(state) = self.rendered_states.get(tool_call_id) {
            state.clear_poison();
        }
    }

    /// TS `createRenderContext(...)`。
    ///
    /// 字段来源：`args` 取 `rendered_args`（从未调用过 `render_call` 时是 `null`），
    /// `state` 按 `tool_call_id` 懒创建，`cwd` 来自依赖里的字符串，`last_component`
    /// 由调用方给出（该槽位上一次返回的组件）。其余字段是固定值：
    /// `execution_started=true`、`args_complete=true`、`show_images=false`、
    /// `duration_ms=None`、`output_pad=1`，`invalidate` 为空操作。
    fn create_render_context(
        &mut self,
        tool_call_id: &str,
        last_component: Option<Box<dyn Component>>,
        expanded: bool,
        is_partial: bool,
        is_error: bool,
    ) -> ToolRenderContext {
        let args = self
            .rendered_args
            .get(tool_call_id)
            .cloned()
            .unwrap_or(JsonValue::Null);
        let state = self.get_state(tool_call_id);
        let invalidate: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        ToolRenderContext {
            args,
            tool_call_id: tool_call_id.to_string(),
            invalidate,
            last_component,
            state,
            cwd: PathBuf::from(self.cwd.as_str()),
            execution_started: true,
            args_complete: true,
            is_partial,
            expanded,
            show_images: false,
            is_error,
            duration_ms: None,
            output_pad: 1,
        }
    }
}

/// JS `String.prototype.trim()` 的空白集合：`WhiteSpace + LineTerminator`。
///
/// 与 Rust 的 [`str::trim`]（Unicode `White_Space`）不同：JS 不把 U+0085 NEL 当空白，
/// 但会把 U+FEFF ZWNBSP 当空白。
fn is_js_trim_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | ' '
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// TS `isBlankRenderedLine(line)`：去掉 SGR 后 `trim()` 为空（JS 空白语义）。
///
/// 组件常给空行也套上颜色/样式转义，所以必须先去掉 SGR 再判断；只含转义序列的行
/// 也算空白行。
fn is_blank_rendered_line(line: &str) -> bool {
    strip_sgr(line)
        .trim_matches(is_js_trim_whitespace)
        .is_empty()
}

/// 去掉一行里的所有 SGR 序列（其它字节原样保留）。
///
/// 识别规则复用 `ansi_to_html` 的 `match_sgr_at`：只去掉完整的 `ESC [ 数字/; m`
/// 序列，光标控制等其它转义保留，因此在 `trim` 判定中不会被误当成空白。
fn strip_sgr(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut last_index = 0_usize;
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        if let Some((_, sequence_end)) = match_sgr_at(bytes, cursor) {
            output.push_str(&line[last_index..cursor]);
            last_index = sequence_end;
            cursor = sequence_end;
            continue;
        }
        cursor += 1;
    }
    output.push_str(&line[last_index..]);
    output
}

/// TS `trimRenderedResultLines(lines)`：只去掉首尾空白行，中间保留。
///
/// 例如 `["", "one", "", "two", ""]` 会变成 `["one", "", "two"]`：中间空行属于结果
/// 内容，不能丢。返回新的 `Vec`（克隆切片），调用方仍持有原始行。
fn trim_rendered_result_lines(lines: &[String]) -> Vec<String> {
    let mut start = 0_usize;
    let mut end = lines.len();
    while start < end && is_blank_rendered_line(&lines[start]) {
        start += 1;
    }
    while end > start && is_blank_rendered_line(&lines[end - 1]) {
        end -= 1;
    }
    lines[start..end].to_vec()
}

/// 创建工具 HTML 渲染器。
///
/// TS: `tool-renderer.ts#createToolHtmlRenderer`。
///
/// 只装配依赖与空缓存，不做校验；`width` 的默认值在每次渲染时取 100。返回的实例
/// 应在一次导出内复用：缓存按 `toolCallId` 累积，跨会话复用会命中上一次的组件。
pub fn create_tool_html_renderer(deps: ToolHtmlRendererDeps) -> DefaultToolHtmlRenderer {
    DefaultToolHtmlRenderer {
        get_tool_renderers: deps.get_tool_renderers,
        theme: deps.theme,
        cwd: deps.cwd,
        width: deps.width,
        rendered_call_components: HashMap::new(),
        rendered_result_components: HashMap::new(),
        rendered_states: HashMap::new(),
        rendered_args: HashMap::new(),
    }
}

impl ToolHtmlRenderer for DefaultToolHtmlRenderer {
    fn render_call(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        args: &JsonValue,
    ) -> Option<String> {
        let outcome = catch_renderer_panic(|| {
            // 先记录参数：即使工具没有 renderCall、或后续渲染 panic，参数也留在映射里
            // （TS 在查渲染器之前就 set）。render_result 的上下文从这里读取。
            self.rendered_args
                .insert(tool_call_id.to_string(), args.clone());
            // 没有渲染器集合、或集合里没有 renderCall，都视为“无自定义渲染器”。
            let tool_def = (self.get_tool_renderers)(tool_name)?;
            let render_call = tool_def.render_call?;

            // 取出该槽位上一次的组件作为 lastComponent（HashMap 需要 remove 才能移出
            // 所有权）。注意 remove 发生在调用渲染器之前：若渲染器 panic，旧组件不会
            // 回到映射（TS 的 Map 在抛错时仍保留旧值），同一 id 下次看到的
            // lastComponent 会是 None。
            let last_component = self.rendered_call_components.remove(tool_call_id);
            let context =
                self.create_render_context(tool_call_id, last_component, false, true, false);
            let component = render_call(args, self.theme.as_ref(), &context);
            self.rendered_call_components
                .insert(tool_call_id.to_string(), component);

            // 组件在 render 之前已写回映射，与 TS 的 `set` → `render` 顺序一致。
            let width = self.width();
            let lines = self
                .rendered_call_components
                .get_mut(tool_call_id)?
                .render(width);
            Some(ansi_lines_to_html(&lines))
        });
        if outcome.is_none() {
            self.clear_state_poison(tool_call_id);
        }
        outcome.flatten()
    }

    fn render_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[UserContentBlock],
        details: &JsonValue,
        is_error: bool,
    ) -> Option<RenderedToolResult> {
        let outcome = catch_renderer_panic(|| {
            // 渲染器集合或 renderResult 缺失时返回 None；renderCall 可以独立存在。
            let tool_def = (self.get_tool_renderers)(tool_name)?;
            let render_result_fn = tool_def.render_result?;

            // TS 只构造 `{ content, details, isError }` 三个字段；Rust 结构体其余字段
            // 必须显式给 None。
            let agent_tool_result: AgentToolResult<JsonValue> = AgentToolResult {
                content: result.to_vec(),
                details: details.clone(),
                structured_content: None,
                usage: None,
                is_error: Some(is_error),
                terminate: None,
            };
            let width = self.width();

            // 折叠视图：以 expanded=false、is_partial=false 调用渲染器，is_error 原样
            // 透传；旧组件作为 lastComponent。与 render_call 相同，remove 先于调用：
            // 渲染器 panic 时旧组件不会回到映射（TS 的 Map 会保留它）。
            let last_component = self.rendered_result_components.remove(tool_call_id);
            let context =
                self.create_render_context(tool_call_id, last_component, false, false, is_error);
            let collapsed_component = render_result_fn(
                &agent_tool_result,
                &ToolRenderResultOptions {
                    expanded: false,
                    is_partial: false,
                },
                self.theme.as_ref(),
                &context,
            );
            self.rendered_result_components
                .insert(tool_call_id.to_string(), collapsed_component);
            let lines = self
                .rendered_result_components
                .get_mut(tool_call_id)?
                .render(width);
            // 结果视图只裁掉首尾空白行（render_call 的输出不做裁剪）。
            let collapsed = ansi_lines_to_html(&trim_rendered_result_lines(&lines));

            // 展开视图总是重新渲染一次（即使内容与折叠视图相同，测试断言两次 render），
            // 且与折叠视图共享同一个 state；`lastComponent` 是刚渲染的折叠组件。
            let last_component = self.rendered_result_components.remove(tool_call_id);
            let context =
                self.create_render_context(tool_call_id, last_component, true, false, is_error);
            let expanded_component = render_result_fn(
                &agent_tool_result,
                &ToolRenderResultOptions {
                    expanded: true,
                    is_partial: false,
                },
                self.theme.as_ref(),
                &context,
            );
            self.rendered_result_components
                .insert(tool_call_id.to_string(), expanded_component);
            let lines = self
                .rendered_result_components
                .get_mut(tool_call_id)?
                .render(width);
            let expanded = ansi_lines_to_html(&trim_rendered_result_lines(&lines));

            // TS: `collapsed && collapsed !== expanded ? { collapsed } : {}`：
            // 空串是假值，内容相同也省略。
            Some(RenderedToolResult {
                collapsed: if !collapsed.is_empty() && collapsed != expanded {
                    Some(collapsed)
                } else {
                    None
                },
                expanded,
            })
        });
        if outcome.is_none() {
            self.clear_state_poison(tool_call_id);
        }
        outcome.flatten()
    }
}
