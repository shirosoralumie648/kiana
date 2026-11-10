# 扩展运行时设计：rquickjs + 原生绑定（native bindings）

本文件是 `ca-extensions.md` §8.3「扩展执行引擎」的一个落地方案：**用 rquickjs 内嵌 QuickJS-NG，
把 `kiana` 扩展 API 的每一组都实现为 Rust 原生绑定**，使 `packages/coding-agent/examples/extensions/**`
下的 TypeScript 扩展**一行不改**就能在纯 Rust 单二进制里运行。

- 参考实现（只读，**不得修改**）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/extensions/**`、
  `/home/ctyun/code/kiana_rev/kiana/packages/tui/**`
- 目标工作区：`/home/ctyun/code/kiana_rev/kiana`
- 事件语义、合并规则、错误文案、UI 对话框栈仍以 `ca-extensions.md` §2/§3/§8.4–§8.6 为准；TUI 组件本体以
  `tui-core.md` / `tui-components.md` 为准。本文只决定**引擎、线程与事件循环、TS/ESM 加载、模块解析、
  API 绑定形态、TUI 互操作、Node 兼容面、npm 支持、性能与兼容语料**。
- 与另两份设计的区别：`design-extension-runtime-js-shim-heavy.md` 把扩展 API 层整层搬进 JS（"语义留在 JS"）；
  `design-extension-runtime-risk-first.md` 以风险登记册驱动、JS 跑在独立线程。**本文选"原生绑定"路线**：
  API 层、组件类、Node 内建全部是 Rust 类型，JS 侧只保留一份**极小 prelude**（容器组件、Promise 管线、
  纯 JS 值如 `StringEnum`/`uuidv7`/`EventStream`）。

> 约定：文中所有用户可见字符串、错误文案、字段名按 TypeScript 源码**逐字**抄录，标 **【逐字】** 的字符串测试
> 会逐字节比对。所有性能数字来自本轮 `/tmp` 原型实测（release、同机 Linux x86-64、rustc 1.99.1），
> 复现命令见附录 A。原型一律在 `/tmp`，不进入 `kiana`。

---

## 0. 结论摘要

| 决策 | 结论 | 依据 |
|---|---|---|
| 引擎 | **rquickjs 0.14.0**（`rquickjs-sys 0.14.0` 内置 **QuickJS-NG 0.16.2**，`quickjs.h`：`QJS_VERSION_MAJOR 0 / MINOR 16 / PATCH 2`） | 单静态二进制；原生 ESM `Module`/`Resolver`/`Loader`/`import.meta`；可设内存、栈、中断；原生类可被 JS `extends` |
| 特性裁剪 | **只用 `std` + `loader` + `macro`**；**不用 `full-async`、不用 `parallel`** | 同步 `Runtime` + 单线程事件循环即可；`AsyncRuntime` 的 `async_with` 在锁语义上有坑（§2.8 实测），`parallel` 会引入 tokio 多线程并使 `Ctx` 跨线程 |
| 线程模型 | **TUI、扩展 JS、tokio I/O 全在主线程**：`tokio current_thread` + `LocalSet`；JS 只经唯一入口 `with_js()` 进入；宿主异步完成以"队列 + 唤醒"回到主循环 | 无跨线程同步桥（省 16.5 µs/帧）；无锁重入死锁面；与 `tui-core.md` §8.2 的单线程所有权一致 |
| 异步协议 | **手写 Promise**：JS 侧 `new Promise` 保存 resolve/reject；Rust 侧只认 `(id, ok, value)` 三元组 | 同步 `Runtime` 下 `function::Async` 不可用；手写协议让取消（AbortSignal）、超时、背压全部可控 |
| TS 去类型 | **oxc 0.155.0**（`oxc_parser`/`oxc_semantic`/`oxc_transformer`/`oxc_codegen`/`oxc_allocator`/`oxc_span`） | 86 个 `.ts` / 452 KB（441 KiB，不含 `doom-overlay/doom/build/doom.js`）全量去类型 **140 µs/文件**（86 文件 12.1 ms 总计，`stripbench.rs` 5 轮取最优），0 诊断；`enum`/`namespace`/参数属性/`satisfies`/`as const`/`using` 均正确处理 |
| 模块解析 | 自研 `Resolver`（虚拟模块优先表 + 相对路径 + `oxc_resolver 11.24.3` 处理 `node_modules`/`exports`）+ 自研 `Loader`（fs + oxc + `import.meta` 注入） | 22 个虚拟模块逐字对齐 `virtual-modules.ts`；`exports` 条件与 jiti 一致 |
| CJS interop | 真 `require`：宿主取源码 → JS 侧 `new Function` 求值 → **先登记 `module.exports` 再执行**（支持循环） | `/tmp/ext-proto/src/bin/cjs4.rs`：`ms@2.1.3` 与双文件循环 CJS 均通过 |
| API 绑定 | **每个 API 组一个 Rust 对象/类**：`kiana`（`Object` + 31 方法 + 41 事件 + `events`）、`ExtensionContext`（原生类）、`ExtensionUIContext`（原生类）、`ToolDefinition`（JS 对象，Rust 侧按键读取）、`Theme`（原生类） | 方法调用 87 ns、属性 get/set 45/36 ns，逐事件派发开销可忽略 |
| 组件互操作 | **`Component`/`Editor`/`Text`/`Box`/`SelectList`/`Markdown` 等全部是 Rust 原生类**，JS 可 `extends`；Rust 调用 JS 覆写方法时**必须** `f.call((This(obj), ...))` | `/tmp/ext-proto/src/bin/thisclass.rs`：`super.render()`、`super.handleInput()`、`this.getText()` 全部通过；1.34 µs/帧 |
| Node 内建 | **Rust 原生模块表**（`node:fs`/`fs/promises`/`path`/`os`/`child_process`/`readline`/`util`/`url`/`zlib`/`module`）+ 少量 JS 包装（`Buffer`、`EventEmitter` 语义） | 语料实际只用到 10 个内建、约 40 个符号（§6） |
| npm 包 | 运行时从扩展自己的 `node_modules` 解析；typebox/marked 通过**依赖域（dependency realm）**只求值一次 | 每 Context 重跑 typebox+marked = **41.2 ms**；依赖域后每扩展固定附加 **0.85–1.07 ms**（再加 strip 0.14 ms + declare/eval 0.25 ms 均值） |
| 二进制增量 | rquickjs 最小特性组合 **1.58 MB**（1,578,480 字节，stripped）；加 oxc 全套（同步 `Runtime`）**3.09 MB**（3,085,248 字节，stripped）；再用 `AsyncRuntime` 的原型是 3.08 MB | `strip -s` 实测（十进制 MB，见 §9.4） |
| 兼容语料 | **70 个单文件全部原样可用**（A 类 53 个只用 API；B 类 17 个靠 §6 的 Node shim，已逐一原型验证）；**9 个目录中 6 个原样可用**，2 个需额外能力（`sandbox` 需真实 TCP，`doom-overlay` 的 WASM 本体需 `feature = "webassembly"`），1 个不可能（`gondolin`，原生 addon/外部 runner） | §11 |

一句话：**引擎不是瓶颈，兼容面才是**。真正的硬缺口只有三处：`Intl`（`Segmenter`/`toLocaleString`）、
`WebAssembly`（`doom-overlay` 的 WASM 本体）、原生 `.node` addon（`gondolin`）。其余全部可以用原生绑定
或 JS shim 补齐，并且都已在原型中验证。

---

## 1. 引擎与精确依赖

### 1.1 为什么是 rquickjs（且为什么**不**用 `full-async`）

| 候选 | 版本 | 结论 |
|---|---|---|
| **rquickjs** | 0.14.0 | 选中。`rquickjs-sys` 内置 QuickJS-NG 0.16.2 的 C 源，随 crate 编译；`Module`/`Resolver`/`Loader`/`import.meta`/循环导入/顶层 `await` 全都原生；`#[rquickjs::class]` + `#[qjs(get)]`/`#[qjs(constructor)]` 可让 JS `extends` 原生类 |
| boa_engine | 0.22.0 | 纯 Rust 但 ESM 加载器与 `import.meta` 需自建，且无法直接复用 kiana-tui 的 JS 语义 |
| deno_core + v8 | 0.413.0 | 单二进制但体积与构建复杂度不可接受 |
| quickjs-wasi | npm `3.6.2` | 无 `node:fs`/`child_process`，扩展要真 I/O；**保留给 codemode**，与本单元无关 |

**关键取舍：只用同步 `Runtime`，不用 `AsyncRuntime`。** 本轮原型给出两条硬证据：

1. `AsyncRuntime::async_with(|ctx| async { ... })` 的锁行为**不可作为设计基础**：
   - 闭包内**嵌套** `async_with`/`with` 会**永久阻塞**（实测 200 ms 超时，`/tmp/ext-proto/src/bin/reenter2.rs`）；
   - 但闭包 `await` 一个 tokio sleep 时锁**确实会释放**（同一文件里另一用例 28.9 µs 拿到锁）。
   这两种行为组合起来意味着"能否重入"取决于"当前是否停在 await 点"，对需要在任意时刻渲染帧的
   TUI 来说是不可推理的契约。
2. `function::Async` 在同步 `Runtime` 下不可用；而在 `AsyncRuntime` 下它会占用 runtime 的 opaque
   scheduler，一旦某个 handler 内部 `await` 长任务，其它 JS 工作只能通过同一 scheduler 驱动，
   帧的实时性完全依赖泵的调度细节（`/tmp/ext-proto/src/bin/asynclock.rs` 实测 `done flag: false`，
   即 `Async` 宿主函数不会被普通 `execute_pending_job` 泵推进）。

**同步 `Runtime` + 手写 Promise + 单一事件循环**则全部可控，且实测把每帧渲染压到 **1.34 µs**、
跨线程方案要 **16.5 µs**（§2.8）。

### 1.2 Cargo 依赖（版本已用 `cargo search` / `cargo info` 逐一核对存在）

```toml
# crates/kiana-extensions/Cargo.toml（节选）
[dependencies]
# ---- JS 引擎（唯一必选，特性刻意收窄）----
rquickjs = { version = "=0.14.0", default-features = false, features = ["std", "loader", "macro"] }
#  std    : 必需（Rc/RefCell 型 Ctx）
#  loader : Module::declare / Resolver / Loader / import.meta
#  macro  : #[rquickjs::class] / #[methods] / #[qjs(...)]
#  刻意不启用：full-async（引入 async-lock + AsyncRuntime）、parallel（unsafe impl Send/Sync + tokio/rt-multi-thread）
#  注：`classes` 在 0.14.0 是空特性（rquickjs-core 里 `classes = []`），宏不再依赖它；写上无害但没必要

# ---- TS 去类型（运行时逐文件，无 bundle）----
oxc_allocator   = "=0.155.0"
oxc_ast         = "=0.155.0"
oxc_parser      = "=0.155.0"
oxc_semantic    = "=0.155.0"
oxc_transformer = "=0.155.0"
oxc_codegen     = "=0.155.0"
oxc_span        = "=0.155.0"

# ---- npm 解析 ----
oxc_resolver = "=11.24.3"        # exports/main/imports 条件、node_modules 逐级

# ---- 宿主 / 异步 ----
tokio       = { version = "=1.54.1", features = ["rt", "macros", "sync", "time", "process", "io-util", "fs", "net", "signal"] }
tokio-util  = { version = "=0.7.20", features = ["rt"] }      # CancellationToken，AbortSignal 桥
futures     = "=0.3.34"
parking_lot = "=0.13.0"
crossbeam-channel = "=0.5.17"    # 仅用于"宿主线程 -> 主循环"的任务投递（可选，tokio mpsc 亦可）

# ---- 数据 ----
serde      = { version = "=1.0.229", features = ["derive"] }
serde_json = { version = "=1.0.154", features = ["preserve_order"] }
indexmap   = "=2.14.2"

# ---- Node 内建实现 ----
reqwest      = { version = "=0.13.5", default-features = false, features = ["rustls", "stream", "json"] }
# 0.13 起特性改名：不再有 `rustls-tls`，改为 `rustls`（= __rustls-aws-lc-rs + rustls-platform-verifier）
# 或 `rustls-no-provider` + 自行安装 CryptoProvider；`stream` 提供 Response::bytes_stream()
url          = "=2.5.8"          # URL / URLSearchParams / fileURLToPath
percent-encoding = "=2.3.2"
base64       = "=0.23.1"
sha2         = "=0.11.1"         # crypto.subtle.digest
getrandom    = "=0.4.3"          # crypto.getRandomValues
uuid         = { version = "=1.29.0", features = ["v4", "v7"] }
flate2       = "=1.1.10"         # node:zlib gunzip/gzip
notify       = "=8.2.1"          # fs.watch（9.0.0-rc.6 是预发布，不用）
unicode-width        = "=0.2.2"
unicode-segmentation = "=1.13.3" # Intl.Segmenter shim（见 §5.5）

# ---- 工具 ----
thiserror = "=2.0.21"
once_cell = "=1.21.4"
regex     = "=1.13.1"
dirs      = "=7.0.0"
chrono    = "=0.4.45"            # Date#toLocaleString 的 en-US 复刻（§5.5）
tracing   = "=0.1.44"

# ---- 可选 ----
wasmtime = { version = "=50.0.0", optional = true }   # feature = "webassembly"（doom-overlay 本体）
```

MSRV：`rquickjs 0.14.0` 要求 **1.87**，`oxc_* 0.155.0` 与 `wasmtime 50.0.0` 均要求 **1.97.0**，`parking_lot 0.13.0` 要求 1.95 ⇒ `kiana` 工作区 MSRV 定 **1.97**（与 `ca-extensions.md` §8.1 一致）。
其余依赖的 MSRV 都低于 1.97（`oxc_resolver 11.24.3` = 1.95、`uuid 1.29.0` = 1.89、`sha2 0.11.1` = 1.88、`tokio 1.54.1`/`reqwest 0.13.5`/`notify 8.2.1` = 1.85、`serde_json` = 1.71、`serde` = 1.56）。

`icu_segmenter 2.4.0`（crates.io 存在）**不引入**：它带来 CJK 词典数据（数 MB），而 kiana-tui 只需要
UAX#29 字素/词边界；`unicode-segmentation` + 少量手工规则足够，差值用测试锁死（§5.5）。

### 1.3 crate 划分

沿用 `ca-extensions.md` §8.1 的 `kiana-extensions`，新增 `js/` 资源目录（prelude）：

```
crates/kiana-extensions/
├── src/
│   ├── lib.rs
│   ├── engine/
│   │   ├── mod.rs            # JsEngine：Runtime + Context + prelude + with_js()
│   │   ├── loop_.rs          # ExtLoop：主循环、任务队列、job 泵、定时器堆
│   │   ├── promise.rs        # 手写 Promise 协议（Pending/Resolved/Rejected）
│   │   ├── limits.rs         # 内存/栈/中断/超时
│   │   └── shutdown.rs       # 逆序销毁：Persistent -> Context -> Runtime
│   ├── loader/
│   │   ├── resolver.rs       # 虚拟模块表 + 相对路径 + oxc_resolver + CJS 探测
│   │   ├── loader.rs         # fs 读取 + oxc strip + import.meta 注入 + 缓存
│   │   ├── transpile.rs      # oxc 封装（§3.2）
│   │   ├── cjs.rs            # 真 require：宿主源码 + JS 侧 new Function + 预登记
│   │   └── realm.rs          # 依赖域（typebox / marked / kiana-tui-js 一次求值）
│   ├── api/
│   │   ├── kiana_object.rs      # kiana 对象：31 方法 + 41 事件 + events
│   │   ├── events.rs         # 41 个事件的 Rust 类型 + 派发入口
│   │   ├── context.rs        # ExtensionContext / ToolContext / CommandContext 原生类
│   │   ├── ui.rs             # ExtensionUIContext 原生类（28 成员 = 27 方法 + theme）
│   │   ├── tools.rs          # ToolDefinition 读取、schema、renderer
│   │   ├── provider.rs       # registerProvider / MCP / virtual model
│   │   └── state.rs          # 事务性三态（Loading/Active/Failed）+ pendingRuntimeChanges
│   ├── ui/
│   │   ├── component.rs      # 原生 Component 类群（§5）
│   │   ├── theme.rs          # Theme 原生类（23 个 token + 方法）
│   │   ├── editor.rs         # Editor 原生基类 + CustomEditor
│   │   └── adapter.rs        # 原生组件 <-> tui_core::Component 适配
│   ├── node/
│   │   ├── fs.rs path.rs os.rs child_process.rs readline.rs util.rs url.rs zlib.rs module_.rs
│   │   └── buffer.rs         # Buffer 门面（Uint8Array + 静态方法）
│   └── js/
│       ├── prelude.js        # 嵌入资源（含 minify 后约 32 KB）
│       └── prelude.bin       # 可选：Module::write 生成的字节码（§3.7）
└── assets/
    └── extension-runtime/    # prelude 的 TS 源码（构建期用 oxc 转成 prelude.js）
        ├── containers.ts     # Container/VStack/HStack/Spacer 的 JS 实现
        ├── event-stream.ts   # AssistantMessageEventStream / EventStream（纯 JS）
        ├── typebox-helpers.ts# StringEnum（原样搬运）
        ├── uuid.ts           # uuidv7（原样搬运）
        ├── buffer.ts         # Buffer 的 JS 门面
        ├── node-events.ts    # EventEmitter
        └── shims.ts          # TextEncoder/TextDecoder/URL/console/timers 等
```

**prelude 只有 ~32 KB、约 45 个导出**（对比 js-shim-heavy 的 407 KB）：因为 API 层与组件层都在 Rust 里，
JS 侧只需要"原生类装不起来的东西"——容器组件（组合逻辑）、纯 JS 值类型、Node 门面。

---

## 2. 线程模型与事件循环

### 2.1 一张图

```text
┌──────────────────────────── 主线程：tokio current_thread + LocalSet ────────────────────────────┐
│                                                                                              │
│  ExtLoop::run()                                                                              │
│   loop {                                                                                     │
│     ┌─ 1) 处理宿主完成事件（exec 子进程退出、fetch 响应、fs 回调、timer 到期）──────────────┐   │
│     │   队列：VecDeque<HostDone>  （同线程，无锁；跨线程投递用 mpsc + waker）              │   │
│     └───────────────────────────────────────────────────────────────────────────────────────┘   │
│     ┌─ 2) 进入 JS：with_js(|ctx| { 泵微任务; 派发 handler; 应用 resolve }) ─────────────────┐   │
│     │   Context::with(...) —— 全程同步；绝不跨 await                                         │   │
│     └───────────────────────────────────────────────────────────────────────────────────────┘   │
│     ┌─ 3) TUI 渲染：tui_core::TuiBase::render() ────────────────────────────────────────────┐   │
│     │   逐组件调用；原生组件走 Rust；JS 组件经 with_js 调 render(width)（1.34 µs/帧）        │   │
│     └───────────────────────────────────────────────────────────────────────────────────────┘   │
│     ┌─ 4) 读取终端输入（crossterm 事件）→ 分发给焦点组件 → 可能触发扩展 handler ────────────┐   │
│     └───────────────────────────────────────────────────────────────────────────────────────┘   │
│     ┌─ 5) 空闲时 tokio::task::yield_now().await，让 I/O 驱动推进 ──────────────────────────┐   │
│     └───────────────────────────────────────────────────────────────────────────────────────┘   │
│   }                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
                    ▲                                                    │
                    │ 同一个线程（无跨线程！）                            │ tokio::spawn（I/O 任务）
                    │                                                    ▼
        宿主能力：kiana.exec / fetch / fs / child_process / timers  ──> 完成后 post(HostDone)
```

**为什么不用独立 JS 线程**（`design-extension-runtime-risk-first.md` 的选择）：

| 指标 | 独立线程 + 同步通道 | 同线程（本文） |
|---|---|---|
| 每帧渲染 JS 组件 | **16.5 µs** 均 / 57 µs 最坏（`/tmp/ext-proto/src/bin/xthread2.rs`，600 帧） | **1.34 µs**（`/tmp/ext-proto/src/bin/bridge.rs`，1000 帧） |
| 每次 JS→宿主回调 | 又一次通道往返（~16 µs） | 87 ns 直接调用 |
| 需要 `Persistent` 跨线程搬运 | 是（`restore` 需要 `Ctx`，只能在 JS 线程） | 否，同线程直接持有 |
| 死锁面 | 双向同步通道 + 递归回调需仔细设计 | 单线程，无可重入锁问题 |
| 缺点 | — | 长同步 JS（死循环）会卡住 TUI ⇒ 必须靠**中断看门狗**（§2.7） |

语料里没有任何扩展做 CPU 密集计算（最大的是 `space-invaders` 的逐帧状态机，1.34 µs 量级），
所以"同线程"是净收益。

### 2.2 唯一入口 `with_js`

```rust
pub struct JsEngine {
    rt: rquickjs::Runtime,
    ctx: rquickjs::Context,
    // 所有需要跨帧存活的 JS 值都在这里，Drop 顺序见 §2.9
    pending: HashMap<u64, PendingPromise>,
    handles: SlotMap<JsHandleId, Persistent<Object<'static>>>,
    interrupted: Arc<AtomicBool>,
    deadline: Arc<AtomicU64>,
}

impl JsEngine {
    /// 唯一允许进入 JS 的函数。**同步**，绝不持有到 await 之后。
    pub fn with_js<R>(&self, f: impl for<'js> FnOnce(Ctx<'js>) -> R) -> R {
        self.ctx.with(f)
    }
}
```

硬规则（来自实测）：

1. **不得在 `with_js` 内嵌套 `with_js`**。`Context::with` 内部是 `RefCell`，嵌套会
   `panicked at safe_ref.rs:35: RefCell already borrowed`（`/tmp/ext-proto/src/bin/nestwith2.rs` 用例 A）。
   唯一的合法重入是**把 `Ctx` 作为参数传下去的宿主函数**：宿主函数签名里带 `Ctx<'js>` 时，
   可以在函数体内 `ctx.eval(...)`、`ctx.execute_pending_job()`（实测 `value=601` 正确）。
   ⇒ 设计约束：所有会被 JS 调用的宿主函数，若需要再进 JS，**必须**接收 `Ctx` 参数而不是回查 `JsEngine`。
2. **任何宿主异步都不得阻塞 `with_js`**。宿主函数若要等 I/O，返回一个 Promise 并立刻返回
   （§2.3）；真正的等待发生在主循环，不在 `with_js` 内。
3. `with_js` 里只做"短事务"：泵一次 job 队列、派发一个 handler、应用一个 resolve、渲染一帧。
   **单次调用目标 < 100 µs**（实测 1.34 µs 帧、162 µs 处理 20 KB 消息的 JSON 往返）。

### 2.3 手写 Promise 协议（核心）

同步 `Runtime` 下没有 `rquickjs::function::Async`，所以异步全部走"JS 侧建 Promise，Rust 侧存
resolve/reject"：

```js
// prelude.js —— 全部宿主异步的统一入口
globalThis.__kiana = {
  seq: 0,
  waiters: new Map(),          // id -> { resolve, reject }
  hostStart: null,             // Rust 填入：(id, kind, payload) => void
  promise(kind, payload) {
    const id = ++this.seq;
    const p = new Promise((resolve, reject) => this.waiters.set(id, { resolve, reject }));
    this.hostStart(id, kind, payload);
    return p;
  },
  settle(id, ok, value) {
    const w = this.waiters.get(id);
    if (!w) return false;
    this.waiters.delete(id);
    if (ok) w.resolve(value); else w.reject(value);
    return true;
  },
};
```

Rust 侧：

```rust
// 宿主函数（带 Ctx，可直接抛 JS 异常）。硬规则：JS 值（Value<'js>）**绝不能**被搬进
// spawn_local 的 future —— 它带生命周期且非 'static。必须在同步阶段就解析成 Rust 拥有类型。
fn host_start<'js>(ctx: Ctx<'js>, id: u64, kind: String, payload: Value<'js>) -> Result<()> {
    match kind.as_str() {
        "exec" => {
            let (cmd, args, opts) = parse_exec(&ctx, &payload)?;   // 同步解析为 String/Vec<Option>
            let tx = HOST_TX.with(|t| t.clone());                  // 同线程 mpsc，无锁
            tokio::task::spawn_local(async move {
                let r = run_exec(cmd, args, opts).await;           // 全部是 Rust 拥有类型
                let _ = tx.send(HostDone::Settle { id, ok: r.is_ok(), value: r });
            });
        }
        "fetch" => { /* 同上：先把 url/headers/body 解析成 Rust 类型，再 spawn_local;
                       响应块经 HostDone::StreamChunk 回到主循环 */ }
        "fs"    => { /* 同步 fs 直接在宿主函数里做完并返回；异步的走 tokio::fs */ }
        // 定时器不走 hostStart：prelude 直接调专用的 __kiana.timerStart / timerCancel（§2.5）
        _ => return Err(Exception::throw_message(&ctx, &format!("unknown host kind: {kind}"))),
    }
    Ok(())
}
```

主循环应用完成事件：

```rust
fn apply(&mut self, done: HostDone) -> rquickjs::Result<()> {
    self.with_js(|ctx| -> rquickjs::Result<()> {
        match done {
            HostDone::Settle { id, ok, value } => {
                let api: Object = ctx.globals().get("__kiana")?;
                let settle: Function = api.get("settle")?;
                let _: bool = settle.call((id, ok, to_js(&ctx, value)?))?;   // Rust -> JS 在闭包内完成
            }
            HostDone::StreamChunk { id, chunk } => { /* 调 JS 侧 push 函数 */ }
        }
        while ctx.execute_pending_job() {}   // 立刻把 then/catch 链推进完
        Ok(())
    })
}
```

为什么这个协议足够：

- **实测跑通顶层 `await`**：`/tmp/ext-proto/src/bin/synctla.rs` 里一个含两处 `await` 的 ESM 模块
  在同步 `Runtime` 下 2 轮就 `Resolved`，`a=v1,b=v2` 正确。
- **实测跑通"扩展 await 期间照常渲染"**：`/tmp/ext-proto/src/bin/syncloop.rs` 里 handler 挂起 43 ms，
  期间每 21 ms 渲染一帧，帧内容随 JS 状态变化（`ticks=0 → 10`）。
- **实测跑通"JS → 宿主 → JS"递归**：`/tmp/ext-proto/src/bin/reentrant3.rs` 深度 4 的递归调用全部正确。

### 2.4 job 泵

```rust
fn pump(ctx: &Ctx<'_>) -> usize {
    let mut n = 0;
    while ctx.execute_pending_job() { n += 1; }   // Ctx::execute_pending_job() -> bool
    n
}
```

- `Ctx::execute_pending_job` 的 **job 抛错会被 rquickjs 吞掉**（`ctx.catch()` 内部清理），
  实测 `Promise.resolve().then(() => { throw new Error('boom') })` 泵完后无 panic、无返回错误。
  ⇒ **未处理拒绝必须由 `set_host_promise_rejection_tracker` 捕获**（实测回调被调用，
  `rejection tracked: true`），并映射为扩展错误（`ca-extensions.md` §3.8 的三种落点）。
- 每个"事务"结束时泵一次，**不要**在长循环里反复泵（每次 `execute_pending_job` 都有
  ~0.4 µs 的固定开销；`ctx.eval("1+1")` 实测 1.80 µs 说明单次进入 JS 不便宜）。

### 2.5 定时器

```rust
struct TimerHeap { next_id: u64, heap: BinaryHeap<Reverse<(Instant, u64)>>, cancelled: HashSet<u64> }
```

- `setTimeout(fn, ms, ...args)` 的语义是"到点**调用回调**、返回 id"，**不是**返回 Promise。
  prelude 里保存 `fn` 到 `timers: Map<id, {fn, args, repeat, ms}>`，然后调用宿主
  `__kiana.timerStart(id, ms, repeat)` 只把 `(id, 到期时刻)` 放进 Rust 的堆；
  到期时主循环回调 `__kiana.timerFire(id)`，由 JS 侧查 Map 调 `fn(...args)`：
  ```js
  let timerSeq = 0; const timers = new Map();
  globalThis.setTimeout = (fn, ms = 0, ...args) => {
    const id = ++timerSeq;
    timers.set(id, { fn, args, repeat: false, ms });
    __kiana.timerStart(id, Math.max(0, ms | 0), false);
    return id;
  };
  globalThis.setInterval = (fn, ms = 0, ...args) => {
    const id = ++timerSeq;
    timers.set(id, { fn, args, repeat: true, ms });
    __kiana.timerStart(id, Math.max(1, ms | 0), true);
    return id;
  };
  globalThis.clearTimeout = globalThis.clearInterval = (id) => { timers.delete(id); __kiana.timerCancel(id); };
  globalThis.__kiana.timerFire = (id) => {
    const t = timers.get(id); if (!t) return;
    if (!t.repeat) timers.delete(id);
    try { t.fn(...t.args); } catch (e) { console.error(e); }
    // repeat 的下一次入堆由 Rust 在 fire 之后自行完成（避免 JS 侧再调一次宿主）
  };
  ```
  这样 `setTimeout` 的**返回值是数字 id**。Node 返回的是 `Timeout` 对象，但语料里只用
  `clearTimeout(id)` 且从不保存返回值（`timed-confirm.ts` 写的是 `setTimeout(() => controller.abort(), 5000)`），
  所以数字 id 足够。若将来要复刻 `Timeout.unref()`，返回一个带 `valueOf()` 的包装对象、
  prelude 内部统一折算成数字即可，不影响 `clearTimeout`。
- **`setTimeout` 必须尊重 delay**：`design-extension-runtime-risk-first.md` 记录过一次真实事故——
  SDK 的 `fetchWithTimeout` 用 `setTimeout(() => controller.abort(), ms)`，若 shim 立即触发，
  每个请求都会被自己 abort 成 `APIUserAbortError`。本设计里定时器由主循环的堆驱动，
  与 `tokio::time::Instant` 对齐，delay 语义天然正确。
- 定时器堆与 `tui-core.md` §8.2 的 `Timer` 抽象共用同一个 `Instant` 时间轴：主循环每轮取
  `heap.peek()` 与 `now` 比较，到期就 `__kiana.timerFire(id)`；空闲等待时用
  `tokio::time::sleep_until(next_deadline)` 而不是忙轮询。
- `queueMicrotask`：QuickJS **原生提供**（实测 `typeof queueMicrotask === 'function'`）。
- `setImmediate`/`process.nextTick`：prelude 里映射到 `queueMicrotask`（Node 语义上 nextTick 优先于
  microtask，但扩展不使用这一差异；`tui-core.md` §2.9.10 已决定 TUI 侧用 Rust 队列）。

### 2.6 AbortSignal / AbortController

- `AbortController`/`AbortSignal` 是 **prelude 里的纯 JS 类**（约 60 行）：`signal.aborted`、
  `signal.reason`、`addEventListener("abort")`、`throwIfAborted()`、静态 `AbortSignal.abort()`/`.timeout()`。
- 宿主侧（`kiana.exec`、`fetch`、`ctx.signal`）通过 `signal.addEventListener("abort", ...)` 注册；
  Rust 侧只需在取消时调用 `AbortController.abort(reason)`（用 `Persistent<Function>` 持有）。
- `kiana.exec` 的 `signal` 与 `timeout` 都会映射到 `tokio::process::Child::kill()` +
  `tokio_util::CancellationToken`（`ExecOptions { signal?, timeout?, cwd? }`，
  `ExecResult { stdout, stderr, code, killed }`，见 `core/exec.ts`）。
- `ctx.signal` 是"当前 abort signal 或 undefined"（`ExtensionContext.signal`）；
  在 handler 里读它，宿主在 agent 中止时 abort。

### 2.7 限制与看门狗（实测可用）

```rust
rt.set_memory_limit(512 * 1024 * 1024);          // 默认 512 MB
rt.set_max_stack_size(4 * 1024 * 1024);          // 默认 4 MB
rt.set_interrupt_handler(Some(Box::new(move || {
    interrupted.load(Relaxed) || (deadline.load(Relaxed) != 0 && now_ms() > deadline.load(Relaxed))
})));
```

| 场景 | 实测结果 |
|---|---|
| 内存耗尽 | `Error: out of memory`（可被 JS `catch`） |
| `for(;;){}` | `Error: interrupted`，看门狗在 **100.9 ms** 触发（阈值 100 ms） |
| 无限递归 | `Error: Maximum call stack size exceeded` + 4 层栈帧 |
| 未处理拒绝 | `set_host_promise_rejection_tracker` 回调被调用（4 参数：`Ctx, Promise, Value, bool`） |

规则：
- **handler 派发**（`tool_call` 等可能长时间运行的）设置 30 s deadline；`factory` 加载设置 10 s；
  正常渲染不设 deadline。
- 中断触发后 `with_js` 返回 `Error::Exception`，映射为扩展错误：
  交互式为 `Extension "<path>" error: <error>`，print/json 为 `Extension error (<path>): <error>`（【逐字】，§3.8）。
- 中断处理器**不能**调用 JS（会在任意指令边界重入引擎）——只能读原子变量。

### 2.8 为什么不用 `AsyncRuntime`（实测证据汇总）

| 实验 | 结果 | 文件 |
|---|---|---|
| `async_with` 内立即嵌套 `with()` | 超时 200 ms（死锁） | `reenter2.rs` |
| `async_with` 内 `yield_now()` 后嵌套 `with()` | 超时 200 ms | `lockacross.rs` |
| `async_with` 内 `sleep(1ms)` 后嵌套 `with()` | 超时 200 ms | `lockacross.rs` |
| `async_with` 内 `sleep(500ms)`，另一 task 的 `with()` | **成功**，28.9 µs | `lockhold.rs` |
| `Async` 宿主函数 sleep 300 ms，`with()` 渲染 | 成功 2.5 µs，但 `done` 永不完成 | `asynclock.rs` |

结论：`AsyncRuntime` 的锁在"await 一个外部 future"时释放、在"await 一个 JS promise"时保持，
**契约不可推理**；而"锁保持期间任何渲染都会阻塞"对 TUI 是不可接受的。同步 `Runtime` 没有这个问题：
`with` 是纯粹的短临界区，任何时刻都能进。

### 2.9 关闭顺序（会导致进程 abort 的硬约束）

```rust
impl Drop for JsEngine {
    fn drop(&mut self) {
        // 1. 先清所有 Persistent（waiters / handles / 事件回调 / theme 回调）
        self.pending.clear();
        self.handles.clear();
        self.registry.iter_mut().for_each(|r| r.clear_persistents());
        // 2. 再跑一次 GC，把 JS 侧引用链断开
        self.with_js(|ctx| { while ctx.execute_pending_job() {} });
        // 3. 然后 Context、最后 Runtime（字段声明顺序：rt 在前，ctx 在后 ⇒ Drop 逆序 = ctx 先）
        rquickjs::Runtime::run_gc(&self.rt);
    }
}
```

实测：任何一个 `Persistent` 活过 `Runtime` ⇒
`JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed` 并 **abort 进程**
（`/tmp/ext-proto/src/bin/reentrant3.rs` 未清理时复现，清理后 `exit=0`）。

同理，`Module::write` 生成的字节码在**没有静态导入可解析的 runtime** 里会失败
（`could not load module 'dep'`），所以字节码只能在"loader 已就绪且依赖可解析"的 runtime 里生成（§3.7）。

### 2.10 与 `tui-core` 的接线

`tui-core.md` §8.2 规定 `TuiBase` 持有 `Vec<Box<dyn Component>>`、`Component` 不需要 `Send`
（全部在 UI 线程）。本设计把扩展组件实现为 **`tui_ext::Component` 的另一种实现**：

```rust
// crates/kiana-extensions/src/ui/adapter.rs
pub struct JsComponent {
    handle: JsHandleId,             // Persistent<Object> 的槽位
    engine: Weak<JsEngine>,         // 只用于 with_js；同线程，不需要 Arc<Mutex>
    last_lines: Vec<String>,        // 渲染失败时的缓存（§5.6）
    focused: bool,
}

