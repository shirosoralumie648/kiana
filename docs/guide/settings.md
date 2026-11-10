# 设置（settings）

本页讲什么：kiana 的设置文件放在哪里、多个文件如何叠加、怎样修改设置、`/settings` 面板能改什么，以及常用设置键的完整参考。所有键都来自当前代码的 settings schema 与运行期读取路径。

## 快速开始

最常用的三种改法：

```bash
# 1. 在交互界面里打开设置面板
#    输入 /settings，回车

# 2. 直接编辑用户设置文件
$EDITOR ~/.kiana/agent/settings.json

# 3. 让普通设置的手工修改生效：在交互界面输入 /reload
#    （重启 kiana 也可以；permissions / hooks 需新建会话）
```

一个最小的用户设置示例：

```json
{
  "defaultModel": "provider/model-id",
  "defaultThinkingLevel": "medium",
  "theme": "system",
  "tuiMode": "fullscreen",
  "compaction": {
    "enabled": true,
    "reserveTokens": 16384
  },
  "permissions": {
    "mode": "default"
  }
}
```

注意：

- 设置文件是严格 JSON，不支持注释和尾逗号。
- 文件内容应为 JSON 对象；顶层数组虽然会被按“下标键对象”展开，但不建议使用。
- `kiana config` 打开的是“资源启用/禁用”界面，不是 settings 编辑器；不要把它和 `/settings` 混用。

## 设置文件与优先级

用户可编辑的设置文件有三个：

| 文件 | 路径 | 用途 | 生效条件 |
| --- | --- | --- | --- |
| 用户（全局）设置 | `~/.kiana/agent/settings.json` | 个人偏好，作用于所有项目 | 总是读取；`KIANA_CODING_AGENT_DIR` 可改配置目录 |
| 项目设置 | `<项目>/.kiana/settings.json` | 项目共享配置，通常提交到仓库 | 仅在项目受信任时读取 |
| 项目本地设置 | `<项目>/.kiana/settings.local.json` | 机器本地配置，通常加入 `.gitignore` | 仅在项目受信任时读取；作为 local 配置层参与 `permissions`、`hooks` 等分层合并，类型化 Settings 键不从该文件读取 |

`<agentDir>` 默认为 `~/.kiana/agent`，可用环境变量 `KIANA_CODING_AGENT_DIR` 覆盖。

叠加规则：

1. **用户 → 项目**：同一个键以项目值为准。
2. **嵌套对象递归合并**：例如用户设置 `compaction.enabled`，项目只设置 `compaction.reserveTokens`，两者都会生效。
3. **普通数组整体替换**：高层数组替换低层数组；`defaultTools` 是例外，纯 `+name` / `-name` 列表会追加到继承列表。
4. **项目本地只参与分层配置**：`settings.local.json` 作为 local 配置层参与 `permissions`、`hooks` 等读取；类型化 Settings 键（`theme`、`defaultModel` 等）不从该文件读取，请写进 `settings.json`。
5. **权限数组合并**：`permissions.rules`、`allow`、`deny`、`ask`、`addDirs` 等跨层取并集（去重）；`permissions.mode` 以项目本地层为准。
6. **命令行 flag 覆盖本次运行**：例如 `--tui-mode`、`--permission-mode`、`--thinking`、`--use-theme` 只影响当前进程，不写回设置文件。

此外还存在系统/管理员层文件（不是日常编辑的三个文件）：

| 路径 | 说明 |
| --- | --- |
| `/etc/kiana/settings.json` | 主机级配置 |
| `/etc/kiana/managed-settings.json` | 管理员下发的托管配置 |
| `/etc/kiana/enterprise-settings.json` | 企业策略配置 |

