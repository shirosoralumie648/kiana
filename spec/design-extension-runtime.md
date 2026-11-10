# 扩展运行时最终设计：rquickjs + Rust 原生组件 + JS 语义层

本文是三份候选设计的合成与定稿，是 `ca-extensions.md` §8.3「扩展执行引擎」的落地设计：

- 候选：`design-extension-runtime-risk-first.md`（下称 RF）、`design-extension-runtime-rquickjs-native-bindings.md`（NB）、
  `design-extension-runtime-js-shim-heavy.md`（JS）。
- 目标不变：**在 Rust 单静态二进制内原样运行 `packages/coding-agent/examples/extensions/**` 的 TypeScript 扩展**，
  `ExtensionAPI` / `ExtensionContext` / `ExtensionUIContext` / `ToolDefinition` / 41 个事件与
  `packages/coding-agent/src/core/extensions/types.ts` 逐字一致；事件合并规则、错误文案、UI 对话框语义
  以 `ca-extensions.md` §2/§3/§8.4–§8.6 为准。
- 本文只决定引擎、线程与事件循环、模块加载、API 绑定形态、TUI 互操作、Node 兼容面、locale、测试与实施顺序。
  所有数字都是本轮 `/tmp` 原型实测（release、Linux x86-64、rustc 1.99.1）；原型不进入 `kiana`。

---

## 0. 评审结论与最终决策

### 0.1 三案评分

评分口径：兼容性 = 不改一行 TS 能跑多少语料；保真度 = 事件/合并/文案/渲染字节的可观察等价度；
实现风险 = 分数越高风险越低；性能 = 热路径与启动；可维护性 = 概念数与长期演进成本。

| 设计 | 兼容性 | 保真度 | 实现风险 | 性能 | 可维护性 | 均分 | 名次 |
|---|---|---|---|---|---|---|---|
| risk-first（RF） | 8.5 | 8.0 | 7.0 | 8.0 | 7.5 | **7.8** | 1 |
| rquickjs-native-bindings（NB） | 8.0 | 8.5 | 5.0 | 9.5 | 6.5 | 7.5 | 2 |
| js-shim-heavy（JS） | 8.0 | 9.0 | 6.0 | 6.5 | 5.5 | 7.0 | 3 |

判定理由（各一句）：

- **RF 胜出**：风险登记册驱动、真 CJS `require`（`/tmp/sbprobe` 的 664 模块依赖图与 SDK SSE 全链路）、
  `?v=` 去模块缓存的 `moduleCache:false` 等价、诚实的 A/B/C/D 兼容分级、以及最细的 npm 生态证据。
  它把「引擎不是风险、兼容面才是风险」说对了。
- **NB 次之**：单线程 `with_js`、手写 Promise、`This` 纪律、关闭顺序、原生类可被 JS `extends` 这些工程结论
  最硬（本轮全部复测通过），但把 41 个事件、上下文、UI 全部做成 Rust 原生绑定，绑定面过大；
  每扩展一个 Context + 依赖域（dependency realm）是为规避 41 ms 重求值而引入的额外机制，在单 Context 下不必要。
- **JS 第三**：把派发/合并语义留在 JS、用 kiana-tui 源码跑出字节级夹具、指出「Intl 必须先于任何模块安装」，
  这三条是全轮最有价值的洞察；但让 301 KB 的 kiana-tui 与 Intl polyfill 跑在 QuickJS 里换来 10–20× 的
  宽度/截断退化与 47–75 ms 启动，且与 `tui-core.md` 已定的 Rust 组件树形成两套实现，长期不可维护。

### 0.2 最终架构一句话

**rquickjs 0.14.0 同步 Runtime；TUI、JS 引擎、tokio 全在同一个主线程（单线程所有权）；扩展 API 与事件派发语义
用 JS 写（逐行移植 `loader.ts`/`runner.ts`），组件与主题用 Rust 原生类，模块加载用 oxc + 真 CJS `require` +
`?v=` 去缓存，typebox 用 npm 真包。**

### 0.3 采纳 / 拒绝清单

| 来源 | 采纳 | 理由 |
|---|---|---|
| RF | 风险登记册与 A/B/C/D 口径；真 CJS `require`（IIFE + 预登记 `module.exports`）；`?v=<gen>-<seq>` 去缓存；`import.meta.url` 用真实路径；Node 内建清单；§14.4 的三条 npm 回归；`sandbox`/`doom`/`gondolin` 判定 | 全部有原型支撑，且是兼容面的主体 |
| RF | §7.1 原则：JS 里必须是真对象/真函数的东西一律 JS 实现，Rust 只给 `__kiana_host.*` 原语 | 与 JS 案的「语义留在 JS」合流 |
| NB | **单线程所有权**（TUI + JS + tokio `current_thread` + `LocalSet`），唯一入口 `with_js`，禁止嵌套；宿主异步不得阻塞 `with_js` | 见 §2.0：跨线程渲染有死锁面且慢 10–20× |
| NB | 手写 Promise 协议（JS 建 Promise，Rust 存 waiter）；定时器堆；AbortSignal shim；内存/栈/中断限制数值；`Persistent` 逆序关闭 | 本轮全部复测通过，包含会 abort 进程的关闭顺序 |
| NB | `This` 纪律；原生构造函数不能装 JS 字段（本轮新证）；`#[qjs(get)]`/`#[qjs(set)]` 访问器 | 见 §5.2、§5.3 |
| NB | typebox 用 **npm 真包**（1.3.27），而不是手写 builder | `Compile` 的错误文案与 `Symbol.for("TypeBox.Kind")` 无法手写对齐；实测 287 导出可用、JIT 可用 |
| JS | **派发与合并语义留在 JS**：把 `runner.ts` 的 13 个 emitter 逐行移植到 `js/dispatch.js` | 引用相等、`undefined` 键存在性、原地修改、`sameMessages` 都在 JS 里自然成立 |
| JS | 单 Context 的理由（`Symbol.for`、`instanceof`、共享虚拟模块对象） | 与 Node 同 realm 语义一致；顺带消掉依赖域 |
| JS | 字节级 TUI 夹具（6 个样本，含 ANSI/CJK/emoji ZWJ/tab）；「Intl 必须先于任何触及它的模块安装」 | 作为原生组件的验收基线 |
| JS | `Function::call` 的更正（有 `this`，但必须显式传） | 已在 §0.4 记录 |
| 拒绝 | RF 的独立 JS 线程 + 跨线程同步桥 | §2.0 三条证据 |
| 拒绝 | NB 的每扩展一个 Context + 依赖域 | 单 Context 下 typebox 只求值一次；多 Context 反而破坏 `instanceof` 与 theme 的 `Symbol.for` 单例 |
| 拒绝 | NB 的「41 个事件 + 31 方法 + UI 全部 Rust 原生对象」 | 绑定面与文案维护成本翻倍，且事件对象要多一次跨语言物化 |
| 拒绝 | JS 的「kiana-tui 全量 JS + esbuild 构建期打包 + 字节码预热」 | 双实现、10–20× 热路径退化、构建期依赖 Node/esbuild |
| 拒绝 | RF/NB/JS 共同假设的「Node locale 固定 en-US」 | 实测 Node 取环境 locale（见 §6.4），必须按 locale 策略实现 |

### 0.4 对三案的事实更正（本轮复测）

| 三案说法 | 实测 | 影响 |
|---|---|---|
| RF §2.2 与 NB §1.2 的 `reqwest features = ["rustls-tls"]` | 0.13.5 **没有** `rustls-tls`，只有 `rustls` / `rustls-no-provider` | 照抄会编译失败；本文用 `rustls` |
| JS 案称「`Function::call` 没有 `this`」 | `This` 存在于 `rquickjs::function`；`f.call((This(obj), args))` 正确派发到 JS 覆写（输出 `OK native-top(10)+ · sub(native-top(20))+red · native-top(30)+ · true`） | 该说法错误；`This` 纪律是必做项 |
| RF §3 称 `Persistent` 可跨线程保存/恢复 | `Persistent<T>` 是 `!Send`（编译错误 ``*mut JSRuntime` cannot be sent between threads``）；`restore` 还会校验 runtime 指针 | 跨线程方案只能用 id + 通道；本文单线程后无此问题 |
| 三案均未提 | **原生 `#[qjs(constructor)]` 里通过 `This<Object>` 设置的属性不会留在实例上**（实测 `ownKeys` 为空、`peek=MISSING`）；从普通原生方法设置则保留 | 原生类不得在构造期写 JS 字段；需要字段的用访问器或 JS 包装子类（§5.3、§5.4） |
| 三案均假设 Node 的 `toLocaleString` 是 en-US 固定格式 | Node 取 `LC_ALL > LANG`（`LANGUAGE` 不生效），本机默认 `zh-CN`：`2024/1/2 03:04:05`；de-DE `2.1.2024, 03:04:05` / `1.234.567,5`；ja-JP `2024/1/2 3:04:05` | locale 必须成为设计项（§6.4） |
| JS §10.3 称「Rust 字素边界服务把截断从 4392 ms 降到 2411 ms」 | 该数字是 JS polyfill 内下沉边界的收益；在本文架构里 kiana-tui 不在 JS 中运行，收益无意义 | 不作为选型依据 |
| RF §12.1「27.0 µs/帧」 vs 本轮复测 | 32.5 µs 平均 / 314.8 µs 最坏（600 帧），同机抖动 | 仍是 16.7 ms 预算的 2%–9.4%（30 个组件时），但见 §2.0 的死锁面 |
| RF §12.1「全量去类型 46.8 ms」 vs NB「12.1 ms」 | 同一任务两种实现口径；本轮 `stripbench` 复测 140 µs/文件 × 86 = 12.16 ms，本文取本轮值 | 46.8 ms 与 NB 的「依赖域加载 46.8 ms」是不同实验的巧合同值，不要混用 |
| RF 的依赖域含 `marked` | 本设计不跑 kiana-tui 的 Markdown 源码，`marked` 无需在 JS 侧求值 | typebox 单独求值（实测 typebox+marked 合计 46.8 ms） |
| NB §1.2 写 `classes` 特性 | 0.14.0 中 `classes`/`array-buffer`/`properties`/`allocator` 已是空特性并有 `#[deprecated]`（`rquickjs-core/src/lib.rs:107–146`） | 不要写 |

---

## 1. 引擎与依赖

### 1.1 引擎选型（核对后）

| 候选 | 版本 | 单二进制 | ESM 加载 | 结论 |
|---|---|---|---|---|
| **rquickjs** | 0.14.0（内置 quickjs-ng 0.16.2，`QJS_VERSION 0/16/2`） | 是 | 原生 `Module`/`Resolver`/`Loader`/`import.meta`/TLA/循环导入 | 选用 |
| boa_engine | 0.22.0 | 是 | 需自建 ESM 加载器与 `import.meta` | 不选 |
| deno_core + v8 | 0.413.0 | 是 | 最全 | 体积/构建不可接受 |
| quickjs-wasi | npm 3.6.2 | 需 WASI 运行时 | 无 `node:fs` | 留给 codemode |

### 1.2 Cargo 依赖（精确版本，均已 `cargo info` 核对）

```toml
# crates/kiana-extensions/Cargo.toml（节选）
[dependencies]
# ---- JS 引擎：只要 std/loader/macro ----
rquickjs = { version = "=0.14.0", default-features = false, features = ["std", "loader", "macro"] }
#  不开 full-async（AsyncRuntime/async-lock/futures 均不需要，手写 Promise 协议）；
#  不开 parallel（会用 unsafe impl Send/Sync 并引入多线程假设）；
#  classes/array-buffer/properties/allocator 在 0.14.0 是空特性，不写。
#  实测该组合可编译并覆盖本设计全部用法（Runtime/Context、set_loader、limits、interrupt、
#  4 参数 rejection tracker、#[rquickjs::class]、Class::create_constructor、Function::new、
#  Promise::new/state/result/finish、Ctx::execute_pending_job），二进制 1,746,544 B。

# ---- TS 去类型 ----
oxc_allocator   = "=0.155.0"    # MSRV 1.97.0
oxc_ast         = "=0.155.0"
oxc_parser      = "=0.155.0"
oxc_semantic    = "=0.155.0"
oxc_transformer = "=0.155.0"
oxc_codegen     = "=0.155.0"
oxc_span        = "=0.155.0"

# ---- npm 解析 ----
oxc_resolver = "=11.24.3"       # MSRV 1.95.0；exports/main/#imports/node_modules 逐级

# ---- 宿主 / 异步（单线程）----
tokio        = { version = "=1.54.1", features = ["rt", "macros", "sync", "time", "process", "io-util", "fs", "net", "signal"] }
tokio-util   = "=0.7.20"        # CancellationToken
futures      = "=0.3.34"        # LocalSet / block_on 胶水（不进入 JS 引擎特性）

# ---- 数据 ----
serde      = { version = "=1.0.229", features = ["derive"] }
serde_json = { version = "=1.0.154", features = ["preserve_order", "raw_value"] }

# ---- Node 内建与全局 ----
reqwest      = { version = "=0.13.5", default-features = false, features = ["rustls", "stream", "json", "gzip", "deflate", "brotli"] }
url          = "=2.5.8"
percent-encoding = "=2.3.2"
base64       = "=0.23.1"
sha2         = "=0.11.1"
getrandom    = "=0.4.3"
uuid         = { version = "=1.29.0", features = ["v4", "v7"] }
flate2       = "=1.1.10"
notify       = "=8.2.1"         # fs.watch（9.0.0-rc.6 为预发布，不用）
unicode-width        = "=0.2.2"
unicode-segmentation = "=1.13.3" # Intl.Segmenter shim / UAX#29

# ---- locale（默认开启；--no-default-features 可裁掉，见 §6.4）----
icu_decimal  = { version = "=2.4.0", optional = true, default-features = false, features = ["compiled_data", "alloc"] }
icu_datetime = { version = "=2.4.0", optional = true, default-features = false, features = ["compiled_data"] }
icu_time     = { version = "=2.4.0", optional = true, default-features = false, features = ["compiled_data"] }
icu_locale   = { version = "=2.4.0", optional = true }

# ---- 工具 ----
thiserror = "=2.0.21"
tracing   = "=0.1.44"
regex     = "=1.13.1"

# ---- 可选 ----
wasmtime = { version = "=50.0.0", optional = true }   # feature = "webassembly"（doom-overlay）

[features]
default = ["locale-data"]
locale-data = ["dep:icu_decimal", "dep:icu_datetime", "dep:icu_time", "dep:icu_locale"]
webassembly = ["dep:wasmtime"]
```

