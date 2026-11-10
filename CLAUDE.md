# CLAUDE.md

Kiana 是编程代理（coding agent）的 Rust 实现，对标 TypeScript 参照实现做**行为级**重写。
workspace 含 32 个 crate、5 个二进制目标，edition 2024，`rust-version = "1.97"`。

## 先读哪份文档

| 需要什么 | 读 |
|---|---|
| 整体心智模型、仓库地图、阅读路线 | `docs/START_HERE.md` |
| crate 划分、依赖边、共享约定、决策记录 | `ARCHITECTURE.md`（§4 共享约定、§5 模块树） |
| 注释怎么写、什么算合格 | `docs/COMMENT_STYLE.md` |
| 新人常犯的理解错误 | `docs/COMMON_MISTAKES.md` |
| 某个移植单元的规范与差距 | `spec/*.md` |
| 一次任务的完整时序 | `docs/EXECUTION_FLOW.md` |
| 术语 | `docs/GLOSSARY.md` |

只在任务需要时读对应文档，不要一次性加载全部。

## 常用命令

```bash
cargo check -p <crate>                    # 单 crate 检查（优先；全量很慢）
cargo nextest run -p <crate>              # 单 crate 测试（首选；比 cargo test 快，见 .config/nextest.toml）
cargo build -p kiana-env && cargo nextest run --workspace --profile ci  # 全量验收（先建 daemon；已知时序 flake 重试两次，FLAKY 需确认）
cargo test -p <crate>                     # 单 crate 测试（无 nextest 时，或需要 doctest）
cargo test --workspace                    # 全量：约 500 个测试二进制，耗时且吃磁盘
cargo test --doc -p <crate>               # doctest（nextest 不覆盖）
cargo build --release -p kiana-coding-agent
cargo fmt --check -p <crate>              # 只校验；禁止用 cargo fmt 修改文件
cargo clippy -p <crate>
cargo insta review                        # 审阅 insta 快照
python3 scripts/check_comment_only_diff.py --baseline <dir>
python3 scripts/comment_audit.py
```

## 硬性约束

1. **行为保真是产品目标**：错误文案、事件顺序、JSON 键序、落盘格式、默认值、超时都要与
   TS 参照实现一致。改动行为前先读对应的 `spec/*.md` 与测试。
2. **`TS 源：packages/...` 注释不是遗留代码**，是保真溯源。不要删除或“清理”它。
3. **JSON 不要直接用 `serde_json` 序列化**。凡是写入磁盘、发给模型或进入协议流的 JSON，
   必须走 `kiana-json`（`stringify`、`format_js_number`、`sort_js_keys`），以复刻 JavaScript
   的数字格式、对象键序与转义行为。见 `ARCHITECTURE.md` §4.3。
4. **单线程事件循环**：主程序运行在 `tokio::task::LocalSet` 上，rquickjs 与部分会话状态是
   `!Send`。出现 `spawn_local`、`Rc`、`RefCell` 是设计使然，不要改造成多线程。
5. **取消用 `CancellationToken`**，沿调用链显式传递；不要用轮询或超时替代取消语义。
6. **注释/文档类任务**：只允许改注释与 Markdown，禁止运行 `cargo fmt` 改文件（只能 `--check`），
   禁止重排代码；用 `scripts/check_comment_only_diff.py` 验证确实只动了注释。

## 测试与快照

- 快照测试用 `insta`；待审快照是 `*.snap.new`（已 gitignore），`*.snap` 需要提交。
- `[profile.test]` 设为 `debug = 0`。全量测试的二进制体积巨大（曾占满 100GB 磁盘），
  不要随意改回默认 debuginfo。
- 优先跑单 crate 测试；跑全量前先确认磁盘余量。

## 依赖与工具

- 依赖版本在 `Cargo.toml` 全部精确 pin。**不要升级版本**；新增依赖需同步
  `ARCHITECTURE.md` 附录 A。
- LSP：rust-analyzer 可用（定义跳转、引用、hover、文档符号）。首次索引本 workspace 较慢。