普通设置键由用户/项目两层决定，项目层高于用户层；在权限等分层解析路径中，用户/项目层同样高于系统/管理员层。策略键（`allowManagedPermissionRulesOnly`、`allowManagedHooksOnly`、`disableBypassPermissionsMode`、`allowedMcpServers`、`deniedMcpServers`）在合并后由管理员层强制生效，下层无法放宽。Windows 上这些路径位于 `%ProgramData%\kiana\`。

## 如何编辑

### `/settings` 面板

在交互界面输入 `/settings` 打开设置面板：

- 面板只读取一次当前值；打开期间外部修改不会自动刷新。
- 面板的修改写入**用户设置文件**（`~/.kiana/agent/settings.json`），不写项目设置。
- 多数修改立即作用于当前会话；少数（如 `collapseChangelog`、`enableInstallTelemetry`、`quietStartup`）在下一次读取时生效。
- 面板支持按标签模糊搜索（只过滤标签，不搜索描述），一次显示 10 行。

| 按键 | 作用 |
| --- | --- |
| `Up` / `Down` | 移动选中项 |
| `Enter` / `Space` | 循环切换取值，或打开子菜单 |
| 直接输入字符 | 输入搜索词（搜索框为空时，`Space` 仍是切换取值） |
| `Esc` | 关闭子菜单或整个面板 |

### 直接编辑文件

- 用户设置：`~/.kiana/agent/settings.json`。
- 项目设置：`<项目>/.kiana/settings.json`（需要项目受信；kiana 内部的写入路径会做信任检查）。
- 项目本地：`<项目>/.kiana/settings.local.json`（参与 `permissions`、`hooks` 等分层合并；类型化 Settings 键不生效）。

编辑后：

- 在交互界面执行 `/reload`，会重新读取全局与项目两个设置作用域并应用运行期设置；键位文件也会一起重载。
- `permissions` 与 `hooks` 在会话创建时装配，`/reload` 不会重建权限引擎或 hooks 配置；修改它们后请新建会话或重启 kiana。`settings.local.json` 也只在会话创建时读取。
- 重启 `kiana` 最稳妥。
- 如果文件不是合法 JSON，该作用域会保留上一次的值（首次启动则使用默认值），加载错误会被记录，不会阻止会话启动。

### 写入行为

- 保存时只覆盖本会话修改过的字段，磁盘上其它字段与未知键会保留。
- 程序内对项目设置与项目本地设置的写入在项目未受信时会被拒绝，错误文案为 `Project is not trusted; refusing to write project settings`。
- 权限审批里的 `Always allow (this machine)` 会向 `settings.local.json` 的 `permissions.rules` 追加规则；`Always allow (this project)` 会向项目 `settings.json` 追加规则（两者都要求项目受信）。`Always allow (this session)` 只记在内存里。

## `/settings` 面板能改什么

面板项与其对应设置如下。面板项名称按界面英文标签列出。

### 模型、会话与请求

| 面板项 | 设置键 | 可选值 |
| --- | --- | --- |
| Auto-compact | `compaction.enabled` | `true` / `false` |
| Steering mode | `steeringMode` | `one-at-a-time` / `all` |
| Follow-up mode | `followUpMode` | `one-at-a-time` / `all` |
| Transport | `transport` | `sse` / `websocket` / `websocket-cached` / `auto` |
| Session share destination | `sessionShareDestination` | `auto` / `gist` / `radius` |
| HTTP idle timeout | `httpIdleTimeoutMs` | `30 sec` / `1 min` / `2 min` / `5 min` / `disabled` |
| Cache warming | `cacheWarming` | `off` / `streaming` / `idle` |
| Hide thinking | `hideThinkingBlock` | `true` / `false` |
| Default thinking level per model | `modelThinkingLevels` | 子菜单：为单个模型设置或清除覆盖等级 |
| Skill commands | `enableSkillCommands` | `true` / `false` |

### 终端与显示

| 面板项 | 设置键 | 可选值 |
| --- | --- | --- |
| Show images | `terminal.showImages` | `true` / `false`；仅当终端支持图片时出现 |
| Image width | `terminal.imageWidthCells` | `60` / `80` / `120`；仅当终端支持图片时出现 |
| Auto-resize images | `images.autoResize` | `true` / `false` |
| Block images | `images.blockImages` | `true` / `false` |
| Mermaid diagrams | `markdown.mermaid` | `off` / `final` / `streaming` |
| TUI mode | `tuiMode` | `regular` / `fullscreen` |
| Fullscreen exit output | `fullscreenExitOutput` | `transcript` / `resume-hint` |
| Fullscreen scrollbar | `fullscreenScrollbar` | `auto` / `always` / `hidden` |
| Fullscreen copy on select | `fullscreenCopyOnSelect` | `true` / `false` |
| Fullscreen wheel scrolling | `fullscreenWheelScrollLines` | `auto` / `1` / `2` / `3` / `5` / `10`（当前值不在列表时会追加显示） |
| Theme | `theme` | 已发现主题 + `automatic`（分别选择浅色/深色主题） |
| Language | `ui.locale` | `English` / `简体中文` |
| Show hardware cursor | `showHardwareCursor` | `true` / `false` |
| Editor padding | `editorPaddingX` | `0` / `1` / `2` / `3` |
| Output padding | `outputPad` | `0` / `1` |
| Autocomplete max items | `autocompleteMaxVisible` | `3` / `5` / `7` / `10` / `15` / `20` |
| Clear on shrink | `terminal.clearOnShrink` | `true` / `false` |
| Terminal progress | `terminal.showTerminalProgress` | `true` / `false` |

### 启动与行为

| 面板项 | 设置键 | 可选值 |
| --- | --- | --- |
| Cache miss notices | `showCacheMissNotices` | `true` / `false` |
| Collapse changelog | `collapseChangelog` | `true` / `false` |
| Quiet startup | `quietStartup` | `true` / `header` / `false` |
| Install telemetry | `enableInstallTelemetry` | `true` / `false` |
| Default project trust | `defaultProjectTrust` | `Ask` / `Always trust` / `Never trust` |
| Double-escape action | `doubleEscapeAction` | `tree` / `fork` / `none` |
| Tree filter mode | `treeFilterMode` | `default` / `no-tools` / `user-only` / `labeled-only` / `all` |
| Warnings | `warnings` | 子菜单：`Anthropic extra usage` 开关 |

面板**不能**修改默认模型；默认模型请用 `/model` 选择器选中后按 `Ctrl+S`，或直接编辑 `defaultProvider` / `defaultModel`。

## 顶层键参考

`schema` 声明了 58 个类型化顶层键。下表“已知默认”只列出 `SETTINGS_DEFAULTS` 中明确存在的值；`—` 表示没有可确认的全局默认，由 getter 或调用方决定，不要把它当作“空值”。`permissions`、`hooks`、`cron` 等不在 58 个类型化键中，见后文“schema 之外、运行期会读取的键”。

### 模型与思考

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `defaultProvider` | string | — | 默认 provider；与 `defaultModel` 配对使用 |
| `defaultModel` | string | — | 默认模型 ID；`/model` 选择器按 `Ctrl+S` 可写入 |
| `defaultThinkingLevel` | `off` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max` | — | 全局默认思考等级 |
| `modelThinkingLevels` | record<string, 同上枚举> | — | 按模型覆盖思考等级；键为精确的 `provider/id`，不做 glob |
| `enabledModels` | string[] | — | `Ctrl+P` / `/scoped-models` 使用的模型 pattern 列表；可用 glob 与模糊匹配 |
| `thinkingBudgets` | object | — | 各思考等级的 token 预算，见下表 |