工作区 MSRV 定 **1.97**（oxc 0.155 要求；rquickjs 1.87、oxc_resolver 1.95、tokio/reqwest/notify 1.85）。
**不引入** `icu_segmenter`：kiana-tui 的组件在 Rust 侧，字素/词边界由 `unicode-segmentation` + 夹具校准；
CJK 词典数据（数 MB）没有收益。

### 1.3 体积与启动

| 组合 | stripped 体积 | 来源 |
|---|---|---|
| rquickjs 最小特性（std/loader/macro） | 1,746,544 B（1.75 MB） | `/tmp/minfeat` 本轮 |
| rquickjs full-async 对照 | 1,943,184 B | 同上 |
| rquickjs + oxc 全套 | 3,085,248 B（3.09 MB） | NB `/tmp/ext-proto` |
| rquickjs + oxc + tokio + LTO/strip/1 CGU | 3.94 MB | RF `/tmp/combined` |
| 再加 ICU4X 日期+数字（实测） | 空程序 288,456 B -> 1,486,264 B（+1.20 MB） | `/tmp/icumin` 本轮：`icu_datetime` + `icu_decimal`，`default-features=false` + `compiled_data`，输出 `1/2/2024, 3:04:05 AM` 与 `1,234,567.5` |
| 目标 | **≤ 5.2 MB** | 上表合计 |

启动（均为实测）：单扩展 `strip + declare + eval + factory` 0.2–1.6 ms；86 文件纯 strip 12.1 ms；
typebox+marked 一次求值 46.8 ms（本文只求值 typebox，数值应更低，实施第 1 步实测）；
prelude 合计约 92 KB，`Module::declare` 约 11 ms（标定：40 KB = 3.4 ms、100 KB = 12.2 ms、400 KB = 103.6 ms，即 78 ns/byte 起步、大文件超线性），不需要字节码预热。

---

## 2. 执行模型：单线程所有权

### 2.0 为什么不用独立 JS 线程（对 RF 的唯一定向偏离）

RF 的核心是「主线程 TUI + 专用 JS 线程 + 同步桥」，本轮复测后放弃，三条证据：

1. **`Persistent` 不能跨线程**。`Persistent<Function>` 编译即报 ``*mut JSRuntime` cannot be sent between threads``。
   跨线程方案只能传 `u64` id + 通道，JS 侧状态全部留在 JS 线程；宿主侧对象（session、tools、theme）
   要么镜像一份，要么每次访问一次同步往返。
2. **渲染期的宿主访问会死锁**。`examples/extensions/custom-footer.ts:35,50` 的 `render(width)` 内部直接调用
   `ctx.sessionManager.getBranch()` 与 `ctx.model`（`todo.ts` 的 `renderResult` 也读 `ctx` 闭包捕获的会话状态）。
   RF 的渲染路径是「主线程 `SyncSender` 等 JS 线程返回」；此时 JS 若再向主线程要 session 数据，
   就是一个必须额外实现「渲染期服务循环」的死锁面。RF 原文没有展开这一层，而它是每帧都会走的路。
3. **代价与收益倒挂**。同线程渲染实测 1.34 µs/帧（NB）/ 2.9 µs（RF 本地对照）；跨线程 27.0–32.5 µs 平均、
   最坏 280–315 µs。`overlay-qa-tests.ts` 一帧可含 30+ 组件；按最坏值算 30 × 314.8 µs = 9.4 ms，
   已逼近 16.7 ms 帧预算，而 TUI 的其余工作还要在同一帧完成。

同时，`tui-core.md` §8.2 已规定 `TuiBase` 单线程持有 `Vec<Box<dyn Component>>`、`Component: !Send`。
单线程是既定架构，不是新增假设。

**代价（必须承认）**：JS 里的死循环会冻结 UI，直到 100 ms 看门狗触发 `Error: interrupted`；
JS 里一次很慢的同步 `fs.readFileSync` 也会占住一帧。前者由 `set_interrupt_handler` 兜底，
后者属于扩展自己的选择（Node 里同样阻塞事件循环）。

### 2.1 主循环

```text
主线程（tokio current_thread + LocalSet）
  ExtLoop::run():
    loop {
      select! {
        // 1) TUI 输入（crossterm 事件）/ 渲染 tick（16 ms 节流）
        // 2) 定时器堆到点 → __kiana.timerFire(id)
        // 3) 宿主完成队列（tokio task 完成、进程退出、HTTP chunk）→ __kiana.settle / pushChunk
        // 4) 扩展加载队列（每次处理一个文件，处理完 yield 一次，保证帧不饿死）
      }
      pump_jobs();                 // while ctx.execute_pending_job() {}
      rt.run_gc_if_needed();       // 在 ctx.with 之外
      if dirty && frame_due() { tui.render(); }
    }
```

- `TuiBase`、`JsEngine`、`tokio::task::LocalSet` 全部在同一线程；**没有** `Arc<Mutex>`、通道、`Send` 约束。
- 所有宿主异步（`kiana.exec`、`fetch`、`spawn`、`net` 服务器、定时器）都是 `spawn_local` + 完成队列，
  JS 侧拿到的是 Promise；JS 永不阻塞等待 I/O。
- 同步宿主函数（`ctx.cwd`、`theme.fg`、`fs.readFileSync`、`path`、`Buffer`）直接在调用栈里完成。
- 单次 `with_js` 目标 < 100 µs；扩展加载按文件分片，每片之后 `yield_now`。

### 2.2 唯一入口 `with_js`（NB §2.2，实测）

```rust
pub struct JsEngine {
    rt: rquickjs::Runtime,
    ctx: rquickjs::Context,
    pending: HashMap<u64, PendingPromise>,          // 全部 Persistent，Drop 顺序见 §2.8
    handles: SlotMap<JsHandleId, Persistent<Object<'static>>>,
    interrupted: Arc<AtomicBool>,
    deadline_ms: Arc<AtomicU64>,
}

impl JsEngine {
    /// 唯一允许进入 JS 的函数。同步；绝不跨 await 持有。
    pub fn with_js<R>(&self, f: impl for<'js> FnOnce(Ctx<'js>) -> R) -> R { self.ctx.with(f) }
}
```

硬规则（均有原型）：

1. **不得在 `with_js` 内嵌套 `with_js`**：`Context::with` 是 `RefCell`，嵌套会
   `panicked at safe_ref.rs:35: RefCell already borrowed`。唯一合法重入是宿主函数签名里带 `Ctx<'js>`
   （此时可在函数体内继续 `eval`/`execute_pending_job`）。所以所有会被 JS 调用的宿主函数都接收 `Ctx`，
   不回查 `JsEngine`。
2. 宿主异步不得阻塞 `with_js`：返回 Promise 立刻返回，等待发生在主循环。
3. `rt.run_gc()` **必须在 `ctx.with` 之外**调用，否则 `RefCell already borrowed`。
4. `Value<'js>` 不得逃出闭包；出闭包前转成 Rust 拥有类型或 `Persistent`。

### 2.3 手写 Promise 协议（NB §2.3，本轮复测）

```js
// js/prelude.js —— 宿主异步的唯一入口
globalThis.__kiana = {
  seq: 0,
  waiters: new Map(),
  hostStart: null,                       // Rust 注入：(id, kind, payload) => void
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
    ok ? w.resolve(value) : w.reject(value);
    return true;
  },
};
```

```rust
// 宿主函数带 Ctx，可同步解析参数并抛 JS 异常
fn host_start<'js>(ctx: Ctx<'js>, id: u64, kind: String, payload: Value<'js>) -> Result<()> {
    match kind.as_str() {
        "exec"  => { let (cmd, args, opts) = parse_exec(&ctx, &payload)?;      // 先转成 Rust 类型
                     let tx = HOST_TX.with(|t| t.clone());
                     tokio::task::spawn_local(async move {
                         let r = run_exec(cmd, args, opts).await;
                         let _ = tx.send(HostDone::Settle { id, ok: r.is_ok(), value: r });
                     }); }
        "fetch" => { /* 同上；响应块走 HostDone::StreamChunk */ }
        "spawn" => { /* 同上 */ }
        _ => return Err(Exception::throw_message(&ctx, &format!("unknown host kind: {kind}"))),
    }
    Ok(())
}
```

完成事件在主循环里回填：`__kiana.settle(id, ok, value)` 之后立刻 `pump_jobs()`。
实测覆盖：顶层 `await`（同步 Runtime 下 2 轮 Resolved）、handler 挂起 43 ms 期间照常渲染、
JS→宿主→JS 深度 4 递归。

### 2.4 job 泵与未处理拒绝

```rust
fn pump_jobs(ctx: &Ctx<'_>) -> usize { let mut n = 0; while ctx.execute_pending_job() { n += 1; } n }
```

- `Ctx::execute_pending_job` **吞掉 job 抛出的错误**（实测 `Promise.resolve().then(() => { throw ... })`
  泵完无 panic、无错误返回）。所以**必须**安装 `set_host_promise_rejection_tracker`
  （签名 `Box<dyn for<'a> Fn(Ctx<'a>, Value<'a>, Value<'a>, bool) + 'static>`，4 参数），
  映射为扩展错误（`ca-extensions.md` §3.8 三种落点）。
- 每次事务结束时泵一次；不要在长循环里反复泵（单次固定开销 ~0.4 µs）。

### 2.5 定时器与 `queueMicrotask`

- `setTimeout/setInterval/clearTimeout/clearInterval`：prelude 维护 `Map<id, deadline>`，调用
  `__kiana.timerStart(id, dueMs)` / `__kiana.timerCancel(id)`；主循环维护最小堆，`select!` 的等待用
  `tokio::time::sleep_until(next)`，到点 `__kiana.timerFire(id)`。
- **`setTimeout` 必须真的等 delay**。`/tmp/sbprobe` 的真实事故：`@anthropic-ai/sdk` 的
  `fetchWithTimeout` 用 `setTimeout(() => controller.abort(), ms)`；shim 若立即执行，每个请求都会被自己
  abort 成 `APIUserAbortError("Request was aborted.")`，而错误栈里看不到定时器。回归断言见 §10.4。
- `clearTimeout` 必须真正移除；`setInterval` 不要求无漂移，但上一拍未完成时不得堆积（同 key 跳过）。
- `queueMicrotask` QuickJS 原生提供，直接用；`ui_prompt_start/end` 在 JS 侧用原生 `queueMicrotask`
  发出（等价 `runner.ts` 的延后到微任务末尾）。
- `setImmediate`/`process.nextTick` 映射到 `queueMicrotask`（TUI 调度用 Rust 队列，不经过 JS）。

### 2.6 AbortSignal / AbortController

prelude 纯 JS 类（约 80 行）：`aborted`、`reason`、`throwIfAborted()`、`addEventListener/removeEventListener`、
`AbortSignal.any()`、`AbortSignal.timeout()`；**abort 之后注册的监听器必须立刻（微任务）触发**
（`/tmp/stream_probe` 初版漏掉此条导致挂起）。Rust 侧用 `CancellationToken`：`ctx.signal` 由 Rust 传入，
中止时调用已保存的 `abort` 函数（`Persistent<Function>`）。

### 2.7 限制与看门狗（数值实测）

```rust
rt.set_memory_limit(512 * 1024 * 1024);     // 可配；OOM -> Error: out of memory
rt.set_max_stack_size(4 * 1024 * 1024);     // 无限递归 -> Error: Maximum call stack size exceeded
rt.set_interrupt_handler(Some(Box::new(move || interrupted.load(Relaxed)
    || (deadline != 0 && now_ms() > deadline))));
```

| 场景 | 实测 |
|---|---|
| `for(;;){}` | `Error: interrupted`，100.9 ms 触发（阈值 100 ms） |
| 内存耗尽 | `Error: out of memory`（可被 JS catch） |
| 未处理拒绝 | tracker 回调被调用 |

规则：handler 派发 30 s deadline、factory 10 s、渲染不设 deadline；中断处理器只能读原子变量，**不得调用 JS**。
中断后映射为扩展错误：交互式 `Extension "<path>" error: <error>`，print/json `Extension error (<path>): <error>`
（【逐字】，§4.9）。

### 2.8 关闭与代际重载（会 abort 进程的硬约束）

```rust
impl Drop for JsEngine {
    fn drop(&mut self) {
        self.pending.clear();                    // 1. 清所有 Persistent（waiters/handles/回调/theme）
        self.handles.clear();
        self.registry.clear_persistents();
        self.with_js(|ctx| { while ctx.execute_pending_job() {} });   // 2. 断引用链
        rquickjs::Runtime::run_gc(&self.rt);     // 3. 必须在 with 之外
        // 4. 字段顺序 rt 在前、ctx 在后 => Drop 逆序，ctx 先于 rt
    }
}
```

实测：任何一个 `Persistent` 活过 Runtime 会触发
`JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed` 并 **abort 进程**（清理后 exit=0）。
`Persistent::save` 不增加引用计数之外的 dup；`Clone` 不 dup；`restore` 校验 runtime 指针
（`Error::UnrelatedRuntime`）。

代际：`/reload` 或会话替换 = 整代销毁再建。旧代 JS 侧的 `kiana`/`ctx` 句柄必须失效：
`invalidate()` 置 `Failed` 并抛出 stale 文案（§4.6）。模块名里的 `<gen>` 递增。

### 2.9 扩展组件与 TUI 的接线

```rust
// crates/kiana-extensions/src/ui/adapter.rs
pub struct JsComponent {
    handle: JsHandleId,          // Persistent<Object> 槽位
    engine: Weak<JsEngine>,      // 同线程，仅用于 with_js
    last_lines: Vec<String>,     // 渲染失败的兜底
}

impl tui_core::Component for JsComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let engine = self.engine.upgrade().expect("engine alive");
        engine.with_js(|ctx| {
            let obj = self.handle.restore(&ctx)?;
            let f: Function = obj.get("render")?;
            f.call((This(obj.clone()), width))       // This 必传（§5.2）
        }).unwrap_or_else(|e| { self.report(e); self.last_lines.clone() })
    }
    fn handle_input(&mut self, data: &str) { /* 同上；调 handleInput */ }
    fn invalidate(&mut self) { /* 调 invalidate?.() */ }
    fn wants_key_release(&self) -> bool { /* 读 wantsKeyRelease */ }
}
```

- 组件在渲染中调用 `tui.requestRender()` 只置 dirty 标志，**不得递归渲染**（与 `tui-core.md` 的
  `requestRender(force)` 语义一致）。
- 渲染失败按 `ca-extensions.md` §3.6 上报，返回上一次成功的行（首次失败返回 `[]`），同一组件同一错误去重。
- `dispose()` 在对话框/overlay 关闭时调用，并释放对应 `Persistent`。