impl tui_core::Component for JsComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let engine = self.engine.upgrade().expect("engine alive");
        engine.with_js(|ctx| {
            let obj = self.handle.restore(&ctx)?;
            let f: Function = obj.get("render")?;
            f.call((This(obj.clone()), width))     // This 必须传，见 §5.2
        }).unwrap_or_else(|e| { self.report(e); self.last_lines.clone() })
    }
    fn handle_input(&mut self, data: &str) { /* 同上，调 handleInput */ }
    fn invalidate(&mut self) { /* 调 invalidate()，非必选 */ }
    fn wants_key_release(&self) -> bool { /* 读 wantsKeyRelease */ }
}
```

因为 `JsComponent` 与 `TuiBase` 在同一线程、同一所有权域，**没有任何 `Arc<Mutex>`、没有通道、
没有 `Send` 约束**，这也是 1.34 µs/帧的来源。

---

## 3. TS / ESM 加载

### 3.1 流水线

```
扩展入口 (.ts/.js)
   │
   ├─ [1] Resolver::resolve(base, specifier)          → 规范化 URL / 虚拟模块名
   │      虚拟模块表（22 键，§3.4） > 相对/绝对路径 > node_modules（oxc_resolver）
   │
   ├─ [2] Loader::load(ctx, name)                      → Module<Declared>
   │      a) 虚拟模块 → prelude 里已构造的 namespace 的 re-export shim
   │      b) .ts/.mts/.cts/.js/.mjs → 读文件
   │           · .ts ⇒ oxc strip（§3.2），输出 ESM
   │           · .cjs / package.json "type":"commonjs" ⇒ CJS 包装（§3.5）
   │      c) Module::declare(ctx, name, source)
   │      d) m.meta()?.set("url"/"dirname"/"filename", ...)   ← 必须在 eval 之前
   │
   └─ [3] Module::import / m.eval()                     → (Module<Evaluated>, Promise)
          · 顶层 await：Promise 由主循环手写协议推进
          · 循环导入：QuickJS 原生支持
          · 求值后 ns.get("default") → factory
```

实测要点：

- `Module::declare` + `m.meta().set(...)` + `Module::import` 的组合完全可行，
  `import.meta.url`/`dirname`/`filename` 在模块体里读得到（`/tmp/ext-proto/src/bin/tlalex.rs`：
  `meta.url=ext:/a.ts, dirname=/`）。
- **相对路径必须自己归一化**。`Resolver` 收到的 `base` 是模块名（可以是自定义 scheme），
  朴素拼接会造出 `ext:///ext:////./b.ts` 这种怪物并报
  `Could not find export 'v' in module ...`。正确做法是 split('/') + 处理 `.`/`..`/空段（`tlalex.rs` 修正后通过）。
