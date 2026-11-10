# kiana 架构（TypeScript → Rust 全量重写）

本文是 `/home/ctyun/code/kiana_rev/kiana`（TypeScript，11 个包、252+202 个源文件、约 19 万行 src）
到 `/home/ctyun/code/kiana_rev/kiana`（Cargo workspace，32 个 crate）的架构定稿。

- 目标：**以参考项目的最佳设计和实际效果为准**——对齐公开 API 语义、错误消息、事件顺序、
  线格式、落盘格式、默认值与超时、确定性指纹；历史 TS 移植只作参考，参考行为不是最佳设计时以当前设计为准。
- 每个单元的详细移植规范在 `spec/*.md`（28 份单元规范，含 1 份运行时设计定稿；另有 `spec/GAPS.md`
  记录阶段 1 审计出的未决项）；本文只做**跨单元**决策：
  crate 划分、依赖方向、共享约定、模块树、二进制、实施顺序与所有权边界。
- 骨架期（2026-10-26）：30 个 crate 全部可 `cargo check --workspace --all-targets`（0 warning）、
  `cargo clippy --workspace --all-targets` 无问题、`cargo fmt --check` 通过；
  当前为 32 个 crate（新增 `kiana-hooks`、`kiana-memory`），workspace 全量回归由各波
  wire agent 统一执行；跨 crate 的核心类型已在骨架里完整定义（见 §4.9 的清单），
  后续并行实现按这些类型编码。

阅读顺序建议：先 §1–§2（谁依赖谁），再 §3（为什么这样拆），实现时查 §4（约定）与 §5（自己 crate
的模块树 + 每文件对应的 TS 源），最后 §7（阶段与文件所有权）。

---

## 1. TypeScript 包 → Rust crate 映射

一个 TS 包对应一个 crate；**唯一**例外是 `packages/coding-agent`（252 个源文件、76,372 行）——
它的 10 份单元规范本身就把内部分成了依赖方向清晰的 16 个子系统（见下表 16 行），合成一个 crate 会让 25 个以上
实现者改同一批文件。拆分的依据是 §3.1 的 SCC 分析，不是主观分组。

| TS 包 / 单元 | Rust crate | 规格 |
|---|---|---|
| `packages/telemetry`（8 文件 / 1,572 行） | `kiana-telemetry` | `telemetry-mcp.md` |
| `packages/mcp`（18 / 3,287） | `kiana-mcp` | `telemetry-mcp.md` |
| `packages/ai`（202 / 28,507） | `kiana-ai` | `ai-core.md`、`ai-auth.md`、`ai-api-anthropic-openai.md`、`ai-api-other.md` |
| `packages/agent`（6 / 2,541） | `kiana-agent-core` | `agent-codemode.md` |
| `packages/codemode`（16 / 3,018） | `kiana-codemode` | `agent-codemode.md` |
| `packages/tui`（46 / 19,674） | `kiana-tui` | `tui-core.md`、`tui-text.md`、`tui-components.md` |
| `packages/chord`（41 / 15,193） | `kiana-chord` | `chord-core.md`、`chord-rest.md` |
| `packages/durable`（84 / 27,695） | `kiana-durable` | `durable-harness.md`、`durable-rest.md` |
| `packages/env/daemon`（16 个 `.rs` / 4,998） | `kiana-env`（bin） | `env-evals-durable-testing.md` |
| `packages/env/src`（8 / 2,346） | `kiana-env-client` | `env-evals-durable-testing.md` |
| `packages/evals/src`（5 / 1,446） | `kiana-evals`（bin） | `env-evals-durable-testing.md` |
| `packages/coding-agent`（252 / 76,372） | 16 个 crate（见下表） | `ca-*.md` 共 10 份 |
| （无 TS 对应，新增） | `kiana-json` | 本文 §3.2 |
| （无 TS 对应，新增） | `kiana-permissions` | 本 crate 模块文档 |
| （无 TS 对应，新增） | `kiana-sandbox` | 本 crate 模块文档 |
| （无 TS 对应，新增） | `kiana-hooks` | `docs/history/porting/plan.md` §五 |
| （无 TS 对应，新增） | `kiana-memory` | `docs/history/porting/plan.md` §五 |

`packages/coding-agent/src` 的 16 个 crate 与 TS 子树的对应（行数为实际统计）：

| crate | TS 源 | 文件 / 行 |
|---|---|---|
| `kiana-core` | `config.ts` + `utils/**` + `core/{event-bus,exec,timings,diagnostics,source-info,kiana-manifest,output-guard,http-dispatcher,slash-commands,mcp-servers}.ts` | 47 / 5,360 |
| `kiana-session-core` | `core/{messages,session-manager,session-cwd,session-export,usage-totals,cache-stats,virtual-models,nested-tool-calls}.ts` | 8 / 3,092 |
| `kiana-config` | `core/{settings-*,provider-composer,model-*,auth-storage,models-store,runtime-credentials,auth-guidance,cache-warmer,keybindings*,key-id-schema,resolve-config-value,trust-manager,remote-catalog-provider,client-identity,telemetry,project-trust,radius,experimental,defaults}.ts` | 26 / 7,465 |
| `kiana-theme` | `modes/interactive/theme/*.ts`（`utils/{syntax-highlight,fs-watch}.ts` 的实现归 `kiana-core`，本 crate 只调用） | 5 / 2,261 |
| `kiana-compaction` | `core/compaction/*.ts` | 4 / 1,654 |
| `kiana-export` | `core/export-html/*.ts` | 3 / 750 |
| `kiana-ext-api` | `core/extensions/types.ts` | 1 / 2,308 |
| `kiana-tools` | `core/tools/**` + `core/bash-executor.ts` | 25 / 4,794 |
| `kiana-ui-kit` | `modes/interactive/components/{auth-url,bordered-loader,countdown-timer,diff,dynamic-border,keybinding-hints,kiana-logo,themed-text,visual-truncate}.ts` | 9 / 536 |
| `kiana-resources` | `core/{package-manager,resource-loader,skills,prompt-templates,system-prompt,bug-report,bug-report-upload,crash-log,footer-data-provider}.ts` | 9 / 6,151 |
| `kiana-extensions` | `core/extensions/{index,loader,runner,wrapper,virtual-modules,jiti-loader,jiti-static-loader}.ts`（`types.ts` 在 `kiana-ext-api`，不重复计） | 7 / 2,745 |
| `kiana-session` | `core/{agent-session,agent-session-runtime,agent-session-services,sdk}.ts` | 4 / 5,617 |
| `kiana-builtin-extensions` | `extensions/**`（mcp / codemode / llama / tool-search，含 3 个 `.lazy.ts`） | 25 / 7,585 |
| `kiana-interactive` | `modes/interactive/**`（去掉 theme 与 9 个 ui-kit 组件）+ `modes/index.ts` | 49 / 19,342 |
| `kiana-rpc-print` | `modes/{index,json-event,print-mode}.ts` + `modes/rpc/**` | 7 / 2,047 |
| `kiana-coding-agent` | `cli.ts`、`main.ts`、`migrations.ts`、`index.ts`、`package-manager-cli.ts`、`rpc-entry.ts`、`cli/**`、`bun/**`、`experimental/source-resolver.ts` | 23 / 4,584 |

（表中数字为该 crate 负责的 TS 源 `wc -l` 实测值（不含 `.d.ts`）。文件数列和为 252：`modes/index.ts`
在 `kiana-interactive` 与 `kiana-rpc-print` 各计一次、`core/index.ts`（97 行）不单列，两者抵消；行数列和为
76,291 = 76,372 − 97 + 16，拆分口径见 §3。`core/index.ts` 的再导出面按来源分到 `kiana-session-core`、
`kiana-session`、`kiana-coding-agent::sdk_exports` 三处。`utils/highlight-js.d.ts` 与 `providers/data-json.d.ts`
是纯类型声明，无运行期代码，不占 `.rs` 文件。

---

## 2. crate 依赖图

### 2.1 分层（按最长路径）

`[workspace.dependencies]` 里的 31 个 path 依赖构成 DAG（所有 crate 的 `Cargo.toml` 只引用
`workspace = true` 的 path 依赖，因此这张图就是编译顺序）：

```
L 0  kiana-env        kiana-json        kiana-sandbox     kiana-telemetry
L 1  kiana-ai         kiana-chord       kiana-codemode   kiana-mcp        kiana-tui
L 2  kiana-agent-core kiana-durable
L 3  kiana-core       kiana-env-client
L 4  kiana-session-core  kiana-permissions
L 5  kiana-compaction  kiana-config
L 6  kiana-theme      kiana-hooks       kiana-memory
L 7  kiana-ext-api     kiana-ui-kit
L 8  kiana-export      kiana-tools
L 9  kiana-extensions
L10  kiana-builtin-extensions        kiana-resources
L11  kiana-session
L12  kiana-interactive kiana-rpc-print
L13  kiana-coding-agent
L14  kiana-evals
```

### 2.2 边（每条边都有实际用途，不允许“顺手加依赖”）

```
kiana-ai            -> kiana-json, kiana-telemetry
kiana-chord         -> kiana-json, kiana-telemetry
kiana-codemode      -> kiana-json
kiana-mcp           -> kiana-json
kiana-tui           -> kiana-json
kiana-agent-core    -> kiana-ai
kiana-core          -> kiana-agent-core, kiana-ai, kiana-tui
kiana-durable       -> kiana-ai, kiana-chord, kiana-codemode, kiana-json, kiana-telemetry
kiana-env-client    -> kiana-chord, kiana-durable
kiana-session-core  -> kiana-agent-core, kiana-ai, kiana-core
kiana-permissions   -> kiana-core, kiana-sandbox
kiana-compaction    -> kiana-agent-core, kiana-ai, kiana-core, kiana-session-core
kiana-config        -> kiana-agent-core, kiana-ai, kiana-core, kiana-json, kiana-session-core, kiana-tui
kiana-theme         -> kiana-config, kiana-core, kiana-tui
kiana-ext-api       -> kiana-agent-core, kiana-ai, kiana-compaction, kiana-config, kiana-core, kiana-session-core, kiana-theme, kiana-tui
kiana-ui-kit        -> kiana-core, kiana-theme, kiana-tui
kiana-export        -> kiana-ai, kiana-core, kiana-ext-api, kiana-session-core, kiana-theme, kiana-tui
kiana-tools         -> kiana-agent-core, kiana-ai, kiana-core, kiana-ext-api, kiana-sandbox, kiana-session-core, kiana-theme, kiana-tui, kiana-ui-kit
kiana-extensions    -> kiana-agent-core, kiana-ai, kiana-config, kiana-core, kiana-ext-api, kiana-session-core, kiana-theme, kiana-tools, kiana-tui
kiana-builtin-extensions -> kiana-agent-core, kiana-ai, kiana-codemode, kiana-config, kiana-core, kiana-ext-api, kiana-extensions,
                         kiana-json, kiana-mcp, kiana-session-core, kiana-theme, kiana-tools, kiana-tui, kiana-ui-kit
kiana-resources     -> kiana-agent-core, kiana-ai, kiana-config, kiana-core, kiana-ext-api, kiana-extensions, kiana-session-core,
                    kiana-theme, kiana-tui
kiana-session       -> kiana-agent-core, kiana-ai, kiana-compaction, kiana-config, kiana-core, kiana-export, kiana-ext-api,
                    kiana-extensions, kiana-hooks, kiana-json, kiana-memory, kiana-permissions, kiana-resources, kiana-session-core,
                    kiana-theme, kiana-tools, kiana-tui
kiana-hooks         -> kiana-json
kiana-memory        -> （无 workspace 依赖）
kiana-interactive   -> kiana-agent-core, kiana-ai, kiana-builtin-extensions, kiana-config, kiana-core, kiana-export, kiana-ext-api,
                    kiana-extensions, kiana-permissions, kiana-resources, kiana-session, kiana-session-core, kiana-theme,
                    kiana-tools, kiana-tui, kiana-ui-kit
kiana-rpc-print     -> kiana-agent-core, kiana-ai, kiana-config, kiana-core, kiana-ext-api, kiana-extensions, kiana-session,
                    kiana-session-core, kiana-theme, kiana-tools, kiana-tui
kiana-coding-agent  -> kiana-agent-core, kiana-ai, kiana-builtin-extensions, kiana-compaction, kiana-config, kiana-core,
                    kiana-export, kiana-ext-api, kiana-extensions, kiana-interactive, kiana-resources, kiana-rpc-print,
                    kiana-session, kiana-session-core, kiana-theme, kiana-tools, kiana-tui
kiana-evals         -> kiana-ai, kiana-coding-agent
kiana-env           -> （无）
kiana-json          -> （无）
kiana-sandbox       -> （无）
kiana-telemetry     -> （无）
```

### 2.3 与 TS 包图对照

TS 的包依赖（`package.json` 的 dependencies）是：

```
agent        -> ai
ai           -> telemetry
chord        -> telemetry
codemode     -> （无）
mcp          -> （无）
telemetry    -> （无）
tui          -> （无）
durable      -> ai, chord, codemode, telemetry
env          -> chord, durable          # 注意：env 的 @earendil-works/kiana-durable 依赖在 TS 侧是
evals        -> ai, coding-agent        #   packages/env/package.json 声明的，daemon 自己零依赖
coding-agent -> agent, ai, codemode, mcp, tui
```

对照结论：

1. **方向不变**：Rust 图里的每条跨包边都能在 TS 图里找到 PATH（例如 `kiana-core -> kiana-ai` 来自
   coding-agent → ai；`kiana-tui -> kiana-json` 是新增的“语义层”边，见 §3.2）。
2. **新增的边只有一类**：向上抽出的共享层（`kiana-json`：JS 数字/键序/字符串语义；`kiana-tui ->
   kiana-json`；`kiana-ui-kit`：9 个无业务依赖的渲染小件；`kiana-ext-api`：`types.ts` 的类型面；
   `kiana-session-core`：会话数据层）。除此之外**没有**任何 TS 图之外的依赖：`kiana-durable` 曾一度
   被写成依赖 `kiana-agent-core`（因为 `harness/generation.ts` 的注释提到 agent loop，但该文件没有
   任何 import），骨架里已删除，与 TS 的 `package.json` 一致。
3. **TS 侧 coding-agent 是“巨石”**：它依赖 5 个包，但内部还有 25 个以上互相引用的
   `core/*.ts`。Rust 侧把它拆成 16 个 crate 并排成 L3–L13，是为了让并行实现者互不改文件；
   SCC 证据见 §3.1。

---

## 3. 拆分依据与每一处偏离的理由

### 3.0 方法：SCC 分析

对每个 TS 包做文件级 import 图（`import`/`export ... from`，相对路径），再跑 Tarjan 求强连通分量，
分别统计「含 `import type`」与「不含 `import type`」两种口径。规则：

- 运行时 SCC（不含 `import type`）**必须**在同一个 crate 内，否则需要 trait 转发层。
- 仅类型 SCC（含 `import type`）可以拆到不同 crate，但拆分点必须是「纯类型」，且实施时可能
  需要把类型上移（§3.5）。
- 0 个 SCC 的包可以按“一个 TS 目录一个 crate”随意切，但切分仍以**不产生运行时环**为前提。

实测结果（`packages/*/src`，排除 `*.test.ts`/`*.d.ts`）：

| 包 | 文件 | 边 | 含 `type` 的 SCC | 不含 `type` 的 SCC |
|---|---|---|---|---|
| ai | 202 | 781 | 1 个 42 文件 | 无 |
| coding-agent | 252 | 1021 | 5 个（22 / 3 / 2 / 2 / 2） | 1 个 3 文件 |
| durable | 84 | 311 | 2 个（27 / 2） | 1 个 5 文件 |
| chord | 41 | 126 | 4 个（5 / 4 / 2 / 2） | 1 个 3 文件（`delta/`） |
| telemetry | 8 | 17 | 1 个 4 文件 | 无 |
| tui | 46 | 137 | 无 | 无 |
| agent | 6 | 11 | 无 | 无 |
| mcp | 18 | 41 | 无 | 无 |
| codemode | 16 | 48 | 无 | 无 |
| env | 8 | 17 | 无 | 无 |
| evals | 5 | 7 | 无 | 无 |

具体成员（只列影响拆分的）：

- **ai**：42 文件 SCC = `api/*`（含 `lazy.ts`）+ `models.ts` + `models-store.ts` + `types.ts` +
  `auth/{context,credential-store,resolve,types}.ts` + `env-api-keys.ts` + `telemetry.ts` +
  `utils/*`（diagnostics、error-body、estimate、event-stream、headers、identity、json-parse、
  model-operations、models-error、node-http-proxy、openrouter-cost、provider-env、
  streaming-json、text、transcript）。最短环是 `models.ts -> utils/identity.ts -> models.ts`
  与 `models.ts -> api/google-vertex.ts -> types.ts -> models.ts`。**结论：`kiana-ai` 必须是单 crate。**
- **coding-agent 运行时 SCC（3 文件）**：`core/agent-session-runtime.ts` ↔
  `core/agent-session-services.ts` ↔ `core/sdk.ts`。三者整体进 `kiana-session`（不拆）。
- **coding-agent 类型 SCC（22 文件）**：`core/extensions/types.ts` + `core/tools/**`（8 个工具 +
  8 个 renderer + `index.ts` + `render-utils.ts` + `tool-definition-wrapper.ts`）+
  `core/bash-executor.ts` + `core/model-resolver.ts` + `cli/args.ts`。环的成因是
  `types.ts` 引用每个工具的 `Input/Details` 与 renderer 类型，工具又引用 `types.ts` 的
  `ToolDefinition`。**结论：类型面必须上移成独立 crate `kiana-ext-api`（§3.4），其余留在
  `kiana-tools`/`kiana-config`/`kiana-coding-agent`。**
- **coding-agent 其余 4 个小 SCC**：`package-manager.ts` ↔ `source-info.ts`（§3.5）、
  `theme.ts` ↔ `system-theme.ts`（同 crate `kiana-theme`）、`extensions/codemode/{renderer,tool}.ts`
  （同 crate `kiana-builtin-extensions`）、`agent-session-runtime/services/sdk`（同 crate）。
- **durable 运行时 SCC（5 文件）**：`harness/{compaction,generation,live,submissions,tool}.ts`
  ——同 crate，不拆。27 文件类型 SCC 横跨 `documents/entries/tasks/timers/session/**/harness/**`，
  说明 `kiana-durable` 也必须是单 crate。
- **chord 运行时 SCC（3 文件）**：`delta/{diff,index,tracker}.ts`——同 crate。4 文件类型 SCC
  `{cluster,connection,host,reload}.ts` 也同 crate。

### 3.1 `kiana-ai` 是单 crate（拒绝 `kiana-ai-types` / `kiana-ai-utils`）

