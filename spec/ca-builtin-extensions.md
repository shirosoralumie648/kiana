# `ca-builtin-extensions` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-builtin-extensions** 单元（四个内置扩展：MCP 集成、
codemode 工具、tool_search 工具、llama.cpp 提供者与 `/llama` 管理界面）的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-builtin-extensions`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）。
> `APP_NAME` = `kiana`，`APP_TITLE` = `π`，`CONFIG_DIR_NAME` = `.kiana`，`VERSION` = `1.4.0`
> （取自 `packages/coding-agent/package.json`，见 `ca-cli` §2.6）。
> `ENV_AGENT_DIR` = `KIANA_CODING_AGENT_DIR`；`getAgentDir()` 见 `ca-cli`。`getDocsPath()` = `<package>/docs`。
> 本单元**不修改** TS 树中的任何文件；所有路径均以 `packages/coding-agent/` 为根（另有说明者除外）。

---

## 目录

0. 单元边界与阅读顺序
1. 概览
2. 逐文件规范
   2.1 `src/extensions/index.ts`
   2.2 `src/extensions/mcp/config.ts`
   2.3 `src/extensions/mcp/log.ts`
   2.4 `src/extensions/mcp/tools.ts`
   2.5 `src/extensions/mcp/resources.ts`
   2.6 `src/extensions/mcp/runtime.ts`
   2.7 `src/extensions/mcp/oauth.ts`
   2.8 `src/extensions/mcp/ui.ts`
   2.9 `src/extensions/mcp/index.ts`
   2.10 `src/extensions/mcp/cli.ts`
   2.11 `src/extensions/codemode/tool.ts`
   2.12 `src/extensions/codemode/execute.ts`
   2.13 `src/extensions/codemode/renderer.ts`
   2.14 `src/extensions/codemode/index.ts` 与 lazy/worker 入口
   2.15 `src/extensions/tool-search/tool.ts` 与 `index.ts`
   2.16 `src/extensions/llama/client.ts`
   2.17 `src/extensions/llama/provider.ts`
   2.18 `src/extensions/llama/huggingface.ts`
   2.19 `src/extensions/llama/index.ts`
   2.20 `src/extensions/llama/ui.ts`
   2.21 文档（`docs/mcp.md`、`docs/codemode.md`、`docs/llama-cpp.md`、`docs/cli.md`、`docs/environment-variables.md`）
3. 用户可见表面（逐字）
4. 外部格式（磁盘 / 环境变量 / HTTP / SSE / 会话条目）
5. 跨单元接口（精确类型）
6. npm 依赖与 Rust crate 映射（含行为桥接）
7. 测试清单
8. Rust 模块布局与最难复现的部分
9. 附录 A：逐字字符串索引（按文件）
10. Audit additions（对抗性完整性审计追加：就地修正之外的补全与交叉核对）

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
src/extensions/index.ts                  (14 行)   内置扩展注册表
src/extensions/mcp/index.ts            (1347 行)   MCP 扩展主体（生命周期、工具注册、菜单、/mcp）
src/extensions/mcp/config.ts             (243 行)   mcp.json 读取 / 合并 / 编辑
src/extensions/mcp/log.ts                 (69 行)   mcp.log 追加与轮转
src/extensions/mcp/tools.ts              (391 行)   MCP 工具 → kiana ToolDefinition 适配
src/extensions/mcp/resources.ts          (339 行)   三个资源工具
src/extensions/mcp/runtime.ts            (454 行)   连接、传输、重试、状态机
src/extensions/mcp/oauth.ts              (558 行)   OAuth 凭据库、刷新锁、登录流程
src/extensions/mcp/ui.ts                 (259 行)   /mcp 管理器视图
src/extensions/mcp/cli.ts                (594 行)   `kiana mcp` 子命令
src/extensions/mcp/runtime.lazy.ts         (2 行)
src/extensions/mcp/cli.lazy.ts             (2 行)
src/extensions/codemode/tool.ts          (411 行)   codemode 工具定义与描述生成
src/extensions/codemode/execute.ts       (850 行)   沙箱执行、worker 准入、输出格式化
src/extensions/codemode/renderer.ts      (156 行)   codemode 渲染器
src/extensions/codemode/index.ts          (49 行)   codemode 扩展
src/extensions/codemode/execute.lazy.ts    (2 行)
src/extensions/codemode/worker.ts          (6 行)
src/extensions/tool-search/tool.ts       (244 行)   BM25、tokenizer、tool_search 工具
src/extensions/tool-search/index.ts       (18 行)
src/extensions/llama/client.ts           (348 行)   llama.cpp HTTP 客户端
src/extensions/llama/provider.ts         (299 行)   llama.cpp Provider
src/extensions/llama/huggingface.ts      (158 行)   Hugging Face 搜索
src/extensions/llama/index.ts            (230 行)   /llama 命令逻辑
src/extensions/llama/ui.ts               (542 行)   /llama TUI
```

合计 **7 585** 行。

相关文档（行为权威，必须一致）：

```
docs/mcp.md                     (272 行)
docs/codemode.md                (230 行)
docs/llama-cpp.md               (121 行)
docs/cli.md                     （“Tools”“MCP commands”章节）
docs/environment-variables.md   (102 行，本单元相关的只有 KIANA_CODEMODE_MAX_WORKERS 一行)
```

相关测试（第 7 节逐条列出）：

```
test/mcp-extension.test.ts                     (29 个 it)
test/mcp-command.test.ts                        (8 个 it)
test/mcp-oauth-store.test.ts                    (3 个 it)
test/mcp-oauth-refresh.test.ts                 (11 个 it)
test/mcp-manager-view.test.ts                   (2 个 test)
test/codemode-renderer.test.ts                  (5 个 it)
test/codemode-worker-config.test.ts             (3 个 test)
test/codemode-worker-admission.test.ts          (4 个裸 it + 1 个 it.each（3 个参数）= 5 处声明)
test/llama-extension.test.ts                   (15 个 it)
test/tool-search.test.ts                        (9 个 it)
test/suite/agent-session-mcp.test.ts          (1273 行；44 个裸 it + 4 个 it.each = 48 处声明)
test/suite/agent-session-mcp-oauth.test.ts      (9 个 it)
test/suite/agent-session-codemode.test.ts      (25 个 it)
test/suite/regressions/10285-mcp-tool-renderers.test.ts (2 个 it)
test/suite/mcp-oauth-server.ts                  （测试用 OAuth 服务器，182 行）
test/mcp-conformance/{README.md,client.ts,run.ts,baseline.json}
```

### 0.2 本单元**不**实现（由其它单元拥有，本单元只调用）

| 拥有单元 | 内容 | 本单元如何消费 |
|---|---|---|
| `telemetry-mcp` | `@earendil-works/kiana-mcp` 整包：`McpClient`、`StdioTransport`、`StreamableHttpTransport`、`InMemoryTransport`、JSON-RPC、`CallToolResult`/`ContentBlock`/`toLlmContent`、`McpError`/`McpHttpError`/`McpSessionExpiredError`/`McpAuthRequiredError`、`@earendil-works/kiana-mcp/oauth`（`McpOAuthProvider`、`authorizeMcp`、`OAuthCallbackServer`、`parseWwwAuthenticate`、`stepUpScope`、`OAuthIssuerMismatchError`、`McpOAuthAuthorizationRequiredError`、`McpOAuthState`） | 直接被 `runtime.ts`、`oauth.ts`、`tools.ts`、`resources.ts`、`cli.ts` 导入 |
| `agent-codemode` | `@earendil-works/kiana-codemode`：`CodemodeSandbox`、`parseCodemodeSource`、`renderToolSample`、`renderToolOutputType`、`toCodemodeIdentifier`、`CODEMODE_SOURCE_GRAMMAR`、`loadQuickJSWasm`、worker 入口 | 被 `tool.ts`、`execute.ts` 导入 |
| `ai-core` / `ai-api-*` | `kiana-ai`：`Model`/`Provider`/`ClassifierModel`/`Usage`/`TextContent`/`ImageContent`/`JsonValue`、`stream`/`streamSimple`、`llamaCppClassifyApi`、`typesafeSystemOneApi`、`isModelType` | `llama/provider.ts`、`codemode/execute.ts`、`mcp/tools.ts`、`mcp/resources.ts`、`mcp/oauth.ts`（`oauthErrorHtml`/`oauthSuccessHtml`） |
| `tui-core` / `tui-text` / `tui-components` | `kiana-tui`：`Component`、`Container`、`Focusable`、`Input`、`SelectItem`、`SelectList`、`Spacer`、`Text`、`TUI`、`truncateToWidth`、`visibleWidth`、`fuzzyFilter`（仅 `llama/ui.ts`） | `mcp/ui.ts`、`mcp/index.ts`、`llama/ui.ts`、`codemode/renderer.ts` |
| `ca-resources` | `core/mcp-servers.ts`：`McpExposure`、`McpServerConfig`、`mcpNamespace`、`validateMcpServerConfig`、`getMcpToolExposure`、`createToolNameMatcher`、`LIST_MCP_RESOURCES_TOOL` 等（见 `ca-resources` §2.9，**本文不重复**） | `mcp/config.ts`、`mcp/index.ts`、`mcp/cli.ts` |
| `ca-extensions` | `core/extensions/types.ts`（`ExtensionAPI`、`ExtensionFactory`、`ExtensionContext`、`ToolDefinition`、`ToolExposure`、`ToolNamespace`、`ToolLoadout`、`ToolInfo`、`McpServersChangeEvent`、事件与处理器类型）、`core/extensions/loader.ts`、`core/extensions/runner.ts` | 全部扩展工厂与事件处理器 |
| `ca-session` | `core/agent-session.ts`（`AgentSession`）、`core/session-manager.ts`（`SessionManager`、`SessionEntry`、`CustomEntry`）、`core/messages.ts`、`core/nested-tool-calls.ts` | `kiana.appendEntry`、`codemode-store` 条目、嵌套工具调用上下文 |
| `ca-config-models` | `core/settings-manager.ts`（`Settings`、`CodemodeMode`）、`core/settings-defaults.ts`（`SETTINGS_DEFAULTS`）、`core/auth-storage.ts`（`FileAuthStorageBackend`、`InMemoryAuthStorageBackend`、`AuthStorageBackend`）、`core/model-registry.ts`（`ModelRegistry`）、`core/trust-manager.ts`（`ProjectTrustStore`）、`core/usage-totals.ts`（`combineUsage`）、`core/keybindings.ts`（`KeybindingsManager`）、`core/resolve-config-value.ts`（`resolveConfigValueOrThrow`、`resolveHeadersOrThrow`：`$NAME` / `${NAME}` / `!command`） | settings/auth/trust/model registry、codemode 预算与 usage 合并、`env`/`headers`/`oauth.clientSecret` 解析 |
| `ca-tools-compaction-export` | `core/tools/truncate.ts`（`truncateMiddle`、`formatSize`）、`core/tools/render-utils.ts`（`formatToolCallWithArgs`、`getTextOutput`、`replaceTabs`、`startToolTimer`、`toolTimerText`、`str`）、`utils/output-files.ts`（`writeOutputFile`）、`modes/interactive/components/{keybinding-hints,visual-truncate}.ts`、`modes/interactive/theme/theme.ts`（`theme` 全局代理、`Theme`、`getSelectListTheme`、`highlightCode`）、`utils/text.ts`（`stripBom`） | MCP / codemode / llama |
| `ca-interactive-components` | `modes/interactive/components/{dynamic-border,auth-url}.ts`（`DynamicBorder`、`AuthUrlComponent`，其 `copy()` 内部用剪贴板） | `mcp/ui.ts`、`mcp/tools.ts`、`llama/ui.ts`、`codemode/renderer.ts` |
| `ca-cli` | `config.ts`（`getAgentDir`、`getDocsPath`、`getQuickJSWasmPath`、`getCodemodeWorkerSpecifier`、`resolveCodemodeWorkerSpecifier`、`VERSION`、`APP_NAME`、`CONFIG_DIR_NAME`）、`utils/open-browser.ts`（`openBrowser`）、`main.ts` 中的 `mcp` 分支 | `mcp/index.ts`、`mcp/cli.ts`、`codemode/execute.ts` |

**边界不变量**：

- `core/mcp-servers.ts` 的校验/注册表逻辑由 `ca-resources` 拥有，本文只写“被谁调用、期望什么”。
- `kiana-mcp` 的线协议、stdio/HTTP 传输、SSE、OAuth 提供者算法由 `telemetry-mcp` 拥有；
  本文只写**本单元如何使用它们**（超时、重试、挑战处理、状态机）。
- `kiana-codemode` 的 QuickJS VM、prelude、worker 协议由 `agent-codemode` 拥有；
  本文只写**本单元的沙箱构造参数、worker 准入与输出整形**。
- `/mcp`、`/mcp login|logout|reconnect`、`/llama` 三个斜杠命令属于本单元；
  `kiana mcp ...` 子命令也属于本单元（`main.ts` 只负责识别 `args[0] === "mcp"` 并转交）。

### 0.3 与兄弟单元的重叠点（必须对齐）

1. **`mcp__<server>__<tool>` 命名**：`mcpNamespace()`（`ca-resources`）与 `createMcpToolName()`（本单元）是
   两个不同的函数。前者把 `-` → `_` 得到命名空间；后者把**所有非 `[A-Za-z0-9_]`** 换成 `_` 并做 64 字符
   截断 + sha256 后缀。二者必须同时存在且语义不同。
2. **exposure 映射**：`McpExposure`（4 值）→ `ToolExposure`（5 值）：
   `codemode → deferred`，其余同名。见 §2.4 `toToolExposure`。
3. **`codemode` 的 `reachesTools`**：MCP 扩展的 `tool_call` 处理器依赖它决定“等哪些服务器”。
   实现必须让 `ToolDefinition.reachesTools` 在 Rust 中可被 MCP 扩展调用（见 §5）。
4. **`mcp_servers` 系统提示 section**：本单元写入 `systemPromptOptions.sections["mcp_servers"]`；
   系统提示拼装由 `ca-session` 拥有。
5. **`kiana mcp` CLI**：`ca-cli` 负责 `args[0] === "mcp"` 的分派与退出码；本单元的 `runMcpCommand` 返回 `number`。
6. **`KIANA_CODEMODE_MAX_WORKERS`**：文档在 `docs/environment-variables.md`，实现在本单元 `execute.ts`。

### 0.4 关键不变量（Rust 中必须逐条成立）

1. **字节级文案**：所有【逐字】字符串必须完全一致，包括末尾句号、双引号、`…`(U+2026)、
   `·`(U+00B7)、`✓`/`✗`/`⊘`/`█`/`─`、`→`、缩进空格数与换行数。
2. **顺序确定性**：服务器排序、工具排序、菜单排序、BM25 排序、catalog 选择、Map 迭代顺序都必须可复现。
   `localeCompare` 在 TS 中走 ICU；Rust 端必须使用等价排序（见 §8.3）。
3. **大小限制**：20 KiB MCP 输出、4096 字符 servers section、250 字符服务器描述、64 字符工具名、
   5 MiB 日志轮转、256 MiB VM、2 个 worker、60 秒 worker 等待、15 秒 HTTP 超时、30 秒刷新偏移。
4. **幂等与去重**：同一工具的重复注册（`registerTool` 同名）必须走“后注册者替换，且保留 hidden 兼容”。
5. **生命周期**：`session_shutdown` 之后不得再有 `ctx.ui.*` 调用；迟到的异步结果必须丢弃。
6. **文件写入**：`mcp.json` 重写时保留缩进与其它字段，末尾恰好一个 `\n`；`mcp-auth.json` 用 2 空格缩进 + `\n`。
7. **不修改 TS 树**：本单元的所有行为都以只读方式参考。

### 0.5 建议阅读顺序

1. `src/extensions/index.ts` → 了解四个内置扩展与 `replaceable`/`builtin` 语义。
2. `mcp/config.ts` → `mcp.json` 的形状、scope、override。
3. `mcp/tools.ts` → 工具名、结果转换、输出截断（后面到处复用）。
4. `mcp/runtime.ts` → 状态机与重试；`mcp/oauth.ts` → 凭据与登录。
5. `mcp/index.ts` → 生命周期与 `/mcp`。
6. `mcp/cli.ts` → `kiana mcp`。
7. `tool-search/tool.ts` → BM25（codemode 也用它）。
8. `codemode/tool.ts` → 描述生成；`codemode/execute.ts` → 执行与输出。
9. `llama/*`。

---

## 1. 概览

### 1.1 目的

四个内置扩展把外部能力接到 kiana 的工具管道上：

- **MCP**（`createMcpExtension`）：连接 `mcp.json` 与 `kiana.registerMcpServer()` 注册的服务器，
  把服务器工具注册为 `mcp__<server>__<tool>`，按 exposure 决定模型如何触达；
  附带三个资源工具、`/mcp` 管理器、`kiana mcp` CLI、OAuth 登录与 `mcp_servers` 系统提示 section。
- **codemode**（`createCodemodeExtension`）：注册 `codemode` 工具（默认**非激活**），
  让模型写 JavaScript 在 QuickJS 沙箱里批量调用其它工具、跑分类器/图像模型。
- **tool_search**（`createToolSearchExtension`）：注册 `tool_search` 工具（默认**非激活**），
  用 BM25 搜索尚未声明的工具并激活它们。
- **llama.cpp**（`llamaExtension`）：注册原生 Provider `llama.cpp`（router 模式）与 `/llama` 管理界面
  （加载/卸载/下载模型、Hugging Face 搜索、进度条）。

### 1.2 在 monorepo 中的位置

```
kiana-ai ──────────────► kiana-agent-core ──────► coding-agent
   ▲                        │                 ├─ core/extensions（契约）
   │                        │                 ├─ extensions/mcp      ◄── kiana-mcp
   │                        │                 ├─ extensions/codemode ◄── kiana-codemode（+ quickjs-wasi）
   │                        │                 ├─ extensions/tool-search
   │                        │                 └─ extensions/llama     ◄── kiana-ai（Provider/Classifier API）
   └────────────────────────┴─────────────────┘
```

- 依赖方向：本单元 → `kiana-mcp`、`kiana-codemode`、`kiana-ai`、`kiana-tui`、`coding-agent` 内部 core。
- **反向依赖**：`core/extensions/types.ts` 只以**类型**引用 `extensions/mcp`? 否——`types.ts` 不导入本单元；
  `src/index.ts` 重导出 `createMcpExtension`、`createCodemodeExtension`、`createToolSearchExtension`（见 `ca-cli` §2.8）。
  `extensions/index.ts` 是唯一聚合点。

### 1.3 入口点

| 入口 | 位置 | 触发 |
|---|---|---|
| `builtInExtensions` 数组 | `src/extensions/index.ts` | 资源加载器构造内置扩展 |
| `createMcpExtension(options?)` | `mcp/index.ts` 末尾 `export default createMcpExtension()` | 同上 |
| `createCodemodeExtension(options?)` | `codemode/index.ts` 末尾 `export default` | 同上 |
| `createToolSearchExtension()` | `tool-search/index.ts` 末尾 `export default` | 同上 |
| `llamaExtension(kiana)` | `llama/index.ts` 末尾 `export default function llamaExtension` | 同上 |
| `runMcpCommand(args, options)` | `mcp/cli.ts:185` | `main.ts:618-619`：`args[0] === "mcp"` 时 `loadMcpCommand()` |
| `/mcp` 斜杠命令 | `mcp/index.ts` `kiana.registerCommand("mcp", …)` | 用户输入 |
| `/llama` 斜杠命令 | `llama/index.ts` `kiana.registerCommand("llama", …)` | 用户输入 |
| `mcp_servers` 系统提示 section | `mcp/index.ts` `before_agent_start` | 每次 prompt |
| `renderServersSection` | `mcp/index.ts` 导出 | 测试与内部 |
| `readCodemodeStore` | `codemode/execute.ts` 导出 | 测试与内部 |

`builtInExtensions` **逐字**：

```ts
export const builtInExtensions: InlineExtension[] = [
	{ name: "llama.cpp", factory: llamaExtension, builtin: true },
	// Replaceable: an extension that registers `codemode`, `tool_search`, or `/mcp` (such as a third-party
	// MCP extension) takes over instead of running alongside the built-in one.
	{ name: "codemode", factory: codemodeExtension, replaceable: true, builtin: true },
	{ name: "tool-search", factory: toolSearchExtension, replaceable: true, builtin: true },
	{ name: "mcp", factory: mcpExtension, replaceable: true, builtin: true },
];
```

语义：`llama.cpp` **不可**被替换（`replaceable` 缺省 false）；其余三个 `replaceable: true`。
`builtin: true` 表示可用于 `-builtin:<name>` 形式的禁用（`kiana config` / `"extensions": ["-builtin:mcp"]`）。
`--no-mcp` 走的是 `disabledBuiltinExtensions: ["mcp"]`（`main.ts`），见 `ca-cli` §2.4。

### 1.4 建议移植顺序

1. `tool-search`（BM25 + tokenizer，纯函数，无 IO）——codemode 也依赖它。
2. `mcp/config.ts` + `mcp/log.ts`（纯文件 IO，易测）。
3. `mcp/tools.ts`（依赖 `kiana-mcp` 的 `toLlmContent` 与 `truncateMiddle`）。
4. `mcp/resources.ts`。
5. `mcp/runtime.ts` + `mcp/oauth.ts`（依赖 `kiana-mcp` 与锁文件）。
6. `mcp/index.ts`（装配）。
7. `mcp/ui.ts` + `mcp/cli.ts`。
8. `codemode/tool.ts` → `codemode/execute.ts` → `codemode/renderer.ts` → `codemode/index.ts`。
9. `llama/client.ts` → `huggingface.ts` → `provider.ts` → `ui.ts` → `index.ts`。
10. `extensions/index.ts` 与 `src/index.ts` 重导出。

### 1.5 符号统计

本单元源文件中 `export` 声明的符号（脚本按 `export (default)? (function|class|const|type|interface|enum) X` 与
`export { a, b as c }` 精确统计，同一文件内去重）：

| 文件 | 导出符号数 | 明细 |
|---|---|---|
| `extensions/index.ts` | 1 | `builtInExtensions` |
| `mcp/config.ts` | 13 | 类型 5（`McpExposure`/`McpStdioServerConfig`/`McpHttpServerConfig`/`McpOAuthConfig`/`McpServerConfig`）+ `getMcpToolExposure` + `McpServerEntry` + `LoadedMcpConfig` + `loadMcpConfig` + `McpServerConfigPatch` + `updateMcpServerConfig` + `addMcpServerConfig` + `removeMcpServerConfig` |
| `mcp/log.ts` | 2 | `formatMcpLogMessage`、`McpServerLog` |
| `mcp/tools.ts` | 15 | 含 `READ_MCP_RESOURCE_TOOL` 再导出、4 个类型（`McpToolDetails`/`McpOutputSaver`/`McpToolCaller`/`ConvertMcpResultOptions`） |
| `mcp/resources.ts` | 5 | 3 个常量 + `McpResourceServer` + `createMcpResourceToolDefinitions` |
| `mcp/runtime.ts` | 7 | `McpTransportFactory`、`createDefaultTransport`、`McpServerConnection` + **4** 个再导出（`McpServerLog`、`McpOAuthCredentialStore`、`McpSignInCancelledError`、`signInMcpServer`） |
| `mcp/oauth.ts` | 8 | 4 类型（`McpOAuthSettings`/`McpOAuthServerStore`/`McpAuthProvider`/`McpSignInPrompt`）+ `McpOAuthCredentialStore`、`createMcpAuthProvider`、`McpSignInCancelledError`、`signInMcpServer` |
| `mcp/ui.ts` | 4 | `McpMenu`、`McpUi`、`McpManagerView`、`showMcpManager` |
| `mcp/index.ts` | 7 | `McpTransportFactory`（类型再导出）、`McpExtensionOptions`、`MCP_SERVERS_SECTION`、`MAX_SERVERS_SECTION_CHARS`、`McpServerListing`、`renderServersSection`、`createMcpExtension` |
| `mcp/cli.ts` | 2 | `runMcpCommand`、`McpCommandOptions` |
| `mcp/runtime.lazy.ts` | 1 | `loadMcpRuntime` |
| `mcp/cli.lazy.ts` | 1 | `loadMcpCommand` |
| `codemode/tool.ts` | 19 | |
| `codemode/execute.ts` | 2 | `readCodemodeStore`、`executeCodemode` |
| `codemode/renderer.ts` | 1 | `codemodeRenderers` |
| `codemode/index.ts` | 2 | `CodemodeExtensionOptions`、`createCodemodeExtension`（default 同一工厂） |
| `codemode/execute.lazy.ts` | 1 | `loadCodemodeExecutor` |
| `codemode/worker.ts` | 0 | 仅副作用导入 |
| `tool-search/tool.ts` | 15 | |
| `tool-search/index.ts` | 1 | `createToolSearchExtension`（default 同一工厂） |
| `llama/client.ts` | 10 | |
| `llama/provider.ts` | 4 | `LLAMA_PROVIDER_ID`、`DEFAULT_LLAMA_SERVER_URL`、`LlamaProviderController`、`createLlamaProvider` |
| `llama/huggingface.ts` | 5 | |
| `llama/index.ts` | 1 | default `llamaExtension` |
| `llama/ui.ts` | 4 | `LlamaManagerAction`、`LlamaUi`、`showLlamaUi`、`runWithProgress` |
| **合计** | **131** | 其中纯类型（`type`/`interface`，含 `export type { … }` 里的类型再导出）**52** 个，值符号 **79** 个（`52 + 79 = 131`，与按文件计数一致）；扣除跨文件重复（`READ_MCP_RESOURCE_TOOL`、`McpTransportFactory`、`McpServerLog`、`McpOAuthCredentialStore`、`McpSignInCancelledError`、`signInMcpServer`）后唯一符号 **125**（由脚本对全部 25 个源文件重新统计确认） |

`StructuredOutput.exportedSymbolCount` 报告 **131**（按文件计数的导出声明总数）。

---

## 2. 逐文件规范

### 2.1 `src/extensions/index.ts`（14 行）

见 §1.3 的逐字代码。无其它逻辑。默认导出的四个工厂：

```ts
import codemodeExtension from "./codemode/index.ts";
import llamaExtension from "./llama/index.ts";
import mcpExtension from "./mcp/index.ts";
import toolSearchExtension from "./tool-search/index.ts";
```

Rust 侧对应一个 `fn built_in_extensions() -> Vec<InlineExtension>`（`ca-cli` 已声明此签名）。

---

### 2.2 `src/extensions/mcp/config.ts`（243 行）

#### 2.2.1 类型

```ts
export interface McpServerEntry {
	name: string;
	config: McpServerConfig;
	/** Config file that defined the entry, or the path of the extension that registered it. */
	source: string;
	/** "global" | "project" | "extension" */
	scope?: "global" | "project" | "extension";
	/** Project mcp.json with an override of this global server's enabled/exposure/toolExposure. */
	override?: string;
}

export interface LoadedMcpConfig {
	servers: McpServerEntry[];
	autoEnableCodemode?: boolean;
	errors: string[];
	/** The project mcp.json when the project is trusted. */
	projectConfig?: string;
}
```

再导出（**不重新实现**）：

```ts
export type { McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpStdioServerConfig }
	from "../../core/mcp-servers.ts";
export { getMcpToolExposure } from "../../core/mcp-servers.ts";
```

同时本地 `import` 了 `mcpNamespace`、`validateMcpServerConfig`。

#### 2.2.2 常量与判定

```ts
const OVERRIDE_KEYS = ["enabled", "exposure", "toolExposure"];

/** Whether an entry overrides a server defined elsewhere instead of defining one. */
function isOverride(value: Record<string, unknown>): boolean {
	return value.command === undefined && value.url === undefined && value.type === undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

interface McpConfigState {
	servers: Map<string, McpServerEntry>;
	autoEnableCodemode?: boolean;
	errors: string[];
}
```

**注意**：`isOverride` 只看三个键是否为 `undefined`。`{"type":"stdio"}` 有 `type` → **不是** override。

#### 2.2.3 `readConfigFile(path, scope, state)`

伪代码（顺序即实现顺序）：

```text
if (!existsSync(path)) return;
try { parsed = JSON.parse(stripBom(readFileSync(path, "utf8"))); }
catch (e) { errors.push(`${path}: ${msg(e)}`); return; }

if (!isRecord(parsed) || (parsed.mcpServers !== undefined && !isRecord(parsed.mcpServers))) {
	errors.push(`${path}: expected an object with an "mcpServers" object`); return;
}

if (typeof parsed.autoEnableCodemode === "boolean") state.autoEnableCodemode = parsed.autoEnableCodemode;
else if (parsed.autoEnableCodemode !== undefined) errors.push(`${path}: autoEnableCodemode must be a boolean`);

for (const [name, value] of Object.entries(parsed.mcpServers ?? {})) {
	if (scope === "project" && isRecord(value) && isOverride(value)) {
		const base = servers.get(name);
		const extra = Object.keys(value).filter(k => !OVERRIDE_KEYS.includes(k));
		if (!base)
			errors.push(`${path}: server "${name}" needs "command" or "url", or a global server to override`);
		else if (extra.length > 0)
			errors.push(`${path}: server "${name}": an override can only set ${OVERRIDE_KEYS.join(", ")}`);
		else {
			const config = validateMcpServerConfig(name, { ...base.config, ...value });
			if (typeof config === "string") errors.push(`${path}: ${config}`);
			else servers.set(name, { ...base, config, override: path });
		}
		continue;
	}
	const config = validateMcpServerConfig(name, value);
	if (typeof config === "string") { errors.push(`${path}: ${config}`); continue; }
	const clash = [...servers.keys()].find(other => other !== name && mcpNamespace(other) === mcpNamespace(name));
	if (clash) { errors.push(`${path}: server "${name}" conflicts with "${clash}"`); continue; }
	if (scope === "project" && "url" in config && config.auth) {
		errors.push(`${path}: server "${name}": auth is only allowed in the global mcp.json`); continue;
	}
	servers.set(name, { name, config, source: path, scope });
}
```

逐字错误文案（不含 `path` 前缀的由 `validateMcpServerConfig` 产生，见 `ca-resources` §2.9.7）：

| 条件 | 文案 |
|---|---|
| JSON 解析失败 | `{path}: {error.message}` |
| 顶层非对象 / `mcpServers` 非对象 | `{path}: expected an object with an "mcpServers" object` |
| `autoEnableCodemode` 非布尔 | `{path}: autoEnableCodemode must be a boolean` |
| 项目 override 无对应全局 | `{path}: server "{name}" needs "command" or "url", or a global server to override` |
| 项目 override 含额外键 | `{path}: server "{name}": an override can only set enabled, exposure, toolExposure` |
| 命名空间冲突 | `{path}: server "{name}" conflicts with "{clash}"` |
| 项目内 `auth` | `{path}: server "{name}": auth is only allowed in the global mcp.json` |

要点：

- **BOM 剥离**：`stripBom` 去掉开头 U+FEFF。
- **override 合并**：`{...base.config, ...value}`——项目值**浅覆盖**全局字段；未出现的键保留全局值（含 `env`/`headers`/`auth`）。
  合并结果**仍要再校验**（因此 override 里写非法 `enabled: "yes"` 会被拒）。
- override 成功时 `servers.set(name, {...base, config, override: path})`：`source`/`scope` **保留全局的**，
  只多一个 `override` 字段。因此 `McpServerEntry.scope === "global"` 且 `override !== undefined` 表示“全局服务器 + 项目覆盖”。
- `extra` 用 `Object.keys(value)` 顺序；错误信息里的键列表固定为 `enabled, exposure, toolExposure`（`OVERRIDE_KEYS.join(", ")`）。
- 非 override 条目**不**与已有条目去重（同名后写覆盖先写），但会检查**命名空间冲突**（`-` 与 `_` 视为同一个）。
- 冲突检查在 `servers.keys()` 上（`Map` 插入顺序），找**第一个**冲突者。
- 项目 `scope` 的 `auth` 检查发生在冲突检查**之后**、`servers.set` 之前。

#### 2.2.4 `loadMcpConfig(options)`

```ts
export function loadMcpConfig(options: { agentDir: string; cwd: string; projectTrusted: boolean }): LoadedMcpConfig
```

顺序：读 `<agentDir>/mcp.json`（scope `global`）→ 若 `projectTrusted`，`projectConfig = join(cwd, CONFIG_DIR_NAME, "mcp.json")`
并读它（scope `project`）。返回：

```ts
{
	servers: [...state.servers.values()],
	...(state.autoEnableCodemode === undefined ? {} : { autoEnableCodemode: state.autoEnableCodemode }),
	errors: state.errors,
	...(projectConfig ? { projectConfig } : {}),
}
```

- `autoEnableCodemode` 若未在**任一**文件出现则该键**不存在**（调用方默认 true）。
  项目文件的值覆盖全局文件的值（读顺序决定）。
- `projectConfig` 只要项目被信任就存在（**即使文件不存在**）——因为它是 `/mcp` 保存项目 override 的目标路径。
- 禁用服务器**包含**在结果中（`enabled: false`）。
- `servers` 顺序 = 全局文件键顺序，然后项目新增的键顺序；项目 override 不改变位置（Map.set 已存在的键不改顺序）。

#### 2.2.5 `McpServerConfigPatch` 与 `updateMcpServerConfig`

```ts
export interface McpServerConfigPatch { enabled?: boolean; exposure?: McpExposure; }

export function updateMcpServerConfig(
	path: string, name: string, patch: McpServerConfigPatch, options: { override?: boolean } = {},
): void
```

实现：

```text
editMcpServers(path, (servers, parsed) => {
	let server = servers?.[name];
	if (server === undefined && options.override) { server = {}; parsed.mcpServers = { ...servers, [name]: server }; }
	if (!isRecord(server)) throw new Error(`${path} does not define MCP server "${name}"`);
	const keepDefaults = isOverride(server);
	if (patch.enabled !== undefined) {
		if (patch.enabled && !keepDefaults) delete server.enabled;
		else server.enabled = patch.enabled;
	}
	if (patch.exposure !== undefined) {
		if (patch.exposure === "codemode" && !keepDefaults) delete server.exposure;
		else server.exposure = patch.exposure;
	}
	return true;
});
```

**默认值折叠规则**：`enabled: true` 与 `exposure: "codemode"` 是默认值，在**非** override 条目上写入时**删除键**，
以保持文件干净。override 条目（`isOverride` 真）必须保留显式值，因为它要覆盖全局。

#### 2.2.6 `addMcpServerConfig` / `removeMcpServerConfig`

```ts
export function addMcpServerConfig(path: string, name: string, config: McpServerConfig): boolean
// 存在则 replaced = true；无论存在与否都 target[name] = config 并写回。返回 replaced。

export function removeMcpServerConfig(path: string, name: string): boolean
// 文件不存在 → false（不创建文件）。
// 存在但无该服务器 → edit 返回 false（不写文件），返回 false。
// 删除成功 → true。
```

#### 2.2.7 `editMcpServers(path, edit)`

```text
const text = existsSync(path) ? stripBom(readFileSync(path, "utf8")) : undefined;
const parsed = text === undefined ? {} : JSON.parse(text);       // 解析失败直接抛出（JSON.parse 的错误）
if (!isRecord(parsed) || (parsed.mcpServers !== undefined && !isRecord(parsed.mcpServers)))
	throw new Error(`${path}: expected an object with an "mcpServers" object`);
const servers = isRecord(parsed.mcpServers) ? parsed.mcpServers : undefined;
if (!edit(servers, parsed)) return;
const indent = (text && /^([ \t]+)\S/m.exec(text)?.[1]) || "  ";
mkdirSync(dirname(path), { recursive: true });
writeFileSync(path, `${JSON.stringify(parsed, null, indent)}\n`);
```

要点（**必须逐字节复刻**）：

- **缩进探测**：正则 `/^([ \t]+)\S/m` 找第一个“行首至少一个空白 + 非空白”的缩进串；找不到则 `"  "`（两个空格）。
  TS 里 `indent` 实际是**字符串**传给 `JSON.stringify` 的 `space` 参数——`JSON.stringify` 对字符串 space
  只取其**前 10 个字符**作为缩进单元。Rust 需复刻这一截断。
- 仅当 `edit` 返回 `true` 才写文件；`remove` 不存在的服务器因此不会创建/改动文件。
- 文件创建用 `mkdirSync(..., {recursive:true})`。
- 输出 = `JSON.stringify(parsed, null, indent) + "\n"`；**键顺序** = JS 对象插入顺序（Rust 需 `serde_json` + `preserve_order`）。
- 数值序列化必须与 JS 一致（整数不带 `.0`，浮点用最短往返表示），见 §8.3。
- 不做原子替换、不加文件锁（并发写可能丢更新，与 TS 相同）。

---

### 2.3 `src/extensions/mcp/log.ts`（69 行）

```ts
const MAX_LOG_BYTES = 5 * 1024 * 1024;   // 5 MiB

export function formatMcpLogMessage(server: string, params: unknown, now: Date = new Date()): string
export class McpServerLog { readonly path: string; write(server: string, params: unknown): void }
```

#### 2.3.1 `formatMcpLogMessage`

```text
message = isRecord(params) ? params : { data: params }
level   = typeof message.level === "string" ? message.level : "info"
logger  = (typeof message.logger === "string" && message.logger) ? ` ${message.logger}:` : ""
text    = formatData(message.data).replace(/\r?\n/g, "\n    ")
return `${now.toISOString()} [${server}] ${level}${logger} ${text}\n`
```

`formatData`：字符串原样；否则 `JSON.stringify(data) ?? String(data)`；`JSON.stringify` 抛错则 `String(data)`。

**逐字示例**：`params = {level:"error", logger:"db", data:"a\nb"}`、`server="x"`、
`now = 2024-01-02T03:04:05.678Z` →

```
2024-01-02T03:04:05.678Z [x] error db: a
    b
```

（末尾带 `\n`。）

- `now.toISOString()` 固定 `YYYY-MM-DDTHH:mm:ss.sssZ`（毫秒 3 位，UTC）。
- 续行缩进**恰好 4 个空格**；`\r\n` 与 `\n` 都归一为 `\n    `。
- `message.data === undefined` → `JSON.stringify(undefined)` 返回 `undefined` → `String(undefined)` = `"undefined"`。
- 非记录 `params`（字符串、数组、null）→ 包成 `{ data: params }`，`level` 默认 `"info"`，无 logger 部分。
  注意：`isRecord(null)` 为 false → `{data:null}` → `"null"`。

#### 2.3.2 `McpServerLog.write`

```text
line = formatMcpLogMessage(server, params)
try {
	if (size === undefined) { mkdirSync(dirname(path), {recursive:true}); size = currentSize(); }
	if (size > MAX_LOG_BYTES) {
		if (currentSize() > MAX_LOG_BYTES) renameSync(path, `${path}.1`);
		size = currentSize();
	}
	appendFileSync(path, line);
	size += Buffer.byteLength(line);
} catch { /* ignore */ }
```

- `currentSize()`：`statSync(path).size`，异常返回 0。
- 轮转条件 `size > MAX`（严格大于）；轮转前**再次**检查磁盘上的真实大小（其它进程可能已轮转）。
- 覆盖式 `rename`（POSIX 上原子替换 `mcp.log.1`）。
- 全部错误吞掉：日志是 best-effort。
- 每次 write 一次同步 append，保证多进程下每条消息是单次追加。

---

### 2.4 `src/extensions/mcp/tools.ts`（391 行）

#### 2.4.1 常量与类型

```ts
export function toToolExposure(exposure: McpExposure): ToolExposure {
	return exposure === "codemode" ? "deferred" : exposure;
}
const MAX_TOOL_NAME_LENGTH = 64;
export const MCP_OUTPUT_MAX_BYTES = 20 * 1024;
const OUTPUT_PREVIEW_LINES = 5;
export { READ_MCP_RESOURCE_TOOL };

export interface McpToolDetails {
	server: string;
	tool: string;
	fullOutputPath?: string;
}
export type McpOutputSaver = (data: string | Uint8Array, extension: string) => Promise<string>;
export function saveToTempFile(data, extension): Promise<string>   // writeOutputFile("kiana-mcp", extension, data)
export interface McpToolCaller {
	callTool(name: string, args: Record<string, unknown>, options: McpRequestOptions): Promise<CallToolResult>;
}
```

#### 2.4.2 `createMcpToolName(server, tool, isTaken = () => false)`

```text
name = `mcp__${server}__${tool}` 中所有 /[^A-Za-z0-9_]/g → "_"
if (name.length <= 64 && !isTaken(name)) return name
hash = sha256(`${server}\0${tool}`).hex.slice(0, 8)
return `${name.slice(0, 64 - 8 - 1)}_${hash}`   // 55 字符 + "_" + 8 = 64
```

- 长度按 **UTF-16 code unit** 计（源串已全是 ASCII，等价于字节数）。
- `isTaken` 的语义见 §2.9.5：任何“属于别的工具 / 本批已用 / 本批中有重名（sanitize 后）”都算 taken。
- 哈希输入含 **NUL** 分隔符，因此 `(server="a", tool="b\0c")` 之类不会碰撞成别的组合。

#### 2.4.3 `textOf`

```ts
function textOf(content: readonly (TextContent | ImageContent)[]): string {
	return content.filter(b => b.type === "text").map(b => b.text).join("\n");
}
```

#### 2.4.4 `CONTENT_BLOCK_SCHEMA`（逐字）

```ts
const CONTENT_BLOCK_SCHEMA = {
	type: "object",
	properties: {
		type: { enum: ["text", "image", "audio", "resource_link", "resource"] },
		text: { type: "string" },
		data: { type: "string" },
		mimeType: { type: "string" },
	},
	required: ["type"],
	additionalProperties: true,
};
```

#### 2.4.5 `relocateSchema(schema, pointer)`

```ts
const SCHEMA_DATA_KEYWORDS = new Set(["const", "enum", "default", "examples"]);
const SCHEMA_MAP_KEYWORDS = new Set([
	"properties","patternProperties","dependentSchemas","dependencies","$defs","definitions",
]);

function relocateSchema(schema: unknown, pointer: string): unknown {
	if (Array.isArray(schema)) return schema.map(item => relocateSchema(item, pointer));
	if (typeof schema !== "object" || schema === null) return schema;
	const relocated: Record<string, unknown> = {};
	for (const [key, value] of Object.entries(schema)) {
		if (key === "$ref" && typeof value === "string" && (value === "#" || value.startsWith("#/")))
			relocated[key] = `${pointer}${value.slice(1)}`;
		else if (SCHEMA_DATA_KEYWORDS.has(key)) relocated[key] = value;
		else if (SCHEMA_MAP_KEYWORDS.has(key) && typeof value === "object" && value !== null && !Array.isArray(value))
			relocated[key] = Object.fromEntries(Object.entries(value).map(([n, s]) => [n, relocateSchema(s, pointer)]));
		else relocated[key] = relocateSchema(value, pointer);
	}
	return relocated;
}
```

行为要点：

- `"#"` → `pointer`（`#/properties/structuredContent`）；`"#/x"` → `pointer + "/x"`。
- **数据关键字**的值原样保留（不再递归）——所以 `default: { $ref: "#/stays" }` 不会被改写（测试 #10416 断言）。
- **映射关键字**的值是 `name -> schema` 映射：**键**不递归，**值**递归。
- 其它键一律递归（包括 `items`、`anyOf`、`allOf`、`oneOf`、`not`、`if/then/else`…）。
- 远程 `$ref`（`https://…`）与非 `#` 开头的 `$ref` 原样保留。
- **键顺序**保持原顺序（`Object.entries` + 新对象插入）。

#### 2.4.6 `createMcpResultSchema(structuredContentSchema)`

```ts
return {
	type: "object",
	properties: {
		content: { type: "array", items: CONTENT_BLOCK_SCHEMA },
		structuredContent: structuredContentSchema
			? relocateSchema(structuredContentSchema, "#/properties/structuredContent")
			: {},
		isError: { type: "boolean" },
	},
	required: ["content"],
	additionalProperties: true,
} as unknown as TSchema;
```

- 无输出 schema 时 `structuredContent: {}`——表示“任意 JSON 值”（测试 #10416 断言渲染为 `structuredContent?: unknown`）。
- `additionalProperties: true` 容纳新协议字段。

#### 2.4.7 `limitMcpContent(content, saveOutput = saveToTempFile)`

```text
combined = textOf(content)
truncation = truncateMiddle(combined, 20*1024)
if (!truncation.truncated) return { content }
try { fullOutputPath = await saveOutput(combined, ".txt");
      where = `[Full output: ${fullOutputPath} (read it with offset/limit)]`; }
catch (e) { where = `[Could not save the full output: ${msg(e)}]`; }
tokens = Math.ceil(truncation.totalBytes / 4)
text = `Warning: truncated output (original token count: ${tokens})\nTotal output lines: ${truncation.totalLines}\n\n${truncation.content}\n\n${where}`
return { content: [{type:"text", text}, ...content.filter(b => b.type === "image")],
         ...(fullOutputPath ? { fullOutputPath } : {}) }
```

依赖 `truncateMiddle`（`ca-tools` 单元）：

```ts
export function truncateMiddle(content: string, maxBytes: number): MiddleTruncationResult
// { content, truncated, removedChars, totalBytes, totalLines }
// 未截断：原样返回，truncated=false
// 截断：headEnd = floor(maxBytes/2)（向前回退到 UTF-8 字符边界）
//       tailStart = buf.length - (maxBytes - floor(maxBytes/2))（向后前进到字符边界）
//       content = head + `…${removedChars} chars truncated…` + tail
//       removedChars = 被删掉的 Unicode 码点数（Array.from(...).length）
// totalLines = splitLinesForCounting(content).length（末尾换行不计一行；空串为 0 行）
```

- **`saveOutput` 只在真正截断时调用**（未截断不落盘，测试断言 `saved` 长度为 1）。
- 截断后的 content 顺序：**一个 text 块 + 所有 image 块**（保持原相对顺序）。
- 文本块数量从 N 变为 1，`textOf` 因此丢失原来的块边界（原本就以 `\n` 连接，语义一致）。
- `fullOutputPath` 仅在保存成功时出现。

#### 2.4.8 `extensionOf(uri)` / `isTextMimeType(mimeType)`

```ts
function extensionOf(uri: string): string {
	const path = URL.canParse(uri) ? new URL(uri).pathname : uri;
	return /\.[A-Za-z0-9]{1,8}$/.exec(path)?.[0] ?? ".bin";
}

function isTextMimeType(mimeType: string | undefined): boolean {
	if (!mimeType) return false;
	const type = mimeType.split(";", 1)[0].trim().toLowerCase();
	return type.startsWith("text/") || type === "application/json" || type.endsWith("+json") || type.endsWith("+xml");
}
```

- `URL.canParse` 是 WHATWG 判定（Rust 用 `url::Url::parse().is_ok()`）。
- 扩展名正则取**最后一个**匹配（`\.[A-Za-z0-9]{1,8}$` 锚定末尾，故 1–8 个字母数字）。
- `mimeType.split(";", 1)[0]` = 取分号前的部分（`split` 的限长 1）。
- 注意 `application/json` 单独列出，`+json`/`+xml` 后缀覆盖 `application/ld+json` 等。

#### 2.4.9 `blockToContent(server, block, options)`

```text
if (block.type === "resource_link") {
	details = [block.mimeType, block.size === undefined ? undefined : formatSize(block.size)].filter(Boolean)
	read = options.readableResources ? `. Read it with read_mcp_resource (server "${server}")` : ""
	description = block.description ? `: ${block.description}` : ""
	text = `[Resource ${block.uri} "${block.title ?? block.name}"${details.length ? ` (${details.join(", ")})` : ""}${description}${read}]`
	return [{type:"text", text}]
}
if (block.type === "resource" && "blob" in block.resource && !block.resource.mimeType?.startsWith("image/")) {
	data = Buffer.from(blob, "base64")            // 宽松解码：非法字符跳过
	if (isTextMimeType(mimeType)) return [{type:"text", text: data.toString("utf8")}]
	kind = `${mimeType ?? "unknown type"}, ${formatSize(data.length)}`
	try { path = await saveOutput(data, extensionOf(uri)); return [`[Binary resource ${uri} (${kind}) saved to ${path}]`] }
	catch (e) { return [`[Binary resource ${uri} (${kind}) could not be saved: ${msg(e)}]`] }
}
return toLlmContent({ content: [block] })
```

- 资源链接**总是**变成一行文本（不读内容）。
- `formatSize`（`ca-tools`）：`<1024` → `"${bytes}B"`；`<1024*1024` → `${(bytes/1024).toFixed(1)}KB`；否则 `${(bytes/1048576).toFixed(1)}MB`。
  例：`2048` → `"2.0KB"`，`4` → `"4B"`。
- 嵌入的 **image** blob（`mimeType` 以 `image/` 开头）走 `toLlmContent` → `{type:"image", data, mimeType}`。
- 嵌入的 **text** 资源直接成为文本（`Buffer.toString("utf8")` 会替换非法字节为 U+FFFD）。
- 其它（audio、未知 type）走 `toLlmContent`（`telemetry-mcp` §11）：
  `audio` → `[audio ${mimeType} omitted]`；未知 type → `[unsupported MCP content ${type}]`。

#### 2.4.10 `toModelContent(server, blocks, options = {})`

`Promise.all(blocks.map(...)).flat()`——**并行**转换，结果保持输入顺序。

#### 2.4.11 `convertMcpResult(server, tool, result, options = {})`

```text
converted = result.content.length > 0 ? await toModelContent(server, result.content, options)
                                      : toLlmContent(result)   // 无 content 时用 structuredContent 的 JSON（2 空格缩进）
if (result.isError && textOf(converted) === "")
	converted.push({type:"text", text:`MCP tool ${server}/${tool} returned an error`})
{ content, fullOutputPath } = await limitMcpContent(converted, options.saveOutput)
{ _meta, ...scriptResult } = result
return {
	content,
	details: { server, tool, ...(fullOutputPath ? {fullOutputPath} : {}) },
	structuredContent: scriptResult,
	...(result.isError ? { isError: true } : {}),
}
```

**关键语义**（测试逐条断言）：

- `structuredContent` 是**整个 `CallToolResult` 去掉 `_meta`**（`{content, structuredContent?, isError?}`），
  脚本（codemode）拿到的就是这个对象；模型的 `outputSchema` 描述的是它。
- `isError` 结果也保留 `structuredContent`（脚本能读到 `isError: true`）。
- 无 content 但有 `structuredContent` → `toLlmContent` 给 `JSON.stringify(structuredContent, null, 2)`。
- 空 content + `isError` → 追加 `MCP tool {server}/{tool} returned an error`（无标点）。
- 只有 `textOf(converted) === ""` 时才追加该行（有空白文本就不加）。
- `details.fullOutputPath` 仅在截断并保存成功时出现。

#### 2.4.12 `toParameters(schema)`

```ts
return { ...schema, type: schema.type ?? "object",
         ...(schema.properties === undefined ? { properties: {} } : {}) };
```

保证对象 schema 一定有 `properties`（部分 provider 拒绝没有 `properties` 的对象 schema）。

#### 2.4.13 `toToolAnnotations(tool)`

```ts
const ANNOTATION_HINTS = ["readOnlyHint","destructiveHint","idempotentHint","openWorldHint"] as const;
// 仅复制 typeof === "boolean" 的项；结果为空对象时返回 undefined
```

#### 2.4.14 `createMcpToolDefinition(options)`

```ts
options: {
	server: string; tool: McpTool; name: string; exposure: McpExposure;
	namespace: ToolNamespace; timeoutMs: number;
	getClient: () => Promise<McpToolCaller>;
	readableResources?: () => boolean;
}
```

字段：

| 字段 | 值 |
|---|---|
| `name` | `options.name` |
| `label` | `${server}/${tool.name}` |
| `description` | `tool.description?.trim() || title || \`MCP tool ${tool.name} from server ${server}\``，其中 `title = tool.title ?? tool.annotations?.title` |
| `parameters` | `toParameters(tool.inputSchema)` |
| `outputSchema` | `createMcpResultSchema(tool.outputSchema)` |
| `exposure` | `toToolExposure(options.exposure)` |
| `namespace` | `options.namespace` |
| `annotations` | 有才设置 |
| `renderCall`/`renderResult` | `createMcpToolRenderers(label)` |

`execute(toolCallId, params, signal, onUpdate)`：

```text
client = await options.getClient()
result = await client.callTool(tool.name, (params ?? {}) as Record<string, unknown>, {
	signal, timeoutMs: options.timeoutMs,
	onProgress: (progress) => {
		total = progress.total === undefined ? "" : `/${progress.total}`
		text  = progress.message ?? `Progress ${progress.progress}${total}`
		onUpdate?.({ content: [{type:"text", text}], details: { server, tool: tool.name } });
	},
})
return convertMcpResult(server, tool.name, result, { readableResources: options.readableResources?.() });
```

**进度文案逐字**：`Progress {progress}{"/"+total}`，或服务器的 `progress.message` 原样。

#### 2.4.15 `createMcpToolRenderers(label)`

```ts
renderCall(args, theme, context) {
	const component = (context.lastComponent as Text | undefined) ?? new Text("", 0, 0);
	component.setText(formatToolCallWithArgs(label, args, theme, context.expanded));
	return component;
}
renderResult(result, options, theme, context) {
	const component = (context.lastComponent as Container | undefined) ?? new Container();
	component.clear();
	const output = getTextOutput(result, context.showImages).trim();
	if (!output) return component;                       // 空输出：整个组件为空（无 Spacer）
	const color = context.isError ? "error" : "toolOutput";
	const styled = replaceTabs(output).split("\n").map(line => theme.fg(color, line)).join("\n");
	component.addChild(new Spacer(1));
	if (options.expanded) component.addChild(new Text(styled, 0, 0));
	else {
		component.addChild(new VisualLinePreview({
			text: styled, maxVisualLines: 5, keep: "start",
			formatHint: (hidden) =>
				`${theme.fg("muted", `... (${hidden} more lines,`)} ${keyHint("app.tools.expand", "to expand")}${theme.fg("muted", ")")}`,
		}));
		const fullOutputPath = result.details?.fullOutputPath;
		if (fullOutputPath) component.addChild(new Text(theme.fg("muted", `Full output: ${fullOutputPath}`), 0, 0));
	}
	return component;
}
```

- `formatToolCallWithArgs`（`ca-tools`，逐字实现见下）：
  展开时每个参数一行 `"  {key}: {value}"`（字符串原样，其它 `JSON.stringify(v, null, 2)`；
  `replaceTabs` 后去掉 `\r`，内部换行改为 `"\n    "`），整体 `muted` 着色，前缀 `<bold toolTitle(label)>` + `\n`；
  折叠时 `k=JSON.stringify(v)` 以**单空格**连接，长度 > 100（`COLLAPSED_ARGS_CHARS`）时
  取前 97 字符 + `"..."`（三个 ASCII 点），前缀为 `<header> ` + muted(preview)。
  `args == null` 或空对象 → 只有 header。
- `VisualLinePreview` 按**视觉行**（换行后宽度 50 的软换行）截取前 5 行，再追加提示行。
- 提示行逐字：`... (N more lines,` + 空格 + 键提示 + 空格 + `to expand` + `)`；`muted` 着色分两段（`...` 段与 `)`）。
- `Full output: <path>` 只在**折叠**且 `details.fullOutputPath` 存在时出现。

---

### 2.5 `src/extensions/mcp/resources.ts`（339 行）

#### 2.5.1 接口与判定

```ts
export interface McpResourceServer {
	name: string;
	timeoutMs: number;
	resourcesPage(cursor: string | undefined, options: McpRequestOptions): Promise<ListResourcesResult>;
	resourceTemplatesPage(cursor: string | undefined, options: McpRequestOptions): Promise<ListResourceTemplatesResult>;
	allResources(options: McpRequestOptions): Promise<Resource[]>;
	allResourceTemplates(options: McpRequestOptions): Promise<ResourceTemplate[]>;
	readResource(uri: string, options: McpRequestOptions): Promise<ReadResourceResult>;
}

function isMcpAppResource(item: { uri?: string; uriTemplate?: string; mimeType?: string }): boolean {
	const uri = item.uri ?? item.uriTemplate ?? "";
	return uri.startsWith("ui://") || /;\s*profile\s*=\s*"?mcp-app"?/i.test(item.mimeType ?? "");
}

function listed<T extends { _meta?: unknown }>(server: string, item: T): Record<string, unknown> {
	const { _meta, icons: _icons, ...rest } = item as T & { icons?: unknown };
	return { server, ...rest };
}
```

- MCP App 资源（`ui://` 或 `profile=mcp-app`）**不列出**。
- 列出时删除 `_meta` 与 `icons`，并把 `server` **放在最前**（键顺序：`server` 然后其余原顺序）。

#### 2.5.2 参数/输出 schema（逐字）

```ts
const stringProperty = (description: string) => ({ type: "string", description });
const SERVER_FILTER = stringProperty("MCP server name. Omit to list every server with resources.");
const CURSOR = stringProperty("Opaque cursor from a previous call with the same server; omit for the first page.");
const LIST_PARAMETERS = {
	type: "object",
	properties: { server: SERVER_FILTER, cursor: CURSOR },
	additionalProperties: false,
};
const READ_PARAMETERS = {
	type: "object",
	properties: {
		server: stringProperty(
			"MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources.",
		),
		uri: stringProperty("Resource URI to read. Must be one of the URIs returned by list_mcp_resources."),
	},
	required: ["server", "uri"],
	additionalProperties: false,
};

const optionalString = { type: "string" };
const LISTING_ERRORS = {
	type: "array",
	description: "Servers that could not be listed",
	items: { type: "object",
		properties: { server: { type: "string" }, error: { type: "string" } },
		required: ["server", "error"] },
};
const LIST_OUTPUT_SCHEMA = {
	type: "object",
	properties: {
		server: optionalString,
		resources: {
			type: "array",
			items: { type: "object",
				properties: {
					server: { type: "string" }, uri: { type: "string" }, name: { type: "string" },
					title: optionalString, description: optionalString, mimeType: optionalString,
					size: { type: "number" },
				},
				required: ["server", "uri", "name"] },
		},
		nextCursor: optionalString,
		errors: LISTING_ERRORS,
	},
	required: ["resources"],
};
const LIST_TEMPLATES_OUTPUT_SCHEMA = { /* 同上，键为 resourceTemplates，items 含
	uriTemplate: { type: "string", description: "RFC 6570 URI template" }, required: ["server","uriTemplate","name"] */ };
const READ_OUTPUT_SCHEMA = {
	type: "object",
	properties: {
		server: { type: "string" },
		uri: { type: "string" },
		contents: { type: "array", items: { anyOf: [
			{ type: "object", properties: { uri: {type:"string"}, mimeType: optionalString, text: {type:"string"} },
			  required: ["uri","text"] },
			{ type: "object", properties: { uri: {type:"string"}, mimeType: optionalString,
			  blob: { type: "string", description: "base64" } }, required: ["uri","blob"] },
		] } },
	},
	required: ["server", "uri", "contents"],
};
```

`LIST_TEMPLATES_OUTPUT_SCHEMA` 的完整键顺序与 `LIST_OUTPUT_SCHEMA` 平行（`server`、`resourceTemplates`、`nextCursor`、`errors`；
`resourceTemplates` 的 item 属性为 `server`、`uriTemplate`、`name`、`title`、`description`、`mimeType`）。

#### 2.5.3 `stringArgument(params, key)`

```ts
const value = (params as Record<string, unknown> | undefined)?.[key];
if (value === undefined || value === null) return undefined;
if (typeof value !== "string") throw new Error(`${key} must be a string`);
return value.trim() || undefined;
```

- `null` 等价于缺省（严格 schema 下可选参数以 null 表示）。
- 空白字符串 → `undefined`。

#### 2.5.4 `jsonResult(tool, server, payload)`

```ts
const { content, fullOutputPath } = await limitMcpContent([{ type: "text", text: JSON.stringify(payload) }]);
return {
	content,
	details: { server: server ?? "", tool, ...(fullOutputPath ? { fullOutputPath } : {}) },
	structuredContent: payload,
};
```

JSON 为**紧凑**（无空格）。`details.server` 在无 server 参数时是空串。

#### 2.5.5 `createMcpResourceToolDefinitions({ exposure, servers })`

```ts
const readOnly: ToolAnnotations = { readOnlyHint: true };

findServer(name):
	servers = options.servers()
	server = servers.find(s => s.name === name)
	if (server) return server
	available = servers.map(s => s.name).join(", ")
	throw new Error(`MCP server "${name}" has no resources${available ? `. Servers with resources: ${available}` : ""}`)
```

`list(params, signal, key, page, all)`：

```text
serverName = stringArgument(params, "server")
cursor     = stringArgument(params, "cursor")
visible    = item => !isMcpAppResource(item)
if (serverName) {
	server = findServer(serverName)
	result = await page(server, cursor, { signal, timeoutMs: server.timeoutMs })
	return { server: server.name, [key]: result.items.filter(visible).map(i => listed(server.name, i)),
	         ...(result.nextCursor === undefined ? {} : { nextCursor: result.nextCursor }) }
}
if (cursor) throw new Error("cursor can only be used when a server is specified")
servers = [...options.servers()].sort((a, b) => a.name.localeCompare(b.name))
results = await Promise.allSettled(servers.map(s => all(s, { signal, timeoutMs: s.timeoutMs })))
items = []; errors = []
results.forEach((r, i) => {
	const name = servers[i].name
	if (r.status === "fulfilled") items.push(...r.value.filter(visible).map(item => listed(name, item)))
	else errors.push({ server: name, error: errorMessage(r.reason) })
})
return { [key]: items, ...(errors.length > 0 ? { errors } : {}) }
```

- 单服务器：**一页**，带 `nextCursor`（若服务器给了；即使空串也要保留——`=== undefined` 判断）。
- 多服务器：**所有页**，`Promise.allSettled`，错误进 `errors` 数组（每项 `{server, error}`），成功的项照常返回。
- 多服务器时结果**按服务器名 `localeCompare` 排序**后的顺序拼接。
- 多服务器时**不返回 `nextCursor`**、也不返回顶层 `server`。
- 有错误才出现 `errors` 键（空数组不出现）。

三个工具定义：

| 字段 | `list_mcp_resources` | `list_mcp_resource_templates` | `read_mcp_resource` |
|---|---|---|---|
| `name`/`label` | `list_mcp_resources` | `list_mcp_resource_templates` | `read_mcp_resource` |
| `description`（逐字） | `Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.` | `Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.` | `Read a specific resource from an MCP server given the server name and resource URI.` |
| `parameters` | `LIST_PARAMETERS` | `LIST_PARAMETERS` | `READ_PARAMETERS` |
| `outputSchema` | `LIST_OUTPUT_SCHEMA` | `LIST_TEMPLATES_OUTPUT_SCHEMA` | `READ_OUTPUT_SCHEMA` |
| `exposure` | `toToolExposure(options.exposure)` | 同 | 同 |
| `annotations` | `{ readOnlyHint: true }` | 同 | 同 |

`read_mcp_resource.execute`：

```text
serverName = stringArgument(params, "server"); uri = stringArgument(params, "uri")
if (!serverName) throw new Error("server must be provided")
if (!uri) throw new Error("uri must be provided")
server = findServer(serverName)
result = await server.readResource(uri, { signal, timeoutMs: server.timeoutMs })
blocks = result.contents.flatMap(contents => [
	...(result.contents.length > 1 ? [{ type: "text", text: `${contents.uri}:` }] : []),
	{ type: "resource", resource: contents },
])
converted = await toModelContent(server.name, blocks)
{ content, fullOutputPath } = await limitMcpContent(
	converted.length > 0 ? converted : [{ type: "text", text: `Resource ${uri} is empty.` }])
contents = result.contents.map(({ _meta, ...rest }) => rest)
return { content,
         details: { server: server.name, tool: READ_MCP_RESOURCE_TOOL, ...(fullOutputPath ? {fullOutputPath} : {}) },
         structuredContent: { server: server.name, uri, contents } }
```

- 多个 contents 时每个前面加一行 `${uri}:`（**带冒号**）。
- 空结果文案逐字：`Resource ${uri} is empty.`。
- `structuredContent` 的键顺序：`server`、`uri`、`contents`。
- **注意**：`read_mcp_resource` 调用 `toModelContent` 时**没有**传 `readableResources`（默认 `undefined` → false），
  但这里的块是 `resource` 类型，不涉及资源链接文案。

---

### 2.6 `src/extensions/mcp/runtime.ts`（454 行）

#### 2.6.1 常量与状态

```ts
const DEFAULT_TIMEOUT_SECONDS = 60;
const STDERR_TAIL_CHARS = 2_000;
const CONNECT_RETRY_DELAYS_MS = [250, 1_000];
type ServerState = "connecting" | "connected" | "disconnected" | "needs-auth" | "failed" | "closed";
export type McpTransportFactory = (entry, cwd, authProvider) => McpTransport;
```

#### 2.6.2 谓词

```ts
function isTransientError(error: unknown): boolean {
	if (error instanceof McpHttpError)
		return error.status === 408 || error.status === 429 || (error.status >= 500 && error.status !== 501);
	return error instanceof TypeError;
}

function signInRequiredMessage(entry: McpServerEntry): string {
	const provider = "url" in entry.config ? entry.config.auth?.provider : undefined;
	return `MCP server "${entry.name}" requires sign-in. Run ${provider ? `/login ${provider}` : "/mcp"} to sign in.`;
}

function usesOAuth(entry: McpServerEntry): boolean {
	const { config } = entry;
	if (!("url" in config) || config.auth) return false;
	return !Object.keys(config.headers ?? {}).some(h => h.toLowerCase() === "authorization");
}

function expandHome(value: string): string {
	if (value === "~") return homedir();
	if (value.startsWith("~/") || (process.platform === "win32" && value.startsWith("~\\")))
		return join(homedir(), value.slice(2));
	return value;
}
```

- `501 Not Implemented` **不**重试（服务器明确表示不支持）。
- `TypeError` 视为网络层失败（Node fetch 的 `fetch failed`）。
- OAuth 判定：HTTP + 无 `auth` + headers 里**大小写不敏感地**没有 `authorization`。

#### 2.6.3 `createDefaultTransport(entry, cwd, authProvider)`

```ts
if ("url" in config) {
	return new StreamableHttpTransport({
		url: config.url,
		headers: resolveHeadersOrThrow(config.headers, `MCP server "${name}"`),
		authProvider,
	});
}
env = {}
for (const [key, value] of Object.entries(config.env ?? {}))
	env[key] = resolveConfigValueOrThrow(value, `MCP server "${name}" env "${key}"`);
return new StdioTransport({
	command: expandHome(config.command),
	args: config.args?.map(expandHome),
	cwd: resolve(cwd, expandHome(config.cwd ?? ".")),
	env,
	stderr: "pipe",
});
```

- **stdio 的 cwd 默认是会话 cwd**（`resolve(cwd, ".")`）。
- `env` 值支持 `${NAME}` / `!command`（`resolve-config-value.ts`，属 `ca-tools` 单元）。
- `args` 的每一项都做 `~` 展开；`command` 也是。
- `stderr: "pipe"`——用于失败时抓取尾部 2000 字符。

#### 2.6.4 `withoutTemplates` / `listTemplates`

```ts
async function withoutTemplates<T>(list: () => Promise<T>, empty: T): Promise<T> {
	try { return await list(); }
	catch (error) {
		if (error instanceof McpError && error.code === JSON_RPC_ERROR_CODES.methodNotFound) return empty;
		throw error;
	}
}
function listTemplates(client, options = {}) {
	return withoutTemplates(() => client.listResourceTemplates(options), []);
}
```

只吞 `-32601`（method not found）；其它错误照抛。

#### 2.6.5 `class McpServerConnection implements McpToolCaller, McpResourceServer`

公共字段（**顺序即声明顺序**）：

```ts
readonly entry: McpServerEntry;
state: ServerState = "connecting";
error: string | undefined;
tools: McpTool[] = [];
hasResources = false;
instructions: string | undefined;
challenge: OAuthChallenge | undefined;
```

私有字段：`client?: McpClient`、`opening?: Promise<McpClient>`、`readonly shutdown = new AbortController()`、
`stderrTail?: string`、`cwd`、`createTransport`、`authProvider`、`onTools`、`onChange`、`log`。

**构造函数**：

```text
entry/cwd/createTransport/onTools/onChange/log 保存
url = this.oauthUrl
provider = "url" in config ? config.auth?.provider : undefined
authProvider =
	url  ? createMcpAuthProvider({ serverUrl: url, store: credentials.forServer(name, url),
	                               settings: () => this.oauthSettings(),
	                               onChallenge: c => { this.challenge = c } })
	: provider ? { token: async () => options.providerToken?.(provider), settled: async () => {} }
	: undefined
```

**访问器**：

| 成员 | 行为 |
|---|---|
| `get name()` | `entry.name` |
| `private get closed()` | `shutdown.signal.aborted` |
| `get timeoutMs()` | `(config.timeout ?? 60) * 1000` |
| `get oauthUrl()` | `usesOAuth(entry) && "url" in config ? config.url : undefined` |
| `oauthSettings()` | 无 `oauth` → `{}`；否则 `{clientId, clientSecret: 惰性 resolveConfigValueOrThrow(…, \`MCP server "${name}" oauth.clientSecret\`), callbackPort, callbackUrl, scope, clientName, clientRegistration, authServerMetadataUrl: new URL(...)}` |

`oauthSettings()` 的 `clientSecret` **每次调用都重新解析**（惰性，失败只影响刷新/登录，不影响连接建立）。

**`getClient()`**：

```text
if (closed) return Promise.reject(new Error(`MCP server "${name}" is shut down`));
if (client?.connectionState === "connected") return Promise.resolve(client);
opening ??= open().finally(() => { opening = undefined });
return opening;
```

**请求方法**（全部经 `withClient`）：

| 方法 | 调用 | readOnly |
|---|---|---|
| `callTool(name, args, options)` | `client.callTool(name, args, options)` | false |
| `readResource(uri, options)` | `client.readResource(uri, options)` | true |
| `resourcesPage(cursor, options)` | `client.listResourcesPage(cursor, options)` | true |
| `resourceTemplatesPage(cursor, options)` | `withoutTemplates(() => client.listResourceTemplatesPage(cursor, options), { resourceTemplates: [] })` | true |
| `allResources(options)` | `client.listResources(options)` | true |
| `allResourceTemplates(options)` | `listTemplates(client, options)` | true |

**`withClient(run, readOnly = false)`**：

```text
for (attempt = 1; ; attempt++) {
	client = await getClient()
	try { return await run(client) }
	catch (error) {
		if (readOnly && attempt === 1 && error instanceof McpHttpError && isTransientError(error)) {
			await sleep(250); continue;                       // CONNECT_RETRY_DELAYS_MS[0]
		}
		if (error instanceof McpSessionExpiredError && attempt === 1) {
			if (this.client === client) this.client = undefined;   // 不 close：其它 in-flight 调用会同样 404 重试
			continue;
		}
		if (!needsSignIn(error)) throw error;
		await dropClient(client);
		markNeedsAuth();
		throw new Error(signInRequiredMessage(entry));
	}
}
```

**`needsSignIn(error)`**：`error instanceof McpOAuthAuthorizationRequiredError ||
(authProvider !== undefined && error instanceof McpAuthRequiredError)`。

**`reconnect()`**：`await opening?.catch(()=>{})` → 若有 client 则 `dropClient` → `await getClient()`。

**`signOut()`**：`await opening?.catch(()=>{})` → 若有 client 则 `dropClient` → 若未 closed 则 `markNeedsAuth()`。

**`markNeedsAuth()`**：`state = "needs-auth"; error = undefined; changed()`。

**`dropClient(client)`**：`if (this.client === client) this.client = undefined; await client.close().catch(()=>{})`。

**`open()`**：

```text
state = "connecting"; changed();
retries = ("url" in config) ? [250, 1000] : []
for (attempt = 0; ; attempt++) {
	stderrTail = undefined
	try { return await connectOnce() }
	catch (error) {
		delay = retries[attempt]
		if (closed || delay === undefined || !isTransientError(error)) throw connectFailed(error)
		await sleep(delay, shutdown.signal).catch(()=>{})
		if (closed) throw connectFailed(error)
	}
}
```

- **stdio 不重试**（`retries = []`）。
- 重试等待可被 `close()` 打断。

**`connectOnce()`**：

```text
client = new McpClient({ name: "kiana", version: VERSION, requestTimeoutMs: timeoutMs,
                         roots: [{ uri: pathToFileURL(cwd).href, name: basename(cwd) }] })
if (log) client.onNotification("notifications/message", params => log.write(entry.name, params))
let closing: Promise<void> | undefined
closeClient = () => { closing ??= client.close().catch(()=>{}); return closing }
shutdown.signal.addEventListener("abort", closeClient, { once: true })
let transport
try {
	transport = createTransport(entry, cwd, authProvider)
	await client.connect(transport)
	client.onNotification("notifications/tools/list_changed", () => { void refreshTools(client) })
	stdio = transport instanceof StdioTransport ? transport : undefined
	client.onClose(() => handleClientClose(client, stdio))
	tools = client.serverCapabilities?.tools ? await client.listTools() : []
	if (closed) throw new Error("shut down while connecting")
	if (client.connectionState !== "connected") throw new Error("connection closed during setup")
	this.client = client; this.tools = tools
	hasResources = client.serverCapabilities?.resources !== undefined
	instructions = client.instructions?.trim() || undefined
	state = "connected"; error = undefined
	onTools(this); changed()
	return client
} catch (error) {
	await closeClient()
	if (transport instanceof StdioTransport) stderrTail = transport.stderr.trim().slice(-2000) || undefined
	throw error
} finally { shutdown.signal.removeEventListener("abort", closeClient) }
```

- **`tools/list` 只在服务器声明 `tools` capability 时调用**（否则 `tools = []`）。方法序列因此是
  `["initialize"]` 或 `["initialize","tools/list"]`（测试断言）。
- `hasResources` = `serverCapabilities.resources !== undefined`（**不**主动列资源）。
- `instructions` trim 后为空 → `undefined`。
- `roots` 的 `name` 是 `basename(cwd)`，`uri` 是 `file://` URL。

**`connectFailed(error)`**：

```text
if (needsSignIn(error) && !closed) { markNeedsAuth(); return new Error(signInRequiredMessage(entry)); }
state = closed ? "closed" : "failed"
error = stderrTail ? `${msg(error)}\n${stderrTail}` : msg(error)
changed()
return new Error(`MCP server "${name}" failed to connect: ${this.error}`)
```

**`handleClientClose(client, stdio)`**：

```text
if (this.client !== client || closed) return
this.client = undefined
state = "disconnected"
stderr = stdio?.stderr.trim().slice(-2000)
error = stderr ? `Connection closed\n${stderr}` : "Connection closed"
changed()
```

**`refreshTools(client)`**：

```text
try { tools = await client.listTools()
      if (this.client !== client || closed) return
      this.tools = tools; onTools(this) }
catch (error) { this.error = `Failed to refresh tools: ${msg(error)}` }
changed()
```

（`changed()` 在 catch 与 try 两条路径都执行一次。）

**`close()`**：

```text
shutdown.abort()
state = "closed"; changed()
client = this.client; this.client = undefined
await Promise.all([client?.close().catch(()=>{}), opening?.catch(()=>{})])
await authProvider?.settled()
```

`settled()` 等待进行中的刷新保存轮换后的 token（否则退出会丢 grant）。

---

### 2.7 `src/extensions/mcp/oauth.ts`（558 行）

#### 2.7.1 常量

```ts
const CALLBACK_HOST = "127.0.0.1";
const CALLBACK_PATH = "/callback";
const CLIENT_METADATA_BASE_URL = "https://kiana.dev/oauth";  // Rust: KIANA_API_BASE_URL/oauth；未配置时 CIMD 明确报不可用
const FALLBACK_REDIRECT_URL = `http://127.0.0.1/callback`;
const REFRESH_SKEW_MS = 30_000;
const OAUTH_REQUEST_TIMEOUT_MS = 15_000;
const REFRESH_LOCK_STALE_MS = 20_000;
const REFRESH_LOCK_WAIT_MS = 25_000;
const REFRESH_LOCK_RETRY_MS = 100;
```

`McpOAuthSettings`（**本文件导出**，定义就在 `oauth.ts` 顶部，见 §2.7.1 下方；`runtime.ts` 从 `./oauth.ts` 导入它，故 §2.6 不重复定义）、
`CallbackSettings { host, redirectHost, port, path, fixedRedirectUrl }`（**非导出**，仅本文件内部）。

```ts
export interface McpOAuthSettings {
	clientId?: string;
	clientSecret?: string;              // 已解析
	callbackPort?: number;
	callbackUrl?: string;
	scope?: string;                     // 空格分隔
	clientName?: string;                // 默认 APP_NAME
	clientRegistration?: "dcr" | "cimd";
	authServerMetadataUrl?: URL;        // 已是 URL 对象
}
```

#### 2.7.2 `callbackSettings(settings)`

```text
url = new URL(settings.callbackUrl ?? `http://${CALLBACK_HOST}${CALLBACK_PATH}`)
address = url.hostname.replace(/^\[|\]$/g, "")           // 去掉 IPv6 方括号
port = url.port ? Number(url.port) : settings.callbackPort
fixedRedirectUrl =
	url.port ? settings.callbackUrl                       // 配置里带端口 → 原样使用
	: port !== undefined ? (url.port = String(port), url.href)
	: undefined
return {
	host: address === "localhost" ? "127.0.0.1" : address, // localhost 监听 127.0.0.1
	redirectHost: address,
	port,
	path: url.pathname,
	fixedRedirectUrl,
}
```

#### 2.7.3 scope 合并与状态序列化

```ts
function mergeScopes(...scopes: (string | undefined)[]): string | undefined {
	const merged = [...new Set(scopes.flatMap(s => s?.split(/\s+/).filter(Boolean) ?? []))];
	return merged.length > 0 ? merged.join(" ") : undefined;
}
type StoredStates = Record<string, McpOAuthState>;
function parseStates(content) {
	if (!content?.trim()) return {};
	const parsed = JSON.parse(content);
	return (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) ? parsed : {};
}
function serializeStates(states) { return `${JSON.stringify(states, null, 2)}\n`; }
function storeKeys(name, serverUrl) {
	const legacyKey = String(new URL(serverUrl));
	return { key: `${mcpNamespace(name)}|${legacyKey}`, legacyKey };
}
```

- 存储键：`mcp__<name>|<规范化 URL>`；`String(new URL(url))` = WHATWG 序列化（`href`）。
- legacy 键仅 URL（旧版本写入）。

#### 2.7.4 `McpOAuthCredentialStore`

```ts
constructor(backend?: AuthStorageBackend, lockDir?: string) {
	this.backend = backend ?? new FileAuthStorageBackend(join(getAgentDir(), "mcp-auth.json"));
	this.lockDir = backend ? lockDir : getAgentDir();
}
```

`forServer(name, serverUrl): McpOAuthServerStore`：

| 方法 | 行为 |
|---|---|
| `load()` | 在 `backend.withLock` 内：解析 states；若 `states[key]` 存在或 `states[legacyKey]` 不存在 → `{result: states[key]}`；否则把 legacy 搬到 key（`states[key]=states[legacyKey]; delete states[legacyKey]`）并写回 |
| `save(state)` | `write(states => { states[key] = state })` |
| `withRefreshLock(fn)` | `this.withRefreshLock(key, fn)` |

`withRefreshLock(key, fn)`：

```text
if (!lockDir) return fn()
mkdirSync(lockDir, { recursive: true, mode: 0o700 })
hash = sha256(key).hex.slice(0, 16)
release = await lockfile.lock(join(lockDir, `mcp-auth-refresh-${hash}`), {
	realpath: false, stale: 20_000,
	retries: { retries: 250, factor: 1, minTimeout: 100, maxTimeout: 100 },
	onCompromised: () => {},
})
try { return await fn() } finally { await release().catch(()=>{}) }
```

`retries = REFRESH_LOCK_WAIT_MS / REFRESH_LOCK_RETRY_MS = 250`。

`tokens(name, serverUrl)`：**不加**锁写；`withLock` 只读，**不做 legacy 接管**，返回 `(states[key] ?? states[legacyKey])?.tokens`。

`remove(name, serverUrl)`：`withLock`；`stored = key in states ? key : legacyKey in states ? legacyKey : undefined`；
无则 `{result:false}`；有则删除并写回，返回 `true`。

`write(update)`：`withLock`，解析 → `update(states)` → 写回序列化结果。

#### 2.7.5 `timedFetch(fetch = globalThis.fetch)`

```ts
return (input, init) => {
	const timeout = AbortSignal.timeout(15_000);
	return fetch(input, { ...init, signal: init?.signal ? AbortSignal.any([init.signal, timeout]) : timeout });
};
```

#### 2.7.6 `callbackId(serverUrl)`

```ts
const url = new URL(serverUrl); url.hash = "";
return createHash("sha256").update(url.href).digest().subarray(0, 9).toString("base64url");
```

9 字节 → base64url 恰好 **12** 个字符（无填充）。

#### 2.7.7 `clientMetadataDocument(serverUrl, redirectUrl, metadata)`

```text
if (!metadata?.client_id_metadata_document_supported
    || !metadata.token_endpoint_auth_methods_supported?.includes("none"))
	throw new Error('The authorization server does not support Client ID Metadata Documents for public clients; remove oauth.clientRegistration "cimd"')
if (metadata.authorization_response_iss_parameter_supported)
	return { url: "https://kiana.dev/oauth/client.json", redirectUrl }
id = callbackId(serverUrl)
redirect = new URL(redirectUrl); redirect.pathname = `/callback/${id}`
return { url: `https://kiana.dev/oauth/${id}/client.json`, redirectUrl: redirect.href }
```

#### 2.7.8 `createProvider(...)`

```ts
new McpOAuthProvider({
	serverUrl, redirectUrl,
	clientMetadata: { client_name: settings.clientName ?? APP_NAME },
	clientMetadataDocument: settings.clientRegistration === "cimd"
		? metadata => clientMetadataDocument(serverUrl, redirectUrl, metadata) : undefined,
	clientId: settings.clientId, clientSecret: settings.clientSecret,
	store, onRedirect,
});
```

#### 2.7.9 `createMcpAuthProvider(options)`

```ts
interface McpAuthProvider extends AuthProvider { settled(): Promise<void>; }
```

**`refresh(staleToken, fetch = globalThis.fetch, challenge?)`**：

```text
refreshing ??= store.withRefreshLock(async () => {
	state = await store.load()
	if (state?.tokens?.access_token !== staleToken) return;          // 别人已刷新
	if (!state?.tokens?.refresh_token) throw new McpOAuthAuthorizationRequiredError()
	settings = options.settings()
	redirectUrl = callbackSettings(settings).fixedRedirectUrl
	           ?? registeredRedirectUrls(state.clientInformation)[0]
	           ?? FALLBACK_REDIRECT_URL
	provider = createProvider(serverUrl, store, settings, redirectUrl, () => {})
	result = await authorizeMcp(provider, {
		serverUrl, resourceMetadataUrl: challenge?.resourceMetadataUrl,
		authorizationServerMetadataUrl: settings.authServerMetadataUrl,
		scope: challenge?.scope, fetch: timedFetch(fetch),
	})
	if (result === "REDIRECT") throw new McpOAuthAuthorizationRequiredError()
}).finally(() => { refreshing = undefined })
return refreshing
```

**`token()`**：

```text
await refreshing?.catch(()=>{})
state = await store.load()
token = state?.tokens?.access_token
expired = state?.tokensExpireAt !== undefined && state.tokensExpireAt - 30_000 <= Date.now()
if (!expired || !state?.tokens?.refresh_token) return token
await refresh(token).catch(()=>{})                  // 失败则用旧 token，交给 401 决定
return (await store.load())?.tokens?.access_token
```

**`onUnauthorized(context)`**：

```text
challenge = parseWwwAuthenticate(context.response.headers.get("www-authenticate"))
options.onChallenge(challenge)
if (challenge.error === "insufficient_scope") throw new McpOAuthAuthorizationRequiredError()
await refresh(context.token, context.fetch, challenge)
```

**`settled()`**：`await refreshing?.catch(()=>{})`。

设计要点：

- **同进程内共享一次刷新**（`refreshing` promise）；**跨进程**由锁文件串行。
- 锁内先 `load()` 再比对 `access_token !== staleToken` → 若别人已换 token 则**直接返回**（避免二次使用已轮换的 refresh token）。
- 刷新**不可取消**（服务器可能已经轮换 refresh token，答复必须保存）。

#### 2.7.10 `McpSignInPrompt` / `McpSignInCancelledError`

```ts
export interface McpSignInPrompt {
	showAuthorizationUrl(url: URL): void;
	promptForRedirectUrl(signal: AbortSignal): Promise<string | undefined>;
}
export class McpSignInCancelledError extends Error {
	constructor() { super("Sign-in cancelled"); this.name = "McpSignInCancelledError"; }
}
```

#### 2.7.11 `responseFromRedirectUrl(input, state, redirectUrl)`

```text
try { url = new URL(input.trim()) }
catch { throw new Error("Expected the full redirect URL from the browser address bar") }
if (url.origin !== redirectUrl.origin || url.pathname !== redirectUrl.pathname)
	throw new Error("The redirect URL does not match this sign-in's redirect URI")
error = url.searchParams.get("error")
if (error) throw new Error(url.searchParams.get("error_description") ?? error)
if (url.searchParams.get("state") !== state) throw new Error("The redirect URL belongs to a different sign-in")
code = url.searchParams.get("code")
if (!code) throw new Error("The redirect URL does not contain an authorization code")
return { code, iss: url.searchParams.get("iss") ?? undefined }
```

（全部逐字。）

#### 2.7.12 `waitForAuthorizationResponse(callback, state, redirectUrl, prompt, signal)`

```text
controller = new AbortController()
promptSignal = signal ? AbortSignal.any([controller.signal, signal]) : controller.signal
fromBrowser = callback.waitForCallback(state, redirectUrl.pathname)
fromUser = prompt.promptForRedirectUrl(promptSignal).then(input => {
	if (!input?.trim()) throw new McpSignInCancelledError()
	return responseFromRedirectUrl(input, state, redirectUrl)
})
try { return await Promise.race([fromBrowser, fromUser]) }
finally { controller.abort(); fromBrowser.catch(()=>{}); fromUser.catch(()=>{}) }
```

#### 2.7.13 `listenForCallback(settings, extraPaths, port, required)`

```text
options = { host: settings.host, redirectHost: settings.redirectHost, path: settings.path, extraPaths,
            renderPage: page => page.ok
              ? oauthSuccessHtml("Signed in to the MCP server. You may now close this page.")
              : oauthErrorHtml(page.message, page.details) }
try { return await OAuthCallbackServer.listen({ ...options, port: port ?? 0 }) }
catch (error) {
	if (required || port === undefined) throw error
	return OAuthCallbackServer.listen(options)        // 让系统分配空闲端口
}
```

- 固定端口（`settings.port !== undefined`，或在 `signInMcpServer` 里从已注册 redirect URI 推出的端口）失败时**必须**抛错。
- 否则退到空闲端口。

#### 2.7.14 `signInMcpServer(options)`

```text
{ serverUrl, store, settings, challenge?, prompt, signal? }

if (signal?.aborted) throw new McpSignInCancelledError()
stored = await store.load()
stepUp = challenge?.error === "insufficient_scope"
callbackOptions = callbackSettings(settings)
registered = registeredRedirectUrls(stored?.clientInformation)[0]
preferredPort = callbackOptions.port ?? (registered ? Number(new URL(registered).port) || undefined : undefined)
cimd = settings.clientRegistration === "cimd"
callback = await listenForCallback(callbackOptions, cimd ? [`/callback/${callbackId(serverUrl)}`] : [],
                                   preferredPort, callbackOptions.port !== undefined)
redirectUrl = callbackOptions.fixedRedirectUrl ?? callback.redirectUrl
try {
	if (stored) {
		next = { ...stored }
		delete next.oauthState                                  // 每次登录都有新的 state
		keepClient = settings.clientId
			|| (cimd ? !stored.clientInformation
			         : registeredRedirectUrls(stored.clientInformation).includes(redirectUrl))
		if (!keepClient) { delete next.clientInformation; delete next.tokens; delete next.tokensExpireAt }
		await store.save(next)
	}
	authorizationUrl = undefined
	provider = createProvider(serverUrl, store, settings, redirectUrl, url => { authorizationUrl = url })
	flow = { serverUrl, fetch: timedFetch(), signal,
	         resourceMetadataUrl: challenge?.resourceMetadataUrl,
	         authorizationServerMetadataUrl: settings.authServerMetadataUrl,
	         scope: mergeScopes(settings.scope,
	                            stepUp ? stepUpScope(stored?.tokens?.scope, challenge?.scope) : challenge?.scope) }
	skipRefresh = stepUp
	if ((await authorizeMcp(provider, { ...flow, skipRefresh })) === "AUTHORIZED") return
	if (!authorizationUrl) throw new Error("OAuth flow did not produce an authorization URL")
	state = await provider.state()
	authorizationRedirectUrl = new URL(authorizationUrl.searchParams.get("redirect_uri") ?? redirectUrl)
	prompt.showAuthorizationUrl(authorizationUrl)
	{ code, iss } = await waitForAuthorizationResponse(callback, state, authorizationRedirectUrl, prompt, signal)
	await authorizeMcp(provider, { ...flow, authorizationCode: code, iss })
} catch (error) {
	if (signal?.aborted) throw new McpSignInCancelledError()      // 把取消原因统一成取消
	throw error
} finally { await callback.close() }
```

要点：

- **总是有新的 `state`**：删掉旧的 `oauthState`。
- `keepClient` 规则：显式 `clientId` 永远保留；CIMD 模式只有在**从未注册过**时才保留（CIMD 不落盘 client，
  所以有 `clientInformation` 说明是以前 DCR 注册的，需要替换）；否则要求已注册 redirect URI 包含本次 `redirectUrl`。
- 不保留时清掉 `clientInformation` / `tokens` / `tokensExpireAt`（重新注册 + 重新授权）。
- `insufficient_scope` → `skipRefresh = true` + scope 叠加 `stepUpScope(已授予, 挑战要求)`，
  再与配置 `settings.scope` 合并去重。
- `authorizationRedirectUrl` 取自 **authorization URL 的 `redirect_uri` 参数**（若没有则用 `redirectUrl`）——
  这允许 CIMD 的服务器专属路径。
- 无论成功失败，`finally` 都关闭回调服务器。

---

### 2.8 `src/extensions/mcp/ui.ts`（259 行）

#### 2.8.1 接口

```ts
export interface McpMenu {
	title: string;
	details?: string;
	error?: string;
	items: SelectItem[];
	empty?: string;
	selected?: string;
	confirmLabel: string;
	cancelLabel: string;
}
export interface McpUi {
	menu(build: () => McpMenu, subscribe?: (listener: () => void) => () => void): Promise<string | undefined>;
	status(title: string, message: string, onCancel?: () => void): void;
	redirectUrl(title: string, authorizationUrl: string, signal: AbortSignal): Promise<string | undefined>;
}
```

#### 2.8.2 `frame(theme, title, body, footer?)`

```ts
const container = new Container();
container.addChild(new DynamicBorder(text => theme.fg("accent", text)));
container.addChild(new Text(theme.fg("accent", theme.bold(title)), 1, 0));
for (const child of body) container.addChild(child);
if (footer) {
	container.addChild(new Spacer(1));
	container.addChild(new Text(theme.fg("dim", footer), 1, 0));
}
container.addChild(new DynamicBorder(text => theme.fg("accent", text)));
```

标题：`accent` + 粗体，左内边距 1。footer：`dim`，左内边距 1，前面一个空行。

#### 2.8.3 `McpManagerView`

- 常量 `MAX_VISIBLE_ITEMS = 12`。
- 构造时内容 = `frame(theme, "MCP servers", [new Text(theme.fg("muted", "Loading…"), 1, 1)])`
  （注意 padding 为 `(1,1)`：左右 1，上下 1）。
- `focused` getter/setter 转发到当前 `inputTarget`。
- `setContent(content, inputHandler?, inputTarget?)`：先关掉旧 target 的 focused，替换内容，再按当前 focused 打开，
  `tui.requestRender()`。

**`menu(build, subscribe?)`**：

```text
return new Promise(resolve => {
	let unsubscribe, settled = false
	finish(value) { if (settled) return; settled = true; unsubscribe?.(); resolve(value) }
	let selected
	render() {
		menu = build()
		wanted = selected ?? menu.selected
		body = []
		if (menu.details) body.push(new Text(theme.fg("muted", menu.details), 1, 0))
		if (menu.error)   body.push(new Text(theme.fg("error", menu.error), 1, 0))
		body.push(new Spacer(1))
		footer = `${keyHint("tui.select.confirm", menu.confirmLabel)} • ${keyHint("tui.select.cancel", menu.cancelLabel)}`
		if (menu.items.length === 0) {
			body.push(new Text(theme.fg("muted", menu.empty ?? "Nothing to show."), 1, 0))
			setContent(frame(theme, menu.title, body, keyHint("tui.select.cancel", menu.cancelLabel)),
			           data => { if (keybindings.matches(data, "tui.select.cancel")) finish(undefined) })
			return
		}
		list = new SelectList(menu.items, Math.min(menu.items.length, 12), getSelectListTheme())
		index = menu.items.findIndex(item => item.value === wanted)
		if (index !== -1) list.setSelectedIndex(index)
		selected = list.getSelectedItem()?.value
		list.onSelectionChange = item => { selected = item.value }
		list.onSelect = item => finish(item.value)
		list.onCancel = () => finish(undefined)
		body.push(list)
		setContent(frame(theme, menu.title, body, footer), data => list.handleInput(data))
	}
	render()
	unsubscribe = subscribe?.(() => { if (!settled) render() })
})
```

行为：

- 每次订阅回调都**重建整棵组件树**，但用 `selected` 记住选中项（`selected ?? menu.selected`）。
- 空列表时 footer **只有 cancel 提示**（没有 confirm），空格键/回车无动作，Esc 取消。
- 默认空文案 `"Nothing to show."`。
- 菜单项超过 12 个时 `SelectList` 内部滚动。

**`status(title, message, onCancel?)`**：

```text
body = [new Spacer(1), new Text(theme.fg("muted", message), 1, 0)]
if (!onCancel) { setContent(frame(theme, title, body)); return; }
setContent(frame(theme, title, body, keyHint("tui.select.cancel", "cancel")),
           data => { if (keybindings.matches(data, "tui.select.cancel")) onCancel(); })
```

- 可取消时 footer 逐字为 `<Esc键名>` + ` cancel`；不可取消时**没有 footer**（测试断言渲染不含 "cancel"）。

**`redirectUrl(title, authorizationUrl, signal)`**：

```text
if (signal.aborted) { finish(undefined); return; }
signal.addEventListener("abort", onAbort, { once: true })
input = new Input()
link = new AuthUrlComponent(tui, authorizationUrl)
body = [
	new Spacer(1),
	new Text(theme.fg("muted", "Approve access in your browser. If it did not open, visit:"), 1, 0),
	link,
	new Spacer(1),
	new Text(theme.fg("muted", "If the browser runs on another machine, paste the URL it was redirected to:"), 1, 0),
	input,
]
setContent(frame(theme, title, body,
	`${keyHint("tui.select.confirm", "submit")} • ${keyHint("tui.select.cancel", "cancel")}`),
	(data) => {
		if (keybindings.matches(data, "tui.select.confirm")) {
			value = input.getValue().trim(); if (value) finish(value); return;
		}
		if (keybindings.matches(data, "tui.select.cancel")) { finish(undefined); return; }
		if (keybindings.matches(data, "app.message.copy")) { void link.copy(); return; }
		input.handleInput(data)
	}, input)
```

- `signal` 触发（浏览器回调到达）→ resolve `undefined`，**不报错**。
- 空输入按确认键**不**结束（等待继续输入）。
- 复制键调用 `AuthUrlComponent.copy()`（OSC52 / 本地剪贴板，`tui-components` 单元）。

**`handleInput(data)`**：`this.inputHandler?.(data); this.tui.requestRender()`。

**`render(width)`**：渲染当前内容，逐行若 `visibleWidth(line) > width` 则 `truncateToWidth(line, width, "")`。
`invalidate()`：转发 `content.invalidate()`。

#### 2.8.4 `showMcpManager(ctx, manage)`

```ts
await ctx.ui.custom<void>((tui, theme, keybindings, done) => {
	const view = new McpManagerView(tui, theme, keybindings);
	void manage(view).then(
		() => done(),
		(error) => { ctx.ui.notify(error instanceof Error ? error.message : String(error), "error"); done(); },
	);
	return view;
});
```

`manage` 抛错 → 通知错误文本（`error` 级）并关闭视图。

---

### 2.9 `src/extensions/mcp/index.ts`（1347 行）

#### 2.9.1 选项与常量

```ts
export interface McpExtensionOptions {
	loadConfig?: (ctx: ExtensionContext) => LoadedMcpConfig;
	createTransport?: McpTransportFactory;
	credentials?: McpOAuthCredentialStore;
	logPath?: string;
	openUrl?: (url: string) => void;
	updateConfig?: (entry: McpServerEntry, patch: McpServerConfigPatch) => void;
	startupWaitMs?: number;
}
export type { McpTransportFactory } from "./runtime.ts";

const DEFAULT_STARTUP_WAIT_MS = 10_000;

const RESOURCE_TOOL_NAMES: ReadonlySet<string> = new Set([
	LIST_MCP_RESOURCES_TOOL, LIST_MCP_RESOURCE_TEMPLATES_TOOL, READ_MCP_RESOURCE_TOOL,
]);

/** Name of the system prompt section that lists the servers whose tools are not declared. */
export const MCP_SERVERS_SECTION = "mcp_servers";
const MAX_SERVER_DESCRIPTION_CHARS = 250;
export const MAX_SERVERS_SECTION_CHARS = 4096;

const EXPOSURE_DESCRIPTIONS: Record<Exclude<McpExposure, "hidden">, string> = {
	codemode: "called from codemode scripts, which find them with searchTools()",
	deferred: "not declared until tool_search loads them, then called directly; no codemode needed",
	direct: "declared to the model like built-in tools",
};

const MCP_USAGE = "Usage: /mcp, /mcp login [server], /mcp logout [server], /mcp reconnect [server]";
```

内部 `interface McpServer`：

```ts
interface McpServer {
	entry: McpServerEntry;
	connection?: McpServerConnection;
	registeredConfig?: string;   // 扩展注册的服务器：注册时的 JSON.stringify(config)
	message?: string;            // 最近一次 /mcp 动作的失败结果
	attempt?: symbol;            // 当前连接尝试的标识
	ready?: Promise<void>;       // 本次尝试结束（连接或失败）
	closing?: Promise<void>;     // 已分离连接的清理；替换连接前等待它
}
```

#### 2.9.2 小工具函数

```ts
function errorMessage(error) { return error instanceof Error ? error.message : String(error); }
function firstLine(text) { return text.split("\n", 1)[0] ?? ""; }
function isEnabled(server) { return server.entry.config.enabled !== false; }
function exposureOf(entry): McpExposure { return entry.config.exposure ?? "codemode"; }
function configuredExposures(entry): Set<McpExposure> {
	return new Set([exposureOf(entry), ...Object.values(entry.config.toolExposure ?? {})]);
}
function hasDirectTools(entry) { return configuredExposures(entry).has("direct"); }
function hasIndirectTools(entry) {
	const e = configuredExposures(entry); return e.has("codemode") || e.has("deferred");
}
```

#### 2.9.3 `serversSectionIntro(reaches)`

```ts
let intro = "MCP servers whose tools are not declared to you.";
if (reaches.has("codemode")) intro += " Call the tools of `codemode` servers from codemode scripts.";
if (reaches.has("tool_search")) intro += " Load the tools of `tool_search` servers with `tool_search`.";
return intro;
```

#### 2.9.4 `truncate(text, max)`

```ts
if (text.length <= max) return text;
return max <= 1 ? "" : `${text.slice(0, max - 1).trimEnd()}…`;
```

- 长度为 **UTF-16 code unit**（Rust 需按 UTF-16 索引）。
- `max <= 1` → 空串。
- 先截到 `max-1` 个单元，再 `trimEnd()`，再拼 `…`。总长可能小于 `max`。

#### 2.9.5 `serverSummary` / `renderServersSection`

```ts
export interface McpServerListing { entry: McpServerEntry; connection?: { instructions?: string } }

function serverSummary(server) {
	const text = server.entry.config.description?.trim() || server.connection?.instructions || "";
	return (text.split("\n", 1)[0] ?? "").trim();
}
```

`renderServersSection(servers)`（**导出**，测试直接调用）：

```text
listed = servers.filter(s => isEnabled(s) && hasIndirectTools(s.entry))
                .sort((a, b) => a.entry.name.localeCompare(b.entry.name))
if (listed.length === 0) return undefined

reaches = listed.map(s => configuredExposures(s.entry).has("codemode") ? "codemode" : "tool_search")
intro = serversSectionIntro(new Set(reaches))
heads = listed.map((s, i) => `- ${mcpNamespace(s.entry.name)} (${reaches[i]})`)

omitted(count) = count > 0
	? [`- … ${count} more server${count === 1 ? "" : "s"}; find their tools with searchTools()`]
	: []

size(kept) = [intro, ...heads.slice(0, kept), ...omitted(listed.length - kept)].join("\n").length
let kept = listed.length
while (kept > 0 && size(kept) > 4096) kept--

perServer = kept === 0 ? 0
	: Math.min(250, Math.floor((4096 - size(kept)) / kept) - 2)

lines = listed.slice(0, kept).map((server, index) => {
	const summary = perServer > 0 ? truncate(serverSummary(server), perServer) : "";
	return summary ? `${heads[index]}: ${summary}` : heads[index];
})
return [intro, ...lines, ...omitted(listed.length - kept)].join("\n")
```

逐字示例（测试断言）：

```
MCP servers whose tools are not declared to you. Call the tools of `codemode` servers from codemode scripts. Load the tools of `tool_search` servers with `tool_search`.
- mcp__docs (codemode): Docs search.
- mcp__later (tool_search)
- mcp__plain (codemode): From instructions.
```

（`direct` 服务器**不出现**；只有 `direct` 服务器时返回 `undefined`。）

细节：

- 排序用 `localeCompare`（**不是**字节序）。同名前缀 `server1`/`server10`/`server2` 的 ICU 排序与字典序不同。
- `kept` 从全部开始递减，直到 `size(kept) <= 4096`（`size` 含 intro、前 `kept` 行、以及与最终输出相同的 omission 行）。
- 描述预算 `perServer`：剩余字符均分给每个服务器，再减去 `": "` 的 2 个字符；上限 250。
  若算出 ≤ 0 则描述为空（只留标题行）。
- `size(kept)` 与最终输出的差别只在描述文本：所以最终长度 `<= 4096` 一定成立（测试断言）。
- omission 行单复数：`more server` / `more servers`。
- 该函数**每次** `before_agent_start` 都重新计算；结果相同则不改变 section（由 `ca-session` 决定是否追加）。
- **不含**任何 `direct`-only 服务器，也不含已禁用服务器（测试 `renderServersSection([server("direct",…,"direct")]) === undefined`）。

#### 2.9.6 `reachesServer(reach, server)`

```ts
if (reach === "all") return true;
const namespace = mcpNamespace(server);
return reach.some(word => word.includes(namespace));
```

“word 包含命名空间”即匹配（可能过度匹配，如 `mcp__docs_extra` 也包含 `mcp__docs`）。

#### 2.9.7 `describeState(server, withError = true)`

```text
if (!isEnabled(server)) return "disabled"
connection = server.connection
if (!connection) return "starting"
switch (connection.state) {
	"needs-auth": "needs sign-in"
	"failed": withError ? `failed: ${firstLine(connection.error ?? "unknown error")}` : "failed"
	"connected": `connected · ${tools.length} tool${tools.length === 1 ? "" : "s"}${hasResources ? " · resources" : ""}`
	"connecting": "connecting…"
	default: connection.state
}
```

逐字样例：`connected · 1 tool`、`connected · 3 tools · resources`、`failed: spawn x ENOENT`、`connecting…`（U+2026）。

#### 2.9.8 `attentionRank(server)`

```ts
if (!isEnabled(server)) return 5;
switch (server.connection?.state) {
	"needs-auth": 0
	"failed": 1
	"disconnected": 2
	"connected": 4
	default: 3           // connecting / undefined
}
```

#### 2.9.9 `describeTransport(entry)`

```ts
return "url" in config ? config.url : [config.command, ...(config.args ?? [])].join(" ");
```

#### 2.9.10 扩展工厂的内部状态

`createMcpExtension(options)` 返回 `(kiana: ExtensionAPI) => void`，闭包变量：

```ts
let servers: McpServer[] = [];
let configuredEntries: McpServerEntry[] = [];
let configErrors: string[] = [];
let projectConfig: string | undefined;
let overridden: string[] = [];
let autoEnableCodemode = true;
let warnedUnreachable = false;
let pending: Promise<unknown> | undefined;
let waitedForStartup = false;
const startupWaitMs = options.startupWaitMs ?? 10_000;
let sessionCwd = process.cwd();
let credentials = options.credentials;
let modelRegistry: ModelRegistry | undefined;
let serverLog: McpServerLog | undefined;
const openUrl = options.openUrl ?? openBrowser;
const updateConfig = options.updateConfig
	?? ((entry, patch) => updateMcpServerConfig(entry.override ?? entry.source, entry.name, patch,
	                                            { override: entry.override !== undefined }));
let session = new AbortController(); session.abort();     // 初始为已中止
const backgroundActions = new Set<Promise<void>>();
const listeners = new Set<() => void>();
```

`track(work)`：

```ts
const task = work.then(() => undefined, () => undefined);
backgroundActions.add(task);
void task.finally(() => backgroundActions.delete(task));
return work;      // 返回原始 promise（保留结果/错误）
```

`emitChange()`：按插入顺序调用每个 listener。`subscribe(listener)`：加入并返回 `() => listeners.delete(listener)`。

`connections()`：`servers.flatMap(s => s.connection ? [s.connection] : [])`。
`findServer(name)`：`servers.find(s => s.entry.name === name)`。

#### 2.9.11 `registeredServers()`

```ts
const registered: McpServer[] = [];
const overriddenNames: string[] = [];
for (const { name, config, extensionPath } of kiana.getMcpServers()) {
	const configured = configuredEntries.find(entry => mcpNamespace(entry.name) === mcpNamespace(name));
	if (configured) {
		overriddenNames.push(`"${name}" registered by ${extensionPath} is overridden by "${configured.name}" in ${configured.source}`);
		continue;
	}
	registered.push({ entry: { name, config, source: extensionPath, scope: "extension" },
	                  registeredConfig: JSON.stringify(config) });
}
return { servers: registered, overridden: overriddenNames };
```

逐字 override 文案：`"{name}" registered by {extensionPath} is overridden by "{configured.name}" in {configured.source}`。
判重按**命名空间**（`-`/`_` 等价）。

#### 2.9.12 惰性凭据与日志

```ts
const getCredentials = (runtime) => { credentials ??= new runtime.McpOAuthCredentialStore(); return credentials; };
const getServerLog  = (runtime) => { serverLog ??= new runtime.McpServerLog(options.logPath ?? join(getAgentDir(), "mcp.log")); return serverLog; };
```

#### 2.9.13 工具名所有权与注册

```ts
const toolOwners = new Map<string, string>();          // kiana 工具名 → "<server>\0<tool>"
const serverTools = new Map<string, Set<string>>();    // server → 当前提供的工具名
const definitions = new Map<string, ToolDefinition<TSchema, McpToolDetails>>();
```

**`registerToolRenderer`**（扩展初始化时注册一次）：

```ts
kiana.registerToolRenderer((toolName, next) => {
	const match = /^mcp__(.+?)__(.+)$/.exec(toolName);
	return next() ?? (match ? createMcpToolRenderers(`${match[1]}/${match[2]}`) : undefined);
});
```

用途：恢复的会话可以在服务器尚未连接时渲染对 MCP 工具的调用。正则**非贪婪**匹配中段。

**`registerTools(connection)`**：

```text
server = connection.entry.name
entry = findServer(server)?.entry ?? connection.entry
description = entry.config.description?.trim()
namespace = {
	name: mcpNamespace(server),
	aliases: [server],                                  // describeNamespace("docs") 找到 mcp__docs
	...(description ? { description } : {}),
	...(connection.instructions ? { instructions: connection.instructions } : {}),
}
previous = serverTools.get(server) ?? new Set()
current = new Set()
// 与 Codex 一致：所有 sanitize 后同名的工具都加哈希后缀，因此保留裸名的那个与列表顺序无关
plain = [...new Set(connection.tools.map(t => t.name))].map(tool => createMcpToolName(server, tool))

assignName(tool, owner):
	name = createMcpToolName(server, tool, candidate => {
		existing = toolOwners.get(candidate)
		return (existing !== undefined && existing !== owner) || current.has(candidate)
			|| plain.indexOf(candidate) !== plain.lastIndexOf(candidate)
	})
	toolOwners.set(name, owner); current.add(name); return name

for (const tool of connection.tools) {
	definition = createMcpToolDefinition({
		server, tool,
		name: assignName(tool.name, `${server}\0${tool.name}`),
		exposure: getMcpToolExposure(entry.config, tool.name),
		namespace,
		timeoutMs: connection.timeoutMs,
		getClient: async () => {
			const current = findServer(server);
			if (!current || !isEnabled(current)) throw new Error(`MCP server "${server}" is disabled.`);
			const client = current.connection;
			if (!client) throw new Error(`MCP server "${server}" is still starting.`);
			if (getMcpToolExposure(current.entry.config, tool.name) === "hidden"
				|| (client.state === "connected" && !client.tools.some(t => t.name === tool.name)))
				throw new Error(`MCP tool "${server}/${tool.name}" is no longer available.`);
			return client;
		},
		readableResources: () => resourceServers().some(s => s.name === server),
	})
	definitions.set(definition.name, definition)
	kiana.registerTool(definition)
}
serverTools.set(server, current)
for (const name of previous)
	if (!current.has(name) && definitions.get(name)) kiana.registerTool({ ...definitions.get(name), exposure: "hidden" })
syncResourceTools()
```

要点（全部逐字）：

- 三个执行期错误：`MCP server "{server}" is disabled.` / `MCP server "{server}" is still starting.` /
  `MCP tool "{server}/{tool}" is no longer available.`
- `getClient` 在**执行时**重新查找服务器（可能是替换后的新连接）。
- 撤下的工具不能注销，因此**以 hidden 重新注册同一份定义**；服务器重新提供时按配置的 exposure 再注册（覆盖前一次）。
- 命名冲突判定包含“本批 sanitize 结果重复”这一项 → 重复者**全部**加哈希（包括本可保留裸名的那个）。

**`hideTools(server)`**：

```ts
for (const name of serverTools.get(server) ?? []) {
	const definition = definitions.get(name);
	if (definition) kiana.registerTool({ ...definition, exposure: "hidden" });
}
serverTools.set(server, new Set());
syncResourceTools();
```

#### 2.9.14 资源工具同步

```ts
const serversWithResources = () => servers.filter(s =>
	s.connection?.hasResources && isEnabled(s) && exposureOf(s.entry) !== "hidden");
const resourceServers = () => serversWithResources().flatMap(s => s.connection ? [s.connection] : []);

let resourceToolsExposure: McpExposure | undefined;

const syncResourceTools = () => {
	const exposures = new Set(serversWithResources().map(s => exposureOf(s.entry)));
	const exposure = (["direct", "codemode", "deferred"] as const).find(c => exposures.has(c));
	const next = exposure ?? "hidden";
	if (next === resourceToolsExposure || (resourceToolsExposure === undefined && next === "hidden")) return;
	const wasDirect = resourceToolsExposure === "direct";
	resourceToolsExposure = next;
	const resourceDefinitions = createMcpResourceToolDefinitions({ exposure: next, servers: resourceServers });
	for (const definition of resourceDefinitions) kiana.registerTool(definition);
	if (wasDirect) {
		const names = new Set(resourceDefinitions.map(d => d.name));
		kiana.setActiveTools(kiana.getActiveTools().filter(name => !names.has(name)));
	}
};
```

- 宽度优先级：`direct` > `codemode` > `deferred` > `hidden`。
- 首次且无资源时不注册（`resourceToolsExposure === undefined && next === "hidden"` 直接返回）。
- 由 `direct` 变窄时，从 active 集合里移除三个资源工具（它们原本注册即激活）。
- 三个资源工具共用一个 `servers` 回调（调用时求值），因此服务器变化**无需**重注册也能反映。

#### 2.9.15 `ensureDiscoveryActive(ctx)`

```text
exposures = new Set()
for (const server of servers)
	if (isEnabled(server)) for (const exposure of configuredExposures(server.entry)) exposures.add(exposure)
needsCodemode    = exposures.has("codemode")
needsToolSearch  = exposures.has("deferred")
if (!needsCodemode && !needsToolSearch) return

tools = kiana.getAllTools()
hasReachingTool = (name) => tools.some(t => t.name === name && t.reachesTools)
hasCodemode    = hasReachingTool("codemode")
hasToolSearch  = hasReachingTool("tool_search")

active = kiana.getActiveTools()
activate = []
if (needsCodemode && hasCodemode && autoEnableCodemode && !active.includes("codemode")) activate.push("codemode")
if (needsToolSearch && hasToolSearch && !active.includes("tool_search")) activate.push("tool_search")
if (activate.length > 0) kiana.setActiveTools([...active, ...activate])

reachable = [...active, ...activate]
if (hasCodemode   && reachable.includes("codemode"))   return
if (hasToolSearch && reachable.includes("tool_search")) return
if (warnedUnreachable) return
warnedUnreachable = true
reason = (needsCodemode && hasCodemode && !autoEnableCodemode) ? " (autoEnableCodemode is false)" : ""
ctx.ui.notify(`MCP tools are only reachable from the codemode or tool_search tool, but neither is active${reason}; they cannot be called.`, "warning")
```

- 只看**配置**（连接前就能算出），因此 codemode 在服务器连接前就被激活。
- 只激活**声明了 `reachesTools`** 的 `codemode`/`tool_search` 工具（另一扩展的同名工具若无此字段则不算）。
- 警告每次会话**只发一次**（`warnedUnreachable`，`session_start` 时重置）。
- 逐字警告含条件后缀 ` (autoEnableCodemode is false)`。

#### 2.9.16 登录 token 观察

```ts
const tokensAtSignIn = new Map<McpServerConnection, string>();
const storedTokens = (connection) => {
	const url = connection.oauthUrl;
	return url && credentials ? JSON.stringify(credentials.tokens(connection.name, url) ?? null) : "null";
};
const onConnectionChange = (connection) => {
	if (connection.state !== "needs-auth") tokensAtSignIn.delete(connection);
	else if (!tokensAtSignIn.has(connection)) tokensAtSignIn.set(connection, storedTokens(connection));
	emitChange();
};
const reconnectSignedIn = async (ctx) => {
	const signedIn = [...tokensAtSignIn].filter(([c, tokens]) => storedTokens(c) !== tokens);
	if (signedIn.length === 0) return;
	for (const [c] of signedIn) tokensAtSignIn.delete(c);
	await Promise.allSettled(signedIn.map(([c]) => c.reconnect()));
	ensureDiscoveryActive(ctx);
};
```

用途：**另一个进程**（如 agent 跑 `kiana mcp login`）可能写入凭据；`turn_start` 时对比快照并自动重连。

#### 2.9.17 `createConnection` / `startConnection` / `waitForServers`

**`createConnection(server, isCurrent)`**（async）：

```text
runtime = await loadMcpRuntime()
if (!isCurrent()) return undefined
connection = new runtime.McpServerConnection({
	entry: server.entry,
	cwd: sessionCwd,
	createTransport: options.createTransport ?? runtime.createDefaultTransport,
	credentials: getCredentials(runtime),
	providerToken: async provider => modelRegistry?.getApiKeyForProvider(provider),
	log: getServerLog(runtime),
	onTools: c => { if (isCurrent()) registerTools(c) },
	onChange: c => { if (isCurrent()) onConnectionChange(c); else tokensAtSignIn.delete(c) },
})
server.connection = connection
emitChange()
return connection
```

**`startConnection(server, after?)`**：

```text
const signal = session.signal
const attempt = Symbol()
server.attempt = attempt
isCurrent = () => !signal.aborted && server.attempt === attempt && isEnabled(server) && servers.includes(server)
ready = (async () => {
	await after
	if (!isCurrent()) return
	const connection = await createConnection(server, isCurrent)
	if (!connection || !isCurrent()) return
	await connection.getClient().catch(() => undefined)
})()
server.ready = ready.catch(() => undefined)
return ready          // 加载 runtime 失败时 reject
```

**`waitForServers(waiting, signal)`**：

```text
for (;;) {
	ready = waiting.flatMap(s => isEnabled(s) && servers.includes(s) && s.ready ? [{server: s, ready: s.ready}] : [])
	if (ready.length === 0 || signal?.aborted) return
	await Promise.race([Promise.all(ready.map(r => r.ready)), abortPromise(signal)])
	if (signal?.aborted) return
	if (ready.every(r => !isEnabled(r.server) || !servers.includes(r.server) || r.ready === r.server.ready)) return
}
```

循环的意义：等待期间可能有**新的** `ready`（重连）替换掉旧的，这时要再等。

#### 2.9.18 `reportProblems(ctx, only?)`

```ts
const lines = only ? [] : configErrors.map(error => `config: ${error}`);
for (const server of only ?? servers) {
	const state = server.connection?.state;
	if (state === "needs-auth" || state === "failed") lines.push(`${server.entry.name}: ${describeState(server)}`);
}
if (lines.length === 0) return;
ctx.ui.notify(`MCP servers need attention:\n${lines.map(line => `  ${line}`).join("\n")}\nRun /mcp to fix.`, "warning");
```

逐字模板（每行两空格缩进）：

```
MCP servers need attention:
  config: <error>
  <name>: failed: <first line>
Run /mcp to fix.
```

#### 2.9.19 `saveConfig` / `signIn` / `signOut` / `reconnect` / `setEnabled` / `setExposure`

**`saveConfig(server, patch, inProject = false)`**：

```text
override = inProject ? projectConfig : server.entry.override
entry = override ? { ...server.entry, override } : server.entry
if (entry.scope !== "extension") {
	try { updateConfig(entry, patch) }
	catch (error) { return `Could not update ${entry.override ?? entry.source}: ${errorMessage(error)}` }
}
server.entry = { ...entry, config: { ...entry.config, ...patch } }
return undefined
```

- 扩展注册的服务器**不写文件**，只改内存。
- `inProject` 让全局服务器的改动落到**项目文件**（新增 override）。

**`signIn(server, prompt, cancel?)`**：

```text
connection = server.connection; url = connection?.oauthUrl
if (!connection || !url) return `MCP server "${server.entry.name}" does not use OAuth.`
runtime = await loadMcpRuntime()
signal = cancel ? AbortSignal.any([session.signal, cancel]) : session.signal
try { await track(runtime.signInMcpServer({ serverUrl: url,
		store: getCredentials(runtime).forServer(server.entry.name, url),
		settings: connection.oauthSettings(), challenge: connection.challenge, prompt, signal })) }
catch (error) {
	if (error instanceof runtime.McpSignInCancelledError) return "Sign-in cancelled."
	return `Sign-in failed: ${errorMessage(error)}`
}
connection.challenge = undefined
try { await connection.reconnect() } catch (error) { return `Signed in, but ${errorMessage(error)}` }
return undefined
```

逐字：`MCP server "{name}" does not use OAuth.`、`Sign-in cancelled.`、`Sign-in failed: {msg}`、`Signed in, but {msg}`。

**`signOut(server)`**：无连接或无 `oauthUrl` → `false`；否则 `credentials.remove(name, url)` 后 `connection.signOut()`，
返回 `removed`。

**`reconnect(server)`**：

```text
connection = server.connection
if (!connection) return `MCP server "${server.entry.name}" is disabled.`
previous = server.ready
ready = (async () => { await previous; await connection.reconnect() })()
server.ready = ready.catch(() => undefined)
try { await ready; return undefined } catch (error) { return errorMessage(error) }
```

**`setEnabled(server, enabled, inProject = false)`**：

```text
failed = saveConfig(server, { enabled }, inProject); if (failed) return failed
if (!enabled) {
	connection = server.connection
	server.attempt = undefined; server.connection = undefined
	if (connection) server.closing = Promise.all([connection.close(), server.ready]).then(() => undefined)
	closing = server.closing
	hideTools(server.entry.name)
	emitChange()
	try { await closing } finally { if (server.closing === closing) server.closing = undefined }
	return undefined
}
await startConnection(server, server.closing)
return undefined
```

**`setExposure(server, exposure)`**：

```text
failed = saveConfig(server, { exposure }); if (failed) return failed
if (server.connection?.state === "connected") registerTools(server.connection)
syncResourceTools()
indirect = new Set(kiana.getAllTools().filter(t => t.exposure !== "direct").map(t => t.name))
tools = serverTools.get(server.entry.name) ?? new Set()
kiana.setActiveTools(kiana.getActiveTools().filter(name => !tools.has(name) || !indirect.has(name)))
emitChange()
return undefined
```

目的：不再 `direct` 暴露的工具必须从 active 集合里移除（否则仍会被声明）。

#### 2.9.20 管理器：`notices` / `serversMenu` / `serverMenu`

```ts
const notices = () => [
	...configErrors.map(error => `config: ${error}`),
	...overridden.map(line => `overridden: ${line}`),
];
```

**`serversMenu()`**（逐字）：

```ts
{
	title: "MCP servers",
	error: notices().join("\n") || undefined,
	items: [...servers]
		.sort((a, b) => attentionRank(a) - attentionRank(b) || a.entry.name.localeCompare(b.entry.name))
		.map(server => ({
			value: server.entry.name,
			label: server.entry.name,
			description: `${describeState(server)} · ${exposureOf(server.entry)} · ${server.entry.override ? "global, project override" : (server.entry.scope ?? server.entry.source)}`,
		})),
	empty: `No MCP servers configured. Add them to ${resolve(getAgentDir(), "mcp.json")} or .kiana/mcp.json.`,
	confirmLabel: "manage",
	cancelLabel: "close",
}
```

描述的三段：状态、exposure、来源（override 时固定 `global, project override`；否则 `scope ?? source`）。

**`serverMenu(name)`**：

```ts
const server = findServer(name);
if (!server) return { title: name, items: [], empty: "This server is no longer configured.", confirmLabel: "", cancelLabel: "back" };

saved = entry.scope === "extension" ? "for this session"
	: entry.override ? "saved to the project mcp.json"
	: entry.scope ? `saved to the ${entry.scope} mcp.json`
	: "saved to mcp.json";
inProject = entry.scope === "global" && !entry.override && projectConfig !== undefined;
inProjectSaved = "saved to the project mcp.json";

items = []
if (!isEnabled(server)) {
	items.push({ value: "enable", label: "Enable", description: saved })
	if (inProject) items.push({ value: "enable-project", label: "Enable in this project", description: inProjectSaved })
} else {
	state = connection?.state
	if (state === "needs-auth") items.push({ value: "signin", label: "Sign in", description: "opens the browser" })
	if (state === "connected" && connection)
		items.push({ value: "tools", label: "Tools", description: `${connection.tools.length} offered` })
	if (state === "failed" || state === "disconnected" || state === "connected" || state === "needs-auth")
		items.push({ value: "reconnect", label: "Reconnect" })
	if (state === "connected" && connection?.oauthUrl)
		items.push({ value: "signout", label: "Sign out", description: "deletes the stored credentials" })
	items.push({ value: "exposure", label: "Exposure", description: exposureOf(entry) })
	items.push({ value: "disable", label: "Disable", description: saved })
	if (inProject) items.push({ value: "disable-project", label: "Disable in this project", description: inProjectSaved })
}

details = [
	describeTransport(entry),
	`${entry.scope ?? "config"}: ${entry.source}`,
	...(entry.override ? [`project override: ${entry.override}`] : []),
	`State: ${describeState(server, false)}`,
]
error = [server.message, connection?.state === "connected" ? undefined : connection?.error]
	.filter(line => line !== undefined).join("\n")
return {
	title: `MCP server ${name}`,
	details: details.join("\n"),
	error: error || undefined,
	items,
	selected: items[0]?.value,
	confirmLabel: "select",
	cancelLabel: "back",
}
```

- 已连接时**不**显示 `connection.error`（那通常是过期的）。
- `selected` 是**第一项**的 value（不是当前状态项）。

**`showTools(ui, server)`**：

```ts
exposure = exposureOf(server.entry)
overridden = Object.keys(server.entry.config.toolExposure ?? {}).length > 0
await ui.menu(() => ({
	title: `Tools of ${server.entry.name}`,
	details: `Exposure ${exposure}: ${exposure === "hidden" ? "unreachable" : EXPOSURE_DESCRIPTIONS[exposure]}${overridden ? "\nSome tools override it with toolExposure." : ""}`,
	items: (server.connection?.tools ?? []).map(tool => {
		toolExposure = getMcpToolExposure(server.entry.config, tool.name)
		description = firstLine(tool.description ?? "")
		return { value: tool.name, label: tool.name,
		         description: toolExposure === exposure ? description : `[${toolExposure}] ${description}` }
	}),
	empty: "The server offers no tools.",
	confirmLabel: "back",
	cancelLabel: "back",
}));
```

**`chooseExposure(ui, server)`**：

```ts
current = exposureOf(server.entry)
choice = await ui.menu(() => ({
	title: `Exposure of ${server.entry.name}`,
	details: server.entry.scope === "extension"
		? `Applies to this session; the server is registered by ${server.entry.source}.`
		: `Saved to ${server.entry.override ?? server.entry.source}.`,
	items: (Object.keys(EXPOSURE_DESCRIPTIONS) as ...).map(exposure => ({
		value: exposure,
		label: `${exposure === current ? "✓ " : "  "}${exposure}`,
		description: EXPOSURE_DESCRIPTIONS[exposure],
	})),
	selected: current,
	confirmLabel: "save",
	cancelLabel: "back",
}))
if (!choice || choice === current) return undefined
return setExposure(server, choice)
```

- 选项顺序固定为 `Object.keys(EXPOSURE_DESCRIPTIONS)` 的插入顺序：`codemode`、`deferred`、`direct`（**不含 hidden**）。
- 当前项前缀 `"✓ "`，其它 `"  "`（两个空格）。

**`signInWithUi(ui, server)`**：

```ts
title = `Sign in to ${server.entry.name}`
cancel = new AbortController()
status = (message) => ui.status(title, message, () => cancel.abort())
let authorizationUrl = ""
status("Contacting the authorization server…")
return signIn(server, {
	showAuthorizationUrl: url => { authorizationUrl = url.href; openUrl(url.href) },
	promptForRedirectUrl: async signal => {
		const value = await ui.redirectUrl(title, authorizationUrl, signal)
		status("Connecting…")
		return value
	},
}, cancel.signal)
```

逐字：`Contacting the authorization server…`、`Connecting…`（均 U+2026）。

**`runInBackground(ctx, server, operation)`**：

```ts
const ready = operation();
const attempt = server.attempt;
const signal = session.signal;
const finish = (message) => {
	if (signal.aborted || server.attempt !== attempt || !servers.includes(server)) return;
	server.message = message; ensureDiscoveryActive(ctx); emitChange();
};
void track(ready.then(finish, error => finish(errorMessage(error))));
```

**`runAction(ui, ctx, server, action)`**：

| action | 行为 |
|---|---|
| `"signin"` | `message = await signInWithUi(ui, server)` |
| `"reconnect"` | `runInBackground(ctx, server, async () => { await reconnect(server); return undefined })`（错误由状态呈现） |
| `"signout"` | `await signOut(server)` |
| `"tools"` | `await showTools(ui, server)` |
| `"exposure"` | `message = await chooseExposure(ui, server)` |
| `"enable"` / `"disable"` / `"enable-project"` / `"disable-project"` | `runInBackground(ctx, server, () => setEnabled(server, action.startsWith("enable"), action.endsWith("-project")))` |

之后：`if (signal.aborted) return; server.message = message; ensureDiscoveryActive(ctx); emitChange();`

**`manage(ui, ctx)`**：

```ts
for (;;) {
	const name = await ui.menu(serversMenu, subscribe);
	if (!name) return;
	for (;;) {
		const action = await ui.menu(() => serverMenu(name), subscribe);
		const server = findServer(name);
		if (!action || !server) break;
		await runAction(ui, ctx, server, action);
	}
}
```

即：服务器列表 → 服务器详情，详情返回 `undefined`（取消）回到列表；服务器消失也回列表。

#### 2.9.21 `formatStatus()`

```ts
if (servers.length === 0 && configErrors.length === 0 && overridden.length === 0)
	return `No MCP servers configured. Add them to ${resolve(getAgentDir(), "mcp.json")} or .kiana/mcp.json.`;

const lines = servers.map(server => {
	const { name } = server.entry;
	const exposure = exposureOf(server.entry);
	const connection = server.connection;
	if (connection?.state === "needs-auth") return `${name}: needs sign-in, run /mcp login ${name} (${exposure})`;
	const tools = connection?.state === "connected" ? `, ${connection.tools.length} tools` : "";
	const state = !isEnabled(server) ? "disabled"
		: connection?.state === "disconnected" ? "disconnected, reconnects on next call"
		: (connection?.state ?? "starting");
	const error = connection?.error && connection.state !== "connected"
		? `\n    ${connection.error.split("\n").join("\n    ")}` : "";
	return `${name}: ${state}${tools} (${exposure})${error}`;
});
for (const error of configErrors) lines.push(`config error: ${error}`);
for (const line of overridden) lines.push(`overridden: ${line}`);
return lines.join("\n");
```

逐字样例：

```
docs: connecting… (codemode)
later: disabled (deferred)
auth: needs sign-in, run /mcp login auth (direct)
bad: failed (codemode)
    spawn x ENOENT
config error: server "x" needs either "command" (stdio) or "url" (streamable HTTP)
```

注意 `needs-auth` 分支**不**带 tool 计数也不带 `error`；错误缩进固定 4 个空格。

#### 2.9.22 `pickServer` / `oauthPick` / `loginCommand`

```ts
const pickServer = async (name, ctx, options: { eligible, preferred, none }) => {
	if (name) {
		const server = findServer(name);
		if (!server) ctx.ui.notify(`No MCP server named "${name}".`, "error");
		else if (!options.eligible(server)) ctx.ui.notify(options.none, "error");
		return server && options.eligible(server) ? server : undefined;
	}
	const candidates = servers.filter(options.eligible);
	if (candidates.length === 0) { ctx.ui.notify(options.none, "info"); return undefined; }
	const preferred = candidates.filter(options.preferred);
	if (candidates.length === 1) return candidates[0];
	if (preferred.length === 1) return preferred[0];
	const choice = await ctx.ui.select("MCP server", candidates.map(s => s.entry.name));
	return candidates.find(s => s.entry.name === choice);
};

const usesOAuth = (server) => server.connection?.oauthUrl !== undefined;
const oauthPick = {
	eligible: usesOAuth,
	preferred: (server) => server.connection?.state === "needs-auth",
	none: "No enabled MCP server uses OAuth. Only HTTP servers without an Authorization header do.",
};
```

逐字：`No MCP server named "{name}".`、`No enabled MCP server uses OAuth. Only HTTP servers without an Authorization header do.`、
选择框标题 `MCP server`。

**`loginCommand(server, ctx)`**：

```ts
const { name } = server.entry;
if (!ctx.hasUI) { ctx.ui.notify(`Signing in to MCP server "${name}" requires interactive mode.`, "error"); return; }
const signal = session.signal;
let failure;
if (ctx.mode === "tui") {
	await showMcpManager(ctx, async (ui) => { failure = await signInWithUi(ui, server); });
} else {
	failure = await signIn(server, {
		showAuthorizationUrl: url => {
			ctx.ui.notify(`Sign in to MCP server "${name}" in your browser:\n${url.href}`, "info");
			openUrl(url.href);
		},
		promptForRedirectUrl: (signal) => ctx.ui.input(
			`Waiting for sign-in to "${name}". If the browser cannot reach this machine, paste the URL it was redirected to.`,
			"http://127.0.0.1:.../callback?code=...",
			{ signal }),
	});
}
if (signal.aborted) return;
if (failure) {
	ctx.ui.notify(failure, failure === "Sign-in cancelled." ? "info" : "error");
	return;
}
ensureDiscoveryActive(ctx);
ctx.ui.notify(`Signed in to MCP server "${name}" (${server.connection?.tools.length ?? 0} tools).`, "info");
```

逐字：`Signing in to MCP server "{name}" requires interactive mode.`、`Sign in to MCP server "{name}" in your browser:\n{url}`、
输入框提示与占位符 `http://127.0.0.1:.../callback?code=...`、`Signed in to MCP server "{name}" ({n} tools).`

#### 2.9.23 事件处理器

**`session_start`**：

```ts
const loaded = (options.loadConfig ?? defaultLoadConfig)(ctx);
configErrors = loaded.errors;
projectConfig = loaded.projectConfig;
autoEnableCodemode = loaded.autoEnableCodemode ?? true;
warnedUnreachable = false;
waitedForStartup = false;
sessionCwd = ctx.cwd;
modelRegistry = ctx.modelRegistry;
session = new AbortController();
const signal = session.signal;
configuredEntries = loaded.servers;
const registered = registeredServers();
overridden = registered.overridden;
servers = [...loaded.servers.map(entry => ({ entry })), ...registered.servers];
emitChange();
ensureDiscoveryActive(ctx);
const enabled = servers.filter(isEnabled);
if (enabled.length === 0) { reportProblems(ctx); return; }
const runtime = new Promise(resolve => setImmediate(resolve)).then(() => loadMcpRuntime());
pending = Promise.all(enabled.map(server => startConnection(server, runtime)))
	.then(() => { if (!signal.aborted) reportProblems(ctx); })
	.catch(error => { try { ctx.ui.notify(`MCP failed to load: ${errorMessage(error)}`, "error"); } catch {} });
```

- 服务器顺序：**配置文件中的先，注册的后**。
- `setImmediate` 让首帧渲染先发生，再加载 MCP runtime。
- `pending` 供 `/mcp` 的 `await pending` 使用。
- 加载失败逐字：`MCP failed to load: {msg}`。

**`waitForDirectServers(ctx)`**：

```ts
if (waitedForStartup) return;
waitedForStartup = true;
const ready = servers.flatMap(s => isEnabled(s) && hasDirectTools(s.entry) && s.ready ? [s.ready] : []);
if (ready.length === 0) return;
const finished = await Promise.race([
	Promise.all(ready).then(() => true),
	new Promise(resolve => { timer = setTimeout(() => resolve(false), startupWaitMs); }),
]);
clearTimeout(timer);
if (!finished) ctx.ui.notify("MCP servers are still connecting; their tools become available once connected.", "info");
```

逐字提示：`MCP servers are still connecting; their tools become available once connected.`
**每个会话只等一次**（`waitedForStartup`）。

**`before_agent_start`**：

```ts
await waitForDirectServers(ctx);
const { sections } = event.systemPromptOptions;
const section = renderServersSection(servers);
if (section) sections[MCP_SERVERS_SECTION] = section;
else delete sections[MCP_SERVERS_SECTION];
```

**`tool_call`**：

```ts
const tool = kiana.getAllTools().find(candidate => candidate.name === event.toolName);
if (!tool) return;
const readyServers = servers.filter(server => isEnabled(server) && server.ready);
if (readyServers.length === 0) return;
let waiting = [];
const reach = tool.reachesTools?.(event.input);
if (reach !== undefined) waiting = readyServers.filter(server => reachesServer(reach, server.entry.name));
else if (RESOURCE_TOOL_NAMES.has(tool.name)) waiting = readyServers;
else {
	const owner = toolOwners.get(event.toolName)?.split("\0", 1)[0];
	waiting = readyServers.filter(server => server.entry.name === owner);
}
await waitForServers(waiting, ctx.signal);
```

- `reachesTools` 存在（codemode / tool_search）→ 按词匹配。
- 资源工具 → 等**所有**就绪服务器。
- 普通 MCP 工具 → 等 `toolOwners` 里记录的那个服务器（`<server>\0<tool>` 取前半）。
- 其它工具（无 owner）→ 等待列表为空，立即返回。

**`turn_start`**：`if (tokensAtSignIn.size > 0) await reconnectSignedIn(ctx);`

**`mcp_servers_change`**：

```ts
const signal = session.signal;
if (signal.aborted) return;
const registered = registeredServers();
overridden = registered.overridden;
const next = new Map(registered.servers.map(s => [s.entry.name, s]));
const removed = servers.filter(s => s.entry.scope === "extension"
	&& next.get(s.entry.name)?.registeredConfig !== s.registeredConfig);
servers = servers.filter(s => !removed.includes(s));
for (const server of removed) hideTools(server.entry.name);
const added = registered.servers.filter(s => !findServer(s.entry.name));
servers.push(...added);
emitChange();
ensureDiscoveryActive(ctx);
await Promise.all(removed.map(s => s.connection?.close()));
const connecting = added.filter(isEnabled);
if (signal.aborted || connecting.length === 0) return;
try {
	await Promise.all(connecting.map(s => startConnection(s)));
	if (signal.aborted) { await Promise.all(connecting.map(s => s.connection?.close())); return; }
} catch (error) { ctx.ui.notify(`MCP failed to load: ${errorMessage(error)}`, "error"); return; }
if (signal.aborted) return;
reportProblems(ctx, connecting);
```

语义：**未注册的和配置变了的扩展服务器被丢弃**（后者随后作为新服务器重新加入）；新增的立即连接；
`reportProblems(ctx, connecting)` 只报这些新服务器的问题（不含 config 错误）。

**`session_shutdown`**：

```ts
session.abort();
const closing = connections();
servers = [];
emitChange();
await Promise.all([...backgroundActions, ...closing.map(c => c.close())]);
```

#### 2.9.24 `/mcp` 命令

```ts
kiana.registerCommand("mcp", {
	description: "Manage MCP servers: sign in, reconnect, enable or disable, and change exposure",
	getArgumentCompletions: (prefix) => {
		const [action, server, ...rest] = prefix.trimStart().split(/\s+/);
		if (rest.length > 0) return null;
		if (server === undefined) {
			return ["login", "logout", "reconnect"]
				.filter(item => item.startsWith(action ?? ""))
				.map(item => ({ value: `${item} `, label: item }));
		}
		if (action !== "login" && action !== "logout" && action !== "reconnect") return null;
		const items = servers
			.filter(candidate => action === "reconnect" ? candidate.connection !== undefined : usesOAuth(candidate))
			.filter(candidate => candidate.entry.name.startsWith(server))
			.map(candidate => ({ value: `${action} ${candidate.entry.name}`, label: candidate.entry.name,
			                     description: describeState(candidate) }));
		return items.length > 0 ? items : null;
	},
	handler: async (args, ctx) => {
		const [action, name, ...extra] = args.trim().split(/\s+/).filter(Boolean);
		if (action === undefined) {
			if (ctx.mode === "tui") await showMcpManager(ctx, ui => manage(ui, ctx));
			else { await pending; ctx.ui.notify(formatStatus(), "info"); }
			return;
		}
		if (extra.length > 0) { ctx.ui.notify(MCP_USAGE, "warning"); return; }
		await pending;
		switch (action) {
			case "login": {
				const server = await pickServer(name, ctx, oauthPick);
				if (server) await loginCommand(server, ctx);
				return;
			}
			case "logout": {
				const server = await pickServer(name, ctx, oauthPick);
				if (!server) return;
				const removed = await signOut(server);
				ctx.ui.notify(removed
					? `Signed out of MCP server "${server.entry.name}".`
					: `No stored credentials for MCP server "${server.entry.name}".`, "info");
				return;
			}
			case "reconnect": {
				const server = await pickServer(name, ctx, {
					eligible: candidate => candidate.connection !== undefined,
					preferred: candidate => candidate.connection?.state === "failed"
						|| candidate.connection?.state === "disconnected",
					none: "No enabled MCP server to reconnect.",
				});
				if (!server) return;
				const failure = await reconnect(server);
				if (failure) ctx.ui.notify(failure, "error");
				else { ensureDiscoveryActive(ctx);
				       ctx.ui.notify(`Reconnected to MCP server "${server.entry.name}" (${describeState(server)}).`, "info"); }
				return;
			}
			default: ctx.ui.notify(MCP_USAGE, "warning");
		}
	},
});
```

逐字：`Manage MCP servers: sign in, reconnect, enable or disable, and change exposure`（命令描述）、
`No enabled MCP server to reconnect.`、`Signed out of MCP server "{name}".`、
`No stored credentials for MCP server "{name}".`、`Reconnected to MCP server "{name}" ({state}).`、
`MCP_USAGE`（见 §2.9.1）。

`defaultLoadConfig(ctx)`：

```ts
return loadMcpConfig({ agentDir: getAgentDir(), cwd: ctx.cwd, projectTrusted: ctx.isProjectTrusted() });
```

文件末尾：`export default createMcpExtension();`

---

### 2.10 `src/extensions/mcp/cli.ts`（594 行）

#### 2.10.1 `HELP`（逐字，含 chalk 着色）

模板（`chalk.bold("Usage:")` 与全文件同色；`chalk.dim` 用于 `HELP_HINT`）：

```
Usage:
  kiana mcp add <server> [options] -- <command> [args...]
  kiana mcp add <server> [options] --url <url>
  kiana mcp remove <server> [-l]
  kiana mcp list [--json]
  kiana mcp login <server> [--timeout <seconds>]
  kiana mcp logout <server>

Configure and check MCP servers and sign in to OAuth servers without starting a session.
Reads ~/.kiana/agent/mcp.json and, in trusted projects, .kiana/mcp.json.

Commands:
  add <server>            Add or replace a server in mcp.json
  remove <server>         Remove a server from mcp.json
  list                    Show state, tools, and errors (exits 1 on failure)
  login <server>          Sign in through the browser
  logout <server>         Delete the stored OAuth credentials

Options for add and remove:
  -l, --local             Use .kiana/mcp.json in the current project instead of the global file

Options for add:
  --url <url>             Streamable HTTP server URL (instead of a command)
  --env <KEY=VALUE>       Environment variable for a stdio server (repeatable)
  --cwd <dir>             Working directory for a stdio server
  --header <KEY=VALUE>    HTTP header (repeatable)
  --bearer-token-env-var <NAME>
                          Send "Authorization: Bearer ${NAME}"
  --oauth-client-id <id>  Pre-registered OAuth client id
  --oauth-client-secret <secret>
                          OAuth client secret (may be ${NAME} or !command)
  --oauth-callback-port <port>
                          Fixed OAuth callback port
  --oauth-client-name <name>
                          Client name sent when registering with the OAuth server
  --exposure <mode>       codemode (default), deferred, direct, or hidden
  --description <text>    What the server offers, shown in the system prompt

Other options:
  --json                  Print the list as JSON
  --timeout <seconds>     Time limit for the whole sign-in (default: 300)
```

注意 `${CONFIG_DIR_NAME}` 展开为 `.kiana`（`Reads ~/.kiana/agent/mcp.json`、`Use .kiana/mcp.json`），
`${NAME}` 在 `--bearer-token-env-var` 与 `--oauth-client-secret` 说明里是**字面量**（模板里写作 `\${NAME}`）。

`HELP_HINT` = `` chalk.dim(`Use "${APP_NAME} mcp --help" for usage.`) `` → 去色后 `Use "kiana mcp --help" for usage.`

`DEFAULT_LOGIN_TIMEOUT_SECONDS = 300`。

#### 2.10.2 `McpCommandOptions` / `ServerReport`

```ts
export interface McpCommandOptions {
	cwd: string; agentDir: string;
	credentials?: McpOAuthCredentialStore;
	openUrl?: (url: string) => void;
	log?: (line: string) => void;
	error?: (line: string) => void;
}
interface ServerReport {
	name: string; scope: string; source: string; override?: string;
	enabled: boolean; exposure: string; transport: string; state: string; tools: string[];
	toolExposure?: Record<string, string>;
	resources?: true;
	error?: string;
}
```

`ServerReport` 键顺序即 JSON 输出顺序：`name, scope, source, override?, enabled, exposure, transport, state, tools,
toolExposure?, resources?, error?`。

#### 2.10.3 `parseOptions(args, known, error, maxPositionals = Infinity)`

```text
positional = []; values = new Map(); lists = new Map()
for (index = 0; index < args.length; index++) {
	arg = OPTION_ALIASES.get(args[index]) ?? args[index]          // "-l" → "--local"
	if (arg === "--" || positional.length >= maxPositionals) {
		positional.push(...args.slice(arg === "--" ? index + 1 : index)); break;
	}
	if (!arg.startsWith("--")) { positional.push(arg); continue; }
	name = arg.slice(2); kind = known[name]
	if (!kind) { error(`Unknown option ${arg}.\n${HELP_HINT}`); return undefined; }
	if (kind === "flag") { values.set(name, true); continue; }
	value = args[++index]
	if (value === undefined) { error(`${arg} needs a value.`); return undefined; }
	if (kind === "list") lists.set(name, [...(lists.get(name) ?? []), value]);
	else values.set(name, value);
}
return { positional, values, lists }
```

- `OPTION_ALIASES = new Map([["-l", "--local"]])`。
- `maxPositionals` 达到后**其余全部**作为位置参数（含以 `--` 开头的）——这样 `kiana mcp add name cmd --flag`
  能把 `--flag` 交给子命令（测试断言）。
- 未知选项错误：`Unknown option {arg}.\n{HELP_HINT}`（`arg` 是**别名替换后**的写法，即 `--local` 不会出现）。
- 缺值错误：`{arg} needs a value.`（无 HELP_HINT）。

#### 2.10.4 `runMcpCommand(args, options): Promise<number>`

```ts
const log = options.log ?? (line => console.log(line));
const error = options.error ?? (line => console.error(line));
const [command, ...rest] = args;
if (command === undefined || command === "help" || args.includes("--help") || args.includes("-h")) { log(HELP); return 0; }

const projectConfig = join(options.cwd, CONFIG_DIR_NAME, "mcp.json");
if (command === "add" || command === "remove")
	return command === "add" ? add(rest, projectConfig, options, log, error)
	                         : remove(rest, projectConfig, options, log, error);

const projectTrusted = new ProjectTrustStore(options.agentDir).get(options.cwd) === true;
const loaded = loadMcpConfig({ agentDir: options.agentDir, cwd: options.cwd, projectTrusted });
const untrustedNote = !projectTrusted && existsSync(projectConfig)
	? `${projectConfig} is ignored because the project is not trusted. Start ${APP_NAME} in the project to trust it.`
	: undefined;
const credentials = options.credentials ?? new McpOAuthCredentialStore();

switch (command) {
	case "list": {
		const parsed = parseOptions(rest, { json: "flag" }, error);
		if (!parsed) return 1;
		if (parsed.positional.length > 0) { error(`Usage: kiana mcp list [--json]\n${HELP_HINT}`); return 1; }
		return list(loaded, parsed.values.has("json"), untrustedNote, options, credentials, log);
	}
	case "login": case "logout": {
		const parsed = parseOptions(rest, command === "login" ? { timeout: "value" } : {}, error);
		if (!parsed) return 1;
		const [name, ...extra] = parsed.positional;
		if (!name || extra.length > 0) { error(`Usage: kiana mcp ${command} <server>\n${HELP_HINT}`); return 1; }
		const entry = loaded.servers.find(s => s.name === name);
		if (!entry) {
			error(`No MCP server named "${name}".${untrustedNote ? ` ${untrustedNote}` : ""} Configured: ${loaded.servers.map(s => s.name).join(", ") || "none"}.`);
			return 1;
		}
		const connection = createConnection(entry, options, credentials);
		const url = connection.oauthUrl;
		if (!url) {
			error(`MCP server "${name}" does not use OAuth. Only HTTP servers without an Authorization header do.`);
			return 1;
		}
		if (command === "logout") {
			const removed = credentials.remove(name, url);
			log(removed ? `Signed out of MCP server "${name}".` : `No stored credentials for MCP server "${name}".`);
			return 0;
		}
		const timeout = Number(parsed.values.get("timeout") ?? 300);
		if (!Number.isFinite(timeout) || timeout <= 0) { error("--timeout must be a positive number of seconds."); return 1; }
		try { return await login(entry, connection, url, timeout * 1000, options, credentials, log, error) }
		finally { await connection.close(); }
	}
	default:
		error(`Unknown mcp command "${command}".\n${HELP_HINT}`);
		return 1;
}
```

逐字错误/提示：

| 场景 | 文案 |
|---|---|
| 未知子命令 | `Unknown mcp command "{command}".\n{HELP_HINT}` |
| `list` 多余位置参数 | `Usage: kiana mcp list [--json]\n{HELP_HINT}` |
| `login/logout` 参数个数 | `Usage: kiana mcp {command} <server>\n{HELP_HINT}` |
| 服务器不存在 | `No MCP server named "{name}."[ {untrustedNote}] Configured: {names 逗号空格连接 或 "none"}.` |
| 非 OAuth | `MCP server "{name}" does not use OAuth. Only HTTP servers without an Authorization header do.` |
| 非法 timeout | `--timeout must be a positive number of seconds.` |
| 未信任项目提示 | `{projectConfig} is ignored because the project is not trusted. Start kiana in the project to trust it.` |

`createConnection(entry, options, credentials)`：

```ts
new McpServerConnection({ entry, cwd: options.cwd, createTransport: createDefaultTransport, credentials,
	log: new McpServerLog(join(options.agentDir, "mcp.log")), onTools: () => {} })
```

#### 2.10.5 `parsePairs(option, pairs, error)`

```text
if (!pairs) return {}
record = {}
for (const pair of pairs) {
	separator = pair.indexOf("=")
	if (separator <= 0) { error(`--${option} expects KEY=VALUE, got "${pair}".`); return undefined }
	record[pair.slice(0, separator)] = pair.slice(separator + 1)
}
return record
```

- `separator <= 0` 同时拒绝“无 `=`”和“以 `=` 开头”。
- 值可含 `=`（只按**第一个**等号切分，测试 `B=x=y`）。

#### 2.10.6 `add(args, projectConfig, options, log, error)`

usage 逐字：`Usage: kiana mcp add <server> [options] (--url <url> | -- <command> [args...])\n{HELP_HINT}`

已知选项（`parseOptions` 第三参）：

```ts
{ local: "flag", url: "value", env: "list", cwd: "value", header: "list",
  "bearer-token-env-var": "value", "oauth-client-id": "value", "oauth-client-secret": "value",
  "oauth-callback-port": "value", "oauth-client-name": "value", exposure: "value", description: "value" }
```

`maxPositionals = 2`。

```text
[name, ...command] = positional
url = values.get("url")
if (!name || (url === undefined) === (command.length === 0)) { error(usage); return 1 }

httpOnly = ["header","bearer-token-env-var","oauth-client-id","oauth-client-secret","oauth-callback-port","oauth-client-name"]
stdioOnly = ["env","cwd"]
misplaced = (url === undefined ? httpOnly : stdioOnly).find(o => values.has(o) || lists.has(o))
if (misplaced) { error(`--${misplaced} only applies to ${url === undefined ? "HTTP servers (--url)" : "stdio servers"}.`); return 1 }

if (typeof url === "string") {
	headers = parsePairs("header", lists.get("header"), error); if (!headers) return 1
	bearer = value("bearer-token-env-var")
	if (bearer !== undefined) headers.Authorization = `Bearer \${${bearer}}`
	port = value("oauth-callback-port")
	oauth = {
		...(clientId === undefined ? {} : { clientId }),
		...(clientSecret === undefined ? {} : { clientSecret }),
		...(port === undefined ? {} : { callbackPort: Number(port) }),
		...(clientName === undefined ? {} : { clientName }),
	}
	config = { url, ...(Object.keys(headers).length > 0 ? { headers } : {}), ...(Object.keys(oauth).length > 0 ? { oauth } : {}) }
} else {
	env = parsePairs("env", lists.get("env"), error); if (!env) return 1
	[executable, ...commandArgs] = command
	config = { command: executable, ...(commandArgs.length > 0 ? { args: commandArgs } : {}),
	           ...(Object.keys(env).length > 0 ? { env } : {}), ...(cwd === undefined ? {} : { cwd }) }
}
if (exposure !== undefined) config.exposure = exposure
if (description !== undefined) config.description = description
validated = validateMcpServerConfig(name, config)
if (typeof validated === "string") { error(validated); return 1 }

project = values.has("local")
path = project ? projectConfig : join(options.agentDir, "mcp.json")
scope = project ? "project" : "global"
try { replaced = addMcpServerConfig(path, name, validated) }
catch (e) { error(`Could not update ${path}: ${errorMessage(e)}`); return 1 }
log(`${replaced ? "Replaced" : "Added"} ${scope} MCP server "${name}" in ${path}.`)
if (project && new ProjectTrustStore(options.agentDir).get(options.cwd) !== true)
	log(`The project is not trusted, so ${path} is ignored until you start ${APP_NAME} in the project and trust it.`)
mayNeedSignIn = "url" in validated
	&& !Object.keys(validated.headers ?? {}).some(h => h.toLowerCase() === "authorization")
log(`Check it with: ${APP_NAME} mcp list${mayNeedSignIn ? `. If it requires sign-in: ${APP_NAME} mcp login ${name}` : ""}`)
return 0
```

逐字错误/输出：

| 场景 | 文案 |
|---|---|
| 用法 | （见上） |
| 选项放错位置 | `--{option} only applies to HTTP servers (--url).` 或 `--{option} only applies to stdio servers.` |
| KEY=VALUE 非法 | `--{option} expects KEY=VALUE, got "{pair}".` |
| 校验失败 | `validateMcpServerConfig` 的错误串原样（见 `ca-resources` §2.9.7） |
| 写文件失败 | `Could not update {path}: {msg}` |
| 成功 | `Replaced`/`Added` + ` {scope} MCP server "{name}" in {path}.` |
| 项目未信任 | `The project is not trusted, so {path} is ignored until you start kiana in the project and trust it.` |
| 收尾提示 | `Check it with: kiana mcp list` + 可选 `. If it requires sign-in: kiana mcp login {name}` |

要点：

- **`--` 之前的选项属于 `kiana mcp add`，之后属于子命令**；但 `maxPositionals = 2` 意味着一旦读到 `name` 与
  `command[0]`，后续 token（即使以 `--` 开头）都进 positional。
- `--bearer-token-env-var X` 写入 `headers.Authorization = "Bearer ${X}"`（**字面量 `${` + 名字 + `}`**）。
- `--oauth-callback-port` 用 `Number(port)`（非数值 → `NaN` → 校验报 `oauth.callbackPort must be a port number`）。
- HTTP 配置**不含空 `headers` 或空 `oauth` 键**。
- `mayNeedSignIn` 只看 `headers` 是否含 `authorization`（大小写不敏感）；有 `auth` 字段时也仍会提示（因为 CLI 不产生 `auth`）。

#### 2.10.7 `remove(args, projectConfig, options, log, error)`

```text
parsed = parseOptions(args, { local: "flag" }, error); if (!parsed) return 1
[name, ...extra] = parsed.positional
if (!name || extra.length > 0) { error(`Usage: kiana mcp remove <server> [-l]\n${HELP_HINT}`); return 1 }
project = parsed.values.has("local")
globalConfig = join(options.agentDir, "mcp.json")
path = project ? projectConfig : globalConfig
scope = project ? "project" : "global"
try { removed = removeMcpServerConfig(path, name) }
catch (e) { error(`Could not update ${path}: ${errorMessage(e)}`); return 1 }
if (removed) { log(`Removed ${scope} MCP server "${name}" from ${path}.`); return 0 }
other = loadMcpConfig({ agentDir: options.agentDir, cwd: options.cwd, projectTrusted: true })
	.servers.find(s => s.name === name && s.scope !== scope)
error(`No ${scope} MCP server named "${name}" in ${path}.${other ? ` It is defined in ${other.source}${other.scope === "project" ? "; use --local" : "; omit --local"}.` : ""}`)
return 1
```

逐字：

- 成功：`Removed {scope} MCP server "{name}" from {path}.`
- 用法：`Usage: kiana mcp remove <server> [-l]\n{HELP_HINT}`
- 找不到：`No {scope} MCP server named "{name}" in {path}.` + 可选 ` It is defined in {source}; use --local.`
  或 ` It is defined in {source}; omit --local.`
- **注意**：查找 `other` 时使用 `projectTrusted: true`，因此即使项目未信任也能指出“在项目里”。

#### 2.10.8 `list(loaded, json, untrustedNote, options, credentials, log)`

```ts
const reports = await Promise.all(loaded.servers.map(async entry => {
	const report: ServerReport = {
		name: entry.name, scope: entry.scope ?? "global", source: entry.source,
		...(entry.override ? { override: entry.override } : {}),
		enabled: entry.config.enabled !== false,
		exposure: entry.config.exposure ?? "codemode",
		transport: describeTransport(entry),
		state: "disabled", tools: [],
	};
	if (!report.enabled) return report;
	const connection = createConnection(entry, options, credentials);
	try { await connection.getClient() } catch { /* 状态与错误记录在连接上 */ }
	report.state = connection.state;
	report.tools = connection.tools.map(t => t.name);
	const overrides = connection.tools.flatMap(t => {
		const exposure = getMcpToolExposure(entry.config, t.name);
		return exposure === report.exposure ? [] : [[t.name, exposure] as const];
	});
	if (overrides.length > 0) report.toolExposure = Object.fromEntries(overrides);
	if (connection.hasResources) report.resources = true;
	if (connection.state !== "connected" && connection.error) report.error = connection.error;
	await connection.close();
	return report;
}));
const failed = loaded.errors.length > 0
	|| reports.some(report => report.enabled && report.state !== "connected");

if (json) {
	log(JSON.stringify({ servers: reports, errors: loaded.errors, ...(untrustedNote ? { note: untrustedNote } : {}) }, null, 2));
	return failed ? 1 : 0;
}
if (reports.length === 0 && loaded.errors.length === 0)
	log(`No MCP servers configured. Add them to ${join(options.agentDir, "mcp.json")} or .kiana/mcp.json.`);
for (const report of reports) {
	const state = report.state === "connected" ? `connected, ${report.tools.length} tool${report.tools.length === 1 ? "" : "s"}`
		: report.state === "needs-auth" ? "needs sign-in" : report.state;
	log(`${report.name}: ${state} (${report.exposure}, ${report.scope})`);
	log(`  ${report.transport}`);
	if (report.override) log(`  project override: ${report.override}`);
	if (report.state === "needs-auth") log(`  sign in with: kiana mcp login ${report.name}`);
	if (report.tools.length > 0) {
		const tools = report.tools.map(t => {
			const exposure = report.toolExposure?.[t];
			return exposure ? `${t} [${exposure}]` : t;
		});
		log(`  tools: ${tools.join(", ")}`);
	}
	if (report.resources) log("  offers resources");
	if (report.error) log(`  ${report.error.split("\n").join("\n  ")}`);
}
for (const configError of loaded.errors) log(`config error: ${configError}`);
if (untrustedNote) log(untrustedNote);
return failed ? 1 : 0;
```

逐字/行为：

- 每个启用的服务器**并行**连接（`Promise.all`），每份报告独立。
- 退出码：出现 config 错误，或**任一启用服务器非 `connected`** → 1；否则 0（禁用服务器不影响）。
- JSON 输出：2 空格缩进，无末尾换行（`console.log` 会加一个 `\n`）；键顺序
  `{ servers: [...], errors: [...], note? }`；`servers` 数组顺序 = `loaded.servers` 顺序。
- 人类可读行（全部逐字）：

```
{name}: {state} ({exposure}, {scope})
  {transport}
  project override: {override}
  sign in with: kiana mcp login {name}
  tools: {a, b [direct]}
  offers resources
  {error 每一行都缩进两格}
config error: {error}
{untrustedNote}
```

- `connected, 1 tool` / `connected, 2 tools`（单复数）。
- `needs-auth` 显示为 `needs sign-in`。
- `tools:` 行只在有工具时出现；有 `toolExposure` 覆盖的工具加 ` [exposure]` 后缀。
- 错误文本的**每个换行后**补两空格缩进。
- 空列表时才输出 `No MCP servers configured. Add them to {agentDir}/mcp.json or .kiana/mcp.json.`
  （注意：这里是 `join(options.agentDir, "mcp.json")`，与 `/mcp` 状态里的 `resolve(getAgentDir(), "mcp.json")` 等价但写法不同）。

#### 2.10.9 `login(entry, connection, url, timeoutMs, options, credentials, log, error)`

```ts
const { name } = entry;
try {
	await connection.getClient();
	log(`Already signed in to MCP server "${name}" (${connection.tools.length} tools).`);
	return 0;
} catch {
	if (connection.state !== "needs-auth") {
		error(`MCP server "${name}" failed to connect: ${connection.error ?? "unknown error"}`);
		return 1;
	}
}
const openUrl = options.openUrl ?? openBrowser;
const interactive = process.stdin.isTTY === true && options.openUrl === undefined;
try {
	await signInMcpServer({
		serverUrl: url,
		store: credentials.forServer(name, url),
		settings: connection.oauthSettings(),
		challenge: connection.challenge,
		prompt: {
			showAuthorizationUrl: authorizationUrl => {
				log(`Sign in to MCP server "${name}" in your browser:\n${authorizationUrl.href}`);
				openUrl(authorizationUrl.href);
			},
			promptForRedirectUrl: signal => waitForRedirectUrl(signal, interactive),
		},
		signal: AbortSignal.timeout(timeoutMs),
	});
} catch (signInError) {
	error(signInError instanceof McpSignInCancelledError
		? `Sign-in to MCP server "${name}" was cancelled or not completed within ${Math.round(timeoutMs / 1000)} seconds.`
		: `Sign-in to MCP server "${name}" failed: ${errorMessage(signInError)}`);
	return 1;
}
connection.challenge = undefined;
try { await connection.reconnect() }
catch (connectError) { error(`Signed in, but ${errorMessage(connectError)}`); return 1; }
log(`Signed in to MCP server "${name}" (${connection.tools.length} tools).`);
return 0;
```

逐字：`Already signed in to MCP server "{name}" ({n} tools).`、
`MCP server "{name}" failed to connect: {error ?? "unknown error"}`、
`Sign in to MCP server "{name}" in your browser:\n{url}`、
`Sign-in to MCP server "{name}" was cancelled or not completed within {round(seconds)} seconds.`、
`Sign-in to MCP server "{name}" failed: {msg}`、`Signed in, but {msg}`、`Signed in to MCP server "{name}" ({n} tools).`

#### 2.10.10 `waitForRedirectUrl(signal, interactive)`

```text
if (!interactive) {
	if (!signal.aborted) await new Promise(resolve => signal.addEventListener("abort", resolve, { once: true }))
	return undefined
}
readline = createInterface({ input: process.stdin, output: process.stderr })
try { return await readline.question("If the browser cannot reach this machine, paste the URL it was redirected to: ", { signal }) }
catch { return undefined }
finally { readline.close() }
```

- `interactive` 仅当 **stdin 是 TTY 且未注入 `openUrl`**（测试注入 `openUrl` 从而禁用交互）。
- 提示逐字（末尾一个空格）：`If the browser cannot reach this machine, paste the URL it was redirected to: `
- 问题输出到 **stderr**。

---

### 2.11 `src/extensions/codemode/tool.ts`（411 行）

模块文档（顶层注释）已概括契约，逐字要点：

- 脚本可用 `tools`、`ALL_TOOLS`、`text()`、`image()`、`exit()`、`store()`/`load()`、`console.*`、`return <value>`，
  可用 `// @options:` 首行，可经 `models.*` 访问模型目录、分类器、图像模型。
- 结果以 `Script completed` / `Script failed` 头开始。
- 嵌套调用经 agent loop 的 `ctx.executeTool`，验证/钩子/权限与直接调用完全一致；**只有脚本输出**进入模型上下文。
- 嵌套结果映射：声明了 `outputSchema` 的工具 → `structuredContent`（错误结果若带 `structuredContent` 也一样）；
  其它工具 → 其文本内容（多块以 `\n` 连接）为单个字符串；失败/被阻止/非法调用 → 以工具错误文本 reject。
- 失败脚本返回**正常错误结果**，保留部分输出，后接 `Script error:` 与错误。
- `store()`/`load()` 跨调用保存 JSON 值；成功脚本把写入作为 `codemode-store` custom entry 追加到会话，
  因此每个分支只看到自己路径上的值。

#### 2.11.1 常量与导出

```ts
export const CODEMODE_TOOL_NAME = "codemode";
export const CODEMODE_STORE_ENTRY_TYPE = "codemode-store";
export interface CodemodeStoreEntryData { set: Record<string, unknown>; delete: string[] }
export type CodemodeModelRuntime = Pick<ModelRegistry,
	"getModelsOfType" | "getAvailableOfType" | "getModelOfType" | "classify" | "generateImages">;
export interface CodemodeToolOptions {
	getToolNamespace?: (toolName: string) => ToolNamespace | undefined;
	getToolGuidelines?: () => ReadonlyMap<string, readonly string[]>;
	models?: boolean;
	appendEntry?: (customType: string, data: CodemodeStoreEntryData) => void;
	getMode?: () => CodemodeMode;                       // "on" | "only"
	getInlineBudget?: () => number | undefined;
}
const TEXT_OUTPUT_SCHEMA: CodemodeJsonSchema = { type: "string" };

export const codemodeSchema = Type.Object({
	code: Type.String({ description: "Raw JavaScript source." }),
});
export type CodemodeToolInput = Static<typeof codemodeSchema>;
export type CodemodeNestedCallStatus = "running" | "ok" | "error" | "cancelled";
export interface CodemodeNestedCall {
	id: string;                 // "<codemode call id>/<n>"
	name: string;
	args: string;               // 参数紧凑 JSON，供显示截断
	status: CodemodeNestedCallStatus;
	durationMs?: number;
	error?: string;             // 供显示截断的错误文本
	cost?: number;              // 报告了 usage 的 models.* 调用，USD
}
export interface CodemodeToolDetails { calls: CodemodeNestedCall[]; fullOutputPath?: string }

export const codemodeToolSystemPromptContribution = {
	snippet: "Run JavaScript that calls other tools",
	guidelines: [
		"Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.",
	],
} as const;

export const CODEMODE_DOCS_PATH = join(getDocsPath(), "codemode.md");
export const DEFAULT_CODEMODE_INLINE_BUDGET = SETTINGS_DEFAULTS.codemode.inlineBudget;   // 3000
const CHARS_PER_TOKEN = 4;
```

#### 2.11.2 `DESCRIPTION_INTRO`（逐字，含反引号）

```
Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.
- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.
- Optional first line: `// @options: {"max_output_tokens": 10000, "timeout_ms": 60000}`
```

#### 2.11.3 `describeGlobals(models)`

```ts
const lines = [
	"Globals:",
	"- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.",
	"- `store(key, value)` and `load(key)` keep JSON values across codemode calls.",
	"- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.",
];
if (models) lines.push(`- \`models\`: classifiers and image generation. Read ${CODEMODE_DOCS_PATH} first.`);
return lines.join("\n");
```

#### 2.11.4 `toCodemodeDeclaration(tool, guidelines = [])`

```ts
const bullets = guidelines.flatMap(g => g.trim() ? [`- ${g.trim()}`] : []);
return {
	name: tool.name,
	description: bullets.length > 0 ? `${tool.description.trim()}\n\n${bullets.join("\n")}` : tool.description,
	inputSchema: tool.parameters,
	outputSchema: tool.outputSchema ?? { type: "string" },
};
```

- 无 guidelines 时 **description 原样**（不 trim）；有 guidelines 时先 `trim()` 再拼接。
- `outputSchema` 缺省为 `{type:"string"}`。

`getCodemodeCallableTools(tools)`：`tools.filter(t => t.name !== "codemode")`。

#### 2.11.5 `CodemodeDescriptionOptions`

```ts
{ models?: boolean; namespaces?: ReadonlyMap<string, ToolNamespace>;
  deferred?: ReadonlySet<string>; guidelines?: ReadonlyMap<string, readonly string[]>;
  inlineBudget?: number }
```

#### 2.11.6 `renderToolSection(declaration)`（内部）

```ts
const id = toCodemodeIdentifier(declaration.name);
const heading = id === declaration.name ? `### \`${id}\`` : `### \`${id}\` (\`${declaration.name}\`)`;
return `${heading}\n${renderToolSample(declaration).trim()}`;
```

#### 2.11.7 `selectCatalog(groups, budget)`（内部，预算分配算法）

```ts
if (budget === undefined) return 全部条目名集合;
queues = groups.map(g => [...g.entries].sort((a, b) => a.cost - b.cost));
shown = new Set(); remaining = budget;
active = queues.filter(q => q.length > 0);
while (active.length > 0) {
	active = active.filter(queue => {
		const next = queue[0];
		if (next.cost > remaining) return false;      // 该组退出（被永久丢弃）
		remaining -= next.cost;
		shown.add(next.name);
		queue.shift();
		return queue.length > 0;
	});
}
return shown;
```

关键语义（**轮转**，不是简单贪心）：每轮每个仍活跃的组各放**一个最便宜的**条目；
某组的“下一个”放不下时该组**退出**（它后面的条目也全不放），其它组继续。
因此每个命名空间在任一命名空间完成之前都已被代表。

`cost = Math.ceil(section.length / 4)`（section 是 UTF-16 长度）。

#### 2.11.8 `createCodemodeDescription(tools, options = {})`

```ts
declarations = getCodemodeCallableTools(tools)
	.filter(t => !options.deferred?.has(t.name))
	.map(t => toCodemodeDeclaration(t, options.guidelines?.get(t.name)));

groups = new Map<string, CatalogGroup>([["", { namespace: undefined, entries: [] }]]);
for (const declaration of declarations) {
	const namespace = options.namespaces?.get(declaration.name);
	const key = namespace ? `ns:${namespace.name}` : "";
	const group = groups.get(key) ?? { namespace, entries: [] };
	groups.set(key, group);
	group.entries.push({ name: declaration.name, section: renderToolSection(declaration),
	                     cost: Math.ceil(section.length / CHARS_PER_TOKEN) });
}
ordered = [...groups.values()].sort((a, b) =>
	a.namespace === undefined ? -1
	: b.namespace === undefined ? 1
	: a.namespace.name.localeCompare(b.namespace.name));
shown = selectCatalog(ordered, options.inlineBudget);

sections = [DESCRIPTION_INTRO, describeGlobals(options.models === true)];
if (declarations.length === 0) return sections.join("\n\n");

toolSections = ["Nested tools:"];
for (const { namespace, entries } of ordered) {
	const visible = entries.filter(e => shown.has(e.name));
	if (namespace) {
		const listing = visible.length === entries.length ? ""
			: visible.length === 0 ? " (tools not listed)" : " (some tools not listed)";
		const description = namespace.description?.trim();
		toolSections.push(`## ${namespace.name}${listing}${description ? `\n${description}` : ""}`);
	}
	for (const entry of visible) toolSections.push(entry.section);
}
sections.push(toolSections.join("\n\n"));
return sections.join("\n\n");
```

结构（逐字）：

```
<DESCRIPTION_INTRO>

Globals:
- ...

Nested tools:

## <ns 名>[ (tools not listed)| (some tools not listed)]
<ns description>

### `tool_id` (`raw name`)
<tool sample>
```

细节：

- 无命名空间的组**始终排最前**（`""` 键的组先插入，比较器也强制顺序）。
- 组内条目保持声明顺序（然后 `selectCatalog` 内部按 cost 排序，但输出按**原序**过滤 `shown`）。
- `selectedCatalog` 只影响是否展示，不影响顺序。
- `deferred` 工具**完全不参与**（连分组/预算都不占），因此描述在 MCP 服务器连接、工具晚注册时保持不变。
- 组内所有条目都被 `selectCatalog` 丢弃时 → ` (tools not listed)`；部分丢弃 → ` (some tools not listed)`。
- 命名空间描述为空字符串时**不加换行**。
- `declarations.length === 0` 时**没有** `Nested tools:` 段。
- 唯一的分隔符是 `"\n\n"`。

#### 2.11.9 `describeFields(schema, depth)` / `describeOutput(schema)` / `describeScriptCall(tool)`

```ts
function describeFields(schema, depth): string | undefined {
	if (typeof schema !== "object" || schema === null) return undefined;
	const { type, properties, required } = schema;
	if (type !== "object" || typeof properties !== "object" || properties === null) return undefined;
	const requiredNames = new Set(Array.isArray(required) ? required : []);
	const fields = Object.entries(properties).map(([name, field]) => {
		const nested = depth > 0 ? describeFields(field, depth - 1) : undefined;
		return `${name}${requiredNames.has(name) ? "" : "?"}${nested ? `: ${nested}` : ""}`;
	});
	return `{ ${fields.join(", ")} }`;
}
function describeOutput(schema) {
	const type = renderToolOutputType(schema);
	if (type === "string") return "a string";
	const fields = describeFields(schema, 1);
	return `\`${fields ?? type.replace(/\s+/g, " ")}\``;
}
function describeScriptCall(tool) {
	return `${tool.description.trim()}\n\nCodemode: \`tools.${toCodemodeIdentifier(tool.name)}(args)\` resolves to ${describeOutput(...)}.`;
}
```

例：`{ output, exit_code, full_output_path? }`、嵌套一层 `{ content, structuredContent?: { results } }`。

#### 2.11.10 `prepareCodemodeLoadout(loadout, options)`

```ts
const mode = options.getMode?.() ?? SETTINGS_DEFAULTS.codemode.mode;     // "on"
const isDirect = (tool) => loadout.getExposure(tool.name) === "direct";
const callable = getCodemodeCallableTools(loadout.callable);
const callableNames = new Set(callable.map(t => t.name));
const descriptions: Record<string, string> = {};
if (mode === "on") {
	for (const tool of loadout.declared)
		if (callableNames.has(tool.name)) descriptions[tool.name] = describeScriptCall(tool);
}
const listed = mode === "only" ? callable : callable.filter(t => !isDirect(t));
const namespaces = new Map(listed.flatMap(t => {
	const ns = loadout.getNamespace(t.name); return ns ? [[t.name, ns] as const] : [];
}));
const guidelines = new Map(listed.map(t => [t.name, loadout.getPromptGuidelines(t.name)] as const));
descriptions[CODEMODE_TOOL_NAME] = createCodemodeDescription(listed, {
	models: options.models === true,
	namespaces, guidelines,
	deferred: new Set(listed.filter(t => loadout.getExposure(t.name) === "deferred").map(t => t.name)),
	inlineBudget: options.getInlineBudget?.() ?? DEFAULT_CODEMODE_INLINE_BUDGET,
});
const declaredNames = new Set(loadout.declared.map(t => t.name));
return {
	descriptions,
	hiddenDeclarations: mode === "only"
		? callable.filter(t => isDirect(t) && declaredNames.has(t.name)).map(t => t.name)
		: [],
};
```

两种模式（逐字文档）：

- `on`（默认）：**已声明**且**可调用**的工具，其 description 追加 `Codemode: \`tools.x(args)\` resolves to ...`；
  codemode 自身描述只列**非 direct** 的可调用工具。
- `only`：codemode 描述列**全部**可调用工具；请求中**去掉** active `direct` 工具的声明（`hiddenDeclarations`）。

要点：按 **exposure** 而非 active 集合决定是否列出，所以 `tool_search` 加载工具时 codemode 描述不变（不重复声明）。

#### 2.11.11 `getScriptReach(code)`（`reachesTools`）

```ts
const DISCOVERY_GLOBALS = /\b(searchTools|describeNamespace|describeTool|ALL_TOOLS)\b/;
if (typeof code !== "string") return [];
return DISCOVERY_GLOBALS.test(code) ? "all" : (code.match(/\w+/g) ?? []);
```

即使用任一发现全局 → `"all"`；否则返回源码中所有 `\w+` 词（Rust 需与 JS `\w` 一致：ASCII `[A-Za-z0-9_]`，无 Unicode）。

#### 2.11.12 `createCodemodeToolDefinition(options = {})`

```ts
return {
	name: CODEMODE_TOOL_NAME,
	label: CODEMODE_TOOL_NAME,
	description: createCodemodeDescription([], { models: options.models === true }),   // 激活时被替换
	promptSnippet: codemodeToolSystemPromptContribution.snippet,
	promptGuidelines: [...codemodeToolSystemPromptContribution.guidelines],
	parameters: codemodeSchema,
	exposure: "model-only",                       // 脚本不能启动别的脚本
	prepareLoadout: (loadout) => prepareCodemodeLoadout(loadout, options),
	reachesTools: (input) => getScriptReach(input.code),
	constrainedSampling: { type: "grammar", variants: { openai_lark: CODEMODE_SOURCE_GRAMMAR } },
	execute: async (toolCallId, params, signal, onUpdate, ctx) =>
		(await loadCodemodeExecutor()).executeCodemode(toolCallId, params, signal, onUpdate, ctx, options),
	...codemodeRenderers,
};
```

`createCodemodeTool(tools = [], options = {})`：包装为 `AgentTool` 并 `Object.assign` 覆盖 `description`/`promptSnippet`/`promptGuidelines`
（供 SDK/测试直接使用；`description` 用传入工具列表）。

---

### 2.12 `src/extensions/codemode/execute.ts`（850 行）

#### 2.12.1 常量

```ts
const ARGS_PREVIEW_CHARS = 200;
const ERROR_PREVIEW_CHARS = 500;
const MAX_CONCURRENT_MODEL_CALLS = 4;
const CODEMODE_MEMORY_LIMIT_BYTES = 256 * 1024 * 1024;     // 256 MiB
const DEFAULT_MAX_CODEMODE_WORKERS = 2;
const CODEMODE_WORKER_WAIT_MS = 60_000;
const DEFAULT_MAX_OUTPUT_TOKENS = 10_000;
const CHARS_PER_TOKEN = 4;
const MODEL_TYPES: ReadonlySet<string> = new Set(["chat", "image", "classifier"]);
const IMAGE_EXTENSIONS = { "image/png": ".png", "image/jpeg": ".jpg", "image/gif": ".gif", "image/webp": ".webp" };
```

模块状态（进程级）：

```ts
let activeCodemodeWorkers = 0;
const codemodeWorkerWaiters: (() => void)[] = [];
```

#### 2.12.2 工作者准入（worker admission）

```ts
function maxCodemodeWorkers(): number {
	const value = Number(process.env.KIANA_CODEMODE_MAX_WORKERS);
	return Number.isInteger(value) && value > 0 ? value : 2;
}
function reserveCodemodeWorker(signal, timeoutMs): Promise<"reserved" | "aborted" | "timeout">
function admitCodemodeWorkers(): void
function releaseCodemodeWorker(): void
```

`reserveCodemodeWorker`：

```text
if (signal?.aborted) return "aborted"                       // 同步短路，不入队
return new Promise(resolve => {
	settle(outcome):
		clearTimeout(timer);
		signal?.removeEventListener("abort", onAbort);
		const index = codemodeWorkerWaiters.indexOf(grant);
		if (index >= 0) codemodeWorkerWaiters.splice(index, 1);
		resolve(outcome);
	grant = () => settle("reserved");
	onAbort = () => settle("aborted");
	timer = setTimeout(() => settle("timeout"), timeoutMs);
	codemodeWorkerWaiters.push(grant);
	signal?.addEventListener("abort", onAbort, { once: true });
	admitCodemodeWorkers();                                  // 同步尝试
})
```

`admitCodemodeWorkers`：

```ts
while (codemodeWorkerWaiters.length > 0 && activeCodemodeWorkers < maxCodemodeWorkers()) {
	activeCodemodeWorkers++;
	codemodeWorkerWaiters.shift()?.();      // grant() → settle("reserved") → 从队列 splice（已 shift 掉 → index -1）
}
```

`releaseCodemodeWorker`：`activeCodemodeWorkers--; admitCodemodeWorkers();`。

**先到先得**（FIFO）；`maxCodemodeWorkers()` **每次**调用都重读 env（SDK 宿主可运行时设置）。
`KIANA_CODEMODE_MAX_WORKERS` 必须是**正整数**（`Number.isInteger && > 0`），否则回落 2。

#### 2.12.3 小工具

```ts
function truncateText(text, maxChars) { return text.length > maxChars ? `${text.slice(0, maxChars - 3)}...` : text; }
function previewArgs(args) { if (args === undefined) return ""; try { return truncateText(JSON.stringify(args) ?? "", 200); } catch { return ""; } }
function textOf(result) { return (result.content ?? []).filter(b => b.type === "text").map(b => b.text).join("\n"); }
function toModelType(value) { ... else throw new Error(`Unknown model type ${JSON.stringify(value)}. Use "chat", "image", or "classifier".`) }
function toProvider(value) { undefined|null → undefined; 非 string → throw new Error("provider must be a string") }
function toModelInfo(model) { const info = { ...model }; delete info.headers; return info; }
function withArticle(word) { return `${/^[aeiou]/.test(word) ? "an" : "a"} ${word}`; }
```

#### 2.12.4 `describeValue(value)`（错误信息里的值描述，逐字）

```text
undefined/null → "undefined" / "null"
array → 空 ? "an empty array" : "an array"
object → 无键 ? "{}" : `{ k1, k2, ... }`（最多 6 个键，超过加 ", ..."）
string → "a string"
其它 → `a ${typeof value}`   // 如 "a number"、"a boolean"
```

#### 2.12.5 上下文校验（classifier / images）

```ts
const CLASSIFIER_CONTEXT_SHAPE =
	'{ state: { ... }, images?: [{ type: "image", data: <base64>, mimeType }], questions: { <id>: { type: "choice", instructions, criteria: { <label>: <meaning> } } | { type: "score", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: "bool", instructions, criteria: { true: <meaning>, false: <meaning> } } } }';
```

`checkClassifierContext(context)` 的 `fail(problem)` 统一构造：

```
models.classify() <problem>. Expected context: <CLASSIFIER_CONTEXT_SHAPE>. See "Classify" in <CODEMODE_DOCS_PATH>.
```

校验顺序与逐字 problem 串：

| 顺序 | 条件 | problem |
|---|---|---|
| 1 | context 非对象/数组/null | `expects a context object as its second argument, got ${describeValue(context)}` |
| 2 | `state` 非对象/数组/null | `context.state must be an object, got ${describeValue(state)}` |
| 3 | `images` 不是数组 | `context.images must be an array, got ${describeValue(images)}` |
| 4 | 每个 image 非 `{type:"image",data:string,mimeType:string}` | `context.images[${index}] must be an image block, got ${describeValue(image)}` |
| 5 | `questions` 非对象/数组/空 | `context.questions must map question IDs to questions, got ${describeValue(questions)}` |
| 6 | question 非对象/数组/null | `context.questions.${id} must be a question object, got ${describeValue(question)}` |
| 7 | `instructions` 非字符串 | `context.questions.${id}.instructions must be a string` |
| 8a | `choice` 且 criteria 非对象/数组/值非全字符串（且**非空**） | `context.questions.${id} is a "choice" question, so criteria must map each label to its meaning` |
| 8b | `score` 且 criteria 非非空字符串数组 | `context.questions.${id} is a "score" question, so criteria must list the levels as strings, lowest first` |
| 8c | `bool` 且 `criteria.true`/`criteria.false` 非字符串 | `context.questions.${id} is a "bool" question, so criteria must be { true: string, false: string }` |
| 8d | 其它 type | `context.questions.${id}.type must be "choice", "score", or "bool", got ${JSON.stringify(type)}` |

`isStrings(values)`：`values.length > 0 && every(string)`。所以 `choice` 的空 criteria 也算错。

`checkImagesContext(context)`：

```
models.generateImages() <problem>. Expected context: { input: [{ type: "text", text: <prompt> }, ...optional { type: "image", data: <base64>, mimeType } references] }. See "Generate images" in <CODEMODE_DOCS_PATH>.
```

| 条件 | problem |
|---|---|
| 非对象/数组/null | `expects a context object as its second argument, got ${describeValue(context)}` |
| `input` 非非空数组 | `context.input must be a non-empty array of blocks, got ${describeValue(input)}` |
| block 非 text（含 `text:string`）或 image（含 `data`/`mimeType` string） | `context.input[${index}] must be a text or image block, got ${describeValue(block)}` |

#### 2.12.6 `createLimiter(limit)`（内部并发限制器，**按调用顺序**）

```ts
let active = 0; const waiting: (() => void)[] = [];
return async (run) => {
	if (active >= limit) await new Promise<void>(resolve => waiting.push(resolve));
	active++;
	try { return await run(); }
	finally { active--; waiting.shift()?.(); }
};
```

注意：`active++` 在等待**之后**；唤醒是 FIFO 单发。

#### 2.12.7 `readCodemodeStore(branch)`（导出）

```ts
const store = new Map<string, unknown>();
for (const entry of branch) {
	if (entry.type !== "custom" || entry.customType !== "codemode-store" || !isStoreEntryData(entry.data)) continue;
	for (const key of entry.data.delete) store.delete(key);
	for (const [key, value] of Object.entries(entry.data.set)) store.set(key, value);
}
return Object.fromEntries(store);
```

- 按**分支顺序从根**应用；先 `delete` 再 `set`（同一条目内）。
- `isStoreEntryData`：`set` 是非 null 对象且 `delete` 是字符串数组。
- 输出键顺序 = Map 插入顺序（删除后重新插入会移到最后）。

#### 2.12.8 输出整形

`valueText(value)`：字符串原样；否则 `JSON.stringify(value) ?? String(value)`。

`formatOutput(output)`：

```text
total = output.filter(item => item.type === "text" && !item.console).length
items = []; consoleLines = []; index = 0
for (const item of output) {
	if (item.type === "image") items.push(item)
	else if (item.console) consoleLines.push(item.text)
	else { index++; items.push({ type: "text", text: total > 1 ? `==> text ${index}/${total} <==\n${item.text}` : item.text }) }
}
if (consoleLines.length > 0)
	items.push({ type: "text", text: `<console_output>\n${consoleLines.join("\n")}\n</console_output>` })
```

即：`total > 1` 时每个文本项前加 `==> text i/total <==`；`console` 行**全部**集中在末尾单个 `<console_output>` 块。

`joinAdjacentText(items)`：把相邻文本项合并，每部分以换行开始（若上一部分不以 `\n` 结尾则补 `\n`）；
（**性能注意**：只检查 `lastPart`，避免 O(n²) 字符串拼接）。图片项打断合并。

`formatCallSummary(calls)`：

```ts
if (calls.length === 0) return "No tool calls were made.";
return `Tool calls made before the failure (they are not undone): ${calls.map(c => `${c.name} (${c.status})`).join(", ")}`;
```

`formatError(result, calls)`：

```text
kind === "script"  → error.stack ?? `${error.name ?? "Error"}: ${error.message}`
kind === "timeout" → `Script timed out: ${error.message}`
kind === "aborted" → `Script aborted: ${error.message}`
否则               → `Script sandbox failed: ${error.message}`
返回 `${head}\n\n${formatCallSummary(calls)}`
```

#### 2.12.9 输出溢出与图片落盘

`spillOutput(text)`：`writeOutputFile("kiana-codemode", ".txt", text)` → `{ path }`；失败 → `{ error: msg }`。

`IMAGE_EXTENSIONS` 见上；无扩展名 → `throw new Error(\`No file extension for image type ${mimeType}\`)`。

`saveImages(items)`：每个 `image` 前插一个文本项：

- 成功：`[Image saved to ${path} (${mimeType}, ${formatSize(bytes.length)})]`
- 失败：`[Image (${mimeType}, ${formatSize(n)}) could not be saved: ${msg}]`

同一 `data` 的图片**只保存一次**（`Map<data, Promise<label>>`），但**每个**出现处都插标签（指向同一路径）。
`formatSize` 见 `ca-tools-compaction-export`（`${bytes}B` / `x.xKB` / `x.xMB`）。

`truncateOutput(items, maxTokens)`：

```text
texts = 文本项文本; combined = texts.join("\n"); budget = maxTokens * 4
if (texts.length === 0 || combined.length <= budget) return { items }
headChars = floor(budget / 2); tailChars = budget - headChars;
removed = combined.length - headChars - tailChars
head = combined.slice(0, headChars); tail = tailChars > 0 ? combined.slice(-tailChars) : ""
text = `Warning: truncated output (original token count: ${Math.ceil(combined.length / 4)})\nTotal output lines: ${combined.split("\n").length}\n\n${head}…${Math.ceil(removed / 4)} tokens truncated…${tail}`
spilled = await spillOutput(combined)
text += "path" in spilled
	? `\n\n[Full output: ${path} (read with offset/limit)]`
	: `\n\n[Could not save the full output: ${error}]`
return { items: [{ type: "text", text }, ...images], ...(path ? { fullOutputPath: path } : {}) }
```

注意省略号是单字符 `…`（U+2026），三处均如此；`head`/`tail` 按 **UTF-16** 切分。

#### 2.12.10 `toScriptValue(tool, outcome)`

```ts
if (tool.outputSchema && result.structuredContent !== undefined) return result.structuredContent;
const text = textOf(result);
if (outcome.isError) throw new Error(text || `Tool "${tool.name}" failed`);
return text;
```

#### 2.12.11 `executeCodemode(...)` 主流程（逐段）

```ts
const startedAt = performance.now();
const { code, options: sourceOptions } = parseCodemodeSource(input.code);
const calls: CodemodeNestedCall[] = [];
// models.* 调用的 usage，嵌套工具调用由会话侧统计
let modelUsage: Usage | undefined; let generatedImages = 0;
const addGeneratedImages = (count: number) => { generatedImages += count; };
const addModelUsage = (usage: Usage) => {
	modelUsage = modelUsage ? combineUsage(modelUsage, usage) : usage;   // core/usage-totals.ts
};
const snapshot = () => ({ calls: calls.map(c => ({ ...c })) });
const publish = () => onUpdate?.({ content: [], details: snapshot() });
publish();                                  // 先让行变 running，计时器启动
```

**sandboxTools**：

```ts
callable = ctx ? getCodemodeCallableTools(ctx.tools) : []
guidelines = options.getToolGuidelines?.()
samples = Map(callable.map(t => [t.name, renderToolSample(toCodemodeDeclaration(t, guidelines?.get(t.name)))]))
sandboxTools = callable.map(tool => ({
	name: tool.name,
	description: samples.get(tool.name),
	execute: async (args, { signal: callSignal }) => {
		record = { id: `${toolCallId}/?`, name: tool.name, args: previewArgs(args), status: "running" }
		calls.push(record); publish();
		callStartedAt = performance.now();
		if (!ctx) throw new Error("Tool calls need a session");
		outcome = await ctx.executeTool(tool.name, args, { signal: callSignal });
		record.id = outcome.toolCall.id;              // 实际为 "<calling id>/<n>"
		record.durationMs = performance.now() - callStartedAt;
		if (outcome.isError) {
			record.status = callSignal.aborted ? "cancelled" : "error";
			record.error = truncateText(textOf(outcome.result) || `Tool "${tool.name}" failed`, 500);
		} else record.status = "ok";
		publish();
		return toScriptValue(tool, outcome);
	},
}))
```

**工作者预留与失败结果**：

```ts
deadline = sourceOptions.timeoutMs ?? Infinity;
waitStartedAt = performance.now();
admission = await reserveCodemodeWorker(signal, Math.min(deadline, 60_000));
waitedMs = performance.now() - waitStartedAt;
if (admission !== "reserved") {
	waited = ((performance.now() - startedAt) / 1000).toFixed(1);
	reason = signal?.reason;
	error = admission === "aborted"
		? `Script aborted: ${reason instanceof Error ? reason.message : "Execution aborted"}`
		: deadline <= 60_000
			? `Script timed out: Execution timed out after ${deadline} ms while waiting for a free codemode worker`
			: `No codemode worker became free within 60 seconds. All ${maxCodemodeWorkers()} workers, shared by every session in this process, are held by other scripts. Retry after they finish.`;
	return {
		content: [
			{ type: "text", text: `Script failed\nWall time ${waited} seconds\nOutput:\n` },
			{ type: "text", text: `Script error:\n${error}\n\nThe script did not start.` },
		],
		details: snapshot(), isError: true,
	};
}
```

- 等待时长**计入** `deadline`；等待上限为 `min(deadline, 60000)`。
- 无空闲 worker 分支的秒数字面量来自 `${CODEMODE_WORKER_WAIT_MS / 1000}` = `60`（JS 数字转字符串为 `"60"`）；
  该分支只在 `deadline > 60_000` 时出现。

**沙箱执行**：

```ts
let reserved = true;
let sandbox; let result;
try {
	sandbox = new CodemodeSandbox({
		tools: sandboxTools,
		globals: [
			...createDiscoveryGlobals(callable, samples, options),
			...(options.models && ctx
				? createModelGlobals(ctx.modelRegistry, toolCallId, calls, publish, addModelUsage, addGeneratedImages)
				: []),
		],
		timeoutMs: Math.max(1, Math.round(deadline - waitedMs)),
		memoryLimitBytes: 256 * 1024 * 1024,
		wasm: loadQuickJSWasm(getQuickJSWasmPath()),
		workerUrl: getCodemodeWorkerSpecifier(),
		acquireWorker: () => {
			if (!reserved) throw new Error("The script already settled");
			reserved = false;                       // 之后由沙箱在 worker 真正退出时 release
			return releaseCodemodeWorker;
		},
	});
	const store = ctx ? readCodemodeStore(ctx.sessionManager.getBranch()) : {};
	result = await sandbox.execute(code, { signal, store });
} finally {
	await sandbox?.close();
	if (reserved) { reserved = false; releaseCodemodeWorker(); }
}
for (const call of calls) if (call.status === "running") call.status = "cancelled";
```

**结果整形**：

```ts
const scriptOutput = [...result.output];
if (result.ok) {
	const { set, delete: deleted } = result.storeWrites;
	if (Object.keys(set).length > 0 || deleted.length > 0)
		options.appendEntry?.("codemode-store", { set, delete: deleted });
	if (result.value !== undefined) scriptOutput.push({ type: "text", text: valueText(result.value) });
}
const items = formatOutput(scriptOutput);
if (!result.ok) items.push({ type: "text", text: `Script error:\n${formatError(result, calls)}` });
if (generatedImages > 0 && !items.some(i => i.type === "image"))
	items.push({ type: "text", text: `Note: models.generateImages() returned ${generatedImages} image${generatedImages === 1 ? "" : "s"} that the script did not show. Show each image block of result.output with image(block).` });

const truncated = await truncateOutput(joinAdjacentText(items), sourceOptions.maxOutputTokens ?? 10_000);
const output = joinAdjacentText(await saveImages(truncated.items));
const wallTime = ((performance.now() - startedAt) / 1000).toFixed(1);
const header = `${result.ok ? "Script completed" : "Script failed"}\nWall time ${wallTime} seconds\nOutput:\n`;
const details = snapshot();
if (truncated.fullOutputPath) details.fullOutputPath = truncated.fullOutputPath;
return {
	content: [{ type: "text", text: header }, ...output],
	details,
	...(modelUsage ? { usage: modelUsage } : {}),
	...(result.ok ? {} : { isError: true }),
};
```

顺序要点（**不可交换**）：`formatOutput` → 错误项 → 未展示图片提示 → `joinAdjacentText` → `truncateOutput`
→ `saveImages` → 再 `joinAdjacentText`。先截断再加图片标签，保证路径不被截掉。

`wallTime` 是 `toFixed(1)`（如 `0.0`、`12.3`），用于渲染器的 `SCRIPT_HEADER` 正则匹配。

#### 2.12.12 `createDiscoveryGlobals(tools, samples, options)`

```ts
const ranker = new Bm25Ranker();
const entry = (name) => ({ name: toCodemodeIdentifier(name), description: samples.get(name) ?? "" });
```

三个全局（均为 `spread: true`，即参数展开为多参）：

**`searchTools(query, options?)`**

```text
query 非 string → throw new Error("searchTools() expects a query string")
limit = options?.limit ?? 8
limit 非整数/≤0 → throw new Error("searchTools() limit must be a positive integer")
namespace = options?.namespace
namespace 非 undefined/null 且非 string → throw new Error("searchTools() namespace must be a string")
documents = tools.flatMap(tool => {
	const ns = options.getToolNamespace?.(tool.name);
	if (namespace && (!ns || !isNamespaceName(ns, namespace))) return [];
	return [createToolSearchDocument(tool, ns)];
})
return ranker.rank(query, documents, limit).map(m => entry(m.name))
```

**`describeTool(name)`**

```text
非 string → throw new Error("describeTool() expects a tool name")
tool = tools.find(t => t.name === name || toCodemodeIdentifier(t.name) === name)
return tool ? samples.get(tool.name) : undefined
```

**`describeNamespace(name)`**

```text
非 string → throw new Error("describeNamespace() expects a namespace name")
遍历 tools：ns = getToolNamespace(tool.name)；若 ns 且 isNamespaceName(ns, name)：
	namespace ??= ns; names.push(toCodemodeIdentifier(tool.name))
if (!namespace) return undefined
return { name: ns.name, ...(ns.description ? { description } : {}), ...(ns.instructions ? { instructions } : {}), tools: names }
```

`isNamespaceName(namespace, query)`：

```ts
const queryId = toCodemodeIdentifier(query);
return [namespace.name, ...(namespace.aliases ?? [])].some(n => n === query || toCodemodeIdentifier(n) === queryId);
```

例：`dev-radius` 与 `dev_radius` 互相匹配。

#### 2.12.13 `createModelGlobals(models, toolCallId, calls, publish, addUsage, addGeneratedImages)`（`addUsage` 即 `addModelUsage`）

`limit = createLimiter(4)`；`callCount` 从 0 开始，每个 `models.classify`/`models.generateImages` 自增。

**`runModelCall(name, type, [model, context], checkContext, run)`**：

```text
listHint = `List the ${type} models you can use with models.getAvailableOfType("${type}").`
given = model
if (typeof given?.provider !== "string" || typeof given?.id !== "string") {
	undefinedHint = (model === undefined || model === null)
		? " models.getModelOfType() returns undefined for an unknown provider or id." : ""
	throw new Error(`${name}() expects ${withArticle(type)} model as its first argument, got ${describeValue(model)}.${undefinedHint} ${listHint}`)
}
provider = given.provider; id = given.id; ref = `${provider}/${id}`
resolved = models.getModelOfType(type, provider, id)
if (!resolved) {
	actualType = [...MODEL_TYPES].find(other => other !== type && models.getModelOfType(other, provider, id) !== undefined)
	throw new Error(actualType
		? `"${ref}" is ${withArticle(actualType)} model, not ${withArticle(type)} model. ${listHint}`
		: `Unknown ${type} model "${ref}". ${listHint}`)
}
checked = checkContext(context)
record = { id: `${toolCallId}/${name}/${++callCount}`, name, args: `${resolved.provider}/${resolved.id}`, status: "running" }
calls.push(record); publish();
startedAt = performance.now();
result = await limit(() => run(resolved, checked));
record.durationMs = performance.now() - startedAt;
record.status = result.stopReason === "stop" ? "ok" : result.stopReason === "aborted" ? "cancelled" : "error";
if (result.errorMessage) record.error = truncateText(result.errorMessage, 500);
if (result.usage) { record.cost = result.usage.cost.total; addUsage(result.usage); }   // addUsage = addModelUsage
publish(); return result;
```

- `MODEL_TYPES` 的迭代顺序（Set 插入序）决定 `actualType` 的选取：`chat`、`image`、`classifier`。
- 行 `args` 只显示 `provider/id`，绝不显示 prompt 或图片数据。
- 只按 provider+id 解析模型，**不接受**脚本提供的 `baseUrl`/`headers`（防凭据泄漏）。

**五个全局实现**：

| 名称 | 行为 |
|---|---|
| `models.getModelsOfType(type, provider?)` | `toModelType(type)`、`toProvider(provider)`，返回 `map(toModelInfo)`（同步） |
| `models.getAvailableOfType(type, provider?)` | 异步，传 `{ signal }`，返回 `map(toModelInfo)` |
| `models.getModelOfType(type, provider, id)` | provider/id 非 string → throw `` `models.getModelOfType(type, provider, id) expects three strings, got (${args.map(describeValue).join(", ")}). The provider and the id are separate arguments, for example models.getModelOfType("classifier", "typesafe", "jev-latest").` ``；找到返回 `toModelInfo`，否则 `undefined`（JSON 里为 `null`） |
| `models.classify(model, context)` | `runModelCall("models.classify","classifier",…, checkClassifierContext, (m,c) => models.classify(m,c,{signal}))` |
| `models.generateImages(model, context)` | `runModelCall("models.generateImages","image",…, checkImagesContext, async (m,c) => { r = await models.generateImages(m,c,{signal}); addGeneratedImages(r.output.filter(b => b.type==="image").length); return r })` |

`toModelType` 错误：`Unknown model type ${JSON.stringify(value)}. Use "chat", "image", or "classifier".`

---

### 2.13 `src/extensions/codemode/renderer.ts`（156 行）

#### 2.13.1 常量与逐字串

```ts
const CODE_PREVIEW_LINES = 10;
const CALL_PREVIEW_COUNT = 8;
const OUTPUT_PREVIEW_LINES = 5;
const COLLAPSED_ARGS_CHARS = 80;
const SCRIPT_HEADER = /^Script (completed|failed)\nWall time [\d.]+ seconds\nOutput:\n$/;
```

`expandHint(theme, hidden, noun)`：

```ts
`${theme.fg("muted", `... (${hidden} more ${noun},`)} ${keyHint("app.tools.expand", "to expand")}${theme.fg("muted", ")")}`
```

去色后：`... (3 more lines, <key> to expand)`（`keyHint` 见 `tui-components`：`dim(keyText) + muted(" to expand")`）。

`formatDuration(ms)`：`undefined` → `""`；`< 1000` → `${Math.round(ms)}ms`；否则 `${(ms/1000).toFixed(1)}s`。

`formatCost(cost)`：``` `$${cost >= 0.01 ? cost.toFixed(2) : cost.toPrecision(2)}` ``` —— 例如 `$0.012`、`$1.23`、`$0.00012`。

`statusIcon(call, theme)`：`running` → `warning`＋`…`；`ok` → `success`＋`✓`；`error` → `error`＋`✗`；
`cancelled` → `muted`＋`⊘`。

`formatCall(call, theme, expanded)`：

```ts
const args = !expanded && call.args.length > 80 ? `${call.args.slice(0, 77)}...` : call.args;
const duration = formatDuration(call.durationMs);
let line = `${statusIcon(call, theme)} ${theme.fg("toolTitle", call.name)}`;
if (args) line += ` ${theme.fg("muted", args)}`;
if (duration) line += ` ${theme.fg("dim", duration)}`;
if (call.cost) line += ` ${theme.fg("dim", formatCost(call.cost))}`;
if (expanded && call.error) line += `\n    ${theme.fg("error", call.error.split("\n").join("\n    "))}`;
return line;
```

即每个嵌套调用一行：`<icon> <name> <args> <duration> <cost>`；展开时错误另起一行、缩进 4 空格。

#### 2.13.2 `codemodeRenderers`

**`renderCall(args, theme, context)`**

```ts
startToolTimer(context);
const code = str((args as { code?: unknown } | undefined)?.code);
const title = theme.fg("toolTitle", theme.bold("codemode"));
const component = (context.lastComponent as Container | undefined) ?? new Container();
component.clear();
if (code === null) { component.addChild(new Text(`${title} ${theme.fg("error", "[invalid arg]")}`, 0, 0)); return component; }
component.addChild(new Text(title, 0, 0));
if (code) {
	const highlighted = highlightCode(replaceTabs(code.replace(/\r/g, "").trimEnd()), "javascript").join("\n");
	component.addChild(context.expanded
		? new Text(highlighted, 0, 0)
		: new VisualLinePreview({ text: highlighted, maxVisualLines: 10, keep: "start",
		                          formatHint: hidden => expandHint(theme, hidden, "lines") }));
}
return component;
```

- 标题 `codemode`（bold + `toolTitle` 色）；`code` 非字符串 → `[invalid arg]`（error 色）。
- 脚本含 `// @options:` 行，故选项随脚本显示。
- 折叠时显示前 10 个**视觉行**（`VisualLinePreview`），提示 `... (N more lines, … to expand)`。

**`renderResult(result, options, theme, context)`**

```ts
const timerText = toolTimerText(context, options.isPartial);
const component = (context.lastComponent as Container | undefined) ?? new Container();
component.clear();
const calls = result.details?.calls ?? [];
if (calls.length > 0) {
	const shown = options.expanded ? calls : calls.slice(-8);          // 折叠显示最后 8 个
	const lines = shown.map(call => formatCall(call, theme, options.expanded));
	if (shown.length < calls.length)
		lines.unshift(`${theme.fg("muted", `... (${calls.length - shown.length} earlier calls,`)} ${keyHint("app.tools.expand", "to expand")}${theme.fg("muted", ")")}`);
	const priced = calls.filter(call => call.cost);
	if (priced.length > 1) {
		const total = priced.reduce((sum, call) => sum + (call.cost ?? 0), 0);
		lines.push(theme.fg("muted", `Model calls: ${formatCost(total)}`));
	}
	component.addChild(new Spacer(1));
	component.addChild(new Text(lines.join("\n"), 0, 0));
}

const [first, ...rest] = result.content;
const hasHeader = first?.type === "text" && SCRIPT_HEADER.test(first.text);
const output = options.isPartial ? "" : getTextOutput({ ...result, content: hasHeader ? rest : result.content },
                                                       context.showImages).trim();
if (output) {
	const color = context.isError ? "error" : "toolOutput";
	const styled = replaceTabs(output).split("\n").map(line => theme.fg(color, line)).join("\n");
	component.addChild(new Spacer(1));
	if (options.expanded) component.addChild(new Text(styled, 0, 0));
	else {
		component.addChild(new VisualLinePreview({ text: styled, maxVisualLines: 5, keep: "start",
		                                           formatHint: hidden => expandHint(theme, hidden, "lines") }));
		const fullOutputPath = result.details?.fullOutputPath;
		if (fullOutputPath) component.addChild(new Text(theme.fg("muted", `Full output: ${fullOutputPath}`), 0, 0));
	}
}
if (timerText) { component.addChild(new Spacer(1)); component.addChild(new Text(theme.fg("muted", timerText), 0, 0)); }
return component;
```

渲染细节（必须一致）：

- 调用列表**上方**空一行；输出段上方再空一行；计时器段上方再空一行。
- 折叠时取**最后** 8 个调用（不是前 8 个），并在顶部插 `... (N earlier calls, … to expand)`。
- `Model calls: $x` 只在**有价**调用 **> 1** 个时出现，且汇总**全部**调用（含被折叠的）。
- 输出头像被丢弃（`Script completed\nWall time …\nOutput:\n`）；无效参数（无头）时保留全部内容。
- 折叠输出按**视觉行**取前 5 行；其后追加 `Full output: <path>`（若有）。
- `timerText` 来自 `toolTimerText`：`Elapsed <time>`（部分）或 `Took <time>`（完成），格式见 `ca-tools-compaction-export`。

---

### 2.14 `src/extensions/codemode/index.ts`（49 行）+ 入口

```ts
export interface CodemodeExtensionOptions {
	mode?: CodemodeMode;          // 覆盖 codemode.mode 设置
	inlineBudget?: number;        // 覆盖 codemode.inlineBudget
	models?: boolean;             // 默认 true
}
function readMode(kiana) { return kiana.getSettings().codemode?.mode === "only" ? "only" : SETTINGS_DEFAULTS.codemode.mode; }
function readInlineBudget(kiana) {
	const budget = kiana.getSettings().codemode?.inlineBudget;
	return typeof budget === "number" && Number.isFinite(budget) && budget >= 0 ? budget : undefined;
}
export function createCodemodeExtension(options = {}): ExtensionFactory {
	return (kiana) => {
		kiana.registerTool({
			...createCodemodeToolDefinition({
				appendEntry: (type, data) => kiana.appendEntry(type, data),
				models: options.models ?? true,
				getToolNamespace: (name) => kiana.getAllTools().find(t => t.name === name)?.namespace,
				getToolGuidelines: () => new Map(kiana.getAllTools().map(t => [t.name, t.promptGuidelines ?? []])),
				getMode: () => options.mode ?? readMode(kiana),
				getInlineBudget: () => options.inlineBudget ?? readInlineBudget(kiana),
			}),
			defaultActive: false,                      // 默认不激活
		});
	};
}
export default createCodemodeExtension();
```

`readInlineBudget` 的接受条件：`number` 且有限且 `>= 0`；否则 `undefined`（表示不限额）。

`worker.ts` 全文只有一行副作用导入：`import "@earendil-works/kiana-codemode/worker";`（打包时作为独立入口）。
`execute.lazy.ts`：`export const loadCodemodeExecutor = () => import("./execute.ts");`

**激活方式**（逐字文档）：`--tools`、`defaultTools` 设置，或 `setActiveTools()`；
当 MCP 工具只能经脚本访问时 MCP 扩展会自动激活它。

---

### 2.15 `src/extensions/tool-search/`（tool.ts 244 行 + index.ts 18 行）

#### 2.15.1 常量与类型

```ts
export const TOOL_SEARCH_TOOL_NAME = "tool_search";
export const DEFAULT_TOOL_SEARCH_LIMIT = 8;
export interface ToolSearchDocument { name: string; text: string }
export interface ToolSearchMatch { name: string; score: number }
export interface ToolRanker { rank(query, documents, limit): ToolSearchMatch[] }
```

#### 2.15.2 `STOP_WORDS`（逐字 22 项，顺序无关的 Set）

```ts
["a","an","and","are","as","at","be","by","for","from","in","is","it","of","on","or","that","the","this","to","with"]
```

#### 2.15.3 `stem(term)`（朴素单数化，顺序敏感）

```text
if (length > 4 && endsWith("ies"))                          → slice(0,-3) + "y"
if (length > 4 && /(ches|shes|sses|xes|zes)$/.test(term))   → slice(0,-2)
if (length > 3 && endsWith("s") && !endsWith("ss"))         → slice(0,-1)
else 原样
```

例：`issues`→`issue`、`searches`→`search`、`boxes`→`box`、`classes`→`class`、`status`→`status`。

#### 2.15.4 `tokenize(text)`（导出，逐字四步）

```ts
text.replace(/([a-z0-9])([A-Z])/g, "$1 $2")        // fooBar → foo Bar
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")     // HTTPServer → HTTP Server
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(term => term.length > 0 && !STOP_WORDS.has(term))
    .map(stem)
```

#### 2.15.5 `schemaText(schema, parts)`（内部）

```text
if (!isObject(schema)) return
if (typeof schema.description === "string") parts.push(schema.description)
if (isObject(schema.properties))
	for (const [name, property] of Object.entries(schema.properties)) { parts.push(name); schemaText(property, parts) }
schemaText(schema.items, parts)
for (const key of ["anyOf", "oneOf", "allOf"]) {
	const variants = schema[key]
	if (Array.isArray(variants)) for (const v of variants) schemaText(v, parts)
}
```

`isObject(value)`：`typeof === "object" && !== null && !Array.isArray`。
遍历顺序：`description` → 各属性（**键顺序**）→ `items` → `anyOf`/`oneOf`/`allOf`。

#### 2.15.6 `createToolSearchDocument(tool, namespace?)`（导出）

```ts
const parts = [tool.name, tool.name.replaceAll("_", " "), tool.description];
schemaText(tool.parameters, parts);
if (namespace) parts.push(namespace.name, namespace.description ?? "", namespace.instructions ?? "");
return { name: tool.name, text: parts.filter(part => part.trim()).join(" ") };
```

- 名称出现两次（原样 + 下划线换空格），因此 `mcp__docs__search` 也能被 `mcp docs search` 命中。
- 空/空白片段被丢弃，用**单空格**连接。

#### 2.15.7 `Bm25Ranker.rank(query, documents, limit)`

```text
queryTerms = [...new Set(tokenize(query))]                 // 去重，保留首次出现顺序
if (queryTerms.length === 0 || documents.length === 0 || limit <= 0) return []

termCounts[i] = Map<term, count>（对 documents[i].text 分词）
lengths[i] = 该 Map 值之和
averageLength = sum(lengths) / documents.length || 1       // 0 时用 1

idf[term] = Math.log(1 + (N - df + 0.5) / (df + 0.5))      // N = documents.length, df = 含该词的文档数

for each document i:
	score = 0
	for term in queryTerms:
		count = termCounts[i].get(term); if (!count) continue
		norm = k1 * (1 - b + b * lengths[i] / averageLength)
		score += idf[term] * (count * (k1 + 1)) / (count + norm)
	if (score > 0) matches.push({ name: document.name, score })

return matches.sort((a, b) => b.score - a.score).slice(0, limit)     // 排序稳定 → 同分保持文档顺序
```

默认 `k1 = 1.2`、`b = 0.75`（构造器可选覆盖）。

Rust 注意：`Array.prototype.sort` 在 V8 中**稳定**，Rust 需用 `sort_by`（`slice::sort_by` 稳定）并保持 NaN/相等比较行为一致。

#### 2.15.8 `toolSearchSchema`

```ts
Type.Object({
	query: Type.String({ description: "Search query for deferred tools." }),
	limit: Type.Optional(Type.Number({ description: `Maximum number of tools to return. Defaults to ${DEFAULT_TOOL_SEARCH_LIMIT}.` })),
})
```

去模板后：`Maximum number of tools to return. Defaults to 8.`

同时导出类型 `export type ToolSearchInput = Static<typeof toolSearchSchema>`（**与 schema 同形**：
`{ query: string; limit?: number }`）。Rust 侧对应 `ToolSearchInput { query: String, limit: Option<f64> }`
（`limit` 在 TS 里是 `number`，非整数/负数由 §2.15.10 的运行时校验拒绝，不是 schema 拒绝）。

#### 2.15.9 `ToolSearchToolDetails` / `ToolSearchToolOptions`

```ts
export interface ToolSearchResultTool { name: string; description: string }
export interface ToolSearchToolDetails { loaded: string[] }
export interface ToolSearchToolOptions {
	tools?: Pick<ExtensionAPI, "getAllTools" | "getActiveTools" | "setActiveTools">;
}
```

#### 2.15.10 `searchAndLoad(tools, query, limit)`（内部）

```ts
const active = tools.getActiveTools();
const candidates = tools.getAllTools().filter(tool => isSearchable(tool.exposure) && !active.includes(tool.name));
const documents = candidates.map(tool => createToolSearchDocument(tool, tool.namespace));
const matches = new Bm25Ranker().rank(query, documents, limit);
if (matches.length > 0) tools.setActiveTools([...active, ...matches.map(m => m.name)]);
return matches.map(m => ({ name: m.name, description: candidates.find(t => t.name === m.name)?.description ?? "" }));
```

- `isSearchable(exposure)`：`codemode | deferred`。
- 只搜**尚未激活**的候选。
- 一次 `setActiveTools` 批量激活（追加顺序 = 分数顺序），因此记录进 transcript 后 `/tree`、resume、fork 都能保留。
- 已在 active 中的工具**不会**被重复放入。

#### 2.15.11 `TOOL_SEARCH_DESCRIPTION`（逐字）

```
# Tool discovery

Searches over deferred tool metadata with BM25 and exposes matching tools for the next model call.

Some of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.
```

（不列出可搜工具或命名空间，故注册工具时描述不变。）

#### 2.15.12 `createToolSearchToolDefinition(options = {})`

```ts
return {
	name: "tool_search",
	label: "tool_search",
	description: TOOL_SEARCH_DESCRIPTION,
	promptSnippet: "Search for tools that are not loaded yet and load the matches",
	parameters: toolSearchSchema,
	exposure: "model-only",
	reachesTools: () => "all",
	async execute(_toolCallId, { query, limit }) {
		if (query.trim() === "") throw new Error("query must not be empty");
		const max = limit ?? 8;
		if (!Number.isInteger(max) || max <= 0) throw new Error("limit must be a positive integer");
		const tools = options.tools ? searchAndLoad(options.tools, query, max) : [];
		const text = tools.length === 0
			? "No matching tools found."
			: `Loaded ${tools.length} tool${tools.length === 1 ? "" : "s"}. They are available from your next call:\n${tools
					.map(tool => `- ${tool.name}: ${tool.description.trim().split(/\r?\n/)[0]}`).join("\n")}`;
		return { content: [{ type: "text", text }], details: { loaded: tools.map(t => t.name) } };
	},
};
```

逐字样例：

```
Loaded 2 tools. They are available from your next call:
- mcp__docs__search: Search the docs.
- mcp__docs__read: Read a doc page.
```

（每行 `<name>: <description 第一行 trim>`；`Loaded 1 tool.` 用单数。）

`index.ts`：

```ts
export function createToolSearchExtension(): ExtensionFactory {
	return (kiana) => { kiana.registerTool({ ...createToolSearchToolDefinition({ tools: kiana }), defaultActive: false }); };
}
export default createToolSearchExtension();
```

（`tool_search` 默认**不激活**；用 `--tools`、`defaultTools` 或 `setActiveTools()` 激活。）

---

### 2.16 `src/extensions/llama/client.ts`（348 行）

#### 2.16.1 类型

```ts
export type LlamaModelStatus = "unloaded" | "loading" | "loaded" | "downloading" | "sleeping";

export interface LlamaModelInfo {
	id: string;
	aliases?: string[];
	status: {
		value: LlamaModelStatus;
		args?: string[];
		failed?: boolean;
		exit_code?: number;
		progress?: Record<string, { done: number; total: number }>;
	};
	architecture?: { input_modalities?: string[]; output_modalities?: string[] };
	source?: string;
	meta?: { n_ctx?: number; n_ctx_train?: number; size?: number; ftype?: string };
}
export interface LlamaModelsResponse { data: LlamaModelInfo[]; object?: string }
export interface LlamaServerProps { models_autoload?: boolean; chat_template?: string }
export interface LlamaModelEvent { model: string; event: string; data?: unknown }
export interface LlamaProgress { message: string; ratio?: number; detail?: string }
```

#### 2.16.2 `errorMessage(payload, fallback)`（内部）

```text
payload 非对象/null → fallback
payload.error 非对象/null → fallback
payload.error.message 非空字符串 → 它
否则 → fallback
```

#### 2.16.3 `isModelInfo(value)`（内部）

`object && !null && typeof id === "string" && typeof status?.value === "string"`。

#### 2.16.4 `linkSignal(source, target)`

```ts
if (!source) return () => {};
if (source.aborted) { target.abort(source.reason); return () => {}; }
const abort = () => target.abort(source.reason);
source.addEventListener("abort", abort, { once: true });
return () => source.removeEventListener("abort", abort);
```

#### 2.16.5 `sleep(ms, signal)`

```text
signal 已中止 → reject(signal.reason ?? new Error("Cancelled"))
否则 setTimeout(ms) → resolve；中止 → clearTimeout 后 reject(signal.reason ?? new Error("Cancelled"))
```

#### 2.16.6 `parseLoadProgress(data)`（内部）

```text
progress = data.progress；非对象 → undefined
stage = typeof progress.current === "string" ? current : typeof progress.stage === "string" ? stage : undefined
stages = Array.isArray(progress.stages) ? stages.filter(x => typeof x === "string") : []
stageRatio = typeof progress.value === "number" ? clamp(value, 0, 1) : undefined
ratio = stageRatio
if (stage && stages.length > 0) { index = stages.indexOf(stage); if (index >= 0) ratio = (index + (stageRatio ?? 0)) / stages.length }
return { message: stage ? `Loading ${stage.replaceAll("_", " ")}` : "Loading model", ratio }
```

例：`stages = ["init","load_tensors"]`、`current = "load_tensors"`、`value = 0.5` → `ratio = (1 + 0.5)/2 = 0.75`，
`message = "Loading load tensors"`。

#### 2.16.7 `parseDownloadProgress(data)`（内部）

```text
nested = data.progress；files = (nested 是对象) ? nested : data
done = 0; total = 0
for (const value of Object.values(files)) {
	if (value 非对象/null) continue
	if (typeof value.done !== "number" || typeof value.total !== "number") continue
	done += value.done; total += value.total
}
if (total <= 0) return undefined
return { message: "Downloading model", ratio: done / total, detail: `${formatBytes(done)} / ${formatBytes(total)}` }
```

#### 2.16.8 `formatBytes(bytes)`（导出）

```text
bytes < 1024 → `${bytes} B`
units = ["KiB","MiB","GiB","TiB"]; value = bytes/1024; unit = "KiB"
for (index = 1; index < 4 && value >= 1024; index++) { value /= 1024; unit = units[index] }
format = value >= 10 ? toFixed(1) : toFixed(2)
return `${format} ${unit}`
```

例：`512 B`、`1.50 KiB`、`10.0 MiB`、`1.25 GiB`、`1023.99 KiB`。

#### 2.16.9 `normalizeLlamaServerUrl(value)`（导出）

```ts
const url = new URL(value.trim());
if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Server URL must use http or https");
url.hash = "";
url.search = "";
url.pathname = url.pathname.replace(/\/+$/u, "").replace(/\/v1$/u, "") || "/";
return url.toString().replace(/\/$/u, "");
```

关键行为（WHATWG URL 语义，必须完全一致）：

- `value.trim()` 后才解析；非法 URL 由 `new URL` 抛 `TypeError: Invalid URL`。
- 清空 hash 与 search。
- 路径先去**尾部所有** `/`，再在**结果**上匹配 `/v1$` 并去掉；若变成空串则设为 `"/"`。
- `url.toString()` 会给空路径补 `/`（如 `http://h:8080` → `http://h:8080/`），最后 `.replace(/\/$/,"")` 去掉。
- 结果示例：`http://127.0.0.1:8080`、`https://host/prefix`、`http://h:8080/` → `http://h:8080`。
- 注意 `http://h/v1/v1` 只去掉最后一个 `/v1` → `http://h/v1`；`http://h/v1/` → 先尾斜杠 → `http://h/v1` → 再去 → `http://h`。
- 默认端口/大小写/IDN 归一由 WHATWG URL 决定（`HTTP://H:80/x` → `http://h/x`）。
- Rust 需用 `url` crate（WHATWG 兼容）并逐一复刻上述顺序。

`llamaInferenceUrl(serverUrl)`：`` `${normalizeLlamaServerUrl(serverUrl)}/v1` ``。

#### 2.16.10 `class LlamaClient`

```ts
readonly serverUrl: string;      // normalizeLlamaServerUrl(serverUrl)
private readonly apiKey: string | undefined;
```

**`request(path, init = {})`**（私有）：

```text
headers = new Headers(init.headers)
if (init.body !== undefined) headers.set("Content-Type", "application/json")
if (apiKey) headers.set("Authorization", `Bearer ${apiKey}`)
timeout = AbortSignal.timeout(15_000)
signal = init.signal ? AbortSignal.any([init.signal, timeout]) : timeout
response = await fetch(`${serverUrl}${path}`, { ...init, headers, signal })
payload = await response.json()  // 解析失败 → undefined
if (!response.ok) throw new Error(errorMessage(payload, `llama.cpp returned HTTP ${response.status}`))
return payload
```

**方法**：

| 方法 | 请求 | 错误 |
|---|---|---|
| `list({ reload?, signal? })` | `GET /models` 或 `/models?reload=1` | payload 非对象或 `data` 非数组 → `llama.cpp returned an invalid model catalog`；任一条目不满足 `isModelInfo` → `Server is not running in llama.cpp router mode` |
| `props({ model?, signal? })` | `GET /props` 或 `GET /props?model=<m>&autoload=false`（`new URLSearchParams` 编码） | 非对象 → `{}`；只保留 boolean `models_autoload` 与 string `chat_template` |
| `load(model, signal?)` | `POST /models/load`，body `{"model":"<id>"}` | 走 request |
| `unload(model, signal?)` | `POST /models/unload`，body 同上 | 走 request |
| `unloadAndWait(model, signal?)` | 先 unload，然后循环 `list()` 找该 id；不存在或 `status.value === "unloaded"` → 返回；否则 `sleep(100, signal)` | — |
| `download(model, signal?)` | `POST /models`，body 同上 | — |
| `watch(onEvent, signal?)` | `GET /models/sse` 流 | 非 ok 或无 body → `llama.cpp SSE returned HTTP ${status}` |
| `loadAndWait(model, onProgress, signal?)` | 见 §2.16.11 | `Model failed to load` / `Model exited with code ${n}` |
| `downloadAndWait(model, onProgress, signal?)` | 见 §2.16.12 | `Download failed` 等 |

`props` 的查询串：`new URLSearchParams({ model, autoload: "false" })` → `model=<encoded>&autoload=false`（注意 `false` 无引号）。

**`watch` 的 SSE 解析**（逐字算法，Rust 必须一致）：

```text
reader = response.body.getReader(); decoder = new TextDecoder(); buffer = ""
loop:
	chunk = await reader.read(); if (chunk.done) break
	buffer += decoder.decode(chunk.value, { stream: true }).replaceAll("\r\n", "\n")
	boundary = buffer.indexOf("\n\n")
	while (boundary >= 0) {
		frame = buffer.slice(0, boundary); buffer = buffer.slice(boundary + 2)
		data = frame.split("\n")
			.filter(line => line.startsWith("data:"))
			.map(line => line.slice(5).trimStart())
			.join("\n")
		if (data) try { event = JSON.parse(data); if (event && typeof event.model === "string" && typeof event.event === "string") onEvent(event) } catch {}
		boundary = buffer.indexOf("\n\n")
	}
```

要点：`\r\n` 先归一为 `\n`；只有 `data:` 行参与；**冒号后一个空格可有可无**（`trimStart()`）；
多行 `data:` 用 `\n` 连接；JSON 解析失败或字段不全**静默丢弃**（目录轮询仍是权威）。

#### 2.16.11 `loadAndWait(model, onProgress, signal?)`

```text
watcher = new AbortController(); unlink = linkSignal(signal, watcher)
eventLoaded = false; eventError = undefined
void watch(event => {
	if (event.model !== model) return
	if (event.event !== "model_status" && event.event !== "status_change") return
	data = event.data
	if (data?.status === "loaded") eventLoaded = true
	if (data?.status === "unloaded") eventError = "Model failed to load"
	progress = parseLoadProgress(event.data); if (progress) onProgress(progress)
}, watcher.signal).catch(() => {})
try {
	await load(model, signal)
	onProgress({ message: "Loading model" })
	loop:
		if (signal?.aborted) throw signal.reason ?? new Error("Cancelled")
		entry = (await list({ signal })).find(m => m.id === model)
		if (entry?.status.value === "loaded") return entry
		if (eventLoaded && !entry) return { id: model, status: { value: "loaded" } }
		if (entry?.status.failed || eventError)
			throw new Error(entry?.status.exit_code === undefined
				? (eventError ?? "Model failed to load")
				: `Model exited with code ${entry.status.exit_code}`)
		await sleep(250, signal)
} finally { unlink(); watcher.abort() }
```

轮询间隔 **250 ms**；`watcher` 在 `finally` 中止。

#### 2.16.12 `downloadAndWait(model, onProgress, signal?)`

```text
watcher = new AbortController(); unlink = linkSignal(signal, watcher)
finished = false; failure = undefined; sawDownloading = false; polls = 0
void watch(event => {
	if (event.model !== model) return
	if (event.event === "download_finished") finished = true
	if (event.event === "download_failed") failure = errorMessage(event.data, "Download failed")
	if (event.event === "download_progress") {
		sawDownloading = true
		progress = parseDownloadProgress(event.data); if (progress) onProgress(progress)
	}
}, watcher.signal).catch(() => {})
try {
	await download(model, signal)
	onProgress({ message: "Downloading model" })
	loop:
		if (signal?.aborted) throw signal.reason ?? new Error("Cancelled")
		if (failure) throw new Error(failure)
		models = await list({ signal }); polls++
		entry = models.find(m => m.id === model)
		if (entry?.status.value === "downloading") {
			sawDownloading = true
			progress = parseDownloadProgress(entry.status.progress); if (progress) onProgress(progress)
		} else if (finished || (entry && (sawDownloading || polls >= 2))) {
			return this.list({ reload: true, signal })
		}
		await sleep(500, signal)
} finally { unlink(); watcher.abort() }
```

轮询间隔 **500 ms**。返回 **reload 后的**完整目录。结束条件：SSE 报 `download_finished`，
或该条目存在且（曾见到 downloading 或已轮询 ≥ 2 次）。

---

### 2.17 `src/extensions/llama/provider.ts`（299 行）

#### 2.17.1 常量

```ts
export const LLAMA_PROVIDER_ID = "llama.cpp";
export const DEFAULT_LLAMA_SERVER_URL = "http://127.0.0.1:8080";
```

#### 2.17.2 服务器 URL 解析

```ts
function credentialServerUrl(credential) {
	const value = credential?.env?.LLAMA_BASE_URL;
	return typeof value === "string" && value.trim() ? normalizeLlamaServerUrl(value) : undefined;
}
async function resolveServerUrl(ctx, credential) {
	const configured = credentialServerUrl(credential) ?? (await ctx.env("LLAMA_BASE_URL"))?.trim();
	return configured ? normalizeLlamaServerUrl(configured) : undefined;
}
```

优先级：凭据 `env.LLAMA_BASE_URL` → 环境变量 `LLAMA_BASE_URL` → `undefined`（不回落默认值）。

#### 2.17.3 可选性判定

```ts
function modelIsSelectable(model, routerAutoload): boolean {
	if (model.status.value === "loaded") return true;
	if (model.status.value === "sleeping") return true;      // 请求会自动唤醒
	return routerAutoload && model.status.value === "unloaded" && !model.status.failed && model.source === "preset";
}
async function routerAutoloadEnabled(client, catalog, signal): Promise<boolean> {
	if (!catalog.some(m => m.status.value === "unloaded" && m.source === "preset")) return false;
	try { return (await client.props({ signal })).models_autoload === true } catch { return false }
}
```

注意：只有**存在未加载 preset** 时才去查 `/props`（避免多余请求）。

#### 2.17.4 上下文窗口

```ts
function configuredContextWindow(model) {
	const args = model.status.args ?? [];
	for (let index = 0; index < args.length - 1; index++) {
		const flag = args[index];
		if (flag !== "--ctx-size" && flag !== "-c" && flag !== "-ctx") continue;
		const contextWindow = Number(args[index + 1]);
		if (Number.isSafeInteger(contextWindow) && contextWindow > 0) return contextWindow;
	}
	return undefined;
}
function contextWindowOf(model, cachedContextWindow?) {
	const runtimeContextWindow = model.meta?.n_ctx;
	if (runtimeContextWindow && runtimeContextWindow > 0) return runtimeContextWindow;
	const configuredContext = configuredContextWindow(model);
	if (configuredContext) return configuredContext;
	if (cachedContextWindow && cachedContextWindow > 0) return cachedContextWindow;
	const trainingContextWindow = model.meta?.n_ctx_train;
	return trainingContextWindow && trainingContextWindow > 0 ? trainingContextWindow : 128000;
}
```

优先级：`meta.n_ctx` → `status.args` 中的 `--ctx-size|-c|-ctx` → 旧缓存 → `meta.n_ctx_train` → **128000**。
只取**第一个**匹配的合法标志（循环 `args.length - 1`，跳过最后一个孤立参数）。

#### 2.17.5 `toKianaClassifierModel(model, serverUrl, cachedContextWindow?)`

```ts
const decision = isDecisionModel(model);
return {
	type: "classifier",
	id: model.id,
	name: model.id,
	api: decision ? "typesafe-system-one" : "llama-cpp-classify",
	provider: "llama.cpp",
	baseUrl: decision ? llamaInferenceUrl(serverUrl) : serverUrl,     // decision 用 .../v1，其它用根
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: contextWindowOf(model, cachedContextWindow),
};
```

`isDecisionModel(model)`：`model.architecture?.output_modalities?.includes("decisions") === true`。
`isChatModel(model)`：`!isDecisionModel(model) || output_modalities 含 "text"`。

#### 2.17.6 `toKianaModel(model, serverUrl, props?, cachedContextWindow?)`

```ts
const contextWindow = contextWindowOf(model, cachedContextWindow);
const reasoning = props?.chat_template?.includes("enable_thinking") === true;
return {
	id: model.id,
	name: model.id,
	api: "openai-completions",
	provider: "llama.cpp",
	baseUrl: llamaInferenceUrl(serverUrl),
	reasoning,
	...(reasoning && { thinkingLevelMap: { off: "off", minimal: null, low: null, medium: "medium", high: null, xhigh: null } }),
	input: model.architecture?.input_modalities?.includes("image") ? ["text", "image"] : ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow,
	maxTokens: contextWindow,
	compat: {
		supportsStore: false,
		supportsDeveloperRole: false,
		supportsReasoningEffort: false,
		supportsUsageInStreaming: true,
		supportsStrictMode: false,
		maxTokensField: "max_tokens",
		...(reasoning && { thinkingFormat: "qwen-chat-template" }),
	},
};
```

思考支持只在**已加载**模型的 `chat_template` 含 `enable_thinking` 时开启；
开启时 `thinkingLevelMap` 仅 `off` 与 `medium` 有映射，其余为 `null`（不可用）。

#### 2.17.7 `LlamaProviderController` 与 `createLlamaProvider()`

```ts
export interface LlamaProviderController {
	provider: Provider<"openai-completions">;
	setCatalog(models: readonly LlamaModelInfo[], serverUrl: string, options?: { routerAutoload?: boolean }): void;
}
```

`setCatalog`：

```ts
const selectable = catalog.filter(model => modelIsSelectable(model, options.routerAutoload === true));
models = selectable.filter(isChatModel).map(model => toKianaModel(model, serverUrl));
classifiers = selectable.map(model => toKianaClassifierModel(model, serverUrl));
```

（`setCatalog` **不传** props，因此如此设置的模型一律 `reasoning: false`。）

`provider` 字段：

| 字段 | 值/行为 |
|---|---|
| `id` | `"llama.cpp"` |
| `name` | `"llama.cpp"` |
| `baseUrl` | `llamaInferenceUrl("http://127.0.0.1:8080")` = `http://127.0.0.1:8080/v1` |
| `auth.apiKey.name` | `"llama.cpp server"` |
| `auth.apiKey.login(interaction)` | 见下 |
| `auth.apiKey.check({ctx,credential})` | 有 serverUrl → `{ type: "api_key", source: credential ? "stored credential" : "LLAMA_BASE_URL" }`，否则 `undefined` |
| `auth.apiKey.resolve({ctx,credential})` | 见下 |
| `getModels()` | 当前 chat 模型数组 |
| `getAllModels()` | `[...models, ...classifiers]` |
| `refreshModels(context)` | 见 §2.17.8 |
| `stream(model, context, options)` | `stream(model, context, options)`（kiana-ai compat） |
| `streamSimple(...)` | `streamSimple(...)` |
| `classify(model, context, options)` | `model.api === "typesafe-system-one" ? decisionClassifier : fallbackClassifier` 的 `classify` |

`login`（逐字提示）：

```ts
const enteredUrl = await interaction.prompt({
	type: "text",
	message: "llama.cpp server URL",
	placeholder: process.env.LLAMA_BASE_URL ?? DEFAULT_LLAMA_SERVER_URL,
});
const serverUrl = normalizeLlamaServerUrl(enteredUrl.trim() || process.env.LLAMA_BASE_URL || DEFAULT_LLAMA_SERVER_URL);
const apiKey = (await interaction.prompt({ type: "secret", message: "API key (optional)" })).trim();
await new LlamaClient(serverUrl, apiKey || undefined).list({ signal: interaction.signal });   // 验证连通
return { type: "api_key", key: apiKey || undefined, env: { LLAMA_BASE_URL: serverUrl } };
```

`resolve`：

```ts
const serverUrl = await resolveServerUrl(ctx, credential);
if (!serverUrl) return undefined;
const apiKey = credential?.key ?? (await ctx.env("LLAMA_API_KEY")) ?? "local";
return {
	auth: { apiKey, baseUrl: llamaInferenceUrl(serverUrl) },
	env: { ...credential?.env, LLAMA_BASE_URL: serverUrl },
	source: credential ? "stored credential" : "LLAMA_BASE_URL",
};
```

- API key 回退顺序：凭据 key → `LLAMA_API_KEY` → 字面量 `"local"`。
- `baseUrl` 始终是 `.../v1`（OpenAI 兼容端点）。

#### 2.17.8 `refreshModels(context)`

```text
cachedContextWindows = new Map()
if (context.stored) {
	stored = context.stored.models.filter(m => m.provider === "llama.cpp")
	restored = stored.filter(isModelType(m, "chat") && m.api === "openai-completions")
	restoredClassifiers = stored.filter(isLlamaClassifierModel)
	for (model of [...restored, ...restoredClassifiers]) cachedContextWindows.set(model.id, model.contextWindow)
	if (!(await context.publish({ update: () => { models = restored; classifiers = restoredClassifiers } }))) return
}
if (!context.allowNetwork || context.signal.aborted || context.credential?.type !== "api_key") return
serverUrl = credentialServerUrl(context.credential); if (!serverUrl) return
client = new LlamaClient(serverUrl, context.credential.key)
catalog = await client.list({ signal: context.signal }); if (aborted) return
routerAutoload = await routerAutoloadEnabled(client, catalog, signal); if (aborted) return
selectable = catalog.filter(m => modelIsSelectable(m, routerAutoload))
refreshed = await Promise.all(selectable.filter(isChatModel).map(async model => {
	cachedContextWindow = cachedContextWindows.get(model.id)
	if (model.status.value !== "loaded") return toKianaModel(model, serverUrl, undefined, cachedContextWindow)
	props = await client.props({ model: model.id, signal })
	return toKianaModel(model, serverUrl, props, cachedContextWindow)
}))
refreshedClassifiers = selectable.map(m => toKianaClassifierModel(m, serverUrl, cachedContextWindows.get(m.id)))
if (aborted) return
await context.publish({
	persist: { models: [...refreshed, ...refreshedClassifiers], checkedAt: Date.now() },
	update: () => { models = refreshed; classifiers = refreshedClassifiers },
})
```

逐字注释语义：只有**已加载**模型能在无副作用的情况下暴露 chat template；未加载 autoload preset 需要加载、
查询 sleeping 模型可能唤醒它们——这些模型在加载并被后续刷新发现前**没有**思考支持；decision 模型不需要模板。

`isLlamaClassifierModel(model)`：`isModelType(model,"classifier") && (api === "llama-cpp-classify" || api === "typesafe-system-one")`。

---

### 2.18 `src/extensions/llama/huggingface.ts`（158 行）

#### 2.18.1 常量

```ts
const DEFAULT_HUGGING_FACE_URL = "https://huggingface.co";
const QUANTIZATION_PATTERN =
	/(?:^|[-_.])((?:UD-)?(?:IQ\d(?:_[A-Z0-9]+)+|Q\d(?:_[A-Z0-9]+)+|BF16|F16|F32|MXFP\d(?:_[A-Z0-9]+)*))$/iu;
const SHARD_SUFFIX_PATTERN = /-\d{5}-of-\d{5}$/u;
```

- `QUANTIZATION_PATTERN` 有 `i` 与 `u` 标志，锚定在**末尾**，前面必须是开头或 `-`/`_`/`.`。
- 分组 1 = 量化名（如 `Q4_K_M`、`UD-Q4_K_XL`、`IQ2_M`、`BF16`、`MXFP4`）。
- `SHARD_SUFFIX_PATTERN` 去掉 `-00001-of-00003` 之类分片后缀。

#### 2.18.2 类型

```ts
export interface HuggingFaceModel { id: string; downloads: number }
export interface HuggingFaceQuantization { name: string; size?: number }
export interface HuggingFaceModelDetails { id: string; gated: false | "auto" | "manual"; quantizations: HuggingFaceQuantization[] }
```

#### 2.18.3 `payloadError(payload, fallback)` / `parseRateLimitDelay(value)`

```ts
payloadError: payload.error 为**非空字符串** → 它；否则 fallback
parseRateLimitDelay: value?.match(/(?:^|;)t=(\d+)/u) → Number(match[1]) 或 undefined
```

#### 2.18.4 `readToken(path)` / `findHuggingFaceToken(env = process.env)`

```ts
readToken: (await readFile(path, "utf8")).trim() → 非空则返回；任何异常 → undefined
findHuggingFaceToken:
	fromEnvironment = env.HF_TOKEN?.trim(); if (fromEnvironment) return 它
	paths = [
		env.HF_TOKEN_PATH,
		env.HF_HOME ? join(env.HF_HOME, "token") : undefined,
		env.XDG_CACHE_HOME ? join(env.XDG_CACHE_HOME, "huggingface", "token") : undefined,
		join(homedir(), ".cache", "huggingface", "token"),
	].filter(Boolean)
	for (const path of new Set(paths)) { token = await readToken(path); if (token) return token }
	return undefined
```

顺序与去重（`new Set`）都要保持。

#### 2.18.5 `class HuggingFaceClient`

```ts
constructor(token?: string, baseUrl = "https://huggingface.co") {
	this.token = token;
	this.baseUrl = baseUrl.replace(/\/+$/u, "");
}
```

`request(path, signal?)`：

```text
headers = new Headers(); if (token) headers.set("Authorization", `Bearer ${token}`)
timeout = AbortSignal.timeout(15_000)
response = await fetch(`${baseUrl}${path}`, { headers, signal: signal ? AbortSignal.any([signal, timeout]) : timeout })
payload = await response.json()  // 失败 → undefined
if (!response.ok) {
	if (response.status === 429) {
		delay = Number(response.headers.get("retry-after")) || parseRateLimitDelay(response.headers.get("ratelimit"))
		throw new Error(delay ? `Hugging Face rate limit reached; retry in ${delay}s` : "Hugging Face rate limit reached")
	}
	throw new Error(payloadError(payload, `Hugging Face returned HTTP ${response.status}`))
}
return payload
```

注意：`Number("garbage") || parse...` → `NaN` 为**假值**，因此回落到 `ratelimit` 头解析。

`search(query, signal?)`：

```text
params = new URLSearchParams({ search: query, filter: "gguf", sort: "downloads", direction: "-1", limit: "20" })
payload = await request(`/api/models?${params}`)
payload 非数组 → throw new Error("Hugging Face returned invalid search results")
payload.flatMap(value => value 非对象/null/无 string id → []
	→ [{ id, downloads: typeof downloads === "number" ? downloads : 0 }])
```

查询串顺序（`URLSearchParams` 保持插入序）：`search=<q>&filter=gguf&sort=downloads&direction=-1&limit=20`。

`details(id, signal?)`：

```text
encodedId = id.split("/").map(encodeURIComponent).join("/")
payload = await request(`/api/models/${encodedId}?blobs=true`)
payload 非对象/null → throw new Error("Hugging Face returned invalid model details")
sizes = Map<quantization, { total: number; complete: boolean }>
if (Array.isArray(payload.siblings)) for (const value of payload.siblings) {
	if (value 非对象/null) continue
	if (typeof value.rfilename !== "string" || !rfilename.toLowerCase().endsWith(".gguf")) continue
	filename = rfilename.split("/").at(-1)
	if (filename.toLowerCase().startsWith("mmproj")) continue
	stem = filename.slice(0, -5).replace(SHARD_SUFFIX_PATTERN, "")
	quantization = stem.match(QUANTIZATION_PATTERN)?.[1]?.toUpperCase()
	if (!quantization) continue
	current = sizes.get(quantization) ?? { total: 0, complete: true }
	if (typeof value.size === "number") current.total += value.size
	else current.complete = false
	sizes.set(quantization, current)
}
quantizations = [...sizes]
	.map(([name, size]) => ({ name, size: size.complete ? size.total : undefined }))
	.sort((left, right) => {
		if (left.name === "Q4_K_M") return -1;
		if (right.name === "Q4_K_M") return 1;
		return ((left.size ?? MAX_SAFE_INTEGER) - (right.size ?? MAX_SAFE_INTEGER)) || left.name.localeCompare(right.name);
	})
return {
	id: typeof payload.id === "string" ? payload.id : id,
	gated: payload.gated === "auto" || payload.gated === "manual" ? payload.gated : false,
	quantizations,
}
```

排序语义：`Q4_K_M` 恒在最前（两侧都命中时 `-1` 优先，实际不会发生）；
其余按大小升序（缺失大小视为 `Number.MAX_SAFE_INTEGER`），大小相同按名称 `localeCompare`。

---

### 2.19 `src/extensions/llama/index.ts`（230 行）

#### 2.19.1 小工具

```ts
function modelIsLoaded(model) { return model.status.value === "loaded" || model.status.value === "sleeping"; }
function isConnectionError(error) {
	if (!(error instanceof Error)) return false;
	const message = `${error.name} ${error.message}`.toLowerCase();
	return message.includes("fetch failed") || message.includes("timeout") || message.includes("network");
}
function connectionErrorMessage(error) { return isConnectionError(error) ? "Could not connect to the server." : error instanceof Error ? error.message : String(error); }
function parseHuggingFaceModel(value) {
	const colon = value.indexOf(":", value.indexOf("/") + 1);
	return colon < 0 ? { repository: value } : { repository: value.slice(0, colon), quantization: value.slice(colon + 1) };
}
```

`parseHuggingFaceModel` 只在**第一个 `/` 之后**找 `:`，因此 `owner/repo:Q4_K_M` → `{repository:"owner/repo", quantization:"Q4_K_M"}`；
`repo`（无 `/`）时 `indexOf("/",0) = -1`，从 0 起找 `:`。`value = "a:b/c"` → 从索引 1 起无 `:` → 不切分。

`configuredClient(ctx)`：

```ts
const result = await ctx.modelRegistry.getProviderAuth("llama.cpp");
if (!result) { ctx.ui.notify(`Configure llama.cpp with /login llama.cpp`, "warning"); return undefined; }
const configuredUrl = result.env?.LLAMA_BASE_URL;
const serverUrl = normalizeLlamaServerUrl(
	typeof configuredUrl === "string" && configuredUrl ? configuredUrl : (result.auth.baseUrl ?? ""));
return new LlamaClient(serverUrl, result.auth.apiKey);
```

逐字提示：`Configure llama.cpp with /login llama.cpp`。

#### 2.19.2 扩展主体

```ts
export default function llamaExtension(kiana: ExtensionAPI): void {
	const provider = createLlamaProvider();
	kiana.registerProvider(provider.provider);
	...
	kiana.registerCommand("llama", { description: "Manage llama.cpp router models", handler });
}
```

**`syncCatalog(ctx, client, catalog?)`**：

```ts
const signal = AbortSignal.timeout(15_000);
const current = catalog ?? (await client.list({ signal }));
provider.setCatalog(current, client.serverUrl);
const result = await ctx.modelRegistry.refresh({ providers: ["llama.cpp"], allowNetwork: true, signal });
if (result.aborted) throw new Error("Model catalog refresh timed out.");
const refreshError = result.errors.get("llama.cpp");
if (refreshError) throw refreshError;
return current;
```

`allowNetwork: true` 的逐字注释理由：`/llama` 已联系过配置的服务器，故即使在 `KIANA_OFFLINE` 下也保持联网刷新。
逐字错误：`Model catalog refresh timed out.`

**`loadModel(ctx, ui, client, catalog, target)`**：

```text
loaded = catalog.filter(m => m.id !== target.id && modelIsLoaded(m))
replace = false
if (loaded.length > 0) {
	choice = await ui.select(`${loaded.length} model${loaded.length === 1 ? " is" : "s are"} loaded`,
		["Unload all and load", "Keep loaded and load", "Cancel"])
	if (!choice || choice === "Cancel") return
	replace = choice === "Unload all and load"
}
restoreLoaded = async () => {
	ctx.ui.notify("Restoring previously loaded models")
	for (const model of loaded) await client.loadAndWait(model.id, () => {})
	await syncCatalog(ctx, client)
}
if (replace) for (const model of loaded) await client.unloadAndWait(model.id)
try {
	result = await runWithProgress(ui, {
		title: "Loading model", model: target.id, initialMessage: "Starting…",
		cancelTitle: "Stop loading?", cancelMessage: target.id,
		run: (signal, update) => client.loadAndWait(target.id, update, signal),
		cancel: () => client.unload(target.id),
	})
	if (result.cancelled) { if (replace) await restoreLoaded(); return }
	refreshed = await syncCatalog(ctx, client)
	loadedModel = refreshed.find(m => m.id === target.id)
	ctx.ui.notify(loadedModel?.status.value === "loaded" ? `Loaded ${target.id}` : `Load started for ${target.id}`)
} catch (error) {
	if (replace) { try { await restoreLoaded() } catch { /* 保留原始加载错误 */ } }
	throw error
}
```

逐字串：`{n} model is loaded` / `{n} models are loaded`、`Unload all and load`、`Keep loaded and load`、`Cancel`、
`Restoring previously loaded models`、`Loaded {id}`、`Load started for {id}`。

**`unloadModel(ctx, ui, client, model)`**：

```text
if (!(await ui.confirm("Unload model?", model.id))) return
await client.unloadAndWait(model.id)
await syncCatalog(ctx, client)
ctx.ui.notify(`Unloaded ${model.id}`)
```

**`downloadModel(ctx, ui, client)`**：

```text
huggingFace = new HuggingFaceClient(await findHuggingFaceToken())
selected = await ui.searchModels((query, signal) => huggingFace.search(query, signal)); if (!selected) return
parsed = parseHuggingFaceModel(selected)
ui.showStatus("Loading model details", parsed.repository)
details = await huggingFace.details(parsed.repository)
if (details.gated) {
	approval = details.gated === "manual" ? "Manual approval is required" : "Accept the access terms"
	choice = await ui.select(
		`Hugging Face access required\n${details.id}\n\n${approval} at:\nhttps://huggingface.co/${details.id}\n\nThe llama.cpp server needs HF_TOKEN with access.`,
		["Continue", "Back"])
	if (choice !== "Continue") return
}
quantization = parsed.quantization
if (!quantization && details.quantizations.length > 0) {
	options = details.quantizations.map(entry => {
		detail = [entry.size === undefined ? undefined : formatBytes(entry.size),
		          entry.name === "Q4_K_M" ? "recommended" : undefined].filter(Boolean).join(" · ")
		return detail ? `${entry.name} · ${detail}` : entry.name
	})
	choice = await ui.select(`Select quantization\n${details.id}`, options); if (!choice) return
	quantization = details.quantizations[options.indexOf(choice)]?.name; if (!quantization) return
}
model = quantization ? `${details.id}:${quantization}` : details.id
result = await runWithProgress(ui, {
	title: "Downloading model", model, initialMessage: "Starting…",
	cancelTitle: "Stop download?", cancelMessage: model,
	run: (signal, update) => client.downloadAndWait(model, update, signal),
	cancel: () => client.unload(model),
})
if (result.cancelled) return
await syncCatalog(ctx, client, result.value)
ctx.ui.notify(`Downloaded ${model}`)
```

逐字：`Loading model details`、`Manual approval is required`、`Accept the access terms`、
授权对话框全文（见上，含 `https://huggingface.co/{id}`）、`Continue`、`Back`、`Select quantization\n{id}`、
量化项 `` `${name} · ${size}` ``（Q4_K_M 追加 ` · recommended`）、`Downloaded {model}`。
**取消下载时调用的是 `unload`**（llama.cpp 用它中止下载）。

#### 2.19.3 `/llama` 命令

```ts
kiana.registerCommand("llama", {
	description: "Manage llama.cpp router models",
	handler: async (_args, ctx) => {
		if (ctx.mode !== "tui") { ctx.ui.notify("/llama is available in interactive mode", "warning"); return; }
		const client = await configuredClient(ctx);
		if (!client) return;
		await showLlamaUi(ctx, async (ui) => {
			const readCatalog = async () => {
				while (true) {
					try { return await syncCatalog(ctx, client) }
					catch (error) {
						if ((await ui.connectionError(client.serverUrl, connectionErrorMessage(error))) === "close") return undefined
					}
				}
			};
			let catalog = await readCatalog();
			if (!catalog) return;
			while (true) {
				const action = await ui.showModels(client.serverUrl, catalog);
				if (action.type === "close") return;
				let actionError;
				try {
					if (action.type === "download") await downloadModel(ctx, ui, client);
					else if (modelIsLoaded(action.model)) await unloadModel(ctx, ui, client, action.model);
					else if (action.model.status.value === "unloaded") await loadModel(ctx, ui, client, catalog, action.model);
					else ctx.ui.notify(`${action.model.id} is ${action.model.status.value}`, "warning");
				} catch (error) { actionError = error }
				const refreshed = await readCatalog();
				if (!refreshed) return;
				catalog = refreshed;
				if (actionError && !isConnectionError(actionError))
					ctx.ui.notify(actionError instanceof Error ? actionError.message : String(actionError), "error");
			}
		});
	},
});
```

要点：

- 非 TUI → `"/llama is available in interactive mode"`（warning），不进入界面。
- `readCatalog` 是**无限重试循环**：连接错误交给 `ui.connectionError`，用户选 Close 才返回 `undefined`。
- 动作分发：`download` → 下载；`loaded|sleeping` → 卸载；`unloaded` → 加载；
  其它状态（`loading`/`downloading`）→ 通知 `` `${id} is ${status}` ``（warning）。
- 每个动作后**重新读取**目录；连接错误不再二次通知（`ui.connectionError` 已处理）。

---

### 2.20 `src/extensions/llama/ui.ts`（542 行）

#### 2.20.1 常量与类型

```ts
const DOWNLOAD_VALUE = "\0download";       // NUL 前缀，不可能与模型 id 冲突
export type LlamaManagerAction = { type: "model"; model: LlamaModelInfo } | { type: "download" } | { type: "close" };
interface ProgressState extends LlamaProgress { title: string; model: string }
```

#### 2.20.2 `contextLabel(model)`

```ts
const context = model.meta?.n_ctx ?? model.meta?.n_ctx_train;
if (context) return context >= 1000 ? `${Math.round(context / 1000)}k` : String(context);
const args = model.status.args ?? [];
for (let index = 0; index < args.length - 1; index++) {
	if (args[index] !== "--ctx-size" && args[index] !== "-c" && args[index] !== "-ctx") continue;
	const value = Number(args[index + 1]);
	if (Number.isFinite(value) && value > 0) return value >= 1000 ? `${Math.round(value / 1000)}k` : String(value);
}
return undefined;
```

注意与 `provider.configuredContextWindow` 的差别：这里接受**任意有限正数**（含小数），且先看 `meta`。

#### 2.20.3 `modelDescription(model)`

```ts
const details = [];
const loaded = model.status.value === "loaded" || model.status.value === "sleeping";
if (loaded) details.push("loaded");
else if (model.status.value !== "unloaded") details.push(model.status.value);
const context = loaded ? contextLabel(model) : undefined;
if (context) details.push(`${context} context`);
return details.join(" · ");
```

例：已加载 → `loaded · 32k context`；加载中 → `loading`；未加载 → `""`。

#### 2.20.4 `selectTheme(theme)` / `frame(...)`

```ts
selectTheme = {
	selectedPrefix: t => theme.fg("accent", t),
	selectedText: t => theme.fg("accent", t),
	description: t => theme.fg("muted", t),
	scrollInfo: t => theme.fg("dim", t),
	noMatch: t => theme.fg("warning", t),
}
frame(theme, title, body, footer?) = Container[
	DynamicBorder(accent),
	Text(accent(bold(title)), 1, 0),        // paddingLeft=1, paddingTop=0
	...body,
	...(footer ? [Spacer(1), Text(dim(footer), 1, 0)] : []),
	DynamicBorder(accent),
]
```

`Text(text, 1, 0)` → `paddingX=1`（左右各 1），`paddingY=0`。

#### 2.20.5 `LlamaUi` 接口

```ts
export interface LlamaUi {
	showModels(serverUrl: string, models: LlamaModelInfo[]): Promise<LlamaManagerAction>;
	select(title: string, options: string[]): Promise<string | undefined>;
	confirm(title: string, message: string): Promise<boolean>;
	connectionError(serverUrl: string, message: string): Promise<"retry" | "close">;
	searchModels(search: (query: string, signal: AbortSignal) => Promise<HuggingFaceModel[]>): Promise<string | undefined>;
	showStatus(title: string, message: string): void;
	progress(state: ProgressState): Promise<void>;
	updateProgress(state: ProgressState): void;
}
```

#### 2.20.6 `compactCount(value)`

```ts
if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
if (value >= 1_000) return `${(value / 1_000).toFixed(value >= 100_000 ? 0 : 1)}k`;
return String(value);
```

例：`999`、`1.5k`、`100k`、`1.2M`、`10M`。

#### 2.20.7 `class HuggingFaceSearch`（`Container & Focusable`）

状态：`results`、`filteredResults`、`selectedIndex`、`query`、`status`（初值 `"Type at least 2 characters"`）、
`debounce`、`request`、`closed`、`_focused`；共享 `cache: Map<string, HuggingFaceModel[]>`（键为**小写查询**）。

构造时加入：

```ts
this.addChild(new Text(theme.fg("dim", "Model name or owner/repository[:quant]"), 1, 0));
this.addChild(this.input);                       // kiana-tui Input
this.addChild(new Spacer(1));
this.addChild(this.resultsContainer);
this.updateResults();
```

`updateResults()`：

```text
maxVisible = 10
start = max(0, min(selectedIndex - floor(10/2), filteredResults.length - maxVisible))
end = min(start + maxVisible, filteredResults.length)
for (index = start; index < end; index++) {
	prefix = index === selectedIndex ? "→ " : "  "
	details = `${compactCount(model.downloads)} downloads`
	选中行：theme.fg("accent", `${prefix}${model.id}  ${details}`)          // id 与 details 之间两个空格
	其它行：`${prefix}${model.id}${theme.fg("muted", `  ${details}`)}`
}
if (start > 0 || end < filteredResults.length)
	addChild(Text(theme.fg("dim", `  (${selectedIndex + 1}/${filteredResults.length})`), 0, 0))
if (filteredResults.length === 0) addChild(Text(theme.fg("dim", `  ${status}`), 0, 0))
else if (status === "Searching Hugging Face…") addChild(Text(theme.fg("dim", `  ${status}`), 0, 0))
tui.requestRender()
```

`filterResults()`：有 `query` → `fuzzyFilter(results, query, m => m.id)` 取 id 集合，再按 `results` 原序过滤；
无 query → 全部。随后 `selectedIndex = min(selectedIndex, max(0, len-1))` 并 `updateResults()`。

`scheduleSearch()`：

```text
clearTimeout(debounce); request?.abort(); request = undefined
if (query.length < 2) { status = "Type at least 2 characters"; filterResults(); return }
cached = cache.get(query.toLowerCase())
if (cached) { results = cached; status = cached.length === 0 ? "No GGUF models found" : ""; filterResults(); return }
status = "Searching Hugging Face…"; filterResults()
debounce = setTimeout(() => void runSearch(query), 500)
```

`runSearch(query)`：

```text
request = new AbortController(); this.request = request
try {
	results = await search(query, request.signal)
	cache.set(query.toLowerCase(), results)
	if (closed || request.signal.aborted || this.query !== query) return
	this.results = results; selectedIndex = 0
	status = results.length === 0 ? "No GGUF models found" : ""
	filterResults()
} catch (error) {
	if (closed || request.signal.aborted || this.query !== query) return
	this.results = []; status = error message; filterResults()
} finally { if (this.request === request) this.request = undefined }
```

`close(model)`：幂等（`closed` 守护）；清 debounce、中止请求、调用 `onSelectModel(model)`。

`handleInput(data)` 的顺序（**必须一致**）：

```text
tui.select.up    → 循环上移（0 → 末尾），return
tui.select.down  → 循环下移（末尾 → 0），return
tui.select.confirm →
	exact = /^[^/\s]+\/[^:\s]+(?::[^\s:]+)?$/u.test(query) ? query : undefined
	selected = exact ?? filteredResults[selectedIndex]?.id
	if (selected) close(selected)
	return
tui.select.cancel → close(undefined); return
其它 → input.handleInput(data)
	query = input.getValue().trim()
	if (query === this.query) return
	this.query = query; scheduleSearch()
```

即输入框里形如 `owner/repo[:quant]` 的文本可直接确认（无需搜索结果）。

#### 2.20.8 `class LlamaView`（`LlamaUi & Focusable`）

状态：`searchCache`、`content`、`inputHandler`、`inputTarget`、`progressPromise`、`progressResolver`、
`showingProgress`、`_focused`。构造时内容为 `frame(theme, "llama.cpp models", [Text(muted("Loading…"), 1, 1)])`。

`setContent(content, inputHandler?, inputTarget?)`：解除旧焦点 → 清 progress 状态 → 换内容 → 赋新焦点
（`inputTarget.focused = _focused`）→ `requestRender()`。

**`showModels(serverUrl, models)`**：

```ts
const sorted = [...models].sort((left, right) => {
	const loaded = Number(right.status.value === "loaded") - Number(left.status.value === "loaded");
	return loaded || left.id.localeCompare(right.id);
});
const byId = new Map(sorted.map(m => [m.id, m]));
const items = [
	...sorted.map(m => ({ value: m.id, label: m.id, description: modelDescription(m) })),
	{ value: DOWNLOAD_VALUE, label: "Download model…", description: "Hugging Face owner/repository[:quant]" },
];
return new Promise(resolve => {
	const list = new SelectList(items, Math.min(items.length, 12), selectTheme(theme),
		{ minPrimaryColumnWidth: 36, maxPrimaryColumnWidth: 56 });
	list.onSelect = item => { if (item.value === DOWNLOAD_VALUE) resolve({ type: "download" })
		else { const model = byId.get(item.value); if (model) resolve({ type: "model", model }) } };
	list.onCancel = () => resolve({ type: "close" });
	setContent(frame(theme, "llama.cpp models",
		[Text(dim(serverUrl), 1, 0), Spacer(1), list],
		`${keyHint("tui.select.confirm", "load/unload/download")} • ${keyHint("tui.select.cancel", "close")}`), list);
});
```

排序：`loaded` 优先（`sleeping` **不**算 loaded，排后），其余按 `id.localeCompare`。
**视口高度 `min(items.length, 12)`**；主列宽 `36..56`；最后一项固定 `Download model…`。
页脚：`<confirm 键> load/unload/download • <cancel 键> close`（分隔符 `" • "`）。

**`select(title, options)`**：

```ts
list = new SelectList(options.map(o => ({ value: o, label: o })), Math.min(options.length, 12), selectTheme(theme));
onSelect → resolve(item.value)；onCancel → resolve(undefined)
内容 = frame(theme, title, [Spacer(1), list], `${keyHint("tui.select.confirm", "select")} • ${keyHint("tui.select.cancel", "cancel")}`)
```

**`confirm(title, message)`**：`(await select(`${title}\n${message}`, ["Yes", "No"])) === "Yes"`。

**`connectionError(serverUrl, message)`**：

```ts
const choice = await select(`llama.cpp unavailable\n${serverUrl}\n\n${message}`, ["Retry", "Close"]);
return choice === "Retry" ? "retry" : "close";
```

**`searchModels(search)`**：新建 `HuggingFaceSearch`（共享 `searchCache`），内容为
`frame(theme, "Download model", [Spacer(1), component], "<confirm> select • <cancel> back")`，
`inputHandler` 与 `inputTarget` 都是该组件。

**`showStatus(title, message)`**：`frame(theme, title, [Spacer(1), Text(muted(message), 1, 0)])`（无页脚）。

**`progress(state)`**：首次创建并缓存 `progressPromise`（带 resolver）；`showingProgress = true`；
调用 `updateProgress(state)`；返回该 promise（用户按取消键时 resolve）。

**`updateProgress(state)`**：

```text
if (!showingProgress) return
body = [Text(theme.fg("text", state.model), 1, 0), Spacer(1), Text(theme.fg("muted", state.message), 1, 0)]
if (state.ratio !== undefined) {
	available = 40
	filled = Math.round(clamp(state.ratio, 0, 1) * 40)
	body.push(Text(theme.fg("accent",
		`${"█".repeat(filled)}${"─".repeat(available - filled)} ${Math.round(state.ratio * 100)}%`), 1, 0))
}
if (state.detail) body.push(Text(theme.fg("dim", state.detail), 1, 0))
content = frame(theme, state.title, body, keyHint("tui.select.cancel", "stop"))
inputHandler = undefined
tui.requestRender()
```

进度条：**40 格**，`█`（U+2588）填充、`─`（U+2500）空白，后接空格与百分比（`Math.round(ratio*100)`，未再 clamp）。

**`handleInput(data)`**：

```ts
if (progressResolver && keybindings.matches(data, "tui.select.cancel")) {
	const resolve = progressResolver; progressPromise = undefined; progressResolver = undefined; resolve(); return;
}
inputHandler?.handleInput?.(data);
tui.requestRender();
```

**`render(width)`**：

```ts
return content.render(width).map(line => visibleWidth(line) > width ? truncateToWidth(line, width, "") : line);
```

即超宽行按**可见宽度**截断（`visibleWidth` 忽略 ANSI）。

`invalidate()`：`content.invalidate()`。

#### 2.20.9 `showLlamaUi` / `runWithProgress`

```ts
export async function showLlamaUi(ctx, run) {
	await ctx.ui.custom<void>((tui, theme, keybindings, done) => {
		const view = new LlamaView(tui, theme, keybindings);
		void run(view).then(() => done(), (error) => { ctx.ui.notify(error message, "error"); done(); });
		return view;
	});
}
```

`runWithProgress(ui, options)`：

```ts
const controller = new AbortController();
const state = { title, model, message: initialMessage };
const settled = options.run(controller.signal, progress => { Object.assign(state, progress); ui.updateProgress(state) })
	.then(value => ({ ok: true, value }), error => ({ ok: false, error }));
let completed = false;
settled.finally(() => { completed = true; });

while (!completed) {
	const outcome = await Promise.race([settled.then(() => "settled"), ui.progress(state).then(() => "stop")]);
	if (outcome === "settled") break;
	const stop = await ui.confirm(options.cancelTitle, options.cancelMessage);
	if (!stop || completed) continue;
	try { await options.cancel() } finally { controller.abort(new Error("Cancelled")) }
	await settled;
	return { cancelled: true };
}
const result = await settled;
if (!result.ok) throw result.error;
return { cancelled: false, value: result.value };
```

取消路径：`confirm` 确认后先执行 `options.cancel()`（如 `unload`），再 `abort(new Error("Cancelled"))`，然后等 `settled`。

---

### 2.21 文档（`docs/`）

三份文档是**行为契约**的一部分（用户可见文本与限制），Rust 版必须保持同样的叙述与数值。

#### 2.21.1 `docs/mcp.md`（272 行）要点

- 快速上手：`kiana mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem .`、
  `kiana mcp add docs --url https://example.com/mcp --bearer-token-env-var DOCS_TOKEN`、`kiana mcp list`；
  默认写用户级，`--local`/`-l` 写项目级（`kiana mcp add -l tools --env API_KEY='${TOOLS_KEY}' -- uvx tools-mcp`）。
  在会话内用 `/mcp` 查看/登录/重连/改 exposure/启停；在会话外改动后需 `/reload`。
- 配置文件：`~/.kiana/agent/mcp.json`（全局）与项目 `.kiana/mcp.json`（仅信任项目）；项目文件按服务器名覆盖全局。
- **仅 override 的项目条目**：不含 `command`/`url`/`type` 的项目条目只覆盖同名全局服务器的
  `enabled`、`exposure`、`toolExposure`，其余（含 `env`/`headers`/`auth`）沿用全局。
- 传输：stdio（`command`/`args`/`env`/`cwd`）与 streamable HTTP（`url`/`headers`/`auth`/`oauth`）。
  相对 `cwd` 按会话目录解析；`command`、参数、`cwd` 里的前导 `~/` 指 home。
- **每请求 `timeout`**：秒，默认 60；**服务器的 progress 通知会重置它**。`enabled: false` 保留条目不连接。
- 配置规则（逐字要点）：服务器名只允许字母、数字、`_`、`-`；工具名 `mcp__<server>__<tool>`，
  非字母数字下划线一律换成 `_`，因此撞名的工具**全部**加哈希后缀；仅 `-`/`_` 不同的服务器名视为同一个
  （第二个被拒，`mcp.json` 覆盖注册的）。`type` 可选；`sse` 被拒（文档给出 `/mcp` 代替 `/sse` 的提示）。
  `command` 是**一个**可执行文件而不是 shell 命令串。`env`/`headers` 的值可用 `${GITHUB_TOKEN}` 或
  **整值**的 `!command`（例：`"Authorization": "!echo Bearer $(gh auth token)"`）。非法条目**报告并跳过**，
  不影响其它服务器连接。
- Exposure 表：`codemode`（默认）、`deferred`、`direct`、`hidden`，以及 `toolExposure` 按工具覆盖。
  `codemode-deferred` 是 `codemode` 的**别名**。`direct` 的工具也可从 codemode 调用。
  `codemode`/`deferred` 的工具**两种间接机制都能到达**（脚本可调、`tool_search` 也能加载）；
  codemode 调用不依赖激活的工具集，故在 `/tree`、resume、fork 之后仍可用；`tool_search` 加载的工具
  记录在 transcript 里并在该分支继续声明。
- `kiana mcp list` 会标记 exposure 与其服务器不同的工具；`/mcp` 的 Tools 视图显示生效的 exposure。
- 资源：`list_mcp_resources`、`list_mcp_resource_templates`、`read_mcp_resource` 三个工具及 `hasResources` 条件。
  三个工具覆盖"每个已启用、非 hidden、且有资源的服务器"，其 exposure 取这些服务器中**最宽**的一个：
  `direct` > `codemode`/`deferred`。资源链接在工具结果里指出 `read_mcp_resource` 与服务器名。
  `ui://` 与 `text/html;profile=mcp-app` 的 MCP App 资源、以及资源图标都不列出。
  **资源读取与列举在瞬时 HTTP 错误（408/429/5xx）后重试一次；工具调用不重试**（服务器可能已执行）。
- OAuth：`/mcp login`、`kiana mcp login`、回调端口、`oauth.clientId`/`clientSecret`/`callbackPort`/`clientName`、
  动态客户端注册、CIMD（`https://kiana.dev` 客户端元数据文档）。
  OAuth 只用于**没有 `Authorization` 头**的 HTTP 服务器。凭据属于"服务器名 + URL"：
  同 URL 不同名（如按账号）各自登录，不同 `mcp.json` 里同名同 URL 共享一次登录。
  `callbackUrl` 必须是用 `http` 的 `localhost`/`127.0.0.1`/`[::1]`，Kiana **原样发送**；省略端口时用
  `callbackPort` 或空闲端口补上（RFC 8252）。`clientSecret` 可用环境变量或命令。
  `scope` 空格分隔；服务器声明了 scope 时请求声明的，之后追加的 scope 请求叠加到配置值上。
  默认 `client_name` 是 `kiana`；`clientName` 只在**注册**时发送（改名要先 sign out）。
  CIMD：`client_id` 为 `https://kiana.dev/oauth/client.json`，回调 `http://127.0.0.1:<port>/callback`；
  授权响应不带 `iss`（RFC 9207）时改用服务器专属文档 `https://kiana.dev/oauth/<id>/client.json` 与
  `/callback/<id>` 路径。`cimd` 不能与 `clientId`/`clientName` 组合，`callbackUrl` 必须是
  `localhost`/`127.0.0.1` 且路径为 `/callback`，且授权服务器必须声明支持 CIMD 与 public clients。
  发现走 RFC 9728（protected resource metadata）并校验授权服务器的 issuer（RFC 8414）；
  服务器广告了错误/缺失的授权服务器时用 `authServerMetadataUrl` 直指元数据文档（**按配置信任**，
  必须 HTTPS，loopback 例外）。
- **迁移表**（用户可见）：Claude Desktop/Claude Code/Cursor 直接复制 `mcpServers` 条目；VS Code 从顶层
  `servers` 移入并把 `${input:...}` 换成 `${NAME}` 环境变量；Codex 把 `[mcp_servers.<name>]` TOML 字段
  转 JSON；OpenCode 把 `"type":"local"` 拆成 `command`+`args`、`environment`→`env`、`{env:NAME}`→`${NAME}`，
  `"type":"remote"` 转为 URL 条目。
- 系统提示节 `mcp_servers`：只列**未声明**工具（codemode/deferred）的服务器，`direct` 不列，总长 ≤ 4096 字符。
  有 codemode 或 deferred 工具的服务器会被列出，带"如何到达"与一行 `description`（连接后回退到 instructions）。
  节内容**变化时**（例如服务器连接后摘要才可用）Kiana 把新节**追加**到对话，而不是改工具声明，以保住缓存。
  `describeNamespace()` 与 `searchTools()` 的 `namespace` 参数接受 `mcp__dev-radius`、`mcp__dev_radius`、
  `dev-radius`、`dev_radius` 四种写法。
- 激活：服务器以 `codemode` 连接时激活 `codemode`；`deferred` 服务器激活 `tool_search`。
  想不经过检索就让模型看到某工具，用 `toolExposure` 给它 `direct`。
  `--tools` 不含 `mcp__` 前缀时**不**移除 MCP 工具（`kiana --tools read,codemode,'mcp__radius__*'` 只留 radius）；
  `--exclude-tools` 同样的模式语法；`--no-mcp` 单次运行禁用 MCP。
  想在没有 MCP 服务器时保留 codemode 激活：settings 里 `"defaultTools": ["+codemode"]`；
  阻止自动激活 codemode：`mcpServers` 旁 `"autoEnableCodemode": false`（项目值覆盖用户值）。
  当既没有 `codemode` 也没有 `tool_search` 激活、而存在非 direct 工具时，Kiana **警告一次**。
- 20 KB 截断：超过 20 KB 的文本结果**中间**被移除，围绕 `…N chars truncated…` 标记；全文存临时文件并在结果里命名。
  codemode 脚本拿到**完整**结果并可自行裁剪后再返回。
- 脚本拿到的 MCP `CallToolResult` 包含 `content`/`structuredContent`/`isError`；`isError` 在脚本内 resolve、
  对直接调用报错；`image(result.content[0])` 转发图片块。**server instructions 不属于任何工具描述**，
  脚本用 `describeNamespace("mcp__<server>")` 读取（同时返回工具名）。
- 故障：`/mcp` 管理器（列表带状态、工具数、exposure、配置来源；需要关注者排前）、`failed: …`、`needs sign-in`、
  `Run /mcp to fix.`。`kiana mcp list` 连接每个启用的服务器并打印状态、工具与错误；条目非法或启用的服务器未连接 → 退出码 1。
- 后台连接语义（逐字）：会话启动就后台连接所有启用服务器；首个 prompt **只**为有 `direct` 工具的服务器
  等待最多 **10 秒**；其它服务器按需等待——codemode 脚本等待它命名的服务器（`mcp__<server>`），
  调 `searchTools()` 或读 `ALL_TOOLS` 时等待全部；`tool_search` 与资源工具也等待全部。
  HTTP 网络错误与瞬时状态（408、429、5xx）**重试两次**；断开的连接显示为 disconnected 并在下次调用重连；
  服务器宣告工具列表变化时新增工具、撤下的工具变为不可达。
  **停止 stdio 服务器**：关 stdin → SIGTERM → SIGKILL，作用于其**进程组**（也会停掉 `npx`/`uvx` 这类包装器起的子进程）。
- 日志：服务器 `notifications/message` 追加到 `~/.kiana/agent/mcp.log`，格式
  `<time> [<server>] <level> <logger>: <message>`；超过 5 MB 移到 `mcp.log.1`。
- 权限：每个 MCP 调用都走 Kiana 的工具管线，`tool_call`/`tool_result` 处理器（含权限门）对所有 MCP 工具生效；
  **从 codemode 脚本发出的调用带 `parentToolCallId`**（= codemode 调用 ID）。`kiana.getAllTools()` 报告服务器声明的
  annotations：`readOnlyHint`、`destructiveHint`、`idempotentHint`、`openWorldHint`；资源工具标记为 read-only。
- `--no-mcp` 禁用内建 MCP 扩展。扩展与 SDK 一节：
  `kiana.registerMcpServer(name, config)` 以 `mcpServers` 条目的同形配置注册当前会话的服务器（连接、出现在 `/mcp` 且来源为扩展）；
  对注册服务器的 enabled/exposure 改动只作用于当前会话；**文件里同名的服务器优先**，`/mcp` 会列出被覆盖的注册。
  `kiana mcp` shell 命令**不加载扩展**，只看到文件配置的服务器。
  安装了会注册 `/mcp` 的扩展（如 `kiana-mcp-adapter`）就**替换**内建 MCP：Kiana 不再读 `mcp.json`、不再连它的服务器，
  `/mcp` 归该扩展；移除扩展恢复。只想禁用内建：`kiana config` 里关掉 Built-in 的 `mcp`，或 settings 里
  `"extensions": ["-builtin:mcp"]`。注册了同名 `codemode`/`tool_search` 的扩展同样替换内建工具；
  只有当它声明了 `reachesTools` 时 Kiana 才会为 MCP 服务器激活替换品并按需等待服务器。
  shell 级 `kiana mcp` 始终用内建实现。
  SDK 会话**不加载内建扩展**：需自行把 MCP 扩展、codemode 扩展（给 `codemode` 服务器）、tool-search 扩展
  （给 `deferred` 服务器）加进 resource loader。

#### 2.21.2 `docs/codemode.md`（230 行）要点

- 工具输入是**裸 JavaScript 源码**，不是 JSON、也不是 markdown 代码块；作为 async 函数体在 QuickJS 沙箱运行
  （顶层 `await` 与 `return` 可用）。
- 全局表：`tools.<name>(args)`、`ALL_TOOLS`、`text(value)`、`image(value)`、`console.log/info/warn/error/debug`、
  `return value`、`exit()`、`store(key, value)`/`load(key)`、`searchTools(query, { limit?, namespace? })`、
  `describeTool(name)`、`describeNamespace(name)`、`models`。
- `// @options:` **必须是第一行**；出现在别处会在脚本运行前就失败。字段 `max_output_tokens`（默认 **10000**）、
  `timeout_ms`（默认**不设**，是整个脚本的硬 deadline，**包含等待空闲 worker 的时间**；图片生成可能数分钟）。
- 限制：无 Node/文件系统/网络/定时器；`timeout_ms` 上限 `2147483647`；VM 堆 256 MiB；Out-of-memory 抛
  `InternalError: out of memory`；Bun 上中止/到达 deadline 只是"及时结束调用"，不轮询 interrupt handler 的
  内建可能继续算到返回。
- **输出失败上限**：脚本在超过 **16777216** 个字符的文本与 base64 图片数据、或超过 **100000** 次
  `text()`/`image()`/`console` 调用时失败（建议改用工具写文件）。
- **worker 准入**：全进程最多 2 个存活 worker（`KIANA_CODEMODE_MAX_WORKERS` 可改）；**被取消的 worker 在退出前仍占槽**；
  满载时新脚本按**到达顺序**等待；60 秒内（或 `timeout_ms` 之前）拿不到 worker 则**不运行**直接失败。
  一个等待永远不会 settle 的 promise（且没有挂起的工具调用）的脚本会**立即失败**（因为没有定时器）。
  脚本不能启动其它 `codemode` 脚本。
- 调用语义：`tools` 的方法名是工具标识符（非合法 JS 标识符字符变 `_`，故 MCP 的
  `mcp__dev-radius__search` 是 `tools.mcp__dev_radius__search`），每个方法收**一个**参数对象。
  有 output schema 的工具 resolve 成结构化值：`bash` → `{ output, truncated, full_output_path?, exit_code, wall_time_seconds }`
  （**非零退出码也 resolve**；`output` 最多 1 MiB，超出保留首尾各 512 KiB 加省略标记，`truncated` 为真、全文在
  `full_output_path`）；MCP 工具 → `CallToolResult`；`read` → 文本，或图片块
  `{ type: "image", data, mimeType, note }`（`image()` 展示，`note` 是随之而来的文本如缩放提示）；
  其它工具（`edit`/`write`）→ 文本输出。失败/被拦/参数非法的调用 reject 一个带工具错误文本的 `Error`
  （用 `Promise.allSettled()` 保住成功结果）。
- **`codemode` 描述**列出工具的 TypeScript 声明并按命名空间分组；`deferred` exposure 的工具（含默认 exposure 的
  MCP 工具）不列出，故描述在 MCP 服务器连接时不变。列出声明共享 **3000** 估算 token 的预算
  （settings 的 `codemode.inlineBudget`）。`codemode.mode`：`on`（默认）保持声明并改写其描述；
  `only` 时把它们从模型隐藏、改在 `codemode` 描述里列出。`codemode` 描述、`describeTool()`、`ALL_TOOLS` 里的
  工具声明都带工具的 prompt guidelines（系统提示规则只覆盖已声明的工具）。
- `store()`：`undefined` 删除键；**只有成功的脚本**才追加 `codemode-store` custom entry，恢复会话后值仍在、
  每个分支只看到自己路径上的写入。单个值最多 **262144** 个 JSON 字符、全部值合计最多 **1048576**；
  不要存图片数据（用 `image()`，它也会落盘）。
  超限错误文案逐字（`agent-codemode` 的 prelude，`STORE_HINT` = "store() is for small state such as IDs or summaries.
  Show images with image(), keep large data in variables, or write it to a file with a tool."）：
  `store("<key>") value has <N> characters of JSON, more than the limit of 262144. <STORE_HINT>` 与
  `store is full: stored values would exceed 1048576 characters of JSON. Delete keys with store(key, undefined). <STORE_HINT>`
  （均 `RangeError`）。非 JSON 可序列化：`store("<key>") value is not JSON-serializable: <…>` / `… is not JSON-serializable`（`TypeError`）。
- **输出超限错误**逐字：`script output exceeded the limit of 16777216 characters or 100000 text(), image(), and console calls.
  Print a summary instead, or write large data to a file with a tool.`（`RangeError`，脚本失败）。
  `image()` 参数错误：`image expects a non-empty image URL string, an object with image_url, or an image block { type: "image", data, mimeType }`。
- `models` API 表：`getModelsOfType`、`getAvailableOfType`、`getModelOfType`、`classify`、`generateImages`；
  `Classify` 与 `Generate images` 两节的 context 形状（与 §2.12.5 的 `CLASSIFIER_CONTEXT_SHAPE` 一致）。
  `classify()`/`generateImages()` 只用 `model` 的 `provider` 与 `id`（`{ provider, id }` 也行），
  **不因 provider 错误而抛**（看 `stopReason`/`errorMessage`）；每脚本最多 **4** 个并发（多出的排队），
  usage 加到 `codemode` 工具结果并计入会话成本。聊天模型可列出但**不能**从脚本运行。
  `ClassifierResult.answers` 按 question ID；`choice` 带 `probabilities` 与 `confidence`，
  `score` 是期望档位（0..`criteria.length-1`），`bool` 是 `true` 的概率。
- 输出约定：`Script completed`/`Script failed` + wall time + `Output:`；`==> text N/M <==`、`<console_output>`、
  图片落盘标签（路径在图片**之前**）、截断提示与 `[Full output: …]`。失败的脚本保留部分输出，后接 `Script error:`
  与错误。调用是真实的：失败前的调用不回滚；脚本结束时仍在跑的调用被取消，未 await 的 promise 被丢弃。

#### 2.21.3 `docs/llama-cpp.md`（121 行）要点

- 前提：llama.cpp **router 模式**服务器（要较新的构建）；默认 `http://127.0.0.1:8080`；`/login llama.cpp` 配置；
  环境变量 `LLAMA_BASE_URL`、`LLAMA_API_KEY`；API key 可省略（回退 `"local"`）。
- **启动 router 的官方命令行**（文档逐字给出，Rust 侧的文档/帮助需保留）：

  ```bash
  llama-server \
    --models-dir ~/models \
    --no-models-autoload \
    --jinja \
    --host 127.0.0.1 \
    --port 8080 \
    -ngl 999 \
    -c 32768
  ```

  选项含义：`--models-dir` 发现本地 GGUF；`--no-models-autoload` 让加载只能经 `/llama` 显式触发；
  `--jinja` 启用兼容 chat template 与工具调用；`-ngl 999` 尽量把层卸到 GPU；`-c 32768` 给每个加载模型设上下文
  （省略则用模型原生上下文，可能吃更多内存）。**不要**传 `--model`/`-m`（会进单模型模式）或 `-hf`。
  启动参数里用 `--api-key` 时与 Kiana 侧 key 保持一致；本地只监听就保留 `--host 127.0.0.1`。
- 目录布局（文档逐字树）：单文件模型可直接放模型目录；多模态与多分片模型要各自子目录——
  `<model>/<model>.gguf` + `mmproj-F16.gguf`；分片 `…-00001-of-00003.gguf` 等三件。手工加文件后要**重启** router。
- `/login llama.cpp` 输入 router URL 与可选 API key。若 router 用 `--no-models-autoload`，`/login` 只存连接，
  需 `/llama` 加载模型、再 `/model` 选中。环境变量可替代 `/login`。
- `llama.cpp` Provider：chat 模型（`openai-completions`）、classifier 模型（`llama-cpp-classify` 或
  `typesafe-system-one`）；成本恒为 0；上下文窗口推导顺序。
- 思考支持：`chat_template` 含 `enable_thinking` → `qwen-chat-template` 思考格式，等级 `off`/`medium`。
- `/llama` 管理器：选未加载模型→加载；选已加载→卸载；**Download model…**→搜 Hugging Face→选仓库与量化；
  也接受精确的 `owner/repository[:quant]`；加载/下载中按 Escape 会**确认取消**。
  Hugging Face 搜索用 `HF_TOKEN`，再依次查 `$HF_TOKEN_PATH`、`$HF_HOME/token`、`$XDG_CACHE_HOME/huggingface/token`、
  `~/.cache/huggingface/token`；无凭据也能搜（限流更低）。下载 gated 仓库前警告并给出访问页链接；
  **下载由 llama.cpp server 执行**，所以该进程也要有 `HF_TOKEN`。
  若已有其它模型加载，Kiana 问先卸载还是保留；**不会静默卸载**，**从不删除模型文件**；router 可能被别的客户端共用，
  故 `/llama` 总显示 router 的当前状态。
- `/model` 里出现已加载与 sleeping 模型；sleeping 选中即唤醒；router 开了 autoload 时未加载的 preset 模型也出现并在
  选中时加载；`--no-models-autoload` 时需先经 `/llama` 加载。
- router 断开时 `/llama` 显示 **Retry** 与 **Close**；Retry 重连并刷新模型状态，**不重放**被打断的操作。
- 分类器（文档一节，行为契约）：
  - **Decision models**（Julia-1、Laya、Kev、lev、OpenJev 等）走 llama.cpp 的 `/v1/systemone`，
    API 为 `typesafe-system-one`，**只作为 classifier 出现、不出现在 `/model`**。
  - **Chat models** 也以同 ID 列为 classifier（API `llama-cpp-classify`）。
  - llama.cpp **0.6.0+** 在 router 的模型列表里报告 decision 模型：`architecture.output_modalities` 含 `decisions`；
    router 从 GGUF metadata 读，故未加载/sleeping 的也能识别；更老的构建不报，decision 模型会被列为 chat 模型。
  - chat-as-classifier 的提示构造：每个 question 一个 chat prompt = state、请求里**所有** question、**再一次 state**、
    最后是带单 token 标签的 question。标签字母用于 choice（最多 **62** 个选项）、`Yes`/`No` 用于 bool、
    数字用于 score（最多 **10** 档）。state 出现两次（读第二遍时已带着问题），在小模型上提升 JevBench 准确率。
    Kiana 读标签的 next-token 概率并归一化；choice 返回每个选项概率与 `(n * peak - 1) / (n - 1)` 的 confidence，
    score 返回期望档位。
  - 原始标签概率通常过度自信：**每请求**的 `temperature` 选项在归一化前除 logits（>1 更平缓），不改变答案。
  - question 串行执行；最后问题之前的内容对请求内所有 question 相同，故 server 的 **prompt cache** 只算一次；
    state 出现两次，需要两倍上下文。
  - 小模型可能听从写在 state 里的指令（prompt 要求把 state 当数据判断，但非保证）。
  - Qwen3.5 这类 hybrid 模型没有 context checkpoint 就无法回退部分缓存的 prompt；若每个 question 都重算整个 state，
    启动 router 时加 `--ctx-checkpoints 32 --checkpoint-min-step 0`。
- 故障排查（逐字命令）：`curl http://127.0.0.1:8080/health`、`curl http://127.0.0.1:8080/models`。
  常见项：`/llama` 无模型→查 `--models-dir`、目录布局、重启 router；`--no-models-autoload` 下 `/model` 缺模型→先 `/llama`；
  加载失败/内存过高→降 `-c` 或卸载别的模型；"不是 router 模式"→去掉 `--model`/`-m`/`-hf`。
- 移除 provider：`kiana config` 关掉 Built-in 的 `llama.cpp`，或 settings 里 `"extensions": ["-builtin:llama.cpp"]`。

#### 2.21.4 `docs/cli.md` / `docs/environment-variables.md` 相关行（已在 `ca-cli` 记录，此处仅列本单元相关的逐字项）

- `kiana mcp add|remove|list|login|logout` 命令表与 `--no-mcp`、`--tools`、`--exclude-tools`。
- `KIANA_CODEMODE_MAX_WORKERS`：见 §4.3。
- **环境变量面（本单元实际读取的只有这些）**：`KIANA_CODEMODE_MAX_WORKERS`（`process.env`，每次调用重读）、
  `LLAMA_BASE_URL`/`LLAMA_API_KEY`（经 `ctx.env`，凭据 `env` 优先）、`HF_TOKEN`/`HF_TOKEN_PATH`/`HF_HOME`/
  `XDG_CACHE_HOME`/`HOME`（`findHuggingFaceToken`，读 `process.env`）。`docs/environment-variables.md`
  里与本单元相关且**只有** `KIANA_CODEMODE_MAX_WORKERS` 一行（§4.3 已逐字给出）；`LLAMA_*`、`HF_*` 在该文件里**未**列出
  （只在 `docs/llama-cpp.md` 出现）。
- `docs/cli.md` 的 `Tools` 一节另有两条与本单元相关：`--tools` 的 `mcp__` 前缀语义（见 2.21.1）、
  以及 codemode 的启用方式（`--tools codemode`、`defaultTools`、`setActiveTools()`）。

---

## 3. 用户可见表面（逐字）

本节汇总所有**必须逐字一致**的用户可见文本。实现时建议统一放进 `strings` 模块并用快照测试锁定。

### 3.1 斜杠命令

| 命令 | 描述（逐字） | 可用模式 |
|---|---|---|
| `/mcp` | `Manage MCP servers: sign in, reconnect, enable or disable, and change exposure` | 全部（非 TUI 打印状态） |
| `/llama` | `Manage llama.cpp router models` | 仅 `tui` |

`/mcp` 参数（`MCP_USAGE`，逐字）：

```
Usage: /mcp, /mcp login [server], /mcp logout [server], /mcp reconnect [server]
```

`/mcp` 参数补全（`getArgumentCompletions`）：

- 第一层：`login`、`logout`、`reconnect`，按输入前缀过滤，`value` 为 `"<action> "`（带尾空格），`label` 为动作名。
- 第二层：仅当动作是 `login|logout|reconnect`；候选按服务器过滤（`reconnect` 要求 `connection !== undefined`，
  其它要求 `usesOAuth`），按名称前缀过滤，`value` 为 `"<action> <name>"`，`label` 为服务器名，
  `description` 为 `describeState(server)`；无候选项时返回 `null`（不补全）。
- 已有第三个参数时返回 `null`。

`/llama` 不接受参数（`_args` 忽略）。

### 3.2 `/mcp` 菜单（TUI）

**服务器列表**：标题 `MCP servers`；`error` 为 `config: …` / `overridden: …` 行；条目
`<name>` + 描述 `<state> · <exposure> · <scope|"global, project override">`；空列表文案
`No MCP servers configured. Add them to <agentDir>/mcp.json or .kiana/mcp.json.`；
确认键标签 `manage`，取消键标签 `close`。

**服务器详情**：标题 `MCP server <name>`；`details` 四行（传输、来源、可选 project override、`State: …`）；
`items` 按状态生成（见 §2.9.20），标签逐字：`Enable`、`Enable in this project`、`Sign in`（描述 `opens the browser`）、
`Tools`（描述 `<n> offered`）、`Reconnect`、`Sign out`（描述 `deletes the stored credentials`）、`Exposure`、
`Disable`、`Disable in this project`；`confirmLabel` `select`，`cancelLabel` `back`。

**工具列表**：标题 `Tools of <name>`；details
`Exposure <exposure>: <unreachable|EXPOSURE_DESCRIPTIONS[exposure]>`，有 per-tool 覆盖时追加
`\nSome tools override it with toolExposure.`；条目 `<tool name>` + 描述（覆盖时前缀 `[<exposure>] `）；
空列表 `The server offers no tools.`

**Exposure 选择**：标题 `Exposure of <name>`；details
`Applies to this session; the server is registered by <source>.` 或 `Saved to <override ?? source>.`；
三项 `codemode`/`deferred`/`direct`，当前项标签前缀 `"✓ "`、其它 `"  "`，描述为 `EXPOSURE_DESCRIPTIONS`；
`confirmLabel` `save`。

**登录状态页**（`McpManagerView`）：标题 `Sign in to <name>`，状态行 `Contacting the authorization server…` /
`Connecting…`；可取消时页脚含 `cancel`（取消键）。

### 3.3 `/llama` 界面

| 界面 | 逐字 |
|---|---|
| 主标题 | `llama.cpp models` |
| 初始加载 | `Loading…` |
| 服务器行 | `<serverUrl>`（`dim`） |
| 下载项 | `Download model…` + 描述 `Hugging Face owner/repository[:quant]` |
| 主列表页脚 | `<confirm> load/unload/download • <cancel> close` |
| 通用选择页脚 | `<confirm> select • <cancel> cancel` |
| 富搜索页标题 | `Download model`；页脚 `<confirm> select • <cancel> back` |
| 搜索输入提示 | `Model name or owner/repository[:quant]` |
| 搜索状态 | `Type at least 2 characters`、`Searching Hugging Face…`、`No GGUF models found` |
| 搜索条目 | `→ <id>  <n> downloads`（选中，accent）/ `  <id>` + muted(`  <n> downloads`) |
| 滚动信息 | `  (<index+1>/<total>)` |
| 模型描述 | `loaded` / `loading` / `downloading` / `sleeping` … ` · <ctx> context` |
| 不可用 | `llama.cpp unavailable\n<serverUrl>\n\n<message>`，选项 `Retry` / `Close` |
| 加载中 | 标题 `Loading model`，正文 `<id>`、消息（`Starting…` / `Loading <stage>`）、进度条、`<done> / <total>`；页脚 `<cancel> stop` |
| 下载中 | 标题 `Downloading model`，同上 |
| 取消确认 | `Stop loading?` + `<id>` / `Stop download?` + `<model>`，选项 `Yes` / `No` |
| 卸载确认 | `Unload model?` + `<id>`，选项 `Yes` / `No` |
| 加载冲突 | `<n> model is loaded` / `<n> models are loaded`，选项 `Unload all and load` / `Keep loaded and load` / `Cancel` |
| 量化选择 | `Select quantization\n<id>`，选项 `<NAME>[ · <size>][ · recommended]` |
| 授权要求 | 见 §2.19.2（`Hugging Face access required…`），选项 `Continue` / `Back` |
| 状态页 | `Loading model details` + `<repository>` |

进度条：40 格，`█`×filled + `─`×(40-filled) + ` ` + `<percent>%`（accent 色）。

### 3.4 通知（`ctx.ui.notify`）

| 级别 | 文本（逐字） |
|---|---|
| warning | `MCP tools are only reachable from the codemode or tool_search tool, but neither is active${reason}; they cannot be called.` |
| warning | `MCP servers need attention:\n  <line>\n  …\nRun /mcp to fix.` |
| info | `MCP servers are still connecting; their tools become available once connected.` |
| error | `MCP failed to load: <msg>` |
| error | `No MCP server named "<name>".` |
| error/info | `No enabled MCP server uses OAuth. Only HTTP servers without an Authorization header do.`（error 当显式指定名字时；info 当自动挑选且无候选时） |
| error | `Signing in to MCP server "<name>" requires interactive mode.` |
| info | `Sign in to MCP server "<name>" in your browser:\n<url>` |
| info | `Signed in to MCP server "<name>" (<n> tools).` |
| info/error | `Sign-in cancelled.`（info）/ 其它失败（error） |
| info | `Signed out of MCP server "<name>".` |
| info | `No stored credentials for MCP server "<name>".` |
| error | `No enabled MCP server to reconnect.` |
| info | `Reconnected to MCP server "<name>" (<state>).`（失败时为 error：`<failure>`） |
| warning | `MCP_USAGE` |
| warning | `Configure llama.cpp with /login llama.cpp` |
| warning | `/llama is available in interactive mode` |
| warning | `<id> is <status>` |
| error | 各动作抛出的错误消息原文 |

**注意**：`Reconnected to MCP server …` 为 `info`；`MCP servers need attention:` 为 `warning`。

### 3.5 工具渲染（`codemode`）

调用行：`codemode`（bold + `toolTitle`）；折叠时脚本前 10 视觉行 + `... (N more lines, <key> to expand)`；
无效参数 `codemode [invalid arg]`。

结果：

```
<icon> <name> <args> <duration> <cost>
...
Model calls: $0.012

<脚本输出，前 5 视觉行>
Full output: /tmp/kiana-codemode-xxxxxxxx.txt

Took 1.2s
```

- 图标：`…`（running，warning）、`✓`（ok，success）、`✗`（error，error）、`⊘`（cancelled，muted）。
- 折叠提示（调用列表）：`... (N earlier calls, <key> to expand)`。
- 展开时错误行：4 空格缩进、error 色、内部换行同样缩进。

### 3.6 MCP 工具渲染

`createMcpToolRenderers(label)`（`label = "<server>/<tool>"`）由 `mcp/tools.ts` 定义（见 §2.4）；
调用行显示 `<server>/<tool>`，结果按 MCP 内容块渲染。正则 `/^mcp__(.+?)__(.+)$/` 保证恢复会话时也能渲染。

### 3.7 主题色使用

| 用途 | 主题键 |
|---|---|
| 标题、选中项、边框、进度条 | `accent` |
| 工具名 | `toolTitle` |
| 参数、次要说明、计时器、`Model calls:`、`Full output:` | `muted` |
| 时长、滚动信息、页脚提示 | `dim` |
| 错误图标、错误文本、`[invalid arg]` | `error` |
| 成功图标 | `success` |
| 运行中图标 | `warning` |
| 脚本输出 | `toolOutput`（错误时 `error`） |
| 进度正文（模型 id） | `text` |

### 3.8 布局常量

| 常量 | 值 |
|---|---|
| codemode 脚本预览行数 | 10 |
| codemode 输出预览行数 | 5 |
| codemode 折叠调用数 | 8（取最后 8 个） |
| codemode 折叠参数宽度 | 80 字符 |
| llama 主列表视口 | `min(items, 12)` |
| llama 主列表主列宽 | `36 .. 56` |
| llama 富搜索视口 | 10 |
| llama 进度条宽度 | 40 |
| 菜单/列表边框 | `DynamicBorder`（accent） |
| `frame` 文本左内边距 | 1 |

---

## 4. 外部格式（逐字）

### 4.1 磁盘文件

| 路径 | 写入者 | 权限 | 说明 |
|---|---|---|---|
| `~/.kiana/agent/mcp.json` | `kiana mcp add/remove`、`/mcp` 的 setEnabled/setExposure | 默认 | 全局 MCP 配置 |
| `<cwd>/.kiana/mcp.json` | 同上（`-l`/`--local` 或 project override） | 默认 | 项目配置，仅信任项目读取 |
| `~/.kiana/agent/mcp-auth.json` | OAuth 流程 | `0o600` | 每服务器 OAuth 状态 |
| `~/.kiana/agent/mcp.log` | 运行时（`McpServerLog`） | 默认 | 服务器日志，5 MiB 轮转 |
| `~/.kiana/agent/mcp-auth-refresh-<sha256(key).hex[..16]>.lock/`（目录） | 刷新锁（proper-lockfile） | — | `lockDir = backend ? 构造参数 : getAgentDir()`；传入 backend 且未给 lockDir 时**不加锁**。proper-lockfile 在目标路径后追加 `.lock` 并创建**目录**，内含 `mtime` 心跳 |
| `<tmpdir>/kiana-codemode-<16 hex>.txt` | codemode 溢出 | `0o600`，`wx` | 完整输出 |
| `<tmpdir>/kiana-codemode-<16 hex>.{png,jpg,gif,webp}` | codemode 图片 | `0o600`，`wx` | 图片落盘 |

`writeOutputFile` 实现（`ca-tools-compaction-export` 亦有记载）：`join(tmpdir(), `${prefix}-${randomBytes(8).toString("hex")}${ext}`)`，
`writeFile(path, data, { mode: 0o600, flag: "wx" })`。

### 4.2 `mcp.json`

顶层：

```json
{
  "mcpServers": { "<name>": { … } },
  "autoEnableCodemode": true
}
```

服务器条目字段（`McpServerConfig`，校验细节见 `ca-resources` §2.9）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | `"stdio"` \| `"http"`（TS 声明） | 可选。`command` 选 stdio、`url` 选 streamable HTTP；显式写时必须是 `stdio`、`http` 或 `streamable-http`（后两者等价，校验后配置里**保留原字面量**，故 Rust 枚举需容忍 `streamable-http`）。`"sse"` 被拒：`server "<name>": legacy SSE transport is not supported; use the streamable HTTP URL`。**无 `command` 也无 `url`** → `server "<name>" needs either "command" (stdio) or "url" (streamable HTTP)`（项目 override 之外的条目） |
| `command` | string | stdio 可执行文件（**单个**可执行文件，不是 shell 命令串） |
| `args` | string[] | stdio 参数 |
| `env` | Record<string,string> | stdio 环境变量；值可用 `${NAME}` 或 `!command` |
| `cwd` | string | stdio 工作目录；相对路径按会话目录解析，`~/` 开头按 home 解析 |
| `url` | string | streamable HTTP URL |
| `headers` | Record<string,string> | HTTP 头；值可用 `${NAME}` 或 `!command`（`!cmd` 必须占满整个值） |
| `auth` | `{ provider: string }` | 用某个 kiana provider 的 token 代替 OAuth；**仅**全局 `mcp.json` 允许 |
| `oauth` | `{ clientId?, clientSecret?, callbackPort?, callbackUrl?, scope?, clientName?, clientRegistration?, authServerMetadataUrl? }` | OAuth 细节 |
| `exposure` | `"codemode" \| "deferred" \| "direct" \| "hidden"` | 默认 `codemode`；`codemode-deferred` 是 `codemode` 的别名，校验时归一为 `codemode` |
| `toolExposure` | Record<string, exposure> | 按工具名/模式（`*` 通配）覆盖；**精确名优先于模式**，模式间按**插入顺序**取第一个匹配 |
| `enabled` | boolean | 默认 `true` |
| `timeout` | number | **每请求**超时秒数；默认 `60`；服务器的 progress 通知会重置它。必须为正数（`server "<name>": timeout must be a positive number of seconds`） |
| `description` | string | 系统提示节里的服务器摘要（首行）；也让 `tool_search` 按它排序该服务器工具，并作为 `describeNamespace()` 的描述。未配置时用连接后服务器 instructions 的首行 |

**写入保持**：`editMcpServers` 用正则 `/^([ \t]+)\S/m` 探测缩进（默认 2 空格），
以 `JSON.stringify(parsed, null, indent) + "\n"` 重写全文；未知字段与键顺序保持不变（JS 对象键序：整数键升序在前，
其余插入序）。

**BOM 处理（更正）**：读取时 `stripBom(readFileSync(path, "utf8"))` 会**去掉**开头 U+FEFF
（`JSON.parse` 本身不接受 BOM）；写回用的是 `JSON.stringify(parsed, …)`，**BOM 不会被写回**。
测试 `reads and edits mcp.json files that start with a UTF-8 BOM` 断言的是「能正常读取并在无 BOM 的前提下编辑」，
不是「保留 BOM」（它读回文件后直接 `JSON.parse(readFileSync(global, "utf8"))`，若 BOM 仍在会抛错）。

### 4.3 环境变量

| 变量 | 使用者 | 语义 |
|---|---|---|
| `KIANA_CODEMODE_MAX_WORKERS` | `execute.ts` | 正整数字符串 → 并发 worker 上限；否则 2。**每次**调用重读 |
| `LLAMA_BASE_URL` | provider/llama index | 服务器根 URL（凭据 env 优先于进程 env） |
| `LLAMA_API_KEY` | provider resolve | 无凭据 key 时的 API key；仍无则 `"local"` |
| `HF_TOKEN` | `findHuggingFaceToken` | Hugging Face token（trim 后非空才用） |
| `HF_TOKEN_PATH` | 同上 | token 文件路径 |
| `HF_HOME` | 同上 | `<HF_HOME>/token` |
| `XDG_CACHE_HOME` | 同上 | `<XDG_CACHE_HOME>/huggingface/token` |
| `HOME`（隐含） | 同上 | `~/.cache/huggingface/token` |

`docs/environment-variables.md` 关于 `KIANA_CODEMODE_MAX_WORKERS` 的逐字行：

```
Maximum codemode workers alive at once across all sessions in the process; default `2`. Each worker's script may use up to 256 MB. Raise it when one process runs several sessions, such as in-process subagents. Further scripts wait for a free worker. See [Codemode](codemode.md)
```

### 4.4 `mcp-auth.json`

顶层是 `Record<key, McpOAuthState>`；键 `"<mcpNamespace(name)>|<new URL(serverUrl) 字符串>"`，
旧版键为 `String(new URL(serverUrl))`（仅按 URL）。

```ts
interface McpOAuthState {
	serverUrl: string;                       // 规范化后的 URL
	clientInformation?: OAuthClientInformationMixed;
	tokens?: { access_token, token_type?, refresh_token?, expires_in?, scope?, … };
	tokensExpireAt?: number;                 // epoch ms
	codeVerifier?: string;                   // 进行中的 PKCE
	oauthState?: string;                     // 进行中的 state
	discovery?: AuthorizationServerMetadata;
}
```

序列化：`JSON.stringify(states, null, 2) + "\n"`（2 空格，末尾换行，无 BOM）。
读取：空/空白 → `{}`；`JSON.parse` 结果非对象或数组 → `{}`（**不报错**）。
写入：经 `FileAuthStorageBackend.withLock`（proper-lockfile 同步锁，重试 10 次、每次间隔 20 ms 忙等），
文件模式 `0o600`，缺目录时用 `0o700` 递归创建，缺文件时先写 `"{}"`。

### 4.5 `mcp.log`

每行（`formatMcpLogMessage`）：

```
<ISO 8601 UTC> [<server>] <level>[ <logger>:] <text>
```

- 时间戳为 `new Date().toISOString()`（如 `2026-10-26T12:34:56.789Z`）。
- `logger` 为空时无该片段；非空时形如 ` <logger>:`（**前导一个空格 + 名字 + 冒号**，无方括号），
  因此 `level` 与 `logger` 之间恰好一个空格，`logger` 与 `text` 之间也是一个空格。
- 文本中的换行替换为 `"\n    "`（4 空格续行）。
- 超过 5 MiB（5 * 1024 * 1024）时先把现有文件重命名为 `<path>.1`（覆盖旧 `.1`）再写新文件。

### 4.6 codemode 会话条目（`codemode-store`）

追加为 session custom entry：

```json
{ "type": "custom", "customType": "codemode-store", "data": { "set": { "k": <json> }, "delete": ["k2"] } }
```

- 只在脚本**成功**且 `set` 非空或 `delete` 非空时追加。
- 读取时按分支从根折叠：先 `delete` 再 `set`；`data` 形状不符则**跳过**该条目。

### 4.7 llama.cpp HTTP 端点

| 方法 | 路径 | 请求 | 响应 |
|---|---|---|---|
| GET | `/models` | — | `{ data: LlamaModelInfo[], object?: string }` |
| GET | `/models?reload=1` | — | 同上（重新扫描磁盘） |
| GET | `/props` | — | `{ models_autoload?: boolean, chat_template?: string, … }` |
| GET | `/props?model=<id>&autoload=false` | — | 同上（查询单个模型） |
| POST | `/models/load` | `{"model":"<id>"}` | 任意 |
| POST | `/models/unload` | `{"model":"<id>"}` | 任意 |
| POST | `/models` | `{"model":"<id>"}` | 任意（触发下载） |
| GET | `/models/sse` | `Accept` 由 fetch 默认 | SSE 流，`data: {"model","event","data"}` |

通用请求头：`Content-Type: application/json`（有 body 时）、`Authorization: Bearer <apiKey>`（有 key 时）；
每次请求 15 s 超时（`AbortSignal.timeout`），并与外部 signal 取 `any`。

错误消息：优先 `payload.error.message`，否则 `llama.cpp returned HTTP <status>`。

`GET /models` 的条目形状（必须逐字段解析）：

```json
{ "id": "…", "aliases": ["…"], "status": { "value": "loaded", "args": ["--ctx-size","32768"], "failed": false, "exit_code": 0, "progress": { "f": { "done": 1, "total": 2 } } }, "architecture": { "input_modalities": ["text","image"], "output_modalities": ["text"] }, "source": "preset", "meta": { "n_ctx": 32768, "n_ctx_train": 131072, "size": 123, "ftype": "Q4_K_M" } }
```

`status.value` 取值：`unloaded`、`loading`、`loaded`、`downloading`、`sleeping`。

**SSE 事件类型**：

| `event` | 用途 |
|---|---|
| `model_status` / `status_change` | `data.status === "loaded"` / `"unloaded"`；`data.progress` 用于加载进度 |
| `download_progress` | `data.progress` 或 `data` 本身是 `{file: {done,total}}` |
| `download_finished` | 下载完成 |
| `download_failed` | `data.error.message` 或 `"Download failed"` |

### 4.8 Hugging Face HTTP 端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/models?search=<q>&filter=gguf&sort=downloads&direction=-1&limit=20` | 搜索 |
| GET | `/api/models/<owner>/<repo>?blobs=true` | 详情（含 `siblings[].rfilename`/`size`、`gated`） |

请求头：`Authorization: Bearer <token>`（有 token 时）；15 s 超时。

错误：

- 429：`retry-after`（秒）或 `ratelimit` 头中的 `t=<n>` → `Hugging Face rate limit reached; retry in <n>s`；
  都没有 → `Hugging Face rate limit reached`。
- 其它非 2xx：`payload.error`（非空字符串）否则 `Hugging Face returned HTTP <status>`。
- 非法搜索响应：`Hugging Face returned invalid search results`。
- 非法详情响应：`Hugging Face returned invalid model details`。

### 4.9 kiana-ai Provider / auth 形状（llama.cpp）

```ts
Provider<"openai-completions"> = {
	id: "llama.cpp", name: "llama.cpp",
	baseUrl: "http://127.0.0.1:8080/v1",
	auth: { apiKey: { name: "llama.cpp server", login, check, resolve } },
	getModels, getAllModels, refreshModels, stream, streamSimple, classify,
}
```

`check` 返回 `{ type: "api_key", source: "stored credential" | "LLAMA_BASE_URL" }` 或 `undefined`；
`resolve` 返回 `{ auth: { apiKey, baseUrl }, env: { ...credential.env, LLAMA_BASE_URL }, source }`；
`refreshModels` 的 `persist` 为 `{ models: [...chat, ...classifiers], checkedAt: <epoch ms> }`。

### 4.10 RPC / JSON 模式

本单元不直接定义 RPC 方法；但它注册的四个工具（`codemode`、`tool_search`、三个 `*_mcp_resource*`）
在 `--mode rpc` 下与其他工具一样通过工具调用通道暴露，参数 schema 见 §2.11.1、§2.15.8、§2.5.
`/mcp`、`/llama` 在非 TUI 模式下的行为见 §3.1（`/mcp` 打印状态文本，`/llama` 拒绝）。

---

## 5. 跨单元接口（精确类型）

### 5.1 本单元消费的外部类型（必须存在且字段一致）

**来自 `kiana-mcp`（单元 `telemetry-mcp`）**：

```ts
// 值
McpClient, StdioTransport, StreamableHttpTransport,
JSON_RPC_ERROR_CODES, McpAuthRequiredError, McpError, McpHttpError, McpSessionExpiredError,
toLlmContent,
// 类型
AuthProvider, CallToolResult, ContentBlock, ListResourcesResult, ListResourceTemplatesResult,
McpRequestOptions, Tool as McpTool, McpTransport, ReadResourceResult, Resource,
ResourceTemplate, McpFetch,
// @earendil-works/kiana-mcp/oauth
McpOAuthAuthorizationRequiredError, McpOAuthProvider, authorizeMcp, OAuthCallbackServer,
parseWwwAuthenticate, stepUpScope,
AuthorizationServerMetadata, McpOAuthState, McpOAuthStateStore, OAuthCallback, OAuthCallbackPage,
OAuthChallenge, OAuthClientInformationMixed, OAuthClientMetadataDocument,
```

Rust：`kiana_mcp` crate（`telemetry-mcp` 单元）必须暴露等价的 `McpClient`、`Transport` trait、
`CallToolResult`/`ContentBlock`/`Tool`/`Resource`/`ResourceTemplate` 结构与 OAuth 类型；
本单元不复制其协议实现。

**来自 `kiana-codemode`（单元 `agent-codemode`）**：

```ts
// 主入口
CodemodeSandbox, loadQuickJSWasm, parseCodemodeSource, renderToolSample, toCodemodeIdentifier,
CodemodeOutputItem, CodemodeResult, CodemodeTool, CodemodeJsonSchema,
// /declarations
renderToolOutputType, renderToolSample, toCodemodeIdentifier,
// /source
CODEMODE_SOURCE_GRAMMAR,
// /worker（副作用导入）
"@earendil-works/kiana-codemode/worker"
```

逐字 import（`codemode/tool.ts` 与 `codemode/execute.ts` 的并集）：

```ts
import type { CodemodeJsonSchema, CodemodeTool } from "@earendil-works/kiana-codemode";
import { renderToolOutputType, renderToolSample, toCodemodeIdentifier } from "@earendil-works/kiana-codemode/declarations";
import { CODEMODE_SOURCE_GRAMMAR } from "@earendil-works/kiana-codemode/source";
import { type CodemodeOutputItem, type CodemodeResult, CodemodeSandbox, type CodemodeTool,
	loadQuickJSWasm, parseCodemodeSource, renderToolSample, toCodemodeIdentifier } from "@earendil-works/kiana-codemode";
```

注意 `renderToolSample` 与 `toCodemodeIdentifier` 在**两个**入口都有导出，本单元两处都用（`tool.ts` 用 `/declarations`，
`execute.ts` 用主入口）；`readCodemodeStore`/`executeCodemode` 是本单元自己导出、不是 kiana-codemode 的符号。

`CodemodeSandbox` 构造参数（本单元传入，Rust 侧 `CodemodeSandbox::new(...)` 必须接受同样字段）：

| 字段 | 本单元取值 |
|---|---|
| `tools` | `Vec<CodemodeTool>`：`{ name, description: <sample>, execute(args, {signal}) }`，`spread` 标志 |
| `globals` | 发现全局 3 个 + 模型全局 5 个（均 `spread: true`） |
| `timeoutMs` | `max(1, round(deadline - waitedMs))` |
| `memoryLimitBytes` | `268435456` |
| `wasm` | `loadQuickJSWasm(getQuickJSWasmPath())` |
| `workerUrl` | `getCodemodeWorkerSpecifier()` |
| `acquireWorker` | 返回 `release` 回调；第二次调用抛 `"The script already settled"` |

`CodemodeResult`：`{ ok: true, output: CodemodeOutputItem[], storeWrites: { set, delete }, value? }`
或 `{ ok: false, error: { kind: "script"|"timeout"|"aborted"|"sandbox", name?, message, stack? }, output }`；
`CodemodeOutputItem` = `{ type:"text", text, console?: boolean } | ImageContent`。
`sandbox.execute(code, { signal, store })`；`sandbox.close()` 幂等。

**来自 `kiana-ai`（`ai-core`）**：

```ts
// 来自 "llama/provider.ts"（注意：JsonValue 不在 provider.ts，而在 mcp/tools.ts 与 mcp/resources.ts）
AnyModel, ApiKeyCredential, AuthContext, AuthResult, ClassifierModel, isModelType, Model,
Provider, ProviderStreamOptions, RefreshModelsContext,          // @earendil-works/kiana-ai
llamaCppClassifyApi,                                            // @earendil-works/kiana-ai/api/llama-cpp-classify.lazy
typesafeSystemOneApi,                                           // @earendil-works/kiana-ai/api/typesafe-system-one.lazy
stream, streamSimple,                                           // @earendil-works/kiana-ai/compat（值，不是类型）
// 来自 "codemode/execute.ts"
AnyModel, ClassifierContext, ImageContent, ImagesContext, ModelType, ModelTypeMap, TextContent, Usage,
// 来自 "mcp/oauth.ts"
oauthErrorHtml, oauthSuccessHtml,                               // @earendil-works/kiana-ai/utils/oauth-page
// 来自 "mcp/tools.ts" / "mcp/resources.ts"
ImageContent, JsonValue, TextContent,
```

对应 import 语句（逐字，供交叉核对）：

```ts
import type { AnyModel, ApiKeyCredential, AuthContext, AuthResult, ClassifierModel, isModelType, Model,
	Provider, ProviderStreamOptions, RefreshModelsContext } from "@earendil-works/kiana-ai";
import { llamaCppClassifyApi } from "@earendil-works/kiana-ai/api/llama-cpp-classify.lazy";
import { typesafeSystemOneApi } from "@earendil-works/kiana-ai/api/typesafe-system-one.lazy";
import { stream, streamSimple } from "@earendil-works/kiana-ai/compat";
import { oauthErrorHtml, oauthSuccessHtml } from "@earendil-works/kiana-ai/utils/oauth-page";
```

注意 `isModelType` 与 `stream`/`streamSimple` 是**值**（运行时使用），其余为类型；
`ModelTypeMap` 只在 `execute.ts` 的 `CodemodeModelRuntime`/`toModelType` 用到。

Rust：`kiana_ai` crate 需提供 `Provider` trait（含 `get_models`/`get_all_models`/`refresh_models`/`stream`/
`stream_simple`/`classify`/`auth.api_key.{login,check,resolve}`）、`ClassifierApi`（`llama-cpp-classify`、
`typesafe-system-one`）、`Model`/`ClassifierModel` 结构与 `Usage`。

**来自 `kiana-tui`（`tui-core` / `tui-text` / `tui-components`）与 `ca-interactive-components`**：

```ts
// kiana-tui（tui-core / tui-text / tui-components）
Component, Container, Focusable, Input, SelectItem, SelectList, Spacer, Text, TUI,
truncateToWidth, visibleWidth, fuzzyFilter,   // fuzzyFilter only in llama/ui.ts
// ca-interactive-components
DynamicBorder (modes/interactive/components/dynamic-border),
AuthUrlComponent (modes/interactive/components/auth-url),
keyHint (modes/interactive/components/keybinding-hints),
VisualLinePreview (modes/interactive/components/visual-truncate),
// ca-tools-compaction-export / ca-config-models
Theme, getSelectListTheme, highlightCode (modes/interactive/theme/theme),
KeybindingsManager (core/keybindings.ts),
```

**来自 `ca-resources`（`core/mcp-servers.ts`）**：

```ts
type McpExposure = "codemode" | "deferred" | "direct" | "hidden";
interface McpServerConfig { /* 见 §4.2 */ }
interface McpStdioServerConfig / McpHttpServerConfig / McpOAuthConfig
mcpNamespace(server: string): string            // `mcp__${server.replace(/-/g, "_")}`
validateMcpServerConfig(name, config): McpServerConfig | string
getMcpToolExposure(config, toolName): McpExposure
createToolNameMatcher(pattern): (name: string) => boolean
LIST_MCP_RESOURCES_TOOL = "list_mcp_resources"
LIST_MCP_RESOURCE_TEMPLATES_TOOL = "list_mcp_resource_templates"
READ_MCP_RESOURCE_TOOL = "read_mcp_resource"
isMcpToolName(name): boolean
interface RegisteredMcpServer { name: string; config: McpServerConfig; extensionPath: string }
class McpServerRegistry
```

**来自 `ca-extensions` / `ca-session` / `ca-config-models`**：

```ts
// ca-extensions
ExtensionAPI, ExtensionFactory, ExtensionContext, ExtensionToolContext, ExtensionCommandContext,
ExtensionUIContext, ToolDefinition, ToolExposure, ToolNamespace, ToolLoadout, ToolLoadoutChanges,
ToolInfo, ToolAnnotations, McpServersChangeEvent, RegisteredCommand, ToolRenderers, ToolRenderContext,
// ca-session
AgentTool, AgentToolResult, AgentToolCallOutcome (all @earendil-works/kiana-agent-core，仅类型),
SessionEntry, SessionManager,
// ca-config-models
ModelRegistry, Settings, CodemodeMode, SETTINGS_DEFAULTS, ProjectTrustStore, FileAuthStorageBackend,
InMemoryAuthStorageBackend, AuthStorageBackend,
combineUsage,                     // core/usage-totals.ts（codemode/execute.ts 合并 models.* 的 usage）
```

**来自 `ca-tools-compaction-export`**：

```ts
formatSize, truncateMiddle,       // core/tools/truncate.ts
formatToolCallWithArgs, getTextOutput, replaceTabs, startToolTimer, toolTimerText, str,  // render-utils
writeOutputFile,                  // utils/output-files.ts
stripBom,                         // utils/text.ts（ca-tools-compaction-export §2.28.4）
wrapToolDefinition,               // core/tools/tool-definition-wrapper.ts（ca-tools-compaction-export §2.15）
                                  //   仅 codemode/tool.ts 的 createCodemodeTool 使用：把 ToolDefinition 包成 AgentTool，
                                  //   并显式透传 name/label/description/parameters/outputSchema/constrainedSampling/
                                  //   prepareArguments/executionMode/execute（execute 的 ctx 参数可选，缺省时由 ctxFactory 生成）
```

**来自 `ca-interactive-components` / `ca-cli` / `ca-config-models`**：

```ts
DynamicBorder, AuthUrlComponent, keyHint, VisualLinePreview,   // modes/interactive/components/*
theme, Theme, getSelectListTheme, highlightCode,              // theme/theme.ts（ca-tools-compaction-export §5.6）
app.message.copy / AuthUrlComponent.copy()                      // 剪贴板（tui-components）
// ca-cli
APP_NAME ("kiana"), CONFIG_DIR_NAME (".kiana"), VERSION, getAgentDir(), getDocsPath(),
getQuickJSWasmPath(), getCodemodeWorkerSpecifier(), resolveCodemodeWorkerSpecifier(runtime, url),
openBrowser,                       // utils/open-browser.ts
// ca-config-models
resolveConfigValueOrThrow, resolveHeadersOrThrow,   // core/resolve-config-value.ts §2.11
```

### 5.2 本单元对外提供的接口（其它单元依赖）

| 消费者 | 符号 | 用途 |
|---|---|---|
| `ca-cli`（`main.ts`） | `loadMcpCommand()` → `runMcpCommand(args, {cwd, agentDir})` | `kiana mcp …` 子命令 |
| `ca-cli`（`main.ts:578`，`extensionFactories`） | `builtInExtensions` 中的四个工厂：`llamaExtension`（default）、`createCodemodeExtension()`、`createToolSearchExtension()`、`createMcpExtension()` | 内建扩展 |
| `ca-session`（系统提示） | `MCP_SERVERS_SECTION`、`renderServersSection` | `mcp_servers` section |
| `ca-session`（工具） | codemode/tool_search/MCP 工具的 `ToolDefinition` | 注册 |
| `src/index.ts` 重导出 | `createCodemodeExtension`、`CodemodeExtensionOptions`、`createMcpExtension`、`McpExtensionOptions`、`McpTransportFactory`、`createToolSearchExtension` | SDK 用户直接构造内建扩展 |
| 本单元内部（不经 `src/index.ts`） | `codemodeRenderers`（`codemode/renderer.ts`）、`createMcpToolRenderers(label)`（`mcp/tools.ts`） | 由 `codemode/tool.ts`、`mcp/index.ts` 自身使用；`createMcpToolRenderers` 亦被 §7.14 回归测试直接导入 |
| `ca-session`（session entries） | `CODEMODE_STORE_ENTRY_TYPE`（`"codemode-store"`，`codemode/tool.ts`）、`readCodemodeStore` | 会话恢复时重建 `store()` 数据 |
| 其它 | `normalizeLlamaServerUrl`、`llamaInferenceUrl`、`formatBytes`、`LLAMA_PROVIDER_ID`、`DEFAULT_LLAMA_SERVER_URL`、`createLlamaProvider` | llama.cpp Provider |
| 其它 | `tokenize`、`Bm25Ranker`、`createToolSearchDocument`、`DEFAULT_TOOL_SEARCH_LIMIT` | 检索 |

### 5.3 事件契约（本单元注册的处理器）

| 事件 | 处理器位置 | 返回值/副作用 |
|---|---|---|
| `session_start` | `mcp/index.ts`、`llama` 无 | 载入配置、建连接、`ensureDiscoveryActive`、`reportProblems` |
| `before_agent_start` | `mcp/index.ts` | 写/删 `sections["mcp_servers"]` |
| `tool_call` | `mcp/index.ts` | 等待相关服务器就绪（**不**拦截调用） |
| `turn_start` | `mcp/index.ts` | 检测跨进程登录并重连 |
| `mcp_servers_change` | `mcp/index.ts` | 增删扩展注册的服务器并连接 |
| `session_shutdown` | `mcp/index.ts` | 中止、关闭所有连接、等待后台动作 |

`tool_call` 事件形状（本单元只用这三个字段）：

```ts
{ type: "tool_call", toolName: string, input: unknown, /* … */ }
```

`ToolInfo`（`kiana.getAllTools()` 元素）：`Pick<ToolDefinition, "name"|"description"|"parameters"|
"promptGuidelines"|"reachesTools"> & { exposure: ToolExposure; namespace?: ToolNamespace;
annotations?: ToolAnnotations; sourceInfo: SourceInfo }`。

### 5.4 Rust 侧 trait 边界建议

```rust
// kiana-mcp（telemetry-mcp 单元）
pub trait McpTransport: Send + Sync { /* send/recv JSON-RPC */ }
pub struct McpClient { /* … */ }
pub struct CallToolResult { pub content: Vec<ContentBlock>, pub structured_content: Option<Value>, pub is_error: Option<bool>, pub meta: Option<Value> }

// kiana-codemode（agent-codemode 单元）
pub struct CodemodeSandbox { /* … */ }
impl CodemodeSandbox {
    pub fn new(opts: CodemodeSandboxOptions) -> Self;
    pub async fn execute(&mut self, code: &str, opts: ExecuteOptions) -> CodemodeResult;
    pub async fn close(&mut self);
}
pub struct CodemodeSandboxOptions {
    pub tools: Vec<CodemodeToolDef>,
    pub globals: Vec<CodemodeToolDef>,
    pub timeout_ms: u32,
    pub memory_limit_bytes: usize,
    pub wasm: QuickJsWasm,
    pub worker_url: WorkerSpecifier,
    pub acquire_worker: Box<dyn Fn() -> ReleaseFn + Send + Sync>,
}

// kiana-ai（ai-core 单元）
pub trait Provider {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> &str;
    fn auth(&self) -> &ProviderAuth;
    fn get_models(&self) -> &[Model];
    fn get_all_models(&self) -> Vec<ModelRef>;
    fn refresh_models(&self, ctx: RefreshModelsContext) -> BoxFuture<'_, Result<()>>;
    /* stream / stream_simple / classify */
}
```

本单元在 Rust 中应实现：

```rust
pub trait ExtensionFactory { fn install(&self, kiana: &mut dyn ExtensionApi); }
pub trait ExtensionApi {
    fn register_tool(&mut self, def: ToolDefinition);
    fn register_tool_renderer(&mut self, resolver: Box<dyn Fn(&str) -> Option<ToolRenderers>>);
    fn register_command(&mut self, name: &str, cmd: RegisteredCommand);
    fn register_provider(&mut self, provider: Box<dyn Provider>);
    fn get_all_tools(&self) -> Vec<ToolInfo>;
    fn get_active_tools(&self) -> Vec<String>;
    fn set_active_tools(&mut self, names: Vec<String>);
    fn append_entry(&mut self, custom_type: &str, data: Value);
    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer>;
    fn get_settings(&self) -> &Settings;
    fn on(&mut self, event: ExtensionEvent, handler: Box<dyn Fn(Event) -> BoxFuture<'static, ()>>);
}
```

---

## 6. npm 依赖与 Rust crate 映射

### 6.1 直接依赖（本单元源文件实际 import 的包）

| npm 包 | 版本 | 用在哪 | 建议 Rust crate | 行为桥接（必须逐条处理） |
|---|---|---|---|---|
| `@earendil-works/kiana-mcp` | 1.4.0 | `runtime.ts`、`oauth.ts`、`tools.ts`、`resources.ts` | 内部 `kiana_mcp`（`telemetry-mcp` 单元） | 协议/传输/OAuth 算法由该单元实现；本单元只调 API |
| `@earendil-works/kiana-codemode` | 1.4.0 | `tool.ts`、`execute.ts` | 内部 `kiana_codemode`（`agent-codemode` 单元） | QuickJS 沙箱与 prelude 由该单元负责 |
| `@earendil-works/kiana-ai` | 1.4.0 | `llama/provider.ts`、`codemode/execute.ts` | 内部 `kiana_ai` | Provider/模型/Usage 类型一一对应 |
| `@earendil-works/kiana-tui` | 1.4.0 | `mcp/ui.ts`、`llama/ui.ts`、`codemode/renderer.ts` | 内部 `kiana_tui` (`tui-core` + `tui-components`) | `SelectList` 布局、`fuzzyFilter`、`truncateToWidth`/`visibleWidth`、`Input`、`DynamicBorder` |
| `@earendil-works/kiana-agent-core` | 1.4.0 | 仅类型（`AgentTool`、`AgentToolResult`、`AgentToolCallOutcome`） | 内部 `kiana_agent_core` | 类型 |
| `typebox` | 1.3.27 | `tool.ts`（schema）、`tool-search`、`resources.ts`、`mcp/tools.ts`（`TSchema`） | `serde_json::Value` + `schemars`（构造）或手写 `Value` | **仅需产出等价的 JSON Schema**；`Static<>` 类型在 Rust 用结构体 |
| `chalk` | 6.0.0 | `mcp/cli.ts`（`HELP`、`HELP_HINT`） | 内部 `kiana_tui::style`（已有 ANSI 层）或 `anstyle`/`owo-colors` | chalk 6 默认检测 TTY/`NO_COLOR`/`FORCE_COLOR`；`chalk.dim(x)` 产生 `\x1b[2m…\x1b[22m`，`chalk.bold` → `\x1b[1m…\x1b[22m`，嵌套时关闭序列合并（`\x1b[22m` 同时关 bold+dim）。必须与现有 `kiana_tui` 的颜色层使用同一开关策略 |
| `proper-lockfile` | 4.1.2 | `oauth.ts`（刷新锁） | `fd-lock` / 自写 lockfile（`.lock` 目录 + mtime 心跳） | **协议级兼容**：proper-lockfile 用 `<path>.lock` **目录**、`mtime` 心跳、`stale` 判定、`retries` 退避、`realpath:false`；Rust 侧需读写同样的目录与 mtime，才能与 TS 版**跨进程互操作**（同一 `~/.kiana/agent`）。若只要求 Rust↔Rust，可用 `fd-lock`，但文档要求行为一致 → 建议复刻目录锁 |
| `node:crypto` `createHash("sha256")` | — | `tools.ts`（名称哈希）、`oauth.ts`（锁文件名） | `sha2` | 取 `hex()[..8]`（工具名）/`hex()[..16]`（锁名），小写十六进制 |
| `node:fs` / `node:fs/promises` | — | config/log/oauth/huggingface | `std::fs` + `tokio::fs` | `readFile("utf8")` 的 BOM 处理（`stripBom`）、`writeFileSync(mode)`、`mkdirSync(recursive, mode: 0o700)`、`renameSync` 覆盖语义 |
| `node:path` | — | 多处 | `std::path` | Windows 分隔符与 `join`/`resolve` 语义（`getAgentDir()` 已是绝对路径） |
| `node:os` `homedir()` | — | `huggingface.ts` | `dirs`/`home` | `$HOME` → 平台默认 |
| `node:readline/promises` | — | `cli.ts`（粘贴回调 URL） | `tokio::io::BufReader` + stdin | `question(prompt, {signal})` 输出到 **stderr**；中止时 reject（被捕获为 `undefined`） |
| `node:timers/promises` `setTimeout` | — | `runtime.ts` | `tokio::time::sleep` | 可中止 |
| `node:url` `pathToFileURL`、`new URL` | — | `runtime.ts`、`client.ts`、`oauth.ts` | `url` crate | **WHATWG URL**：默认端口省略、路径归一、百分号编码集合、IDN/punycode、`toString()` 精确格式 |
| `undici`（间接 `fetch`） | 8.10.2 | 所有 HTTP | `reqwest`（rustls-tls） | `AbortSignal.timeout(15000)` = 总超时（含连接+读取）；`AbortSignal.any` = 先到先 abort；`response.json()` 解析失败吞掉 |
| `quickjs-wasi`（间接） | 3.6.2 | `execute.ts` 通过 `kiana-codemode` | 见 `agent-codemode` | 256 MiB 堆上限、`InternalError: out of memory` 文本 |
| `globals: fetch`（Node 内建） | — | `runtime.ts`/`oauth.ts`（kiana-mcp 内） | `reqwest` | 见 `telemetry-mcp` |

### 6.2 间接依赖（通过 `kiana-mcp`/`kiana-codemode`/`kiana-ai`）

| npm 包 | 影响本单元的行为 |
|---|---|
| `cross-spawn` | stdio 传输的进程启动（Windows `.cmd` 解析） |
| `undici` | HTTP 传输、SSE、代理、`ProxyAgent` |
| `@silvia-odwyer/photon-node` | 图片缩放（本单元只在 codemode `image()` 的产物上间接涉及） |
| `highlight.js` | `highlightCode(code, "javascript")`（codemode 调用渲染） |
| `shiki`/主题 | 否 |

### 6.3 必须显式桥接的 JS 语义（按影响面排序）

1. **UTF-16 索引**：`truncate`、`truncateText`、`truncateOutput` 的 `slice`、`length`，
   `renderToolSection` 的 `cost` 计算，`selectCatalog` 的预算，`describeValue` 的键数，
   `formatCall` 的 `args.slice(0, 77)`，`compactCount` 无关。
   Rust：`text.encode_utf16().count()` 与按码元切片；**不能**用 `str::chars`。
2. **`localeCompare`**：服务器排序、菜单排序、命名空间排序、量化名排序、模型 id 排序。
   JS 默认 `localeCompare` 走 ICU 的 **root/default locale** 排序（大小写、数字串、重音）。
   Rust：`icu_collator`（`Collator::try_new("und")`）或 `rust_icu`；若只用 `str::cmp`，
   `server2` vs `server10`、`A` vs `a` 的结果可能不同（**测试会捕获**）。
   若无法引入 ICU，可用与 ICU root 一致的简化实现并在测试中锁定（`docs`/`later` 等 ASCII 小写场景一致）。
3. **`JSON.stringify`**：
   - 2 空格缩进的 `mcp.json`/`mcp-auth.json`/`--json` 输出（`serde_json::to_string_pretty` 的
     缩进与空格一致，但**数组/对象空值**格式必须一致：`{}`、`[]`，JS 相同）。
   - 键顺序：整数键升序在前，其余插入序 → 需 `serde_json` 的 `preserve_order`（`IndexMap`），
     并对**纯数字键**做额外排序（`IndexMap` 不自动排序整数键）。`mcpServers` 内的服务器名一般非数字，
     但 `toolExposure`/`env`/`headers` 可能出现数字键。
   - `undefined`：JS 对象中 `undefined` 值省略、数组变 `null`；`JSON.stringify(undefined)` → `undefined`。
     Rust 用 `Option` + 省略。
   - 数字：`1e+21`、`-0`→`0`、`1.0`→`1`。Rust `serde_json` 的 f64 打印与 JS 不同（如 `1e21` vs `1e+21`），
     涉及 `mcp.json` 的 `callbackPort` 等整数字段无碍；若出现浮点需自定义序列化。
4. **`Number()`/`Number.isInteger`/`Number.isSafeInteger`**：
   - `Number(process.env.KIANA_CODEMODE_MAX_WORKERS)`：`""`→0、`" 2 "`→2、`"2.0"`→2、`"0x10"`→16、
     `"1e2"`→100、`"Infinity"`→Infinity、`"abc"`→NaN。Rust 需实现 ECMAScript `StringToNumber`
     （至少上述子集）。
   - `Number.isSafeInteger` = 整数且 `|x| <= 2^53-1`。
   - `Number("") === 0` 影响 `--oauth-callback-port ""`、`--timeout ""`。
5. **`toFixed`/`toPrecision`**：`formatBytes`（1~2 位小数）、`formatCost`（`toFixed(2)`/`toPrecision(2)`）、
   `formatDuration`（`toFixed(1)`）、`wallTime`（`toFixed(1)`）、`compactCount`、进度百分比。
   JS `toFixed` 用**四舍五入到最近、平局取偶**的十进制表示（实际为正确舍入的十进制）。
   Rust `format!("{:.1}")` 亦是正确舍入但**半值取偶**与 JS 一致；`toPrecision(2)` 需自行实现
   （例如 `0.0123` → `0.012`、`0.000123` → `0.00012`、`1.5e-7` → `1.5e-7`）。
6. **`Math.round`**：`.5` 向 +∞（`Math.round(-0.5)` = `-0`）。Rust `f64::round` 远离零 → 需 `(x + 0.5).floor()`。
   影响 `Math.round(ms)`、`Math.round(context/1000)`、`Math.round(ratio*100)`、`Math.round(filled)`。
7. **`String.prototype.trim`/`trimEnd`/`trimStart`**：JS 的空白集合 = `WhiteSpace + LineTerminator`
   （含 NBSP U+00A0、BOM U+FEFF、U+2028/U+2029）。Rust `str::trim` 用 `char::is_whitespace`（不含 U+FEFF）。
   `truncate()` 的 `trimEnd()` 与 `normalizeLlamaServerUrl` 的 `trim()` 受影响 → 需自定义 `js_trim`。
8. **正则方言**：
   - `/^mcp__(.+?)__(.+)$/`：非贪婪 → Rust `regex` 同样（左最先）。注意 `.` 在 JS 不匹配 `\n`（无 `s` 标志），
     Rust 同样。
   - `/([a-z0-9])([A-Z])/g`、`/([A-Z]+)([A-Z][a-z])/g`：全局替换；Rust `Regex::replace_all` 语义等价，
     但**替换串** `"$1 $2"` 在 Rust 为 `"$1 $2"`（相同写法）。
   - `/[^a-z0-9]+/` 与 `/^\s+/` 等：JS `\s` ⊃ Rust `\s`（见 7）。
   - `/(ches|shes|sses|xes|zes)$/`：等价。
   - `/^[aeiou]/`：等价。
   - `/^[^/\s]+\/[^:\s]+(?::[^\s:]+)?$/u`：等价。
   - `/-\d{5}-of-\d{5}$/u`、`/(?:^|[-_.])((?:UD-)?(?:IQ\d(?:_[A-Z0-9]+)+|Q\d(?:_[A-Z0-9]+)+|BF16|F16|F32|MXFP\d(?:_[A-Z0-9]+)*))$/iu`：
     `u` 标志影响 surrogate 处理（对 ASCII 无差别）；`i` 需 Unicode 大小写不敏感（`(?i)`）。
   - `/\b(searchTools|...)\b/`：JS `\b` 基于 `\w` = `[A-Za-z0-9_]`（非 Unicode）→ Rust 需 `[A-Za-z0-9_]`
     边界（`regex::Regex` 的 `\b` 默认 Unicode → 需 `(?-u:\b)` 或改写成显式字符类）。
   - `/^Script (completed|failed)\nWall time [\d.]+ seconds\nOutput:\n$/`：注意 `$` 在 JS 不匹配末尾换行前
     （无 `m`），Rust `$` 默认匹配文本末尾或末尾 `\n` 之前 → 需 `\z` 或 `(?-m:$)` 校验。**这是易错点**。
   - `/\r?\n/`（tool_search 描述第一行）：等价。
   - `/^([ \t]+)\S/m`：`m` 标志 + `.` 不含换行（用 `\S` 无影响）→ Rust `(?m)^([ \t]+)\S`。
   - `/\/+$/u`、`/\/v1$/u`：等价。
   - `/^[^/\s]+\/.../`：见上。
   - `/(?:^|;)t=(\d+)/u`：等价。
9. **`Object.keys`/`Object.entries` 顺序**：见 3。
10. **`Array.prototype.sort` 稳定性**：V8 稳定 → Rust 用 `sort_by`（稳定）。
11. **`Set`/`Map` 迭代顺序**：插入序 → `IndexSet`/`IndexMap`（或 `Vec`+`HashSet`）。
12. **`String.prototype.replaceAll`**：字符串模式替换全部出现（`stage.replaceAll("_"," ")`、`name.replaceAll("_"," ")`、
    `text.replaceAll("\r\n","\n")`）；Rust `str::replace` 等价；**replacement 中的 `$`** 在字符串模式不特殊
    （`replaceAll` 用字符串搜索）→ Rust `str::replace` 相同。
13. **`Date.toISOString()`**：`YYYY-MM-DDTHH:mm:ss.sssZ`（毫秒固定 3 位，年份 4 位，超出范围抛错）。
    Rust `chrono` 的 `to_rfc3339_opts(SecondsFormat::Millis, true)` 一致。
14. **`Buffer.from(data, "base64")`**：标准 base64（含 `+/`），非法字符忽略、缺失填充容忍。
    Rust `base64::engine::general_purpose::STANDARD` 对非法字符**报错** → 需宽松解码器。
15. **`TextDecoder` 流式解码**：SSE chunk 边界可能切断多字节字符 → Rust 需 `String::from_utf8` 的增量状态
    （`encoding_rs::Decoder` 或自写 carry-over buffer）。
16. **`performance.now()`**：单调毫秒浮点；`elapsed` 用于 `toFixed(1)` 的 wall time。
17. **`AbortSignal.reason`**：默认 `DOMException("This operation was aborted","AbortError")`，
    `controller.abort(new Error("Cancelled"))` 时为该 Error。`loadAndWait` 抛 `signal.reason ?? new Error("Cancelled")`。
18. **`Error.message`/`Error.name`**：`isConnectionError` 拼 `${error.name} ${error.message}` 并小写匹配
    `fetch failed`/`timeout`/`network`。Rust 需保留错误来源名称（如 `reqwest::Error` 的 "error sending request for url ..."）。
    这意味着 Rust 侧必须把网络错误的文本映射成 TS 版形状，否则 `/llama` 的连接错误识别会失效。
19. **`structuredClone`/深拷贝**：`snapshot()` 的 `calls.map(c => ({...c}))` 是浅拷贝；`calls` 元素字段是原始值
    （字符串/数字/枚举）→ 浅拷贝足够。
20. **`WeakRef`/GC**：无。

### 6.4 依赖清单（Rust `Cargo.toml` 建议）

```toml
[package]
name = "kiana-builtin-extensions"
edition = "2021"

[dependencies]
kiana-ai = { path = "../kiana-ai" }
kiana-agent-core = { path = "../kiana-agent-core" }
kiana-mcp = { path = "../kiana-mcp" }
kiana-codemode = { path = "../kiana-codemode" }
kiana-tui = { path = "../kiana-tui" }
kiana-resources = { path = "../kiana-resources" }   # mcp-servers.ts
kiana-session = { path = "../kiana-session" }
tokio = { version = "1", features = ["rt-multi-thread","macros","time","sync","io-std","fs","process"] }
tokio-util = { version = "0.7", features = ["rt"] }         # CancellationToken
reqwest = { version = "0.12", default-features = false, features = ["json","stream","rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
url = "2"
sha2 = "0.10"
hex = "0.4"
base64 = "0.22"
regex = "1"
futures = "0.3"
async-trait = "0.1"
thiserror = "2"
chrono = { version = "0.4", features = ["serde"] }
indexmap = { version = "2", features = ["serde"] }
unicode-width = "0.2"
fd-lock = "4"                # 或自写 lockfile 目录锁
dirs = "5"
tracing = { version = "0.1", optional = true }
icu_collator = "1"           # localeCompare 等价（可选，见 §8）
```

---

## 7. 测试清单（逐文件）

Rust 侧应对应 `#[test]`/`#[tokio::test]`，并保留断言中的**逐字文本**。全部测试位于
`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/test/`。

### 7.1 `mcp-extension.test.ts`（868 行，29 个 `it`）

**`describe("MCP config")`**

| 测试 | 断言的行为 |
|---|---|
| `merges global and trusted project servers and validates entries` | 合并顺序、项目覆盖、`scope`/`source` 字段、校验错误进 `errors` |
| `lets project entries override enabled and exposure of global servers` | 覆盖仅限 `enabled`/`exposure` 等可覆盖键（`OVERRIDE_KEYS`） |
| `reads and edits mcp.json files that start with a UTF-8 BOM` | BOM **剥离后可读**（`loadMcpConfig` 无错误、两个服务器都在）；`addMcpServerConfig` 写回后文件以**无 BOM** 的 `{` 开头，故可直接 `JSON.parse(readFileSync(..., "utf8"))`（**不是**「保留 BOM」） |
| `rejects server names that differ only in - and _` | 命名空间冲突校验 |
| `validates exposure and reads autoEnableCodemode with project precedence` | `exposure` 取值、`autoEnableCodemode` 项目优先 |
| `validates the OAuth callback URL, scope, and client name` | OAuth 字段校验文本 |
| `resolves per-tool exposure from exact names, then patterns in order` | `getMcpToolExposure` 精确名优先、模式按插入序 |
| `validates provider auth and accepts it only in the global mcp.json` | `auth` 仅全局允许 |

**`describe("MCP tools")`**

| 测试 | 断言 |
|---|---|
| `creates provider-safe tool names` | `mcp__docs__search`、`mcp__my_server__get_item_v2`、64 字符 + `_<8hex>`、冲突加哈希 |
| `declares the CallToolResult scripts receive…` | `createMcpResultSchema` 的 `$ref` 重定位（`#/$defs/Hit~1v1`、`#/properties/hits`、`#` 自引用、`#/definitions/Hit`、远程 `$ref`）+ `renderToolOutputType` 逐字输出 |
| `converts results, passing the CallToolResult to scripts and flagging errors` | `convertMcpResult` 的 `content`/`isError` 处理 |
| `points resource links to read_mcp_resource and saves binary resources` | `[Resource …]` 文本、二进制落盘 |
| `cuts the middle of model-facing text over 20KB and keeps the full result for scripts` | `limitMcpContent` 头部/尾部与 `[Full output: …]` |
| `cuts multi-byte text only at character boundaries` | UTF-8 边界安全 |

**`describe("MCP connections")`**（运行时状态机）

| 测试 | 断言 |
|---|---|
| `starts a new session and retries once when the session expired` | `McpSessionExpiredError` → 重建会话并重试一次 |
| `connects to servers without the tools capability without listing tools` | 无 `tools` 能力 → 不调 `listTools` |
| `marks a dropped connection and reconnects on the next call` | `disconnected` + 下次调用重连 |
| `retries HTTP connections that fail with a transient error` | 瞬时错误重试 |
| `closes a connection that is still initializing before close returns` | `close()` 等待初始化完成 |
| `stops waiting to retry a connection when closed` | 关闭中止重试等待 |
| `retries resource reads, but not tool calls, after a transient HTTP error` | 资源读重试、工具调用不重试 |
| `asks OAuth servers that keep rejecting requests for a new sign-in` | 401 循环 → `needs-auth` |
| `sends the provider token and asks for the provider login when the server rejects it` | provider token 与 `providerToken` 回调 |
| `appends server log messages to the log file` | `formatMcpLogMessage` 落盘 |
| `resolves the OAuth client secret lazily` | `clientSecret` 的 `${NAME}`/`!cmd` 延迟解析 |
| `connects to servers with resources without listing them` | `hasResources` 能力探测 |

**`describe("MCP servers section")`**（3 个）

| 测试 | 断言 |
|---|---|
| `lists servers with how their tools are reached and the first line of their description` | 逐字 3 行（见 §2.9.5），`direct` 排除 |
| `shortens descriptions to fit the size limit` | 40 台 × 400 字符 → ≤ 4096、41 行、含 `- mcp__server39 (codemode): x` |
| `leaves out the last servers when their names alone do not fit` | 200 台 → 末行为 `- … N more servers; find their tools with searchTools()`，且 `lines-2+omitted = 200` |

### 7.2 `mcp-command.test.ts`（197 行，8 个 `it`）

| 测试 | 断言 |
|---|---|
| `lists servers with their state, tools, and errors, and fails while anything is wrong` | 逐字 `fixture: connected, 1 tool (codemode, global)`、`  tools: echo`、`broken: failed (codemode, global)\n  kiana-test-missing-mcp-server\n  spawn kiana-test-missing-mcp-server ENOENT`、`parked: disabled (codemode, global)`、`config error: `、`server "bad" needs either "command"`；退出码 1 / 0 |
| `prints JSON for scripts` | JSON 结构、`servers[].state/tools`、退出码 |
| `rejects unknown servers and servers without OAuth for login and logout` | `No MCP server named "…"`、非 OAuth 文本 |
| `adds stdio servers and passes options after the command through` | `--` 之后参数原样进 `args` |
| `adds HTTP servers and keeps other content of the file` | `headers` 含 `Authorization: "Bearer ${DOCS_TOKEN}"`、`exposure`、`description`；文件其它内容保留；`oauth.clientId/clientName`；提示 `If it requires sign-in: kiana mcp login sentry` |
| `rejects invalid add invocations without writing` | 8 种非法调用 → 退出码 1 且**不创建** `mcp.json` |
| `adds and removes project servers` | `-l`/`--local` 写 `.kiana/mcp.json`；未信任提示；`It is defined in <path>; use --local.`；删除后 `{ "mcpServers": {} }` |
| `removes global servers` | 保留其它服务器与顺序；`No global MCP server named "broken"` |

### 7.3 `mcp-oauth-store.test.ts`（3 个 `it`）

| 测试 | 断言 |
|---|---|
| `keeps separate credentials for servers sharing a SERVER_URL` | 键含名字 → 同 URL 两个账号互不影响；`remove` 后 `tokens` 仍可取 |
| `moves credentials stored by SERVER_URL to the first server that loads them` | `tokens()` **不**迁移；`load()` 迁移并删除旧键；`-`/`_` 视为同名 |
| `signs out of credentials stored by SERVER_URL` | `remove` 删旧键；再次 `remove` 返回 `false` |

### 7.4 `mcp-oauth-refresh.test.ts`（265 行，11 个 `it`）

| 测试 | 断言 |
|---|---|
| `refreshes once when several processes find the same token rejected` | 跨进程刷新锁 → 只刷新一次 |
| `waits for a running refresh to save the new tokens` | 等待其它进程写入后复用 |
| `rejects an authorization response from another issuer` | `iss` 不匹配 → 拒绝 |
| `keeps the granted scope when the server asks for more` | `stepUpScope` 保留原 scope |
| `uses the configured authorization server metadata URL` | `authServerMetadataUrl` |
| `registers dynamically by default, even when the server supports documents` | 默认 DCR |
| `uses kiana's document when authorization responses name their issuer` | CIMD（`iss` 存在） |
| `uses a document and callback path specific to the MCP server without the iss parameter` | 每服务器独立回调路径 |
| `replaces a registered client when switching to the document` | DCR → CIMD 切换替换客户端 |
| `accepts the authorization response only on the server-specific redirect URI` | 回调 URI 严格匹配 |
| `fails instead of registering when the server does not support documents` | 无 CIMD 支持时失败，不回落 DCR |

### 7.5 `mcp-manager-view.test.ts`（2 个 `test`）

| 测试 | 断言 |
|---|---|
| `cancels the running operation with the cancel key` | `status("Sign in to issues","Contacting the authorization server…", onCancel)` 后渲染含 `cancel`；`ESC` 调用 `onCancel` 一次（#10565） |
| `ignores the cancel key for operations that cannot be cancelled` | 无 `onCancel` 时渲染**不含** `cancel`，`ESC` 无副作用 |

### 7.6 `codemode-renderer.test.ts`（5 个 `it`）

| 测试 | 断言 |
|---|---|
| `hides the script header and shows the output` | 结果不含 `Script completed`/`Wall time`，含脚本输出 |
| `shows the cost of model calls and their total` | `$0.012` 等格式；`Model calls: $` 汇总 |
| `shows results without a header, such as rejected options` | 无 `SCRIPT_HEADER` 时保留内容 |
| `limits collapsed output to wrapped lines, not logical lines` | 折叠 5 个**视觉行**（单行长 JSON） |
| `shows the elapsed time while the script runs and the total once it finished` | `Elapsed …` / `Took …` |

`render()` 辅助：宽度 200、`stripVTControlCharacters`、每行 `trimEnd`、整体 `trim`。

### 7.7 `codemode-worker-config.test.ts`（3 个 `test`）

| 测试 | 断言 |
|---|---|
| `uses a relative source entrypoint in Bun binaries` | `resolveCodemodeWorkerSpecifier("bun-binary", "file:///B:/~BUN/root/config.js")` → `"./src/extensions/codemode/worker.ts"` |
| `uses the emitted worker beside the bundled Node module` | `"bundled-node"` → `new URL("file:///app/chunks/codemode-worker.js")` |
| `uses the kiana-codemode worker when unbundled` | `"unbundled"` → `undefined` |

（`resolveCodemodeWorkerSpecifier` 属 `ca-cli`，但测试在本单元目录下 → 移植时归属 `ca-cli`。）

### 7.8 `codemode-worker-admission.test.ts`（173 行，4 个裸 `it` + `1 个 it.each` 三个参数，共 5 处声明）

逐字模板：`it.each(["timeout", "abort", "success"] as const)("bounds surviving workers after %s", …)`。

| 测试 | 断言 |
|---|---|
| `it.each(["timeout","abort","success"])("bounds surviving workers after %s")` | 两次运行后，幸存 worker 不超过上限；槽位在 worker **真正退出**后才释放（#10748） |
| `starts no worker for a launch that resumes after its result timed out` | `acquireWorker` 在结果 settled 后抛 `"The script already settled"` |
| `queues a concurrent burst behind two workers` | 并发突发按 FIFO 排队，最多 2 个活跃 |
| `reads the limit from KIANA_CODEMODE_MAX_WORKERS` | 设为 1 时串行 |
| `ignores an invalid KIANA_CODEMODE_MAX_WORKERS` | 非法值回落 2 |

### 7.9 `llama-extension.test.ts`（744 行，15 个 `it`）

| 测试 | 断言 |
|---|---|
| `registers a native provider and /llama command` | 命令描述 `Manage llama.cpp router models`；provider id `llama.cpp` |
| `normalizes management and inference URLs` | `http://127.0.0.1:8080/v1/` → `http://127.0.0.1:8080`；`https://example.com/prefix/v1` → `https://example.com/prefix`；`file:///tmp/llama` → 抛 `http or https` |
| `exposes loaded and sleeping models with router metadata` | 只列 `loaded`/`sleeping`；`baseUrl` `…/v1`；`contextWindow`/`maxTokens`；`input` 含 image |
| `discovers chat-template thinking support for loaded models` | `/props?model=qwen&autoload=false`；`enable_thinking` → `reasoning` + `thinkingLevelMap` + `thinkingFormat` |
| `persists and restores selectable models for cache-only startup refreshes` | `context.stored` 恢复 + `publish({update})`；不联网 |
| `preserves cached llama.cpp context for unloaded autoload presets` | 未加载 preset 沿用缓存 `contextWindow` |
| `exposes unloaded presets only when router autoload is enabled` | `/props.models_autoload` 决定是否列出 |
| `classifies with selectable models through llama-server` | classifier 模型 `llama-cpp-classify` |
| `exposes decision models reported by the catalog as native System One classifiers` | `output_modalities` 含 `decisions` → `typesafe-system-one` + `baseUrl .../v1` |
| `lists decision models that also output text for chat` | 同时含 `text` 时也作为 chat 模型 |
| `hides unloaded presets when router autoload is disabled` | `models_autoload` false → 不列 preset |
| `stays dormant until configured and stores URL plus optional key` | `check` 无 URL → `undefined`；`login` 存 `env.LLAMA_BASE_URL` 与 key |
| `searches Hugging Face and reads quantizations plus access requirements` | 搜索、`gated`、量化（`Q4_K_M` 优先、大小排序、`size` 缺失） |
| `loads with SSE progress and waits for the loaded catalog state` | `loadAndWait` 的进度消息与 250 ms 轮询 |
| `downloads with byte progress and returns the refreshed catalog` | `downloadAndWait` 的 `Downloading model`、`KiB/MiB` 详情、`reload=1` |

### 7.10 `tool-search.test.ts`（126 行，9 个 `it`）

**`describe("tokenize")`**

| 测试 | 断言 |
|---|---|
| `splits camelCase and snake_case, drops stop words, and folds plurals` | 驼峰/下划线/停止词/复数 |

**`describe("Bm25Ranker")`**

| 测试 | 断言 |
|---|---|
| `ranks by term relevance and respects the limit` | 顺序与 `limit` |
| `returns nothing for unknown or empty queries` | `kubernetes`、`the`（全停止词）、`tickets`（无同义词）→ `[]` |
| `includes the namespace in the search text` | 命名空间描述参与检索 |

**`describe("codemode description catalog")`**

| 测试 | 断言 |
|---|---|
| `lists everything without a budget` | `Nested tools:`、`## mcp__github\nGitHub server`、`## mcp__docs\n\n### \`mcp__docs`、`find unlisted tools, such as MCP tools`、`await searchTools(query, { limit?, namespace? })`、`await describeTool(name)`、`await describeNamespace(name)`（#10555） |
| `fills the budget round-robin, cheapest first, and says what is missing` | 预算 170：`read_notes` 在、`mcp__docs (some tools not listed)`、`mcp__docs__search` 在、`mcp__docs__long` 不在；**确定性**（两次输出相同） |
| `leaves deferred tools and their namespaces out entirely` | `deferred` 集合中的工具与命名空间完全不出现；与剔除后输入输出**完全相同** |
| `leaves namespace instructions out` | `## mcp__github\n\n### \`mcp__github__a\``（instructions 不进描述） |
| `lists only namespaces with a zero budget` | `inlineBudget: 0` → 只列命名空间标题，无工具段 |

### 7.11 `suite/agent-session-mcp.test.ts`（1273 行，44 个裸 `it` + 4 个 `it.each` = 48 处声明，集成）

覆盖（分组名 + 关键断言）：

- **工具过滤**：`--tools` 不含 MCP 时保留、`--no-tools` 全删、`--tools` 的 `mcp__` 前缀过滤、
  `--exclude-tools` 模式、声明与否。
- **exposure 路由**：`codemode` 工具可被脚本调用而对模型隐藏；跨 `/tree` 导航仍可调用；
  `-`/`_` 名称互认（双向 `it.each`）；模型直接调用 codemode-only 工具被拒；`direct` 声明给模型。
- **资源工具**：`list_mcp_resources`/`list_mcp_resource_templates`/`read_mcp_resource`；
  codemode 服务器资源可从脚本调用。
- **per-tool exposure 覆盖**、**服务器描述与 instructions**、**autoEnableCodemode=false**、
  **经 active tool_search 可达**、**deferred 经 codemode 可达**、**两者都不可用时的警告**、
  **脚本对 direct 工具的解析说明**、**激活其它扩展的 codemode 工具（含 `reachesTools` 判定）**、
  **不激活同名但无 `reachesTools` 的 codemode**。
- **时序**：不因 codemode 服务器连接阻塞首个 prompt；连接后 codemode 描述不变；连接前先列出服务器、
  下一 prompt 补摘要；`direct` 服务器在列服务器前等待；仅 `direct` 的服务器不出现在 section；
  脚本命名的服务器等待；脚本标识符与名称不同时也等待；未命名不等待；`tool_search` 前等待；
  首个 prompt 等待 `direct` 服务器到 `startupWaitMs`；codemode 不能调用自身/未激活 direct 工具。
- **检索**：`searchTools()`/`describeTool()`；`tool_search` 激活 deferred 并在分支上保留；已声明时无可加载。
- **扩展注册的服务器**：加载期注册即连接；会话中注册/注销；`mcp.json` 优先于注册（`it.each(["my-docs","my_docs"])`）；
  拒绝别的扩展已注册的名字；无扩展连接时报告。
- **resume/reload**：`tool_search` 已加载的工具在服务器重连后重新声明；`it.each` 多场景；
  `/reload` 后重新声明；下一 prompt 开始后才注册的工具不激活。

**四个 `it.each` 的逐字模板**（文件里 44 个裸 `it(` + 4 个 `it.each(` = 48 处声明；写作 `44 个 it` 指**裸 it** 计数）：

| 位置 | 模板 | 参数 |
|---|---|---|
| L409 | `routes tools whose names differ only in - and _ (reverse: %s)` | `[false, true]`（`reverse`） |
| L463 | `withdraws and restores %s MCP tools the server changes` | `["direct", "codemode"]` |
| L1093 | `prefers the mcp.json server over a registered %s` | `["my-docs", "my_docs"]` |
| L1227 | `%s restored tools when an extension sets the loadout before they register` | `[["drops", ["read"], false], ["keeps", undefined, true]]` |

后两个分属 `describe("AgentSession MCP servers registered by extensions")` 与
`describe("AgentSession MCP tools after resume and reload")`（三个 describe：
`AgentSession MCP integration`、`… servers registered by extensions`、`… tools after resume and reload`）。

### 7.12 `suite/agent-session-mcp-oauth.test.ts`（9 个 `it`）

| 测试 | 断言 |
|---|---|
| `signs in through the browser, refreshes expired tokens, and signs out` | PKCE 流程、刷新、登出 |
| `accepts a pasted redirect URL when the browser cannot reach the callback` | 粘贴回调 URL |
| `cancels a sign-in waiting on <path> when the session shuts down` | 关闭会话取消登录 |
| `closes the session without refreshing an expiring token` | 关闭不刷新 |
| `uses the configured callback URL and scope` | `callbackUrl`/`scope` |
| `registers with the configured client name` | `client_name` |
| `adds the listening port to a callback URL without one` | 无端口时补监听端口 |
| `gives up on kiana mcp login after --timeout, also while the authorization server hangs` | `--timeout` 对挂起的授权服务器也生效 |
| `uses credentials from kiana mcp login on the next turn` | 跨进程凭据复用 |

### 7.13 `suite/agent-session-codemode.test.ts`（25 个 `it`）

| 测试 | 断言 |
|---|---|
| `presents callable tools per codemode.mode` | `on` 与 `only` 的描述差异、`hiddenDeclarations` |
| `shows the guidelines of tools that do not fit the inline budget through describeTool()` | `describeTool()` 仍含 guidelines |
| `runs nested calls in parallel and returns only the script result` | 并行、只有脚本输出进上下文 |
| `routes nested calls through extension hooks` | `tool_call`/`tool_result` 钩子生效 |
| `adds the usage of nested results to the codemode result` | `usage` 合并 |
| `keeps structured content that tool_result handlers replace along with the content` | `structuredContent` 替换 |
| `attaches only the images the script passes to image(), in output order, each after its saved path` | 图片顺序、路径在前 |
| `marks where each text item starts and puts console lines last in one text block` | `==> text i/N <==`、`<console_output>` |
| `reports script failures as results that keep partial output and the calls that ran` | `Script error:` + 调用摘要 |
| `applies the timeout_ms option and rejects invalid options` | `timeout_ms`、非法选项 |
| `limits script memory so runaway allocations fail inside the script` | 256 MiB |
| `truncates output to the token budget and spills the full text` | 截断头/尾、`[Full output: …]` |
| `formats output at the item limit in linear time` | 线性时间（#10678） |
| `resolves bash calls to structured results, also for non-zero exit codes` | `outputSchema` → `structuredContent` |
| `resolves read calls to text for text files and to image blocks that image() shows` | 文本/图片 |
| `persists store() writes as custom entries for later calls` | `codemode-store` |
| `appends nothing for failed scripts or scripts without writes` | 无写入不追加 |
| `runs without a session, starting from an empty store` | 无 ctx |
| `loads the values written on the current branch` | 分支隔离 |
| `folds store entries from the root, ignoring malformed data` | 折叠与跳过 |
| `declares models only for the session's own codemode tool` | `models` 命名空间 |
| `lists models and classifies with catalog auth, ignoring script-supplied fields` | 忽略脚本 `baseUrl`/`headers` |
| `generates images with catalog auth and attaches them through image()` | 图片 |
| `notes generated images that the script did not show` | `Note: models.generateImages() returned …` |
| `reports provider errors as results and invalid arguments as exceptions` | provider 错误 → 结果；参数错误 → 异常 |

### 7.14 `suite/regressions/10285-mcp-tool-renderers.test.ts`（2 个 `it`）

| 测试 | 断言 |
|---|---|
| `renders calls to MCP tools that are not registered` | 正则 `/^mcp__(.+?)__(.+)$/` 回退渲染（#10285） |
| `renders them in HTML exports too` | HTML 导出同样可渲染 |

### 7.15 `test/mcp-conformance/`（非 vitest，外部一致性套件）

- `README.md`（45 行，2590 B）要点：
  - 跑法：`npm run test:mcp-conformance`；CI 在 `.github/workflows/ci.yml` 的 `mcp-conformance` job。
    首次需联网用 `npx` 取固定版本 `@modelcontextprotocol/conformance@0.2.0-alpha.11`；**禁用 install scripts**；
    需要 POSIX shell。
  - 机制：对每个协商到的协议版本（`2025-03-26`、`2025-06-18`、`2025-11-25`），`run.ts` 逐个跑套件的 client scenario；
    套件起 scenario server，把 `client.ts` 打上去。`client.ts` 用 kiana 对 `mcp.json` HTTP 服务器跑的那套代码：
    `McpServerConnection` 连接并调工具，`signInMcpServer` 跑 `/mcp` 触发的 OAuth 登录；模拟浏览器取授权 URL 并把
    redirect 送到 kiana 的 loopback 回调；服务器要求再次登录（例如要更多 scope）时模拟用户**最多再登录三次**。
  - 比对：套件报告的每个 check 与 `baseline.json` 比；额外的 `kiana-client` check 记录 `client.ts` 是否跑完该 scenario。
    某些 scenario 期望客户端放弃（`auth/scope-retry-limit`），也照常进 baseline。
  - **失败条件三条**：baseline 里通过的 check 失败或缺失；某个 check 失败但 baseline 未把它列为失败；
    baseline 里的 scenario 没跑。开始通过的 check 会被报告（应更新 baseline 锁定）。
  - 选项：`--mode`、`--scenario`（均可重复）、`--verbose`（打印套件输出与 client 日志）、
    `--keep-results`（保留套件的 `checks.json` 与 client 输出）、`--update-baseline`（把完整跑的结果写回
    `baseline.json`，需人工 review，不要用它掩盖回归）。README 给出的逐字示例：

    ```bash
    node packages/coding-agent/test/mcp-conformance/run.ts --mode 2025-11-25 --scenario auth/scope-step-up --verbose
    node packages/coding-agent/test/mcp-conformance/run.ts --update-baseline
    ```
  - **已知失败（必须与 baseline 一起保留）**：`elicitation-sep1034-client-defaults`（kiana 不支持 elicitation）、
    `auth/basic-cimd`（kiana 用动态注册而不是 CIMD）、`auth/scope-retry-limit`（`kiana-client` 按设计失败）。
  - **`2026-07-28` 未覆盖**：它是无状态协议，kiana 未实现。
- `run.ts`（365 行，12832 B）、`client.ts`（200 行，7795 B）、`baseline.json`（413 行，10763 B）。
- Rust 侧：若保留该套件，需要一个可执行的 `client` 二进制（或以 `--mcp-conformance-client` 模式运行），
  输出与 `baseline.json` 相同的 `checks.json`。**建议**在 Rust 迁移中保留为可选的 `xtask`。
  三个已知失败与 `2026-07-28` 的缺席在 Rust 侧也必须保持"已知/未覆盖"，而不是悄悄变成通过。

### 7.16 测试基础设施

| 文件 | 作用 |
|---|---|
| `test/suite/mcp-oauth-server.ts`（182 行，7447 B） | 测试用授权服务器（发现、DCR、token、PKCE 校验） |
| `packages/mcp/test/fixtures/stdio-server.mjs`（1.0 KB） | `kiana mcp list` / `mcp-command.test.ts` 用的 echo 服务器 |
| `packages/mcp/test/fixtures/stubborn-server.mjs`（861 B） | 只被 `packages/mcp/test/stdio.test.ts` 使用（忽略 stdin EOF 与 SIGTERM 的服务器），**不属于** coding-agent 的测试面，但 `telemetry-mcp` 的关闭语义测试依赖它 |
| `packages/mcp/src/testing/index.ts` | 逐字：`export { createInMemoryTransportPair, InMemoryTransport } from "../transports/in-memory.ts";`（只此一行） |

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 crate 与模块

新增 crate `kiana-builtin-extensions`（`kiana/crates/kiana-builtin-extensions`），或按现有工作区习惯并入
`kiana-coding-agent` 的 `extensions` 模块。推荐独立 crate，依赖 `kiana-mcp`、`kiana-codemode`、`kiana-ai`、`kiana-tui`、
`kiana-resources`、`kiana-session`。

```text
crates/kiana-builtin-extensions/
├── Cargo.toml
└── src/
    ├── lib.rs                    // 四个工厂与导出
    ├── mcp/
    │   ├── mod.rs                // create_mcp_extension、builtInExtensions 中的注册
    │   ├── config.rs             // LoadedMcpConfig、load/update/add/remove、edit_mcp_servers
    │   ├── log.rs                // format_mcp_log_message、McpServerLog
    │   ├── tools.rs              // to_tool_exposure、create_mcp_tool_name、limit_mcp_content、
    │   │                         //   convert_mcp_result、create_mcp_result_schema、renderers
    │   ├── resources.rs          // 三个资源工具
    │   ├── runtime.rs            // McpServerConnection 状态机、transport 工厂
    │   ├── oauth.rs              // McpOAuthCredentialStore、sign_in_mcp_server、锁
    │   ├── ui.rs                 // McpManagerView、show_mcp_manager
    │   ├── manager.rs            // 菜单（servers_menu/server_menu/show_tools/choose_exposure）
    │   ├── section.rs            // render_servers_section、describe_state、attention_rank
    │   └── cli.rs                // run_mcp_command、HELP、parse_options、add/remove/list/login
    ├── codemode/
    │   ├── mod.rs                // create_codemode_extension
    │   ├── tool.rs               // 描述生成、select_catalog、prepare_loadout、reaches
    │   ├── execute.rs            // worker 准入、execute_codemode、输出整形
    │   ├── globals.rs            // 发现全局与 models 全局
    │   └── renderer.rs           // codemode_renderers
    ├── tool_search/
    │   ├── mod.rs                // create_tool_search_extension
    │   ├── bm25.rs               // tokenize、stem、Bm25Ranker、create_tool_search_document
    │   └── tool.rs               // tool_search 工具定义
    ├── llama/
    │   ├── mod.rs                // llama_extension
    │   ├── client.rs             // LlamaClient、normalize、format_bytes、SSE
    │   ├── provider.rs           // create_llama_provider、模型映射
    │   ├── huggingface.rs        // HuggingFaceClient、find_huggingface_token
    │   └── ui.rs                 // LlamaView、HuggingFaceSearch、run_with_progress
    ├── strings.rs                // 全部逐字文案常量（便于快照测试与审校）
    └── js/
        ├── mod.rs                // js_trim、js_to_fixed、js_to_precision、js_round、
        │                         //   utf16_len/slice、number_to_string、locale_compare
        └── regexes.rs            // 预编译正则（含 (?-u:\b)、(?-m:$) 修正）
```

### 8.2 关键类型

```rust
// ---- mcp ----
pub struct McpExtensionOptions {
    pub load_config: Option<Arc<dyn Fn(&ExtensionContext) -> LoadedMcpConfig + Send + Sync>>,
    pub create_transport: Option<McpTransportFactory>,
    pub credentials: Option<Arc<McpOAuthCredentialStore>>,
    pub log_path: Option<PathBuf>,
    pub open_url: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub update_config: Option<Arc<dyn Fn(&McpServerEntry, &McpServerConfigPatch) + Send + Sync>>,
    pub startup_wait_ms: Option<u64>,   // 默认 10_000
}

pub struct McpServer {
    pub entry: McpServerEntry,
    pub connection: Option<Arc<McpServerConnection>>,
    pub registered_config: Option<String>,
    pub message: Option<String>,
    pub attempt: Option<u64>,          // symbol 的替代：单调计数器
    pub ready: Option<SharedFuture<()>>,
    pub closing: Option<SharedFuture<()>>,
}

pub enum McpState { Starting, Connecting, Connected, NeedsAuth, Failed, Disconnected }

pub struct McpServerConnection {
    pub entry: McpServerEntry,
    state: RwLock<McpState>,
    error: RwLock<Option<String>>,
    tools: RwLock<Vec<McpTool>>,       // 注意：字段名 tools 与类型 McpTool 冲突 → 用 McpToolInfo
    instructions: RwLock<Option<String>>,
    has_resources: AtomicBool,
    oauth_url: RwLock<Option<Url>>,
    challenge: RwLock<Option<OAuthChallenge>>,
    // …
}

// ---- codemode ----
pub struct CodemodeNestedCall {
    pub id: String,
    pub name: String,
    pub args: String,
    pub status: NestedCallStatus,   // running | ok | error | cancelled
    pub duration_ms: Option<f64>,
    pub error: Option<String>,
    pub cost: Option<f64>,
}
pub struct CodemodeToolDetails { pub calls: Vec<CodemodeNestedCall>, pub full_output_path: Option<PathBuf> }

struct WorkerAdmission { active: AtomicUsize, waiters: Mutex<VecDeque<Waiter>> }
// 或 Tokio Semaphore + 自定义 FIFO（Semaphore 不保证 FIFO → 需自建队列）

// ---- tool-search ----
pub struct Bm25Ranker { pub k1: f64, pub b: f64 }
pub struct ToolSearchDocument { pub name: String, pub text: String }
pub struct ToolSearchMatch { pub name: String, pub score: f64 }

// ---- llama ----
pub struct LlamaModelInfo {
    pub id: String,
    pub aliases: Option<Vec<String>>,
    pub status: LlamaModelStatus,
    pub architecture: Option<LlamaArchitecture>,
    pub source: Option<String>,
    pub meta: Option<LlamaModelMeta>,
}
pub struct LlamaStatus {
    pub value: String,           // 用字符串以容忍未知状态
    pub args: Option<Vec<String>>,
    pub failed: Option<bool>,
    pub exit_code: Option<i64>,
    pub progress: Option<IndexMap<String, LlamaFileProgress>>,
}
pub struct LlamaClient { pub server_url: String, api_key: Option<String>, http: reqwest::Client }
pub struct LlamaProgress { pub message: String, pub ratio: Option<f64>, pub detail: Option<String> }
```

### 8.3 `localeCompare` 的 Rust 等价

TS 中 `a.localeCompare(b)` 无参数 = `Intl.Collator(undefined, {})`，使用运行环境的默认 locale（Node 通常
`en-US`/`root`）。其特性：

- 大小写：小写排在大写附近（`a` < `A` < `b`），不是码点序。
- 数字：`"server2"` < `"server10"`（按数字串比较，ICU root 的 numeric 默认 **false**；
  但 `en-US` 的默认 `numeric:false` → `"server10" < "server2"`！**需实测锁定**）。
- 标点：`-`/`_` 的权重低于字母。
- 重音：`é` 与 `e` 同主权重。

**移植策略**：先用一个小型 Node 脚本生成所需场景的预期顺序表（本单元涉及的服务器名/模型 id/量化名
都是 ASCII），再决定：

1. 若范围内的字符串全是 `[A-Za-z0-9._-]`，可用 ICU root 的近似实现：
   `primary: lowercase(base letter)`，再按 `-`/`_`/`.` 低权重、再按大小写、再按原串。
2. 若需严格一致，引入 `icu_collator` 并用相同 `locale: "en-US"` 与 `options`。
3. 测试里锁定：`tool-search.test.ts` 的名称排序、`mcp-extension.test.ts` 的 `server0..server39` 与
   `server-with-a-long-name-N`（含数字后缀 0..199，**会暴露 numeric 差异**）、`llama-extension` 的
   `id.localeCompare`、`huggingface` 的量化名排序。

**建议**：迁移时把 `localeCompare` 统一替换为 `js_locale_compare(a, b)`，其实现先查表（ASCII 快路径），
否则走 `icu_collator`；并新增一个 golden 测试文件固化 Node 的输出。

### 8.4 最难复现的部分（按风险排序）

1. **`codemode` 沙箱与 worker 准入**
   - 槽位必须在 **worker 真正退出** 后释放（不是脚本结果 settle 时）；`acquireWorker` 只调用一次，
     第二次抛 `"The script already settled"`。
   - 取消/超时/成功三条路径都不得泄漏槽位（测试 `bounds surviving workers after %s`）。
   - 等待上限 60 s，且等待时间计入 `timeout_ms`；超时/中止时生成的 `Script failed` 结果文本必须逐字。
   - QuickJS 256 MiB 堆上限与 `InternalError: out of memory` 文本由 `agent-codemode` 保证。
   - Rust 中若用线程池：`std::thread` + `crossbeam`，或用 `tokio::task::spawn_blocking` + 自建 FIFO 队列；
     **不要**用 `Semaphore`（不保证 FIFO，测试 `queues a concurrent burst behind two workers` 依赖到达顺序）。
   - 取消是协作式的：wasm 内死循环依赖 `interrupt handler`（`agent-codemode`），Rust 必须确保
     `close()` 不阻塞在死循环上（terminate + 不等退出）。

2. **输出整形流水线的精确顺序**
   `formatOutput` → 错误段 → 未展示图片提示 → `joinAdjacentText` → `truncateOutput` → `saveImages`
   → `joinAdjacentText` → 头部。任何一步顺序改变都会让 `truncateOutput` 的 token 计数、图片路径位置、
   `fullOutputPath` 归属产生差异。
   `joinAdjacentText` 的“检查上一段是否以 `\n` 结尾”必须只看最后一个非空段（避免 O(n²)，测试
   `formats output at the item limit in linear time`）。

3. **`renderServersSection` 的预算循环**
   - `size(kept)` 用 `join("\n").length`（UTF-16）；
   - `kept` 从全部递减；
   - `perServer = min(250, floor((4096 - size(kept)) / kept) - 2)`；
   - `truncate` 先截 `max-1` 再 `trimEnd` 再补 `…`；
   - omission 行单复数。
   三个测试分别锁定 ≤4096、行数、末行格式与 `lines-2+omitted = 200`。

4. **`createCodemodeDescription` 的轮转预算**
   - 组序：无命名空间组在前，其余 `localeCompare`；
   - 组内按 cost 升序（**稳定**排序，同 cost 保持声明序）；
   - 每轮每组一个；放不下的组**永久退出**；
   - 输出按原声明序过滤 `shown`；
   - `(tools not listed)` / `(some tools not listed)` 的判定用 `visible.length` 与 `entries.length`；
   - deferred 工具既不显示也不计入预算，且不影响描述（与剔除后输入**完全相等**）。
   测试 `fills the budget round-robin, cheapest first` 用 170 token 预算精确卡边界。

5. **MCP 工具名哈希与冲突**
   - `createMcpToolName`：sanitize → 截断 64 → `_<sha256(original).hex[..8]>`；
   - **同批 sanitize 结果重复者全部加哈希**（含本可保留裸名者），因此与列表顺序无关；
   - 名字已被其它工具占用（`toolOwners`）也加哈希；
   - 冲突检测发生在 `assignName` 时，需要 `current`（本批已分配）与 `plain`（本批 sanitize 结果）两个集合。
   测试 `creates provider-safe tool names` 锁定 `^mcp__server__x+_[0-9a-f]{8}$` 与长度 64。

6. **`createMcpResultSchema` 的 `$ref` 重定位**
   需要把服务器 `outputSchema` 的 JSON Schema 引用重写进 `structuredContent`：
   - `#/$defs/Hit~1v1` → 命名空间内引用；
   - `#/properties/hits` → 指向 `structuredContent.properties.hits`；
   - `#` → 自引用（递归终止）；
   - `#/definitions/Hit`；
   - 远程 `$ref` 原样保留；
   - **数据位置上形如 `$ref` 的值不得改写**（测试 `default: { $ref: "#/stays" }`）；
   - `renderToolOutputType` 的逐字输出（属性按字母序！`again, children, default, hits, remote`）。
   这是本单元最容易写错的一段（需要“按 schema 位置”而非“按值”遍历）。

7. **OAuth 跨进程刷新锁 + CIMD/DCR 选择**
   - `proper-lockfile` 的目录锁与 mtime 心跳必须与 TS 版互操作（同一 `~/.kiana/agent`）；
   - `stale: 20000`、`retries: 250 × 100ms`（25 s）、`onCompromised: noop`；
   - 锁路径 `<lockDir>/mcp-auth-refresh-<sha256(key).hex[..16]>`，proper-lockfile 实际创建同名 `.lock` 目录；
   - CIMD vs DCR 的判定（`iss` 参数、服务器是否支持文档）、回调路径的服务器特定化、
     `stepUpScope` 保留 scope；
   - **多个进程同时刷新只刷新一次**（测试 `refreshes once when several processes find the same token rejected`）。

8. **`whatwg URL` 归一**
   `normalizeLlamaServerUrl` 依赖 WHATWG 解析：`url.pathname = ...` 的赋值会触发百分号编码、
   `toString()` 省略默认端口、`http://h` → `http://h/`、IDN → punycode。
   Rust `url` crate 是 WHATWG 兼容但**细节有差**（如 `url.pathname` 的设置语义、`%` 编码集合）。
   测试 `normalizes management and inference URLs` 只覆盖 3 例，但 `LLAMA_BASE_URL` 可来自用户 →
   建议加 golden 测试（含 `http://h:80/`, `HTTP://H`, `http://h/a/../b`, `http://user:pass@h`, `http://[::1]:8080`）。

9. **SSE 解析（llama 与 MCP）**
   - `\r\n` → `\n`；只在 `\n\n` 处切帧；跨 chunk 的半帧要缓冲；
   - 只取 `data:` 行，`slice(5).trimStart()`；
   - 多行 `data:` 用 `\n` 连接；
   - JSON 解析失败**静默丢弃**；
   - 缺 `model`/`event` 字段丢弃；
   - `TextDecoder` 的流式 UTF-8（多字节跨 chunk）。
   MCP 侧的 SSE 由 `telemetry-mcp` 负责，但本单元的 `/llama` 进度依赖 llama 的 SSE 语义。

10. **`toFixed`/`toPrecision`/`Number` 解析**
    `formatBytes`（`1.50 KiB`、`10.0 MiB`）、`formatCost`（`$0.012`、`$1.23`）、
    `wallTime`（`0.0`）、`compactCount`（`1.5k`、`100k`、`1.2M`）、
    `--timeout` 的 `Number()`（`""` → 0 → 报错 `must be a positive number`）。
    Rust 需实现或引入 `ryu`/自定义 `js_number` 模块，并用快照测试锁定。

11. **`describeState`/`formatStatus` 的分支组合**
    状态 × 启用 × 资源 × 错误 的组合很多，且 `needs-auth` 分支**不**带工具数/错误、
    `disconnected` 有特殊文案、错误缩进 4/2 空格不同（`formatStatus` 4 空格，`list` 2 空格）。
    建议为每个分支写独立快照测试。

12. **MCP 生命周期竞态**
    - `attempt: symbol` 的等价物；迟到结果必须丢弃；
    - `closing` 与 `ready` 的 await 顺序；
    - `setEnabled(false)` 先解绑再 `close()`，并 `hideTools`；
    - `hideTools` 用 hidden 重新注册同一份定义（不能注销）；
    - `mcp_servers_change` 中“配置变了的扩展服务器”先移除再作为新服务器加入（`registeredConfig` 字符串比较）；
    - `session_shutdown` 后不得再 `ctx.ui.*`。
    Rust 中用 `tokio_util::CancellationToken` + `Arc<AtomicU64>` generation 计数。

13. **`startupWaitMs` 的“只等一次”语义**
    `waitedForStartup` 在 `session_start` 重置；`before_agent_start` 每次调用 `waitForDirectServers`，
    但只有 **第一次** 真正等待；超时提示 `MCP servers are still connecting; their tools become available once connected.`
    只发一次。

14. **`tool_call` 的等待选择**
    `reachesTools` 优先；资源工具等全部；MCP 工具按 `toolOwners` 的 `"<server>\0<tool>"` 取 owner；
    其它工具立即返回。`reachesServer("all")` 恒真；否则“词包含命名空间”。
    `waitForServers` 的循环要处理“等待期间新 `ready` 替换旧 `ready`”的情况。

15. **`/llama` 的取消竞态**
    `runWithProgress` 用 `Promise.race([settled, ui.progress()])`；确认取消后先 `options.cancel()`
    再 `controller.abort(...)`，然后 `await settled`；`completed` 标记防止重复确认。
    Rust 中 `select!` 需保证 `ui.progress()` 的 future 在每次循环重新创建（不能复用一个已完成的 future）。

16. **TUI 布局细节**
    `SelectList` 的视口/列宽、`frame()` 的边框与内边距、`HuggingFaceSearch` 的窗口滑动
    （`start = max(0, min(selected - 5, len - 10))`）、进度条 40 格、
    `LlamaView::render` 的 `visibleWidth > width → truncateToWidth(line, width, "")`。
    这些需要与 `tui-core`/`tui-components` 的实现严格对齐。

### 8.5 迁移顺序建议

1. `strings.rs` + `js/`（UTF-16、trim、数字格式化、locale compare 的 golden 测试）。
2. `tool_search`（纯函数，最容易验证）。
3. `mcp/config.rs` + `mcp/log.rs`（文件 I/O）。
4. `mcp/tools.rs` + `mcp/resources.rs`（依赖 `kiana-mcp` 类型）。
5. `mcp/runtime.rs` + `mcp/oauth.rs`（依赖 `telemetry-mcp`）。
6. `mcp/section.rs` + 菜单 + `mcp/cli.rs`。
7. `mcp/mod.rs` 生命周期。
8. `codemode/tool.rs`（描述）→ `codemode/execute.rs`（沙箱，依赖 `agent-codemode`）。
9. `codemode/renderer.rs`。
10. `llama/*`。

### 8.6 验收脚本建议

```
cargo test -p kiana-builtin-extensions
cargo test -p kiana-builtin-extensions --features snapshot   # 逐字快照
# 与 TS 对齐的端到端：
#   kiana mcp list --json   与  kiana mcp list --json   逐字节比较（同一 mcp.json）
#   kiana mcp add/list/remove 的文件写入逐字节比较
#   /llama 的 UI 快照（固定终端宽度 80/120）
#   codemode 渲染快照（宽度 200，strip ANSI）
```

---

## 附录 A：逐字字符串索引（按文件）

> 便于实现时集中校对。行号指 TS 源码（只读参考）。

### A.1 `mcp/index.ts`

- `"Manage MCP servers: sign in, reconnect, enable or disable, and change exposure"`
- `"MCP servers"`、`"MCP server "`（前缀 + 名字）、`"Tools of "`（前缀 + 名字）、`"Exposure of "`（前缀）
- `"No MCP servers configured. Add them to <agentDir>/mcp.json or .kiana/mcp.json."`
- `"global, project override"`、`"Applies to this session; the server is registered by <source>."`、
  `"Saved to <override ?? source>."`
- `"Enable"`、`"Enable in this project"`、`"Sign in"`、`"opens the browser"`、`"Tools"`、`"<n> offered"`、
  `"Reconnect"`、`"Sign out"`、`"deletes the stored credentials"`、`"Exposure"`、`"Disable"`、
  `"Disable in this project"`、`"select"`、`"back"`、`"manage"`、`"close"`、`"save"`
- `"This server is no longer configured."`、`"The server offers no tools."`
- `"State: <describeState(server,false)>"`
- `"saved to the project mcp.json"`、`"saved to the <scope> mcp.json"`、`"saved to mcp.json"`、`"for this session"`
- `"Exposure <exposure>: <unreachable|desc>"`、`"\nSome tools override it with toolExposure."`
- `"✓ "`、`"  "`
- `"Contacting the authorization server…"`、`"Connecting…"`、`"Sign in to <name>"`
- `"MCP servers need attention:\n…\nRun /mcp to fix."`
- `"disabled"`、`"starting"`、`"needs sign-in"`、`"failed: <first line>"`、`"connected · N tool(s)"`、
  `" · resources"`、`"connecting…"`
- `"codemode"`、`"deferred"`、`"direct"`、`"tool_search"`、`"codemode"`/`"tool_search"`（reaches 标签）
- `"MCP servers whose tools are not declared to you."`、`" Call the tools of \`codemode\` servers from codemode scripts."`、
  `" Load the tools of \`tool_search\` servers with \`tool_search\`."`
- `"- … N more server(s); find their tools with searchTools()"`

### A.2 `mcp/cli.ts`

见 §2.10 的表格与 `HELP` 全文。

### A.3 `mcp/tools.ts`

- `[Full output: <path> (read it with offset/limit)]`（**注意有 `it`**，与 codemode 的 `(read with offset/limit)` 不同）
- `[Resource <uri> "<title ?? name>"<details><description><read>]`
- `mcp__<server>__<tool>`、冲突后缀 `_<8hex>`
- 渲染器的 `[invalid arg]`、`... (N more lines, … to expand)` 等（见 §2.4）

### A.4 `codemode/tool.ts` / `execute.ts` / `renderer.ts`

见 §2.11–§2.13 的逐字块。

### A.5 `tool-search/tool.ts`

见 §2.15 的 `TOOL_SEARCH_DESCRIPTION`、`"No matching tools found."`、`"Loaded N tool(s). They are available from your next call:\n…"`、
`"query must not be empty"`、`"limit must be a positive integer"`。

### A.6 `llama/*`

见 §2.16–§2.20 的表格。

---

## Audit additions（对抗性完整性审计追加）

本节由独立审计追加：上面各节已**就地**修正的错误不在此重复；这里只补此前**缺失**的内容、逐字契约与交叉核对结果。
审计范围：`src/extensions/` 全部 25 个 `.ts`（7585 行，与 §1 的行数表逐文件核对一致）、`docs/{mcp,codemode,llama-cpp,environment-variables}.md`、
`test/` 下 10 个单元测试 + 5 个 suite 测试（含 `agent-session-tool-orchestration.test.ts`）+ `test/auth-url-copy.test.ts`
+ `test/suite/mcp-oauth-server.ts` + `test/mcp-conformance/` 四文件，
以及 `packages/mcp/{src/testing,test/fixtures}` 与 `core/{mcp-servers,usage-totals,tools/tool-definition-wrapper,extensions/types,nested-tool-calls}.ts` 的相关行。

### Audit-A.1 测试清单的交叉核对结果（行数 / 声明数全部实测）

| 文件 | 行数 | 声明数 | 备注 |
|---|---|---|---|
| `test/mcp-extension.test.ts` | 868 | 29 `it` | 与 §7.1 一致 |
| `test/mcp-command.test.ts` | 197 | 8 `it` | 一致 |
| `test/mcp-oauth-store.test.ts` | 55 | 3 `it` | 一致 |
| `test/mcp-oauth-refresh.test.ts` | 265 | 11 `it` | 一致 |
| `test/mcp-manager-view.test.ts` | 40 | 2 `test` | 一致 |
| `test/codemode-renderer.test.ts` | 130 | 5 `it` | 一致 |
| `test/codemode-worker-config.test.ts` | 21 | 3 `test` | 一致 |
| `test/codemode-worker-admission.test.ts` | 173 | 4 `it` + 1 `it.each`（3 参） | 见 §7.8 的模板补注 |
| `test/llama-extension.test.ts` | 744 | 15 `it` | 一致 |
| `test/tool-search.test.ts` | 126 | 9 `it` | 一致 |
| `test/suite/agent-session-mcp.test.ts` | 1273 | 44 `it` + 4 `it.each` | 模板见 §7.11 |
| `test/suite/agent-session-mcp-oauth.test.ts` | 273 | 9 `it` | 一致 |
| `test/suite/agent-session-codemode.test.ts` | 1032 | 25 `it` | 一致 |
| `test/suite/regressions/10285-mcp-tool-renderers.test.ts` | 72 | 2 `it` | 一致 |
| `test/suite/mcp-oauth-server.ts` | 182 | — | 辅助模块 |
| `test/mcp-conformance/{README.md,client.ts,run.ts,baseline.json}` | 45 / 200 / 365 / 413 | — | 字节数见 §7.15 |

**§7 之前漏列的两个测试文件**（都直接 import 本单元源码，Rust 侧必须一并移植）：

| 文件 | 声明数 | 与本单元的关系 |
|---|---|---|
| `test/auth-url-copy.test.ts`（60 行） | 3 `test` | 其中 `MCP sign-in screen copies the authorization URL` 直接用 `McpManagerView.redirectUrl()`：渲染含 `ctrl+x to copy`；按 `\x18` 后出现 `Copied URL to clipboard` 且 `copyToClipboard(URL)` 被调用一次。另两个测 `LoginDialogComponent`（不属本单元）。Rust 侧 `McpManagerView` 的登录屏必须保留 `ctrl+x` 复制键与两条文案 |
| `test/suite/agent-session-tool-orchestration.test.ts`（145 行） | 3 `it` | `registers codemode and tool_search inactive until they are named`：`getAllTools()` 含 `codemode`/`tool_search`，但 `getActiveToolNames()` 默认是 `["read","bash","edit","write"]`；`allowedToolNames: ["read","codemode"]` → `["read","codemode"]`；`initialActiveToolNames: ["tool_search"]` → `["tool_search"]`。另两个测 `ctx.executeTool` 的 `nestedCalls` 语义（见 Audit-A.3） |

`mcp-extension.test.ts` 顶层 `describe` **恰好 4 个**（L38 `MCP config`、L291 `MCP tools`、L464 `MCP connections`、
L827 `MCP servers section`），29 个 `it` 全部分布在这 4 组内；§7.1 已逐组列表，此处只补充计数约束
（`MCP servers section` 恰好 3 个测试）。

### Audit-A.2 `docs/mcp.md` 里此前缺失、且属于**行为契约**的条目

以下每条都在 §2.21.1 补入或此处补充，逐字文本请以 `docs/mcp.md` 为准。

1. **`codemode-deferred` 别名**：`McpExposure` 的校验值只有 4 个（`codemode`/`deferred`/`direct`/`hidden`），
   `codemode-deferred` 是 `codemode` 的旧名，`resolveExposureAliases` 在校验前归一；Rust 的枚举反序列化也必须接受别名，
   否则老配置会突然报 `exposure must be one of "codemode", "deferred", "direct", "hidden"`。
2. **`timeout` 与 progress 通知**：`timeout` 是**每请求**秒数（默认 60），来自服务器的 progress notification 会**重置**它。
   实现上见 `runtime.ts` 的 `timeoutMs` 与 `McpRequestOptions`；Rust 侧不能在超时后误判"服务器挂死"。
3. **`type` 的三个可接受值**：`stdio`、`http`、`streamable-http`（后两者等价），`sse` 明确拒绝并给出改用 `/mcp` 的提示。
4. **项目 override 的边界**：只含 `enabled`/`exposure`/`toolExposure` 的项目条目覆盖同名全局服务器，其余字段（含
   `env`/`headers`/`auth`）沿用全局——这正是 `isOverride()` 的三个 `=== undefined` 判断。
5. **`${NAME}` 与 `!command`**：`env`/`headers` 的值可用环境变量与命令，但 `!command` 必须占满**整个值**
   （`"Authorization": "!echo Bearer $(gh auth token)"`）；`oauth.clientSecret` 同样。
6. **`kiana mcp list` 的退出码语义**：任一条目非法或任一启用服务器未连接 → 退出码 **1**；全部正常 → 0。
   `/mcp` 显示完整连接错误与失败 stdio 服务器的 stderr 尾部（`runtime.ts` 保留 2000 字符 `STDERR_TAIL_CHARS`）。
7. **重试与重连的精确数字**：HTTP 网络错误/瞬时状态（408、429、5xx）**重试两次**（`CONNECT_RETRY_DELAYS_MS = [250, 1000]`，
   即首次失败后 250 ms、1000 ms 各一次）；**501 不重试**；资源读/列举在瞬时错误后重试**一次**；工具调用**不重试**。
   会话过期（`McpSessionExpiredError`）单独走"重建会话再试一次"的路径。
8. **stdio 关闭顺序**：关 stdin → `SIGTERM` → `SIGKILL`，**作用于进程组**（`killProcessTree`，见 `telemetry-mcp` §2.x 与
   `packages/mcp/src/transports/stdio.ts` 的 `process.kill(-pid, …)`）。文档承诺这能顺带停掉 `npx`/`uvx` 包装器。
9. **`mcp_servers` 节的追加语义**：节内容变化时（例如服务器连接后摘要才可用）Kiana 把**新节追加**到对话，而不改工具声明，
   以保住 prompt cache。这解释了 §7.11 里 `lists a server before it connects and appends its summary with the next prompt`。
10. **`describeNamespace()` / `searchTools({ namespace })` 的四种写法**：`mcp__dev-radius`、`mcp__dev_radius`、
    `dev-radius`、`dev_radius`。Rust 侧归一化用 `mcpNamespace(name)` 后比较，并对 `-`/`_` 互相替换。
11. **两种间接机制互通**：`codemode` 与 `deferred` 的工具**都能**被脚本调用**且**被 `tool_search` 加载；
    codemode 调用不依赖激活工具集（`/tree`、resume、fork 后仍可用），`tool_search` 加载的工具记录在 transcript 并在分支内继续声明。
12. **`kiana mcp list` 的 exposure 标记**：与服务器 exposure 不同的工具会被标出；`/mcp` 的 Tools 视图显示**生效** exposure。
13. **警告一次**：既无 `codemode` 也无 `tool_search` 激活、又存在非 direct 工具时，Kiana 警告一次（逐字见 §3；
    `warnedUnreachable` 在 `session_start` 重置）。
14. **20 KB 截断的标记逐字**：`…N chars truncated…`（中间移除），全文存临时文件；脚本侧拿完整结果。
15. **权限与 annotations**：所有 MCP 调用都走 Kiana 工具管线；**codemode 发起的调用带 `parentToolCallId`**；
    `kiana.getAllTools()` 报告 `readOnlyHint`/`destructiveHint`/`idempotentHint`/`openWorldHint`；资源工具标记 read-only。
16. **扩展/替换/SDK 三节**：`kiana.registerMcpServer(name, config)`；同名文件条目优先并列出被覆盖的注册；
    注册服务器的 enabled/exposure 改动**只影响当前会话**；`kiana mcp` shell 命令**不加载扩展**；
    注册 `/mcp` 的扩展**替换**内建（不再读 `mcp.json`）；`"extensions": ["-builtin:mcp"]` 或 `kiana config` 关闭内建；
    注册同名 `codemode`/`tool_search` 的扩展也替换内建，但只有声明 `reachesTools` 时才参与等待与激活；
    SDK 会话**不加载内建扩展**，需手动加入三个扩展。
17. **迁移表**：Claude/Cursor 复制、VS Code 的 `servers`→`mcpServers` 且 `${input:…}`→`${NAME}`、
    Codex TOML→JSON、OpenCode `type:local/remote` 与 `environment`→`env`、`{env:NAME}`→`${NAME}`。

### Audit-A.3 跨单元语义：codemode 嵌套调用在会话里的可见形状

`core/extensions/types.ts` 的 `ExtensionToolContext.executeTool()` 文档（逐字语义）：

- 调用 id 是 `<发起调用 id>/<n>`；`tool_call`、`tool_result`、`tool_execution_*` 事件都带 `parentToolCallId`；
- 该调用**不**出现在 transcript 里；其**有界**记录作为 `nestedCalls` 挂到**发起工具的结果消息**上；
- 永不因工具失败而 reject：未知工具、校验错误、被拦截、抛错都回 `isError: true`。

对应测试：`agent-session-tool-orchestration.test.ts` 的 `supports tools that call other tools under any name through the extension API`
（断言 `nestedCalls` 被持久化到 toolResult 消息）与 `leaves results without nested calls unchanged`
（无嵌套调用时结果上**没有** `nestedCalls` 键）。Rust 侧 `execute_codemode` 产出的 `CodemodeNestedCall[]`
必须走同一套 `parentToolCallId`/`nestedCalls` 通道，否则 `/tree` 恢复与 HTML 导出都会缺数据。

### Audit-A.4 `docs/codemode.md` 里此前缺失的数值与文案

（§2.21.2 已补全，这里只把**最容易漏的硬数值**集中列出，便于 CI 断言）

| 常量 | 值 | 出处 |
|---|---|---|
| `MAX_OUTPUT_CHARS` | `16 * 1024 * 1024` = **16777216** | `packages/codemode/src/runtime/prelude-source.ts:42` |
| `MAX_OUTPUT_ITEMS` | **100000** | 同上 :43 |
| `MAX_STORE_VALUE_CHARS` | `256 * 1024` = **262144** | 同上 :34 |
| `MAX_STORE_TOTAL_CHARS` | `1024 * 1024` = **1048576** | 同上 :35 |
| `max_output_tokens` 默认 | **10000** | `docs/codemode.md`（`// @options:` 一节） |
| `codemode.inlineBudget` 默认 | **3000** | `core/settings-defaults.ts:59` |
| worker 上限默认 | **2**，等待上限 **60000 ms** | `codemode/execute.ts:58,69` |
| 每脚本并发 `models.*` 调用 | **4** | `codemode/execute.ts:50` |
| VM 堆 | **256 MiB** = `268435456` | `execute.ts:56` |
| `SUPPORTED_FIELDS` | `max_output_tokens`、`timeout_ms`（`MAX_TIMEOUT_MS = 2147483647`） | `packages/codemode/src/source.ts` |

错误文案（`RangeError`）：
`script output exceeded the limit of 16777216 characters or 100000 text(), image(), and console calls. Print a summary instead, or write large data to a file with a tool.`
`store("<key>") value has <N> characters of JSON, more than the limit of 262144. <STORE_HINT>`
`store is full: stored values would exceed 1048576 characters of JSON. Delete keys with store(key, undefined). <STORE_HINT>`
其中 `<STORE_HINT>` = `store() is for small state such as IDs or summaries. Show images with image(), keep large data in variables, or write it to a file with a tool.`

文档还承诺的行为（Rust 侧必须在行为上一致，不只是数字）：
- 取消的 worker **在退出前仍占槽**；满载时按**到达顺序**等待；60 秒或 `timeout_ms` 先到者失败且**不运行脚本**。
- 等待永不 settle 的 promise（且无挂起工具调用）→ **立即失败**。
- Bun 上停止/deadline 只保证"及时结束调用"，不轮询 interrupt handler 的内建可能继续算。
- 失败脚本保留部分输出与已发生的调用；脚本结束时仍在跑的调用被取消；未 await 的 promise 丢弃。
- `classify()`/`generateImages()` 只用 `provider`+`id`，**不抛**（看 `stopReason`/`errorMessage`），每脚本 4 并发，usage 计入会话成本。

### Audit-A.5 `docs/llama-cpp.md` 里此前缺失的条目

§2.21.3 已补入；要点索引：

- router 启动命令行与 6 个选项含义；**不要** `--model`/`-m`/`-hf`；目录布局（单文件 / `mmproj` / `-0000N-of-0000M` 分片）；
  手工加文件后要重启 router。
- `/login llama.cpp` 只存连接（`--no-models-autoload` 时），`/llama` 加载、`/model` 选择。
- `/llama` 的下载由**服务器**执行，故服务器进程也要 `HF_TOKEN`；gated 仓库先警告并给访问页链接；
  搜索 token 查找顺序 `HF_TOKEN` → `$HF_TOKEN_PATH` → `$HF_HOME/token` → `$XDG_CACHE_HOME/huggingface/token` →
  `~/.cache/huggingface/token`；无凭据可搜但限流更低。
- 已加载其它模型时询问先卸载还是保留；**从不静默卸载**、**从不删除模型文件**；router 可能被共用，`/llama` 总显示当前状态。
- `/model` 含 sleeping（选中唤醒）与（autoload 时）未加载 preset；断线时 `/llama` 提供 **Retry**/**Close**，Retry **不重放**。
- 分类器：decision models（`/v1/systemone`、`typesafe-system-one`、**不在 `/model`**）；chat models 同 ID 也列为
  `llama-cpp-classify`；llama.cpp **0.6.0+** 用 `architecture.output_modalities` 含 `decisions` 报告（GGUF metadata，
  未加载也能识别），老构建会误列为 chat。chat-as-classifier 的 prompt：state → 所有 question → **state 再次** → 带标签的 question；
  标签字母 ≤**62** 选项、`Yes`/`No`、数字 ≤**10** 档；choice 的 confidence `(n * peak - 1) / (n - 1)`；
  每请求 `temperature` 在归一化前除 logits；question 串行且共享 prompt cache（state 占两倍上下文）；
  hybrid（Qwen3.5）需 `--ctx-checkpoints 32 --checkpoint-min-step 0`。
- 故障排查的 `curl /health`、`/models`；移除 provider 用 `"extensions": ["-builtin:llama.cpp"]`。

### Audit-A.6 Rust 映射的行为缺口（§8 之外的补充）

1. **`McpExposure` 必须是"带别名 + 保留原字面量"的枚举**：`type` 字段的 `streamable-http` 会在校验后保留在配置里
   （`validateMcpServerConfig` 原样返回 `value`），因此 `McpServerConfig.type` 在 Rust 里不能折叠成只有两个变体后
   再序列化回去，否则 `updateMcpServerConfig` 写回的文件会与 TS 版不同。
2. **`wrapToolDefinition` 的字段透传**：`createCodemodeTool()` 用 `wrapToolDefinition` 把 `ToolDefinition` 包成 `AgentTool`，
   显式透传 `name`/`label`/`description`/`parameters`/`outputSchema`/`constrainedSampling`/`prepareArguments`/`executionMode`/`execute`。
   任何漏掉的字段都会让 SDK 用户直接构造的 codemode 工具行为不同（例如丢掉 `constrainedSampling` 的 grammar）。
3. **`startupWaitMs` 只是首个 prompt 的上限**：`DEFAULT_STARTUP_WAIT_MS = 10_000`，测试里用
   `holds the first prompt for servers with direct tools only up to startupWaitMs` 精确卡边界；Rust 不能用整体超时替代。
4. **`renderServersSection` 的 `perServer` 可能为负**：`floor((4096 - size(kept)) / kept) - 2` 在极端情况下 ≤ 0，
   此时描述为空但**标题行仍在**；Rust 的整数除法必须向下取整（`i32::div_euclid`），且 `kept === 0` 时 `perServer = 0`。
5. **`attentionRank` 的 6 档**：`needs-auth` 0、`failed` 1、`disconnected` 2、其它未连接 3、`connected` 4、`disabled` 5；
   `/mcp` 的菜单按 `attentionRank` 升序、同档按 `localeCompare` 名字排序。
6. **`mcp_servers` 节的 `reaches` 值域**：只有 `codemode` 与 `tool_search` 两个字符串（`tool_search` 由 `deferred` 映射而来），
   `{ direct, … }` 的服务器被过滤掉；Rust 侧不要把它写成"exposure 原名"。
7. **`describeState` 的 `connected · N tool(s)[ · resources]`**：单复数与 ` · resources` 后缀逐字（§2.9.7 已有，
   但本审计发现 `failed:` 分支在 `withError=false` 时**只写 `failed`**，用于紧凑列表）。
8. **哈希细节（不是 HMAC）**：`createMcpToolName` 的哈希输入是 `` `${server}\0${tool}` ``（NUL 分隔，**不是** `server + tool`），
   取 `sha256(hex)[..8]`，截断到 `MAX_TOOL_NAME_LENGTH - 9` 个字符后接 `_<hash>`；`oauth.ts` 的刷新锁文件名用
   `sha256(key).hex[..16]`。两处都用**小写十六进制**。
9. **`env` 解析的时机**：`createDefaultTransport` 里 `env[key] = resolveConfigValueOrThrow(value, "MCP server \"<name>\" env \"<key>\"")`；
   `headers` 走 `resolveHeadersOrThrow`（同 §5.1）。Rust 侧必须在**每次建连接**时解析（不是加载配置时），
   这样 `/reload` 与 `!command` 的变化才能生效。
10. **`/mcp` 与 `/llama` 在非 TUI 模式**：`/mcp` 打印状态文本；`/llama` 直接 `notify("/llama is available in interactive mode", "warning")` 并返回，
    不进入界面（§2.20 已有，此处强调这是**用户可见契约**）。

### Audit-A.7 文档与源码不一致 / 需在 Rust 侧显式决策的两处

1. `docs/codemode.md` 说"tools 的方法名是工具标识符"，而 `describeNamespace()` 的别名四种写法是小写 `dev-radius` 等；
   两者不冲突，但 Rust 侧的 `toCodemodeIdentifier` 与 `describeNamespace` 归一化是**两套规则**，不要合并。
2. `docs/mcp.md` 的 `mcp_servers` 一节说 "Kiana updates the section when a prompt starts"，而 §2.9.5 的实现是
   每次 `before_agent_start` 重算、内容相同则不改；"追加"由 `ca-session` 决定。Rust 侧保持这一分工即可。

---

（审计追加完）
