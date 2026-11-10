# 快速开始

本页讲什么：从本仓库的 `dist/v0.1.0-alpha` 产物启动 `kiana`，把它放进 `PATH`，完成第一次认证，
然后进行第一次交互式会话、一次性 `-p` 运行，并选择常用的权限模式。

## 版本说明

> 本仓库 workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`。后者来自
> `kiana-core` 的 TS 兼容常量（`crates/kiana-core/src/config.rs` 中的 `VERSION`），是有意保留的
> 差异，不是构建错误；`v0.1.0-alpha` 只是本地产物目录名，不是 git tag。

当前验证范围是 Linux x86_64。仓库里没有 macOS/Windows 产物，相关行为未实测。

## 1. 运行二进制

### 直接运行

在仓库根目录执行：

```bash
cd /home/ctyun/code/kiana
./dist/v0.1.0-alpha/kiana --version
# 1.4.0
```

`kiana --help` 会打印完整选项；子命令帮助用 `kiana <command> --help`。

产物目录里还有 `kiana-rpc`。它是强制以 `--mode rpc` 启动的入口，供编辑器插件和脚本集成使用；
用法见 [CLI 参考](cli.md#输出模式)。

### 校验产物（可选）

```bash
cd /home/ctyun/code/kiana/dist/v0.1.0-alpha
sha256sum -c SHA256SUMS
# kiana: 成功
# kiana-rpc: 成功
```

### 放到 PATH

临时把产物目录加入当前 shell：

```bash
export PATH="/home/ctyun/code/kiana/dist/v0.1.0-alpha:$PATH"
hash -r
kiana --version
# 1.4.0
```

长期使用可以把同一行写进 `~/.bashrc` / `~/.zshrc`，或者做符号链接：

```bash
mkdir -p ~/.local/bin
ln -sf /home/ctyun/code/kiana/dist/v0.1.0-alpha/kiana ~/.local/bin/kiana
```

确保 `~/.local/bin` 在 `PATH` 里。之后直接运行 `kiana`。

### 从源码构建（可选）

```bash
cd /home/ctyun/code/kiana
cargo build --release -p kiana-coding-agent
./target/release/kiana --version
```

构建耗时较长，日常使用本地产物即可。

## 2. 首次认证

`kiana` 需要先有一个可用的模型和对应的 provider 凭据。凭据有两个来源：

1. provider 的 API key / token 环境变量；
2. 交互模式里的 `/login <provider>`，凭证保存到 `<agentDir>/auth.json`
   （默认 `~/.kiana/agent/auth.json`）。

### 2.1 用环境变量认证

以 Anthropic 为例：

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
kiana auth check --provider anthropic --no-refresh
# ready
```

`kiana auth check` 的退出码和文本输出是一一对应的：

| 输出 | 含义 | 退出码 |
| --- | --- | --- |
| `ready` | 找到可用凭据 | `0` |
| `not_ready` | provider 存在，但没有配置凭据 | `1` |
| `invalid` | 运行时/读取失败，无法判断 | `2` |

加上 `--json` 便于脚本读取：

```bash
kiana auth check --provider anthropic --no-refresh --json
# {"status":"ready","provider":"anthropic","authType":"api_key"}
```

缺少凭据时：

```bash
kiana auth check --provider anthropic --no-refresh --json
# {"status":"not_ready","provider":"anthropic","reason":"credentials_not_configured"}
```

`--no-refresh` 表示不刷新过期 OAuth。默认情况下 `check` 会刷新过期 OAuth；不希望在检查时产生
网络请求就显式加上它。

`--credentials` 让 `check` 把凭据也打印出来（或在 JSON 中包含 `credentials` 字段）。注意不要把
带凭据的输出贴到日志里。

### 2.2 打印凭据给外部客户端

`auth print-api-key` / `auth print-bearer-token` 会把“真实请求会使用的凭据”原样写到 stdout，
适合给外部客户端使用：

```bash
kiana auth print-api-key --provider anthropic
kiana auth print-bearer-token --provider anthropic --min-expiry 1h
```

