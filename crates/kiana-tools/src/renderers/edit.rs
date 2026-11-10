//! core/tools/renderers/edit.ts。
//!
//! `edit` 工具的 TUI 渲染，`renderShell: "self"` 的完整自绘框架：
//!
//! - 调用行是一个 [`BoxComponent`]（paddingX = `outputPad`、paddingY = 1），背景按
//!   `toolPendingBg` / `toolSuccessBg` / `toolErrorBg` 切换；有预览时追加 Spacer + diff。
//! - `argsComplete` 为真时异步调用 [`crate::edit_diff::compute_edits_diff`] 预演 diff；
//!   结果写入共享 [`RenderState`]（TS 的 `EditRenderState.callComponent`），完成后请求重绘。
//! - 结果落定时把 `details.diff` 写回预览；**只有预览真正变化**才重建盒子
//!   （`edit-tool-no-full-redraw` 的验收点）。`argsComplete` 从未为真的回放场景由
//!   `renderResult` 从已存结果恢复预览。
//! - 结果 diff 走 [`kiana_ui_kit::diff::render_diff`] 并带 `filePath`；预览 diff 不带。
//!
//! # 状态机（每个 tool call 一份 `RenderState`）
//! 状态存在 `RenderState["editRender"]` 下，序列化为 [`EditRenderState`]，字段含义：
//! - `preview` + `preview_args_key`：预览内容与它对应的参数指纹
//!   （`JSON.stringify({path, edits})`）；
//! - `preview_pending`：已 spawn 预演、尚未回写；
//! - `settled_error`：结果已以错误落定，头部背景应保持错误色；
//! - `call_rendered`：`render_call` 是否跑过，等价 TS 的 `callComponent` 是否存在。
//!
//! `render_call` 的转换：
//! 1. 首次调用置 `call_rendered = true`；
//! 2. 参数指纹变化时清空 `preview`、`preview_pending` 与 `settled_error`——旧参数的
//!    预演结果不能再显示；
//! 3. `args_complete && 有可预览输入 && preview.is_none() && !preview_pending` 时置
//!    `preview_pending = true` 并 spawn 预演；完成回调先比对指纹（参数可能已变），只有
//!    仍匹配才写回预览并 `invalidate()` 请求重绘；
//! 4. 用当前状态重建盒子；背景色由 `preview` 类型或 `settled_error` 决定。
//!
//! `render_result` 的转换：
//! - `call_rendered` 为假（只有结果、没有调用组件的回放路径）时不写状态，也不以预览做
//!   结果区去重；结果 diff 直接渲染在结果区；
//! - 有调用组件且 `details.diff` 是字符串时把它写回预览（结果落定后调用行显示 diff，
//!   空串也写回，但空串不会在结果区产生内容）；
//! - 结果 diff 与预览 diff 相同（或结果 diff 为空串）时结果区不渲染，避免同一个 diff
//!   出现两次。
//!
//! # 失败模式
//! - 预演失败（文件不存在/不可读/编辑不适用）以 [`EditPreview::Error`] 存储并渲染红色
//!   头部；若结果的错误文本与预览错误相同，结果区不再重复输出；
//! - 后台预演优先 `tokio::spawn`；没有 tokio 运行时时退回独立线程 + current-thread
//!   runtime。runtime 建不起来就静默放弃预览（调用行保持 pending 背景），不影响工具
//!   执行本身；
//! - 状态锁中毒时用 `into_inner` 继续（渲染状态不是数据完整性边界，宁可画出陈旧预览）。
//!
//! # 注意事项
//! - `preview` 的 `EditDiffResult`/`EditDiffError` 用 `#[serde(untagged)]` 区分，字段形状
//!   属于渲染层接口（见 `crate::edit_diff`）。
//! - 两处路径读取的优先级不同：显示用的 `pick_path_value` 先 `file_path` 后 `path`，
//!   而预演用的 `get_renderable_preview_input` 先 `path` 后 `file_path`。两者同时给出且
//!   不同时，调用行显示 `file_path`，预演却针对 `path`；现有测试只覆盖了前者的优先级，
//!   修改任一优先级前请先补测试。
//! - 本文件每次 `render_call`/`render_result` 返回全新组件，不复用 `last_component`；
//!   TS 的原地更新通过 `invalidate()` 触发下一帧重建来等价实现。