---

## 3. 模块加载：TS / ESM / CJS / 虚拟模块 / npm

### 3.1 流水线

```text
specifier ──Resolver──▶ 绝对路径 或 虚拟键 或 node:<name>
             │
             ▼
        Loader::load(name)
             ├─ 虚拟键（22 个）        → 预置 JS 源 / 嵌入的 npm 文件 → Module::declare
             ├─ .ts/.tsx/.mts/.cts    → oxc 去类型（§3.2）→ Module::declare
             ├─ .js/.mjs              → 原文 → Module::declare（ESM）
             ├─ .cjs 或 CJS 探测命中   → 真 CJS 加载器（§3.5）
             └─ node:<name>           → Node 内建 shim 模块（§6）
```

`Resolver` 的 `base` 是导入者模块名（实测 `base="dep"`；文件模块即传给 `Module::declare` 的名字）。
本设计对**文件模块**统一用「真实绝对路径 + `?v=`」作为模块名（§3.4），所以 `base` 可能是 `.../index.ts?v=3-7`，
取 `dirname` 前先 `split('?')`。

### 3.2 oxc 去类型（与 jiti/Babel 路径等价）

```rust
// crates/kiana-extensions/src/loader/transpile.rs —— 与 /tmp/tsprobe2、/tmp/ext-proto tsstrip 一致
let alloc = Allocator::default();
let st = SourceType::from_path(Path::new(name)).unwrap_or_else(|_| SourceType::ts());
let ret = Parser::new(&alloc, src, st).with_options(ParseOptions::default()).parse();
if !ret.diagnostics.is_empty() {
    return Err(LoadError::Parse { path: name.into(), diagnostics: ret.diagnostics });
}
let mut program = ret.program;
// 必须 with_enum_eval(true)，否则 Transformer 在 oxc 0.155 会 panic（硬约束）
let scoping = SemanticBuilder::new().with_enum_eval(true).build(&program).semantic.into_scoping();
let mut options = TransformOptions::default();
options.decorator.legacy = true;                       // jiti/Babel 走 legacy 装饰器
let tr = Transformer::new(&alloc, Path::new(name), &options);
let _ = tr.build_with_scoping(scoping, &mut program);  // Transformer 不做 ESM→CJS
let code = Codegen::new().build(&program).code;
```

实测覆盖：

| 语法 | 结果 |
|---|---|
| 类型注解、`interface`、`type`、`satisfies`、`as const`、`using` | 删除 / 正确降级 |
| `enum` / `const enum` / `namespace` | 生成 IIFE |
| 参数属性 `constructor(public x: T)` | 生成赋值 |
| `import type` / `export type` / `import { type X }` | 整条删除（因此类型名不占运行时导出面） |
| `import.meta`、动态 `import()`、顶层 await | 保留（QuickJS 原生支持） |
| legacy 装饰器 | 生成 `@oxc-project/runtime` helper 导入；语料无装饰器，若出现则注册对应虚拟模块 |

全量 86 文件 / 451,654 B：**0 失败**，纯 strip 12.1 ms（140 µs/文件）。JSX/TSX 可解析但本期不做变换（语料无）。

### 3.3 22 个虚拟模块（逐字对齐 `virtual-modules.ts`）

| 键 | 指向 |
|---|---|
| `typebox` / `@sinclair/typebox` | 嵌入的 typebox 1.3.27（`build/index.mjs`） |
| `typebox/compile` / `@sinclair/typebox/compile` | 同上 `build/compile/index.mjs` |
| `typebox/value` / `@sinclair/typebox/value` | 同上 `build/value/index.mjs` |
| `@earendil-works/kiana-agent-core` / `@mariozechner/kiana-agent-core` | 同一 namespace（`js/virtual/kiana-agent-core.js`） |
| `@earendil-works/kiana-tui` / `@mariozechner/kiana-tui` | 同一 namespace（`js/virtual/kiana-tui.js` + 原生类） |
| `@earendil-works/kiana-ai` / `@earendil-works/kiana-ai/compat` | **同一 namespace**（compat 是 core 的严格超集；`/tmp/spec_parts` 统计的并集） |
| `@earendil-works/kiana-ai/models` / `oauth` / `providers/all` | 各自独立 namespace |
| `@mariozechner/kiana-ai` / `.../compat` / `.../models` / `.../oauth` / `.../providers/all` | 与上面对应同实例 |
| `@earendil-works/kiana-coding-agent` / `@mariozechner/kiana-coding-agent` | 同一 namespace（34 个运行期导出，§8.2） |

身份要求（`11156-kiana-ai-models-virtual-module.test.ts`）：同一键的多次 import 必须是**同一个对象**；
本设计单 Context + 每键一个 namespace `Persistent<Object>`，天然满足。
注入方式：prelude 建 `globalThis.__kiana_virtual` 对象，每个键一个 getter 返回 Rust 预置的 namespace；
虚拟模块源只做 `export const X = __kiana_virtual["key"].X`。这样 Rust 不必为每个导出建 `Function`。

`kiana-tui` 的 namespace 由两部分拼成：Rust 原生类/函数（§5.1）+ JS 常量与组合类（`Key`、
`CURSOR_MARKER`、`Container`、`Stack` 系列、`renderFakeCursor`、`defineTool`）。

### 3.4 `moduleCache: false` 的等价实现

jiti 设 `moduleCache: false`：每次 `jiti.import(entry)` 重新执行模块。QuickJS 的模块表是**每 Context**
（`quickjs.c:610 ctx->loaded_modules`，`js_find_loaded_module` 按 ctx 查找），且按模块名去重。
因此：

- **扩展入口**用唯一模块名：`<abs-path>?v=<generation>-<seq>`（`seq` 每代递增）。同名文件带不同 query
  天然各执行一次；相对依赖保持真实路径（同代共享）。实测：`?v=1`/`?v=2` 各执行一次，`./dep.js` 两份实例共享。
- `import.meta.url` 仍注入不带 query 的真实 `file://` URL（`meta.set("url", ...)`）。
- 相对依赖同代共享与 jiti「每次 import 重建整棵图」有细微差别，但工厂缓存语义是「命中缓存不重跑模块但重跑工厂」，
  且 reload 换代，**不会产生可观察差异**。需要逐条对齐时开 `KIANA_EXT_STRICT_NOCACHE=1`：给整棵依赖图加同一 query。
- 工厂缓存键 = `(resolvedPath, cwd, generation)`（`loader.ts` 的 `extensionCache`）；命中时不重跑模块但重跑工厂。

### 3.5 CJS：真 `require`，不是 ESM 包装

朴素包装器只对自包含 CJS 成立。`shell-quote@1.12.0`（`exports.quote = require('./quote')`）、
`zod@3.25.76`（`index.cjs` require 子文件）、`@pondwader/socks5-server`（`require("net")`）都会抛
`ReferenceError: require is not defined`（`/tmp/sbprobe` 首次运行的实际报错）。

**正确做法（实测通过）**：

1. Loader 把 CJS 文件变成一个 ESM 模块，其 `default` 是 CJS 加载器求值后的 `module.exports`。
2. `require` 由宿主提供、递归解析；解析与 ESM 的 `Resolver` 共用（虚拟键 → 相对 → `node_modules`），
   但只接受 CJS/JSON；命中 ESM 抛 `ERR_REQUIRE_ESM`（Node 22+ 的 `require(esm)` 第一版不做）。
3. CJS 缓存以**绝对路径为键**放在 JS 侧 `globalThis.__kiana_cjs = Object.create(null)`
   （Rust 侧 `Persistent<Value>` 缓存会遇到 `'js` 逃逸，编译期报
   `returning this value requires that 'js must outlive 'static`）。
4. 模块体用 `new Function("module","exports","require","__filename","__dirname", src)` 求值：
   自动获得 `this === module.exports`、`arguments`、独立函数作用域，无需改写源码。
5. `exports` 必须先绑定为 `module.exports`；模块体重新赋值 `module.exports` 时以新值为准（Node 语义）。
6. **外层 IIFE 是必需的，不是风格**：`@anthropic-ai/sdk` 的 `index.js:3` 在模块顶层写
   `exports = module.exports = function (...args) {...}`。若源码直接拼在 `const exports = module.exports;` 之后，
   QuickJS 报 `'exports' is read-only`（V8 报 `Assignment to constant variable.`，两种模式都抛 TypeError）。
   IIFE 的形参 `exports` 是可写绑定，才符合 CJS。
7. `require.resolve` 返回绝对路径；`require.cache` 指向 `__kiana_cjs`；`require.main = undefined`。

Loader 的 CJS 分支（实测通过）：

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

`__kiana_make_require` 是 prelude 的函数，只调两个**同步**宿主函数 `__kiana_resolve(from, spec)` 与
`__kiana_read(path)`（后者对 ESM 文件返回 `null` 触发 `ERR_REQUIRE_ESM`）。`require` 本身同步，文件读取在
JS 线程直接 `std::fs`，不进 tokio。

判定 CJS：扩展名 `.cjs`，或 `package.json` 的 `type != "module"` 且文件含 `require(`/`module.exports`/`exports.`。

### 3.6 npm 依赖从哪找

- 扩展的依赖在自己的 `node_modules`（`with-deps/`、`sandbox/`、`custom-provider-anthropic/` 等；
  monorepo 把 5 个示例目录列为 workspace）。解析从**扩展文件所在目录**逐级向上找，而不是 kiana 二进制位置
  （与 jiti 一致；`with-deps` 的注释明确说明）。
- `typebox`/`marked`/`kiana-tui` 不需要安装：`typebox` 由二进制内嵌；`marked` 与 `kiana-tui` 不参与运行时（§5）。
- 嵌入方式：`crates/kiana-extensions/vendor/typebox/`（只放 `build/**/*.mjs`，统计 691 文件 / 865 KB；
  `.d.mts` 不入库）。`build.rs` 遍历该目录，生成
  `static TYPEBOX_FILES: &[(&str, &[u8])] = &[("build/index.mjs", include_bytes!(...)), ...];`
  的 include 表（不使用 `rust-embed`，避免额外宏依赖）。Loader 用同一套 `Module::declare` 服务，
  因此 `typebox`、`typebox/compile`、`typebox/value` 共享同一模块缓存与 `Symbol.for("TypeBox.Kind")`。

**为什么用真 typebox 而不是手写 builder**：typebox 1.3.27 是 ESM、有 exports map、JIT 通过
`new (globalThis.Function)` 实现（`Environment.CanEvaluate` 在 QuickJS 下为 true，实测可 Compile）；
`/tmp/tbtest` 实测 287 个导出全部可用，`Type.Object/Optional/Array/Number/Boolean/String` 足够覆盖语料。
手写 builder 无法复刻 `Compile().Errors()` 的 `keyword`/`instancePath`/`message` 与
`Symbol.for("TypeBox.Kind")` 判定；错误文案是用户可见的（`validateToolArguments` 的
`Validation failed for tool ...`），必须逐字。

### 3.7 schema 校验的职责划分（对三案争议的定论）

`packages/ai/src/utils/validation.ts` 的 `validateToolArguments` 做四件事：`normalizeOptionalNulls`、
`Value.Convert`、`Compile(schema).Check/Errors`、非 typebox schema 的 `coerceWithJsonSchema` 回退。
`ai-core.md` 建议 Rust 侧重写。定论是**按 schema 的来源分派**：

| schema 来源 | 校验实现 | 理由 |
|---|---|---|
| 扩展注册的工具（`registerTool({ parameters: Type.* })`），以及扩展用 `create*ToolDefinition` 展开重注册的内建工具 | **JS 侧 typebox**：Rust 在调用 `execute` 前调用 `__kiana.validateToolArgs(handleId, args)`，JS 内用 `WeakMap<schema, Compile>` 缓存（与 `validation.ts:6` 同构），返回强制转换后的 args 或抛出逐字错误 | 错误文案、`Value.Convert` 的隐式转换、`$ref`、typebox 符号只有真包能逐字一致 |
| Rust 内建工具、以及不经过扩展的 agent 调用 | **Rust 侧 `ai-core.md` 的 JSON-Schema 移植** | 不需要进 JS，热路径更短；这些 schema 由 Rust 生成，文案由 Rust 自测锁定 |
| 内建工具的定义被 JS 侧构造时（`createReadToolDefinition` 等） | parameters 由 Rust 调 JS typebox 构造一次（`Type.Object(...)`），使 `TypeBox.Kind` 符号真实存在 | 否则 JS 侧会走 `coerceWithJsonSchema` 回退，行为与 Node 不同 |

这条划分同时删除了 NB「必须靠依赖域里的 typebox」与 RF「Rust 手写校验器」的冲突：
typebox 求值一次（单 Context + 模块缓存），每个 schema 的 `Compile` 结果按 schema 对象缓存。

### 3.8 解析顺序（Node ESM + jiti 对齐）

1. 虚拟键精确匹配（22 键，含子路径）。
2. `./`、`../`、`/`：带扩展名直接用；无扩展名依次 `.ts`、`.tsx`、`.js`、`.mjs`、`.cjs`（TS 优先，jiti 语义）；
   目录 → `package.json` 的 `exports`/`module`/`main` → `index.{ts,js}`。
3. 裸 specifier：`node:`/内建名 → shim 表（返回 `node:<name>`）；否则从 `base` 目录逐级 `node_modules`
   （`oxc_resolver` 处理 `exports` 条件 `import`/`require`/`node`/`default`、`#imports`、symlink）。
4. 失败 → `Error loading module '<spec>'`（与 rquickjs loader 错误文本一致，
   实测 `Error loading module 'node:child_process': No such file or directory (os error 2)`）。

`tsconfigPaths`：只在扩展目录里存在 `tsconfig.json` 时按 `paths` 解析（`oxc_resolver` 支持），
否则跳过。`kiana` 自身没有 TS 源码树，不存在 jiti 的 `isTypeScriptSourceRuntime` 分支。

### 3.9 依赖加载顺序与循环

QuickJS 原生支持循环导入与顶层 await。唯一要求：**虚拟模块与 Node shim 必须在该代第一个扩展求值前
声明完毕**（否则模块级 `const` 处于 TDZ，报 `X is not initialized`）。做法：`Generation::new()` 里先
`Module::declare` 全部 shim/virtual 源（惰性求值，靠 `Module::import` 触发），再加载扩展。

---

## 4. 扩展 API：JS 侧对象 + Rust 原语

### 4.1 分层

