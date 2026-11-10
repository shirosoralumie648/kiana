# 扩展运行时设计（风险优先）：内嵌 JS 引擎与 TS/ESM 加载

本文是 `ca-extensions` §8.3「扩展执行引擎」的落地设计。目标：**在 Rust 单静态二进制内运行现有
TypeScript 扩展，扩展 API 与 `packages/coding-agent/src/core/extensions/types.ts` 逐字一致**。

写法：先把 `examples/extensions/**`（70 个单文件 + 9 个目录，共 86 个 `.ts`、451 KB）里**实际用到**的
宿主能力挖出来，按「QuickJS 原生提供 / 需 shim / 无法等价」分档，再据此选引擎、定模块划分。
结论均有 `/tmp` 原型与实测支撑（原型一次性，不进入 `kiana`；文中给出复现命令与数字）。

与 `ca-extensions` 的关系：本文只决定**引擎、加载、绑定、事件循环、Node 兼容面**；事件语义、合并规则、
错误文案、UI 对话框栈仍以 `ca-extensions` §2/§8.4–§8.6 为准。crate 布局沿用其 §8.1 的 `kiana-extensions`；
本文补充其中 `loader/js_host.rs` 的内部结构与新增的 `js/` 资源目录。

---

## 0. 结论摘要

| 决策 | 结论 | 依据 |
|---|---|---|
| 引擎 | **rquickjs 0.14.0**（内置 quickjs-ng 0.16.2，C 源随 crate 编译） | 唯一同时满足「单静态二进制 + 成熟宏与模块加载器 + 可设内存/栈/中断限制」的候选 |
| TS 去类型 | **oxc 0.155.0**（parser + semantic + transformer + codegen） | 86 文件 / 451 KB 全量去类型 **46.8 ms**，0 失败 |
| 模块解析 | 自研 Resolver（`oxc_resolver 11.24.3` 做 `node_modules`/`exports`）+ 自研 Loader | 虚拟模块、npm 包、相对导入三种形态都在 Loader 内分流 |
| 并发 | **1 个专用 JS 线程 + 1 个 JSRuntime/代**，主线程 tokio + TUI | JSRuntime 非线程安全；同代所有扩展共享一个 runtime 才能复刻「同进程 + 共享虚拟模块对象」 |
| 异步 | 不用 `AsyncRuntime`/`AsyncContext`；**手写 Promise + 宿主任务队列 + `Persistent` waiter** | `rquickjs::function::Async` 在非 async runtime 下 panic；手写方案可精确把事件循环放在 JS 线程 |
| 跨线程渲染 | 同步通道调用 JS `render(width)`；实测 **27.0 µs/帧**（最坏 280 µs），JS 本地渲染 2.9 µs | `/tmp/xthread` 600 帧实测；60 fps 预算 16.7 ms，余量充足 |
| 二进制增量 | rquickjs + oxc（LTO/strip/codegen-units=1）= **3.94 MB** | `/tmp/combined` `stat` 实测 |
| 最大硬缺口 | **无 `Intl`**（`Intl.Segmenter` 不存在，`Number#toLocaleString` 无分组） | `/tmp/featprobe` 实测；kiana-tui 的字素/词切分依赖它 |
| 因此 | **kiana-tui 不作为 JS 包运行**，而是「Rust 原生类 + JS 薄工具层」的混合虚拟模块 | 见 §10.3；这是与「原样跑 kiana-tui」的关键分歧，须在验收中承认 |
| npm 依赖 | **不是风险**：`@anthropic-ai/sdk@0.52.0`（43 模块，CJS 与 ESM 入口均通，含 SSE 流）与 `@anthropic-ai/sandbox-runtime@0.0.26`（664 模块、含 `zod@3.25.76` 校验）均已在 QuickJS 下跑通 | `/tmp/sbprobe` 实测；前提是 §5.6.1 的真 CJS `require` 与 §9 的 `fetch`/Web Streams shim |

一句话：**引擎不是风险，兼容面才是风险**。风险集中在 Intl、原生 addon、WebAssembly、以及 `node:net`/`node:http`
这类**有状态宿主能力**四处；npm 包兼容性已由 `/tmp/sbprobe` 证伪了「QuickJS 跑不了真实 npm 生态」的担忧。
本文逐一给出可执行方案或明确的「不可等价」结论。

---

## 1. 风险登记册

| ID | 风险 | 严重度 | 证据 | 处置 |
|---|---|---|---|---|
| R1 | **无 `Intl`**：`Intl.Segmenter` 缺失；`Number#toLocaleString` 不做千分位；`Date#toLocaleString` 输出固定 `MM/DD/YYYY, hh:mm:ss AM`（UTC+8 主机） | 高 | `/tmp/featprobe` | 见 §9.2：`Intl` shim（`Segmenter` 用 Rust UAX#29 + `NumberFormat` 最小实现；`Collator`/`DateTimeFormat` 不实现）；且 kiana-tui 改由 Rust 提供 |
| R2 | **原生 addon 无法加载**：`gondolin` 依赖 `ssh2`（可选 `cpu-features`）、`@earendil-works/gondolin-krun-runner-*`；`sandbox` 依赖 `bubblewrap`/`sandbox-exec` 子进程 | 高 | `package-lock.json`：`ssh2@1.17.0`、`cpu-features@0.0.10`（optional）、`gondolin-krun-runner-linux-x64@0.14.0` | `gondolin` **不等价**（见 §11.3）；`sandbox` 可等价（纯子进程 + 文本配置，见 §11.2） |
| R3 | **无 WebAssembly** | 中 | `/tmp/featprobe`：`WebAssembly === undefined` | 默认不提供；可选 `wasmtime` 特性把 `WebAssembly` 绑到宿主实例（`doom-overlay` 需要，见 §11.3） |
| R4 | **无 Web Streams / `fetch` / `crypto` / `TextEncoder` / `URL` / `AbortController` / `structuredClone` / `console` / `setTimeout`** | 中 | `/tmp/featprobe` 全为 `undefined`（`atob`/`btoa`/`queueMicrotask`/`WeakRef`/`FinalizationRegistry`/`performance` 存在） | 全部以 JS shim + Rust 宿主函数提供（§8、§9） |
| R5 | **CJS 语义**：单文件自包含 CJS（`ms`）用包装器即可；**多文件 CJS**（`shell-quote`、`zod/index.cjs`、`socks5-server`）在模块体内调 `require`，包装器直接抛 `ReferenceError: require is not defined` | 中 | `/tmp/cjsmod`（`ms` 通过）+ `/tmp/sbprobe`（`shell-quote` 失败，改造后通过） | 改为**真 CommonJS 加载器**：宿主 `require` + JS 侧 `new Function` 求值 + JS 侧 `/tmp` 缓存（§5.6.1）；不做 CJS→ESM 静态转换 |
| R6 | **`node:net`/`node:http` TCP 服务器**：`sandbox/` 的依赖链要真的 `listen` 并读 `address().port`；桩会给出假代理端口 | 中 | `/tmp/sbprobe`：`initialize()` 打到「Failed to create bridge sockets after 5 attempts」；`http-proxy.js`/`socks5-server` 用 `createServer`/`createConnection` | tokio `TcpListener`/`TcpStream` + `Server`/`Socket` shim（§8）；工作量大但无研究风险 |
| R7 | **Node 解析边界**：`exports` 条件、`main`、深路径、`node_modules` 逐级、`#imports`、`tsconfig paths` | 中 | `ca-extensions` §8.6-1；examples 用 `tsconfigPaths: true` 的 `@earendil-works/*` 实际全走虚拟模块 | `oxc_resolver` + 显式虚拟模块优先表（§5） |
| R8 | **`structuredClone` 语义**：`tool_result`/`message_end` 事件里扩展会克隆任意宿主对象 | 中 | `debug-provider.ts:71`、`validation.ts:321` | JS shim 实现结构化克隆子集（§9.5） |
| R9 | **`toLocaleString` 输出差异**：`entry-renderer.ts`、`input-transform.ts`、`overlay-qa-tests.ts` 直接展示给用户 | 低 | 实测格式固定 | 用 Rust `chrono` + 显式格式复刻 Node 的 en-US 输出（§9.2） |
| R10 | **`ui.custom` 的组件是 JS 对象**，每帧被 Rust TUI 调用 | 中 | 30 处 `ctx.ui.custom`/`ui.custom` | 跨线程同步渲染，实测 27 µs/帧（§10）；`render` 抛错按 `ca-extensions` §3.6 上报 |
| R11 | **模块级 `const` TDZ + 只 `export default` 的运行时语义** | 低 | `/tmp/loadprobe`：stub 缺符号时会报 `X is not initialized`，真实实现下不应出现 | 加载顺序测试（§14.1）覆盖：虚拟模块必须先于扩展模块声明完毕 |
| R12 | **`process.nextTick` 影响 TUI 渲染时序**（`requestRender` 用 `nextTick`） | 低 | `tui-core.md` §2.9.10 | Rust TUI 侧用 `yield_now` 队列代替（`tui-core` 已定），JS shim 的 `nextTick` 只服务扩展自身 |
| R13 | **工厂是 async 时必须有 top-level await 与宿主 await 的衔接** | 低 | `docs/extensions.md`「Kiana waits for an asynchronous factory」 | 工厂调用结果若是 Promise，走 §6.3 的 await 协议 |

---

## 2. 引擎选型（为什么是 rquickjs）

### 2.1 候选对比

| 方案 | 版本（crates.io 实测） | 单二进制 | ESM/模块加载 | 成熟度/风险 | 结论 |
|---|---|---|---|---|---|
| **rquickjs** | 0.14.0（`rust-version 1.87`） | 是（C 编译进 crate，`rquickjs-sys` 内置 quickjs-ng 0.16.2） | 原生 `Module`/`Resolver`/`Loader`/`import.meta`/循环导入 | 采用 | 选用 |
| boa_engine | 0.22.0 | 是 | 有 `Module` API，但 ESM 加载器与 `import.meta` 仍需自建 | 纯 Rust；性能按公开基准通常低于 QuickJS（未在本机复测） | 不选 |
| deno_core + v8 | 0.413.0（`deno_node 0.198.0`） | 是（V8 静态链接，本机未测体积；V8 自身通常数十 MB 量级） | 最完整（含 `deno_node`） | 构建与 ABI 复杂度高；体积与启动成本不可接受 | 不选 |
| quickjs-wasi | **crates.io 无此 crate**（仅 npm `quickjs-wasi@3.6.2`） | 需自带 `.wasm` 与 WASI 运行时 | 无 `node:fs` | 与「Rust 单二进制 + Node shim」方向冲突；只适合 codemode 沙箱 | 不选（保留给 codemode） |

`rquickjs` 的关键 API 均已核对源码（`~/.cargo/registry/src/.../rquickjs-core-0.14.0`）：

- `Ctx::execute_pending_job()`、`Runtime::is_job_pending()`/`execute_pending_job()` —— 手动泵微任务（`context/ctx.rs:399`、`runtime/raw.rs:249`）。
- `Runtime::set_memory_limit`、`set_max_stack_size`、`set_interrupt_handler`、`set_host_promise_rejection_tracker`（`runtime/base.rs:112–167`）—— 沙箱化与死循环中断。
- `Promise::state()/result()/finish()/into_future()`（`value/promise.rs:87–156`）—— 同步与异步两条路径。
- `Persistent::save/restore`（`persistent.rs:88/102`）—— 跨帧保存 JS 回调，且 `restore` 需要 `Ctx`。
- `Module::declare/namespace/meta`、`Module::write`（字节码）、`Module::load`（`value/module.rs:335/448/469`）。
- `#[rquickjs::class]` + `#[derive(Trace, JsLifetime)]` + `#[qjs(constructor)]`/`#[qjs(rename = "...")]` —— 原生类，JS 可 `extends`（`/tmp/classprobe` 实测 `instanceof`、`super`、JS 字段与 native 方法共存）。

### 2.2 关键版本与特性（已核对存在）

```toml
# crates/kiana-extensions/Cargo.toml（节选，版本均已 cargo info 核对）
rquickjs      = { version = "0.14.0", features = ["full-async", "loader", "macro", "parallel"] }
rquickjs-sys  = "0.14.0"     # 可选：仅用于锁定内置 quickjs-ng 0.16.2（quickjs.h: QJS_VERSION 0/16/2）
oxc_allocator = "0.155.0"
oxc_parser    = "0.155.0"
oxc_semantic  = "0.155.0"
oxc_transformer = "0.155.0"
oxc_codegen   = "0.155.0"
oxc_span      = "0.155.0"
oxc_resolver  = "11.24.3"
tokio         = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "fs", "process", "io-util", "net"] }
reqwest       = { version = "0.13.5", default-features = false, features = ["rustls-tls", "stream"] }
flate2        = "1.1.10"       # node:zlib 的 gzip/gunzip
unicode-segmentation = "1.13.3" # Intl.Segmenter shim / UAX#29
unicode-width = "0.2.2"
notify        = "8.2.1"        # fs.watch（9.0.0-rc.6 为预发布，不用）
wasmtime      = { version = "50.0.0", optional = true }  # feature = "webassembly"（doom-overlay）
getrandom     = "0.4.3"        # crypto.getRandomValues
uuid          = "1.29.0"       # crypto.randomUUID / uuidv7
sha2          = "0.11.1"       # crypto.subtle.digest("SHA-256")（cargo info 核对）
url           = "2.5.8"        # node:url / URL / URLSearchParams
chrono        = "0.4.45"       # Date#toLocaleString 复刻（§9.2）
base64        = "0.23.1"      # atob/btoa 的 Rust 侧（也可用 JS 实现）
```

