# Kiana

Kiana 编程代理的 Rust 实现。工作区包含 32 个 crate，主程序是 `kiana`。

## 文档

- 用户文档（安装、CLI、TUI、权限、MCP、hooks、沙箱等）：[docs/guide/README.md](docs/guide/README.md)
- 架构与开发文档入口：[docs/START_HERE.md](docs/START_HERE.md)

## 构建

需要 Rust 工具链（edition 2024）。在仓库根目录：

```bash
cargo build --release -p kiana-coding-agent
```

构建产物：

- `target/release/kiana`：交互式 / 非交互式主程序
- `target/release/kiana-rpc`：RPC 入口

## 运行

```bash
./target/release/kiana
```

配置文件默认位于 `~/.kiana/agent`，会话位于 `~/.kiana/agent/sessions`。可以用环境变量覆盖：

```bash
KIANA_CODING_AGENT_DIR=/path/to/agent \
KIANA_CODING_AGENT_SESSION_DIR=/path/to/sessions \
./target/release/kiana
```

## 网络端点与品牌隔离

Kiana 不内置任何上游服务域名，也不会回退到外部 Kiana 站点。Kiana 自建服务统一由
`KIANA_API_BASE_URL` 配置：

| 功能 | 未配置时的行为 |
|---|---|
| 远端模型目录刷新 | 只使用内置与已缓存模型 |
| 最新版本检查 | 跳过检查 |
| 安装遥测 | 跳过上报 |
| MCP `cimd` OAuth 元数据 | 明确报错并提示改用 `dcr` 或配置端点 |
| `/share` 查看器链接 | 仍可创建 gist，但提示查看器 URL 未配置 |
| 托管更新 | 要求 `KIANA_INSTALLER_API_BASE` 或 `KIANA_API_BASE_URL` |

示例：

```bash
export KIANA_API_BASE_URL=https://your-kiana-service.example
```

派生端点：

- 模型目录：`$KIANA_API_BASE_URL/api/models/...`
- 版本检查：`$KIANA_API_BASE_URL/api/latest-version`
- 安装遥测：`$KIANA_API_BASE_URL/api/report-install`
- MCP CIMD：`$KIANA_API_BASE_URL/oauth/...`
- 分享查看器：`$KIANA_API_BASE_URL/session/`
- 托管安装器：`$KIANA_API_BASE_URL/api/installer/releases`（或 `KIANA_INSTALLER_API_BASE`）

完全离线运行：

```bash
KIANA_OFFLINE=1 ./target/release/kiana
```

Radius 网关可通过 `KIANA_RADIUS_GATEWAY` 覆盖；未登录 Radius 时不会发起请求。

## 测试

```bash
cargo test --workspace
```