```text
js/prelude.js        纯 JS 基础设施：Promise 协议、定时器、AbortSignal、Buffer、process、console、
                     Intl/locale、structuredClone、EventEmitter、CJS 加载器、node:* shim 入口
js/api.js            makeExtensionApi(host)：`kiana` 对象、41 个 on、31 个方法、events、事务三态机
js/context.js        makeContext(host, kind)：读时求值的 getter + 方法
js/dispatch.js       runner.ts 的 13 个 emitter 逐行移植（合并语义在此）
js/virtual/*.js      22 个键的导出面（多数只是从 __kiana_virtual 取引用）
Rust bindings/*.rs   注册表所有权、动作方法、UI 对话框、原生组件、provider 桥
```

**原则（RF §7.1 + JS §13 合流）**：凡是「JS 里必须是真对象/真函数」的（`kiana.on`、handler、`defineTool`、
`ToolRendererResolver`、`Theme.fg`、`EventStream`、组件对象），一律 JS 实现；Rust 只提供窄原语
（`__kiana_host.*`）。事件对象**在 JS 侧构造并原地传递**，Rust 只在需要时读取字段（`ca-extensions.md` §8.6-11），
`undefined` 键的存在性用 `hasOwnProperty` 保留。

### 4.2 `kiana` 对象（计数已核对 `types.ts`）

```js
kiana = {
  // 41 个 on 重载
  on(event, handler) -> () => void,
  // 31 个非 on 方法名
  registerTool, registerCommand, registerShortcut, registerFlag, getFlag,
  registerMessageRenderer, registerMarkdownTransformer, registerEntryRenderer, registerToolRenderer,
  sendMessage, sendUserMessage, appendEntry, setSessionName, getSessionName, setLabel,
  exec, getActiveTools, getAllTools, getSettings, setActiveTools, getCommands,
  setModel, getThinkingLevel, setThinkingLevel,
  registerProvider, unregisterProvider, registerMcpServer, unregisterMcpServer, getMcpServers,
  registerVirtualModel, unregisterVirtualModel,
  // 1 个字段
  events: { emit(channel, data), on(channel, handler) -> () => void },
}
```

41 个事件名（逐字，按 `types.ts` 顺序）：

```
project_trust            resources_discover        session_start
session_info_changed     session_before_switch      session_before_fork
session_before_compact   session_compact            session_compact_failed
session_shutdown         mcp_servers_change         session_before_tree
session_tree             context                    context_with_system
cache_warming_decision   before_provider_request    before_provider_headers
after_provider_response  provider_stream_event      before_agent_start
agent_start              agent_end                  agent_before_settle
agent_settled            ui_prompt_start            ui_prompt_end
turn_start               turn_end                   message_start
message_update           message_end                 tool_execution_start
tool_execution_update    tool_execution_end          model_select
thinking_level_select    tool_call                  tool_result
user_bash                input
```

`ExtensionUIContext` = **28 个成员**（27 方法 + 只读 `theme`；`setWidget` 两个重载 ⇒ 29 条声明）。
`ExtensionContext` = **18 个具名成员**（含可选 `thinkingLevel?`）。
`ToolDefinition` = **20 个字段**；`ToolRenderContext` = **14 个字段**。

### 4.3 事务性 API 状态机（`loader.ts:247–560` 逐字）

```js
state: "loading" | "active" | "failed"
pendingFlagValues: Map<string, boolean|string>
pendingRuntimeChanges: Array<() => void>
loadingUnsubscribers: Array<() => void>
```

规则（每条对应 `loader.ts` 行号）：

1. 动作方法在加载期抛
   `Extension runtime not initialized. Action methods cannot be called during extension loading.`
   【逐字，`loader.ts:162`】；`setModel` 是特例：`Promise.reject(new Error("Extension runtime not initialized"))`
   【逐字，`:186`】。
2. `registerTool()` 加载期合法（`:288`）；`refreshTools` 为空函数。
3. 注册型调用进 `pendingRuntimeChanges`，`commit()` 时按 `bindCore` 的 9 步顺序 flush，然后 `state = "active"`。
4. 工厂抛错 ⇒ `discard()`：`state = "failed"`、退订 `loadingUnsubscribers`、清空 pending（不留半成品）。
5. `invalidate(msg?)`（`:186–205`）：首次生效，后续忽略；默认文案【逐字】：
   ```
   This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().
   ```
   同时退订该扩展在 `kiana.events` 上的全部订阅。
6. `Failed` 状态下任何方法抛
   `Extension "<path>" failed to load and its API is no longer active.`【逐字，`:259`】。
7. `getFlag(name)`：未注册返回 `undefined`；否则优先 `runtime.flagValues`，其次 `pendingFlagValues`（`:343–346`）。

注册校验文案（【逐字】，`loader.ts`）：

```
Tool "<name>" registered by extension "<path>" must define an object parameter schema.                       (:301)
Command registered by extension "<path>" must have a non-empty string name. Use kiana.registerCommand("name", { description, handler }).   (:310)
Command "/<name>" registered by extension "<path>" must define handler().                                    (:315)
Invalid default for flag "<name>": expected <type>, got <typeof>.                                            (:341)
Invalid MCP server registered by extension "<path>": <validated>                                             (:478)
MCP server "<name>" is already registered by extension "<owner>"                                            (:487)
MCP server "<name>" conflicts with registered server "<other>"                                               (:492)
```

加载结果文案（`:650`、`:658`）：

```
Extension does not export a valid factory function: <extensionPath>
Failed to load extension: <message>
```

### 4.4 上下文对象（18 个成员，读时求值）

`runner.ts:884–975` 的 `createContext()` 全部是 **getter 读时求值**，每次读取前 `runner.assertActive()`。
Rust 侧对应一个 `ExtensionContext` 原生类，`#[qjs(get)]` 访问器每次回调宿主；方法（`isIdle`、
`isProjectTrusted`、`abort`、`hasPendingMessages`、`shutdown`、`getContextUsage`、`compact`、
`getSystemPrompt`）在 `#[rquickjs::methods]` 里。

```rust
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ExtensionContext")]
pub struct ExtContext {
    #[qjs(skip_trace)] ext_id: ExtId,
    #[qjs(skip_trace)] kind: CtxKind,          // Base / Tool / Command / Replaced
    #[qjs(skip_trace)] ui: Persistent<Object<'static>>,
    #[qjs(skip_trace)] cwd: String,
}
```

| 成员 | 绑定 |
|---|---|
| `ui` / `mode` / `hasUI` / `cwd` / `model` / `scopedModels` / `thinkingLevel` / `signal` | getter → `__kiana_host.ctxGet(kind, name)`（同步） |
| `sessionManager` | 原生类实例（15 个只读方法，`session-manager.ts:245–262`） |
| `modelRegistry` | 原生类（`getApiKey`/`getModel`/…） |
| `isIdle` / `isProjectTrusted` / `hasPendingMessages` / `getContextUsage` / `getSystemPrompt` / `abort` / `shutdown` / `compact` | 方法 → 宿主 |
| `ExtensionToolContext` 额外 `tools`、`executeTool(name, args, options?) -> Promise<AgentToolCallOutcome>` | `kind == Tool` 时 `tools` 非空；`executeTool` 恒存在，非 tool ctx 调用抛错 |
| `ExtensionCommandContext` 额外 `getSystemPromptOptions()`、`waitForIdle()`、`newSession()`、`fork()`、`navigateTree()`、`switchSession()`、`reload()` | `kind == Command` |
| `ReplacedSessionContext` 额外 `sendMessage`/`sendUserMessage` 返回 `Promise<void>` | `kind == Replaced`；**同名不同义，按 kind 分派，不能共用一个函数** |

stale 检查在每个访问器/方法入口执行（与 `assertActive()` 一致）；测试用 §4.3 的两条逐字文案。

### 4.5 UI 上下文（28 个成员）

| 成员 | 行为 |
|---|---|
| `select/confirm/input/editor` | 返回 Promise；`opts.signal`/`opts.timeout` 生效；超时自动消解并显示倒计时 |
| `notify` | 推入 TUI 消息区（`info`/`warning`/`error`） |
| `onTerminalInput(handler)` | 注册 JS 回调，返回退订；handler 返回 `{consume?, data?}` |
| `setStatus(key, text \| undefined)` | 状态栏 map，`undefined` 删除 |
| `setWorkingMessage` / `setWorkingVisible` / `setWorkingIndicator` / `setHiddenThinkingLabel` | 直接字段 |
| `setWidget(key, string[] \| factory \| undefined, opts?)` | 数组 → 每行 `Text`，**最多 10 行** + `"... (widget truncated)"`【逐字，`interactive-mode.ts:2425–2429`，`MAX_WIDGET_LINES=10` 在 `:2486`】；工厂 → 每帧调用一次并缓存；`placement` 默认 `"aboveEditor"` |
| `setFooter(factory \| undefined)` / `setHeader(factory \| undefined)` | 工厂 `(tui, theme, footerData?) => Component & { dispose? }` |
| `setTitle` | OSC 2 终端标题 |
| `custom(factory, options?)` | 工厂 `(tui, theme, keybindings, done) => Component \| Promise<Component>`；`overlay`、`overlayOptions`（对象或每帧求值的函数）、`onHandle` |
| `pasteToEditor` / `setEditorText` / `getEditorText` | 直接操作 Editor |
| `addAutocompleteProvider(factory)` | 存 JS 工厂，Editor 输入时调用 |
| `setEditorComponent(factory \| undefined)` / `getEditorComponent()` | 见 §5.4 |
| `theme`（只读） | 返回**同一个** `Theme` 原生类实例 |
| `getAllThemes` / `getTheme(name)` / `setTheme(name \| Theme)` | `{ success, error? }`；RPC 下 `{ success:false, error:"Theme switching not supported in RPC mode" }`【逐字】 |
| `getToolsExpanded` / `setToolsExpanded` | 直接字段 |

**无 UI（print/json）逐字默认值**（`runner.ts:331–362` 的 `noOpUIContext`）：
`select -> undefined`、`confirm -> false`、`input -> undefined`、`getEditorText -> ""`、
`getEditorComponent -> undefined`、`getAllThemes -> []`、`getTheme -> undefined`、
`setTheme -> { success:false, error:"UI not available" }`、`getToolsExpanded -> false`、
`custom -> undefined`（never resolve 之外的 `undefined as never`）、其余空操作、
`onTerminalInput -> () => {}`。

`ui_prompt_start/end` 包装（`runner.ts:581–622`）在 JS 侧实现：`select/confirm/input/editor/custom`
外层进入时 `uiPromptDepth++`（0→1 才发 start，带 `kind`/`title`）、`finally` 时 `--`（0 才发 end）；
事件用 `queueMicrotask` 发出。

### 4.6 事件派发：13 个 emitter 的合并语义（JS 侧逐行移植）

`runner.ts` 有 **1 个泛型 `emit` + 1 个 `emitBoundary` + 11 个专用 emitter**。下列行为必须在 `js/dispatch.js`
里逐行复刻，并用夹具锁定。

| emitter | 合并 / 短路语义（源码位置） |
|---|---|
| `emit`（泛型）| 逐 handler `await`，`try/catch` → `emitError` 后继续；仅 `session_before_*` 系列取返回值，`cancel` 立即返回（`:1096–1124`，`cancel` 在 `:1107`） |
| `emitBoundary`（`turn_end`/`agent_before_settle`）| 共享草稿 `entries`/`continue`/`context`；每个 handler 后重新 `buildContext(entries)`；抛错 → `valid=false` + `Invalid boundary entries: <msg>`【逐字】；最终 `valid ? {entries, continue, context, valid:true} : {entries:[], continue:false, context, valid:false}`（`runner.ts:1036–1090`） |
| `emitCacheWarmingDecision` | 从 `event.action` 起，**最后一次 override 获胜**（`:1128–1150`） |
| `emitMessageEnd` | 逐 handler 换 `currentMessage`；返回消息 `role` 不同 → `message_end handlers must return a message with the same role`【逐字】并 continue；未修改最终返回 `undefined`（`:1151–1189`） |
| `emitToolResult` | 补丁链：`content` 替换时若同一次结果没有 `structuredContent` 则 **`delete`** 旧值；`details`/`structuredContent`/`isError`/`usage` 各自独立；未修改返回 `undefined`；返回对象只含这 5 个键（`:1190–1248`） |
| `emitToolCall` | **无 try/catch**（错误向上穿透）；`result.block` 立即返回；否则记录最后一次结果（`:1249–1268`） |
| `emitUserBash` | **唯一向上抛**；`isUserBashEventResult` XOR 校验（`operations`/`result` 恰一个）；非法结果抛 `Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object`【逐字】（`:1269–1304`、`:142–165`） |
| `emitContext` | 两阶段：`context` handler 只看非 system 消息、原地改与返回值都接受（`sameMessages` 引用相等判定）、改动后 `restoreSystemMessages` 把首个 system 消息前置；然后 `context_with_system` 看全量，返回值即结果；丢失开头 system 消息时 emitError 但**采用**结果（`:1305–1367`） |
| `emitBeforeProviderRequest` | 返回 `undefined` 保持当前 payload，否则替换（`:1368–1398`） |
| `emitBeforeProviderHeaders` | **原地修改**，返回值忽略（`:1399–1426`） |
| `emitBeforeAgentStart` | `ctx.getSystemPrompt` 覆盖为读当前 `currentOptions` 的渲染结果；`result.systemPrompt` 写 `forceSystemPrompt`（链式，后者覆盖）；`result.message` 逐个收集；事件上的 `systemPrompt` 是 getter（`:1427–1478`） |
| `emitResourcesDiscover` | 三类路径数组拼接，每项 `{ path, extensionPath }`（`:1479–1524`） |
| `emitInput` | `action: "handled"` 立即返回；`"transform"` 链式替换 `text`/`images`；其余继续；最终与入参不同才返回 `transform`，否则 `continue`（`:1525–1567`） |
| `emitProjectTrustEvent` | **第一个非 `undecided` 的结果获胜**，其余忽略；返回 `{ result?, errors }`（`:302–330`） |

通用骨架（每条 handler）：

```js
for (const e of snapshot())          // snapshotEventHandlers：extensions.map(e => handlers.slice())
  for (const h of e.handlers) {
    try { const r = await h(event, ctx); ...merge... }
    catch (err) { host.emitError({ extensionPath: e.path, event: type, error: msg(err), stack: stack(err) }); }
  }
```

- **快照**：`snapshotEventHandlers` 每次派发取一次；嵌套派发重新取快照（`runner.ts:276–278`）。
- **错误文案**：`err instanceof Error ? err.message : String(err)`，stack 同理。
- **派发顺序** = 扩展注册顺序 → 该扩展内 handler 注册顺序。
- **handler 返回值是 Promise 时**：JS 侧 `await` 即可，宿主不需要知道；Rust 调 `__kiana_dispatch.<emitter>(args)`
  拿到 Promise，登记 waiter，settle 后继续（§2.3）。