use std::sync::Arc;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, RenderState, ToolRenderContext, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::{StyleFn, Text};
use kiana_tui::tui::{Component, Container};

use crate::edit_diff::{Edit, EditDiffError, EditDiffResult, compute_edits_diff};
use crate::render_utils::{render_tool_path, str};

/// 渲染状态在 `RenderState` JSON 对象里的键。
const EDIT_STATE_KEY: &str = "editRender";

/// `EditPreview = EditDiffResult | EditDiffError`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum EditPreview {
    /// 预演成功。
    Result(EditDiffResult),
    /// 预演失败（文件不可读/编辑不适用）。
    Error(EditDiffError),
}

impl EditPreview {
    fn as_result(&self) -> Option<&EditDiffResult> {
        match self {
            EditPreview::Result(result) => Some(result),
            EditPreview::Error(_) => None,
        }
    }

    fn as_error(&self) -> Option<&EditDiffError> {
        match self {
            EditPreview::Error(error) => Some(error),
            EditPreview::Result(_) => None,
        }
    }
}

/// `EditRenderState`：每个 tool call 共享的渲染状态。
///
/// TS 的状态是 `{callComponent?}`（组件实例）；Rust 的 [`RenderState`] 只能存 JSON，
/// 因此这里保存预览与请求键，组件每帧重建。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRenderState {
    /// 当前预览（diff 或错误）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<EditPreview>,
    /// 预览对应的参数键（`JSON.stringify({path, edits})`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_args_key: Option<String>,
    /// 是否有预演请求在飞行中。
    #[serde(default)]
    pub preview_pending: bool,
    /// 结果是否已经以错误落定。
    #[serde(default)]
    pub settled_error: bool,
    /// `renderCall` 是否已经运行过（TS `state.callComponent` 是否已创建）。
    ///
    /// `renderResult` 只有在调用组件存在时才回写预览（TS 行为）；否则直接把结果 diff
    /// 渲染到结果区（例如只渲染结果的导出路径）。
    #[serde(default)]
    pub call_rendered: bool,
}

