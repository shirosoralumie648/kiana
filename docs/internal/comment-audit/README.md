# 全仓注释审计清单

本清单扫描 Rust 注释、Markdown 文档与 Cargo 注释中与 TypeScript 参照实现相关的表述，用于清理“TS 保真”这一已过时的前提。

- 扫描文件：2126
- 命中行：34196
- 完整逐行清单：[`scan.csv`](scan.csv)
- 可直接处理：A 类 [`scan-A.csv`](scan-A.csv)、D 类 [`scan-D.csv`](scan-D.csv)

## 分类口径

| 类别 | 含义 | 建议动作 |
|---|---|---|
| A | 把“与 TS 一致/逐字一致/保真”当作硬约束 | 改写为当前设计约束，或删除；不要再用 TS 作为理由 |
| B | 提到 TS 的行为/格式/顺序，但主要是功能说明 | 随对应代码替换时更新；保留可验证的技术原因 |
| C | 仅记录 TS 出处 | 保留，降级为“历史来源（非约束）” |
| D | 有“必须一致/逐字”等强约束，但没有明确提到 TS | 人工判断是否仍有独立技术原因 |

## 总量

| 类别 | 行数 |
|---|---:|
| A | 89 |
| B | 18868 |
| C | 14660 |
| D | 579 |

| 区域 | 行数 |
|---|---:|
| crates | 23319 |
| spec | 9716 |
| root | 853 |
| docs | 308 |

## A 类：按 crate

| crate | A 类行数 |
|---|---:|
| `kiana-ui-kit` | 2 |
| `kiana-session` | 1 |

## A 类：按文件（前 30）

| 文件 | A 类行数 |
|---|---:|
| `spec/ca-tools-compaction-export.md` | 9 |
| `spec/durable-harness.md` | 6 |
| `spec/ca-interactive-mode.md` | 5 |
| `spec/ai-api-other.md` | 4 |
| `spec/ai-core.md` | 4 |
| `spec/ca-interactive-components.md` | 4 |
| `spec/ca-resources.md` | 4 |
| `spec/ca-session.md` | 3 |
| `spec/chord-rest.md` | 3 |
| `spec/design-extension-runtime.md` | 3 |
| `spec/tui-core.md` | 3 |
| `docs/ARCHITECTURE_OVERVIEW.md` | 2 |
| `docs/COMMENT_STYLE.md` | 2 |
| `docs/EXECUTION_FLOW.md` | 2 |
| `spec/GAPS.md` | 2 |
| `spec/ai-api-anthropic-openai.md` | 2 |
| `spec/ai-auth.md` | 2 |
| `spec/ca-rpc-print.md` | 2 |
| `spec/chord-core.md` | 2 |
| `spec/design-extension-runtime-risk-first.md` | 2 |
| `spec/durable-rest.md` | 2 |
| `spec/env-evals-durable-testing.md` | 2 |
| `spec/tui-components.md` | 2 |
| `spec/tui-text.md` | 2 |
| `CLAUDE.md` | 1 |
| `crates/kiana-session/src/background/tool.rs` | 1 |
| `crates/kiana-ui-kit/src/diff.rs` | 1 |
| `crates/kiana-ui-kit/src/visual_truncate.rs` | 1 |
| `docs/DEBUGGING_GUIDE.md` | 1 |
| `docs/START_HERE.md` | 1 |

## 建议清理批次

1. **中央文档与规范**：`ARCHITECTURE.md`、`README.md`、`docs/START_HERE.md`、`docs/EXECUTION_FLOW.md`、`spec/*.md`。先把“行为完全一致”改成“历史移植目标，已废弃”。
2. **正在改的 crate**：`kiana-permissions`、`kiana-sandbox`、`kiana-session`、`kiana-tools`、`kiana-compaction`。随权限/沙箱/工具替换一起清理。
3. **高密度 crate**：A 类行数最多的 crate 分批处理；每批只改注释，不夹带代码改动，便于 review。
4. **测试与黄金文件**：`tests/`、`benches/` 里的 TS 断言先保留；等对应行为真正替换时，再改测试名和快照。
5. **历史来源**：C 类不删；统一标为“来源（历史，非约束）”，必要时迁移到 `docs/PROVENANCE.md`。