- **高频事件物化**：`message_update`/`tool_execution_update`/`provider_stream_event` 传 `Persistent` 句柄；
  `context` 整段历史一次 JSON（135 KB ≈ 3.5 ms）；其余低频事件走 JSON。

### 4.7 工具：`ToolDefinition`（20 个字段）

```
name label description promptSnippet promptGuidelines parameters constrainedSampling renderShell
prepareArguments outputSchema exposure namespace reachesTools annotations defaultActive
prepareLoadout executionMode execute renderCall renderResult
```

绑定策略（NB §4.3，采纳）：**不构造 Rust 镜像结构体**，保留 `Persistent<Object>` 原样对象 + 按需读字段。

```rust
pub struct RegisteredTool {
    pub obj: Persistent<Object<'static>>,        // 扩展可能后续修改它（minimal-mode 用 {...tools.read, renderCall} 展开）
    pub name: String,
    pub dispose: Option<Persistent<Function<'static>>>,   // 兼容扩展提供的 dispose
}
```

- `execute(toolCallId, params, signal, onUpdate, ctx)`：信号是 §2.6 shim；`onUpdate` 是 JS 函数；
  返回 `AgentToolResult { content, details, isError?, structuredContent? }`；Promise 走 §2.3。
- **每次调用一个新 ctx**（`wrapper.ts` 的 `wrapToolDefinition` 语义），且带 `executeTool` 与 `tools`。
- 校验按 §3.7：扩展工具在 JS 内用 typebox `Compile` 校验，错误文案逐字
  `Validation failed for tool "<name>":\n  - <path>: <message>\n\nReceived arguments:\n<json>`。
- `toToolDeclaration` 走 `JSON.parse(JSON.stringify(schema))`，symbol 键天然被忽略。

`renderCall(args, theme, context)` / `renderResult(result, options, theme, context)` 返回组件；`ToolRenderContext`
的 14 个字段：`args toolCallId invalidate lastComponent state cwd executionStarted argsComplete
isPartial expanded showImages isError durationMs outputPad`。`invalidate` 是 Rust 提供的 `Function`；
`lastComponent` 是上次渲染返回值（按 `toolCallId` 缓存，供动画复用）。

### 4.8 Provider / MCP / VirtualModel

| API | 绑定 |
|---|---|
| `registerProvider(provider)` / `registerProvider(name, config)` | 两个重载；都进 `pendingProviderRegistrations`（`loader.ts:461–471`），`bindCore` 时交给 `kiana-ai` 注册表 |
| `unregisterProvider(name)` | 进 pending（加载期）或立即生效 |
| `registerMcpServer(name, config)` | 先 `validateMcpServerConfig`，字符串结果 → 抛错；再查所有者与 `mcp__` 命名空间冲突；`structuredClone` 后进 registry |
| `registerMcpServer` 的冲突按 `mcpNamespace(name)`（`-`→`_`）比较，非仅字符串相等 | 语料 `telemetry-mcp` 的契约 |
| `registerVirtualModel(model)` | `definition.route = (request) => model.route(request, runtime.createContext())`；pending 直到 bind |
| `events`（EventBus） | JS `EventEmitter` shim：`on` 包 `try/catch` 并 `console.error("Event handler error (<channel>):", err)`；`emit` 同步广播 |

`streamSimple(model, context, options)` 返回 `AssistantMessageEventStream`（真 JS 类，`event-stream.ts` 原样搬运：
`push`/`end`/`result()`/`[Symbol.asyncIterator]` + `FifoQueue`）。Rust 调用后 `await` 其 `result()`（手写 Promise 协议），
再把 chunk 转发进 Rust 的 `kiana-ai` 流状态机。方向已实测：JS `for await` 消费宿主推送与「缓冲后消费」两条路都通。

### 4.9 错误落点（三种，逐字）

```
Extension "<path>" error: <error>                                    交互式聊天区（error 色）
Extension error (<path>): <error>                                    print/json stderr
{"type":"extension_error","extensionPath":"<path>","event":"<event>","error":"<error>"}   RPC stdout
```

handler 抛错不中断其它 handler（`emitUserBash` 例外，向上抛）。同一组件/同一 handler 的相同错误去重打印。

---

## 5. TUI 组件互操作

### 5.1 形态划分（kiana-tui 19 个运行期符号 + 组合类）

语料从 `@earendil-works/kiana-tui` 实际导入的**运行期**符号 19 个：
`Box CURSOR_MARKER Container Editor Input Key Markdown SelectList SettingsList Spacer Text
fuzzyFilter isKeyRelease matchesKey parseKey renderFakeCursor truncateToWidth visibleWidth wrapTextWithAnsi`。

`coding-agent` 还提供 `CustomEditor`、`BorderedLoader`、`DynamicBorder`、`getMarkdownTheme`、
`getSettingsListTheme`、`keyHint`。

| 符号 | 实现 | 理由 |
|---|---|---|
| `Text`、`Box`、`Spacer`、`Input`、`SelectList`、`SettingsList`、`Loader`、`ScrollView`、`Image`、`TruncatedText`、`Markdown`、`CancellableLoader`、`DynamicBorder` | **Rust 原生类**（`#[rquickjs::class]`），`render(width)` 直接调 `kiana-tui` 的 Rust 组件（`tui-components.md`） | 与 TUI 其余部分同一实现，渲染字节天然一致；`Intl.Segmenter` 由 Rust 字素服务提供 |
| `Editor`、`CustomEditor` | **Rust 原生类**（`components/editor.ts` 的 2473 行移植；`CustomEditor` 是 `coding-agent` 侧的 Rust 类，继承 Editor） | 需要 `super.render()`/`super.handleInput()` 与 `renderTopBorder` 覆写（§5.4） |
| `Container`、`Stack`、`VStack`、`HStack` | **JS**（prelude 约 90 行，逐行移植 `tui.ts:358–429` 与 `components/stack.ts`） | 纯组合 + `children` 数组；`TuiBase.containsComponent` 用 `root instanceof Container`，所以 JS `Container` 必须是**同一个**类（见下） |
| `CURSOR_MARKER` = `"\x1b_kiana:c\x07"`、`FAKE_CURSOR_START` = `"\x1b_kiana:fc\x07"`、`FAKE_CURSOR_END` = `"\x1b_kiana:/fc\x07"`、`renderFakeCursor` | JS 常量/函数（逐字） | 字符串只被扩展拼接，无行为 |
| `visibleWidth`、`truncateToWidth`、`wrapTextWithAnsi`、`sliceByColumn`、`fuzzyFilter`、`matchesKey`、`parseKey`、`isKeyRelease`、`Key` | **Rust 函数/对象**（复用 `keys.rs`/`utils.rs`/`fuzzy.rs`） | 性能关键路径；必须与 TUI 其它部分同一套宽度/键位规则 |
| `MarkdownTheme`、`SelectListTheme`、`SettingsListTheme`、`EditorTheme`、`ImageTheme` | **JS 普通对象**（由 Rust 从当前主题生成后传给 JS） | 主题是值不是行为 |

**`Container` 的 `instanceof` 约束**：`tui.ts:744` 的 `containsComponent` 用 `root instanceof Container`。
文档挂载的组件树由 Rust 的 `TuiBase` 持有；扩展自己的 `Container` 子树是 JS 对象（被 `ui.custom` 返回）。
因此：JS `Container` 类只用于**扩展内部**组合与渲染；当扩展把 `Container` 作为 `custom` 的返回值交给 Rust 时，
`JsComponent` 直接调它的 `render(width)`，Rust 不需要判断它是不是 `Container`。若将来需要「扩展把子组件插进
宿主树」，必须走 `addChild(component)` 的显式接口，不能依赖 `instanceof`。

`BorderedLoader` 与 `DynamicBorder` 放 Rust（与 NB 的 JS 方案不同）：它们只组合 `Container` +
`CancellableLoader` + 边框字符串，放 Rust 后 `question.ts`/`questionnaire.ts`/`qna.ts`/`handoff.ts` 的
`new BorderedLoader(...)` 与 `extends` 都走同一实现，且 `dispose`/`stop` 的时序由 Rust 保证。

### 5.2 `This` 纪律（最关键的一条，本轮复测）

从 Rust 调用 JS 对象上的方法时**必须把对象作为 `This` 传入**，否则 JS 子类的 `super` 拿不到 `this`：

```rust
// 反例：this 为 undefined -> "Error converting from js 'undefined' into type 'object'"
let f: Function = obj.get("render")?;
f.call::<_, Vec<String>>((width,))?;

// 正例：
let f: Function = obj.get("render")?;
f.call::<_, Vec<String>>((This(obj.clone()), width))?;
```

`This` 在 `rquickjs::function`（也在 prelude 里），`FromParam for This<T>` 读 `params.this()`
（`rquickjs-core/src/value/function/params.rs:306`）。适用面：`Component.render/handleInput/handleMouse/
invalidate`、`Editor` 的每个可覆写方法、工厂返回的组件、`Theme` 上由 JS 提供的样式函数。

本轮 `/tmp/sendchk` 的原生↔JS 虚拟派发实测（`class Editor extends __NativeEditor`，JS 覆写 `render` 调
`super.render`、覆写 `renderTopBorder` 读 JS 字段 `borderColor`）：

```
OK native-top(10)+|sub(native-top(20))+red|native-top(30)+|true
```

即：Rust 基类 `render` 通过 `this.renderTopBorder` 与 `This` 派发到 JS 覆写，JS 侧 `borderColor` 可读。

### 5.3 原生类的构造与字段（本轮新证，必须遵守）

**原生 `#[qjs(constructor)]` 里通过 `This<Object>` 设置的属性不会留在实例上**。实测：
`new W()` 之后 `ownKeys` 为空、`w.peek()` 返回 `MISSING`；而从普通原生方法设置属性则保留
（`peek` 读到 `from-native-method`）。这是 rquickjs 0.14 + quickjs-ng 的实际行为，三份候选设计都没写。

后果与对策：

1. 原生类的构造函数**只初始化 Rust 字段**，不写 JS 可见属性。
2. 需要 JS 可见且可写的字段（`Editor.borderColor`、`Editor.onSubmit`、`Input.focused`、
   `SelectList.onSelect`/`onCancel`）用 **`#[qjs(get)]`/`#[qjs(set)]` 访问器**实现：
   ```rust
   #[rquickjs::methods]
   impl Editor {
       #[qjs(get, rename = "borderColor")] fn get_border_color<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> { ... }
       #[qjs(set, rename = "borderColor")] fn set_border_color<'js>(&self, f: Function<'js>) -> Result<()> { ... }
   }
   ```
   宏按「JS 属性名」把方法收进同一个访问器：上例两个 Rust 方法都 `rename = "borderColor"`，
   于是生成一个 `get`+`set` 的属性（`methods.rs` 的 `accessors.entry(function.name(...))` 分组）。
   单一方法不能同时标 `get` 与 `set`（宏直接报 `a function can't both be a setter and a getter`）。
   本轮 `/tmp/sendchk/src/bin/getset.rs` 实测：`w.borderColor` 未赋值时为 `undefined`、赋值后读回函数、
   `Object.keys(w)` 为空、`hasOwnProperty("borderColor") === false`（访问器在原型上）。
   因为访问器在原型上而**不在自有属性上**，`setCustomEditorComponent` 的
   `if (newEditor.borderColor !== undefined)` 依赖 getter 有默认值：`Editor` 的 `borderColor` getter
   必须默认返回当前 `EditorTheme.borderColor`（构造时从 theme 取），否则默认编辑器换自定义编辑器时
   不会复制边框色。其余「默认值在构造期确定」的字段（`paddingX`、`autocompleteMaxVisible`）同理。
3. 访问器**在原型上**，所以 `this.borderColor = fn` 会走 setter（不会创建自有属性）。examples 的写法
   （`editor.onSubmit = ...`、`this.borderColor(...)`）因此都能工作。只有「必须是自有属性」的场合
   （被 `Object.assign`/展开拷贝、或被 `hasOwnProperty` 检查）才需要 JS 包装层：
   ```js
   class Editor extends __NativeEditor {
     constructor(tui, theme, options) {
       super(tui, theme, options);
       Object.defineProperty(this, "borderColor", { value: theme.borderColor, writable: true, configurable: true });
     }
   }
   ```
   注意 `Object.defineProperty` 会遮蔽原型访问器，之后 Rust 侧读不到该字段——所以只对**纯 JS 字段**（不回传 Rust）
   使用；`onSubmit`/`onChange`/`focused` 这类 Rust 需要读的字段一律用访问器。
4. 原生类持有 JS 回调用 `Persistent<Function>` + `#[qjs(skip_trace)]`；`Trace`/`JsLifetime` 派生必备。

### 5.4 三个编辑器子类（语料最硬的 UI 约束）

`modal-editor.ts`、`rainbow-editor.ts`、`border-status-editor.ts`：

```ts
class BorderStatusEditor extends CustomEditor {
  constructor(tui, theme, keybindings) { super(tui, theme, keybindings, { paddingX: 0 }); }
  render(width) { const lines = super.render(width); ... this.borderColor(text) ... }
}
ctx.ui.setEditorComponent((tui, theme, kb) => new BorderStatusEditor(tui, theme, kb));
```

要求：

1. `CustomEditor` 是 `Editor` 的 Rust 子类，构造签名 `(tui, theme, keybindings, options?)`，
   `options.paddingX` 先生效（`super(tui, theme, keybindings, { paddingX: 0 })` 必须可用）。
2. JS `extends CustomEditor` 后 `super.render(width)`/`super.handleInput(data)` 进入 Rust 实现（§5.2 实测）。
3. 基类 `render(width)` 内部调用 `this.renderTopBorder(width, hiddenLineCount)` / `renderBottomBorder(...)`，
   **通过 `This` 派发**，使 JS 覆写生效；`borderColor` 通过访问器读。
4. `CustomEditor.handleInput` 的顺序（`custom-editor.ts`）：`actionHandlers` → `keybindings.matches(...)` →
   paste image → `onExtensionShortcut` → `super.handleInput`。
5. `setCustomEditorComponent` 的接线（`interactive-mode.ts:2828–2930`）必须逐条复刻：保存当前文本 →
   `disposeActiveSelector()` → 清容器 → 调工厂 → 复制 `onSubmit`/`onChange` → `setText(currentText)` →
   复制 `borderColor`（若存在）→ `setPaddingX`/`setAutocompleteMaxVisible`（若存在）→ `setAutocompleteProvider`
   → 鸭子判定 `"actionHandlers" in customEditor && instanceof Map` 后复制 `onEscape`/`onCtrlD`/`onPasteImage`/
   `onExtensionShortcut` 与全部 action handler → `addChild` → `setFocus` → `requestRender`。
   注意源码注释明确写「`instanceof` fails across jiti module boundaries」，本设计同样是跨模块对象，
   **必须用鸭子判定**，不能 `instanceof CustomEditor`。