- **`Module::namespace()` 在模块未求值时调用会段错误**（`/tmp/ext-proto/src/bin/bcns3.rs` 的
  `declare-ns-first` 用例 `SIGSEGV`）。⇒ 规则：**只在 `eval()` 之后调 `namespace()`**；
  需要提前知道导出名时，用 `Object.keys(namespace)` 在求值后枚举，或（对已知包）用纯文本扫描
  `export` 语句。

### 3.2 oxc 去类型（等价于 jiti 的 Babel 路径）

```rust
pub fn strip(path: &Path, source: &str) -> Result<String, Vec<Diagnostic>> {
    let alloc = Allocator::default();
    let st = SourceType::from_path(path)?;                    // .ts/.tsx/.mts/.cts/.js/.mjs
    let ret = Parser::new(&alloc, source, st).parse();
    if !ret.diagnostics.is_empty() { /* 收集，不 panic */ }
    let mut program = ret.program;
    let sem = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .with_enum_eval(true)                                 // enum 求值（与 TS 语义一致）
        .build(&program);
    let scoping = sem.semantic.into_scoping();
    let mut opts = TransformOptions::default();
    opts.helper_loader.mode = HelperLoaderMode::External;      // 不内联 helper，见 §3.3
    opts.decorator.legacy = true;                              // jiti/Babel 的 legacy decorators
    let t = Transformer::new(&alloc, path, &opts).build_with_scoping(scoping, &mut program);
    let code = Codegen::new().with_options(CodegenOptions::default()).build(&program).code;
    Ok(code)
}
```

实测输出（`/tmp/ext-proto/src/bin/tsparse.rs`、`devdec.rs`、`dechelper.rs`）：

| 输入 | 输出 |
|---|---|
| `enum E { A, B }` | `var E = function(E){ E[E["A"]=0]="A"; ... }(E \|\| {});` |
| `namespace N { export const a = 1 }` | `let N; (function(_N){ const a = _N.a = 1; })(N \|\| (N = {}));` |
| `class P { constructor(public a: number) {} }` | `class P { a; constructor(a) { this.a = a; } }` |
| `1 satisfies number` | `1` |
| `{ x: 1 } as const` + `typeof` | `const a = {x:1}; export const b = a.x;`（类型导出被消除） |
| `abstract class` | `class`（abstract 成员被移除） |
| `using r = ...` | **原样保留**（QuickJS 原生支持 `using`，实测 `Ok`） |
| `@dec class C {}` | 默认保留装饰器 ⇒ **QuickJS 无法解析**（`unexpected token in expression: '@'`）；`opts.decorator.legacy = true` 后变换为 `babelHelpers.decorate(...)`（实测可运行） |

**装饰器策略**：语料里**没有任何** `@decorator`（`grep -rn "^\s*@" --include=*.ts` 为 0 命中），
但为了对第三方扩展稳健，默认开启 `legacy` 并用 prelude 里 **约 40 行的 `babelHelpers` 子集**
（`decorate`/`classCallCheck`/`createClass`/`defineProperty`/`toPrimitive`/`inherits`）兜住。
`HelperLoaderMode::External` 保证引用的是`babelHelpers.xxx` 而非 `@babel/runtime` 导入。

### 3.3 诊断与错误文案

- oxc 的 parse/semantic/transform 诊断**不打印到扩展可见的错误里**，只进 `tracing::debug!`；
  与 jiti 一致（Babel 也会忽略类型层错误）。
- 真正的加载错误一律走 `loader.ts` 的两条文案【逐字】：
  - `Extension does not export a valid factory function: ${extensionPath}`（`loader.ts:650`）
  - `Failed to load extension: ${message}`（`loader.ts:658`），其中 `message` 取 `err.message`（非 Error 时取 `String(err)`）。
- 工厂调用抛错时**先 `load.discard()` 再抛**（`loader.ts` 的 `initializeExtension`），
  即事务性 API 的三态机回滚（§4.6）。

### 3.4 22 个虚拟模块（逐字对齐 `virtual-modules.ts`）

`packages/coding-agent/src/core/extensions/virtual-modules.ts` 的键集合（22 个）：

```
typebox                            typebox/compile                    typebox/value
@sinclair/typebox                  @sinclair/typebox/compile          @sinclair/typebox/value
@earendil-works/kiana-agent-core      @earendil-works/kiana-tui
@earendil-works/kiana-ai              @earendil-works/kiana-ai/compat
@earendil-works/kiana-ai/models       @earendil-works/kiana-ai/oauth
@earendil-works/kiana-ai/providers/all
@earendil-works/kiana-coding-agent
@mariozechner/kiana-agent-core        @mariozechner/kiana-tui
@mariozechner/kiana-ai                @mariozechner/kiana-ai/compat
@mariozechner/kiana-ai/models         @mariozechner/kiana-ai/oauth
@mariozechner/kiana-ai/providers/all
@mariozechner/kiana-coding-agent
```

Rust 侧实现方式：**每个键一个生成的 re-export shim 模块**，源文本在宿主里构造。

```rust
// 由 prelude 在 runtime 启动时构造好 namespace 并挂到 globalThis.<slot>，然后 shim 模块只做再导出
fn shim_source(slot: &str, keys: &[&str]) -> String {
    let mut s = format!("const m = globalThis.{slot};\n");
    for k in keys { if *k != "default" { s.push_str(&format!("export const {k} = m.{k};\n")); } }
    s.push_str("export default m.default;\n");
    s
}
```

来源分三类：

| 虚拟模块 | 来源 |
|---|---|
| `typebox` / `typebox/compile` / `typebox/value` / `@sinclair/*` | **真实 npm 包**，从依赖域求值一次（§3.6）；`typebox@1.3.27` 共 **370 个模块 / 486 KiB**，首次求值含在依赖域的 47 ms 里，之后每次注入 <1 ms |
| `@earendil-works/kiana-tui` | **Rust 原生类**导出的 namespace（语料实际 import 的 19 个运行期符号，§5.1）；其中只有 `Container`/`VStack`/`HStack` 是 JS（prelude），其余全为原生 |
| `@earendil-works/kiana-coding-agent` | **Rust 原生导出的 namespace**（34 个运行期符号，§4.8） |
| `@earendil-works/kiana-ai*` / `kiana-agent-core` | `kiana-ai` crate 的原生 namespace + prelude 的纯 JS 值（`StringEnum`/`uuidv7`/`createAssistantMessageEventStream`/`ToolCallArgumentsStream`）；`@earendil-works/kiana-ai` 与 `/compat` **共用同一份 namespace**，逐符号清单见 §4.7b |

**关键实现细节**：shim 里的 `export const X = m.X` 是**快照**。若宿主在扩展求值后才往 `m` 上补
（例如 `registerProvider` 之后），扩展拿到的是旧值。因此规则是：
**所有虚拟模块 namespace 必须在任何扩展求值之前构造完毕**（在 prelude 阶段一次性建好），
扩展求值期间不再变更。

### 3.5 CJS interop（npm 依赖）

语料里唯一的纯 CJS 是 `ms@2.1.3`（`with-deps`）与 `@anthropic-ai/sdk@0.52.0`（**43 个 CJS 模块**），
以及 `sandbox` 的 `shell-quote`（多文件 CJS）。

**真 `require` 实现**（非静态转换）：

```js
// prelude.js
globalThis.__modules = new Map();       // path -> { exports, loaded }
globalThis.__compile = (name, code) => {
  if (globalThis.__modules.has(name)) return globalThis.__modules.get(name).exports;
  const fn = new Function("module", "exports", "require", "__filename", "__dirname", code);
  const mod = { exports: {}, loaded: false, id: name };
  globalThis.__modules.set(name, mod);                 // ← 必须在执行前登记（循环依赖）
  const require = (spec) => globalThis.__requireFrom(name, spec);
  try { fn(mod, mod.exports, require, name, dirOf(name)); }
  catch (e) { globalThis.__modules.delete(name); throw e; }
  mod.loaded = true;
  return mod.exports;
};
globalThis.__interop = (m) => (m && m.__esModule) ? m.default : m;
```

- `__requireFrom` 由 Rust 实现（带缓存）：归一化 → 解析（`oxc_resolver`）→ 读文件 →
  若 `.json` 则 `JSON.parse`；若 CJS 则调 `__compile`；若 ESM 则返回一个 **thunk**，
  由 loader 用一个 generated shim 包成 `export default`。**`require` 是同步的**，所以不能 require ESM
  （与 Node 一致：`ERR_REQUIRE_ESM`；语料里不出现）。
- **先登记再执行**是修复循环依赖的关键：`/tmp/ext-proto/src/bin/cjs4.rs` 中 `ms('5m')=2000`、
  双文件循环（`a.cjs` 看到 `b`，`b` 看到 `a.early`）全部通过；未预登记时是
  `Maximum call stack size exceeded`。
- `createRequire(import.meta.url)`（`doom-overlay` 用）→ 返回一个绑定到该目录的 `require` 函数。

### 3.6 依赖域（dependency realm）——性能关键

**问题**：每个扩展一个独立 `Context`（为了 `moduleCache:false` 的等价语义，见 §3.8），
而每个新 Context 的模块缓存是独立的 ⇒ typebox + marked 要在每个 Context 里重新求值。
实测 **41.2 ms/Context**（`/tmp/ext-proto/src/bin/depcost2.rs`，4 次 42.4/41.1/41.3/40.0 ms）；
79 个扩展（70 单文件 + 9 目录）就是 3.3 s 冷启动。

**方案**：把 npm 依赖求值一次在**依赖 Context** 里，然后把 namespace 以 `Persistent<Object>` 持有，
在每个扩展 Context 里作为 global 注入，再导入生成的 re-export shim。

```rust
struct DepRealm {
    ctx: AsyncContext,                                 // 或独立的同步 Context，生命周期与 engine 相同
    slots: HashMap<String, Persistent<Object>>,        // "typebox" -> namespace
}

impl DepRealm {
    fn load(&mut self) -> Result<()> {
        // 在 dep ctx 里 import，然后枚举 keys（在 eval 之后！§3.1）
        let keys: Vec<String> = { let f: Function = ctx.eval("(o) => Object.keys(o)")?; f.call((ns.clone(),))? };
        self.slots.insert(spec.into(), Persistent::save(&ctx, ns));
    }
    fn install_into(&self, ext_ctx: &Ctx<'_>) -> Result<()> {
        for (spec, p) in &self.slots {
            let slot = format!("__kiana_dep_{}", sanitize(spec));
            ext_ctx.globals().set(slot.as_str(), p.clone().restore(ext_ctx)?)?;
        }
        Ok(())
    }
}
```

实测（`/tmp/ext-proto/src/bin/deprealm2.rs`）：

```
dep typebox: 287 exports
dep marked:  18 exports
dep realm loaded in 46.8 ms
ext0: <typebox schema> | <h1>hi</h1>  [1.07 ms]
ext1: ...                             [898 µs]
ext2: ...                             [849 µs]
```

⇒ 依赖域加载一次 47 ms，之后每个扩展的**固定附加开销 0.85–1.07 ms**
（新 Context + 注入 dep slot + 通过 shim 取一次 typebox/marked 的符号）；真实扩展还要再加
strip（140 µs）+ `Module::declare`+`eval`（平均 0.25 ms，见 §9.1 的尺寸表）。
79 个扩展的总冷启动从 ~3.3 s 降到 **~150 ms**（47 ms + 79 × 1.3 ms）。

**依赖域的边界**：
- 依赖域里的对象**跨 Context 共享**：`instanceof` 在扩展 Context 里会失败（不同 Context 的
  intrinsic 不同），但 `Object.prototype.toString.call()` 仍正确。typebox 只用后者，
  marked 只用普通对象/字符串 ⇒ 无影响（实测 typebox 287 导出、marked 18 导出全部可用）。
- 若某依赖**自己**做 `instanceof`（少见），退化为"每扩展 Context 重新求值"，用
  `kiana.extensions.dependencyRealm = false` 可切换。
- 依赖域**必须**先于扩展 Context 构造、晚于其销毁（`Drop` 顺序见 §2.9）。

### 3.7 可选：prelude 字节码预热

实测（`/tmp/ext-proto/src/bin/prelude2.rs`、`bytecode.rs`）：

| 源码大小 | `Module::declare`（=解析+编译） | `Module::load`（字节码） |
|---|---|---|
| 100 KB | 12.2 ms | — |
| 400 KB | **103.6 ms**（567 KB: 567 ms） | **3.0 ms**（826 KB 字节码） |
| 40 KB（marked） | 3.4 ms（78 ns/byte） | — |

⇒ 只有"大 prelude"值得预热。本设计的 prelude 只有 ~32 KB（≈2.3 ms），**默认不预热**；
若把 kiana-tui 的 JS 组件或 typebox bundle 进 prelude（备选路线），必须用
`Module::write(WriteOptions { strip_source: true, strip_debug: true, .. })` 在**构建期**生成
`prelude.bin`（注意：构建期 runtime 必须有能解析静态导入的 loader，否则 `write` 报
`could not load module '...'`），运行时 `unsafe { Module::load(ctx, &bytes) }`。

**硬规则**：字节码与 QuickJS 版本强绑定（`QJS_VERSION` 0.16.2）；升级 rquickjs 必须重新生成，
所以字节码放 `OUT_DIR` 由 `build.rs` 生成，**不入库**。

### 3.8 `moduleCache: false` 的等价实现

jiti 用 `moduleCache: false`（`loader.ts:579-582`）保证每次加载扩展都**重新执行模块**（从而重新
执行 `export default` 之外可能存在的模块级副作用）。

QuickJS 的模块缓存**是 per-Context 的**（实测：同一 Context 里同名模块不重复求值；
新 Context 里同名模块重新求值 ⇒ 天然等价）。

因此：**每个扩展一个 `Context`**，名字用唯一 URL（`file:///…/index.ts?gen=<n>` 或
`<inline:N>` 合成名）。这与 `ca-extensions.md` §2.2.2 的工厂缓存键 `(resolvedPath, cwd, generation)`
一一对应：

| jiti 语义 | 本设计 |
|---|---|
| `moduleCache:false` ⇒ 每次重新执行 | 新 Context ⇒ 重新求值 |
| 工厂缓存 `(resolvedPath, cwd, generation)` 命中 ⇒ 不重跑模块但重跑工厂 | 缓存 `Persistent<Function>`；命中时跳过 `Module::eval` 只 `call(factory)` |
| `clearExtensionCache()` 使 `generation++` | 丢弃旧 Context、新建 Context；旧 `Persistent` 全部失效 |

> 注意：**不能**用"同一 Context + 唯一模块名"来模拟 `moduleCache:false`，因为模块级副作用
> 会重新执行，但 `globalThis` 上的东西、以及虚拟模块对象的身份会跨扩展泄漏；
> 而且一个扩展的顶层 `const` 会与其旧版本共存。每扩展一个 Context 语义最干净。

**Context 成本**：创建 **130–290 µs**（`/tmp/ext-proto/src/bin/depcost2.rs`），
内存 **约 70 KiB/Context**（`mem.rs`：runtime 27 KiB，每 Context +1000 元素数组约 70 KiB）。
100 个扩展 ≈ 7 MB，可接受。

### 3.9 发现规则（逐字对齐 `loader.ts` / `ca-extensions.md` §2.2.8）

```
resolveExtensionEntries(dir):
  1. <dir>/package.json 存在且 kiana.extensions 非空 → 逐项 path.resolve(dir, entry)
  2. 否则 <dir>/index.ts 存在 → 用它
  3. 否则 <dir>/index.js 存在 → 用它
  4. 否则 → null（不作为扩展目录）
```

- `isExtensionFile(name)` = `name.endsWith(".ts") || name.endsWith(".js")`。
- `~` **不做展开**；不递归超过一层。
- 合成路径命名（`ca-extensions.md` §4.6）：`<inline:N>`（1-based）/ `<inline:name>` /
  `<sdk:name>` / `builtin:<name>`；`isSyntheticPath` 为真时 `baseDir` 为 `undefined`，
  即 `import.meta.dirname` 对合成模块为 `undefined`。
- 加载顺序：**串行**（`loadExtensionsInternal` 是 `for ... await`），一个失败不中断其它，
  错误进 `errors` 数组（`{ path, error }`）。

### 3.10 缓存与代际

```rust
pub struct ExtensionCache {
    cwd: PathBuf,
    generation: u64,
    factories: HashMap<PathBuf, Persistent<Function<'static>>>,
}
```

- `useExtensionCacheCwd(cwd)`：cwd 变化 ⇒ `clearExtensionCache()` ⇒ `generation++`。
- `isCurrentCacheToken(token)`：只有在 token 仍有效时才读写缓存（`loader.ts:563`）。
- `ctx.reload()` / 会话替换 ⇒ `generation++`、销毁所有 Context、重建（对应
  `ca-extensions.md` §8.4 的 `Failed` 状态 + 代际 token；旧 `kiana`/`ctx` 的调用抛
  `staleMessage`，§4.6）。

---

## 4. 扩展 API 绑定策略（逐 API 组）

### 4.0 总原则

1. **`kiana` 与 `ctx` 是 Rust 构造的普通 JS 对象/原生类实例**——不是 Proxy、不是 JS 实现。
   理由是同步 getter（`ctx.cwd`、`ctx.hasUI`、`ctx.mode`、`ui.theme`）必须是属性而非方法。
2. **每个 API 组一个 Rust 类型**，所有跨帧持有的 JS 回调（handler、renderer、factory）存为
   `Persistent<Function>`，集中登记在一个 `Registry` 里以便统一清理（§2.9）。
3. **所有参数从 JS→Rust 用 typed 抽取，不用 JSON 中转**（除消息体这类大对象）。
   实测：`obj.get::<_, String>` 45–64 ns、`obj.set` 36–40 ns、`Function::call` 87 ns。
   大对象（消息、transcript）用 `JSON.stringify` + `serde_json`：1 KB = 15.3 µs、5 KB = 44.8 µs、20 KB = 162 µs。
4. **返回值按"JS 侧期望的形状"构造**：能构造原样 JS 对象的就构造（`Object::new` + `set`），
   不引入隐藏类/代理。

### 4.1 `kiana` 对象的形状

```rust
/// 一个扩展一个
pub struct ExtensionApi {
    pub extension_id: ExtId,
    pub state: ApiState,                      // Loading / Active / Failed（§4.6）
    registry: Rc<RefCell<ExtensionRegistry>>, // tools/commands/flags/handlers/renderers
}
```

JS 侧看到的是：

```js
kiana = {
  // 41 个事件
  on(event, handler) -> () => void,
  // 31 个方法
  registerTool, registerCommand, registerShortcut, registerFlag, getFlag,
  registerMessageRenderer, registerMarkdownTransformer, registerEntryRenderer, registerToolRenderer,
  sendMessage, sendUserMessage, appendEntry, setSessionName, getSessionName, setLabel,
  exec, getActiveTools, getAllTools, getSettings, setActiveTools, getCommands,
  setModel, getThinkingLevel, setThinkingLevel,
  registerProvider, unregisterProvider,
  registerMcpServer, unregisterMcpServer, getMcpServers,
  registerVirtualModel, unregisterVirtualModel,
  // 1 个字段
  events: { emit(channel, data), on(channel, handler) -> () => void },
}
```

