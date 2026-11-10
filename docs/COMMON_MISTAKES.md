# 常见理解误区

下面每一条都是**新人读这个仓库时最容易产生的误解**。每条给出正确理解与证据位置。
若你发现某条已经过时，请修正本文并在 `COMMENT_PROGRESS.md` 记录。

---

## 1. “源码里到处写 `TS 源：packages/...`，是不是移植没清理干净？”

不是。Kiana 的产品目标是与 TypeScript 参照实现**可观察行为一致**：相同的错误文案、
事件顺序、JSON 键序、落盘格式、默认值。这些注释是**保真溯源**：告诉你这段代码在
复刻哪个 TS 文件，改动前该去对照哪份 `spec/*.md` 与哪组测试。

正确做法：修改行为前先读对应的 `spec` 与测试；不要因为“看起来像遗留”就删除溯源注释。

---

## 2. “JSON 输出直接用 `serde_json::to_string` 就行吧？”

不行。`serde_json` 与 JavaScript 的 `JSON.stringify` 在四处不同：

- 数字格式：`1e+21`、`1e-7`、`-0` → `"0"`、`NaN`/`Infinity` → `null`；
- 对象键序：JS 整数键优先，再按插入序；
- 字符串转义与 UTF-16 行为；
- 错误消息文本。

凡是会写入磁盘、发给模型或进入协议流的 JSON，必须走 `kiana-json`
（`stringify`、`format_js_number`、`sort_js_keys`）。证据：
`crates/kiana-json/src/lib.rs` 模块文档、仓库根 `ARCHITECTURE.md` §4.3。

---

## 3. “Rust 的 `char` 就是字符，按它算长度就行吧？”

JS 的字符串按 **UTF-16 code unit** 计数与切片，Rust 的 `str` 是 UTF-8 字节，
`chars()` 是 Unicode 标量。对 emoji 等非 BMP 字符，两者计数不同。凡是复刻
`string.length`/`slice`/`substring`/`padStart` 的地方，必须按 UTF-16 计数并避免拆开
代理对。证据：仓库根 `ARCHITECTURE.md` §4.6、`kiana-json/src/js_parse.rs` 的模块文档
（错误位置按 UTF-16 计数）。

---

## 4. “异步任务用 `tokio::spawn` 就行吧？”

主程序运行在 **current-thread + `LocalSet`** 上，扩展运行时（rquickjs）与部分会话
状态是 `!Send`。**持有这些状态的任务**必须用 `spawn_local`（或 `run_on_session_thread`
投递回会话线程）；`tokio::spawn` 只适用于 `Send` future（仓库里确实也有这种用法）。
放错地方会直接编译失败，或迫使你错误地给状态加锁。注意：在 current-thread runtime 上
`tokio::spawn` 仍在本线程执行，选错 API 的代价是 `Send` 约束，不是线程切换。
证据：`crates/kiana-coding-agent/src/main.rs`、仓库根 `ARCHITECTURE.md` §4.2。

---

## 5. “事件流结束了就代表成功；失败会发一个错误事件吧？”

`agent_loop` 返回的流在循环**正常结束**时才 `end()`；循环内部失败（provider 报错、
`convertToLlm` panic 等）时任务直接退出，**不 end、不发错误事件**，`result()` 永远
pending。这与 TS 没有 `catch` 的行为一致。调用方必须同时处理失败与取消路径。
证据：`crates/kiana-agent-core/src/agent_loop.rs` 顶部文档与 `run_agent_loop`。

---

## 6. “并行工具调用的结果顺序应该和事件顺序一致吧？”

并行批次的语义是：

- `tool_execution_start` / prepare 按**源顺序**；
- 真正执行并发；
- 结果消息按**源顺序**收集；
- `tool_execution_end` 事件按**完成顺序**发出。

这是刻意保留的 TS 语义，不要“顺手”统一顺序。
证据：`agent_loop.rs` 的 `execute_tool_calls_parallel` 与 `spec/agent-codemode.md` §8.2。

---

## 7. “Rust 里传的是值，工具参数改了就改了吧？”

TS 的 assistant 消息、`newMessages` 与 `currentContext.messages` 引用同一个对象，
`prepareArguments` 的就地改写对三者同时可见。Rust 是值语义，所以工具批次执行后要调用
`sync_assistant_message` 把改写写回上下文与本轮返回值。删掉它会改变模型看到的参数。
证据：`agent_loop.rs` 的 `sync_assistant_message` 注释。

---