### 会话、上下文与网络

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `transport` | `auto` / `sse` / `websocket` / `websocket-cached` | `auto` | 支持多种传输的 provider 的首选传输方式 |
| `sessionShareDestination` | `auto` / `gist` / `radius` | `auto` | `/share` 的上传目标；`auto` 优先 Radius，否则 GitHub Gist |
| `steeringMode` | `all` / `one-at-a-time` | `one-at-a-time` | 流式回复中 Steering 队列的取用方式 |
| `followUpMode` | `all` / `one-at-a-time` | `one-at-a-time` | Follow-up 队列的取用方式 |
| `compaction` | object | 见下表 | 自动上下文压缩 |
| `branchSummary` | object | 见下表 | 切换分支时的摘要 |
| `retry` | object | 见下表 | 请求重试与 provider 超时 |
| `sessionDir` | string | — | 会话存储目录；优先级 `--session-dir` > `KIANA_CODING_AGENT_SESSION_DIR` > `sessionDir` > 默认目录 |
| `cacheWarming` | `off` / `streaming` / `idle` | `streaming` | 缓存预热策略 |
| `httpProxy` | string | — | HTTP/HTTPS 代理地址；启动时从全局设置读取 |
| `httpIdleTimeoutMs` | number 或 `"disabled"` | `300000`（5 分钟） | 等待响应头/响应体的最大空闲间隔；`disabled` 表示不限制 |
| `websocketConnectTimeoutMs` | number 或 `"disabled"` | — | WebSocket 连接超时；`disabled` 表示不限制 |