### 5.5 `Theme`

`theme.fg(...)` 在语料 86 个文件里出现 272 次（`grep -rn` 计数），是最高频 API 调用。Rust 原生类，方法直接返回 ANSI 字符串：

```
fg bg bold italic underline inverse strikethrough getFgAnsi getBgAnsi getColorMode
getThinkingBorderColor getBashModeBorderColor style colors appearance
```

`theme` 的 `Symbol.for` 键（`theme.ts:648–663`，新旧两套）都要注册：扩展通过
`globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")]` 读当前主题，必须拿到同一个 Rust 原生实例。
`ctx.ui.theme` 与 `ctx.ui.getTheme(name)` 返回同一实例。

### 5.6 widget / footer / header / custom

| API | 绑定 |
|---|---|
| `setWidget(key, string[], opts)` | 每个字符串一个 `Text`，**最多 10 行**，超出追加 `"... (widget truncated)"`【逐字】 |
| `setWidget(key, factory, opts)` | 每帧调用 `factory(tui, theme)` 一次，`invalidate` 时重算；返回组件若为 Promise 不等待（工厂契约是同步） |
| `setFooter(factory)` / `setHeader(factory)` | 工厂 `(tui, theme, footerData)`；`footerData` 提供 `getGitBranch()`/`getExtensionStatuses()`/`onBranchChange()`/`getAvailableProviderCount()`；返回对象的 `dispose` 在替换/关闭时调用 |
| `custom(factory, opts)` | 工厂可返回 Promise；`overlay:true` 走 overlay 栈；`done(result)` resolve；关闭时 `dispose?.()` 并恢复编辑框焦点与文本 |

`TUI` 代理对象（`factory(tui, ...)` 的参数）暴露 `tui.ts:464–493` 的接口：`requestRender(force?)`、
`renderNow(force?)`、`showOverlay/hideOverlay`、`start/stop`、`addChild/removeChild`、`setFocus`、
`terminal`、`getChildComponents` 等；语料用到 `requestRender()`（35 次）、`showOverlay(`（3 次）、`start`/`stop`（各 1 次）。
全部转发到 `TuiBase`；`requestRender` 在渲染中调用只置 dirty。

`OverlayOptions` 字段：`width minWidth maxHeight anchor offsetX offsetY row col margin visible nonCapturing`
（`tui.ts:254`）；`SizeValue` 支持数字或 `"50%"`；`anchor` 九宫格。`OverlayHandle`：
`hide() setHidden(b) isHidden() focus() unfocus() isFocused() getBounds()`。
overlay 栈的 `focusOrder` 单调计数器与「取最大 focusOrder 的可捕获可见项」规则见 `tui-core.md` §8.2。

### 5.7 性能预期

| 指标 | 实测 | 说明 |
|---|---|---|
| JS `render(80)` 同线程往返（读回 6–8 行） | **1.34 µs/帧**（NB）/ 2.9 µs（RF 本地） | 一次 `with_js` + 一次 `Function::call` |
| Rust 原生组件 `Text.render` | 与 TUI 其余部分同量级（亚 µs） | 无跨语言 |
| `theme.fg` | 原生方法，87 ns 量级 | |
| 30 个 JS 组件/帧 | ≈ 40–90 µs | 16.7 ms 预算的 0.3–0.5% |
| `visibleWidth`/`truncateToWidth` | Rust，`unicode-width` + 字素迭代 | 无 JS polyfill 的 10–20× 退化 |

对比：RF 跨线程 27–32.5 µs/帧（最坏 280–315 µs），30 个组件最坏 9.4 ms；JS 案 `truncateToWidth`
ANSI+CJK 20k 次 4.39 s（Node 295 ms）。本设计两项都避开。

### 5.8 `Intl`（最大缺口）

QuickJS **没有 `Intl`**（`typeof Intl === "undefined"`），但内置了固定的
`Date#toLocaleString/toLocaleDateString/toLocaleTimeString`（`MM/DD/YYYY, hh:mm:ss AM`）与
不做千分位的 `Number#toLocaleString`。kiana-tui 的 `utils.ts` 在**模块顶层**就 `new Intl.Segmenter(...)`，
所以：

- 本设计不跑 kiana-tui 的 JS 源码，`utils.ts` 的顶层 Segmenter 不存在；
- 但 **prelude 必须在任何可能触及 `Intl` 的模块之前安装 `Intl` 与 `toLocale*` 覆盖**（JS 案的教训）；
- 扩展自己用 `Intl`（语料无直接使用，但第三方扩展可能有）时提供最小实现：

```js
// js/prelude.js —— 安装顺序见 §9
globalThis.Intl = globalThis.Intl || {};
globalThis.Intl.Segmenter = class {
  constructor(locale, options = {}) { this.granularity = options.granularity || "grapheme"; }
  segment(input) { return __kiana_segment(String(input), this.granularity); }   // Rust: UAX#29
};
globalThis.Intl.NumberFormat = class { /* 见 §6.4：locale 数据可用时走 ICU4X，否则 en-US */ };
globalThis.Intl.DateTimeFormat = class { /* 同上 */ };
globalThis.Intl.Collator = class { /* 不支持：抛 RangeError("Intl.Collator is not supported") */ };
```

`__kiana_segment` 返回 `[{ segment, index, input, isWordLike? }]`，索引是 **UTF-16** 偏移（`encode_utf16().count()` 累加）。

`toLocaleString` **覆盖内置方法**（`Object.defineProperty`），输出对齐 Node 的 locale 语义（§6.4），
并在 CHANGELOG 写明「覆盖 QuickJS 内置行为」。

---

## 6. Node 内建与全局

### 6.1 语料实际用量（86 文件统计）

| 模块 | 用到的 API | 实现 |
|---|---|---|
| `node:path`(12)/`path`(3) | `join resolve dirname basename relative isAbsolute sep posix.* extname` | Rust；POSIX 语义逐条复刻（`resolve("..")` 归一化、`relative` 边界已实测） |
| `node:fs`(10)/`fs`(2) | `readFileSync writeFileSync appendFileSync existsSync readdirSync({withFileTypes}) statSync stat access readFile writeFile appendFile mkdtemp unlinkSync rmdirSync mkdir watch constants createReadStream Dirent promises.*` | 同步直调 `std::fs`；`watch` 用 `notify`；`createReadStream` 最小 `Readable`；`fs.promises` 走 `spawn_local` |
| `node:child_process`(5)/`child_process`(2) | `spawn spawnSync execFile execSync exec`、`stdout/stderr.on`、`on("close"/"error")`、`kill` | `tokio::process` + `ChildProcess` shim（EventEmitter + Readable 风格）；`exec` 走 `/bin/sh -c`，`execFile` 不走 shell |
| `node:os`(1)/`os`(2) | `homedir tmpdir platform arch` | `std::env` |
| `node:url`(3) | `fileURLToPath pathToFileURL` | `url` crate |
| `node:util`(1) | `promisify`（含 `util.promisify.custom`） | JS 实现 |
| `node:zlib`(1) | `gunzipSync` | `flate2` |
| `node:readline`(1) | `createInterface({input})` + `for await (line of rl)` | JS 按行切分 AsyncIterator（`git-merge-and-resolve.ts`） |
| `node:module`(1) | `createRequire(import.meta.url)` | 返回走 Resolver/Loader 的 `require`（`doom-overlay`） |
| `node:events` | `EventEmitter` | JS 实现（`on/once/off/emit/removeAllListeners`） |
| `node:net`/`node:http`/`https` | 源码未直接 import，但 `sandbox/` 依赖链**必需**（§7.3） | tokio `TcpListener`/`TcpStream` + `Server`/`Socket` shim |
| `node:tls`/`dgram`/`worker_threads`/`vm` | 未用 | 不提供（`tls` 抛 `ERR_UNSUPPORTED`） |

### 6.2 全局

`console`（log/warn/error → kiana 日志；`console.error` 用于 EventBus 错误）、`process`（见下）、
`Buffer`、`TextEncoder`/`TextDecoder`（UTF-8；`{stream:true}` 增量）、`URL`/`URLSearchParams`、
`fetch`、`crypto`（`getRandomValues`/`randomUUID`/`subtle.digest("SHA-256")`）、
`setTimeout/clearTimeout/setInterval/clearInterval`、`queueMicrotask`（原生）、`performance`（原生）、
`atob`/`btoa`（原生）、`WeakRef`/`FinalizationRegistry`（原生）、`structuredClone`（JS 子集）、
`AbortController`/`AbortSignal`（JS）、`TextEncoderStream` 不提供。

`process` shim 要点：

```js
globalThis.process = {
  env: /* Rust 注入的可写普通对象 */,
  argv: ["kiana", /* kiana 可执行文件路径 */],
  execPath: /* kiana 二进制路径 */,
  platform: "linux" | "darwin" | "win32",
  cwd: () => __kiana_host.cwd(),
  exit: (code) => { throw new ExitSignal(code); },   // 协作式退出，不立即终止
  stdout: { write(s) { __kiana_host.write(1, s) }, isTTY: true, columns: 80, on() {}, once() {} },
  stderr: { write(s) { __kiana_host.write(2, s) }, isTTY: true, on() {}, once() {} },
  versions: { node: "22.0.0" },                       // 伪造；SDK 用它判分支（§7.4）
  kill: (pid, sig) => __kiana_host.kill(pid, sig),
};
```

`process.argv[1]` 必须指向 kiana 可执行文件（`subagent` 用它重 spawn 自身，`overlay-qa-tests` 同理）。

### 6.3 关键语义风险

- `fs.readdirSync` **不排序**（`subagent/agents.ts:71`、`claude-rules.ts:34` 直接遍历；Node 也不排序）。
- `path.resolve` 的 `..` 归一化与 Windows 盘符/UNC（Linux 单测 + Windows 单测）。
- `Buffer.toString("utf8")` 非法序列替换为 U+FFFD。
- `spawn` 的 `stdio: ["ignore","pipe","pipe"]`：`ignore` 不得让子进程继承 kiana 的 stdin
  （`overlay-qa-tests.ts` 用它启动自身）。
- `process.exit` 走正常 shutdown 流程（投递 `Exit(code)`）。
- `process.kill(pid, signal)` → `libc::kill`（`overlay-qa-tests`）。

### 6.4 locale 策略（三案共同遗漏，必须单列）

实测 Node（26，本机 ICU）：

| 环境 | `Intl.DateTimeFormat().resolvedOptions().locale` | `new Date(2024,0,2,3,4,5).toLocaleString()` | `(1234567.5).toLocaleString()` |
|---|---|---|---|
| 未设置 LC_ALL/LANG | `zh-CN`（本机 LANG=zh_CN.UTF-8） | `2024/1/2 03:04:05` | `1,234,567.5` |
| `LC_ALL=de_DE.UTF-8` | `de-DE` | `2.1.2024, 03:04:05` | `1.234.567,5` |
| `LC_ALL=ja_JP.UTF-8` | `ja-JP` | `2024/1/2 3:04:05` | `1,234,567.5` |
| `LC_ALL=en_US.UTF-8` | `en-US` | `1/2/2024, 3:04:05 AM` | `1,234,567.5` |

优先级：`LC_ALL` > `LANG` > `en-US` 回退；`LANGUAGE` 不被 Node 使用；`TZ` 生效。
20 个 locale 的完整对照表（en-GB/de-DE/fr-FR/ru-RU/ko-KR/da-DK/sv-SE/fi-FI/pl-PL/tr-TR/hi-IN/th-TH/ar-EG 等）
已采集，实施时作为夹具。

策略（三档，可编译期裁剪）：

1. **`locale-data` 特性（默认开启）**：用 ICU4X 2.4.0 的 `icu_datetime`（`YMDT::short()` +
   `YearStyle::Full`）与 `icu_decimal`（`DecimalFormatter`）实现 `Date#toLocaleString/toLocaleDateString/
   toLocaleTimeString` 与 `Number#toLocaleString`。已实测与 Node **逐字一致**的 locale（`YMDT::short()` + `YearStyle::Full`）：
   `en-US`（`1/2/2024, 3:04:05 AM`）、`en-GB`、`zh-CN`、`fr-FR`、`ru-RU`、`sv-SE`、`hi-IN`；
   **已测出零填充/分隔符差异**的：`ja-JP`（ICU4X `2024/01/02 3:04:05` vs Node `2024/1/2 3:04:05`）、
   `de-DE`（`02.01.2024` vs `2.1.2024`）、`da-DK`（`02.01.2024` vs `2.1.2024`）、`es-ES`（`03:04:05` vs `3:04:05`）、
   `ko-KR`（`2024/1/2 오전` vs `2024. 1. 2. 오전`）。差异按 locale 记入 §7.4 的诚实清单，并在测试里按「本地化文本不比字节」处理。
   体积实测：`/tmp/icumin` 的 `default-features=false`（只留 `compiled_data`）构建，空程序 288,456 B、加 `icu_datetime` + `icu_decimal` 后 1,486,264 B，即**增量 1.20 MB**；需要更多字段/locale 的格式化器时还会再增少量数据。
2. **locate 解析**：`LC_ALL` > `LANG` > `en-US`，只取 `ll_CC`/`ll-CC` 前缀，非法值回退。
3. **`--no-default-features`**：不引入 ICU4X，一律 en-US 手写格式（`chrono` 格式化）；
   体积 −1.2 MB，适合极简分发。

`Intl.Collator`/`PluralRules`/`RelativeTimeFormat` 不实现；`localeCompare` 回退字节序
（`ca-extensions.md` §6.1 已接受该差异，语料只有 ASCII 排序）。

### 6.5 `fetch` 与 Web Streams

`custom-provider-anthropic` 的 `streamSimple` 用 `fetch` + `response.body.getReader()`（SSE）；
`doom-overlay/wad-finder.ts` 下载 WAD。实现：

- Rust `reqwest` 发起请求；返回 JS `Response`：`status statusText ok url headers`、
  `text()/json()/arrayBuffer()`（Promise）、`body` 为 `ReadableStream`。
- `Headers` 必须**大小写不敏感**（初版用 `Map` 存原 key，导致 `.delete(name)`/`.append(name)` 命中不同键，
  SDK 报「not a function」式错误）；`headers.entries()` 供 SDK 读 `request-id`。