注意：`ca-extensions` §6.4 里写的 `rquickjs = "0.9"` 已过时，落地必须用 **0.14.0**；`full-async` 已包含
`loader`/`macro`/`indexmap`/`phf` 等，`classes` 特性在 0.14.0 已废弃（功能默认启用，源码 `lib.rs:128-136` 的
`#[deprecated]` 注释明确写了 "now enabled by default"），因此**不要**再写 `classes`（会触发 deprecation 警告）。
Rust 工具链：本机 `rustc 1.99.1`；oxc 要求 `1.97.0`，rquickjs 要求 `1.87`，均在 `kiana` 的 MSRV 之上。

### 2.3 体积与启动

| 组合 | stripped 体积 | 备注 |
|---|---|---|
| oxc（strip 流水线）单独 | 2.93 MB | `/tmp/oxcsize` |
| rquickjs（full-async/loader/macro）单独 | 1.89 MB | `/tmp/rqprobe` |
| **两者 + tokio + LTO + codegen-units=1 + strip** | **3.94 MB** | `/tmp/combined` 实测；含 tokio 的链接后仍未超 4 MB |

启动代价：单个扩展的「去类型 + declare + eval + 工厂调用」合计 **0.2–1.6 ms**（见 §12.1 实测表）；
86 个文件全量去类型 46.8 ms，属于一次性成本（可按文件并行，见 §12.1）。

---

## 3. 线程模型与总体架构

```text
┌─────────────────────────────── main thread (tokio multi-thread runtime) ───────────────────────────────┐
│  kiana-tui render loop (crossterm/ratatui 等价物, 见 tui-core)                                             │
│  ExtensionRunner (Rust)  ── dispatch ──┐                                                               │
│  adapters/{interactive,rpc,print_json} │  host: kiana-ai/kiana-session/kiana-tools/exec/fs/...                  │
└────────────────────────────────────────┼───────────────────────────────────────────────────────────────┘
                                         │  JsBridge::call / call_sync（mpsc + oneshot / sync_channel）
┌────────────────────────────────────────▼───────────────────────────────────────────────────────────────┐
│ JS thread（独占 JSRuntime；扩展「一代」= 1 runtime + 1 context）                                        │
│   Loop:                                                                                                │
│     1) drain job queue: ctx.execute_pending_job() until false                                          │
│     2) recv host task (with timeout = 下一个 timer deadline)                                            │
│     3) 执行任务（restore Persistent → call JS → 若返回 Promise 则登记 waiter）                          │
│     4) resolve 完成的 Promise（restore 对应的 resolve 函数并调用）                                      │
│   GC: 每次任务后 rt.run_gc()（可配阈值）；内存/栈/中断限制在 Runtime 上设置                              │
└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

要点：

1. **一个扩展代（generation）= 一个 JSRuntime**。`/reload` 或替换会话时整代销毁再建，对应
   `ca-extensions` §2.2.2 的 `clearExtensionCache`/generation 语义；旧代 `kiana`/`ctx` 句柄在 JS 侧必须失效
   （`ca-extensions` §8.4 的 `Failed` 状态 + 代际 token）。
2. **所有扩展共享同一 context**，因此 22 个虚拟模块的**对象身份**天然一致（`/tmp/rqprobe`
   `shared-identity-across-importers: same-instance=true`），这是 `11156-kiana-ai-models-virtual-module.test.ts`
   的必要条件。
3. **JS 线程不与 tokio 混用**：`Ctx`/`Function`/`Object` 都是 `!Send`，只能留在 JS 线程；跨线程只传
   `Persistent`（其 `rt` 指针仅在同一 runtime 内 restore，且 `restore` 需要 `Ctx`）与纯数据。
   实测（`/tmp/loop_probe`）`Persistent` 跨线程保存/恢复可用；`Runtime` 本身有 `unsafe impl Send/Sync`
   （`runtime/base.rs:232/240`），但 `Context`/`Ctx` 没有，**不要把 `Ctx` 送出线程**。
4. **同步桥**：`SyncSender<HostCall>` + `Receiver<Reply>`，用于 `render(width)`、`handleInput(data)`、
   `component.invalidate()`、`matchesKey` 之类必须同步返回的调用。实测往返 **27.0 µs**
   （`/tmp/xthread` 600 帧，`worst=279 µs`；本机 CPU 抖动时最坏值仍是 60 fps 预算的 1.7%）。
5. **异步桥**：宿主调用返回 `Promise`；JS 线程把 resolve/reject 存为 `Persistent`，宿主完成时投递
   `HostEvent::Resolve{id, value}`；JS 线程 restore 后调用。这与 `ca-extensions` §8.5「串行 await」契合：
   派发器可以用 `await` 逐个 handler。

---

## 4. 生命周期状态机（与 `ca-extensions` §8.4 对齐）

```text
Generation::new()
  ├─ JSRuntime::new + limits + loader
  ├─ Context::full → install shims → eval js/prelude.js（§9.1）→ 生成 22 个虚拟模块（惰性）
  ├─ 对每个扩展：load_module(path) → 得 `export default` → call(factory, api)
  │     · 工厂可返回 Promise：登记 waiter，等 settle 后再继续（docs/extensions.md 契约）
  │     · 失败 → ExtensionError{extension_path, event:"load", error} → 进入 errors 列表，不中断其他扩展
  └─ 进入 Running：accept HostTask
Generation::drop()
  ├─ 先发 session_shutdown（由 runner 负责，JS 线程执行）
  ├─ 清空 waiter 表（reject 所有挂起 Promise，消息与 JS 侧 `AbortError` 对齐）
  └─ drop Context → drop Runtime（所有 Persistent 必须先 drop，否则 runtime drop 会 abort 进程）