### 界面与终端

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `theme` | string | — | 主题名，或 `light/dark` 自动形式；未设置时主题控制器使用 `system` |
| `tuiMode` | `regular` / `fullscreen` | `fullscreen` | 交互界面布局 |
| `fullscreenExitOutput` | `transcript` / `resume-hint` | `transcript` | 退出全屏时是否把 transcript 重绘到主屏；恢复提示在任何正常退出都会打印 |
| `fullscreenScrollbar` | `auto` / `always` / `hidden` | `auto` | 全屏滚动条 |
| `fullscreenCopyOnSelect` | bool | `true` | 全屏下选中即复制 |
| `fullscreenWheelScrollLines` | number 或 `"auto"` | `auto` | 滚轮每格滚动行数 |
| `hideThinkingBlock` | bool | `false` | 隐藏 assistant 的 thinking 块 |
| `showCacheMissNotices` | bool | `false` | 在 transcript 中显示 cache 成本与 provider 恢复诊断 |
| `quietStartup` | bool 或 `"header"` | `false` | 关闭启动期详细输出；`header` 只保留启动 header |
| `collapseChangelog` | bool | `false` | 更新后显示精简 changelog |
| `terminal` | object | 见下表 | 终端显示能力与图片设置 |
| `images` | object | 见下表 | 图片发送行为 |
| `ui` | object | 见下表 | 界面语言 |
| `markdown` | object | 见下表 | Markdown 渲染 |
| `warnings` | object | 见下表 | 单项警告开关 |
| `editorPaddingX` | number | `0` | 编辑器水平内边距（面板可选 0–3） |
| `outputPad` | number | `1` | 消息、工具输出与命令输出的水平内边距 |
| `autocompleteMaxVisible` | number | `5` | 自动补全下拉最多可见项 |
| `showHardwareCursor` | bool | `false` | 使用终端硬件光标；`KIANA_HARDWARE_CURSOR=1` 可作为回退 |
| `doubleEscapeAction` | `fork` / `tree` / `none` | `tree` | 空编辑器连按两次 `Esc` 的动作 |
| `treeFilterMode` | `default` / `no-tools` / `user-only` / `labeled-only` / `all` | `default` | 打开 `/tree` 时的默认过滤 |

### 启动、Shell 与统计

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `externalEditor` | string | — | `Ctrl+G` 使用的外部编辑器命令；空时依次回退 `VISUAL`、`EDITOR`、`nano`（Windows 为 `notepad`） |
| `shellPath` | string | — | bash 工具使用的 shell 路径；非空值会展开 `~` |
| `shellCommandPrefix` | string | — | 每条 shell 命令前追加的字符串 |
| `npmCommand` | string[] | — | 包管理使用的 npm 命令 argv 覆盖 |
| `defaultProjectTrust` | `ask` / `always` / `never` | `ask` | 没有扩展或已保存决定时的项目信任策略 |
| `enableInstallTelemetry` | bool | `true` | 安装/更新后的匿名版本 ping；`KIANA_TELEMETRY` 可覆盖 |
| `enableAnalytics` | bool | `false` | 使用统计开关；开启且缺少 `trackingId` 时会生成一个 |
| `trackingId` | string | — | 统计 ID |
| `deviceId` | string | — | 设备 ID；需要时自动生成 |
| `lastChangelogVersion` | string | — | 上次已读的 changelog 版本，用于判断新条目 |

