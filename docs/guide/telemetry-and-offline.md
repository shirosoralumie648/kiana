# 遥测、离线与外部服务

本页讲什么：Kiana 会访问哪些外部服务、安装遥测与匿名用量开关分别控制什么、`/share` 的查看器链接如何生成，以及如何用离线模式关闭启动期网络操作。

> 版本说明：workspace 的包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`；后者是 TS 兼容常量，有意保留，不是构建错误。安装遥测上报的版本也取 `1.4.0`。

## 快速开始

```bash
# 只关闭安装遥测：settings.json 保持不变，本次运行用环境变量覆盖
KIANA_TELEMETRY=0 kiana

# 离线启动：跳过启动期网络操作
KIANA_OFFLINE=1 kiana
# 与上面等价
kiana --offline

# 使用自建 Kiana 服务（Kiana 不内置任何上游服务域名）
export KIANA_API_BASE_URL="https://kiana.example.com"
kiana
```

三个开关彼此独立：`KIANA_TELEMETRY` 只管安装遥测与归因请求头；`KIANA_OFFLINE` 关闭启动期网络操作；`KIANA_API_BASE_URL` 决定“Kiana 自建服务”在哪里（它也会影响可用的远端模型目录）。它们都不改变对话内容。

## 安装遥测

安装遥测（install telemetry）只在交互模式启动、检测到全新安装或 changelog 有新版本时发送一次，用来统计版本分布；它不是 OpenTelemetry，也不上传对话内容。

| 项目 | 行为 |
| --- | --- |
| 触发时机 | 交互模式启动，且本次启动检测到全新安装或 changelog 有新版本条目 |
| 请求 | `GET {KIANA_API_BASE_URL}/api/report-install?version=<VERSION>` |
| 请求头 | `User-Agent: kiana/<version> (<platform>; <runtime>; <arch>)` |
| 请求体 | 无 |
| 超时 | 5 秒；fire-and-forget，响应内容被丢弃，不重试 |
| 发送的数据 | 版本号（query）与 User-Agent 中的版本/平台/运行时/架构；不含会话内容、提示词、文件路径或 `trackingId` |
| 跳过条件 | `KIANA_OFFLINE` 为真、安装遥测关闭、`KIANA_API_BASE_URL` 未配置 |
| 非交互模式 | 不触发（版本检查与 changelog 展示只发生在交互模式） |

### 开关与优先级

| 设置 / 环境变量 | 默认 | 作用 |
| --- | --- | --- |
| `enableInstallTelemetry`（settings） | `true` | 关闭后不发送安装遥测，同时关闭模型请求中的 Kiana 归因信息 |
| `KIANA_TELEMETRY`（环境变量） | 未设置 | 只要已定义就覆盖 settings；环境变量只在本次进程生效 |

`KIANA_TELEMETRY` 的真值表（大小写不敏感，不做 trim）：

| 值 | 结果 |
| --- | --- |
| 未设置 | 回退到 `enableInstallTelemetry` |
| `1` / `true` / `yes`（任意大小写） | 开启 |
| 其它任何已定义值（含空串、`0`、`false`、`no`、`on`） | 关闭 |

注意：`kiana --help` 把该变量写成“`1/true/yes` 开，`0/false/no` 关”，实际实现是“任何非真值都关”，因此 `KIANA_TELEMETRY=on` 不会开启遥测。

### 归因请求头

同一个开关还决定模型请求是否携带 Kiana 归因信息（`KIANA_ATTRIBUTION` 常量，不是环境变量）：开启时，在 OpenRouter、NVIDIA、OpenCode 等支持归因渠道的请求中附加相应请求头；关闭时不附加。该行为每次请求前重新读取开关，修改环境变量或设置后对下一次请求生效。

### 关闭安装遥测

交互式会话中可以用 `/settings` 打开设置菜单，把 **Install telemetry** 改为 `false`。也可以直接编辑 `settings.json`：

```json
{
  "enableInstallTelemetry": false
}
```

只想临时关闭时，用 `KIANA_TELEMETRY=0 kiana`；这不会改写设置文件。

## 匿名用量分析（enableAnalytics）

`enableAnalytics` 的默认值是 `false`。通过设置接口首次把它设为 `true` 时，如果 settings 中没有 `trackingId`，会生成一个 UUID 并写入 `trackingId`；手动在 `settings.json` 里写 `"enableAnalytics": true` 不会触发这一步。

当前实现与限制：

- 本仓库只有“保存开关 + 生成 `trackingId`”的实现；没有发现读取 `enableAnalytics` 并向服务端上传匿名用量数据的代码路径。因此当前版本不应期待该开关产生可见的上报行为。
- 首次运行向导文案中提到的 `/privacy` 在当前版本没有对应斜杠命令，不能用来查看已上报的数据。与遥测相关的设置入口目前只有 `/settings` 中的 **Install telemetry** 开关。
- `/bug` 生成的报告在收集元数据时会剔除 `trackingId` 与 `deviceId`；`deviceId` 用于部分 provider 的 OAuth 设备流程，不是分析标识。

## 外部服务端点

Kiana 不内置任何上游服务域名，也不会回退到外部 Kiana 站点。所有 Kiana 自建服务统一由以下变量配置：

| 环境变量 | 作用 | 默认 | 自身未配置或为空时 |
| --- | --- | --- | --- |
| `KIANA_API_BASE_URL` | Kiana 自建服务的根地址 | 无 | 相关网络功能不可用（不会回退到任何默认域名） |
| `KIANA_INSTALLER_API_BASE` | 托管更新的 release API | `KIANA_API_BASE_URL` + `/api/installer/releases` | 回退到派生值；`KIANA_API_BASE_URL` 也没有时托管更新报“不可用” |
| `KIANA_SHARE_VIEWER_URL` | `/share` 查看器链接的 base URL | `KIANA_API_BASE_URL` + `/session/` | 回退到派生值；两者都没有时 gist 仍可创建，但不显示查看器链接 |

取值细节：

- `KIANA_API_BASE_URL` 会 trim 首尾空白、去掉末尾所有 `/`；不校验 URL 格式，请写完整的 `http(s)://` 地址。
- `KIANA_INSTALLER_API_BASE` 会 trim；空串按未设置处理。
- `KIANA_SHARE_VIEWER_URL` 只判断非空、**不 trim**，首尾空白会原样进入链接。