/// 取共享状态锁；中毒时取回内部值继续。
///
/// 渲染状态只是缓存：某次渲染 panic 不应让后续所有帧都无法渲染，因此不复用
/// `unwrap()` 的失败路径。
fn lock_state(state: &RenderState) -> std::sync::MutexGuard<'_, JsonValue> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 读取 `EditRenderState`；状态缺失或不是对象时返回默认值。
///
/// 反序列化失败（例如被旧版本写入了不兼容字段类型）同样回落到默认值：宁可丢掉预览，
/// 也不能让渲染回调 panic。`EditRenderState` 的字段都带 `serde(default)`，部分字段缺失
/// 的对象仍可正常解析。
fn read_edit_state(state: &RenderState) -> EditRenderState {
    let guard = lock_state(state);
    guard
        .get(EDIT_STATE_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// 写回 `EditRenderState`。
///
/// 共享状态可能被其它渲染器/旧版本占用成非对象（`RenderState` 只是 JSON），此时整体替换
/// 成新对象再写入 `"editRender"`；序列化失败时写 `Null`（正常不会发生，因为字段都可序列化）。
fn write_edit_state(state: &RenderState, edit_state: &EditRenderState) {
    let mut guard = lock_state(state);
    if !guard.is_object() {
        *guard = JsonValue::Object(serde_json::Map::new());
    }
    if let JsonValue::Object(object) = &mut *guard {
        object.insert(
            EDIT_STATE_KEY.to_string(),
            serde_json::to_value(edit_state).unwrap_or(JsonValue::Null),
        );
    }
}

/// `args?.file_path ?? args?.path` 的字符串形态（保留 null 与空串的区别）。
fn pick_path_value(args: Option<&JsonValue>) -> Option<&JsonValue> {
    let object = args.and_then(JsonValue::as_object)?;
    match object.get("file_path") {
        Some(value) if !value.is_null() => Some(value),
        _ => object.get("path"),
    }
}

/// `getRenderablePreviewInput(args)`：返回 `(path, edits)`。
///
/// 提取规则：
/// 1. 路径先取 `path`；它不是字符串时再取 `file_path`（注意与显示用的
///    `pick_path_value` 顺序相反）。两者都没有字符串形态时返回 `None`；`path` 是空串时
///    也直接返回 `None`，不会再去看 `file_path`（TS 的 `if (!path)`）。
/// 2. 优先解析 `edits` 数组：数组非空时要求每个元素都有字符串 `oldText`/`newText`；
///    任一元素不合规就整体放弃，继续尝试下面的单条形式。
/// 3. 最后尝试顶层的 `oldText`/`newText`（旧参数格式），两者都是字符串才生成一条
///    `Edit`。
///
/// 返回的 `None` 表示「当前参数还不足以预演」，调用方不应 spawn 后台任务。
fn get_renderable_preview_input(args: Option<&JsonValue>) -> Option<(String, Vec<Edit>)> {
    let args = args?;
    let path = match args.get("path") {
        Some(JsonValue::String(path)) => path.clone(),
        _ => match args.get("file_path") {
            Some(JsonValue::String(path)) => path.clone(),
            _ => return None,
        },
    };
    // TS: `if (!path) return null` —— 空字符串同样不生成预览。
    if path.is_empty() {
        return None;
    }

    if let Some(JsonValue::Array(items)) = args.get("edits")
        && !items.is_empty()
    {
        let parsed: Option<Vec<Edit>> = items
            .iter()
            .map(|item| {
                let object = item.as_object()?;
                let old_text = object.get("oldText")?.as_str()?;
                let new_text = object.get("newText")?.as_str()?;
                Some(Edit {
                    old_text: old_text.to_string(),
                    new_text: new_text.to_string(),
                })
            })
            .collect();
        if let Some(edits) = parsed {
            return Some((path, edits));
        }
    }

    let old_text = args.get("oldText").and_then(JsonValue::as_str)?;
    let new_text = args.get("newText").and_then(JsonValue::as_str)?;
    Some((
        path,
        vec![Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }],
    ))
}

/// `JSON.stringify({ path, edits })`。
///
/// 用作「预览是否对应当前参数」的指纹；路径与每处 oldText/newText 都参与。`None` 表示
/// 当前参数不可预览，此时也会与已存指纹比较，从而清掉旧预览。
fn preview_args_key(input: Option<&(String, Vec<Edit>)>) -> Option<String> {
    let (path, edits) = input?;
    let value = serde_json::json!({
        "path": path,
        "edits": edits,
    });
    serde_json::to_string(&value).ok()
}

/// `formatEditCall(args, theme, cwd)`：调用行 `edit {path}`。
///
/// 路径取 `file_path ?? path`（nullish coalescing：`file_path` 为 `null` 才回落到
/// `path`）；缺失/空串显示 `...`，非字符串显示 `[invalid arg]`，其余经
/// `render_tool_path` 做 `~` 压缩和超链接。
fn format_edit_call(args: Option<&JsonValue>, theme: &Theme, cwd: &str) -> String {
    let path_display = render_tool_path(str(pick_path_value(args)).as_deref(), theme, cwd, None);
    format!(
        "{} {path_display}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("edit"))
    )
}

/// 结果详情里的 `diff` / `firstChangedLine`。
///
/// details 不是对象、`diff` 不是字符串、`firstChangedLine` 不是 `u64` 时对应位置为
/// `None`；这里要求严格类型，不像 `super::details` 那样做 JS 宽松转换。非错误结果即使
/// 带 details 也可能两者皆空（例如结果来自旧版本）。
fn result_diff_and_line(result: &AgentToolResult<JsonValue>) -> (Option<String>, Option<u64>) {
    let Some(object) = result.details.as_object() else {
        return (None, None);
    };
    let diff = object
        .get("diff")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    let first_changed_line = object.get("firstChangedLine").and_then(JsonValue::as_u64);
    (diff, first_changed_line)
}

/// `formatEditResult(args, preview, result, theme, isError)`：结果区文本。
///
/// 返回 `None` 表示结果区什么都不画。三种情况：
/// 1. 错误结果：把文本内容块用 `'\n'` 拼接后染成错误色；文本为空、或与预览错误完全
///    相同（预览头部已经显示）时返回 `None`；
/// 2. 非错误结果：`details.diff` 非空且与预览 diff 不同时，用 `render_diff` 渲染并附带
///    `filePath` 供超链接；相同/为空时返回 `None`，避免同一 diff 出现两次；
/// 3. 其余情况返回 `None`。
///
/// 错误分支只拼接文本块，忽略图片块（`Image` 不计入 `error_text`）。
fn format_edit_result(
    args: Option<&JsonValue>,
    preview: Option<&EditPreview>,
    result: &AgentToolResult<JsonValue>,
    theme: &Theme,
    is_error: bool,
) -> Option<String> {
    let raw_path = str(pick_path_value(args));
    let preview_diff = preview
        .and_then(EditPreview::as_result)
        .map(|result| result.diff.clone());
    let preview_error = preview
        .and_then(EditPreview::as_error)
        .map(|error| error.error.clone());
    if is_error {
        let error_text = result
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                kiana_ai::types::UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if error_text.is_empty() || Some(&error_text) == preview_error.as_ref() {
            return None;
        }
        return Some(theme.fg(ThemeColor::Error, &error_text));
    }

    let (result_diff, _) = result_diff_and_line(result);
    // TS: `if (resultDiff && resultDiff !== previewDiff)` —— 空字符串是 falsy，不渲染。
    if let Some(result_diff) = result_diff
        && !result_diff.is_empty()
        && Some(&result_diff) != preview_diff.as_ref()
    {
        let options = kiana_ui_kit::diff::RenderDiffOptions {
            file_path: raw_path.clone(),
        };
        return Some(kiana_ui_kit::diff::render_diff(
            &result_diff,
            Some(&options),
        ));
    }

    None
}

/// `getEditHeaderBg(preview, settledError, theme)`。
///
/// TS 的闭包捕获调用方传入的 `theme`；Rust 的 [`StyleFn`] 必须 `'static` 而 [`Theme`]
/// 不可 Clone，因此先在**传入的主题**上算出背景 SGR 前缀，再按 `theme.bg(token, text)`
/// 的格式拼回（`Theme::bg` 的格式是 `{ansi}{text}\x1b[49m`）。
fn edit_header_bg(preview: Option<&EditPreview>, settled_error: bool, theme: &Theme) -> StyleFn {
    let token = match preview {
        Some(EditPreview::Error(_)) => ThemeBg::ToolErrorBg,
        Some(EditPreview::Result(_)) => ThemeBg::ToolSuccessBg,
        None => {
            if settled_error {
                ThemeBg::ToolErrorBg
            } else {
                ThemeBg::ToolPendingBg
            }
        }
    };
    let colored_empty = theme.bg(token, "");
    let prefix = colored_empty
        .strip_suffix("\x1b[49m")
        .map(str::to_string)
        .unwrap_or(colored_empty);
    Arc::new(move |text: &str| format!("{prefix}{text}\x1b[49m"))
}

/// `buildEditCallComponent(component, args, theme, cwd, outputPad)`。
///
/// 每次调用都新建 [`BoxComponent`]（paddingY = 1，paddingX = `output_pad`），因为
/// `RenderState` 不能保存组件实例。有预览时在调用行下追加 `Spacer` + 预览体：成功预览是
/// 无 `filePath` 的 diff，失败预览是错误色消息。背景由 [`edit_header_bg`] 决定，覆盖整个
/// 盒子（含 padding）。
fn build_edit_call_component(
    args: Option<&JsonValue>,
    preview: Option<&EditPreview>,
    settled_error: bool,
    theme: &Theme,
    cwd: &str,
    output_pad: usize,
) -> BoxComponent {
    let mut component =
        BoxComponent::new(1, 1, Some(edit_header_bg(preview, settled_error, theme)));
    component.set_padding_x(output_pad);
    component.clear();
    component.add_child(Box::new(Text::with_padding(
        format_edit_call(args, theme, cwd),
        0,
        0,
    )));

    let Some(preview) = preview else {
        return component;
    };
    let body = match preview {
        EditPreview::Error(error) => theme.fg(ThemeColor::Error, &error.error),
        EditPreview::Result(result) => kiana_ui_kit::diff::render_diff(&result.diff, None),
    };
    component.add_child(Box::new(Spacer::new()));
    component.add_child(Box::new(Text::with_padding(body, 0, 0)));
    component
}

/// `setEditPreview(component, preview, argsKey) -> changed`。
///
/// 写回预览并清掉 `preview_pending`；返回值表示「调用行需要重绘」。比较规则固定：
/// 两种错误只比 `error` 文本；两个成功结果只比 `diff` 与 `first_changed_line`；
/// 错误/成功之间切换恒视为变化。`preview_args_key` 一并更新为本次结果的参数指纹。
fn set_edit_preview(
    state: &mut EditRenderState,
    preview: EditPreview,
    args_key: Option<String>,
) -> bool {
    let changed = match &state.preview {
        None => true,
        Some(current) => match (current, &preview) {
            (EditPreview::Error(current), EditPreview::Error(next)) => current.error != next.error,
            (EditPreview::Result(current), EditPreview::Result(next)) => {
                current.diff != next.diff || current.first_changed_line != next.first_changed_line
            }
            _ => true,
        },
    };
    state.preview = Some(preview);
    state.preview_args_key = args_key;
    state.preview_pending = false;
    changed
}

/// 后台预演：在 tokio 运行时里 spawn；没有运行时则用独立线程 + current-thread runtime。
///
/// 完成回调不直接改调用方手里的 `EditRenderState` 副本，而是重新加锁读取共享状态，再比对
/// `preview_args_key`：参数可能在预演期间被改写，指纹不符就丢弃这次结果（避免把旧参数的
/// diff 画到新调用行上）。
///
/// 失败路径：拿不到 tokio 运行时时新建线程与 current-thread runtime；runtime 构建失败则
/// 什么都不写（预览停在 pending，调用行保持 pending 背景）。`invalidate` 由调用方提供，
/// 必须满足 `Send + Sync`，因为回调可能跑在 tokio worker 或这个独立线程上。
fn spawn_preview(
    path: String,
    edits: Vec<Edit>,
    cwd: String,
    state: RenderState,
    args_key: Option<String>,
    invalidate: Arc<dyn Fn() + Send + Sync>,
) {
    let finish = move |result: Result<EditDiffResult, EditDiffError>| {
        let mut edit_state = read_edit_state(&state);
        // 指纹不符说明参数已变：丢弃这次结果，不要覆盖新参数的预览/待定状态。
        if edit_state.preview_args_key == args_key {
            let preview = match result {
                Ok(result) => EditPreview::Result(result),
                Err(error) => EditPreview::Error(error),
            };
            set_edit_preview(&mut edit_state, preview, args_key.clone());
            write_edit_state(&state, &edit_state);
            drop(edit_state);
            invalidate();
        }
    };

    let future = async move {
        let result = compute_edits_diff(&path, &edits, &cwd).await;
        finish(result);
    };

    // 优先复用宿主已有的 tokio 运行时；导出/测试路径可能没有运行时，此时另起线程。
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(future),
                Err(_) => {
                    // 无法建立运行时时不产生预览（等价于预演失败于基础设施）。
                }
            }
        });
    }
}