### 资源、工具与 codemode

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `packages` | `(string 或 object)[]` | — | 已安装的包；对象形式支持 `source`、`autoload`、`extensions`、`skills`、`prompts`、`themes` |
| `extensions` | string[] | — | 额外扩展文件或目录 |
| `skills` | string[] | — | 额外 skills 文件或目录 |
| `prompts` | string[] | — | 额外 prompt template 文件或目录 |
| `themes` | string[] | — | 额外主题文件或目录 |
| `enableSkillCommands` | bool | `true` | 把 skills 注册为 `/skill:<name>` 命令 |
| `defaultTools` | string[] | — | 默认工具选择。纯工具名列表替换内置默认（`read`、`bash`、`edit`、`write`）；纯 `+name` / `-name` 列表在继承列表上增删 |
| `codemode` | object | 见下表 | codemode 工具描述与内联预算 |

### 其他

| 键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `$schema` | string | — | 可选的 JSON Schema 引用；运行期不读取其值 |

## 嵌套键参考

### `compaction`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | 上下文过大时自动压缩 |
| `reserveTokens` | number | `16384` | 为 prompt 与回复保留的 token |
| `keepRecentTokens` | number | `20000` | 压缩时保留的最近 token |
| `modelOverrides` | record<`provider/id`, object> | — | 按模型覆盖 token 设置；键为精确 `${provider}/${id}` |
| `modelOverrides[].reserveTokens` | number | — | 缺省时沿用父级 |
| `modelOverrides[].keepRecentTokens` | number | — | 缺省时沿用父级 |

### `branchSummary`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `reserveTokens` | number | `16384` | 分支摘要预留 token |
| `skipPrompt` | bool | `false` | 跳过摘要前的确认提示 |

### `retry`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | 启用自动重试 |
| `maxRetries` | number | `3` | 最大重试次数 |
| `baseDelayMs` | number | `2000` | 基础退避延迟（毫秒） |
| `maxAgentDelayMs` | number | `60000` | agent 层退避上限（毫秒） |
| `provider.timeoutMs` | number | — | provider 请求超时（毫秒） |
| `provider.maxRetries` | number | — | provider 层最大重试次数 |
| `provider.maxRetryDelayMs` | number | `60000` | provider 层最大重试延迟（毫秒） |

旧键 `retry.maxDelayMs` 会在加载迁移中写入 `retry.provider.maxRetryDelayMs`。

### `terminal`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `showImages` | bool | `true` | 在终端内联渲染图片 |
| `imageWidthCells` | number | `60` | 内联图片宽度（终端列） |
| `clearOnShrink` | bool | `false` | 内容变少时清除空行；可能引起闪烁 |
| `showTerminalProgress` | bool | `false` | 在终端标签栏显示 OSC 9;4 进度 |
| `hyperlinks` | bool 或 `"auto"` | — | 终端超链接能力 |
| `images` | `kitty` / `iterm2` / `auto` / `false` | — | 强制图片协议或关闭图片；`true` 不是合法值 |
| `trueColor` | bool 或 `"auto"` | — | 强制真彩能力 |

### `images`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `autoResize` | bool | `true` | 把大图缩放到 2000×2000 以内 |
| `blockImages` | bool | `false` | 阻止图片发送给模型 |

### `ui`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `locale` | string | — | 界面语言标识；缺失时回退旧键 `language`，再回退 `"en"` |

模型回答语言与 UI 语言是两套设置：UI 使用 `ui.locale`。模型回答语言按“显式配置 > `KIANA_OUTPUT_LANGUAGE` > 设置文件”决定；设置文件中先读顶层 `output_language`，再读 `ui.outputLanguage` / `ui.output_language`。当前 `ui.locale` 可读；`/settings` 的 `Language` 项会写入 `ui.locale` 并立即重新加载翻译器，但内置语言包只有 en/zh，覆盖范围有限，详见 [国际化（i18n）](i18n.md)。

### `thinkingBudgets`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `minimal` / `low` / `medium` / `high` | number | — | 各思考等级的 token 预算 |