`--min-expiry` 只用于 `print-bearer-token`，取值形如 `30m`、`1h`，默认 `30m`。两个打印命令都要求
至少给出 `--provider` 或 `--model`。

### 2.3 交互式登录

进入交互模式后：

```text
/login anthropic
```

按提示完成认证。`/logout` 删除已保存的 provider 认证。所有 `/login` 写入的凭据都在
`~/.kiana/agent/auth.json`；可用 `KIANA_CODING_AGENT_DIR` 改变 agent 目录。

### 2.4 常见 provider 环境变量

| Provider | 环境变量 |
| --- | --- |
| Anthropic | `ANTHROPIC_API_KEY`、`ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_OAUTH_TOKEN` |
| OpenAI | `OPENAI_API_KEY` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY`，配合 `AZURE_OPENAI_BASE_URL` 或 `AZURE_OPENAI_RESOURCE_NAME`；可选 `AZURE_OPENAI_API_VERSION`、`AZURE_OPENAI_DEPLOYMENT_NAME_MAP` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| Groq | `GROQ_API_KEY` |
| xAI | `XAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Mistral | `MISTRAL_API_KEY` |
| Amazon Bedrock | `AWS_PROFILE`、`AWS_ACCESS_KEY_ID`、`AWS_SECRET_ACCESS_KEY`、`AWS_BEARER_TOKEN_BEDROCK`、`AWS_REGION` |
| Cloudflare | `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID`；AI Gateway 还需要 `CLOUDFLARE_GATEWAY_ID` |