`spec/ai-api-other.md` §8.1 建议拆成 `kiana-ai-types`（types.ts）、`kiana-ai-utils`（utils/*）、`kiana-ai`
三层。**不采纳**，原因：

1. `types.ts` 与 `api/*` 之间有真实类型环（上面 42 文件 SCC），拆开后 `kiana-ai-types` 必须为每个
   api 模块的 Options 类型暴露一个手写占位，或者引入 trait 转发层；两者都会让 202 个文件的
   “逐字对齐”变得难以核对。
2. `utils/*` 里 `models-error.ts`、`model-operations.ts`、`identity.ts` 与 `models.ts` 同属环，
   拆出去仍要反向依赖。
3. 单 crate 下 `cargo check -p kiana-ai` 已经是 108 个骨架文件、增量编译约 1 秒；拆成 3 个 crate
   的收益（并行编译）小于维护成本。

**但模块边界照旧**：`kiana-ai::types`、`kiana-ai::api::*`、`kiana-ai::utils::*`、`kiana-ai::auth::*`、
`kiana-ai::providers::*` 与 TS 目录一一对应，实现者可以分片并行（§7 的 ai-team 内部分工）。

### 3.2 `kiana-json`：新增的共享语义层

TS 里“JSON 数字/键序/字符串语义”散落在 `ai/src/utils/{json-parse,streaming-json,text}.ts`、
`chord/src/json.ts`、`coding-agent/src/utils/json.ts`，每处各有一份近似实现。Rust 侧不这么做：
`serde_json` 的默认行为在**四个**地方与 JS 不同（`1e21`、`1e-7`、`-0`、`NaN`/`Infinity`），
而键序（整数键升序优先）也不符合 JS 对象语义。`kiana-json` 是唯一的实现点：

- `js_num`：ECMAScript `Number::toString`（`ryu` 分解 + 规则化），`js_object` 的数字输出走它。
- `js_object`：`sort_js_keys`（整数索引键升序）+ `stringify`/`stringify_pretty`。
- `js_string`：`String(v)`、truthiness、`typeof`（错误消息与日志是用户可见输出，格式需稳定，改动需同步测试）。
- `js_parse`：`JSON.parse` 与带修复/流式解析的分层入口。

禁用规则：**任何会写到磁盘、发到网络或进入模型上下文的 JSON 都必须走
`kiana_json::js_object::stringify`**，不得直接用 `serde_json::to_string`（`ARCHITECTURE.md` §4.3）。

`kiana-json` 已交付（约 2,300 行源码 + 74 条测试），零 `kiana-*` 依赖（`serde`/`serde_json`/`ryu`/
`indexmap`/`thiserror`/`num-bigint`/`num-traits`），是 L0，任何 crate 都能用。
`kiana-tui`/`kiana-mcp`/`kiana-codemode`/`kiana-ai`/`kiana-chord` 的 `Cargo.toml` 里已经引用它。

### 3.3 `kiana-config` 承接 `core/{defaults,experimental,radius,telemetry}.ts`（从 `kiana-core` 移出）

这四个文件合计 39 行，但它们的读者在上层：

- `defaults.ts`（`DEFAULT_THINKING_LEVEL = "medium"`、`THINKING_LEVEL_OPTIONS`）被
  `model-resolver`/`model-runtime`（`kiana-config`）与 `kiana-coding-agent` 读；
- `experimental.ts`（`areExperimentalFeaturesEnabled`，`KIANA_EXPERIMENTAL === "1"`）被
  `kiana-coding-agent` 读；
- `radius.ts` 需要 `kiana-ai` 的 `providers::radius_config`（`RADIUS_PROVIDER_ID`、
  `ENV_RADIUS_GATEWAY`、`RADIUS_MCP_URL`、`get_radius_gateway_url`）；
- `telemetry.ts`（`isInstallTelemetryEnabled(settings)`）读 `Settings`，只能放 `kiana-config`。

若留在 `kiana-core`，`kiana-core` 就要为了 `radius.ts` 引入 `kiana-ai` 的 provider 语义（它已经依赖
`kiana-ai`，但语义上不该承载 provider 配置）；且 `kiana-config` 已经依赖 `kiana-core`，反向调用会成环。
**`http-dispatcher.ts` 留在 `kiana-core`**：它是 HTTP 客户端装配（超时、`undici` 全局 fetch 的等价物），
不读 settings，`kiana-ai` 与 `kiana-config` 都要用。

### 3.4 `kiana-ext-api`：从 22 文件类型 SCC 抽出的“唯一权威”类型 crate

`core/extensions/types.ts`（2,308 行）是扩展作者可见的**全部**契约：`ExtensionAPI`、
`ExtensionContext`、`ExtensionUIContext`、`ToolDefinition`、`ToolRenderContext`、41 个事件、`defineTool`。
它被 `kiana-tools` 的 renderer、`kiana-interactive` 的组件、`kiana-extensions` 的 runner 与
`kiana-resources` 的 system prompt 同时引用。抽成 L7 的独立 crate 后：

- `kiana-tools`（L8）、`kiana-extensions`（L9）、`kiana-resources`（L10）、`kiana-session`（L11）、
  `kiana-interactive`（L12）都向下依赖它，不再互相引用；
- 22 文件 SCC 的其余部分（具体工具的 `Input/Details`、renderer）留在 `kiana-tools`，
  `kiana-ext-api` 只放**跨 crate 共享的** `ToolDefinition`/`ToolRenderContext`/`ToolLoadout` 与
  工具 IO 类型（`kiana_ext_api::tool_types`）；
- `types.ts` 里 `import type { SourceInfo } from "../source-info.ts"`、
  `SlashCommandInfo from "../slash-commands.ts"`、`ResourceDiagnostic`、`ReadonlyFooterDataProvider`
  这几条指向 `kiana-core`/`kiana-resources` 的类型，按 §3.5 的规则上移或改由 `kiana-core` 提供。

### 3.5 跨 crate 的“契约上移”清单（唯一权威定义位置）

同一个 TS 文件只能有一个 crate 拥有实现（§5 规则 4）。TS 里因为都在 `packages/coding-agent/src`
下而随意互相 import 的类型，在 Rust 里必须落在**依赖链上最低的、所有读者都能访问**的 crate：

| TS 符号 | 定义位置（Rust） | 再导出 | 为什么不能放在原文件所在 crate |
|---|---|---|---|
| `SourceInfo`、`SourceScope`、`SourceOrigin`、`PathMetadata`、`BUILTIN_PATH_PREFIX`、`createSourceInfo`、`createSyntheticSourceInfo`、`getSyntheticPathSource`、`isSyntheticPath` | `kiana-core::source_info` | `kiana-resources::source_info` | 读者含 `kiana-theme`（L6）、`kiana-ext-api`（L7）、`kiana-extensions`（L9，`loader.ts`）；原文件 `package-manager.ts` 在 `kiana-resources`（L10） |
| `ResourceCollision`、`ResourceDiagnostic`（TS 在 `resource-loader.ts` 内定义） | `kiana-core::diagnostics` | `kiana-resources::diagnostics` | `kiana-extensions::runner`（L9）与 `kiana-interactive`（L12）都要用 |
| `resetTimings`、`time`、`printTimings`（`core/timings.ts`） | `kiana-core::timings` | `kiana-resources::timings` | `kiana-extensions::loader`（L9）调用 `time("extensions")` |
| `KianaManifest`、`readKianaManifest`（`core/kiana-manifest.ts`） | `kiana-core::kiana_manifest` | `kiana-resources::manifest` | `kiana-extensions::loader`（L9）与包管理（L10）共用 |
| `BUILTIN_SLASH_COMMANDS`、`SlashCommandInfo`、`SlashCommandSource`（`core/slash-commands.ts`） | `kiana-core::slash_commands` | — | `kiana-ext-api`（L7）的 `types.ts` 引用 `SlashCommandInfo` |
| `ReadonlyFooterDataProvider`（`core/footer-data-provider.ts` 的接口） | `kiana-ext-api::ui`（trait） | `kiana-resources::footer_data_provider` 实现它 | `kiana-ext-api` 是 L7，不能依赖 L10 的 `kiana-resources` |
| `BashResult`、`BashOperations`、`ExecOptions`、`ExecResult` | `kiana-ext-api::tool_result` | — | `types.ts` 与 `kiana-tools`/`kiana-session` 共用 |

实现约束：**同一符号在整个 workspace 只能有一个 `struct`/`enum`/`trait` 定义**；下游 crate
用 `pub use` 暴露到与 TS 同名的路径（例如 `kiana_resources::source_info::SourceInfo`），
不得复制定义。

### 3.6 `package-manager-cli.ts` 归 `kiana-coding-agent`，不归 `kiana-resources`

`spec/ca-resources.md` §8.1 把它放在 `kiana-resources`。**不采纳**：该文件（1,151 行）import 了
`cli/config-selector.ts`、`cli/project-trust.ts` 与 `config.ts`，即依赖 `kiana-coding-agent` 的 CLI 层；
放到 L10 的 `kiana-resources` 会形成 `kiana-resources -> kiana-coding-agent -> kiana-resources` 的环。
它只被 `main.ts` 引用（`handlePackageCommand`/`handleConfigCommand`/`cleanupManagedInstall`），
放 `kiana-coding-agent` 不产生任何额外依赖。骨架里 `kiana-resources` 的该模块已删除，
`kiana-coding-agent::package_manager_cli` 保留。

### 3.7 其余拆分的取舍

| 建议（来自 spec） | 决定 | 理由 |
|---|---|---|
| `ca-config-models.md` §8.1：单 crate `kiana-coding-agent` 承载 cli/session/resources/tools | 拒绝 | 会得到 2.5 万行以上的巨型 crate，20+ 实现者改同一批文件；且 `kiana-session` ↔ `kiana-cli` 在单 crate 内仍要人为分层 |
| `ca-session.md` §8.1：独立 `ca-session` crate | 采纳但**再拆** | 会话数据层（`session-manager`/`messages`/`virtual-models`/`usage-totals`/`cache-stats`/`session-cwd`/`session-export`/`nested-tool-calls`）被 `kiana-config`、`kiana-compaction`、`kiana-resources`、`kiana-tools` 在 L10 以下就需要，故拆出 `kiana-session-core`（L4）；`AgentSession` 本体留在 `kiana-session`（L11） |
| `ca-tools-compaction-export.md` §8.1：单 crate `kiana-coding-agent` | 拒绝（同上），但采纳其**模块划分** | 三个子系统各自成 crate：`kiana-tools`（L8）、`kiana-compaction`（L5）、`kiana-export`（L8） |
| `ca-interactive-mode.md` §8.1：`kiana-interactive` 单 crate | 采纳 | 19,342 行；`components/` 下 48 个文件已在同一 crate 内按文件拆分 |
| `ca-resources.md` §8.1：独立 `kiana-resources` | 采纳 | 且把 `package-manager-cli.ts` 移出（§3.6） |
| `ca-cli.md` §8.1：crate 名 `kiana-cli` | 改名 `kiana-coding-agent` | 与 TS 包名一致；`kiana` 二进制名不变 |
| `chord-rest.md` §8.1：单 crate + feature 子路径 | 采纳 | feature：`default = ["delta","context"]`、`node`、`sqlite`、`sqlite-node`、`sqlite-cloudflare`、`telemetry-sqlite`、`ws` |
| `tui-core.md`/`tui-text.md`/`tui-components.md`：`kiana-tui-{core,text,components}` | 合成单 crate `kiana-tui` | tui 包 0 个 SCC，但三份规范共享 `Component`/`Terminal`/颜色类型，拆开只会引入循环或重复类型 |
| `durable-rest.md` §8.1：两个 crate（核心 + 存储） | 单 crate `kiana-durable` + feature | 27 文件类型 SCC 横跨存储与 harness；`sqlite`/`cloudflare`/`images`/`node` 用 feature 门控 |
| `env-evals-durable-testing.md`：env 的 daemon 与 client | 拆 `kiana-env`（bin）+ `kiana-env-client`（lib） | daemon 是**独立 Rust 二进制**（零依赖、只讲帧协议），client 依赖 `kiana-chord`/`kiana-durable`，必须分开 |
| `ca-extensions.md` + 4 份设计文档：`kiana-extensions` | 采纳单 crate，但**运行时全部 feature 门控** | `rquickjs`+`oxc`+`wasmtime`+`icu` 是重依赖；骨架 `default = []`，阶段 5 起把 `runtime` 加进 default（设计文档 §1.2） |
| `ai-api-other.md` §8.1：`kiana-ai-types`/`kiana-ai-utils` | 拒绝（§3.1） | 42 文件运行时 SCC |
| 无 | 新增 `kiana-ui-kit`（L7） | `components/` 里 9 个文件（`auth-url`/`bordered-loader`/`countdown-timer`/`diff`/`dynamic-border`/`keybinding-hints`/`kiana-logo`/`themed-text`/`visual-truncate`，共 536 行）被 `kiana-tools` 的 renderer（L8）与 `kiana-interactive`（L12）共用，必须位于 L8 之上、不能再低 |
| 无 | 新增 `kiana-session-core`（L4） | 见上 |
| 无 | 新增 `kiana-ext-api`（L7） | 见 §3.4 |
| 无 | 新增 `kiana-json`（L0） | 见 §3.2 |

### 3.8 与 spec 里“建议布局”不一致时的仲裁规则

1. **依赖 DIRECTION 优先**：任何 spec 里“建议单 crate”但会引入环的，一律按本文拆分。
2. **文件总数不变**：拆分只改变 crate 边界，不改变“一个 TS 文件一个 `.rs` 文件”的映射（§5 规则 1）。
3. **实现者以骨架的 `//!` 头为准**：若 spec 文本与骨架文件头冲突，报告给架构者，不要两边都实现。
4. **spec 的验收清单不因拆分而减少**：每个 spec 的测试清单按 TS 文件落点归到对应 crate 的
   `tests/` 下（§4.8）。

---

## 4. 共享约定

所有 crate 必须遵守；这些约定是并行实现能拼起来的前提。

### 4.1 错误类型策略

| 层 | 类型 | 规则 |
|---|---|---|
| 叶子 crate 的**内部**错误 | `thiserror` 枚举 | 只在 crate 内使用；`#[error("...")]` 的文案必须是 TS 中对应的英文消息（逐字） |
| 跨 crate 的**公开**错误 | 手写 `enum`/`struct` + 手写 `Display` | 见下；不用 `thiserror` 的字段展开，因为 `Display` 必须**逐字**等于 TS 的 `error.message` |
| 任意错误的**透传** | `Box<dyn std::error::Error + Send + Sync>` | 仅在任务边界（后台任务、扩展回调）使用；不允许进入 JSON 或 UI 文案 |
| `anyhow` | **禁止** | 工作区已 pin 但不允许在库代码使用（会丢失字面消息） |

各 crate 已有的公开错误契约（骨架中已定义，**不得改名**）：

- `kiana_ai::AiError { kind, message, name, detail }`：`Display` == TS `error.message`；
  `name` 保留 `"Error"`/`"AbortError"`/`"ModelsError"`。`AiErrorKind` 用于分支。
  `kiana_ai::ModelsError { code, message, cause_detail }`：`ModelsErrorCode` 6 个变体。
- `kiana_mcp::McpError::{JsonRpc, ConnectionClosed, Timeout, Aborted}`：`name()` 返回
  `"McpError"`/`"McpConnectionClosedError"`/`"McpTimeoutError"`/`"AbortError"`（注意 abort 的 name
  就是 `AbortError`，上层按名字判断取消）；`connection_closed()` 的消息逐字
  `"MCP connection closed"`；`Timeout` 的 `Display` 逐字 `"MCP request timed out after {n}ms"`。
- `kiana_chord::ChordError { code, message, reason, details }` + `ChordErrorCode`（12 个 snake_case
  变体，`ALL` 顺序即校验顺序）：`Display` == TS 的 `code[/reason]: message`；
  `retryable()` 只有 `Superseded`/`UpgradeConflict`；`ChordAggregateError` 对应 `AggregateError`。
- `kiana_telemetry::{TelemetryError, TypedMetricsError, StatusError}`：`TelemetryError` 保留
  `message` 与可选 `cause` 文本。
- `kiana_json::ParseError { message, position }`。
- 其余 crate（`kiana-core`、`kiana-durable`、`kiana-config` 等）在实现阶段各自定义，但必须满足
  「公开错误 `Display` == TS `error.message`」这条规则。

跨 crate 的错误转换统一写在**错误定义所在的 crate**（`impl From<X> for Y`），不要在调用方写。

### 4.2 异步运行时

- 统一 `tokio`（`rt`、`rt-multi-thread`、`macros`、`sync`、`time`、`process`、`io-util`、`fs`、
  `net`、`signal`），版本 pin `=1.54.1`。
- **取消**：TS 的 `AbortSignal` → `tokio_util::sync::CancellationToken` + 可选的
  `Arc<Mutex<Option<Cause>>>`（保留 `signal.reason`）。`kiana-ai` 的 `HttpClient::execute` 签名已固定为
  `signal: Option<&CancellationToken>`。
- **事件流**：TS 的 `EventStream`/`FifoQueue` → `tokio::sync::mpsc` + `oneshot`；
  `kiana-ai::event_stream::AssistantMessageEventStream` 已定义（`push`/`end`/`result`/`recv`，
  容量 `EVENT_CHANNEL_CAPACITY = 4096`）。
- **单线程 crate**：`kiana-durable`（事务/session 状态机）与 `kiana-extensions`（rquickjs 的 `Persistent`
  是 `!Send`）必须跑在 `tokio::task::LocalSet` 上；这两个 crate 的公开 API 不得要求 `Send` 的 future。
- **TUI**：`kiana-tui` 的 `Component` trait **不要求 `Send`**（整个 TUI 在单线程上运行，与 TS 事件循环
  一致）；`Tui::query_terminal_colors` 返回 `Pin<Box<dyn Future + '_>>`（非 `Send`）。
- **锁**：跨 `await` 持锁必须显式 `#[allow(clippy::await_holding_lock)]` 并注释原因；优先用
  `tokio::sync::Mutex`，只有纯内存短临界区用 `std::sync::Mutex`。
- **不引入** `async-std`/`smol`/`futures-executor` 的第二套运行时；`futures` 只用于组合子。

### 4.3 serde 与 JS 语义 JSON

1. **字段名**：TS 接口字段是 camelCase 时，Rust 结构体用 `snake_case` 字段 + `#[serde(rename_all =
   "camelCase")]`；TS 字段本身是 kebab/特殊拼写时逐字段 `#[serde(rename = "...")]`
   （例：`TerminalColorMode::Palette256` → `"256color"`，`Frame::RootEnd` → `rootId`）。
2. **可选字段**：TS 的 `field?: T` → `Option<T>` + `#[serde(skip_serializing_if = "Option::is_none")]`。
   但**反序列化的默认值**有独立技术原因：`??`（仅 `null`/`undefined` 触发）与 `||`（任何 falsy 触发，如 `0`/`""`/`false`）的差别要在 `Default` 或自定义反序列化里体现，否则同一输入会得到不同的运行时取值与落盘结果。
3. **键序**：`serde_json` 开启 `preserve_order`（`IndexMap`），结构体字段顺序 == TS 接口声明顺序 ==
   输出顺序。任何 `serde_json::Map`/`Value` 在写出前经过 `kiana_json::js_object::sort_js_keys`。
4. **数字**：任何浮点写出走 `kiana_json::js_num::format_js_number`（`1e+21`、`1e-7`、`-0`→`"0"`、
   `NaN`/`Inf`→`null`）。整数在内存里可以是 `i64`/`u64`，但一旦进入模型可见文本或会话文件，
   必须符合 JS 的 f64 语义（`kiana_json::js_parse` 的 `normalize_numbers`）。
5. **`undefined`**：JSON 没有 `undefined`。对象里省略 → `Option` + `skip_serializing_if`；
   数组里 → 显式 `Value::Null`。
6. **枚举**：TS 字符串字面量联合 → `#[serde(rename_all = "camelCase"|"snake_case"|"lowercase")]`；
   判别式联合（`{ type: "text", text }`）→ `#[serde(tag = "type")]`；`untagged` 只用于 TS 的
   `A | B` 且**判别顺序**与 TS 类型守卫一致的地方（`JsonRpcMessage` 已这样做）。
7. **时间**：TS 的 `Date.now()` 毫秒 → `i64`；`chrono` 只用于需要格式化/解析人类可读时间的
   （changelog、导出 HTML）。不引入 `time` crate 的第二套类型。
8. **禁止** `serde_json::json!` 产生会写出的对象（它的键序是插入序，但嵌套 builder 容易漏
   `sort_js_keys`）；测试里可以用。

关键测试（骨架已包含）：`kiana-json` 的 6 条（整数键序、`1e21`、`1e-7`、`-0`、NaN→null、`deep_equal`）。

### 4.4 日志

- 统一 `tracing`（`=0.1.44`）+ `tracing-subscriber`（`=0.3.20`）。**不引入** `log` 门面。
- TS 的 `console.warn`/`console.error` 在库代码里是**用户可见输出**（例如
  `[kiana] Failed to load extension ...`），是用户可见的输出契约，需逐字保留；实现方式：
  - 启动期（`kiana-coding-agent`）安装 subscriber，`stderr` 输出；
  - 库代码里凡是 TS 用 `console.*` 的地方，先写 `tracing::warn!`/`error!`，并在消息文本里
    保留 TS 原文字面量（不加前缀）。
- 交互模式（TUI 已接管屏幕）下 `console.*` 的输出在 TS 里被重定向到日志文件；Rust 侧由
  `kiana-interactive` 安装的 subscriber 写入同一路径。
- `RUST_LOG` 与 `KIANA_DEBUG` 不作为公开配置面；调试开关只认 `KIANA_TIMING`（见 §4.5），避免多套日志开关导致行为分叉。

### 4.5 环境变量（用户可见契约，名称与取值稳定）

进程配置：

| 变量 | 语义 |
|---|---|
| `KIANA_CODING_AGENT_DIR` | 配置目录，默认 `~/.kiana/agent` |
| `KIANA_CODING_AGENT_SESSION_DIR` | 会话目录，`--session-dir` 优先 |
| `KIANA_PACKAGE_DIR` | 包目录（Nix/Guix store 路径） |
| `KIANA_OFFLINE` | 禁止自动网络活动（模型目录刷新等） |
| `KIANA_API_BASE_URL` | Kiana 自建服务（模型目录、版本检查、安装器、遥测、OAuth CIMD）的根 URL；**无默认值**，未设置时相关网络功能停用 |
| `KIANA_INSTALLER_API_BASE` | 托管更新 release API；未设置时回退到 `KIANA_API_BASE_URL/api/installer/releases` |
| `KIANA_SKIP_VERSION_CHECK` | 禁止版本检查（端点由 `KIANA_API_BASE_URL` 提供） |
| `KIANA_TELEMETRY` | `1/true/yes` 或 `0/false/no`：安装遥测与 provider attribution |
| `KIANA_CODEMODE_MAX_WORKERS` | 进程级 codemode worker 上限，默认 `2` |
| `KIANA_CACHE_RETENTION` | `long` 时启用扩展 prompt cache |
| `KIANA_SHARE_VIEWER_URL` | `/share` 查看器基地址；未设置时回退到 `KIANA_API_BASE_URL/session/` |
| `KIANA_RADIUS_GATEWAY` | `/bug` 上传的网关源 |
| `KIANA_EXPERIMENTAL` | `"1"` 启用实验特性 |
| `KIANA_TIMING` | `"1"` 启用启动计时（`kiana_core::timings`） |
| `KIANA_CLEAR_ON_SHRINK` / `KIANA_HARDWARE_CURSOR` / `KIANA_CLIPBOARD_OSC52` / `KIANA_HYPERLINKS` / `KIANA_PROGRAM_STATUS` / `KIANA_IMAGE_PROTOCOL` / `KIANA_TRUE_COLOR` / `KIANA_TUI_ESC_TIMEOUT` | 终端行为覆盖（细节见 `packages/coding-agent/docs/environment-variables.md`，由 `kiana-tui`/`kiana-interactive` 消费） |
| `VISUAL` / `EDITOR` | 外部编辑器回退 |
| `HTTP_PROXY` / `HTTPS_PROXY` | 出站 HTTP 代理 |

Kiana 自建服务不内置上游域名。`KIANA_API_BASE_URL` 未配置时：模型目录只使用内置/缓存模型，
版本检查与安装遥测跳过，MCP CIMD 明确报不可用，`/share` 仅显示 gist 链接并提示查看器未配置；
托管更新要求 `KIANA_INSTALLER_API_BASE` 或 `KIANA_API_BASE_URL`。

进程标记与 shell 工具注入：`AI_AGENT=kiana`、`KIANA_CODING_AGENT=true`（CLI/RPC 入口设置）；
`KIANA_SESSION_ID`、`KIANA_SESSION_FILE`、`KIANA_PROVIDER`、`KIANA_MODEL`、`KIANA_REASONING_LEVEL`（仅注入
`bash`/`powershell` 工具，`!`/`!!` 命令不注入）。

构建期：`KIANA_ENV_VERSION`（`kiana-env` 的 `build.rs` 读取，未设置时回退 `packages/env/package.json` 的
`version` 字面量 `1.4.0`，与参照 daemon 读 `../package.json` 的结果一致；见 §8 第 7 条）；
`KIANA_MODEL_DATA_DIR`（`kiana-ai` 的 `build.rs` 读取模型目录 JSON；**未设置时回退仓库内
`crates/kiana-ai/data/`**，所以普通 `cargo build` 不需要 Node，见 §8 第 21 条与
`crates/kiana-ai/data/README.md`）。版本常量 `KIANA_AI_VERSION`/`VERSION` **不是**环境变量，是字面量
（§8 第 24 条）。

测试期：`KIANA_ENV_DAEMON`（`packages/env/test/daemon.ts` 的等价物）、`KIANA_TEST_*` 只在测试里使用。

### 4.6 字符串索引：UTF-16 vs UTF-8

TS 的 `string.length`/`slice`/`indexOf` 都按 **UTF-16 code unit**；Rust 的 `str` 是 UTF-8 字节、
`chars()` 是 Unicode 标量。凡是 TS 里做了 `length`/`slice`/`substring`/`padStart` 的地方：

1. 计数与截断一律 `s.encode_utf16().count()`；截断必须**不拆代理对**（相邻两个 code unit 组成
   ≥ U+10000 时延后切）。
2. 宽度（终端列）用 `unicode-width`，与 TS 的 `string-width`/`wcwidth` 对齐；
   `Intl.Segmenter` 的 UAX#29 用 `unicode-segmentation`（差异按 `design-extension-runtime.md` §7.4
   的诚实清单记录）。
3. 已经明确的落点：`kiana-tui` 的 `BoundedTerminalWriter`（单次 write ≤ 1 MiB 且不拆代理对）、
   `kiana-resources::skills::validate`（名字/描述长度按 UTF-16）、`kiana-compaction` 的 token 估算边界、
   `kiana-export::ansi_to_html` 的列宽。
4. JSON 转义（`😀` 形式）：`kiana_json::js_string::js_quote` 是唯一实现。

### 4.7 时间、随机与确定性

- 时间：`Date.now()` → `SystemTime::now()` 毫秒 `i64`；monotonic → `std::time::Instant`。
  TS 测试里对时间戳的断言（只断言存在/≥0）照搬。
- 随机：TS 的 `crypto.randomUUID()` → `uuid::Uuid::new_v4()`；`getrandom` 只用于需要 OS 熵的
  场景。**任何**进入会话文件/遥测的 id 格式是稳定契约（`uuid` v4 字符串、`sha2` 十六进制前缀），改动需同步下游读取方与测试。
- 确定性指纹：`chord` 的 `hash64`、`kiana-ai` 的 `shortHash`、`kiana-resources` 的 identity 都要求
  跨语言一致；实现时以 TS 的输入字节为准，用 `sha2` + 相同截断规则。

### 4.8 测试策略

TS 侧共 **706 个 `*.test.ts`、约 199,262 行**（ai 182/46,486、coding-agent 317/68,570、
durable 77/38,400、chord 53/15,898、tui 44/19,459、agent 4/4,050、codemode 6/2,073、
mcp 5/1,601、env 6/1,142、evals 6/815、telemetry 6/768）。移植规则：

1. **一对一移植**：每个 `foo.test.ts` → 对应 crate 的 `tests/foo.rs`（或模块内 `#[cfg(test)] mod tests`，
   当测试需要访问私有项时）。`describe`/`it` 的**标题逐字保留**（英文原文），因为失败报告需要与历史测试名对照。
2. **快照**：TS 的 `toMatchSnapshot`/内联快照 → `insta`（`=1.43.2`）；`.snap` 提交进仓库，
   `.snap.new` 进 `.gitignore`（已配置）。TS 快照里的 ANSI 序列、JSON 文本、渲染行必须逐字节相同。
3. **黄金测试（对 TS 二进制）**：`tests/golden/` 下用 `include_str!` 保存 TS 侧真实输出
   （会话 JSONL、RPC 事件流、HTML 导出、chord 帧、telemetry 快照），Rust 侧比较字节。
   黄金文件由仓库根的一个脚本从 `kiana` 的构建产物生成并提交；**禁止**在 CI 里跑 Node 生成它们。
4. **telemetry store 一致性**：`kiana-telemetry::testing` 提供与 TS 同名的
   `create_storage_conformance`/store-conformance 六条，所有 store 实现（memory、sqlite、
   indexeddb、durable 的 JSONL）都要跑。
5. **属性测试**：`proptest`（`=1.12.0`）用于 `kiana-json` 的数字格式化（与 JS 的 `Number::toString`
   对照表）、`kiana-chord` 的 delta 应用（`apply(apply(x, ops))` 幂等性）、`kiana-tui` 的宽度/截断。
6. **HTTP**：`wiremock`（`=0.6.5`）替代 TS 的 `fetch` 假实现；provider 适配器的 SSE 分块回复
   用 `wiremock` 的 `ResponseTemplate` 拼接，断言与 TS 相同的消息序列。
7. **CLI 端到端**：`kiana-coding-agent/tests/cli_*.rs` 用 `assert_cmd` 风格（直接 `std::process::Command`
   调用 `target/debug/kiana`），断言退出码、stdout/stderr 逐字、落盘文件。
8. **测试禁止**：真实网络、真实 provider key、真实 `~/.kiana`（用 `tempfile` + `KIANA_CODING_AGENT_DIR`）；
   时间相关断言不得依赖墙钟。
9. **覆盖率门槛**：每个 crate 的公开 API 至少被一个测试触达；`kiana-json`/`kiana-chord`/`kiana-mcp` 的
   逐字消息要求 100% 分支覆盖（骨架已有 20 条测试打底）。

### 4.9 骨架中已固定的跨 crate 契约（实现者直接编码，不要改签名）

> 2026-10-26 架构者补充了 4 行（`kiana-ai` 的 `http/` 与 `streaming_json.rs`、`kiana-agent-core` 的
> `proxy.rs`、`kiana-tui` 的 `native/**`、`kiana-durable` 的 `env/path/**`），并把 §8 的未决条目
> 全部标成「已决 / decided」。冻结规则不变：这些文件的公开签名只有架构者能改。

| crate | 文件 | 内容 |
|---|---|---|
| `kiana-json` | `js_num`/`js_object`/`js_parse`/`js_string` | 已实现（约 2,300 行源码 + 74 条测试）；`format_js_number`、`to_radix_string`、`parse_js_number`、`stringify`、`stringify_pretty`、`sort_js_keys`、`deep_equal`、`js_string`、`js_typeof`、`parse`/`repair`/`parse_with_repair`/`parse_streaming` |
| `kiana-telemetry` | `index.rs`（约 39 个公开项）、`metrics.rs`、`memory.rs`、`noop.rs`、`render.rs`、`testing.rs` | `TelemetryContext`/`TelemetryStore`/`TelemetryPropagation` trait、`TelemetrySpan`、`Stored*`、`define_telemetry_schema`、`TypedMetrics` |
| `kiana-ai` | `types.rs`（1,158 行）、`error.rs`、`event_stream.rs`、`http/{mod,reqwest_client}.rs`、`utils/streaming_json.rs`、`utils/version.rs`、`models/*`、`providers/catalog.rs` | 内容块/消息/模型/cost/usage/流事件；`AiError`/`ModelsError`；`AssistantMessageEventStream`；`HttpClient` trait + 默认 reqwest 实现；`ToolCallArgumentsStream`（流式工具参数，含 `partial()` 的 revision）；`KIANA_AI_VERSION`；42 个 provider 工厂模块 |
| `kiana-agent-core` | `types.rs`（约 37 个公开项）、`identity.rs`、`stream_fn.rs`、`proxy.rs` | `AgentMessage`/`AgentTool`/`AgentEvent`/`AgentState`/`AgentLoopConfig`/`StreamFn` 等；`default_agent_identity()`；进程级 `set_default_stream_fn`；`stream_proxy`/`ProxyStreamOptions`/`ProxyAssistantMessageEvent`（§8 第 18 条） |
| `kiana-tui` | `tui.rs`、`terminal_colors.rs`、`colors.rs`、`editor_component.rs`、`native/**` | `Component`/`Focusable`/`Tui`/`Terminal`/`OverlayHandle` trait、鼠标事件、overlay 几何、`Color`/`TextStyle`/`ProgramStatus`、`CURSOR_MARKER` 等常量；`NativeClipboard`/`ClipboardValue`（三态）/`ClipboardError`/`NativeModifiers`/`get_native_clipboard()`/`get_native_module_candidates()`（`kiana-core`、`kiana-interactive` 共用，§8 第 17 条） |
| `kiana-chord` | `errors.rs`、`wire.rs`、`context/*`、`state.rs`、`watch.rs` | `ChordError`/12 码；`Frame` 23 个 tag + `PROTOCOL_VERSION = 4` + `validate()`；`Context` trait 面 |
| `kiana-mcp` | `jsonrpc.rs`、`protocol_types.rs` | JSON-RPC 消息 untagged 枚举 + `parse_json_rpc_message`（判别顺序逐字）、`McpError`、`LATEST_PROTOCOL_VERSION` |
| `kiana-durable` | `types.rs`、`env/*`（含 `env/path/{posix,win32}`）、`storage/*`、`images/*` | 文档/条目/任务记录类型、`ExecutionEnv` 契约、JSONL/SQLite 后端 + IndexedDB/Cloudflare 占位（§8 第 2、3 条）、**`node:path` posix/win32 的唯一实现**（`kiana-env-client` 复用，§8 第 23 条）、Photon 等价物（§8 第 20 条） |
| `kiana-ext-api` | `types.rs`、`tool_types.rs`、`tool_result.rs`、`events.rs`、`system_prompt.rs`、`ui.rs` | 扩展公开 API 的唯一权威面（见 §3.4/§3.5） |

其余 crate 在骨架里只有 `//!` 说明与空模块：**这是刻意的**——它们的类型由实现者在阶段开工时
按 spec 定义，避免架构者预设错误形状。若某个类型需要跨 crate，先在本表补一行再实现。

### 4.10 代码规范

- `edition = "2024"`、`rust-version = "1.97"`（`oxc 0.155` 的 MSRV）、`resolver = "3"`。
- `cargo fmt`（默认配置）必须通过；`cargo clippy --workspace --all-targets` 必须 0 warning。
- **禁止** `#![allow(clippy::all)]`/`#![allow(warnings)]`；局部例外必须写明理由（骨架里已有
  4 处 `module_inception`、2 处 `large_enum_variant`、1 处 `very_complex_type`、1 处
  `await_holding_lock`，全部带注释）。
- 公开项必须有 `///` 文档；涉及 TS 对应关系时在文档里写 `TS: <file>#<symbol>`。
- 不引第三方 `unwrap()`/`expect()` 进入公开 API 路径；内部不变量用 `expect("...")` 并写明前提。
- crate 间**不得**通过 `pub(crate)` 偷渡；跨 crate 接口一律 `pub` 且在 `lib.rs` 再导出。

---

## 5. 每个 crate 的模块树

本节内容由骨架的实际文件生成（每行 `<文件>  <该文件的 TS 源与职责>`）。所有文件都已存在、
参与 `cargo check`，实现时直接替换文件内容即可。

映射规则（全部 crate 一致）：

1. **一个 TS 文件 → 一个 `.rs` 文件**，路径把 `-` 换成 `_`、目录保留。例外只有下面列出的几处。
2. **目录级 `mod.rs` 只放 `pub mod` 声明与一段目录说明**；实现代码都在具体文件里，
   这样并行实现时不会互相冲突。
3. **同一 TS 文件只允许一个 crate 拥有实现**，其余 crate 通过 `pub use` 再导出（§3.5 列全）。
4. 每个文件顶部的 `//!` 第一段就是该文件的 TS 源与一句话职责；实现者以它为准，
   **不要**根据文件名猜。
5. 标 `TODO(<crate>, stage N)` 的文件头是开工信号；`TODO` 消失即该文件完成。

特殊映射（不是一对一的地方）：

- `kiana-ai/src/api/lazy.rs`：合并 15 个 `api/*.lazy.ts`（动态 import 包装，Rust 侧编译期固定；
  `cloudflare-workers-ai-system-one.lazy.ts`、`llama-cpp-classify.lazy.ts` 等都在内）。
- `kiana-ai/src/providers/catalog.rs`：42 个 `providers/<id>.models.ts` + `providers/data/<id>.json`
  的编译期产物（`build.rs` 生成；`providers/<id>.rs` 仍是 42 个独立工厂文件）。
  `providers/data-json.d.ts` 是 `declare module "*.json"` 的类型垫片，无运行期代码，不生成 `.rs`。
- `kiana-ai/src/models/generated.rs`：`models.generated.ts` 的聚合面（只再导出）。
- `kiana-ai/src/compat.rs` + `compat/extension_oauth_types.rs`：`compat.ts`（302 行，全部 `export *`），
  并**合并** `legacy-api-aliases.ts`（108 行，16 个 `@deprecated` 常量，纯别名）与 `oauth.ts`
  （10 行，只 `export type` 自 `compat/extension-oauth-types.ts`）——三者都只有再导出、无独立状态，
  以 `pub use` 落在 `compat.rs`；`oauth.ts` 的 7 个类型走 `compat::extension_oauth_types`。
- `kiana-ai/src/bin/kiana-ai.rs` 与 `kiana-evals/src/main.rs`：都是**薄入口**（只有 `fn main` 调 crate 内的
  `cli.rs`/`lib.rs`），实现全在 `cli.rs`；不要把 `cli.ts` 的逻辑写进 `main.rs`。
- `kiana-resources/src/loader/mod.rs` 与 `kiana-extensions/src/loader/mod.rs`：对应 1,000 行以上的
  单个 TS 文件（`resource-loader.ts` 1,322 行、`extensions/loader.ts` 879 行）的门面 +
  按职责拆出的子模块；拆分的每个 `.rs` 头部写明自己负责的符号。
- `kiana-coding-agent/src/app.rs`：`main.ts`（1,001 行）的编排主体；`src/main.rs` 只是 `cli.ts` 的
  等价入口（`setup_cli()` + `app::run(argv)`）。
- `kiana-env/src/sys/{mod,unix,windows}.rs`：daemon 自身就是 Rust；`sys/` 是平台层（原
  `daemon/src/main.rs` 里的 `#[cfg]` 分支），其余文件与 `daemon/src/*.rs` 一一对应。
- `kiana-tui/src/image.rs` 与 `keys.rs`：分别**再导出** `native/**`（不是“一 TS 一 `.rs`”）。
  `spec/tui-core.md` §2.16/§2.17 的 `NativeClipboard`/`getNativeClipboard()` 的 trait 契约与三态
  语义在 `src/native/mod.rs`（§4.9 已登记），平台实现按 feature 放在 `native/{linux_x11,darwin,win32}.rs`；
  `native-module-path.ts` 的候选路径语义在 `native/module_path.rs`。决策见 §8 第 17 条。
- `kiana-codemode/Cargo.toml` 的 feature 已固定为 `default = []` / `wasm` / `quickjs` / `cloudflare`
  （§8 第 22 条）；`js/prelude.js` 已从 `spec/agent-codemode.md` 附录 A 逐字导出（17,233 字节，
  FNV-1a `8c6c9242`），`runtime/prelude_source.rs` 用 `include_str!` 引用；`cloudflare.rs` 是
  明确的 not supported 占位（§8 第 3 条）。

#### `kiana-json`（阶段 0，5 文件）

```
js_num.rs     ECMAScript 数字 → JSON 文本。
js_object.rs  JS 对象键序与 `JSON.stringify` 等价输出。
js_parse.rs   `JSON.parse` 等价入口，以及「带修复」解析的分层。
js_string.rs  JS 的 `String(v)`、truthiness 与 typeof。
lib.rs        共享 JS 语义 JSON 层。
```

#### `kiana-telemetry`（阶段 0，7 文件）

```
index.rs    index.ts：遥测契约（context / span / store / propagation / schema）。
lib.rs      对应 packages/telemetry：TelemetryContext/Span/Metrics/Store/Propagation 契约与内存实现。
memory.rs   InMemoryTelemetryContext、Recorded* 快照（store-conformance 6 条）。
metrics.rs  metrics.ts：计数器/上下计数/直方图的类型化包装与 schema 定义。
noop.rs     `noop.ts`：`NOOP_TELEMETRY_CONTEXT` / `NOOP_METRICS` / `NOOP_PROPAGATION` 及其类型。
render.rs   render.ts 的文本渲染。
testing.rs  testing/{index,conformance,types}.ts 的 Rust 化：可复用的 store 一致性测试函数。
```

#### `kiana-ai`（阶段 1，155 文件）

```
api/anthropic_messages.rs                api/anthropic-messages.ts。
api/azure_openai_config.rs               api/azure-openai-config.ts。
api/azure_openai_responses.rs            api/azure-openai-responses.ts。
api/bedrock_converse_stream.rs           api/bedrock-converse-stream.ts。
api/classifier_shared.rs                 api/classifier-shared.ts。
api/cloudflare.rs                        api/cloudflare.ts / api/cloudflare-ai-binding.ts。
api/cloudflare_workers_ai_system_one.rs  api/cloudflare-workers-ai-system-one.ts。
api/constrained_sampling.rs              api/constrained-sampling.ts。
api/github_copilot_headers.rs            api/github-copilot-headers.ts。
api/google_generative_ai.rs              api/google-generative-ai.ts。
api/google_shared.rs                     api/google-shared.ts。
api/google_vertex.rs                     api/google-vertex.ts。
api/lazy.rs                              api/lazy.ts。
api/llama_cpp_classify.rs                api/llama-cpp-classify.ts。
api/mistral_conversations.rs             api/mistral-conversations.ts。
api/mod.rs                               api 目录：每个 API 实现一个文件。
api/openai_codex_responses.rs            api/openai-codex-responses.ts。
api/openai_completions.rs                api/openai-completions.ts。
api/openai_decisions.rs                  api/openai-decisions.ts。
api/openai_prompt_cache.rs               api/openai-prompt-cache.ts。
api/openai_responses.rs                  api/openai-responses.ts。
api/openai_responses_shared.rs           api/openai-responses-shared.ts。
api/openrouter_images.rs                 api/openrouter-images.ts。
api/kiana_messages.rs                       api/kiana-messages.ts。
api/simple_options.rs                    api/simple-options.ts。
api/system_one_shared.rs                 api/system-one-shared.ts。
api/transform_messages.rs                api/transform-messages.ts。
api/typesafe_system_one.rs               api/typesafe-system-one.ts。
auth/context.rs                          auth/context.ts。
auth/credential_store.rs                 auth/credential-store.ts。
auth/helpers.rs                          auth/helpers.ts。
auth/mod.rs                              auth 目录：凭据与 OAuth。
auth/oauth/anthropic.rs                  auth/oauth/anthropic.ts。
auth/oauth/callback_server.rs            auth/oauth/callback-server.ts。
auth/oauth/device_code.rs                auth/oauth/device-code.ts。
auth/oauth/github_copilot.rs             auth/oauth/github-copilot.ts。
auth/oauth/kimi_coding.rs                auth/oauth/kimi-coding.ts。
auth/oauth/load.rs                       auth/oauth/load.ts。
auth/oauth/meta.rs                       auth/oauth/meta.ts。
auth/oauth/mod.rs                        auth/oauth 目录：各 provider 的 OAuth 流程。
auth/oauth/openai_chatgpt.rs             auth/oauth/openai-chatgpt.ts。
auth/oauth/openai_codex.rs               auth/oauth/openai-codex.ts。
auth/oauth/openrouter.rs                 auth/oauth/openrouter.ts。
auth/oauth/pkce.rs                       auth/oauth/pkce.ts。
auth/oauth/radius.rs                     auth/oauth/radius.ts。
auth/oauth/xai.rs                        auth/oauth/xai.ts。
auth/resolve.rs                          auth/resolve.ts。
auth/types.rs                            auth/types.ts。
bedrock_provider.rs                      bedrock-provider.ts。
bin/kiana-ai.rs                             packages/ai/src/cli.ts
bun_oauth.rs                             `bun-oauth.ts`：`registerBunOAuthFlows()`——把 10 个 OAuth 流静态登记进 `auth/oauth/load`。
cli.rs                                   cli.ts。
compat.rs                                `compat.ts`（302 行）：旧全局 `kiana-ai` 面的兼容入口。
compat/extension_oauth_types.rs          `compat/extension-oauth-types.ts`：旧扩展 OAuth 提示类型（`OAuthPrompt` / `OAuthAuthInfo` /
env_api_keys.rs                          env-api-keys.ts。
error.rs                                 crate 级错误类型。
event_stream.rs                          `AssistantMessageEventStream`：可推送、可等待结果的事件流。
http/mod.rs                              契约：`HttpClient`/`HttpRequest`/`HttpResponse`/`HttpError`/`ByteStream`/`SharedHttpClient`（§4.9 冻结）。
http/reqwest_client.rs                  默认 `HttpClient` 实现（reqwest + rustls，feature 无需开关）；`FetchFunction` 的 Rust 等价物，provider 适配层不直接依赖 reqwest。
image_models.rs                          image-models.ts / images.ts / images-api-registry.ts。
index.rs                                 index.ts 再导出面。
lib.rs                                   对应 packages/ai（含 ai-auth 与全部 api/provider 实现）。
model_catalog.rs                         model-catalog.ts。
models/cost.rs                           calculateCost 与价格分档。
models/generated.rs                      `models.generated.ts`（309 行，生成物）：`MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS`
models/mod.rs                            models.ts / models-store.ts / models.generated.ts（`model-catalog.ts` 在 crate 根的 `model_catalog`）。
models/provider.rs                       Provider trait 与 CreateProviderOptions。
models/store.rs                          ModelsStore / InMemoryModelsStore。
models/thinking.rs                       clampThinkingLevel / getSupportedThinkingLevels。
providers/all.rs                         providers/all.ts。
providers/amazon_bedrock.rs              providers/amazon-bedrock.ts：`amazon-bedrock` provider 工厂（含 auth/api 映射与模型表引用）。
providers/ant_ling.rs                    providers/ant-ling.ts：`ant-ling` provider 工厂（含 auth/api 映射与模型表引用）。
providers/anthropic.rs                   providers/anthropic.ts：`anthropic` provider 工厂（含 auth/api 映射与模型表引用）。
providers/azure.rs                       providers/azure.ts：`azure` provider 工厂（含 auth/api 映射与模型表引用）。
providers/baseten.rs                     providers/baseten.ts：`baseten` provider 工厂（含 auth/api 映射与模型表引用）。
providers/catalog.rs                     `providers/<id>.models.ts`（42 个分片）+ `providers/data/<id>.json` + `models.generated.ts` 的编译期产物。
providers/cerebras.rs                    providers/cerebras.ts：`cerebras` provider 工厂（含 auth/api 映射与模型表引用）。
providers/cloudflare_ai_gateway.rs       providers/cloudflare-ai-gateway.ts：`cloudflare-ai-gateway` provider 工厂（含 auth/api 映射与模型表引用）。
providers/cloudflare_auth.rs             providers/cloudflare-auth.ts。
providers/cloudflare_stream.rs           providers/cloudflare-stream.ts。
providers/cloudflare_workers_ai.rs       providers/cloudflare-workers-ai.ts：`cloudflare-workers-ai` provider 工厂（含 auth/api 映射与模型表引用）。
providers/compat_schema.rs               providers/compat-schema.ts。
providers/deepseek.rs                    providers/deepseek.ts：`deepseek` provider 工厂（含 auth/api 映射与模型表引用）。
providers/faux.rs                        providers/faux.ts。
providers/fireworks.rs                   providers/fireworks.ts：`fireworks` provider 工厂（含 auth/api 映射与模型表引用）。
providers/github_copilot.rs              providers/github-copilot.ts：`github-copilot` provider 工厂（含 auth/api 映射与模型表引用）。
providers/google.rs                      providers/google.ts：`google` provider 工厂（含 auth/api 映射与模型表引用）。
providers/google_vertex.rs               providers/google-vertex.ts：`google-vertex` provider 工厂（含 auth/api 映射与模型表引用）。
providers/groq.rs                        providers/groq.ts：`groq` provider 工厂（含 auth/api 映射与模型表引用）。
providers/huggingface.rs                 providers/huggingface.ts：`huggingface` provider 工厂（含 auth/api 映射与模型表引用）。
providers/images/mod.rs                  providers/images/register-builtins.ts。
providers/kimi_coding.rs                 providers/kimi-coding.ts：`kimi-coding` provider 工厂（含 auth/api 映射与模型表引用）。
providers/meta.rs                        providers/meta.ts：`meta` provider 工厂（含 auth/api 映射与模型表引用）。
providers/minimax.rs                     providers/minimax.ts：`minimax` provider 工厂（含 auth/api 映射与模型表引用）。
providers/minimax_cn.rs                  providers/minimax-cn.ts：`minimax-cn` provider 工厂（含 auth/api 映射与模型表引用）。
providers/mistral.rs                     providers/mistral.ts：`mistral` provider 工厂（含 auth/api 映射与模型表引用）。
providers/mod.rs                         providers 目录：42 个 builtin provider 工厂（顺序见 `spec/ai-core.md` §2.3.1）+ 目录级辅助模块。
providers/model_schema.rs                providers/model-schema.ts。
providers/moonshotai.rs                  providers/moonshotai.ts：`moonshotai` provider 工厂（含 auth/api 映射与模型表引用）。
providers/moonshotai_cn.rs               providers/moonshotai-cn.ts：`moonshotai-cn` provider 工厂（含 auth/api 映射与模型表引用）。
providers/nvidia.rs                      providers/nvidia.ts：`nvidia` provider 工厂（含 auth/api 映射与模型表引用）。
providers/openai.rs                      providers/openai.ts：`openai` provider 工厂（含 auth/api 映射与模型表引用）。
providers/openai_codex.rs                providers/openai-codex.ts：`openai-codex` provider 工厂（含 auth/api 映射与模型表引用）。
providers/opencode.rs                    providers/opencode.ts：`opencode` provider 工厂（含 auth/api 映射与模型表引用）。
providers/opencode_go.rs                 providers/opencode-go.ts：`opencode-go` provider 工厂（含 auth/api 映射与模型表引用）。
providers/opencode_headers.rs            providers/opencode-headers.ts。
providers/openrouter.rs                  providers/openrouter.ts：`openrouter` provider 工厂（含 auth/api 映射与模型表引用）。
providers/openrouter_api.rs              `providers/openrouter-api.ts`（77 行）：`OpenRouterPricing` / `OpenRouterModel` / `openRouterCost`（`openrouter.ts` 依赖的纯数据模型面）。
providers/qwen_token_plan.rs             providers/qwen-token-plan.ts：`qwen-token-plan` provider 工厂（含 auth/api 映射与模型表引用）。
providers/qwen_token_plan_cn.rs          providers/qwen-token-plan-cn.ts：`qwen-token-plan-cn` provider 工厂（含 auth/api 映射与模型表引用）。
providers/qwen_token_plan_individual.rs  providers/qwen-token-plan-individual.ts：`qwen-token-plan-individual` provider 工厂（含 auth/api 映射与模型表引用）。
providers/radius.rs                      providers/radius.ts：`radius` provider 工厂（含 auth/api 映射与模型表引用）。
providers/radius_config.rs               providers/radius-config.ts。
providers/together.rs                    providers/together.ts：`together` provider 工厂（含 auth/api 映射与模型表引用）。
providers/typesafe.rs                    providers/typesafe.ts：`typesafe` provider 工厂（含 auth/api 映射与模型表引用）。
providers/vercel_ai_gateway.rs           providers/vercel-ai-gateway.ts：`vercel-ai-gateway` provider 工厂（含 auth/api 映射与模型表引用）。
providers/xai.rs                         providers/xai.ts：`xai` provider 工厂（含 auth/api 映射与模型表引用）。
providers/xiaomi.rs                      providers/xiaomi.ts：`xiaomi` provider 工厂（含 auth/api 映射与模型表引用）。
providers/xiaomi_token_plan_ams.rs       providers/xiaomi-token-plan-ams.ts：`xiaomi-token-plan-ams` provider 工厂（含 auth/api 映射与模型表引用）。
providers/xiaomi_token_plan_cn.rs        providers/xiaomi-token-plan-cn.ts：`xiaomi-token-plan-cn` provider 工厂（含 auth/api 映射与模型表引用）。
providers/xiaomi_token_plan_sgp.rs       providers/xiaomi-token-plan-sgp.ts：`xiaomi-token-plan-sgp` provider 工厂（含 auth/api 映射与模型表引用）。
providers/zai.rs                         providers/zai.ts：`zai` provider 工厂（含 auth/api 映射与模型表引用）。
providers/zai_coding_cn.rs               providers/zai-coding-cn.ts：`zai-coding-cn` provider 工厂（含 auth/api 映射与模型表引用）。
session_resources.rs                     `session-resources.ts`：`SessionResourceCleanup` 注册表与 `cleanupSessionResources`。
telemetry.rs                             telemetry.ts。
types.rs                                 types.ts：内容块、消息、模型、usage、请求上下文与流事件。
utils/abort.rs                           utils/abort.ts。
utils/abort_signals.rs                   utils/abort-signals.ts。
utils/diagnostics.rs                     utils/diagnostics.ts。
utils/error_body.rs                      utils/error-body.ts。
utils/estimate.rs                        utils/estimate.ts。
utils/event_stream.rs                    utils/event-stream.ts。
utils/hash.rs                            utils/hash.ts。
utils/headers.rs                         utils/headers.ts。
utils/identity.rs                        utils/identity.ts。
utils/json_parse.rs                      utils/json-parse.ts。
utils/mod.rs                             utils 目录：纯函数与事件流。
utils/model_operations.rs                utils/model-operations.ts。
utils/models_error.rs                    utils/models-error.ts。
utils/node_http_proxy.rs                 utils/node-http-proxy.ts。
utils/oauth_page.rs                      utils/oauth-page.ts。
utils/openrouter_cost.rs                 utils/openrouter-cost.ts。
utils/overflow.rs                        utils/overflow.ts。
utils/provider_env.rs                    utils/provider-env.ts。
utils/provider_retry.rs                  utils/provider-retry.ts。
utils/retry.rs                           utils/retry.ts。
utils/sanitize_unicode.rs                utils/sanitize-unicode.ts。
utils/sleep.rs                           utils/sleep.ts。
utils/streaming_json.rs                  utils/streaming-json.ts。
utils/text.rs                            utils/text.ts。
utils/transcript.rs                      utils/transcript.ts。
utils/typebox_helpers.rs                 utils/typebox-helpers.ts。
utils/uuid.rs                            utils/uuid.ts。
utils/validation.rs                      utils/validation.ts。
utils/version.rs                         utils/version.ts。
```

#### `kiana-mcp`（阶段 1，9 文件）

```
auth_provider.rs   auth-provider.ts。
client.rs          client.ts：McpClient 与 handleMessage 分类顺序。
index.rs           index.ts 再导出面。
jsonrpc.rs         `protocol/jsonrpc.ts`：JSON-RPC 2.0 消息与 MCP 错误类型。
lib.rs             对应 packages/mcp：JSON-RPC、协议类型、stdio/streamable-http/in-memory 传输、OAuth。
oauth.rs           oauth/{callback,discovery,errors,flow,provider,types}.ts。
protocol_types.rs  protocol/types.ts + protocol/content.ts：LATEST_PROTOCOL_VERSION、capabilities、Tool/Resource/Prompt 类型。
testing.rs         testing/index.ts 的内存服务器夹具。
transports.rs      transports/{transport,stdio,in-memory,streamable-http}.ts。
```

#### `kiana-tui`（阶段 1，55 文件）

```
alt_screen.rs                     tui-alt-screen.ts。
alt_screen_search.rs              alt-screen-search.ts。
autocomplete.rs                   autocomplete.ts。
colors.rs                         `colors.ts`：颜色解析、色彩空间变换与 ANSI 序列拼接。
components/box_component.rs       components/box.ts。
components/cancellable_loader.rs  components/cancellable-loader.ts。
components/editor/mod.rs          components/editor.ts（word wrap/paste/autocomplete/visual lines）。
components/h_stack.rs             components/h-stack.ts。
components/image.rs               components/image.ts。
components/input.rs               components/input.ts。
components/loader.rs              components/loader.ts。
components/markdown/mod.rs        components/markdown.ts。
components/mod.rs                 components 目录（48 个组件中属于 tui 包的部分）。
components/mouse_region.rs        components/mouse-region.ts。
components/scroll_view.rs         components/scroll-view.ts。
components/select_list.rs         components/select-list.ts。
components/settings_list.rs       components/settings-list.ts。
components/spacer.rs              components/spacer.ts。
components/stack.rs               components/stack.ts。
components/text_component.rs      components/text.ts。
components/truncated_text.rs      components/truncated-text.ts。
components/v_stack.rs             components/v-stack.ts。
editor_component.rs               `editor-component.ts`（2.5 KB）：`EditorComponent` 接口（扩展可替换的编辑器契约）。
flash.rs                          components/alt-screen-flash.ts。
fuzzy.rs                          fuzzy.ts。
image.rs                          image.ts 的再导出面（`native-platform.ts` 的内容在 `native/`）。
native/mod.rs                     `native/{clipboard.h,napi.h}`：`NativeClipboard`/`ClipboardValue`/`ClipboardError`/`NativeModifiers` trait 与三态语义（§8 第 17 条）。
native/linux_x11.rs               `native/linux/**`：X11 选择区读取（feature `x11`；无 `DISPLAY` 返回 `Unavailable`）。
native/darwin.rs                  `native/darwin/**`：AppKit `NSPasteboard`（feature `darwin`）。
native/win32.rs                   `native/win32/**`：Win32 剪贴板与 VT 输入（feature `win32`）。
native/module_path.rs             `native-module-path.ts`：`.node` 候选路径解析（Rust 只保留 `get_native_module_candidates` 的语义）。
index.rs                          index.ts 的 176 个符号再导出面。
keybindings.rs                    keybindings.ts。
keys.rs                           keys.ts / native-modifiers.ts。
latex.rs                          latex.ts。
layout.rs                         layout.ts。
layout_node.rs                    layout-node.ts。
lib.rs                            对应 packages/tui：tui-core + tui-text + tui-components + terminal 合为一个 crate。
main_screen.rs                    tui-main-screen.ts。
markdown/block.rs                 block tokens。
markdown/inline.rs                inline tokens。
markdown/latex_ext.rs             latex 扩展。
markdown/lexer.rs                 lexer。
markdown/list.rs                  列表渲染。
markdown/mod.rs                   marked 与 marked-cjk-friendly 的等价 lexer/renderer。
markdown/table.rs                 表格渲染。
oklab.rs                          oklab.ts。
program_status.rs                 program-status.ts。
stdin.rs                          stdin-buffer.ts。
terminal.rs                       terminal.ts。
terminal_colors.rs                `terminal-colors.ts` + `colors.ts` 的类型契约。
terminal_image.rs                 terminal-image.ts。
text.rs                           utils.ts / word-navigation.ts / kill-ring.ts / undo-stack.ts。
tui.rs                            `tui.ts`：组件协议、鼠标事件、overlay、`TUI` 门面。
wheel.rs                          wheel-scroll.ts。
```

#### `kiana-codemode`（阶段 1，19 文件）

```
cloudflare.rs              cloudflare.ts（feature cloudflare）。
declarations.rs            declarations.ts。
identifier.rs              identifier.ts。
index.rs                   index.ts 再导出面。
lib.rs                     对应 packages/codemode。
portable.rs                portable.ts。
prelude.rs                 prelude.js（include_str!）。
runtime/execution.rs       runtime/execution.ts。
runtime/host.rs            runtime/host.ts。
runtime/inline.rs          runtime/inline.ts。
runtime/mod.rs             runtime 目录。
runtime/prelude_source.rs  `runtime/prelude-source.ts`（19.4 KB）：沙箱内嵌的 prelude 脚本源码。
runtime/protocol.rs        runtime/protocol.ts。
runtime/remote.rs          runtime/remote.ts。
runtime/vm.rs              runtime/vm.ts。
runtime/worker.rs          runtime/worker.ts。
source.rs                  source.ts。
types.rs                   types.ts。
wasm.rs                    wasm.ts。
```

#### `kiana-agent-core`（阶段 1，8 文件）

```
agent.rs       agent.ts：队列、生命周期。
agent_loop.rs  agent-loop.ts。
identity.rs    index.ts 的 `DEFAULT_AGENT_IDENTITY`。
index.rs       index.ts 再导出面。
lib.rs         对应 packages/agent：Agent 状态机、agent loop、stream fn、事件。
proxy.rs       proxy.ts。
stream_fn.rs   stream-fn.ts：默认 `StreamFn` 的进程级单例（TS 用模块级变量 + `setDefaultStreamFn`）。
types.rs       Agent 跨 crate 契约：状态、事件、工具定义、钩子配置。
```

#### `kiana-chord`（阶段 1，59 文件）

```
cell.rs                           cell.ts。
cluster.rs                        cluster.ts。
connection/feed.rs                connection/feed.ts。
connection/mod.rs                 connection 目录。
connection/relay.rs               connection/relay.ts。
context/index.rs                  `context/index.ts` 的实现体。
context/mod.rs                    `context/index.ts`：`Context` trait、`ContextKey`、`EmptyContext`、`Background/Todo` 组合子、
contract/coverage.rs              contract/coverage.ts。
contract/describe.rs              contract/describe.ts。
contract/mod.rs                   contract 目录。
declare.rs                        declare.ts。
delta/apply_immutable_trusted.rs  delta/apply-immutable-trusted.ts。
delta/diff.rs                     delta/diff.ts。
delta/draft.rs                    delta/draft.ts。
delta/mod.rs                      delta 目录。
delta/pending.rs                  delta/pending.ts。
delta/revision_validator.rs       delta/revision-validator.ts。
delta/tracker.rs                  delta/tracker.ts。
errors.rs                         `errors.ts`：`CHORD_ERROR_CODES`、[`ChordError`] 与构造函数。
host/facets.rs                    host/facets.ts。
host/mod.rs                       host 目录。
host/roots.rs                     host/roots.ts。
host/routes.rs                    host/routes.ts。
host/tables.rs                    host/tables.ts。
index.rs                          index.ts 再导出面。
json.rs                           json.ts。
lib.rs                            对应 packages/chord（package name kiana-chord，lib name chord）。
link.rs                           link.ts。
loader.rs                         loader.ts。
manager.rs                        manager.ts（feature node）。
node.rs                           `node.ts`（385 B）：Node 侧的再导出面（feature `node`）。
object/declarations.rs            object/declarations.ts。
object/invoke.rs                  object/invoke.ts。
object/mod.rs                     object 目录。
object/object_impl.rs             object/object.ts。
object/refs.rs                    object/refs.ts。
object/resource.rs                object/resource.ts。
output.rs                         output.ts。
publish.rs                        publish.ts。
registry.rs                       registry.ts。
reload/commit.rs                  reload/commit.ts。
reload/gate.rs                    reload/gate.ts。
reload/mod.rs                     reload 目录。
reload/prepare.rs                 reload/prepare.ts。
remote.rs                         remote.ts。
schema.rs                         schema.ts。
sqlite/cloudflare.rs              sqlite/cloudflare.ts。
sqlite/database.rs                sqlite/database.ts。
sqlite/migrations.rs              sqlite/migrations.ts。
sqlite/mod.rs                     sqlite 目录（feature sqlite）。
sqlite/node.rs                    sqlite/node.ts。
sqlite/schema.rs                  sqlite/schema.ts。
state.rs                          `state.ts`：`ReplicatedState` 家族、`StateSubscriber`、`Publisher`、`Attached`。
telemetry_sqlite/mod.rs           telemetry-sqlite 目录。
telemetry_sqlite/store.rs         telemetry-sqlite/store.ts。
types.rs                          types.ts。
watch.rs                          `watch.ts`：`WatchLifecycle`、`Watch`、`NativeWatch`、`WatchEnd`、`WatchHandle`。
websocket.rs                      websocket.ts（feature ws）。
wire.rs                           `wire.ts`：链路帧（`Frame`）、`PROTOCOL_VERSION` 与错误编解码。
```

#### `kiana-durable`（阶段 1，84 文件）

```
codemode/codemode.rs            codemode/codemode.ts。
codemode/describe.rs            codemode/describe.ts。
codemode/mod.rs                 codemode 目录。
codemode/models.rs              codemode/models.ts。
codemode/output.rs              codemode/output.ts。
env/decode.rs                   env/decode.ts。
env/line_scan.rs                env/line-scan.ts。
env/mod.rs                      env 目录。
env/node.rs                     env/node.ts。
env/node_watch.rs               env/node-watch.ts。
env/path/mod.rs                 `node:path` 的 `PathFlavor`（posix/win32 选择）与模块文档（§8 第 23 条）。
env/path/posix.rs               `node:path` 的 posix 语义（`join`/`resolve`/`dirname`/`basename`/`is_absolute`/`relative`/`normalize`/`SEP`）。
env/path/win32.rs               `node:path` 的 win32 语义（同上；盘符、反斜杠、UNC）。
harness/agent.rs                harness/agent.ts。
harness/compaction.rs           harness/compaction.ts。
harness/context.rs              harness/context.ts。
harness/define.rs               harness/define.ts。
harness/events.rs               harness/events.ts。
harness/generation.rs           harness/generation.ts。
harness/harness.rs              harness/harness.ts。
harness/inbox.rs                harness/inbox.ts。
harness/json.rs                 harness/json.ts。
harness/live.rs                 harness/live.ts。
harness/mod.rs                  harness 目录（23 个文件）。
harness/partial_changes.rs      harness/partial-changes.ts。
harness/prompt.rs               harness/prompt.ts。
harness/provider.rs             harness/provider.ts。
harness/registry.rs             harness/registry.ts。
harness/scheduler.rs            harness/scheduler.ts。
harness/submissions.rs          harness/submissions.ts。
harness/task_graph.rs           harness/task-graph.ts。
harness/telemetry.rs            harness/telemetry.ts。
harness/tool.rs                 harness/tool.ts。
harness/types.rs                harness/types.ts。
harness/usage.rs                harness/usage.ts。
harness/util.rs                 harness/util.ts。
harness/view.rs                 harness/view.ts。
images/cloudflare.rs            images/cloudflare.ts。
images/exif.rs                  images/exif.ts。
images/mod.rs                   images 目录（feature images）：后端映射与“不做 worker 线程”的理由（§8 第 20 条）。
images/node.rs                  images/node.ts / images/node-worker.ts。
index.rs                        index.ts 再导出面。
lib.rs                          对应 packages/durable。
search.rs                       search.ts。
session/forks.rs                session/forks.ts。
session/mod.rs                  session 目录。
session/session.rs              session/session.ts。
session/transaction.rs          session/transaction.ts。
storage/indexeddb.rs            storage/indexeddb.ts。
storage/jsonl/mod.rs            storage/jsonl 目录。
storage/jsonl/node.rs           storage/jsonl/node.ts。
storage/jsonl/storage.rs        storage/jsonl/storage.ts。
storage/memory.rs               storage/memory.ts。
storage/mod.rs                  storage 目录。
storage/scan.rs                 storage/scan.ts。
storage/sqlite/cloudflare.rs    storage/sqlite/cloudflare.ts。
storage/sqlite/database.rs      storage/sqlite/database.ts。
storage/sqlite/migrations.rs    storage/sqlite/migrations.ts。
storage/sqlite/mod.rs           storage/sqlite 目录（feature sqlite）。
storage/sqlite/node.rs          storage/sqlite/node.ts。
storage/sqlite/schema.rs        storage/sqlite/schema.ts。
storage/sqlite/storage.rs       storage/sqlite/storage.ts。
testing/assertions.rs           testing/assertions.ts。
testing/env_conformance.rs      testing/env-conformance.ts。
testing/mod.rs                  testing 目录。
testing/runner.rs               testing/runner.ts。
testing/storage_benchmark.rs    testing/storage-benchmark.ts。
testing/storage_conformance.rs  testing/storage-conformance.ts。
testing/types.rs                testing/types.ts。
timers.rs                       timers.ts。
tool_search/mod.rs              tool-search 目录。
tool_search/tool_search.rs      tool-search/tool-search.ts。
tools/bash.rs                   tools/bash.ts。
tools/edit.rs                   tools/edit.ts。
tools/edit_diff.rs              tools/edit-diff.ts。
tools/env.rs                    tools/env.ts。
tools/file_mutation_queue.rs    tools/file-mutation-queue.ts。
tools/image.rs                  tools/image.ts / tools/image-processor.ts。
tools/mod.rs                    tools 目录。
tools/path_utils.rs             tools/path-utils.ts。
tools/read.rs                   tools/read.ts。
tools/write.rs                  tools/write.ts。
truncate.rs                     truncate.ts。
types.rs                        types.ts / ids.ts / documents.ts / entries.ts / tasks.ts / errors.ts。
```

#### `kiana-env`（阶段 1，15 文件）

```
decode.rs       decode.rs。
errors.rs       errors.rs。
exec.rs         exec.rs。
frame.rs        frame.rs。
fs.rs           fs.rs。
list.rs         list.rs。
main.rs         对应 packages/env/daemon（原生 Rust 二进制 kiana-env）：远端执行守护进程。
output.rs       output.rs。
pty.rs          pty.rs。
scan.rs         scan.rs。
sys/mod.rs      `sys/`：平台层（unix / windows）。
sys/unix.rs     unix 平台层。
sys/windows.rs  windows 平台层。
watch.rs        watch.rs。
window.rs       window.rs。
```

#### `kiana-env-client`（阶段 2，9 文件）

```
connection.rs  connection.ts：daemon 帧、hello、ping（5s）、lost 判定。
errors.rs      errors.ts。
index.rs       index.ts 再导出面。
lib.rs         对应 packages/env 的 TS 客户端：SSH 部署、RemoteExecutionEnv、watch、terminal。
list_files.rs  list-files.ts。
remote_env.rs  remote-env.ts。
ssh.rs         ssh.ts：daemon 部署与 ~/.kiana/mobile/tools/kiana-env。
terminal.rs    terminal.ts。
watch.rs       watch.ts。
```

#### `kiana-core`（阶段 2，33 文件）

```
abort.rs             utils/abort.ts。
ansi.rs              utils/ansi.ts。
changelog.rs         utils/{changelog,version-check,windows-self-update,wsl,deprecation,kiana-user-agent}.ts。
child_process.rs     utils/child-process.ts。
clipboard.rs         utils/{clipboard,clipboard-command,open-browser}.ts。
config.rs            config.ts（656 行，46 个导出）：安装方式检测与自更新命令（`detectInstallMethod`/`getSelfUpdateCommand`/`getSelfUpdateUnavailableInstruction`/`getUpdateInstruction`/`detectInstallChange`/`InstallMethod`/`SelfUpdateCommand`/`InstallChange`）、包资源路径（`getPackageDir`/`getThemesDir`/`getExportTemplateDir`/`getDocsPath`/`getExamplesPath`/`getChangelogPath`/`getReadmePath`/`getPackageJsonPath`/`getInteractiveAssetsDir`/`getBundledInteractiveAssetPath`/`findNodePackageDir`）、codemode worker 与 wasm 路径（`getQuickJSWasmPath`/`setEmbeddedQuickJSWasmPath`/`getCodemodeWorkerSpecifier`/`resolveCodemodeWorkerSpecifier`）、agent 目录族（`getAgentDir`/`getCustomThemesDir`/`getModelsPath`/`getAuthPath`/`getSettingsPath`/`getToolsDir`/`getBinDir`/`getPromptsDir`/`getSessionsDir`/`getDebugLogPath`/`expandTildePath`/`getShareViewerUrl`）、常量（`PACKAGE_NAME`/`APP_NAME`/`APP_TITLE`/`CONFIG_DIR_NAME`/`VERSION`/`ENV_AGENT_DIR`/`ENV_SESSION_DIR`/`isBunBinary`/`isBunRuntime`/`isBundledNode`）与 `normalizePath`/`resolvePath`/`canonicalizePath`。逐条语义见 `spec/ca-cli.md` §2.6。
diagnostics.rs       `core/diagnostics.ts`：`ResourceCollision` / `ResourceDiagnostic`。
event_bus.rs         core/event-bus.ts。
exec.rs              core/exec.ts。
frontmatter.rs       utils/frontmatter.ts。
fs_watch.rs          utils/fs-watch.ts。
git.rs               utils/git.ts。
html.rs              utils/html.ts。
http_dispatcher.rs   core/http-dispatcher.ts。
image_process.rs     utils/{image-process,image-convert,image-resize-core,image-resize,image-resize-worker,exif-orientation,photon,clipboard-image,tool-result-images}.ts。
json.rs              utils/json.ts（stripJsonComments/stripBom）。
lib.rs               coding-agent 基础层：config 路径/常量、utils/*、启动期基础设施。
management_http.rs   utils/management-http.ts。
mcp_servers.rs       core/mcp-servers.ts（mcp.json 校验、exposure、注册表）。
mime.rs              utils/mime.ts。
output_files.rs      utils/output-files.ts。
output_guard.rs      core/output-guard.ts。
paths.rs             utils/paths.ts。
kiana_manifest.rs       `core/kiana-manifest.ts`：`KianaManifest` / `read_kiana_manifest`（`package.json` 的 `kiana` 字段校验）。
shell.rs             utils/shell.ts。
slash_commands.rs    `core/slash-commands.ts`：`BUILTIN_SLASH_COMMANDS`（24 项，顺序即 UI 顺序）与
sleep.rs             utils/sleep.ts。
source_info.rs       `core/source-info.ts` 的**完整纯函数面**（`kiana-core` 持有，`kiana-resources` 只再导出）。
syntax_highlight.rs  utils/syntax-highlight.ts（highlight.js：**21 种急切注册** + `loadAllHighlightLanguages` 按需加载全量；`renderHighlightedHtml` 的 scope→格式映射必须逐字）。
text.rs              utils/text.ts。
timings.rs           `core/timings.ts`：`reset_timings` / `time` / `print_timings`（`KIANA_TIMING=1` 启用）。
tools_manager.rs     utils/tools-manager.ts。
zip.rs               utils/zip.ts。
```

#### `kiana-session-core`（阶段 2，10 文件）

```
cache_stats.rs        core/cache-stats.ts。
index.rs              core/index.ts 的数据层再导出面。
lib.rs                对应 coding-agent 会话数据层。
messages.rs           core/messages.ts。
nested_tool_calls.rs  core/nested-tool-calls.ts。
session_cwd.rs        core/session-cwd.ts。
session_export.rs     core/session-export.ts。
session_manager.rs    core/session-manager.ts。
usage_totals.rs       core/usage-totals.ts。
virtual_models.rs     core/virtual-models.ts。
```

#### `kiana-config`（阶段 3，36 文件）

```
auth/lock.rs                 `proper-lockfile` 等价物：目录锁 + 10×20 ms 同步重试 + Windows EPERM 额外重试。
auth/mod.rs                  `core/{auth-storage,runtime-credentials,models-store}.ts` 的内部拆分。
auth/models_store.rs         `FileModelsStore` / `InMemoryCodingAgentModelsStore`。
auth/runtime_credentials.rs  `RuntimeCredentials`（按 provider 的凭证操作串行化）。
auth/storage.rs              `AuthStorage` / `FileAuthStorageBackend` / `ReadOnlyAuthStorage` / `InMemoryAuthStorage`。
auth_guidance.rs             core/auth-guidance.ts。
cache/mod.rs                 `core/cache-warmer.ts` 与 `core/cache-stats.ts`（后者实现在 `kiana-session-core`）。
cache/warmer.rs              `CacheWarmer` + 三个格式化函数。
client_identity.rs           core/client-identity.ts。
defaults.rs                  `core/defaults.ts`、`core/experimental.ts`、`core/radius.ts`、`core/telemetry.ts`。
index.rs                     kiana-config 的再导出面。
keybindings/key_id.rs        `KeyIdSchema` 的手写校验（ECMA 负向前瞻在 Rust `regex` 不可用）。
keybindings/mod.rs           `core/{keybindings,keybindings-schema,key-id-schema}.ts`。
keybindings/schema.rs        `KeybindingsSchema` 构建与迁移。
lib.rs                       对应 ca-config-models 单元：settings / auth / models / trust / keybindings / cache。
models/catalog_http.rs       `requestCatalog` / `readCatalog` / `parseCatalog`（基于 reqwest）。
models/composer.rs           provider 四层合成（models.json → extension → OAuth modifyModels → modelOverrides）。
models/config_file.rs        `ModelConfig` / `ModelsConfigSchema` / `strip_json_comments` / `format_validation_path`。
models/mod.rs                `core/{model-config,provider-composer,model-runtime,model-registry,model-resolver,remote-catalog-provider,models-store}.ts` 的拆分。
models/registry.rs           `ModelRegistry` 门面。
models/remote_catalog.rs     `withRemoteCatalog` / `withPersonalizedCatalog`。
models/resolver.rs           引用解析 / scope / CLI / initial / restore（`selectReference` 6 步 + `selectFuzzy`）。
models/runtime.rs            `ModelRuntime`（实现 `kiana_ai::models::Models`）。
models/virtual_models.rs     虚拟模型定义（`kiana-session-core::virtual_models` 的类型在此接入）。
resolve_config_value.rs      core/resolve-config-value.ts（$ENV/!cmd/$$/$!）。
settings/compaction.rs       compaction token 解析与校验。
settings/defaults.rs         `SETTINGS_DEFAULTS`（反序列化基底）。
settings/diagnostics.rs      `collect` / `deduplicate`（settings 诊断）。
settings/migrations.rs       四个 settings 迁移。
settings/mod.rs              `core/{settings-manager,settings-schema,settings-defaults,settings-diagnostics}.ts` 门面。
settings/schema.rs           `SettingsSchema` 对应的 Rust 结构体 + 校验。
settings/storage.rs          `FileSettingsStorage` / `InMemorySettingsStorage` / `SettingsStorage` trait。
settings/tools.rs            `getToolListError` / `applyToolModifiers` / `mergeDefaultTools` / `resolveDefaultTools`。
trust/manager.rs             `ProjectTrustStore` / `trust.json` / `TRUST_REQUIRING_*`。
trust/mod.rs                 `core/{trust-manager,project-trust}.ts`。
trust/project_trust.rs       `resolve_project_trusted` / prompt / selector helpers。
```

#### `kiana-theme`（阶段 3，8 文件）

```
assets.rs            dark.json / light.json（include_str! 原样保留）。
index.rs             主题相关再导出面。
lib.rs               对应 modes/interactive/theme：Theme 类、system 主题、主题 JSON 校验；`highlightCode` 委托 `kiana-core::syntax_highlight`（不在此 crate 重复实现）。
system_theme.rs      system-theme.ts（由终端颜色生成）。
theme.rs             theme.ts（1086 行）：Theme 类、加载、全局单例、TUI 主题工厂、highlightCode。
theme_controller.rs  theme-controller.ts。
theme_schema.rs      theme-schema.ts。
theme_tokens.rs      theme-tokens.ts（56 个 token 元数据，顺序即 schema 顺序）。
```

#### `kiana-compaction`（阶段 3，5 文件）

```
branch_summarization.rs  core/compaction/branch-summarization.ts。
compaction.rs            core/compaction/compaction.ts（压缩切点、prompt、completeSummarization）。
index.rs                 index.ts 再导出面。
lib.rs                   对应 core/compaction：切点选择、摘要生成、分支摘要。
utils.rs                 core/compaction/utils.ts（FileOperations/序列化）。
```

#### `kiana-export`（阶段 4，5 文件）

```
ansi_to_html.rs   core/export-html/ansi-to-html.ts。
assets.rs         template.html/template.css/template.js 与 vendor/{marked,highlight.js}（include_str!）。
index.rs          core/export-html/index.ts：占位符单次替换、base64 会话数据。
lib.rs            对应 core/export-html：单文件自包含 HTML 导出（模板/CSS/vendored JS 原样嵌入）。
tool_renderer.rs  core/export-html/tool-renderer.ts。
```

#### `kiana-ext-api`（阶段 4，8 文件）

```
events.rs         SessionStartEvent/ProjectTrustEvent/ExtensionEvent。
index.rs          再导出面。
lib.rs            对应 core/extensions/types.ts 的公开契约。
system_prompt.rs  BuildSystemPromptOptions/NormalizedBuildSystemPromptOptions 声明。
tool_result.rs    `BashResult` / `BashOperations` / `ExecOptions` / `ExecResult` 的跨 crate 契约。
tool_types.rs     ToolDefinition/ToolLoadout/ToolNamespace/ToolRenderContext 与各工具 Input/Details。
types.rs          core/extensions/types.ts（2308 行，唯一权威）。
ui.rs             ExtensionUIContext 与对话框/组件工厂选项。
```

#### `kiana-tools`（阶段 4，26 文件）

```
bash.rs                 core/tools/bash.ts。
bash_executor.rs        core/bash-executor.ts。
definition.rs           core/tools/tool-definition-wrapper.ts。
edit.rs                 core/tools/edit.ts。
edit_diff.rs            core/tools/edit-diff.ts。
file_mutation_queue.rs  core/tools/file-mutation-queue.ts。
find.rs                 core/tools/find.ts。
grep.rs                 core/tools/grep.ts。
index.rs                core/tools/index.ts。
lib.rs                  对应 ca-tools-compaction-export 的工具部分。
ls.rs                   core/tools/ls.ts。
output_accumulator.rs   core/tools/output-accumulator.ts。
path_utils.rs           core/tools/path-utils.ts。
powershell.rs           core/tools/powershell.ts。
read.rs                 core/tools/read.ts。
render_utils.rs         core/tools/render-utils.ts。
renderers/bash.rs       core/tools/renderers/bash.ts。
renderers/edit.rs       core/tools/renderers/edit.ts。
renderers/find.rs       core/tools/renderers/find.ts。
renderers/grep.rs       core/tools/renderers/grep.ts。
renderers/ls.rs         core/tools/renderers/ls.ts。
renderers/mod.rs        core/tools/renderers 目录。
renderers/read.rs       core/tools/renderers/read.ts。
renderers/write.rs      core/tools/renderers/write.ts。
truncate.rs             core/tools/truncate.ts。
write.rs                core/tools/write.ts。
```

#### `kiana-ui-kit`（阶段 4，11 文件）

```
auth_url.rs          auth-url.ts。
bordered_loader.rs   bordered-loader.ts。
countdown_timer.rs   countdown-timer.ts。
diff.rs              diff.ts（行级 diff 着色）。
dynamic_border.rs    dynamic-border.ts。
index.rs             再导出面。
keybinding_hints.rs  keybinding-hints.ts（keyHint/keyText）。
lib.rs               无业务依赖的共用渲染小件：供 kiana-tools 渲染器与 kiana-interactive 复用。
kiana_logo.rs           kiana-logo.ts。
themed_text.rs       themed-text.ts。
visual_truncate.rs   visual-truncate.ts（VisualLinePreview）。
```

#### `kiana-resources`（阶段 5，35 文件）

```
bug_report.rs              core/bug-report.ts + bug-report-upload.ts。
crash_log.rs               core/crash-log.ts。
diagnostics.rs             资源诊断类型 `ResourceCollision` / `ResourceDiagnostic` 的再导出面。
footer_data_provider.rs    `core/footer-data-provider.ts`：`FooterDataProvider` 与 `findGitPaths`。
index.rs                   再导出面。
lib.rs                     对应 ca-resources：包管理、资源加载、skills、prompt 模板、系统提示、MCP 配置。
loader/context_files.rs    `load_context_file_from_dir` / `load_project_context_files` / `find_shadowed_context_file`。
loader/mod.rs              `core/resource-loader.ts`（1322 行）：`DefaultResourceLoader` / `ResourceLoader` trait / reload 步骤机。
loader/source_info_map.rs  `find_source_info_for_path` / `get_default_source_info_for_path` / `merge_paths`。
loader/themes.rs           `load_themes` / `load_themes_from_dir` / `load_theme_from_file`（调 `kiana-theme`）。
loader/warnings.rs         `HOST_PROVIDED_EXTENSION_PACKAGES` / `describe_package_install_failure` / `format_package_install_warning` / `omit_replaced_extensions`。
manifest.rs                `KianaManifest` / `read_kiana_manifest` 的再导出面。
package/concurrency.rs     `run_with_concurrency`（不取消其余任务）。
package/discover.rs        `collect_files` / `collect_skill_entries` / auto entries / ancestor agents。
package/exec.rs            `run_command` / `run_command_capture` / `run_command_sync` / spawn 辅助（`kiana_core::exec` 之上）。
package/git.rs             `install_git` / `ensure_git_ref` / marker / prune / remote head。
package/mod.rs             `core/package-manager.ts`（2791 行）：`DefaultPackageManager` / `PackageManager` trait / 公开类型。
package/npm.rs             `npmCommand` / 安装参数 / `get_latest_npm_version` / 全局根。
package/parse.rs           `parse_source` / `parse_npm_spec` / git URL 解析 / identity。
package/paths.rs           安装根目录 / `get_temporary_dir` / `resolve_managed_path` / gitignore。
package/patterns.rs        `apply_patterns` / `matches_any_pattern` / overrides / `expand_package_glob`。
package/resolve.rs         `resolve` / `resolve_package_sources` / dedupe / `to_resolved_paths`。
package/settings_io.rs     settings 里的 source 增删与规范化。
package/types.rs           `PathMetadata`（`kiana_core::source_info::PathMetadata` 的 re-export）/ `ResolvedResource` / `ResolvedPaths` / `ProgressEvent`。
prompts/args.rs            `parse_command_args`。
prompts/expand.rs          `expand_prompt_template`。
prompts/mod.rs             `core/prompt-templates.ts`：`PromptTemplate` / `load_prompt_templates`。
prompts/substitute.rs      `substitute_args`（手写状态机，不用 regex；见 `ca-resources.md` §8.4）。
skills/discover.rs         `load_skills_from_dir_internal` / `load_skill_from_file`。
skills/mod.rs              `core/skills.ts`：`Skill` / `SkillFrontmatter` / `LoadSkillsResult` / `load_skills`。
skills/prompt.rs           `format_skills_for_prompt` / `escape_xml`。
skills/validate.rs         `validate_name` / `validate_description`（JS UTF-16 长度）。
source_info.rs             `core/source-info.ts` 的**再导出面**。
system_prompt.rs           core/system-prompt.ts。
timings.rs                 `KIANA_TIMING` 启动计时的再导出面。
```

#### `kiana-extensions`（阶段 5，31 文件）

```
engine/limits.rs            内存/栈/中断预算与 100 ms 看门狗（`interrupted` 标志 + `set_interrupt_handler`）。
engine/mod.rs               `JsEngine`：rquickjs 运行时、单线程 `with_js` 入口、Promise 协议、限制与关闭顺序。
engine/promise.rs           手写 Promise 协议：JS 建 Promise、Rust 存 waiter；`settle`/reject 与未处理拒绝。
engine/shutdown.rs          `Persistent` 逆序关闭与代际重载（设计文档 §2.8；顺序错误会 abort 进程）。
engine/timers.rs            定时器堆与 `queueMicrotask`（§2.5）。
index.rs                    core/extensions/index.ts 再导出面。
lib.rs                      对应 ca-extensions 的运行时部分：加载器、ExtensionRunner、事件派发、虚拟模块、JS 引擎。
loader/cjs.rs               真 CJS：IIFE + 预登记 `module.exports`（`spec/design-extension-runtime.md` §3.5）。
loader/discovery.rs         `resolveExtensionEntries` / `discoverExtensionsInDir` / manifest 解析（loader.ts 的发现部分）。
loader/factory_cache.rs     扩展工厂缓存：键 `(path, cwd, generation)`；`clear_extension_cache` / `use_extension_cache_cwd` / `is_current_cache_token`。
loader/js_host.rs           rquickjs 宿主：`__kiana_host.*` 原语、`Function::call` 的 `This` 纪律、错误转换。
loader/loader_impl.rs       `rquickjs::Loader` 实现：`Module::declare`、`?v=<gen>-<seq>` 去缓存、真 CJS `require`。
loader/mod.rs               `core/extensions/loader.ts`（879 行）的门面：发现、缓存、策略选择。
loader/resolver.rs          模块解析：Node ESM + jiti 顺序（`spec/design-extension-runtime.md` §3.8），`oxc_resolver` 驱动。
loader/transpile.rs         TS 去类型（oxc）：`spec/design-extension-runtime.md` §3.2。
project_trust.rs            emit_project_trust_event 与 ProjectTrustContext（types.ts 的声明在 kiana-ext-api）。
runner/boundary.rs          `emit_boundary`（最复杂的派发）：`agent_before_settle` 与 `end > continue > undefined` 三方裁决。
runner/commands.rs          `invocation_name` 去重、冲突、`resolve_registered_commands`。
runner/context.rs           `create_context` / `create_tool_context` / `create_command_context`（18 个成员，读时求值）。
runner/dispatch.rs          泛型 `emit` 与 12 个专用派发器（`emitBoundary…emitInput`）。
runner/keybindings.rs       18 个保留 id + `build_builtin_keybindings`。
runner/mcp_registration.rs  `registerMcpServer` 校验 + 所有权。
runner/mod.rs               `core/extensions/runner.ts`（1565 行）门面：`ExtensionRunner`、`bind_core`/`bind_command_context`/`set_ui_context`、`invalidate`。
runner/noop_ui.rs           `no_op_ui_context`（print/json 模式的 UI 上下文）。
runner/renderers.rs         `resolve_tool_renderers`：tool / message / entry / markdown 四类渲染器解析链。
runner/ui_prompt.rs         `with_ui_prompt` / `emit_ui_prompt_event`。
ui/adapter.rs               `JsComponent`：`Persistent<Object>` + `Weak<JsEngine>`，实现 `kiana_tui::Component`。
ui/mod.rs                   扩展 UI 适配层：把 `ExtensionUIContext` 接到三种模式。
ui/print_json.rs            print/json 模式的 no-op UI 上下文。
virtual_modules.rs          core/extensions/virtual-modules.ts（22 个虚拟模块映射）。
wrapper.rs                  core/extensions/wrapper.ts。
```

#### `kiana-session`（阶段 6，TS 映射 6 文件；Kiana 扩展模块另列）

```
agent_session.rs           core/agent-session.ts（4458 行）：门面、钩子、队列、重试、压缩、bash、模型切换、工具注册表。
agent_session_runtime.rs   core/agent-session-runtime.ts：new/switch/fork/import/dispose。
agent_session_services.rs  core/agent-session-services.ts。
index.rs                   core/index.ts 的会话层再导出面。
lib.rs                     对应 ca-session：AgentSession、AgentSessionRuntime/Services、SDK 工厂。
sdk.rs                     core/sdk.ts：createAgentSession（依赖 AgentSession，故放在此 crate 而非 kiana-resources）。
durable_store.rs           background journal 与 Work 账本共用的 per-session durable 存储（目录命名、owner 标记、Session/Tx 提交）。
subagent/                  子代理运行时、AgentDefinition 执行、Work envelope、durable Work 账本（work_store.rs）与顺序 stage 流水线（stages.rs）。
background/                后台任务管理器、durable journal 适配与输出文件。
continuity.rs              执行所有权、checkpoint、background 与 Work 账本接线。
```

#### `kiana-builtin-extensions`（阶段 6，27 文件）

```
codemode/execute.rs   extensions/codemode/execute.ts。
codemode/index.rs     extensions/codemode/index.ts。
codemode/mod.rs       `extensions/codemode/`（5 个文件）。
codemode/renderer.rs  extensions/codemode/renderer.ts。
codemode/tool.rs      extensions/codemode/tool.ts。
codemode/worker.rs    extensions/codemode/worker.ts。
index.rs              `extensions/index.ts`：`builtInExtensions` 清单（顺序与 `replaceable` 语义固定）。
lib.rs                对应 ca-builtin-extensions：mcp / codemode / llama.cpp / tool-search 四个内置扩展。
llama/client.rs       extensions/llama/client.ts。
llama/huggingface.rs  extensions/llama/huggingface.ts。
llama/index.rs        extensions/llama/index.ts。
llama/mod.rs          `extensions/llama/`（5 个文件）。
llama/provider.rs     extensions/llama/provider.ts。
llama/ui.rs           extensions/llama/ui.ts。
mcp/cli.rs            extensions/mcp/cli.ts。
mcp/config.rs         extensions/mcp/config.ts。
mcp/index.rs          extensions/mcp/index.ts。
mcp/log.rs            extensions/mcp/log.ts。
mcp/mod.rs            `extensions/mcp/`（9 个文件）。
mcp/oauth.rs          extensions/mcp/oauth.ts。
mcp/resources.rs      extensions/mcp/resources.ts。
mcp/runtime.rs        extensions/mcp/runtime.ts。
mcp/tools.rs          extensions/mcp/tools.ts。
mcp/ui.rs             extensions/mcp/ui.ts。
tool_search/index.rs  extensions/tool-search/index.ts。
tool_search/mod.rs    `extensions/tool-search/`（2 个文件）。
tool_search/tool.rs   extensions/tool-search/tool.ts。
```

#### `kiana-interactive`（阶段 7，51 文件）

```
assets/mod.rs                             modes/interactive/assets/。
bug_report.rs                             modes/interactive/bug-report.ts。
chat_viewport.rs                          modes/interactive/chat-viewport.ts。
components/armin.rs                       components/armin.ts。
components/assistant_message.rs           components/assistant-message.ts。
components/bash_execution.rs              components/bash-execution.ts（bash 执行的行内展示壳）。
components/branch_summary_message.rs      components/branch-summary-message.ts。
components/compaction_summary_message.rs  components/compaction-summary-message.ts。
components/config_selector.rs             components/config-selector.ts。
components/custom_editor.rs               components/custom-editor.ts（扩展可替换的编辑器）。
components/custom_entry.rs                components/custom-entry.ts。
components/custom_message.rs              components/custom-message.ts。
components/earendil_announcement.rs       components/earendil-announcement.ts。
components/easter_egg_3d.rs               components/easter-egg-3d.ts。
components/extension_editor.rs            components/extension-editor.ts。
components/extension_input.rs             components/extension-input.ts。
components/extension_selector.rs          components/extension-selector.ts。
components/first_time_setup.rs            components/first-time-setup.ts。
components/footer.rs                      components/footer.ts。
components/index.rs                       components/index.ts。
components/login_dialog.rs                components/login-dialog.ts。
components/markdown_transform.rs          components/markdown-transform.ts。
components/mermaid.rs                     components/mermaid.ts。
components/mod.rs                         `modes/interactive/components` 目录（48 个文件）。
components/model_selector.rs              components/model-selector.ts。
components/oauth_selector.rs              components/oauth-selector.ts。
components/radius_login_selector.rs       components/radius-login-selector.ts。
components/scoped_models_selector.rs      components/scoped-models-selector.ts。
components/session_selector.rs            components/session-selector.ts。
components/session_selector_search.rs     components/session-selector-search.ts。
components/settings_selector.rs           components/settings-selector.ts。
components/settings_submenu.rs            components/settings-submenu.ts。
components/show_images_selector.rs        components/show-images-selector.ts。
components/skill_invocation_message.rs    components/skill-invocation-message.ts。
components/status_indicator.rs            components/status-indicator.ts。
components/theme_selector.rs              components/theme-selector.ts。
components/thinking_selector.rs           components/thinking-selector.ts。
components/tool_execution.rs              components/tool-execution.ts。
components/tree_selector.rs               components/tree-selector.ts。
components/trust_selector.rs              components/trust-selector.ts。
components/user_message.rs                components/user-message.ts。
components/user_message_selector.rs       components/user-message-selector.ts。
external_editor.rs                        modes/interactive/external-editor.ts。
index.rs                                  modes/index.ts 的交互侧再导出。
interactive_mode.rs                       modes/interactive/interactive-mode.ts。
lib.rs                                    对应 ca-interactive-mode + ca-interactive-components。
model_catalog_refresh.rs                  modes/interactive/model-catalog-refresh.ts。
model_search.rs                           modes/interactive/model-search.ts。
program_status_reporter.rs                modes/interactive/program-status-reporter.ts。
session_share.rs                          modes/interactive/session-share.ts。
tui_renderer.rs                           modes/interactive/tui-renderer.ts。
```

#### `kiana-rpc-print`（阶段 7，8 文件）

```
index.rs       modes/index.ts 再导出面。
json_event.rs  modes/json-event.ts。
jsonl.rs       modes/rpc/jsonl.ts。
lib.rs         对应 ca-rpc-print：print/json 单次运行、RPC 服务端与客户端、JSONL。
print_mode.rs  modes/print-mode.ts。
rpc_client.rs  modes/rpc/rpc-client.ts。
rpc_mode.rs    modes/rpc/rpc-mode.ts。
rpc_types.rs   modes/rpc/rpc-types.ts。
```

#### `kiana-coding-agent`（阶段 8，22 文件）

```
app.rs                   main.ts（1001 行）：启动编排、会话选择、模式分发、迁移调用。
bin/kiana-rpc.rs            `packages/coding-agent/src/rpc-entry.ts`（13 行）：设置进程标题/标记后转 `--mode rpc`。
bun/mod.rs               `bun/**` 的等价启动逻辑（`bun/{cli,runtime-setup,restore-sandbox-env,sandbox-env-setup}.ts`；`quickjs-wasm.d.ts` 是类型垫片）。原生二进制下全部为 no-op，但 `restoreSandboxEnv` 的 `/proc/self/environ` 语义见 `spec/ca-cli.md` §2.20。
cli/args.rs              `cli/args.ts`（479 行）：`Args` / `Diagnostic` / `parse_args` / `print_help` / thinking 校验。
cli/auth_check.rs        `cli/auth-check.ts`：`checkProviderAuth` / `createAuthCheckModelRuntime`。
cli/auth_command.rs      `cli/auth-command.ts`：`parseAuthCommand` / 文案 / `getAuthCredential`。
cli/config_selector.rs   `cli/config-selector.ts`：`kiana config` 选择器。
cli/credential_print.rs  `cli/credential-print.ts`：`print-api-key` / `print-bearer-token`。
cli/file_processor.rs    `cli/file-processor.ts`：`@file` 参数处理。
cli/initial_message.rs   `cli/initial-message.ts`：初始消息拼接。
cli/list_models.rs       `cli/list-models.ts`：`--list-models` 表格。
cli/mod.rs               `cli/**`（12 个文件）：参数解析、认证子命令、启动期 UI。
cli/project_trust.rs     `cli/project-trust.ts`：启动期 `ProjectTrustContext`（选择器版）。
cli/session_picker.rs    `cli/session-picker.ts`：`--resume` 选择器。
cli/setup.rs             `cli/setup.ts`（13 行）：`setupCli()`——进程标题、`KIANA_CODING_AGENT`/`AI_AGENT` 标记、HTTP dispatcher、警告抑制。
cli/startup_ui.rs        `cli/startup-ui.ts`（253 行）：启动期 TUI、首次设置。
lib.rs                   对应 ca-cli：启动编排、参数解析、CLI 子命令、迁移、SDK 导出面；二进制 `kiana` 与 `kiana-rpc`。
main.rs                  `packages/coding-agent/src/cli.ts`（6 行；bin.kiana = dist/bundle/cli.js）——只调 `setup_cli()` 与 `app::run`。
migrations.rs            migrations.ts。
package_manager_cli.rs   package-manager-cli.ts：install/remove/update/list/config（kiana config 入口）。
sdk_exports.rs           index.ts：SDK 公共导出面（498 行，只 re-export）。
source_resolver.rs       experimental/source-resolver.ts。
```

#### `kiana-evals`（阶段 8，7 文件）

```
cli.rs      evals/src/cli.ts。
docker.rs   evals/src/docker.ts。
harness.rs  evals/src/harness.ts。
lib.rs      对应 packages/evals（私有）：docker 化评测运行器与报告。
main.rs     `[[bin]] name = "kiana-evals"` 的入口。TS 只有一个 `cli.ts`（既是入口又是主逻辑），Rust 侧入口只做参数转发，逻辑在 `cli.rs`；见 §5 特殊映射。
plan.rs     evals/src/plan.ts。
report.rs   evals/src/report.ts。
```

---

## 6. 二进制目标

TS 侧只有两个 `bin`（外加一个独立 Rust daemon、一个私有 eval CLI）：

| 二进制 | TS 源 | Rust 位置 | 说明 |
|---|---|---|---|
| `kiana` | `packages/coding-agent` `bin.kiana = dist/bundle/cli.js` → `src/cli.ts` + `main.ts` | `crates/kiana-coding-agent/src/main.rs`（`[[bin]] name = "kiana"`） | 主 CLI：`--mode` 分发到 interactive/print/json/rpc；子命令 `config`/`package`/`auth`/`migrate` 等 |
| `kiana-rpc` | `packages/coding-agent` `exports["./rpc-entry"] = dist/bundle/rpc-entry.js` → `src/rpc-entry.ts` | `crates/kiana-coding-agent/src/bin/kiana-rpc.rs`（`[[bin]] name = "kiana-rpc"`） | 等价于 `kiana --mode rpc`，但**先**设置 `process.title = "kiana-rpc"`、`KIANA_CODING_AGENT=true`、`AI_AGENT=kiana`，并静默 `process.emitWarning`；`RpcClient` 默认 spawn `node dist/cli.js --mode rpc`，Rust 侧改为 spawn 自身可执行文件 + `--mode rpc`（`cliPath` 缺省值从 `dist/cli.js` 变为 `std::env::current_exe()`，这是**唯一**允许的行为差异，且只影响 `RpcClient` 的默认参数） |
| `kiana-env` | `packages/env/daemon`（已是 Rust：`kiana-env serve --token <hex>`） | `crates/kiana-env/src/main.rs`（`[[bin]] name = "kiana-env"`） | 远端执行守护进程；帧协议 protocol 1；零 workspace 依赖（`serde_json`/`encoding_rs`/`ignore`/`notify` + 平台层） |
| `kiana-ai` | `packages/ai` `bin.kiana-ai = dist/cli.js` → `src/cli.ts` | `crates/kiana-ai/src/bin/kiana-ai.rs`（`[[bin]] name = "kiana-ai"`） | 交互式 OAuth 配置 CLI（`auth.json`）；`readline` 的等价物用 `std::io::stdin` 行读取 |
| `kiana-evals` | `packages/evals`（私有，`node --experimental-strip-types src/cli.ts`） | `crates/kiana-evals/src/main.rs`（`[[bin]] name = "kiana-evals"`） | Docker 化评测评测运行器；需要 `docker` 可执行文件 |
| （原）`image-resize-worker` / `codemode/worker` | `bun build --compile` 额外打包的两个 worker 入口 | **无独立二进制** | 两者在 Rust 侧是 `kiana-core::image_process` 的线程池与 `kiana-codemode` 的 worker 进程池，不需要单独可执行文件 |

其它可执行入口（不是 `bin`，但对外是命令）：

- `kiana config` / `kiana package`：`kiana-coding-agent` 的**子命令**，不是独立二进制
  （`package-manager-cli.ts` 由 `main.ts` 调用）。
- `kiana-env` 的部署由 `kiana-env-client::ssh` 完成（`~/.kiana/mobile/tools/kiana-env`）；二进制名与远端路径是已部署守护进程的定位契约，改动需迁移旧部署。
- `kiana-ai` 的 `auth.json`（当前目录）与 `kiana-coding-agent` 的 `~/.kiana/agent/auth.json` 是**不同**文件，
  两者都要保留。

构建：`cargo build --release --bin kiana --bin kiana-rpc --bin kiana-env --bin kiana-ai --bin kiana-evals`；
`[profile.release]` 已设 `lto = true`、`codegen-units = 1`、`strip = true`。体积目标：
`kiana` 单二进制 ≤ 5.2 MB（含 rquickjs + oxc + ICU4X 的估算，见 `spec/design-extension-runtime.md`
§1.3；当前骨架未启用这些重依赖）。

---

## 7. 实施阶段、所有者与文件所有权边界

### 7.0 并行规则（所有阶段适用）

1. **一个 crate 的 `src/` 只由它的 owner 写**。跨 crate 需要改接口时，在
   `crates/<crate>/src/lib.rs` 的再导出加一行、在本文 §4.9 补一行，然后通知下游 owner；
   不得直接改别人的文件。
2. **`Cargo.toml` 的 `[workspace.dependencies]` 只由架构者改**（pin 版本、加依赖）。
   任何 crate 需要新 pin 时提 issue，不要自己写版本号。
3. **共享契约文件**（§4.9 表里的文件）在阶段 0 冻结；之后的修改必须同时更新本文与所有
   下游 crate 的编译（用 `cargo check --workspace` 验证）。
4. 每个阶段的**验收门槛**：`cargo test -p <crate>` 全绿 + `cargo clippy -p <crate> --all-targets`
   0 warning + 该阶段 spec 的「验收清单」逐条勾掉。
5. 不允许“先合拢再修”：每个 crate 合入时 `cargo check --workspace` 必须通过。

### 7.1 阶段与 owner

| 阶段 | crate | owner | 依赖（可开工的前提） | 主要交付 |
|---|---|---|---|---|
| 0 | `kiana-json` | core-team | 无 | 已交付：`js_num`（`format_js_number`/`to_radix_string`/`parse_js_number`）、`js_object`（`stringify`/`stringify_pretty`/`sort_js_keys`/`deep_equal`）、`js_string`、`js_parse`（V8 兼容 `parse` + `repair`/`parse_with_repair`/`parse_streaming` + 自写 partial-json 0.1.7）；74 条测试 |
| 0 | `kiana-telemetry` | core-team | 无 | 契约 trait + memory + metrics + render + store-conformance 六条 |
| 1 | `kiana-ai` | ai-team | `kiana-json`、`kiana-telemetry` | 202 个 TS 文件；`build.rs` 生成 42 个 provider 目录；`kiana-ai` bin |
| 1 | `kiana-mcp` | ai-team | `kiana-json` | JSON-RPC、5 个传输、OAuth、testing 夹具 |
| 1 | `kiana-tui` | tui-team | `kiana-json` | 46 个 TS 文件（19,674 行）；`tui.rs`/`terminal_colors.rs`/`colors.rs` 已定契约 |
| 1 | `kiana-codemode` | codemode-team | `kiana-json` | quickjs-wasi 沙箱、声明生成、worker/remote；`prelude-source` 资源 |
| 1 | `kiana-agent-core` | agent-team | `kiana-ai` | 6 文件 / 2,541 行；`types.rs` 已定 |
| 1 | `kiana-chord` | chord-team | `kiana-json`、`kiana-telemetry` | 41 文件 / 15,193 行；`wire.rs`/`errors.rs` 已定；feature 门控 |
| 1 | `kiana-durable` | durable-team | `kiana-ai`、`kiana-chord`、`kiana-codemode`、`kiana-telemetry`、`kiana-json` | 84 文件 / 27,695 行；storage 4 后端 + harness |
| 1 | `kiana-env` | env-team | 无（独立 bin） | 16 个 `.rs` / 4,998 行；帧协议与 docs 对齐 |
| 2 | `kiana-env-client` | env-team | `kiana-chord`、`kiana-durable` | 8 文件 / 2,346 行；SSH 部署、`RemoteExecutionEnv` |
| 2 | `kiana-core` | ca-core-team | `kiana-ai`、`kiana-agent-core`、`kiana-tui` | 47 文件 / 5,360 行（`utils/**` + `config.ts` + 10 个 core 模块；含 `syntax_highlight`/`fs_watch`） |
| 2 | `kiana-session-core` | ca-session-team | `kiana-core`、`kiana-ai`、`kiana-agent-core` | 8 文件 / 3,092 行（session-manager 2,018 行是重点） |
| 3 | `kiana-config` | ca-config-team | `kiana-core`、`kiana-session-core`、`kiana-ai`、`kiana-tui`、`kiana-json` | 26 文件 / 7,465 行 |
| 3 | `kiana-theme` | tui-team | `kiana-config`、`kiana-core`、`kiana-tui` | 5 文件 / 2,261 行（`theme.ts` 的 `highlightCode` 在此；highlight.js 21 种语言的等价实现在 `kiana-core::syntax_highlight`，阶段 2 交付） |
| 3 | `kiana-compaction` | ca-session-team | `kiana-core`、`kiana-session-core`、`kiana-ai`、`kiana-agent-core` | 4 文件 / 1,654 行 |
| 4 | `kiana-ext-api` | ca-ext-team | 见 §2（L7） | 1 个 2,308 行的 TS 文件拆成 7 个 `.rs`；**扩展 API 的唯一权威** |
| 4 | `kiana-tools` | ca-tools-team | `kiana-ext-api`、`kiana-ui-kit`、`kiana-session-core`、`kiana-core` | 25 文件 / 4,794 行（8 个工具 + 8 个 renderer） |
| 4 | `kiana-export` | ca-tools-team | `kiana-core`、`kiana-ext-api`、`kiana-session-core`、`kiana-theme`、`kiana-tui` | 3 文件 / 750 行 + 模板/CSS/vendored JS 资源 |
| 4 | `kiana-ui-kit` | tui-team | `kiana-core`、`kiana-theme`、`kiana-tui` | 9 文件 / 536 行 |
| 5 | `kiana-resources` | ca-resources-team | `kiana-ext-api`、`kiana-extensions`、`kiana-config`、`kiana-theme`、`kiana-session-core`、`kiana-core` | 9 文件 / 6,151 行（package-manager 2,791 行拆到 `package/*`） |
| 5 | `kiana-extensions` | ca-ext-team | `kiana-ext-api`、`kiana-tools`、`kiana-config`、`kiana-theme`、`kiana-core`、`kiana-tui`、`kiana-session-core`、`kiana-ai`、`kiana-agent-core` | 7 文件 / 2,745 行（`types.ts` 在 `kiana-ext-api`）+ JS 运行时（rquickjs/oxc）；按设计文档 §11 的 10 步推进；`runtime` feature 在阶段 5 末进 default |
| 6 | `kiana-session` | ca-session-team | L11 全部 | 4 文件 / 5,617 行（`agent-session.ts` 4,458 行是最大单文件） |
| 6 | `kiana-builtin-extensions` | ca-ext-team | `kiana-extensions`、`kiana-mcp`、`kiana-codemode`、`kiana-tools`、`kiana-ui-kit` | 25 文件 / 7,585 行（4 个内置扩展） |
| 7 | `kiana-interactive` | ca-interactive-team | `kiana-session`、`kiana-builtin-extensions`、`kiana-resources`、`kiana-tools`、`kiana-ui-kit`、`kiana-theme` | 49 文件 / 19,342 行（`interactive-mode.ts` 245.7 KB 是最大单文件；含 `modes/index.ts`） |
| 7 | `kiana-rpc-print` | ca-cli-team | `kiana-session`、`kiana-tools`、`kiana-extensions` | 7 文件 / 2,047 行（print/json/rpc 三种模式） |
| 8 | `kiana-coding-agent` | ca-cli-team | L13 全部 | 23 文件 / 4,584 行；`kiana` 与 `kiana-rpc` 两个 bin；SDK 导出面 |
| 8 | `kiana-evals` | evals-team | `kiana-ai`、`kiana-coding-agent` | 5 个 TS 源 / 1,446 行（+ `lib.rs`/`main.rs` 入口）+ docker 资源 |

阶段是**开工门槛**而非串行：阶段 2–8 的所有 crate 在骨架里都已能编译，owner 可在依赖 crate
的**契约**（§4.9）稳定后立即开工，不必等依赖 crate 全部实现完（用 `todo!()`/stub 顶住）。

### 7.2 大文件拆分表（>1,000 行的 TS 文件）

| TS 文件 | 行 | Rust 落点 |
|---|---|---|
| `coding-agent/src/core/agent-session.ts` | 4,458 | `kiana-session/src/agent_session.rs`（单文件；内部按 spec `ca-session.md` §8.1 的分组组织 `impl` 块，必要时在阶段 6 拆成子模块） |
| `coding-agent/src/core/package-manager.ts` | 2,791 | `kiana-resources/src/package/**`（12 个文件） |
| `coding-agent/src/core/session-manager.ts` | 2,018 | `kiana-session-core/src/session_manager.rs` |
| `coding-agent/src/core/settings-manager.ts` | 1,443 | `kiana-config/src/settings/{mod,schema,storage,migrations,tools,compaction,diagnostics,defaults}.rs` |
| `coding-agent/src/core/resource-loader.ts` | 1,322 | `kiana-resources/src/loader/{mod,context_files,themes,warnings,source_info_map}.rs` |
| `coding-agent/src/package-manager-cli.ts` | 1,151 | `kiana-coding-agent/src/package_manager_cli.rs`（可再拆子模块） |
| `coding-agent/src/core/model-runtime.ts` | 1,049 | `kiana-config/src/models/runtime.rs` |
| `coding-agent/src/main.ts` | 1,001 | `kiana-coding-agent/src/app.rs` |
| `ai/src/api/openai-completions.ts` 等 8 个 >30 KB 的 api | 30–62 KB | `kiana-ai/src/api/<name>.rs`（一对一；内部按 spec `ai-api-anthropic-openai.md` 的小节组织） |
| `ai/src/providers/faux.ts` | 22.2 KB | `kiana-ai/src/providers/faux.rs` |
| `chord/src/object.ts` / `host.ts` / `connection.ts` | 72/55/49 KB | `kiana-chord/src/object/**`、`host/**`、`connection/**`（已拆） |
| `durable/src/harness/*` | 23 文件 / 9,598 行 | `kiana-durable/src/harness/**`（一对一） |
| `tui/src/tui-alt-screen.ts` / `tui.ts` / `keys.ts` | 64/51/46 KB | `kiana-tui/src/{alt_screen,tui,keys}.rs`（一对一） |
| `coding-agent/src/modes/interactive/interactive-mode.ts` | 245.7 KB | `kiana-interactive/src/interactive_mode.rs`（阶段 7 开工时可再按 spec `ca-interactive-mode.md` 的章节拆子模块） |
| `coding-agent/src/core/extensions/types.ts` | 2,308 行 | `kiana-ext-api/src/{types,tool_types,tool_result,events,system_prompt,ui,index}.rs`（已拆） |

### 7.3 里程碑

- **M1（阶段 0–1 完成）**：`kiana-json`/`kiana-telemetry`/`kiana-ai`/`kiana-mcp`/`kiana-tui`/`kiana-codemode`/
  `kiana-agent-core`/`kiana-chord`/`kiana-durable`/`kiana-env` 全部通过各自 spec 的验收清单；
  `cargo test --workspace` 全绿。此时 5 个库可以独立发布。
  - **M1 status / M1 状态（2026-10-30，verifier 复核）**：10 个 crate 已全部合入并通过验收——
    `cargo fmt --all --check`、`cargo check --workspace --all-targets`、
    `cargo clippy --workspace --all-targets -- -D warnings` 全绿；
    `cargo test --workspace --no-fail-fast` 结果 **5928 passed / 0 failed / 358 ignored / 470 suites**
    （各 crate：kiana-json 90/0，kiana-telemetry 121/0，kiana-ai 2025/341 ignored，kiana-mcp 437/0，
    kiana-tui 1406/6 ignored，kiana-codemode 338/0，kiana-agent-core 202/0，kiana-chord 232/0，
    kiana-durable 894/11 ignored，kiana-env 183/0）。10 个 crate 的 `src/` 中
    `todo!()`/`unimplemented!()` 的实际调用为 0（`kiana-durable/src/storage/indexeddb.rs` 只有
    一处文档注释提到）；358 条 ignored 全部是 live-API（需要真实凭据）、
    benchmark、manual-TTY、workers-runtime 用例，没有功能缺口。
  - **与 TS 参考的在线对照**（node 26，`/home/ctyun/kiana-work/upstream-reference`）：`kiana-json` 的
    `JSON.parse`/`JSON.stringify` 22/23 向量与固定基线逐字节对齐（唯一差异是已文档化的孤立代理项 →
    U+FFFD），`parseStreamingJson` 全部向量一致；`kiana-tui` 的 `Text.render` 8/8 组与固定基线逐字节对齐；
    `kiana-ai` faux provider 的 `complete` 响应与 `stream` 事件序列语义一致（仅随机 `api` id 不同）。
  - **验证中修掉的问题**（完整测试在最终树上共复跑 3 轮：第 1 轮暴露 kiana-ai 注册表竞态，第 2
    轮暴露 kiana-mcp 两处测试竞态，修复后第 3 轮全绿）：
    1. `AssistantMessageEventStream::push` 的终态竞态：`done` 标志原先在 `state` 锁外先置位，
       并发的 `recv()` 可能观察到「`done` 为真 + 队列为空」而提前返回 `None`，丢掉终态
       `done` 事件（faux 流复现率约 1/3）。现在 `done` 标记、`resolve_result` 与事件投递在
       同一个 `state` 临界区内完成；新增 2000 轮并发回归测试。
    2. `SystemMessage` 键序冲突：`kiana-agent-core` 要求 `toolsAdded` 在前，`kiana-durable` 的
       `kiana.system` 持久化要求 `toolsRemoved` 在前。结构体恢复 TS interface 声明序（§4.3.3），
       durable 的 `persisted_message_json()` 新增 System 分支，按
       `harness/prompt.ts#systemEntry` 的构造序显式重排；durable 的键序回归测试改为断言
       实际落盘形状（`persisted_message_json` 的输出）。
    3. `kiana-mcp` 的两个测试环境竞态（只改测试，待测行为不变）：
       - `stdio_framing` 的 ETXTBSY：测试二进制内其它用例并发 fork 时继承了刚写入脚本的 fd。
         按 Node 的做法短暂重试；`start()` 在 spawn 前就置 `started`（spawn 失败也
         保留），所以同一实例无法重试——每次尝试改为新建 `StdioTransport`。
       - `in_memory` 投递顺序：模块文档已写明「监听器里重入 `send` 入队的消息不保证在外层
         `send` 返回前被处理」（Rust 没有全局微任务检查点）。测试原来在 `await send` 后立即
         断言队列为空，现改为用 `wait_until` 等排空 task 处理完，再断言队列为空且嵌套消息
         未被投递。
    4. 磁盘：默认 dev debuginfo 下 500+ 个测试二进制每个 ~0.5 GB，完整 `cargo test --workspace`
       需要 >100 GB（本次实测 df 从 137 GB 掉到 27 GB，已到危险线）。根 `Cargo.toml` 加了
       `[profile.test] debug = 0 / strip = "debuginfo"`：测试断言与 debug assertions 不变，
       二进制缩到 20–100 MB。
    5. `kiana-ai` 注册表测试隔离：`compat::tests::dispatches_unknown_providers_through_the_legacy_api_registry`
       没有像同模块其它用例那样持有 `REGISTRY_TEST_LOCK`；并行执行时另一用例的
       `reset_api_providers()` 会在「注册」与「分派」之间抹掉覆盖项（复跑实测断言得到 `None`
       而非 `Some(Some("request-key"))`）。补上同一把 `tokio::sync::Mutex` 串行化；只改测试。
  - `cargo fmt --all` 统一了格式；`spec/PROGRESS.md` 恢复表头并补齐 kiana-json / kiana-telemetry /
    kiana-env 的 5 条缺失记录及 M1 验证 gate。进度明细见 `spec/PROGRESS.md`。
- **M2（阶段 2–4 完成）**：`kiana-core`/`kiana-session-core`/`kiana-config`/`kiana-theme`/`kiana-compaction`/
  `kiana-ext-api`/`kiana-tools`/`kiana-export`/`kiana-ui-kit`/`kiana-env-client` 完成；SDK 面
  （`kiana-ext-api` + `kiana-session-core`）可以先给扩展作者试用。
  - **M2 status / M2 状态（2026-11-01，verifier 复核）**：10 个 crate 已全部合入并通过验收——
    `cargo fmt --all --check`、`cargo check --workspace --all-targets`、
    `cargo clippy --workspace --all-targets -- -D warnings` 全绿（阶段 5–8 的骨架 crate 也通过
    `--all-targets` 检查）；`cargo test --workspace` 结果
    **8402 passed / 0 failed / 361 ignored / 664 suites**（各 crate 的 passed/failed/ignored：
    kiana-core 650/0/1，kiana-env-client 117/0/0，kiana-session-core 249/0/0，kiana-compaction 119/0/2，
    kiana-config 627/0/0，kiana-theme 109/0/0，kiana-ext-api 79/0/0，kiana-ui-kit 83/0/0，kiana-export 88/0/0，
    kiana-tools 352/0/0）。10 个 crate 的 `src/` 中 `todo!()`/`unimplemented!()` 的实际调用为 0；
    M2 的 3 条 ignored 全部是文档化用例（kiana-core 的 `&str` API 编译期偏差 1 条；kiana-compaction 的
    live-LLM 2 条，需 `ANTHROPIC_OAUTH_TOKEN`），没有功能缺口。
  - **与 TS 参考的在线对照**（node 26.12.1，`/home/ctyun/kiana-work/upstream-reference`；6 组探针按字节比较）：
    kiana-core `utils/paths.ts` 的 71 个向量（normalize/resolve/relative/format/isLocalPath/
    normalizeWindowsShellPath）与 `config.ts` 的 7 个常量 + 4 种环境状态对齐（历史对照基线）；kiana-session-core
    的 21 条 session entry JSONL 与 `convertToLlm` 的 14 条消息一致；kiana-config `AuthStorage` 的
    read/modify/delete/list + 落盘 `auth.json` 与 `FileModelsStore` 的 write/read/delete +
    `models-store.json` 一致（唯一差异是已文档化的缺口：未知 `type` 的凭证在 Rust 的 `Credential`
    枚举里无法表达，`read` 返回 `None`、`list` 省略该条，TS 原样返回）；kiana-tools read/edit/bash 的
    33 个用例（截断脚注、offset/limit、BOM/CRLF、diff/patch、退出码/空输出）一致；kiana-export 的
    3 个夹具 HTML 与固定基线逐字节对齐（sha256 `9775e404…` / `15c8de4f…` / `2c5cdd74…`）。
  - **验证中修掉的问题**：`cargo fmt` 在 kiana-core / kiana-config / kiana-env-client 的漂移（只改格式）；
    kiana-theme 的 `parse_theme_json_content_reports_syntax_errors_and_strips_the_bom` 读取进程级
    Theme JSON 校验器却没有持有共享 `test_lock`，与安装校验器的用例并发时偶发失败（首个
    workspace 全量复跑实测 1 次），现已在用例开头取锁；kiana-agent-core 的 `proxy::http_client`
    把同一个 `reqwest::Client` 缓存给所有 tokio runtime，测试二进制里每个 `#[tokio::test]` 是独立
    runtime，复用到已销毁 runtime 的空闲连接会以 hyper-util `DispatchGone` 报 `fetch failed`
    （复跑实测 4/20），现按 runtime id 缓存客户端（生产单 runtime 行为不变；proxy 套件复跑
    40/40 全绿）。
- **M3（阶段 5–6 完成）**：`kiana-resources`/`kiana-extensions`（含 JS 运行时）/`kiana-session`/
  `kiana-builtin-extensions` 完成；扩展语料（86 个 `.ts`）全部跑通。
  - **M3 status / M3 状态（2026-11-02，verifier 复核）**：4 个 crate 已全部合入并通过验收——
    `cargo fmt --all --check`、`cargo check --workspace --all-targets`、
    `cargo clippy --workspace --all-targets -- -D warnings` 全绿；
    `cargo test --workspace --no-fail-fast` 结果 **10405 passed / 0 failed / 370 ignored / 795 suites**
    （M3 四 crate：kiana-extensions 848/0/4，kiana-resources 539/0/0，kiana-builtin-extensions 270/0/0，
    kiana-session 347/0/5）。4 个 crate 的 `src/` 中 `todo!()`/`unimplemented!()` 与 TODO/FIXME
    注释均为 0；9 条 ignored 中 5 条是 kiana-session 的 live-LLM 压缩用例（需 `ANTHROPIC_API_KEY`），
    4 条是 kiana-extensions 文档示例的 doctest `ignore`，没有功能缺口。
    `cargo check -p kiana-extensions --no-default-features` 也通过。
  - **与 TS 参考的在线对照**（node 26.12.1，`/home/ctyun/kiana-work/upstream-reference`；探针脚本在
    `/home/ctyun/kiana-work/scratch/m3-verifier/`）：
    1. **扩展运行时**：用真实 TS loader 重新生成语料注册矩阵（86 个 `.ts` / 77 个入口）与
       `tests/fixtures/corpus/registrations.json` 一致（0 diff，历史对照基线）；自制的 ESM（相对 `.ts`
       导入、`node:path`/`node:fs`、`@earendil-works/kiana-tui` 虚拟模块）与 CJS（`require` 本地
       `.cjs`）扩展在两种实现下注册结果一致；rquickjs 的 Promise/微任务顺序、AbortController
       （含默认 `DOMException` 名称与 `This operation was aborted` 文案）、定时器与 interval
       行为与 Node 参考脚本逐项一致。
    2. **资源**：13 个技能目录、`loadSkills` 同名冲突、`formatSkillsForPrompt`、39 条
       `parseCommandArgs`/`substituteArgs`/`expandPromptTemplate` 向量、8 组
       `buildSystemPrompt`/`buildSystemPromptSections` 与 5 个非法 section 名，除环境相关的
       package 路径（docs/examples/README）外与固定基线逐字节对齐。
    3. **内置扩展**：tool-search 的 14 条 tokenize、10 个文档文本与 50 组 BM25 排名与 TS 完全
       一致；MCP 配置在 trusted/untrusted、BOM、损坏 JSON、项目覆盖/命名冲突/项目内 auth 等
       场景下的 `loadMcpConfig` 结果，以及 `update`/`add`/`remove` 写回的 `mcp.json` 文件逐字节
       一致。
    4. **AgentSession**：用 `createAgentSession` + faux provider 在 TS 与 Rust 两侧各跑一次
       prompt，6 条 JSONL（session / model_change / thinking_level_change / system / user /
       assistant）的条目类型、字段集合、sections、toolsAdded（含工具 schema）与 usage 语义
       一致（仅 docs 路径长度导致 1–2 个 token 的估算差异）；TS 的 retry-events/queue/runtime
       三个套件 42 例通过，Rust 对应 5 个套件 47 例通过。
  - **M3 验证发现（记录为 M4 风险，未改语义）**：
    1. 会话 JSONL 的键序与 TS 运行时不同：TS 的 `withToolChanges`（agent-loop）把
       `toolsAdded`/`toolsRemoved` 放在 `timestamp` **之后**，Rust 结构体序列化把 `toolsAdded`
       放在 `timestamp` 之前；TS 的 `Object.assign(..., { thinkingLevel })` 把 `thinkingLevel`
       追加到 assistant 消息**最末**，Rust 在 `usage` 之前。字段与值相同，但逐字节黄金测试会
       失败；M4 需要在事件与会话文件的写出层按 TS 运行时构造序重排。
    2. `load_extensions([目录])`：TS 经 jiti/Node 解析目录（index.ts、package.json main），
       Rust 把目录直接交给模块加载器，报 `Cannot find module`。正常发现流程
       （`resolve_extension_entries` → 入口文件）两侧一致且被 77 个语料入口覆盖；直接传目录是
       公开 API 的边缘差异。
    3. 扩展 provider 的 `oauth` 回调仍未跨 crate 传递（kiana-extensions 与 kiana-session 都置
       `None`，代码注释已记录）；使用 OAuth 的自定义 provider 扩展需在 M4 前补适配器。
    4. 已文档化的非缺陷近似与限制：`setImmediate`/`process.nextTick` 映射为微任务（Node 为
       宏任务），扩展语料不依赖；Node 的定时器在进程启动较慢时会因过期顺序不同而重排，Rust
       使用虚拟时钟，在相同截止时间下顺序一致；默认构建不含 WebAssembly（可选
       `webassembly` 特性基于 wasmtime，Emscripten FS/内存假设仍需 shim），故
       `doom-overlay/` 语料按 C 级跳过，`gondolin/` 因原生 ssh2 按 D 级跳过。
  - **验证中修掉的问题**：`cargo fmt` 在 `kiana-builtin-extensions` 的 4 处漂移（只改格式）。
    其余全量轮次中的失败均为阶段 1/2/4 既有用例的偶发环境敏感，非 M3 回归：
    kiana-durable 的 `harness_view`/`harness_lifecycle` 各偶发 1 次（并发时序；各自单套件
    20/20 通过），kiana-config 的 `model_catalog_protocol` 偶发 1 次 `fetch failed`（本地
    mock server；单套件 3/3 通过）；一次干净的 `cargo test --workspace --no-fail-fast`
    为 10405 passed / 0 failed。M3 四个 crate 的 fmt/check/clippy/test 在最终树上复跑全绿。
- **M4（阶段 7–8 完成）**：`kiana-interactive`/`kiana-rpc-print`/`kiana-coding-agent`/`kiana-evals`；
  `kiana --version`、`kiana -p`、`kiana --mode json`、`kiana --mode rpc`、interactive 全部可用；
  黄金测试与历史基线逐字节对齐，作为回归判定点；当参考项目的旧行为不如当前设计时，以当前设计为准并更新基线。
  - **M4 status / M4 状态（2026-11-04，verifier 复核）**：4 个 crate 已全部合入并通过验收——
    `cargo fmt --all --check`、`cargo check --workspace --all-targets`、
    `cargo clippy --workspace --all-targets -- -D warnings` 全绿；
    `cargo test --workspace --no-fail-fast` 结果 **11830 passed / 0 failed / 399 ignored /
    874 suites**（M4 四个 crate：kiana-interactive 689/0/0（39 suites）、kiana-rpc-print 215/0/24
    （14 suites）、kiana-coding-agent 321/0/5（26 suites）、kiana-evals 197/0/0（10 suites））。
    4 个 crate 的 `src/` 中 `todo!()`/`unimplemented!()` 为 0；M4 的 29 条 ignored 全部是
    live-API（需要 `ANTHROPIC_API_KEY` 与真实 `kiana` 二进制）、`manual TTY`（4 条交互选择器）、
    以及需要 `kiana` 二进制才能验证的信号/干净 stdout 用例（24 条），没有功能缺口。
  - **与 TS 参考的端到端对照**（node 26.12.1，`/home/ctyun/kiana-work/upstream-reference`，经
    `source-resolver.ts` 直接运行 `src/cli.ts`/`src/rpc-entry.ts`；全部使用临时
    `KIANA_CODING_AGENT_DIR` 与本地 mock openai-completions provider，未触碰真实 `~/.kiana`）：
    1. **CLI 表面**：`kiana`/`kiana-rpc` 的 `--version`/`-v`、`--help`/`-h`、未知选项、`--mode`
       非法值、缺少选项值、`-p --unknown-opt` 的 stdout/stderr/退出码**与固定基线逐字节对齐**（用户可见契约，改动需同步测试）。
    2. **print/json**：`kiana -p`（纯文本、触发 `read` 工具）stdout 与固定基线逐字节对齐；
       `kiana --mode json` 的 26/15 条事件在规范化随机 id/timestamp 后语义与键序一致（见下方
       TS 竞态说明）；两轮会话写出的 session JSONL 在规范化 id/timestamp/cwd 后**与历史基线一致**，
       包括 `system`/`assistant`/`user`/`toolResult` 的键序、`sections`、`toolsAdded` 的工具
       schema 与 `toolResult.details`。
    3. **RPC**：脚本化 31 条记录（`get_state`/`prompt`/`follow_up`/`new_session`/
       `set_session_name`/`compact`/`abort`/`get_session_stats`/`get_entries`/未知命令）与
       83 条记录（流式中的 `steer`、`follow_up`、`abort`，`compact`，`new_session` +
       `switch_session`）逐字段一致（仅随机 id/session 路径不同）；`kiana-rpc` 的 CLI 表面同样一致。
    4. **interactive**：在 30×100 伪终端中运行，用 ANSI 屏幕重建比较：首屏、`/help` 后、
       `/model` 选择器打开后、以及 prompt 往返（`hello world` → mock 回复）后的最终屏幕
       **逐字符一致**；`/exit` 正常退出（退出码 0）。参考实现的动画帧数量/顺序随时序变化，
       比较的是屏幕状态而非原始字节流。
  - **M4 验证中修掉的问题**：
    1. **交互模式缺少宿主事件/渲染循环**（M4 阻断性缺陷）：`InteractiveMode::run` 只
       `await` 用户输入；`ProcessTerminal` 的读取线程把字节放进通道，但没有任何循环调用
       `poll_input` → `pump_pending_input`/`handle_terminal_input` → `render_decision`/
       `render_now`。真实进程因此只渲染首帧、完全不处理键盘输入（单元测试直接调用
       `InteractiveMode` 方法，未覆盖 `run` 循环；`startup-benchmark` 路径也绕过了它）。
       修复：`Terminal` 增加带默认实现的 `poll_input`/`poll_timers`/`poll_resize`
       （`ProcessTerminal` 实现、`SharedTerminal` 转发），新增
       `InteractiveRenderer::pump_events`（轮询 stdin/定时器/尺寸 → 分发排队输入 → 按
       `renderDecision` 渲染），由 `InteractiveMode::run` 启动的 10ms 轮询任务驱动；任务只在
       同步作用域内借用 `Inner`，不在 await 点持有借用。伪终端验证见上。
    2. **会话 JSONL 键序**（M3 记录的 M4 风险 1）：wire 事件的键序重排原来只存在于
       `kiana-rpc-print::json_event`，会话文件仍按 Rust 结构体声明序写。修复：把
       `reorder_wire_keys`（含 system/assistant/toolResult/usage/模型目录的构造序规则）移到
       `kiana-agent-core::wire_keys`，`kiana-rpc-print` 再导出以保持公开 API；`kiana-session-core`
       在新建条目的 `StoredFileEntry::to_json_value` 上调用同一函数（从文件加载的原始对象
       仍逐字回放）。现在 system/assistant/toolResult 的键序符合固定落盘契约（与历史基线一致，改动需同步测试）。
    3. **测试断言大小写**（只改测试）：`kiana-session --test agent_session_prompt_extra` 的
       `persistence_failure_rejects_the_prompt` 断言错误文本包含 `"Permission denied"`，
       而实现（正确地）产生 Node 风格 `EACCES: permission denied, open '<path>'`；改为断言
       `EACCES` + 小写 `permission denied`。
  - **已知的 TS 非确定性（不是 Rust 缺陷，最终黄金对比需容忍）**：TS provider 的
    `start` 事件把可变的 `output` 对象**按引用**放进事件队列，agent-loop 读取时才浅拷贝；
    `responseId` 与 `content` 数组的赋值/追加与消费发生在同一事件循环，导致
    `message_start.message` 有时已带 `responseId`/首个内容块、有时为空（实测 5 次中 2 次为空，
    延迟 provider 时稳定为空）。Rust 在 `Start` 时克隆快照（稳定为空、终态字段齐全），与
    “延迟 provider”下的 TS 行为一致；`message_update` 的 `message` 快照同理。逐字节黄金
    对比应使用带时序隔离的 provider，或对这几个字段做恒等归一化。
  - **最终黄金复核（2026-11-04，final validator）**：在最终 Rust 树上复跑
    `cargo fmt --all --check`、`cargo check --workspace --all-targets` 与
    `cargo clippy --workspace --all-targets -- -D warnings`，全部通过。完整
    `cargo test --workspace --no-fail-fast` 为 **11833 passed / 2 failed / 399 ignored /
    876 suites**；两项失败分别是 kiana-config `model_catalog_protocol` 的本地 HTTP 超时，以及
    kiana-mcp `fractional_timeouts_are_truncated_like_node` 的墙钟调度误差（6.55ms 超过 1ms
    容差）。它们分别单独复跑通过（前者两次）；kiana-durable `harness_lifecycle` + `harness_view`
    29 条与 kiana-mcp `streamable_http` 30 条也单独通过。因完整轮次仍有失败，不能称为
    “零失败全绿”。修复者的 11 套件 / **124** 条重点回归在当前树上全部通过。
  - **本轮对照证据（均使用临时 HOME/XDG/agent 目录、本地 mock，无真实 provider）**：
    1. `kiana`/`kiana-rpc` `--help`/`--version` 的 stdout/stderr/退出码逐字节相同；`kiana -p` 的
       14-byte 输出、空 stderr 与退出码逐字节相同。两个 8-line session JSONL 仅在
       随机 id、时间戳、durationMs 和临时路径替换后逐字节相同；**原始 session 不相等**。
       26 条 JSON 事件经上述替换后仍不一致，需另归一化 `message_start` 的可变
       `responseId`/`content` 快照和 `message_update.usage` 才一致。
    2. 162 条 RPC 全量对话（含流式 partial tool call、steer/follow-up/abort、compact、
       new/switch session 与未知命令）两侧退出码为 0、stderr 为空；替换随机 id、时间、路径后
       **153/162** 条逐字节相同，9 条均为上述 TS mutable-output 快照差异；**不是原始 RPC
       逐字节相等**。
    3. 本地 sparse/unknown-field 目录探针的持久化文件与 `getAllModels` JSON 仅替换
       `checkedAt` 后与固定基线一致，包括未知字段、JS 数字/键序与目录顺序。compaction 的
       `systemMessage` 保留 `sections`/`toolsAdded` 原始位置，再覆盖 `timestamp`；其
       `include_str!` golden 可由 retained TS bundle + 生成脚本重现 353 bytes 完全相同。
       这只是固定构造的 compaction golden，**不是固定时钟的整个 session**。
    4. 带自定义主题、extension widget/status、skill/prompt/context 的 30x100 PTY 对照完成
       prompt 往返、help/model selector、90/110 列 resize 与退出。七个重建屏幕的文本、
       光标与颜色/样式元数据相同（仅替换两侧不同的临时路径）；比较**不是原始终端字节流**。
       此轮未测试真实剪贴板。
  - **六类最终修复已验证**：资源展示闭包不再延迟读取 `session_manager()` 而重入
    `InteractiveMode.inner`；compaction `systemMessage` 键序；远端 sparse/未知模型原样保存、
    `IndexMap` 覆盖保留首次位置及 `models/lastModified/etag/checkedAt` 新条目顺序；
    `KIANA_PACKAGE_DIR` 存在时 export-html 从该包读资产；`registerProvider(name, config)` 的
    JS OAuth login/refresh/getApiKey/modifyModels 已接通（含 detached credential lookup）；
    kiana-evals 使用 TS monorepo 作为 Docker build context，不再误用 Cargo workspace。
    最终验证未再修改产品代码。
  - **仍未证明完整一致 / 精确剩余缺口**：
    1. `registerProvider(providerObject)` 的 `JsNativeProvider` 仍把 `auth` 固定为
       `ProviderAuth::default()`，不支持对象自带的 OAuth/API-key 交互适配；named-config
       OAuth 已通过，不能据此宣称该 overload 全通。
    2. evals fatal stderr 仍缺 Node 风格的 stack/source excerpt；真实 Docker 构建/运行、
       报告与退出码未实测，当前只有 fake-docker argv/文件协议与本地 harness 对照。
    3. immediate mock 下 TS 的可变 output 别名与 Rust 快照语义仍有上述事件差异；无固定
       时钟/id 的完整 session 原始字节证据，TS `dist/bundle/cli.js` built bundle 未对照。
    4. 真实硬件终端动画/剪贴板、live providers、Windows/macOS 实机、Cloudflare/IndexedDB
       不适用后端均未覆盖；JS OAuth 的取消尚未实现完整 AbortSignal 感知的 pending-call
       清理。故原始目标“功能和交互完全一样”**未被完整证明**。

---

## 8. 未决事项与风险

1. **`kiana-durable` 与 `packages/agent` 的关系已澄清**：TS 的 durable 不 import agent（只有一处注释），
   骨架里曾误加 `kiana-agent-core` 依赖，已删除。若实现阶段发现 `harness` 需要 `AgentTool` 形状，
   应上移到 `kiana-durable::types` 而不是反向依赖。
2. **IndexedDB 后端**（`durable/src/storage/indexeddb.ts`，1,211 行）：**已决 / decided — (b) 不移植**。
   IndexedDB 是浏览器 API，M1 没有 `wasm32-unknown-unknown` 目标，也没有可以承载它的运行时。
   落点：`kiana-durable/src/storage/indexeddb.rs` 保留模块与类型（`IndexedDbStorageOptions` /
   `IndexedDbStorage`），构造入口 `IndexedDbStorage::open()` **永远**返回
   `StorageBackendUnsupported::IndexedDb`，`Display` 逐字
   `"IndexedDB storage is not supported in the Rust build"`；模块文档写明原因与“不要用 sqlite 模拟”。
   `storage-conformance` 把这个后端记为“不适用”，不是失败。
3. **Cloudflare Workers 适配**（`durable/storage/sqlite/cloudflare.ts`、`durable/images/cloudflare.ts`、
   `chord/sqlite/cloudflare.ts`、`codemode/cloudflare.ts`）：**已决 / decided — M1 不移植**。
   Rust 侧没有 `DurableObjectStorage` / `cloudflare:workers` / Wrangler `CompiledWasm` 的等价物。
   落点（feature 门控的占位，编译通过、调用即明确报错）：
   `kiana-durable::storage::sqlite::cloudflare`、`kiana-durable::images::cloudflare`、
   `kiana-chord::sqlite::cloudflare`、`kiana-codemode::cloudflare`。错误文案逐字：
   `"Cloudflare Durable Object storage is not supported in the Rust build"`（storage/images）、
   `"Cloudflare Durable Object SQLite is not supported in the Rust build"`（chord）、
   `"Cloudflare image processing is not supported in the Rust build"`（images）、
   `"Cloudflare Workers codemode is not supported in the Rust build"`（codemode）。
   feature 名保留（`cloudflare`、`sqlite-cloudflare`），以便阶段 8 之后若引入 wasm32 目标只需替换实现。
   `kiana-codemode::portable` 的 `RemoteCodemodeSandbox` 客户端侧与平台无关，仍然可用。
4. **扩展运行时的 `runtime` feature**：骨架 `default = []`，阶段 5 末要改成
   `default = ["runtime", "locale-data"]`。这会引入 rquickjs/oxc/ICU（约 5 MB），需要 CI 有
   C 编译器与较长构建时间；`webassembly`（wasmtime，doom 示例）保持可选。
5. **模型目录数据**：**已决 / decided — vendored（提交进 kiana）**。`packages/ai/src/providers/data/*.json`
   在 TS 仓库里是 git-ignored 的生成物（`npm run hydrate-model-data` =
   `node scripts/generate-models.ts --strict --data-only`）。2026-10-26 用该命令生成后，43 个文件
   （42 个 provider + `.manifest.json`，1.1 MiB）已复制到 `crates/kiana-ai/data/`，每个文件的 sha256
   都对过 `.manifest.json`；来源、校验方式与再生成步骤见 `crates/kiana-ai/data/README.md`。
   `KIANA_MODEL_DATA_DIR` 仍然有效（构建期覆盖，指向另一份同结构目录），**未设置时**回退到仓库内
   `crates/kiana-ai/data/`，所以 `cargo build` 不再依赖 Node/CI hydrate。build.rs 的契约见第 21 条。
6. **`kiana-ai` 的 `bedrock` provider**：**已决 / decided — 手写 SigV4 + 手写 AWS event-stream 解码，
   不引入 AWS SDK**。TS 用 `@aws-sdk/client-bedrock-runtime`（Smithy 中间件链）；Rust 侧按
   `spec/ai-api-other.md` 的 REST 形状直接实现：
   - SigV4：`sha2 = "=0.11.1"` + `hmac = "=0.13.0"`（已 pin，`kiana-ai` 已引入；实测与 node `crypto`
     的签名逐字节对齐）；
   - `application/vnd.amazon.eventstream`：手写分帧（`total_length`/`headers_length`/prelude CRC、
     `:event-type`/`:message-type`、`chunk`/`event`/`exception`），不引入 `aws-smithy-*`；
   - 凭据链、region 解析、`BedrockRuntime` 错误映射按 spec 手写。
   理由：`aws-sdk-rust` 的 Smithy 中间件与 TS 的错误文本/重试语义无法一一对齐，且会引入
   ~40 个传递依赖与 tokio 版本约束。
7. **`kiana-env` 的 `KIANA_ENV_VERSION`**：TS 的 daemon `build.rs` 直接读 `../package.json`，因此参照实现
   的上报值永远是 npm 包版本（`1.4.0`），不可能是 crate 版本。Rust 工作区不再位于 npm 包内，
   改为 CI 注入环境变量；未设置时回退 `packages/env/package.json#version` 的字面量 `1.4.0`
   （**不是** `CARGO_PKG_VERSION`，workspace 的 `0.1.0` 与 npm 包无关，上报它会让 `hello.version`
   与 `kiana-env --version` 偏离参照实现）。发布脚本仍应设置它，且 npm 包版本变化时要同步
   `crates/kiana-env/build.rs` 的 `NPM_VERSION`；`tests/version.rs` 的 `env_version_matches_npm_package`
   在能找到 `packages/env/package.json` 时对比该文件，否则对比 `RECORDED_NPM_VERSION`。
8. **`kiana-rpc` 的 `RpcClient` 默认 `cliPath`**：TS 是 `dist/cli.js` + `node`；Rust 改为
   `current_exe()`（§6）。若外部调用方依赖 `RpcClientOptions.cliPath` 的默认值，需要在
   `kiana-rpc-print` 的文档里注明这一处差异。
9. **`kiana-interactive` 的 245.7 KB 单文件**：阶段 7 开工时需要先把 `interactive-mode.ts` 的
   方法按 spec `ca-interactive-mode.md` 的章节拆成子模块（否则单文件 1.5 万行，无法并行）。
   拆分方案由 ca-interactive-team 在阶段 7 开始前提交给架构者确认。
10. **`anyhow` 虽已 pin 但不允许使用**（§4.1）；如果某个 crate 需要错误上下文链，用
    `Box<dyn Error>` + 显式 `source()`，不要引入 `anyhow`。
11. **`tui` 的 2 个未入树文件**：`editor-component.ts`（已在 `kiana-tui/src/editor_component.rs`）
    与 `undo-stack.ts`（并进 `kiana-tui/src/text.rs`）。`undo-stack` 是泛型 `UndoStack<S>`，
    Rust 侧实现为 `kiana_tui::text::UndoStack<S>`，不单独成文件。
12. **`mcp` 的 `oauth/{callback,discovery}` 与 4 个 transport 文件**已并入
    `kiana-mcp/src/oauth.rs` 与 `transports.rs`（各自一个 `.rs`，头部列明合并了哪几个 TS 文件）。
    若阶段 1 发现需要独立文件，owner 可自行拆分，不影响 crate 边界。
13. **`spec/*.md` 里的依赖版本是“建议”，不是权威**：多份 spec 仍写 `reqwest = "0.12"`、`notify = "6"`、
    `edition = "2021"`、`x11rb = "0.13"`、`arboard` 等（`ca-resources.md` §6.3、`ca-interactive-mode.md`
    §6.3、`ca-builtin-extensions.md` §6.3、`tui-core.md` §8.1 等）。**唯一权威是根 `Cargo.toml`**
    （`reqwest 0.13.5` / `notify 8.2.1` / `edition 2024` / `rust-version 1.97`），§7.0 规则 2 已规定
    只有架构者能改 pin。spec 里的版本号按“意图”阅读（`x11rb 0.14.0`、`objc2-app-kit 0.3.2`
    已于 2026-10-26 补 pin，见附录 A；`arboard` **不引入**——剪贴板走 `kiana-tui::native` 与 OSC 52，
    见第 17 条）。
14. **`.d.ts` 垫片不生成 `.rs` 文件**：`packages/ai/src/providers/data-json.d.ts`、
    `coding-agent/src/utils/highlight-js.d.ts`、`coding-agent/src/bun/quickjs-wasm.d.ts` 都是纯类型声明，
    其行为由对应的 Rust 类型/builder 承担；§5 的“一 TS 文件一 `.rs`”规则不适用于它们。
15. **阶段 1 完整性审计的未决项集中在 `spec/GAPS.md`**：G1 依赖缺失（`kiana-core`/`kiana-ai`/`kiana-agent-core`/
    `kiana-env-client`/内置扩展的 HTTP、图片、ZIP、base64；`kiana-tui` 原生层无 pin）、G2 构建期产物
    （`kiana-ai`/`kiana-codemode` 的 `build.rs`、prelude、打包资源）、G3 未分配单元（examples/docs/7 个测试文件/
    `scripts/` 与 CI）、G4 规范矛盾（`kiana-config`/`kiana-session`/`kiana-ext-api` 的落点）、G5 版本常量与 npm 发布。
    每条都给出涉及文件与建议 owner；在开工前由架构者逐条决策（尤其 G1.1、G5.1 会阻塞阶段 2）。

以下 16–30 条是 2026-10-26 由架构者**已决 / decided** 的条目（GAPS 的 G1/G2/G4/G5 逐条答复），
实现者直接照此编码；与旧条文冲突时以本节为准。

16. **G1.1 `kiana-core` 的依赖已补齐**（已决 / decided）：`kiana-core/Cargo.toml` 已加
    `reqwest`（`http_dispatcher.rs` 的 `ClientBuilder`、`management_http.rs` 的 `redirect: manual`）、
    `base64`（OSC 52 / data URL）、`flate2` + `crc32fast`（`zip.rs` 的 `deflateRawSync`/`crc32`），
    以及 feature `images`（`photon-rs` + `image` + `kamadak-exif`）。阶段 2 开工时把 `images`
    加进 default。`arboard` **不引入**：剪贴板走 `kiana-tui::native`（第 17 条）与 OSC 52。
17. **G1.5 `kiana-tui` 原生平台层**（已决 / decided — 方案 (a)）：实现在 `kiana-tui/src/native/**`，
    不新建 `kiana-tui-native` crate。`NativeClipboard` / `ClipboardValue` / `ClipboardError` /
    `NativeModifiers` 的 trait 与三态语义进 §4.9 契约表；平台依赖按 `#[cfg]` + 可选 feature：
    `x11 = ["dep:x11rb"]`（Linux）、`darwin = [objc2, objc2-foundation, objc2-app-kit]`（macOS）、
    `win32 = [windows-sys]`（Windows）、`native` 聚合。**默认 `default = []`**：无后端时
    `get_native_clipboard()` 返回 `Unavailable`，不 panic。`kiana-core`/`kiana-interactive` 只依赖
    `kiana-tui` 的公开 trait（依赖方向 `kiana-core -> kiana-tui` 已在 §2 允许）。
18. **G1.2 `kiana-agent-core` 的 `streamProxy`**（已决 / decided — 方案 (a)）：`kiana-agent-core` 直接加
    `reqwest`，`stream_proxy` 保持 TS 的**直连**形状（`POST ${proxyUrl}/api/stream`，不经过
    `kiana_ai::http::HttpClient` 注入点，因为 TS 的 `proxy.ts` 就是用全局 `fetch`，与 provider 的
    `options.fetch` 无关）。公开签名固定为 `(model, context, options) -> AssistantMessageEventStream`（与现有 provider 适配层和扩展 API 的调用方一致），
    `options` 里带 `signal`/`auth_token`/`proxy_url`。
19. **G1.3 `kiana-ai` 的 HTTP 实现**（已决 / decided）：骨架的单文件 `src/http.rs` 拆成目录
    `src/http/{mod.rs,reqwest_client.rs}`（模块树见 §5，§4.9 的 `HttpClient` 契约**不改名**）。
    `mod.rs` 由架构者冻结（`HttpRequest`/`HttpResponse`/`HttpError`/`ByteStream`/`SharedHttpClient`），
    `reqwest_client.rs` 由 ai-team 实现。`fetch` 注入点保证 provider 适配层不直接依赖 reqwest；
    `HttpError::message` 是用户可见契约，使用 `error.message` 的逐字文本（reqwest 的错误文本要转写，
    不要把 `reqwest::Error` 的 Display 直接透出）。不自动重试：重试在 provider 层。
20. **G2.4 Photon 等价物**（已决 / decided — 原生 `photon-rs`，接受末位舍入）：TS 的
    `@silvia-odwyer/photon` 0.3.3 / `photon-node` 0.3.4 都是 wasm；Rust 用同一份 Photon 源码的
    `photon-rs = "=0.3.3"` **原生**编译（内部就是 `image 0.24` 的 `imageops::resize`）。
    2026-10-26 实测（320×240、Lanczos3）：JPEG quality 75 与 photon-node 的 wasm **逐字节相同**；
    PNG 的像素数据只差 **1 个通道的末位**（307,200 字节中的 2 个字节，来自解码器版本差异），
    编码层（`image 0.25` 的 PNG 编码器）与 photon-rs 逐字节相同。结论：**不引入 wasm 运行时**、
    不手写 Lanczos3；`image-process`/`image-resize-*`/`tool-result-images` 的黄金测试只比对
    尺寸/格式/base64 限额，PNG 的字节比对放宽（差异记入各 spec 的诚实清单）。落点：
    `kiana-durable::images::{node,exif,cloudflare}`（feature `images`）与 `kiana-core::image_process`。
21. **G2.1 模型目录的 `build.rs` 契约**（已决 / decided）：数据已 vendored（第 5 条）。`build.rs`
    的目录选择顺序是 **`KIANA_MODEL_DATA_DIR`（若设置）→ 仓库内 `crates/kiana-ai/data/`**；两者都缺
    才 `panic!`（等价 TS 的 `check:model-data` 门禁），**不要**生成空表静默通过。必须：
    对每个 JSON 发 `cargo:rerun-if-changed=`；按 `.manifest.json` 校验 sha256；键顺序按
    `spec/ai-core.md` §2.3.1 的 42 个 provider。生成物是 `providers/catalog.rs` 的
    `MODELS`/`IMAGE_MODELS`/`CLASSIFIER_MODELS` 三张 `IndexMap`；`getBuiltinModelDataGeneratedAt()`
    解析 `.manifest.json` 的 `generatedAt`。
22. **G2.2 codemode 的 VM 后端与 feature**（已决 / decided — wasm 优先）：
    `kiana-codemode/Cargo.toml` 的 feature 为 `default = []`、`wasm = [wasmtime]`（首选，嵌入与 TS
    同一份 `quickjs-wasi` 3.6.2 的 `quickjs.wasm`，只跑 core module）、`quickjs = [rquickjs]`
    （备选，不实现）、`cloudflare = []`（第 3 条）。阶段 1 末把 `wasm` 加进 default。
    prelude 已从 `spec/agent-codemode.md` 附录 A 逐字导出为 `crates/kiana-codemode/js/prelude.js`
    （17,233 字节，FNV-1a `8c6c9242`），由 `runtime/prelude_source.rs` 用 `include_str!` 引用；
    测试断言字节数与哈希。
23. **G4.3 `node:path` 的唯一实现位置**（已决 / decided）：`kiana-durable::env::path::{posix,win32}`
    （`kiana-durable/src/env/path/**`）。`kiana-env-client` 通过
    `pub use kiana_durable::env::path::posix as node_path;` 复用，**不得**出现第二份实现；
    `kiana-core::paths` 是 coding-agent 的 `utils/paths.ts`，不是 `node:path`，不参与。
    同时 `kiana-durable` 已加 `encoding_rs`（`env/decode.rs` 的 `StreamDecoder`），`kiana-env-client`
    直接复用 `kiana_durable::env::decode`，不自己再加一份解码器。
24. **G5.1 版本常量**（已决 / decided — 字面量 = TS `package.json` 的当前值 `1.4.0`）：
    `kiana_ai::KIANA_AI_VERSION = "1.4.0"`（`packages/ai/package.json`）、`kiana_core::config::VERSION =
    "1.4.0"`（`packages/coding-agent/package.json`）、`PACKAGE_NAME = "@earendil-works/kiana-coding-agent"`、
    `APP_NAME = "kiana"`、`APP_TITLE = "π"`、`CONFIG_DIR_NAME = ".kiana"`。**不读 `CARGO_PKG_VERSION`**
    （workspace 是 `0.1.0`），也**不新增**构建期注入变量（避免 CI 忘记设置时静默发错版本）；
    CI 校验这两个常量分别等于对应 `package.json` 的 `version`。`kiana-env` 的 `KIANA_ENV_VERSION` 保持
    第 7 条的构建期注入（daemon 是独立二进制、需要与 npm 包对齐，且已有实现）。
25. **G5.2 npm 分发**（已决 / decided — M1 不做）：Rust 版只发二进制（5 个 `[[bin]]`），
    **不**继续发 npm 包（不引入 napi/postinstall 下载器）。因此 `spec/ca-cli.md` 的
    `package-distribution.test.ts` 属于“不移植”，替代为 `cargo build --release` 的产物布局断言
    （bin 名、`--version` 输出、`getPackageDir()` 的 `KIANA_PACKAGE_DIR` 覆盖）。G2.3 的
    docs/examples/schemas 资源定位与 G3.4 的 CI 章节随发布阶段（阶段 8）单独决策。
26. **Windows 分支**（已决 / decided）：spec 明确描述的平台分支（`windows-self-update.ts`、
    `sys/windows.rs`、Win32 剪贴板/VT 输入、Job Objects、命名管道）按 `cfg(windows)` **移植**，
    不放 `todo!()` 了事；但 **CI 与验收目标是 linux + macOS**——Windows 代码只在 `cargo check
    --target x86_64-pc-windows-msvc` 下保证编译，不进入黄金测试矩阵。Termux 分支按 `cfg(unix)`
    + 运行时探测处理。
27. **locale 基线**（已决 / decided — en-US 为基准 + ICU4X 可选）：**en-US 是基线**——
    `Intl`/`toLocaleString`/`localeCompare` 在**没有** locale 数据时的输出按 en-US 实现
    （`spec/design-extension-runtime.md` §6.4 的“无 locale 数据”路径），这也是阶段 0–4 的
    唯一行为。`kiana-extensions` 的 `locale-data` feature（ICU4X 2.4.0）是**可选**依赖：
    阶段 5 末随第 4 条一起进 default（`default = ["runtime","locale-data"]`），届时开启 20 个
    locale 的对照表；不带该 feature 的构建必须继续只走 en-US 路径并保持测试绿（`--no-default-features`
    回归）。`localeCompare` 在 feature 关闭时回退字节序；已知差异按 §7.4 的诚实清单记录。
28. **G1.4 `kiana-env-client` 的依赖已补齐**（已决 / decided）：`sha2`（上传校验）、`base64`
    （`utf16le` → base64 的 `-EncodedCommand` 与 76 列分块）、`hex`、`getrandom`（serve token）、
    `url`（`fileURLToPath`）。`utf16le` 编码手写（`base64` crate 不含），写进 `ssh.rs` 的注释。
29. **G1.6 其余 crate 的 HTTP/crypto**（已决 / decided — 不加新 pin，按阶段补 `workspace = true`）：
    根 `Cargo.toml` 已有全部所需 pin（`reqwest`/`sha2`/`base64`/`hex`/`flate2`/`url`/`uuid`/
    `getrandom`）。`kiana-builtin-extensions`、`kiana-resources`、`kiana-config`、`kiana-interactive` 各自的
    owner 在自己的阶段开工时把依赖写进 `Cargo.toml`（`{ workspace = true }`），**不需要**再找架构者。
    共享同一个 `reqwest::Client`（由 `kiana-core::http_dispatcher` 构造）以免代理/超时行为不一致。
30. **`kiana-mcp` / `kiana-tui` / `kiana-durable` / `kiana-env-client` 的依赖已补齐**（已决 / decided）：
    `kiana-mcp`：`sha2`（PKCE S256）、`getrandom`、`uuid`、`futures-core`（SSE）；
    `kiana-tui`：`tokio`/`tokio-util`/`async-trait`/`futures-core` + 第 17 条的平台依赖；
    `kiana-durable`：`encoding_rs`/`uuid`/`sha2`/`dirs`/`url` + feature `images` 的 `photon-rs`；
    `kiana-env-client`：第 28 条。**没有任何 crate 允许直接写版本号**（§7.0 规则 2）。

31. **`kiana-json::js_parse::parse` 的孤立代理项 → U+FFFD**（已决 / decided — 被迫偏差）：
    V8 的 `JSON.parse` 接受 `"\uD800"` 并产生孤立代理项（`JSON.stringify` 会写回 `"\ud800"`）；
    Rust 的 `String` 不能表示孤立代理项。`parse` **接受**该输入（接受度必须与 V8 一致，否则
    `parse_with_repair`/`parse_streaming` 的回退层数与 TS 不同），但把每个未配对 code unit
    替换为 U+FFFD，再次序列化得到 `"\uFFFD"`。测试用 fixture 的 `value_fffd` 列比对。

32. **`kiana-json::js_parse::parse` 的非有限数 → `Value::Null`**（已决 / decided — 被迫偏差）：
    V8 的 `JSON.parse("1e400")` 是 `Infinity`，而 `serde_json::Number::from_f64` 对非有限值
    返回 `None`，§4.9 的签名又冻结为 `Value`。`parse` 接受该输入并存成 `null`，因此
    `JSON.stringify` 的文本一致（`JSON.stringify(Infinity) === "null"`）。**不等价**的观察点：
    `String(1e400)`（TS `"Infinity"` vs Rust `"null"`）、`Boolean`（`true` vs `false`）、
    `typeof`（`"number"` vs `"object"`），以及 `partial-json` 的 `NaN`/`Infinity` 字面量
    （同样落 `Null`，用 `PartialKind::NonFiniteNumber` 标注以免被 `result ?? {}` 换掉）。
    要消除偏差需要自定义数字类型，属于 §4.9 契约变更，另开条目再议。

33. **`kiana-json` 的深度处理**（已决 / decided）：`JSON.parse`/`JSON.stringify` 在 V8 里
    5,000,000 层嵌套仍成功；Rust 的递归实现在 ~16k 层就 abort 进程，而模型可控的 JSON 只要
    ~28 KB 就能触发。因此 `js_parse::parse`（显式栈）、`js_object::{sort_js_keys,
    stringify, stringify_pretty, deep_equal}` 与自写的 partial-json 解析器全部改成**迭代**
    实现。残留限制来自 `serde_json::Value` 自身的递归 `Drop`（外部类型，8 MB 栈上约
    100k–1M 层）；本 crate 不包装 `Value`，故不处理。`kiana-ai` 的流式解析器若也构造深 `Value`，
    需自行注意这一点。

---

## 附录 A：workspace pin 表

`Cargo.toml` 的 `[workspace.dependencies]` 共 119 项（31 个 path crate + 88 个外部 crate），
全部精确 pin（`=x.y.z`），2026-10-26 用 `cargo info` 校验并由 `cargo check --workspace` 实际解析。
未被任何 crate 引用的条目是惰性的（Cargo 不解析），保留它们是为了后续阶段不必临时选版本。

| crate | 版本 | 特性 |
|---|---|---|
| serde | 1.0.229 | derive, rc |
| serde_json | 1.0.154 | preserve_order, raw_value |
| indexmap | 2.14.2 | serde |
| ryu | 1.0.20 | — |
| num-bigint | 0.4.6 | — |
| num-traits | 0.2.19 | — |
| unicode-width | 0.2.2 | — |
| unicode-segmentation | 1.13.3 | — |
| unicode-normalization | 0.1.26 | — |
| encoding_rs | 0.8.42 | — |
| percent-encoding | 2.3.2 | — |
| url | 2.5.8 | — |
| base64 | 0.23.1 | — |
| hex | 0.4.3 | — |
| mime | 0.3.17 | — |
| sha2 | 0.11.1 | — |
| hmac | 0.13.0 | —（Bedrock 手写 SigV4，§8 第 6 条） |
| uuid | 1.29.0 | v4, v7 |
| getrandom | 0.4.3 | — |
| rand | 0.9.2 | — |
| regex | 1.13.1 | — |
| fancy-regex | 0.16.2 | — |
| glob | 0.3.3 | — |
| globset | 0.4.20 | — |
| ignore | 0.4.33 | — |
| semver | 1.0.27 | — |
| tokio | 1.54.1 | rt, rt-multi-thread, macros, sync, time, process, io-util, fs, net, signal |
| tokio-util | 0.7.20 | — |
| futures | 0.3.34 | — |
| futures-core | 0.3.34 | — |
| async-trait | 0.1.89 | — |
| libc | 0.2.190 | — |
| thiserror | 2.0.21 | — |
| anyhow | 1.0.100 | —（禁止在库代码使用） |
| tracing | 0.1.44 | — |
| tracing-subscriber | 0.3.20 | — |
| notify | 8.2.1 | — |
| tempfile | 3.20.0 | — |
| dirs | 6.0.0 | — |
| chrono | 0.4.42 | — |
| nix | 0.30.1 | signal, process |
| x11rb | 0.14.0 | —（`kiana-tui` feature `x11` 的原生剪贴板/修饰键） |
| objc2 | 0.6.5 | —（`kiana-tui` feature `darwin`） |
| objc2-foundation | 0.3.2 | — |
| objc2-app-kit | 0.3.2 | — |
| socket2 | 0.6.0 | — |
| windows-sys | 0.61.2 | Win32_Foundation, Win32_Security, Win32_Storage_FileSystem, Win32_System_Console, Win32_System_DataExchange, Win32_System_IO, Win32_System_JobObjects, Win32_System_LibraryLoader, Win32_System_Memory, Win32_System_Ole, Win32_System_Threading, Win32_UI_Input_KeyboardAndMouse, Win32_UI_WindowsAndMessaging（§8 第 17、26 条） |
| reqwest | 0.13.5 | default-features=false; rustls, stream, json, gzip, deflate, brotli |
| http | 1.5.0 | — |
| http-body-util | 0.1.5 | — |
| hyper | 1.12.0 | — |
| hyper-util | 0.1.21 | — |
| tokio-tungstenite | 0.30.0 | rustls-tls-native-roots（Codex WebSocket；`kiana-chord` feature `ws`） |
| flate2 | 1.1.10 | — |
| crc32fast | 1.5.0 | — |
| zstd | 0.14.3 | —（Codex 请求体压缩） |
| rusqlite | 0.41.0 | — |
| image | 0.25.10 | — |
| kamadak-exif | 0.6.1 | — |
| photon-rs | 0.3.3 | default-features=false（Photon 等价物，§8 第 20 条） |
| rquickjs | 0.14.0 | default-features=false; std, loader, macro |
| oxc_allocator / oxc_ast / oxc_parser / oxc_semantic / oxc_transformer / oxc_codegen / oxc_span | 0.155.0 | — |
| oxc_resolver | 11.24.3 | — |
| wasmtime | 50.0.0 | — |
| icu_decimal | 2.4.0 | default-features=false; compiled_data, alloc |
| icu_datetime | 2.4.0 | default-features=false; compiled_data |
| icu_time | 2.4.0 | default-features=false; compiled_data |
| icu_locale | 2.4.0 | — |
| insta | 1.43.2 | — |
| wiremock | 0.6.5 | — |
| proptest | 1.12.0 | — |
| similar | 3.2.1 | — |

说明：

- `reqwest` 0.13 的特性名是 **`rustls`**（不是旧版的 `rustls-tls`）；`default-features = false`
  以避免拉起 `native-tls`。
- `rquickjs` 的 `default-features = false` + `std`/`loader`/`macro` 已由原型实测足够
  （`spec/design-extension-runtime.md` §1.2、附录 A 的 `/tmp/minfeat`）。
- `globset = 0.4.20` 是为与 `ignore 0.4.33` 的内部依赖对齐（更高版本会被 Cargo 解析成两份）。
- `edition = "2024"`、`rust-version = "1.97"` 来自 oxc 0.155 的 MSRV；工具链实测
  `cargo/clippy 0.1.99 (rustc 1.99.1)`。
- 2026-10-26 架构者新增的 7 个 pin（`hmac`、`x11rb`、`objc2`、`objc2-foundation`、`objc2-app-kit`、
  `zstd`、`photon-rs`）全部用 `cargo info name@x.y.z` 校验存在，并由 `cargo check --workspace`
  实际解析过；`photon-rs 0.3.3` 内部依赖 `image 0.24.9`，与 pin 的 `image 0.25.10` 并存
  （两份 `image` 是**刻意**的：0.25 用于新代码，0.24 只被 photon-rs 拉入，见 §8 第 20 条）。

## 附录 B：骨架自检

```
cargo check --workspace --all-targets                      # 0 warning
cargo clippy --workspace --all-targets                      # No issues found
cargo clippy -p kiana-tui --features x11,native --all-targets  # No issues found
cargo clippy -p kiana-durable --features images,sqlite,cloudflare,node,wasm --all-targets
cargo clippy -p kiana-core --features images --all-targets     # No issues found
cargo fmt --all -- --check                                 # clean
cargo test --workspace                                     # 26 passed（60 suites）
cargo test -p kiana-tui --features x11,native                 # 6 passed（native_clipboard）
```

> 2026-10-26 架构者复跑（`CARGO_TARGET_DIR=target`、`CARGO_BUILD_JOBS=4`）：以上全部通过；
> `x11`/`images`/`sqlite`/`cloudflare`/`node`/`wasm` 这些**非默认 feature** 组合也单独 clippy 过，
> 以免默认构建掩盖 feature 门控里的 dead code/未用依赖。

骨架规模（2026-10-26 快照）：30 个 crate、756 个 `.rs` 文件、10,516 行（含注释与契约类型）；
当前规模为 32 个 crate（新增 `kiana-hooks`、`kiana-memory`）；其中真正实现的
跨 crate 契约约 6,800 行（`kiana-json` 约 2,300、`kiana-telemetry` 766、`kiana-ai` 的 `types.rs`/`error.rs`/
`event_stream.rs`/`http/` 1,630、`kiana-tui` 的 `tui.rs`/`terminal_colors.rs`/`colors.rs`/`native/`
约 1,100、`kiana-chord` 的 `wire.rs`/`errors.rs` 686、`kiana-mcp` 的 `jsonrpc.rs` 333、
`kiana-agent-core/src/proxy.rs` 与 `kiana-ai/src/utils/streaming_json.rs` 约 480）。