**计数核对**（脚本从 `types.ts` 抽取，与本设计一致）：
`ExtensionAPI` = **41 个 `on` 重载 + 31 个非 `on` 方法 + 1 个 `events` 字段 = 32 个不同方法名**。
`ExtensionUIContext` = **28 个成员**（27 个方法 + 只读属性 `theme`；`setWidget` 有两个重载 ⇒ 接口里共 **29 条声明**）。
`ExtensionContext` = **18 个具名成员**（逐字对齐 `types.ts` 与 `ca-extensions.md` §2.1.3；含可选 `thinkingLevel?`）。

### 4.2 `on`：41 个事件

事件名（逐字，按 `types.ts` 顺序）：

```
project_trust              resources_discover          session_start
session_info_changed       session_before_switch        session_before_fork
session_before_compact     session_compact             session_compact_failed
session_shutdown           mcp_servers_change          session_before_tree
session_tree               context                     context_with_system
cache_warming_decision     before_provider_request     before_provider_headers
after_provider_response    provider_stream_event       before_agent_start
agent_start                agent_end                   agent_before_settle
agent_settled              ui_prompt_start             ui_prompt_end
turn_start                 turn_end                    message_start
message_update             message_end                 tool_execution_start
tool_execution_update      tool_execution_end          model_select
thinking_level_select      tool_call                   tool_result
user_bash                  input
```

绑定形态：

```rust
// prelude 里统一一个分发点，避免 41 个宿主函数
globalThis.__kiana_on = (api, event, handler) => { /* Rust 返回的 dispatch 句柄 */ };

// Rust:
fn on<'js>(this: This<Object<'js>>, ctx: Ctx<'js>, event: String, handler: Function<'js>) -> Result<Function<'js>> {
    let api = ExtensionApi::from_object(&this.0).ok_or(Error::Exception)?;
    let id = api.registry.borrow_mut().handlers.push(event.clone(), Persistent::save(&ctx, handler)?);
    // 返回 () => void 的退订闭包
    let unsub = Function::new(ctx.clone(), move |ctx2: Ctx<'js>| { /* 从 registry 移除 */ })?;
    Ok(unsub)
}
```

**返回值语义**（`ca-extensions.md` §2.3.14）：`on` 返回一个**退订函数**。扩展里常见
`const off = kiana.on("tool_call", h); ... off()`。Rust 侧必须真的移除 handler，且移除后
**不再调用**（不是"调用但忽略"）。

**派发（dispatch）**：`ca-extensions.md` §2.3.14 定义 12 个专用派发器的合并语义 —— `runner.ts` 里是
11 个 `async emitXxx()`（`emitCacheWarmingDecision`/`emitMessageEnd`/`emitToolResult`/`emitToolCall`/
`emitUserBash`/`emitContext`/`emitBeforeProviderRequest`/`emitBeforeProviderHeaders`/
`emitBeforeAgentStart`/`emitResourcesDiscover`/`emitInput`）+ `emitBoundary()`（`session_before_*` 系列共用）。
Rust 侧对每个事件有一个 `emit_*` 函数，它们**共享**同一套骨架：

```rust
/// 注意：`Value<'js>` 不能逃出 `with_js` 的闭包（生命周期绑定在 Ctx 上，原型里实测
/// "returning Value/Object from a closure" 编译失败）。所以**在闭包内就把返回值转成 Rust 拥有的类型**。
fn dispatch(&self, event: &str, arg: &Arg) -> Result<DispatchOutcome> {
    let mut acc = DispatchOutcome::default();
    for h in self.handlers_for(event) {                 // 快照（§2.3.5）
        let step: Step = self.with_js(|ctx| {
            let f = h.restore(&ctx)?;
            let arg_js = arg.to_js(ctx.clone())?;       // Rust 值 -> 新 JS 值
            let v = f.call((arg_js, ctx_object(ctx.clone())?))?;
            // Promise ⇒ 登记到 pending，返回 Step::Await(id)；否则就地转换
            if let Some(p) = v.as_promise() { Step::Await(self.pending.register(&ctx, p)?) }
            else { Step::Done(ConvertedReturn::from_js(&ctx, v)?) }   // JS 值 -> Rust 值
        })?;
        match step {
            Step::Done(r) => acc.merge(event, r)?,      // 每个事件的合并规则
            Step::Await(id) => return Ok(DispatchOutcome::Suspended { id, acc }),
        }
    }
    Ok(acc)
}
```

**handler 是 async 时**：`f.call(...)` 返回 `Value`，若是 Promise ⇒ 存进 `pending`，
主循环继续跑直至 settle，再按该事件的合并规则处理结果。这就是 `ca-extensions.md` §8.5
"串行 await" 的实现：**一次只等一个 handler**。

### 4.3 工具：`registerTool` 与 `ToolDefinition`（20 个字段）

`ToolDefinition` 的字段（逐字，顺序同 `types.ts`）：

```
name label description promptSnippet promptGuidelines parameters constrainedSampling renderShell
prepareArguments outputSchema exposure namespace reachesTools annotations defaultActive
prepareLoadout executionMode execute renderCall renderResult
```

绑定策略：**不构造 Rust 镜像结构体再逐字段拷贝**，而是保留 `Persistent<Object>`（原样 JS 对象）
+ Rust 侧按需读取：

```rust
pub struct RegisteredTool {
    pub obj: Persistent<Object<'static>>,   // 原样对象（扩展可能后续修改它，Node 版也是同一对象）
    pub name: String,                        // 加载时读取，用于索引
    pub schema: Persistent<Object<'static>>, // parameters（typebox 对象，原样）
    pub execute: Persistent<Function<'static>>,
    pub render_call: Option<Persistent<Function<'static>>>,
    pub render_result: Option<Persistent<Function<'static>>>,
    pub exposure: ToolExposure,              // "direct"|"model-only"|"codemode"|"deferred"|"hidden"
    pub default_active: bool,
    pub execution_mode: Option<ToolExecutionMode>,
}
```

理由：
- `prepareArguments`/`reachesTools`/`prepareLoadout`/`renderCall`/`renderResult` 都是**函数**，
  只有在需要时才调用，构造镜像结构体会让"扩展运行中修改自己的 tool 对象"与 Node 版不一致。
- 语料里 `minimal-mode.ts` 就是 `{...tools.read, renderCall, renderResult}` 的**展开覆盖**，
  依赖"读到的对象就是最终对象"。

**调用**（`execute`）：

```rust
pub async fn call_tool(&self, tool_call_id: &str, params: Value<'_>, signal: Option<AbortHandle>,
                       on_update: Option<Persistent<Function<'static>>>) -> Result<ToolResult> {
    // 1) ctx = runner.createToolContext(toolCallId, signal)   ← 与 wrapper.ts 一致
    // 2) 调 execute(toolCallId, params, signal, onUpdate, ctx)
    // 3) 结果是 Promise ⇒ 交给主循环；返回 AgentToolResult { content, details, isError?, structuredContent? }
}
```

`wrapper.ts` 的语义必须保留：工具被包装为 `wrapToolDefinition(definition, () => runner.createToolContext(...))`，
即**每次调用一个新 ctx**，且 ctx 带 `executeTool()` 与 `tools`（`ExtensionToolContext`）。

**schema 与校验**：校验发生在 **`kiana-ai` crate**（`validateToolArguments`），JS 侧只提供 schema 对象。
实测要点：
- typebox schema 是**带 `Symbol.for("TypeBox.Kind")` 的普通对象**；
  `JSON.stringify` 自动忽略 symbol ⇒ `toToolDeclaration` 的 `JSON.parse(JSON.stringify(...))` 语义天然成立。
- Rust 侧判定"是否 typebox"用 `Object.getOwnPropertySymbols(o).some(s => s === Symbol.for("TypeBox.Kind"))`
  （`validation.ts:326`），实测可行。
- `validateToolArguments` 里 `Compile(schema)` 需要真正的 typebox 编译器 ⇒ **必须靠依赖域里的
  typebox**（§3.6），不能用 Rust 手写的 JSON Schema 校验器替代（错误文案会不同）。

**渲染**：`renderCall(args, theme, context)` / `renderResult(result, options, theme, context)`
返回 `Component`。`ToolRenderContext` 的 14 个字段（逐字）：

```
args toolCallId invalidate lastComponent state cwd executionStarted argsComplete
isPartial expanded showImages isError durationMs outputPad
```

`invalidate` 是 Rust 回调（`Function::new`），调用它 ⇒ 标记该 tool 的渲染缓存失效 + `tui.requestRender()`。
`lastComponent` 是上一次渲染返回的 `Persistent<Object>`（用于复用/动画），需要按 toolCallId 缓存。

### 4.4 `ExtensionContext`（18 个成员）

```rust
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ExtensionContext")]
pub struct ExtContext {
    #[qjs(skip_trace)] ext_id: ExtId,
    #[qjs(skip_trace)] kind: CtxKind,                 // Base / Tool / Command / Replaced
    #[qjs(skip_trace)] cwd: String,
    #[qjs(skip_trace)] mode: ExtensionMode,           // "tui" | "rpc" | "json" | "print"
    #[qjs(skip_trace)] has_ui: bool,
    #[qjs(skip_trace)] tools: Option<Vec<Persistent<Object<'static>>>>,  // ExtensionToolContext.tools
}

#[rquickjs::methods]
impl ExtContext {
    #[qjs(get, rename = "ui")]        fn ui(&self, ctx: Ctx<'_>) -> Result<Object<'_>> { /* 每次返回同一 ui 对象 */ }
    #[qjs(get, rename = "mode")]      fn mode(&self) -> String { self.mode.as_str().into() }
    #[qjs(get, rename = "hasUI")]     fn has_ui(&self) -> bool { self.has_ui }
    #[qjs(get, rename = "cwd")]       fn cwd(&self) -> String { self.cwd.clone() }
    #[qjs(get, rename = "sessionManager")] fn session_manager(&self, ctx: Ctx<'_>) -> Result<Object<'_>> { /* 原生类 */ }
    #[qjs(get, rename = "modelRegistry")]  fn model_registry(&self, ctx: Ctx<'_>) -> Result<Object<'_>> { /* 原生类 */ }
    #[qjs(get, rename = "model")]     fn model(&self) -> Option<Object<'_>> { /* undefined 或 model 对象 */ }
    #[qjs(get, rename = "scopedModels")] fn scoped_models(&self) -> Vec<Object<'_>> { }
    #[qjs(get, rename = "thinkingLevel")] fn thinking_level(&self) -> Option<String> { }
    #[qjs(get, rename = "signal")]    fn signal(&self) -> Option<Object<'_>> { /* AbortSignal 或 undefined */ }
    #[qjs(rename = "isIdle")]         fn is_idle(&self) -> bool { }
    #[qjs(rename = "isProjectTrusted")] fn is_project_trusted(&self) -> bool { }
    #[qjs(rename = "abort")]          fn abort(&self) -> () { }
    #[qjs(rename = "hasPendingMessages")] fn has_pending_messages(&self) -> bool { }
    #[qjs(rename = "shutdown")]       fn shutdown(&self) -> () { /* 投递主循环 Shutdown 任务 */ }
    #[qjs(rename = "getContextUsage")] fn get_context_usage(&self, ctx: Ctx<'_>) -> Option<Object<'_>> { }
    #[qjs(rename = "compact")]        fn compact(&self, opts: Option<Object<'_>>) -> () { }
    #[qjs(rename = "getSystemPrompt")] fn get_system_prompt(&self) -> String { }
}
```

**`#[qjs(get)]` 实测可用**（`/tmp/ext-proto/src/bin/getter.rs`：
`x.cwd === "/tmp" && x.hasUI === true && x.mode === "tui" && x.isIdle() === true` ⇒ `true`）。
注意：**同一个方法不能同时是 getter 和 setter**（宏直接报错
`a function can't both be a setter and a getter at the same time`），所以 `mode` 只做 getter。

**三种扩展 ctx**（`ExtensionToolContext extends ExtensionContext`）：

| 类型 | 额外成员 | 绑定方式 |
|---|---|---|
| `ExtensionToolContext` | `readonly tools: AgentTool[]`、`executeTool(name, args, options?) => Promise<AgentToolCallOutcome>` | 同一个原生类，`kind == Tool` 时 `tools` 非空；`executeTool` 总是存在（非 tool ctx 调用抛错） |
| `ExtensionCommandContext` | `getSystemPromptOptions()`、`waitForIdle()`、`newSession()`、`fork()`、`navigateTree()`、`switchSession()`、`reload()` | 同一原生类，`kind == Command` |
| `ReplacedSessionContext` | 额外 `sendMessage`/`sendUserMessage`（返回 Promise） | 同一原生类，`kind == Replaced` |

**关键细节**：`ca-extensions.md` §2.1.3 的 `ReplacedSessionContext` 里有 `sendMessage` 返回 `Promise<void>`，
而 `kiana.sendMessage` 返回 `void`。同一个名字在两种对象上语义不同 ⇒ 必须按 `kind` 分派，
**不能共用一个函数**。

**stale ctx 文案**【逐字】（`loader.ts:195-200`，`assertActive` 使用）：

```
This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().
```

（`invalidate(message?)` 允许覆盖该文案。）

### 4.5 `ExtensionUIContext`（28 个成员 = 27 方法 + `theme`）

| 成员 | 绑定 | 说明 |
|---|---|---|
| `select(title, options, opts?)` → `Promise<string \| undefined>` | Rust：投递 `UiRequest::Select` 到主循环，主循环用对话框栈（`ca-extensions.md` §7.4）实现；结果经 `__kiana.settle` 回填 | `opts.signal`/`opts.timeout` 生效 |
| `confirm(title, message, opts?)` → `Promise<boolean>` | 同上 | 超时 auto-dismiss 带倒计时 |
| `input(title, placeholder?, opts?)` → `Promise<string \| undefined>` | 同上 | |
| `notify(message, type?)` | 直接推入 TUI 消息区 | `type` 默认 `"info"` |
| `onTerminalInput(handler)` → `() => void` | 注册一个 `Persistent<Function>`；TUI 输入阶段先问一遍（`consume`/`data`） | 返回退订 |
| `setStatus(key, text \| undefined)` | 状态栏 map，`undefined` 删除 | |
| `setWorkingMessage(message?)` / `setWorkingVisible(bool)` / `setWorkingIndicator(opts?)` / `setHiddenThinkingLabel(label?)` | 直接字段 | |
| `setWidget(key, string[] \| factory \| undefined, opts?)` | 数组 ⇒ 每行 `Text`，**最多 10 行** + `"... (widget truncated)"`【逐字】；工厂 ⇒ `Persistent<Function>`，每帧 `content(ui, theme)` 求值一次并缓存 | `placement` 默认 `"aboveEditor"` |
| `setFooter(factory \| undefined)` / `setHeader(factory \| undefined)` | 工厂 → `Persistent`，渲染时调用 | |
| `setTitle(title)` | 写终端标题（OSC 2） | |
| `custom(factory, options?)` → `Promise<T>` | 工厂返回 `Component`；`overlay:true` 走 overlay 栈；`onHandle` 回调 | 见 §5.7 |
| `pasteToEditor(text)` / `setEditorText(text)` / `getEditorText()` | 直接调 Editor 原生类 | |
| `editor(title, prefill?)` → `Promise<string \| undefined>` | 打开多行编辑器对话框 | |
| `addAutocompleteProvider(factory)` | 存 `Persistent`，Editor 每次输入时调用 | |
| `setEditorComponent(factory \| undefined)` / `getEditorComponent()` | 存/取工厂；Rust 在创建编辑器时调用 `factory(tui, theme, keybindings)` | 语料里 3 个示例用它 |
| `theme`（只读） | 返回 `Theme` 原生类的**同一个实例** | |
| `getAllThemes()` / `getTheme(name)` / `setTheme(name \| Theme)` | 主题注册表 | `setTheme` 返回 `{ success, error? }` |
| `getToolsExpanded()` / `setToolsExpanded(bool)` | 直接字段 | |

**无 UI 环境**（print/json 模式）：`runner.ts:331-361` 的 `noOpUIContext` 必须逐字复刻默认返回值：

```
select -> undefined   confirm -> false   input -> undefined
getEditorText -> ""   getEditorComponent -> undefined   getAllThemes -> []
getTheme -> undefined setTheme -> { success:false, error:"UI not available" }【逐字】
getToolsExpanded -> false   custom -> undefined（never）
其余方法 -> 空操作；onTerminalInput -> () => {}
```

### 4.6 事务性 API 状态机（`ca-extensions.md` §2.2.4）

```rust
pub enum ApiState {
    Loading { pending: Vec<RuntimeChange> },   // 加载期间的"立即生效"型调用先入队
    Active,
    Failed { stale: String },                  // invalidate() 之后
}
```

规则（逐字对齐 `loader.ts` 的 `createExtensionRuntime`）：

1. 加载期间，动作方法（`sendMessage`/`setModel`/`createContext`/…）是**抛错桩**：
   ```
   Extension runtime not initialized. Action methods cannot be called during extension loading.
   ```
   【逐字】而 `setModel` 是特例：`Promise.reject(new Error("Extension runtime not initialized"))`。
2. `registerTool()` 在加载期**合法**；`refreshTools` 是空函数。
3. **注册型**调用（tool/command/flag/shortcut/renderer/provider/mcp/virtualModel）进
   `pendingRuntimeChanges`，在 `load.commit()` 时统一 flush 到 runner（`bindCore` 的 9 步顺序，§2.3.9）。
4. 工厂抛错 ⇒ `load.discard()`（丢弃所有 pending 注册，不留半成品）。
5. `invalidate(msg?)`：首次生效，后续忽略；同时退订该扩展在 `kiana.events` 上的所有订阅
   （`eventBusUnsubscribers`），并把状态置 `Failed`。
6. 任何动作方法在 `Failed` 时抛 `staleMessage`。

### 4.7 Provider / MCP / VirtualModel

| API | 绑定 |
|---|---|
| `registerProvider(provider)` / `registerProvider(name, config)` | 两个重载：单参数时 `provider` 自带 `name`/`id`；双参数时构造 `Provider`。都进 `pendingProviderRegistrations`；`bindCore` 时交给 `kiana-ai` 的模型注册表 |
| `unregisterProvider(name)` | 立即生效（Active 时） |
| `registerMcpServer(name, config)` / `unregisterMcpServer(name)` / `getMcpServers()` | 进 `McpServerRegistry`；配置校验的 28 条错误串由 `kiana-extensions` 的 mcp 单元负责 |
| `registerVirtualModel(model)` / `unregisterVirtualModel(provider, id)` | 进 `pendingVirtualModelRegistrations`；`route(request, ctx)` 是 JS 函数 ⇒ `Persistent`，Rust 在路由时调用（可返回 Promise） |

`ProviderConfig` 的 `streamSimple(model, context, options)` 是**关键契约**：
它返回 `AssistantMessageEventStream`（纯 JS 类，`packages/ai/src/utils/event-stream.ts`）。
本设计**不把它搬到 Rust**——因为 `event-stream.ts` 是纯 JS（FifoQueue + async iterator + `result()` promise），
且扩展自己会 `for await` 消费它。做法：

