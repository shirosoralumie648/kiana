# 按周并行移植 — 脚手架记录

日期：2026-11-07

本文件只记录**脚手架**（新 crate 的空壳 + workspace 接线），不包含 hooks / memory
的任何业务实现。

## 创建了什么

### 1. `crates/kiana-hooks/`

- `Cargo.toml`
  - `name = "kiana-hooks"`；
  - `version.workspace = true`、`edition.workspace = true`、
    `rust-version.workspace = true`、`license.workspace = true`、`publish = false`
    （与 `crates/kiana-permissions/Cargo.toml` 的头部形状一致）；
  - 暂无 `[dependencies]`。
- `src/lib.rs`
  - crate 级文档注释：说明职责只有「解析 hooks 配置、编译 matcher、派发 handler、
    执行退出码 / JSON 契约」，以及当前处于脚手架阶段；
  - `pub struct HooksConfig;`（`Debug, Default, Clone, Copy, PartialEq, Eq`），
    占位类型，无任何逻辑。

### 2. `crates/kiana-memory/`

- `Cargo.toml`
  - `name = "kiana-memory"`，同样继承 workspace 元数据；
  - 暂无 `[dependencies]`。
- `src/lib.rs`
  - crate 级文档注释：说明跨会话记忆的存储、召回、抽取、整合、脱敏与遗忘职责；
  - `pub struct MemoryConfig;`（`Debug, Default, Clone, Copy, PartialEq, Eq`），
    占位类型，无任何逻辑。

### 3. 根 `Cargo.toml`

- `[workspace] members` 追加：
  - `crates/kiana-hooks`
  - `crates/kiana-memory`
- `[workspace.dependencies]` 追加：
  - `kiana-hooks = { path = "crates/kiana-hooks" }`
  - `kiana-memory = { path = "crates/kiana-memory" }`

未改动其它 crate，未改动 `docs/history/porting/plan.md`，未新建其它 crate。

### 4. `docs/history/porting/handoffs/`

已创建空目录，供各周任务写交接记录。

## 真实 check 结果

在 `/home/ctyun/code/kiana` 下实际执行：

```text
$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo check -p kiana-hooks -p kiana-memory
cargo check (2 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s
EXIT=0
```

结论：两个空壳 crate 均可编译，workspace 接线有效。`Cargo.lock` 已由 cargo 自动
更新以纳入两个新包。

## 后续说明

- 两个 crate 目前没有任何依赖；等实现开始时再按 `docs/history/porting/plan.md` 的依赖
  方向补 `[dependencies]`。
- `HooksConfig` / `MemoryConfig` 只是占位，字段与派生可按实现需要修改。