/// `editRenderers`：`edit` 的自绘渲染器集合。
///
/// `render_shell` 为 [`kiana_ext_api::tool_types::RenderShell::SelfRender`]：调用行与结果区
/// 的框架、`output_pad` 都由本文件自己画。`render_call` 负责推进状态机并可能启动后台
/// 预演；`render_result` 负责把结果 diff 回写预览、在必要时于结果区补画 diff，并在状态
/// 变化时调用 `context.invalidate` 让下一帧的 `render_call` 重建调用行。
///
/// 调用方必须为同一个 tool call 复用同一份 `RenderState`（`ToolRenderContext.state`）；
/// 否则 `render_result` 读到的 `call_rendered` 恒为假，结果 diff 只会画在结果区，调用行
/// 永远不会显示 diff。
pub fn edit_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = Arc::new(|args, theme, context: &ToolRenderContext| {
        let mut edit_state = read_edit_state(&context.state);
        let mut state_changed = false;
        if !edit_state.call_rendered {
            // TS 在 `getEditCallRenderComponent` 里创建并记下调用组件。
            edit_state.call_rendered = true;
            state_changed = true;
        }
        let preview_input = get_renderable_preview_input(Some(args));
        let args_key = preview_args_key(preview_input.as_ref());

        // 参数指纹变化：旧预览、进行中的请求与错误落定状态一律作废。
        if edit_state.preview_args_key != args_key {
            edit_state.preview = None;
            edit_state.preview_args_key = args_key.clone();
            edit_state.preview_pending = false;
            edit_state.settled_error = false;
            state_changed = true;
        }
        if state_changed {
            write_edit_state(&context.state, &edit_state);
        }

        // 只在参数完整、可预览且没有结果/在飞请求时启动一次预演；预演完成会 invalidate。
        if context.args_complete
            && let Some((path, edits)) = &preview_input
            && edit_state.preview.is_none()
            && !edit_state.preview_pending
        {
            edit_state.preview_pending = true;
            write_edit_state(&context.state, &edit_state);
            spawn_preview(
                path.clone(),
                edits.clone(),
                context.cwd.to_string_lossy().into_owned(),
                Arc::clone(&context.state),
                args_key.clone(),
                Arc::clone(&context.invalidate),
            );
        }

        Box::new(build_edit_call_component(
            Some(args),
            edit_state.preview.as_ref(),
            edit_state.settled_error,
            theme,
            &context.cwd.to_string_lossy(),
            context.output_pad,
        )) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, _options, theme, context: &ToolRenderContext| {
            let mut edit_state = read_edit_state(&context.state);
            let preview_input = get_renderable_preview_input(Some(&context.args));
            let args_key = preview_args_key(preview_input.as_ref());
            // 错误结果即使 details 里带 diff 也不写回预览；只有成功结果能更新调用行。
            let (result_diff, first_changed_line) = if context.is_error {
                (None, None)
            } else {
                result_diff_and_line(result)
            };

            let mut changed = false;
            // TS 只在调用组件存在时回写预览（`if (callComponent)`）。
            let has_call_component = edit_state.call_rendered;
            if has_call_component {
                if let Some(result_diff) = result_diff {
                    changed |= set_edit_preview(
                        &mut edit_state,
                        EditPreview::Result(EditDiffResult::new(result_diff, first_changed_line)),
                        args_key,
                    );
                }
                if edit_state.settled_error != context.is_error {
                    edit_state.settled_error = context.is_error;
                    changed = true;
                }
                if changed {
                    write_edit_state(&context.state, &edit_state);
                    // TS 原地更新 call 组件；Rust 里请求重绘让 renderCall 用新状态重建。
                    (context.invalidate)();
                }
            }

            let preview = if has_call_component {
                edit_state.preview.as_ref()
            } else {
                None
            };
            let output = format_edit_result(
                Some(&context.args),
                preview,
                result,
                theme,
                context.is_error,
            );
            match output {
                Some(output) => {
                    let mut component = Container::new();
                    component.add_child(Box::new(Spacer::new()));
                    component.add_child(Box::new(Text::with_padding(
                        output,
                        context.output_pad,
                        0,
                    )));
                    Box::new(component) as Box<dyn Component>
                }
                None => Box::new(Container::new()) as Box<dyn Component>,
            }
        });

    ToolRenderers {
        render_shell: Some(kiana_ext_api::tool_types::RenderShell::SelfRender),
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}