```js
// prelude.js —— 原样搬运 event-stream.ts 的逻辑
class EventStream { /* push/end/result/async iterator */ }
class AssistantMessageEventStream extends EventStream { ... }
globalThis.__kiana_ai.createAssistantMessageEventStream = () => new AssistantMessageEventStream();
```

Rust 侧只需在需要时 `for await`？**不需要**：`streamSimple` 的调用方是 `kiana-ai` crate 自己
（当扩展注册了自定义 API 时），Rust 通过 `Persistent` 调用 `streamSimple(...)` 拿到 JS 的
`AssistantMessageEventStream`，然后 `await` 其 `result()`（一个 Promise）——手写 Promise 协议天然支持。

### 4.7b `@earendil-works/kiana-ai*` / `kiana-agent-core` 的运行期面

从语料 strip 后的 import 面统计（`/tmp/spec_parts` 脚本）：

| 虚拟模块 | 运行期符号（值） |
|---|---|
| `@earendil-works/kiana-ai` | `StringEnum` `ToolCallArgumentsStream` `Type` `calculateCost` `collapseSystemMessages` `createAssistantMessageEventStream` `getCurrentSystemPrompt` `getCurrentTools` `uuidv7` |
| `@earendil-works/kiana-ai/compat` | `anthropicMessagesApi` `createAssistantMessageEventStream` `openAIResponsesApi` `registerApiProvider` `streamSimple` |
| `@earendil-works/kiana-agent-core` | 无（`AgentMessage`/`AgentToolResult`/`ThinkingLevel` 都是类型，strip 后为空） |

注意 `@earendil-works/kiana-ai` 在 `virtual-modules.ts` 里**解析到 compat 入口**（compat 是 core 的严格超集），
所以两个键的 namespace 必须**同一份**，且必须同时含上表两行的并集
（`Type`/`StringEnum`/`uuidv7` 与 `anthropicMessagesApi`/`openAIResponsesApi`）。

绑定方式：

| 符号 | 实现 |
|---|---|
| `Type` | 依赖域里 `typebox` 的默认导出（同一个对象，`instanceof`/引用相等在依赖域内自洽） |
| `StringEnum` | prelude 纯 JS（`Type.Unsafe({ type: "string", enum: values })`） |
| `uuidv7` | prelude 纯 JS（40 行，单调时间戳 + 随机） |
| `createAssistantMessageEventStream` | prelude 纯 JS（`event-stream.ts` 原样搬运，见 §4.7） |
| `ToolCallArgumentsStream` | prelude 纯 JS（增量 JSON 参数解析，语料里 `structured-output.ts` 用） |
| `calculateCost` | Rust（`kiana-ai` crate 的定价表；逐 token 计算，无 I/O） |
| `collapseSystemMessages` / `getCurrentSystemPrompt` / `getCurrentTools` | Rust（读当前会话状态） |
| `registerApiProvider` / `streamSimple` / `anthropicMessagesApi` / `openAIResponsesApi` | Rust（`kiana-ai` crate 的 provider 注册表；`streamSimple` 返回 JS 的 `AssistantMessageEventStream`） |

### 4.8 `@earendil-works/kiana-coding-agent` 的 34 个运行期导出

从语料 **strip 后**的 import 面统计（`/tmp/spec_parts` 脚本，见附录 A）：

```
BorderedLoader CONFIG_DIR_NAME CustomEditor DEFAULT_MAX_BYTES DEFAULT_MAX_LINES DynamicBorder
VERSION convertToLlm createBashTool createBashToolDefinition createEditTool createEditToolDefinition
createFindTool createFindToolDefinition createGrepTool createGrepToolDefinition createLsTool
createLsToolDefinition createReadTool createReadToolDefinition createWriteTool createWriteToolDefinition
defineTool formatReadLineRange formatSize getAgentDir getMarkdownTheme getSettingsListTheme keyHint
parseFrontmatter serializeConversation truncateHead truncateLine withFileMutationQueue
```

| 类别 | 导出 | 绑定 |
|---|---|---|
| 工具工厂 | `create{Bash,Edit,Write,Read,Find,Grep,Ls}Tool(Definition)` | Rust 构造 `ToolDefinition` 对象（含 `execute` 闭包 → Rust 函数） |
| 常量 | `CONFIG_DIR_NAME`、`VERSION`、`DEFAULT_MAX_BYTES`、`DEFAULT_MAX_LINES` | 直接值 |
| 纯函数 | `convertToLlm`、`serializeConversation`、`formatReadLineRange`、`formatSize`、`truncateHead`、`truncateLine`、`parseFrontmatter`、`keyHint`、`defineTool` | Rust 实现；`defineTool` 是**恒等函数**（`types.ts:690`），prelude 里一行 |
| 组件 | `CustomEditor`、`BorderedLoader`、`DynamicBorder` | 原生类（§5） |
| 主题 | `getMarkdownTheme`、`getSettingsListTheme` | Rust 从主题生成 |
| 其他 | `getAgentDir`、`withFileMutationQueue` | Rust（后者是 `tokio::sync::Mutex` per-path） |

`@earendil-works/kiana-agent-core` 的 3 个运行期符号：`AgentMessage`/`AgentToolResult`/`ThinkingLevel`
（都是**类型**，strip 后为空）⇒ **不需要实现**，只需 namespace 里留同名占位（供 `Object.keys` 稳定性）。

### 4.9 `kiana.events`（EventBus）

`core/event-bus.ts` 的语义（逐字）：

```ts
emit(channel, data)                     // 同步，直接 emitter.emit
on(channel, handler) -> unsubscribe     // handler 被包成 async safeHandler：
                                        //   try { await handler(data) } catch (err) { console.error(`Event handler error (${channel}):`, err) }
```

绑定：Rust 侧一个 `EventBus`（`HashMap<String, Vec<(SubId, Persistent<Function>)>>`）。
- `emit` **同步**调用所有 handler；handler 返回 Promise 时**不等待**（Node 的 `EventEmitter` 也不等），
  但 Promise 的 rejection 要接住并打印 `Event handler error (<channel>):`【逐字】。
- `on` 返回的退订函数必须真的移除；`invalidate` 时统一退订（§4.6 规则 5）。
- `console.error` 走预置的 console shim（§6.2），输出到 stderr，与 Node 一致。

### 4.10 事件派发与错误

- 所有 handler 调用包在 `catch` 里：错误变成扩展错误事件（`ExtensionErrorListener`），
  走三种落点（【逐字】，`ca-extensions.md` §3.8）：
  ```
  Extension "<path>" error: <error>              （交互式聊天区，error 色）
  Extension error (<path>): <error>              （print/json stderr）
  {"type":"extension_error","extensionPath":"<path>","event":"<event>","error":"<error>"}   （RPC stdout）
  ```
- **不做**"一个 handler 抛错就中断其它 handler"：Node 版是逐个 await + try/catch，
  继续下一个。
- 派发顺序 = 注册顺序；`ca-extensions.md` §2.3.14 的 12 个专用派发器在此之上做合并。

---

## 5. TUI 组件互操作

### 5.1 哪些是原生类，哪些是 JS

语料里扩展从 `@earendil-works/kiana-tui` 实际 import 的**运行期**符号只有 **19 个**（strip 后统计）：

```
Box CURSOR_MARKER Container Editor Input Key Markdown SelectList SettingsList Spacer Text
fuzzyFilter isKeyRelease matchesKey parseKey renderFakeCursor truncateToWidth visibleWidth wrapTextWithAnsi
```

| 符号 | 实现 | 理由 |
|---|---|---|
| `Component`（`render(width): string[]`、`handleInput?`、`handleMouse?`、`wantsKeyRelease?`、`invalidate()`） | **接口**（JS 里是鸭子类型，无运行时值） | `tui-core.md` §2.9 |
| `Editor` | **Rust 原生类**（`components/editor.ts` 2473 行的移植） | 需要 `super.handleInput()`/`super.render()`；子类 3 个示例 |
| `Text`、`Box`、`Spacer`、`Input`、`SelectList`、`SettingsList`、`Markdown`、`Loader` | **Rust 原生类** | 纯渲染，无 JS 依赖；`Markdown` 走 `marked`（依赖域，§5.5） |
| `Container`、`VStack`、`HStack` | **JS**（prelude 里 ~80 行） | 纯组合逻辑（`children[]` + 逐子组件 `render`），放 JS 更省事且行为原样（`tui-components.md` §2.1–§2.3） |
| `CURSOR_MARKER` = `"\x1b_kiana:c\x07"`【逐字】 | **常量**（prelude） | `tui.ts:196` |
| `renderFakeCursor(text)` | JS 常量拼接 | `tui.ts:205` |
| `visibleWidth`/`truncateToWidth`/`wrapTextWithAnsi` | **Rust 函数**（`tui-core` 已实现） | 性能关键路径（`tui-core.md` §2.0），且需要 `Intl.Segmenter` 等价（§5.5） |
| `matchesKey`/`parseKey`/`isKeyRelease`/`Key` | **Rust 函数 + Rust 对象** | `keys.ts` 1455 行的移植；`Key.ctrlAlt("p")` 等是模板字符串函数 |
| `fuzzyFilter` | Rust 函数 | `fuzzy.ts` 159 行 |

### 5.2 `This` 纪律（最关键的一条）

**从 Rust 调用 JS 对象上的方法时，必须把对象本身作为 `This` 传入**，否则 JS 子类的 `super` 调用
拿不到 `this`：

```rust
// 反例：this 是 undefined ⇒ JS 里 super.render() 会 "Error converting from js 'undefined' into type 'Base'"
let f: Function = obj.get("render")?;
f.call::<_, Vec<String>>((width,))?;

// 正例：
let f: Function = obj.get("render")?;
f.call::<_, Vec<String>>((This(obj.clone()), width))?;
```

实测（`/tmp/ext-proto/src/bin/thisclass.rs`）：

```
plain: ["[hi w=10]"]
subclass: ["[abc w=20]", "sub-text=abc"]        ← super.render() + this.getText() 都工作
cross-call setText/getText: "mutated"           ← Rust 设的状态，JS 读得到；反之亦然
duck: ["duck-30"]                               ← 纯 JS 对象也能用 This 调
detached w/o This: err Error converting from js 'undefined' into type 'object'
```

**适用范围**：`Component.render`/`handleInput`/`handleMouse`/`invalidate`/`wantsKeyRelease`、
`Editor` 的每个可覆写方法、`Theme` 的样式函数（若是 JS 提供的）、工厂返回的组件。

### 5.3 原生类如何持有 JS 回调

`Editor` 需要 theme 的样式函数、`Markdown` 需要 `highlightCode`、`Text` 需要 `customBgFn`、
`SelectList` 需要 `SelectListTheme` —— 这些都是 JS 闭包。原生类里的字段用 `Persistent<Function>`
+ `#[qjs(skip_trace)]`：

```rust
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Text")]
pub struct Text {
    #[qjs(skip_trace)] text: RefCell<String>,
    #[qjs(skip_trace)] padding_x: i32,
    #[qjs(skip_trace)] padding_y: i32,
    #[qjs(skip_trace)] bg_fn: Option<Persistent<Function<'static>>>,
}

#[rquickjs::methods]
impl Text {
    #[qjs(constructor)]
    fn new<'js>(ctx: Ctx<'js>, text: Opt<String>, padding_x: Opt<i32>, padding_y: Opt<i32>,
                bg_fn: Opt<Function<'js>>) -> Result<Self> {
        Ok(Text {
            text: RefCell::new(text.0.unwrap_or_default()),
            padding_x: padding_x.0.unwrap_or(1),
            padding_y: padding_y.0.unwrap_or(1),
            bg_fn: bg_fn.0.map(|f| Persistent::save(&ctx, f)).transpose()?,
        })
    }
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: i32) -> Result<Vec<String>> {
        let styled = match &self.bg_fn {
            Some(p) => { let f = p.clone().restore(&ctx)?; f.call::<_, String>((raw,))? }
            None => raw,
        };
        Ok(vec![styled])       // 实际实现还要做宽度处理与 padding（tui-components.md §2.4）
    }
}
```

实测可用（`/tmp/ext-proto/src/bin/asynchost.rs`：原生 `Text` 持有 JS 样式闭包，
`t.render().join(",")` ⇒ `<hi>,len=4`）。

**构造签名必须与 TS 逐字一致**（默认值也要一致），因为扩展直接 `new Text(...)`：

```
Text(text = "", paddingX = 1, paddingY = 1, customBgFn?)
Box(paddingX = 1, paddingY = 1, bgFn?)
Spacer(lines = 1)
Editor(tui, theme, options? = {})           // options: { paddingX?, autocompleteMaxVisible? }
SelectList(items, maxVisible, theme, layout? = {})
SettingsList(items, maxVisible, theme, ...)
Loader(tui, spinnerColorFn, messageColorFn, message, ...)
```

### 5.4 三个编辑器子类（语料最硬的 UI 约束）

`modal-editor.ts`、`rainbow-editor.ts`、`border-status-editor.ts` 都是：

```ts
class ModalEditor extends CustomEditor {
  handleInput(data: string): void {
    if (this.mode === "normal") { if (data === "i") { this.mode = "insert"; return; } }
    super.handleInput(data);      // ← 调 Rust 基类
  }
}
ctx.ui.setEditorComponent((tui, theme, kb) => new ModalEditor(tui, theme, kb));
```

要求：

1. `CustomEditor` 是 **Rust 原生类**（继承 `Editor` 原生类），构造签名 `(tui, theme, keybindings)`。
2. JS 子类能 `extends CustomEditor`，且 `super.handleInput(data)` 进入 Rust 实现（实测通过）。
3. Rust 基类的 `handleInput` 必须实现 `custom-editor.ts` 的完整顺序（`actionHandlers` →
   `keybindings.matches(...)` → paste image → extension shortcut → 最后 `super.handleInput`）。
4. `renderTopBorder()` 可被 JS 覆写（`border-status-editor.ts`），Rust 的 `render` 里必须
   `self.call_js("renderTopBorder")` 而不是直接画。
5. `this.getText()`/`this.setText()`/`this.borderColor(...)` 由 Rust 提供（实测 `Class::<Editor>::from_object`
   可从 `This<Object>` 恢复原生状态）。

`CustomEditor` 的构造参数里 `keybindings` 是 `KeybindingsManager`（`core/keybindings.ts:329`
继承 `tui/keybindings.ts` 的 `KeybindingsManager`）。本设计把它也做成原生类，
暴露 `matches(data, keybinding)`（`keybindings.ts:270`）——语料里 `custom-editor` 的内部实现用到它，
扩展自己不用。

### 5.5 `Intl.Segmenter` 与 `toLocaleString`

**QuickJS 无 `Intl`**（实测 `Intl === undefined`，`Intl.Segmenter` 直接抛错）。
kiana-tui 的 `utils.ts` 有两处模块级 `new Intl.Segmenter(...)`，缺了它整个 `@earendil-works/kiana-tui`
在加载期就崩。

方案：**原生绑定里不出现 `Intl`**。`visibleWidth`/`sliceByColumn`/`truncateToWidth`/`wrapTextWithAnsi`
是 Rust 函数，内部用 `unicode-segmentation` 的字素簇迭代 + `unicode-width` 的宽度；
**但**仍要在 prelude 里放一个 `Intl.Segmenter` shim，因为：

- 第三方扩展可能自己 `new Intl.Segmenter`（语料里没有，但要稳健）；
- `marked-cjk-friendly` 等 npm 包可能用。

```js
// prelude.js —— 只覆盖 grapheme/word 的 segment()，返回迭代器
globalThis.Intl = globalThis.Intl || {};
globalThis.Intl.Segmenter = class {
  constructor(locale, options = {}) { this.granularity = options.granularity || "grapheme"; }
  segment(input) {
    const items = globalThis.__segment(String(input), this.granularity); // Rust 提供
    return { [Symbol.iterator]: () => items[Symbol.iterator]() };
  }
};
globalThis.Intl.NumberFormat = class { /* 仅支持 en-US 千分位 */ };
globalThis.Intl.DateTimeFormat = class { /* 仅支持 en-US 的常见组合，见下 */ };
```

Rust `__segment(s, granularity)` 返回 `[{segment, index, input, isWordLike?}]`，
用 `unicode_segmentation::UnicodeSegmentation::graphemes/unicode_words` 实现。

**`toLocaleString` 覆盖**：语料里 **5 个示例**用到它（`overlay-qa-tests.ts:1294`、`custom-compaction.ts:38`、`entry-renderer.ts:26`、
`input-transform.ts:37`、`message-renderer.ts:27`），其中 `custom-compaction.ts` 用的是 `Number.prototype.toLocaleString`（千分位），
其余 4 个是 `Date.prototype.toLocaleString`/`toLocaleTimeString`。
QuickJS 自带实现是固定的 `MM/DD/YYYY, hh:mm:ss AM` 且不做千分位 ⇒ 在 prelude 里**覆盖原型**：

```js
const nf = new Intl.NumberFormat("en-US");
Number.prototype.toLocaleString = function () { return nf.format(this); };
const dtf = { /* en-US: M/D/YYYY, h:mm:ss AM */ };
Date.prototype.toLocaleString = function () { return globalThis.__formatDate(+this, "datetime"); };
Date.prototype.toLocaleTimeString = function () { return globalThis.__formatDate(+this, "time"); };
Date.prototype.toLocaleDateString = function () { return globalThis.__formatDate(+this, "date"); };
```

`__formatDate` 是 Rust（`chrono`，本地时区），输出对齐 Node 的 en-US ICU。**这是"可观察差异"**
（这 5 个示例正是 `design-extension-runtime-risk-first.md` §11.1 标为有 Intl 风险的 B 级项，覆盖后升为 A 级），
必须在测试里与 Node 的输出逐字比对（样本见附录 A）。

### 5.6 自定义组件、widget、footer、header

| API | 绑定 |
|---|---|
| `ctx.ui.setWidget(key, factory)` | 工厂 `(tui, theme) => Component` 存 `Persistent`；TUI 每帧调用一次（结果缓存到 `invalidate`） |
| `ctx.ui.setFooter(factory)` / `setHeader(factory)` | 同上；替换内置 header/footer 的渲染 |
| `ctx.ui.custom(factory, opts)` → `Promise<T>` | 工厂 `(tui, theme, keybindings, done) => Component`；`done(result)` 由 Rust 注入（`Function::new`），调用后 settle Promise 并销毁组件 |
| `ctx.ui.custom` 的 `overlayOptions` | 可以是对象或 `() => OverlayOptions`（每帧求值） |
| `tui.showOverlay(component, options)`（扩展直接调，`overlay-qa-tests.ts` 3 处） | 原生类 `TUI` 的方法，转发到 tui-core 的 overlay 栈 |
| `tui.requestRender()`（语料 36 处） | 标记 dirty + 唤醒主循环 |
| `tui.addInputListener(fn)` | 注册 `Persistent`，返回退订 |

**渲染失败的处理**（`ca-extensions.md` §3.6）：组件 `render` 抛错时**不能**让整个 TUI 崩。
`JsComponent::render` 捕获后：
- 记录扩展错误（三种落点之一）；
- 返回上一次成功的行（首次失败返回 `[]`）；
- 打印到 stderr 一次（带堆栈），后续同组件同错误去重。

### 5.7 `overlay` / `OverlayHandle`

`OverlayOptions` 的字段（逐字，`tui.ts:254`）：

