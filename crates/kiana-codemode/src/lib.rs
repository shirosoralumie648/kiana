//! 对应 packages/codemode。
//!
//! codemode 是「模型写一小段 JavaScript 调用工具」的执行层：本 crate 负责解析首行的
//! `// @options:` 指令、按 JSON Schema 生成模型可见的工具声明、在 QuickJS（wasm）沙箱里运行
//! 脚本，并把工具调用、输出与结局送回宿主。它是 TS 包 `@earendil-works/kiana-codemode` 的
//! 逐文件移植。
//!
//! TS 源：`packages/codemode/src/**`
//!
//! # 这个 crate 解决什么问题
//!
//! `kiana-builtin-extensions` 的 codemode 工具把用途写成三条：批量独立调用
//! （`Promise.allSettled`）、串联工具、过滤大输出——这些都不该拆成很多次单独的工具调用。
//! codemode 改为让模型提交一小段 JavaScript：调用方把工具集渲染成声明交给模型，脚本里可以
//! 多次调用、`Promise.all`、循环并 `return` 一个值，只有脚本的输出进入模型上下文。
//! 为此需要三件相互独立的东西，也就是本 crate 的三层：
//!
//! - **源格式**（[`source`]）：一行可选的 `// @options:` 指令 + JavaScript 正文；
//! - **声明渲染**（[`declarations`]）：工具 JSON Schema → 模型可见的 TypeScript 文本；
//! - **执行**（[`runtime`]）：宿主持有工具、deadline 与记录，VM 只跑脚本，两者用 JSON 文本
//!   消息通信（[`runtime::protocol`]）。
//!
//! # 在系统中的位置
//!
//! 下游（调用本 crate 的地方）：
//!
//! - `kiana-builtin-extensions` 的 codemode 扩展工具：工具定义（含给模型的声明）在模型调用前
//!   创建，用 [`declarations`] 的渲染函数把工具 schema 变成模型可见的声明；模型返回脚本后，
//!   才把 code 交给 [`parse_codemode_source`]，再选择一个 sandbox 执行
//!   （`crates/kiana-builtin-extensions/src/codemode/{tool,execute}.rs`）。
//! - `kiana-durable` 的 codemode 执行路径：通过 [`CodemodeSandboxBase`] 的 `start()` 做确定性
//!   重放，把每次工具结果先写入 journal 再投递给 VM
//!   （`crates/kiana-durable/src/codemode/codemode.rs`）。
//!
//! 上游（本 crate 依赖的东西）：仓库内只有 `kiana-json`（见 `Cargo.toml` 与 ARCHITECTURE.md
//! 的依赖表）。默认 feature `wasm` 打开 `wasmtime`，用来嵌入与 TS 侧同一份 `quickjs.wasm`；
//! `rquickjs` 是未启用的备选后端；`libc` 只在 unix 上使用：`readFile` 的 errno 文案，以及
//! 本地时区偏移（`localtime_r`，供 Date 的 `getTimezoneOffset()`；确定性模式与非 unix 下恒为 0）。
//!
//! # 核心类型与执行模型
//!
//! - 工具输入契约：[`CodemodeTool`]（名字、输入/输出 JSON Schema、可选的 TS 签名，以及
//!   `execute` 回调）、[`CodemodeToolContext`]（取消令牌）、[`CodemodeExecuteOptions`]
//!   （每次执行的取消信号、deadline 覆盖、`load()` 初值）。
//! - 输出契约：[`CodemodeResult`] 的 `Ok`/`Err` 两支都携带截至结局的输出项与工具调用记录，
//!   成功支还带 `store()` 写入；失败用 [`CodemodeError`]，其 `kind` 是 [`CodemodeErrorKind`]。
//! - 执行器：宿主侧状态机是 [`CodemodeSandboxBase`]（deadline、取消、工具调度、终态裁决）。
//!   三个公开后端共享这套宿主逻辑，差别只在 launcher 的传输与终止方式：
//!   [`InlineCodemodeSandbox`]（靠 HostEventLoop 闸门与中断预算）、[`CodemodeSandbox`]
//!   （每次执行一个独立 worker 线程）与 [`RemoteCodemodeSandbox`]（VM 在另一个 isolate，
//!   经 RPC 来回传递调用）。inline、worker 与 remote 的 serve 端都由 `runtime::vm` 把 VM 起在
//!   专用线程（线程名 `kiana-codemode-vm`），worker 线程只负责转发消息。
//!   `tests/sandbox.rs` 对三个后端跑同一组行为断言。
//! - 确定性入口：`start()` 返回 [`CodemodeRun`]；脚本只在宿主 `deliver` 结果时前进。起点时间
//!   由 [`CodemodeRunOptions`] 的 `started_at` 决定，之后每次时钟读取用宿主在 `deliver()` 的
//!   `now` 参数里传入的值，随机数来自 `seed`；宿主按相同的 `seq`/`now` 序列投递即可重放。
//!
//! # 一次典型的脚本执行
//!
//! 1. 调用方（`kiana-builtin-extensions` 的 codemode 工具定义）把工具集渲染成声明交给模型；
//! 2. 模型返回脚本；
//! 3. 调用方用 [`parse_codemode_source`] 解析脚本，拆出正文与 `timeout_ms`/`max_output_tokens`；
//! 4. 调用方构造某个 sandbox（三个后端之一）；`execute()` 使用调用时工具表的快照（[`CodemodeTool`]
//!    的克隆，含构造后 `register_tool` 注册的工具），确定性 `start()` 改用
//!    [`CodemodeRunOptions`] 自带的工具与全局函数；
//! 5. `execute()`/`start()` 在宿主侧建立一次执行：武装 deadline 与取消监听，选择一个 launcher
//!    启动 VM，并把脚本、工具、全局函数与 store 快照作为启动数据交给它；
//! 6. 脚本调用工具时 VM 发来消息，宿主执行工具、记录调用与输出，再把结果投递回去；终态只有
//!    一个，第一个进入终态裁决的事件获胜，之后的事件被忽略。inline 后端的 deadline/取消要等
//!    VM 让出（HostEventLoop 空闲）后才处理，自旋脚本由中断预算终止；
//! 7. 结局折算成 [`CodemodeResult`]；`execute()` 的 future（确定性 run 则是 `CodemodeRun::result`）
//!    完成。
//!
//! 脚本可见的 `tools`、`ALL_TOOLS`、`text()`、`store()` 等由 prelude 提供，见
//! [`runtime::prelude_source`]。
//!
//! # 边界与失败模式
//!
//! - `execute()` 返回 `Result<_, SandboxClosed>`：外层 `Err` 只表示 sandbox 已关闭（例如
//!   `close()` 之后）；脚本自身失败是 `Ok(CodemodeResult::Err(..))`，仍保留部分输出与调用记录。
//! - `start()` 返回 `Result<CodemodeRun, StartError>`：sandbox 已关闭或参数非法（`started_at`
//!   非有限或为负、`seed` 不是 32 位无符号整数、工具名或脚本名不唯一、全局名非法/重复/与
//!   命名空间冲突）时同步失败，此时不创建 VM。
//! - 四种错误类别：`Script`（脚本抛错或语法错误，通常带 `name`/`stack`）、`Timeout`（deadline
//!   到期，或 VM 的中断预算耗尽——inline 后端与 remote 的 serve 端都使用预算）、`Aborted`
//!   （取消信号或 sandbox 关闭）、`Sandbox`（wasm trap、worker 缺失、远端失败等宿主侧故障）。
//!   终态只有一个：第一个进入终态裁决的事件获胜，之后的事件被忽略。inline 后端的 deadline/取消
//!   要等 VM 让出（HostEventLoop 空闲）后才处理，自旋脚本只能由中断预算停下；这段时间里脚本
//!   也可能先报 `done`，让更早到期的 deadline 输掉。
//! - 其它入口的错误：解析失败是 [`CodemodeSourceError`]；加载 wasm 失败是
//!   `wasm::CodemodeWasmError`；[`cloudflare`] 的构造入口恒返回 not supported 错误（不移植）。
//!
//! # 设计约束（为什么这样实现）
//!
//! - **逐字对齐 TS**：这是 `@earendil-works/kiana-codemode` 的移植，公开名字与 JSON 线上形状
//!   必须保持稳定；声明文本与错误消息由锁定测试逐字复核，各模块文档列明已知差异。
//!   改公开类型或序列化前，先读对应的 TS 源与 `spec/agent-codemode.md`。
//! - **一个 TS 文件一个模块**：见 [`runtime`] 的模块文档；实现只允许存在一份，[`index`] 与
//!   [`portable`] 只是再导出面，本身不含实现。
//! - **VM 与宿主解耦**：三个后端共用 [`runtime::execution`] 的宿主逻辑与
//!   [`runtime::protocol`] 的消息形状，传输方式不改变语义，因此同一组断言能覆盖三者。
//!
//! # 入口与公开面
//!
//! TS 的 6 个 package exports 在 Rust 侧落到模块：
//!
//! | TS specifier | Rust |
//! |---|---|
//! | `.`（`index.ts`） | crate 根（= [`index`] 的再导出面） |
//! | `./declarations` | [`declarations`] |
//! | `./source` | [`source`] |
//! | `./portable` | [`portable`] |
//! | `./cloudflare` | [`cloudflare`]（不移植的占位，见 ARCHITECTURE.md §8 第 3 条） |
//! | `./worker` | [`runtime::worker`] |
//!
//! 根面**就是** [`index`] 的面（`pub use index::*`），避免与 `index.ts` 的逐字清单漂移；
//! [`portable`] 的面单独存在于子模块（`kiana_codemode::portable::*`）。两个模块文档里各有
//! 冻结的逐条契约表（spec `agent-codemode.md` §2.2.15 / §3.3）。
//!
//! # 模块 → TS 源
//!
//! | 模块 | TS 源 |
//! |---|---|
//! | [`types`] | `types.ts` |
//! | [`identifier`] | `identifier.ts` |
//! | [`source`] | `source.ts` |
//! | [`wasm`] | `wasm.ts` |
//! | [`declarations`] | `declarations.ts` |
//! | [`prelude`] | `runtime/prelude-source.ts` 的门面（唯一权威在 [`runtime::prelude_source`]） |
//! | [`cloudflare`] | `cloudflare.ts`（不移植的占位） |
//! | [`runtime::protocol`] | `runtime/protocol.ts` |
//! | [`runtime::prelude_source`] | `runtime/prelude-source.ts` |
//! | [`runtime::execution`] | `runtime/execution.ts` |
//! | [`runtime::inline`] | `runtime/inline.ts` |
//! | [`runtime::host`] | `runtime/host.ts` |
//! | [`runtime::remote`] | `runtime/remote.ts` |
//! | [`runtime::vm`] | `runtime/vm.ts` |
//! | [`runtime::worker`] | `runtime/worker.ts` |
//! | [`index`] | `index.ts` 再导出面 |
//! | [`portable`] | `portable.ts` 再导出面 |
//!
//! # 阅读顺序
//!
//! 1. [`types`]：数据契约，后面所有模块都围绕它；
//! 2. [`source`] 与 [`declarations`]：脚本如何进门、工具如何变成模型可见的声明；
//! 3. [`runtime::execution`]：宿主侧状态机（deadline、取消、调用记录、终态裁决）；
//! 4. [`runtime::protocol`]：宿主与 VM 的消息形状；
//! 5. [`runtime::vm`] 与 [`runtime::prelude_source`]：QuickJS 的驱动与脚本可见面；
//! 6. [`runtime::inline`]、[`runtime::host`]、[`runtime::worker`]、[`runtime::remote`]：
//!    三个后端的 launcher 与接线（inline 的 HostEventLoop 闸门 / worker 线程的消息转发 /
//!    远端 RPC），VM 本身由 [`runtime::vm`] 起在专用线程；
//! 7. [`wasm`]：`quickjs.wasm` 的加载；[`index`]、[`portable`]、[`cloudflare`] 收尾。

// --- 子模块 ---
// `cloudflare.ts` 是 not supported 占位（ARCHITECTURE.md §8 第 3 条：「feature 门控的占位，
// 编译通过、调用即明确报错」）。与骨架及同批的三个占位（`kiana-durable` 的 storage/sqlite 与
// images、`kiana-chord` 的 sqlite）一致，**模块本身无条件编译**：`kiana_codemode::cloudflare`
// 在默认 feature 下就存在，常量可用、构造入口返回逐字的 not supported 错误。占位不引入任何
// Workers 依赖，所以不需要 feature 门控；`cloudflare` feature 保留在原位，供阶段 8 之后
// 引入 wasm32 目标时替换实现（届时开门控即可），下游的 `use` 与 feature 组合不用改。
pub mod cloudflare;
pub mod declarations;
pub mod identifier;
pub mod index;
pub mod portable;
pub mod prelude;
pub mod runtime;
pub mod source;
pub mod types;
pub mod wasm;

// 公开面 = 主入口（`index.ts`）的逐字清单；`index.rs` 是唯一权威，见其模块文档的契约表。
pub use index::*;