```

实现约束：`Persistent` 的生命周期必须严格包住 runtime。做法是把所有 `Persistent` 放在
`struct JsState { ctx: Context, waiters: HashMap<u64, Persistent<Function<'static>>>, ... }`，
`Drop` 里先 `waiters.clear()` 再 drop `ctx`。

---

## 5. 模块加载：TS/ESM/CJS/虚拟模块/npm

### 5.1 流水线总览

```text
specifier ──Resolver──▶ 绝对路径 or 虚拟模块键
             │
             ▼
        Loader::load(name)
             ├─ 虚拟模块键 → 预置 JS 源（§9）→ Module::declare
             ├─ .ts/.tsx  → oxc strip（§5.2）→ Module::declare
             ├─ .js/.mjs  → 原文 → Module::declare（ESM 语法）
             └─ .cjs / CJS 检测 → CJS 包装器（§5.6）→ Module::declare
```

`Resolver` 的 `base` 是**导入者自身的模块名**（`/tmp/tdzprobe` 实测：`RESOLVE base="dep" name="dep"`；
对文件模块即 `Loader::load` 时传给 `Module::declare` 的那个 name，我们用绝对路径）。因此 `base` 直接当作
「导入者路径」使用，`dirname` 即可。

### 5.2 oxc 去类型（与 jiti/Babel 等价性）

```rust
// kiana-extensions/src/loader/ts.rs（与 /tmp/tsprobe2 的已验证实现一致）
let alloc = Allocator::default();
let st = SourceType::from_path(Path::new(name)).unwrap_or_else(|_| SourceType::ts());
let ret = Parser::new(&alloc, src, st).with_options(ParseOptions::default()).parse();
if !ret.diagnostics.is_empty() { return Err(...); }
let mut program = ret.program;
// 必须 with_enum_eval(true)，否则 Transformer 会 panic（oxc 0.155 的硬约束）
let scoping = SemanticBuilder::new().with_enum_eval(true).build(&program).semantic.into_scoping();
let mut options = TransformOptions::default();
options.decorator.legacy = true;   // jiti/Babel 走 legacy 装饰器语义
let tr = Transformer::new(&alloc, Path::new(name), &options);
let _ = tr.build_with_scoping(scoping, &mut program);
Codegen::new().build(&program).code
```

实测覆盖（`/tmp/tsprobe`、`/tmp/tsprobe2`）：

| 语法 | 结果 | 说明 |
|---|---|---|
| 类型注解、`interface`、`type`、`satisfies`、`as const` | 支持 | 删除 |
| `enum` / `const enum` / `namespace` | 支持 | 生成 IIFE；`/tmp/enum2.ts` 验证 `const enum` 也被降级 |
| 参数属性（`constructor(public q: T)`） | 支持 | 生成赋值 |
| `import type`、`export type`、`import { type X }` | 支持 | 整条删除（`/tmp/imports3.ts`：`import type ... from "./foo.ts"` 消失） |
| `import.meta`、动态 `import()` | 支持 | 保留原样（`/tmp/meta.ts`） |
| legacy 装饰器 | 支持 | 生成 `@oxc-project/runtime/helpers/decorate*` 导入 → 需注册对应虚拟模块或关闭装饰器（examples 中**无装饰器**） |
| 顶层 await | 支持 | QuickJS 模块原生支持 |
| JSX/TSX | 可解析 | examples 无 JSX；若出现，需额外 `jsx` 变换（不在本期） |
| 全量语料 | **86 文件 / 451,654 B / 0 失败 / 46.8 ms** | `/tmp/tsprobe2` |

注意：`Transformer` 不做 CommonJS 降级（`options.env.module = Module::CommonJS` 对 ESM 源无效），
所以 CJS 必须走 §5.6 的包装器；这与 jiti 的「按文件类型分派」一致，而不是全量 CJS 化。

### 5.3 虚拟模块（22 个键）

键表逐字按 `ca-extensions` §2.6.1 / `virtual-modules.ts`。实现方式：

- 每个键对应 `js/virtual/<key>.js` 源（由 Rust 侧注入宿主对象后 `Module::declare`）；
- `@earendil-works/kiana-coding-agent` 与 `@mariozechner/kiana-coding-agent` 指向**同一个 Module 实例**
  （同一 `Persistent<Object>` 命名空间），满足身份一致；
- `kiana-ai` 根与 `kiana-ai/compat` 同一实例；`models`/`oauth`/`providers/all` 各自独立实例。

注入策略：宿主对象以 `globalThis.__kiana_host` 的只读属性形式存在（`prelude.js` 中 `Object.defineProperty`），
虚拟模块源只做 `const { ... } = globalThis.__kiana_host; export { ... }`。这样 Rust 不必为每个导出逐个
`Function::new`，只需构建一个 host 对象，**绑定成本集中在一次性的 host 构建**。

### 5.4 Node 模块解析顺序

`Resolver::resolve` 的顺序（与 Node ESM + jiti 对齐）：

1. **虚拟模块精确匹配**（22 键，含 `typebox/compile` 等子路径）。
2. `./`、`../`、`/` 开头的相对/绝对路径：
   - 若 specifier 已带扩展名 → 直接用；
   - 无扩展名 → 依次试 `.ts`、`.tsx`、`.js`、`.mjs`、`.cjs`（jiti 的 TS 优先）；
   - 目录 → `package.json` 的 `exports`/`module`/`main` → `index.{ts,js}`。
3. 裸 specifier：
   - `node:`/`fs` 等内建 → 走 shim 表（§8），返回 `node:<name>` 作为模块名；
   - 否则自 `base` 目录向上逐级找 `node_modules/<specifier>`（`oxc_resolver` 处理 `exports` 条件
     `import`/`require`/`node`/`default`、`#imports`、symlink）；
4. 仍失败 → 抛 `Error loading module '<spec>'`，消息格式与 rquickjs 的 loader 错误一致（实测文本：
   `Error loading module 'node:child_process': No such file or directory (os error 2)`）。

`tsconfigPaths: true` 分支：只在 `isTypeScriptSourceRuntime`（源码运行）且**未命中虚拟模块**时启用；
`kiana` 自身没有 TS 源码树，扩展目录里的 `tsconfig.json` 若存在则按其 `paths` 解析（`oxc_resolver`
支持 `tsconfig` 读取），否则跳过。

### 5.5 `moduleCache: false` 的复刻

jiti 设 `moduleCache: false`，即**每次 `import()` 重新执行模块**。rquickjs 原生 `Module` 有内部缓存
（`/tmp/rqprobe`：两次 `import('/tmp/.../loads.js')` → loader 只被调用 1 次，`same=true`）。因此：

- **扩展入口文件**每次通过「唯一模块名」加载：`<abs-path>?v=<generation>-<seq>`。QuickJS 的模块表按名字
  去重，加 query 即天然「不缓存」；同时 `import.meta.url` 仍指向真实路径（`meta.set("url", file_url)`
  用不带 query 的真实 URL）。`/tmp/qprobe` 已验证：同名文件带 `?v=1` 与 `?v=2` 各执行一次（`run1-v7` /
  `run2-v7`），而相对依赖 `./dep.js` 在两份实例间共享（`v7`），`Resolver` 拿到的 `base` 是带 query 的
  模块名（需 `base.split('?')` 后再取 `dirname`）。
- **扩展内部的相对依赖**保持真实路径 → 同一代内共享（这与 jiti 的 `moduleCache:false` 有细微差别：
  jiti 每次 `jiti.import(entry)` 会重建整棵依赖图）。但 `ca-extensions` 的工厂缓存语义是
  「命中缓存时不重跑模块但重跑工厂」，且 reload 会换代，因此**同一代内共享依赖**不会造成可观察差异；
  需要严格逐条对齐时，可对入口依赖图整体加同一 query 后缀（一次性开关 `KIANA_EXT_STRICT_NOCACHE=1`）。

### 5.6 CJS 包装器（npm 依赖）

`/tmp/cjsmod` 已验证：

```rust
let wrapped = format!(
    "const module = {{ exports: {{}} }}; const exports = module.exports;\n{source}\n; export default module.exports;"
);
Module::declare(ctx, name, wrapped)
```

- 扩展里 `import ms from "ms"` → `ms` 即 `module.exports`（`ms@2.1.3` 实测输出
  `1m|172800000|3600000|60000|function|1s`）。
- 命名导入（`import { Type } from "typebox"`）在 CJS 包里不常见；对 `exports.foo = ...` 的 CJS 包，
  wrapper 需额外生成 `export const foo = module.exports.foo;`——用正则/`oxc` 解析顶层 `exports.*` 赋值
  生成命名导出（`oxc` 已有 AST；只做静态赋值模式，动态 `Object.defineProperty` 不支持）。
- 判定 CJS 的规则：扩展名 `.cjs`，或 `package.json` 的 `type !== "module"` 且文件含 `require(`/`module.exports`/`exports.`。

#### 5.6.1 该 wrapper 不够用：多文件 CJS 包必须提供真 `require`（`/tmp/sbprobe` 实测）

上面的 wrapper 只对**自包含**（不调 `require`）的 CJS 包成立。`shell-quote@1.12.0` 是典型反例：

```js
'use strict';
exports.quote = require('./quote');
exports.parse = require('./parse');
```

用上面的 wrapper 加载 `shell-quote` 时，模块体内调用 `require` → QuickJS 抛
`ReferenceError: require is not defined`（`/tmp/sbprobe` 第一次运行的实际报错）。而
`@anthropic-ai/sandbox-runtime` 的 `linux-sandbox-utils.js` 是 `import shellquote from 'shell-quote'`
并在 `wrapWithSandbox` 里用 `shellquote.quote(...)`，因此 `sandbox/` 在朴素 wrapper 下**必然失败**。
`zod@3.25.76` 的 `index.cjs` 同样 `require("./v3/external.cjs")`，`@pondwader/socks5-server` 的
`dist/index.js` 亦 `require("net")`。

**正确做法：CJS 走真正的 CommonJS 加载器，而不是把它包成 ESM。** 具体：

1. Loader 只负责把 CJS 文件变成**一个 ESM 模块，其 `default` 导出是由 CJS 加载器求值后的
   `module.exports`**；`require` 由宿主提供，递归解析子依赖。
2. `require(spec)` 的解析路径与 ESM 的 `Resolver` 共用同一套（虚拟模块 → 相对 → `node_modules`），
   但**只接受 CJS/JSON**；命中 ESM 文件时按 Node 抛 `ERR_REQUIRE_ESM`（Node 22+ 允许
   `require(esm)`，但那要求 ESM 无 top-level await；第一版不做，报错更诚实）。
3. CJS 缓存以**绝对路径为键**放在 JS 侧 `globalThis.__kiana_cjs`（`Object.create(null)`），而不是 Rust 侧
   ——实测 Rust 侧缓存 `Persistent<Value>` 会遇到 `'js` 生命周期逃逸问题（`/tmp/sbprobe` 编译期报
   `returning this value requires that 'js must outlive 'static`）；JS 侧对象天然按 `Ctx` 生命周期存活。
4. 模块体求值用 QuickJS 的 `Function` 构造器（源码已在 JS 侧）：
   `new Function("module","exports","require","__filename","__dirname", src)`。这同时自动获得
   CJS 的 `this === module.exports`、`arguments` 与函数作用域语义，无需改写源码。**注意**：`new Function`
   里的代码拿不到外层模块作用域，正是 CJS 想要的（每个文件独立作用域）。
5. `module.exports` 与 `exports` 的身份：`exports` 必须在调用前绑定成 `module.exports`；若模块体
   重新赋值 `module.exports`，以重新赋值后的值为准（Node 语义）。
6. `require.resolve`/`require.cache`/`require.main`：只实现 `require.resolve`（返回绝对路径）与
   `require.cache`（指向 `__kiana_cjs`）；`require.main` 设为 `undefined`。

落到 Loader 的 CJS 分支（实测可通过）：

```rust
let dir = Path::new(name).parent().unwrap().to_string_lossy().into_owned();
let src = format!(
    "const module = {{ exports: {{}} }};\n\
     const exports = module.exports;\n\
     const require = globalThis.__kiana_make_require({name:?}, {dir:?});\n\
     (function (module, exports, require, __filename, __dirname) {{\n{raw}\n}}) \
     (module, exports, require, {name:?}, {dir:?});\n\
     export default module.exports;\n"
);
```

`__kiana_make_require` 是 prelude 里的 JS 函数，内部只调两个宿主函数
`__kiana_resolve(from, spec)`（返回绝对路径，内建返回 `"\u{1}" + name`）与 `__kiana_read(path)`
（返回源码；对 ESM 文件返回 `null` 以触发 `ERR_REQUIRE_ESM`）。**宿主函数是同步的**，
因为 `require` 本身同步；文件读取在 JS 线程上直接做（`std::fs`），不进 tokio。

**外层 IIFE 不是风格问题，是必需的**：`@anthropic-ai/sdk` 的 `index.js:3` 直接写
`exports = module.exports = function (...args) {...}`——它在模块顶层**重新赋值 `exports`**。
若把源码直接拼在 `const exports = module.exports;` 之后（不带 IIFE），这行会试图给 `const` 赋值。
`/tmp/expconst` 实测：QuickJS 报 `'exports' is read-only`（V8 报 `Assignment to constant variable.`，
严格与 sloppy 模式**都抛 TypeError**，不会静默失败）。也就是说朴素拼接会让 SDK 直接加载失败。
IIFE 的形参 `exports` 是可写绑定，才符合 CJS 语义（Node 里 `exports` 本来就是函数参数）。
`/tmp/sbprobe` 的两个入口（CJS/ESM）都能正确拿到 `typeof Anthropic === "function"` 就是靠这一点。

实测结果（`/tmp/sbprobe`，`sandbox-runtime` 完整依赖图）：`shell-quote` 的两个子模块被正确
`require` 进 `__kiana_cjs`（`nCjs: 2`，键为 `shell-quote/quote.js`、`shell-quote/parse.js`），
`SandboxRuntimeConfigSchema`/`NetworkConfigSchema`/`getDefaultWritePaths` 全部可用，
`zod` 校验行为与 Node 26 逐字一致。

### 5.7 依赖装在哪、怎么找

`examples` 的 npm 依赖都在**扩展目录自己的 `node_modules`**（`with-deps/`、`custom-provider-anthropic/`
等；monorepo 的 `package.json` 把 5 个示例目录列为 workspace）。因此解析必须从**扩展文件所在目录**
逐级向上找 `node_modules`，而不是从 kiana 二进制位置。这与 jiti 行为一致（`with-deps` 的注释明确写了
「jiti resolves modules from the extension's own node_modules」）。

---

## 6. 事件循环与异步协议

### 6.1 宿主 → JS（派发）

```rust
enum HostTask {
    Call { module: ModuleId, export: String, args: Vec<HostValue>, reply: oneshot::Sender<CallResult> },
    Event { handler_id: u64, event: HostValue, reply: oneshot::Sender<CallResult> },
    Render { component_id: u64, width: usize, reply: SyncSender<Result<Vec<String>, JsError>> },
    Input  { component_id: u64, data: String, reply: SyncSender<InputResult> },
    Resolve { promise_id: u64, outcome: Result<HostValue, JsError> },
    Timers { due: Instant },
    Quit,
}
```

`CallResult` 三态：`Value` / `Promise(promise_id)` / `Threw(JsError{message, stack, value_json})`。
`JsError` 的 `message`/`stack` 必须原样保留（`ctx.catch()` → `exception.message()/stack()`），因为
`ca-extensions` §3.6 的错误落点会逐字展示。

### 6.2 JS → 宿主（同步/异步）

```rust
// 同步：fs/path/Buffer/theme.fg/...（必须立即返回）
#[rquickjs::function] fn fs_read_file_sync(ctx: Ctx, path: String) -> Result<Value> { ... }

// 异步：返回 Promise，宿主完成后 resolve
fn exec<'js>(ctx: Ctx<'js>, cmd: String, args: Vec<String>) -> Result<Value<'js>> {
    let (p, resolve, reject) = Promise::new(&ctx)?;
    let id = next_id();
    WAITERS.with(|w| w.borrow_mut().insert(id, Waiter::new(&ctx, resolve, reject)));
    HOST_TX.send(HostRequest::Exec{ id, cmd, args }).unwrap();
    Ok(p.into_value())
}
```

### 6.3 Promise 握手协议（关键）

派发器在 Rust 侧 `await` 一个 JS handler，需要：

1. JS 线程调用 `handler(arg)`；返回值是 `Promise` 时登记 `Persistent` 到 `resolve_fn`，
   并给它挂 `.then(v => host_done(id, 0, v), e => host_done(id, 1, e))`；
2. `host_done` 是 Rust 函数，把 `(id, outcome)` 发回主线程；
3. 主线程 `oneshot` 收到后继续派发下一个 handler（串行，符合 `ca-extensions` §8.5）；
4. JS 线程在每次任务后 `while ctx.execute_pending_job() {}`，保证微任务及时清空；
   `/tmp/loop_probe` 实测 7 个定时器 + Promise 链在 31 ms 内全部完成，`sync-render` 在 handler
   await 期间仍能返回（`/tmp/reent2`：`render-during-await: first render returned 1`）。

不用 `AsyncRuntime` 的理由：`rquickjs::function::Async` 在非 async runtime 下直接 panic
（`tried to use async function in non async runtime`，原型中实测）；手写方案把「事件循环」放在 JS 线程的
`recv_timeout` 上，宿主的 tokio 负责真实 I/O，二者边界清晰，且不依赖 `async_with` 闭包的类型体操。

### 6.4 定时器与 `queueMicrotask`

- `setTimeout/setInterval/clearTimeout/clearInterval`：shim 里维护 `HashMap<u64, deadline>`，
  投递 `HostTask::Timers{due}`；JS 线程 `recv_timeout(next_due - now)`，到点 restore 回调并调用。
  `/tmp/loop_probe` 的 `host-timers: ticks=7 once=true` 与 `/tmp/stream_probe` 的 `timers-shim: fired=21` 已验证。
- **`setTimeout` 必须真的等 `delay`**，不能当微任务立即执行。`/tmp/sbprobe` 实测：`@anthropic-ai/sdk`
  的 `fetchWithTimeout` 是 `setTimeout(() => controller.abort(), ms)`；若 shim 立刻触发，每个请求都会被
  abort 成 `APIUserAbortError("Request was aborted.")`，而错误栈里看不到任何定时器痕迹（排查会误判为
  网络/AbortController 问题）。这是「省事实现」最贵的一个坑。
- `clearTimeout` 必须能从 `HashMap` 里真正移除；`setInterval` 的 drift 不要求精确，但不能因上一拍回调
  慢而堆积（同一 key 的重入要跳过或排队）。
- `queueMicrotask`：QuickJS 原生有（`/tmp/featprobe`），直接用。
- `ui_prompt_start/end` 的 `queueMicrotask` 语义：在 JS 侧用原生 `queueMicrotask` 发送，等价于
  `ca-extensions` §8.5 的「延后到当前微任务队列末尾」。这比 Rust 侧 `yield_now` 更精确。

### 6.5 `AbortSignal` / `AbortController`

QuickJS 无原生实现（`/tmp/featprobe`）。shim 要求（`/tmp/stream_probe` 已验 `aborted=true reason=stop`）：

- `signal.aborted`、`reason`、`throwIfAborted()`、`addEventListener/removeEventListener`；
- **abort 后再 `addEventListener` 必须立刻回调**（`/tmp/stream_probe` 显示初版漏了这条，
  改为「注册时若已 aborted 则 `queueMicrotask` 触发」）；
- `AbortSignal.any([...])`、`AbortSignal.timeout(ms)`；
- 与 Rust 侧 `CancellationToken` 双向绑定：`ctx.signal` 由 Rust 传入时，包一层 JS 对象，abort 由 Rust
  调用 `host_abort(gen, id)` 触发。

### 6.6 中断与超时

- `Runtime::set_interrupt_handler` 配合每代一个 `AtomicBool`：`/reload` 时置位，长循环（扩展里的
  `while(true)`）在 100 ms 内被打断并抛 `Interrupted`。
- 每个 handler 的默认超时沿用 `ca-extensions` 的语义（无超时；`ui.*` 对话框才用 `timeout`）。
- `set_memory_limit(128 * 1024 * 1024)`（可配）；超限时 QuickJS 抛 OOM，转成 `ExtensionError`。

---

## 7. 扩展 API 绑定策略（types.ts 全覆盖）

### 7.1 绑定层次

```text
js/prelude.js           —— 纯 JS 基础设施（shim、Promise 工具、对象构造器）
js/api.js               —— `makeExtensionApi(host)`：订阅表、注册表、事务状态机（Loading/Active/Failed）
js/context.js           —— `makeContext(host, kind)`：读时求值的 getter（ui/mode/hasUI/cwd/model/...）
js/virtual/*.js         —— 22 个虚拟模块
Rust: loader/api.rs     —— create_extension_api 的 Rust 侧镜像（状态、注册表所有权、错误文案）
```

**原则**：凡是「JS 里看起来必须是真对象/真函数」的（`kiana.on`、`defineTool`、`ToolRendererResolver`、
`Theme.fg`、`AssistantMessageEventStream`），一律在 JS 侧实现；Rust 只提供薄的 `__kiana_host.*` 原语
（发送消息、查询设置、注册工具到 registry）。这与 `ca-extensions` §8.6-2「不能是序列化后的数据」一致。

### 7.2 事件（41 个）

`on(event, handler)` 在 JS 侧维护 `Map<event, Array<{handler, owner}>>`，并向 Rust 注册
「该扩展对该事件有兴趣」；派发时 Rust 只把事件送到 JS 线程一次，JS 侧按快照顺序串行调用
（`for (const h of [...list]) await h(event, ctx)`），单个抛错则 `catch` 后上报 `extension_error` 并继续
（`user_bash` 例外，向上抛）。这避免了每个 handler 一次跨线程往返。

事件对象构造：遵守 `ca-extensions` §8.6-11——**在 JS 侧构造并原地传递**，Rust 只在需要时读字段；
需要持久化时才序列化。`undefined` 键的存在性用 `hasOwnProperty` 语义保留。

### 7.3 工具、命令、快捷键、flag、渲染器

| API 组 | 绑定位置 | 关键点 |
|---|---|---|
| `registerTool` | JS 构造 `ToolDefinition`，Rust 存 `Arc<dyn ExtensionTool>` | `execute(toolCallId, params, signal, onUpdate, ctx)`：`signal` 是 §6.5 shim；`onUpdate` 是 JS 函数，Rust 调用时走 `Call`；`renderCall/renderResult` 返回 JS 组件对象（§10） |
| `registerCommand` | JS 表 + Rust `RegisteredCommand` | `invocationName` 去重、保留名冲突由 Rust 按 `ca-extensions` §2.3.9 计算 |
| `registerShortcut` / `registerFlag` / `getFlag` | 同上 | `getFlag` 优先 `pendingFlagValues`（`ca-extensions` §8.4）；`Key` 助手在 `kiana-tui` 虚拟模块里（examples 用 `Key.ctrlAlt("p")`，见 §10.3） |
| `registerMessageRenderer` / `registerEntryRenderer` / `registerMarkdownTransformer` / `registerToolRenderer` | JS 表 | 渲染器返回 JS 组件；`MarkdownTransformer` 是纯字符串变换，直接调用 |
| `sendMessage` / `sendUserMessage` / `appendEntry` / `setSessionName` / `setLabel` / `exec` / `getActiveTools` / `setActiveTools` / `getAllTools` / `getSettings` / `getCommands` / `setModel` / `setThinkingLevel` / `getThinkingLevel` | 直接映射到 Rust `ExtensionApi` trait 方法 | `exec` 是异步（§6.2）；其余同步 |
| `registerProvider` / `unregisterProvider` | JS 侧存 `streamSimple` 等函数，Rust 侧 `kiana-ai` 注册一个 `dyn Provider`，其 `stream_simple` 调到 JS 线程 | `streamSimple` 返回 `AssistantMessageEventStream`（§7.4） |
| `registerMcpServer` / `unregisterMcpServer` / `getMcpServers` | 走 `telemetry-mcp` crate 的注册表 | `mcp_servers_change` 事件由 Rust 触发 |
| `registerVirtualModel` / `unregisterVirtualModel` | Rust `virtual-models` 注册表；`route(request, ctx)` 回调到 JS | `route` 可返回 Promise（§6.3） |
| `events`（EventBus） | JS 侧 `EventEmitter` shim（`on/emit`，handler 包 try/catch 并 `console.error`） | 见 §8 |
| `on`/`off` 返回的退订函数 | JS 闭包 | 加载期订阅要进 `loadingUnsubscribers`（`ca-extensions` §8.4） |

### 7.4 `streamSimple` 与 `AssistantMessageEventStream`

这是**唯一需要在 JS 里实现「有状态流对象」**的 API。`ca-extensions` 的 `ProviderConfig.streamSimple`
返回 `AssistantMessageEventStream`（`packages/ai/src/utils/event-stream.ts`：`push`/`end`/
`[Symbol.asyncIterator]`/`result()`）。绑定：

1. JS 侧提供 `createAssistantMessageEventStream()`（`kiana-ai` 虚拟模块导出），实现与 TS 类**逐字同语义**的
   队列/等待者逻辑（`FifoQueue` + `waiting` + `finalResultPromise`），并附加 `durationMs` 计时；
2. Rust 的 `kiana-ai` 需要一个 `AssistantMessageEventStream` 的 Rust 等价物；`streamSimple` 调用时
   Rust 创建一个 `ChannelStream`，把 JS 侧 `push` 的事件通过 `HostTask::Call` 转发到 Rust 通道；
3. Rust 消费端就是现有的 `for await` 等价物（`tokio::sync::mpsc` + `EventStream` 状态机），
   因此 `model-runtime.ts` 的 `streamSimple` 路径无需知道事件来自 JS。
4. `/tmp/stream_probe` 已验证 JS 侧 `for await` 消费宿主推送的 chunk（`chunk0,chunk1,chunk2`），
   以及「缓冲后再消费」路径（`buffered-stream: b`）——说明两个方向都能工作。

`custom-provider-anthropic` 依赖 `@anthropic-ai/sdk@0.52.0`（安装后 3.5 MB、61 个 `.mjs`，`type: commonjs`，
但有 ESM `exports`）。SDK 需要 `fetch`、`ReadableStream`、`TextDecoder`、`AbortController`、`File`/`Blob`/`FormData`
（仅上传路径）、`process.env`、`crypto`（`randomUUID`）。命中 §8/§9 的 shim 后**可运行**：
`/tmp/sbprobe` 已实测构造、`messages.create`、`messages.stream()`（SSE）全链路通过，输出与 Node 26 控制组
逐字一致，详见 §11.2 与 §14.4。**未能验证的只剩网络层**（真实 TLS/HTTP 分块边界/socket 生命周期），
那是 `fetch` 实现的质量问题，不是 JS 引擎或模块系统的问题（见 §16 Q1）。

### 7.5 `ExtensionContext` / `ExtensionToolContext` / `ExtensionCommandContext`

按 `ca-extensions` §8.2 的约束：**上下文是 trait，每次方法调用做 stale 检查**。JS 侧对应：

```js
function makeContext(host, kind) {
  const ctx = Object.create(null);
  for (const [name, getter] of Object.entries(host.contextGetters)) {
    Object.defineProperty(ctx, name, { get: () => host.readContext(kind, name), enumerable: true });
  }
  // 方法：调用时 host.callContext(kind, name, args)
  return ctx;
}
```

`readContext` / `callContext` 是同步的（因为大部分 getter 是本地数据）；`executeTool`、`waitForIdle`、
`newSession/fork/navigateTree/switchSession/reload/compact` 是异步（Promise，§6.2）。
`sessionManager` 的 15 个只读方法（`getCwd/getSessionDir/getSessionId/getSessionFile/getLeafId/getLeafEntry/
getEntry/getLabel/getBranch/buildContextEntries/buildSessionProjection/getHeader/getEntries/getTree/getSessionName`）
直接映射到 `kiana-session` 的对应方法；`getBranch()`/`getEntries()` 返回 JS 数组（从 Rust 的
`Arc<Vec<Entry>>` 拷贝），examples 里最高频的就是这两个（各 7 次）。

### 7.6 `Theme` 与 `ToolRenderContext`

- `Theme`：Rust 原生类（`#[rquickjs::class]`），方法 `fg/bg/bold/italic/underline/inverse/strikethrough/
  getFgAnsi/getBgAnsi/getColorMode/getThinkingBorderColor/getBashModeBorderColor/style/colors/appearance`，
  内部直接调用 `kiana-tui` 的主题实现（`theme.ts` 已有 Rust 侧对应物，见 `ca-interactive-mode`）。
  `fg` 的返回是 ANSI 字符串，**逐字节一致**是关键（`ca-extensions` §3 的字节比对）。
- `ToolRenderContext` 的 14 个字段：`args/toolCallId/invalidate/lastComponent/state/cwd/executionStarted/
  argsComplete/isPartial/expanded/showImages/isError/durationMs/outputPad`。`invalidate` 是 JS 函数 →
  Rust 存 `Persistent`，调用时 restore；`state` 是扩展自己的任意对象（原样返回）；`lastComponent` 是
  上一次渲染的 JS 组件（原样返回，扩展用它做缓存/复用）。

### 7.7 UI 上下文绑定

`ExtensionUIContext` 的 28 个成员（去重后；`setWidget` 有两个重载）全部是「Rust 适配器 + JS 包装」：

| 方法 | JS 侧行为 | Rust 侧 |
|---|---|---|
| `select/confirm/input/editor` | 返回 Promise；`signal`/`timeout` 在 JS 侧用 §6.5 shim 实现倒计时 UI 文案 | 调用 `adapters/*` 的对话框 |
| `notify/setStatus/setWidget/setFooter/setHeader/setTitle/pasteToEditor/setEditorText/getEditorText/setWorkingMessage/setWorkingVisible/setWorkingIndicator/setHiddenThinkingLabel/getToolsExpanded/setToolsExpanded` | 同步透传 | 直接操作 TUI 状态 |
| `onTerminalInput(handler)` | 注册 JS 回调，返回退订 | TUI 的输入监听器注册；`handler` 返回 `{consume, data}` |
| `custom(factory, options)` | 调 `factory(tui, theme, keybindings, done)`（可返回 Promise），得到 JS 组件；`done(result)` resolve | 把组件挂到对话框栈/overlay；`overlayOptions` 支持函数 → 每帧调用 |
| `addAutocompleteProvider(factory)` / `setEditorComponent(factory)` / `getEditorComponent` | 存 JS 工厂 | 由 `adapters/interactive` 在构建 autocomplete/editor 时调用 JS 工厂 |
| `getAllThemes/getTheme/setTheme` | 同步 | 主题注册表 |
| `theme`（只读 getter） | `Object.defineProperty` | 当前主题对象 |

---

### 7.8 虚拟模块的运行时导出面（examples 实际用到的值）

`types.ts` 之外，扩展的「另一半 API」来自由虚拟模块导出的**运行时值**。下表统计自 86 个示例的
`import { ... }`（括号内为出现次数；`type`-only 导入在去类型后消失，不列入实现清单）。

| 虚拟模块 | 需要实现的值 | 实现位置 |
|---|---|---|
| `kiana-ai`（= `compat`） | `Type`(1)、`StringEnum`(3)、`uuidv7`(3)、`createAssistantMessageEventStream`(2)、`calculateCost`(1)、`collapseSystemMessages`(1)、`getCurrentSystemPrompt`(1)、`getCurrentTools`(1)、`anthropicMessagesApi`(1)、`openAIResponsesApi`(1)、`registerApiProvider`(1)、`streamSimple`(1) | `Type`/`StringEnum` → JS（TypeBox 风 builder，仅需 `Object/String/Number/Boolean/Optional/Array/Union/Literal/Unsafe`）；`uuidv7`/`calculateCost` → Rust；`*Api`/`streamSimple`/`registerApiProvider` → Rust 的 provider 实现，包装成 JS 函数；`createAssistantMessageEventStream` → JS 类（§7.4） |
| `kiana-tui` | `Text`(14)、`matchesKey`(11)、`visibleWidth`(10)、`Key`(6)、`truncateToWidth`(6)、`Container`(4)、`Box`(3)、`Editor`(2)、`Markdown`(2)、`wrapTextWithAnsi`(2)、`isKeyRelease`(2)、`Input`(1)、`Spacer`(1)、`SelectList`(1)、`SettingsList`(1)、`fuzzyFilter`(1)、`parseKey`(1)、`renderFakeCursor`(1)、`CURSOR_MARKER`(1) | 组件类 → Rust 原生类（§10.3）；`Key`/`CURSOR_MARKER` → JS 常量；其余纯函数 → Rust（复用 `kiana-tui` 的 `keys.rs`/`utils.rs`） |
| `kiana-coding-agent` | `getAgentDir`(6)、`CONFIG_DIR_NAME`(5)、`createBashTool`(4)、`withFileMutationQueue`(3)、`CustomEditor`(3)、`BorderedLoader`(2)、`convertToLlm`(2)、`serializeConversation`(2)、`DEFAULT_MAX_BYTES`(2)、`DEFAULT_MAX_LINES`(1)、`formatSize`(2)、`truncateHead`(2)、`defineTool`(2)、`create{Bash,Read,Edit,Write,Find,Grep,Ls}ToolDefinition`(1–3 处)、`create{Bash,Read,Edit,Write,Find,Grep,Ls}Tool`(1–7 处)、`formatReadLineRange`(2)、`DynamicBorder`(2)、`getMarkdownTheme`(2)、`getSettingsListTheme`(1)、`keyHint`(1)、`VERSION`(1)、`parseFrontmatter`(1) | `defineTool` → JS 恒等函数；`*ToolDefinition`/`*Tool`/`format*`/`truncate*`/`convertToLlm`/`serializeConversation`/`parseFrontmatter`/`withFileMutationQueue` → Rust（`kiana-tools`/`kiana-session` 已有）；`CustomEditor`/`BorderedLoader`/`DynamicBorder`/`keyHint`/`getMarkdownTheme`/`getSettingsListTheme` → Rust 原生类/函数（依赖 `Theme`）；`getAgentDir`/`CONFIG_DIR_NAME`/`VERSION`/`DEFAULT_MAX_*` → Rust 常量 |
| `kiana-agent-core` | `ThinkingLevel`（值） | JS 常量 |
| `typebox` / `typebox/compile` / `typebox/value` / `@sinclair/*` | `Type.*`（`String` 30、`Object` 20、`Optional` 19、`Array` 6、`Number` 3、`Boolean` 2） | 同一份 JS builder；**不引入 6.2 MB 的真实 typebox 包**（examples 只用 builder，且 `kiana-ai` 的 `validateToolArguments` 在 Rust 侧用 JSON Schema 校验） |

`type`-only 名称（`ExtensionAPI`、`ExtensionContext`、`Model`、`Message`、`AssistantMessage`、`TranscriptContext`、
`SimpleStreamOptions`、`OAuthCredentials`、`SessionEntry`、`AgentToolResult` 等）在去类型后**不产生任何
导入**（`/tmp/imports3.ts` 已验证），因此不占运行时实现成本——这是选 oxc 而非「按需保留 import」的
直接收益。

---

## 8. Node 内建 shim 清单（按 examples 实际用量排序）

统计自 `examples/extensions/**`（86 文件）：

| 模块 | 用到的 API（次数） | 实现 |
|---|---|---|
| `node:path` (12) / `path` (3) | `join/resolve/dirname/basename/relative/isAbsolute/sep/posix.*/extname` | Rust `std::path` + 手写 POSIX 语义；**必须复刻 `path.resolve("..")` 归一化与 `path.relative` 的边界**（`/tmp/disp_probe` 已验 `/a/b`） |
| `node:fs` (10) / `fs` (2) | `readFileSync/writeFileSync/appendFileSync/existsSync/readdirSync({withFileTypes})/statSync/stat/access/readFile/writeFile/appendFile/mkdtemp/unlinkSync/rmdirSync/mkdir/watch/constants/createReadStream/Dirent/promises.*` | 同步 API 直接调 Rust；`fs.watch` 用 `notify@8.2.1`；`createReadStream` 用一个最小 `Readable`（`on("data")/on("end")/on("error")/pipe`）；`fs.promises` 返回 Promise |
| `node:child_process` (5) / `child_process` (2) | `spawn/spawnSync/execFile/execSync/exec`；`child.stdout.on/stderr.on/on("close")/on("error")/kill` | `tokio::process::Command`；`ChildProcess` shim 提供 EventEmitter 与 `stdout/stderr`（`Readable` 风格）与 `kill(signal)`；`exec` 用 shell（`/bin/sh -c`），`execFile` 不走 shell |
| `node:os` (1) / `os` (2) | `homedir/tmpdir/platform/arch` | `std::env::home_dir` / `std::env::temp_dir` / `std::env::consts::OS`（无需额外 crate） |
| `node:url` (3) | `fileURLToPath/pathToFileURL` | `url` crate 或手写 |
| `node:util` (1) | `promisify` | 手写（`fn[util.promisify.custom]` 支持） |
| `node:zlib` (1) | `gunzipSync` | `flate2` |
| `node:readline` (1) | `createInterface({input})` + `for await (line of rl)` | 手写按行切分的 AsyncIterator（`git-merge-and-resolve.ts` 用） |
| `node:module` (1) | `createRequire(import.meta.url)` | 返回一个 `require` 函数，走 Resolver/Loader（CJS 语义）；`doom-overlay` 用它加载 `doom.js` |
| `process` | `env/cwd/argv/execPath/platform/stdout.write/stderr.write/exit/kill/arch` | host 对象；`process.argv[1]` 需指向 kiana 可执行文件（`subagent` 依赖它重spawn） |
| 全局 `Buffer` | `byteLength/concat/from/toString/isBuffer` | 用 `Uint8Array` + 手写 `Buffer` 类（不改 TypedArray 原型；`Buffer.from` 返回带方法的子类实例） |
| 全局 `TextEncoder/TextDecoder` | `encode/decode` | 手写 UTF-8（QuickJS 字符串是 UTF-8 存储，编解码即 `String` ↔ `Uint8Array`；非 UTF-8 编码不在扩展示例范围内） |
| 全局 `URL/URLSearchParams` | `new URL(...)`、`searchParams` | `url` crate |
| 全局 `structuredClone` | `debug-provider.ts`、`validation.ts` | JS 递归克隆（支持 `Date/Map/Set/ArrayBuffer/TypedArray`；不支持 `Function/WeakMap`） |
| 全局 `console` | `log/warn/error` | 写 kiana 的日志（`log-debug` 等价物） |
| 全局 `fetch` | `custom-provider-anthropic`、`doom-overlay/wad-finder.ts` | `reqwest` + JS `Response`/`ReadableStream` shim（§9.4） |
| 全局 `crypto` | `getRandomValues`、`randomUUID`、`subtle.digest("SHA-256")` | `getrandom` + `uuid` + `sha2` |
| 全局 `setTimeout/clearTimeout/setInterval/clearInterval` | 多 | §6.4 |
| 全局 `performance` | `now()`（QuickJS 原生有 `performance.now`，`/tmp/featprobe` 实测可用） | 直接用 |
| `node:events` | `EventEmitter`（`event-bus.ts` 通过宿主创建，不需 shim；但 npm 包可能用） | 最小实现 |
| `node:stream` | SDK 用 `ReadableStream`（Web）而非 `node:stream` | 不需要 |
| `node:http/https/net` | **examples 源码未直接 import**，但 `sandbox/` 的依赖链**必需**（见下方注） | 需提供最小 `Server`/`Socket`：`createServer().on("request"/"connect")`、`listen(port,host,cb)`、`address()`、`unref()`；`net.connect`/`createConnection`、`net.createServer`（socks5 用）。实现：tokio `TcpListener`/`TcpStream` + `Readable`/`Writable` shim |
| `node:tls/dgram` | 未用（`sandbox` 亦未用） | 不提供 |
| `worker_threads` | 未用 | 不提供 |

**关于 `sandbox/` 的 `node:http`/`node:net` 需求**（`/tmp/sbprobe` 实测）：`@anthropic-ai/sandbox-runtime@0.0.26`
的 `dist/sandbox/http-proxy.js` 直接 `import { createServer } from 'node:http'`、`connect from 'node:net'`；
其依赖 `@pondwader/socks5-server@1.0.10`（CJS，`require("net")`）会 `net.createServer` 与
`net.createConnection`。这些是**真实的 TCP 服务器/套接字**，不是桩：`initialize()` 会 `listen(0,'127.0.0.1')`
并读取 `server.address().port`，随后把该端口写进 `HTTP_PROXY`/`ALL_PROXY` 环境变量传给被沙箱化的子进程。
因此若要让 `sandbox/` 达到 A 级，§8 必须提供**可用的** `node:net` TCP 实现（tokio `TcpListener`/
`TcpStream` + `Readable`/`Writable` 语义），而不能只在 `initialize()` 里返回假端口——假端口会让
被沙箱化的 `npm install`/`git clone` 连到一个不存在的代理而失败。

`/tmp/sbprobe` 的正面结论：即便 `net`/`http` 是桩，整条依赖图（664 个 ESM 模块、921 KB、含 `zod@3.25.76`
的 88 个 `.js`）在 QuickJS 下 **0 语法失败**、链接并求值 **75-83 ms**（4 次实测；Node 26 本机同一 `import()` 为 279-299 ms），
`zod` 的 `safeParse` 成功/失败路径与 `issues[0].code === "invalid_type"` 均与 Node 一致，
`getDefaultWritePaths()` 返回同样 10 项。`initialize()` 推进到「创建 bridge socket」这一步才停
（`Failed to create bridge sockets after 5 attempts`）；补上 `setTimeout` 与 `process` 之后唯一缺的就是
真实 `net`。停点是宿主能力，而非语法或模块系统。

### 8.1 关键语义风险（必须逐条对齐）

- `fs.readdirSync` 的排序：`subagent/agents.ts:71` 与 `claude-rules.ts:34` 直接遍历 `readdirSync` 结果、
  未自行排序，因此 Rust 侧用 `readdir(3)` 的 OS 顺序即可，**不要**额外排序（Node 也不排序；额外排序会
  改变扩展看到的顺序）。
- `path.resolve` 的 `..` 归一化、Windows 盘符/UNC：`/tmp/disp_probe` 只测了 POSIX；Windows 分支需单测。
- `Buffer.toString("utf8")` 的非法序列替换为 U+FFFD。
- `child_process.spawn` 的 `stdio: ["ignore","pipe","pipe"]`：`ignore` 不能让子进程继承 kiana 的 stdin；
  `overlay-qa-tests.ts` 用 `spawn` 启动自身做 overlay 测试。
- `process.exit`：在 JS 线程里不能直接 `exit`，要投递 `HostTask::Exit(code)` 到主线程，走正常 shutdown。
- `process.kill`：`process.kill(pid, signal)`（`overlay-qa-tests` 用）→ `libc::kill`。

---

## 9. 全局 JS shim（prelude.js）

### 9.1 结构

```text
js/prelude.js
  ├─ require/builtin 安装：__kiana_host.* → 全局（Buffer/process/console/fetch/crypto/timers/...）
  ├─ CJS 加载器：__kiana_make_require / __kiana_cjs（§5.6.1）
  ├─ Web 平台对象：Response/Headers/FormData/Blob/File/ReadableStream/AbortController/AbortSignal/URL/URLSearchParams
  ├─ Intl 最小实现（§9.2）
  ├─ Web Streams 子集（ReadableStream/TransformStream，§9.4）
  ├─ structuredClone（§9.5）
  ├─ EventEmitter（见 §8 的 `node:events`）
  └─ 错误工具：makeAbortError、makeTypeError（文案对齐 V8）
```

上表第二、三行的清单不是「以防万一」，而是 `@anthropic-ai/sdk` 与 `sandbox-runtime` 实测**逐个点名**
要求的（`/tmp/sbprobe`，缺一个就报 `X is not defined` 并中断加载/请求）：`fetch`/`Response`/`Headers`/
`ReadableStream`/`TextEncoder`/`TextDecoder`/`AbortController`/`AbortSignal`/`FormData`/`Blob`/`File`/
`URL`/`URLSearchParams`/`console`/`process`/`setTimeout`。`AbortSignal` 必须是**可监听的类**
（`addEventListener("abort", fn)` 在已 abort 时同步/微任务触发一次），`Headers` 必须大小写不敏感。

所有 shim 的 `Error` 文案要尽量对齐 V8（例如 `The "path" argument must be of type string. Received undefined`
——`ca-extensions` §3.5 要求扩展作者看到的 TypeError 文本等价）。**不可能逐字等价**的地方在 §11 明说。

### 9.2 Intl（最大缺口）

QuickJS（quickjs-ng 0.16.2）**没有编译 Intl**（`/tmp/featprobe`：`typeof Intl === "undefined"`）。
但实测 `Date#toLocaleString` / `toLocaleDateString` / `toLocaleTimeString` 与 `Number#toLocaleString`
**是存在的**（内置固定实现）：

| 表达式（TZ=UTC+8 主机） | QuickJS 实测 |
|---|---|
| `new Date(0).toLocaleString()` | `01/01/1970, 08:00:00 AM` |
| `new Date(0).toLocaleDateString()` | `01/01/1970` |
| `new Date(0).toLocaleTimeString()` | `08:00:00 AM` |
| `(1234.5).toLocaleString()` | `1234.5`（**无千分位**） |
| `new Intl.Segmenter(...)` | `Intl is not defined` |

影响面：

- `Intl.Segmenter`：`kiana-tui` 的 `getGraphemeSegmenter()`/`getWordSegmenter()`（`utils.ts:4-5`）在模块加载时
  就 `new Intl.Segmenter(...)`，因此**直接跑 kiana-tui 的 JS 源码会在 import 时崩溃**。这是 §9 采用
  「Rust 原生组件 + JS 薄层」的根本原因。
- `Number#toLocaleString`：`custom-compaction.ts:38`（`tokensBefore.toLocaleString()`）在 UI 里显示
  `1234.5` 而非 `1,234.5`。这是**可观察差异**，必须由 shim 补。
- `Date#toLocaleString`：`entry-renderer.ts:26`、`input-transform.ts:37`、`overlay-qa-tests.ts:1294`
  直接展示；QuickJS 的固定格式与 Node（ICU）的 `1/1/1970, 8:00:00 AM` 不同。

方案（`js/prelude.js` + Rust）：

1. **`Intl.Segmenter`**：JS 类，`segment(text)` 返回可迭代对象，元素 `{segment, index, input}`；
   grapheme 用 Rust `unicode-segmentation` 的 `graphemes(true)`；word 用 `unicode-segmentation` 的
   `unicode_words()` + 标点/空白切分（近似 UAX#29 word boundary）。这与 Node 22 的 ICU 输出
   **在小部分 emoji ZWJ 序列与泰文/高棉文上可能不同**（`ca-interactive-components.md` 已注明同类风险）。
   由于 kiana-tui 在 Rust 侧重写后不再调用它，此 shim 只服务扩展自己（当前 examples 无直接使用）。
2. **`Intl.NumberFormat`**：只实现 `en-US` 的 `decimal`（`useGrouping`、`minimumFractionDigits`、
   `maximumFractionDigits`），其余 `throw new RangeError("Intl.NumberFormat: unsupported locale")`。
3. **`Number#toLocaleString` / `Date#toLocaleString`**：**覆盖内置方法**（`Object.defineProperty`），
   用 `Intl.NumberFormat` 与 `chrono` 复刻 Node 的 `en-US` 输出（`1/1/1970, 8:00:00 AM`）。
   这是「让 examples 输出一致」的最小代价方案；覆盖内置方法需在文档中标注（扩展若显式依赖 QuickJS
   的内置行为会不同，但 examples 没有）。
4. **`Intl.Collator`/`DateTimeFormat`/`PluralRules`**：不实现，抛 `TypeError`。

残余风险：即使 shim，`new Date().toLocaleString()` 的 ICU 时区名/夏令时行为与 Node 仍有差异（§11.3）。

### 9.3 `fetch` 与 Web Streams

`custom-provider-anthropic` 的 `streamSimple` 用 `fetch` + `response.body.getReader()`（SSE 读取）；
`doom-overlay/wad-finder.ts` 用 `fetch` 下载 WAD。实现：

- Rust `reqwest` 发起请求，返回一个 JS `Response` 对象：
  - `status`、`statusText`、`headers.get(name)`、`ok`、`url`；
  - `text()`/`json()`/`arrayBuffer()` 返回 Promise；
  - `body` 是 `ReadableStream`（§9.4），`getReader()` 逐 chunk 返回 `Uint8Array`；
- `Request`/`Headers`/`URLSearchParams` 的最小实现；`fetch(url, {method, headers, body, signal})`。
- SSE 解析（`@anthropic-ai/sdk` 的 `Stream.fromReadableStream` 自己解析）；`TextDecoder` 需要支持
  `{stream: true}`（增量解码）。

### 9.4 Web Streams 子集

`ReadableStream`（`start/pull/cancel`、`getReader()`、`[Symbol.asyncIterator]`、`tee()` 可选）、
`WritableStream`/`TransformStream`（SDK 只用到 `ReadableStream` 与 `makeReadableStream`）。
由于 `fetch` 的 body 由 Rust 推送，`ReadableStream` 需要「宿主推 → JS 读」的桥：JS 侧实现
`ReadableStream` 的 `pull` 时调用 `__kiana_host.readBodyChunk(id)`（返回 Promise），Rust 侧从
`reqwest::Response::chunk()` 逐块回填。`/tmp/stream_probe` 已验证「宿主推 → JS `for await`」与
「JS 缓冲后消费」两条路径。

**`pull`/`read()` 的竞态是这条路径上最容易写错的地方**（`/tmp/sbprobe` 实测踩到）：`pull(controller)`
可能在 `read()` 注册 waiter **之前**就同步 `enqueue`。若实现是「`read()` 时若队列空则挂 waiter」，
挂完之后**不重新检查队列**，那个 Promise 就永远不会 settle——整个 `for await` 静默挂起，无异常、无日志。
正确写法：`push waiter` 之后立刻 `pump()`（或统一让 `enqueue/close/error` 与 `read` 都走同一个 `pump`）。
另外 SDK 的 `ReadableStreamToAsyncIterable` 会**先看 `stream[Symbol.asyncIterator]`**，只有缺失时才
`getReader()`，所以两条路径都要实现且语义一致（`/tmp/sbprobe` 的 polyfill 自测：
`getReader` 与 `for await` 各读 `a,b,c` 均正确）。

`Response` 还需 `headers.entries()`（SDK 读 `request-id`）与 `body` 可为 `ReadableStream`；`Headers`
需 `get/set/append/delete/has/entries` 且 **key 大小写不敏感**（初版用 `Map` 直接存原 key，
导致 `buildHeaders` 里 `.delete(name)` 与 `.append(name, ...)` 命中不同 key，SDK 报 `"not a function"`
式的诡异错误）。

### 9.5 `structuredClone`

支持 `Object/Array/Date/Map/Set/ArrayBuffer/TypedArray/DataView/RegExp`、循环引用、`undefined` 属性；
`Function/Symbol/WeakMap/Promise` 抛 `DataCloneError`（文案对齐 V8）。`debug-provider.ts` 用它克隆
`provider_stream_event` 的 `data`（可能是任意 provider 对象），因此必须能处理**嵌套的宿主对象**
（例如 provider 事件里的 `usage`、`content` 数组）。

---

## 10. TUI 组件互操作

### 10.1 核心约束

`kiana-tui` 的 `Component` 是：
```ts
interface Component { render(width: number): string[]; handleInput?(data: string): void;
  handleMouse?(e): TuiMouseEventResult | undefined; wantsKeyRelease?: boolean; invalidate(): void; }
```
examples 里 **30 处 `ctx.ui.custom`**（`overlay-qa-tests.ts` 占 15 处，其余分布在 `interactive-shell/tools/todo/tic-tac-toe/summarize/space-invaders/snake/question/questionnaire/qna/preset/overlay-test/handoff/doom-overlay`）、
**4 个文件**注册 `registerMessageRenderer`/`registerEntryRenderer`（共 7 处：message 4、entry 3）、
**10 个文件**的 `registerTool` 带 `renderCall`/`renderResult`。组件由**扩展 JS** 定义（类实例或对象字面量），
必须被 Rust TUI 每帧调用。

### 10.2 桥接模型

```text
Rust TUI (render loop) ── render(width) ──▶ JsComponentHandle (Rust)
                                             │ sync call via SyncSender
                                             ▼
                                        JS thread: component.render(width)
                                             │ returns string[]
                                             ▼
                                        Rust: Vec<String>（校验每行宽度 <= width，超宽按 tui-core 截断）
```

- **组件身份**：Rust 侧 `JsComponentHandle { id: u64 }` 实现 `kiana_tui::Component` trait，内部持有
  `Arc<JsBridge>`；`render/handle_input/invalidate` 都走同步桥。`render` 调用失败（JS 抛错）→
  返回 `[theme.fg("error", "<extension error>")]` 并上报（`ca-extensions` §3.6）。
- **输入**：`handle_input(data)` 同步；`wants_key_release` 是属性，创建后读取一次（examples 里
  `overlay-qa-tests` 动态改的话需要每帧读，见下）。
- **属性读取**：`wants_key_release` / `focused` 之类属性可能在运行中变化；实现为「每帧/每次输入前
  同步读一次」（成本 ~1 µs）。
- **`Focusable`**：`isFocusable` 的鸭子判定（`"focused" in component`）→ Rust 侧 `has_focusable(id)`
  同步询问。
- **`dispose()`**：对话框关闭时调用（若存在），并释放 `Persistent`。

性能（`/tmp/xthread` 实测）：

| 指标 | 值 |
|---|---|
| 跨线程 `render(80)` 往返（含 6 行 + Theme.fg + 计数器） | 平均 **27.0 µs**，最坏 **279.6 µs**（600 帧） |
| JS 本地 `render(80)` | 2.9 µs |
| `overlay-qa-tests` 的 45 KB 单文件（100+ 组件） | 未实测；风险是「一帧内多个组件各一次往返」→ 最坏 30 个组件 × 27 µs ≈ 0.8 ms，仍在预算内 |

优化储备：若某组件每帧都渲染且开销大，可把「整棵扩展组件子树」的 `render` 合并为一次 JS 调用
（JS 侧一个 `Container.render`），Rust 只跨线程一次。

### 10.3 `kiana-tui` 虚拟模块的实现形态（关键设计）

**不把 kiana-tui 的 TS 源码编译成 JS 跑在 QuickJS 里**，理由：

1. `utils.ts` 在模块顶层 `new Intl.Segmenter(...)` → 崩溃（无 Intl）；
2. `components/markdown.ts` 依赖 `marked` + `marked-cjk-friendly`（npm 包，JS 可跑但体积/性能另算）；
3. `native-platform.ts` 用 `createRequire` 加载 `.node` 原生模块（剪贴板）→ QuickJS 无法加载；
4. `autocomplete.ts` 依赖 `fs.readdirSync/statSync`（可 shim，但把 kiana-tui 的 autocomplete 逻辑放 JS 无收益）。

因此 `@earendil-works/kiana-tui` 虚拟模块由两部分组成：

- **Rust 原生类**（`#[rquickjs::class]`）：`Text`、`Box`、`Container`、`Spacer`、`Markdown`、`SelectList`、
  `SettingsList`、`Input`、`Editor`、`Loader`、`ScrollView`、`Image`、`TruncatedText`、`HStack`、`VStack`、
  `CancellableLoader`、`DynamicBorder`（后者在 coding-agent 里，但可共用）。每个类的 `render(width)` 直接
  调用 Rust 的 `kiana-tui` 组件实现；`addChild/removeChild/invalidate` 同理。
- **纯 JS 工具**（`js/virtual/kiana-tui.js`）：`Key`（含 `ctrl/alt/shift/super/ctrlShift/...` 助手）、
  `matchesKey`、`parseKey`、`isKeyRelease`、`visibleWidth`、`truncateToWidth`、`wrapTextWithAnsi`、
  `fuzzyFilter`、`renderFakeCursor`、`CURSOR_MARKER`。这些是**纯函数**，可以直接用 Rust 实现（更快）
  或 JS 实现（更易对齐）。建议：`Key`/`CURSOR_MARKER` 用 JS 常量；`matchesKey/parseKey/isKeyRelease`
  调 Rust（复用 `kiana-tui` 的 `keys.rs`）；`visibleWidth/truncateToWidth/wrapTextWithAnsi` 调 Rust
  （`unicode-width` + ANSI 解析，必须与 TUI 其它部分一致）。

风险：`class MyComp extends Container` 在 Rust 原生类上 `extends` 可行（`/tmp/classprobe` 实测
`instanceof-native-base: true`、`super` 调用、JS 字段），但**原生类的属性访问器/方法名**要逐个确认
（`/tmp/classprobe` 的 `virtual-render` 输出 `vim-render:native-render:abc:80` 证明 JS 子类覆写
`render` 后 `super.render` 正常）。`Container.children` 在 JS 侧应当是 `Proxy` 或 getter 返回数组副本？
examples 里 `container.addChild(...)` 用得最多（`Container` 4 次），没有直接改 `children` 数组的；
Rust 原生 `addChild` 即可。

### 10.4 `Theme` 也是原生类

`theme.fg("accent", text)` 出现 317 次，是最高频调用。原生类 + 直接返回 ANSI 是最优路径；
必须保证「同一 token 的 ANSI 与 Rust TUI 渲染的完全一致」（`theme.ts` 已有 Rust 侧实现）。

---

## 11. 兼容性判定（逐示例）

判定口径：

- **A（原样可用）**：`.ts` 不改一行，加载 + 注册 + 主要行为与 TS 版一致；
- **B（可运行，有可观察差异）**：能跑，但输出/时序/数值有差异（逐条列明）；
- **C（需宿主能力，默认不可用）**：需要 WebAssembly/原生 addon/网络等，除非打开特性；
- **D（不可等价）**：依赖 Node 原生 addon 或 QuickJS 无法提供的能力。

### 11.1 单文件示例（70 个）

**A 级（66 个）**：`auto-commit-on-exit.ts`、`bash-spawn-hook.ts`、`bookmark.ts`、`border-status-editor.ts`、
`built-in-tool-renderer.ts`、`claude-rules.ts`、`commands.ts`、`confirm-destructive.ts`、`custom-footer.ts`、
`custom-header.ts`、`debug-provider.ts`、`dirty-repo-guard.ts`、`dynamic-tools.ts`、`event-bus.ts`、
`file-trigger.ts`、`git-checkpoint.ts`、`github-issue-autocomplete.ts`、`git-merge-and-resolve.ts`、
`handoff.ts`、`hello.ts`、`hidden-thinking-label.ts`、`inline-bash.ts`、`input-transform-streaming.ts`、
`interactive-shell.ts`、`jev-router.ts`、`mac-system-theme.ts`、`message-renderer.ts`、`minimal-mode.ts`、
`modal-editor.ts`、`model-status.ts`、`notify.ts`、`overlay-test.ts`、`permission-gate.ts`、`pirate.ts`、
`preset.ts`、`project-trust.ts`、`prompt-customizer.ts`、`protected-paths.ts`、`provider-payload.ts`、
`qna.ts`、`question.ts`、`questionnaire.ts`、`rainbow-editor.ts`、`reload-runtime.ts`、`rpc-demo.ts`、
`send-user-message.ts`、`session-name.ts`、`shutdown-command.ts`、`snake.ts`、`space-invaders.ts`、`ssh.ts`、
`status-line.ts`、`structured-output.ts`、`summarize.ts`、`system-prompt-header.ts`、`tic-tac-toe.ts`、
`timed-confirm.ts`、`titlebar-spinner.ts`、`todo.ts`、`tool-override.ts`、`tools.ts`、`trigger-compact.ts`、
`truncated-tool.ts`、`widget-placement.ts`、`working-indicator.ts`、`working-message-test.ts`。

其中 `ssh.ts`、`interactive-shell.ts`、`mac-system-theme.ts` 依赖 `child_process`，但在 TS 版里同样只是
子进程调用（`ssh` / `defaults read`），没有额外宿主能力，因此判定为 A。

前提：§8/§9 的 shim 全部到位，且 `kiana.exec`/`ctx.ui.*`/`Theme` 的原生实现完成。**注意**：
`minimal-mode.ts` 依赖 `createBashToolDefinition` 等 SDK 工厂（这些在 Rust 侧是 `kiana-tools` 的
`ToolDefinition` 构造器，见 §7.3）；`git-merge-and-resolve.ts` 依赖 `fs.createReadStream` + `readline`；
`overlay-qa-tests.ts`（45 KB，B 级）是最大压力测试，其 `spawn` 自身做 overlay 测试依赖
`process.execPath`/`argv[1]`。

**B 级（4 个，有可观察差异；采用 §9.2 的 `toLocaleString` 覆盖后可升为 A）**：

| 示例 | 差异 | 原因 |
|---|---|---|
| `custom-compaction.ts` | `tokensBefore.toLocaleString()` 输出无千分位 | 无 Intl.NumberFormat |
| `input-transform.ts` | `new Date().toLocaleString()` 格式与 Node ICU 不同 | 无 Intl.DateTimeFormat |
| `entry-renderer.ts` | 同上 | 同上 |
| `overlay-qa-tests.ts` | `new Date().toLocaleTimeString()`（`overlay-qa-tests.ts:1294`） | 同上 |

这四个示例除了该格式化文本外，其余行为均为 A 级；若接受覆盖内置 `toLocaleString`（§9.2），则 70 个
单文件全部为 A 级。

### 11.2 目录示例（9 个）

小计：**7 个 A**（`with-deps`、`dynamic-resources`、`plan-mode`、`subagent`、`sandbox`、`custom-provider-gitlab-duo`、
`custom-provider-anthropic`）、**1 个 C**（`doom-overlay`）、**1 个 D**（`gondolin`）。

| 示例 | 判定 | 依据 |
|---|---|---|
| `with-deps/` | **A** | `ms@2.1.3` 是纯 CJS，`/tmp/cjsmod` 已实测；`typebox` 虚拟模块 |
| `dynamic-resources/` | **A** | `node:path`/`node:url` + `resources_discover` 事件 |
| `plan-mode/` | **A** | `/tmp/loadprobe` 实测：`strip=0.44 ms, declare=1.0 ms, eval=0.09 ms, factory=0.03 ms`，注册 22 项（`on/registerTool/registerCommand/registerFlag/registerShortcut`）|
| `subagent/` | **A（需 shim）** | `spawn` + `readline` + `fs` + `os` + `Buffer`；`/tmp/loadprobe` 在补 `node:child_process` shim 前 DECLARE-FAIL，补齐后即可（`process.argv[1]`/`execPath` 必须指向 kiana 二进制） |
| `sandbox/` | **A（需 shim + 一个前提）** | `/tmp/sbprobe` 已把整条依赖图跑通：`@anthropic-ai/sandbox-runtime@0.0.26` + `zod@3.25.76` + `commander@12` + `shell-quote@1.12` + `@pondwader/socks5-server@1.0.10`，共 **664 个 ESM 模块 / 921 KB / 75-83 ms** 链接求值，0 语法失败；`zod` 的 `safeParse` 行为与 Node 26 逐字一致。它 `spawn` `bwrap`/`socat`（本机 `which bwrap socat` 均有），**无原生 addon**。前提：必须实现**真实可用**的 `node:net`/`node:http` TCP 服务器（socks5-server 与 http-proxy 会 `listen` 并读 `address().port`；假端口会让被沙箱化的子进程连向不存在的代理），以及 §5.6.1 的真 CJS `require`（`shell-quote` 是多文件 CJS）。`initialize()` 实测停在「创建 bridge socket」（`Failed to create bridge sockets after 5 attempts`），原因仅为缺真实 `net`，非语法/模块系统问题 |
| `custom-provider-gitlab-duo/` | **A（需 shim + 实测）** | `/tmp/loadprobe` 实测可加载并注册 provider（`registerProvider` 在 kinds 里）；它调 `anthropicMessagesApi().streamSimple`/`openAIResponsesApi().streamSimple`（`kiana-ai` 虚拟模块内的 Rust 实现）→ 走 Rust 的 provider，不经过 JS SDK |
| `custom-provider-anthropic/` | **A（需 shim；SDK 已实测）** | `/tmp/sbprobe` 实测 `@anthropic-ai/sdk@0.52.0` 在 QuickJS 下**完整跑通**，且 **CJS 入口（`index.js`，43 个 CJS 模块，约 18 ms）与 ESM 入口（`index.mjs`，43 个 ESM 模块 / 178 KB，约 12 ms）两条路径都通**；`new Anthropic({apiKey})` 构造成功、`messages.create()` 走通、**`messages.stream()` 的 SSE 全链路成功**（`finalMessage().content[0].text === "hello"`，`on("text")` 收到 `["hello"]`），与 Node 26 同一 fake-fetch 控制组的输出**逐字相同**。所需全局：`fetch`/`Response`/`Headers`/`ReadableStream`/`TextEncoder`/`TextDecoder`/`AbortController`/`AbortSignal`/`FormData`/`Blob`/`File`/`URL`/`URLSearchParams`/`console`/`process`/`setTimeout`（**`setTimeout` 必须遵守 delay**：SDK 的 `fetchWithTimeout` 用 `setTimeout(() => controller.abort(), ms)`，若 shim 立即触发会把每个请求 abort 成 `APIUserAbortError`——这是实测踩到的坑）。OAuth 用 `crypto.getRandomValues` + `crypto.subtle.digest("SHA-256")`（§9 范围内）|
| `doom-overlay/` | **C** | 需要 `WebAssembly` + `new Function("module","exports",...)` + `FS_createDataFile`（Emscripten FS）→ 需 `wasmtime` 特性 + Emscripten shim；默认关闭 |
| `gondolin/` | **D** | `@earendil-works/gondolin@0.14.0` 依赖 `ssh2`（含可选 `cpu-features` 原生 addon）、`cbor2`、`undici`，以及 `krun-runner` 原生可执行文件。`ssh2` 在纯 JS 下不可用（需要 `crypto` 的 native binding 或纯 JS fallback；`ssh2` 有纯 JS 路径但依赖 `bcrypt-pbkdf`/`asn1`，且 `cpu-features` 为 optional 可跳过）。**结论：默认不可用；若目标机器接受「外部 runner + ssh2 纯 JS」，可降为 C** |

### 11.3 「不能等同」的诚实清单

1. **`Intl`**：`Segmenter` 的 UAX#29 边界、`Collator` 排序、`DateTimeFormat` 的时区名/夏令时、`PluralRules`
   均无法与 Node 的 ICU 完全一致。已覆盖：`toLocaleString` 的 en-US 常见路径。
2. **原生 addon**：`.node` 文件（`gondolin` 的 krun runner、`@silvia-odwyer/photon-node`、kiana-tui 的剪贴板
   native helper）无法加载；`createRequire` 只能加载 JS/CJS。
3. **WebAssembly**：默认无；`wasmtime` 特性提供同步/异步实例化与 Emscripten FS 需要额外 shim，
   `doom-overlay` 的 `new Function` + `FS_createDataFile` 组合仍可能因 Emscripten 运行时的
   `process`/`globalThis` 假设失败。
4. **Node 流语义**：`child_process` 的 `stdio` 继承/`pipe` 背压、`fs.createReadStream` 的
   `highWaterMark`/`pause/resume` 与 Node 不完全一致；examples 只用 `on("data")`，因此当前语料不受影响。
5. **模块解析边界**：`package.json` 的 `imports`（`#foo`）、`exports` 的 `node-addons` 条件、Yarn PnP、
   `--preserve-symlinks` 等不保证；jiti 的 `moduleCache:false` 语义按 §5.5 近似。
6. **`process.nextTick`**：shim 只对扩展代码生效，不参与 TUI 的渲染调度（TUI 用 Rust 侧调度）。
7. **错误文本**：V8 的 `TypeError` 文案（如 `Cannot read properties of undefined (reading 'x')`）与
   QuickJS 的文案不同；`ca-extensions` §3.5 的「扩展作者可见 TypeError」无法逐字一致，只能保证
   `message` 中包含相同的语义信息。
8. **`Function.prototype.toString`**：QuickJS 的函数源码打印格式与 V8 不同；依赖 `fn.toString()`
   做源码分析的库（少见）会得到不同字符串。
9. **`process.versions.node` 伪造**：`@anthropic-ai/sdk` 用 `globalThis.process?.versions?.node` 判断
   Node 版本（`internal/uploads.mjs:4`）。我们提供 `process.versions.node`（固定 `"22.0.0"`）以满足该分支，
   但这会让包以为自己在真 Node 上运行，个别包可能因此走 Node 专属分支（如 `node:stream` 的高阶 API），
   需按包实测（见 §16 Q1/Q2）。
10. **`worker_threads` / `node:vm` / `node:cluster`**：不提供；examples 未使用。
11. **`setTimeout` 的时序必须真实**：SDK 的 `fetchWithTimeout` 用 `setTimeout(() => controller.abort(), ms)`
    实现请求超时。若 shim 把 delay 忽略、立刻执行回调（最省事的写法），**每个请求都会被自己 abort 成
    `APIUserAbortError`**，且错误信息完全看不出根因（`/tmp/sbprobe` 实际踩到，排查花了数轮）。定时器必须
    按 delay 排序、挂在 JS 线程的事件循环上（§6.4），`clearTimeout` 必须能真正取消。
12. **`ReadableStream` 的 `pull` 与 `getReader().read()` 有竞态**：`pull` 可能在 `read()` 注册 waiter
    **之前**同步 `enqueue`；若实现只在 `read()` 里挂 waiter 而不在挂完后重新检查队列，Promise 永不 settle
    （表现为整个流挂死，无报错）。`/tmp/sbprobe` 的第一版 polyfill 正是这个 bug。实现必须在
    `push waiter → 重新 pump`，并支持 `asyncIterator`（SDK 的 `ReadableStreamToAsyncIterable` 优先用
    `Symbol.asyncIterator`，退化路径才用 `getReader`）。

---

## 12. 性能

### 12.1 加载（`/tmp/loadprobe` 实测，release）

| 示例 | strip | declare（含编译） | eval | 工厂调用 | 备注 |
|---|---|---|---|---|---|
| `hello.ts`（0.6 KB） | 0.25 ms | 0.42 ms | 0.04 ms | 0.02 ms | 最小扩展 |
| `permission-gate.ts`（1.0 KB） | 0.09 ms | 0.08 ms | 0.01 ms | 0.03 ms | |
| `todo.ts`（~10 KB） | 0.37 ms | 0.76 ms | 0.09 ms | 0.02 ms | 注册 6 项 |
| `plan-mode/index.ts` | 0.44 ms | 1.00 ms | 0.09 ms | 0.03 ms | 注册 22 项 |
| `custom-provider-gitlab-duo/index.ts` | 0.29 ms | 0.59 ms | 0.05 ms | 0.04 ms | 注册 23 项 |
| 全量 86 文件去类型 | — | — | — | — | **46.8 ms / 451 KB** |

结论：**单个扩展的加载在 1.5 ms 量级**，86 个扩展全量加载 < 150 ms（含 22 个虚拟模块 declare）。

### 12.2 调用开销

| 操作 | 实测 | 说明 |
|---|---|---|
| JS 函数调用（对象参数） | 90–104 ns | 100,000 次循环 |
| JS 函数调用（int 参数） | 108–116 ns | |
| `ctx.eval` 一个 async IIFE（`await null`）+ 微任务排空 + 取结果 | **6.07 µs/次** | 10,000 次；**含解析/编译**，是「每次派发都重新 eval」的上界；真实派发复用已编译函数（≈ 0.1 µs + 一次微任务排空） |
| 跨线程 `render(80)` 往返 | **27.0 µs** | 600 帧；最坏 279.6 µs |
| 单帧渲染（Rust 本地） | 2.9 µs | 对照 |

含义：41 个事件的派发若每个 handler 都 await 一次，成本 ~6 µs/handler；100 个 handler = 0.6 ms，
可接受。TUI 组件渲染是主要成本（27 µs × 组件数），但仍远低于 16.7 ms 帧预算。

### 12.3 内存

- 每个 JSRuntime 默认上限设 128 MB（可配）；一个扩展代的实际占用取决于扩展（TypeBox schema 与
  SDK 是大头：`typebox` build 6.2 MB 磁盘，运行时对象小）。
- `Persistent` 的泄漏风险：所有 waiter 在代销毁时清空；组件 `dispose()` 必须释放。

### 12.4 构建产物

- `rquickjs-sys` 用 `cc` 编译 quickjs-ng 的 C 源（`build.rs` 13.6 KB，含 wasm-shim 目录但默认不走）；
  交叉编译到 musl/Windows 需要 `cc` 目标工具链（已在 `kiana` 的 CI 矩阵内）。
- 与 `oxc` 的 `oxc_allocator`（arena）在高频路径上的 GC 压力：去类型是「一次性」，没有影响。

---

## 13. 模块划分（落到 `ca-extensions` §8.1）

```text
kiana-extensions/src/
├── loader/
│   ├── mod.rs            // 公开函数（同 `ca-extensions` §8.1）
│   ├── js_host.rs        // 线程/桥/代际：JsHost, JsBridge, HostTask, Waiter, Generation
│   ├── engine.rs         // Runtime 构建、limits、interrupt、GC、prelude 装载
│   ├── resolver.rs       // §5.4 的解析顺序（含虚拟模块表、oxc_resolver）
│   ├── loader_impl.rs    // rquickjs Loader：TS strip / CJS wrap / 虚拟模块
│   ├── ts.rs             // oxc 流水线（§5.2）
│   ├── cjs.rs            // CJS 检测与包装、顶层 exports.* 扫描
│   └── cache.rs          // moduleCache:false 的唯一名（§5.5）+ 工厂缓存（§2.2.2）
├── bindings/
│   ├── api.rs            // ExtensionApi 的 JS 对象（js/api.js 的 Rust 宿主原语）
│   ├── context.rs        // contextGetters / callContext（§7.5）
│   ├── ui.rs             // ExtensionUIContext（§7.7）
│   ├── tools.rs          // ToolDefinition ↔ AgentTool（wrapper.rs）
│   ├── renderers.rs      // 消息/条目/markdown/tool renderer
│   ├── provider.rs       // streamSimple ↔ AssistantMessageEventStream（§7.4）
│   ├── classes/          // #[rquickjs::class]：Theme, Text, Box, Container, ...（§10.3）
│   └── host.rs           // __kiana_host 的构建（一个 Object，挂所有原语）
├── node/
│   ├── mod.rs            // 内建模块表（§8）
│   ├── fs.rs, path.rs, child_process.rs, os.rs, url.rs, util.rs, zlib.rs, readline.rs, module.rs
│   ├── globals.rs        // Buffer/process/console/TextEncoder/TextDecoder/URL/...
│   └── net.rs            // fetch/Web Streams/crypto（reqwest + shim）
├── js/                   // 编译期 include_str! 进二进制
│   ├── prelude.js        // §9
│   ├── api.js, context.js
│   └── virtual/{typebox,kiana-ai,kiana-tui,kiana-agent-core,kiana-coding-agent}.js
└── tests/
    ├── corpus.rs         // 86 文件加载/注册项矩阵（§14）
    ├── engine.rs         // 桥/超时/中断/GC
    └── node_shims.rs     // fs/path/Buffer/child_process 的 Node 行为对齐用例
```

`js_host.rs` 的关键类型：

```rust
pub struct JsHost { tx: mpsc::Sender<HostTask>, join: JoinHandle<()>, gen: Generation, limits: Limits }
pub struct JsBridge { tx: mpsc::Sender<HostTask> }
pub struct Waiter { resolve: Persistent<Function<'static>>, reject: Persistent<Function<'static>>, kind: WaiterKind }
pub enum HostTask { Call{..}, Event{..}, Render{..}, Input{..}, Resolve{..}, Timers{..}, Exit(i32), Quit }
pub struct Generation { id: u64, api: Arc<ExtensionApiImpl> }
```

---

## 14. 测试与验收

### 14.1 语料测试（`tests/corpus.rs`）

对 `examples/extensions/**` 的 86 个 `.ts`：

1. 去类型成功 + `Module::declare` 成功（不执行）；
2. 执行工厂，断言注册项集合（tools/commands/flags/shortcuts/renderers 的 key 集合）与
   `ca-extensions` §3.11 的矩阵一致；
3. 对 25 个代表（`ca-extensions` §7.7 列表 + `plan-mode`/`subagent`/`custom-provider-*`）断言关键行为。

`/tmp/loadprobe` 的 15 个用例中有 10 个跑通（含 `plan-mode`、`custom-provider-gitlab-duo`、`debug-provider`），
另外 5 个因 shim 缺失失败（`node:child_process`/`node:fs`/`node:os`/`@anthropic-ai/sdk`），
说明「加载 + 注册」链路可行；这些失败全部是 shim 缺失，不是引擎能力问题。

### 14.2 行为对齐测试

- **TUI 渲染字节**：给定宽度，对 `Text/Box/Container/Theme.fg` 的组合断言 ANSI 字节与 TS 版一致；
- **`Theme` token**：全部 token 的 `getFgAnsi` 对照；
- **对话框栈**：`select/confirm/input/custom` 的 `render(80)` 布局（`ca-extensions` §3.9 的 `→ `、
  `(esc to cancel)`、倒计时）；
- **错误落点**：handler 抛错 → `ExtensionError` 文案与三个 sink（`ca-extensions` §2.8.4）。

### 14.3 压力/边界

- 死循环 → 中断在 100 ms 内；
- OOM → `ExtensionError`，其他扩展不受影响；
- `reload` 期间旧 `ctx` 调用 → `Extension "<path>" failed to load and its API is no longer active`；
- 并发派发（多个工具并行）→ 每条派发在 JS 线程串行、微任务不丢；
- 1000 帧连续 `render` → 无 `Persistent` 泄漏（`rt.run_gc()` 后计数稳定）。

### 14.4 真实 npm 包回归（`/tmp/sbprobe` 的固定用例）

这三个用例的价值在于它们各自打中一个「看起来能跑其实跑不通」的坑，必须进 CI：

1. **多文件 CJS**：`import shellquote from "shell-quote"` → `shellquote.quote(["a b"])` 返回
   `'a\\ b'`。断言 `__kiana_cjs` 里有 `quote.js`/`parse.js` 两项。**只测 `ms` 会误判**（`ms` 自包含，
   朴素包装器就能过，但真实生态里多文件 CJS 才是常态）。
2. **`zod@3.25.76`**：`NetworkConfigSchema.safeParse({allowedDomains:'nope'})` → `success === false`
   且 `error.issues[0].code === "invalid_type"`；`getDefaultWritePaths()` 返回 10 项。
   （与 Node 26 实测逐字一致。）
3. **`@anthropic-ai/sdk@0.52.0` + SSE**：用 fake `fetch` 返回 6 个 SSE chunk，断言
   `messages.stream()` 的 `on("text")` 收到 `["hello"]`、`finalMessage().content[0].text === "hello"`、
   **且 `AbortController.abort()` 调用次数为 0**（后者能直接抓住「定时器立即触发」这个 bug）。
   同一用例在 Node 下跑作控制组，两者输出必须相同。

另加两条**反例断言**（防止实现退化）：`setTimeout(cb, 10)` 的回调不得在微任务阶段就执行；
`ReadableStream` 的 `pull` 中同步 `enqueue` 后 `read()` 必须能拿到值（不得挂起）。

---

## 15. 实施顺序（增量可验证）

1. **引擎骨架**（0.5 周）：`engine.rs` + `js_host.rs` + prelude；跑 `/tmp` 原型的等价用例
   （异步桥、定时器、AbortSignal、跨线程渲染）。
2. **TS 加载**（0.5 周）：`ts.rs` + `resolver.rs` + `loader_impl.rs`；跑 86 文件去类型 + declare。
3. **虚拟模块与 API**（1 周）：22 个虚拟模块 + `api.js` + 事务状态机；跑注册项矩阵。
4. **Node shim**（1.5 周）：`fs/path/child_process/os/url/util/zlib/readline/module` + 全局；其中
   **§5.6.1 的真 CJS `require`**（含 `__kiana_resolve`/`__kiana_read` 两个宿主函数）与 §9.4 的
   `ReadableStream`/`Headers`/`AbortController` 是必须一次做对的，跑 §14.4 的三个真实包用例。
5. **TUI 原生类**（1 周）：`Text/Box/Container/Markdown/...` + `Theme`；跑 `custom-footer`/`todo`/
   `overlay-test` 的渲染字节测试。
6. **Provider/流**（0.5 周）：`streamSimple` + `AssistantMessageEventStream`；跑 `custom-provider-gitlab-duo`，
   并接 §14.4 的 SDK SSE 用例。
7. **`node:net`/`node:http` + 真实代理**（0.5 周，仅当要 `sandbox/`）：tokio `TcpListener`/`TcpStream`
   → `Server`/`Socket`（`listen` 回调、`address()`、`connection`/`request`/`connect` 事件、`pipe`）；
   以 `sandbox-runtime` 的 `initialize()` 跑通为验收（当前实测停在此处，见 §16 Q2）。
8. **可选特性**（按需）：`wasmtime`（doom）、`ssh2` 纯 JS 路径（gondolin）。

---

## 16. 残余风险与待决问题

| # | 问题 | 影响 | 建议 |
|---|---|---|---|
| Q1 | `@anthropic-ai/sdk@0.52.0` 的真实网络行为（TLS、HTTP/2、重试、真实 SSE 分块边界）尚未在 QuickJS 下验证；本地 fake-fetch 已跑通协议层 | `custom-provider-anthropic` 的 A 级判定 | 语义层已无风险（`/tmp/sbprobe`：构造、`messages.create`、`messages.stream` 全通，与 Node 控制组逐字一致）。剩余是 `reqwest` 提供的 `fetch`/`ReadableStream` 实现质量：必须按块 flush SSE（不能缓冲到响应结束），且 `Response.headers.entries()` 可用。建议实施第 6 步接真 key 前先用 `kiana-ai/compat` 的 Rust 路径做对照 |
| Q2 | `sandbox/` 的**真实 TCP 代理**是必须项而非可选项：`node:net`/`node:http` 若只做桩，`initialize()` 会拿到假端口并把 `HTTP_PROXY` 指向不存在的服务 | `sandbox` 的 A 级判定 | 语法/模块兼容性已由 `/tmp/sbprobe` 排除（0 失败，`zod` 行为一致）；剩余工作是把 tokio `TcpListener` 接成 `net.Server`（含 `listen` 回调、`address()`、`connection`/`request`/`connect` 事件、`Socket.pipe`）。这是 1-2 天工作量，不是研究性风险 |
| Q3 | `kiana-tui` 原生类的 API 面是否覆盖 examples 全部用法 | 30 处 `ui.custom` 的组件 | 按 `/tmp/loadprobe` 的注册矩阵 + `overlay-qa-tests` 逐个补齐 |
| Q4 | `toLocaleString` 覆盖是否可接受 | 用户可见文本 | 建议覆盖并在 CHANGELOG 注明 |
| Q5 | `gondolin` 是否值得支持 | 1 个示例 | 默认 D；如需，要求外部 runner 与纯 JS `ssh2` |
| Q6 | 是否接受「kiana-tui 不是 JS 包」 | 扩展作者若 `import { Editor } from "@earendil-works/kiana-tui"` 并 subclass | 原生类 `extends` 已实测可行（`/tmp/classprobe`）；但 JS 侧无法访问 Rust 组件的内部字段（例如 `Editor.text` 若未导出） |

---

## 附录 A：本文用到的原型清单（均在 `/tmp`，不进入 `kiana`）

| 目录 | 验证内容 |
|---|---|
| `/tmp/rqprobe` | 模块缓存/循环导入/身份、Resolver/Loader 签名、`import.meta.url` |
| `/tmp/tdzprobe` | `Resolver` 的 `base` 语义、Loader 返回模块的导出可见性 |
| `/tmp/qprobe` | `?v=N` 唯一模块名实现 `moduleCache:false`（同名文件两次执行、相对依赖共享） |
| `/tmp/loop_probe` | 事件循环 + 定时器 + Promise settle + 同步渲染 + 重入 + 错误转换 |
| `/tmp/stream_probe` | 宿主推 chunk → JS `for await`、AbortSignal shim、timer shim |
| `/tmp/xthread` | 跨线程渲染延迟（27.0 µs 平均 / 279.6 µs 最坏） |
| `/tmp/classprobe` | 原生类 + JS 子类 + `super` + `instanceof` |
| `/tmp/cjsmod` | `ms@2.1.3` CJS 包装 |
| `/tmp/npmprobe` | npm 包安装与 CJS wrapper 的默认导入 |
| `/tmp/tsprobe2` | 86 文件去类型 + QuickJS 解析（46.8 ms） |
| `/tmp/featprobe` | QuickJS 内建能力全表（Intl/WebAssembly/TextEncoder/...） |
| `/tmp/loadprobe` | 10 个真实扩展的 strip/declare/eval/factory（另 5 个因 shim 缺失失败）+ 调用开销 |
| `/tmp/oxcsize`、`/tmp/combined` | 二进制体积（2.93 MB / 3.94 MB） |
| `/tmp/sdktest`、`/tmp/tbtest`、`/tmp/sbtest` | `@anthropic-ai/sdk`、`typebox`、`@anthropic-ai/sandbox-runtime` 的包形态与依赖 |
| `/tmp/expconst` | `const exports` 重新赋值的实测语义（QuickJS `'exports' is read-only` vs V8 `Assignment to constant variable.`），证明 CJS 必须走 IIFE 形参 |
| `/tmp/sbprobe` | **真实 npm 依赖图端到端**：664 模块 `sandbox-runtime`+`zod` 链接 75-83 ms / 0 语法失败；`zod` 校验与 Node 逐字一致；`@anthropic-ai/sdk` 43 模块（CJS 约 18 ms / ESM 约 12 ms 两入口），`messages.create` 与 **`messages.stream` SSE 全链路**（`content[0].text === "hello"`，与 Node 控制组一致）；真 CJS `require` 实现；`ReadableStream`/`Headers`/`AbortController`/timer 四类 shim 的坑 |

复现（示例）：

```bash
source ~/.cargo/env
cd /tmp/featprobe && cargo build --release && ./target/release/featprobe
cd /tmp/loadprobe && cargo build --release && ./target/release/loadprobe
cd /tmp/sbprobe  && cargo build --release
./target/release/sbprobe                                  # sandbox-runtime + zod 图
./target/release/sbprobe /tmp/sdktest/node_modules/@anthropic-ai/sdk/index.mjs   # SDK + SSE
```