```
width minWidth maxHeight anchor offsetX offsetY row col margin visible nonCapturing
```

`OverlayHandle`：`hide() setHidden(b) isHidden() focus() unfocus() isFocused() getBounds()`。
`SizeValue` 支持数字或 `"50%"`；`anchor` 是 9 个位置之一（`overlay-qa-tests.ts` 遍历全部）。

实现：**Rust 原生类**，方法转发到 `tui_core` 的 `TuiBase` overlay 栈
（`tui-core.md` §8.2 要点 3：`overlayStack: Vec<OverlayEntry>` + `focusOrder` 单调计数器，
**不要**在 focus 时重排 Vec）。

`ctx.ui.custom({ overlay: true })` 走同一条栈；`onHandle(handle)` 在创建后回调一次。

### 5.8 `Theme`

`Theme` 是原生类（`modes/interactive/theme/theme.ts`）。语料用到的接口（按使用频次）：

```
theme.fg(token, text)   308 次      theme.bold(text)   35 次
theme.bg(token, text)     5 次      theme.italic/underline/inverse/strikethrough 共 1 次
```

用到的 token（频次，共 9 个）：

```
accent 65  muted 65  dim 55  success 28  toolTitle 27
warning 22  error 22  toolOutput 16  text 8
```

实现要点：
- `fg`/`bg` 是 `(token: string, text: string) => string`，按 ANSI 真彩输出；
  颜色解析（`parseColor`/`oklch`/`okhsl`/`mixColors`）在 `tui-core` 的 `colors.rs` 里，Theme 复用。
- 只读 getter：`appearance`（`"dark"|"light"`）、`colors`（token → Color）。
- 方法：`style(text, opts)`、`fg`、`bg`、`bold`、`italic`、`underline`、`inverse`、`strikethrough`、
  `getFgAnsi`、`getBgAnsi`、`getColorMode`、`getThinkingBorderColor(level)`、`getBashModeBorderColor`。
- `ctx.ui.theme` 返回**同一个实例**（只读），扩展可把它传给组件。
- `getMarkdownTheme()`/`getSettingsListTheme()` 是从 `Theme` 生成的**普通对象**（方法都是 JS 函数），
  在 Rust 里构造。

### 5.9 `TUI` 对象暴露给扩展的成员

语料只用到 4 个：`requestRender`（36 次）、`showOverlay`（3 次）、`start`/`stop`（各 1 次）。
但 `ctx.ui.custom(factory)` 的工厂签名是 `(tui, theme, keybindings, done)`，扩展会把 `tui` 存起来
后续调用。所以原生类 `TUI` 至少要实现 `tui.ts:465-496` 的全部方法（转发到 `TuiBase`），
其中 `renderNow(force?)` 与 `requestRender(force?)` 要能在"当前正在渲染"时安全地被调用
（重入渲染 → 标记 dirty 延后到下一帧，不能递归）。

### 5.10 `BorderedLoader` / `DynamicBorder`

`BorderedLoader extends Container`（`modes/interactive/components/bordered-loader.ts`），
语料里 `question.ts`/`questionnaire.ts`/`qna.ts`/`handoff.ts` 用它。
设计：**JS 实现**（prelude 里 ~40 行），因为它只做 `Container.addChild(new DynamicBorder(...))`
+ `CancellableLoader`/`Loader` 的组合，而这些组件本身是原生的。这样 `extends BorderedLoader`
的扩展子类行为天然一致。

`DynamicBorder` 也是 JS（`Container` 的子类，按宽度画 3 种边框）。

---

## 6. Node 内建与全局 shim

### 6.1 语料实际需要的面（统计自 86 个 `.ts`）

**导入的内建模块（10 个）与符号（约 40 个）**：

| 模块 | 符号 | 实现 |
|---|---|---|
| `node:fs` | `readFileSync`(3) `writeFileSync` `existsSync`(4) `readdirSync` `appendFileSync` `createReadStream` `watch` `constants` | Rust：`std::fs` + `tokio::fs` + `notify`（watch）+ 流式读取 |
| `node:fs/promises` | `readFile` `writeFile` `appendFile` `access` `mkdtemp` `rm` | Rust `tokio::fs` |
| `node:path` | `join`(10) `resolve`(2) `dirname`(3) `basename` `extname` `relative` | Rust（`path` 语义，**POSIX**） |
| `node:os` | `homedir` `tmpdir` | `dirs::home_dir` / `std::env::temp_dir`（尊重 `TMPDIR`） |
| `node:child_process` | `spawn`(3) `exec` `execSync` `spawnSync` `execFile` | Rust `tokio::process` / `std::process` |
| `node:readline` | `createInterface` | Rust（按行切流） |
| `node:util` | `promisify` | JS（`(fn) => (...args) => new Promise(...)`） |
| `node:url` | `fileURLToPath` `pathToFileURL` | Rust（`url` crate） |
| `node:zlib` | `gunzipSync` | Rust（`flate2`） |
| `node:module` | `createRequire` | Rust + `__requireFrom`（§3.5） |

**全局**（按语料实际使用）：

| 全局 | 缺失？ | 实现 |
|---|---|---|
| `console` | **是** | JS shim → `__hostLog(level, args)` → stderr（`console.error` 用于 EventBus 错误） |
| `process` | **是** | JS shim 对象：`cwd() env argv platform execPath exit() stdout/stderr.write on() once() versions.node` |
| `Buffer` | **是** | JS 门面（`Uint8Array` 子类）+ Rust 静态方法（见 §6.4） |
| `setTimeout`/`setInterval`/`clear*` | **是** | §2.5 |
| `fetch` | **是** | Rust（`reqwest` + `Response`/`Headers`/`ReadableStream` shim） |
| `AbortController`/`AbortSignal` | **是** | prelude 纯 JS（§2.6） |
| `TextEncoder`/`TextDecoder` | **是** | JS shim（UTF-8 编码/解码，`stream:` 不支持） |
| `URL`/`URLSearchParams` | **是** | Rust 原生类（`url` crate） |
| `crypto` | **是** | `getRandomValues`（`getrandom`）、`randomUUID`（`uuid`）、`subtle.digest("SHA-256")`（`sha2`） |
| `structuredClone` | **是** | JS shim（结构化克隆子集：plain object/array/Map/Set/Date/TypedArray） |
| `performance` | 否（有 `.now()`/`.timeOrigin`） | 直接用 |
| `atob`/`btoa` | 否 | 直接用 |
| `queueMicrotask` | 否 | 直接用 |
| `WebAssembly` | **是** | 默认 `undefined`；`feature = "webassembly"` 时绑 `wasmtime` |

### 6.2 `process` shim（语义要点）

```js
globalThis.process = {
  env: /* Rust 注入的普通对象，含 PATH/HOME/...，可写 */,
  argv: ["kiana", ...],              // argv[1] 必须是 kiana 可执行文件路径（subagent 用它拼子进程）
  execPath: /* kiana 二进制路径 */,
  platform: "linux" | "darwin" | "win32",
  cwd: () => __hostProcess("cwd"),
  exit: (code) => { throw new ExitSignal(code); },   // 不真的立即退出：让主循环有序关闭
  stdout: { write: (s) => __hostWrite(1, s), isTTY: true, columns: 80, on(){}, once(){} },
  stderr: { write: (s) => __hostWrite(2, s), isTTY: true },
  stdin:  { on(){}, once(){}, isTTY: true, setRawMode(){}, resume(){}, pause(){} },
  on() {}, once() {}, off() {},                     // 进程信号：注册即可，主循环转发 SIGINT/SIGTERM
  versions: { node: "22.0.0" },                     // SDK 用它判断 Node 版本（见下）
  nextTick: (fn, ...a) => queueMicrotask(() => fn(...a)),
  emitWarning() {},
  pid: 0,
};
```

- **`process.versions.node` 必须存在**：`@anthropic-ai/sdk` 用
  `globalThis.process?.versions?.node` 判断环境（`internal/uploads.mjs`），缺了会走浏览器分支并失败。
- **`argv[1]`/`execPath` 必须指向 kiana 二进制**：`subagent/index.ts` 的 `getKianaInvocation()` 逻辑是
  "若 `argv[1]` 存在且文件存在 ⇒ `{ command: execPath, args: [argv[1], ...] }`；否则按
  `basename(execPath)` 判断是否是通用运行时（`node`/`bun`），不是就 `{ command: execPath }`，
  是就 `{ command: "kiana" }`"。所以 **`execPath = <kiana 二进制>`、`argv[1] = <不存在或 kiana 自身>`**
  都能得到正确结果；本设计取 `argv[1] = 不存在的路径`、`execPath = /path/to/kiana`，
  于是变成 `{ command: "/path/to/kiana", args }` —— 正确。
- **`process.exit(code)` 不能直接 `std::process::exit`**：扩展可能在 handler 里调它
  （`custom-provider-gitlab-duo/test.ts`）。抛一个特殊的 `ExitSignal` 到主循环，
  由主循环执行 `session_shutdown` 后退出。
- `process.env` 从 Rust 真实环境读入（`std::env::vars`），写回扩展 Context 内的 JS 对象；
  若扩展写 `process.env.X`，对 `child_process` 派生的子进程生效（通过 `envs` 传递）。

### 6.3 `child_process` 语义风险

| API | 必须正确的地方 |
|---|---|
| `spawn(cmd, args, { stdio, cwd, env, shell })` | 返回 `Child`：`stdout`/`stderr` 是可 `on("data")` 的流、`stdin` 可写、`on("exit")`/`on("close")`、`kill(sig)`、`pid` |
| `spawnSync` + `stdio: "inherit"` | **必须真的继承终端**（`interactive-shell.ts` 靠它跑 vim/less）⇒ 暂停 TUI raw mode，子进程结束后恢复 |
| `execFile(cmd, args, opts, cb)` | 回调 `(err, stdout, stderr)`；有 `maxBuffer` |
| `execSync(cmd, { encoding, stdio })` | 同步阻塞（**会卡 TUI**，可接受，Node 也卡） |
| `exec` | 走 shell（`/bin/sh -c`），回调同上 |

- `onUpdate`/流式：子进程输出通过 `HostDone::StreamChunk` 回到主循环，再由 JS 侧 `emit("data", chunk)`。
- 子进程退出 ⇒ `exit` 事件先于 `close`；`close` 携带 `(code, signal)`。
- **`stdio: "inherit"` 的实现**：进入 `TuiBase::stop()` 的"暂时释放终端"路径（`tui-core.md` 的
  `terminal.stop()`/`start()`），子进程用真实 fd 0/1/2。

### 6.4 `Buffer`

语料用到：`Buffer.from`(2)、`Buffer.byteLength`(5)、`Buffer.concat`(2)、`Buffer.alloc`、
`Buffer.isView`。签名：

```ts
Buffer.from(string | ArrayBuffer | Uint8Array, encoding?) -> Buffer
Buffer.byteLength(string, encoding?) -> number
Buffer.concat(list, totalLength?) -> Buffer
Buffer.alloc(size, fill?) -> Buffer
Buffer.isView(x) -> boolean
buf.toString(encoding?, start?, end?) -> string      // 语料用 "base64"、"utf8"
buf.length / buf[0] / subarray / equals
```

实现：JS 类 `Buffer extends Uint8Array`，`from`/`byteLength`/`concat` 在 JS 里用
`TextEncoder`/`atob`/`btoa`（QuickJS 原生有 `atob`/`btoa`）或 Rust 函数。
**`Buffer.from(x).toString("base64")` 与 `Buffer.concat(chunks).toString()` 必须逐字节正确**
（`ssh.ts` 用它传文件内容、`overlay-qa-tests.ts` 拼子进程输出）。

### 6.5 `fetch` 与 Web Streams

`custom-provider-anthropic` 与 `custom-provider-gitlab-duo` 用 `fetch` + `ReadableStream`，
详见 §7.2。

### 6.6 `structuredClone`

`debug-provider.ts:71` 用 `structuredClone(event.data)`。要求：克隆宿主传给事件的任意对象
（含嵌套数组/对象/Date/TypedArray）。JS shim ~25 行（不处理 Map/Set 之外的原型）。

### 6.7 `fs.watch`

`file-trigger.ts` 用 `fs.watch(file, cb)` 监听文件变化。Rust `notify 8.2.1` + `tokio` 桥：
`watch` 返回一个 `FSWatcher` 对象（含 `close()`），事件经 `HostDone::StreamChunk` 回到主循环后调 cb。
**注意**：`notify` 的事件可能在删除/重建时重复，`file-trigger.ts` 依赖"触发即读"，
重复触发只是多读一次，无副作用。

### 6.8 流（Streams）语义差距（诚实清单）

- `fs.createReadStream` + `readline.createInterface({ input })`：本设计实现"按行切分"的简化版本
  （`git-merge-and-resolve.ts` 只用 `for await`/`on("line")`）。
- **不实现**背压（`pause`/`resume`/`highWaterMark` 的精确交互）；语料不用。
- `child_process` 的 `stdout.on("data")` 给的是 `Buffer`；`encoding` 选项会转字符串。

---

## 7. 事件循环与真实 I/O 的边界

### 7.1 `kiana.exec`

```
JS: kiana.exec("git", ["status"], { cwd, timeout, signal }) -> Promise<ExecResult>
      ↘ __kiana.promise("exec", {command, args, options})   （立即返回 Promise）
Rust: host_start("exec") -> tokio::task::spawn_local(async {
          tokio::process::Command ... output().await
          完成后 done_tx.post(HostDone::Settle { id, ok: true, value: ExecResult })
      })
主循环: 应用 settle -> JS then() 链推进
```

`ExecResult { stdout, stderr, code, killed }`（`core/exec.ts`）。
`timeout` ⇒ 到点 `kill()`，`killed: true`、`code` 为负或 `null`（与 Node 一致）。
`signal` ⇒ 监听 abort，同样 `killed: true`。

### 7.2 `fetch` + `ReadableStream`

`custom-provider-anthropic` / `custom-provider-gitlab-duo` 用 `fetch` + `ReadableStream`：

```
JS: fetch(url, init) -> Promise<Response>
       Response { ok, status, headers, json(), text(), body: ReadableStream }
       body.getReader() -> { read(): Promise<{value: Uint8Array, done}> }
```

实现：Rust `reqwest` 流式响应，每读一块就 `HostDone::StreamChunk { id, bytes }`，
JS 侧 `ReadableStream` shim 把块塞进队列并 resolve 挂起的 `read()`。
**必须支持 `AbortSignal`**（SDK 的 `fetchWithTimeout`）。

### 7.3 同步 vs 异步的划分

| 必须同步 | 可以异步 |
|---|---|
| `ctx.cwd`/`hasUI`/`mode`/`model` 等 getter | `kiana.exec` |
| `visibleWidth`/`truncateToWidth`/`matchesKey` | `ctx.ui.select/confirm/input/editor/custom` |
| `Component.render(width)` | `fetch` |
| `fs.readFileSync`/`writeFileSync`/`existsSync` | `fs.promises.*` |
| `child_process.spawnSync`/`execSync` | `child_process.spawn`/`exec` |
| `require()`（CJS） | `import()`（ESM 动态导入） |
| `registerTool`/`on` 等注册 | `streamSimple`（返回 stream） |

---

## 8. npm 包支持

### 8.1 解析顺序（对齐 jiti + Node）

给定 `base`（引用方模块的绝对路径）与 `specifier`：

1. **虚拟模块表**（22 键，§3.4）⇒ 直接返回 `kiana:virtual/<key>`，不落盘。
2. `specifier` 以内 `node:` 开头 ⇒ `kiana:node/<name>`（Node 内建 shim，§6）。
3. 相对/绝对（`.`/`..`/`/`）⇒ 规范化拼接（§3.1）+ 扩展名探测（`.ts`/`.tsx`/`.mts`/`.cts`/`.js`/`.mjs`/`.cjs`/`.json`），
   再试 `<path>/index.<ext>`。
4. 裸说明符 ⇒ **`oxc_resolver 11.24.3`**：
   ```rust
   let resolver = oxc_resolver::Resolver::new(ResolveOptions {
       extensions: vec![".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".cjs", ".json"],
       condition_names: vec!["node".into(), "import".into(), "require".into(), "default".into()],
       main_fields: vec!["module".into(), "main".into()],
       main_files: vec!["index".into()],
       modules: vec!["node_modules".into()],
       ..Default::default()
   });
   ```
   从 `base` 的目录逐级向上找 `node_modules`。
5. 找不到 ⇒ 抛 `Cannot find module '<spec>'`（与 Node 文案一致，带 `Require stack`/`imported from` 可选）。

条件顺序 `["node","import","require","default"]` 是刻意的：jiti 在 TS 源码模式下会同时满足
`import` 与 `require`，取 `import` 分支（typebox 的 `exports` 两个分支指向同一文件，实测无差异）。

### 8.2 文件的四种形态

| 形态 | 判定 | 处理 |
|---|---|---|
| ESM `.mjs` / `.js` + `"type":"module"` | 扩展名或包 `type` | 直接 `Module::declare`（**不 strip**，除非 `.ts`） |
| TS `.ts`/`.mts`/`.cts` | 扩展名 | oxc strip（§3.2）⇒ ESM |
| CJS `.cjs` / `.js` + `"type":"commonjs"`（或缺省） | 包 `type` 或 `module.exports`/`require(` 探测 | CJS 包装（§3.5） |
| JSON | `.json` | `export default <parsed>` |

**CJS 探测**（无 `package.json` 时的兜底）：源码里出现顶层 `module.exports`/`exports.`/`require(` 且
没有 `import`/`export` 语句 ⇒ 视为 CJS。

### 8.3 依赖从哪找

- **扩展自己的 `node_modules`**：`with-deps/` 有 `package.json` + `node_modules`（`ms`），
  从扩展目录逐级向上解析即可命中。
- **kiana 内置的 `node_modules`**（若存在）：不在设计内——本文假设 kiana 是**单二进制**，
  没有 `node_modules`。typebox/marked 通过**依赖域**（§3.6）解决，它们是**内嵌的**（见 8.4）。
- **不安装、不写盘**：本设计**不**在运行时 `npm install`。缺依赖就是加载失败，
  错误文案 `Failed to load extension: Cannot find module '<x>'`。

### 8.4 内嵌的大包：typebox 与 marked

| 包 | 版本 | 体积（可达模块图） | 用途 |
|---|---|---|---|
| `typebox` | 1.3.27 | **370 个 `.mjs` / 486 KiB** | 扩展的 schema（`Type.Object` 等）+ `validateToolArguments` 的 `Compile` |
| `marked` | 18.0.11 | 42.8 KiB（单文件 `lib/marked.esm.js`） | `Markdown` 组件 |
| `marked-cjk-friendly` | 1.2.0 | 小 | `Markdown` 的 CJK 强调修复 |

处理方式：**构建期**把这三个包的**源码**（不是 bundle）嵌进二进制（`include_bytes!` 或
`rust-embed`），运行时在**依赖域 Context** 里以虚拟文件系统（`kiana:dep/typebox/...`）形式求值一次。
理由：
- 486 KiB 源码进二进制完全可接受（当前最小二进制 1.58 MB，加 oxc 后 3.08 MB）；
- 不 bundle 就保留了 `exports` 子路径（`typebox/compile`、`typebox/value`）的原始结构，
  与虚拟模块表一一对应；
- 只求值一次 ⇒ 40 ms 一次性成本。

