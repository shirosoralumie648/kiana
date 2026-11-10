# Kiana 用户文档

本页讲什么：整套 Kiana 用户文档的总索引。按“入门 / 配置 / 安全与权限 / 能力 / 运行方式 / 运维”分组列出全部页面，给出 5 分钟阅读路径，并说明主题重叠时应该先看哪一页。

> **文档快照**：本套文档对应 2026-11-08 的 TUI 面板轮次；面板集成后的全量测试快照为 `sha256:9f545f83f58a39010859a2e04cc7e0828ee7411b943a9981a0a3f3f43785804e`（13328 passed / 0 failed / 401 ignored）。计算范围是 `crates/**/*.rs`、`crates/**/Cargo.toml`、根 `Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`，使用本地脚本 `/tmp/kiana-src-hash.sh` 计算；测试结束后并发会话又修改了少量源码，最终以当前工作树与 `kiana --help` 为准。文档页面本身不在哈希范围内。
>
> **当前状态：alpha**。workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`，后者来自 `kiana-core` 的 TypeScript 兼容常量，是有意保留的差异，不是构建错误。当前只在 Linux x86_64 上验证，macOS/Windows 行为未实测。
>
> **国际化**：`ui.locale` 已接入 `/settings`（English / 简体中文），审批卡、模态与待发消息的固定文案会随界面语言切换；完整范围见 [国际化（i18n）](i18n.md)。

首次阅读前，可以先确认本机二进制和可用选项：

```bash
# 应输出 1.4.0（TS 兼容常量，见上方说明）
kiana --version