## A 类高优先级样本（前 40 条）

| 位置 | 摘要 |
|---|---|
| `CLAUDE.md:41` | 2. **`TS 源：packages/...` 注释不是遗留代码**，是保真溯源。不要删除或“清理”它。 |
| `crates/kiana-session/src/background/tool.rs:39` | // 只有 continuity 开启时才把 `background` 加进工具声明：基础 bash schema 必须与 TS |
| `crates/kiana-ui-kit/src/diff.rs:142` | /// `parseDiffLine` 的结果（字段名与上游实现逐字对应）。 |
| `crates/kiana-ui-kit/src/visual_truncate.rs:106` | /// `VisualLinePreviewOptions`（字段与上游接口逐字对应）。 |
| `docs/ARCHITECTURE_OVERVIEW.md:211` | 6. **Photon 用原生 `photon-rs`**：与 TS 的 wasm 构建同源，JPEG 逐字节一致、PNG 允许末位差异。 |
| `docs/ARCHITECTURE_OVERVIEW.md:239` | - 源码里的 `TS 源：...` 注释是保真溯源，不是过时信息；修改行为前先查对应 spec 与测试。 |
| `docs/COMMENT_STYLE.md:44` | 否则保留原有精确的 TS 对应关系、字节级保真说明等内容，在其上补充教学式解释。 |
| `docs/COMMENT_STYLE.md:203` | - 用“优化性能”“线程安全”“等价于 TS”作为没有依据的结论。 |
| `docs/DEBUGGING_GUIDE.md:103` | 错误文本必须与 TS 的 `error.message` 一致。若错误文本“看起来像 reqwest 的”， |
| `docs/EXECUTION_FLOW.md:209` | - 这两条路径保证“用户可以在任务中途插话”，且消息顺序与 TS 一致。 |
| `docs/EXECUTION_FLOW.md:231` | `AssistantMessageEvent`；错误文本与事件顺序必须与 TS 对齐。 |
| `docs/START_HERE.md:26` | `TS 源：packages/...` 这样的溯源注释——它们是保真约束，不是冗余信息。 |
| `docs/UNDERSTANDING_GUIDE.md:170` | - **职责**：把会话快照导出成与 TS 原版逐字节一致的单文件自包含 HTML（模板/CSS/JS 内嵌）。 |
| `docs/history/porting/fix-passes/pass-6-phases-11b-12b.md:48` | `kiana-tools` 的 bash schema 保持与 TS 黄金 fixture 逐字节一致；`background` 属性只在 |
| `docs/history/porting/plan.md:5` | > 前提：与旧 TypeScript 参照实现“行为完全一致”的要求已经作废。Kiana 可以直接换掉不好的设计，只保留还有用户价值的历史会话/配置格式。 |
| `spec/GAPS.md:130` | 同时确认 §4.9 的 `HttpClient` trait 是否需要 `reqwest::Error` 的转换（错误文本要与 TS 一致， |
| `spec/GAPS.md:572` | 16. **`crates/kiana-env-client/Cargo.toml`**：description 与 TS `package.json` 逐字一致（`spec/env-evals-durable-testing.md` §2.2.1）。 |
| `spec/PROGRESS.md:73` | \| gate:m1-verification \| all \| verified \| M1 verifier pass 2026-10-30: `cargo fmt --all --check`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --w… |
| `spec/agent-codemode.md:161` | 约定：签名用 TypeScript 原文；行为按实现顺序；所有字符串字面量逐字给出（Rust 必须逐字节一致）； |
| `spec/ai-api-anthropic-openai.md:4455` | ### 5.1 本单元消费的类型（必须由上游单元提供，逐字一致） |
| `spec/ai-api-anthropic-openai.md:5516` | 序列化时必须**省略 `None`** 且键序与 TS 一致（`model, messages, max_tokens, stream, betas, system, |
| `spec/ai-api-other.md:5` | > 环境变量、超时/重试语义全部与 TypeScript 实现**逐字节一致**。 |
| `spec/ai-api-other.md:3080` | `mistral-conversations.ts` 自己有一份**逐字相同**的私有 `safeJsonStringify`（不是从 utils import）。 |
| `spec/ai-api-other.md:3880` | │ │ │ │ └── registry.rs # 全部 *.lazy.ts 的等价物 |
| `spec/ai-api-other.md:4588` | Rust 移植后这些字符串**无法**与 TS 版完全一致，规格应明确它们属于"尽力对齐"而非"逐字节"： |
| `spec/ai-auth.md:136` | 约定：签名用 TypeScript 原文；"行为"按实现顺序描述；所有字符串字面量逐字给出（Rust 必须完全一致）。 |
| `spec/ai-auth.md:2882` | - `AuthPrompt`/`AuthEvent` 的字段名与 TS 完全一致（`intervalSeconds`、`expiresInSeconds`、`verificationUri`、 |
| `spec/ai-core.md:128` | 约定：签名用 TypeScript 原文；"行为"按实现顺序描述；所有字符串字面量逐字给出（Rust 需完全一致）。 |
| `spec/ai-core.md:2018` | #### 2.3.3 每个 `<provider>.models.ts` 分片（42 个文件，各 **14** 行，模板完全一致） |
| `spec/ai-core.md:2397` | 等价校验可作为 CI 门禁。**但生成的 JSON 数据（`src/providers/data/*.json`）必须与 TS 生成器逐字节一致**， |
| `spec/ai-core.md:2431` | - `readModelDataStructure`：provider id 与 `providers/*.models.ts` 文件名集合必须一致（否则 |
| `spec/ca-builtin-extensions.md:7283` | - `proper-lockfile` 的目录锁与 mtime 心跳必须与 TS 版互操作（同一 `~/.kiana/agent`）； |
| `spec/ca-cli.md:2627` | 对 `args.ts` 模板字面量与 §2.5.3 代码块做机械 diff（把 `${APP_NAME}`→`kiana`、`${CONFIG_DIR_NAME}`→`.kiana`、两个 `padEnd(32)` 的环境变量行展开、去掉 chalk 包装、去掉 `${extensionFlagsText}` 占位），**其余 195 行逐字节一致**。唯一差异是 §2.5.3 用 `<扩展段>` 标注插入点。chalk.bold… |
| `spec/ca-config-models.md:4173` | **与其它 crate 的边界**：`kiana-ai`（`Models`/`Provider`/`Model`/`Usage`/`calculateCost`/`lazy_stream`/`ModelsError`）、`kiana_ai::auth`（**不是独立 crate**；`Credential`/`CredentialStore`/`ApiKeyAuth`/`OAuthAuth`，`ARCHITECTURE.md` §… |
| `spec/ca-interactive-components.md:5351` | 逐字符比对（Rust 必须与 TS 完全一致，含 `Float32` 舍入）。见 §8.3。 |
| `spec/ca-interactive-components.md:9634` | - `strip_ansi(s)` 与 `visible_width(s)` 必须与 TS 完全一致（用于断言）。 |
| `spec/ca-interactive-components.md:9794` | 这样 `async` 回调可以直接改状态，与 TS 一致。 |
| `spec/ca-interactive-components.md:9811` | #### ★★★★★ 1. `easter-egg-3d.ts`（1292 行）的逐位一致 |
| `spec/ca-interactive-mode.md:4779` | > `THINKING_DESCRIPTIONS` 与 §2.40 `thinking-selector.ts` 的 `LEVEL_DESCRIPTIONS` **内容完全相同**（两处独立定义，移植时可共享一个常量，但字符串必须逐字一致）。 |
| `spec/ca-interactive-mode.md:6084` | \| `tui-core` \| `TUI` trait（`request_render`、`set_focus`、`get_width`、`get_height`、`set_program_status`、`add_overlay`…）、`TuiMainScreen`、`TuiAltScreen`、`Component` trait（`render(width) -> Vec<String>`、`handle_input(&str)… |

## D 类样本（前 20 条）

| 位置 | 摘要 |
|---|---|
| `ARCHITECTURE.md:440` | `[kiana] Failed to load extension ...`），是用户可见的输出契约，需逐字保留；实现方式： |
| `ARCHITECTURE.md:520` | 当测试需要访问私有项时）。`describe`/`it` 的**标题逐字保留**（英文原文），因为失败报告需要与历史测试名对照。 |
| `ARCHITECTURE.md:1963` | `HttpError::message` 是用户可见契约，使用 `error.message` 的逐字文本（reqwest 的错误文本要转写， |
| `CLAUDE.md:39` | 1. **行为保真是产品目标**：错误文案、事件顺序、JSON 键序、落盘格式、默认值、超时都要与 |
| `crates/kiana-agent-core/Cargo.toml:14` | # （差 1 ULP，会把该值变成 `0.00024375`），开启后与 JS `JSON.parse` 的位模式逐位一致。 |
| `crates/kiana-agent-core/src/agent.rs:1720` | /// 检查抛 `Agent is already processing.`，但正常路径上各入口的同步检查先抛各自逐字文本； |
| `crates/kiana-agent-core/src/loop_guard.rs:16` | //! \| [`LoopDetectorKind::ShellStagnation`] \| 同一 shell 命令且输出逐字相同 \| 2 / 4 \| |
| `crates/kiana-agent-core/src/loop_guard.rs:694` | // 连续段里输出必须逐字相同；只要有一次变化就视为有进展。 |
| `crates/kiana-agent-core/src/types.rs:80` | /// `details`/`structuredContent`）都经过 `JSON.stringify`，落盘与上屏的字节必须一致 |
| `crates/kiana-agent-core/src/wire_keys.rs:148` | /// 分支在当前仓库不可达，本函数不会产生上述包装对象；远端 JSON 的保真（未知字段与插入序） |
| `crates/kiana-agent-core/tests/util/calculate.rs:577` | // 错误消息只要求非空，不要求与其他实现的文本逐字相同； |
| `crates/kiana-ai/Cargo.toml:42` | # 5 万个样本里约 10% 差 1-2 ulp；libm（musl/fdlibm 系）与 V8 逐位一致，才能对上测试 |
| `crates/kiana-ai/build.rs:128` | // manifest 的键集合必须与目录里的数据文件集合完全一致（多一个少一个都报错）。 |
| `crates/kiana-ai/src/api/anthropic_messages.rs:54` | //! - **字节级保真**：`build_params` 中字段的插入顺序是当前稳定契约（JSON 序列化顺序 |
| `crates/kiana-ai/src/api/azure_openai_responses.rs:571` | /// `thrown_value` 只有在快照的消息与 `error.message` 完全一致时才采用它来还原状态码与 |
| `crates/kiana-ai/src/api/bedrock_converse_stream.rs:2229` | /// 文本逐字保留（含前导换行、双空格缩进与 `{error}` 字面量），因为它会出现在 |
| `crates/kiana-ai/src/api/bedrock_converse_stream.rs:2293` | /// - 任何 JSON.parse 失败 → `SyntaxError`（`js_parse` 给出与 V8 逐字一致的文案）。 |
| `crates/kiana-ai/src/api/cloudflare.rs:45` | //! [`HttpClient`] 时完全一致。 |
| `crates/kiana-ai/src/api/constrained_sampling.rs:65` | //! 中断言，不在 fixture 里。完整逐字文本以 `spec/ai-api-anthropic-openai.md` |
| `crates/kiana-ai/src/api/constrained_sampling.rs:803` | /// 消息逐字保留。其他非对象 `variants`（字符串 / 数字 / 数组）的属性访问不抛错，视作未设置。 |

## 使用方式

```bash
# 只看 A 类
awk -F, 'NR==1 || $1=="A"' docs/internal/comment-audit/scan.csv
# 只看某个 crate
awk -F, 'NR==1 || $4=="kiana-tui"' docs/internal/comment-audit/scan.csv
# 只看测试
awk -F, 'NR==1 || $8=="yes"' docs/internal/comment-audit/scan.csv
```

> 注意：这是启发式扫描，不做完整 Rust 词法分析；字符串里的 `//`、URL、Markdown 代码块可能造成少量误报。A 类仍建议逐条人工确认。