### 派生端点

| 功能 | 端点 |
| --- | --- |
| 远端模型目录 | `{KIANA_API_BASE_URL}/api/models/providers/<provider-id>?types=chat%2Cimage%2Cclassifier` |
| 最新版本检查 | `{KIANA_API_BASE_URL}/api/latest-version` |
| 安装遥测 | `{KIANA_API_BASE_URL}/api/report-install` |
| MCP Client ID Metadata（CIMD） | `{KIANA_API_BASE_URL}/oauth/client.json` 或 `/oauth/<callback-id>/client.json` |
| 分享查看器 | `{KIANA_API_BASE_URL}/session/` |
| 托管安装器 | `{KIANA_API_BASE_URL}/api/installer/releases`（或 `KIANA_INSTALLER_API_BASE`） |

### 未配置 API base 时的行为

| 功能 | 行为 |
| --- | --- |
| 远端模型目录刷新 | 只使用内置目录与 `models-store.json` 缓存 |
| 最新版本检查 | 跳过，不显示新版本提示 |
| 安装遥测 | 跳过上报 |
| MCP OAuth CIMD | 明确报错，提示配置端点或改用 DCR（`oauth.clientRegistration: "dcr"`） |
| `/share` 查看器 | gist 仍可创建，但提示查看器 URL 未配置 |
| 托管更新 | 提示 `Managed updates are unavailable: set KIANA_INSTALLER_API_BASE or KIANA_API_BASE_URL.` |

## 分享会话与查看器

`/share` 是交互式斜杠命令（没有对应的 CLI flag），把当前会话导出成 JSONL/HTML 后上传。目标由 settings 的 `sessionShareDestination` 决定：

