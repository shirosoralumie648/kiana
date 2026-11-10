# 第 6 周 handoff（wire agent 必读）

本单元只修改了以下路径：

- `crates/kiana-compaction/**`
- `crates/kiana-session-core/**`
- `crates/kiana-core/src/**`
- `crates/kiana-tools/src/**`、`crates/kiana-tools/Cargo.toml`

**没有**修改 `crates/kiana-session/**`、根 `Cargo.toml`、`Cargo.lock`、`reference_project/**`。
以下接线需要 wire agent 在波次结束后统一应用。

## 0. 依赖与模块现状（无需改 Cargo.toml）

- `kiana-session` 已经依赖 `kiana-compaction`、`kiana-tools`、`kiana-core`、
  `kiana-session-core`；不需要新增依赖。
- 新模块路径：
  - `kiana_compaction::pruning::{PruneConfig, ProtectedToolMatcher, prune_tool_outputs,
    enforce_aggregate_tool_result_budget, PruneOutcome, PruneRecord, tool_result_text}`
  - `kiana_compaction::pipeline::{HistorySnapshot, CompactModelOptions, PipelineOptions,
    PipelineOutcome, run_compaction_pipeline, prepare_with_pruning,
    compact_with_structured_template, check_split_turn_pairing}`
  - `kiana_compaction::fallback::{FallbackConfig, build_fallback_result}`
  - `kiana_compaction::gate::{CompactionGate, GateDecision, AttemptOutcome, history_hash}`
  - `kiana_compaction::ptl::{plan_ptl_retries, drop_oldest_groups, OverflowRecovery}`
  - `kiana_compaction::recovery::{RecoveryInput, RecoveryBudget, CalledSkill,
    PlanRecovery, ContextDeltas, build_recovery_attachment}`
  - `kiana_core::artifact_store::{ArtifactStore, ArtifactHandle}`
  - `kiana_core::output_budget::{apply_output_budget, ToolOutputPolicy}`
  - `kiana_core::file_read_cache::{FileReadSnapshot, FileReadCache}`
  - `kiana_session_core::compaction_support::{HistorySnapshot, CommitTransaction,
    append_compaction_transactional, CompactionCommitError}`
  - `kiana_tools::{OutputBudgetOptions, wrap_agent_tool_with_output_budget,
    wrap_tool_definition_with_output_budget, RestoreOutcome}`
  - `kiana_tools::read::ReadStateRegistry::{snapshots, restore_snapshot}`

## 1. 创建会话级 ArtifactStore

文件：`crates/kiana-session/src/agent_session.rs`，`Core::new` / `Core::build_runtime`。

```rust
use std::sync::Arc;
use kiana_core::artifact_store::ArtifactStore;

// Core 里新增字段：
// artifact_store: std::sync::OnceLock<Arc<ArtifactStore>>,

fn artifact_store(&self) -> Option<Arc<ArtifactStore>> {
    if let Some(store) = self.artifact_store.get() {
        return Some(Arc::clone(store));
    }
    let (Some(dir), Some(session_id)) = (
        self.session_manager.get_session_dir().is_empty().then_some(None)
            .flatten()
            .or_else(|| Some(self.session_manager.get_session_dir())),
        Some(self.session_manager.get_session_id()),
    ) else {
        return None;
    };
    let store = ArtifactStore::new(dir, session_id).ok()?;
    let store = Arc::new(store);
    let _ = self.artifact_store.set(Arc::clone(&store));
    Some(store)
}
```

注意：
- `ReadonlySessionManager::get_session_dir()` 在内存会话里可能是空串；此时可以不创建
  store，工具输出预算退化为截断（`apply_output_budget` 的 `store = None` 分支）。
- 目录由 `ArtifactStore` 首次写入时创建（0700），不要提前 `create_dir_all`。
- `ArtifactStore` 是 `Sync`，用 `Arc` 共享；不要每条工具调用新建。

## 2. 给工具输出套预算（`build_runtime`）

文件：`crates/kiana-session/src/agent_session.rs`，`build_runtime` 的
`wrapped_builtin_tools` / `wrapped_extension_tools`（约 line 10699）之后。