## 8. “AgentSession 和 Agent 不是一回事吗？为什么都有 subscribe/state？”

- `Agent` 是**循环**：消息、模型、工具、steering 队列、事件订阅；
- `AgentSession` 是**业务门面**：配置、认证、持久化、压缩、扩展、资源加载，
  并持有 `Agent`。

需要“跑一轮对话”时用 `AgentSession::prompt`；需要直接驱动循环或做 SDK 集成时用
`Agent`。证据：`kiana-session/src/agent_session.rs`、`kiana-agent-core/src/agent.rs`。

---

## 9. “错误加一层 `anyhow` 上下文不是更好调试吗？”

本仓库**禁止在库代码使用 `anyhow`**。公开错误的 `Display` 必须逐字等于 TS 的
`error.message`，而 `anyhow` 的上下文包装会改变文本。跨 crate 错误转换写在错误定义
所在的 crate（`impl From<X> for Y`），不要在调用方拼字符串。
证据：仓库根 `ARCHITECTURE.md` §4.1。

---

## 10. “IndexedDB / Cloudflare 的模块是没写完的 TODO 吧？”

不是。它们是有意的**产品边界**：

- IndexedDB 后端构造即返回 `StorageBackendUnsupported::IndexedDb`；
- Cloudflare Workers/Durable Object 适配保留 feature，但调用即返回明确的
  “not supported in the Rust build” 错误。

这些行为有测试锁定，不要用 sqlite 去“顺便实现”它们。证据：仓库根 `ARCHITECTURE.md` §8 第 2、3 条，
`kiana-durable/src/storage/indexeddb.rs` 等占位模块的文档。

---

## 11. “关闭 feature 只是减小体积，功能应该差不多吧？”

不完全是。`kiana-extensions` 关闭 `runtime` 后会退化为 `kiana-ext-api` 的再导出面，
扩展加载器/JS 宿主整体消失；`kiana-codemode` 关闭 `wasm` 会失去默认执行后端；
`kiana-durable` 的 `sqlite` 关闭后对应后端不可用。条件编译改变的是**公开面**，
不只是体积。证据：各 crate `Cargo.toml` 的 `[features]` 注释。

---

## 12. “测试失败了就重新生成黄金文件吧？”

不要。黄金测试（`crates/*/tests/golden/`、`*.snap`、fixture）用参照实现的真实输出锁定行为；
在 CI 里运行 Node 重新生成会掩盖回归。正确流程：先判断是代码错了还是预期变了；
预期变更必须同时更新 `spec` 与 `COMMENT_PROGRESS.md` 的说明。
证据：仓库根 `ARCHITECTURE.md` §4.8。

---

## 13. “`KIANA_API_BASE_URL` 不设置应该会回退到官方服务吧？”

不会。Kiana 不内置任何上游服务域名。未配置时：远端模型目录只用缓存、版本检查跳过、
安装遥测跳过、MCP CIMD 明确报错、托管更新要求显式端点。证据：根 `README.md`
“网络端点与品牌隔离”一节。

---

## 14. “单线程 crate 里用 `Mutex` 是多余的，用 `RefCell` 就行吧？”

单线程指的是**任务不跨线程**，不等于没有重入/借用冲突。仓库里使用 `Mutex` 的地方
各有原因（共享句柄、跨回调访问、需要忽略中毒后继续使用），不能一概替换。
看到 `std::sync::Mutex` 时先确认：谁持有它、是否跨 `.await`、是否存在重入路径；
确认后再决定能否改成 `RefCell`。证据：`kiana-session/src/agent_session.rs` 的辅助
`lock()`、扩展 runner 中的锁使用；`docs/COMMENT_STYLE.md` §4.4 对并发注释的要求。

---

## 15. “注释里说‘线程安全’‘性能更好’，应该可以直接信吧？”

本仓库的写作规范要求：没有基准数据不写“性能更好”，没有证明不写“线程安全”。
如果你看到这类结论但没有测试/代码依据，请把它列入 `COMMENT_PROGRESS.md` 的待核实清单。
证据：`docs/COMMENT_STYLE.md` §5、§8。

---

## 16. “`cargo test --workspace` 是最稳妥的验证方式吧？”

它非常昂贵：测试二进制数量极多，`[profile.test]` 专门关闭了调试信息以避免占满磁盘。
日常改动先用 `cargo test -p <crate>`；只有在合并/发布前才跑全量。
证据：根 `Cargo.toml` 的 `[profile.test]` 注释、`docs/START_HERE.md`。
