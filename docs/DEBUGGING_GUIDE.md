# 调试指南

> 目标：出问题时能在**正确的层**定位，而不是盲目 `cargo test --workspace`。
> 文中命令均可直接执行；行为描述以当前源码为准。

---

## 1. 先收集四件事

1. **复现命令**：完整 argv、环境变量（尤其 `KIANA_*`）、工作目录；
2. **模式**：interactive / print / json / rpc（`--mode` 或 `-p`）；
3. **退出码**：`kiana ...; echo $?`；
4. **stdout 与 stderr 分开保存**：很多协议/渲染问题只在其中一路可见。

```bash
kiana -p "..." >/tmp/out.txt 2>/tmp/err.txt; echo "exit=$?"
```

启动阶段耗时（TS 的 `KIANA_TIMING` 等价物）：

```bash
KIANA_TIMING=1 kiana --help 2>&1 | tail -40
```

`kiana_core::timings` 只在该环境变量为 `"1"` 时记录并打印。

---

## 2. 按层定位

| 现象 | 优先检查 | 代码入口 |
|---|---|---|
| 编译失败 | 工具链版本、feature、C 工具链、build.rs | `Cargo.toml`、各 crate `build.rs` |
| 启动即退出 | 参数诊断、认证、模型、信任、端点 | `app.rs` 的 `run_main` |
| 模型请求失败 | 代理、TLS、认证、SSE、超时 | `kiana-ai/src/http/`、`src/api/` |
| 工具行为异常 | 路径解析、权限/信任、输出截断、沙箱 | `kiana-tools`、`kiana-builtin-extensions` |
| TUI 花屏/输入异常 | 终端能力、备用屏、resize、转义序列 | `kiana-tui`、`kiana-interactive` |
| 扩展加载失败 | `ExtensionHost` 生命周期、loader、JS 栈 | `kiana-extensions` |
| 会话丢失/格式错误 | JSONL 写入、键序、cwd、迁移 | `kiana-session-core` |
| 卡死/不返回 | 事件流未 end、取消未传播、锁重入 | `agent_loop.rs`、`local` set |
| 崩溃 | panic 钩子、崩溃日志 | `setup.rs`、`kiana-resources/src/crash_log.rs` |

---

## 3. 构建问题

### 3.1 工具链

- edition 2024，`rust-version = "1.97"`；用 `rustc --version` 确认。
- workspace 的依赖是**精确 pin**；不要为了让某个依赖“更新一点”而改版本，
  这会破坏与 TS 的字节级对齐。

### 3.2 扩展运行时与 C 工具链

`kiana-extensions` 默认启用 `runtime`（rquickjs + oxc）。若构建时报 C 编译器相关错误，
先确认 `cc`/`clang` 可用；若只想编译其他 crate：

```bash
cargo check -p kiana-extensions --no-default-features
cargo check -p kiana-coding-agent --no-default-features
```

`--no-default-features` 会改变公开面（扩展运行时消失），只用于定位构建问题。

### 3.3 模型目录 build.rs

`kiana-ai/build.rs` 读取模型目录数据：优先 `KIANA_MODEL_DATA_DIR`，否则用仓库内
`crates/kiana-ai/data/`。因此普通 `cargo build` 不需要 Node。若报找不到数据，
检查该目录是否存在、`data/README.md` 的校验方式。

### 3.4 磁盘

测试二进制非常多。`[profile.test]` 关闭了 debuginfo 以避免 >100 GB 的构建产物。
如果磁盘吃紧：只跑受影响 crate 的测试；不要反复全量构建。

---

## 4. 启动问题

`run_main`（`crates/kiana-coding-agent/src/app.rs`）按编号步骤执行。常见失败：

| 错误/现象 | 原因 | 处理 |
|---|---|---|
| `Error: ...` + 退出码 1 | 参数诊断或启动期致命错误 | 看 stderr 原文；`FatalStartupError` 由 panic 钩子映射成 Node 风格 |
| `no models available` | 未配置凭据/模型，或远端目录不可用 | 配置 auth；或用 `--model`；离线时用内置/缓存模型 |
| 一直询问信任 | 项目未受信任 | 用交互确认，或检查 `ProjectTrustStore` / `--project-trust` 覆盖 |
| `Extension runtime engine is not installed` | `ExtensionHost` 未创建/创建太晚/被提前 drop | 确认入口先创建宿主；不要在 `main` 前做扩展加载 |
| 会话 cwd 不存在 | 会话文件记录的目录被移动/删除 | 交互模式会提示重选；非交互模式报错退出 |
| 端点相关报错 | `KIANA_API_BASE_URL` 未配置 | 这是有意的：不内置上游域名；按 README 配置 |

`--offline` / `KIANA_OFFLINE=1` 会设置 `KIANA_SKIP_VERSION_CHECK=1` 并跳过网络刷新。

---

## 5. 模型/网络问题

### 5.1 先区分“请求没发出去”和“响应解析失败”

- 请求没发出去：认证、代理、DNS、TLS、超时；
- 发出去了但失败：HTTP 状态、SSE 分块、JSON 解析、事件映射、取消。

`kiana-ai` 的 provider 适配器把厂商响应翻译成统一的 `AssistantMessageEvent`；
错误文本必须与 TS 的 `error.message` 一致。若错误文本“看起来像 reqwest 的”，
说明该路径错误映射没做对，属于 bug 而非环境问题。

### 5.2 代理

`kiana-core::http_dispatcher` 统一配置代理与空闲超时；启动时先应用 bootstrap 设置，
之后设置加载完再应用一次。若代理只在某些请求生效，检查这两次调用之间的路径。

