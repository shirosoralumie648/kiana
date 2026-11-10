# 第 7 周 handoff（wire agent 必读）

本单元没有修改 `crates/kiana-session/**`、根 `Cargo.toml`、`Cargo.lock`、`lib.rs`。
以下接线需要由 wire agent 在波次结束后统一应用。

## 0. 模块挂载现状（无需改动 lib.rs）

三个新模块通过 `crates/kiana-tools/src/index.rs` 的 `#[path]` 声明挂载：

```rust
#[path = "apply_patch.rs"]
pub mod apply_patch;
#[path = "multi_edit.rs"]
pub mod multi_edit;
#[path = "streaming_parser.rs"]
pub mod streaming_parser;
```

`lib.rs` 的 `pub use index::*;` 会把模块名再导出，因此下列路径都可用：

- `kiana_tools::apply_patch::{create_apply_patch_tool_definition, create_apply_patch_tool, ApplyPatchSettings, ApplyPatchToolOptions}`
- `kiana_tools::multi_edit::{plan_file_edits, EditError, EditStrategy, EditStrategyEvent, ...}`
- `kiana_tools::streaming_parser::{StreamingPatchParser, PreviewEvent, parse_patch_with_mode, ParseMode}`

如果后续把模块声明搬进 `lib.rs`，请删掉 `index.rs` 顶部的三组 `#[path]` 声明，避免重复定义。

## 1. `crates/kiana-config/src/settings/schema.rs`：增加 `tools` 设置

当前 `Settings` 没有 `tools` 字段，`ApplyPatchSettings::from_config` 无法从 `Settings`
直接取值。最小改动（保持 camelCase 线格式）：

```rust
// settings_object! { top_level ... pub struct Settings { ... } } 内追加
tools: ToolsSettings = "tools",

// 新增类型（放在 Settings 附近）
pub struct ToolsSettings {
    #[serde(rename = "applyPatch", default, skip_serializing_if = "Option::is_none")]
    pub apply_patch: Option<ApplyPatchModelsSetting>,
}

pub struct ApplyPatchModelsSetting {
    #[serde(default)]
    pub models: Vec<String>,
}
```

注意：`settings_object!` 宏同时生成 `SettingsInput` 与校验器；两者都要包含 `tools`，
否则 `SettingsInput` 会丢弃该键（见 `schema.rs` 末尾校验函数的注释）。

也可以接受 snake_case `apply_patch`：Kiana 的
`ApplyPatchSettings::from_config` 同时识别 `applyPatch` 与 `apply_patch`，但 settings
schema 只允许声明一种线格式，建议用 camelCase 与其它设置一致。

## 2. `crates/kiana-session/src/agent_session.rs`：按模型路由编辑工具

需要两个位置（当前都调用 `create_all_tool_definitions`）：

1. `Core::rebuild_builtin_tool_definitions`（约 line 3552）；
2. `Core::build_runtime`（约 line 7575）。

把：

```rust
create_all_tool_definitions(self.cwd.clone(), Some(self.builtin_tool_options()))
```

替换为：

```rust
let apply_patch_settings = ApplyPatchSettings {
    models: self
        .settings_manager
        .get_settings()
        .tools
        .apply_patch
        .map(|setting| setting.models)
        .unwrap_or_default(),
};
let apply_patch_options = ApplyPatchToolOptions {
    read_state: Some(Arc::clone(&self.read_state)),
    execution_policy: Some(self.execution_policy()),
    ..Default::default()
};
let model_id = self.current_model().map(|model| model.id);
create_all_tool_definitions_for_model(
    self.cwd.clone(),
    Some(self.builtin_tool_options()),
    model_id.as_deref(),
    &apply_patch_settings,
    Some(apply_patch_options),
)
```

`self.current_model()` 在 `Core` 上已有公开方法（`pub fn model(&self)` / `current_model()`
见 `agent_session.rs` 约 line 8278、7876）；如果没有 `current_model()`，用
`self.model().map(|model| model.id)`。构造后返回的 `IndexMap<ToolName, ToolDefinition>`
键序仍然是 8 个内置键：

- 未匹配 `tools.applyPatch.models`：`ToolName::Edit` → `edit` 定义；
- 匹配：`ToolName::Edit` → **name 为 `apply_patch`** 的定义。

**注意**：键仍是 `ToolName::Edit`（apply_patch 不在 `ToolName` 封闭枚举里），但
`ToolDefinition.name` 是 `"apply_patch"`。注册表/系统提示/扩展 runner 都按
`definition.name` 工作，因此行为正确；如果有代码按 `ToolName::Edit.as_str()` 反查
definition 名称，请改为读取 `definition.name`。

`builtin_tool_options()` 里已有的 read/write/edit/execution policy 保持不变；apply_patch
只额外需要 `read_state` 与 `execution_policy`（上面已传）。

### 刷新路径

`refresh_execution_policy_and_tools` 会走同一条 `rebuild_builtin_tool_definitions`，
替换后会自动使用最新策略；不需要新增调用点。

## 3. TUI：apply_patch 流式预览（可选但推荐）

模型流式生成工具参数时，把已到达的 `patch` 文本喂给
`kiana_tools::streaming_parser::StreamingPatchParser`：

```rust
let mut parser = StreamingPatchParser::new();
// 每次参数增量到达：
let events = parser.push_delta(&delta)?; // Vec<PreviewEvent>
// 渲染 events；未完成的行不会产生事件
// 工具调用结束时：
let hunks = parser.finish()?;
```

接线点建议在 `kiana-interactive` 的工具调用渲染/参数流处理里（本单元没有该文件所有权）。
事件语义：

- `StartedPatch`：显示 "Applying patch…"；
- `HunkStarted { kind, path }`：逐文件行（`add`/`delete`/`update`）；
- `MoveTo { path }`；
- `Line { kind, text }`：`+`/`-`/上下文行；
- `EndOfFile`、`Finished`。

`parser.hunks()` 可随时取已解析快照。解析错误（`ParseError`）只用于停止预览，不要
在参数流阶段报工具错误（最终执行会给出带行号的正式错误）。

## 4. SDK 导出（可选）

如果需要模型侧 SDK 暴露 apply_patch，可在 `kiana-coding-agent/src/sdk_exports.rs`
追加：

```rust
pub use kiana_tools::index::{
    create_apply_patch_tool as createApplyPatchTool,
    create_apply_patch_tool_definition as createApplyPatchToolDefinition,
};
```

`kiana-extensions/src/virtual_modules.rs` 若也要暴露，按 `createEditTool` 的
`tool_factory!` 模式添加 `createApplyPatchTool`。本单元没有这些文件的所有权。

## 5. 不需要接线但需要知道

- `crates/kiana-tools/Cargo.toml` 没有新增依赖（复用 `regex` / `unicode-normalization`
  之外的已有依赖；apply_patch 的 diff 复用 `edit_diff::generate_unified_patch`）。
- `crates/kiana-ext-api` 的 `EditToolDetails` 未改；`edit` 的 details 只是在原有
  `diff`/`patch`/`firstChangedLine` 后追加字段。渲染器按名读取，前向兼容。
- `create_all_tool_definitions` / `create_coding_tool_definitions` /
  `create_read_only_tool_definitions` 语义完全不变；golden 工具定义测试仍应通过。
- 若 wire agent 要在 `lib.rs` 统一声明模块，先删除 `index.rs` 的三组 `#[path]` 声明。