- `ReadableStream`：`start/pull/cancel`、`getReader()`、`[Symbol.asyncIterator]`；
  **`pull` 可能在 `read()` 注册 waiter 之前同步 `enqueue`**，实现必须在「挂 waiter 后重新 pump」，
  否则整个 `for await` 静默挂起（`/tmp/sbprobe` 第一版正是此 bug）。
- `TextDecoder` 支持 `{stream:true}`，SSE 必须按块 flush（不能缓冲到响应结束）。

### 6.6 `structuredClone`

`Object/Array/Date/Map/Set/ArrayBuffer/TypedArray/DataView/RegExp`、循环引用、`undefined` 属性；
`Function/Symbol/WeakMap/Promise` 抛 `DataCloneError`（文案对齐 V8）。`debug-provider.ts` 用它克隆
`provider_stream_event` 的任意 provider 对象，必须能处理嵌套宿主对象。

---

## 7. 兼容性判定

口径同 `ca-extensions.md` §3.11：A = 原样可用；B = 可运行有可观察差异；C = 需宿主能力/特性；
D = 不可等价。

### 7.1 单文件示例（70 个）

**A 级 70 / 70**（在 §6 的 shim 与 §6.4 的 locale 覆盖到位后）：

`auto-commit-on-exit bash-spawn-hook bookmark border-status-editor built-in-tool-renderer claude-rules
commands confirm-destructive custom-compaction custom-footer custom-header debug-provider dirty-repo-guard
dynamic-tools entry-renderer event-bus file-trigger git-checkpoint github-issue-autocomplete
git-merge-and-resolve handoff hello hidden-thinking-label inline-bash input-transform input-transform-streaming
interactive-shell jev-router mac-system-theme message-renderer minimal-mode modal-editor model-status notify
overlay-qa-tests overlay-test permission-gate pirate preset project-trust prompt-customizer protected-paths
provider-payload qna question questionnaire rainbow-editor reload-runtime rpc-demo send-user-message
session-name shutdown-command snake space-invaders ssh status-line structured-output summarize
system-prompt-header tic-tac-toe timed-confirm titlebar-spinner todo tool-override tools trigger-compact
truncated-tool widget-placement working-indicator working-message-test`

不含 locale 覆盖时：66 A + 4 B（`custom-compaction`、`input-transform`、`entry-renderer`、`overlay-qa-tests`）。
这 4 个正是 locale 差异项。

### 7.2 目录示例（9 个）

| 示例 | 判定 | 依据 |
|---|---|---|
| `with-deps/` | A | `ms@2.1.3` 纯 CJS（`/tmp/cjsmod`）；typebox 内嵌 |
| `dynamic-resources/` | A | `node:path`/`node:url` + `resources_discover` |
| `plan-mode/` | A | `/tmp/loadprobe`：strip 0.44 ms、declare 1.0 ms、eval 0.09 ms、factory 0.03 ms；注册 22 项 |
| `subagent/` | A | `spawn`+`readline`+`fs`+`os`+`Buffer`；`process.argv[1]`/`execPath` 指向 kiana |
| `sandbox/` | A（需 §7.3 的真实 TCP + 真 CJS） | `/tmp/sbprobe` 664 模块 / 921 KB / 75–83 ms、0 语法失败、`zod` 校验与 Node 逐字一致；`initialize()` 停在 bridge socket 仅因缺真实 `net` |
| `custom-provider-gitlab-duo/` | A | `/tmp/loadprobe` 可加载并注册 provider；走 `kiana-ai` 虚拟模块的 Rust 实现 |
| `custom-provider-anthropic/` | A（需 shim；SDK 已实测） | `/tmp/sbprobe`：`@anthropic-ai/sdk@0.52.0` CJS（43 模块 ~18 ms）与 ESM（43 模块/178 KB ~12 ms）两入口都通；构造、`messages.create`、`messages.stream` SSE 全链路与 Node 控制组逐字一致 |
| `doom-overlay/` | C | 需 `WebAssembly` + Emscripten FS；`wasmtime` 特性 + shim，默认关闭 |
| `gondolin/` | D | `ssh2`（可选 `cpu-features` 原生 addon）+ `cbor2` + `undici` + 原生 runner；默认不可用 |

小计：**7 A / 1 C / 1 D**。

### 7.3 `sandbox/` 的真实 TCP 前提

`@anthropic-ai/sandbox-runtime@0.0.26` 的 `dist/sandbox/http-proxy.js` 直接
`import { createServer } from 'node:http'`、`connect from 'node:net'`；依赖 `@pondwader/socks5-server@1.0.10`
（CJS，`require("net")`）会 `net.createServer`/`net.createConnection`。这些是**真实 TCP 服务器**：
`initialize()` 会 `listen(0,'127.0.0.1')` 读 `server.address().port`，把端口写进 `HTTP_PROXY`/`ALL_PROXY`
传给被沙箱化的子进程。假端口会让 `npm install`/`git clone` 连向不存在的代理。因此必须实现：
`createServer().on("request"/"connect")`、`listen(port, host, cb)`、`address()`、`unref()`、
`net.connect`/`createConnection`、`Socket.pipe`、`Readable`/`Writable` 事件。工作量 1–2 天，无研究风险。

### 7.4 「不能等同」的诚实清单

1. **`Intl`**：`Segmenter` 用 `unicode-segmentation` 近似 ICU（Indic conjunct、emoji ZWJ、区域指示符、
   `\r\n`、Prepend 可能有差异，用夹具校准）；`Collator`/`PluralRules` 不实现；
   `DateTimeFormat`/`toLocaleString` 仅支持 §6.4 列出的 locale，部分 locale 有零填充差异。
2. **原生 `.node` addon**：无法加载；`createRequire` 只能加载 JS/CJS；`gondolin` 因此为 D。
3. **WebAssembly**：默认无；`wasmtime` 特性提供 `WebAssembly.Module/Instance/instantiate`，
   但 Emscripten 的 `FS_createDataFile`/`process` 假设仍需 shim，`doom-overlay` 大概率仍失败。
4. **Node 流语义**：`highWaterMark`/`pause`/`resume`/背压与 Node 不完全一致；语料只用 `on("data")`/`for await`。
5. **错误文本**：V8 的 `TypeError` 文案（`Cannot read properties of undefined (reading 'x')`）与 QuickJS 不同；
   `ca-extensions.md` §3.5 只能保证语义一致，不能逐字一致。**扩展 API 自己的错误文案**（§4.3/§4.9）逐字。
6. **`Function.prototype.toString`**：格式与 V8 不同。
7. **`process.versions.node = "22.0.0"`** 是伪造，个别包会走 Node 专属分支。
8. **`console.log` 走 stderr**（TUI 模式），Node 走 stdout。
9. **`worker_threads`/`node:vm`/`node:cluster`/`tls`**：不提供。
10. **`process.exit()`** 是协作式退出（抛 `ExitSignal`）。
11. **同代相对依赖共享**：`moduleCache:false` 的严格等价需 `KIANA_EXT_STRICT_NOCACHE=1`（§3.4）。
12. **`class extends Container` 的 `instanceof`** 只在 JS 侧成立；跨语言边界（Rust 组件树）不成立（§5.1）。

---

## 8. 虚拟模块运行期导出面

### 8.1 统计口径

86 个 `.ts` 经 oxc strip 后的 `import { ... }` 值（`type`-only 导入整条删除，不占实现成本）。
括号内为出现次数；子路径（`kiana-ai/models`/`oauth`/`providers/all`、`typebox/compile`/`value`、
`@sinclair/*`）在语料中**零使用**，但虚拟键必须存在（第三方扩展可能用）。

### 8.2 `@earendil-works/kiana-ai`（= `/compat`，同一 namespace）

| 运行期符号 | 出现 | 实现 |
|---|---|---|
| `Type` | 1 | typebox 的 `Type` namespace（内嵌 1.3.27，与依赖域里的同一对象） |
| `StringEnum` | 3 | JS：`Type.Unsafe({ type:"string", enum: values })`（原样搬运 `typebox-helpers.ts`） |
| `uuidv7` | 3 | JS：40 行单调时间戳 + 随机 |
| `createAssistantMessageEventStream` | 2 | JS：`event-stream.ts` 原样搬运 |
| `calculateCost` | 1 | Rust：`kiana-ai` 定价表 |
| `collapseSystemMessages` / `getCurrentSystemPrompt` / `getCurrentTools` | 各 1 | Rust：读当前会话状态 |
| `anthropicMessagesApi` / `openAIResponsesApi` / `registerApiProvider` / `streamSimple` | 各 1 | Rust：`kiana-ai` provider 注册表，包成 JS 函数 |
| `ToolCallArgumentsStream` | （NB 统计有，语料 `structured-output.ts` 用） | JS：增量 JSON 参数解析 |

`kiana-ai/models`/`oauth`/`providers/all` 各自独立 namespace，导出 Rust 的 models/oauth/provider 表（供
`11156-kiana-ai-models-virtual-module.test.ts` 的身份断言）。

### 8.3 `@earendil-works/kiana-tui`

19 个运行期符号见 §5.1。附加：`Marked`/`Token`/`Tokens` 是 `index.ts:3` 的再导出，语料不使用；
若第三方扩展用 `new Marked()`，返回一个抛 `Error("marked is not bundled in kiana")` 的桩
（比静默 undefined 更诚实）。

`Container` 的 JS 实现必须逐行对齐 `tui.ts:358–429`：`children: Component[]`、`addChild`、`removeChild`、
`clear`、`invalidate`（逐子调用）、`handleMouse`（按其 `mouseLayout` 按 y 分派）、`render`（拼接子行并记录
`mouseLayout`）。`Stack`/`VStack`/`HStack` 对齐 `components/stack.ts`（`basis/grow/shrink/minSize/maxSize/visible`、
`gap`、`align`、`allocateStackSizes`、`compositeTuiLine`）——语料只 `new Container()`，但运行时值必须齐全。

### 8.4 `@earendil-works/kiana-agent-core`

strip 后**无运行期导出**（`AgentMessage`/`AgentToolResult`/`ThinkingLevel` 都是类型）。`handoff.ts`、
`plan-mode/index.ts`、`subagent/index.ts` 只用 `import type`。因此 namespace 为空对象即可，
但键必须存在（否则 `import type` 被删后不会报错，纯值导入若有则报 `Could not find export`）。

### 8.5 `typebox` / `typebox/compile` / `typebox/value` / `@sinclair/*`

内嵌 typebox 1.3.27 的 `build/index.mjs`（607 B 桶文件）+ 690 个 `.mjs`（865 KB）。
`Type.*` 在语料中的用法：`String`(30)、`Object`(20)、`Optional`(19)、`Array`(6)、`Number`(3)、`Boolean`(2)、
`Literal`/`Union`/`Unsafe` 少量。
`typebox/compile` 提供 `Compile`（§3.7 的 JS 校验路径）；`typebox/value` 提供 `Value.Convert`。
嵌入方式见 §3.6；**求值一次**（单 Context 模块缓存），每个 schema 的 `Compile` 按 schema 对象缓存。

### 8.6 `@earendil-works/kiana-coding-agent`（34 个运行期导出）

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
| 工具工厂 | `create{Bash,Edit,Write,Read,Find,Grep,Ls}Tool(Definition)` | Rust 构造 `ToolDefinition` JS 对象；`parameters` 由 JS typebox 构造一次（§3.7）；`execute` 是 Rust 函数包装 |
| 常量 | `CONFIG_DIR_NAME` `VERSION` `DEFAULT_MAX_BYTES` `DEFAULT_MAX_LINES` `getAgentDir` | 直接值 / Rust 函数 |
| 纯函数 | `convertToLlm` `serializeConversation` `formatReadLineRange` `formatSize` `truncateHead` `truncateLine` `parseFrontmatter` `keyHint` `defineTool`（恒等） `withFileMutationQueue`（per-path Mutex） | Rust；`defineTool` 一行恒等 |
| 组件 | `CustomEditor` `BorderedLoader` `DynamicBorder` | Rust 原生类（§5.1、§5.4） |
| 主题 | `getMarkdownTheme` `getSettingsListTheme` | Rust 从当前主题生成 JS 普通对象 |

`kiana` manifest（`readKianaManifest`）与发现规则由 `ca-extensions.md` §4.1/§4.2 规定，本设计不重复。

---

## 9. prelude 与安装顺序

### 9.1 内容与体积

| 文件 | 内容 | 估算 |
|---|---|---|
| `js/prelude.js` | Promise 协议、定时器、AbortSignal、Buffer、process、console、EventEmitter、CJS 加载器、Node shim 门面、`structuredClone` | ~35 KB |
| `js/intl.js` | `Intl.Segmenter`/`NumberFormat`/`DateTimeFormat` 与 `toLocale*` 覆盖（调用 Rust） | ~6 KB |
| `js/api.js` | `makeExtensionApi`：41 on、31 方法、事务三态机、注册表 | ~18 KB |
| `js/context.js` | `makeContext`（三类 ctx 的差异在此分派） | ~6 KB |
| `js/dispatch.js` | 13 个 emitter 的逐行移植 | ~14 KB |
| `js/virtual/*.js` | 22 个键的导出面 | ~8 KB |
| `js/ui/*.js` | `Container`/`Stack`/`VStack`/`HStack`、`Key` 常量、`CURSOR_MARKER`、`renderFakeCursor`、`defineTool` | ~5 KB |

合计 ~92 KB，`include_str!` 进二进制；`Module::declare` 约 7 ms（78 ns/byte 基线），一次性。
**不做 esbuild 打包、不做字节码预热**（源文件本身就是 ESM，QuickJS 直接 declare 即可）。

### 9.2 安装顺序（严格，JS 案的教训）

```text
1. Rust 建 Runtime/Context，设 limits/interrupt/rejection tracker
2. 注入 __kiana_host（Object，挂全部同步原语）与 __kiana（Promise 协议）
3. declare + evaluate js/intl.js          ← 必须最早：任何模块顶层触碰 Intl/toLocale* 都在其后
4. declare + evaluate js/prelude.js       ← 其余全局与 CJS 加载器
5. declare 22 个虚拟模块源 + node:* shim 源（惰性，import 时求值）
6. 求值 typebox（一次）→ namespace 存 Persistent
7. declare + evaluate js/api.js、js/context.js、js/dispatch.js
8. 加载扩展：resolver → oxc/CJS → declare → eval → 取 default → 调 factory
```

第 3 步先于第 4 步是硬要求：`Date.prototype.toLocaleString` 的覆盖与 `Intl` 的安装必须在任何
可能捕获它们引用的模块之前完成（JS 案的原型正是在这里踩坑：kiana-tui 顶层 Segmenter 先于 polyfill 执行）。