```rust
use kiana_tools::{OutputBudgetOptions, wrap_agent_tool_with_output_budget};

let budget = self.artifact_store().map(|store| OutputBudgetOptions { store });
let apply_budget = |tool: kiana_agent_core::AgentTool| match &budget {
    Some(budget) => wrap_agent_tool_with_output_budget(tool, budget.clone()),
    None => tool,
};

let wrapped_builtin_tools = wrapped_builtin_tools.into_iter().map(apply_budget).collect::<Vec<_>>();
let wrapped_extension_tools = wrapped_extension_tools.into_iter().map(apply_budget).collect::<Vec<_>>();
```

注意：
- 必须在 `wrap_registered_tools` **之后**套，否则扩展包装会覆盖 `execute`。
- 同一工具只套一次；重复套会重复落盘/预览。
- 图片结果原样返回（预算层只处理纯文本）。

## 3. 压缩主流程（自动 + 手动）

文件：`crates/kiana-session/src/agent_session.rs`：

- `Core::run_auto_compaction`（约 line 5186）
- `Core::compact`（约 line 7262）
- `Core::run_default_compaction`（`compact(...)` 的实际调用点，约 line 4960）

### 3.1 选路：保留扩展 hook

扩展 `session_before_compact` 需要 `CompactionPreparation`，因此不要直接把整个函数
换成 `run_compaction_pipeline`；用 `prepare_with_pruning`：

```rust
use kiana_compaction::pipeline::prepare_with_pruning;
use kiana_compaction::pruning::PruneConfig;

let path_entries = self.branch();
let prune_config = PruneConfig::default()
    .with_plan_paths(self.active_plan_paths()); // 没有 plan 时传空 vec
let (pruned, preparation) = prepare_with_pruning(&path_entries, &settings, &prune_config);
let Some(preparation) = preparation else {
    self.compaction_gate.lock().record(history_hash, AttemptOutcome::NoOp, Instant::now());
    return Ok(false); // 或 AlreadyCompacted / NothingToCompact
};
```

- `pruned.entries` 只用于本次摘要请求；**不要**在摘要成功前写回会话。
- `pruned.applied == false` 时 entries 等于原历史。
- 把 `preparation` 继续传给现有的 `session_before_compact` 扩展逻辑；扩展取消时丢弃
  prune（不写 context_edit）。

### 3.2 生成摘要

默认路径（扩展没有提供 compaction 时）：

```rust
use kiana_compaction::pipeline::{CompactModelOptions, compact_with_structured_template};
use kiana_session_core::compaction_support::HistorySnapshot;

let summary_start = HistorySnapshot::capture(&self.session_manager);

let options = CompactModelOptions {
    model: model.clone(),
    api_key: /* 现有 run_default_compaction 的 api_key */,
    headers: /* ... */,
    custom_instructions: hook_instructions.clone(),
    signal: Some(AbortScope::from_token(token.clone())),
    thinking_level: self.thinking_level(),
    stream_fn: /* 现有 stream_fn */,
    env: None,
    retry: None,
    callbacks: None,
    session_id: Some(self.session_manager.get_session_id().to_string()),
};

match compact_with_structured_template(&preparation, &options).await {
    Ok(result) => { /* 3.3 提交 */ }
    Err(error) => {
        // all-or-nothing：不 append 任何东西，历史保持原样。
        if let Some(fallback) = kiana_compaction::fallback::build_fallback_result(
            &preparation,
            &FallbackConfig::default(),
        ) {
            let result = fallback.result; // 仍可提交
            /* 3.3 提交 */
        } else {
            self.compaction_gate.lock().record(
                history_hash, AttemptOutcome::Failed, Instant::now());
            return Err(AgentSessionError::from(error)); // 保留现有错误映射
        }
    }
}
```

也可以直接使用 `run_compaction_pipeline(&path_entries, &settings, &PipelineOptions {
prune, fallback: Some(FallbackConfig::default()), model: options })`，它在内部完成
prune + prepare + 摘要 + fallback；但扩展 hook 拿不到内部 preparation，因此只建议用于
没有扩展 runner 的路径。

### 3.3 提交（all-or-nothing，含 prune 的 context edit）