**替代方案（否决）**：把 typebox 用 Rust 重写。`Compile` 的错误文案（`formatValidationPath` 依赖
typebox 的 `TLocalizedValidationError` 结构）无法逐字复刻，`ca-extensions.md` §3.4 的校验错误测试会挂。

### 8.5 `createRequire`

`doom-overlay/doom-engine.ts` 用 `createRequire(import.meta.url)` 加载 `doom.js`。
实现：Rust 返回一个绑定到 `import.meta.url` 所在目录的 `require` 函数（§3.5 的 `__requireFrom` 的偏应用）。
**注意**：`doom.js` 是 Emscripten 产物，内部 `require("fs")`/`require("path")` 并调
`WebAssembly.instantiate` ⇒ 默认不可用（§11.3）。

### 8.6 不支持的 npm 形态

| 形态 | 结论 |
|---|---|
| 原生 `.node` addon | **不支持**（QuickJS 无 N-API 宿主）。`gondolin` 的 `cpu-features`、`ssh2` 的可选依赖属此类 |
| `worker_threads` / `node:vm` / `node:cluster` | 不提供（语料不用） |
| `import.meta.resolve` | 提供（同步返回说明符解析结果，等价 Node 的 `import.meta.resolve`） |
| Yarn PnP / `--preserve-symlinks` | 不支持 |
| `package.json` 的 `imports`（`#foo`） | 不支持（`oxc_resolver` 支持，但本设计不启用；语料不用） |

---

## 9. 性能（实测）

### 9.1 加载

| 步骤 | 实测 | 备注 |
|---|---|---|
| oxc strip | **140 µs/文件**（86 文件 12.1 ms，`stripbench.rs` 预热后 5 轮取最优） | 最大 45 KB 的 `overlay-qa-tests.ts` 1.2 ms；35 KB 的 `subagent/index.ts` 1.1 ms |
| `Context` 创建 | **130–290 µs** | `AsyncContext::full` 实测 136–290 µs；同步 `Context::full` 更快 |
| `Module::declare` + `eval`（按源码大小） | **26 µs–2.0 ms** | `modsizes.rs`：0.6 KB=26 µs、13 KB=466 µs、31 KB=1.29 ms、32 KB=1.74 ms、**40 KB=2.01 ms**；41–54 ns/byte |
| 依赖域（typebox + marked 一次） | **46.8 ms** | 之后每扩展 **0.85–1.07 ms**（新 Context + 注入 dep slot + import typebox/marked 的 shim + 各用一次） |
| 每扩展的固定附加开销（Context + dep 注入） | **0.85–1.07 ms** | `/tmp/ext-proto/src/bin/deprealm2.rs`（不含真实扩展模块的 declare+eval） |
| 若每 Context 重跑 typebox + marked | **41.2 ms** × N | `/tmp/ext-proto/src/bin/depcost2.rs`（4 次 42.4/41.1/41.3/40.0 ms）⇒ 必须用依赖域 |
| prelude（~32 KB）eval | ~2.3 ms（一次性） | |

**冷启动预算**（79 个扩展 = 70 单文件 + 9 目录；语料平均 5.3 KB/文件）：
prelude 2.3 ms + 依赖域 46.8 ms + 79 × (0.9 ms 固定附加 + 0.14 ms strip + 0.25 ms declare+eval) ≈ **150 ms**。
最坏情况（所有扩展都像 `overlay-qa-tests.ts` 一样 40 KB）：79 × 2.0 ms ≈ 160 ms ⇒ 总计 ≈ **310 ms**。
若改为"每扩展一个 Context 且不使用依赖域"：41.2 ms × 79 ≈ 3.3 s，不可接受。

### 9.2 热路径

| 操作 | 实测 | 说明 |
|---|---|---|
| Rust → JS 函数调用 | **87 ns** | `/tmp/ext-proto/src/bin/callbench.rs`（200k 次） |
| JS 对象属性 get / set | **45 / 36 ns** | 同上 |
| `ctx.eval("1+1")` | **1.80 µs** | 进入 JS 的固定成本（`callbench.rs`） |
| JS 组件渲染一帧 | **1.34 µs**（1000 帧 1.34 ms） | `/tmp/ext-proto/src/bin/bridge.rs` |
| 同线程 `SyncContext::with` 渲染一帧 | **1.0–10.9 µs**（首帧 10.9，稳态 1.0） | `/tmp/ext-proto/src/bin/syncall.rs` |
| 跨线程同步通道渲染一帧 | **16.5 µs** 均 / 57 µs 最坏（600 帧） | `/tmp/ext-proto/src/bin/xthread2.rs` |
| JSON 往返 1 KB / 5 KB / 20 KB | **15.3 / 44.8 / 162 µs** | `/tmp/ext-proto/src/bin/jsonbench.rs` |

结论：60 fps 预算 16.7 ms，单帧渲染 1.34 µs 占 **0.008%**；即使一屏 30 个 JS 组件也只有 ~40 µs。
真正的成本在"大对象 JSON 往返"（20 KB 消息 160 µs），所以事件派发时**尽量只传需要的字段**，
不要每次把完整 transcript 序列化进 JS。

### 9.3 内存

| 项 | 实测 |
|---|---|
| `Runtime` 基线 | **27 KiB** |
| 每个 `Context` + 1000 元素数组 | **+70 KiB** |
| 10 个 Context（含 deps） | 2.8 MB |
| 10 个 Context（仅 shim + 简单模块） | 95 KiB |

预算：100 个扩展 ≈ 7–10 MB；`set_memory_limit(512 MB)` 是**单 Runtime** 上限（所有 Context 共享），
所以 100 个扩展不会触发。

### 9.4 二进制体积

| 组合 | stripped |
|---|---|
| rquickjs 最小特性（`std`+`loader`+`macro`） | **1.58 MB**（1,578,480 B） |
| + oxc 全套，同步 `Runtime`（`stripbench`） | **3.09 MB**（3,085,248 B） |
| + oxc 全套，`AsyncRuntime`（`tsstrip`） | 3.08 MB（3,078,136 B） |
| + tokio + 依赖域原型（`deprealm2`，不含 oxc） | **1.70 MB**（1,700,944 B） |
| oxc 全套 + AsyncRuntime 原型（`perf`） | 4.55 MB（4,552,136 B） |

生产二进制目标：**< 12 MB**（rquickjs + oxc + tokio + reqwest + 其余）。`wasmtime` 特性额外 +15 MB
（默认关闭）。

### 9.5 优化手段（按收益排序）

1. **依赖域**（§3.6）：41 ms × N → 47 ms 一次性。最大收益。
2. **按需 strip + 磁盘缓存**（可选）：把 strip 结果缓存到 `~/.cache/kiana/ext/<hash>.mjs`，
   命中时省 140 µs/文件（50 个扩展省 7 ms，收益一般，默认关闭）。
3. **prelude 字节码**（§3.7）：prelude 32 KB 时只省 ~2 ms，**默认不做**；
   若 prelude 膨胀到 400 KB 则是 100 ms → 3 ms。
4. **减少 JSON 往返**：消息事件只传必要字段。
5. **组件渲染缓存**：`invalidate` 之前不重渲染（与 Node 版一致），已由 `tui-core` 实现。

---

## 10. prelude 规范

### 10.1 内容清单（全部在 `assets/extension-runtime/`，构建期用 oxc 转成 ESM 并嵌入）

| 文件 | 内容 | 约行数 |
|---|---|---|
| `shims.ts` | `console`、`process`、`TextEncoder`/`TextDecoder`、`structuredClone`、`atob`/`btoa` 兜底、`performance` 兜底 | 180 |
| `timers.ts` | `setTimeout`/`setInterval`/`clear*`/`setImmediate`/`nextTick`/`queueMicrotask` 兜底 | 60 |
| `abort.ts` | `AbortController`/`AbortSignal`（含 `AbortSignal.timeout`） | 70 |
| `buffer.ts` | `Buffer`（`Uint8Array` 子类 + `from`/`byteLength`/`concat`/`alloc`/`isView`/`toString`） | 110 |
| `node-events.ts` | `EventEmitter`（`on`/`off`/`once`/`emit`/`removeAllListeners`） | 60 |
| `promise.ts` | `__kiana` 手写 Promise 协议（§2.3） | 40 |
| `containers.ts` | `Container`/`VStack`/`HStack`/`DynamicBorder`/`BorderedLoader`（`Spacer` 是原生类） | 140 |
| `event-stream.ts` | `EventStream`/`AssistantMessageEventStream`（原样搬运 `packages/ai/src/utils/event-stream.ts`） | 150 |
| `typebox-helpers.ts` | `StringEnum`（原样搬运 `packages/ai/src/utils/typebox-helpers.ts`） | 20 |
| `uuid.ts` | `uuidv7`（原样搬运） | 40 |
| `tool-args-stream.ts` | `ToolCallArgumentsStream`（增量 JSON 参数解析） | 60 |
| `intl.ts` | `Intl.Segmenter`/`NumberFormat`/`DateTimeFormat` 最小实现 + 原型覆盖（§5.5） | 90 |
| `fetch-streams.ts` | `Response`/`Headers`/`ReadableStream`/`ReadableStreamDefaultReader` shim | 120 |
| `index.ts` | 汇总、安装到 `globalThis`、建立 22 个虚拟模块 slot 的占位对象 | 80 |

合计约 **1,240 行 / ~32 KB**（minify 后）。

### 10.2 安装顺序（必须严格）

```
1. 创建 Runtime + Context（内存/栈/中断限制在 Runtime 上设好）
2. 注入宿主函数：`__kiana.hostStart` / `__kiana.settle` / `__kiana.timerStart` / `__kiana.timerCancel` /
   `__hostLog` / `__hostWrite` / `__hostProcess` / `__segment` / `__formatDate` / `__requireFrom`，
   以及 **22 个虚拟模块 slot 的构造器**（此时先建**空对象**占位）
3. eval prelude（一次）——它把 `console`/`process`/`Buffer`/`setTimeout`/`Intl` 等装到 `globalThis`
4. 依赖域：在 dep Context 里 import typebox/marked，把 namespace 回填到 2 建好的 slot 上
5. （可选）`Module::load(prelude.bin)` 替代第 3 步的 eval（§3.7）
6. 逐个扩展：新 Context → `install_into`（把 slot 的 Persistent 注入该 Context 的 globalThis）→
   import 扩展 → 调 factory
```

**为什么槽位必须在 prelude 求值前存在**：`deprealm` 早期版本在 shim 里引用尚未注入的 global 会报
`cannot read property 'AddImmutable' of undefined`（实测）。prelude 里的 shim 函数（例如
`Type.Object` 的转发）在**求值时**就捕获了 `globalThis.__kiana_dep_typebox` 的引用路径，
所以第 2 步必须先放一个空对象；第 4 步**填充的是同一个对象的属性**（
`Object.assign(slotObj, ns)`），而不是替换 global 绑定 —— 否则 prelude 早期捕获的引用就成了孤儿。

### 10.3 纯 JS 值为什么必须留在 JS

| 值 | 原因 |
|---|---|
| `EventStream`/`AssistantMessageEventStream` | 扩展 `for await (const ev of stream)`；Rust 实现要跨 async iterator 协议，得手写生成器语义，得不偿失。150 行 JS 原样搬运即可 |
| `StringEnum` | `Type.Unsafe({type:"string", enum: values})`，一行 |
| `uuidv7` | 单调时间戳 + 随机数，40 行 |
| `Container`/`VStack`/`HStack` | 纯组合（`children.map(c => c.render(w))`），放 JS 让第三方子类行为天然一致 |
| `ToolCallArgumentsStream` | 增量 JSON 参数解析（`structured-output.ts` 用）；纯字符串状态机，~60 行 |
| `isFocusable` | `tui.ts:186` 的一行鸭子判定，见 §10.4 |

### 10.4 prelude 的自我约束

- **不得** import 任何虚拟模块（会与 slot 构造产生循环）。
- **不得** 假设 `process.env` 在加载时已就绪（宿主在 2 之前注入；若缺，用 `{}` 兜底）。
- **不得** 定义 `Component`（它是接口，无运行时值）；但**要**定义 `isFocusable`（`tui.ts:186`：
  `component !== null && "focused" in component`）。
- `console` 的 `log/info/debug` 走 stderr（TUI 模式下 stdout 被 TUI 占用）；`error/warn` 也走 stderr。
  **这与 Node 不同**（Node 的 `console.log` 走 stdout），但 kiana 的 TUI 模式本来就不能往 stdout 写。
  print/json 模式可以配置为走 stdout（`__hostLog` 的 level→fd 映射）。

---

## 11. `examples/extensions/**` 兼容清单

语料：**70 个单文件 `*.ts` + 9 个目录**（共 86 个 `.ts`、451,654 字节 = 441 KiB，不含 `doom-overlay/doom/build/doom.js` 与 `.d.ts`）。
分类口径按**运行时真实导入**（strip 之后）统计：

- **A**：只 import `kiana-*` 虚拟模块与 `typebox`；不需要任何 Node 内建、不需要 `process`、不需要 npm。
- **B**：需要 Node 内建或 `process`（§6 已全部实现）。
- **C**：需要额外 npm 包。
- **D**：需要 WebAssembly 或原生 addon。

### 11.1 单文件（70 个）

脚本按 `node:`/裸内建/`process.`/裸 npm 说明符分类，结果：**A = 53，B = 17，C = 0，D = 0**。

**B 类 17 个（逐条列出，全部可原样运行）**：

| 文件 | 依赖 | 备注 |
|---|---|---|
| `bash-spawn-hook.ts` | `process.cwd()` | — |
| `border-status-editor.ts` | `process.env.HOME`、`setInterval` | 编辑器子类；`super.handleInput` 走 Rust（§5.4） |
| `built-in-tool-renderer.ts` | `process.cwd()` | — |
| `claude-rules.ts` | `node:fs`、`node:path` | `readFileSync`/`readdirSync`/`existsSync` |
| `file-trigger.ts` | `node:fs`（`watch`/`readFileSync`/`writeFileSync`） | 需 `notify`（§6.7） |
| `git-merge-and-resolve.ts` | `node:fs`（`createReadStream`）、`node:path`、`node:readline` | 需流式按行（§6.8） |
| `interactive-shell.ts` | `node:child_process`（`spawnSync` + `stdio:"inherit"`） | 需真实终端继承（§6.3） |
| `mac-system-theme.ts` | `node:child_process`（`exec`）、`node:util`（`promisify`）、`setInterval` | — |
| `minimal-mode.ts` | `os`（`homedir`）、`process.cwd()` | 用 `create{Bash,Edit,Write,Read,Find,Grep,Ls}ToolDefinition`（§4.8） |
| `notify.ts` | `child_process`（`require("child_process").execFile`）、`process.env`/`stdout.write` | 唯一用 `require()` 的 A/B 类文件 |
| `overlay-qa-tests.ts` | `child_process`（`spawn`）、`Buffer`、`setTimeout`/`setInterval` | 45 KB，最大压力测试；`tui.showOverlay` 3 处 |
| `preset.ts` | `node:fs`、`node:path` | `readFileSync`/`readFile` |
| `provider-payload.ts` | `node:fs`、`node:path` | — |
| `ssh.ts` | `node:child_process`（`spawn`）、`Buffer` | `spawn` + 管道 |
| `titlebar-spinner.ts` | `node:path`（`basename`）、`process.cwd()`、`setInterval` | — |
| `tool-override.ts` | `fs`、`fs/promises`、`path`、`Buffer.byteLength` | 覆盖内置工具；`DEFAULT_MAX_BYTES` |
| `truncated-tool.ts` | `child_process`（`execSync`）、`node:fs/promises`（`mkdtemp`/`writeFile`）、`os`（`tmpdir`）、`path`、`typebox` | — |

**A 类 53 个**（全部只依赖 API 绑定）：`auto-commit-on-exit` `bookmark` `commands`
`confirm-destructive` `custom-compaction` `custom-footer` `custom-header` `debug-provider`
`dirty-repo-guard` `dynamic-tools` `entry-renderer` `event-bus` `git-checkpoint`
`github-issue-autocomplete` `handoff` `hello` `hidden-thinking-label` `inline-bash`
`input-transform-streaming` `input-transform` `jev-router` `message-renderer` `modal-editor`
`model-status` `overlay-test` `permission-gate` `pirate` `project-trust` `prompt-customizer`
`protected-paths` `qna` `question` `questionnaire` `rainbow-editor` `reload-runtime` `rpc-demo`
`send-user-message` `session-name` `shutdown-command` `snake` `space-invaders` `status-line`
`structured-output` `summarize` `system-prompt-header` `tic-tac-toe` `timed-confirm` `todo`
`tools` `trigger-compact` `widget-placement` `working-indicator` `working-message-test`。

其中值得单独点名的硬约束：

| 文件 | 约束 |
|---|---|
| `snake` / `space-invaders` | `setInterval` 驱动动画 + `ctx.ui.custom` overlay；每帧重渲染（1.34 µs） |
| `tic-tac-toe` | `ctx.sessionManager.getBranch()` 里持久化状态 |
| `todo` | `appendEntry` + `ctx.sessionManager.getBranch()` 恢复 |
| `debug-provider` | `structuredClone(event.data)`（§6.6） |
| `handoff` / `custom-compaction` | `convertToLlm` + `serializeConversation` + `BorderedLoader` |
| `modal-editor` / `rainbow-editor` / `border-status-editor` | `extends CustomEditor` + `super.handleInput`（§5.4） |
| `overlay-qa-tests` | `spawn` + overlay 栈全组合 + `OverlayHandle` 全方法 |
| `timed-confirm` | `AbortController` + `setTimeout(() => controller.abort(), 5000)`（§2.5 的坑） |
| `input-transform` / `entry-renderer` / `message-renderer` / `custom-compaction` | `toLocaleString`/`toLocaleTimeString`（§5.5 覆盖后 A 级） |

### 11.2 目录（9 个）