### `markdown`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `codeBlockIndent` | string | 两个空格 | 代码块缩进 |
| `mermaid` | `off` / `final` / `streaming` | `streaming` | Mermaid 渲染时机 |

### `warnings`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `anthropicExtraUsage` | bool | `true` | Anthropic 订阅认证额外用量警告 |

### `codemode`

| 嵌套键 | 类型 | 已知默认 | 说明 |
| --- | --- | --- | --- |
| `mode` | `on` / `only` | `on` | codemode 工具描述的呈现方式 |
| `inlineBudget` | number | `3000` | 内联工具声明的 token 预算 |

### `packages[]`

| 嵌套键 | 类型 | 说明 |
| --- | --- | --- |
| `source` | string | 包来源；对象形式必填 |
| `autoload` | bool | 是否自动加载 |
| `extensions` / `skills` / `prompts` / `themes` | string[] | 只加载包内指定资源 |

## schema 之外、运行期会读取的键

以下键不在 58 个类型化顶层键中，但会通过原始 JSON 读取。它们没有类型化 schema 校验，写错时不会在加载阶段报错。

| 键 | 形状 | 说明 |
| --- | --- | --- |
| `permissions` | object | 权限模式与规则，见下表 |
| `hooks` | `{ EventName: [{ matcher, hooks: [...] }] }` | hooks 配置；事件名开放 |
| `cron` | bool 或 `{ enabled, dir, intervalSecs }` | 定时任务；默认关闭 |
| `continuity` | bool 或 `{ enabled, dir, execution, checkpoints, background, acceptance }` | continuity 增强；默认关闭 |
| `loopGuard` | `{ enabled, stopOnDetection, softToolCallsPerTurn, hardToolCallMultiplier, globalWindow, thresholds }` | 循环保护；缺省使用保守默认 guard |
| `budget` | `{ maxTurns, maxTokens, maxWallClockMs, maxCostUsd, warnRatios }` | 回合/token/墙钟/费用预算；未配置任何 cap 时不启用 |
| `tools.applyPatch.models` / `tools.apply_patch.models` | string[] | 对指定模型暴露 `apply_patch` 而不是 `edit` |
| `output_language` | string | 模型回答语言（原始键）；顶层优先于 `ui.outputLanguage` |
| `skillActivationTtlSeconds` / `skillActivationTtl` | number | 技能激活有效期（秒）；也接受 `skills.activationTtlSeconds` |
| `language` | string | 旧的 UI 语言键；`ui.locale` 缺失时回退读取 |

### `permissions` 对象

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `mode` / `defaultMode` | `read-only` / `default` / `accept-edits` / `bypass` / `off` | 权限模式；未知值按 `default` 处理（fail-closed）；`mode` 优先于 `defaultMode` |
| `rules` | 规则数组 | 支持 `{tool, decision, reason}`、`{command, decision, reason}`、`{path, decision, reason}`；也支持 `{allow: [...]}` / `{deny: [...]}` / `{ask: [...]}` 简写 |
| `allow` / `deny` / `ask` | 规则数组 | 简写形式 |
| `addDirs` / `workspaceRoots` / `additionalDirectories` | string[] | 额外可写根目录 |
| `enabledMcpjsonServers` / `disabledMcpjsonServers` | string[] | MCP 服务器开关 |
| `allowedMcpServers` / `deniedMcpServers` | array | 管理员 MCP allowlist / denylist；deny 永远合并并优先 |
| `disableBypassPermissionsMode` | bool | 禁止 `bypass` 模式；管理员层可强制 |
| `allowManagedPermissionRulesOnly` | bool | 只保留管理员层的 allow/ask 规则；deny 仍合并 |
| `allowManagedHooksOnly` | bool | 只保留管理员层 hooks |

`permissions` 不是 `Settings` schema 的 58 个类型化键之一；它通过原始 JSON 读取。项目设置与项目本地设置合并时，规则数组取并集，`mode` 以项目本地为准。完整行为见 [权限与审批](permissions.md)。

## 未知键保留