# 查看全部 flag、子命令与内建工具说明
kiana --help
```

## 5 分钟阅读路径

按下面顺序读四页，先跑起来，再知道边界，最后固化自己的配置：

| 顺序 | 页面 | 读完能做什么 |
| --- | --- | --- |
| 1 | [快速开始](getting-started.md) | 运行 `dist/v0.1.0-alpha` 产物，完成首次认证，开始第一次交互会话 |
| 2 | [CLI 参考](cli.md) | 找到要用的 flag、子命令、退出码和 stdin/stdout 约定 |
| 3 | [权限与审批](permissions.md) | 弄清一次工具调用何时被放行、询问或拒绝 |
| 4 | [设置](settings.md) | 把模型、工具、权限等偏好写进 settings，并知道改完如何生效 |

## 全部页面

下面按主题列出 `docs/guide/` 下当前实际存在的全部 `.md` 文件（本索引 `README.md` 除外），顺序是推荐阅读顺序，不按字母排序。

### 入门

| 页面 | 这页讲什么 |
| --- | --- |
| [快速开始](getting-started.md) | 从本仓库 `dist/v0.1.0-alpha` 产物启动 `kiana`，完成首次认证、第一次交互式会话、一次性 `-p` 运行和常用权限模式选择。 |
| [CLI 参考](cli.md) | `kiana` 命令行的完整参考：启动方式、全部 flag、`install`/`remove`/`uninstall`/`update`/`list`/`config`/`auth`/`mcp` 子命令、退出码与 stdin/stdout 约定。 |
| [交互模式（TUI）](interactive-mode.md) | 终端交互界面的启动方式、界面组成、斜杠命令、排队消息、审批卡、主题、TUI 模式与快捷键。 |
| [会话、分支与恢复](sessions.md) | 会话保存在哪里，如何继续/恢复/分叉会话，`/tree`、`/fork`、`/clone`、`/compact`、导入导出与分享，以及 continuity 的当前实现和限制。 |

### 配置

| 页面 | 这页讲什么 |
| --- | --- |
| [设置（settings）](settings.md) | settings 文件放在哪里、多层配置如何叠加、`/settings` 面板能改什么，以及顶层/嵌套设置键与运行期读取键的参考。 |
| [模型与 provider](models-and-providers.md) | 内置哪些 provider、各自认哪些环境变量，`--provider`/`--model`/`--list-models` 如何选模型，thinking 等级、`models.json` 与 `kiana auth` 的行为。 |
| [国际化（i18n）](i18n.md) | `ui.locale` 与模型回答语言两套设置、内置与自定义语言包的加载和回退，以及当前只覆盖部分界面文案的范围说明。 |

### 安全与权限

| 页面 | 这页讲什么 |
| --- | --- |
| [权限与审批](permissions.md) | 权限模式、工具动作分类、规则语法、审批交互、规则持久化与项目信任；回答“这次调用允不允许”。 |
| [沙箱与执行隔离](sandbox.md) | 沙箱策略与后端（bubblewrap/Landlock）、workspace-write/read-only、网络限制与 fail-closed 行为；回答“允许之后在什么环境里执行”。 |
| [Hooks（钩子）](hooks.md) | 如何用 settings 的 `hooks` 在工具调用前后、权限拒绝、prompt 提交、会话与压缩事件上执行本地命令或 HTTP 请求，以及 matcher、退出码和 JSON 输出契约。 |

### 能力

| 页面 | 这页讲什么 |
| --- | --- |
| [内置工具](tools.md) | `read`、`bash`、`powershell`、`edit`、`write`、`grep`、`find`、`ls` 的行为与默认激活状态，工具选择和过滤规则，Web 工具，以及工具审批与沙箱的关系。 |
| [MCP 服务器](mcp.md) | 用 `kiana mcp` 管理 MCP 服务器、`mcp.json` 的字段与作用域、stdio 与 Streamable HTTP 两种传输、OAuth 登录，以及 MCP 子进程的沙箱与环境变量规则。 |
| [Skills 与扩展](skills-and-extensions.md) | Markdown 技能、prompt 模板、主题，以及扩展与包的安装、启用、禁用和排查；`kiana install` 如何把包写进 settings。 |
| [子代理](subagents.md) | 子代理运行时与定义加载协议、权限收窄、工作区隔离和后台任务边界。**当前实现未暴露为 CLI/TUI 工具，只面向嵌入式宿主 API。** |
| [持久记忆](memory.md) | 记忆的作用域与目录、启用方式、`memory` 工具协议、写入/召回/审计流程、遗忘清理与隐私边界。**默认关闭，需 `KIANA_MEMORY=1`。** |
| [IDE 集成](ide.md) | 通过 `~/.kiana/ide/*.lock` 发现编辑器、建立 MCP Streamable HTTP 连接、发送 `ide/contextUpdate` 与处理 diff 通知。**当前仅包含 kiana 侧客户端，需外部 IDE 插件配合。** |

### 运行方式

| 页面 | 这页讲什么 |
| --- | --- |
| [无头模式（`-p`、`--mode json`、RPC）](headless.md) | 在脚本、CI 和编辑器插件里非交互运行：`-p`/`--print` 文本输出、`--mode json` 的 JSONL 事件流、`--mode rpc` 的 JSONL 命令协议、stdin、`@file`、退出码与管道注意事项。 |
| [Daemon 事件服务（`--serve`）](daemon.md) | 用 `kiana --serve` 把会话事件暴露成本机 HTTP 服务：启动参数、bearer token、SSE/WebSocket/REST 端点、事件与 durable/volatile 通道。 |

### 运维

| 页面 | 这页讲什么 |
| --- | --- |
| [遥测、离线与外部服务](telemetry-and-offline.md) | Kiana 会访问哪些外部服务、安装遥测与 `enableAnalytics` 分别控制什么、`/share` 查看器链接如何生成，以及离线模式与 OpenTelemetry 导出。 |
| [故障排查](troubleshooting.md) | 按“现象 → 原因 → 处理”整理启动、权限、沙箱、MCP、daemon、IDE 与磁盘/日志问题，并给出可直接运行的诊断命令。 |

### 内部资料

| 页面 | 这页讲什么 |
| --- | --- |
| [_inventory.md](_inventory.md) | 文档事实底座：写作口径与能力清单的唯一事实来源。它不面向最终用户，不作为教程阅读，也不在下面的阅读路径中。 |

## 能力状态速览

alpha 阶段各能力的接线程度不同，读具体页面前可以先看这张表：

| 能力 | 当前状态 | 详情 |
| --- | --- | --- |
| CLI、交互模式、无头模式、RPC | 已实现 | [CLI 参考](cli.md)、[交互模式](interactive-mode.md)、[无头模式](headless.md) |
| 会话保存、恢复、分叉、导入导出、分享 | 已实现 | [会话、分支与恢复](sessions.md) |
| 权限与沙箱 | 已实现，两层相互独立、可只开一层 | [权限与审批](permissions.md)、[沙箱](sandbox.md) |
| MCP、Hooks、Skills/扩展/主题 | 已实现 | [MCP](mcp.md)、[Hooks](hooks.md)、[Skills 与扩展](skills-and-extensions.md) |
| Daemon 事件服务 | 已实现；默认只监听 `127.0.0.1:0`，可用 `--serve-addr` 覆盖 | [Daemon](daemon.md) |
| 持久记忆 | 已实现并接入会话，但**默认关闭**；无 `/memory` 命令、无 `kiana memory` 子命令、无 settings 开关，用 `KIANA_MEMORY=1` 启用 | [持久记忆](memory.md) |
| 子代理 | 运行时与定义加载器已实现，但**未暴露**为 CLI/TUI 工具或子命令 | [子代理](subagents.md) |
| IDE 集成 | 仅 kiana 侧客户端接线，需要外部 IDE 插件写 lockfile 并提供 MCP 端点 | [IDE 集成](ide.md) |
| 国际化 | 仅约 20 个固定文案 key，`zh` 不会让整个界面变成中文 | [国际化](i18n.md) |

## 主题重叠与合并指引

有些主题会跨多页出现。为避免重复维护，下表给出“先看哪页、再看哪页”的顺序；各页面仍然保留，不合并删除。

| 你可能在找 | 先看（权威页） | 再看（细节或交叉主题） |
| --- | --- | --- |
| 所有 flag、子命令、退出码 | [CLI 参考](cli.md) | 各专题页只给常用示例，不重复完整清单 |
| 设置键、文件优先级、项目信任 | [设置](settings.md) | [权限](permissions.md)、[Hooks](hooks.md)、[模型与 provider](models-and-providers.md) 只讲本主题键的语义 |
| 权限规则与沙箱边界 | [权限与审批](permissions.md) | [沙箱与执行隔离](sandbox.md)；两者独立，推荐同时开启 |
| 内置工具行为与工具审批 | [内置工具](tools.md) | [权限与审批](permissions.md) 负责动作分类与规则；[MCP](mcp.md) 负责 `mcp` 类工具 |
| 斜杠命令与会话语义 | [交互模式](interactive-mode.md) | [会话、分支与恢复](sessions.md) 讲 `/tree`、`/fork`、`/compact` 等命令背后的会话行为 |
| 非交互运行与事件协议 | [无头模式](headless.md) | [Daemon 事件服务](daemon.md) 讲 `--serve` 的端点和通道；事件字段以两页各自章节为准 |
| MCP 通用配置与 IDE 连接 | [MCP 服务器](mcp.md) | [IDE 集成](ide.md) 是 MCP Streamable HTTP 的一个特定客户端场景 |
| `/share` 的导出内容与会话行为 | [会话、分支与恢复](sessions.md) | [遥测、离线与外部服务](telemetry-and-offline.md) 讲查看器 URL 与外部服务端点 |
| 上下文压缩（compaction）行为与压缩前后钩子 | [会话、分支与恢复](sessions.md) | [Hooks](hooks.md) 讲压缩事件如何触发本地命令或 HTTP 请求 |
| 遥测、离线与外部服务 | [遥测、离线与外部服务](telemetry-and-offline.md) | [设置](settings.md) 提供相关键的索引，[CLI 参考](cli.md) 提供 `--offline` 等 flag |
| 出问题时的症状排查 | [故障排查](troubleshooting.md) | 各专题页的“限制与排查”章节讲本主题的细节，不重复诊断流程 |

## 限制与排查

- **快照会过期**：文档对应上面的源码哈希。代码变更后，先重跑 `/tmp/kiana-src-hash.sh` 对比；不一致时以 `kiana --help` 和源码为准，再更新文档。
- **文档与程序不一致**：命令行 flag、settings key、环境变量和斜杠命令均以当前代码与 `kiana --help` 为准。页面中标注“未验证”的行为不要当作承诺。
- **半成品能力**：持久记忆、子代理、IDE 集成、国际化都有明确的范围限制，见“能力状态速览”和各页的“限制与排查 / 限制与未验证”章节。
- **平台范围**：只在 Linux x86_64 上验证；macOS/Windows 未实测。
- **i18n 范围**：`/settings` 可直接切换 English / 简体中文；内置语言包只覆盖约 20 个固定文案 key，完整范围见 [国际化（i18n）](i18n.md)。
- **断链检查**：本索引和全部页面已按本快照对应的工作树检查相对链接与页内锚点；新增页面后请同步更新本页分组和链接。

## 相关页面

- [快速开始](getting-started.md)
- [CLI 参考](cli.md)
- [权限与审批](permissions.md)
- [设置（settings）](settings.md)
- [故障排查](troubleshooting.md)