| 取值 | 行为 | 前提 / 失败提示 |
| --- | --- | --- |
| `auto`（默认） | 先尝试 Radius；没有可用 Radius 凭据时回退到 GitHub gist | — |
| `gist` | 直接使用 GitHub CLI 创建私有 gist | 需要安装并登录 `gh`，否则提示 `GitHub CLI is not logged in. Run 'gh auth login' first.` |
| `radius` | 只上传到 Radius，不回退 gist | 未登录时提示 `Radius authentication is not configured. Run '/login radius' first.` |

gist 成功后，查看器链接按以下顺序生成：

1. `KIANA_SHARE_VIEWER_URL`（非空即使用）；
2. 否则 `KIANA_API_BASE_URL` + `/session/`；
3. 两者都没有时，只显示 gist 链接，并提示 `Share viewer URL is not configured (set KIANA_SHARE_VIEWER_URL or KIANA_API_BASE_URL).`

链接格式是 `{base}#{gist_id}`。`KIANA_SHARE_VIEWER_URL` 原样使用，因此末尾有空白会生成不可用的链接。

Radius 分支不使用查看器 URL：上传响应里的 `artifact.canonical_url` 会直接显示为分享链接。

限制：

- `/share` 只在交互模式可用；非交互模式没有等价命令。
- gist 分支会执行 `gh auth status`，需要 `gh` 在 `PATH` 中且已登录。
- Radius 上传使用内置默认网关地址；`KIANA_RADIUS_GATEWAY` 不改变 `/share` 的上传地址。

## 离线模式

### 启用方式

```bash
KIANA_OFFLINE=1 kiana
# 或
kiana --offline
```

`--offline` 与 `KIANA_OFFLINE=1` 等价。启动器识别的真值是 `1` / `true` / `yes`（大小写不敏感，不做 trim）；空串和其他值是 false。

离线模式会向进程环境写回两个变量：

- `KIANA_OFFLINE=1`
- `KIANA_SKIP_VERSION_CHECK=1`

bash 等直接子进程会继承它们。MCP stdio 服务器默认只继承 `PATH`、`HOME`、`SHELL`、`LANG`、`LC_*`、`TMPDIR` 与代理变量，不会自动继承 `KIANA_OFFLINE`。

### 离线时被跳过的操作

| 操作 | 离线行为 |
| --- | --- |
| 交互式启动与 RPC 后台的模型目录刷新 | 跳过；运行时只使用内置与已缓存模型 |
| 最新版本检查 | 跳过；不显示新版本提示 |
| 安装遥测上报 | 不发送 |
| IDE 自动连接 | 跳过（连接本身是 loopback HTTP） |
| 交互式包更新检查 | 返回空列表，不提示包更新 |
| 缺失扩展包的自动安装、临时 git 源刷新 | 跳过 |
| 托管工具（`fd`、`ripgrep`）下载 | 跳过，并提示 `Offline mode enabled, skipping download.` |

### 离线时仍然可用的功能

离线模式关闭的是**启动期与后台网络操作**，不是全部网络：

- 模型请求照常发出；`--offline` 不会把模型调用改成本地推理。
- MCP 服务器（stdio 或 HTTP）照常连接。
- `web_search` / `web_fetch` 等显式工具照常联网。
- 显式命令不受启动器拦截，例如 `kiana install <source>`、`kiana update --models`、`/share`。其中 `kiana update --models` 会显式打开网络刷新模型目录。
- 配置了 OpenTelemetry 端点时，会话结束仍会尝试导出（见下一节）。

### 注意

- 本页说“`KIANA_OFFLINE` 为真”时，启动器按 `1`/`true`/`yes` 判断；安装遥测、版本检查、模型目录刷新等内部路径按“变量非空”判断。为避免差异，请只用 `KIANA_OFFLINE=1` 开启，并用 unset 关闭。
- 不要用 `KIANA_OFFLINE=0` 期望“完全恢复联网”：启动器把它当 false，但部分内部检查按“变量存在/非空”判断，行为并不一致。要恢复联网，请 unset 该变量。
- `KIANA_OFFLINE` 只影响当前进程及其子进程；不会写入 settings。