```rust
use kiana_session_core::compaction_support::{
    HistorySnapshot, CommitTransaction, append_compaction_transactional,
};
use kiana_session_core::session_manager::ContextEditReplacement;
use kiana_ai::types::JsonValue;

// 摘要在前一步已经成功；此时再检查历史是否在摘要期间被并发修改。
if !summary_start.matches(&self.session_manager) {
    return Err(/* ConcurrentModification */);
}

let mut transaction = CommitTransaction::begin(&self.session_manager);

// 1) 把 prune 的 blank 写成 context_edit（不重写文件、不改原条目）。
if pruned.applied {
    for record in &pruned.pruned {
        let edit_id = self.session_manager.append_context_edit(
            &record.entry_id,
            Some(ContextEditReplacement {
                content: JsonValue::String(record.notice.clone()),
            }),
        )?;
        transaction.record(&edit_id);
    }
}

// 2) 追加 compaction 条目。快照必须在 context_edit 之后重新捕获，
//    才能通过 `append_compaction_transactional` 的“未被并发修改”校验。
let after_edits = HistorySnapshot::capture(&self.session_manager);
let compaction_id = match append_compaction_transactional(
    &mut self.session_manager,
    &after_edits,
    &result.summary,
    Some(&result.first_kept_entry_id),
    result.tokens_before as f64,
    result.details.clone(),
    None,
    result.usage.clone(),
) {
    Ok(id) => {
        transaction.record(&id);
        id
    }
    Err(error) => {
        transaction.rollback(&mut self.session_manager);
        return Err(/* 映射 CompactionCommitError */);
    }
};
```

注意：
- **顺序必须是先摘要成功，再 append context_edit + compaction**。摘要失败时一个条目都
  不能写，否则违反“历史逐字节不变”。
- `summary_start` 快照用于检测“摘要期间”的并发修改；`after_edits` 用于
  `append_compaction_transactional` 的即时校验。两者用途不同，不能合并。
- `ContextEditReplacement.content` 为字符串时，`append_context_edit` 会自动转成
  toolResult 的 text block 数组；不要自己构造块数组。
- `CommitTransaction` 的 marker 必须在第一次 append 之前。
- 任一步 append 失败：`transaction.rollback(&mut self.session_manager)`（内存回滚）。
  磁盘半行风险见 `compaction_support` 模块文档。
- `estimated_tokens_after` 仍按现有逻辑在追加后重算并回填。

## 4. 压缩门控（no-op 缓存 + 5 分钟冷却）

文件：`Core` 新增字段：

```rust
compaction_gate: std::sync::Mutex<kiana_compaction::gate::CompactionGate>,
```

`run_auto_compaction` / `compact` 开头：

```rust
use kiana_compaction::gate::{AttemptOutcome, GateDecision};
use kiana_compaction::pipeline::HistorySnapshot;

let history = HistorySnapshot::capture(&path_entries)?;
let decision = self.compaction_gate.lock().check(history.hash(), std::time::Instant::now());
match decision {
    GateDecision::Proceed => {}
    GateDecision::NoOpCached { .. } | GateDecision::Cooldown { .. } => return Ok(false),
    GateDecision::Disabled => return Ok(false),
}
```

结束：

- 成功 / fallback 成功：`record(hash, AttemptOutcome::Succeeded, now)`；
- 无内容 / 收益不足：`record(hash, AttemptOutcome::NoOp, now)`；
- 摘要失败且 fallback 不可用：`record(hash, AttemptOutcome::Failed, now)`。

注意：手动压缩应忽略 no-op 缓存（用户显式要求），可以只使用冷却或直接重置 gate。

## 5. PTL / provider overflow

`AgentSession` 已有 `overflow_recovery_attempted`：第一次 compact+retry，第二次明确
报错。**不要重复实现一遍**。可选增强：

- 在重试请求的 message 副本上，先尝试 `plan_ptl_retries(&messages)`（最多 3 个计划，
  每个计划丢最旧一组）；某个计划成功就继续，不要修改持久化历史。
- 仍未成功时再走现有 compact+retry；第二次 overflow 仍由现有
  `overflow_recovery_attempted` 返回明确错误。
- 保证 `drop_oldest_groups` 不会把 system 消息或最后一个 group 丢掉。

## 6. 压缩后恢复（hidden context）

### 6.1 压缩前快照

```rust
use kiana_tools::read::ReadStateRegistry;

// Core 里持有实际 read state 的 Arc；如果工具用的是进程级 registry，用
// ReadStateRegistry::global()。
let file_snapshots = read_state.snapshots(5);
```

### 6.2 压缩成功后恢复