### 9.3 prelude 的自我约束

- prelude 只依赖 `__kiana_host`/`__kiana` 两个全局，不 import 任何虚拟模块。
- 不在 prelude 里放业务状态（派发快照、注册表都在 `api.js`/`dispatch.js`）。
- `Object.defineProperty` 安装的全局属性一律 `enumerable: false, configurable: true`（覆盖 `toLocale*` 例外：
  `writable: true`，与原原型一致）。

---

## 10. 测试与验收

### 10.1 语料测试（`crates/kiana-extensions/tests/corpus.rs`）

1. 86 个 `.ts` 全部 strip + declare 成功（0 诊断）；
2. 全部 79 个入口（70 单文件 + 9 目录）执行工厂，断言注册项 key 集合与 `ca-extensions.md` §3.11 矩阵一致；
3. 25 个代表（`ca-extensions.md` §7.7 + `plan-mode`/`subagent`/`custom-provider-*`）断言关键行为。

### 10.2 事件语义测试（`tests/dispatch.rs`）

13 个 emitter 各至少 4 个用例：正常合并、`undefined` 返回、handler 抛错、多扩展顺序；另加：

- `emitBoundary` 的 `valid=false` 修复路径与 `Invalid boundary entries:` 文案；
- `emitToolResult` 的 `structuredContent` 删除（内容替换且未给 structuredContent → 键消失）；
- `emitMessageEnd` 同角色规则与非 Error 抛出（`String(err)`）；
- `emitUserBash` 的 XOR 校验与向上抛；
- `emitContext` 的 `sameMessages` 引用相等（handler 原地改 vs 返回新数组）与 leading system 警告；
- `emitBeforeProviderHeaders` 原地修改；
- `emitInput` 的 `handled` 短路与 `transform` 链；
- `snapshotEventHandlers` 的嵌套派发重新取快照。

### 10.3 TUI 渲染夹具（`tests/render.rs`）

6 个样本（纯 ASCII / ANSI / CJK / emoji ZWJ / 长串 / 含 tab 换行）在 `render(20)`、`truncateToWidth(18, "…")`、
`visibleWidth` 三个函数上与 Node 逐字一致（JS 案 §9.2 的夹具，验收 Rust 侧实现）：

```
hello world              -> w=11  render=["hello world         "] trunc="hello world"
\x1b[31mred\x1b[0m text  -> w=8   render=["\x1b[31mred\x1b[0m text            "]
漢字かなカナ한글          -> w=16  render=["漢字かなカナ한글    "]
a👍🏽b👨‍👩‍👧               -> w=6   render=["a👍🏽b👨‍👩‍👧              "]
"x"*60                   -> w=60  render=3 行×20；trunc="xxxxxxxxxxxxxxxxx\x1b[0m…\x1b[0m"
"line1\nline2\ttab"       -> w=16  render=["line1               ", "line2   tab         "]
```

另有：`Text/Box/Container` 组合的 ANSI 字节；`Theme` 全部 token 的 `getFgAnsi`；对话框栈 `render(80)`
（`→ ` 前缀、`(esc to cancel)`、倒计时 `(5s)` 递减、宽度截断）；widget 10 行截断与
`"... (widget truncated)"`；editor 子类 `super.render()` + `borderColor`（§5.2 的
`native-top/sub/renderTopBorder` 原型直接转成用例）。

### 10.4 真实 npm 包回归（CI 必跑）

1. **多文件 CJS**：`import shellquote from "shell-quote"` → `shellquote.quote(["a b"])` 为 `'a\\ b'`；
   断言 `__kiana_cjs` 含 `quote.js`/`parse.js`（只测 `ms` 会漏，因为 `ms` 自包含）。
2. **`zod@3.25.76`**：`NetworkConfigSchema.safeParse({allowedDomains:'nope'}).error.issues[0].code === "invalid_type"`；
   `getDefaultWritePaths()` 10 项。
3. **`@anthropic-ai/sdk@0.52.0` + SSE**：fake `fetch` 返回 6 个 SSE chunk，断言
   `messages.stream().on("text")` 收到 `["hello"]`、`finalMessage().content[0].text === "hello"`、
   **`AbortController.abort()` 调用次数为 0**（抓「定时器立即触发」bug）；Node 控制组输出必须相同。
4. **反例断言**：`setTimeout(cb, 10)` 不得在微任务阶段执行；`ReadableStream` 的 `pull` 同步 `enqueue` 后
   `read()` 必须拿到值（不得挂起）。

### 10.5 引擎与生命周期

- 死循环 → 100 ms 内 `interrupted`；OOM → `ExtensionError` 且其他扩展不受影响；
- `/reload` 期间旧 ctx 调用 → stale 文案；旧代 `kiana`/`ctx` 全部失效；
- 1000 帧连续渲染 → Persistent 计数稳定（`run_gc` 后）；
- 关闭顺序：故意保留一个 `Persistent` → 必须由 CI 断言「不 abort」（清理后 exit=0）；
- 定时器 7 个 + Promise 链在 31 ms 内完成；handler await 期间渲染仍返回。

### 10.6 验收清单（对齐 `ca-extensions.md` §8.8）

- [ ] 41 个事件全部可 `on`，13 个 emitter 合并语义逐条通过；
- [ ] `ExtensionAPI` 31 方法 + `events`；`ExtensionUIContext` 28 成员；`ExtensionContext` 18 成员；
      `ToolDefinition` 20 字段；`ToolRenderContext` 14 字段；
- [ ] 22 个虚拟键全部可 import，且同键多次 import 身份一致；
- [ ] §4.3 的全部【逐字】文案与 §4.9 的三个错误落点；
- [ ] 79 个入口加载 + 注册矩阵；
- [ ] 70 个单文件 A 级（含 locale 覆盖）；9 个目录 7 A / 1 C / 1 D；
- [ ] §10.3 的字节夹具与 §10.4 的四个包级回归；
- [ ] `sandbox/initialize()` 通过（真实 `net`）；`custom-provider-anthropic` 真 key 下 streaming 可用。

---

## 11. 实施顺序（每步可独立验收）

1. **引擎骨架**（0.5 周）：`engine/{mod,loop_,promise,limits,shutdown}.rs` + `js/prelude.js` 的 Promise/定时器/
   AbortSignal；跑 `/tmp` 原型等价用例（TLA、await 期间渲染、JS→宿主→JS 重入、关闭顺序）。
2. **TS 加载**（0.5 周）：`transpile.rs` + `resolver.rs` + `loader_impl.rs` + `cjs.rs`；跑 86 文件 strip+declare，
   与 `ms`、多文件循环 CJS 两个用例。
3. **虚拟模块 + prelude 安装顺序**（0.5 周）：22 键 + typebox 内嵌 + `intl.js` 先行；跑身份与 `Type.Object` 用例。
4. **API 与派发**（1 周）：`js/api.js`、`js/context.js`、`js/dispatch.js` + Rust `host.rs`；跑 §10.2 全量。
5. **Node shim**（1.5 周）：`fs/path/child_process/os/url/util/zlib/readline/module` + 全局 + 真 CJS `require`；
   跑 §10.4 的 1/2/3。
6. **UI 上下文与原生组件**（1 周）：`Text/Box/Spacer/Input/SelectList/SettingsList/Markdown/Loader/
   ScrollView/Image/TruncatedText/DynamicBorder/BorderedLoader/CustomEditor` + 对话框栈 + widget/footer/header；
   跑 §10.3 全量 + `custom-footer`/`todo`/`overlay-test`。
7. **Provider / MCP / VirtualModel**（0.5 周）：`streamSimple` + `AssistantMessageEventStream`；跑
   `custom-provider-gitlab-duo` 与 SDK SSE。
8. **`node:net`/`node:http` + 真实代理**（0.5 周，为 `sandbox/`）：tokio `TcpListener`/`TcpStream` →
   `Server`/`Socket`；验收 `sandbox-runtime.initialize()`。
9. **locale 数据**（0.3 周）：ICU4X 接入 + 20 locale 夹具；无特性构建回归。
10. **可选特性**：`wasmtime`（doom）、`ssh2` 纯 JS 路径（gondolin 降 C）。

总计约 5.3 周（不含可选特性）。

---

## 12. 残余风险与待决问题

| # | 问题 | 影响 | 建议 |
|---|---|---|---|
| Q1 | `@anthropic-ai/sdk@0.52.0` 的真实网络行为（TLS/HTTP2/重试/真实 SSE 分块）未在 QuickJS 下验证 | `custom-provider-anthropic` 的 A 级 | 协议层已实测；剩余是 `fetch` 实现质量（按块 flush、`headers.entries()`）。真 key 前先用 `kiana-ai/compat` 的 Rust 路径对照 |
| Q2 | `sandbox/` 的真实 TCP 是必须项 | `sandbox` 的 A 级 | 语法/模块兼容已排除；剩余 1–2 天 tokio `TcpListener` → `net.Server` |
| Q3 | `Intl.Segmenter` 的 UAX#29 与 ICU 在部分脚本上的差异 | 宽度/截断的字节夹具 | 夹具校准；差异记入 §7.4 |
| Q4 | locale 零填充差异（ja/ko/pl/tr 等） | 4 个单文件示例的可见文本 | 按 locale 记清单；测试不比字节；必要时补 ICU4X 字段微调 |
| Q5 | 原生类字段只能经访问器或 JS 包装层（§5.3） | 组件 API 面 | 逐一列出 28 个字段的访问器；`Input.focused`、`SelectList.onSelect` 等必须有 setter |
| Q6 | 单线程下 JS 死循环冻结 UI | 体验 | 100 ms 看门狗；文档说明 |
| Q7 | `gondolin` 是否值得支持 | 1 个示例 | 默认 D |
| Q8 | `typebox` 内嵌 865 KB 的维护成本 | 升级路径 | 只跟 `packages/coding-agent/package.json` 的固定版本；升级时重跑 §10.4 |

---

## 附录 A：本设计用到的原型（均在 `/tmp`，不进入 `kiana`）

| 目录 | 验证内容 |
|---|---|
| `/tmp/rqprobe` | 模块缓存/循环导入/身份、Resolver/Loader 签名、`import.meta.url` |
| `/tmp/tdzprobe` | `Resolver` 的 `base` 语义、Loader 导出可见性 |
| `/tmp/qprobe` | `?v=N` 实现 `moduleCache:false`（同名两次执行、相对依赖共享） |
| `/tmp/loop_probe` | 事件循环、定时器、Promise settle、同步渲染、重入、错误转换 |
| `/tmp/stream_probe` | 宿主推 chunk → JS `for await`、AbortSignal、timer |
| `/tmp/xthread` | 跨线程渲染延迟（27.0 / 279.6 µs；本轮复测 32.5 / 314.8 µs） |
| `/tmp/classprobe`、`/tmp/ext-proto/.../thisclass.rs` | 原生类 + JS 子类 + `super` + `instanceof` |
| `/tmp/sendchk`（本轮） | `Persistent` !Send；`This` 虚拟派发；原生构造函数丢字段；原生方法设字段；Persistent 关闭顺序 |
| `/tmp/minfeat`（本轮） | `default-features=false, features=["std","loader","macro"]` 足够（1,746,544 B） |
| `/tmp/cjsmod`、`/tmp/sbprobe` | `ms` CJS；664 模块依赖图、`zod`、SDK CJS/ESM、SSE、真 CJS `require` 的四个坑 |
| `/tmp/tsprobe2`、`/tmp/ext-proto/stripbench` | 86 文件去类型（12.1–46.8 ms 两种口径） |
| `/tmp/featprobe` | QuickJS 内建能力全表（Intl/WebAssembly/TextEncoder/...） |
| `/tmp/loadprobe` | 10 个真实扩展的 strip/declare/eval/factory |
| `/tmp/tbtest` | typebox 1.3.27（691 `.mjs` / 865 KB）+ marked；287 导出、JIT 可用 |
| `/tmp/tbtest` 的 `deprealm2`、`depcost2` | 每 Context 重跑 41.2 ms；依赖域 46.8 ms + 0.85–1.07 ms/扩展（本设计因单 Context 不需要） |
| `/tmp/icusz`（本轮） | ICU4X 2.4.0 与 Node 的 locale 对照（en-US 等逐字一致，ja/ko 等有零填充差异） |
| `/tmp/oxcsize`、`/tmp/combined` | 二进制体积 |

复现（示例）：

```bash
source ~/.cargo/env
cd /tmp/minfeat && cargo build --release && ./target/release/minfeat      # 最小特性
cd /tmp/sendchk && cargo build --release && ./target/release/sendchk      # This / 构造字段 / teardown
cd /tmp/sbprobe && cargo build --release
./target/release/sbprobe                                                  # sandbox-runtime + zod
./target/release/sbprobe /tmp/sdktest/node_modules/@anthropic-ai/sdk/index.mjs   # SDK + SSE
cd /tmp/icusz && cargo run --release --bin dt3                            # locale 对照
```

## 附录 B：与三份候选设计的差异汇总

| 议题 | RF | NB | JS | 本设计 |
|---|---|---|---|---|
| 线程 | 独立 JS 线程 + 同步桥 | 全主线程 | async runtime 多线程 | **全主线程**（RF 偏离，NB 采纳） |
| rquickjs 特性 | `full-async/loader/macro/parallel` | `std/loader/macro` | `full-async/parallel` | **`std/loader/macro`**（NB 采纳，实测足够） |
| 事件派发语义 | Rust 侧 `emit_*` | Rust 侧 `dispatch` | JS 侧逐行移植 | **JS 侧**（JS 采纳） |
| 组件实现 | Rust 原生类 | Rust 原生类 + JS 容器 | kiana-tui 全量 JS | **Rust 原生类 + JS 容器**（RF/NB 主，JS 的容器与夹具） |
| typebox | 手写 builder | 真包 + 依赖域 | 真包 | **真包、单 Context 求值一次**（NB/JS 采纳，依赖域拒绝） |
| 每扩展 Context | 否（共享） | 是 + 依赖域 | 否（单 Context） | **单 Context** |
| CJS | 真 `require` | 真 `require`（预登记循环） | 包装器/bundle | **真 `require` + IIFE**（RF/NB 合流，JS 的 `new Function` 语义） |
| locale | en-US 固定 | en-US 固定 | en-US 固定 | **环境 locale + ICU4X（可选）**（三案遗漏） |
| 原生构造字段 | 未提 | 未提 | 未提 | **访问器 + JS 包装层**（本轮新证） |
| 二进制 | 3.94 MB | 1.58 / 3.09 MB | 未给总量 | **≤ 5.2 MB**（含 ICU4X） |