## OpenTelemetry 导出（高级）

`--help` 没有列出这些变量。Kiana 支持用标准 OpenTelemetry 环境变量把会话追踪/指标导出到 OTLP collector；不配置端点时遥测只在内存中，不产生网络请求。

| 环境变量 | 作用 | 默认 |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | collector 地址 | 无；未设置则不导出 |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | traces 专用地址，优先于通用 endpoint | 无 |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf`（默认）、`http/json`、`grpc` | `http/protobuf`；未知值回退到它 |
| `OTEL_EXPORTER_OTLP_HEADERS` | 逗号分隔的 `key=value` 请求头 | 空 |
| `OTEL_SERVICE_NAME` | 资源属性 `service.name` | `kiana` |
| `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | 设为 `true`（大小写不敏感，可带首尾空白）才捕获提示词/工具参数与结果 | 关闭 |

示例：

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf \
kiana
```

限制与说明：

- 导出发生在会话收尾 `flush` 时，失败只记录在导出结果里，不影响 agent 主流程。
- 默认不记录提示词与工具内容；只有显式设置捕获变量才记录，开启前请确认数据敏感性。该开关在会话遥测构造时读取一次，修改后需要重启会话。
- OTLP 编码是子集，当前没有 gRPC 端到端测试；需要严格验证时优先使用 `http/protobuf`。
- `KIANA_OFFLINE` 不会关闭已配置的 OTLP 导出；需要完全离线时不要设置 OTLP endpoint。

## 限制与排查

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| 安装遥测从未发出 | 非交互启动；不是全新安装/新版本；`KIANA_OFFLINE` 为真；开关关闭；未配置 `KIANA_API_BASE_URL` | 用交互模式启动并确认上述条件；临时设 `KIANA_TELEMETRY=1` 排除设置问题 |
| `KIANA_TELEMETRY=on` 没有开启遥测 | 真值只认 `1`/`true`/`yes`；`on` 属于非真值 | 改成 `KIANA_TELEMETRY=1` |
| 想只关遥测但保留其他联网 | 需要区分两个开关 | 使用 `KIANA_TELEMETRY=0`，不要用 `KIANA_OFFLINE` |
| 离线启动后仍有网络请求 | 模型请求、MCP、web 工具、显式命令、OTLP 都不受 `--offline` 拦截 | 按功能逐项关闭；需要断网时同时移除相关端点和命令 |
| 自建服务下模型目录不更新 | `KIANA_API_BASE_URL` 未配置、URL 不可达，或依赖 RPC 后台刷新时处于离线模式 | 检查端点；显式运行 `kiana update --models` 会主动联网刷新 |
| `/share` 没有查看器链接 | `KIANA_SHARE_VIEWER_URL` 与 `KIANA_API_BASE_URL` 都未配置 | 配置其一；命令仍会显示 gist 链接 |
| Radius 分享提示未登录 | 目标为 `radius` 或 `auto` 且没有 Radius 凭据 | 运行 `/login radius`，或把 `sessionShareDestination` 设为 `gist` |
| gist 分享提示未登录 | `gh` 未安装或未登录 | 安装 GitHub CLI 并运行 `gh auth login` |
| MCP OAuth 提示 CIMD 不可用 | 未配置 `KIANA_API_BASE_URL` | 配置 API base，或改用 DCR 注册方式 |
| 托管更新提示不可用 | 未配置 `KIANA_INSTALLER_API_BASE` 与 `KIANA_API_BASE_URL` | 设置其中任意一个 |
| `enableAnalytics` 打开后没有效果 | 当前实现没有匿名用量上传路径 | 这是已知限制；不要把它当作已生效的上报开关 |

版本与离线相关的启动问题见 [故障排查](troubleshooting.md)。

## 相关页面

- [故障排查](troubleshooting.md)
- [配置与设置](settings.md)
- [权限与审批](permissions.md)
- [MCP 服务器](mcp.md)
- [Daemon 事件服务](daemon.md)
- [CLI 参考](cli.md)