- 用户设置与项目设置按宽松模式加载：未知顶层键不会报错，会保存在内存的原始值区（`extra`）中。
- 写回采用“只覆盖本会话修改过的字段”的选择性持久化，因此磁盘上其它字段以及未修改的未知键会保留。
- 已知键类型不符或为 `null` 时也会保留原始值，getter 按 JS 语义回退；不要把 `null` 当作“删除该键”的可靠方式。
- 系统/管理员层默认启用严格诊断：未知字段会带文件、行列与 key path 记录到诊断里，但诊断不阻止该层参与合并。
- 未知键不会被 schema 校验，也不会获得默认值；写错名字不会生效，也不会自动纠正。

## 项目信任对项目设置的影响

项目设置与项目本地设置都要求项目受信任。判定顺序：

1. 命令行 `--approve` / `--no-approve`（仅本次运行）；
2. 项目里没有需要信任的资源 → 视为受信；
3. 扩展的 `project_trust` 事件；
4. `trust.json` 中该目录（或最近父目录）的已保存决定；
5. `defaultProjectTrust`：`always` / `never` / `ask`；
6. 没有交互 UI 时按不受信处理；有 UI 时弹出选择器。

各状态的影响：

| 状态 | 项目设置 | `settings.local.json` | 项目 MCP / 扩展资源 |
| --- | --- | --- | --- |
| 受信 | 读取并生效；可写入 | 读取 `permissions` / `hooks` 等分层配置；可写入 | 读取 |
| 不受信 | 忽略；写入报错 | 忽略；写入报错 | 忽略 |

- `--approve` / `--no-approve`：只影响本次进程，不写 `trust.json`。
- `/trust`：把选择写入 `<agentDir>/trust.json`，选择器提示需重启 kiana 后生效。
- `trust.json` 是扁平对象，键为目录路径，值为 `true`、`false` 或 `null`；最近的父目录决定会被继承。
- 不受信项目最危险的情形是项目设置里的 `permissions.mode: bypass` 或 `settings.local.json` 中的授权；当前实现会在读取与写入两侧都跳过/拒绝。

## 限制与排查

- **改了文件没生效**：设置是内存快照。普通设置执行 `/reload` 或重启；`permissions` / `hooks` 与 `settings.local.json` 需要新建会话或重启。`/reload` 在流式回复或压缩期间会被拒绝。
- **项目设置被忽略**：检查项目是否受信。可运行 `/trust`，或用 `--approve` 临时信任；`defaultProjectTrust=never` 会直接拒绝。
- **写项目设置报 `Project is not trusted...`**：先信任项目；不受信项目不能给自己写入权限或其它设置。
- **`settings.local.json` 里的类型化设置没反应**：该文件只参与 `permissions`、`hooks` 等分层配置；`theme`、`defaultModel` 等键请写进 `settings.json`。
- **JSON 写坏**：该作用域回退默认值或保留上一次的值，加载错误会被记录；修正 JSON 后 `/reload`。
- **语言项**：内置语言包只有 `en` 与 `zh`，其它 locale 回退英文；界面文案覆盖仍不完整，详见 [国际化（i18n）](i18n.md)。
- **`permissions` 不受 schema 约束**：字段拼错不会在加载时报错，只会静默不生效；请对照上表与 [权限与审批](permissions.md)。
- **管理员策略优先**：`allowManagedPermissionRulesOnly`、`disableBypassPermissionsMode` 等策略键无法被用户/项目设置放宽。
- **统计开关**：`enableAnalytics`、`trackingId`、`deviceId` 的设置与 ID 生成路径可核对；实际上报链路未在本次核对中验证（未验证）。
- **平台范围**：当前只在 Linux x86_64 上验证；macOS/Windows 的路径与终端行为未实测。
- **默认值写法**：表中 `—` 表示没有可确认的全局默认值，由运行期决定；不要假设为 `false`、`0` 或空数组。

## 相关页面

- [交互模式（TUI）](interactive-mode.md)
- [权限与审批](permissions.md)
- [Hooks](hooks.md)
- [CLI 参考](cli.md)
- [国际化（i18n）](i18n.md)