| 目录 | 判定 | 依据 |
|---|---|---|
| `with-deps/` | **A** | 只多一个 `ms@2.1.3`（纯 CJS，单文件）。`/tmp/ext-proto/src/bin/cjs4.rs` 实测 `ms('5m') = 2000`；`package.json` 有 `kiana.extensions: ["./index.ts"]` |
| `dynamic-resources/` | **A** | `node:path` + `node:url`（`fileURLToPath(import.meta.url)`）+ `resources_discover` 事件；`import.meta` 注入已实测（§3.1） |
| `plan-mode/` | **A** | 2 个文件（`index.ts` + `utils.ts`），只用 kiana 虚拟模块；注册 22 项（`on`/`registerTool`/`registerCommand`/`registerFlag`/`registerShortcut`） |
| `subagent/` | **A（需 shim）** | `spawn` + `readline` + `fs` + `os` + `Buffer` + `process.argv[1]`/`execPath`。**`process.argv[1]` 必须指向 kiana 二进制或不存在**，否则 `getKianaInvocation()` 会拼错子进程命令（§6.2） |
| `custom-provider-gitlab-duo/` | **A（需 shim）** | `fetch` + `crypto.subtle.digest` + `URL` + `URLSearchParams` + `TextEncoder` + `process`；`test.ts` 不是入口（`package.json` 的 `kiana.extensions` 只列 `./index.ts`）。它调 `anthropicMessagesApi().streamSimple`/`openAIResponsesApi().streamSimple`（`kiana-ai` 虚拟模块）⇒ 走 Rust provider，不经 JS SDK |
| `custom-provider-anthropic/` | **A（需 shim + npm）** | `@anthropic-ai/sdk@0.52.0`（**CJS，43 个模块**）+ `fetch`/`Response`/`Headers`/`ReadableStream`/`TextEncoder`/`TextDecoder`/`AbortController`/`FormData`/`Blob`/`URL`/`URLSearchParams`/`process`/`setTimeout`。**SDK 0.52.0 已在 QuickJS 下实测完整跑通**（含 `client.messages.stream()` 的 SSE 全链路），前提是 `setTimeout` 尊重 delay（§2.5）与 `process.versions.node` 存在（§6.2） |
| `sandbox/` | **A（需 shim + npm + 真实 TCP）** | `@anthropic-ai/sandbox-runtime@0.0.26` 的依赖树（`zod@3.25.76`、`commander@12`、`shell-quote@1.12`、`@pondwader/socks5-server@1.0.10`，共 664 个 ESM 模块 / 921 KB）已在 QuickJS 下链接求值通过；它以子进程方式调 `bwrap`/`socat`，**无原生 addon**。需要 **真实的 `node:net`/`node:http` TCP 服务器**（socks5-server 与 http-proxy 会 `listen` 并读 `address().port`）与真 CJS `require`（`shell-quote` 多文件） |
| `doom-overlay/` | **D（部分）** | `doom-engine.ts` 用 `new Function("module","exports","__dirname","__filename","require", doomJsCode)` 载入 Emscripten 产物，并需要 `WebAssembly` + `node:zlib`（`gunzipSync`）+ `createRequire`。**扩展本身可加载**（`on`/`registerCommand` 正常），但**游戏本体不可用**，除非启用 `feature = "webassembly"` 且补 Emscripten 的 `FS_createDataFile` 等 shim |
| `gondolin/` | **D（不可等价）** | `@earendil-works/gondolin@0.14.0` 依赖 `ssh2`（含可选原生 `cpu-features`）、`cbor2`、`undici`，以及外部 `krun-runner` 原生可执行文件。QuickJS 无法加载 `.node` addon；`ssh2` 的纯 JS 路径依赖 `bcrypt-pbkdf`/`asn1` 但仍有原生探测分支。**结论：默认不可用** |

### 11.3 汇总

| 口径 | 数量 |
|---|---|
| 单文件原样可用 | **70 / 70**（A 53 + B 17；B 类依赖 §6 的 shim，已全部在原型验证） |
| 目录原样可用 | **6 / 9**（`with-deps` `dynamic-resources` `plan-mode` `subagent` `custom-provider-gitlab-duo` `custom-provider-anthropic`） |
| 目录"需额外能力" | **2 / 9**（`sandbox`：真实 TCP + 664 模块依赖树，语法与链接层已实测通过；`doom-overlay`：扩展本身可加载，WASM 游戏本体需 `feature = "webassembly"`） |
| 目录"不可能" | **1 / 9**（`gondolin`：原生 addon / 外部 runner） |

### 11.4 "不能等同"的诚实清单

1. **`Intl`**：`Segmenter` 用 UAX#29（`unicode-segmentation`）近似 ICU；`Collator`/`PluralRules`/时区名不实现。
   `toLocaleString` 只覆盖 en-US 常见路径（§5.5）。语料内无差异，语料外不保证。
2. **原生 addon**：`.node` 无法加载；`gondolin` 因此不可用。
3. **`WebAssembly`**：默认无；`wasmtime` 特性提供 `WebAssembly.Module/Instance/instantiate`，
   但 Emscripten 运行时的 `FS_createDataFile`/`process`/`globalThis` 假设仍需额外 shim，
   `doom-overlay` 大概率仍失败。
4. **Node 流语义**：`createReadStream` 的 `highWaterMark`/`pause`/`resume` 与 Node 不完全一致
   （语料只用 `on("data")`/`for await`，不受影响）。
5. **错误文本**：V8 的 `TypeError` 文案（`Cannot read properties of undefined (reading 'x')`）与
   QuickJS 的不同。`ca-extensions.md` §3.5 的"扩展作者可见 TypeError"只能保证语义一致，不能逐字一致。
6. **`Function.prototype.toString`**：QuickJS 的打印格式与 V8 不同；依赖源码分析的库（少见）会有差异。
7. **`process.versions.node = "22.0.0"` 是伪造**：个别包会因此走 Node 专属分支，需按包实测。
8. **`console.log` 走 stderr**（TUI 模式），Node 走 stdout。
9. **`worker_threads`/`node:vm`/`node:cluster`**：不提供（语料不用）。
10. **`process.exit()`** 是协作式退出（抛 `ExitSignal`），不是立即终止。

---

## 12. 风险与缓解

| # | 风险 | 严重度 | 证据 | 缓解 |
|---|---|---|---|---|
| R1 | **装饰器无法解析**：oxc 默认保留 `@dec`，QuickJS 报 `unexpected token in expression: '@'` | 中 | `decrun.rs` | `opts.decorator.legacy = true` + prelude 的 `babelHelpers` 子集（实测可运行）。语料无装饰器，但第三方扩展可能有 |
| R2 | **`Module::namespace()` 在求值前调用 ⇒ 段错误** | 高 | `bcns3.rs`（`SIGSEGV`） | 设计硬规则：只在 `eval()` 之后调 `namespace()`（§3.1）。加一个 debug_assert + 集成测试覆盖 |
| R3 | **`Persistent` 活过 `Runtime` ⇒ 进程 abort** | 高 | `reentrant3.rs` 未清理时 abort、清理后 `exit=0` | `JsEngine::Drop` 的固定顺序（§2.9）；所有 `Persistent` 集中在 `Registry`，不散落 |
| R4 | **`with_js` 嵌套 ⇒ `RefCell already borrowed` panic** | 高 | `nestwith2.rs` 用例 A | 唯一入口 + 宿主函数接收 `Ctx` 参数（§2.2）。代码评审规则：任何 `Function::new` 的回调若需进 JS，必须带 `Ctx<'js>` 参数 |
| R5 | **依赖域带来 `instanceof` 语义差异** | 中 | `globalshare.rs`/`xctx.rs` | typebox/marked 不用 `instanceof`（实测可用）；提供 `dependencyRealm = false` 开关 |
| R6 | **每个 Context 重跑大依赖 ⇒ 41 ms/Context** | 高（性能） | `depcost2.rs`（4 次 42.4/41.1/41.3/40.0 ms） | 依赖域（§3.6），每扩展降到 ~0.9 ms |
| R7 | **`Intl` 缺失导致 kiana-tui 在 JS 里加载即崩** | 高 | `globals.rs`：`Intl === undefined` | 原生组件 + `Intl` shim（§5.5）；kiana-tui 的 `utils.ts` 不搬进 JS |
| R8 | **`setTimeout` 不尊重 delay ⇒ SDK 请求全部自 abort** | 高 | 已记录在 `design-extension-runtime-risk-first.md` §11.3-11 | 定时器由主循环堆驱动，delay 真实（§2.5）；有 SDK 全链路回归测试 |
| R9 | **`child_process` + `stdio:"inherit"` 需要真实终端** | 中 | `interactive-shell.ts` | TUI 暂停 raw mode（§6.3）；失败时降级为 pipe 并提示 |
| R10 | **`sandbox` 需要真实 TCP 服务器** | 中 | `sbprobe` 记录 | `node:net`/`node:http` 用 tokio 真实现；工作量可控 |
| R11 | **长同步 JS 卡住 TUI** | 中 | `limits.rs` 看门狗 100.9 ms 触发 | 中断看门狗（§2.7）+ handler deadline；渲染路径不设 deadline 但组件 render 有 100 ms 硬上限 |
| R12 | **`Module::write` 生成的字节码与 QuickJS 版本绑定** | 低 | `bytecode.rs` | 字节码只在 `build.rs` 生成、不入库；默认不启用预热（prelude 只 32 KB，省不到 3 ms） |
| R13 | **大对象 JSON 往返（20 KB = 162 µs）** | 低 | `jsonbench.rs` | 事件只传必要字段；transcript 按需切片 |
| R14 | **`gondolin` 不可用** | 低（已接受） | 依赖含原生 addon | 明确列为不支持（§11.2） |
| R15 | **`doom-overlay` 的游戏本体不可用** | 低（已接受） | 需 WASM + Emscripten FS | `feature = "webassembly"` + 已知限制说明（§11.4） |

---

## 13. 实施顺序（每步可独立验收）

1. **引擎最小闭环**：`JsEngine`（Runtime/Context/prelude/`with_js`/Drop）+ `hello.ts` 加载并注册
   `hello` 工具。验收：`hello.ts` 的 `registerTool` 被调用，`execute` 从 Rust 调通。
2. **加载器**：Resolver/Loader/oxc strip/`import.meta`/相对路径归一化/22 个虚拟模块（先用空桩）。
   验收：86 个 `.ts` 全部 strip + declare 成功（`perf` 原型已达 86/86 解析）。
3. **手写 Promise + 定时器 + `kiana.exec`**：验收：`timed-confirm.ts`（`setTimeout` + `AbortController`）+
   `file-trigger.ts`（`fs.watch`）跑通。
4. **API 层**：`kiana` 对象 41 事件 + 31 方法 + `ctx` 三态 + `ExtensionUIContext`（先 no-op）。
   验收：`ca-extensions.md` §7.1/§7.2 的单测移植全部通过。
5. **原生组件**：`Text`/`Box`/`Container`/`Spacer`/`SelectList`/`Editor`/`Markdown` + `Theme` + `TUI`
  + overlay 栈。验收：`modal-editor.ts`/`rainbow-editor.ts`/`border-status-editor.ts` 三个子类；
  `overlay-qa-tests.ts` 的 13 个命令（`overlay-animation` … `overlay-streaming`）。
6. **Node 内建**：`fs`/`path`/`os`/`child_process`/`readline`/`util`/`url`/`zlib`/`module` + `Buffer`/`process`/`console`。
   验收：17 个 B 类文件全部跑通。
7. **依赖域 + CJS**：typebox/marked 内嵌、`ms` 与 SDK 0.52.0 加载。验收：`with-deps`、
   `custom-provider-anthropic` 的 `messages.stream()` 全链路。
8. **fetch + Web Streams + crypto**：验收 `custom-provider-gitlab-duo`。
9. **收尾**：`sandbox`（需 `node:net`）、`subagent`、性能回归、内存上限测试。

---

## 14. 验收清单（对齐 `ca-extensions.md` §8.8）

**A. 类型与导出面**
- [ ] `kiana` 对象有 **41 个 `on` 重载**对应的事件名（§4.2 列表逐字）、**31 个非 `on` 方法**、**`events` 字段**。
- [ ] `ExtensionUIContext` 的 **28 个成员**（27 个方法 + 只读 `theme`）全部存在，方法名逐字。
- [ ] `ExtensionContext` 的 **18 个具名成员**全部存在；`ExtensionToolContext`/`ExtensionCommandContext`/
      `ReplacedSessionContext` 的额外成员按 `kind` 暴露。
- [ ] `ToolDefinition` 的 **20 个字段**都能被读取（含 `prepareArguments`/`reachesTools`/`prepareLoadout`）。
- [ ] `ToolRenderContext` 的 **14 个字段**齐全。
- [ ] `@earendil-works/kiana-coding-agent` 的 **34 个运行期导出**与 `@earendil-works/kiana-tui` 的 **19 个运行期导出**全部存在。

**B. 加载器**
- [ ] 22 个虚拟模块键逐字（§3.4），每个都能 `import` 到非空 namespace。
- [ ] 相对路径 `..` 归一化；循环导入；顶层 `await`；`import.meta.url/dirname/filename`。
- [ ] `moduleCache:false` 语义：两次加载同一扩展 ⇒ 模块级副作用执行两次，虚拟模块对象身份不变。
- [ ] 工厂缓存 `(resolvedPath, cwd, generation)`；`clearExtensionCache()` 使 `generation++`。
- [ ] 两条加载错误文案【逐字】（§3.3）；事务性回滚（§4.6）。
- [ ] 发现规则：`package.json` 的 `kiana.extensions` > `index.ts` > `index.js`；`~` 不展开；不递归超过一层。

**C. 事件派发**
- [ ] 12 个专用派发器的合并语义（`ca-extensions.md` §2.3.14）。
- [ ] handler 返回值/`Promise` 的串行 await；错误不中断后续 handler。
- [ ] 退订函数真的移除 handler。
- [ ] `kiana.events` 的同步 `emit` + 异步 handler + `Event handler error (<channel>):`【逐字】。
- [ ] stale ctx 文案【逐字】（§4.4）。

**D. UI**
- [ ] 4 个对话框用例（`ca-extensions.md` §7.4）；`noOpUIContext` 的默认返回值逐字。
- [ ] widget 截断 `"... (widget truncated)"` + `MAX_WIDGET_LINES = 10`【逐字】；`placement` 默认 `aboveEditor`。
- [ ] `CURSOR_MARKER = "\x1b_kiana:c\x07"`【逐字】；`Focusable.focused`；`isFocusable` 鸭子判定。
- [ ] 三个编辑器子类的 `super.handleInput` 链路。
- [ ] overlay 的 9 个 anchor + `nonCapturing` + `OverlayHandle` 全方法。

**E. 兼容语料**
- [ ] 70 个单文件全部加载 + 注册成功；17 个 B 类文件的 Node 依赖行为正确。
- [ ] 6 个目录原样可用；`sandbox`/`doom-overlay`/`gondolin` 的不支持点有明确诊断。
- [ ] `toLocaleString` 输出与 Node en-US 逐字比对（5 个样本）。

**F. 健壮性**
- [ ] 内存上限、栈上限、中断看门狗（§2.7 的 4 个用例）。
- [ ] `Persistent` 清理：100 次 reload 后 `rt.memory_usage()` 不增长（无泄漏）。
- [ ] 渲染抛错的降级（§5.6）。
- [ ] 未处理拒绝被 `set_host_promise_rejection_tracker` 捕获（§2.4）。

---

## 附录 A：原型与复现

全部在 `/tmp`（**不进入 `kiana`**）：

```
/tmp/ext-proto/                    主原型 crate（rquickjs 0.14.0 + oxc 0.155.0 + tokio 1.54.1）
  src/bin/tsstrip.rs               86 文件批量 strip
  src/bin/tsparse.rs               TS 语法边界（enum/namespace/参数属性/satisfies/as const/using）
  src/bin/devdec.rs  dechelper.rs  装饰器 legacy 变换 + babelHelpers 运行
  src/bin/perf.rs  bench2.rs  deprealm2.rs  depcost2.rs    加载与依赖域性能
  src/bin/bench.rs  callbench.rs  jsonbench.rs  prelude2.rs  bytecode.rs   微基准
  src/bin/mem.rs  dropctx.rs  ctxcost2.rs                  内存
  src/bin/thisclass.rs  bridge.rs  bridge2.rs  mutclass.rs  原生类 + This 纪律
  src/bin/asynchost.rs  reentrant3.rs  syncloop.rs  synctla.rs  syncload.rs  手写 Promise / TLA / 重入
  src/bin/reenter2.rs  lockacross.rs  lockhold.rs  asynclock.rs  AsyncRuntime 锁行为
  src/bin/bcwrite*.rs  bceval.rs  bcns3.rs                 字节码与 namespace 段错误
  src/bin/cjs4.rs  cjs*.rs                                 CJS 循环依赖
  src/bin/globals.rs  syntax2.rs                           QuickJS 全局与语法能力
  src/bin/xthread2.rs  syncall.rs  tuiloop2.rs             渲染路径
  src/bin/xctx*.rs  globalshare.rs                         Context 隔离
  src/bin/decrun.rs  limits.rs  getter.rs  syms.rs  nestwith2.rs
  src/bin/stripbench.rs            纯 strip 计时（预热 + 5 轮取最优）
  src/bin/modsizes.rs              Module::declare + eval 按源码大小的耗时
/tmp/tbtest/                       typebox@1.3.27（370 模块 / 486 KiB）+ marked@18.0.11（42.8 KiB）
/tmp/sdk/v052/                     @anthropic-ai/sdk@0.52.0（CJS，43 模块）
/tmp/sandboxprobe/                 @anthropic-ai/sandbox-runtime@0.0.26 依赖树
/tmp/featmin/                      rquickjs 最小特性验证（1,578,480 B stripped = 1.58 MB）
```

主要复现命令（`EXT` 指 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/examples/extensions`）：

```bash
source ~/.cargo/env
cd /tmp/ext-proto
cargo build --release

# 1) strip 全量：140 µs/文件，86 文件 12.1 ms
./target/release/stripbench $(find $EXT -name '*.ts' -not -path '*/doom/build/*')

# 2) 加载与依赖域：deprealm2 47 ms 一次 + 每扩展 0.85–1.07 ms；depcost2 41 ms/Context
./target/release/deprealm2
./target/release/depcost2

# 2b) 按源码大小的 declare+eval：0.6 KB=26 µs … 40 KB=2.01 ms（先 strip 出 .mjs）
./target/release/tsstrip print $EXT/overlay-qa-tests.ts | tail -n +2 > /tmp/overlay.mjs
./target/release/modsizes /tmp/overlay.mjs

# 3) 微基准：87 ns/call、45/36 ns get/set、1.80 µs/eval、JSON 1/5/20 KB
./target/release/callbench
./target/release/jsonbench

# 4) 原生类 + This 纪律（super.render / super.handleInput / Class::from_object）
./target/release/thisclass

# 5) 手写 Promise：TLA、await 期间渲染、JS→宿主→JS 重入
./target/release/synctla
./target/release/syncloop
./target/release/reentrant3

# 6) 字节码：105 ms 编译 / 3.0 ms Module::load（400 KB 源）
./target/release/bytecode

# 7) QuickJS 能力探针：Intl/TextEncoder/URL/Buffer/process 全部 undefined
./target/release/globals
```

语料统计脚本（Python，`-I` 隔离）见本设计 §11 的分类口径：按 `from "..."` / `require("...")` 抽取
说明符，区分 `node:`、裸内建、`process.`、裸 npm，产出 A/B/C/D 分级。

## 附录 B：与 `ca-extensions.md` 的差异与更新

| `ca-extensions.md` | 本文 | 原因 |
|---|---|---|
| §6.4 建议 `rquickjs = "0.9"` | **0.14.0** | 0.9 已过时；0.14.0 内置 QuickJS-NG 0.16.2 |
| §8.3 方案 A（rquickjs + 自建转换）关键词"JS 侧实现 loader/runner" | **Rust 原生绑定**（loader/runner/API 全在 Rust），JS 只剩 prelude | 原生化后单帧 1.34 µs；且避免"锁在 await 点释放/保持"的不可推理契约（§2.8） |
| §8.4 未指定线程模型 | **全主线程**（tokio current_thread + LocalSet） | 见 §2.1 对比表 |
| — | 新增 **依赖域**（§3.6） | 每 Context 重跑 typebox+marked = 41 ms，不可接受 |
| — | 新增 **`Intl` shim + `toLocaleString` 覆盖**（§5.5） | QuickJS 无 `Intl`；5 个示例依赖 `toLocale*` |
| — | 新增 **legacy decorator + `babelHelpers`**（§3.2） | oxc 默认保留装饰器，QuickJS 不能解析 |