完整列表（包括 Cerebras、Fireworks、Together、Baseten、ZAI、MiniMax、Moonshot、OpenCode、Kimi、
Meta、Qwen Token Plan、Xiaomi MiMo 等）见 `kiana --help` 的 Environment Variables 段，或
[CLI 参考](cli.md#环境变量)。

### 2.5 一次性 API key

不想改环境变量时，可以用 `--api-key` 把 key 传给本次运行；它不会写入磁盘：

```bash
kiana --model anthropic/claude-sonnet-4-5 --api-key "sk-ant-..." -p "你好"
```

`--api-key` 没有默认值；不写时从 provider 对应的环境变量读取。

### 2.6 认证检查失败怎么排查

| 现象 | 排查 |
| --- | --- |
| `not_ready` + `credentials_not_configured` | 环境变量没有导出到当前进程，或 provider 名称拼错。先在同一 shell 里用 `env` 查看 `API_KEY`，再重跑 `kiana auth check --provider <provider> --no-refresh --json` |
| `ready` 但请求模型仍失败 | 模型目录里没有该模型，或所选模型不属于该 provider。先 `kiana --list-models` 确认可用模型 |
| `invalid` | 读取 auth 文件或模型运行时失败；检查 `~/.kiana/agent` 权限与 JSON 文件是否损坏 |

## 3. 第一次交互

先确认至少有一个可用模型：

```bash
kiana --list-models
```

然后在一个项目目录里启动：

```bash
cd ~/code/my-project
kiana
```

进入交互界面后：

| 操作 | 说明 |
| --- | --- |
| 输入文字后按 Enter | 发送消息给模型 |
| `Ctrl+D`（编辑器为空时） | 退出 `kiana` |
| `Ctrl+C` | 清空当前编辑器内容 |
| `!<command>` | 在 shell 中执行命令，结果进入上下文 |
| `!!<command>` | 执行命令，但把该命令排除出上下文 |
| `@path` | 把文件内容（图片则作为图片附件）加入消息 |

常用斜杠命令：

| 命令 | 作用 |
| --- | --- |
| `/model` | 选择模型 |
| `/settings` | 打开设置菜单 |
| `/login <provider>` | 配置 provider 认证 |
| `/trust` | 保存当前项目的信任决定 |
| `/session` | 查看当前会话信息与统计 |
| `/compact` | 手动压缩会话上下文 |
| `/quit` | 退出 `kiana` |

会话会自动保存（`--no-session` 除外）。下次可以用 `kiana --continue` 继续当前目录最近的会话，或
`kiana --resume` 从列表中选择；详见 [CLI 参考](cli.md#会话)。

首次在某个项目里运行，如果项目带有项目本地资源（`.kiana/settings.json`、项目扩展等），
`kiana` 可能会询问是否信任该项目。单次运行的信任覆盖用 `--approve` / `--no-approve`；
希望以后都信任，用 `/trust` 或设置 `defaultProjectTrust`。项目未受信时，项目本地设置和
`.kiana/mcp.json` 不会被读取。

如果 stdin 或 stdout 不是终端（例如在 CI、脚本或重定向里运行），`kiana` 会自动退化为非交互的
print 模式；需要明确非交互时请直接使用 `-p`。

## 4. 一次性运行（`-p`）

`--print` / `-p` 处理完 prompt 后退出：

```bash
kiana -p "List all .ts files in src/"
```

提示以 `-` 开头时，先用 `--` 结束选项解析：

```bash
kiana -p -- "- Summarize these points"
```

从管道读取输入：

```bash
cat README.md | kiana -p "Summarize this"
```

`@file` 参数：

```bash
kiana @prompt.md @image.png "What color is the sky?"
```

输出 JSON 事件流（每行一个 JSON 对象）：

```bash
kiana --mode json -p "List the top-level directories" > events.jsonl
```

`-p` 的约定：

- stdout 只包含最终 assistant 文本（text 模式），日志、警告和错误走 stderr；
- 正常结束退出码 `0`；请求出错、被中止、输出达到 token 上限或预算上限时退出码 `1`；
- `--mode json` 输出会话头和完整事件流，适合脚本消费；
- 非交互模式没有审批 UI，见下一节。

### 4.1 选择模型与思考级别

```bash
kiana --list-models                      # 列出可用模型
kiana --model sonnet:high "Solve this"   # 模型 pattern + thinking 简写
kiana --model anthropic/claude-sonnet-4-5
kiana --provider openai --model gpt-4o-mini
kiana --thinking high
kiana --models claude-sonnet,claude-haiku,gpt-4o
```

`--provider` 必须和 `--model` 一起用；单独给 `--provider` 会报错。`--models` 定义 Ctrl+P 可循环的
模型范围，支持 glob（如 `github-copilot/*`）和模糊匹配。

当前如果未配置 `KIANA_API_BASE_URL`，只使用内置与已缓存的模型目录；`kiana --list-models` 可能只
列出内置或已缓存的少量模型。需要更多 provider/model 时，配置模型目录（见 [设置](settings.md)）。

## 5. 常用权限模式

`--permission-mode` 接受四个值：

| 模式 | 行为 |
| --- | --- |
| `read-only` | 只允许读；写、执行命令、网络、MCP 工具全部拒绝 |
| `default` | 读自由；写、执行命令、网络、MCP 工具先询问 |
| `accept-edits` | 工作区内编辑直接允许；工作区外写入、执行命令、网络、MCP 工具先询问 |
| `bypass` | 所有工具调用都放行，不再询问（只在可信环境使用） |

```bash
kiana --permission-mode read-only -p "Review the code in src/"
kiana --permission-mode default
kiana --permission-mode accept-edits
kiana --permission-mode bypass
```

> 当前实现：权限层需要显式启用。`--permission-mode`、settings 里的
> `permissions.mode` / `permissions.defaultMode`、或环境变量 `KIANA_PERMISSIONS` /
> `KIANA_PERMISSION_MODE` 三者都没有时，不会创建权限引擎，工具按引入权限层之前的路径直接执行。
> 需要审批行为时，请显式选择 `default` 或 `accept-edits`。

### 5.1 规则与额外目录

`--allow` / `--deny` 可以重复使用。规则支持 `tool:`、`command:`、`path:` 前缀，也可以直接写
工具名、命令或路径：

```bash
kiana --allow 'command:git status'
kiana --deny 'path:~/secrets'
kiana --allow 'tool:mcp__*'
kiana --add-dir ../shared-lib
```

命令规则只对简单命令做前缀匹配；复合命令、重定向、命令替换会按更严格的规则处理。
`--allow` / `--deny` 只影响当前进程，不会写回 `settings.json`。

### 5.2 配置优先级

```text
--permission-mode / --allow / --deny（命令行）
  > settings 的 permissions 对象
  > KIANA_PERMISSIONS / KIANA_PERMISSION_MODE（环境变量）
```

settings 的 `permissions.mode` / `permissions.defaultMode` 决定该对象里的模式；只要 settings 的
`permissions` 对象生效（包含规则或目录），环境变量就不再参与。`--add-dir` 只追加可写根，不会单独
启用权限层。

环境变量示例：

```bash
KIANA_PERMISSIONS=read-only kiana
```

### 5.3 非交互模式的限制

print/json 模式没有审批 UI：任何 `Ask` 都按拒绝处理，没有 `--yes` 自动批准。

- 只读任务：`--permission-mode read-only`。
- 需要工作区内写文件：`--permission-mode accept-edits`。
- 需要执行命令或访问网络：显式写 `--allow` 规则，或在可信环境使用 `--permission-mode bypass`。

完整规则、持久化和审批作用域见 [权限与审批](permissions.md)。

## 6. 文件与目录

`<agentDir>` 默认是 `~/.kiana/agent`，可用 `KIANA_CODING_AGENT_DIR` 覆盖。

| 用途 | 位置 |
| --- | --- |
| 全局设置 | `<agentDir>/settings.json`（默认 `~/.kiana/agent/settings.json`） |
| 项目设置 | `<cwd>/.kiana/settings.json` |
| 项目本地设置 | `<cwd>/.kiana/settings.local.json`（参与 `permissions`、`hooks` 等分层合并；类型化 Settings 键不读取；仅项目受信任时加载） |
| 会话 | `<agentDir>/sessions/--<cwd>--/<timestamp>_<session-id>.jsonl` |
| 认证 | `<agentDir>/auth.json` |
| MCP | `<agentDir>/mcp.json`；受信项目还会读 `<cwd>/.kiana/mcp.json` |
| 项目配置目录 | `<cwd>/.kiana/` |

会话目录优先级：`--session-dir` > `KIANA_CODING_AGENT_SESSION_DIR` > settings 的 `sessionDir` >
默认值。设置项的完整列表与分层规则见 [设置](settings.md)。

## 7. 限制与排查

| 现象 | 原因 / 处理 |
| --- | --- |
| `kiana: command not found` | 当前 shell 没有执行过 `export PATH=...`，或 `hash -r` 没重跑；重新打开终端或重新导出 |
| `kiana --version` 是 `1.4.0`，但 `Cargo.toml` 是 `0.1.0` | 有意保留的 TS 兼容版本常量，见本页开头的版本说明 |
| 非交互运行提示没有模型并退出 `1` | 先 `kiana --list-models` 确认模型可用，再用 `--model` 指定；`--serve` 在非交互启动时同样要求有可用模型 |
| `auth check` 一直 `not_ready` | 环境变量没有导出到当前进程，或 provider 名称不对；用 `--json` 看 `reason` |
| `-p` 里写文件/执行命令被拒绝 | print 模式没有审批 UI；改用 `accept-edits`、`--allow`，或在可信环境用 `bypass` |
| 启动期网络请求很慢或不可用 | `KIANA_OFFLINE=1 kiana ...` 或 `--offline` 关闭启动期网络操作 |
| `/share` 查看器、远端模型目录等功能不可用 | 没有配置 `KIANA_API_BASE_URL`；`/share` 仍可创建 gist，但没有查看器链接 |
| 项目本地设置不生效 | 项目未受信；用 `--approve` 单次信任，或 `/trust` 持久化 |
| 没有 macOS/Windows 产物 | 当前只构建并验证 Linux x86_64 |

## 相关页面

- [CLI 参考](cli.md)：完整 flag、子命令、退出码与 stdin/stdout 约定
- [设置](settings.md)：settings 键、配置分层与项目信任
- [权限与审批](permissions.md)：审批作用域、规则语法与持久化
- [会话、分支与恢复](sessions.md)：继续、恢复、fork 与导入导出
- [MCP 服务器](mcp.md)：`kiana mcp` 与 `mcp.json`
- [故障排查](troubleshooting.md)：按现象排查启动、权限、daemon 等问题
