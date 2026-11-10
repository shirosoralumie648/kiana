# `ca-extensions` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-extensions** 单元（扩展系统：公开扩展 API、
jiti 加载器、扩展运行时 `ExtensionRunner`、`Extension`/`InlineExtension` 对象模型、四种运行模式下的
UI 适配契约，以及 `examples/extensions/**` 兼容语料）的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-extensions`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）。
> `APP_NAME` = `kiana`，`APP_TITLE` = `π`，`CONFIG_DIR_NAME` = `.kiana`，`VERSION` = `1.4.0`
> （取自 `packages/coding-agent/package.json`，见 `ca-cli` §2.6）。
> `ENV_AGENT_DIR` = `KIANA_CODING_AGENT_DIR`；`getAgentDir()` 见 `ca-cli`。
> 所有源文件路径若无特别说明，均相对于 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/`。
> 本单元**不修改** TS 树中的任何文件。

---

## 目录

- [0. 单元边界与阅读顺序](#0-单元边界与阅读顺序)
  - [0.1 属于本单元的源文件](#01-属于本单元的源文件)
  - [0.2 本单元不实现、由其它单元拥有](#02-本单元不实现由其它单元拥有)
  - [0.3 与兄弟 spec 的边界协调](#03-与兄弟-spec-的边界协调)
  - [0.4 阅读顺序](#04-阅读顺序)
- [1. 概览](#1-概览)
  - [1.1 目的与在系统中的位置](#11-目的与在系统中的位置)
  - [1.2 三个对象层](#12-三个对象层)
  - [1.3 生命周期总览](#13-生命周期总览)
  - [1.4 入口点清单](#14-入口点清单)
- [2. 逐文件规范](#2-逐文件规范)
  - [2.1 `src/core/extensions/types.ts`](#21-srccoreextensionstypests)
  - [2.2 `src/core/extensions/loader.ts`](#22-srccoreextensionsloaderts)
  - [2.3 `src/core/extensions/runner.ts`](#23-srccoreextensionsrunnerts)
  - [2.4 `src/core/extensions/index.ts`](#24-srccoreextensionsindexts)
  - [2.5 `src/core/extensions/wrapper.ts`](#25-srccoreextensionswrapperts)
  - [2.6 `virtual-modules.ts` 与 jiti 装载器](#26-virtual-modulests-与-jiti-装载器)
  - [2.7 文档](#27-文档)
  - [2.8 适配层：RPC / 交互式 / print / JSON](#28-适配层rpc--交互式--print--json)
  - [2.9 `ResourceLoader` 中的扩展装配契约](#29-resourceloader-中的扩展装配契约)
  - [2.10 依赖本单元的外部设施](#210-依赖本单元的外部设施)
- [3. 用户可见表面（逐字）](#3-用户可见表面逐字)
- [4. 外部格式（逐字）](#4-外部格式逐字)
- [5. 跨单元接口](#5-跨单元接口)
- [6. npm 依赖与 Rust crate 映射](#6-npm-依赖与-rust-crate-映射)
- [7. 测试清单](#7-测试清单)
- [8. Rust 模块布局与最难复刻之处](#8-rust-模块布局与最难复刻之处)
  - [8.8 验收清单摘要](#88-验收清单摘要按节索引)
- [附录 A：本规范的核对方法](#附录-a本规范的核对方法)
- [附录 B：审计补遗（Audit additions）](#附录-b审计补遗audit-additions)

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件

```
packages/coding-agent/src/core/extensions/types.ts             (2308 行) 公开扩展 API（唯一权威）
packages/coding-agent/src/core/extensions/loader.ts            ( 879 行) jiti 加载、发现、运行时桩、createExtensionAPI
packages/coding-agent/src/core/extensions/runner.ts            (1565 行) ExtensionRunner、事件派发、上下文、诊断
packages/coding-agent/src/core/extensions/index.ts             ( 225 行) 单元内再导出面
packages/coding-agent/src/core/extensions/wrapper.ts           (  29 行) wrapRegisteredTool(s)
packages/coding-agent/src/core/extensions/virtual-modules.ts   (  41 行) VIRTUAL_MODULES 映射
packages/coding-agent/src/core/extensions/jiti-loader.ts       (   3 行) `export { createJiti } from "jiti"`
packages/coding-agent/src/core/extensions/jiti-static-loader.ts(   3 行) `export { createJiti } from "jiti/static"`
packages/coding-agent/src/extensions/index.ts                  (  14 行) builtInExtensions 清单（实现归 ca-builtin-extensions）
```

**文档**（行为契约的一部分，必须读）：

```
packages/coding-agent/docs/extensions.md          (400 行) 生命周期、hook 顺序、组合语义、exposure、nested executeTool
packages/coding-agent/docs/rpc-extension-ui.md    (200 行) RPC 扩展 UI 子协议
packages/coding-agent/docs/tui.md                 (195 行) 自定义组件、焦点、overlay、主题、渲染性能
packages/coding-agent/docs/virtual-models.md      (114 行) 虚拟模型（交叉参考，由 ca-config-models 实现）
```

**兼容语料（必须全部可作为扩展加载并行为一致）**：

```
packages/coding-agent/examples/extensions/**      (80 个条目：70 个 .ts 单文件 + 9 个目录 + README)
packages/coding-agent/examples/extensions/README.md
其中目录型：custom-provider-anthropic/ custom-provider-gitlab-duo/ doom-overlay/ dynamic-resources/
            gondolin/ plan-mode/ sandbox/ subagent/ with-deps/
另外：examples/rpc-extension-ui.ts、examples/rpc-client.ts、examples/sdk/06-extensions.ts
```

**测试**（编码行为，必须移植）：

```
test/extensions-discovery.test.ts                     (567 行)
test/extensions-runner.test.ts                        (1440 行)
test/extensions-input-event.test.ts                   (125 行)
test/interactive-mode-extension-dialogs.test.ts       (152 行)
test/suite/regressions/extension-factory-cache.test.ts
test/suite/regressions/6260-inline-extension-naming.test.ts
test/suite/regressions/8423-extension-factory-failure.test.ts
test/suite/regressions/9540-extension-loader-lazy.test.ts
test/suite/regressions/8237-node-sea-extension-loading.test.ts
test/suite/regressions/2860-replaced-session-context.test.ts
test/suite/regressions/6162-extension-active-tools-next-turn.test.ts
test/suite/regressions/2835-tools-allowlist-filters-extension-tools.test.ts
test/suite/regressions/3592-no-builtin-tools-keeps-extension-tools.test.ts
test/suite/regressions/5109-exclude-tools.test.ts
test/suite/regressions/5080-signal-shutdown-extension-cleanup.test.ts
test/suite/regressions/5943-session-start-notify.test.ts
test/suite/regressions/11156-kiana-ai-models-virtual-module.test.ts
test/suite/regressions/10285-mcp-tool-renderers.test.ts（渲染器解析部分）
test/suite/regressions/1717-2113-agent-session-event-settlement.test.ts
test/suite/regressions/3982-message-end-cost-override.test.ts
test/suite/agent-session-model-extension.test.ts（扩展 hook 部分）
test/suite/agent-session-boundaries.test.ts（boundary 部分）
test/suite/agent-session-tool-orchestration.test.ts（嵌套 executeTool 部分）
test/suite/virtual-models.test.ts（registerVirtualModel 队列与路由）
test/mcp-extension.test.ts（registerMcpServer 相关）、test/llama-extension.test.ts（内置 provider 注册）
test/suite/harness.ts（HarnessOptions / Harness 中与扩展相关的部分）
```

### 0.2 本单元不实现、由其它单元拥有

| 文件 / 包 | 归属单元 | 本单元如何使用 |
|---|---|---|
| `src/extensions/mcp/**`、`src/extensions/codemode/**`、`src/extensions/tool-search/**`、`src/extensions/llama/**` | `ca-builtin-extensions` | 只消费 `builtInExtensions: InlineExtension[]` 清单与 `builtin:` 路径语义 |
| `src/core/tools/tool-definition-wrapper.ts`（`wrapToolDefinition`） | `ca-tools-compaction-export` | `wrapper.ts` 调用它把 `ToolDefinition` 适配成 `AgentTool` |
| `src/core/mcp-servers.ts`（`validateMcpServerConfig`、`McpServerRegistry`、`mcpNamespace`） | `telemetry-mcp` | `loader.ts` 与 `runner.ts` 调用 |
| `src/core/source-info.ts`、`src/core/kiana-manifest.ts`、`src/core/event-bus.ts`、`src/core/timings.ts`、`src/core/exec.ts`、`src/core/diagnostics.ts` | `ca-cli`（utils 与核心小件） | 直接消费；签名见 §5 |
| `src/core/agent-session.ts`（`bindExtensions` / `_bindExtensionCore` / `_refreshToolRegistry` / `_executeNestedToolCall`）、`src/core/nested-tool-calls.ts` | `ca-session` | 本单元定义被注入的动作类型；调用方在 ca-session |
| `src/core/system-prompt.ts`（`buildSystemPrompt`、`normalizeBuildSystemPromptOptions`） | `ca-session` | `emitBeforeAgentStart` 使用 |
| `src/core/model-registry.ts` / `model-runtime.ts`、`src/core/virtual-models.ts` | `ca-config-models` | provider / virtual model 注册落点 |
| `src/modes/interactive/theme/**`、`src/modes/interactive/components/**`、`@earendil-works/kiana-tui` | `tui-core` / `tui-components` | UI 上下文实现依赖 Theme / Component / TUI |
| `@earendil-works/kiana-ai`、`@earendil-works/kiana-agent-core` | `ai-*` / `durable-*` | 事件载荷类型（`Model`、`AgentMessage`、`AgentTool`、`Usage` …） |

### 0.3 与兄弟 spec 的边界协调

- `ca-resources.md` §0.2 的边界表把 `src/core/extensions/**` 记在 `durable-harness` 名下，并称
  `ca-session` 使用 `core/extensions/loader.ts`。本 spec（`ca-extensions`）**主张** `src/core/extensions/**`
  的实现所有权：这批文件是扩展系统的完整实现，语义密集且自成一体，拆散会破坏 `types.ts` 的单一权威。
  `ca-resources` 与 `ca-session` 只通过 §5 列出的签名消费它。
- `ca-builtin-extensions.md` 拥有四个内置扩展的**实现**；本单元只拥有 `builtInExtensions` 清单的形状、
  `builtin:<name>` 路径语义和 `InlineExtension` 对象语义。
- 若实际实现时决定把 `core/extensions` 并入 `durable-harness` crate，则 §8 的模块布局应作为该 crate 的
  `extensions` 子模块落地，接口与本文一致。

### 0.4 阅读顺序

1. `types.ts`（§2.1）→ 全 API 形状；
2. `loader.ts`（§2.2）→ 装载与注册事务；
3. `runner.ts`（§2.3）→ 派发与组合语义；
4. `wrapper.ts` / `virtual-modules.ts` / jiti（§2.5、§2.6）→ 桥接细节；
5. 文档 `docs/extensions.md`、`docs/rpc-extension-ui.md`、`docs/tui.md`（§2.7）；
6. 适配层（§2.8）→ 四种模式的 `ExtensionUIContext`；
7. 测试（§7）与示例语料（§3.9 清单）。

---

## 1. 概览

### 1.1 目的与在系统中的位置

扩展是**在 Kiana 进程内**运行的 TypeScript 模块，导出默认工厂 `default function (kiana: ExtensionAPI)`。
工厂拿到一个 `ExtensionAPI`，用它注册：

- 事件处理器（**41 种事件**，`kiana.on(...)`，返回退订函数）；
- LLM 可调用工具（`kiana.registerTool(...)`）；
- `/` 命令（`kiana.registerCommand`）、键盘快捷键（`kiana.registerShortcut`）、CLI flag（`kiana.registerFlag`）；
- 渲染器（`registerMessageRenderer` / `registerMarkdownTransformer` / `registerEntryRenderer` / `registerToolRenderer`）；
- 模型 provider（`registerProvider`）、虚拟模型（`registerVirtualModel`）、MCP server（`registerMcpServer`）。

扩展运行在宿主进程，拥有同等 OS 权限；可以读 prompt、工具调用、文件、凭据与会话历史（`docs/extensions.md` 开头即声明）。

数据流（自下而上）：

```
文件系统 (.kiana/extensions/**、agentDir/extensions/**、--extension 路径、SDK inline 工厂、builtin:<name>)
        │  discoverAndLoadExtensions / loadExtensions / loadExtensionsCached / loadExtensionFromFactory
        ▼
Extension[]（handlers/tools/messageRenderers/… 的 Map 集合）+ ExtensionRuntime（共享可变状态）
        │  new ExtensionRunner(extensions, runtime, cwd, sessionManager, modelRegistry)
        │  runner.bindCore(actions, contextActions, providerActions?)
        │  runner.bindCommandContext(actions?)
        │  runner.setUIContext(ui, mode)
        ▼
AgentSession / InteractiveMode / RpcMode / print+json 模式 调用 emit* / createContext / createToolContext
        ▼
扩展 handler 看到 ExtensionContext / ExtensionCommandContext / ExtensionToolContext，通过 ctx.ui 与用户交互
```

### 1.2 三个对象层

1. **`Extension`**（加载产物，共 **14** 个字段：5 个标量 + 9 个注册容器）：`path`、`resolvedPath`、`hidden?`、`replaceable?`、`sourceInfo`，以及 **9** 个注册容器
   （`handlers`、`tools`、`messageRenderers`、`toolRenderers?`、`markdownTransformer?`、`entryRenderers?`、
   `commands`、`flags`、`shortcuts`）。一个文件/内联工厂 = 一个 `Extension`。注意其中只有 `handlers`/`tools`/
   `commands`/`flags`/`shortcuts` 会在冲突检测/命令解析中被遍历；`markdownTransformer` 是**单值**（后注册覆盖），
   `toolRenderers`/`entryRenderers` 是可选容器，`sourceInfo` 会在 ResourceLoader 末尾被重新赋值（见 §B.3.6）。
2. **`ExtensionRuntime`**（跨扩展共享、可变）：flag 值、三类 pending provider 注册、MCP registry、
   `createContext`、事件总线订阅跟踪、以及 `ExtensionActions` 的 **15 个** action 方法。加载期是抛错桩，
   `bindCore` 后被真实实现替换。
3. **`ExtensionRunner`**（每会话）：持有 `Extension[]` + runtime + UI 上下文 + 模式 + 模型/会话访问器，
   负责**派发**与**上下文构造**。

### 1.3 生命周期总览

`docs/extensions.md` 的三张 mermaid 图是权威描述。文字化：

**一次正常 prompt 的 run 结构**

```
input → before_agent_start → agent_start → [turn...] → agent_end
       → (还有工作? → agent_start) → agent_before_settle → (要求继续? → agent_start)
       → agent_settled
```

**每个 turn**

```
turn_start → model request → (有工具调用 → 工具执行) → turn_end → (还有 turn? → turn_start) → agent_end
```

**一次 model request 的 hook 顺序（严格）**

1. `context`
2. `context_with_system`
3. `before_provider_headers`
4. `before_provider_request`
5. `after_provider_response`
6. `provider_stream_event` 与 assistant message 事件

**每次工具调用的 hook 顺序**

1. `tool_execution_start`
2. `tool_call`
3. `tool_execution_update`（执行期间，可多次）
4. `tool_result`
5. `tool_execution_end`

**会话启动/替换**

```
进程启动 → (需要时) project_trust → session_start
/new 或 /resume → session_before_switch → session_shutdown → [替换 runtime] → session_start
/fork 或 /clone → session_before_fork   → session_shutdown → [替换 runtime] → session_start
reload          → session_shutdown      → [替换 runtime] → session_start
（不替换则进程退出）
session_start → resources_discover
```

**错误隔离**：Kiana 报告 handler 错误并继续（`emitError`）；`tool_call` handler 抛错 → 该工具被阻止（fail-safe）；
工具执行抛错 → 变成给模型的错误结果。`user_bash` handler 抛错 → 命令被阻止，不落回本地执行。

**加载事务**：`createExtensionAPI` 在 `loading` 状态下把 flag 默认值与 runtime 变更放进
`pendingFlagValues` / `pendingRuntimeChanges`；工厂正常返回后 `commit()`，抛错则 `discard()` 并把 API 置为
`failed`（此后任何 API 调用抛 "failed to load and its API is no longer active"）。

### 1.4 入口点清单

| 入口 | 位置 | 说明 |
|---|---|---|
| `createExtensionRuntime()` | loader.ts | 新建带抛错桩的共享 runtime |
| `loadExtensionFromFactory(factory, cwd, eventBus, runtime, extensionPath = "<inline>")` | loader.ts | 内联/内置扩展加载 |
| `loadExtensions(paths, cwd, eventBus?, runtime?)` | loader.ts | 显式路径，无缓存 |
| `loadExtensionsCached(paths, cwd, eventBus?, runtime?)` | loader.ts | 显式路径，按 cwd+generation 缓存工厂 |
| `discoverAndLoadExtensions(configuredPaths, cwd, agentDir = getAgentDir(), eventBus?)` | loader.ts | 标准位置发现 + 加载 |
| `clearExtensionCache()` | loader.ts | 清缓存并递增 generation |
| `new ExtensionRunner(...)` | runner.ts | 每会话 runner |
| `emitSessionShutdownEvent(runner, event)` | runner.ts | 有 handler 时 emit 并返回 true |
| `emitProjectTrustEvent(result, event, ctx)` | runner.ts | 特殊：第一个 yes/no 立即返回 |
| `wrapRegisteredTool(s)` | wrapper.ts | `RegisteredTool` → `AgentTool` |
| `VIRTUAL_MODULES` | virtual-modules.ts | 扩展可 import 的虚拟模块表 |
| `builtInExtensions` | src/extensions/index.ts | 4 个内置扩展清单 |

`src/core/extensions/index.ts` 把这些（以及全部类型与 9 个类型守卫）从单元边界导出；
`src/index.ts` 再把它们从 SDK 根导出（扩展可以 `import { ... } from "@earendil-works/kiana-coding-agent"`）。

---

## 2. 逐文件规范

### 2.1 `src/core/extensions/types.ts`

2308 行，**唯一的公开扩展 API 权威**。下面按源码顺序给出全部导出（186 个唯一名字：175 个声明 + 11 个再导出）与重要内部类型。
Rust 侧必须提供一一对应的类型（命名 `PascalCase` → `PascalCase`，字段 `camelCase` → `snake_case` 的
serde rename 必须保留线上 JSON 名，见 §4）。

#### 2.1.1 顶部再导出（4 组）

```ts
export type { ExecOptions, ExecResult } from "../exec.ts";
export type { BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions } from "../system-prompt.ts";
export type { AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode };
export type { AppKeybinding, KeybindingsManager } from "../keybindings.ts";
```

`ExecOptions` / `ExecResult` 形状见 §5.1（`ca-cli` 拥有实现）。

#### 2.1.2 UI 上下文类型

**`ExtensionUIDialogOptions`**

```ts
interface ExtensionUIDialogOptions { signal?: AbortSignal; timeout?: number }
```

- `signal`：程序化关闭对话框；abort 后的默认解析值由各方法决定（见下）。
- `timeout`：毫秒；对话框自动关闭并显示倒计时（交互式），或由 agent 侧自动解析（RPC）。

**`WidgetPlacement = "aboveEditor" | "belowEditor"`**

**`ExtensionWidgetOptions { placement?: WidgetPlacement }`**，默认 `"aboveEditor"`。

**`TerminalInputHandler = (data: string) => { consume?: boolean; data?: string } | undefined`**

**`WorkingIndicatorOptions { frames?: string[]; intervalMs?: number }`**

- `frames: []` 隐藏指示器；自定义帧“按原样渲染”，扩展必须自带颜色。
- `intervalMs` 仅对动画指示器有效。

**`AutocompleteProviderFactory = (current: AutocompleteProvider) => AutocompleteProvider`**

**`EditorFactory = (tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager) => EditorComponent`**

**`ExtensionUIContext`** — 完整方法表（Rust trait `ExtensionUiContext`，28 个成员：27 方法 + 只读 `theme`；`setWidget` 两个重载）。每一方法的行为契约在
§2.8.1（交互式实现）、§2.8.2（RPC 实现）、§2.3.10（noOp 默认）三处给出。

| 方法 | 签名 | 无 UI 时默认 |
|---|---|---|
| `select` | `(title: string, options: string[], opts?: ExtensionUIDialogOptions) => Promise<string \| undefined>` | `undefined` |
| `confirm` | `(title: string, message: string, opts?) => Promise<boolean>` | `false` |
| `input` | `(title: string, placeholder?: string, opts?) => Promise<string \| undefined>` | `undefined` |
| `notify` | `(message: string, type?: "info" \| "warning" \| "error") => void` | no-op |
| `onTerminalInput` | `(handler: TerminalInputHandler) => () => void` | 返回 no-op 退订 |
| `setStatus` | `(key: string, text: string \| undefined) => void` | no-op |
| `setWorkingMessage` | `(message?: string) => void` | no-op |
| `setWorkingVisible` | `(visible: boolean) => void` | no-op |
| `setWorkingIndicator` | `(options?: WorkingIndicatorOptions) => void` | no-op |
| `setHiddenThinkingLabel` | `(label?: string) => void` | no-op |
| `setWidget` | 重载 1：`(key, content: string[] \| undefined, options?) => void`；重载 2：`(key, content: ((tui: TUI, theme: Theme) => Component & { dispose?(): void }) \| undefined, options?) => void` | no-op |
| `setFooter` | `(factory: ((tui: TUI, theme: Theme, footerData: ReadonlyFooterDataProvider) => Component & { dispose?(): void }) \| undefined) => void` | no-op |
| `setHeader` | `(factory: ((tui: TUI, theme: Theme) => Component & { dispose?(): void }) \| undefined) => void` | no-op |
| `setTitle` | `(title: string) => void` | no-op |
| `custom<T>` | `(factory: (tui, theme, keybindings, done: (result: T) => void) => (Component & { dispose?(): void }) \| Promise<...>, options?: { overlay?: boolean; overlayOptions?: OverlayOptions \| (() => OverlayOptions); onHandle?: (handle: OverlayHandle) => void }) => Promise<T>` | `undefined as never` |
| `pasteToEditor` | `(text: string) => void` | no-op |
| `setEditorText` | `(text: string) => void` | no-op |
| `getEditorText` | `() => string` | `""` |
| `editor` | `(title: string, prefill?: string) => Promise<string \| undefined>` | `undefined` |
| `addAutocompleteProvider` | `(factory: AutocompleteProviderFactory) => void` | no-op |
| `setEditorComponent` | `(factory: EditorFactory \| undefined) => void` | no-op |
| `getEditorComponent` | `() => EditorFactory \| undefined` | `undefined` |
| `theme`（只读属性） | `Theme` | 全局 `theme` |
| `getAllThemes` | `() => { name: string; path: string \| undefined }[]` | `[]` |
| `getTheme` | `(name: string) => Theme \| undefined` | `undefined` |
| `setTheme` | `(theme: string \| Theme) => { success: boolean; error?: string }` | `{ success: false, error: "UI not available" }` 【逐字】 |
| `getToolsExpanded` | `() => boolean` | `false` |
| `setToolsExpanded` | `(expanded: boolean) => void` | no-op |

#### 2.1.3 扩展上下文类型

**`ContextUsage`**

```ts
interface ContextUsage {
  tokens: number | null;      // 未知时为 null（例如刚压缩完、下一次 LLM 响应之前）
  contextWindow: number;
  percent: number | null;     // tokens 未知时为 null
}
```

**`CompactOptions`**

```ts
interface CompactOptions {
  customInstructions?: string;
  onComplete?: (result: CompactionResult) => void;
  onError?: (error: Error) => void;
}
```

注意：`ctx.compact(options)` 返回 `void`（“触发压缩但不等待完成”）。交互式实现内部是
`void (async () => { try { onComplete?.(await session.compact(customInstructions)) } catch (e) { onError?.(e) } })()`
（见 §2.8.1）。

**`ExtensionMode = "tui" | "rpc" | "json" | "print"`**

**`ExtensionContext`** — 事件处理器与工具执行上下文（getter 语义见 §2.3.7）：

```ts
interface ExtensionContext {
  ui: ExtensionUIContext;
  mode: ExtensionMode;
  hasUI: boolean;                    // TUI 与 RPC 为 true
  cwd: string;
  sessionManager: ReadonlySessionManager;
  modelRegistry: ModelRegistry;
  model: Model<any> | undefined;
  scopedModels: readonly ScopedModel[];   // 只读快照；未配置作用域时为空
  thinkingLevel?: ThinkingLevel;
  isIdle(): boolean;
  isProjectTrusted(): boolean;
  signal: AbortSignal | undefined;   // 只在流式期间有值
  abort(): void;
  hasPendingMessages(): boolean;
  shutdown(): void;                  // 所有上下文可用
  getContextUsage(): ContextUsage | undefined;
  compact(options?: CompactOptions): void;
  getSystemPrompt(): string;
}
```

**`ExecuteToolOptions`**

```ts
interface ExecuteToolOptions {
  signal?: AbortSignal;                 // 默认取调用工具的 signal
  onUpdate?: AgentToolUpdateCallback;   // 接收嵌套工具的部分结果
}
```

**`ExtensionToolContext extends ExtensionContext`**

```ts
interface ExtensionToolContext extends ExtensionContext {
  readonly tools: readonly AgentTool[];        // executeTool 可以调用的工具
  executeTool(name: string, args: unknown, options?: ExecuteToolOptions): Promise<AgentToolCallOutcome>;
}
```

契约（逐字要点）：
- 调用 id 为 `<calling id>/<n>`；`tool_call`、`tool_result`、`tool_execution_*` 事件携带 `parentToolCallId`；
- 不出现在 transcript 中；会话在调用工具的 tool result message 上保留有界记录 `nestedCalls`；
- **对工具失败永不 reject**：未知工具、校验错误、被阻止、抛出的错误都以 `isError: true` 返回。
- 无 `executeTool` 动作时的回退结果（`runner.createToolContext`，见 §2.3.8）：

```json
{"toolCall":{"type":"toolCall","id":"<toolCallId>/0","name":"<name>","arguments":{}},
 "result":{"content":[{"type":"text","text":"Nested tool calls are not available in this context"}],"details":{}},
 "isError":true}
```
【逐字】文本 `Nested tool calls are not available in this context`。

**`ExtensionCommandContext extends ExtensionContext`**

```ts
interface ExtensionCommandContext extends ExtensionContext {
  getSystemPromptOptions(): BuildSystemPromptOptions;
  waitForIdle(): Promise<void>;
  newSession(options?: {
    parentSession?: string;
    setup?: (sessionManager: SessionManager) => Promise<void>;
    withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
  }): Promise<{ cancelled: boolean }>;
  fork(entryId: string, options?: {
    position?: "before" | "at";
    withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
  }): Promise<{ cancelled: boolean }>;
  navigateTree(targetId: string, options?: {
    summarize?: boolean; customInstructions?: string; replaceInstructions?: boolean; label?: string;
  }): Promise<{ cancelled: boolean }>;
  switchSession(sessionPath: string, options?: {
    withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
  }): Promise<{ cancelled: boolean }>;
  reload(): Promise<void>;
}
```

这些操作**只允许在用户发起的命令里**调用（“从生命周期处理器调用会死锁”）。

**`ReplacedSessionContext extends ExtensionCommandContext`**

```ts
sendMessage<T = unknown>(
  message: Pick<CustomMessage<T>, "customType" | "content" | "display" | "details">,
  options?: { triggerTurn?: boolean; deliverAs?: "steer" | "followUp" | "nextTurn" },
): Promise<void>;
sendUserMessage(
  content: string | (TextContent | ImageContent)[],
  options?: { deliverAs?: "steer" | "followUp"; expandPromptTemplates?: boolean },
): Promise<void>;
```

（注意与 `ExtensionAPI` 上的同名方法区别：API 版本返回 `void` 且是 fire-and-forget；替换会话版本返回 `Promise<void>`。）

#### 2.1.4 工具类型

**`ToolRenderResultOptions { expanded: boolean; isPartial: boolean }`**

**`ToolRenderContext<TState = any, TArgs = any>`** — 渲染器上下文：

| 字段 | 类型 | 语义 |
|---|---|---|
| `args` | `TArgs` | 当前工具调用参数；同一 tool call 的 call/result 渲染共享 |
| `toolCallId` | `string` | 同一次执行稳定 |
| `invalidate` | `() => void` | 只重绘该工具执行组件 |
| `lastComponent` | `Component \| undefined` | 该渲染槽上一次返回的组件 |
| `state` | `TState` | 该工具行共享的渲染状态（由 `tool-execution.ts` 初始化） |
| `cwd` | `string` | 该次执行的 cwd |
| `executionStarted` | `boolean` | 执行是否已开始 |
| `argsComplete` | `boolean` | 参数是否已完整 |
| `isPartial` | `boolean` | 结果是否流式中 |
| `expanded` | `boolean` | 结果视图是否展开 |
| `showImages` | `boolean` | TUI 当前是否显示内嵌图片 |
| `isError` | `boolean` | 当前结果是否错误 |
| `durationMs` | `number \| undefined` | 最终结果的耗时；运行中、未运行、或旧记录为 `undefined` |
| `outputPad` | `number` | `outputPad` 设置的水平内缩；`renderShell: "self"` 的渲染器要自己应用 |

**`ToolExposure`** 【逐字文档】：

```
"direct" | "model-only" | "codemode" | "deferred" | "hidden"
```

- `direct`（默认）：active 时声明给模型，且 active 时可调用。
- `model-only`：active 时声明给模型，**永不**可调用（用于编排型/交互型工具）。
- `codemode`：注册后**始终**可调用；除非显式激活，否则不声明给模型。codemode 工具会在描述里列出它。
- `deferred`：同 `codemode`，但 codemode 工具不列出它；`tool_search` 能找到它。
- `hidden`：已注册但不可达。激活它没有效果。

`direct` 与 `model-only` 在注册时即被激活；其余不会。active 集合（`getActiveTools`/`setActiveTools`）
= 声明给模型的集合。

**`ToolAnnotations`**：`readOnlyHint?`、`destructiveHint?`、`idempotentHint?`、`openWorldHint?`（皆 `boolean`），
语义同 MCP tool annotations，作者提供的提示、**不被验证**。

**`ToolNamespace`**

```ts
interface ToolNamespace {
  name: string;                                  // 例如 "mcp__docs"
  aliases?: readonly string[];
  description?: string;                          // 在模型可见的工具列表中随组显示一次
  instructions?: string;                         // 更长指引，不进入列表；describeNamespace() 返回
}
```

**`ToolLoadout`**（只读视图）

```ts
interface ToolLoadout {
  readonly declared: readonly AgentTool[];     // 声明给模型的工具（active），按顺序，原始描述
  readonly callable: readonly AgentTool[];     // 可通过 ctx.executeTool() 调用
  readonly registered: readonly AgentTool[];   // 全部已注册
  getExposure(name: string): ToolExposure;
  getNamespace(name: string): ToolNamespace | undefined;
  getPromptGuidelines(name: string): readonly string[];   // hidden declaration 的工具不贡献指引
}
```

**`ToolLoadoutChanges { descriptions?: Readonly<Record<string,string>>; hiddenDeclarations?: readonly string[] }`**

**`ToolDefinition<TParams extends TSchema = TSchema, TDetails = unknown, TState = any>`** — 全部字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `name` | `string` | LLM 工具调用名 |
| `label` | `string` | UI 人类可读标签 |
| `description` | `string` | 给 LLM |
| `promptSnippet?` | `string` | 默认系统提示 “Available tools” 段的一行摘要；不提供则自定义工具从该段省略 |
| `promptGuidelines?` | `string[]` | 工具 active 时追加到默认系统提示 Guidelines 段 |
| `parameters` | `TParams` | TypeBox schema；**必须是 object schema**（否则注册抛错，见 §2.2.3） |
| `constrainedSampling?` | `false \| ConstrainedSamplingConfig` | provider 侧受限采样；`false` 等价 undefined |
| `renderShell?` | `"default" \| "self"` | 是否由 ToolExecutionComponent 画标准彩色外壳 |
| `prepareArguments?` | `(args: unknown) => Static<TParams>` | schema 校验前修正原始参数；必须返回符合 TParams 的对象 |
| `outputSchema?` | `TSchema` | 成功结果中 `structuredContent` 的 JSON Schema |
| `exposure?` | `ToolExposure` | 默认 `"direct"` |
| `namespace?` | `ToolNamespace` | 分组 |
| `reachesTools?` | `(input: Record<string, unknown>) => "all" \| readonly string[]` | 该调用可能触及哪些工具；`"all"` 或输入中可能命名它们的词（词匹配工具/命名空间名，可能过匹配） |
| `annotations?` | `ToolAnnotations` | 提示 |
| `defaultActive?` | `boolean` | 注册是否激活；默认 direct/model-only 为 true，其余永不在注册时激活；`false` 需 `--tools`/`defaultTools`/`setActiveTools()` 激活 |
| `prepareLoadout?` | `(loadout: ToolLoadout) => ToolLoadoutChanges \| undefined` | active 工具变化时调用 |
| `executionMode?` | `"sequential" \| "parallel"` | 单工具执行模式覆盖 |
| `execute` | `(toolCallId, params, signal, onUpdate, ctx: ExtensionToolContext) => Promise<AgentToolResult<TDetails>>` | 执行 |
| `renderCall?` | `(args, theme, context: ToolRenderContext<TState, Static<TParams>>) => Component` | 调用显示 |
| `renderResult?` | `(result: AgentToolResult<TDetails>, options: ToolRenderResultOptions, theme, context) => Component` | 结果显示 |

**`ToolRenderers = Pick<AnyToolDefinition, "renderShell" | "renderCall" | "renderResult">`**

**`ToolRendererResolver = (toolName: string, next: () => ToolRenderers | undefined) => ToolRenderers | undefined`**

`next()` 返回剩余 resolver（按扩展加载顺序）、再是已注册工具会用的渲染器（`base`）。**`registerToolRenderer` 的
resolver 按扩展加载顺序运行**，`runner.resolveToolRenderers` 逐个串联（§2.3.12）。补充：同一扩展内多次 `registerToolRenderer`
会按注册顺序排在该扩展的位置上（`flatMap`），因此不能假设“每个扩展只有一个 resolver”；`base()` 在 `agent-session` 中是
`() => this.getToolDefinition(toolName)`，对**未注册**工具也可能返回 undefined（例如 MCP 工具尚未声明时，见 `10285-mcp-tool-renderers`）。

**`defineTool<TParams, TDetails = unknown, TState = any>(tool) => tool`** — 恒等函数，只为保留参数推断；
必须找到 0 处运行期行为。

#### 2.1.5 启动 / 资源事件

**`ProjectTrustEvent { type: "project_trust"; cwd: string }`**

**`ProjectTrustEventDecision = "yes" | "no" | "undecided"`**

**`ProjectTrustEventResult { trusted: ProjectTrustEventDecision; remember?: boolean }`**

**`ProjectTrustContext { cwd: string; mode: ExtensionMode; hasUI: boolean; ui: Pick<ExtensionUIContext, "select" | "confirm" | "input" | "notify"> }`**

**`ProjectTrustHandler = (event, ctx: ProjectTrustContext) => Promise<ProjectTrustEventResult> | ProjectTrustEventResult`**

**`ResourcesDiscoverEvent { type: "resources_discover"; cwd: string; reason: "startup" | "reload" }`**
（文档：在 `session_start` 之后触发）

**`ResourcesDiscoverResult { skillPaths?: string[]; promptPaths?: string[]; themePaths?: string[] }`**

**`McpServersChangeEvent { type: "mcp_servers_change"; servers: RegisteredMcpServer[] }`**
在扩展绑定后某个扩展注册/注销 MCP server 时触发。**处理该事件会把扩展标记为“连接注册服务器的那一个”**
（`hasHandlers("mcp_servers_change")` → `reportUnhandledMcpServers()` 不报错）。

#### 2.1.6 会话事件

| 类型 | 字段 | 备注 |
|---|---|---|
| `SessionStartEvent` | `type:"session_start"`, `reason:"startup"\|"reload"\|"new"\|"resume"\|"fork"`, `previousSessionFile?: string` | `new`/`resume`/`fork` 时带 previous |
| `SessionInfoChangedEvent` | `type:"session_info_changed"`, `name: string \| undefined` | 清除名字时为 undefined |
| `SessionBeforeSwitchEvent` | `type:"session_before_switch"`, `reason:"new"\|"resume"`, `targetSessionFile?: string` | 可取消 |
| `SessionBeforeForkEvent` | `type:"session_before_fork"`, `entryId`, `position:"before"\|"at"` | 可取消 |
| `SessionBeforeCompactEvent` | `type:"session_before_compact"`, `preparation: CompactionPreparation`, `branchEntries: SessionEntry[]`, `customInstructions?`, `reason:"manual"\|"threshold"\|"overflow"`, `willRetry: boolean`, `signal: AbortSignal` | 可取消或自定义 |
| `SessionCompactEvent` | `type:"session_compact"`, `compactionEntry: CompactionEntry`, `fromExtension: boolean`, `reason`, `willRetry` | 成功后 |
| `SessionCompactFailedEvent` | `type:"session_compact_failed"`, `reason`, `errorMessage?`, `aborted: boolean`, `willRetry`, `fromExtension` | 失败或中止 |
| `SessionShutdownEvent` | `type:"session_shutdown"`, `reason:"quit"\|"reload"\|"new"\|"resume"\|"fork"`, `targetSessionFile?` | 会话替换时带目标 |
| `TreePreparation` | `targetId`, `oldLeafId: string\|null`, `commonAncestorId: string\|null`, `entriesToSummarize: SessionEntry[]`, `userWantsSummary: boolean`, `customInstructions?`, `replaceInstructions?`, `label?` | |
| `SessionBeforeTreeEvent` | `type:"session_before_tree"`, `preparation: TreePreparation`, `signal` | 可取消 |
| `SessionTreeEvent` | `type:"session_tree"`, `newLeafId: string\|null`, `oldLeafId: string\|null`, `summaryEntry?: BranchSummaryEntry`, `fromExtension?: boolean` | |

`SessionEvent` = 上述 10 个的联合。

#### 2.1.7 Agent 事件

**`ContextEvent { type:"context"; messages: AgentMessage[] }`**

文档要点（逐字语义）：`messages` 是**去掉 system 消息**的会话；prompt 与工具状态属于 Kiana，
handler 返回后 Kiana 会恢复它们，因此 handler 不能丢弃它们，也不需要保留它们。

**`ContextWithSystemEvent { type:"context_with_system"; messages: AgentMessage[] }`**

在每个 `context` handler 跑完、Kiana 恢复 prompt 与工具状态之后触发；`messages` 是**包含 system 消息**的完整
transcript，返回值**按原样发送**——handler 拥有 prompt 与工具声明。

**`BeforeProviderRequestEvent { type:"before_provider_request"; payload: unknown }`** — 可替换 payload。

**`BeforeProviderHeadersEvent { type:"before_provider_headers"; headers: ProviderHeaders }`**

handler **原地修改** `headers`；返回值被忽略；`null` 值删除该 header。

**`AfterProviderResponseEvent { type:"after_provider_response"; status: number; headers: Record<string,string> }`**

**`ProviderStreamEvent { type:"provider_stream_event"; provider: ProviderId; api: Api; model: string; data: unknown }`**

每个解析后的 provider 流事件在 Kiana 归一化之前触发。`data` 是适配器所有、只读；按流顺序 await，
慢 handler 会拖慢流消费；handler 抛错只报告不改变响应。

**`BeforeAgentStartEventBase`**（内部，非导出）

```ts
{ type: "before_agent_start";
  readonly systemPrompt: string;                          // 当前系统提示（由 systemPromptOptions 与之前 handler 的改动渲染）
  systemPromptOptions: NormalizedBuildSystemPromptOptions; // 可变；后续 handler 看到前面 handler 的改动
}
```

**`BeforeAgentStartUserEvent extends Base`**：`messageType: "user"`、`prompt: string`（展开后的原始用户提示）、`images?: ImageContent[]`

**`BeforeAgentStartCustomEvent extends Base`**：`messageType: "custom"`、`message: CustomMessage`（启动本次 run 的自定义消息）

**`BeforeAgentStartEvent`** = 二者联合。

**`AgentStartEvent { type:"agent_start" }`**
**`AgentEndEvent { type:"agent_end"; messages: AgentMessage[] }`**
**`AgentActivityOutcome = "completed" | "aborted" | "error"`**

**边界草稿（boundary drafts）**

```ts
interface CustomEntryDraft { type: "custom"; customType: string; data?: unknown }
interface CustomMessageEntryDraft { type: "custom_message"; customType: string;
  content: string | (TextContent | ImageContent)[]; display: boolean; details?: unknown }
interface ContextEditEntryDraft { type: "context_edit"; targetId: string;
  replacement: ContextEditEntry["replacement"] }
interface CompactionEntryDraft { type: "compaction"; summary: string;
  firstKeptEntryId: string | null;   // null 表示自保持压缩，不保留任何前置条目
  details?: unknown; usage?: Usage }
type SessionBoundaryDraft = CustomEntryDraft | CustomMessageEntryDraft | ContextEditEntryDraft | CompactionEntryDraft;
```

**`BoundaryContextPreview`**

```ts
{ contextEntries: ProjectedSessionEntry[]; contextMessages: AgentMessage[];
  llmMessages: Message[]; pendingMessages: AgentMessage[]; canContinue: boolean }
```

**`BoundaryState`** = `{ entries: SessionBoundaryDraft[]; continue: boolean; context: BoundaryContextPreview; outcome: AgentActivityOutcome }`

**`BoundaryResult { entries?: SessionBoundaryDraft[]; continue?: boolean }`**

**`AgentBeforeSettleEvent extends BoundaryState { type: "agent_before_settle" }`** — 最终结算前触发，可追加条目并
确保一次“下一次 provider 请求”。

**`AgentSettledEvent { type:"agent_settled"; aborted: boolean }`** — agent run 完全结算、且不再有自动重试、
压缩或排队续跑之后。

**UI prompt 事件**

```ts
type UIPromptKind = "select" | "confirm" | "input" | "editor" | "custom";
interface UIPromptStartEvent { type:"ui_prompt_start"; reason:"ui_prompt"; kind: UIPromptKind; title?: string }
interface UIPromptEndEvent   { type:"ui_prompt_end";   reason:"ui_prompt"; kind: UIPromptKind; title?: string }
```

**`TurnStartEvent { type:"turn_start"; turnIndex: number; timestamp: number }`**

**`TurnEndEvent extends BoundaryState`**：`type:"turn_end"`、`turnIndex`、`message: AgentMessage`、
`toolResults: ToolResultMessage[]`、`messageEntryId: string`、`toolResultEntryIds: string[]`。

**`MessageStartEvent { type:"message_start"; message }`**
**`MessageUpdateEvent { type:"message_update"; message; assistantMessageEvent: AssistantMessageEvent }`**
**`MessageEndEvent { type:"message_end"; message }`**

**工具执行事件**

```ts
interface ToolExecutionStartEvent  { type:"tool_execution_start";  toolCallId; toolName; args: any; parentToolCallId? }
interface ToolExecutionUpdateEvent { type:"tool_execution_update"; toolCallId; toolName; args: any; partialResult: any; parentToolCallId? }
interface ToolExecutionEndEvent    { type:"tool_execution_end";    toolCallId; toolName; result: any; isError: boolean;
                                     durationMs?: number;      // 单调时钟测得的 execute() 耗时；未运行时缺省
                                     parentToolCallId? }
```

#### 2.1.8 模型 / 用户 bash / 输入事件

**`ModelSelectSource = "set" | "cycle" | "restore"`**
**`ModelSelectEvent { type:"model_select"; model: Model<any>; previousModel: Model<any> | undefined; source }`**
**`ThinkingLevelSelectEvent { type:"thinking_level_select"; level; previousLevel }`**
**`UserBashEvent { type:"user_bash"; command: string; excludeFromContext: boolean; cwd: string }`**（`!!` 前缀 → `excludeFromContext: true`）
**`InputSource = "interactive" | "rpc" | "extension"`**
**`InputEvent { type:"input"; text: string; images?: ImageContent[]; source: InputSource; streamingBehavior?: "steer" | "followUp" }`**
**`InputEventResult = {action:"continue"} | {action:"transform"; text: string; images?: ImageContent[]} | {action:"handled"}`**

#### 2.1.9 工具事件（`tool_call` / `tool_result`）

`ToolCallEventBase { type:"tool_call"; toolCallId: string; parentToolCallId?: string }`

8 个内建 + 1 个自定义：

| 类型 | `toolName` | `input` |
|---|---|---|
| `BashToolCallEvent` | `"bash"` | `BashToolInput` |
| `PowerShellToolCallEvent` | `"powershell"` | `PowerShellToolInput` |
| `ReadToolCallEvent` | `"read"` | `ReadToolInput` |
| `EditToolCallEvent` | `"edit"` | `EditToolInput` |
| `WriteToolCallEvent` | `"write"` | `WriteToolInput` |
| `GrepToolCallEvent` | `"grep"` | `GrepToolInput` |
| `FindToolCallEvent` | `"find"` | `FindToolInput` |
| `LsToolCallEvent` | `"ls"` | `LsToolInput` |
| `CustomToolCallEvent` | `string` | `Record<string, unknown>` |

**文档语义（逐字）**：`event.input` 可变。原地修改以在执行前修补参数；后续 `tool_call` handler 看到前面的修改。
**修改后不再重新校验**。

`ToolResultEventBase`：

```ts
{ type:"tool_result"; toolCallId: string; parentToolCallId?: string;
  input: Record<string, unknown>;
  content: (TextContent | ImageContent)[];
  structuredContent?: JsonValue;   // 声明 outputSchema 的工具的机器可读结果
  isError: boolean;
  usage?: Usage }
```

9 个具体类型：`BashToolResultEvent`（`details: BashToolDetails | undefined`）、`PowerShellToolResultEvent`、
`ReadToolResultEvent`、`EditToolResultEvent`（`EditToolDetails`）、`WriteToolResultEvent`（`details: undefined`）、
`GrepToolResultEvent`、`FindToolResultEvent`、`LsToolResultEvent`、`CustomToolResultEvent`（`details: unknown`）。

**9 个类型守卫（全部导出，必须一一对应）**

```ts
isBashToolResult / isPowerShellToolResult / isReadToolResult / isEditToolResult / isWriteToolResult /
isGrepToolResult / isFindToolResult / isLsToolResult         // 都判定 e.toolName === "<name>"
isToolCallEventType(toolName: string, event: ToolCallEvent): boolean   // 实现即 event.toolName === toolName
```

`isToolCallEventType` 有 8 个重载（内建名自动收窄）+ 1 个泛型重载；运行期实现只有比较字符串。
注意文档：直接用 `event.toolName === "bash"` 收窄不成立，因为 `CustomToolCallEvent.toolName` 是 `string`。

**`ExtensionEvent`** = 全部事件联合（顺序见源码：ProjectTrust、ResourcesDiscover、McpServersChange、SessionEvent、
ContextEvent、ContextWithSystemEvent、CacheWarmingDecisionEvent、BeforeProviderRequestEvent、
BeforeProviderHeadersEvent、AfterProviderResponseEvent、ProviderStreamEvent、BeforeAgentStartEvent、
AgentStartEvent、AgentEndEvent、AgentBeforeSettleEvent、AgentSettledEvent、UIPromptStartEvent、UIPromptEndEvent、
TurnStartEvent、TurnEndEvent、MessageStartEvent、MessageUpdateEvent、MessageEndEvent、ToolExecutionStart/Update/End、
ModelSelectEvent、ThinkingLevelSelectEvent、UserBashEvent、InputEvent、ToolCallEvent、ToolResultEvent）。

#### 2.1.10 事件结果类型

```ts
interface ContextEventResult { messages?: AgentMessage[] }
type TurnEndEventResult = BoundaryResult;
type AgentBeforeSettleEventResult = BoundaryResult;
type BeforeProviderRequestEventResult = unknown;
export type { CacheWarmingDecisionEvent, CacheWarmingDecisionEventResult } from "../cache-warmer.ts";

interface ToolCallEventResult { block?: boolean; reason?: string; terminate?: boolean }
```

- `terminate`：提示 agent 在当前工具批次后停止。**只有当批次内每个最终工具结果都为 true 时才提前结束。**

```ts
type UserBashEventResult =
  | { operations: BashOperations; result?: never }   // 自定义执行操作
  | { operations?: never; result: BashResult };      // 完整替换：扩展已执行，用这个结果
```

**`ToolResultEventResult`**

```ts
{ content?: (TextContent|ImageContent)[]; details?: unknown; structuredContent?: JsonValue;
  isError?: boolean; usage?: Usage }
```

文档：省略的字段保持原样；**替换 `content` 而未同时返回 `structuredContent` 会丢弃结构化内容**（因为可能不再匹配）。
要保留就一并返回。

**`MessageEndEventResult { message?: AgentMessage }`** — 替换必须保持原 message role。

**`BeforeAgentStartEventResult { message?: Pick<CustomMessage,"customType"|"content"|"display"|"details">; systemPrompt?: string }`**

**`SessionBeforeSwitchResult { cancel?: boolean }`**
**`SessionBeforeForkResult { cancel?: boolean; skipConversationRestore?: boolean }`**
**`SessionBeforeCompactResult { cancel?: boolean; compaction?: CompactionResult }`**
**`SessionBeforeTreeResult { cancel?: boolean; summary?: { summary: string; details?: unknown; usage?: Usage }; customInstructions?: string; replaceInstructions?: boolean; label?: string }`**

#### 2.1.11 消息/条目渲染

**`MessageRenderOptions { expanded: boolean; outputPad: number }`**
**`MarkdownTransformContext { messageType: "user"|"assistant"|"assistant-thinking"; isStreaming: boolean; availableWidth: number; messageId?: string; timestamp?: string }`**
（`messageId`/`timestamp` 只在从持久会话条目渲染时存在）
**`MarkdownTransformer = (markdown: string, context: MarkdownTransformContext) => string`**
**`EntryRenderOptions { expanded: boolean }`**
**`MessageRenderer<T = unknown> = (message: CustomMessage<T>, options: MessageRenderOptions, theme: Theme) => Component | undefined`**
**`EntryRenderer<T = unknown> = (entry: CustomEntry<T>, options: EntryRenderOptions, theme: Theme) => Component | undefined`**

#### 2.1.12 命令注册

```ts
interface RegisteredCommand {
  name: string;
  sourceInfo: SourceInfo;
  description?: string;
  getArgumentCompletions?: (argumentPrefix: string) => AutocompleteItem[] | null | Promise<AutocompleteItem[] | null>;
  handler: (args: string, ctx: ExtensionCommandContext) => Promise<void>;
}
interface ResolvedCommand extends RegisteredCommand { invocationName: string }
```

#### 2.1.13 `ExtensionAPI`

**`ExtensionHandler<E, R = undefined> = (event: E, ctx: ExtensionContext) => Promise<R | void> | R | void`**

`on()` 共 **41 个重载**（与 41 个事件名一一对应；逐字按源码顺序）：
`project_trust`(ProjectTrustHandler)、`resources_discover`、`session_start`、`session_info_changed`、
`session_before_switch`、`session_before_fork`、`session_before_compact`、`session_compact`、
`session_compact_failed`、`session_shutdown`、`mcp_servers_change`、`session_before_tree`、`session_tree`、
`context`、`context_with_system`、`cache_warming_decision`、`before_provider_request`、
`before_provider_headers`、`after_provider_response`、`provider_stream_event`、`before_agent_start`、
`agent_start`、`agent_end`、`agent_before_settle`、`agent_settled`、`ui_prompt_start`、`ui_prompt_end`、
`turn_start`、`turn_end`、`message_start`、`message_update`、`message_end`、`tool_execution_start`、
`tool_execution_update`、`tool_execution_end`、`model_select`、`thinking_level_select`、`tool_call`、
`tool_result`、`user_bash`、`input`。

（`on` 的实现只接受 `(event: string, handler)`；Rust 侧用一个
`on(&self, event: ExtensionEventKind, handler: Box<dyn ...>) -> Unsubscribe` 即可覆盖全部 41 个。）

其余成员：**31 个不同方法名**（其中 `registerProvider` 有两个重载，故接口里共 32 条方法声明）
**+ 1 个字段 `events: EventBus`**。完整清单（接口声明顺序）：
`registerTool`、`registerCommand`、`registerShortcut`、`registerFlag`、`getFlag`、
`registerMessageRenderer`、`registerMarkdownTransformer`、`registerEntryRenderer`、`registerToolRenderer`、
`sendMessage`、`sendUserMessage`、`appendEntry`、`setSessionName`、`getSessionName`、`setLabel`、`exec`、
`getActiveTools`、`getAllTools`、`getSettings`、`setActiveTools`、`getCommands`、`setModel`、
`getThinkingLevel`、`setThinkingLevel`、`registerProvider`(×2)、`unregisterProvider`、`registerMcpServer`、
`unregisterMcpServer`、`getMcpServers`、`registerVirtualModel`、`unregisterVirtualModel`、`events`（字段）。

| 成员 | 签名 | 运行期实现要点（§2.2.3） |
|---|---|---|
| `registerTool` | `<TParams, TDetails, TState>(tool: ToolDefinition) => void` | 校验 object schema；写 `extension.tools`；`runtime.refreshTools()` |
| `registerCommand` | `(name: string, options: Omit<RegisteredCommand,"name"\|"sourceInfo">) => void` | 校验非空 string name 与 handler function |
| `registerShortcut` | `(shortcut: KeyId, options: { description?: string; handler: (ctx) => Promise<void>\|void }) => void` | 写 `extension.shortcuts`，附 `extensionPath` |
| `registerFlag` | `(name, options: { description?; type: "boolean"\|"string"; default?: boolean\|string }) => void` | 校验 default 类型；写 flags；默认值进 pending/flagValues |
| `getFlag` | `(name) => boolean \| string \| undefined` | 未注册返回 undefined |
| `registerMessageRenderer` | `<T>(customType: string, renderer: MessageRenderer<T>) => void` | Map 覆盖写 |
| `registerMarkdownTransformer` | `(transformer: MarkdownTransformer) => void` | **单值**（后写覆盖） |
| `registerEntryRenderer` | `<T>(customType, renderer: EntryRenderer<T>) => void` | 惰性建 Map |
| `registerToolRenderer` | `(resolver: ToolRendererResolver) => void` | 追加到 `toolRenderers[]` |
| `sendMessage` | `<T>(message: Pick<CustomMessage<T>, "customType"\|"content"\|"display"\|"details">, options?: { triggerTurn?; deliverAs?: "steer"\|"followUp"\|"nextTurn" }) => void` | runtime 动作；**不**自动触发 turn（除非 `triggerTurn`）；`content`/`display`/`details` 为必填 pick（值可为 undefined，但键必须在类型上提供） |
| `sendUserMessage` | `(content: string \| (TextContent \| ImageContent)[], options?: { deliverAs?: "steer"\|"followUp"; expandPromptTemplates? }) => void` | runtime 动作；**总是**触发一个 turn；`deliverAs` 无 `"nextTurn"` |
| `appendEntry` | `<T>(customType: string, data?: T) => void` | runtime 动作 |
| `setSessionName` | `(name: string) => void` | runtime |
| `getSessionName` | `() => string \| undefined` | runtime |
| `setLabel` | `(entryId: string, label: string \| undefined) => void` | runtime |
| `exec` | `(command: string, args: string[], options?: ExecOptions) => Promise<ExecResult>` | `execCommand(command, args, options?.cwd ?? cwd, options)` |
| `getActiveTools` | `() => string[]` | runtime |
| `getAllTools` | `() => ToolInfo[]` | runtime |
| `getSettings` | `() => Settings` | runtime（全局与项目合并后的副本） |
| `setActiveTools` | `(toolNames: string[]) => void` | runtime；未知与 hidden 被忽略 |
| `getCommands` | `() => SlashCommandInfo[]` | runtime；由 `ca-session` 注入：扩展命令（`name` 用 **invocationName**、`source:"extension"`）→ prompt 模板（`source:"prompt"`）→ skills（`name` 前缀 `skill:`、`source:"skill"`）；`SlashCommandInfo = { name, description?, source: "extension"\|"prompt"\|"skill", sourceInfo }` |
| `setModel` | `(model: Model<any>) => Promise<boolean>` | runtime；未配置认证返回 false |
| `getThinkingLevel` | `() => ThinkingLevel` | runtime |
| `setThinkingLevel` | `(level: ThinkingLevel) => void` | runtime；clamp 到模型能力 |
| `registerProvider` | 重载 `(provider: Provider) => void` / `(name: string, config: ProviderConfig) => void` | 见 §2.2.3 事务语义 |
| `unregisterProvider` | `(name: string) => void` | |
| `registerMcpServer` | `(name: string, config: McpServerConfig) => void` | 见 §2.2.3 校验与冲突 |
| `unregisterMcpServer` | `(name: string) => void` | |
| `getMcpServers` | `() => RegisteredMcpServer[]` | `runtime.mcpServers.list()` |
| `registerVirtualModel` | `<TState>(model: ExtensionVirtualModel<TState>) => void` | route 包一层传 `runtime.createContext()` |
| `unregisterVirtualModel` | `(provider: string, id: string) => void` | |
| `events` | `EventBus`（`emit(channel, data)`、`on(channel, handler) => unsubscribe`） | 订阅自动跟踪，runtime invalidate 时退订 |

**`ExtensionVirtualModel<TState = unknown> extends Omit<VirtualModelDefinition<TState>, "route">`**

```ts
route(request: ModelRouteRequest<TState>, ctx: ExtensionContext): ModelRoute<TState> | Promise<ModelRoute<TState>>;
```

**`ProviderConfig`** — 全部字段：

```ts
{
  name?: string;                 // UI 显示名
  baseUrl?: string;              // 定义 models 时必填
  apiKey?: string;               // 字面量 / $ENV 或 ${ENV} 插值 / 前导 !command；定义 models 时必填（除非 oauth）
  api?: Api;                     // 定义 models 时 provider 或 model 级必填
  streamSimple?: (model, context: TranscriptContext, options?: SimpleStreamOptions) => AssistantMessageEventStream;
  images?: Partial<Record<ImageApi, ProviderImages>>;
  classifiers?: Partial<Record<ClassifierApi, ProviderClassifier>>;
  headers?: Record<string, string>;
  authHeader?: boolean;          // true 时加 Authorization: Bearer <resolved api key>
  models?: ProviderModelConfig[]; // 提供则替换该 provider 的所有现有模型
  refreshModels?(context: RefreshModelsContext): Promise<ProviderModelConfig[]>;
  oauth?: {
    name: string;
    isSubscription?: boolean;
    usesCallbackServer?: boolean;            // @deprecated，仅为源码兼容；规范 auth 流程忽略
    login(callbacks: OAuthLoginCallbacks): Promise<OAuthCredentials>;
    refreshToken(credentials, signal: AbortSignal): Promise<OAuthCredentials>;
    getApiKey(credentials: OAuthCredentials): string;
    modifyModels?(models: Model<Api>[], credentials: OAuthCredentials): Model<Api>[];  // legacy 同步投影
  };
}
```

`streamSimple` 契约（逐字）：
- 必须在发送前调用 `options.onPayload` 并使用其返回的替换 payload；
- 必须在收到响应后、消费 body 前调用 `options.onResponse`；
- 可调用 `options.onProviderStreamEvent(data, model)`（解析后的流事件，归一化之前）；
- `context` 是归一化 transcript：prompt 与 tools 从 system messages 读取
  （`getCurrentSystemPrompt(context.messages)`、`getCurrentTools(context.messages)`）。

`ProviderModelConfigBase`：`id`、`name`、`api?`（string 覆盖）、`baseUrl?`、`input: ("text"|"image")[]`、
`inputLimits?`、`cost`、`headers?`。

- `ProviderChatModelConfig`：`type?: "chat"`（省略即 chat）、`api?: Api`、`reasoning: boolean`、
  `thinkingLevelMap?`、`promptCache?`（秒；未设则禁用 cache warming）、`contextWindow`、`maxTokens`、
  `samplingParams?`、`compat?`。
- `ProviderImageModelConfig`：`type: "image"`、`api?: ImageApi`、`output: ("text"|"image")[]`。
- `ProviderClassifierModelConfig`：`type: "classifier"`、`api?: ClassifierApi`、`contextWindow`。
- `ProviderModelConfig` = 联合。

**`ExtensionFactory = (kiana: ExtensionAPI) => void | Promise<void>`**

**`InlineExtension`**

```ts
type InlineExtension =
  | ExtensionFactory
  | { name: string;
      factory: ExtensionFactory;
      hidden?: boolean;        // 从启动 Extensions 列表省略
      replaceable?: boolean;   // 若有别的扩展注册了同名 tool/command/flag，本扩展被整体省略（不报冲突）
      builtin?: boolean };     // 用 builtin:<name> 路径加载；默认加载、kiana config 可见、可被禁用
```

文档要点（逐字）：
- `name`：启动 Extensions 列表与错误中显示为 `<inline:name>`；带 `builtin` 时错误/诊断中为 `builtin:name`。
- `replaceable`：被替换时本扩展**整体**省略；工厂仍会运行，所以它只应注册 tool/command/flag/event handler。
- `builtin`：`builtin:<name>` 是资源路径；默认加载、`kiana config` 列出、`-builtin:<name>`（`extensions` 设置）
  与 `--no-extensions` 可禁用、`-e builtin:<name>` 显式加载；从启动列表隐藏；在 project trust 解析**之后**加载，
  因此不能处理 `project_trust`。

#### 2.1.14 加载产物类型

```ts
interface RegisteredTool { definition: ToolDefinition; sourceInfo: SourceInfo }

interface ExtensionFlag { name: string; description?: string; type: "boolean" | "string";
  default?: boolean | string; extensionPath: string }

interface ExtensionShortcut { shortcut: KeyId; description?: string;
  handler: (ctx: ExtensionContext) => Promise<void> | void; extensionPath: string }

type HandlerFn = (...args: unknown[]) => Promise<unknown>;
```

15 个 action 型 handler 别名（导出）+ 2 个其它 handler 别名（`TerminalInputHandler`、`ProjectTrustHandler`）= **17 个 `*Handler` 别名**：
`SendMessageHandler`、`SendUserMessageHandler`、`AppendEntryHandler`、
`SetSessionNameHandler`、`GetSessionNameHandler`、`GetActiveToolsHandler`、`GetAllToolsHandler`、
`GetSettingsHandler`、`GetCommandsHandler`、`SetActiveToolsHandler`、`RefreshToolsHandler`、`SetModelHandler`、
`GetThinkingLevelHandler`、`SetThinkingLevelHandler`、`SetLabelHandler`。

**`ToolInfo`**

```ts
Pick<ToolDefinition, "name" | "description" | "parameters" | "promptGuidelines" | "reachesTools"> & {
  exposure: ToolExposure; namespace?: ToolNamespace; annotations?: ToolAnnotations; sourceInfo: SourceInfo;
}
```

**`ExtensionRuntimeState`**（loader 创建、注册期与运行期共用）

```ts
{
  flagValues: Map<string, boolean | string>;
  pendingProviderRegistrations: Array<{ name: string; config: ProviderConfig; extensionPath: string }>;
  pendingNativeProviderRegistrations: Array<{ provider: Provider; extensionPath: string }>;
  pendingVirtualModelRegistrations: Array<{ definition: VirtualModelDefinition; extensionPath: string }>;
  createContext: () => ExtensionContext;             // runner 绑定前抛错
  assertActive: () => void;                          // 陈旧实例调用时抛错
  invalidate: (message?: string) => void;
  trackEventBusSubscription: (unsubscribe: () => void) => () => void;
  registerProvider: (name, config, extensionPath?) => void;
  registerNativeProvider: (provider, extensionPath?) => void;
  unregisterProvider: (name, extensionPath?) => void;
  mcpServers: McpServerRegistry;
  registerVirtualModel: (definition, extensionPath?) => void;
  unregisterVirtualModel: (provider, id) => void;
}
```

**`ExtensionActions`**（**15 个**动作，见上表同名项；`ExtensionRuntime` 另有 `createContext`/`mcpServers`/`flagValues` 等非 action 字段）：
`sendMessage`、`sendUserMessage`、`appendEntry`、`setSessionName`、`getSessionName`、`setLabel`、
`getActiveTools`、`getAllTools`、`getSettings`、`setActiveTools`、`refreshTools`、`getCommands`、`setModel`、
`getThinkingLevel`、`setThinkingLevel`（共 15 个字段）。

**`ExtensionContextActions`**

```ts
{ getModel; getScopedModels; isIdle; isProjectTrusted; getSignal; abort; hasPendingMessages; shutdown;
  getContextUsage; compact; getSystemPrompt;
  getSystemPromptOptions?;                 // 缺省时用 normalizeBuildSystemPromptOptions({ cwd })
  executeTool?: (callerId, name, args, options) => Promise<AgentToolCallOutcome>;
  getCallableTools?: () => readonly AgentTool[] }
```

**`ExtensionCommandContextActions`**：`waitForIdle`、`newSession`、`fork`、`navigateTree`、`switchSession`、`reload`。

**`ExtensionRuntime extends ExtensionRuntimeState, ExtensionActions`**

**`Extension`**

```ts
{ path: string; resolvedPath: string; hidden?: boolean; replaceable?: boolean; sourceInfo: SourceInfo;
  handlers: Map<string, HandlerFn[]>;
  tools: Map<string, RegisteredTool>;
  messageRenderers: Map<string, MessageRenderer>;
  toolRenderers?: ToolRendererResolver[];
  markdownTransformer?: MarkdownTransformer;
  entryRenderers?: Map<string, EntryRenderer>;
  commands: Map<string, RegisteredCommand>;
  flags: Map<string, ExtensionFlag>;
  shortcuts: Map<KeyId, ExtensionShortcut> }
```

**`LoadExtensionsResult`**

```ts
{ extensions: Extension[];
  errors: Array<{ path: string; error: string }>;
  warnings?: Array<{ path: string; warning: string }>;
  runtime: ExtensionRuntime }   // 动作在 runner.initialize() 前是抛错桩
```

**`ExtensionError { extensionPath: string; event: string; error: string; stack?: string }`**

### 2.2 `src/core/extensions/loader.ts`

879 行。职责：jiti 装载 TS/JS、扩展工厂缓存、运行时桩、每个扩展的 API 事务、目录发现与标准位置装配。

#### 2.2.1 模块级常量与运行期探测

```ts
const isNodeSeaBinary =
  ("sea" in process.features && process.features.sea === true) ||
  process.getBuiltinModule("node:sea")?.isSea() === true;
const isTypeScriptSourceRuntime = !isBunBinary && path.extname(fileURLToPath(import.meta.url)) === ".ts";
const usesEmbeddedModules = isBunBinary || isNodeSeaBinary || isBundledNode;
```

三条解析分支（**必须逐条复刻行为差异**，见 §2.6）：

| 条件 | jiti 选项 |
|---|---|
| `usesEmbeddedModules`（Bun 编译二进制 / Node SEA / bundled Node） | `{ virtualModules: VIRTUAL_MODULES, tryNative: false }` |
| 否则 `isTypeScriptSourceRuntime`（源码 TS 运行） | `{ virtualModules: VIRTUAL_MODULES, tsconfigPaths: true }` |
| 否则（未打包的 Node 发行版） | `{ alias: getAliases() }` |

两者都叠加 `{ moduleCache: false }`（每次调用都重新求值模块体）。

延迟加载器：

```ts
let createJitiPromise: Promise<typeof createJiti> | undefined;
function getCreateJiti() {
  createJitiPromise ??= (usesEmbeddedModules ? import("./jiti-static-loader.ts") : import("./jiti-loader.ts"))
      .then(m => m.createJiti);
  return createJitiPromise;
}
let virtualModulesPromise: Promise<Record<string, unknown>> | undefined;
function getVirtualModules() { virtualModulesPromise ??= import("./virtual-modules.ts").then(m => m.VIRTUAL_MODULES); ... }
```

`getAliases()`（懒求值 + 记忆化）——**逐条路径构造**：

```
__dirname = dirname(fileURLToPath(import.meta.url))            // src/core/extensions
packageIndex = path.resolve(__dirname, "../..", "index.js")    // <pkg>/index.js
typeboxEntry        = require.resolve("typebox")
typeboxCompileEntry = require.resolve("typebox/compile")
typeboxValueEntry   = require.resolve("typebox/value")
packagesRoot = path.resolve(__dirname, "../../../../")
resolveWorkspaceOrImport(workspaceRelativePath, specifier):
    workspacePath = path.join(packagesRoot, workspaceRelativePath)
    return fs.existsSync(workspacePath) ? workspacePath : fileURLToPath(import.meta.resolve(specifier))
kianaAgentCoreEntry  = resolveWorkspaceOrImport("agent/dist/index.js", "@earendil-works/kiana-agent-core")
kianaTuiEntry        = resolveWorkspaceOrImport("tui/dist/index.js", "@earendil-works/kiana-tui")
kianaAiCompatEntry   = resolveWorkspaceOrImport("ai/dist/compat.js", "@earendil-works/kiana-ai/compat")
kianaAiModelsEntry   = resolveWorkspaceOrImport("ai/dist/models.js", "@earendil-works/kiana-ai/models")
kianaAiOauthEntry    = resolveWorkspaceOrImport("ai/dist/oauth.js", "@earendil-works/kiana-ai/oauth")
kianaAiProvidersEntry= resolveWorkspaceOrImport("ai/dist/providers/all.js", "@earendil-works/kiana-ai/providers/all")
```

**别名表（22 条键，逐字；`getAliases()` 的返回对象共 22 个键）**：

```
"@earendil-works/kiana-coding-agent"      -> packageIndex
"@earendil-works/kiana-agent-core"        -> kianaAgentCoreEntry
"@earendil-works/kiana-tui"               -> kianaTuiEntry
"@earendil-works/kiana-ai/providers/all"  -> kianaAiProvidersEntry
"@earendil-works/kiana-ai/compat"         -> kianaAiCompatEntry
"@earendil-works/kiana-ai/models"         -> kianaAiModelsEntry
"@earendil-works/kiana-ai/oauth"          -> kianaAiOauthEntry
"@earendil-works/kiana-ai"                -> kianaAiCompatEntry
"@mariozechner/kiana-coding-agent"        -> packageIndex
"@mariozechner/kiana-agent-core"          -> kianaAgentCoreEntry
"@mariozechner/kiana-tui"                 -> kianaTuiEntry
"@mariozechner/kiana-ai/providers/all"    -> kianaAiProvidersEntry
"@mariozechner/kiana-ai/compat"           -> kianaAiCompatEntry
"@mariozechner/kiana-ai/models"           -> kianaAiModelsEntry
"@mariozechner/kiana-ai/oauth"            -> kianaAiOauthEntry
"@mariozechner/kiana-ai"                  -> kianaAiCompatEntry
"typebox"                              -> typeboxEntry
"typebox/compile"                      -> typeboxCompileEntry
"typebox/value"                        -> typeboxValueEntry
"@sinclair/typebox"                    -> typeboxEntry
"@sinclair/typebox/compile"            -> typeboxCompileEntry
"@sinclair/typebox/value"              -> typeboxValueEntry
```

（共 22 条键；注意 kiana-ai 根解析到 **compat** 入口——旧全局 API 仍可用。）

#### 2.2.2 工厂缓存

```ts
let extensionCacheCwd: string | undefined;
let extensionCacheGeneration = 0;
const extensionCache = new Map<string, ExtensionFactory>();

export function clearExtensionCache(): void { extensionCache.clear(); extensionCacheCwd = undefined; extensionCacheGeneration++; }

function useExtensionCacheCwd(cwd: string): { cwd: string; generation: number } {
  const resolvedCwd = resolvePath(cwd);
  if (extensionCacheCwd !== undefined && extensionCacheCwd !== resolvedCwd) clearExtensionCache();
  extensionCacheCwd = resolvedCwd;
  return { cwd: resolvedCwd, generation: extensionCacheGeneration };
}
function isCurrentCacheToken(t): boolean { return t !== undefined && extensionCacheCwd === t.cwd && extensionCacheGeneration === t.generation; }
```

语义：
- 缓存键是 **resolved 扩展路径**，值是 **工厂函数**（模块体已被求值，但工厂体不会重跑——注意与
  `moduleCache: false` 的关系：`jiti.import` 每次重新执行模块，拿到新工厂；**只有命中缓存且 token 仍有效时**
  才复用旧工厂并因此**不重跑模块**）。
- 缓存只在 `loadExtensionsCached`（→ `loadExtensionsInternal(..., useCache = true)`）中启用；
  `loadExtensions`、`discoverAndLoadExtensions` **不用缓存**。
- cwd 变化自动清缓存；`clearExtensionCache()` 把 generation +1，先前的 token 全部失效。
- 工厂结果**不缓存 null**（模块无有效导出时返回 undefined，不写入 Map）。

#### 2.2.3 `createExtensionRuntime()`

返回 `ExtensionRuntime`：除 `refreshTools: () => {}` 外，全部动作方法为抛错桩。

**【逐字】** `notInitialized()` 抛：

```
Extension runtime not initialized. Action methods cannot be called during extension loading.
```

`setModel` 例外：`() => Promise.reject(new Error("Extension runtime not initialized"))`。
`refreshTools`：`() => {}`（注册工具在加载期合法，只有绑定后才需要刷新）。

内部状态：
- `state: { staleMessage?: string }`；`assertActive()`：有 staleMessage 即 `throw new Error(staleMessage)`。
- `eventBusUnsubscribers: Set<() => void>`。

`invalidate(message?)`：若已有 staleMessage 直接返回（**幂等，首次生效**）；否则设置
`state.staleMessage = message ?? <默认长文案>` 并调用并清空所有事件总线退订。

**【逐字】默认 stale 文案**（loader 与 runner 各有一份，内容相同）：

```
This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().
```

`trackEventBusSubscription(unsubscribe)`：

```
active = true
tracked = () => { if (!active) return; active = false; set.delete(tracked); unsubscribe(); }
set.add(tracked); return tracked;
```

（可重复调用；invalidate 时一次性全部退订。）

**pre-bind 注册队列**

```
registerProvider(name, config, extensionPath = "<unknown>") → pendingProviderRegistrations.push({name, config, extensionPath})
registerNativeProvider(provider, extensionPath = "<unknown>") → pendingNativeProviderRegistrations.push({provider, extensionPath})
unregisterProvider(name) →
    pendingProviderRegistrations = filter(r => r.name !== name)
    pendingNativeProviderRegistrations = filter(r => r.provider.id !== name)
registerVirtualModel(definition, extensionPath = "<unknown>") → pendingVirtualModelRegistrations.push(...)
unregisterVirtualModel(provider, id) → filter(({definition}) => definition.provider !== provider || definition.id !== id)
```

其它字段：`flagValues = new Map()`、`mcpServers = new McpServerRegistry()`、`createContext = notInitialized`。

#### 2.2.4 `createExtensionAPI(extension, runtime, cwd, eventBus)`

返回 `{ api, commit, discard }`。内部：

```
pendingFlagValues = new Map<string, boolean|string>()
pendingRuntimeChanges: Array<() => void> = []
loadingUnsubscribers: Array<() => void> = []
state: "loading" | "active" | "failed" = "loading"
```

`assertActive()`：

```
if (state === "failed") throw new Error(`Extension "${extension.path}" failed to load and its API is no longer active.`)
runtime.assertActive()
```

`applyRuntimeChange(change)`：loading → push 到 pendingRuntimeChanges；否则立即执行。
`clearPending()`：清空三个容器。

**`on(event, handler)`**：`assertActive()`；包一层 `(...args) => handler(...args)` 存入
`extension.handlers`；返回退订函数：从数组 splice 掉该 handler（找不到则返回），数组空时删除该 key。

**`registerTool(tool)`**

```
assertActive()
if (typeof tool.parameters !== "object" || tool.parameters === null || Array.isArray(tool.parameters))
    throw new Error(`Tool "${tool.name}" registered by extension "${extension.path}" must define an object parameter schema.`)
extension.tools.set(tool.name, { definition: tool, sourceInfo: extension.sourceInfo })
runtime.refreshTools()
```

【逐字】`Tool "<name>" registered by extension "<path>" must define an object parameter schema.`

**`registerCommand(name, options)`**

```
assertActive()
if (typeof name !== "string" || name.length === 0)
    throw new Error(`Command registered by extension "${extension.path}" must have a non-empty string name. Use kiana.registerCommand("name", { description, handler }).`)
if (typeof options?.handler !== "function")
    throw new Error(`Command "/${name}" registered by extension "${extension.path}" must define handler().`)
extension.commands.set(name, { name, sourceInfo: extension.sourceInfo, ...options })
```

**`registerShortcut(shortcut, options)`**：`assertActive()`；`extension.shortcuts.set(shortcut, { shortcut, extensionPath: extension.path, ...options })`。
（**不做** key 合法性校验；冲突在 `runner.getShortcuts` 处理。）

**`registerFlag(name, options)`**

```
assertActive()
if (options.default !== undefined && typeof options.default !== options.type)
    throw new Error(`Invalid default for flag "${name}": expected ${options.type}, got ${typeof options.default}`)
extension.flags.set(name, { name, extensionPath: extension.path, ...options })
if (options.default !== undefined && !runtime.flagValues.has(name)) {
  if (loading) { if (!pendingFlagValues.has(name)) pendingFlagValues.set(name, options.default) }
  else runtime.flagValues.set(name, options.default)
}
```

【逐字】`Invalid default for flag "<name>": expected <boolean|string>, got <typeof>`（`typeof` 取值如 `number`、`object`）。

**渲染器注册**

```
registerMessageRenderer(customType, renderer) → extension.messageRenderers.set(customType, renderer)
registerMarkdownTransformer(t)             → extension.markdownTransformer = t       // 单值
registerEntryRenderer(customType, renderer)→ (extension.entryRenderers ??= new Map()).set(...)
registerToolRenderer(resolver)             → (extension.toolRenderers ??= []).push(resolver)
```

**`getFlag(name)`**：`assertActive()`；未注册 → `undefined`；否则 `runtime.flagValues.has(name) ? runtime.flagValues.get(name) : pendingFlagValues.get(name)`。
（因此加载期读取自己刚注册的默认值可用。）

**动作方法与 flags/命令一致**：全部先 `assertActive()`，再委托 `runtime.*`。
`exec` 特例：`execCommand(command, args, options?.cwd ?? cwd, options)`（cwd 缺省是扩展加载时的 cwd）。

**`registerProvider`**（两个重载的共同实现）

```
assertActive()
if (typeof providerOrName === "string") {
  if (!config) throw new Error("Provider config is required when registering by name")   // 【逐字】
  applyRuntimeChange(() => runtime.registerProvider(providerOrName, config, extension.path))
  return
}
applyRuntimeChange(() => runtime.registerNativeProvider(providerOrName, extension.path))
```

`unregisterProvider(name)`：`applyRuntimeChange(() => runtime.unregisterProvider(name, extension.path))`。

**`registerMcpServer(name, config)`**

```
assertActive()
validated = validateMcpServerConfig(name, config)
if (typeof validated === "string")
    throw new Error(`Invalid MCP server registered by extension "${extension.path}": ${validated}`)
owner = runtime.mcpServers.get(name)?.extensionPath
if (owner !== undefined && owner !== extension.path)
    throw new Error(`MCP server "${name}" is already registered by extension "${owner}"`)
clash = runtime.mcpServers.list().find(s => s.name !== name && mcpNamespace(s.name) === mcpNamespace(name))
if (clash) throw new Error(`MCP server "${name}" conflicts with registered server "${clash.name}"`)
server = { name, config: structuredClone(validated), extensionPath: extension.path }
applyRuntimeChange(() => runtime.mcpServers.register(server))
```

名称与命名空间规则见 §4.4；`validateMcpServerConfig` 的错误文案全部是【逐字】（§4.4.2）。

`unregisterMcpServer(name)` → `applyRuntimeChange(() => runtime.mcpServers.unregister(name, extension.path))`
（只删自己注册的）。`getMcpServers()` → `runtime.mcpServers.list()`（副本，按注册顺序）。

**`registerVirtualModel(model)`**

```
assertActive()
definition = { ...model, route: (request) => model.route(request as ModelRouteRequest<TState>, runtime.createContext()) }
applyRuntimeChange(() => runtime.registerVirtualModel(definition, extension.path))
unregisterVirtualModel(provider, id) → applyRuntimeChange(() => runtime.unregisterVirtualModel(provider, id))
```

要点：路由在 runner 绑定之后运行，因此**每次请求现建上下文**。

**`events`**

```
emit(channel, data) { assertActive(); eventBus.emit(channel, data); }
on(channel, handler) {
  assertActive();
  const unsubscribe = runtime.trackEventBusSubscription(eventBus.on(channel, handler));
  if (state === "loading") loadingUnsubscribers.push(unsubscribe);
  return unsubscribe;
}
```

**`commit()`**

```
if (state !== "loading") return;
runtime.assertActive();
for (const [name, value] of pendingFlagValues) if (!runtime.flagValues.has(name)) runtime.flagValues.set(name, value);
for (const apply of pendingRuntimeChanges) apply();
state = "active";
clearPending();
```

**`discard()`**

```
if (state !== "loading") return;
state = "failed";
for (const unsubscribe of loadingUnsubscribers) unsubscribe();
clearPending();
```

**事务性验收点**（tests 会检查）：
1. 工厂抛错 → 已注册的 tools/commands/flags/handlers 留在 `extension` 上但该 `Extension` 不会进入结果
   （`initializeExtension` 抛错，`loadExtension` 捕获 → `{extension:null, error}`）；该 API 的所有后续调用抛
   "failed to load and its API is no longer active."。
2. 加载期注册的 flag 默认值直到 `commit()` 才写入 `runtime.flagValues`；加载期**显式** `setFlagValue` 优先。
3. 加载期的 provider/virtual model/MCP 变更排队，`commit()` 时按**注册顺序**应用。
4. `discard()` 后会退订加载期建立的事件总线订阅。

#### 2.2.5 模块加载与缓存判定

```ts
async function loadExtensionModule(extensionPath: string, cacheToken?: ExtensionCacheToken) {
  if (isCurrentCacheToken(cacheToken)) {
    const cachedFactory = extensionCache.get(extensionPath);
    if (cachedFactory) return cachedFactory;
  }
  const createJitiImpl = await getCreateJiti();
  const resolutionOptions = usesEmbeddedModules
      ? { virtualModules: await getVirtualModules(), tryNative: false }
      : isTypeScriptSourceRuntime
        ? { virtualModules: await getVirtualModules(), tsconfigPaths: true }
        : { alias: getAliases() };
  const jiti = createJitiImpl(import.meta.url, { moduleCache: false, ...resolutionOptions });
  const module = await jiti.import(extensionPath, { default: true });
  const factory = module as ExtensionFactory;
  if (typeof factory !== "function") return undefined;
  if (isCurrentCacheToken(cacheToken)) extensionCache.set(extensionPath, factory);
  return factory;
}
```

注意：缓存 key 是**已经 resolvePath 过的路径**（调用方传 `resolvedPath`）。
`jiti.import(path, { default: true })` 取模块的**默认导出**；不是函数即视为无效。

#### 2.2.6 `createExtension` / `initializeExtension` / `loadExtension`

```ts
function createExtension(extensionPath: string, resolvedPath: string): Extension {
  const source = getSyntheticPathSource(extensionPath) ?? "local";
  const baseDir = isSyntheticPath(extensionPath) ? undefined : path.dirname(resolvedPath);
  return { path: extensionPath, resolvedPath,
    sourceInfo: createSyntheticSourceInfo(extensionPath, { source, baseDir }),
    handlers: new Map(), tools: new Map(), messageRenderers: new Map(), entryRenderers: new Map(),
    commands: new Map(), flags: new Map(), shortcuts: new Map() };
}
```

`createSyntheticSourceInfo` 补齐 `scope: "temporary"`、`origin: "top-level"`（§4.6）。

```ts
async function initializeExtension(factory, extensionPath, resolvedPath, cwd, eventBus, runtime): Promise<Extension> {
  const extension = createExtension(extensionPath, resolvedPath);
  const load = createExtensionAPI(extension, runtime, cwd, eventBus);
  try { await factory(load.api); load.commit(); }
  catch (error) { load.discard(); throw error; }
  time(`${extensionPath} factory`, "extensions");    // KIANA_TIMING=1 时记录
  return extension;
}

async function loadExtension(extensionPath, cwd, eventBus, runtime, cacheToken?) {
  const resolvedPath = resolvePath(extensionPath, cwd, { normalizeUnicodeSpaces: true });
  try {
    const factory = await loadExtensionModule(resolvedPath, cacheToken);
    time(`${extensionPath} module import`, "extensions");
    if (!factory) return { extension: null, error: `Extension does not export a valid factory function: ${extensionPath}` };
    const extension = await initializeExtension(factory, extensionPath, resolvedPath, cwd, eventBus, runtime);
    return { extension, error: null };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return { extension: null, error: `Failed to load extension: ${message}` };
  }
}
```

**【逐字】两条加载错误**：
- `Extension does not export a valid factory function: <extensionPath>`（注意用**原始** path，不是 resolvedPath）
- `Failed to load extension: <message>`

**`loadExtensionFromFactory(factory, cwd, eventBus, runtime, extensionPath = "<inline>")`**

```ts
const resolvedCwd = resolvePath(cwd);
return initializeExtension(factory, extensionPath, extensionPath /* resolvedPath = path */, resolvedCwd, eventBus, runtime);
```

即内联扩展的 `resolvedPath === path === "<inline>"` 或 `"<inline:name>"` 或 `"builtin:mcp"`。

#### 2.2.7 批量加载

```ts
async function loadExtensionsInternal(paths, cwd, eventBus?, runtime?, useCache = false): Promise<LoadExtensionsResult> {
  const extensions = []; const errors = []; const warnings = [];
  const cacheToken = useCache ? useExtensionCacheCwd(cwd) : undefined;
  const resolvedCwd = cacheToken?.cwd ?? resolvePath(cwd);
  const resolvedEventBus = eventBus ?? createEventBus();
  const resolvedRuntime = runtime ?? createExtensionRuntime();
  for (const extPath of paths) {
    const { extension, error } = await loadExtension(extPath, resolvedCwd, resolvedEventBus, resolvedRuntime, cacheToken);
    if (error) { errors.push({ path: extPath, error }); continue; }
    if (extension) extensions.push(extension);
  }
  return { extensions, errors, warnings: [], runtime: resolvedRuntime };   // warnings 恒为空数组
}
```

`loadExtensions` = 无缓存版本；`loadExtensionsCached` = `useCache = true`。
**串行**加载（`for...of` + await），顺序即结果顺序与事件处理器顺序。

#### 2.2.8 发现规则

```ts
function isExtensionFile(name) { return name.endsWith(".ts") || name.endsWith(".js"); }
```

**`resolveExtensionEntries(dir)` → `string[] | null`**

```
1) dir/package.json 存在 → readKianaManifest(packageJsonPath)：
   若 manifest?.extensions?.length：
     entries = []
     for extPath of manifest.extensions:
        resolvedExtPath = path.resolve(dir, extPath)     // 注意：不展开 ~
        if fs.existsSync(resolvedExtPath) entries.push(resolvedExtPath)
     if (entries.length > 0) return entries
2) dir/index.ts 存在 → [index.ts]
3) dir/index.js 存在 → [index.js]
4) return null
```

`readKianaManifest`（`ca-cli` 拥有，形状见 §4.2）：只读 `pkg.kiana` 对象上的 `extensions/skills/prompts/themes`；
每项必须是**全字符串数组**；解析失败或 `kiana` 非对象返回 `null`。BOM 会被剥离。

验收点（来自 tests）：
- `~entry.ts` 与 `~/entry.ts` 必须被当作目录内的普通路径解析（**不能**做 tilde 展开）：
  `path.resolve(dir, "~entry.ts")` → `<dir>/~entry.ts`。见 `6260`/discovery 的 tilde 测试。
- `package.json` 的 `kiana.extensions` **优先于** `index.ts`。
- 只保留**存在**的条目；全都不存在则回退到 `index.ts`。
- 无 `kiana` 字段的 `package.json` 被忽略（不是错误）。

**`discoverExtensionsInDir(dir)` → `string[]`**

```
if (!existsSync(dir)) return []
try {
  entries = readdirSync(dir, { withFileTypes: true })
  for entry of entries:
    entryPath = join(dir, entry.name)
    // 1) 直接文件：.ts / .js（isFile 或 isSymbolicLink）
    if ((entry.isFile() || entry.isSymbolicLink()) && isExtensionFile(entry.name)) { push(entryPath); continue }
    // 2/3) 子目录或符号链接
    if (entry.isDirectory() || entry.isSymbolicLink()) {
       const entries2 = resolveExtensionEntries(entryPath); if (entries2) push(...entries2)
    }
} catch { return [] }
```

- **只递归一层**；更深层必须用 `package.json` manifest。
- readdir 顺序 = 文件系统顺序（Rust `read_dir` 顺序通常也如此，但测试只断言集合/排序后的结果；
  实现应按 OS 顺序并在测试里排序比较，同时**不得**自行排序，因为扩展加载顺序影响 handler 顺序）。
- 任何 IO 异常吞掉并返回 `[]`（包括部分已收集结果？——不会：`catch` 直接 `return []`）。

**`discoverAndLoadExtensions(configuredPaths, cwd, agentDir = getAgentDir(), eventBus?)`**

```
resolvedCwd = resolvePath(cwd); resolvedAgentDir = resolvePath(agentDir)
allPaths = []; seen = Set<string>()
addPaths(paths): for p of paths { const resolved = path.resolve(p); if (!seen.has(resolved)) { seen.add(resolved); allPaths.push(p) } }

// 1) 项目扩展：<cwd>/.kiana/extensions
addPaths(discoverExtensionsInDir(path.join(resolvedCwd, CONFIG_DIR_NAME, "extensions")))
// 2) 全局扩展：<agentDir>/extensions
addPaths(discoverExtensionsInDir(path.join(resolvedAgentDir, "extensions")))
// 3) 显式配置路径
for p of configuredPaths:
  resolved = resolvePath(p, resolvedCwd, { normalizeUnicodeSpaces: true })
  if (existsSync(resolved) && statSync(resolved).isDirectory()) {
     entries = resolveExtensionEntries(resolved)
     if (entries) { addPaths(entries); continue }
     addPaths(discoverExtensionsInDir(resolved)); continue
  }
  addPaths([resolved])

return loadExtensions(allPaths, resolvedCwd, eventBus)     // 不带缓存，用新 runtime
```

去重键是 `path.resolve(p)` 的**字符串**，但存入 `allPaths` 的是**原始** `p`。

#### 2.2.9 本文件内的导入依赖

```ts
import { CONFIG_DIR_NAME, getAgentDir, isBunBinary, isBundledNode } from "../../config.ts";
import { resolvePath } from "../../utils/paths.ts";
import { createEventBus } from "../event-bus.ts";
import { execCommand } from "../exec.ts";
import { McpServerRegistry, mcpNamespace, validateMcpServerConfig } from "../mcp-servers.ts";
import { readKianaManifest } from "../kiana-manifest.ts";
import { createSyntheticSourceInfo, getSyntheticPathSource, isSyntheticPath } from "../source-info.ts";
import { time } from "../timings.ts";
import { require } from "node:module"; // createRequire(import.meta.url) 用于 require.resolve("typebox")
```

`getAgentDir()` 默认 `<home>/.kiana/agent`（见 `ca-cli`）。

### 2.3 `src/core/extensions/runner.ts`

1565 行。本单元最核心、语义最密集的文件。Rust 侧建议实现为一个 `ExtensionRunner` 结构体 + 一组
`emit_*` 方法，每个方法的组合语义分开实现（不要试图泛化，语义差异是本质的）。

#### 2.3.1 保留的快捷键 id 清单

```ts
const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS = [
  "app.interrupt","app.clear","app.exit","app.suspend","app.thinking.cycle",
  "app.model.cycleForward","app.model.cycleBackward","app.model.select","app.tools.expand",
  "app.thinking.toggle","app.editor.external","app.message.copy","app.message.followUp",
  "tui.input.submit","tui.select.confirm","tui.select.cancel","tui.input.copy",
  "tui.editor.deleteToLineEnd",
] as const;
```

**18 项**。注释（逐字语义）：只保留“编辑器全局”快捷键；picker 专属绑定不在其中。

#### 2.3.2 `buildBuiltinKeybindings(resolvedKeybindings)`

```ts
type BuiltInKeyBindings = Partial<Record<KeyId, { keybinding: string; restrictOverride: boolean }>>;
```

算法：

```
for (const [keybinding, keys] of Object.entries(resolvedKeybindings)) {
  if (keys === undefined) continue;
  const keyList = Array.isArray(keys) ? keys : [keys];
  const restrictOverride = RESERVED.includes(keybinding);
  for (const key of keyList) {
    const normalizedKey = key.toLowerCase();
    const existing = builtinKeybindings[normalizedKey];
    if (existing?.restrictOverride && !restrictOverride) continue;   // 保留项胜出，与遍历顺序无关
    builtinKeybindings[normalizedKey] = { keybinding, restrictOverride };
  }
}
```

关键点：同一物理键被多个动作绑定时，**保留动作胜出**（先写入的非保留项会被后到的保留项覆盖；
先写入的保留项不会被非保留项覆盖）。

#### 2.3.3 `isUserBashEventResult(value)`

```
不是对象或为 null → false
hasOperations = candidate.operations !== undefined
hasResult = candidate.result !== undefined
if (hasOperations === hasResult) → false            // 两者都有或都没有都非法
if (hasOperations) {
   operations 必须是对象且 typeof operations.exec === "function"
} else {
   result 必须是对象，且：
     typeof result.output === "string"
     "exitCode" in result && (result.exitCode === undefined || typeof result.exitCode === "number")
     typeof result.cancelled === "boolean"
     typeof result.truncated === "boolean"
     (result.fullOutputPath === undefined || typeof result.fullOutputPath === "string")
}
```

注意三处容易移植错的细节（`test/extensions-runner.test.ts` 的 `it.each` 回归 #9068 会精确命中）：
1. `"exitCode" in result` 是**键存在性**检查，不是真值检查：`{ output, cancelled, truncated }` 缺 `exitCode`
   键即判非法（`exitCode` 显式写成 `undefined` 才合法）。
2. `hasOperations === hasResult` 时返回 false —— `{}`（两者皆无）与同时给 `operations` + `result` 都非法。
3. `operations` 分支只检查 `exec` 是函数，**不**检查其它字段；`result` 分支要求 `output` 为 string。

Rust 对应：
```rust
pub enum UserBashEventResult {
    Operations(Box<dyn BashOperations>),          // validate: exec 可调用
    Result(BashResultOverride),                  // output:String, exit_code:Option<i64>(键必须存在),
                                                 // cancelled:bool, truncated:bool, full_output_path:Option<String>
}
pub fn is_user_bash_event_result(v: &JsValue) -> bool
```
由于 `exec` 是 JS 函数、`exitCode` 的“键存在但为 undefined”不是 Rust `Option` 能表达的，
校验必须在 JS 边界完成（rquickjs 侧读 `Object::has("exitCode")` + `undefined` 判定），
再把校验结果以 tagged enum 交给 Rust。

#### 2.3.4 辅助类型

```ts
export type BeforeAgentStartTrigger =
  | Pick<BeforeAgentStartUserEvent, "messageType" | "prompt" | "images">
  | Pick<BeforeAgentStartCustomEvent, "messageType" | "message">;

interface BeforeAgentStartCombinedResult {
  messages: NonNullable<BeforeAgentStartEventResult["message"]>[];
  systemPromptOptions: NormalizedBuildSystemPromptOptions;
}

type RunnerEmitEvent = Exclude<ExtensionEvent,
  ToolCallEvent | ProjectTrustEvent | ToolResultEvent | UserBashEvent | ContextEvent | ContextWithSystemEvent |
  CacheWarmingDecisionEvent | BeforeProviderRequestEvent | BeforeProviderHeadersEvent | BeforeAgentStartEvent |
  MessageEndEvent | ResourcesDiscoverEvent | InputEvent | TurnEndEvent | AgentBeforeSettleEvent>;

type BoundaryBaseEvent = Omit<TurnEndEvent, "entries"|"continue"|"context">
                      | Omit<AgentBeforeSettleEvent, "entries"|"continue"|"context">;

interface BoundaryDispatchResult { entries: SessionBoundaryDraft[]; continue: boolean;
  context: BoundaryContextPreview; valid: boolean }

export type NewSessionHandler / ForkHandler / NavigateTreeHandler / SwitchSessionHandler / ReloadHandler / ShutdownHandler
export type ExtensionErrorListener = (error: ExtensionError) => void;
```

（`TurnEndEvent` 与 `AgentBeforeSettleEvent` 的 handler 有专用 `emitBoundary`；`emit()` 处理其余
RunnerEmitEvent。）

#### 2.3.5 模块级函数

**`emitSessionShutdownEvent(runner, event): Promise<boolean>`**

```
if (runner.hasHandlers("session_shutdown")) { await runner.emit(event); return true }
return false
```

**`snapshotEventHandlers(extensions, eventType)`**

```ts
extensions.map(ext => ({ ext, handlers: ext.handlers.get(eventType)?.slice() ?? [] }))
```

**快照语义（重要）**：每次派发开始时取快照。因此：
- handler 在派发中移除自己或邻居，不影响**本次**派发（tests: `allows self-removal without skipping neighboring handlers`、
  `keeps removed pending handlers in the current dispatch`）；
- 派发期间新增的注册只在**下一次**派发生效（`defers registrations made during dispatch until the next dispatch`）；
- 嵌套派发（handler 内再次 emit）使用**新**快照（`uses a fresh handler list for nested dispatches`）；
- 重复注册的同一个 handler 可独立移除（`removes duplicate registrations independently and cleans up the last handler`）。

**`sameMessages(left, right)`**：长度相同且逐项 `===`（引用相等）。

**`restoreSystemMessages(current, visible, returned)`**

```
if (sameMessages(returned, visible)) return current;          // 未改变：保留所有 system 消息原位（保护缓存前缀）
const head = getCurrentSystemMessage(current);
return head ? [head, ...returned] : returned;                 // 改变：把重放的 prompt 段与工具声明压成一条前导 system
```

文档（逐字）：未改变的会话保留每条 system 消息原位，支持模型的中途 system 消息缓存前缀；改变过的会话得到
“重放的 prompt 段 + 工具声明”作为**一条前导 system 消息**，因此裁剪/窗口化/从压缩摘要切片都不能丢弃它们。

**`emitProjectTrustEvent(extensionsResult, event, ctx)`**

```ts
for (const { ext, handlers } of snapshotEventHandlers(extensionsResult.extensions, "project_trust")) {
  for (const handler of handlers) {
    try {
      const r = await handler(event, ctx);
      if (r.trusted === "undecided") continue;
      return { result: r, errors };              // 第一个 yes/no 立即返回（含 errors so far）
    } catch (error) {
      errors.push({ extensionPath: ext.path, event: event.type,
                    error: error instanceof Error ? error.message : String(error),
                    stack: error instanceof Error ? error.stack : undefined });
    }
  }
}
return { errors };
```

注意：`project_trust` 是**唯一**会“短路返回”的普通事件，且返回类型不带 `cancel`。

#### 2.3.6 `noOpUIContext`

```ts
const noOpUIContext: ExtensionUIContext = {
  select: async () => undefined,
  confirm: async () => false,
  input: async () => undefined,
  notify: () => {},
  onTerminalInput: () => () => {},
  setStatus: () => {}, setWorkingMessage: () => {}, setWorkingVisible: () => {},
  setWorkingIndicator: () => {}, setHiddenThinkingLabel: () => {},
  setWidget: () => {}, setFooter: () => {}, setHeader: () => {}, setTitle: () => {},
  custom: async () => undefined as never,
  pasteToEditor: () => {}, setEditorText: () => {}, getEditorText: () => "",
  editor: async () => undefined, addAutocompleteProvider: () => {},
  setEditorComponent: () => {}, getEditorComponent: () => undefined,
  get theme() { return theme; },                     // 全局主题
  getAllThemes: () => [], getTheme: () => undefined,
  setTheme: (_theme) => ({ success: false, error: "UI not available" }),    // 【逐字】
  getToolsExpanded: () => false, setToolsExpanded: () => {},
};
```

`hasUI()` 的实现是比较引用：`this.uiContext !== noOpUIContext`。**Rust 侧**应改为 `Option<Arc<dyn UiContext>>`，
`has_ui = ui_context.is_some()`，但要注意 `setUIContext(undefined)` 必须让 `hasUI()` 变为 false。

#### 2.3.7 构造与绑定

```ts
constructor(extensions: Extension[], runtime: ExtensionRuntime, cwd: string,
            sessionManager: SessionManager, modelRegistry: ModelRegistry)
```

构造后默认值（逐条）：

| 字段 | 默认 |
|---|---|
| `uiContext` | `noOpUIContext` |
| `mode` | `"print"` |
| `getModel` | `() => undefined` |
| `getScopedModels` | `() => []` |
| `isIdleFn` | `() => true` |
| `isProjectTrustedFn` | `() => true` |
| `getSignalFn` | `() => undefined` |
| `waitForIdleFn` | `async () => {}` |
| `abortFn` | `() => {}` |
| `hasPendingMessagesFn` | `() => false` |
| `getContextUsageFn` | `() => undefined` |
| `compactFn` | `() => {}` |
| `getSystemPromptFn` | `() => ""` |
| `getSystemPromptOptionsFn` | `() => normalizeBuildSystemPromptOptions({ cwd: this.cwd })` |
| `executeToolFn` | `undefined` |
| `getCallableToolsFn` | `() => []` |
| `reportedMcpServers` | `new Set<string>()` |
| `newSessionHandler` / `forkHandler` / `navigateTreeHandler` / `switchSessionHandler` | `async () => ({ cancelled: false })` |
| `reloadHandler` | `async () => {}` |
| `shutdownHandler` | `() => {}` |
| `shortcutDiagnostics` / `commandDiagnostics` | `[]` |
| `staleMessage` | `undefined` |
| `uiPromptDepth` | `0` |
| `activeUIPrompt` | `undefined` |

**`bindCore(actions, contextActions, providerActions?)`** — 顺序严格：

1. 把 15 个 action 逐个复制到 `this.runtime`（`sendMessage`… `setThinkingLevel`）；
2. `this.runtime.createContext = () => this.createContext()`；
3. 复制 contextActions 到 runner 字段；`getSystemPromptOptions` 缺省时用
   `() => normalizeBuildSystemPromptOptions({ cwd: this.cwd })`；`getCallableTools` 缺省 `() => []`；
4. `this.runtime.mcpServers.setChangeListener(() => { void this.emit({type:"mcp_servers_change", servers: runtime.mcpServers.list()}); this.reportUnhandledMcpServers(); })`；
5. 冲刷 `pendingProviderRegistrations`（**逐个 try/catch**）：

```
try {
  providerActions?.registerProvider ? providerActions.registerProvider(name, config)
                                    : this.modelRegistry.registerProvider(name, config)
} catch (err) {
  this.emitError({ extensionPath, event: "register_provider",
                   error: err.message, stack: err.stack })
}
```

6. `runtime.pendingProviderRegistrations = []`；
7. 同样冲刷 `pendingNativeProviderRegistrations`（事件名同为 `"register_provider"`），然后清空；
8. `registerVirtualModel` 局部函数：`providerActions?.registerVirtualModel ? ... : this.modelRegistry.registerVirtualModel(definition)`；
   逐个冲刷 `pendingVirtualModelRegistrations`，错误事件名 **`"register_virtual_model"`**；然后清空；
9. 此后把 runtime 的 5 个注册函数替换为**立即生效**版本：

```
runtime.registerProvider      = (name, config) => providerActions?.registerProvider ? providerActions.registerProvider(name, config) : this.modelRegistry.registerProvider(name, config)
runtime.registerNativeProvider= (provider) => providerActions?.registerNativeProvider ? ... : this.modelRegistry.registerProvider(provider)
runtime.unregisterProvider    = (name) => providerActions?.unregisterProvider ? ... : this.modelRegistry.unregisterProvider(name)
runtime.registerVirtualModel  = registerVirtualModel
runtime.unregisterVirtualModel= (provider, id) => providerActions?.unregisterVirtualModel ? ... : this.modelRegistry.unregisterVirtualModel(provider, id)
```

**`bindCommandContext(actions?)`**：有 actions 就直接赋值 6 个字段；无 actions 则恢复为默认（空实现/`{cancelled:false}`）。

**`setUIContext(uiContext?, mode = "print")`**

```
this.uiContext = uiContext ? this.wrapUIPromptContext(uiContext) : noOpUIContext;
this.mode = mode;
```

#### 2.3.8 UI prompt 计数与事件

**`wrapUIPromptContext(ui)`**：用 `{...ui}` 浅拷贝（保留 `theme` getter 需注意——`{...ui}` 会把 getter 求值成值；
TS 侧实际如此，行为可接受）后替换 5 个方法：

```
select:  (title, options, opts) => withUIPrompt("select", title, () => ui.select(...))
confirm: (title, message, opts) => withUIPrompt("confirm", title, () => ui.confirm(...))
input:   (title, placeholder, opts) => withUIPrompt("input", title, () => ui.input(...))
editor:  (title, prefill) => withUIPrompt("editor", title, () => ui.editor(...))
custom:  (factory, options) => withUIPrompt("custom", undefined, () => ui.custom(...))
```

**`withUIPrompt(kind, title, run)`** —— **嵌套只发一次 start/end**：

```
outerPrompt = (this.uiPromptDepth++ === 0)
if (outerPrompt) {
  this.activeUIPrompt = { kind, title };
  emitUIPromptEvent({ type:"ui_prompt_start", reason:"ui_prompt", kind, ...(title ? { title } : {}) })
}
finish = () => {
  if (--this.uiPromptDepth > 0) return;
  this.uiPromptDepth = 0;
  const prompt = this.activeUIPrompt ?? { kind, title };
  this.activeUIPrompt = undefined;
  emitUIPromptEvent({ type:"ui_prompt_end", reason:"ui_prompt", kind: prompt.kind, ...(prompt.title ? {title: prompt.title} : {}) })
}
try { return run().finally(finish) } catch (err) { finish(); throw err }
```

注意：`run()` 同步抛错时也会 `finish()`（`finally` 不会执行，因为 `run()` 从未返回 Promise）。
`title` 为空字符串/undefined 时事件里**不含** `title` 字段。

**`emitUIPromptEvent(event)`**：`queueMicrotask(() => { void this.emit(event) })` —— 异步、不阻塞、错误被 emit 的
error 隔离吞掉。Rust 侧：`tokio::spawn`（或 `spawn_local`）异步派发，且不 await。

#### 2.3.9 只读访问器与注册表查询

| 方法 | 语义 |
|---|---|
| `getUIContext()` | 当前（包装后的）UI 上下文 |
| `hasUI()` | `uiContext !== noOpUIContext` |
| `getExtensionPaths()` | `extensions.map(e => e.path)` |
| `getAllRegisteredTools()` | 按扩展顺序合并；**同名第一个胜出**；返回 `RegisteredTool[]` |
| `getToolDefinition(name)` | 按扩展顺序找 `ext.tools.get(name)`，返回 `definition` |
| `getFlags()` | 合并所有 flags，**同名第一个胜出** |
| `setFlagValue(name, value)` | `runtime.flagValues.set(name, value)` |
| `getFlagValues()` | `new Map(runtime.flagValues)`（副本） |
| `getMessageRenderer(customType)` | 按扩展顺序，第一个有该 customType 的 |
| `getMarkdownTransformers()` | `extensions.flatMap(ext => ext.markdownTransformer ? [t] : [])`（按加载顺序） |
| `getEntryRenderer(customType)` | 同上（entryRenderers 可能不存在） |
| `resolveToolRenderers(toolName, base)` | 见 §2.3.12 |
| `getModelRegistry()` | 构造时的 modelRegistry |
| `getRegisteredCommands()` | `commandDiagnostics = []` 后返回 `resolveRegisteredCommands()` |
| `getCommandDiagnostics()` | 恒为最近一次 `getRegisteredCommands()` 之后 `[]`（当前实现从不 push —— 保留这个“空”行为） |
| `getCommand(name)` | 在 resolved 列表中按 **invocationName** 精确匹配 |
| `shutdown()` | `shutdownHandler()` |
| `getActiveTools()` | `assertActive()` 后 `runtime.getActiveTools()` |
| `invalidate(message = <长文案>)` | 若 `!staleMessage`：设置并 `runtime.invalidate(message)` |
| `onError(listener)` | 加入 `Set`，返回删除函数 |
| `emitError(error)` | 遍历 `Set` 调用（**同步**，无 try/catch） |
| `hasHandlers(eventType)` | 任一扩展有非空 handler 数组即 true |

**`getShortcuts(resolvedKeybindings)`（完整算法）**

```
this.shortcutDiagnostics = [];
builtinKeybindings = buildBuiltinKeybindings(resolvedKeybindings)
extensionShortcuts = new Map<KeyId, ExtensionShortcut>()
addDiagnostic(message, extensionPath) {
  this.shortcutDiagnostics.push({ type: "warning", message, path: extensionPath });
  if (!this.hasUI()) console.warn(message);         // 无 UI 时同时打到 stderr
}

for (const ext of this.extensions) {
  for (const [key, shortcut] of ext.shortcuts) {
    normalizedKey = key.toLowerCase()
    builtIn = builtinKeybindings[normalizedKey]
    if (builtIn?.restrictOverride === true) {
      addDiagnostic(`Extension shortcut '${key}' from ${shortcut.extensionPath} conflicts with built-in shortcut. Skipping.`, shortcut.extensionPath)
      continue
    }
    if (builtIn?.restrictOverride === false) {
      addDiagnostic(`Extension shortcut conflict: '${key}' is built-in shortcut for ${builtIn.keybinding} and ${shortcut.extensionPath}. Using ${shortcut.extensionPath}.`, shortcut.extensionPath)
      // 不 continue：允许覆盖
    }
    existing = extensionShortcuts.get(normalizedKey)
    if (existing) {
      addDiagnostic(`Extension shortcut conflict: '${key}' registered by both ${existing.extensionPath} and ${shortcut.extensionPath}. Using ${shortcut.extensionPath}.`, shortcut.extensionPath)
    }
    extensionShortcuts.set(normalizedKey, shortcut)      // 后注册者胜出
  }
}
return extensionShortcuts
```

三条冲突文案【逐字】：
1. `Extension shortcut '<key>' from <path> conflicts with built-in shortcut. Skipping.`
2. `Extension shortcut conflict: '<key>' is built-in shortcut for <keybinding> and <path>. Using <path>.`
3. `Extension shortcut conflict: '<key>' registered by both <existingPath> and <path>. Using <path>.`

`getShortcutDiagnostics()` 返回 `shortcutDiagnostics`。

**`resolveRegisteredCommands()`（重复命令的 invocationName 规则）**

```
commands = []; counts = Map<string, number>
for ext of extensions: for cmd of ext.commands.values(): { commands.push(cmd); counts[cmd.name]++ }

seen = Map<string, number>(); taken = Set<string>()
return commands.map(cmd => {
  occurrence = (seen.get(cmd.name) ?? 0) + 1; seen.set(cmd.name, occurrence)
  invocationName = counts.get(cmd.name) > 1 ? `${cmd.name}:${occurrence}` : cmd.name
  if (taken.has(invocationName)) {
    suffix = occurrence
    do { suffix++; invocationName = `${cmd.name}:${suffix}` } while (taken.has(invocationName))
  }
  taken.add(invocationName)
  return { ...cmd, invocationName }
})
```

语义：**只要同名出现 >1 次，所有实例都带 `:n` 后缀**（第 1 个是 `name:1`）；与其它名字撞车时向后找空位。
命令顺序 = 扩展加载顺序 × 每个扩展内 `commands` Map 的插入顺序。

**`reportUnhandledMcpServers()`**

```
if (this.hasHandlers("mcp_servers_change")) return;
for (const server of this.runtime.mcpServers.list()) {
  if (this.reportedMcpServers.has(server.name)) continue;
  this.reportedMcpServers.add(server.name);
  this.emitError({ extensionPath: server.extensionPath, event: "register_mcp_server",
                   error: `MCP server "${server.name}" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support` })
}
```

【逐字】错误文本见上。**每个 server 名只报一次**（Set 去重，跨调用保留）。

**`invalidate(message?)` / `assertActive()`**

```ts
invalidate(message = "<长文案>") { if (!this.staleMessage) { this.staleMessage = message; this.runtime.invalidate(message) } }
private assertActive() { if (this.staleMessage) throw new Error(this.staleMessage) }
```

#### 2.3.10 上下文构造

**`createContext(): ExtensionContext`** — 每次调用返回**新对象**，所有值都在**读取时**解析
（“changes via bindCore/bindUI are reflected”）。每个 getter 先 `runner.assertActive()`：

| 成员 | 读时行为 |
|---|---|
| `ui` | `assertActive(); return runner.uiContext` |
| `mode` | `assertActive(); return runner.mode` |
| `hasUI` | `assertActive(); return runner.hasUI()` |
| `cwd` | `assertActive(); return runner.cwd` |
| `sessionManager` | `assertActive(); return runner.sessionManager` |
| `modelRegistry` | `assertActive(); return runner.modelRegistry` |
| `model` | `assertActive(); return getModel()`（bindCore 时的快照引用） |
| `scopedModels` | `assertActive(); return getScopedModels()` |
| `thinkingLevel` | `assertActive(); return runner.runtime.getThinkingLevel()` |
| `isIdle()` | `assertActive(); return runner.isIdleFn()` |
| `isProjectTrusted()` | `assertActive(); return runner.isProjectTrustedFn()` |
| `signal` | `assertActive(); return runner.getSignalFn()` |
| `abort()` | `assertActive(); runner.abortFn()` |
| `hasPendingMessages()` | `assertActive(); runner.hasPendingMessagesFn()` |
| `shutdown()` | `assertActive(); runner.shutdownHandler()` |
| `getContextUsage()` | `assertActive(); runner.getContextUsageFn()` |
| `compact(opts)` | `assertActive(); runner.compactFn(opts)` |
| `getSystemPrompt()` | `assertActive(); runner.getSystemPromptFn()` |

**Rust 设计**：不要用 `struct { ui: ... }` 的 eager 字段；应实现为 trait 方法（`fn ui(&self) -> &dyn UiContext` 等），
因为“读时校验 stale + 读时取值”是测试断言的一部分（`2860-replaced-session-context` 检查 stale ctx 在
session 替换后抛错；`scopedModels` 测试检查读时反映最新动作）。

**`createToolContext(toolCallId, signal): ExtensionToolContext`**

```ts
return Object.defineProperties(this.createContext() as ExtensionToolContext, {
  tools: { get() { runner.assertActive(); return runner.getCallableToolsFn() } },
  executeTool: { value: async (name, args, options = {}) => {
      runner.assertActive();
      if (!runner.executeToolFn) return { toolCall: {type:"toolCall", id:`${toolCallId}/0`, name, arguments:{}},
        result: { content: [{type:"text", text:"Nested tool calls are not available in this context"}], details: {} },
        isError: true };
      return runner.executeToolFn(toolCallId, name, args, { ...options, signal: options.signal ?? signal });
  } },
});
```

**`createCommandContext(): ExtensionCommandContext`**

```ts
const context = Object.defineProperties({}, Object.getOwnPropertyDescriptors(this.createContext()));
context.getSystemPromptOptions = () => { this.assertActive(); return this.getSystemPromptOptionsFn() };
context.waitForIdle          = () => { this.assertActive(); return this.waitForIdleFn() };
context.newSession           = (o) => { this.assertActive(); return this.newSessionHandler(o) };
context.fork                = (e, o) => { this.assertActive(); return this.forkHandler(e, o) };
context.navigateTree        = (t, o) => { this.assertActive(); return this.navigateTreeHandler(t, o) };
context.switchSession       = (p, o) => { this.assertActive(); return this.switchSessionHandler(p, o) };
context.reload              = () => { this.assertActive(); return this.reloadHandler() };
```

注释（逐字）：必须用 `Object.getOwnPropertyDescriptors` 而不是对象展开，因为展开会把 `createContext()` 的
受保护 getter 提前求值一次，把旧值冻结进返回对象，从而**绕过 stale 检查**。Rust 侧天然按方法实现，无此问题。

#### 2.3.11 边界派发 `emitBoundary`

```ts
async emitBoundary(baseEvent: BoundaryBaseEvent,
  buildContext: (entries: SessionBoundaryDraft[]) => BoundaryContextPreview | Promise<BoundaryContextPreview>)
  : Promise<BoundaryDispatchResult>
```

算法：

```
ctx = this.createContext()
entries = []; shouldContinue = false
context = await buildContext(entries)      // 初始预览（buildContext 抛错则整个调用抛出）
valid = true

for ({ ext, handlers } of snapshotEventHandlers(this.extensions, baseEvent.type)) {
  for (handler of handlers) {
    event = { ...baseEvent, entries, continue: shouldContinue, context }
    try {
      r = await handler(event, ctx)
      if (r?.entries !== undefined) entries = r.entries
      if (r?.continue !== undefined) shouldContinue = r.continue
    } catch (err) {
      this.emitError({ extensionPath: ext.path, event: baseEvent.type, error: err.message, stack })
    }
    // 每个 handler 之后都重建预览，无论成功失败
    try { context = await buildContext(entries); valid = true }
    catch (err) {
      valid = false
      this.emitError({ extensionPath: ext.path, event: baseEvent.type,
                       error: `Invalid boundary entries: ${err.message}`, stack })
    }
  }
}
return valid ? { entries, continue: shouldContinue, context, valid: true }
             : { entries: [], continue: false, context, valid: false }
```

【逐字】`Invalid boundary entries: <message>`

语义要点（tests 明确断言）：
- 每个 handler 看到**目前累积**的 `entries` 与 `continue`（共享草稿链）；
- handler 返回 `{}` 或缺字段 → 不改变当前值（“preserves omitted result fields”）；
- handler 抛错 → 它自身的修改（在抛错前对 event.entries 的赋值不走，因为通过返回值）不生效，但**前一个** handler
  的修改保留（`keeps shared mutations made before a handler throws` 测的是 `event.entries` 数组内部原地改动，
  因为 `entries` 是同一数组引用，原地 push 会保留）；
- 预览非法（buildContext 抛错）→ 记录错误但**继续**让后续 handler 修复；只有最终仍非法才返回
  `{entries:[], continue:false, context, valid:false}`；
- `valid` 只反映**最后一次** buildContext 的结果。

`entries` 的原地可变性是关键：JS 里 handler 可以 `event.entries.push(...)`。Rust 侧 `event.entries` 应为
`Vec<Draft>` 的**克隆 + 写回**？不——要保持“原地修改在抛错时也保留”，Rust 应把 `entries` 作为 `&mut Vec` 或
`Arc<Mutex<Vec>>` 交给 handler，或把 event 设计为 `entries: Vec<Draft>` 并在 handler 返回后合并：
若 handler panic/Err 且它改了参数（`&mut`），修改自然保留。推荐：`handler(&mut BoundaryEvent, &ctx)` 形式，
让 `entries`/`continue` 是可变字段，返回值只作为覆盖。但必须同时支持“返回值省略 = 不变”。
具体 Rust 签名建议：

```rust
pub struct BoundaryEvent { pub kind: BoundaryKind, pub base: BoundaryBase, pub entries: Vec<SessionBoundaryDraft>,
                           pub r#continue: bool, pub context: BoundaryContextPreview }
pub type BoundaryHandler = Box<dyn Fn(&mut BoundaryEvent, &ExtensionContext) -> Result<Option<BoundaryResult>, Error> + Send + Sync>;
```

必须在 spec 实现时对照 tests：`chains shared draft proposals and preserves omitted result fields`、
`reports invalid boundary previews and lets later handlers repair the proposal`、
`keeps shared mutations made before a handler throws`。

#### 2.3.12 通用派发 `emit(event)`

```ts
async emit<TEvent extends RunnerEmitEvent>(event: TEvent): Promise<RunnerEmitResult<TEvent>> {
  const ctx = this.createContext();
  let result: SessionBeforeEventResult | undefined;
  for (const { ext, handlers } of snapshotEventHandlers(this.extensions, event.type)) {
    for (const handler of handlers) {
      try {
        const handlerResult = await handler(event, ctx);
        if (this.isSessionBeforeEvent(event) && handlerResult) {
          result = handlerResult;
          if (result.cancel) return result;         // 取消立即短路
        }
      } catch (err) {
        this.emitError({ extensionPath: ext.path, event: event.type, error: message, stack });
      }
    }
  }
  return result;
}
```

- 只有 4 个 `session_before_*` 事件有返回值语义：**最后一个非 cancel 的结果胜出**，`cancel: true` 立即返回；
- 其它事件：返回值被丢弃（通知型 / 原地修改型）；
- 全程错误隔离；
- `isSessionBeforeEvent` 判定 4 个 type。

**`emit()` 的使用约定**（来自 `RunnerEmitEvent` 排除表）：以下事件**不走** `emit()`，各有专用方法：
`tool_call`、`tool_result`、`user_bash`、`context`、`context_with_system`、`cache_warming_decision`、
`before_provider_request`、`before_provider_headers`、`before_agent_start`、`message_end`、
`resources_discover`、`input`、`turn_end` + `agent_before_settle`（用 `emitBoundary`）、`project_trust`（专用）。

#### 2.3.13 `resolveToolRenderers`

```ts
resolveToolRenderers(toolName, base: () => ToolRenderers | undefined): ToolRenderers | undefined {
  const resolvers = this.extensions.flatMap(ext => ext.toolRenderers ?? []);
  const resolve = (index) => index < resolvers.length ? resolvers[index](toolName, () => resolve(index + 1)) : base();
  return resolve(0);
}
```

`next()` 是**惰性**的：resolver 可以完全不调用 `next()` 而直接返回自己的渲染器（短路），也可以
`next() ?? mine` 只在无渲染器时填充。顺序：扩展加载顺序 → 已注册工具 → `base`。
任意 resolver 抛错会向上传播（无 try/catch）；调用方（HTML 导出/TUI）需自行处理。

#### 2.3.14 专用派发方法（逐个）

**`emitCacheWarmingDecision(event) => Promise<CacheWarmingAction>`**

```
ctx; action = event.action
for handlers: try { r = await handler(event, ctx); if (r?.action !== undefined) action = r.action } catch → emitError
return action
```

最后一个覆盖胜出；事件自身 `action` 是初始值。

**`emitMessageEnd(event) => Promise<AgentMessage | undefined>`**

```
ctx; currentMessage = event.message; modified = false
for handlers:
  try {
    r = await handler({ ...event, message: currentMessage }, ctx)
    if (!r?.message) continue
    if (r.message.role !== currentMessage.role) {
      emitError({ extensionPath, event: "message_end", error: "message_end handlers must return a message with the same role" })
      continue
    }
    currentMessage = r.message; modified = true
  } catch → emitError
return modified ? currentMessage : undefined
```

【逐字】`message_end handlers must return a message with the same role`

**`emitToolResult(event) => Promise<ToolResultEventResult | undefined>`**

```
ctx; currentEvent = { ...event }       // 浅拷贝，结构化字段共享引用
modified = false
for handlers:
  try {
    r = await handler(currentEvent, ctx)
    if (!r) continue
    if (r.content !== undefined) {
       currentEvent.content = r.content
       if (r.structuredContent === undefined) delete currentEvent.structuredContent   // 关键删除规则
       modified = true
    }
    if (r.details !== undefined) { currentEvent.details = r.details; modified = true }
    if (r.structuredContent !== undefined) { currentEvent.structuredContent = r.structuredContent; modified = true }
    if (r.isError !== undefined) { currentEvent.isError = r.isError; modified = true }
    if (r.usage !== undefined) { currentEvent.usage = r.usage; modified = true }
  } catch → emitError
if (!modified) return undefined
return { content, details, structuredContent, isError, usage }   // 即使某些字段原本 undefined 也显式包含
```

注意返回对象总是**包含全部 5 个键**（JS 里 `structuredContent: undefined` 的键在 JSON 序列化时消失，但对象
自身有该键）。Rust 侧用 `Option` + `#[serde(skip_serializing_if)]` 或自定义序列化以匹配 JSON。

**`emitToolCall(event) => Promise<ToolCallEventResult | undefined>`**

```
ctx; result = undefined
for ({ handlers } of snapshot) {          // 注意：不捕获 ext，也不需要
  for (handler of handlers) {
    const r = await handler(event, ctx);  // 无 try/catch：抛错向外传播（fail-closed）
    if (r) { result = r; if (result.block) return result }
  }
}
return result
```

**关键差异**：`tool_call` **没有**错误隔离。handler 抛错会传播到 `AgentSession._beforeToolCall`，
那里把调用当作被阻止（fail-safe）。文档：“A tool_call handler failure blocks the tool as a fail-safe”。
`block: true` 立即短路；后续 handler 不再运行。`terminate` 随 result 一起返回。
`event.input` 是共享可变对象——handler 原地修改让后续 handler 看到（Rust 需 `&mut` 或内部可变性）。

**`emitUserBash(event) => Promise<UserBashEventResult | undefined>`**

```
ctx
for handlers:
  try {
    r = await handler(event, ctx)
    if (r === undefined) continue                 // 交给下一个 handler，最终落回本地执行
    if (!isUserBashEventResult(r)) throw new Error("Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object")
    return r                                      // 短路
  } catch (err) {
    emitError({ extensionPath, event: "user_bash", error, stack })
    throw err                                     // 重新抛出：命令被阻止，不落回本地执行
  }
return undefined
```

【逐字】无效结果文案如上。

**`emitContext(messages) => Promise<AgentMessage[]>`** — 两阶段：

```
ctx; currentMessages = structuredClone(messages)         // 深克隆

// 阶段 1：context
for ({ext, handlers} of snapshot("context")) {
  for (handler of handlers) {
    try {
      visibleMessages = currentMessages.filter(m => m.role !== "system")
      visibleSnapshot = visibleMessages.slice()
      r = await handler({ type: "context", messages: visibleMessages }, ctx)
      returned = r?.messages ?? (sameMessages(visibleMessages, visibleSnapshot) ? undefined : visibleMessages)
      if (!returned) continue
      currentMessages = restoreSystemMessages(currentMessages, visibleSnapshot, returned)
    } catch → emitError(event: "context")
  }
}

// 阶段 2：context_with_system
for ({ext, handlers} of snapshot("context_with_system")) {
  for (handler of handlers) {
    try {
      hadLeadingSystemMessage = currentMessages[0]?.role === "system"
      r = await handler({ type: "context_with_system", messages: currentMessages }, ctx)
      currentMessages = r?.messages ?? currentMessages
      if (hadLeadingSystemMessage && currentMessages[0]?.role !== "system") {
        emitError({ extensionPath, event: "context_with_system",
          error: "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage()." })
      }
    } catch → emitError(event: "context_with_system")
  }
}
return currentMessages
```

【逐字】前导 system 丢失文案见上。注意：错误只报告，**仍然采纳** handler 的输出。

`context` 阶段支持两种修改方式：(a) 返回新数组（`result.messages`）；(b) **原地**改 `event.messages`
（`visibleMessages` 被 splice/push 后与 snapshot 不再逐项相等）——两者都触发 `restoreSystemMessages`。

**`emitBeforeProviderRequest(payload) => Promise<unknown>`**

```
currentPayload = payload
for handlers: try { r = await handler({type:"before_provider_request", payload: currentPayload}, ctx);
                    if (r !== undefined) currentPayload = r } catch → emitError
return currentPayload
```

`undefined` 返回值表示“不替换”（因此 handler 无法把 payload 显式替换为 `undefined`）。

**`emitBeforeProviderHeaders(headers) => Promise<ProviderHeaders>`**

```
ctx
for handlers: try { await handler({ type:"before_provider_headers", headers }, ctx) } catch → emitError
return headers            // 同一对象；返回值为 undefined、被忽略
```

**`emitBeforeAgentStart(trigger, systemPromptOptions) => Promise<BeforeAgentStartCombinedResult>`**

```
currentOptions = normalizeBuildSystemPromptOptions(systemPromptOptions)
renderCurrentSystemPrompt = () => buildSystemPrompt(currentOptions)
ctx = Object.defineProperties({}, getOwnPropertyDescriptors(this.createContext()))
ctx.getSystemPrompt = () => { this.assertActive(); return renderCurrentSystemPrompt() }
messages = []

for ({ext, handlers} of snapshot("before_agent_start")) {
  for (handler of handlers) {
    try {
      event = { ...trigger, type: "before_agent_start",
                get systemPrompt() { return renderCurrentSystemPrompt() },     // 惰性：读时渲染当前选项
                systemPromptOptions: currentOptions }                          // 同一可变对象
      r = await handler(event, ctx)
      if (r) {
        if (r.message) messages.push(r.message)
        if (r.systemPrompt !== undefined) currentOptions.forceSystemPrompt = r.systemPrompt
      }
    } catch → emitError
  }
}
return { messages, systemPromptOptions: currentOptions }
```

语义要点：
- `event.systemPrompt` 是 **getter**，每次读取都按“当前（已含前面 handler 改动）”的选项重新渲染；
- `event.systemPromptOptions` 是**同一个对象**，handler 原地修改让后续 handler 看到
  （所以 `ctx.getSystemPrompt()` 在 handler 内部读到的也是最新渲染结果——`849` 测试：
  “keeps ctx.getSystemPrompt() in sync with chained system prompt updates”）；
- 返回 `systemPrompt` 等价于设置 `forceSystemPrompt`（替换整段 prompt，但 transcript 仍记录结构化段落）；
- 返回的 `message` 全部收集，按 handler 顺序。

**`emitResourcesDiscover(cwd, reason)`**

```
ctx; skillPaths=[]; promptPaths=[]; themePaths=[]
for ({ext, handlers} of snapshot("resources_discover")) {
  for (handler of handlers) {
    try {
      r = await handler({ type:"resources_discover", cwd, reason }, ctx)
      if (r?.skillPaths?.length)  skillPaths.push(...r.skillPaths.map(p => ({ path: p, extensionPath: ext.path })))
      if (r?.promptPaths?.length) promptPaths.push(...)
      if (r?.themePaths?.length)  themePaths.push(...)
    } catch → emitError
  }
}
return { skillPaths, promptPaths, themePaths }         // 元素形如 { path, extensionPath }
```

**`emitInput(text, images, source, streamingBehavior?) => Promise<InputEventResult>`**

```
ctx; currentText = text; currentImages = images
for ({ext, handlers} of snapshot("input")) {
  for (handler of handlers) {
    try {
      r = await handler({ type:"input", text: currentText, images: currentImages, source, streamingBehavior }, ctx)
      if (r?.action === "handled") return r                       // 立即短路
      if (r?.action === "transform") {
        currentText = r.text
        currentImages = r.images ?? currentImages                  // 省略则保留原图
      }
    } catch → emitError
  }
}
return (currentText !== text || currentImages !== images)
     ? { action: "transform", text: currentText, images: currentImages }
     : { action: "continue" }
```

注意最后用**引用/值比较**决定是否报告 transform；`images` 可能是 `undefined`（返回值里仍带该键，
JSON 序列化时消失）。测试期望：`{ action: "transform", text: "T:hi", images: imgs }`（原图引用），
链式 transform 结果为 `{ action:"transform", text:"X[1][2]", images: undefined }`。

### 2.4 `src/core/extensions/index.ts`

225 行，**纯再导出**，无逻辑。Rust 侧对应 crate 的 `lib.rs` 的 `pub use` 面。必须导出的名字
（逐条，供模块边界检查）：

再导出类型（来自 `../slash-commands.ts`、`../source-info.ts`）：
`SlashCommandInfo`、`SlashCommandSource`、`SourceInfo`。

函数：
`createExtensionRuntime`、`discoverAndLoadExtensions`、`loadExtensionFromFactory`、`loadExtensions`、
`ExtensionRunner`（类）、`wrapRegisteredTool`、`wrapRegisteredTools`。

类型（runner.ts）：`ExtensionErrorListener`、`ForkHandler`、`NavigateTreeHandler`、`NewSessionHandler`、
`ShutdownHandler`、`SwitchSessionHandler`。

类型（types.ts，按源码字母序）：见 §2.1 全部导出。

**导出计数（供校验，已用脚本核对）**：
- `types.ts`：`export` 声明 175 个 + `export type { ... }` 再导出 11 个（去重后 **186** 个唯一名字）。
- `loader.ts` 6、`runner.ts` 11（含 `ExtensionRunner` 类）、`wrapper.ts` 2、`virtual-modules.ts` 1。
- **单元内导出面唯一名字合计 206** = 195 个本地声明（`types` 175 + `loader` 6 + `runner` 11 + `wrapper` 2 +
  `virtual-modules` 1）+ `types.ts` 从兄弟模块再导出的 11 个名字（`ExecOptions`、`ExecResult`、
  `BuildSystemPromptOptions`、`NormalizedBuildSystemPromptOptions`、`AgentToolResult`、`AgentToolUpdateCallback`、
  `ToolExecutionMode`、`AppKeybinding`、`KeybindingsManager`、`CacheWarmingDecisionEvent`、
  `CacheWarmingDecisionEventResult`）。
- `index.ts` 对外再导出 **193 个唯一名字**（无重复）：`types.ts` 177（167 类型 + 10 个类型守卫/`defineTool`）、`loader.ts` 4、`runner.ts` 7（`ExtensionRunner` + 6 个类型）、`wrapper.ts` 2、`slash-commands.ts` 2、`source-info.ts` 1。
- 因此“公开扩展 API”的验收计数：**`index.ts` 193**；若按单元内部导出面计：**206**。
关键分组（供校验）：
- 事件：`AfterProviderResponseEvent`、`AgentActivityOutcome`、`AgentBeforeSettleEvent`、
  `AgentBeforeSettleEventResult`、`AgentEndEvent`、`AgentSettledEvent`、`AgentStartEvent`、
  `BashToolCallEvent`、`BashToolResultEvent`、`BeforeAgentStartCustomEvent`、`BeforeAgentStartEvent`、
  `BeforeAgentStartEventResult`、`BeforeAgentStartUserEvent`、`BeforeProviderHeadersEvent`、
  `BeforeProviderRequestEvent`、`BeforeProviderRequestEventResult`、`BoundaryContextPreview`、
  `BoundaryResult`、`BoundaryState`、`CacheWarmingDecisionEvent`、`CacheWarmingDecisionEventResult`、
  `CompactionEntryDraft`、`ContextEditEntryDraft`、`ContextEvent`、`ContextEventResult`、
  `ContextWithSystemEvent`、`CustomEntryDraft`、`CustomMessageEntryDraft`、`CustomToolCallEvent`、
  `CustomToolResultEvent`、`EditToolCallEvent`、`EditToolResultEvent`、`FindToolCallEvent`、
  `FindToolResultEvent`、`GrepToolCallEvent`、`GrepToolResultEvent`、`InputEvent`、`InputEventResult`、
  `InputSource`、`LsToolCallEvent`、`LsToolResultEvent`、`McpServersChangeEvent`、`MessageEndEvent`、
  `MessageEndEventResult`、`MessageStartEvent`、`MessageUpdateEvent`、`ModelSelectEvent`、
  `ModelSelectSource`、`PowerShellToolCallEvent`、`PowerShellToolResultEvent`、`ProjectTrustContext`、
  `ProjectTrustEvent`、`ProjectTrustEventDecision`、`ProjectTrustEventResult`、`ProjectTrustHandler`、
  `ProviderStreamEvent`、`ReadToolCallEvent`、`ReadToolResultEvent`、`ResourcesDiscoverEvent`、
  `ResourcesDiscoverResult`、`SessionBeforeCompactEvent`、`SessionBeforeCompactResult`、
  `SessionBeforeForkEvent`、`SessionBeforeForkResult`、`SessionBeforeSwitchEvent`、
  `SessionBeforeSwitchResult`、`SessionBeforeTreeEvent`、`SessionBeforeTreeResult`、
  `SessionBoundaryDraft`、`SessionCompactEvent`、`SessionCompactFailedEvent`、`SessionEvent`、
  `SessionInfoChangedEvent`、`SessionShutdownEvent`、`SessionStartEvent`、`SessionTreeEvent`、
  `ThinkingLevelSelectEvent`、`ToolCallEvent`、`ToolCallEventResult`、`ToolExecutionEndEvent`、
  `ToolExecutionStartEvent`、`ToolExecutionUpdateEvent`、`ToolResultEvent`、`ToolResultEventResult`、
  `TreePreparation`、`TurnEndEvent`、`TurnEndEventResult`、`TurnStartEvent`、`UIPromptEndEvent`、
  `UIPromptKind`、`UIPromptStartEvent`、`UserBashEvent`、`UserBashEventResult`、`WriteToolCallEvent`、
  `WriteToolResultEvent`。
- 上下文/API：`CompactOptions`、`ContextUsage`、`Extension`、`ExtensionAPI`、`ExtensionActions`、
  `ExtensionCommandContext`、`ExtensionCommandContextActions`、`ExtensionContext`、
  `ExtensionContextActions`、`ExtensionError`、`ExtensionEvent`、`ExtensionFactory`、`ExtensionFlag`、
  `ExtensionHandler`、`ExtensionMode`、`ExtensionRuntime`、`ExtensionShortcut`、`ExtensionToolContext`、
  `ExtensionUIContext`、`ExtensionUIDialogOptions`、`ExtensionVirtualModel`、`ExtensionWidgetOptions`、
  `InlineExtension`、`LoadExtensionsResult`、`ReplacedSessionContext`。
- 工具：`ToolAnnotations`、`ToolDefinition`、`ToolExposure`、`ToolInfo`、`ToolLoadout`、
  `ToolLoadoutChanges`、`ToolNamespace`、`ToolRendererResolver`、`ToolRenderers`、
  `ToolRenderResultOptions`、`ToolExecutionMode`（re-export）、`AgentToolResult`（re-export）、
  `AgentToolUpdateCallback`（re-export）。
- 其它：`AppKeybinding`、`KeybindingsManager`（re-export）、`BuildSystemPromptOptions`、
  `NormalizedBuildSystemPromptOptions`、`ExecOptions`、`ExecResult`、`ExecuteToolOptions`、
  `MarkdownTransformContext`、`MarkdownTransformer`、`MessageRenderer`、`MessageRenderOptions`、
  `EntryRenderer`、`EntryRenderOptions`、`EditorFactory`、`AutocompleteProviderFactory`、
  `TerminalInputHandler`、`WidgetPlacement`、`WorkingIndicatorOptions`、`RegisteredCommand`、
  `RegisteredTool`、`ResolvedCommand`、`ProviderConfig`、`ProviderModelConfig`、
  `SendMessageHandler`、`SendUserMessageHandler`、`AppendEntryHandler`、`GetActiveToolsHandler`、
  `GetAllToolsHandler`、`GetCommandsHandler`、`GetThinkingLevelHandler`、`SetActiveToolsHandler`、
  `SetLabelHandler`、`SetModelHandler`、`SetThinkingLevelHandler`。
- 类型守卫（值导出）：`defineTool`、`isBashToolResult`、`isEditToolResult`、`isFindToolResult`、
  `isGrepToolResult`、`isLsToolResult`、`isPowerShellToolResult`、`isReadToolResult`、
  `isToolCallEventType`、`isWriteToolResult`。

`src/index.ts`（SDK 根，498 行，属 `ca-cli` 的“经由本单元导出”部分）再导出上述面；扩展可以
`import { <任何上述名字> } from "@earendil-works/kiana-coding-agent"`。Rust 侧无需“SDK 根再导出”这一层
（JS 扩展由 rquickjs 提供兼容 shim，见 §8.3）。

### 2.5 `src/core/extensions/wrapper.ts`

29 行，两函数：

```ts
export function wrapRegisteredTool(registeredTool: RegisteredTool, runner: ExtensionRunner): AgentTool {
  return wrapToolDefinition(registeredTool.definition, (toolCallId, signal) => runner.createToolContext(toolCallId, signal));
}
export function wrapRegisteredTools(registeredTools: RegisteredTool[], runner: ExtensionRunner): AgentTool[] {
  return registeredTools.map(tool => wrapRegisteredTool(tool, runner));
}
```

适配细节由 `ca-tools-compaction-export` 的 `wrapToolDefinition` 负责：

```ts
{ name, label, description, parameters, outputSchema, constrainedSampling, prepareArguments, executionMode,
  execute: (toolCallId, params, signal, onUpdate, ctx?) =>
      definition.execute(toolCallId, params, signal, onUpdate, ctx ?? ctxFactory?.(toolCallId, signal)) }
```

要点：**传入的 ctx 优先**（如果 agent-core 已给上下文，就不新建）；否则用 `createToolContext` 新建，
保证“扩展工具与事件处理器共享同一套上下文语义”。

Rust 侧：

```rust
pub fn wrap_registered_tool(rt: &RegisteredTool, runner: &ExtensionRunner) -> AgentTool;
pub fn wrap_registered_tools(rts: &[RegisteredTool], runner: &ExtensionRunner) -> Vec<AgentTool>;
```

注意 `ExtensionRunner` 需要以 `Arc` 共享给工具闭包（`create_tool_context` 会被调用多次、可能并发）。

### 2.6 `virtual-modules.ts` 与 jiti 装载器

#### 2.6.1 `VIRTUAL_MODULES`（**22 个键**，逐字）

```ts
{
  "typebox": bundledTypebox,
  "typebox/compile": bundledTypeboxCompile,
  "typebox/value": bundledTypeboxValue,
  "@sinclair/typebox": bundledTypebox,
  "@sinclair/typebox/compile": bundledTypeboxCompile,
  "@sinclair/typebox/value": bundledTypeboxValue,
  "@earendil-works/kiana-agent-core": bundledKianaAgentCore,
  "@earendil-works/kiana-tui": bundledKianaTui,
  "@earendil-works/kiana-ai": bundledKianaAiCompat,
  "@earendil-works/kiana-ai/compat": bundledKianaAiCompat,
  "@earendil-works/kiana-ai/models": bundledKianaAiModels,
  "@earendil-works/kiana-ai/oauth": bundledKianaAiOauth,
  "@earendil-works/kiana-ai/providers/all": bundledKianaAiProviders,
  "@earendil-works/kiana-coding-agent": bundledKianaCodingAgent,
  "@mariozechner/kiana-agent-core": bundledKianaAgentCore,
  "@mariozechner/kiana-tui": bundledKianaTui,
  "@mariozechner/kiana-ai": bundledKianaAiCompat,
  "@mariozechner/kiana-ai/compat": bundledKianaAiCompat,
  "@mariozechner/kiana-ai/models": bundledKianaAiModels,
  "@mariozechner/kiana-ai/oauth": bundledKianaAiOauth,
  "@mariozechner/kiana-ai/providers/all": bundledKianaAiProviders,
  "@mariozechner/kiana-coding-agent": bundledKianaCodingAgent,
}
```

（`@earendil-works/kiana-coding-agent` 与 `@mariozechner/kiana-coding-agent` 指向同一个
`bundledKianaCodingAgent`。）

注释（逐字语义）：`kiana-ai` 根解析到 **compat** 入口（核心入口的严格超集），以保持旧全局 API 运行期可用；
`@earendil-works/kiana-coding-agent` 的导入是安全的，因为 `loader.ts` 的导出**不**从 `index.ts` 再导出。

**关键验收点（`11156-kiana-ai-models-virtual-module.test.ts`）**：`@earendil-works/kiana-ai/models` 与
`@mariozechner/kiana-ai/models` 必须都解析到同一个 models 模块。

#### 2.6.2 jiti 装载器

```ts
// jiti-loader.ts
export { createJiti } from "jiti";
// jiti-static-loader.ts
export { createJiti } from "jiti/static";
```

注释：普通 Node 运行时用 jiti 的惰性 transform（只有在原生加载失败、且扩展需要转换时才加载 Babel）；
编译二进制需要 jiti 的静态入口，以便 Bun/SEA 打包器内嵌 Babel transform（模块本身仍然惰性）。

Rust 侧**没有对应实现**——这是本单元最大的移植决策点（§8.3）。

### 2.7 文档

#### 2.7.1 `docs/extensions.md`（400 行）——必须落到测试的契约点

1. **工厂可以同步或异步**；Kiana 等待异步工厂完成后才继续启动（允许启动期抓配置或注册 provider）。
2. **不要在工厂里启动进程/套接字/watcher/timer**：某些调用加载扩展但不启动会话。长生命周期资源从
   `session_start` 或用到它的命令/工具里启动；会话级资源在幂等 `session_shutdown` 中关闭。
3. 生命周期三图（§1.3 已文字化）。
4. **hook 顺序**（model request 6 步、tool execution 5 步）——与 §1.3 相同，测试会按顺序断言。
5. “Kiana 在 run 启动期间（含压缩检查与 `before_agent_start`）算忙，因此此时发送的消息进入正在启动的 run。”
6. “消息生命周期事件也覆盖 user 与 tool-result 消息；model request 列表只在每个 turn 中出现 assistant 流的
   位置。”
7. 组合语义表（Notification / In-place mutation / Chained transformation / Gate / Actionable boundary /
   Last override）——见 §2.3.14 各路派发实现。
8. `before_agent_start`：`event.messageType` 为 `"user"` 时带 `prompt` 与 `images`；`"custom"` 时带 `message`
   且无 `prompt`。返回的 `message` 对两种 run 都会加入。**优先改 prompt 段/selected tools/guidelines**，让 Kiana 追加
   transcript delta；返回 `systemPrompt` 或设置 `forceSystemPrompt` 会替换整个 prompt。
9. `message_end` 可替换已定稿消息但必须保留 role；`tool_call` 可改 input 或阻止；`tool_result` 组合、逐个看到
   前面改动。
10. `provider_stream_event`：每个解析后的 provider 流事件、归一化之前；`event.data` 是 Kiana 能拿到的最早结构化值，
    不一定等于原始 HTTP 字节或 SSE 帧；只读；只通知、不持久化；**按流顺序 await**，慢 handler 拖慢流；
    handler 错误只报告。
11. `context` 与 `context_with_system` 的差别（前者不含 system，Kiana 恢复；后者拥有完整 transcript，
    必须保持 index 0 有 system）。
12. `turn_end` / `agent_before_settle` 是“可操作边界”：可链式提议 `custom`、`custom_message`、`context_edit`、
    `compaction` 条目，并返回 `continue: true` 请求一次下一模型请求。**无条件 continuation 会死循环**。
13. `cache_warming_decision`：入参 `{type:"cache_warming_decision", warmCost, missCost, continuationProbability, action}`（继承 `CacheWarmingDecision`），handler 可返回 `{action:"warm"|"stop"}`；**最后返回 action 的 handler 胜出**（逐个覆盖，而非首个）。`action:"stop"` 会结束 warming 直到下一次真实请求。默认（无 handler 或都不返回）保持 `event.action`；调用点在 `sdk.ts:338`（`extensionRunnerRef.current?.emitCacheWarmingDecision(event) ?? event.action`）。
14. 同一 assistant 消息的工具调用可**并行**；不要假设兄弟调用/结果存在；`ctx.signal` 用于当前 turn 拥有的嵌套
    工作；命令与空闲会话事件通常没有操作 signal。
15. `user_bash`：返回 `undefined` → 传给下一个 handler，最终本地执行；返回 `operations` 或 `result` → 停止传播；
    **handler 失败会阻止命令**，不落回本地执行。
16. **工具契约**：自定义工具 = name + 模型可见 description + TypeBox 参数 schema + `execute()`；结果需
    `content` 与 `details`；无结构化 details 用 `details: undefined`；嵌套模型调用的 `usage` 要并入结果
    （会话总计准确）。从 `execute()` 抛错 = 失败工具结果；返回对象**不**标记为 error；`terminate: true` 只在
    **整批**每个完成的工具都同意终止时才生效。
17. 共享可变内存状态的工具用 `sequential`；改文件的工具用 `withFileMutationQueue()` 包住完整读-改-写；
    截断大结果并告诉模型完整输出位置。
18. `outputSchema` + 匹配的 `structuredContent`：模型仍收到 `content`；程序化调用方（codemode 脚本）收到
    `structuredContent` 而非文本。无 `outputSchema` 的工具以文本内容传给脚本。要报告“带数据的失败”，返回
    `isError: true` 的结果而不是抛错。
19. **嵌套调用**：`ctx.executeTool(name, args, {signal, onUpdate})` 经过参数校验与 `tool_call`/`tool_result`
    handler；发 `tool_execution_start/update/end`，都带 `parentToolCallId`；id 为 `<parent id>/<n>`；这些 id
    不出现在 transcript；嵌套调用不加 transcript 条目，结果只回到调用工具；会话在调用工具的 tool result message
    上保留有界记录 `nestedCalls`（name、arguments、status、duration、error；**从不含 results**），用于压缩文件
    列表与 HTML 导出。**每次调用参数超过 8 KiB 或每次工具结果超过 32 KiB 的省略；最多保留 256 次调用；
    `complete: false` 标记有丢失**。嵌套结果的 `usage`（任意深度）都加到调用工具的结果 `usage` 上。
    `ctx.tools` 列出 `ctx.executeTool()` 可调用的工具。
20. **exposure** 5 值与激活规则（§2.1.4）。
21. `prepareLoadout`：active 工具变化时运行；返回 declared 工具的替换 `descriptions`（含自身）与
    `hiddenDeclarations`（保持 active 与可调用，但请求里不声明；transcript 仍声明，从而 active 集在 `/tree` 与
    resume 后存活）。默认系统提示会把这些工具从工具列表与规则中省略；当文件读取器被隐藏时，skills 提示也不点名
    任何工具。
22. `reachesTools(input)`：`"all"` 或输入中可能命名它们的词（词匹配工具/命名空间名，可过匹配）。
23. **动态激活**：先注册全部工具，保持可选工具 inactive，用 `kiana.setActiveTools()` 选择；名字必须已注册，未知
    被忽略。Kiana 在 transcript 第一条 system 消息里记录初始 prompt 与工具集，然后在下一个模型请求前追加工具与
    prompt 变更；不能表达这个过渡的 provider 会收到完整 transcript checkpoint（可能使缓存前缀失效）。
24. **工具渲染**：`renderCall`/`renderResult` 同时用于交互式 transcript 与 HTML 导出；
    `registerToolRenderer((toolName, next) => renderers)` 可为任何工具（含未注册工具）选择渲染器；
    `next()` 返回剩余 resolver 与已注册工具的渲染器，所以 `next() ?? mine` 只是补位。
25. **MCP**：`registerMcpServer(name, config)`，`config` 形状同 `mcp.json` 的 `mcpServers` 条目：stdio 的
    `command`/`args`/`env`/`cwd`，HTTP 的 `url`/`headers`/`oauth`，外加 `exposure`、`toolExposure`、
    `description`、`enabled`、`timeout`。加载期注册的 server 在会话启动时与 `mcp.json` server 一起连接；
    加载后注册的立即连接；`unregisterMcpServer()` 关闭连接并使工具不可达。注册不保存；同名 `mcp.json` server
    优先，`/mcp` 显示覆盖。同名重复注册替换本扩展先前的注册；别的扩展注册的名字、非法名字、非法 config 抛错。
26. **上下文与会话变更**：`ExtensionContext` 提供 cwd、mode、UI、session manager、model runtime、abort signal、
    context usage、压缩与关闭控制；`ctx.modelRegistry.streamSimple()` 做 provider 中立嵌套模型调用。
    `ExtensionCommandContext` 增加 wait-until-idle、reload、tree 导航、会话替换（**仅命令**，从生命周期处理器
    调用会死锁）。会话替换使旧上下文失效：切换前只捕获纯数据，替换后用 `withSession` 给的新 ctx。
27. **状态存放**：工具状态跟随活动分支 → tool-result `details`；不进模型上下文的持久数据 → `kiana.appendEntry()`；
    既要存储又发给模型的自定义内容 → `kiana.sendMessage()`；跨会话数据 → 外部存储。分支敏感状态在 `session_start`
    用 `ctx.sessionManager.getBranch()` 重建；不要用每个文件条目重建（被放弃的分支代表替代历史）。
    自定义存储内容要出现在 transcript 时注册 entry 或 message renderer。
28. **UI 与模式**：扩展在 interactive、RPC、JSON、print 四种模式都加载；交互式提供完整终端 UI；RPC 可转发
    受支持对话框与通知但不支持自定义终端组件；JSON 与 print 无 UI。用 `ctx.mode === "tui"` 守护终端专属行为，
    用 `ctx.hasUI` 判断交互式与 RPC 客户端支持的交互。工具与事件行为要与渲染解耦。
29. **错误与清理**：Kiana 报告 handler 错误并尽可能继续；`tool_call` handler 失败 → 工具被阻止（fail-safe）；
    工具执行失败 → 给模型的错误结果。在 `session_shutdown` 释放资源，即使正常路径已尝试清理；清理必须幂等
    （取消、reload、会话替换、进程退出可能收敛到同一路径）。`ctx.shutdown()` 请求有序进程关闭。
30. 示例参考表（permission-gate / prompt-customizer / todo / dynamic-tools / qna / modal-editor /
    auto-commit-on-exit / ssh / custom-provider-gitlab-duo）——这些就是 §3.9 的兼容语料清单。

#### 2.7.2 `docs/rpc-extension-ui.md`（200 行）

见 §4.3（协议形状逐字）与 §2.8.2（实现）。

#### 2.7.3 `docs/tui.md`（195 行）——扩展组件的契约要点

- 组件为给定宽度渲染**行数组**；可选处理键盘/鼠标输入；状态或主题相关输出变化时必须**失效缓存**。
- 每行必须落在给定宽度内；用 `visibleWidth()`、`truncateToWidth()`、`sliceByColumn()`、`wrapTextWithAnsi()`，
  不要自己算宽度（ANSI、宽字符、emoji、组合字符）。Kiana 每行后重置样式与超链接，所以每行都要重新应用样式。
- 状态变化后失效组件并调用注入的 `tui.requestRender()`；TUI 会合并渲染请求。
- 组件清单：`Text`/`Markdown`/`Image`/`TruncatedText`；`Container`/`VStack`/`HStack`/`Box`/`Spacer`；
  `Input`/`Editor`；`SelectList`/`SettingsList`；`ScrollView`；`Loader`/`CancellableLoader`；`MouseRegion`。
- 键盘：`matchesKey()` 与 `Key`；扩展组件应使用注入的 `KeybindingsManager`。
- 显示文本光标的组件必须实现 `Focusable` 并把 `CURSOR_MARKER` 放在可视光标**紧前**；用 `renderFakeCursor()`
  包住光标单元（TUI 画反色；硬件光标模式下在 marker 之后隐藏）。容器必须把 `focused` 传播给子 `Input`/`Editor`，
  否则中日韩 IME 候选窗口会出现在错误位置。
- 替换主编辑器时扩展 `CustomEditor`（保留应用快捷键与 agent 控制）；把不处理的键转发给基类；通过清空
  custom editor factory 恢复默认。
- 鼠标：仅 fullscreen 路由给组件；未处理的滚轮滚动最近 `ScrollView`；未处理的主键拖拽留给 transcript 选择；
  OSC 8 链接优先于包围的点击区域。regular 模式不接管鼠标（终端拥有 scrollback）——每个交互都要有键盘路径。
- `ctx.ui.custom()` 临时把一个组件交给交互区并在其调用完成回调时 resolve。`overlay: true` 画在现有内容之上；
  overlay 选项控制大小、锚点、偏移、边距、响应式可见性；`OverlayHandle` 可改焦点或暂时 `setHidden()`；
  有焦点的 overlay 跨普通渲染持有输入；自定义 overlay 不要调用 `OverlayHandle.hide()`（用完成回调）。
  每个组件实例属于一次交互。
- 主题：用回调传入的 `theme`；`theme.style(text, { fg, bg, bold })` 组合前景/背景与属性；
  `theme.colors` 取具体色；`theme.appearance` 为 `"dark" | "light"`；用 `getMarkdownTheme()` 渲染 Markdown；
  **不要把带主题色的字符串长期存储**（除非 `invalidate()` 会重建）。
- 性能：渲染在交互路径上；按宽度与内容缓存昂贵布局/高亮，并在 `invalidate()` 清缓存；自定义工具渲染要处理
  部分结果，可在能安全更新时复用上一次组件。`KIANA_TUI_WRITE_LOG` 捕获原始 ANSI 流用于诊断。

示例（扩展组件模式）：`preset.ts`、`tools.ts`（选择与设置列表）、`qna.ts`（可取消异步 UI）、
`modal-editor.ts`（替换编辑器）、`custom-footer.ts`、`widget-placement.ts`、`doom-overlay/`（持续渲染 overlay）。

#### 2.7.4 `docs/virtual-models.md`（114 行，交叉参考）

`ca-config-models` 实现主体；本单元只需保证：
- `kiana.registerVirtualModel()` / `unregisterVirtualModel()` 的排队与 reload 规则与 `registerProvider` 一致；
- `route(request, ctx)` 的 `ctx` 是**每次请求新建**的扩展上下文；
- 路由失败（抛错、返回虚拟模型、返回无凭据模型）→ 请求以错误响应结束；
- assistant 消息记录**物理** model/thinkingLevel；选择留在 `model_change` / `thinking_level_change`；
- `thinkingLevels` 默认 `["off"]`；`input` 默认文本+图片；
- 状态：`undefined` 或返回 `request.state` 本身保持当前状态；其它对象作为新状态在请求发出**前**存储（即使随后失败）；
  `direct` 请求无状态且忽略返回的状态。

### 2.8 适配层：RPC / 交互式 / print / JSON

#### 2.8.1 交互式模式（`src/modes/interactive/interactive-mode.ts`）

**`createExtensionUIContext()` 映射**（行 2633 起）：

| 方法 | 实现 |
|---|---|
| `select` | `this.showExtensionSelector(title, options, opts)` |
| `confirm` | `this.showExtensionConfirm(title, message, opts)` |
| `input` | `this.showExtensionInput(title, placeholder, opts)` |
| `notify` | `this.showExtensionNotify(message, type)` |
| `onTerminalInput` | `this.addExtensionTerminalInputListener(handler)` |
| `setStatus` | `this.setExtensionStatus(key, text)` |
| `setWorkingMessage` | 设 `workingMessage`；若当前状态指示器 kind === "working"，`setMessage(message ?? defaultWorkingMessage)` |
| `setWorkingVisible` / `setWorkingIndicator` / `setHiddenThinkingLabel` | 对应私有方法 |
| `setWidget` | `this.setExtensionWidget(key, content, options)` |
| `setFooter` / `setHeader` | `this.setExtensionFooter/Header(factory)` |
| `setTitle` | `this.ui.terminal.setTitle(title)` |
| `custom` | `this.showExtensionCustom(factory, options)` |
| `pasteToEditor` | `this.editor.handleInput(`\x1b[200~${text}\x1b[201~`)`——**bracketed paste**，从而触发粘贴折叠 |
| `setEditorText` | `this.editor.setText(text)` |
| `getEditorText` | `this.editor.getExpandedText?.() ?? this.editor.getText()` |
| `editor` | `this.showExtensionEditor(title, prefill)` |
| `addAutocompleteProvider` | push 到 `autocompleteProviderWrappers` 后 `setupAutocompleteProvider()` |
| `setEditorComponent` | `this.setCustomEditorComponent(factory)` |
| `getEditorComponent` | `this.editorComponentFactory` |
| `theme`（getter） | 全局 `theme` |
| `getAllThemes` | `getAvailableThemesWithPaths()` |
| `getTheme` | `getThemeByName(name)` |
| `setTheme` | `Theme` 实例 → `themeController.setThemeInstance(...)`；字符串 → `themeController.setThemeName(name)`，成功且设置值不同则 `settingsManager.setTheme(name)`；返回 `{success, error?}` |
| `getToolsExpanded` | `this.toolOutputExpanded` |
| `setToolsExpanded` | `this.setToolsExpanded(expanded)` |

**对话框栈（`pushExtensionDialog` / `removeExtensionDialog` / `showTopExtensionDialog`）**

```
pushExtensionDialog(component, blocked?) {
  this.disposeActiveSelector();
  this.extensionDialogs.push({ component, blocked });
  this.showTopExtensionDialog();
}
removeExtensionDialog(component) {
  index = findIndex(d => d.component === component); if (index === -1) return
  splice(index, 1)
  if (index === this.extensionDialogs.length) this.showTopExtensionDialog()     // 只在其为栈顶时刷新
}
showTopExtensionDialog() {
  top = this.extensionDialogs.at(-1)
  component = top?.component ?? this.editor
  this.editorContainer.clear(); this.editorContainer.addChild(component)
  this.ui.setFocus(component)
  this.programStatus.setBlocked("extension-dialog", top?.blocked)
  this.ui.requestRender()
}
```

文档（逐字语义）：对话框形成栈——新对话框隐藏当前可见者，关闭最上层后重新显示前一个。**每个对话框都保持可回答**，
因此没有对话框 Promise 会挂起；在另一个对话框内部打开的对话框（例如自定义组件里弹 confirm）可正常工作。

**`showExtensionSelector(title, options, opts?, blocked = { kind: "question", message: title })`**

```
new Promise(resolve => {
  if (opts?.signal?.aborted) { resolve(undefined); return }
  finish(option) { opts?.signal?.removeEventListener("abort", onAbort); selector.dispose(); removeExtensionDialog(selector); resolve(option) }
  onAbort = () => finish(undefined)
  opts?.signal?.addEventListener("abort", onAbort, { once: true })
  selector = new ExtensionSelectorComponent(title, options, o => finish(o), () => finish(undefined),
     { tui: this.ui, timeout: opts?.timeout, onToggleToolsExpanded: () => this.toggleToolOutputExpansion() })
  pushExtensionDialog(selector, blocked)
})
```

**`showExtensionConfirm(title, message, opts?)`**

```
result = await this.showExtensionSelector(`${title}\n${message}`, ["Yes", "No"], opts, { kind: "permission", message: title })
return result === "Yes"
```

【逐字】确认框标题是 `title + "\n" + message`，选项为 `["Yes", "No"]`，选中 `"Yes"` 为 true。
`blocked` 用 `kind: "permission"`，`message: title`。

**`showExtensionInput(title, placeholder, opts?)`**：结构与 selector 相同，组件 `ExtensionInputComponent`，
`blocked = { kind: "question", message: title }`；`placeholder` 参数在组件里是 `_placeholder`（**当前不显示**）。

**`showExtensionEditor(title, prefill?)`**：`ExtensionEditorComponent(this.ui, this.keybindings, title, prefill,
finish, () => finish(undefined), undefined, settingsManager.getExternalEditorCommand())`，
`blocked = { kind: "question", message: title }`。**无 signal/timeout 支持**。

**`showExtensionCustom(factory, options?)`**

```
savedText = this.editor.getText(); isOverlay = options?.overlay ?? false
new Promise((resolve, reject) => {
  component; overlayHandle; closed = false
  restoreEditor() { if (component) removeExtensionDialog(component); this.editor.setText(savedText); this.ui.requestRender() }
  close(result) {
    if (closed) return; closed = true
    if (isOverlay) overlayHandle?.hide(); else restoreEditor()     // 只关自己这个 overlay
    resolve(result)
    try { component?.dispose?.() } catch { /* 忽略 dispose 错误 */ }
  }
  Promise.resolve(factory(this.ui, theme, this.keybindings, close)).then(c => {
    if (closed) return
    component = c
    if (isOverlay) {
      resolveOptions = () => options?.overlayOptions
          ? (typeof options.overlayOptions === "function" ? options.overlayOptions() : options.overlayOptions)
          : (c.width ? { width: c.width } : undefined)
      overlayHandle = this.ui.showOverlay(c, resolveOptions())
      options?.onHandle?.(overlayHandle)
    } else pushExtensionDialog(c)
  }).catch(err => { if (closed) return; if (!isOverlay) restoreEditor(); reject(err) })
})
```

**`showExtensionNotify(message, type?)`**：`"error"` → `showError`；`"warning"` → `showWarning`；
否则（含 `"info"` 与 undefined）→ `showStatus`。

**`showExtensionError(extensionPath, error, stack?)`**（runner 的 `onError` 落点）

```
errorMsg = `Extension "${extensionPath}" error: ${error}`          // 【逐字】
chatContainer.addChild(new ThemedText(() => theme.fg("error", errorMsg), 1, 0))
if (stack) {  // 跳过第一行（与 errorMsg 重复），其余行 dim + 缩进
```

**`setupExtensionShortcuts(extensionRunner)`**

```
shortcuts = extensionRunner.getShortcuts(this.keybindings.getEffectiveConfig())
if (shortcuts.size === 0) return
createContext() → 一个 mode:"tui"、hasUI:true 的 ExtensionContext（见下）
this.defaultEditor.onExtensionShortcut = (data) => {
  for ([shortcutStr, shortcut] of shortcuts)
    if (matchesKey(data, shortcutStr)) {
      Promise.resolve(shortcut.handler(createContext())).catch(err =>
        this.showError(`Shortcut handler error: ${err instanceof Error ? err.message : String(err)}`))   // 【逐字】
      return true
    }
  return false
}
```

**逐字** `Shortcut handler error: <message>`（`<message>` = `err instanceof Error ? err.message : String(err)`）

快捷键处理器用的 `createContext()` 是**独立构造**的（不走 runner.createContext）：
`ui: this.createExtensionUIContext()`、`mode:"tui"`、`hasUI:true`、`cwd: sessionManager.getCwd()`、
`sessionManager`、`modelRegistry: extensionRunner.getModelRegistry()`、`model: session.model`、
`scopedModels: session.scopedModels`、`thinkingLevel: session.thinkingLevel`、
`isIdle: () => session.isIdle`、`isProjectTrusted: () => settingsManager.isProjectTrusted()`、
`signal: session.agent.signal`、`abort: () => this.restoreQueuedMessagesToEditor({ abort: true })`、
`hasPendingMessages: () => session.pendingMessageCount > 0`、`shutdown: () => this.shutdownRequested = true`、
`getContextUsage`、`compact(options)`（异步执行 + onComplete/onError）、`getSystemPrompt: () => session.systemPrompt`。

**`setExtensionWidget(key, content, options?)`**

```
placement = options?.placement ?? "aboveEditor"
removeExisting(map) { const e = map.get(key); e?.dispose?.(); map.delete(key) }
removeExisting(extensionWidgetsAbove); removeExisting(extensionWidgetsBelow)
if (content === undefined) { renderWidgets(); return }
if (Array.isArray(content)) {
  container = new Container()
  for (const line of content.slice(0, MAX_WIDGET_LINES /* 10 */)) container.addChild(new Text(line, 1, 0))
  if (content.length > MAX_WIDGET_LINES)
      container.addChild(new ThemedText(() => theme.fg("muted", "... (widget truncated)"), 1, 0))   // 【逐字】
  component = container
} else component = content(this.ui, theme)
target = placement === "belowEditor" ? extensionWidgetsBelow : extensionWidgetsAbove
target.set(key, component)
renderWidgets()
```

【逐字】截断行 `... (widget truncated)`，muted 色，缩进 1/0。widget 文本行用 `Text(line, 1, 0)`（左内缩 1）。

**`renderWidgets()` / `renderWidgetContainer`**

```
上方容器：spacerWhenEmpty = true, leadingSpacer = true
下方容器：spacerWhenEmpty = false, leadingSpacer = false
若 widgets 为空且 spacerWhenEmpty → 加一个 Spacer(1)
否则若有 leadingSpacer → 先加 Spacer(1)，再按插入顺序加所有组件
```

`MAX_WIDGET_LINES = 10`（静态常量，注释：“限制总 widget 行数以防视口溢出”）。

**`setExtensionFooter(factory)`**：dispose 旧自定义 footer；清空 `footerContainer`；有 factory 则
`factory(this.ui, theme, this.footerDataProvider)` 并加入；否则恢复内置 `footer`；`requestRender()`。

**`setExtensionHeader(factory)`**：若 `builtInHeader` 尚未初始化则**直接返回**（早期初始化调用被忽略）；
dispose 旧自定义 header；找到当前 header 在 `headerContainer.children` 的 index；有 factory 则创建并替换/前插
（index === -1 时 `unshift`），`isExpandable` 时同步 `setExpanded(toolOutputExpanded)`；否则恢复内置。

**`setExtensionStatus(key, text)`**：`footerDataProvider.setExtensionStatus(key, text)` + `requestRender()`。

**`addExtensionTerminalInputListener(handler)`**：`{ handler, unsubscribe: ui.addInputListener(handler) }` 存入
`extensionTerminalInputSubscriptions`；返回的退订函数同时删除集合项。

**`setWorkingIndicator(options?)`** / **`setWorkingVisible(visible)`** / **`setHiddenThinkingLabel(label?)`**：
见源码；关键用户可见值：`hiddenThinkingLabel = label ?? defaultHiddenThinkingLabel`；
`setWorkingVisible(false)` 清 working 指示器并重绘；`setWorkingIndicator` 更新当前指示器的帧。

**`setCustomEditorComponent(factory | undefined)`**（鸭子类型兼容）

```
editorComponentFactory = factory
currentText = this.editor.getText()
disposeActiveSelector(); editorContainer.clear()
if (factory) {
  newEditor = factory(this.ui, getEditorTheme(), this.keybindings)
  newEditor.onSubmit = defaultEditor.onSubmit; newEditor.onChange = defaultEditor.onChange
  newEditor.setText(currentText)
  if (newEditor.borderColor !== undefined) newEditor.borderColor = defaultEditor.borderColor
  if (newEditor.setPaddingX !== undefined) newEditor.setPaddingX(defaultEditor.getPaddingX())
  if (newEditor.setAutocompleteMaxVisible !== undefined) newEditor.setAutocompleteMaxVisible(defaultEditor.getAutocompleteMaxVisible())
  if (newEditor.setAutocompleteProvider && this.autocompleteProvider) newEditor.setAutocompleteProvider(this.autocompleteProvider)
  // 若传入对象有 actionHandlers（Map）——视为 CustomEditor 子类（跨 jiti 模块边界不能用 instanceof）：
  if ("actionHandlers" in newEditor && newEditor.actionHandlers instanceof Map) {
     if (!newEditor.onEscape) newEditor.onEscape = () => defaultEditor.onEscape?.()
     if (!newEditor.onCtrlD) newEditor.onCtrlD = () => defaultEditor.onCtrlD?.()
     if (!newEditor.onPasteImage) newEditor.onPasteImage = () => defaultEditor.onPasteImage?.()
     if (!newEditor.onExtensionShortcut) newEditor.onExtensionShortcut = d => defaultEditor.onExtensionShortcut?.(d)
     for ([action, handler] of defaultEditor.actionHandlers) newEditor.actionHandlers.set(action, handler)
  }
  this.editor = newEditor
} else {
  defaultEditor.setText(currentText); this.editor = defaultEditor
}
editorContainer.addChild(this.editor)
若 activeStatusIndicator：重挂到编辑器（embedded）或 statusContainer
ui.setFocus(this.editor); ui.requestRender()
```

**`resetExtensionUI()`**（reload / 会话替换时）：dispose 全部对话框并清空；`ui.hideOverlay()`；清终端输入监听；
`setExtensionFooter(undefined)`；`setExtensionHeader(undefined)`；清 widget；清 extension statuses；
footer 失效；清 autocomplete wrappers；`setCustomEditorComponent(undefined)`；重建 autocomplete；
`defaultEditor.onExtensionShortcut = undefined`；更新终端标题；`workingMessage = undefined`；
`workingVisible = true`；`setWorkingIndicator()`；若工作指示器存在，重置消息为
`${defaultWorkingMessage} (${keyText("app.interrupt")} to interrupt)`；`setHiddenThinkingLabel()`。

**`createProjectTrustContext(cwd)`**

```ts
{ cwd, mode: "tui", hasUI: true, ui: { select: ui.select, confirm: ui.confirm, input: ui.input, notify: ui.notify } }
```

**组件逐字布局（`tui-components` 单元的组件，但布局属扩展可见表面）**

`ExtensionSelectorComponent`：

```
DynamicBorder()
Spacer(1)
Text(theme.fg("accent", theme.bold(title)), 1, 0)      // titleText
[若有 description] Spacer(1) + Text(theme.fg("text", description), 1, 0)
Spacer(1)
[listContainer]
Spacer(1)
Text(rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm","select") + "  " + keyHint("tui.select.cancel","cancel"), 1, 0)
Spacer(1)
DynamicBorder()
```

列表项：选中 `theme.fg("accent","→ ") + theme.fg("accent", option)`；未选中 `"  " + theme.fg("text", option)`
（两空格前缀）。键：`app.tools.expand` → onToggleToolsExpanded；`tui.select.up` 或 `"k"` → 上移（clamp 到 0）；
`tui.select.down` 或 `"j"` → 下移（clamp 到末项）；`tui.select.confirm` 或 `"\n"` → 选中当前项（**若该项为空字符串
则不回调**）；`tui.select.cancel` → 取消。倒计时标题：`theme.fg("accent", theme.bold(`${baseTitle} (${s}s)`))`。

`ExtensionInputComponent`：

```
DynamicBorder()
Spacer(1)
Text(theme.fg("accent", title), 1, 0)
[若有 description] Spacer(1) + Text(theme.fg("text", description), 1, 0)
Spacer(1)
<Input>（若有 initialValue 则 setValue）
Spacer(1)
Text(`${keyHint("tui.select.confirm","submit")}  ${keyHint("tui.select.cancel","cancel")}`, 1, 0)
Spacer(1)
DynamicBorder()
```

倒计时标题（注意与 selector 不同，**无 bold**）：`theme.fg("accent", `${baseTitle} (${s}s)`)`。
键：confirm 或 `"\n"` → 提交输入值；cancel → 取消；其它转发 `input.handleInput`。`focused` setter 传播给 input。

`ExtensionEditorComponent`：

```
DynamicBorder()
Spacer(1)
Text(theme.fg("accent", title), 1, 0)
[若有 description] Spacer(1) + Text(theme.fg("text", description), 1, 0)
Spacer(1)
<Editor(tui, getEditorTheme(), editorOptions)>（prefill 则 setText；onSubmit → 提交）
Spacer(1)
Text(keyHint("tui.select.confirm","submit") + "  " + keyHint("tui.input.newLine","newline") + "  " +
     keyHint("tui.select.cancel","cancel") + `  ${keyHint("app.editor.external","external editor")}`, 1, 0)
Spacer(1)
DynamicBorder()
```

外部编辑器命令：`externalEditorCommand || process.env.VISUAL || process.env.EDITOR || (win32 ? "notepad" : "nano")`。
按 `app.editor.external` 时 `tui.stop()` → `editInExternalEditor({command, content})`；`status === "complete"` 时
`editor.setText(result.content)`；`finally { tui.start(); tui.requestRender(true) }`。

`CountdownTimer`（可复用）：

```
remainingSeconds = Math.ceil(timeoutMs / 1000)
onTick(remainingSeconds)                    // 立即先 tick 一次
intervalId = setInterval(() => {
  remainingSeconds--
  onTick(remainingSeconds)
  tui?.requestRender()
  if (remainingSeconds <= 0) { dispose(); onExpire() }
}, 1000)
```

只在 `opts?.timeout && opts.timeout > 0 && opts.tui` 时创建。`dispose()` 清 interval（幂等）。

**`keybinding-hints.ts` 格式（逐字）**

```
formatKeyText(key, {capitalize}): key.split("/").map(k => k.split("+").map(part =>
    capitalize ? part[0].toUpperCase()+part.slice(1) : part).join("+")).join("/")
    // 且 darwin 上 part.toLowerCase() === "alt" → "option"
keyText(keybinding)         = formatKeys(getKeybindings().getKeys(keybinding))
keyDisplayText(keybinding)  = formatKeys(..., { capitalize: true })
keyHint(keybinding, desc)   = theme.fg("dim", keyText(kb)) + theme.fg("muted", ` ${desc}`)
rawKeyHint(key, desc)       = theme.fg("dim", formatKeyText(key)) + theme.fg("muted", ` ${desc}`)
```

#### 2.8.2 RPC 模式（`src/modes/rpc/rpc-mode.ts`）

`createDialogPromise(opts, defaultValue, request, parseResponse)`：

```
if (opts?.signal?.aborted) return Promise.resolve(defaultValue)
id = crypto.randomUUID()
new Promise((resolve, reject) => {
  timeoutId
  cleanup() { if (timeoutId) clearTimeout(timeoutId); opts?.signal?.removeEventListener("abort", onAbort);
              pendingExtensionRequests.delete(id) }
  onAbort() { cleanup(); resolve(defaultValue) }
  opts?.signal?.addEventListener("abort", onAbort, { once: true })
  if (opts?.timeout) timeoutId = setTimeout(() => { cleanup(); resolve(defaultValue) }, opts.timeout)
  pendingExtensionRequests.set(id, { resolve: r => { cleanup(); resolve(parseResponse(r)) }, reject })
  output({ type: "extension_ui_request", id, ...request })
})
```

四个对话框的 `request` 字段与解析：

| 方法 | request | 默认值 | 解析 |
|---|---|---|---|
| `select` | `{ method:"select", title, options, timeout: opts?.timeout }` | `undefined` | `cancelled ? undefined : ("value" in r ? r.value : undefined)` |
| `confirm` | `{ method:"confirm", title, message, timeout: opts?.timeout }` | `false` | `cancelled ? false : ("confirmed" in r ? r.confirmed : false)` |
| `input` | `{ method:"input", title, placeholder, timeout: opts?.timeout }` | `undefined` | 同 select |
| `editor` | 手写 Promise：`{ type:"extension_ui_request", id, method:"editor", title, prefill }` | 无 signal/timeout | `cancelled → undefined`；`"value" in r → r.value`；否则 `undefined` |

注意：`select`/`confirm`/`input` 即使 `opts.timeout` 为 undefined，请求对象里也带 `timeout: undefined`
（JSON 序列化时消失）。`editor` **不**支持 `ExtensionUIDialogOptions`（签名只接受 title/prefill）。

其它方法（逐字）：

| 方法 | 请求 |
|---|---|
| `notify(message, type)` | `{ type:"extension_ui_request", id: randomUUID(), method:"notify", message, notifyType: type }` |
| `setStatus(key, text)` | `{ ..., method:"setStatus", statusKey: key, statusText: text }` |
| `setWidget(key, content, options)` | 仅当 `content === undefined \|\| Array.isArray(content)`：`{ ..., method:"setWidget", widgetKey: key, widgetLines: content, widgetPlacement: options?.placement }`；工厂函数被**忽略**（不发请求） |
| `setTitle(title)` | `{ ..., method:"setTitle", title }` |
| `setEditorText(text)` | `{ ..., method:"set_editor_text", text }` |
| `pasteToEditor(text)` | 委托 `this.setEditorText(text)`（无终端粘贴处理） |

无操作/降级（逐字）：`onTerminalInput()` 返回 `() => {}`；`setWorkingMessage`、`setWorkingVisible`、
`setWorkingIndicator`、`setHiddenThinkingLabel`、`setFooter`、`setHeader`、`addAutocompleteProvider`、
`setEditorComponent`、`setToolsExpanded` 全为 no-op；`getEditorText()` → `""`；`getEditorComponent()` →
`undefined`；`getToolsExpanded()` → `false`；`custom()` → `undefined as never`；`getAllThemes()` → `[]`；
`getTheme()` → `undefined`；`theme` getter → 全局 `theme`；`setTheme()` →
`{ success: false, error: "Theme switching not supported in RPC mode" }`（【逐字】）。

RPC 模式 `bindExtensions`：`uiContext: createExtensionUIContext()`、`mode: "rpc"`、
`commandContextActions`（waitForIdle / newSession / fork / navigateTree / switchSession / reload）、
`shutdownHandler: () => shutdownRequested = true`、
`onError: err => output({ type:"extension_error", extensionPath, event, error })`（**不含 stack**）。

`pendingExtensionRequests` 的响应分发：stdin 收到 `extension_ui_response` 时按 `id` 查找并调用
`resolve`（找不到则忽略；见 `5868-rpc-unknown-command-id` 的相关行为）。

#### 2.8.3 print / JSON 模式（`src/modes/print-mode.ts`）

```
await session.bindExtensions({
  mode: mode === "json" ? "json" : "print",
  commandContextActions: { waitForIdle, newSession, fork, navigateTree, switchSession, reload },
  onError: err => console.error(`Extension error (${err.extensionPath}): ${err.error}`),   // 【逐字】
})
```

- **不提供 `uiContext`** → runner 保持 `noOpUIContext`，`hasUI === false`。
- JSON 与 print 的区别仅在于事件如何写到 stdout（JSON 每行一个事件）；扩展侧无差别（`ctx.mode` 不同）。
- 【逐字】`Extension error (<extensionPath>): <error>`。

#### 2.8.4 错误在上层的呈现

| 位置 | 文案 |
|---|---|
| 交互式聊天区 | `Extension "<path>" error: <error>`（error 色，缩进 1/0；stack 后续行 dim）【逐字】 |
| print/json stderr | `Extension error (<path>): <error>`【逐字】 |
| RPC stdout | `{"type":"extension_error","extensionPath":...,"event":...,"error":...}`【逐字键名】 |
| 启动诊断（`main.ts`） | `Failed to load extension "<path>": <error>`（type "error"）【逐字】 |
| 启动诊断（警告） | `Extension package "<path>": <warning>`（type "warning"）【逐字】 |
| 交互式 `[Extension issues]` 段 | error 项：`  <formattedPath>` + `    <message>`；warning 项同格式但 warning 色；无 path 时只 `  <message>` |
| 崩溃提示（`formatCrashExtensionHint`） | `A stack frame came from loaded <extension(s)> \`<path>\`[, and \`<path2>\`], which may be involved. Try disabling <it|them> with \`kiana config\`, or run \`kiana -ne\` to confirm.`【逐字】 |

`formatCrashExtensionHint` 细节：matches 过滤掉非字符串与空串；`quoted = matches.map(m => `\`${m}\``)`；
标题连接：1 个 → 该项；2 个 → `A and B`；≥3 → `A, B, and C`；名词 `extension`/`extensions`；
代词 `it`/`them`（单复数一致）。

### 2.9 `ResourceLoader` 中的扩展装配契约

（`ca-resources` 拥有实现，但扩展语义在此，必须一致。）

**内联工厂命名**（`loadExtensionFactories`）：

```
for ([index, input] of this.extensionFactories.entries()) {
  isNamed = typeof input !== "function"
  factory = isNamed ? input.factory : input
  extensionPath = `<inline:${isNamed ? input.name : index + 1}>`        // 【逐字】1-based
  try {
    extension = await loadExtensionFromFactory(factory, this.cwd, this.eventBus, runtime, extensionPath)
    extension.hidden = isNamed && input.hidden
    extension.replaceable = isNamed && input.replaceable === true
    extensions.push(extension)
  } catch (error) {
    errors.push({ path: extensionPath, error: error instanceof Error ? error.message : "failed to load extension" })  // 【逐字】兜底
  }
}
```

**`builtin:<name>` 路径**（`loadExtensionPaths`）：

```
磁盘路径交给 loadExtensionsCached(paths.filter(!isBuiltinPath), cwd, eventBus, runtime)
for path of paths.filter(isBuiltinPath):
  builtin = this.builtinExtensions.get(path.slice("builtin:".length))
  if (!builtin) { result.errors.push({ path, error: `Unknown built-in extension: ${path}` }); continue }   // 【逐字】
  try {
    extension = await loadExtensionFromFactory(builtin.factory, this.cwd, this.eventBus, result.runtime, path)
    extension.hidden = true
    extension.replaceable = builtin.replaceable === true
    result.extensions.push(extension)
  } catch (error) {
    result.errors.push({ path, error: error instanceof Error ? error.message : "failed to load extension" })
  }
```

注意 `builtin:` 扩展的 `path === resolvedPath === "builtin:<name>"`，`hidden = true`，`sourceInfo.source === "builtin"`，
`sourceInfo.baseDir === undefined`。

**`replaceable` 省略规则**（`omitReplacedExtensions`）：

```
names(ext) = [...tools.keys().map(n=>`tool:${n}`), ...commands.keys().map(n=>`command:${n}`), ...flags.keys().map(n=>`flag:${n}`)]
taken = new Map(非 replaceable 扩展的 names → 该扩展)
return extensions.filter(ext => {
  if (!ext.replaceable) return true
  replacement = names(ext).map(name => ({name, extension: taken.get(name)})).find(x => x.extension !== undefined)
  if (!replacement) return true
  if (ext.path.startsWith("builtin:")) {
    builtinName = path.slice("builtin:".length)
    [kind, rawName] = replacement.name.split(":", 2)
    registeredName = kind === "command" ? `/${rawName}` : kind === "flag" ? `--${rawName}` : rawName
    warnings.push({ path: ext.path, warning:
      `Extension ${replacement.extension.path} registers ${kind} \`${registeredName}\`, so built-in extension \`${builtinName}\` was not loaded. To use \`${builtinName}\`, run \`kiana config\` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time.` })   // 【逐字】
  }
  return false
})
```

**冲突诊断**（`detectExtensionConflicts`，警告级 error 条目）：

```
toolOwners = Map<string,string>; flagOwners = Map<string,string>
for ext of extensions:
  for toolName of ext.tools.keys():
    owner = toolOwners.get(toolName)
    if (owner && owner !== ext.path) conflicts.push({ path: ext.path, message: `Tool "${toolName}" conflicts with ${owner}` })
    else toolOwners.set(toolName, ext.path)
  for flagName of ext.flags.keys():
    owner = flagOwners.get(flagName)
    if (owner && owner !== ext.path) conflicts.push({ path: ext.path, message: `Flag "--${flagName}" conflicts with ${owner}` })
    else flagOwners.set(flagName, ext.path)
```

【逐字】`Tool "<name>" conflicts with <ownerPath>`、`Flag "--<name>" conflicts with <ownerPath>`。
（命令冲突**不**在此检测——由 `getBuiltInCommandConflictDiagnostics` 与 `:n` 后缀处理。见 §3.7。）

**host-provided 包警告**（`collectExtensionPackageWarnings`）：

```
HOST_PROVIDED_EXTENSION_PACKAGES = {
  "@earendil-works/kiana-agent-core","@earendil-works/kiana-ai","@earendil-works/kiana-coding-agent",
  "@earendil-works/kiana-tui","@mariozechner/kiana-agent-core","@mariozechner/kiana-ai",
  "@mariozechner/kiana-coding-agent","@mariozechner/kiana-tui","@sinclair/typebox","typebox" }
对每个扩展所属 packageRoot 的 package.json：
  hostDependencies = 其 dependencies 中属于上表的名字，排序
  若非空 → warning: `Host-provided extension packages must be declared in peerDependencies with a "*" range, not dependencies: ${hostDependencies.join(", ")}. Installed copies can bypass the extension loader and create duplicate runtime modules.`   // 【逐字】
  path 为 package.json 路径
```

**`mergeExtensionWarnings(result, warnings)`**：按 `path` 去重合并（后者覆盖前者），保序（`Map` 插入序）。

**扩展路径存在性**：`additionalExtensionPaths` 中 `isLocalPath(p)` 且不存在 →
`{ path: resolved, error: "Extension path does not exist: <resolved>" }`（【逐字】）。

**配置禁用**：`noExtensions` 时跳过；`disabledBuiltinExtensions` 里的名字对应 `builtin:<name>` 不加载
（`--no-mcp` → `["mcp"]`）；`extensions` 设置中的 `-builtin:<name>` 同理。

**`applyExtensionFlagValues(resourceLoader, extensionFlagValues)`**（`ca-session` 拥有，语义如下）：

```
if (!extensionFlagValues) return []
registeredFlags = Map<name, {type}>（遍历全部扩展 flags，后者覆盖前者）
unknownFlags = []
for ([name, value] of extensionFlagValues):
  flag = registeredFlags.get(name)
  if (!flag) { unknownFlags.push(name); continue }
  if (flag.type === "boolean") { runtime.flagValues.set(name, true); continue }
  if (typeof value === "string") { runtime.flagValues.set(name, value); continue }
  diagnostics.push({ type: "error", message: `Extension flag "--${name}" requires a value` })      // 【逐字】
if (unknownFlags.length) diagnostics.push({ type: "error",
  message: `Unknown option${unknownFlags.length === 1 ? "" : "s"}: ${unknownFlags.map(n=>`--${n}`).join(", ")}` })  // 【逐字】
```

**扩展对资源路径的贡献**（`AgentSession.buildExtensionResourcePaths`）：

```
source = isSyntheticPath(extensionPath) ? `extension:${extensionPath.replace(/[<>]/g,"")}`
                                        : `extension:${basename(extensionPath).replace(/\.(ts|js)$/,"")}`
baseDir = isSyntheticPath(extensionPath) ? undefined : dirname(extensionPath)
metadata = { source, scope: "temporary", origin: "top-level", baseDir }
```

### 2.10 依赖本单元的外部设施

| 设施 | 文件 | 本单元用到的部分 |
|---|---|---|
| `SourceInfo` | `src/core/source-info.ts` | `BUILTIN_PATH_PREFIX = "builtin:"`；`getSyntheticPathSource`、`isSyntheticPath`、`createSyntheticSourceInfo`（§4.6） |
| `KianaManifest` | `src/core/kiana-manifest.ts` | `readKianaManifest(packageJsonPath): KianaManifest \| null`（§4.2） |
| `EventBus` | `src/core/event-bus.ts` | `createEventBus()`；`emit(channel, data)`；`on(channel, handler) => unsubscribe`；handler 被包成 async，抛错 `console.error(\`Event handler error (${channel}):\`, err)`【逐字】 |
| `timings` | `src/core/timings.ts` | `KIANA_TIMING=1` 启用；`time(`${extensionPath} factory`, "extensions")`、`time(`${extensionPath} module import`, "extensions")` |
| `execCommand` | `src/core/exec.ts` | `spawn(shell:false, stdio:["ignore","pipe","pipe"])`，`StringDecoder("utf8")` 跨块解码（多字节 UTF-8 不劈开），返回 `{stdout, stderr, code, killed}`；`timeout` 用 `SIGTERM`；`signal` 中止 |
| `ResourceDiagnostic` | `src/core/diagnostics.ts` | `{ type: "warning" \| "error" \| "collision"; message; path?; collision?: ResourceCollision }` |
| `KeybindingsManager` | `src/core/keybindings.ts` | `APP_KEYBINDINGS`（`defaultKeys` + `description`）；`getEffectiveConfig()`；`matches(data, keybinding)` |
| `NESTED_CALL_LIMITS` | `src/core/nested-tool-calls.ts` | `maxCalls: 256`、`maxArgumentBytesPerCall: 8192`、`maxArgumentBytesTotal: 32768`、`maxErrorChars: 500`（ca-session 拥有，但契约来自本单元文档） |
| `findExtensionStackMatches` | `src/core/crash-log.ts` | 栈帧 → 扩展标签（用于崩溃提示） |
| `getAvailableThemesWithPaths` / `getThemeByName` | `theme.ts` / `theme-controller.ts` | `getAllThemes` / `getTheme` / `setTheme` |

**`APP_KEYBINDINGS` 与本单元相关的默认键**（用于保留冲突判定与文档）：
`app.interrupt`=`escape`、`app.clear`=`ctrl+c`、`app.exit`=`ctrl+d`、`app.suspend`=`ctrl+z`（win 空）、
`app.thinking.cycle`=`shift+tab`、`app.thinking.save`=`ctrl+s`、`app.model.cycleForward`=`ctrl+p`、
`app.model.cycleBackward`=`shift+ctrl+p`（win `alt+p`）、`app.model.select`=`ctrl+l`、`app.tools.expand`=`ctrl+o`、
`app.thinking.toggle`=`ctrl+t`、`app.session.toggleNamedFilter`=`ctrl+n`、`app.editor.external`=`ctrl+g`、
`app.message.copy`=`ctrl+x`、`app.message.followUp`=`alt+enter`（win `ctrl+q`）、
`app.message.dequeue`=`alt+up`（win `alt+q`）、`app.clipboard.pasteImage`=`ctrl+v`（win `alt+v`）、
`app.session.new/tree/fork/resume` 默认空、`app.tree.foldOrUp`=`["ctrl+left","alt+left"]`（darwin 反序）、
`app.tree.unfoldOrDown` 同理、`app.tree.editLabel`=`shift+l`、`app.tree.toggleLabelTimestamp`=`shift+t`、
`app.session.togglePath`=`ctrl+p`、`app.session.toggleSort`=`ctrl+s`、`app.session.rename`=`ctrl+r`、
`app.session.delete`=`ctrl+d`、`app.session.deleteNoninvasive`=`ctrl+backspace`、`app.models.save`=`ctrl+s`。
TUI 层键：`tui.input.submit`=`enter`、`tui.input.newLine`=…、`tui.input.copy`、`tui.select.up/down/confirm/cancel`、
`tui.editor.deleteToLineEnd`（详见 `tui-core` spec）。

---

## 3. 用户可见表面（逐字）

本节汇总所有由本单元决定、用户能看到的字符串。除特别说明外均为【逐字】。颜色名指主题 token。

### 3.1 CLI flags（与扩展有关）

来自 `printHelp`（`ca-cli` 拥有输出，但本单元负责扩展 flag 的追加格式）：

```
  --extension, -e <path>         Load an extension file or builtin:<name> (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery and built-in extensions (explicit -e paths still work)
  --no-mcp                       Disable built-in MCP support: no servers connect and no MCP tools
```

帮助正文中的一行（说明扩展可注册额外 flag）：

```
Extensions can register additional flags (e.g., --plan from plan-mode extension).
```

**扩展 flag 段落格式**（`extensionFlagsText`，当 `extensionFlags.length > 0`）：

```
\n<bold>Extension CLI Flags:</bold>\n
<每行: ("  --" + name + (type === "string" ? " <value>" : "")).padEnd(30) + (description ?? "Registered by " + extensionPath)>
\n
```

- `padEnd(30)` 以**字符数**（UTF-16 code unit）计，不足补空格；
- 描述缺省为 `Registered by <extensionPath>`；
- 行间 `\n`，整体前后各一个 `\n`（前面再接 `Extensions can register...` 那一段）。
- 默认 flag 描述示例：`Registered by /home/u/.kiana/agent/extensions/ssh.ts`。

### 3.2 命令与 invocationName

- 扩展命令在 `/` 后的名字是 `ResolvedCommand.invocationName`。
- 与内建命令冲突时（`getBuiltInCommandConflictDiagnostics`）：
  - 无重名扩展（`invocationName === name`）：
    `Extension command '/<name>' conflicts with built-in interactive command. Skipping in autocomplete.`
  - 有重名（带 `:n`）：
    `Extension command '/<name>' conflicts with built-in interactive command. Available as '/<invocationName>'.`
  - 两者都是 `type: "warning"`，`path: command.sourceInfo.path`。
- 内建命令名集合（24 个，`BUILTIN_SLASH_COMMANDS`）：`settings`、`model`、`tree`、`thinking`、`scoped-models`、
  `export`、`import`、`share`、`bug`、`copy`、`name`、`session`、`changelog`、`hotkeys`、`fork`、`clone`、`trust`、
  `login`、`logout`、`new`、`compact`、`resume`、`reload`、`quit`。
  逐字描述：`settings`="Open settings menu"、`model`="Select model (opens selector UI)"（`argumentHint: "<provider/model>"`）、
  `tree`="Navigate session tree (switch branches)"、`thinking`="Set thinking level"（hint `<level>`）、
  `scoped-models`="Enable/disable models for Ctrl+P cycling"、`export`="Export session (HTML default, or specify path: .html/.jsonl)"、
  `import`="Import and resume a session from a JSONL file"、`share`="Share session via a Radius artifact or a GitHub gist"、
  `bug`="Report a bug to the Kiana developers"（hint `<description>`）、`copy`="Copy last agent message to clipboard"、
  `name`="Set session display name"、`session`="Show session info and stats"、`changelog`="Show changelog entries"、
  `hotkeys`="Show all keyboard shortcuts"、`fork`="Create a new fork from a previous user message"、
  `clone`="Duplicate the current session at the current position"、`trust`="Save project trust decision for future sessions"、
  `login`="Configure provider authentication"（hint `<provider>`）、`logout`="Remove provider authentication"、
  `new`="Start a new session"、`compact`="Manually compact the session context"、`resume`="Resume a different session"、
  `reload`="Reload keybindings, extensions, skills, prompts, themes, and context files"、`quit`="Quit ${APP_NAME}"（即 `Quit kiana`）。
  **注意**：冲突判定用的是 `command.name`（`getBuiltInCommandConflictDiagnostics` 在 interactive-mode 中），
  因此 `quit` 也算保留名。

### 3.3 快捷键冲突（三条，见 §2.3.9）

```
Extension shortcut '<key>' from <path> conflicts with built-in shortcut. Skipping.
Extension shortcut conflict: '<key>' is built-in shortcut for <keybinding> and <path>. Using <path>.
Extension shortcut conflict: '<key>' registered by both <existingPath> and <path>. Using <path>.
```

无 UI 时同时 `console.warn(message)`（走 stderr）。

### 3.4 注册期错误（扩展加载失败原因，`LoadExtensionsResult.errors[].error`）

```
Extension does not export a valid factory function: <extensionPath>
Failed to load extension: <message>
```

诊断层再加前缀（`main.ts`）：

```
Failed to load extension "<path>": <error>
Extension package "<path>": <warning>
```

`ResourceLoader` 补充：

```
Unknown built-in extension: <path>
Extension path does not exist: <resolved>
Tool "<name>" conflicts with <ownerPath>
Flag "--<name>" conflicts with <ownerPath>
```

`InlineExtension` 加载失败的兜底 message 是 `failed to load extension`（小写 f，无句号）。

内联扩展路径命名：`<inline:N>`（N 从 1 开始，bare 工厂）或 `<inline:name>`（命名包装）。启动 Extensions 列表
与错误中显示。`builtin:` 扩展在错误/诊断中显示为 `builtin:name`，且不出现在启动列表（`hidden`）。

### 3.5 注册 API 抛出的错误（扩展作者能看到的 TypeError 等价文本）

```
Extension runtime not initialized. Action methods cannot be called during extension loading.
Extension runtime not initialized                       (setModel 的 reject)
Extension "<path>" failed to load and its API is no longer active.
Tool "<name>" registered by extension "<path>" must define an object parameter schema.
Command registered by extension "<path>" must have a non-empty string name. Use kiana.registerCommand("name", { description, handler }).
Command "/<name>" registered by extension "<path>" must define handler().
Invalid default for flag "<name>": expected <boolean|string>, got <typeof default>
Provider config is required when registering by name
Invalid MCP server registered by extension "<path>": <validated>
MCP server "<name>" is already registered by extension "<owner>"
MCP server "<name>" conflicts with registered server "<clash>"
```

stale 上下文（默认长文案，见 §2.2.3）：

```
This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().
```

### 3.6 派发期错误（`ExtensionError`，进入界面/日志）

| 事件 | 文本 |
|---|---|
| `message_end` | `message_end handlers must return a message with the same role` |
| `user_bash` | `Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object` |
| `context_with_system` | `Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage().` |
| `turn_end` / `agent_before_settle` | `Invalid boundary entries: <message>` |
| `register_mcp_server` | `MCP server "<name>" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support` |
| 任意 | handler 抛出的 `error.message` 原样（无包装） |

### 3.7 启动期资源列表（交互式）

- 段落标题：`[Extensions]`（`theme.fg("mdHeading", "[Extensions]")`），空行分隔。
- 折叠内容：`theme.fg("dim", "  " + labels.sort(localeCompare).join(", "))`（按 locale 排序、去空、trim）。
- 展开内容：按 scope 分组（`tui-core` 的 `formatScopeGroups`），扩展路径经过
  `formatExtensionDisplayPath`（去掉结尾 `/index.ts` 或 `/index.js` 后再走 `formatDisplayPath`）。
- 紧凑标签 `getCompactExtensionLabels`：
  - 包来源扩展用 `getCompactExtensionLabel`；
  - 非包扩展：路径段若多于一段且最后一段是 `index.ts`/`index.js` 则去掉；
  - 单文件与重名消歧逻辑见 `getCompactNonPackageExtensionLabel`（`tui-core` 拥有，但扩展路径格式化属本单元
    可见表面）；对 `<inline:*>` / `builtin:*` 合成路径应有稳定显示。
- 诊断段标题：`[Extension issues]`（`theme.fg("warning", ...)`），先列 errors 再列 warnings，然后命令诊断
  （含内建冲突），最后快捷键诊断。
- 其它段标题：`[Skill conflicts]`、`[Prompt conflicts]`、`[Theme conflicts]`（不属于本单元，但相邻显示）。

### 3.8 扩展错误的三种落点（逐字，见 §2.8.4）

```
Extension "<path>" error: <error>              （交互式聊天区，error 色）
Extension error (<path>): <error>               （print/json stderr）
{"type":"extension_error","extensionPath":"<path>","event":"<event>","error":"<error>"}   （RPC stdout）
```

### 3.9 对话框与 widget 的用户可见文本

确认框：标题 `"<title>\n<message>"`，选项 `["Yes", "No"]`。
选择器底部提示（由 `keyHint` 组合，实际键名来自 keybindings）：

```
↑↓ navigate  <keyText("tui.select.confirm")> select  <keyText("tui.select.cancel")> cancel
```

输入框底部提示：`<keyText("tui.select.confirm")> submit  <keyText("tui.select.cancel")> cancel`

编辑器底部提示：
`<keyText("tui.select.confirm")> submit  <keyText("tui.input.newLine")> newline  <keyText("tui.select.cancel")> cancel  <keyText("app.editor.external")> external editor`

倒计时标题：selector 为 `"<title> (Ns)"`（accent + bold），input/editor 为 `"<title> (Ns)"`（accent，无 bold）。
`s = Math.ceil(timeoutMs / 1000)`，每秒递减，到 0 触发取消。

Widget 截断行：`... (widget truncated)`（muted 色，左内缩 1）。Widget 最多 10 行（不含截断行）。

选择器选中前缀：`→ `（accent），未选中前缀为两个空格。列表项左内缩 1。

### 3.10 扩展作者看到的 API 文档要点（会进 `kiana --help` 与 docs）

- `--no-tools` 同时禁用内建与扩展工具；`--no-builtin-tools` 只禁内建，保留扩展/自定义工具；`--tools` 支持
  `+name`/`-name` 与通配；`--exclude-tools` 对包括 MCP 在内的所有工具生效。
- 扩展 flag 通过 CLI 未知 flag 收集（`unknownFlags`）：`--name=value`、`--name value`（下一个不以 `-`/`@` 开头）、
  否则 `true`。这由 `ca-cli` 解析、由 `applyExtensionFlagValues` 落到本单元 flag 表。

### 3.11 `examples/extensions/**` 兼容语料清单（70 个 .ts 单文件 + 9 个目录 + README = 80 个条目，必须逐一可加载）

单文件（70）：`auto-commit-on-exit.ts`、`bash-spawn-hook.ts`、`bookmark.ts`、`border-status-editor.ts`、
`built-in-tool-renderer.ts`、`claude-rules.ts`、`commands.ts`、`confirm-destructive.ts`、`custom-compaction.ts`、
`custom-footer.ts`、`custom-header.ts`、`debug-provider.ts`、`dirty-repo-guard.ts`、`dynamic-tools.ts`、
`entry-renderer.ts`、`event-bus.ts`、`file-trigger.ts`、`git-checkpoint.ts`、`git-merge-and-resolve.ts`、
`github-issue-autocomplete.ts`、`handoff.ts`、`hello.ts`、`hidden-thinking-label.ts`、`inline-bash.ts`、
`input-transform-streaming.ts`、`input-transform.ts`、`interactive-shell.ts`、`jev-router.ts`、`mac-system-theme.ts`、
`message-renderer.ts`、`minimal-mode.ts`、`modal-editor.ts`、`model-status.ts`、`notify.ts`、`overlay-qa-tests.ts`、
`overlay-test.ts`、`permission-gate.ts`、`pirate.ts`、`preset.ts`、`project-trust.ts`、`prompt-customizer.ts`、
`protected-paths.ts`、`provider-payload.ts`、`qna.ts`、`question.ts`、`questionnaire.ts`、`rainbow-editor.ts`、
`reload-runtime.ts`、`rpc-demo.ts`、`send-user-message.ts`、`session-name.ts`、`shutdown-command.ts`、`snake.ts`、
`space-invaders.ts`、`ssh.ts`、`status-line.ts`、`structured-output.ts`、`summarize.ts`、`system-prompt-header.ts`、
`tic-tac-toe.ts`、`timed-confirm.ts`、`titlebar-spinner.ts`、`todo.ts`、`tool-override.ts`、`tools.ts`、
`trigger-compact.ts`、`truncated-tool.ts`、`widget-placement.ts`、`working-indicator.ts`、`working-message-test.ts`。
（合计 70 个 .ts 单文件；`custom-provider-gitlab-duo/test.ts` 属于目录条目，不单列。）

目录（9）：`custom-provider-anthropic/`、`custom-provider-gitlab-duo/`（含 `test.ts`）、`doom-overlay/`（5 文件 +
README）、`dynamic-resources/`、`gondolin/`（531 行 index）、`plan-mode/`（index+utils+README）、`sandbox/`（+package.json）、
`subagent/`（index 1036 行 + agents.ts + README）、`with-deps/`（+package.json + node_modules 依赖）。

**API 使用矩阵（示例 × 用到的 API，供实现后回归）**

| 示例 | `kiana.*` 使用 | `ctx.ui.*` / 组件使用 |
|---|---|---|
| `auto-commit-on-exit.ts` | `on`(session_shutdown), `exec` | `notify` |
| `bash-spawn-hook.ts` | `registerTool` | — |
| `bookmark.ts` | `registerCommand`, `setLabel` | `notify` |
| `border-status-editor.ts` | `on`, `exec`, `getThinkingLevel` | `setEditorComponent`, `setFooter`, `setWorkingVisible`, `theme` |
| `built-in-tool-renderer.ts` | `registerTool` | — |
| `claude-rules.ts` | `on`(context) | `notify` |
| `commands.ts` | `registerCommand`, `getCommands` | `confirm`, `notify`, `select` |
| `confirm-destructive.ts` | `on`(tool_call) | `confirm`, `notify`, `select` |
| `custom-compaction.ts` | `on`(session_before_compact) | `notify` |
| `custom-footer.ts` | `registerCommand` | `setFooter`, `setStatus`, `notify`, `requestRender` |
| `custom-header.ts` | `on`, `registerCommand` | `setHeader`, `notify` |
| `debug-provider.ts` | `on`(provider_stream_event 等), `registerCommand`, `registerEntryRenderer`, `appendEntry` | `notify`, `setStatus` |
| `dirty-repo-guard.ts` | `on`(tool_call), `exec` | `notify`, `select` |
| `dynamic-tools.ts` | `on`, `registerCommand`, `registerTool` | `notify` |
| `entry-renderer.ts` | `registerCommand`, `registerEntryRenderer`, `appendEntry` | — |
| `event-bus.ts` | `events`, `on`, `registerCommand` | `notify` |
| `file-trigger.ts` | `on`(message_*), `sendMessage` | `notify` |
| `git-checkpoint.ts` | `on`, `exec` | `notify`, `select` |
| `github-issue-autocomplete.ts` | `on`, `exec` | `addAutocompleteProvider`, `notify` |
| `git-merge-and-resolve.ts` | `on`, `exec`, `sendUserMessage` | `notify` |
| `handoff.ts` | `registerCommand` | `custom`, `editor`, `notify`, `setEditorText` |
| `hello.ts` | `registerTool` | — |
| `hidden-thinking-label.ts` | `on`, `registerCommand` | `setHiddenThinkingLabel`, `notify` |
| `inline-bash.ts` | `on`(tool_call), `exec` | `notify` |
| `input-transform.ts` / `-streaming.ts` | `on`(input) / `exec` | `notify` |
| `interactive-shell.ts` | `on`(tool_call) | `custom`, `requestRender`, `start`, `stop` |
| `jev-router.ts` | `registerVirtualModel` | — |
| `mac-system-theme.ts` | `on`(session_start) | `setTheme` |
| `message-renderer.ts` | `registerCommand`, `registerMessageRenderer`, `sendMessage` | — |
| `minimal-mode.ts` | `registerTool` | — |
| `modal-editor.ts` | `on` | `setEditorComponent` |
| `model-status.ts` | `on`(model_select) | `notify`, `setStatus` |
| `notify.ts` | `on` | `notify` |
| `overlay-qa-tests.ts` | `registerCommand` | `custom`(overlay 全部选项), `setEditorText`, `notify`, `showOverlay`, `requestRender` |
| `overlay-test.ts` | `registerCommand` | `custom`(overlay), `notify` |
| `permission-gate.ts` | `on`(tool_call) | `select` |
| `pirate.ts` | `on`, `registerCommand` | `notify` |
| `preset.ts` | `on`, `registerCommand`, `registerFlag`, `registerShortcut`, `getFlag`, `getActiveTools`, `getAllTools`, `setActiveTools`, `setModel`, `setThinkingLevel`, `getThinkingLevel`, `appendEntry` | `custom`, `notify`, `requestRender`, `setStatus`, `theme` |
| `project-trust.ts` | `on`(project_trust) | `input`, `notify`, `select` |
| `prompt-customizer.ts` | `on`(before_agent_start) | — |
| `protected-paths.ts` | `on`(tool_call) | `notify` |
| `provider-payload.ts` | `on`(before_provider_request/headers) | — |
| `qna.ts` | `registerCommand` | `custom`, `notify`, `setEditorText` |
| `question.ts` / `questionnaire.ts` | `registerTool` | `custom`, `requestRender` |
| `rainbow-editor.ts` | `on` | `setEditorComponent`, `requestRender` |
| `reload-runtime.ts` | `registerCommand`, `registerTool`, `sendUserMessage` | — |
| `rpc-demo.ts` | `on`, `registerCommand` | `confirm`, `editor`, `input`, `notify`, `select`, `setEditorText`, `setStatus`, `setTitle`, `setWidget` |
| `send-user-message.ts` | `registerCommand`, `sendMessage`, `sendUserMessage` | `notify` |
| `session-name.ts` | `registerCommand`, `setSessionName`, `getSessionName` | `notify` |
| `shutdown-command.ts` | `registerCommand`, `registerTool`, `exec` | — |
| `snake.ts` / `space-invaders.ts` / `tic-tac-toe.ts` | `registerCommand`, `registerTool`, `registerMessageRenderer`, `appendEntry`, `sendMessage`, `setSessionName`, `on` | `custom`, `notify`, `requestRender` |
| `ssh.ts` | `registerTool`, `registerFlag`, `getFlag`, `on` | `notify`, `setStatus`, `theme` |
| `status-line.ts` | `on` | `setStatus`, `theme` |
| `structured-output.ts` | `registerTool`(outputSchema/structuredContent) | — |
| `summarize.ts` | `registerCommand` | `custom`, `notify` |
| `system-prompt-header.ts` | `on` | `setStatus` |
| `timed-confirm.ts` | `registerCommand` | `confirm`, `select`, `notify` |
| `titlebar-spinner.ts` | `on`, `getSessionName` | `setTitle` |
| `todo.ts` | `on`, `registerCommand`, `registerTool` | `custom`, `notify` |
| `tool-override.ts` | `registerCommand`, `registerTool` | `notify` |
| `tools.ts` | `on`, `registerCommand`, `getActiveTools`, `getAllTools`, `setActiveTools`, `appendEntry` | `custom`, `notify`, `requestRender` |
| `trigger-compact.ts` | `on`, `registerCommand` | `notify` |
| `truncated-tool.ts` | `registerTool` | — |
| `widget-placement.ts` | `on` | `setWidget` |
| `working-indicator.ts` | `on`, `registerCommand` | `setWorkingIndicator`, `setStatus`, `notify`, `theme` |
| `working-message-test.ts` | `on` | `setWorkingIndicator`, `setWorkingMessage` |
| `custom-provider-anthropic/index.ts` | `registerProvider`（含 oauth + streamSimple） | — |
| `custom-provider-gitlab-duo/index.ts` | `registerProvider`（oauth、headers、authHeader、models） | — |
| `doom-overlay/index.ts` | `registerCommand` | `custom`(持续渲染 overlay), `notify` |
| `dynamic-resources/index.ts` | `on`(resources_discover) | — |
| `gondolin/index.ts` | `on`, `registerCommand`, `registerTool` | `notify`, `setStatus`, `theme` |
| `plan-mode/index.ts` | `on`, `registerCommand`, `registerFlag`, `registerShortcut`, `getFlag`, `getActiveTools`, `setActiveTools`, `sendMessage`, `sendUserMessage`, `appendEntry` | `editor`, `notify`, `select`, `setStatus`, `setWidget`, `theme` |
| `sandbox/index.ts` | `on`, `registerCommand`, `registerFlag`, `registerTool`, `getFlag` | `notify`, `setStatus`, `theme` |
| `subagent/index.ts` | `registerTool` | `confirm` |
| `with-deps/index.ts` | `registerTool`（依赖自身 node_modules） | — |

另：`examples/rpc-extension-ui.ts`（RPC UI 客户端）、`examples/rpc-client.ts`、`examples/sdk/06-extensions.ts`
（SDK inline 工厂），以及 `examples/extensions/README.md` 的示例索引必须保持一致。

---

## 4. 外部格式（逐字）

### 4.1 磁盘布局

| 路径 | 含义 |
|---|---|
| `<cwd>/.kiana/extensions/` | 项目本地扩展目录（`CONFIG_DIR_NAME = ".kiana"`） |
| `<agentDir>/extensions/` | 全局扩展目录（`agentDir` 默认 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖） |
| `<dir>/<name>.ts` / `.js` | 直接扩展文件 |
| `<dir>/<name>/index.ts` / `index.js` | 目录入口 |
| `<dir>/<name>/package.json` 的 `kiana.extensions` | 多入口清单 |
| `--extension <path>` / `-e <path>` | 显式路径（文件、目录、或 `builtin:<name>`） |
| `extensions` 设置项 | 包来源（`npm:`/`git:`/…）或本地路径；`-builtin:<name>` 禁用内置 |

发现顺序（`discoverAndLoadExtensions`）：项目目录 → 全局目录 → 配置路径；按 `path.resolve` 去重，保留首次出现。
目录配置路径先试 manifest/index，再退化为“发现该目录下的直接文件与一层子目录”。

### 4.2 `package.json` 的 `kiana` manifest

```json
{
  "name": "my-package",
  "kiana": {
    "extensions": ["./ext1.ts", "./ext2.ts"],
    "skills": ["..."],
    "prompts": ["..."],
    "themes": ["..."]
  }
}
```

解析规则（`readKianaManifest`）：

- 读文件（剥离 UTF-8 BOM）→ `JSON.parse`；任何错误返回 `null`；
- 顶层必须是对象，且 `pkg.kiana` 必须是对象（非数组），否则 `null`；
- 只处理 4 个字段 `extensions`/`skills`/`prompts`/`themes`；每个必须是**全字符串数组**，否则该字段被忽略
  （不整体失败）；
- 未出现的字段不出现在返回对象里；
- 扩展条目是**相对目录**解析（`path.resolve(dir, entry)`），不做 `~` 展开、不做 `@` 剥离；
- 只保留 `fs.existsSync` 为真的条目；若最终为空，则回退到 `index.ts`/`index.js`；
- `kiana.extensions` 存在且非空时**优先于** index 文件。

### 4.3 RPC 扩展 UI 子协议（JSON over stdout/stdin，逐行）

**请求（Kiana → 客户端）**——全部带 `type: "extension_ui_request"`、`id`（`crypto.randomUUID()`）、`method`：

```json
{"type":"extension_ui_request","id":"<uuid>","method":"select","title":"<t>","options":["a","b"],"timeout":10000}
{"type":"extension_ui_request","id":"<uuid>","method":"confirm","title":"<t>","message":"<m>","timeout":5000}
{"type":"extension_ui_request","id":"<uuid>","method":"input","title":"<t>","placeholder":"<p>","timeout":3000}
{"type":"extension_ui_request","id":"<uuid>","method":"editor","title":"<t>","prefill":"Line 1\nLine 2"}
{"type":"extension_ui_request","id":"<uuid>","method":"notify","message":"<m>","notifyType":"info|warning|error"}
{"type":"extension_ui_request","id":"<uuid>","method":"setStatus","statusKey":"<k>","statusText":"<text>|undefined"}
{"type":"extension_ui_request","id":"<uuid>","method":"setWidget","widgetKey":"<k>","widgetLines":["..."],"widgetPlacement":"aboveEditor|belowEditor"}
{"type":"extension_ui_request","id":"<uuid>","method":"setTitle","title":"<t>"}
{"type":"extension_ui_request","id":"<uuid>","method":"set_editor_text","text":"<t>"}
```

字段细节：
- `select`/`confirm`/`input` 的 `timeout` 字段即使为 undefined 也在对象里（序列化后消失）；`editor` 没有。
- `editor` 不支持 timeout/signal（agent 侧没有自动解析；只能等响应或进程结束）。
- `setWidget`：只有 `content === undefined` 或数组时才发请求；`widgetLines` 为 `undefined` 表示清除；
  `widgetPlacement` 缺省时字段为 `undefined`（序列化消失）。
- `notify` 的 `notifyType` 缺省时字段为 `undefined`。
- `setStatus` 的 `statusText: undefined` 表示清除该 key。

**响应（客户端 → Kiana）**：

```json
{"type":"extension_ui_response","id":"<uuid>","value":"Allow"}      // select / input / editor
{"type":"extension_ui_response","id":"<uuid>","confirmed":true}     // confirm
{"type":"extension_ui_response","id":"<uuid>","cancelled":true}     // 任何对话框
```

取消语义：`select`/`input`/`editor` → `undefined`；`confirm` → `false`。
`id` 必须匹配请求；未知 `id` 被忽略。

**超时与 abort**：`ExtensionUIDialogOptions.timeout` 毫秒后 agent 侧以默认值解析（`select`/`input`/`editor` →
`undefined`，`confirm` → `false`）；`signal` abort 同样立即解析默认值。交互式模式下 timeout 还显示倒计时
（`(Ns)`）并在到 0 时取消。

**扩展错误事件**（RPC stdout，独立于上面的子协议）：

```json
{"type":"extension_error","extensionPath":"<path>","event":"<event>","error":"<message>"}
```

### 4.4 MCP server 配置（`registerMcpServer` 的 `config` 形状）

与 `mcp.json` 的 `mcpServers` 条目同形（实现归 `telemetry-mcp`，此处列出本单元校验的边界）。

#### 4.4.1 形状

```ts
// 公共
{ exposure?: "codemode" | "deferred" | "direct" | "hidden";   // 默认 "codemode"
  description?: string;
  toolExposure?: Record<string, McpExposure>;                  // 精确名优先，其后按对象顺序匹配带 * 的模式
  enabled?: boolean;                                           // 默认 true
  timeout?: number }                                           // 秒；正数；默认 60；进度通知会重置

// stdio
{ type?: "stdio"; command: string; args?: string[]; env?: Record<string,string>; cwd?: string }

// HTTP
{ type?: "http"; url: string; headers?: Record<string,string>; oauth?: McpOAuthConfig;
  auth?: { provider: string } }
```

别名：`"codemode-deferred"` → `"codemode"`（`exposure` 与每个 `toolExposure` 值都会解析）。
`mcpNamespace(server) = "mcp__" + server.replace(/-/g, "_")`。
`SERVER_NAME = /^[A-Za-z0-9_-]+$/`。`isMcpToolName(name)` = `name.startsWith("mcp__")` 或
name ∈ {`list_mcp_resources`,`list_mcp_resource_templates`,`read_mcp_resource`}。

#### 4.4.2 校验错误文案（全部【逐字】，`validateMcpServerConfig`）

```
invalid server name "<name>" (use letters, digits, "_" and "-")
server "<name>" must be an object
server "<name>": exposure must be one of "codemode", "deferred", "direct", "hidden"
server "<name>": toolExposure must map tool names to exposures
server "<name>": toolExposure "<tool>" must be one of "codemode", "deferred", "direct", "hidden"
server "<name>": enabled must be a boolean
server "<name>": description must be a string
server "<name>": timeout must be a positive number of seconds
server "<name>": legacy SSE transport is not supported; use the streamable HTTP URL
server "<name>": url must be an http or https URL
server "<name>": headers must map names to strings
server "<name>": oauth must be an object
server "<name>": oauth.clientId must be a string
server "<name>": oauth.clientSecret must be a string
server "<name>": oauth.callbackPort must be a port number
server "<name>": oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment
server "<name>": oauth.callbackUrl and oauth.callbackPort name different ports
server "<name>": oauth.scope must be a string
server "<name>": oauth.clientName must be a non-empty string
server "<name>": oauth.clientRegistration must be "dcr" or "cimd"
server "<name>": oauth.clientRegistration "cimd" cannot be combined with oauth.clientId or oauth.clientName
server "<name>": oauth.clientRegistration "cimd" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback
server "<name>": oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]
server "<name>": auth.provider must be a provider name
server "<name>": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]
server "<name>": args must be an array of strings
server "<name>": env must map names to strings
server "<name>": cwd must be a string
server "<name>" needs either "command" (stdio) or "url" (streamable HTTP)
```

`LOOPBACK_HOSTS = ["localhost", "127.0.0.1", "[::1]"]`。
`isLoopbackRedirectUri(value)`：可解析、`protocol === "http:"`、host ∈ LOOPBACK_HOSTS、`search === ""`、`hash === ""`。

#### 4.4.3 `McpServerRegistry`

```
register(server)   → servers.set(server.name, server); changeListener?.()
unregister(name, extensionPath) → 仅当 servers.get(name)?.extensionPath === extensionPath 时 delete + changeListener?.()
get(name) → 条目或 undefined
list()     → 按注册顺序的**副本**数组
setChangeListener(fn)
```

`registerMcpServer` 用 `structuredClone(validated)` 存入，防止扩展后续改配置对象影响已注册项。

### 4.5 会话条目与消息形状（扩展可见）

扩展通过 `ctx.sessionManager`（只读）、`kiana.appendEntry`、`kiana.sendMessage`、`sendUserMessage` 触碰这些条目：

| 条目 `type` | 关键字段 |
|---|---|
| `custom` | `customType: string`、`data?: unknown`（不进入 LLM 上下文） |
| `custom_message` | `customType`、`content: string \| (TextContent\|ImageContent)[]`、`display: boolean`、`details?: unknown`（**会**发给模型） |
| `context_edit` | `targetId`、`replacement` |
| `compaction` | `summary`、`firstKeptEntryId: string \| null`、`details?`、`usage?` |
| `model_change` / `thinking_level_change` | 记录虚拟模型选择（虚拟模型路由） |

消息形状（`CustomMessage<T>`）：`{ role: "custom"; customType: string; content; display: boolean; details?: T; timestamp }`
（`ca-session` 拥有精确定义；扩展 API 只接受 `Pick<..., "customType"|"content"|"display"|"details">`）。

`nestedCalls`（tool result message 上）：

```json
{"calls":[{"id":"<parent>/<n>","name":"<tool>","status":"ok|error|unfinished",
           "arguments":{...}?,"argumentsBytes":<n>?,"durationMs":<n>?,"error":"<≤500 chars>"?}],
 "complete":true}
```

限制：`maxCalls 256`；单次参数 > 8 KiB 或累计 > 32 KiB → 省略 `arguments` 并记录 `argumentsBytes`，
`complete=false`；`error` 截断到 500 字符；`durationMs` 四舍五入；`snapshot()` 在“无调用且 complete”时返回
`undefined`，否则 `complete = complete && 无 unfinished`。

### 4.6 `SourceInfo` 与合成路径

```ts
type SourceScope = "user" | "project" | "temporary";
type SourceOrigin = "package" | "top-level";
interface SourceInfo { path: string; source: string; scope: SourceScope; origin: SourceOrigin; baseDir?: string }
const BUILTIN_PATH_PREFIX = "builtin:";
```

```ts
getSyntheticPathSource(path):
  if (path.startsWith("builtin:")) return "builtin"
  if (path.startsWith("<") && path.endsWith(">")) return path.slice(1,-1).split(":")[0] || "temporary"
  return undefined
isSyntheticPath(path) = path.startsWith("builtin:") || path.startsWith("<")

createSyntheticSourceInfo(path, { source, scope?, origin?, baseDir? }) =
  { path, source, scope: scope ?? "temporary", origin: origin ?? "top-level", baseDir }
```

因此扩展的 `sourceInfo`：
- 文件：`{ path: <用户写的 path>, source: "local", scope: "temporary", origin: "top-level", baseDir: dirname(resolvedPath) }`
  （`source` 还会被 `ResourceLoader.applyExtensionSourceInfo` 用路径元数据覆盖为 `npm:...` 等）；
- `<inline:N>` / `<inline:name>`：`source = "inline"`，`baseDir = undefined`；
- `<sdk:name>`：`source = "sdk"`；
- `builtin:<name>`：`source = "builtin"`，`baseDir = undefined`。

`getExtensionSourceLabel`（资源路径 metadata 用）：
- 合成路径 → `extension:<path 去掉 <> 后>`（例如 `extension:inline:foo`、`extension:builtin:mcp`）；
- 文件路径 → `extension:<basename 去掉 .ts/.js>`。

### 4.7 环境变量

| 变量 | 影响 |
|---|---|
| `KIANA_TIMING=1` | 打开 `time()` 采样；扩展加载会记录 `<path> factory` 与 `<path> module import`（namespace `extensions`），并随启动计时打印 |
| `KIANA_CODING_AGENT_DIR` | `getAgentDir()`，决定全局扩展目录 `<agentDir>/extensions` |
| `KIANA_TUI_WRITE_LOG` | 记录原始 ANSI 流（TUI 诊断；供扩展组件调试） |
| `VISUAL` / `EDITOR` | 扩展编辑器组件的外部编辑器命令（优先于平台默认 `nano`/`notepad`） |
| `KIANA_OFFLINE` | 禁用启动网络操作（影响 provider 注册后的模型目录刷新，非本单元逻辑） |
| `KIANA_CODEMODE_MAX_WORKERS` | codemode 并发（`ca-builtin-extensions` / `agent-codemode`） |

扩展 flag 的值**不**来自环境变量，只来自 CLI 未知 flag（经 `applyExtensionFlagValues`）。

### 4.8 事件总线

```ts
interface EventBus { emit(channel: string, data: unknown): void;
                     on(channel: string, handler: (data: unknown) => void): () => void }
interface EventBusController extends EventBus { clear(): void }
```

实现基于 Node `EventEmitter`；`on` 会把 handler 包成 async 安全函数：

```
try { await handler(data) } catch (err) { console.error(`Event handler error (${channel}):`, err) }
```

【逐字】前缀 `Event handler error (` + channel + `):`。`emit` 同步触发所有监听器（Node EventEmitter 语义），
但每个监听器的 handler 是 async 的，因此实际的 `await handler(data)` 不阻塞 `emit` 返回。
Rust 侧：`tokio::sync::broadcast` **不**匹配（需要保留同步 emit + 异步 handler 的即发即忘语义）。
建议自实现 `EventBus { listeners: RwLock<HashMap<String, Vec<Handler>>> }`，`emit` 同步遍历并对每个 handler
`tokio::spawn` 一个任务（或 `spawn_local` 以支持非 Send 的 JS 回调），错误打到 stderr。

扩展订阅自动经 `runtime.trackEventBusSubscription` 跟踪，runtime invalidate（会话替换/reload）时统一退订。

### 4.9 定时/顺序保证

- 扩展加载**串行**（保持注册顺序 = handler 顺序 = 命令顺序 = flag 顺序）。
- 同一事件内 handler 按扩展加载顺序、再按扩展内注册顺序**串行 await**（`for ... await`）。
- 例外：`emitToolCall` 无 try/catch；`before_provider_headers` 返回值忽略；`ui_prompt_*` 事件异步（microtask）。
- 工具调用本身可并行（agent 层决定）；扩展不能假设兄弟调用/结果存在。
- `mcp_servers_change` 的触发出现在 `McpServerRegistry.register/unregister` 的同步回调中：`bindCore` 里
  `setChangeListener(() => { void this.emit(...); this.reportUnhandledMcpServers() })` → 异步派发，不等待。

---

## 5. 跨单元接口

本节给出 Rust 侧必须存在的类型与签名。crate 名按 §8.1 的建议：`kiana-extensions`。所有异步接口为 `async fn`
（`tokio`）；需要与 JS 回调交互的部分（扩展 handler 是 JS 函数）由 §8.3 的 JS 桥提供，Rust 侧把 handler 抽象为
`dyn` 回调。

### 5.1 本单元消费的其它单元接口

```rust
// ---- ca-cli: src/core/source-info.rs ----
pub const BUILTIN_PATH_PREFIX: &str = "builtin:";
pub enum SourceScope { User, Project, Temporary }
pub enum SourceOrigin { Package, TopLevel }
pub struct SourceInfo { pub path: String, pub source: String, pub scope: SourceScope,
                        pub origin: SourceOrigin, pub base_dir: Option<PathBuf> }
pub fn get_synthetic_path_source(path: &str) -> Option<String>;
pub fn is_synthetic_path(path: &str) -> bool;
pub struct SyntheticSourceInfoOptions { pub source: String, pub scope: Option<SourceScope>,
                                        pub origin: Option<SourceOrigin>, pub base_dir: Option<PathBuf> }
pub fn create_synthetic_source_info(path: &str, opts: SyntheticSourceInfoOptions) -> SourceInfo;

// ---- ca-cli: src/core/kiana-manifest.rs ----
pub struct KianaManifest { pub extensions: Option<Vec<String>>, pub skills: Option<Vec<String>>,
                        pub prompts: Option<Vec<String>>, pub themes: Option<Vec<String>> }
pub fn read_kiana_manifest(package_json_path: &Path) -> Option<KianaManifest>;

// ---- ca-cli: src/core/event-bus.rs ----
pub type EventHandler = Box<dyn Fn(serde_json::Value) -> BoxFuture<'static, ()> + Send + Sync>;
pub trait EventBus: Send + Sync {
    fn emit(&self, channel: &str, data: serde_json::Value);
    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe;
    fn clear(&self);
}
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

// ---- ca-cli: src/core/timings.rs / diagnostics.rs ----
pub fn time(label: &str, namespace: TimingNamespace);        // TimingNamespace::Extensions
pub struct ResourceDiagnostic { pub kind: DiagnosticKind /* Warning | Error | Collision */,
                                pub message: String, pub path: Option<String>,
                                pub collision: Option<ResourceCollision> }

// ---- ca-cli: src/core/exec.rs ----
pub struct ExecOptions { pub signal: Option<AbortSignal>, pub timeout: Option<Duration>, pub cwd: Option<PathBuf> }
pub struct ExecResult { pub stdout: String, pub stderr: String, pub code: i32, pub killed: bool }
pub async fn exec_command(command: &str, args: &[String], cwd: &Path, options: Option<ExecOptions>) -> ExecResult;

// ---- ca-cli: src/utils/paths.rs ----
pub struct ResolvePathOptions { pub normalize_unicode_spaces: bool, /* … trim, expand_tilde, strip_at_prefix */ }
pub fn resolve_path(input: &str, cwd: Option<&Path>, opts: ResolvePathOptions) -> PathBuf;

// ---- telemetry-mcp: src/core/mcp-servers.rs ----
pub enum McpExposure { Codemode, Deferred, Direct, Hidden }
pub fn validate_mcp_server_config(name: &str, raw: &serde_json::Value) -> Result<McpServerConfig, String>;
pub fn mcp_namespace(server: &str) -> String;                 // format!("mcp__{}", server.replace('-', "_"))
pub struct RegisteredMcpServer { pub name: String, pub config: McpServerConfig, pub extension_path: String }
pub struct McpServerRegistry { /* … */ }
impl McpServerRegistry {
    pub fn register(&self, server: RegisteredMcpServer);
    pub fn unregister(&self, name: &str, extension_path: &str);
    pub fn get(&self, name: &str) -> Option<RegisteredMcpServer>;
    pub fn list(&self) -> Vec<RegisteredMcpServer>;
    pub fn set_change_listener(&self, f: Box<dyn Fn() + Send + Sync>);
}

// ---- ca-tools-compaction-export: src/core/tools/tool-definition-wrapper.rs ----
pub type ToolContextFactory = Box<dyn Fn(&str, Option<AbortSignal>) -> ExtensionToolContext + Send + Sync>;
pub fn wrap_tool_definition<TDetails>(def: &ToolDefinition, ctx_factory: Option<ToolContextFactory>) -> AgentTool;

// ---- ca-session: src/core/system-prompt.rs ----
pub struct BuildSystemPromptOptions { /* 见 §2 与 §5.2；字段与 TS 一一对应 */ }
pub struct NormalizedBuildSystemPromptOptions { /* 集合字段补全（默认 selected_tools = read/bash/edit/write） */ }
pub fn normalize_build_system_prompt_options(input: BuildSystemPromptOptions) -> NormalizedBuildSystemPromptOptions;
pub fn build_system_prompt(input: &BuildSystemPromptOptions) -> String;
pub fn get_current_system_message(messages: &[AgentMessage]) -> Option<AgentMessage>;

// ---- ca-config-models ----
pub struct ModelRegistry;   // register_provider / unregister_provider / register_native_provider /
                            // register_virtual_model / unregister_virtual_model / find / get_model …
pub struct Provider;        // 原生 provider 对象（含 stream_simple 等）
pub struct ProviderConfig;  // §2.1.13
pub struct VirtualModelDefinition<TState>;

// ---- tui-core / tui-components ----
pub struct Theme;  pub struct Component;  pub struct Tui;  pub struct KeybindingsManager;
pub struct EditorComponent;  pub struct EditorTheme;  pub struct OverlayOptions;  pub struct OverlayHandle;
pub struct ReadonlyFooterDataProvider;  pub struct AutocompleteProvider;  pub struct AutocompleteItem;
```

### 5.2 本单元暴露给其它单元的类型与函数

```rust
// ===== kiana-extensions::types =====
pub enum ExtensionMode { Tui, Rpc, Json, Print }   // TS 是字符串联合类型 "tui" | "rpc" | "json" | "print"；Rust 用 enum + as_str()，序列化/比较必须按字符串语义
pub enum ToolExposure { Direct, ModelOnly, Codemode, Deferred, Hidden }
pub enum WidgetPlacement { AboveEditor, BelowEditor }
pub enum ProjectTrustDecision { Yes, No, Undecided }
pub enum AgentActivityOutcome { Completed, Aborted, Error }
pub enum InputSource { Interactive, Rpc, Extension }
pub enum ModelSelectSource { Set, Cycle, Restore }
pub enum UIPromptKind { Select, Confirm, Input, Editor, Custom }

pub struct ContextUsage { pub tokens: Option<u64>, pub context_window: u64, pub percent: Option<f64> }
pub struct CompactOptions { pub custom_instructions: Option<String>,
                            pub on_complete: Option<Box<dyn Fn(CompactionResult) + Send + Sync>>,
                            pub on_error: Option<Box<dyn Fn(ExtensionErrorValue) + Send + Sync>> }
pub struct ExtensionUIDialogOptions { pub signal: Option<AbortSignal>, pub timeout: Option<Duration> }
pub struct ExtensionWidgetOptions { pub placement: WidgetPlacement }   // default AboveEditor
pub struct WorkingIndicatorOptions { pub frames: Option<Vec<String>>, pub interval_ms: Option<u64> }

pub struct ToolRenderResultOptions { pub expanded: bool, pub is_partial: bool }
pub struct ToolRenderContext { pub args: serde_json::Value, pub tool_call_id: String,
    pub invalidate: Box<dyn Fn() + Send + Sync>, pub last_component: Option<Component>,
    pub state: RenderState, pub cwd: PathBuf, pub execution_started: bool, pub args_complete: bool,
    pub is_partial: bool, pub expanded: bool, pub show_images: bool, pub is_error: bool,
    pub duration_ms: Option<u64>, pub output_pad: usize }
pub struct ToolAnnotations { pub read_only_hint: Option<bool>, pub destructive_hint: Option<bool>,
                             pub idempotent_hint: Option<bool>, pub open_world_hint: Option<bool> }
pub struct ToolNamespace { pub name: String, pub aliases: Option<Vec<String>>,
                           pub description: Option<String>, pub instructions: Option<String> }
pub struct ToolLoadoutChanges { pub descriptions: Option<BTreeMap<String, String>>,
                                pub hidden_declarations: Option<Vec<String>> }
pub trait ToolLoadout {
    fn declared(&self) -> &[AgentTool]; fn callable(&self) -> &[AgentTool]; fn registered(&self) -> &[AgentTool];
    fn get_exposure(&self, name: &str) -> ToolExposure;
    fn get_namespace(&self, name: &str) -> Option<&ToolNamespace>;
    fn get_prompt_guidelines(&self, name: &str) -> &[String];
}
pub struct ToolDefinition {
    pub name: String, pub label: String, pub description: String,
    pub prompt_snippet: Option<String>, pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: JsonSchema,                                // 必须是 object schema
    pub constrained_sampling: Option<ConstrainedSampling>,      // None == false == undefined
    pub render_shell: Option<RenderShell>,                     // Default | Self
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    pub output_schema: Option<JsonSchema>,
    pub exposure: Option<ToolExposure>,                        // default Direct
    pub namespace: Option<ToolNamespace>,
    pub reaches_tools: Option<ReachesToolsFn>,
    pub annotations: Option<ToolAnnotations>,
    pub default_active: Option<bool>,
    pub prepare_loadout: Option<PrepareLoadoutFn>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub execute: ToolExecuteFn,
    pub render_call: Option<RenderCallFn>,
    pub render_result: Option<RenderResultFn>,
}
pub fn define_tool(def: ToolDefinition) -> ToolDefinition;      // 恒等

// 事件与结果：一一对应 TS（全部 struct + serde rename_all = "camelCase"），例如
pub struct MessageEndEvent { pub r#type: &'static str, pub message: AgentMessage }
pub struct MessageEndEventResult { pub message: Option<AgentMessage> }
pub struct ToolCallEventResult { pub block: Option<bool>, pub reason: Option<String>, pub terminate: Option<bool> }
pub struct ToolResultEventResult { pub content: Option<Vec<ContentBlock>>, pub details: Option<Value>,
    pub structured_content: Option<Value>, pub is_error: Option<bool>, pub usage: Option<Usage> }
pub struct InputEventResult;     // enum：Continue / Transform { text, images } / Handled
pub struct UserBashEventResult;  // enum：Operations(BashOperations) / Result(BashResult)
pub struct BoundaryResult { pub entries: Option<Vec<SessionBoundaryDraft>>, pub r#continue: Option<bool> }
pub struct BeforeAgentStartEventResult { pub message: Option<CustomMessagePayload>, pub system_prompt: Option<String> }
pub struct ResourcesDiscoverResult { pub skill_paths: Option<Vec<String>>, pub prompt_paths: Option<Vec<String>>,
                                     pub theme_paths: Option<Vec<String>> }
pub struct SessionBeforeSwitchResult { pub cancel: Option<bool> }
pub struct SessionBeforeForkResult  { pub cancel: Option<bool>, pub skip_conversation_restore: Option<bool> }
pub struct SessionBeforeCompactResult { pub cancel: Option<bool>, pub compaction: Option<CompactionResult> }
pub struct SessionBeforeTreeResult { pub cancel: Option<bool>, pub summary: Option<TreeSummary>,
    pub custom_instructions: Option<String>, pub replace_instructions: Option<bool>, pub label: Option<String> }
pub struct ProjectTrustEventResult { pub trusted: ProjectTrustDecision, pub remember: Option<bool> }
pub enum CacheWarmingAction { Warm, Stop }

// ===== 上下文 trait（不是 struct：见 §2.3.10 的“读时解析”要求）=====
pub trait ExtensionUiContext: Send + Sync {
    async fn select(&self, title: &str, options: &[String], opts: Option<ExtensionUIDialogOptions>) -> Option<String>;
    async fn confirm(&self, title: &str, message: &str, opts: Option<ExtensionUIDialogOptions>) -> bool;
    async fn input(&self, title: &str, placeholder: Option<&str>, opts: Option<ExtensionUIDialogOptions>) -> Option<String>;
    fn notify(&self, message: &str, kind: Option<NotifyKind>);
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe;
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn set_widget(&self, key: &str, content: WidgetContent, options: Option<ExtensionWidgetOptions>);
    fn set_footer(&self, factory: Option<FooterFactory>);
    fn set_header(&self, factory: Option<HeaderFactory>);
    fn set_title(&self, title: &str);
    async fn custom<T: 'static>(&self, factory: CustomFactory<T>, options: Option<CustomOptions>) -> T;
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
    async fn editor(&self, title: &str, prefill: Option<&str>) -> Option<String>;   // TS: editor(title, prefill?)，**没有** dialog options
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);
    fn set_editor_component(&self, factory: Option<EditorFactory>);
    fn get_editor_component(&self) -> Option<EditorFactory>;
    fn theme(&self) -> &Theme;
    fn get_all_themes(&self) -> Vec<ThemeInfo>;                 // { name, path: Option<PathBuf> }
    fn get_theme(&self, name: &str) -> Option<Theme>;
    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult;     // { success, error? }
    fn get_tools_expanded(&self) -> bool;
    fn set_tools_expanded(&self, expanded: bool);
}

pub trait ExtensionContextApi: Send + Sync {
    fn ui(&self) -> &dyn ExtensionUiContext;
    fn mode(&self) -> ExtensionMode;
    fn has_ui(&self) -> bool;
    fn cwd(&self) -> &Path;
    fn session_manager(&self) -> &dyn ReadonlySessionManager;
    fn model_registry(&self) -> &ModelRegistry;
    fn model(&self) -> Option<Model>;
    fn scoped_models(&self) -> Vec<ScopedModel>;
    fn thinking_level(&self) -> Option<ThinkingLevel>;
    fn is_idle(&self) -> bool;
    fn is_project_trusted(&self) -> bool;
    fn signal(&self) -> Option<AbortSignal>;
    fn abort(&self);
    fn has_pending_messages(&self) -> bool;
    fn shutdown(&self);
    fn get_context_usage(&self) -> Option<ContextUsage>;
    fn compact(&self, options: Option<CompactOptions>);
    fn get_system_prompt(&self) -> String;
}

pub trait ExtensionToolContextApi: ExtensionContextApi {
    fn tools(&self) -> Vec<AgentTool>;
    async fn execute_tool(&self, name: &str, args: Value, options: Option<ExecuteToolOptions>) -> AgentToolCallOutcome;
}

pub trait ExtensionCommandContextApi: ExtensionContextApi {
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
    async fn wait_for_idle(&self);
    async fn new_session(&self, options: Option<NewSessionOptions>) -> NewSessionResult;
    async fn fork(&self, entry_id: &str, options: Option<ForkOptions>) -> NewSessionResult;
    async fn navigate_tree(&self, target_id: &str, options: Option<NavigateTreeOptions>) -> NewSessionResult;
    async fn switch_session(&self, session_path: &str, options: Option<SwitchSessionOptions>) -> NewSessionResult;
    async fn reload(&self);
}

// ===== 注册表 / 产物 =====
pub struct ExtensionFlag { pub name: String, pub description: Option<String>,
                          pub type_: FlagType /* Boolean | String */, pub default: Option<FlagValue>,
                          pub extension_path: String }        // 与 ca-cli §5.2 的 ExtensionFlag 保持一致
pub struct ExtensionShortcut { pub shortcut: String, pub description: Option<String>,
                              pub handler: ShortcutHandler, pub extension_path: String }
pub struct RegisteredCommand { pub name: String, pub source_info: SourceInfo, pub description: Option<String>,
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
    pub handler: CommandHandler /* (args, ctx: ExtensionCommandContext) -> BoxFuture<'static, ()> */ }
pub struct ResolvedCommand { pub command: RegisteredCommand, pub invocation_name: String }
pub struct RegisteredTool { pub definition: ToolDefinition, pub source_info: SourceInfo }
pub struct ToolInfo { pub name: String, pub description: String, pub parameters: JsonSchema,
    pub prompt_guidelines: Option<Vec<String>>, pub reaches_tools: Option<ReachesToolsFn>,
    pub exposure: ToolExposure, pub namespace: Option<ToolNamespace>, pub annotations: Option<ToolAnnotations>,
    pub source_info: SourceInfo }
pub struct Extension { pub path: String, pub resolved_path: String, pub hidden: bool, pub replaceable: bool,
    pub source_info: SourceInfo,
    pub handlers: BTreeMap<String, Vec<ExtensionHandlerFn>>,
    pub tools: IndexMap<String, RegisteredTool>,
    pub message_renderers: IndexMap<String, MessageRenderer>,
    pub tool_renderers: Option<Vec<ToolRendererResolver>>,
    pub markdown_transformer: Option<MarkdownTransformer>,
    pub entry_renderers: Option<IndexMap<String, EntryRenderer>>,
    pub commands: IndexMap<String, RegisteredCommand>,
    pub flags: IndexMap<String, ExtensionFlag>,
    pub shortcuts: IndexMap<String, ExtensionShortcut> }
pub enum InlineExtension { Factory(ExtensionFactory),
    Named { name: String, factory: ExtensionFactory, hidden: bool, replaceable: bool, builtin: bool } }
pub struct LoadExtensionsResult { pub extensions: Vec<Extension>, pub errors: Vec<LoadError>,
    pub warnings: Vec<LoadWarning>, pub runtime: Arc<ExtensionRuntime> }
pub struct LoadError { pub path: String, pub error: String }
pub struct LoadWarning { pub path: String, pub warning: String }
pub struct ExtensionError { pub extension_path: String, pub event: String, pub error: String,
                            pub stack: Option<String> }

// ===== 动作集合（ca-session 注入）=====
pub struct ExtensionActions { pub send_message: …, pub send_user_message: …, pub append_entry: …,
    pub set_session_name: …, pub get_session_name: …, pub set_label: …, pub get_active_tools: …,
    pub get_all_tools: …, pub get_settings: …, pub set_active_tools: …, pub refresh_tools: …,
    pub get_commands: …, pub set_model: …, pub get_thinking_level: …, pub set_thinking_level: … }
pub struct ExtensionContextActions { pub get_model: …, pub get_scoped_models: …, pub is_idle: …,
    pub is_project_trusted: …, pub get_signal: …, pub abort: …, pub has_pending_messages: …,
    pub shutdown: …, pub get_context_usage: …, pub compact: …, pub get_system_prompt: …,
    pub get_system_prompt_options: Option<…>, pub execute_tool: Option<…>, pub get_callable_tools: Option<…> }
pub struct ExtensionCommandContextActions { pub wait_for_idle: …, pub new_session: …, pub fork: …,
    pub navigate_tree: …, pub switch_session: …, pub reload: … }
pub struct ProviderActions { pub register_provider: Option<…>, pub register_native_provider: Option<…>,
    pub unregister_provider: Option<…>, pub register_virtual_model: Option<…>,
    pub unregister_virtual_model: Option<…> }

// ===== loader =====
pub fn create_extension_runtime() -> Arc<ExtensionRuntime>;
pub fn clear_extension_cache();
pub async fn load_extension_from_factory(factory: ExtensionFactory, cwd: &Path, event_bus: Arc<dyn EventBus>,
    runtime: Arc<ExtensionRuntime>, extension_path: Option<&str>) -> Result<Extension, String>;
pub async fn load_extensions(paths: &[String], cwd: &Path, event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<Arc<ExtensionRuntime>>) -> LoadExtensionsResult;
pub async fn load_extensions_cached(paths: &[String], cwd: &Path, event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<Arc<ExtensionRuntime>>) -> LoadExtensionsResult;
pub async fn discover_and_load_extensions(configured_paths: &[String], cwd: &Path,
    agent_dir: Option<&Path>, event_bus: Option<Arc<dyn EventBus>>) -> LoadExtensionsResult;

// ===== runner =====
impl ExtensionRunner {
    pub fn new(extensions: Vec<Extension>, runtime: Arc<ExtensionRuntime>, cwd: PathBuf,
               session_manager: Arc<dyn SessionManager>, model_registry: Arc<ModelRegistry>) -> Self;
    pub fn bind_core(&self, actions: ExtensionActions, context_actions: ExtensionContextActions,
                     provider_actions: Option<ProviderActions>);
    pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>);
    pub fn set_ui_context(&self, ui: Option<Arc<dyn ExtensionUiContext>>, mode: ExtensionMode);
    pub fn get_ui_context(&self) -> Option<Arc<dyn ExtensionUiContext>>;
    pub fn has_ui(&self) -> bool;
    pub fn get_extension_paths(&self) -> Vec<String>;
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool>;
    pub fn get_tool_definition(&self, tool_name: &str) -> Option<ToolDefinition>;
    pub fn get_flags(&self) -> IndexMap<String, ExtensionFlag>;
    pub fn set_flag_value(&self, name: &str, value: FlagValue);
    pub fn get_flag_values(&self) -> IndexMap<String, FlagValue>;
    pub fn get_shortcuts(&self, resolved_keybindings: &KeybindingsConfig) -> IndexMap<String, ExtensionShortcut>;
    pub fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic>;
    pub fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic>;
    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand>;
    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand>;
    pub fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer>;
    pub fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer>;
    pub fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer>;
    pub fn resolve_tool_renderers(&self, tool_name: &str,
        base: &dyn Fn() -> Option<ToolRenderers>) -> Option<ToolRenderers>;
    pub fn get_model_registry(&self) -> Arc<ModelRegistry>;
    pub fn invalidate(&self, message: Option<String>);
    pub fn on_error(&self, listener: Box<dyn Fn(ExtensionError) + Send + Sync>) -> Unsubscribe;
    pub fn emit_error(&self, error: ExtensionError);
    pub fn report_unhandled_mcp_servers(&self);
    pub fn has_handlers(&self, event_type: &str) -> bool;
    pub fn create_context(&self) -> Arc<dyn ExtensionContextApi>;
    pub fn create_tool_context(&self, tool_call_id: &str, signal: Option<AbortSignal>) -> Arc<dyn ExtensionToolContextApi>;
    pub fn create_command_context(&self) -> Arc<dyn ExtensionCommandContextApi>;
    pub fn shutdown(&self);
    pub fn get_active_tools(&self) -> Vec<String>;

    pub async fn emit_boundary(&self, base: BoundaryBaseEvent, build_context: BuildBoundaryContext)
        -> BoundaryDispatchResult;
    pub async fn emit(&self, event: RunnerEmitEvent) -> Option<SessionBeforeResultAny>;
    pub async fn emit_cache_warming_decision(&self, event: CacheWarmingDecisionEvent) -> CacheWarmingAction;
    pub async fn emit_message_end(&self, event: MessageEndEvent) -> Option<AgentMessage>;
    pub async fn emit_tool_result(&self, event: ToolResultEvent) -> Option<ToolResultEventResult>;
    pub async fn emit_tool_call(&self, event: ToolCallEvent) -> Option<ToolCallEventResult>;
    pub async fn emit_user_bash(&self, event: UserBashEvent) -> Option<UserBashEventResult>;
    pub async fn emit_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage>;
    pub async fn emit_before_provider_request(&self, payload: Value) -> Value;
    pub async fn emit_before_provider_headers(&self, headers: ProviderHeaders) -> ProviderHeaders;
    pub async fn emit_before_agent_start(&self, trigger: BeforeAgentStartTrigger,
        options: BuildSystemPromptOptions) -> BeforeAgentStartCombinedResult;
    pub async fn emit_resources_discover(&self, cwd: &Path, reason: ResourcesDiscoverReason)
        -> ResourcesDiscoverPaths;      // { skill_paths, prompt_paths, theme_paths } 元素 { path, extension_path }
    pub async fn emit_input(&self, text: &str, images: Option<Vec<ImageContent>>, source: InputSource,
        streaming_behavior: Option<StreamingBehavior>) -> InputEventResult;
}
pub async fn emit_session_shutdown_event(runner: &ExtensionRunner, event: SessionShutdownEvent) -> bool;
pub async fn emit_project_trust_event(result: &LoadExtensionsResult, event: ProjectTrustEvent,
    ctx: ProjectTrustContext) -> ProjectTrustOutcome;   // { result?: ProjectTrustEventResult, errors: Vec<ExtensionError> }

// ===== wrapper =====
pub fn wrap_registered_tool(rt: &RegisteredTool, runner: &Arc<ExtensionRunner>) -> AgentTool;
pub fn wrap_registered_tools(rts: &[RegisteredTool], runner: &Arc<ExtensionRunner>) -> Vec<AgentTool>;

// ===== builtin 清单 =====
pub fn builtin_extensions() -> Vec<InlineExtension>;
```

### 5.3 调用方向与注入点（谁调用谁）

| 调用方 | 被调 | 说明 |
|---|---|---|
| `ca-resources::DefaultResourceLoader` | `load_extensions_cached` / `load_extension_from_factory` | 装配扩展集合，处理 `builtin:` 与 inline 工厂、`replaceable` 省略、冲突诊断 |
| `ca-cli::main` | `LoadExtensionsResult.errors/warnings` | 转成启动诊断（`Failed to load extension "…"`） |
| `ca-session::AgentSession` | `new ExtensionRunner` / `bind_core` / `bind_command_context` / `set_ui_context` | 每会话创建；`bindExtensions` 后 emit `session_start` + `report_unhandled_mcp_servers` + `resources_discover` |
| `ca-session` | 全部 `emit_*` | 见 `ca-session` §5.3；顺序与语义由本单元定义 |
| `ca-session::NestedToolCallRunner` | `ExtensionContextActions.execute_tool` | 嵌套调用走 agent 工具管线 |
| `ca-session` | `wrap_registered_tools` | 把扩展工具变成 `AgentTool` 参与 `_toolRegistry` |
| `ca-config-models::ModelRuntime` | `ProviderActions` 回调 | provider / virtual model 注册落点 |
| `telemetry-mcp`（MCP 扩展） | `has_handlers("mcp_servers_change")` / `get_mcp_servers` | 判断是否由本扩展连接注册服务器 |
| `tui-*` 与各 mode | `ExtensionUiContext` 实现 | 交互式 / RPC / noOp |
| `ca-tools-compaction-export`（HTML 导出） | `resolve_tool_renderers` | 为任意工具（含未注册）选择渲染器 |
| `ca-cli::print_help` | `ExtensionFlag[]` | 追加 "Extension CLI Flags:" 段 |

**事件顺序契约（ca-session 必须遵守，本单元断言）**：

```
bindExtensions: setUIContext → bindCommandContext → onError → emit(session_start)
              → reportUnhandledMcpServers() → emitResourcesDiscover(reason)
每 turn：     emit(turn_start) → [emitContext → emitBeforeProviderHeaders → emitBeforeProviderRequest
              → provider stream: emit(after_provider_response) / emit(provider_stream_event) / message events]
              → 每个工具：emit(tool_execution_start) → emitToolCall → [执行] → emitToolResult → emit(tool_execution_end)
              → emitBoundary(turn_end)
run：         emit(before_agent_start) 在 input 之后、agent_start 之前
              emitBoundary(agent_before_settle) → emit(agent_settled)
```

### 5.4 与 `ca-session` 的 `nestedCalls` 契约

`ctx.executeTool()` 的任何实现必须满足：

1. 调用 id 为 `<callerId>/<n>`，`n` 从 1 开始（每个 caller 独立计数）；
2. 事件 `tool_execution_start/update/end` 带 `parentToolCallId = callerId`；
3. 记录进 `NestedCallRecorder`（`maxCalls 256`、`maxArgumentBytesPerCall 8192`、`maxArgumentBytesTotal 32768`、
   `maxErrorChars 500`）；`arguments` 用 `JSON.stringify(args ?? {})` 的 UTF-8 字节数判定；
4. 结果 `usage` 逐层相加进调用工具的 result `usage`；
5. 失败（未知工具、校验失败、被阻止、执行抛错）一律 `isError: true`，**不 reject**；
6. 嵌套调用不写 transcript 条目；
7. 调用可并发；当 agent 的 `toolExecution === "sequential"` 时，嵌套调用也串行（同一个排队尾指针）；
8. 嵌套调用的 `tool_call` 抛错 → 该嵌套调用被阻止（fail-closed）。

补充实现细节（`src/core/nested-tool-calls.ts`，属 `ca-session`，但直接决定 `executeTool` 的可观察行为）：

- **记录字段**：`NestedToolCallRecord { id, name, status: "unfinished" | "ok" | "error", arguments?,
  argumentsBytes?, durationMs?, error? }`；`NestedToolCalls { calls, complete }`。
- **`arguments` 省略规则**：先 `JSON.stringify(args ?? {})`，按 UTF-8 字节数（`TextEncoder`）判定；
  单次超 8192 或累计超 32768 → 只写 `argumentsBytes`、不写 `arguments`，并置 `complete = false`。
- **`complete` 的最终值**：`complete && 所有 call 的 status !== "unfinished"`；`calls` 为空且 `complete` 时
  整个 `nestedCalls` 为 `undefined`（不写字段）。
- **`error` 截断**：`textOf(outcome.result)` = 所有 `type === "text"` 的 content 块文本用 `"\n"` 连接；
  取前 500 个 UTF-16 code unit（`slice(0, 500)`）。
- **`durationMs`**：`Math.round(performance.now() - start)`；`tool_execution_end` 里 `durationMs === undefined` 时
  **省略该键**（不是写 null）。
- **usage 合并**：每个嵌套结果的 `usage` 用 `combineUsage` 累加（含被丢弃记录的调用），最终加到调用工具的
  tool result message 的 `usage` 上；嵌套结果本身不落 transcript。
- **并发**：`isSequential()` 或该工具 `executionMode === "sequential"` 时进入全局队列（`queueTail` 链），
  且持有队列的调用内部的再嵌套不会自锁（`holdsQueue`）。
- **scope 生命周期**：按调用 id 建 scope，`takeRecord(toolCallId)` 取出并删除；`clear()` 清全部。

**本单元自身**（`createToolContext`）只负责：无 `executeToolFn` 时的回退结果（§2.3.10）、signal 缺省继承、
以及 `<callerId>/0` 这个回退 id（注意：**回退用 `/0`，真实实现从 `/1` 开始**）。

---

## 6. npm 依赖与 Rust crate 映射

### 6.1 本单元直接使用的 npm 包

| npm 包 | 用途 | 位置 | 建议 Rust 替代 | 行为差异与补救 |
|---|---|---|---|---|
| `jiti@2.7.0` | 运行期 TS/ESM 转换与模块解析（`createJiti`、`jiti.import`、`moduleCache:false`、`virtualModules`、`alias`、`tsconfigPaths`、`tryNative`） | loader.ts | **无等价 crate**。方案 A：`rquickjs` + 自建 TS→JS 转换管线（`oxc`/`swc`）；方案 B：进程内嵌 Node/Bun 子进程；方案 C：只支持 JS 扩展（用 `rquickjs` 直接跑），TS 扩展用 `oxc` 转换。见 §8.3 | `require.resolve`、`import.meta.resolve` 的解析算法（Node ESM resolution + `exports` 条件）、`tsconfig` paths、`~` 包解析、`node_modules` 逐级查找都必须复刻 |
| `typebox@1.3.27` | 扩展作者用来写 `parameters`/`outputSchema` 的 `Type.Object(...)`；作为 virtual module 提供 | virtual-modules.ts | `schemars` / `serde_json::Value`；**必须提供 JS 侧 `Type` 兼容 shim** | TypeBox 对象是**带 `Symbol` 标记的普通对象**（`[Kind]`、`[Optional]` 等）。Rust 侧需要的只是“是否为 object schema”与 JSON Schema 转换。JS shim 必须实现 `Type.Object/Union/String/Number/Boolean/Array/Optional/...` 子集，且 `JSON.stringify` 结果与真实 TypeBox 一致 |
| `jiti/static` | 编译二进制内嵌 Babel 的入口 | jiti-static-loader.ts | 无 | 仅用于 Bun/SEA 打包；Rust 单二进制不需要 |
| `chalk@6.0.0` | 帮助文本加粗（`Extension CLI Flags:`、`kiana`） | 通过 `ca-cli` 的 `printHelp` | `owo-colors` / `anstream` 或自写 ANSI | chalk 6 在非 TTY / `NO_COLOR` / `FORCE_COLOR` 下自动禁用；须与 `ca-cli` 的全局判定一致。帮助文本的 `padEnd(30)` 按 UTF-16 长度，Rust 用 `chars().count()`（仅 ASCII 名字时等价） |
| `node:fs`（`existsSync`/`readdirSync`/`statSync`/`readFileSync`） | 发现与 manifest | loader.ts | `std::fs` / `tokio::fs` | `readdirSync(withFileTypes)` 的 `isSymbolicLink()` 语义：**不跟随**链接判断类型；Rust `DirEntry::file_type()` 相同。`existsSync` 对不可读路径返回 false |
| `node:path`（`resolve`/`join`/`dirname`/`basename`/`extname`） | 路径 | loader.ts / runner.ts | `std::path` + `path-clean` 或自写 POSIX 语义 | Node 的 `path.resolve`/`join` 在 Linux 上是 POSIX；Windows 上 `path.win32` 语义不同（盘符、`\\`）。**必须区分平台**；`resolvePath` 还要处理 `~`、`@` 前缀、Unicode 空格（见 `ca-cli`） |
| `node:module`（`createRequire`/`require.resolve`） | 解析 typebox 入口 | loader.ts | `import_resolver` / 自写 | 只在 built Node 分支使用，Rust 可省或替换为资源解析 |
| `node:url`（`fileURLToPath`/`import.meta.resolve`） | 路径与别名 | loader.ts | `url` crate | |
| `node:events`（`EventEmitter`） | EventBus | event-bus.ts | 自实现（§4.8） | Node 的 `emit` 同步调用所有监听器；`off` 移除匹配的包装函数。Rust 需要 `Arc` 句柄比较 |
| `node:child_process`（`spawn`） | `kiana.exec()` | exec.ts | `tokio::process::Command` | `shell: false`、`stdio: ["ignore","pipe","pipe"]`、`SIGTERM` 超时、`killed` 标志、UTF-8 跨块解码（`encoding_rs` 或 `String::from_utf8_lossy` 逐块 + 增量解码器） |
| `crypto.randomUUID()` | RPC 请求 id | rpc-mode.ts | `uuid` crate（v4） | 格式 v4 小写带连字符 |
| `structuredClone()` | `emitContext` 深克隆、`registerMcpServer` 配置克隆 | runner.ts / loader.ts | `serde_json` round-trip 或 `Clone` | 必须保持 JSON 语义（`undefined` 字段在 clone 中保留为键，但序列化消失；Date/Map 等扩展值不可克隆——实际类型是 JSON 值，无差异） |
| `queueMicrotask` | `ui_prompt_*` 异步派发 | runner.ts | 见 §8.5 | 微任务语义：在当前同步栈结束后、任何 I/O 前运行；Rust 用 `tokio::spawn` 或 `spawn_local` 近似（顺序保证更弱，需在测试中放宽或在派发点显式插入任务） |
| `performance.now()` | 嵌套调用 durationMs | nested-tool-calls.ts（ca-session） | `std::time::Instant` | 单调时钟，取整到毫秒 |
| `path.localeCompare` | 启动列表排序 | interactive-mode.ts | `str::cmp`（UTF-8 字节序） | **差异**：`localeCompare` 依赖 ICU 区域设置；对 ASCII 扩展名等价，对中日韩标签可能不同。测试只断言包含关系，可用字节序并在 spec 说明 |
| `TextEncoder` | 参数字节数 | nested-tool-calls.ts | `str::len()`（UTF-8 字节） | `TextEncoder` 就是 UTF-8，等价 |
| `node:os.homedir` | `~` 展开 | paths.ts | `dirs` crate | |

### 6.2 间接（通过 virtual modules 暴露给扩展）的包

扩展可以 `import` 到宿主模块，因此 Rust 实现必须提供**同名同形的 JS 面**（如果采用 rquickjs 方案）：

| 模块 | 内容 | Rust 侧来源 |
|---|---|---|
| `typebox`、`typebox/compile`、`typebox/value`、`@sinclair/*` | 全部 TypeBox API | JS shim + Rust JSON Schema 模型 |
| `@earendil-works/kiana-agent-core` / `@mariozechner/kiana-agent-core` | `Agent`、`AgentTool`、工具管线类型 | `durable-rest`/`durable-harness` crate 的 JS 绑定 |
| `@earendil-works/kiana-tui` / `@mariozechner/kiana-tui` | 组件、`visibleWidth`、`truncateToWidth`、`matchesKey`、`Key`、`CURSOR_MARKER`、`renderFakeCursor` 等 | `tui-core`/`tui-components` 的 JS 绑定（或纯 JS 实现桥回 Rust 渲染） |
| `@earendil-works/kiana-ai`（→ compat）、`/compat`、`/models`、`/oauth`、`/providers/all` + `@mariozechner/*` | 模型/流/认证/提供商目录 | `ai-*` crate 的 JS 绑定 |
| `@earendil-works/kiana-coding-agent` / `@mariozechner/kiana-coding-agent` | SDK 根导出面（含 `CustomEditor`、`defineTool`、全部扩展类型/守卫） | 本单元 + `ca-*` 其它 crate 的 JS 绑定 |

**关键**：`kiana-ai` 根必须解析到 **compat** 入口；`kiana-ai/models` 必须可用（`11156` 测试）。

### 6.3 JS ↔ Rust 语义桥接表

| JS 语义 | 差异点 | Rust 处理 |
|---|---|---|
| 字符串索引（UTF-16 code unit） | Rust `String` 是 UTF-8 字节 / `char` 是标量 | 所有暴露给 JS 的字符串切片/长度（`slice`、`length`、`charAt`、`padEnd`、正则 lastIndex）必须由 JS 引擎（rquickjs）处理，**不要**在 Rust 侧用 `len()` 冒充。Rust 内部计算展示宽度用 `unicode-width` |
| 字符串比较 / `===` | 对象引用相等 | `AgentMessage` 的 `sameMessages` 是**引用相等**；Rust 需用 `Arc` + `Arc::ptr_eq` 或插入稳定 id（不可用 `PartialEq` 深比较，否则语义变化：原地改写会误判为“未改变”） |
| 数值 | 只有 `f64`（`number`）；JSON 数字无 int/float 区分 | JSON 值与 `Usage` 等字段用 `serde_json::Number`；`Math.ceil`、`Math.round` 显式实现；大整数（>2^53）行为需与 JS 一致（用 f64 或 i64 + 校验） |
| `Date`/时间戳 | 毫秒数 `number`；ISO 字符串 | `TurnStartEvent.timestamp`、`MarkdownTransformContext.timestamp`（ISO string）用 `chrono`；ISO 格式必须与 `toISOString()` 一致（含 `Z`，毫秒 3 位，无时区偏移） |
| 正则 | JS RegExp（回溯、`/u`、命名组、lookbehind） | Rust `regex` **不支持** backreference/lookbehind。本单元用到的正则简单：`SERVER_NAME`、`UNICODE_SPACES`、`/\/index\.[cm]?[jt]s$/`、`/^https?:$/`、`/^(.*\/node_modules)\/(@?[^/]+(?:\/[^/]+)?)$/`、`/\.(ts|js)$/`、`/\.[cm]?[jt]s$/` 等——全部可用 `regex` 表达。**但扩展自己的正则跑在 JS 引擎里**，不受影响 |
| JSON 键顺序 | JS 对象保留插入顺序 | `serde_json` 启用 `preserve_order`（`indexmap`）；`BTreeMap` 会改变顺序，禁止用于线上对象 |
| `undefined` vs 缺失 | `{a: undefined}` 与 `{}` 在 `in`/`Object.keys` 上不同，但 JSON 序列化相同 | 事件对象在**同一进程内**传给 JS handler 时，若用 serde 序列化会丢失“键存在但值为 undefined”。需要区分的地方：`InputEvent.result` 的 `images`、`tool_result` 返回值的 `structuredContent`（“返回 undefined” vs “不返回”语义相同，可接受）；`Partial` 语义用 `Option` 即可 |
| 异常 | `throw` 任意值（不仅 Error） | handler 抛非 Error 时 `String(err)`；Rust 用 `ExtensionThrown { value: JsValue, message: String }`，`message` 取 `err.message` 或 `String(err)` |
| `structuredClone` | 支持 Map/Set/Date/cycle | 本单元只克隆 JSON 值，用 `serde_json` round-trip |
| 微任务/事件循环 | 单线程、微任务先于宏任务 | 见 §8.5；`emitUIPromptEvent` 用“把事件塞进队列，在当前 await 点之后运行” |
| `Object.defineProperties` 惰性 getter | 读时求值 | Rust trait 方法天然如此 |
| 鸭子类型（`"actionHandlers" in newEditor`） | 跨模块边界不能用 `instanceof` | Rust 侧通过显式 trait 对象 + 可选能力（`Option<Box<dyn CustomEditorExt>>`）表达；JS shim 必须保留 `in` 检测行为 |
| `console.warn` / `console.error` | 打到 stderr | `eprintln!`（不加颜色） |
| `Object.entries` 顺序 | 整数键先升序，其余插入序 | 需要逐条检查：`resolvedKeybindings` 迭代顺序、`toolExposure` 模式匹配顺序（“对象中第一个匹配的模式”）。Rust 用 `IndexMap` 保插入序；**但整数样式的键会被 JS 提前**——`toolExposure` 的键是工具名（非纯数字），安全 |
| `String(err)` | Error → `"Error: msg"` | 仅在非 Error 分支；Error 分支取 `.message` |

### 6.4 建议的 Cargo 依赖

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "process", "io-util", "fs"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
indexmap = { version = "2", features = ["serde"] }
thiserror = "2"
parking_lot = "0.12"
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
unicode-width = "0.2"
regex = "1"
tracing = "0.1"

# JS 扩展执行（方案见 §8.3）
rquickjs = { version = "0.9", features = ["full-async", "parallel"] }   # 或 quickjs-wasi（与 TS 同款）
oxc = { ... }        # TS/JS 解析与转换（或 swc_core）
swc_common = "..."   # 备选

# 路径/文件
dirs = "6"
path-clean = "1"

# 可选
owo-colors = "4"     # 仅当本 crate 直接输出 ANSI（正常应交给 ca-cli/tui-core）
```

---

## 7. 测试清单

每个测试文件 → 断言的行为。Rust 侧应建同名测试（`#[tokio::test]`），文件名与用例名保持一致以便对照。

### 7.1 `test/extensions-discovery.test.ts`（567 行，31 个 `it` 用例）

前置：`mkdtempSync(os.tmpdir()/kiana-ext-test-)`，建 `<temp>/extensions`；`discoverAndLoadExtensions([], tempDir, tempDir)`。

| 用例 | 断言 |
|---|---|
| `discovers direct .ts files in extensions/` | 2 个扩展，`basename` 排序为 `["bar.ts","foo.ts"]`，无错误 |
| `loads the coding-agent entrypoint without rewriting kiana-ai provider subpaths` | `import { getAgentDir } from "@earendil-works/kiana-coding-agent"` 可解析；1 个扩展、0 错误 |
| `does not infer package ownership from ancestor manifests`（#9863） | 祖先 `package.json` 声明依赖、`node_modules` 内有真实实现时，**仍用宿主模块**：扩展能注册命令、`warnings` 为空 |
| `keeps the type-only kiana-ai OAuth compatibility barrel resolvable` | `import * as oauth from "@earendil-works/kiana-ai/oauth"` 可解析 |
| `discovers direct .js files in extensions/` | 1 个扩展 |
| `discovers subdirectory with index.ts` / `index.js` | 子目录入口被发现 |
| `prefers index.ts over index.js` | 同时存在时取 `.ts` |
| `discovers subdirectory with package.json kiana field` | manifest 条目被加载 |
| `keeps package.json kiana extension entries with leading tilde package-relative` | `["~entry.ts","~/entry.ts"]` 解析为 `<dir>/~entry.ts` 与 `<dir>/~/entry.ts`（**不做 tilde 展开**），2 个扩展、0 错误 |
| `package.json can declare multiple extensions` | `["./ext1.ts","./ext2.ts"]` → 2 个扩展 |
| `package.json with kiana field takes precedence over index.ts` | manifest 优先 |
| `ignores package.json without kiana field, falls back to index.ts` | 回退 |
| `ignores subdirectory without index or package.json` | 不加载 |
| `does not recurse beyond one level` | 深层不加载 |
| `handles mixed direct files and subdirectories` | 混合收集 |
| `skips non-existent paths declared in package.json` | 不存在的条目被跳过（全不存在时回退 index） |
| `loads extensions and registers commands` | `commands.has("test")` |
| `loads extensions and registers tools` | `tools.has(name)`；用 `Type.Object({})` |
| `reports errors for invalid extension code` | `errors[0].path` 含 `invalid.ts`，0 扩展 |
| `handles explicitly configured paths` | 绝对路径文件被加载 |
| `resolves dependencies from extension's own node_modules` | `examples/extensions/with-deps` 能加载并注册 `parse_duration` 工具（依赖解析走扩展自己的 `node_modules`） |
| `registers message and entry renderers` | `markdownTransformer` 存在、`messageRenderers.has("my-custom-type")`、`entryRenderers?.has("my-entry-type")` |
| `reports error when extension throws during initialization` | error 含 `Initialization failed!`，0 扩展 |
| `reports error when extension has no default export` | error 含 `does not export a valid factory function` |
| `allows multiple extensions to register different tools` | 各自工具都在 |
| `loads extension with event handlers` | `handlers` 有对应 key |
| `loads extension with shortcuts` | `shortcuts` 有 key |
| `loads extension with flags` | `flags` 有 name |
| `loadExtensions only loads explicit paths without discovery` | 不做目录发现 |
| `loadExtensions with no paths loads nothing` | 空结果 |

### 7.2 `test/extensions-runner.test.ts`（1440 行，51 个 `it` 块；其中 1 个 `it.each` 展开为 6 例，共 56 例）

setup：`createRunner(...extSources)` 把扩展源码写入 `<temp>/extensions/eN.ts` 后 `discoverAndLoadExtensions`，
再用 `SessionManager.inMemory()` + `createInMemoryModelRegistry(AuthStorage.inMemory())` 构造 `ExtensionRunner`。

| describe / 用例 | 断言 |
|---|---|
| `scopedModels` / `reflects the getScopedModels context action on ctx.scopedModels` | `bindCore` 的 `getScopedModels` 被读时反映 |
| `project_trust` / `continues past undecided handlers and returns the first yes/no decision` | 跳过 `undecided`，返回首个 yes/no；错误被收集 |
| `shortcut conflicts`（8 个用例） | 见 §2.3.9 的三条文案与“保留键阻止、非保留键警告后允许、保留项胜出与遍历顺序无关、一个保留动作多键、两个扩展同名快捷键” |
| `tool collection` / `collects tools from multiple extensions` | 合并、按扩展顺序 |
| `tool collection` / `rejects extension tools without a parameter schema` | 加载错误含 `must define an object parameter schema` |
| `tool collection` / `keeps first tool when two extensions register the same name` | 第一个胜出 |
| `command collection` / `collects commands from multiple extensions` | 顺序 |
| `command collection` / `gets command by invocation name` | `getCommand("name")` |
| `command collection` / `suffixes duplicate extension commands in insertion order` | `name:1`、`name:2` |
| `context creation` / 5 个用例 | `signal` 暴露；默认 `mode === "print"`、`hasUI === false`；`isProjectTrusted` 反映；RPC UI → `mode==="rpc"`、`hasUI===true`；TUI UI → `mode==="tui"`、`hasUI===true` |
| `error handling` / `calls error listeners when handler throws` | `onError` 收到 `{extensionPath, event, error}` |
| `error handling` / `fails closed when a user_bash handler throws` | `emitUserBash` **reject**，错误被上报 |
| `error handling` / `fails closed when a user_bash handler returns %s`（`it.each` 6 例，回归 #9068） | `{}`、`{operations:null}`、`{operations:{}}`、`{result:null}`、`{result:{output:"handled"}}`、`{operations:{exec}, result:{...完整...}}`：**6 例全部**让 `emitUserBash` reject 并含 `Invalid user_bash handler result`（最后一例因 XOR 同时给两者而非法）。合法形状的单例在下一行用例中验证。**注意**：`result` 形状要求 `output`(string)、`exitCode` **键存在**（可为 `undefined`）、`cancelled`(bool)、`truncated`(bool) 齐全；`operations` 与 `result` **不能同时**出现（XOR，见 §2.3.3）。第 3 例 `{operations:{}}` 因缺 `exec` 失败；第 5 例因缺 `exitCode` 键失败；第 6 例因 XOR 失败。 |
| `error handling` / `accepts valid user_bash operations and result overrides` | 两种合法形状被接受并返回 |
| `message and entry renderers` / 3 个用例 | Markdown transformer 按加载顺序；message renderer 按 customType；entry renderer 按 customType |
| `flags` / `collects flags from extensions`、`keeps first flag when two extensions register the same name`、`rejects default values that do not match the flag type`、`can set flag values` | 见 §2.2.4 registerFlag / getFlags / setFlagValue |
| `before_agent_start` / `keeps ctx.getSystemPrompt() in sync with chained system prompt updates` | 多个 handler 改 `systemPromptOptions` 后，后一个 handler 里读 `ctx.getSystemPrompt()` 得到最新渲染 |
| `resolves tool renderers in extension load order, each able to defer to the next` | resolver 链与 `next()` |
| `boundary chaining` / `chains shared draft proposals and preserves omitted result fields` | 草稿链 + 省略字段不变 |
| `boundary chaining` / `reports invalid boundary previews and lets later handlers repair the proposal` | buildContext 抛错 → 错误上报 + `valid` 反映最后一次 |
| `boundary chaining` / `keeps shared mutations made before a handler throws` | 原地改动保留 |
| `tool_result chaining` / `chains content modifications across handlers` | 内容替换链 |
| `tool_result chaining` / `preserves previous modifications when later handlers return partial patches` | 局部补丁不覆盖先前 |
| `provider registration` / `bindCore ignores invalid queued registrations and reports extension error` | 队列冲刷时抛错 → `emitError`，其它继续 |
| `provider registration` / `pre-bind unregister removes all queued registrations for a provider` | 两个队列都被过滤 |
| `provider registration` / `post-bind register and unregister take effect immediately` | 绑定后直接调 registry |
| `command context` / `passes fork options through to the bound handler` | `fork(entryId, options)` 透传 |
| `event subscriptions` / 5 个用例 | 自我移除不跳过邻居；重复注册独立移除；派发中移除仍参与本次；派发中新增延后到下次；嵌套派发用新快照 |
| `hasHandlers` / `returns true when handlers exist for event type` | |
| `before_provider_headers` / `lets a handler mutate headers in place and preserves existing headers` | 原地改 + 保留原 header |
| `before_provider_headers` / `isolates a throwing handler and still applies the others` | 错误隔离 |

### 7.3 `test/extensions-input-event.test.ts`（125 行，9 个用例）

| 用例 | 断言 |
|---|---|
| `returns continue when no handlers, undefined return, or explicit continue` | 三种情况都 `{action:"continue"}` |
| `transforms text and preserves images when omitted` | `{action:"transform", text:"T:hi", images: imgs}`（原引用） |
| `transforms and replaces images when provided` | 新图替换 |
| `chains transforms across multiple handlers` | `"X[1][2]"`，`images: undefined` |
| `short-circuits on handled and skips subsequent handlers` | 后续 handler 未运行（`globalThis.testVar` 仍 false） |
| `passes source correctly for all source types` | `interactive`/`rpc`/`extension` |
| `passes streamingBehavior correctly` | `steer`/`followUp`/undefined |
| `catches handler errors and continues` | 结果 `continue`，`errs` 含 `"boom"` |
| `hasHandlers returns correct value` | |

### 7.4 `test/interactive-mode-extension-dialogs.test.ts`（152 行，4 个用例）

用 `Proxy` 把 `InteractiveMode.prototype` 的方法绑到假上下文（`extensionDialogs`、`editorContainer`、`editor`、
`focused`、`ui.setFocus`、`programStatus.setBlocked`、`disposeActiveSelector`、`toggleToolOutputExpansion`）。

| 用例 | 断言 |
|---|---|
| `shows the earlier confirm again after the later one is answered`（#11166） | 后开的 "Second" 可见，`setBlocked` 最后一次为 `("extension-dialog", {kind:"permission", message:"Second"})`；Escape → `second === false`；随后 "First" 可见且 `setBlocked` 为 First；`"\r"` → `first === true`；焦点回到 editor；`setBlocked` 最后为 `("extension-dialog", undefined)` |
| `removes a hidden dialog when its signal aborts` | 隐藏对话框 abort → 立即 `false`，不影响栈顶；栈顶确认后焦点回 editor |
| `stacks concurrent inline custom components`（#9909） | 两个 `custom` 都创建；后开者可见；`done[1]("two")` → second resolve "two" 且 "custom one" 可见；`done[0]("one")` → first resolve "one"，焦点回 editor |
| `shows a confirm opened from an inline custom component and then restores the component` | custom 内 `handleInput` 打开 confirm；confirm 可见；Escape → false；custom 恢复可见；`done("kept")` → custom resolve "kept"；焦点回 editor |

共同断言辅助 `visible(context)`：`editorContainer.children` 恰好是 `[focused]`，且 `render(80).join("\n")` 含指定文本。

### 7.5 `test/suite/regressions/*`（扩展相关）

| 文件 | 断言要点 |
|---|---|
| `extension-factory-cache.test.ts`（4 用例） | 同 cwd 缓存命中时**不重跑模块，但重跑工厂**；`loadExtensions` 不缓存；resource loader reload 清缓存；缓存按 cwd 隔离 |
| `6260-inline-extension-naming.test.ts`（4 用例） | bare 工厂显示 `<inline:N>`（N 从 1）；命名显示 `<inline:name>`；命名工厂保留 `hidden`；混合顺序正确 |
| `8423-extension-factory-failure.test.ts`（2 用例，**没有** `test.failing`/`skip`） | 用例 1 `discards runtime changes and disables the failed API`：工厂抛错后 `runtime.flagValues` 不含 `failed-flag`、`pendingProviderRegistrations` 只剩并发成功工厂的 `working-provider`、加载期 `events.on` 订阅被退订（再 emit 不触发）、`capturedApi.registerFlag(...)` 抛 `Extension "<failing>" failed to load and its API is no longer active.`；加载期 `kiana.getFlag("failed-flag")` 仍读到 pending 默认值 `true`。用例 2 `does not discard a concurrently loaded factory's provider`：慢失败工厂与快成功工厂并发，失败方 `discard()` 不得清掉成功方已 commit 的 provider（按注册项而非整表清空）。**两个都是正常通过的断言，必须按断言复刻** |
| `9540-extension-loader-lazy.test.ts`（1 用例） | jiti/Babel 只在需要时加载（懒） |
| `8237-node-sea-extension-loading.test.ts`（1 用例） | SEA 场景用 `jiti/static` 分支 |
| `11156-kiana-ai-models-virtual-module.test.ts`（2 用例） | `kiana-ai/models` 在两种 scope 下都可解析 |
| `2860-replaced-session-context.test.ts`（3 用例） | `withSession` 重绑；目标为替换后的会话；旧 `kiana`/`ctx` 失效抛错；fork/switchSession 的 `withSession` |
| `6162-extension-active-tools-next-turn.test.ts`（3 用例） | `setActiveTools` 在同一 run 的下一个 provider 请求前生效；run 期间 `getSystemPrompt` 反映刷新；保留 `before_agent_start` 的 systemPrompt 覆盖 |
| `2835-tools-allowlist-filters-extension-tools.test.ts`（2 用例） | 白名单同时过滤内建与扩展工具；空白名单禁用全部 |
| `3592-no-builtin-tools-keeps-extension-tools.test.ts`（3 用例） | `no-builtin-tools` 保留扩展工具；`noTools: "all"` 全禁；经 services 传递 |
| `5109-exclude-tools.test.ts`（3 用例） | 排除同时作用于内建与扩展；排除优先于白名单；模式匹配 |
| `5080-signal-shutdown-extension-cleanup.test.ts`（9 个 `test` + 1 个 `test.each`×3 = 12 例） | `signal-triggered shutdown emits session_shutdown before terminal writes`；`interactive quit stops the TUI before emitting session_shutdown`；`interactive quit prints a resume hint for persisted sessions`；`signal-triggered shutdown does not print a resume hint`；`re-entrant shutdown is a no-op`；`test.each` 三例 `stdin read EIO / setRawMode EIO / setRawMode ENOTTY exits quietly without recording a crash`（exitCode 129，不调用 `recordCrash`、不 `ui.stop`）；`other stdin errors still crash`；`uncaught dead terminal errors are not recorded as crashes`；`other uncaught errors are still recorded as crashes`；`handlers are removed on unregister` |
| `5943-session-start-notify.test.ts`（7 用例） | `renders loaded resources before restored messages without stale entries`；`renders replacement session state before session_start handlers can notify`；`subscribes before replacement session_start handlers send messages`；`… send user messages`；`runs the reload render hook before reload session_start handlers can notify`；`refreshes hideThinkingBlock before rebuilding chat during reload`；`keeps the reload blocker focused until async reload completes` |
| `10285-mcp-tool-renderers.test.ts`（2 用例） | `renders calls to MCP tools that are not registered`；`renders them in HTML exports too` |
| `1717-2113-agent-session-event-settlement.test.ts`（2 用例） | `keeps persisted assistant/toolResult message order when extension message_end handlers yield`；`runs tool_call handlers after the assistant tool-use message is settled in the session` |
| `3982-message-end-cost-override.test.ts`（1 用例） | `allows extensions to replace finalized assistant usage cost` |
| `2753-reload-stale-resource-settings.test.ts`（1 用例） | reload 应用更新的顶层 prompt 设置 |
| `2023-queued-slash-command-followup.test.ts`（1 用例） | 扩展来源的排队 slash-command follow-up 被当作原始用户文本，不派发命令 |
| `5596-missing-theme-export.test.ts`（1 用例） | 配置主题缺失时用回退主题导出（与 `setTheme` 诊断相关） |

### 7.6 `test/suite/` 中与扩展语义直接相关的用例

| 文件 | 相关用例 |
|---|---|
| `agent-session-model-extension.test.ts` | `allows extension tool_call handlers to block tool execution`；`… tool_result handlers to modify tool results`；`… context handlers to modify messages before the LLM call`；`… input handlers to transform or handle input`；`… extension commands to inspect live system prompt options`；`… before_agent_start handlers to inject custom messages and modify the system prompt`；`bindExtensions emits session_start and reload emits session_shutdown then session_start` |
| `agent-session-boundaries.test.ts`（26 个测试块，展开 29 例） | core：`commits a retain-none turn_end compaction and explicitly continues once`、`continues from an agent_before_settle custom message before final settlement`、`keeps a boundary replacement verbatim through threshold compaction`、`dispatches actionable turn_end for synthetic run failures`、`does not let an invalid explicit continuation suppress natural tool continuation`、`commits pre-settlement drafts but suppresses continuation when aborted during settle` |
| `agent-session-tool-orchestration.test.ts` | `supports tools that call other tools under any name through the extension API`；`registers codemode and tool_search inactive until they are named`；`leaves results without nested calls unchanged` |
| `virtual-models.test.ts`（13 用例） | `routes each request, including retries, while the selection stays virtual`；`routes requests after extension messages as continuations`；`ends the run with an error response when routing fails and keeps the last physical limits`；`stores router state on the branch and passes it to later requests`；`does not route compactions that an extension supplies` 等 |
| `agent-session-runtime.test.ts` / `agent-session-queue.test.ts` | 事件顺序与消息排队（间接） |
| `mcp-extension.test.ts`（第一组 "MCP config"） | `rejects server names that differ only in - and _`（`mcpNamespace` 冲突）；`validates exposure and reads autoEnableCodemode with project precedence`；`resolves per-tool exposure from exact names, then patterns in order` |
| `llama-extension.test.ts` | `registers a native provider and /llama command`（`registerProvider(provider)` 原生重载） |

### 7.7 兼容语料测试（建议新增）

对 `examples/extensions/**` 每个文件：
1. **加载成功**（`errors` 为空，或已知的“需要宿主能力缺失”可跳过并记录）；
2. 注册项数量与 §3.11 矩阵一致（tools/commands/flags/handlers/renderers 的 key 集合）；
3. 关键行为（选 20 个代表）：`hello.ts`、`permission-gate.ts`、`input-transform.ts`、`input-transform-streaming.ts`、
   `prompt-customizer.ts`、`provider-payload.ts`、`dynamic-tools.ts`、`todo.ts`、`tools.ts`、`preset.ts`、
   `custom-compaction.ts`、`trigger-compact.ts`、`reload-runtime.ts`、`event-bus.ts`、`session-name.ts`、
   `widget-placement.ts`、`working-indicator.ts`、`hidden-thinking-label.ts`、`timed-confirm.ts`、
   `overlay-test.ts`、`rpc-demo.ts`、`jev-router.ts`、`structured-output.ts`、`truncated-tool.ts`、
   `custom-provider-anthropic/`、`plan-mode/`、`subagent/`。

---

## 8. Rust 模块布局与最难复刻之处

### 8.1 crate 划分

建议新建独立 crate `kiana-extensions`（位于 `/home/ctyun/code/kiana_rev/kiana/crates/kiana-extensions/`）——`ARCHITECTURE.md` §3.7 已采纳；依赖按骨架为 `kiana-agent-core`、`kiana-ai`、`kiana-config`、`kiana-core`（source-info、diagnostics、event-bus、timings、exec、paths、kiana-manifest）、`kiana-ext-api`、`kiana-session-core`、`kiana-theme`、`kiana-tools`、`kiana-tui`。注意：

- **公开类型契约在 `kiana-ext-api`**（`core/extensions/types.ts`，L7），本 crate 只实现 loader/runner/wrapper/virtual-modules/engine。本节 `types/` 子树里的公开类型（`LoadExtensionsResult`、`InlineExtension`、`ExtensionRuntime`、`ToolDefinition` 等）**不在本 crate 定义**，由 `kiana_ext_api::{types,tool_types,events,ui}` 定义后 re-export。
- 骨架里**没有** `kiana-session`（L11）依赖——`kiana-session-core`（L4）已提供 `SessionEntry`/`UsageEntry` 等；`kiana-resources`（L10）也不能被本 crate 依赖（会成环）。
- `durable-harness` 只是 `spec/durable-harness.md` 的**单元名**（对应 `packages/durable` → `kiana-durable` crate），与 `core/extensions/**` 无关；§0.3 的旧说法以此为准。

若团队最终决定把 `core/extensions` 并入 `kiana-durable` crate（本 spec **不推荐**：`kiana-durable` 是 L1，无法依赖 `kiana-config`/`kiana-theme`/`kiana-tools`），则 §8 的模块布局应作为该 crate 的
`extensions` 子模块落地，接口与本文一致；但 §0.3 的“`durable-harness` 拥有 `core/extensions/**`”说法**作废**。

```
kiana-extensions/
├── Cargo.toml
└── src/
    ├── lib.rs                 // 重导出面（对应 index.ts）
    ├── types/
    │   ├── mod.rs             // 所有事件/结果/工具/上下文类型 + 守卫
    │   ├── ui.rs              // ExtensionUiContext 及相关
    │   ├── context.rs         // ExtensionContext / ExtensionToolContext / ExtensionCommandContext / ReplacedSessionContext
    │   ├── tools.rs           // ToolDefinition、ToolLoadout、ToolExposure、ToolNamespace、ToolRenderers、define_tool
    │   ├── events.rs          // 事件结构体与结果类型（按 10 组）
    │   ├── api.rs             // ExtensionApi trait、ExtensionRuntime/Registry、flag/shortcut/command 注册项
    │   ├── mcp.rs             // McpServerConfig 导入面 + McpServerRegistry 接口（实现在 telemetry-mcp）
    │   └── guards.rs          // is_tool_call_event / variant 判别 + 类型守卫
    ├── loader/
    │   ├── mod.rs             // 公开函数：create_extension_runtime / load_extension / load_extensions / load_extensions_cached / discover_and_load_extensions / clear_extension_cache / use_extension_cache_cwd / is_current_cache_token
    │   ├── factory_cache.rs   // 缓存键 (path, cwd, generation)
    │   ├── api.rs             // create_extension_api（事务性待定状态机）
    │   ├── discovery.rs       // resolve_extension_entries / discover_extensions_in_dir / manifest 解析
    │   ├── runtime.rs         // 三个 jiti 分支的策略选择（见 §8.3）
    │   └── js_host.rs         // rquickjs 宿主：virtual modules、require/import 解析、错误转换
    ├── runner/
    │   ├── mod.rs             // ExtensionRunner、bind_core / bind_command_context / set_ui_context、invalidate
    │   ├── dispatch.rs        // 泛型 emit 及 12 个专用派发器（emitBoundary…emitInput）
    │   ├── boundary.rs        // emit_boundary（最复杂）
    │   ├── context.rs         // create_context / create_tool_context / create_command_context
    │   ├── ui_prompt.rs       // with_ui_prompt / emit_ui_prompt_event
    │   ├── keybindings.rs     // 18 个保留 id + build_builtin_keybindings
    │   ├── renderers.rs       // resolve_tool_renderers / message / entry / markdown
    │   ├── commands.rs        // invocation_name 去重、冲突、resolve_registered_commands
    │   ├── mcp_registration.rs// registerMcpServer 校验 + 所有权
    │   └── noop_ui.rs         // no_op_ui_context
    ├── wrapper.rs             // wrap_tool_definition
    ├── virtual_modules.rs     // 常量表（22 个键）
    ├── adapters/
    │   ├── mod.rs
    │   ├── interactive.rs     // 交互式 UI 上下文（对话框栈、widget、footer/header、快捷键、终端输入、工作指示器、自定义编辑器）
    │   ├── rpc.rs             // RPC UI 子协议（dialog promise 表、request/response 编解码）
    │   ├── print_json.rs      // print/json（全部 no-op / 默认值）
    │   └── errors.rs          // showExtensionError 的 3 个 sink
    ├── resources.rs           // ResourceLoader 契约（inline/builtin/omitReplaced/冲突/warnings/flag 应用/路径构建）
    ├── builtin.rs             // builtInExtensions 清单
    └── tests/
        ├── discovery.rs
        ├── runner.rs
        ├── input_event.rs
        ├── dialogs.rs
        └── corpus.rs          // examples/extensions 语料
```

### 8.2 关键类型（骨架）

```rust
// ---------- 身份与注册项 ----------
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceInfo {
    pub path: String,          // "builtin:llama.cpp" / "<inline:1>" / "<sdk:foo>" / 绝对路径
    pub source: String,        // TS 里是普通 string："local" | "inline" | "builtin" | "sdk" | "cli" | "npm:..." | 包来源
    pub scope: SourceScope,    // "user" | "project" | "temporary"（source-info.ts 的 SourceScope）
    pub origin: SourceOrigin,  // "package" | "top-level"——**只有这两值**
    pub base_dir: Option<PathBuf>,
}

pub struct RegisteredTool { pub definition: ToolDefinition, pub source_info: SourceInfo }
pub struct ExtensionFlag { pub name: String, pub type_: FlagType, pub description: Option<String>, pub default: Option<FlagValue>, pub extension_path: String }
pub struct ExtensionShortcut { pub key: String, pub description: Option<String>, pub handler: ShortcutHandler, pub extension_path: String }
pub struct RegisteredCommand { pub name: String, pub source_info: SourceInfo, pub description: Option<String>, pub get_argument_completions: Option<ArgumentCompletionsFn>, pub handler: CommandHandler }

// ---------- 扩展对象 ----------
#[derive(Default)]
pub struct LoadedExtension {
    pub path: String,                      // 用于错误报告的 display path（可能是 builtin:/<inline:N>）
    pub resolved_path: String,
    pub source_info: SourceInfo,
    pub handlers: HashMap<EventKind, Vec<HandlerEntry>>,   // 顺序敏感 → IndexMap<EventKind, Vec<_>>
    pub tools: IndexMap<String, ToolDefinition>,
    pub message_renderers: IndexMap<String, MessageRenderer>,
    pub tool_renderers: Option<Vec<ToolRendererResolver>>,
    pub markdown_transformer: Option<MarkdownTransformer>,
    pub entry_renderers: Option<IndexMap<String, EntryRenderer>>,
    pub commands: IndexMap<String, RegisteredCommand>,
    pub flags: IndexMap<String, ExtensionFlag>,
    pub shortcuts: IndexMap<String, ExtensionShortcut>,
    pub hidden: bool,
}

// ---------- 派发用统一结果 ----------
pub enum EmitOutcome<T> { Ok(T), Err(ExtensionError) }
pub struct ExtensionError { pub extension_path: String, pub event: &'static str, pub error: String /* JS 异常原始值或 message */ }

// ---------- UI ----------
pub trait ExtensionUiContext: Send + Sync {
    fn select(&self, title: &str, options: &[String], opts: &DialogOptions) -> BoxFuture<'_, Option<String>>;
    fn confirm(&self, title: &str, message: &str, opts: &DialogOptions) -> BoxFuture<'_, bool>;
    fn input(&self, title: &str, placeholder: Option<&str>, opts: &DialogOptions) -> BoxFuture<'_, Option<String>>;
    fn notify(&self, message: &str, kind: Option<NotifyKind>);           // "info" | "warning" | "error"
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe;
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn set_widget(&self, key: &str, content: WidgetContent, opts: Option<ExtensionWidgetOptions>);
    fn set_footer(&self, factory: Option<FooterFactory>);
    fn set_header(&self, factory: Option<HeaderFactory>);
    fn set_title(&self, title: &str);
    fn custom<T: 'static>(&self, factory: CustomFactory<T>, opts: Option<CustomOptions>) -> BoxFuture<'_, T>;
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
    fn editor(&self, title: &str, prefill: Option<&str>) -> BoxFuture<'_, Option<String>>;   // 无 timeout/signal；RPC 的 editor 请求也不带 timeout
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);
    fn set_editor_component(&self, factory: Option<EditorFactory>);
    fn get_editor_component(&self) -> Option<EditorFactory>;
    fn theme(&self) -> &Theme;                                          // 只读属性
    fn get_all_themes(&self) -> Vec<ThemeInfo>;                         // { name, path: Option<PathBuf> }
    fn get_theme(&self, name: &str) -> Option<Theme>;
    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult;             // 无 UI: { success:false, error:"UI not available" }
    fn get_tools_expanded(&self) -> bool;
    fn set_tools_expanded(&self, expanded: bool);

    // 注意：TS 的 ExtensionUIContext **没有**下列方法，勿加：
    //   editorBorderColor / setAutocompleteProvider / setWorkingIndicator 之外的 setWorking* 变体
    //   （`setWorkingVisible` 存在；`setAutocompleteProvider` 只作为**编辑器实例**的方法出现，见 §2.8.1）
}

// ---------- 上下文 ----------
/// 严格对应 TS `ExtensionContext` / `ExtensionToolContext` / `ExtensionCommandContext` 的三个 trait
/// 已在 §5.2 给出**权威签名**（本节不重复，以免两处漂移）。要点复述：
/// - `ExtensionContextApi` 只有 TS 里的那些成员：`ui`/`mode`/`hasUI`/`cwd`/`sessionManager`/`modelRegistry`/
///   `model`/`scopedModels`/`thinkingLevel`/`isIdle`/`isProjectTrusted`/`signal`/`abort`/`hasPendingMessages`/
///   `shutdown`/`getContextUsage`/`compact`/`getSystemPrompt`。**没有** `getActiveTools`/`setActiveTools`/
///   `getAllTools`/`getSystemPromptOptions`/`refreshSystemPrompt`——那些在 `kiana.*`（`ExtensionAPI`）或
///   `ExtensionCommandContext` 上。
/// - `ExtensionToolContextApi: ExtensionContextApi` 增加 `tools`（读时检查）与 `executeTool`。
/// - `ExtensionCommandContextApi: ExtensionContextApi` 增加 `getSystemPromptOptions`/`waitForIdle`/`newSession`/
///   `fork`/`navigateTree`/`switchSession`/`reload`。
```

**关键设计约束**

1. **上下文必须是 trait，且每个方法在调用时做 stale 检查**（`assertActive`）。不能把字段拷贝进结构体：`ctx.signal`、`ctx.model`、`ctx.scopedModels`、`ctx.getSystemPrompt()` 都是读时求值（`Object.defineProperties` 的 getter）。Rust 用 `&self` 方法 + 内部 `Arc<dyn Host>`。
2. **`entries` 是共享可变状态**：`tool_result` 链中多个 handler 看到的 `event.entries` 是同一数组（原地改写可见）。Rust 用 `Arc<Mutex<Vec<Entry>>>`，并保证 handler 之间看到彼此的改动且在 handler 抛错时保留改动（§2.3.14 `emitToolResult`）。
3. **`AgentMessage` 的身份语义**：`sameMessages` 用引用相等 → Rust 用 `Arc<dyn AgentMessage>` + `Arc::ptr_eq`。
4. **handler 顺序**：所有 `Vec<Handler>` 保插入顺序（`IndexMap`/`Vec`），不能是 `HashMap`。
5. **`structuredContent` 的删除**：`ToolResultEventResult` 里“存在但为 undefined”表示删除。Rust 用 `enum OptionalPatch<T> { Absent, Undefined, Value(T) }`。
6. **错误绝不上抛给宿主**（除 `emitUserBash`）：每个 handler 包在 `catch` 里，`emit_error` 后继续。

### 8.3 扩展执行引擎（最难）

TS 用 jiti 在**同进程内**把 `.ts`/`.js` 转成 CJS 并 `require`，且：
- `moduleCache: false` → 每次 `import()` 都重新执行模块；
- `virtualModules: {...}` → `@earendil-works/kiana-*` 等解析到**宿主注入的对象**；
- `alias: getAliases()` → `react`、`zod` 等别名（见 §2.2.1 的 22 项）；
- `tryNative: false`（embedded）与 `tsconfigPaths: true`（TS 源）。

Rust 实现只有三条现实路线：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A. rquickjs + 自建转换** | `rquickjs`（QuickJS）作为 JS 引擎；`.ts` 用 `oxc`/`swc` 去类型后交给引擎；`virtualModules` 用 `ModuleLoader` 自定义 `resolve`/`load` 返回宿主对象的 JS 包装；`require` 语义手工实现 | 单二进制、无外部进程 | TS 转换必须对齐 jiti（装饰器、`enum`、`import type` 消除、`?`/`!` 语法、`const enum`、`import.meta`）；模块解析必须复刻 Node ESM + CJS interop；性能与 GC 隔离需评估 |
| **B. 内嵌 Node/Bun 子进程** | 子进程跑真 jiti + 真 Node 模块解析；Rust 通过 IPC（JSON 行）代理所有 API 调用 | 兼容性最高（几乎零差异） | 与“纯 Rust 单二进制”目标冲突；IPC 往返使同步属性（`ctx.cwd`、`ctx.hasUI`）需要预取或全异步化；RPC/TUI 的同步渲染回调难代理 |
| **C. quickjs-wasi**（与 TS 依赖同款引擎） | 用 `quickjs-wasi` crate，运行时加载 `.wasm` | 与 kiana 已用的 QuickJS 一致（`quickjs-wasi@3.6.2` 是某 builtin 扩展的运行环境） | WASI 下无 `node:fs` 等；仍需自己实现模块解析 |

**推荐 A**，并把“Node 内建模块”以最小 shim 注入：`node:fs`（同步读/写/readdir/stat/exists）、`node:path`（posix+win32）、`node:os`、`node:url`、`node:events`、`node:child_process`（`spawn` 异步）、`node:crypto`（`randomUUID`）、`process`（`env`、`cwd`、`platform`、`argv`、`stdout/stderr` 写）、`Buffer`/`TextEncoder`/`TextDecoder`、`setTimeout/clearTimeout/setInterval/queueMicrotask`、`structuredClone`、`URL`。这些 shim 必须**逐条**与 Node 行为对齐（尤其 `fs.readdirSync` 排序、`path.resolve` 的 `..` 归一化、`Buffer.toString` 的 UTF-8 替换字符）。

`exports` 条件解析：jiti 用 `node`、`import`、`require`、`default`；package.json `exports` 中的 `types`/`browser` 在本单元不使用。

### 8.4 事务性 API（`createExtensionApi`）

状态机三态：`Loading` → `Active` / `Failed`。要点：
- 加载期间 `registerMcpServer`、`registerProvider`、`registerVirtualModel`、`unregister*` 只进 `pendingRuntimeChanges`；`registerTool/Command/Shortcut` 立即写入（失败可通过 `discard` 撤销）。
- `trackEventBusSubscription`：加载期间的订阅记入 `loadingUnsubscribers`，`discard` 时全部退订。
- `commit()` 后 `pendingFlagValues` 应用到 binding。
- 任何方法在 `Failed` 状态抛 `Extension "<path>" failed to load and its API is no longer active`（逐字见 §2.2.4）。
- `getFlag(name)`：优先 `pendingFlagValues`，其次已应用值；未注册返回 `undefined`。

Rust 表达：`ExtensionApiImpl { state: Arc<RwLock<ApiState>> }`，`ApiState { status: Loading|Active|Failed, pending_flag_values, pending_runtime_changes, loading_unsubscribers, ... }`；`commit/discard` 用 `take()` 保证幂等。**注意**：`registerTool` 在加载期直接进入 `extension.tools`，所以 `Extension` 对象也须共享可变（`Arc<Mutex<LoadedExtension>>`），且 `loadExtension` 在工厂返回后读取它。

### 8.5 事件派发与并发模型

- 全部 handler 调用是 **async**（可 await），但派发本身在调用者的任务里**串行**执行（`for handler in snapshot { handler(...).await }`）。
- `emit()` 的通用语义：遍历所有扩展的所有该事件 handler；每个 handler 的返回值合并到共享 `result`（语义每事件不同，见 §2.3.14）；单个抛错 → `emit_error` 继续。
- `ui_prompt_start` / `ui_prompt_end` 用 `queueMicrotask` 延后到当前微任务队列末尾发出，因此**不在** `withUIPrompt` 的同步区间内。Rust 无微任务队列：用 `tokio::task::yield_now()` 后再 emit（顺序近似），或在 `with_ui_prompt` 返回后 `spawn` 一个立即执行的 task。测试（如果有）需容许“start 与 prompt 的实际显示之间无强顺序”。
- `emitUserBash` 是唯一**向上抛**的：任一 handler 抛错 → 整个 `emit_user_bash` 返回 `Err`，调用方（bash 执行器）按 fail-closed 处理。
- `emitToolCall` 无 try/catch（说明错误已在上层处理），其它派发器都有。
- 重入：`emit` 期间允许 handler 触发另一次 `emit`；每次派发用**快照**（`snapshotEventHandlers`），嵌套派发重新取快照。

### 8.6 最难复刻之处（按难度排序）

1. **jiti 模块加载等价性**（§8.3）：TS 语法覆盖（装饰器、`enum`、`namespace`、参数属性、`import type`、`satisfies`、`as const`、`?.`/`??` 已由引擎支持）、CJS/ESM 互操作（`export default` 与 `module.exports`、`__esModule`）、Node 模块解析（`exports` 条件、`main`、深路径、`node_modules` 逐级、symlink 保留 vs 解析）、`moduleCache:false` 每次重新执行、virtual module 的**对象身份**（同一进程内多次 import 必须拿到同一个 `Type` 对象）。
2. **virtual module 的完整 API 面**（22 个键）：`kiana-ai` 的 compat 入口（`getModel`、`streamSimple`、`complete`、OAuth `login`/`refresh`、`getModels`、`getProviders`）、`kiana-tui` 的组件与键位工具、`kiana-coding-agent` 的 SDK（`defineTool`、全部类型守卫、`CustomEditor` 类）——这些在 JS 侧必须是**真 JS 对象/函数**，不能是“序列化后的数据”。
3. **12 个专用派发器的差异化合并语义**（§2.3.14；另有泛型 `emit` 与 `emitError`）：`before_agent_start` 的 systemPrompt 链（后 handler 读 `ctx.getSystemPrompt()` 需看到前一个的改动）、`context` 的两阶段（先全部收集再应用）、`tool_call` 的 `terminate` 短路、`tool_result` 的补丁链与 `structuredContent` 删除、`input` 的 transform 链与 `handled` 短路、`session_before_*` 的默认值、`before_provider_headers` 的原地改、`resources_discover` 的数组拼接。
4. **Boundary 草稿链**（§2.3.11 最复杂的一段）：`turn_end`/`agent_before_settle` 的 `custom`/`custom_message`/`context_edit`/`compaction` 四类草稿；共享可变草稿对象；`valid` 标志由 `buildContext` 抛错决定并可由后续 handler 修复；省略字段不变；`CompactionEntryDraft.firstKeptEntryId` 的类型是 `string | null`——`null` 表示“自保持压缩，不保留任何前置条目”，与省略该字段（字段不变）语义不同。**没有** `retain` 字段。
5. **UI 对话框栈与焦点恢复**（§2.8.1）：编辑器槽位内多层对话框；隐藏对话框的 signal/unsubscribe 立即出栈；可见的在顶层；resolve 后 `setBlocked` 恢复与 `editor.focused()`；`custom` 内的嵌套对话框；`render(80)` 的精确布局（`→ ` 前缀、`(esc to cancel)`、倒计时 `(5s)` 递减、宽度截断）。
6. **RPC UI 子协议**：`extension_ui_request`/`response` 的字段与 `timeout`/`autoResolve` 语义；`custom` 在 RPC 下返回 `undefined`；`setTheme` 返回 `{success:false,error:"Theme switching not supported in RPC mode"}`；无响应时按 `timeout` 到点自动解析（默认值见 §2.8.2）。
7. **`invocationName` 去重与冲突诊断**（§2.3.9）：`name`、`name:1`、`name:2`……与保留的 24 个内建命令名冲突；短横线/大小写敏感；`getShortcuts` 的保留键（18 个）与“保留项胜出”算法。
8. **预览/预览草稿的“无效”上报**：`buildContext` 抛错 → `emitError` 但继续；`valid` 最终值由最后一次尝试决定；错误消息文本与 `extensionPath`。
9. **时间与 `signal`**：`AbortSignal` 语义（`aborted` 之后的 `addEventListener` 立即触发、`reason`、组合信号）。Rust 用 `tokio_util::sync::CancellationToken` + 子令牌；但 `AbortSignal` 的 `throwIfAborted`、`any()` 组合需在 JS shim 层实现。
10. **`ui_prompt_*` 的微任务时序** 与 `hasHandlers` 的精确语义（只看当前已注册、包含 loading 期尚未 commit 的？——见 §2.3.9）。
11. **JSON 键顺序与 `undefined` 语义**（§6.3）：事件对象往返 JS↔Rust 时如果走 serde，会丢失“键存在但为 undefined”；推荐事件对象**在 JS 侧构造并原地传递**（rquickjs `Object` 句柄），Rust 只在需要时读取；序列化只用于持久化（session entries）。
12. **`source_info` 的合成路径**与错误报告里的显示路径（§2.8.4 / §4.6）：`builtin:foo`、`<inline:1>`（**1 基**；`loadExtensionFactories` 用 `index + 1`，不存在 `<inline:0>`）、`<inline:name>`、`<sdk:name>`、包内 `node_modules/.../extensions/x.ts` 的截断规则。
13. **工厂缓存**（§2.2.2）：键是 `(resolvedPath, cwd, generation)`；命中时**不重跑模块但重跑工厂**（因此工厂里的闭包状态每次新）；`clearExtensionCache()` 与 `useExtensionCacheCwd(cwd)`；stale token 判定。
14. **MCP 注册的命名空间冲突**：`mcp__` + `-`→`_`；同名不同来源的所有权检查；变更监听 → `mcp_servers_change` + `reportUnhandledMcpServers`。
15. **`wrapRegisteredTool` 的适配**：把 `ToolDefinition` 适配为 agent-core 的 `AgentTool`（`execute(toolCallId, params, signal, onUpdate, ctx)` → `execute(...)`，`renderCall`/`renderResult` 保留），以及 `ToolRenderContext` 的 14 个字段（`args`、`toolCallId`、`invalidate`、`lastComponent`、`state`、`cwd`、
`executionStarted`、`argsComplete`、`isPartial`、`expanded`、`showImages`、`isError`、`durationMs`、`outputPad`）全部正确填充。
16. **`getOwnPropertyDescriptors` 的只读 getter**（`createCommandContext`）：命令上下文里的某些字段是 **getter**（读时求值），不是快照；Rust trait 方法天然如此，但需确保每次读取都反映最新状态。

### 8.7 实施顺序建议

1. `types/`（纯数据 + trait），对照 §2.1 与 §5.2 逐项检查。
2. `runner/`（除 boundary/UI prompt）+ `wrapper.rs` + `noop_ui.rs`，跑 §7.2/§7.3 的大部分。
3. `boundary.rs` + UI prompt，跑 §7.6 的 boundary 测试。
4. `loader/`（先只支持 JS，用 rquickjs；再补 TS 转换），跑 §7.1。
5. `adapters/rpc.rs`（相对独立，跑 RPC 模式测试）。
6. `adapters/interactive.rs`（依赖 `kiana-tui`，跑 §7.4）。
7. `resources.rs` + `builtin.rs` + `mcp_registration.rs`。
8. `tests/corpus.rs`：examples 全量加载 + 矩阵比对。

### 8.8 验收清单摘要（按节索引）

实现完成后逐条打勾；每条后面给的是“验证方式”。

**A. 类型与导出面**
- [ ] `kiana_extensions` 公开面与 TS `src/core/extensions/index.ts` 的 **193** 个名字一一对应（§2.4 / §5.2）。验证：脚本比对名字集合。
- [ ] `types.ts` 的 **186** 个导出全部落地（§2.1）。验证：脚本比对。
- [ ] 41 个事件名、`ExtensionAPI` 的 31 个方法名 + `events` 字段、`ExtensionUIContext` 的 28 个成员（27 方法 + 只读属性 `theme`；`setWidget` 有两个重载，故接口里共 29 条声明）、`ExtensionContext` 的 18 个成员（§2.1.2 / §2.1.3 / §2.1.13）。

**B. 加载器**
- [ ] 三种 jiti 分支的选择条件（§2.2.1）。
- [ ] `getAliases()` 22 项别名逐字一致（§2.2.1）。
- [ ] 工厂缓存键 `(resolvedPath, cwd, generation)`；命中不重跑模块但重跑工厂（§2.2.2 / §7.5）。
- [ ] 目录发现规则：直接文件、`index.ts` 优先于 `index.js`、`package.json` 的 `kiana.extensions` 优先、`~` 不做展开、不递归超过一层（§2.2.8 / §7.1）。
- [ ] 事务性 API 三态与 4 个接收点；失败后错误文案逐字（§2.2.4 / §2.3.4 / §7.5 `8423`）。
- [ ] 合成路径命名 `<inline:N>` / `<inline:name>` / `<sdk:name>` / `builtin:`（§4.6 / §7.5 `6260`）。

**C. Runner 派发**
- [ ] 12 个专用派发器的合并语义逐个单测（§2.3.14）。
- [ ] `emit_boundary` 的草稿链 + `valid` 修复 + 抛错保留原地改动（§2.3.11 / §7.2）。
- [ ] `emit_tool_result` 的“内容替换时删除 structuredContent”规则（§2.3.14）。
- [ ] `emit_message_end` 的同角色规则与错误文案（§2.3.14）。
- [ ] `emit_user_bash` 的 XOR + `exitCode` 键存在性 + fail-closed（§2.3.3 / §7.2）。
- [ ] 事件快照语义 5 例（§2.3.5 / §7.2 `event subscriptions`）。
- [ ] `invocationName` 去重与 3 条冲突文案（§2.3.9 / §3.2 / §3.3）。
- [ ] `ui_prompt_start/end` 的微任务延后（§2.3.8）。

**D. 适配层**
- [ ] TUI 对话框栈 4 个用例（§7.4）。
- [ ] widget 截断 `... (widget truncated)`、`MAX_WIDGET_LINES = 10`、`placement` 默认 `aboveEditor`（§2.8.1 / §3.9）。
- [ ] RPC 9 个请求方法 + 3 种响应 + `extension_error`（§2.8.2 / §4.3）。
- [ ] print/json 全部 no-op 与默认返回值（§2.3.6 / §2.8.3）。
- [ ] 三种错误 sink 的逐字文案（§2.8.4 / §3.8）。

**E. 外部格式**
- [ ] 磁盘布局、`package.json` 的 `kiana` manifest、env 变量（§4.1 / §4.2 / §4.7）。
- [ ] MCP 配置校验的 28 条错误串与 `mcp__` 命名空间（§4.4）。
- [ ] `nestedCalls` 的 8 条规则与限值（§5.4）。
- [ ] EventBus 的同步 emit + 异步 handler 与错误前缀（§4.8）。

**F. 兼容语料**
- [ ] 70 个单文件 + 9 个目录示例全部可加载（§3.11 / §7.7）。
- [ ] API 使用矩阵的每一格都能被对应示例触发（§3.11）。

**G. 测试移植**
- [ ] §7.1（31 例）、§7.2（56 例）、§7.3（9 例）、§7.4（4 例）全部有 Rust 对应。
- [ ] §7.5 表列出的 19 个回归文件、§7.6 的 suite 用例，以及 §B.6 补遗的 19 个回归文件、§B.6.2 表中的 12 个顶层测试文件与 8 个（`test/` 或 `test/suite/`）文件（后者的断言点可能超出 §7.6 已有描述）按行为移植（允许按 Rust 架构调整 setup，但断言不变）。

---

## 附录 A：本规范的核对方法

- **字符串**：所有标 **【逐字】** 的文本均取自源码原文；Rust 测试用 `assert_eq!` 逐字节比对（含空白与标点）。
- **计数**：导出数、事件数、示例数、测试数均已在写作时用脚本核对（见 §2.4 的导出计数、§7 的用例数、§3.11 的示例数）。
- **行号**：文中引用的 `loader.ts:200` 之类行号对应当前只读树；若上游更新导致偏移，以**函数名**为准。
- **禁止**：不得修改 `/home/ctyun/code/kiana_rev/kiana` 下任何文件；本规范只写 `/home/ctyun/code/kiana_rev/kiana/spec/`。

---

## 附录 B：审计补遗（Audit additions）

> 说明：正文（§1–§8）中与源码不符之处已在**原处**直接改正；本附录只补充其余**遗漏**与跨节精确化说明。除非另外注明，引号内文本均为【逐字】。写作方式：先独立通读 `packages/coding-agent/src/core/extensions/**`、`src/core/resource-loader.ts`、`src/core/agent-session*.ts`、`src/core/agent-session-services.ts`、`src/core/project-trust.ts`、`src/core/trust-manager.ts`、`src/modes/{interactive,rpc,print-mode,json}*`、`docs/extensions.md`、`docs/rpc-extension-ui.md`、`docs/tui.md`、`examples/extensions/**` 与相关测试，再与正文逐条对照。
>
> 本附录共 9 个小节、63 条；与正文冲突时以本附录为准（正文已同步修正的除外）。
>
> **已在正文原处改正的条目**（跨多次审计累计，供 diff 复核）：`<inline:0>`→`<inline:1>`（1 基）、`agent-session-boundaries` 用例数、`5080` 的 9+1×3 例与用例名、G 项“19 个回归文件”与 `§A.6` 悬空引用（现指向 §B.6）、`ExtensionAPI` 的 31 方法/32 声明口径、`ExtensionUIContext` 的 29→28 成员（27 方法+`theme`）、`Extension` 的 14 字段/9 容器说明、`getCommands` 行补全、`SourceInfo`/`ExtensionFlag`/`ExtensionShortcut`/`RegisteredCommand` 结构、`ToolRenderContext` 14 字段、`tui.md` 行数、`builtin:` 命令描述（24 个，含 `quit`）、移除 `compaction.retain`、合并为单一 `ExtensionMode` 枚举、两处 `editor` 声明去掉 dialog opts、`8423` 无 `test.failing`、`session_compact_failed`/`cache_warming_decision` 的补充、`sendMessage`/`sendUserMessage` 签名、`registerToolRenderer` 链的“同扩展优先”错误描述。
>
> **禁止**修改 `/home/ctyun/code/kiana_rev/kiana`。

### B.1 绑定对象 `ExtensionBindings` 与 `bindExtensions` 全流程

**[B.1.1]** `ExtensionBindings`（`src/core/agent-session.ts:302`，**只从该文件导出**；包根 `src/index.ts` 与 `core/extensions/index.ts` 都**没有**再导出它）：

```ts
export interface ExtensionBindings {
  uiContext?: ExtensionUIContext;
  mode?: ExtensionMode;
  commandContextActions?: ExtensionCommandContextActions;
  abortHandler?: () => void;
  shutdownHandler?: ShutdownHandler;
  onError?: ExtensionErrorListener;
}
```

`AgentSession.bindExtensions(bindings)` 只对**非 undefined** 字段赋值（重复 bind 不会清掉先前值），随后：

```
_applyExtensionBindings(runner)
await runner.emit(session_start)
runner.reportUnhandledMcpServers()
await extendResourcesFromExtensions(session_start.reason === "reload" ? "reload" : "startup")
```

`_applyExtensionBindings(runner)`（reload 重建 runner 时也复用同一函数）：

```
runner.setUIContext(this._extensionUIContext, this._extensionMode)
runner.bindCommandContext(this._extensionCommandContextActions)
this._extensionErrorUnsubscriber?.()
this._extensionErrorUnsubscriber = this._extensionErrorListener ? runner.onError(this._extensionErrorListener) : undefined
```

顺序要点：先 `session_start`，再 `reportUnhandledMcpServers()`，最后 `resources_discover`（`extendResourcesFromExtensions` 在无该 handler 时直接返回；有贡献时 `resourceLoader.extendResources(...)` + `_rebuildSystemPrompt(activeToolNames)`，`reason` 为 `"reload"` 或 `"startup"`）。错误监听器每次重绑都先退订旧的，避免重复。

**[B.1.2]** `ctx.abort()` 不总是落到底层 `session.abort()`：

```
if (this._extensionAbortHandler) { this._extensionAbortHandler(); return }
void this.abort()
```

交互式提供的 `abortHandler` 是 `this.restoreQueuedMessagesToEditor({ abort: true })`（先把 steering + followUp 队列并回编辑器文本，再 abort）。RPC / print / json 都**不提供**，所以走 `void session.abort()`。`ctx.shutdown()` 走 `this._extensionShutdownHandler?.()`：交互式为 `shutdownRequested = true; if (this.session.isIdle) void this.shutdown()`；RPC 只置 `shutdownRequested = true`；print/json 无 handler → Runner 默认 `shutdownHandler = () => {}`。

**[B.1.3]** `AgentSession.reload()` 的“是否重新广播”守卫只看 **4** 个绑定（**不含** `abortHandler`）：

```
hasBindings = _extensionUIContext || _extensionCommandContextActions || _extensionShutdownHandler || _extensionErrorListener
if (hasBindings) { await options?.beforeSessionStart?.(); emit(session_start{reason:"reload"}); reportUnhandledMcpServers(); extendResourcesFromExtensions("reload") }
```

reload 顺序：`emitSessionShutdownEvent(oldRunner, {type:"session_shutdown", reason:"reload"})` → `oldRunner.invalidate()` → `settingsManager.reload()` → `syncQueueModesFromSettings()` → `resetApiProviders()` → `resourceLoader.reload()` → `_buildRuntime({ activeToolNames: [...当前 active, ...新增 defaultTools], flagValues: 旧 runner 的 getFlagValues(), includeAllExtensionTools: true })`。`_buildRuntime` 内部先按传入的 `flagValues` 覆盖 `extensionsResult.runtime.flagValues`（逐项 `set`，不清空旧值），再 `new ExtensionRunner(...)`、更新 `_extensionRunnerRef.current`、`_bindExtensionCore(newRunner)` + `_applyExtensionBindings(newRunner)`（因此 UI/命令上下文/错误监听器在 reload 后自动接上新 runner，**不**依赖 mode 再次 bindExtensions），最后 `_refreshToolRegistry(...)`；`beforeSessionStart` 回调只在 `hasBindings` 为真时于 `session_start` 之前调用。注意：`oldRunner.invalidate()` 之后、新 runner 建立之前，任何仍持有旧 `kiana`/`ctx` 的异步代码都会抛 stale；而 `ctx.reload()` 的 Promise 在 `session_start` 处理完后才 resolve。

**[B.1.4]** `AgentSession.dispose()` **不**发 `session_shutdown`：它中止 retry / compaction / branch summary / bash / agent，用长 stale 文案 `runner.invalidate(...)`，断开 agent、清空 listener、取消 cache warmer、`cleanupSessionResources(sessionId)`。Rust 侧不要把它映射成“正常关闭事件”。


**[B.1.5]** `_bindExtensionCore` 注入的 15 个动作里有几个“吞错并转事件”的细节（正文 §5 只给了类型）：

- `sendMessage` / `sendUserMessage`：异步失败**不**冒泡给调用者，而是 `runner.emitError({ extensionPath: "<runtime>", event: "send_message" | "send_user_message", error })`；`extensionPath` 固定为字面量 **`"<runtime>"`**。
- `appendEntry`：写入后若 `sessionManager.getEntry(entryId)` 存在，额外 `_emit({ type: "entry_appended", entry })`；custom entry 先落盘再发事件。
- `setSessionName`：转调 session 的 `setSessionName`（发 `session_info` 条目并 emit `session_info_changed`，见 §3.6），不是直接改 sessionManager。
- `getSessionName` 直接读 `sessionManager.getSessionName()`；`setLabel` 直接 `sessionManager.appendLabelChange(entryId, label)`（**不**发 `session_info_changed`；目标条目不存在时抛 `Entry <id> not found`）。
- `setModel` 在**无配置认证**时返回 `false` 且不切换（`hasConfiguredAuth`）；有认证时 `await this.setModel(model)` 后返回 `true`。所以 `ctx.modelRegistry`/`kiana.setModel` 的布尔返回值不能简单映射为 `Result` 的 Ok。
- `getCommands` 的拼接顺序固定为 扩展（`invocationName`）→ prompt 模板 → skills（`skill:<name>`）；`description` 可为 undefined；三者都带 `sourceInfo`。

### B.2 创建服务时的 pending 注册冲刷（`src/core/agent-session-services.ts`）

**[B.2.1]** `createAgentSessionServices(options)` 在 `resourceLoader.reload(...)` 之后**立刻**冲刷三张 pending 表（顺序固定），每条 `try/catch` 独立，失败产生 `AgentSessionRuntimeDiagnostic`（`type: "error"`）：

```
for (const { name, config, extensionPath } of pendingProviderRegistrations)        // 旧式 provider config
    try { modelRuntime.registerProvider(name, config) } catch (e) { message: `Extension "${extensionPath}" error: ${message}` }
pendingProviderRegistrations = []
for (const { provider, extensionPath } of pendingNativeProviderRegistrations)
    try { modelRuntime.registerNativeProvider(provider) } catch → 同上文案
pendingNativeProviderRegistrations = []
for (const { definition, extensionPath } of pendingVirtualModelRegistrations)
    try { modelRuntime.registerVirtualModel(definition) } catch → 同上文案
pendingVirtualModelRegistrations = []
await modelRuntime.refresh({ allowNetwork: false })
diagnostics.push(...applyExtensionFlagValues(resourceLoader, options.extensionFlagValues))
```

【逐字】`Extension "<extensionPath>" error: <message>`。注意：这里**总是**以 `allowNetwork: false` 刷新一次模型目录；网络刷新由 `model-runtime.ts` 的 `process.env.KIANA_OFFLINE === undefined` 另行门控（`--offline` 等价 `KIANA_OFFLINE=1`）。

**[B.2.2]** `applyExtensionFlagValues(resourceLoader, extensionFlagValues)`（`ca-session` 拥有；正文 §2.9 已给主体，这里补三个精确点）：

- 注册表来自**全部**扩展的 `extension.flags`，同名**后者覆盖前者**（与 `runner.getFlags()` 的“第一个胜出”相反）；
- `boolean` 型 flag 一律写 `true`，**忽略**传入值（即使 CLI 给的是字符串）；
- `string` 型 flag 收到非字符串值（例如 `--name` 后面没有值，CLI 记为 `true`）时产生 `{ type:"error", message: "Extension flag \"--${name}\" requires a value" }`（模板串，实际渲染为 `Extension flag "--<name>" requires a value`）；
- 未注册的名字聚合为一条：`Unknown option: --a`（1 个）或 `Unknown options: --a, --b`（多个，逗号+空格连接）。

**[B.2.3]** `main.ts` 的诊断拼装：

```
diagnostics = [
  ...projectTrustDiagnostics,        // onExtensionError 收集，type "warning"；消息由 project-trust.ts 包成 `Extension "<path>" project_trust error: <error>`
  ...services.diagnostics,           // B.2.1 的注册错误 + applyExtensionFlagValues 错误
  ...collectSettingsDiagnostics(settingsManager),   // 设置层诊断（如未知键）
  ...resourceLoader.getExtensions().errors.map(({path,error}) => ({ type:"error",
        message:`Failed to load extension "${path}": <error>` })),
  ...(resourceLoader.getExtensions().warnings ?? []).map(({path,warning}) => ({ type:"warning",
        message:`Extension package "${path}": <warning>` })),
]
```

`createAgentSessionServices` 传入 `modelRuntimeSignal: AbortSignal.timeout(15_000)`（**15 秒**网络/初始化超时）、`extensionFlagValues: parsed.unknownFlags`，以及 `disabledBuiltinExtensions: parsed.noMcp ? ["mcp"] : undefined`。诊断是否致命见 `main.ts:922`：只有包含 `"Failed to load extension"` 的诊断会触发“扩展加载失败”分支，`services.diagnostics` 的执行期注册错误**不**致命。

`extensionFlagValues` 来自 `parsed.unknownFlags`（CLI 未知 flag 的收集结果，规则见 §3.10）。

### B.3 `ResourceLoader` 装配的剩余细节

**[B.3.1]** `isBuiltinExtension(input)` 的判定是 `typeof input !== "function" && input.builtin === true`；构造时 `builtinExtensions = new Map(factories.filter(isBuiltinExtension).map(i => [i.name, i]))`，并把 `[...builtinExtensions.keys()]` 交给 package manager，因此 `settings.json` 的 `extensions` / `--extension` 都可以写 `builtin:<name>`。内置清单【逐字】：`llama.cpp`（非 replaceable）、`codemode`、`tool-search`、`mcp`（三者 `replaceable: true, builtin: true`）。

**[B.3.2]** 预信任阶段 `loadProjectTrustExtensions()`：先把 `settingsManager.setProjectTrusted(false)` 并 `reload()`，再 `loadCurrentExtensionSet({ includeInlineFactories: true })`。因此 `project_trust` handler 只能来自**个人/全局**扩展与**临时 CLI** 扩展（对应 `docs/extensions.md` 的 “Only personal and explicit command-line extensions can participate in the `project_trust` event that runs before project extensions load.”）。拿到决策后 `settingsManager.setProjectTrusted(projectTrusted)`，再次 `settingsManager.reload()`，才进入正式装配。

**[B.3.3]** `loadCurrentExtensionSet({ includeInlineFactories })`：先算 `extensionPaths`（`noExtensions ? cliEnabledExtensions : merge(cliEnabled, enabled)`），并**无条件过滤掉所有 `builtin:` 前缀路径**（注释：built-in 扩展留到最终阶段，因为项目设置可能禁用它们，而“已加载的扩展无法卸载”）；`includeInlineFactories === false` 时在合并完 install/package 警告后直接返回。`true` 时再 `loadExtensionFactories(extensionsResult.runtime)` 追加内联扩展、`omitReplacedExtensions` 并并入替换警告。注意 `disabledBuiltinExtensions` 的过滤**不在**这里，而在 `reload()` 的 `extensionPaths` 计算处（见 B.3.4）。该函数用 `loadExtensionsCached`（缓存命中按 cwd/generation），`loadFinalExtensionSet` 用 `loadExtensionPaths`（跳过 builtin 并可从预信任 runtime 继续）。

**[B.3.4]** 正式 `reload()` 的完整顺序（正文 §2.9 未覆盖）：

```
if (loaded) clearExtensionCache()
if (options.resolveProjectTrust) { preTrustExtensions = await loadProjectTrustExtensions();
                                    projectTrusted = await options.resolveProjectTrust({ extensionsResult: preTrustExtensions });
                                    settingsManager.setProjectTrusted(projectTrusted) }
await settingsManager.reload()
resolvedPaths = await packageManager.resolve()
cliExtensionPaths = await packageManager.resolveExtensionSources(additionalExtensionPaths, { temporary: true })
installWarnings = collectPackageInstallWarnings(resolvedPaths.installFailures, cliExtensionPaths.installFailures)
metadataByPath = new Map()   // 存到实例字段 resourceMetadataByPath；先由 resolved+cli 资源填充，再按需补 "cli" 元数据
extensionPaths = (noExtensions ? cliEnabled : mergePaths(cliEnabled, enabled)).filter(p =>
    !p.startsWith("builtin:") || !disabledBuiltinExtensions.has(p.slice("builtin:".length)))
packageWarnings = collectExtensionPackageWarnings(extensionPaths, metadataByPath)
extensionsResult = await loadFinalExtensionSet(extensionPaths, preTrustExtensions)
mergeExtensionWarnings(extensionsResult, [...installWarnings, ...packageWarnings])
for (p of additionalExtensionPaths) if (isLocalPath(p) && !existsSync(resolveResourcePath(p)))
    extensionsResult.errors.push({ path: resolved, error: `Extension path does not exist: ${resolved}` })
extensionsResult = extensionsOverride ? extensionsOverride(extensionsResult) : extensionsResult
applyExtensionSourceInfo(extensionsResult.extensions, metadataByPath)
```

**[B.3.5]** `loadFinalExtensionSet(extensionPaths, preTrustExtensions)` 的合并语义（移植时最容易错）：

```
preloaded = preTrustExtensions?.extensions ?? []
preloadedByPath = Map(preloaded.filter(e => !e.path.startsWith("<inline:")).map(e => [e.resolvedPath, e]))
failedPreloadPaths = Set((preTrustExtensions?.errors ?? []).map(e => resolveExtensionLoadPath(e.path)))
remainingPaths = extensionPaths.filter(p => !preloadedByPath.has(resolve(p)) && !failedPreloadPaths.has(resolve(p)))
remainingExtensions = await loadExtensionPaths(remainingPaths, preTrustExtensions?.runtime)   // 复用同一 runtime
loadedByPath = new Map(preloadedByPath); 并入 remainingExtensions.extensions
inlineExtensions = preTrustExtensions ? { extensions: preloaded.filter(path.startsWith("<inline:")), errors: [] }
                                     : await loadExtensionFactories(remainingExtensions.runtime)
orderedExtensions = extensionPaths.map(p => loadedByPath.get(resolve(p))).filter(defined); 再 push(...inlineExtensions.extensions)
result = { extensions: omitReplacedExtensions(orderedExtensions, replacementWarnings),
           errors: [...(preTrust?.errors ?? []), ...remaining.errors, ...inline.errors],
           warnings: [...(preTrust?.warnings ?? []), ...(remaining.warnings ?? [])],
           runtime: remainingExtensions.runtime }
mergeExtensionWarnings(result, replacementWarnings); addExtensionConflictDiagnostics(result)
```

顺序含义：扩展顺序严格跟 `extensionPaths`（而不是装载完成顺序），内联扩展永远排在最后；预信任阶段失败过的路径不会重试；内联扩展在预信任阶段**已经加载**过就不再重跑工厂（避免重复注册）。

**[B.3.6]** `applyExtensionSourceInfo(extensions, metadataByPath)`：对每个扩展 `extension.sourceInfo = findSourceInfoForPath(extension.path, undefined, metadataByPath) ?? getDefaultSourceInfoForPath(extension.path)`，然后**把同一个对象**赋给 `command.sourceInfo`（每个命令）与 `tool.sourceInfo`（每个工具）。因此命令/工具报告来源时与扩展一致；`registerCommand`/`registerTool` 里按当时的 `extension.sourceInfo` 先写入的值会被**整体覆盖**（若调用方通过 `...options` 塞了自定义 `sourceInfo`，也会一并被覆盖，见 B.4.1）。注意：**只有** ResourceLoader 构造的扩展会走这一步；`loadExtensionFromFactory`/测试里手工构造的 `Extension` 保持 `createExtension` 里 `createSyntheticSourceInfo(extensionPath, { source: getSyntheticPathSource(extensionPath) ?? "local", baseDir: isSyntheticPath(extensionPath) ? undefined : dirname(resolvedPath) })` 的结果（`scope` 默认 `"temporary"`、`origin` 默认 `"top-level"`，见 §B.8.12）。

**[B.3.7]** 安装失败警告【逐字】：

```
formatPackageInstallWarning(failure) = `Extension package "${failure.source}": ${describePackageInstallFailure(failure)}`
describePackageInstallFailure(failure) =
  temporary → `failed to install (${failure.message}). Check the source and try again.`
  user/project → `failed to install (${failure.message}). Run "${APP_NAME} remove ${failure.source}${scope === "project" ? " --local" : ""}" to remove it from settings.json.`
```

`PackageInstallFailure = { source: string; scope: SourceScope; message: string }`（`src/core/package-manager.ts:99`；`SourceScope` 为 `"user" | "project" | "temporary"`）。`collectPackageInstallWarnings` 产生 `{ path: failure.source, warning: describePackageInstallFailure(failure) }`——即 `warnings` 里的 `warning` **不含** `Extension package "...": ` 前缀，前缀由 `main.ts` 在拼 `Extension package "<path>": <warning>` 时再加；若直接调用 `formatPackageInstallWarning` 则已含前缀，**不要**二次拼接。`kiana config`（`package-manager-cli.ts:879-886`）先对 global + project（项目未信任时两者为同一份）的 `installFailures` 做 `Set` 去重，再逐条打印黄色 `Warning: <warning>`。

**[B.3.8]** `collectExtensionPackageWarnings(extensionPaths, metadataByPath)` 以 `metadataByPath.get(extensionPath)?.packageRoot` 去重出包根集合（`Set`，顺序为首次出现顺序），对每个存在的 `<packageRoot>/package.json` 执行 `JSON.parse(stripBom(readFileSync(...)))`，**没有** try/catch：包内 `package.json` 语法错误会在 reload 期间直接抛出（与 `7187-malformed-package-manifest` 不同，后者测的是 `kiana` manifest 字段校验）。只检查 `dependencies`（`peerDependencies` 里的宿主包不告警），命中 `HOST_PROVIDED_EXTENSION_PACKAGES` 后按字母序 `sort()` 拼接；警告的 `path` 是 **package.json 路径**，不是扩展路径。`packageRoot` 缺失（如临时 CLI 文件路径）时该扩展完全不参与此检查。

**[B.3.9]** `mergeExtensionWarnings(result, warnings)` 按 `path` 去重、**后者覆盖前者**、保持首次插入位置（`new Map([...old, ...new])`）。合并顺序：`reload()` 里先 `[...installWarnings, ...packageWarnings]`，再在 `loadFinalExtensionSet` 内并入 `replacementWarnings`；`loadCurrentExtensionSet` 里也是先 install 后 package 再替换。

**[B.3.10]** `detectExtensionConflicts` 的产物进入 **`errors`**（不是 warnings），`path` 为后注册者。`addExtensionConflictDiagnostics` 在建好最终集合后调用一次。

**[B.3.11]** `project_trust` 决策阶梯（`resolveProjectTrusted`，`src/core/project-trust.ts:47`）必须整体移植，正文 §2.3.5 只写了事件本身：

```
1. trustOverride !== undefined            → 直接返回该值（不 emit 事件、不读 store、不看 default）
2. !hasTrustRequiringProjectResources(cwd) → true（无项目资源，直接信任；不 emit 事件）
3. extensionsResult 存在 → emitProjectTrustEvent(...)；
     对每个 error：onExtensionError(`Extension "<path>" project_trust error: <error>`)
     result 非空：trusted = result.trusted === "yes"；result.remember === true 时 trustStore.set(cwd, trusted)；返回 trusted
4. trustStore.get(cwd) !== null → 返回该决策
5. defaultProjectTrust ?? SETTINGS_DEFAULTS.defaultProjectTrust：
     "always" → true；"never" → false；"ask"（或其它）→ 继续
6. !ctx.hasUI → false
7. ctx.ui.select(formatProjectTrustPrompt(cwd), getProjectTrustOptions(cwd, { includeSessionOnly: true }).map(o => o.label))
     选中 → saveProjectTrustPromptResult（按 option.updates 批量写 store）→ 返回 selected.trusted
     未选中/undefined → false
```

【逐字】提示文案：

```
Trust project folder?
<cwd>

This allows <APP_NAME> to load <CONFIG_DIR_NAME> settings and resources, install missing project packages, and execute project extensions.
```

`hasTrustRequiringProjectResources(cwd)`：`<cwd>/<CONFIG_DIR_NAME>/{settings.json, mcp.json, extensions, skills, prompts, themes, SYSTEM.md, APPEND_SYSTEM.md}` 任一存在，或从 cwd 向上任一层 `.agents/skills` 存在（`$HOME/.agents/skills` 除外——用户级技能视为始终可信）。选项标签与写入语义（`getProjectTrustOptions`，`includeSessionOnly: true`）：

| 标签（逐字） | trusted | 写入 store 的 updates |
|---|---|---|
| `Trust` | true | `[{path: cwd, decision: true}]` |
| `Trust parent folder (<parentPath>)`（仅当存在父目录） | true | `[{path: parent, decision: true}, {path: cwd, decision: null}]` |
| `Trust (this session only)` | true | `[]`（不持久化） |
| `Do not trust` | false | `[{path: cwd, decision: false}]` |
| `Do not trust (this session only)` | false | `[]` |

注意 `main.ts` 侧还有一层缓存与短路：`shouldResolveProjectTrust = parsed.projectTrustOverride === undefined && cachedProjectTrust === undefined && hasTrustRequiringProjectResources(cwd)`；否则 `projectTrusted = cached ?? parsed.projectTrustOverride ?? (!hasTrustRequiringProjectResources || trustStore.get(cwd) === true)`。即**一旦本进程内解析过某 cwd，就不再 emit `project_trust`**（`projectTrustByCwd`），这解释了文档所说“can run again when session replacement enters a new working directory”——仅当新 cwd 尚未出现在缓存中。


**[B.3.12]** `VIRTUAL_MODULES` 的键数经实测确认为 **22** 个（正文 §2.6.1 的清单与计数正确，正文照录）：6 个 typebox 族键（`typebox`、`typebox/compile`、`typebox/value`、`@sinclair/typebox`、`@sinclair/typebox/compile`、`@sinclair/typebox/value`）+ 8 个 `@earendil-works/*` + 8 个 `@mariozechner/*`。补充两条正文未明说的实现细节：源码对象字面量中 `typebox` 是**裸键**（无引号），其余键为带引号字符串（不影响语义，但逐字抄写时不要“补”引号）；`@earendil-works/kiana-coding-agent` 与 `@mariozechner/kiana-coding-agent` 指向**包根 `src/index.ts`** 的命名空间对象（源码注释逐字：“This import is safe because loader.ts exports are not re-exported from index.ts.”），因此 `loader.ts` 的任何新导出都**不得**从 `src/index.ts` 再导出，否则虚拟模块会泄漏 loader 内部符号。

### B.4 注册 API 的 `...options` 展开覆盖

**[B.4.1]** `registerCommand` / `registerShortcut` / `registerFlag` 都是**先写计算字段、再展开 options**，因此 JS 调用方可以覆盖它们：

```
extension.commands.set(name, { name, sourceInfo: extension.sourceInfo, ...options })
extension.shortcuts.set(shortcut, { shortcut, extensionPath: extension.path, ...options })
extension.flags.set(name, { name, extensionPath: extension.path, ...options })
```

即 `kiana.registerCommand("x", { handler, name: "y" })` 实际注册名为 `y`；`kiana.registerFlag("a", { type:"boolean", extensionPath:"/evil", ... })` 会改掉归属。TS 类型用 `Omit<...>` 排除了这些键，但运行期不校验。Rust 侧应**不暴露**这些字段（或显式以固定字段优先），并在兼容测试里记录该差异，不要盲目复刻可覆盖行为。

**[B.4.2]** `registerTool` 不走展开：`extension.tools.set(tool.name, { definition: tool, sourceInfo: extension.sourceInfo })`，并且立刻 `runtime.refreshTools()`（加载期该函数是 no-op；绑定后是 `_refreshToolRegistry()`）。注册前有两道运行期校验，文案逐字：

```
Tool "<name>" registered by extension "<path>" must define an object parameter schema.
Command registered by extension "<path>" must have a non-empty string name. Use kiana.registerCommand("name", { description, handler }).
Command "/<name>" registered by extension "<path>" must define handler().
Invalid default for flag "<name>": expected <boolean|string>, got <typeof default>
```

注意 `registerTool` 的 schema 校验只要求 `typeof parameters === "object" && parameters !== null && !Array.isArray(parameters)`，**不**校验 JSON Schema 结构；`registerFlag` 若同时给了 `default` 且类型不符则**抛错**（不是诊断），且该抛错发生在写入 `extension.flags` **之前**。

**[B.4.3]** `registerFlag` 的默认值写入：仅当 `options.default !== undefined && !runtime.flagValues.has(name)`；loading 期进 `pendingFlagValues`（**已存在则不覆盖**），active 期直接写 `runtime.flagValues`。加载期显式 `setFlagValue`（runner 层）先写入 `runtime.flagValues`，commit 时因 `has(name)` 为真而保留。

**[B.4.4]** 四个渲染器注册的容器语义各不相同，正文 §2.1 的表只写了“Map/追加”，这里给精确规则：

| 方法 | 写入 | 覆盖语义 |
|---|---|---|
| `registerMessageRenderer(customType, renderer)` | `extension.messageRenderers.set(customType, renderer)` | 同类型**后写覆盖**；跨扩展由渲染管线按加载顺序合并 |
| `registerMarkdownTransformer(transformer)` | `extension.markdownTransformer = transformer` | **单值**，后写覆盖（无 Map） |
| `registerEntryRenderer(customType, renderer)` | `(extension.entryRenderers ??= new Map()).set(customType, renderer)` | 同类型**后写覆盖**；容器惰性创建 |
| `registerToolRenderer(resolver)` | `(extension.toolRenderers ??= []).push(resolver)` | **数组追加**（同扩展内多个 resolver 全保留，按注册顺序） |

resolver 链由 `runner.resolveToolRenderers(toolName, base)` 实现：把所有扩展的 `toolRenderers` 按加载顺序 `flatMap` 成一个数组，`resolve(i)` 调用 `resolvers[i](toolName, () => resolve(i+1))`，越界时调用 `base()`（`agent-session` 传入的是 `() => this.getToolDefinition(name)`，即内建/已注册工具定义）。因此 `next()` **不是**“同扩展优先”，而是**全局加载顺序**的下一个 resolver，最后才是 base。另外 `getMessageRenderer` 与 `runner.getEntryRenderer` 都是**首个**匹配扩展胜出（遍历 `extensions` 返回第一个），`getMarkdownTransformers()` 返回**全部**转换器数组（渲染时按序应用），三者的合并策略互不相同。

### B.5 各模式的绑定与可见串补遗

**[B.5.1]** 交互式 `bindExtensions` 的完整 payload：`uiContext`、`mode: "tui"`、`abortHandler`（见 B.1.2）、`commandContextActions`、`shutdownHandler`、`onError: error => showExtensionError(error.extensionPath, error.error, error.stack)`。`commandContextActions` 逐项行为：

- `newSession`：先 `clearStatusIndicator()`，`runtimeHost.newSession(options)`；抛错 → `handleFatalRuntimeError("Failed to create session", error)`；
- `fork`：成功且未取消时 `editor.setText(result.selectedText ?? "")` 并 `showStatus("Forked to new session")`；抛错 → `handleFatalRuntimeError("Failed to fork session", error)`；
- `navigateTree`：未取消时 `chatContainer.clear()`、`renderInitialMessages()`、若 `result.editorText` 非空且编辑器为空则写入、`showStatus("Navigated to selected point")`、`void flushCompactionQueue({ willRetry: false })`；
- `switchSession` → `handleResumeSession(sessionPath, options)`；`reload` → `handleReloadCommand()`；`waitForIdle` → `session.waitForIdle()`。

【逐字】`Forked to new session`、`Navigated to selected point`、`Failed to create session`、`Failed to fork session`。

**[B.5.2]** `handleFatalRuntimeError(prefix, error): Promise<never>` 的精确顺序：`showError(<prefix>: <message>)` → `getCrashExtensionHint(error)`（try/catch 内 `formatCrashExtensionHint(findExtensionStackMatches(error.stack, resourceLoader.getExtensions().extensions))`，失败时 undefined；有值则以 `warning` 色插入）→ `recordCrash("fatal_error", error)` 为真时以 `muted` 色插入崩溃上报说明 → `stopThemeWatcher()` → `this.stop("transcript")` → `process.exit(1)`。命令处理器看到的是进程退出，而不是可捕获的返回值。

**[B.5.3]** `showExtensionError(extensionPath, error, stack?)` 的精确排版：

```
errorMsg = `Extension "${extensionPath}" error: ${error}`      // error 色，ThemedText(pad 1, 0)
if (stack) {
  stackLines = stack.split("\n").slice(1)                      // 丢掉首行（与 errorMsg 重复）
  if (stackLines.length > 0)
    addChild(new ThemedText(() => stackLines.map(line => theme.fg("dim", `  ${line.trim()}`)).join("\n"), 1, 0))
}
requestRender()
```

注意 `line.trim()` 会去掉缩进再统一加两个空格；整个 stack 是**一个** ThemedText 子节点（多行字符串），不是逐行节点。

**[B.5.4]** 交互式字面量默认值与 notify 的三路渲染：

- `defaultWorkingMessage = "Working"`、`defaultHiddenThinkingLabel = "Thinking..."`（三个点）。
- `setWorkingMessage(message)` 在存在 `kind === "working"` 的活动指示器时调用 `setMessage(message ?? defaultWorkingMessage)`；`resetExtensionUI()` 把活动指示器消息重置为 `${defaultWorkingMessage} (${keyText("app.interrupt")} to interrupt)`。
- `setHiddenThinkingLabel(label)` 为 `hiddenThinkingLabel = label ?? defaultHiddenThinkingLabel`。
- `notify(msg, type)` 三路：`"error"` → `showError(msg)`；`"warning"` → `showWarning(msg)`，后者固定打印 **`Warning: <msg>`**（`theme.fg("warning", ...)`，前置一个 Spacer）；其它（`"info"`/undefined）→ `showStatus(msg)`（`theme.fg("dim", ...)`，复用上一条 status 组件时只更新文本）。同一条 status 连续调用会复用现有节点而不是新增。

**[B.5.5]** `ctx.ui.theme` 在交互式里是**全局 `theme` 的 getter**；`setTheme` 对 `Theme` 实例走 `themeController.setThemeInstance`，对字符串走 `setThemeName`，成功后若 `settingsManager.getTheme() !== name` 再持久化；返回 `{ success, error? }`。RPC 的实现恒返回 `{ success: false, error: "Theme switching not supported in RPC mode" }`；noOp 为 `"UI not available"`。

**[B.5.6]** RPC 的 `notify` 把 `type` 原样写入 `notifyType`，省略时为 `undefined`（JSON 序列化后字段消失）。`docs/rpc-extension-ui.md` 同时声明 “Defaults to `"info"` if omitted.” —— 这是**客户端**默认值，Kiana 侧不做填充；Rust 端口若在客户端实现需补默认，服务端保持不发该字段。

**[B.5.7]** print/json 绑定：`mode: mode === "json" ? "json" : "print"`、`commandContextActions`（waitForIdle / newSession / fork / navigateTree / switchSession / reload；后三者直接透传 runtimeHost，没有交互式的状态提示）、`onError` 用 `console.error` 打印 `Extension error (<extensionPath>): <error>`（源码模板串为 `Extension error (${err.extensionPath}): ${err.error}`）；**无** `uiContext`、**无** `shutdownHandler`、**无** `abortHandler`。

**[B.5.8]** RPC 的 `onError` 不走交互式 `showExtensionError`，而是 `output({ type: "extension_error", extensionPath, event, error })`（**只**三个字段，无 stack）。`event` 为触发错误的扩展事件名（provider 冲刷时为 `"register_provider"`，`mcp_servers_change` 缺失时为 `"register_mcp_server"`）。交互式只显示 `error`+`stack`，**不**显示 `event`。

**[B.5.9]** RPC 的 `shutdownHandler` 只置 `shutdownRequested = true`；真正退出发生在每条命令处理完后的 `checkShutdownRequested()`（`handleInputLine` 末尾），因此 `ctx.shutdown()` 之后当前命令仍可继续执行，退出码为 0。交互式在空闲时立即 `void this.shutdown()`。RPC 的 `abortHandler` **未提供**（`ctx.abort()` 直接 `void session.abort()`）。

**[B.5.10]** `reportUnhandledMcpServers()` 【逐字】：当**没有任何** `mcp_servers_change` handler 时，对每个尚未报告过的 `runtime.mcpServers.list()` 条目 emit 一条 `register_mcp_server` 错误：

```
MCP server "<name>" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support
```

`reportedMcpServers` 按 **server.name** 去重（不是 extensionPath），跨多次调用只报一次。它在 `bindExtensions` 与 `reload()`（有绑定）后各调用一次——这正是“扩展用 `mcp_servers_change` 替换内建 MCP 支持”这一契约的运行时兜底（正文 §4.10 只描述了事件，未描述该兜底）。


### B.6 测试清单补遗

**[B.6.1]** 正文 §7.5 表列 19 个文件；同一目录下另有 **19 个**与扩展语义直接相关的回归文件未被列入（用例数为实测）：

| 文件（用例数） | 断言要点 |
|---|---|
| `3686-session-name-event.test.ts`（3） | `setSessionName`（session 侧与 `kiana.setSessionName` 侧）都发 `session_info_changed`，扩展 handler 依次收到 `{name:"first"}`、`{name:"second"}`；`sessionManager.getSessionName()` 同步 |
| `5996-session-name-newlines.test.ts`（2） | `appendSessionInfo` 把 `[\r\n]+` 换成空格并 `trim()`：`"from\nextension"` → `"from extension"`；扩展路径同样生效 |
| `5998-blocked-tool-terminate.test.ts`（1） | `tool_call` 返回 `{ block: true, reason, terminate: true }`：工具不执行（`execute` 抛错也看不到）、只剩 1 个待用响应、`tool_execution_end.result.terminate === true`、transcript 里有 `isError` 的 toolResult |
| `6363-agent-settled-event.test.ts`（3） | 重试后只发一次 `agent_settled`（`agent_end.willRetry` 依次 true/false，`agent_settled` 仅 1 个且 `aborted:false`）；`agent_end` 里排的 followUp 跑完才 settle；命令里的 `ctx.waitForIdle()` 等到 session 级 settle |
| `7253-manual-compact-during-response.test.ts`（1） | 响应进行中调 `session.compact()`：先落盘 `stopReason:"aborted"` 的 assistant 条目，再写 compaction 条目；`compaction_start/end` reason 均为 `"manual"`，compaction 条目恰好 1 个 |
| `8989-fork-compaction-label-boundary.test.ts`（1） | fork 边界落在 label 条目上时，`createBranchedSession` 把 `firstKeptEntryId` 修正到其后的真实消息，`buildSessionContext` 保留 `compactionSummary` + 后续消息 |
| `8964-extension-provider-streaming.test.ts`（`it.each`×2 = 2 例） | 扩展命令用 `ctx.modelRegistry.stream` / `streamSimple` 调用 `registerProvider` 注册的 provider：收到扩展配置的 `apiKey`、文本正确、`getApiProvider(faux.api)` 始终 `undefined`（provider 未安装进全局表） |
| `9068-user-bash-fail-closed.test.ts`（`test.each` 3 + `test.each` 2 = 5） | RPC 三例：handler 抛错 → 请求失败且不执行 bash（execute 0 次）；返回 `{}` → 错误含 `Invalid user_bash handler result` 且不执行；返回 `undefined` → 正常执行 1 次。交互式两例（`!pwd` / `!!pwd`）：返回 `{}` 时 fail-closed，事件为 `{type:"user_bash", command:"pwd", excludeFromContext, cwd}` |
| `9789-context-handler-system-messages.test.ts`（6） | 从 compactionSummary 切片后仍合成恰好 1 条前导 system 且 toolNames == active tools；未改动会话时保留中途 system 原位（共 2 条）；无返回值原地改 `event.messages` 生效；handler 在重放头之后**追加**的 system 保留；`context_with_system` 在 `context` 之后运行且输出逐字采用；丢掉前导 system 时输出仍生效并 emitError `context_with_system: Handler removed the leading system message...` |
| `5217-compaction-reason.test.ts`（3） | `before_compact`/`compact` 的 `reason`：`compact()` → `"manual"`；`_runAutoCompaction("threshold", false)` → `"threshold"`；`("overflow", true)` → `reason:"overflow"` 且 `willRetry:true` |
| `7193-event-bus-lifecycle.test.ts`（1） | reload/dispose 后扩展持有的事件总线 listener 被移除（宿主 listener 不受影响）；旧 API `getCommands()` 抛 `stale after session replacement or reload`；连续两次 reload 也不泄漏 |
| `3688-tree-cancel-compacting.test.ts`（1） | `session_before_tree` 取消导航时清空 branch summary 状态 |
| `7150-rpc-prompt-during-compaction.test.ts`（1） | 手工压缩进行中，RPC prompt 被拒绝 |
| `9178-tree-during-compaction.test.ts`（2） | 压缩进行中拒绝导航（leaf 不能变）；第一次等待期间拒绝第二次导航 |
| `9340-9777-auto-compaction-cancellation.test.ts`（3 + `it.each`×2 = 5 例） | abort 后不启动 post-run 自动压缩；取消 summarization 的认证；`compaction_start` 内同步取消；`it.each` 两例 `reports matching error text / an unrelated AbortError as a failure`（`aborted === false`、`errorMessage` 含原文）；扩展 `{cancel:true}` 记为 `compaction_end.aborted === true` |
| `8261-subagent-project-trust.test.ts`（2） | 已信任项目跳过逐次确认；未信任的交互式项目保留确认 |
| `8724-in-memory-fork-active-tool.test.ts`（1） | 工具 turn 进行中做内存 fork：被 abort 的 turn 不追加到替换会话（`bindExtensions({})` 即可，无需 UI） |
| `8935-parallel-preflight-abort.test.ts`（1） | 并行 `tool_call` 中第二个 handler 调 `ctx.abort()`：两个 preflight 都跑、但**没有任何工具执行**、`tool_result` handler 一次也不触发 |
| `pre-prompt-compaction-no-continue.test.ts`（1） | 长度截断的停顿在下一次 prompt 前先压缩，不从 assistant 消息继续 |

（`5433-extension-oauth-prompt-input`、`7187-malformed-package-manifest`、`7731-tui-method-wrapping` 名字含 extension/TUI 但分别是登录对话框、package manifest 字段校验、TUI reference 包装，**不属于**本单元契约，可不算入。）

**[B.6.2]** 顶层 `test/*.test.ts` 与 `test/suite/*.test.ts` 中与扩展语义直接相关、正文未列（或只列了部分用例）的文件：

| 文件（用例数） | 断言要点 |
|---|---|
| `compaction-extensions.test.ts`（8，`describe.skipIf(!API_KEY)`） | 需要 `ANTHROPIC_OAUTH_TOKEN || ANTHROPIC_API_KEY`，否则整组跳过（移植时保留跳过条件）；覆盖 `before_compact`/`compact` 事件、取消压缩、自定义压缩、压缩后条目、抛错回退默认、多扩展顺序、`before_compact` 数据、不同 `tokensBefore` 仍采用扩展结果 |
| `compaction-extensions-example.test.ts`（3） | 文档示例类型检查；示例经 `modelRegistry.complete` 分发；`compact` 事件字段 |
| `branch-summary-extensions.test.ts`（1） | 扩展提供的 branch summary `usage` 并入 session 总计 |
| `git-merge-and-resolve-extension.test.ts`（9） | `skips when not a git repository`；`skips when no upstream is configured`；`re-sends conflicts when in an unfinished merge`；`skips when working tree is dirty and not in a merge`；`skips when fetch fails`；`skips when merge is clean`；`sends conflict report as a follow-up`；`handles empty ours or theirs sections`；`skips message when merge fails but no conflict markers found` |
| `input-transform-streaming-example.test.ts`（6） | steering 期间跳过 exec；空闲命中触发器时转换；作为 followUp 排队时转换；不匹配、diff 为空、git 失败三种“继续”路径 |
| `jev-router-example.test.ts`（2） | 首改走 Sol、其余走 Luna；Terra 规划并忽略失败编辑 |
| `plan-mode-extension.test.ts`（4） | 切换 plan mode 时保留自定义 active tools；无计划不提示；精化按 followUp user 消息排队；执行按 followUp custom 消息排队 |
| `nested-tool-calls.test.ts`（6） | id 为 `<callerId>/<n>` 且事件带 parent id 并记录；嵌套调用记在模型发起的调用上；任意深度 usage 求和；串行工具被并发调用时串行化；`NestedCallRecorder` 省略超大参数并按上限丢弃；总参数字节上限与 `complete:false` |
| `agent-session-dynamic-tools.test.ts`（4） | 自定义 bash spawn hook 之前暴露 session 状态且可 opt-out；初始化后注册工具会刷新 registry；SDK 自定义工具返回 source metadata；无 `promptSnippet` 时保持 active 但从 available tools 省略 |
| `trigger-compact-extension.test.ts`（1） | 仅在 context usage 越过阈值时自动压缩 |
| `agent-session-dynamic-provider.test.ts`（5） | 顶层 `registerProvider` 覆盖 active model；`session_start` 时覆盖；加载期注册 native provider；命令期覆盖（不 reload）；命令期注册 native provider |
| `agent-session-runtime-events.test.ts`（4） | `/new` 与 `/resume` 的 `session_before_switch` + `session_start`；取消被尊重；`beforeSessionInvalidate` 在 `session_shutdown` 之后、`rebindSession` 之前；`session_before_fork` + `session_start` 与取消 |
| `test/virtual-models.test.ts`（17，3 个 describe：`getBranchSelection` 3、`ModelRuntime virtual models` 7、`createAgentSession with virtual models` 7；**与 `test/suite/virtual-models.test.ts` 是两个文件**） | `getBranchSelection`：`looks up only the last model_change`、`uses the last model_change without responses after it`、`uses the latest physical response without a model_change`；`ModelRuntime virtual models`：`lists a virtual model and routes it to a physical model with a clamped thinking level`、`reports the failed request of a retry separately from the latest successful response`、`lists several virtual models under a provider with physical models`、`rejects routes to virtual or unknown models`、`routes direct streamSimple calls within the routed model's limits`、`does not forward caller credentials to a routed model of another provider`、`fails unrouted stream calls on virtual models`；`createAgentSession with virtual models`：`restores the virtual selection instead of the physical model that answered`、`restores a virtual selection registered right before the session opens`、`falls back to the physical model when the virtual model is not registered`、`falls back to the last physical response when the transcript ends with a routing failure`、`resumes the selection made before tree navigation left its model_change on another branch`、`does not record a physical selection on every prompt while requests are redirected`、`records an explicit model override on resume with the next prompt`（原概括“`getBranchSelection` 只认最后一个 `model_change` / 最近一次物理响应”保留成立）`ModelRuntime` 虚拟模型路由与 thinking clamp、重试失败上报、虚拟 provider 列表、拒绝路由到虚拟/未知模型、直接 `streamSimple` 受物理限制约束、不转发跨 provider 凭据、虚拟模型上未路由的 stream 失败；开新会话时恢复虚拟选择（含刚注册、未注册回退、路由失败回退、跨分支选择、不记录物理选择、显式覆盖） |
| `test/suite/agent-session-codemode.test.ts`（25，3 个 describe） | `AgentSession codemode tool`（9）：`presents callable tools per codemode.mode`、`shows the guidelines of tools that do not fit the inline budget through describeTool()`、`runs nested calls in parallel and returns only the script result`、`routes nested calls through extension hooks`、`adds the usage of nested results to the codemode result`、`keeps structured content that tool_result handlers replace along with the content`、`attaches only the images the script passes to image(), in output order, each after its saved path`、`marks where each text item starts and puts console lines last in one text block`、`reports script failures as results that keep partial output and the calls that ran`；`codemode options and store`（11）：`applies the timeout_ms option and rejects invalid options`、`limits script memory so runaway allocations fail inside the script`、`truncates output to the token budget and spills the full text`、`formats output at the item limit in linear time`、`resolves bash calls to structured results, also for non-zero exit codes`、`resolves read calls to text for text files and to image blocks that image() shows`、`persists store() writes as custom entries for later calls`、`appends nothing for failed scripts or scripts without writes`、`runs without a session, starting from an empty store`、`loads the values written on the current branch`、`folds store entries from the root, ignoring malformed data`；`codemode models`（5）：`declares models only for the session's own codemode tool`、`lists models and classifies with catalog auth, ignoring script-supplied fields`、`generates images with catalog auth and attaches them through image()`、`notes generated images that the script did not show`、`reports provider errors as results and invalid arguments as exceptions` |
| `test/suite/agent-session-mcp.test.ts`（44 个 `it` + 4 个 `it.each` 各展开 2 例 = 52 例；3 个 describe） | `AgentSession MCP integration`（37 个 `it` + 4 个 `it.each`×2）：`--tools`/`--no-tools`/`--exclude-tools` 与 `mcp__` 前缀、codemode-only 工具的声明与跨 tree 可调用、直接调用被拒、资源工具、per-tool exposure、server 描述/instructions、`autoEnableCodemode`、`reachesTools` 与 tool_search/codemode 激活、启动等待（`startupWaitMs`）、脚本未命名的 server 不等待等；`AgentSession MCP servers registered by extensions`（4）：`connects servers registered while extensions load`、`connects and disconnects servers registered during the session`、`rejects names another extension registered`、`reports registered servers when no extension connects them`；`AgentSession MCP tools after resume and reload`（3）：`declares tools tool_search loaded again on resume once their server connects`、`does not activate restored tools that register after the next prompt starts`、`declares tools tool_search loaded again after /reload` |
| `test/suite/agent-session-mcp-oauth.test.ts`（9） | 浏览器登录/刷新/登出、粘贴重定向 URL、shutdown 取消等待中的登录（`it` 模板循环）、不刷新过期 token 地关闭、callback URL/scope、client name、无端口时补端口、`--timeout`（含授权服务器挂起）、`kiana mcp login` 凭据用于下一 turn |
| `test/mcp-extension.test.ts`（29，4 个 describe：`MCP config` 8、`MCP tools` 6、`MCP connections` 12、`MCP servers section` 3） | `MCP config`（8）：全局+受信任项目合并与校验、项目覆盖 enabled/exposure、BOM、`-`/`_` 冲突、exposure 与 `autoEnableCodemode` 项目优先、OAuth callback URL/scope/client name、per-tool exposure 精确名→模式按序、provider auth 仅全局；`MCP tools`（6）：provider-safe 名字、CallToolResult 声明、结果转换与错误标记、resource link、20KB 中段截断、多字节边界；`MCP connections`（12）：会话过期重试、无 tools capability、断线标记与重连、瞬时错误重试、初始化中关闭、关闭停止重试、resource 读重试、OAuth 重新登录等；`MCP servers section`（3）：`lists servers with how their tools are reached and the first line of their description`（逐字格式：`- mcp__<server> (codemode): <描述首行>`、`- mcp__<server> (tool_search)`，`direct` exposure 不列出，无描述时用 `connection.instructions`；全为 direct 时整段为 undefined）；`shortens descriptions to fit the size limit`（描述截断到 `MAX_SERVERS_SECTION_CHARS` 以内）；`leaves out the last servers when their names alone do not fit`（末行逐字 `- … <N> more servers; find their tools with searchTools()`） |
| `test/llama-extension.test.ts`（15） | `registers a native provider and /llama command`；`normalizes management and inference URLs`；`exposes loaded and sleeping models with router metadata`；`discovers chat-template thinking support for loaded models`；`persists and restores selectable models for cache-only startup refreshes`；`preserves cached llama.cpp context for unloaded autoload presets`；`exposes unloaded presets only when router autoload is enabled`；`classifies with selectable models through llama-server`；`exposes decision models reported by the catalog as native System One classifiers`；`lists decision models that also output text for chat`；`hides unloaded presets when router autoload is disabled`；`stays dormant until configured and stores URL plus optional key`；`searches Hugging Face and reads quantizations plus access requirements`；`loads with SSE progress and waits for the loaded catalog state`；`downloads with byte progress and returns the refreshed catalog` |
| `test/suite/agent-session-model-extension.test.ts`（18，正文只列 7 个扩展 hook 用例） | 其余 11 例为模型/thinking 行为（逐字）：`setModel saves the model to the session and emits model_select`、`only persists model and thinking defaults when requested`、`persists the requested default thinking level even when the current model clamps it`、`cycleModel and cycleThinkingLevel are session-only by default`、`applies per-model thinking level override on model switch`、`falls back to current session thinking level when no per-model or global default is configured`、`per-model override takes priority over global default during model switch`、`cycles through scoped models and preserves the scoped thinking preference`、`clamps thinking levels to model capabilities and cycles available levels`、`cycles xhigh before max when both are supported`、`throws when setModel is called without configured auth`。另有扩展 hook 7 例：`allows extension tool_call handlers to block tool execution`、`allows extension tool_result handlers to modify tool results`、`allows extension context handlers to modify messages before the LLM call`、`allows extension input handlers to transform or handle input`、`allows extension commands to inspect live system prompt options`、`allows before_agent_start handlers to inject custom messages and modify the system prompt`、`bindExtensions emits session_start and reload emits session_shutdown then session_start`。具体描述：`setModel` 保存并 emit `model_select`；仅在请求时持久化；被 clamp 时仍持久化请求的默认 thinking；`cycleModel`/`cycleThinkingLevel` 默认仅 session；切换时应用 per-model thinking 覆盖；无 per-model/全局默认时回退；per-model 优先于全局；scoped 循环保留 thinking 偏好；按能力 clamp 并循环可用级；同时支持时 `xhigh` 在 `max` 前；无认证时 `setModel` 抛错 |
| `test/suite/agent-session-boundaries.test.ts` | 26 个 `it` + 1 个 `it.each`×3（`preserves steering / follow-up / both queue scheduling around a turn_end handoff`）= 29 例，两个 describe：`AgentSession actionable boundaries`、`durable length recovery`（正文原写 24+，已改为 26 个测试块；精确展开数为 29） |

**[B.6.3]** 用例数更正：`5080-signal-shutdown-extension-cleanup.test.ts` 是 **9 个 `test` + 1 个 `test.each`×3 = 12 例**（原文写“10 个 test”，已改正）。`test/extensions-runner.test.ts` 的盘点保持 51 个 `it` 块（共 51 条 `it`/`it.each` 调用，1 个 `it.each` 展开 6 例）= 51 - 1 + 6 = 56 例（`it.each` 位于 `fails closed when a user_bash handler returns %s`）。

### B.7 文档（docs）契约补遗

**[B.6.4]** 另外两个**已在 §7.5 表中但描述不完整**的文件，补充精确用例名：

- `11227-recovery-omits-tool-results.test.ts`（1）：`pre-prompt recovery of a resumed length stop omits the truncated call and its result`——恢复时**不**把被截断的 toolCall 及其 result 放进重建的上下文（与长度恢复相关，`agent-session` 侧）。
- `5868-rpc-unknown-command-id.test.ts`（1 个 `test`）：`preserves the request id on unknown command errors`——未知命令的错误响应必须回带请求 `id`（与 §B.5.8 的 `Unknown command: <type>` 配套）。

**[B.6.5]** `test/suite/agent-session-compaction.test.ts`（27 例，其中 **11 处**使用扩展 hook：10 处 `kiana.on("session_before_compact")` + 1 处 `kiana.on("session_compact_failed")`）与 `test/suite/agent-session-compaction-model-overrides.test.ts`（2 例，各 1 处 `kiana.on("session_before_compact")`：`uses the newly selected model without changing ordinary settings`、`captures model identity before awaiting summarization auth`）主要属于 `ca-session`/`ca-compaction`，但其中的扩展 hook 断言（`cancel`、自定义 compaction、`session_compact_failed` 事件、模型覆盖时仍走扩展）应作为**跨单元契约**在 Rust 集成测试中覆盖；本单元只登记，不重复盘点全部用例数。注意 `session_compact_failed` 是事件表的成员（§2.1.5），正文 §7 未列任何直接用例；唯一直接用例是 `test/suite/agent-session-compaction.test.ts` 的 `notifies extensions when auto-compaction fails`（断言 `reason:"threshold"`、`aborted:false`、`willRetry:false`、`fromExtension:false`、`errorMessage:"Auto-compaction failed: summary generator blew up"`；该前缀归 `ca-session` 所有，见其 `_runAutoCompaction` 段，本单元只消费事件）。


**[B.6.6]** `test/mcp-extension.test.ts` 的 `MCP servers section` 组测的是 **`ca-builtin-extensions`** 拥有的 `renderServersSection`（`src/extensions/mcp/index.ts`），但它是 **MCP 扩展与扩展系统的接口面**，移植时不要遗漏这些逐字契约：

- `MCP_SERVERS_SECTION = "mcp_servers"`（系统提示段落名）；`MAX_SERVERS_SECTION_CHARS = 4096`；单条描述上限 `MAX_SERVER_DESCRIPTION_CHARS = 250`。
- 首行 `serversSectionIntro(reaches)` 逐字：`MCP servers whose tools are not declared to you.`，若含 codemode 追加字符串 “ Call the tools of `codemode` servers from codemode scripts.”，若含 tool_search 追加 “ Load the tools of `tool_search` servers with `tool_search`.”（两段按 codemode → tool_search 顺序拼接，去重后判断；注意每段前有一个**前导空格**）。
- 每个条目：`- <mcpNamespace(name)> (codemode|tool_search)`；有摘要时追加 `: <摘要>`；摘要取 `config.description.trim()` 首个换行前的内容，为空则用 `connection.instructions` 首行，再为空则不显示冒号后缀。
- 排序：`entry.name.localeCompare`；过滤：仅 `isEnabled` 且 exposure 含 `codemode` 或 `deferred`；无此类 server 时返回 `undefined`。
- 放不下时的收尾行逐字：`- … <N> more server(s); find their tools with searchTools()`（`N === 1` 时为 `server`，否则 `servers`）。
- 描述截断：`truncate(text, max) = max <= 1 ? "" : text.slice(0, max-1).trimEnd() + "…"`；每服务器可用长度 `min(250, floor((4096 - size(kept)) / kept) - 2)`。
- `direct` exposure 的 server 不出现在该段（`hasIndirectTools` 只看 `configuredExposures`）。

**[B.7.1]** `docs/extensions.md` 的额外契约点（正文 §2.7.1 未列）：

- “Only personal and explicit command-line extensions can participate in the `project_trust` event that runs before project extensions load.”（对应 B.3.2）
- “`project_trust` runs before trusted project resources load and can run again when session replacement enters a new working directory.”
- “Reload replaces the extension runtime, so code after `await ctx.reload()` must not reuse state from the old runtime.”（与下一条同页出现，措辞略有重复，非笔误）
- “Retries, recovery, compaction, and queued messages can start another agent run after `agent_end`, so `agent_end` is not a final-idle signal.”（对应 `agent_settled` 的语义与 6363 测试）
- “Reload and session replacement invalidate the old runtime. Code after `await ctx.reload()` still belongs to the old runtime and must not reuse its session-bound state.”（原文同段，先一句在 line 145 前部，后一句在 line 50；两者都要收进逐字表）
- `exposure: "hidden"` 的退出方法：“Re-register a tool with `exposure: "hidden"` to withdraw it, since tools cannot be unregistered.”（正文只说 hidden 不可达，未给撤销方法）
- namespace 别名匹配：“a name and the same name as a script identifier (`my-docs`, `my_docs`) match alike”；实现为 `toCodemodeIdentifier(name)`（首个字符须 `[A-Za-z_$]`，其后 `[A-Za-z0-9_$]`，其余字符→`_`，空名→`_`；`mcp__docs__search` 不变、`my-tool`→`my_tool`）。`aliases` 供 `describeNamespace(name)` 与 `searchTools()` 的 `namespace` 选项使用。
- MCP annotation 缺省语义：“Missing hints take the MCP defaults: a tool is not read-only, and may be destructive and reach an open world.”；并给出 permission-gate 组合式 `needsApproval` 示例。
- `prepareLoadout` 的触发时机是“whenever the active tools change”，入参含 declared / callable / 每个 registered 工具的 exposure、namespace、prompt guidelines；默认系统提示会从工具列表与规则中省略 hidden declaration，且文件读取器被隐藏时 skills 提示不点名任何工具；`codemode` 只用该 hook + `exposure` + `reachesTools` + `ctx.executeTool()`。
- `reachesTools`：`"all"` 或输入中可能命名它们的词；`codemode` 对“搜索/列出工具”的脚本返回 `"all"`，否则返回脚本中的词；`tool_search` 恒 `"all"`；MCP 扩展仅当同名工具声明了 `reachesTools` 才激活 `codemode`/`tool_search`。

**[B.7.2]** `docs/tui.md` 的额外契约点：

- “Do not create a second terminal renderer inside an extension.”（正文未收录）
- 完整的最小自定义屏幕示例（命令 `pick-target`）：`ctx.mode !== "tui"` 时 `notify("pick-target requires interactive mode", "error")`；`ctx.ui.custom<T>` 工厂内缓存 `cachedWidth`/`cachedLines`，在 `invalidate()` 清缓存、状态变更后 `tui.requestRender()`，用注入的 `keybindings.matches(data, "tui.select.up/down/confirm/cancel")`，通过 `done()` 结束；示例不显示文本光标故无需 `Focusable`。
- 主题：“Use `theme.style()` to combine foreground and background colors with text attributes”（`{ fg:"success", bg:"toolSuccessBg", bold:true }`）；`theme.colors` 暴露具体色，颜色运算用 `mixColors()`；`theme.appearance` 为 `"dark"`/`"light"`；token 设为终端默认色时按终端宣布色或猜测；Kiana 按终端能力转 truecolor/256 色；token 每主题只转换一次，尽量在渲染路径外算具体色；`theme.fg()`/`theme.bg()` 仍可用。
- overlay：`overlayOptions` 可为函数以动态更新；`OverlayHandle.setHidden()` 可临时隐藏；有焦点的 overlay 跨普通渲染保留输入；不要把 `ctx.ui.custom()` 的 overlay 用 `OverlayHandle.hide()` 关闭。
- 组件清单与 `Focusable`/`CURSOR_MARKER`/`renderFakeCursor()`/`MouseRegion`/regular vs fullscreen/`KIANA_TUI_WRITE_LOG` 的表述与正文一致，可保留。

### B.8 其它行为与风险点


**[B.7.3]** `docs/extensions.md` 中的可执行片段（逐字语义，Rust 文档与测试可直接复用）：

```ts
const hints = kiana.getAllTools().find((tool) => tool.name === event.toolName)?.annotations;
const needsApproval =
  hints?.destructiveHint === true ||
  (!hints?.readOnlyHint && ((hints?.destructiveHint ?? true) || (hints?.openWorldHint ?? true)));
if (needsApproval && !(await ctx.ui.confirm("Allow tool call?", event.toolName))) {
  return { block: true, reason: `${event.toolName} was not approved` };
```

要点：`readOnlyHint === true` 直接放行（除非 `destructiveHint === true` 优先拦截）；`destructiveHint`/`openWorldHint` 缺省按 **true** 处理，与 B.7.1 的“MCP 默认”一致。`kiana.getAllTools()` 的每项至少含 `name`、`exposure`、`namespace`、`annotations`、`reachesTools`。

**[B.7.4]** 注册即激活规则（逐字）：“Registering a `direct` or `model-only` tool activates it; the other exposures are not activated on registration.” 即 `hidden` / `prompt` 等其余 exposure 注册后**不**自动进入 active set；active set 指“declared to the model”的集合。这与 §3.9 的注册表语义相互补充，Rust 实现应在 `register_tool` 后仅对这两种 exposure 调 `activate`。

**[B.7.5]** `docs/tui.md` 中 `ctx.ui.custom()` 的组件契约（补充 §2.1 的签名表，不改变签名）：组件对象必须提供 `render(width)`、`handleInput(data)`、`invalidate()`；`invalidate()` 必须在状态或主题相关输出变化后清缓存，随后 `tui.requestRender()`；`done(value)` 即 resolve；`overlay: true` 时 `overlayOptions` 可为函数以动态更新尺寸/锚点/边距/可见性，`OverlayHandle.setHidden()` 仅用于临时隐藏，**不能**用来结束 `ctx.ui.custom()`（原文：“Do not call `OverlayHandle.hide()` on an overlay created by `ctx.ui.custom()`”）；有焦点的 overlay 跨普通渲染保留输入，需通过 handle 显式释放焦点或转发。另：每一行渲染都必须 fit 到给定宽度，宽度用 `visibleWidth()`/`truncateToWidth()`/`sliceByColumn()`/`wrapTextWithAnsi()` 计算（ANSI、宽字符、emoji、组合字符都会改变显示宽度）；Kiana 每行后重置样式与超链接，需逐行重新应用。


**[B.8.1]** `commit()` 的顺序有一个易错点：先判 `state !== "loading"`，再 `runtime.assertActive()`，**之后**才写 flag、跑 pending 变更并把 state 置 `"active"`。若工厂执行期间 runtime 已被 invalidate（会话替换/reload），`commit()` 会抛 stale 错误 → `initializeExtension` 的 catch 调 `discard()` → 工厂整体作为“加载失败”上报，且 `pendingFlagValues` / `pendingRuntimeChanges` 一条都不会应用。Rust 端口必须保留“先校验再提交”的原子性。

**[B.8.2]** `discard()` 只退订 `loadingUnsubscribers`（加载期通过 `api.events.on` 建立的订阅）；`commit()` 之后建立的订阅由 runtime 级 `trackEventBusSubscription` 统一管理，`invalidate` 时退订。

**[B.8.3]** `getFlag(name)` 先 `assertActive()`，再要求 `extension.flags.has(name)`，最后 `runtime.flagValues.has(name) ? runtime.flagValues.get(name) : pendingFlagValues.get(name)`。因此“加载期读到自己注册 flag 的默认值”只在 flag 已注册时成立；factory 失败后 API 整体不可用。

**[B.8.4]** `wrapUIPromptContext(ui)` 用 `{...ui}` 浅拷贝，并把 `select`/`confirm`/`input`/`editor`/`custom` 五个方法替换为经 `withUIPrompt(kind, title, fn)` 的包装（用于 `uiPromptDepth`/`activeUIPrompt` 记账，`kind` 分别为 `"select" | "confirm" | "input" | "editor" | "custom"`，`custom` 的 title 为 undefined）。浅拷贝会把 `theme` getter **求值一次**变成普通值；Rust trait 天然按方法读，但要确保 `theme` 仍反映最新主题（交互式实现里 `theme` 是 getter）。另注意 `setUIContext(undefined, mode)` 会把 `uiContext` 换回**共享的** `noOpUIContext` 单例，而 `mode` 独立保存。

**[B.8.5]** 交互式 `getBuiltInCommandConflictDiagnostics` 过滤条件是 `builtinNames.has(command.name)`（用 `name`，不是 `invocationName`），`path = command.sourceInfo.path`，`type: "warning"`。因此一个扩展命令即使以 `name:1` 出现也会被报冲突。

**[B.8.6]** `runner.getCommandDiagnostics()` 恒为 `[]`：`getRegisteredCommands()` 每次先把 `commandDiagnostics = []` 再返回，当前实现没有任何地方 push。移植时保留该“空”行为（测试可能断言为空）。

**[B.8.7]** `registerNativeProvider` 在 bindCore 冲刷时若无 `providerActions`，落到 `this.modelRegistry.registerProvider(provider)`（同一个方法，原生重载）；`unregisterProvider` 在 pre-bind 阶段同时从两张表按名/id 过滤，post-bind 才调 `unregisterProvider`。

**[B.8.8]** `applyExtensionSourceInfo` 的覆盖发生在 `extensionsOverride` **之后**，因此 override 返回的扩展也会被重新赋 sourceInfo；命令/工具的 `sourceInfo` 与扩展共享同一对象，Rust 用 `Arc<SourceInfo>` 可保持身份。

**[B.8.9]** `createExtensionRuntime()`（`loader.ts:160`）的桩行为必须逐条映射：

- 除 `refreshTools`（no-op）、`setModel`（`Promise.reject(new Error("Extension runtime not initialized"))`）以及注册类方法外，所有动作方法共用同一个 `notInitialized`，抛 `Extension runtime not initialized. Action methods cannot be called during extension loading.`；
- 预绑定期 `registerProvider` / `registerNativeProvider` / `registerVirtualModel` 的 `extensionPath` 默认参数是 **`"<unknown>"`**（调用方不传时），因此 `registerProvider("p", cfg)` 直接调用会得到 `extensionPath === "<unknown>"`；
- 预绑定期 `unregisterProvider(name)` **同时**从 `pendingProviderRegistrations`（按 `r.name`）与 `pendingNativeProviderRegistrations`（按 `r.provider.id`）过滤；`unregisterVirtualModel(provider, id)` 按 `definition.provider` + `definition.id` 精确匹配；
- `invalidate(message?)` 只在首次设置 `staleMessage`（后续调用直接返回），默认文案即 §B.1.4 的长串；设置后立即退订所有 `trackEventBusSubscription` 注册项并清空集合；
- `trackEventBusSubscription(unsubscribe)` 返回的包装退订是**幂等**的（第二次调用 no-op），并在触发时自行从集合移除；
- `assertActive()` 只检查 `staleMessage`（不检查是否已 bindCore）；这就是“加载期动作方法不可用、invalidate 后全部不可用”两级语义的来源。


**[B.8.10]** `emitError` 的 **`event` 取值清单**（正文 §3.6 只给了部分文本）：`register_provider`（两种 provider 重载共用）、`register_virtual_model`、`register_mcp_server`、`message_end`、`tool_result`、`user_bash`、`context`、`context_with_system`、`before_provider_request`、`before_provider_headers`、`before_agent_start`、`resources_discover`、`input`、`cache_warming_decision`、`turn_end`/`agent_before_settle`（`baseEvent.type`）、以及 `_bindExtensionCore` 注入的 `send_message`/`send_user_message`（`extensionPath` 固定 `"<runtime>"`）。`stack` 只在有 Error 时填，`error` 恒为字符串；RPC 的 `extension_error` 只透传 `extensionPath/event/error`。

**[B.8.11]** `emitError` 同步遍历 `errorListeners`（`Set`），监听器抛错会中断后续监听器并冒泡到 emit 点；Rust 端口应在遍历时捕获并忽略单个监听器的异常（或定义为不可 panic）以保持宿主稳定。`onError(listener)` 返回 `() => this.errorListeners.delete(listener)`，重复调用幂等；`_applyExtensionBindings` 每次先调用旧退订再注册新监听器（见 B.1.1），所以**不会**出现重复投递。


**[B.8.12]** `createExtension(path, resolvedPath)` 的 `sourceInfo` 初始值：`source = getSyntheticPathSource(extensionPath) ?? "local"`（`builtin:` 前缀 → `"builtin"`；`<...>` 合成路径取冒号前的段，如 `<inline:N>`/`<inline:name>` → `"inline"`、`<sdk:name>` → `"sdk"`，空段回退 `"temporary"`；普通文件路径无匹配 → `"local"`），`scope` 与 `origin` 走 `createSyntheticSourceInfo` 的默认值 `"temporary"`/`"top-level"`，`baseDir` 仅对非合成路径取 `dirname(resolvedPath)`（合成路径为 undefined）。`entryRenderers` 在构造时**已初始化**为空 Map（与 `toolRenderers` 的惰性创建不同），`toolRenderers` / `markdownTransformer` 初始不存在。

**[B.8.13]** `Extension` 的 9 个容器在构造时的初始状态：`handlers`/`tools`/`messageRenderers`/`entryRenderers`/`commands`/`flags`/`shortcuts` 为非可选空 Map；`toolRenderers?`、`markdownTransformer?` 为可选且初始 undefined。Rust 侧建议统一用 `Vec`/`IndexMap` 并在序列化快照中保持“空容器存在、可选字段缺失”的区别（源码里 `runner.getEntryRenderer` 用 `ext.entryRenderers?.get(...)`，但 `createExtension` 恒初始化它；Rust 侧保留可选类型即可）。

### B.9 Rust 映射补遗

**[B.9.1]** `ExtensionBindings` → `SessionBindings`（在 ca-session 侧），六个字段全部 `Option`，`bind_extensions` 必须复刻 B.1.1 的“只赋非 None + `_apply` + emit session_start + report MCP + resources_discover”顺序；`abort/shutdown` 的降级链（B.1.2）由各 mode 注入。

**[B.9.2]** 需要新增 `create_agent_session_services` 等价物（`kiana-extensions` 提供 helper 或由 ca-session 实现）：冲刷三张 pending 表 → `refresh(allow_network:false)` → `apply_extension_flag_values`，诊断文案 `Extension "<path>" error: <msg>`；`apply_extension_flag_values` 的“同名后者覆盖、boolean 恒 true、string 须 string、未知聚合”四条规则（B.2.2）要有单测。

**[B.9.3]** `ResourceLoader` 侧的装配顺序（B.3.4/B.3.5）建议在 `ca-resources` 里做逐字移植，尤其是：`extensionPaths` 顺序决定最终顺序、内联扩展排最后、预信任失败路径不重试、`runtime` 复用 `remainingExtensions.runtime`、`extensionsOverride` 后再 `applyExtensionSourceInfo`。缺失这些会导致 `project_trust` 阶段与正式加载的扩展集合不一致（2753 / 2860 / 8261 类测试会发现）。

**[B.9.4]** `toCodemodeIdentifier` 必须在 Rust 里实现（namespace 别名匹配、codemode 脚本的工具名），不能只用字符串相等匹配 `mcp__` 前缀；MCP 的 `mcpNamespace(server) = "mcp__" + server.replace(/-/g, "_")` 已在 §4.4，但别名归一化是另一条规则。

**[B.9.5]** UI 侧：`no_op_ui_context` 的 `set_theme` 返回 `"UI not available"`，RPC 返回 `"Theme switching not supported in RPC mode"`，交互式走 theme controller 并按需持久化设置（B.5.5）。`has_ui` 用 `Option` 判定（§2.3.6 已说明），但 `set_ui_context(None)` 后必须让 `ctx.hasUI` 立即为 false。

***