### 5.3 用测试复现协议问题

provider 的流式行为有 `wiremock` 测试与黄金 fixture：

```bash
cargo test -p kiana-ai --test <相关测试> -- --nocapture
```

不要用真实 API key 跑测试；测试禁止真实网络（仓库根 `ARCHITECTURE.md` §4.8）。

---

## 6. 工具问题

- **路径**：工具参数里的相对路径基于会话 cwd；`kiana-core::paths` 做词法规范化，
  不碰文件系统（除少数函数）。
- **权限/信任**：项目未受信任时，项目级配置与扩展不会加载；某些工具可能被拦截。
- **输出**：大输出会被截断/落盘（见 `kiana-tools/src/truncate.rs`、
  `output_accumulator.rs`），模型看到的是截断后的文本。
- **bash/powershell**：环境变量会被注入 `KIANA_SESSION_ID` 等；`!`/`!!` 命令不注入。
- **编辑冲突**：文件修改有队列（`file_mutation_queue.rs`）；并发编辑同一文件的顺序
  由该队列保证。

---

## 7. TUI 问题

- **非 TTY**：print/json/rpc 模式不初始化 TUI；若在管道里跑 interactive 会异常。
- **终端能力**：颜色、超链接、图片协议、剪贴板都有环境变量覆盖
  （见仓库根 `ARCHITECTURE.md` §4.5 的终端行为变量）。
- **备用屏**：`alt_screen.rs`；异常退出后终端未恢复时，用 `reset` 或重新登录。
- **panic**：交互模式安装了自己的 panic 钩子（`interactive_mode/lifecycle.rs`），
  会尽量恢复终端并写崩溃日志。
- **崩溃日志**：`~/.kiana/agent/crashes.json`（`kiana_resources::crash_log::crash_log_path`），
  最多保留 5 条、7 天有效；`/bug` 会读取并附加。

---

## 8. 扩展问题

1. 确认 `ExtensionHost` 已创建且存活（见 §4）；
2. 看加载器错误：TS 路径解析、去类型、CJS 转换、虚拟模块；
3. JS 异常会带 JS 栈；`kiana-interactive` 会尝试把栈映射回扩展源码；
4. 事件顺序：扩展的 `on(...)` 回调与代理事件同线程分发；如果事件“丢了”，先确认
   扩展是否在加载完成前注册、是否被 `dispose` 取消；
5. 关闭：扩展运行时在 Drop 时显式关闭；不要在回调里 `tokio::spawn` 持有 JS 值。

---

## 9. 会话与持久化问题

- 会话文件是一行一个 JSON；键序与数字格式由 `kiana-json` 保证；
- 写入失败会作为运行期持久化错误返回，不会静默；
- 旧会话打不开：先看 `kiana-coding-agent/src/migrations.rs` 是否有对应迁移；
- 会话 cwd 丢失：见 §4；
- 不要手工编辑 JSONL 后期待键序稳定；需要修复时用程序读写并保持原有键序。

---

## 10. 卡死与并发问题

### 10.1 `result()` 永不返回

最常见原因：agent loop 内部失败或 panic，流没有 `end()`。检查：
- provider 是否返回错误；
- `convertToLlm`/钩子是否 panic；
- 是否在等待一个已经放弃的任务。

### 10.2 取消没有生效

取消是 `CancellationToken` 的**协作式**传播：工具/请求必须主动检查。检查：
- `stream_fn` 是否把 signal 传给了 HTTP 客户端；
- 工具批次是否在每次调用后 `is_aborted`；
- 扩展回调是否阻塞了单线程（会拖住取消）。

### 10.3 借用/重入 panic

单线程 crate 里常见 `RefCell` 重入或 `Mutex` 嵌套。注意：
- 本仓库的 `lock()` 辅助通常忽略 poisoning（`unwrap_or_else(|p| p.into_inner())`），
  所以一次 panic 不会让后续锁全部失败，但状态可能不一致；
- 不要在持锁时调用可能重入的用户回调。

### 10.4 看不到 `tracing` 日志

库代码使用 `tracing::warn!` 等，但 **2026-11-05 审计时 workspace 源码中没有找到任何
`tracing_subscriber` 安装点**（全仓 0 处引用）。没有 subscriber 时 tracing 事件会被丢弃，
因此默认情况下这些日志不输出；若日志不出现，先确认是否有依赖偷偷安装了 subscriber，
不要把“没有日志”当成“没有发生”。（这项已确认并保留在 `COMMENT_PROGRESS.md` 的待核实清单中：
是否应安装 subscriber 属于产品决策，本次注释改造不修改代码。）

---

## 11. 常用命令

```bash
# 只检查受影响的 crate
cargo check -p kiana-agent-core --all-targets
cargo test -p kiana-json
cargo test -p kiana-ai --test <name> -- --nocapture

# 文档构建（检查 intra-doc link / 私有项链接 / 代码块）
RUSTDOCFLAGS='-D warnings' cargo doc -p <crate> --no-deps

# 格式检查（不要自动格式化）
cargo fmt --all -- --check

# 全量（昂贵）
cargo test --workspace
```

调试 panic：

```bash
RUST_BACKTRACE=1 kiana -p "..." 2>&1 | tail -80
```

---

## 12. 不要做的事

- 不要为了“先跑起来”把公开错误换成 `anyhow` 或改错误文案；
- 不要重新生成黄金文件来让测试变绿；
- 不要在库代码里加 `unwrap()`/`expect()` 除非能写清不变量；
- 不要用 sqlite 实现明确声明“不支持”的平台后端；
- 不要在未确认归属时回滚工作区里的用户改动。