```rust
use kiana_compaction::recovery::{
    RecoveryInput, RecoveryBudget, build_recovery_attachment, collect_called_skills,
    collect_context_deltas,
};

let attachment = build_recovery_attachment(
    &RecoveryInput {
        files: file_snapshots,
        // 从本分支条目里收集已调用技能（skill / skill_read / skill:// read）；
        // 上层如果已有更精确的技能记录，也可以直接传 Vec<CalledSkill>。
        skills: collect_called_skills(&path_entries, 20),
        plan: self.plan_recovery_state(),        // PlanRecovery，没有就 Default
        // 从条目收集工具/agent/MCP 增量；上层有更精确的运行时增量时自行覆盖。
        deltas: collect_context_deltas(&path_entries),
    },
    &RecoveryBudget::default(),
);

for warning in &attachment.warnings {
    self.emit(AgentSessionEvent::Notice { /* warning */ });
}
for file in &attachment.restored_files {
    let snapshot = kiana_core::file_read_cache::FileReadSnapshot {
        path: file.path.clone(),
        content_hash: file.content_hash,
        line_count: file.line_count,
        ranges: file.ranges.clone(),
        text: file.text,
        captured_at_ms: 0,
        content_excerpt: String::new(),
    };
    let _ = read_state.restore_snapshot(&snapshot);
}
// 把 attachment.text 作为 hidden/system attachment 注入下一次请求；
// 不要生成 toolResult 消息。
```

更简单的做法：保留 6.1 的完整 `Vec<FileReadSnapshot>`，恢复时直接对原始快照调用
`restore_snapshot`（`RestoredFile` 只是预算内的子集 + 路径/哈希）。

注意：
- `attachment.text` 是 system/hidden context，不是工具结果；不要伪造
  `ToolResultMessage`。
- 缺失内容只 warning，不阻塞压缩。
- 未恢复的路径继续被 read gate 拒绝，直到模型重新 `read`（期望行为）。
- 恢复会追加尾部 token；如果 provider 对 prefix cache 敏感，可只恢复文件 read state、
  不注入完整 excerpt（`RecoveryBudget { max_file_chars: 0, .. }`）。

### 6.3 聚合 tool-result 预算

在每次构建请求前（`convert_to_llm` 之前）：

```rust
use kiana_compaction::pruning::{enforce_aggregate_tool_result_budget, PruneConfig};

let outcome = enforce_aggregate_tool_result_budget(
    &projected_entries,
    50_000, // token 上限；按模型窗口调整
    self.artifact_store().as_deref(),
    &PruneConfig::default().with_plan_paths(self.active_plan_paths()),
);
// 与 prune 一样，用 append_context_edit 把 outcome.entries 中变化的条目持久化：
for record in &outcome.cleared {
    let edit_id = self.session_manager.append_context_edit(
        &record.entry_id,
        Some(ContextEditReplacement {
            content: JsonValue::String(record.notice.clone()),
        }),
    )?;
}
// 或只在本次请求的副本上生效。
```

注意：
- 清除只改 content，不删消息、不拆 tool pair。
- 占位符 `[Old tool result content cleared; artifact: <path>]` 包含稳定路径；压缩后
  仍能解析到同一 artifact。
- 没有 artifact store 时使用 `[Old tool result content cleared]`。

## 7. 注意事项汇总

1. **任何 context_edit / compaction append 都必须发生在摘要成功之后**；失败路径不能
   改变历史（自动测试 `pipeline.rs` 锁定）。
2. `CommitTransaction` 只回滚内存；磁盘半行无法回滚。落盘失败后应停止继续写该会话。
3. `ContextEditEntry` 是追加式投影编辑，不改原条目；这与“原地 blank”在模型上下文
   上的效果一致，同时保持 append-only 会话文件。
4. `protected_tools` 的 active plan 路径需要调用方传入；默认只有 skill/artifact 保护。
5. `CompactionGate` 的 `Instant` 由调用方传入；测试不需要 sleep。
6. 结构化模板只在新函数 `compact_with_structured_template` 中生效；旧 `compact`
   保持 TS 冻结 prompt。
7. `ArtifactStore::new` 的 `session_id` 必须是单段路径；`SessionManager` 的合法
   session id 已满足。
8. 工具输出预算 wrapper 是执行后的纯文本层：`details` 不是对象时不会写入
   `artifact` 元数据（结果本身仍然预算化）。
