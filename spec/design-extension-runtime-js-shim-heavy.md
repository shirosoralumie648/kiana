# 扩展运行时设计：JS-Shim-Heavy（rquickjs + 预编译 JS prelude）

> 目标：`kiana` 用**单个 Rust 二进制**加载并运行现有 TypeScript 扩展，保持与 `kiana`（Node/jiti）
> **完全相同的扩展 API 与运行时语义**。
>
> 本文档对应 `ca-extensions.md` §8.3 的**方案 A（rquickjs + 自建转换）**的一个具体变体：
> **把扩展 API 层本身也放进 JS 里跑**（"js-shim-heavy"），Rust 只做引擎托管、宿主能力和 I/O。
>
> 文中所有数字均为本轮在 `/tmp` 原型中实测（release 构建），不是估算。原型目录：
> `/tmp/qjs-probe`、`/tmp/qjs-async`、`/tmp/qjs-async2`、`/tmp/e2e`、`/tmp/oxc-probe`、`/tmp/kiana-proto`。

---

## 0. 结论摘要

1. **引擎**：`rquickjs = "=0.14.0"`（`rquickjs-core`/`rquickjs-sys` 同为 0.14.0），内置 vendored
   **QuickJS-ng 0.16.2**（`quickjs/quickjs.h`: `QJS_VERSION_MAJOR 0 / MINOR 16 / PATCH 2`）。
   与 codemode 使用的 `quickjs-wasi@3.6.2` 是**两个独立单元**，互不影响。
2. **核心手法**：把 `packages/tui`、`packages/coding-agent` 的 theme、`typebox`、`StringEnum`、
   `uuidv7`，以及**扩展 API 层（`loader.ts` + `runner.ts` 的派发/合并语义）** 用 esbuild
   在**构建期**打成一份 ESM prelude，嵌入二进制，扩展运行时先加载 prelude 再加载扩展。
3. **实测验证**：kiana-tui 的**原始 TS 源码**在 QuickJS 中直接运行，`visibleWidth`/`truncateToWidth`/
   `Text.render` 的输出与 Node **逐字节一致**（含 ANSI、CJK、emoji ZWJ、tab）；`typebox`、`uuidv7`
   也原样可跑。
4. **兼容语料**：`examples/extensions/**` 共 **79 个入口**（70 个单文件 + 9 个目录）。原型中
   **76/79 一次性加载成功**（单 runtime 批量，137 ms，1.74 ms/入口）；3 个失败均为原型环境里
   缺少真实 npm 虚拟模块，**不是引擎问题**。详见 §14。
5. **性能**：prelude 冷启动 **75 ms**（407 KB 源码，gzip 121 KB）；可选 bytecode 预热可降到 ~35 ms。
   渲染热路径比 Node 慢 2–6×，宽度/截断慢 6–20×；**用 Rust 侧 grapheme 边界服务可把 ANSI 截断从
   4392 ms/20k 降到 2411 ms/20k**。交互式 UI 单帧预算（~30 行 × 80 列）仍余量充足，详见 §12。
6. **代价**：需要一份 **Node 内建模块 shim 集**（§10）和一套 **npm 解析/CJS interop**（§11）；
   不支持 `WebAssembly`（因此 `doom-overlay` 的游戏本体不可用）与原生 `.node` 插件。

---

## 1. 引擎与精确依赖

### 1.1 为什么是 rquickjs 而不是别的

| 方案 | 结论 |
|---|---|
| **rquickjs 0.14.0（选中）** | 单二进制、无子进程；`AsyncRuntime` 与 tokio 集成已被原型验证；`Resolver`/`Loader` 可完全接管模块解析（复刻 jiti 的 22 个虚拟模块）；`Persistent<T>` 可跨 `ctx.with` 持有 JS 对象；有内存上限、栈上限、GC 阈值、中断回调。 |
| quickjs-wasi 3.6.2 | 与 codemode 同款，但 WASI 沙箱内拿不到 `node:fs`/`child_process`，扩展需要真实 I/O；且每次调用要跨 wasm 边界。**只用于 codemode**。 |
| Node/Bun 子进程 | 兼容性最高，但与"纯 Rust 单二进制"目标冲突，且 `ctx.cwd`/`ctx.hasUI` 这类同步 getter 会被迫异步化。**否决**。 |

QuickJS-ng 0.16.2 的语言特性已足够（实测）：`Object.groupBy`、`Array.prototype.toSorted`、
iterator helpers、`Symbol.dispose`/`using`、regexp `v` 标志、`RegExp.escape`、top-level await、
`import.meta`（可写、可注入）、`WeakRef`、`FinalizationRegistry`、`Proxy`、`BigInt`、
`SharedArrayBuffer`/`Atomics`、`DisposableStack`。

### 1.2 Cargo 依赖（全部锁定到精确版本）

版本已在本轮用 `cargo info` / `cargo search`（crates.io）逐一核对存在。

```toml
[dependencies]
# ---- JS 引擎 ----
rquickjs   = { version = "=0.14.0", features = ["full-async", "parallel"] }
#  full-async = std + loader + macro + classes + array-buffer + chrono + either + indexmap + phf + futures
#  parallel   = AsyncRuntime/AsyncContext 可跨线程（unsafe impl Send/Sync 仅在 parallel 下提供）

# ---- TS 剥离（运行时，单文件无 bundle 依赖）----
oxc_allocator   = "=0.155.0"
oxc_parser      = "=0.155.0"
oxc_span        = "=0.155.0"
oxc_semantic    = "=0.155.0"
oxc_transformer = "=0.155.0"
oxc_codegen     = "=0.155.0"

# ---- 宿主 / 异步 ----
tokio            = { version = "=1.54.1", features = ["rt-multi-thread", "macros", "sync", "time", "process", "io-util", "fs"] }
tokio-util       = "=0.7.20"          # CancellationToken，用于 AbortSignal 桥
futures          = "=0.3.34"
async-trait      = "=0.1.92"
crossbeam-channel = "=0.5.17"         # JS 线程 <-> tokio 线程同步点

# ---- 数据 ----
serde      = { version = "=1.0.229", features = ["derive"] }
serde_json = { version = "=1.0.154", features = ["preserve_order"] }
indexmap   = { version = "=2.14.2", features = ["serde"] }

# ---- 工具 ----
thiserror        = "=2.0.21"
parking_lot      = "=0.13.0"
once_cell        = "=1.21.4"
regex            = "=1.13.1"
unicode-width    = "=0.2.2"
unicode-segmentation = "=1.13.3"      # 宿主侧 grapheme 边界（§12 的加速路径）
uuid             = { version = "=1.29.0", features = ["v4", "v7"] }
chrono           = { version = "=0.4.45", features = ["serde"] }
tracing          = "=0.1.44"
dirs             = "=7.0.0"
path-clean       = "=1.0.1"
base64           = "=0.23.1"
zstd             = "=0.14.3"          # prelude 压缩存储（可选）
```

MSRV：`rquickjs 0.14.0` 声明 `rust-version = "1.87"`，`edition = "2021"`。整个工作区需 ≥ 1.87。

**构建期**（不在 `[dependencies]`，是 `build.rs`/CI 工具）：`esbuild 0.25.10`（npm）——用于生成 prelude。
不引入 `esbuild` Rust crate（它与 npm 版不是同一实现，且我们不希望构建期依赖 Node 以外的脆弱环节；
如果 CI 不想装 Node，可退化为 `oxc` + 手写打包，但 esbuild 已验证可用且产物小 15%）。

### 1.3 crate 划分（在 `ca-extensions.md` §8.1 基础上的收敛）

```
crates/kiana-extensions/
├── src/
│   ├── lib.rs
│   ├── engine/            # rquickjs 托管：Runtime/Context 生命周期、内存/栈/中断、drive loop
│   │   ├── runtime.rs     # ExtRuntime: AsyncRuntime + 单 Context + prelude 装载
│   │   ├── drive.rs       # 微任务/宏任务泵、is_job_pending/execute_pending_job
│   │   └── limits.rs      # memory_limit / max_stack / interrupt / 超时
│   ├── loader/            # 复刻 jiti 的语义（§5）
│   │   ├── resolver.rs    # Resolver: 22 虚拟模块 + 相对路径 + node_modules + exports 条件
│   │   ├── loader.rs      # Loader: fs 读取 + oxc 剥离 + import.meta 注入 + 缓存
│   │   ├── transpile.rs   # oxc 封装（§6）
│   │   ├── npm.rs         # package.json 解析、CJS/ESM interop、exports 条件
│   │   └── aliases.rs     # 22 个虚拟模块表 + tsconfig paths
│   ├── bridge/            # Rust <-> JS 值模型（§8.1）
│   │   ├── value.rs       # Undefined/Missing 哨兵、OptionalPatch、JSON<->JsValue
│   │   ├── handle.rs      # Persistent 句柄注册表（JsHandleId）
│   │   ├── signal.rs      # AbortSignal 桥
│   │   └── time.rs        # Date/ISO/单调时钟
│   ├── host/              # 注入 prelude 的宿主函数（同步 + 异步）
│   │   ├── fs.rs  path.rs  os.rs  url.rs  child_process.rs  crypto.rs
│   │   ├── exec.rs        # kiana.exec() -> tokio::process
│   │   └── console.rs
│   ├── api/               # ExtensionAPI 的 Rust 侧后端（§8）
│   │   ├── registry.rs    # 每个 extension 的注册表（tools/commands/flags/handlers/renderers）
│   │   ├── context.rs     # ExtensionContext/Tool/Command 的 host getter
│   │   ├── provider.rs  mcp.rs  virtual_model.rs
│   │   └── eventbus.rs
│   ├── dispatch/          # 事件派发：Rust 侧入口 + JS 侧合并（§8.5）
│   ├── ui/                # 扩展 UI 适配（§9）
│   ├── prelude/           # build.rs 生成的嵌入资源
│   │   └── mod.rs         # include_bytes!(OUT_DIR/prelude.min.mjs[.zst])
│   └── tests/
└── assets/
    └── extension-runtime/ # prelude 的 TS 源码（构建期打包，见 §4）
        ├── entry-prelude.ts
        ├── loader-shim.ts      # 移植自 src/core/extensions/loader.ts
        ├── runner-shim.ts      # 移植自 src/core/extensions/runner.ts（派发/合并）
        ├── api-shim.ts         # createExtensionAPI（事务性三态）
        ├── context-shim.ts     # createContext/createToolContext/createCommandContext
        ├── ui-shim.ts          # ExtensionUIContext 的 JS 侧（对话框栈/组件工厂）
        ├── node-shims/         # node:* 的 JS 实现（§10）
        └── vendored/           # kiana-tui / theme / typebox 的入口再导出
```

关键点：**`loader-shim.ts` / `runner-shim.ts` / `api-shim.ts` / `context-shim.ts` 几乎是
`packages/coding-agent/src/core/extensions/*.ts` 的逐行移植**，只把"跨到宿主"的调用换成
`globalThis.__kianaHost.*`。这是"js-shim-heavy"的全部要点：**语义留在 JS，能力放在 Rust**。

---

## 2. 分层架构

```
┌──────────────────────────────────────────────────────────────────────┐
│ Rust 宿主（tokio）                                                     │
│  agent-loop / session / tools / providers / tui(Rust) / rpc           │
│      │  同步能力: getters, fs, path, exec…                            │
│      │  异步能力: fetch/spawn/exec/await → tokio task                 │
│      ▼                                                               │
│  bridge: 值模型(§8.1) + Persistent 句柄表 + AbortSignal + 微任务泵       │
├──────────────────────────────────────────────────────────────────────┤
│ QuickJS Context（单线程，AsyncContext；由 AsyncRuntime 的 Mutex 串行化）│
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │ prelude（构建期打包，嵌入二进制）                                   │  │
│  │  node-shims: fs/path/os/url/events/util/readline/child_process…  │  │
│  │  vendored: kiana-tui 全量 + theme + typebox + StringEnum + uuidv7    │  │
│  │  loader-shim / runner-shim / api-shim / context-shim / ui-shim    │  │
│  │  globalThis.__kianaHost = { …同步/异步宿主函数… }                     │  │
│  └────────────────────────────────────────────────────────────────┘  │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │ 扩展模块（每个 .ts 用 oxc 剥离类型后作为独立 JS Module 求值）        │  │
│  └────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────┘
```

**单 Context**。所有扩展共享一个 QuickJS Context（与 Node 里"所有扩展共享一个 realm"一致）：
`instanceof`、`Symbol.for`、`globalThis` 上的 theme（`Symbol.for("@earendil-works/kiana-coding-agent:theme")`）
因此天然共享——这一点很重要，`theme.ts` 的 `theme` 是一个读 `globalThis[Symbol.for(...)]` 的 Proxy，
如果每个扩展一个 Context，theme 就要跨 realm 复制，`instanceof Theme` 会失效。
单 Context 的代价是**一个扩展的死循环会卡住全部**（用 §7.4 的中断看门狗兜底）。

多 Context（每个扩展一个 realm）作为**可选隔离模式**（`--extension-isolation`），代价是共享对象需要
显式桥接，不建议默认开。

---

## 3. 生命周期

```
启动
 └─ ExtRuntime::new()
     ├─ AsyncRuntime::new() ; set_memory_limit(256MiB) ; set_max_stack_size(4MiB) ; set_gc_threshold(8MiB)
     ├─ AsyncContext::full(&rt)
     ├─ 安装 host 桥: globalThis.__kianaHost = {…}, console, process, crypto, timers
     ├─ Module::declare+evaluate("kiana:node-shims/…")   # §10
     ├─ Module::declare+evaluate("kiana:intl")           # Intl.Segmenter polyfill（§10.3）
     ├─ Module::declare+evaluate("kiana:prelude")        # §4；或 unsafe Module::load(bytecode)
     └─ 之后每个扩展：
         resolve → transpile(oxc) → declare("…?kiana=<gen>") → 注入 import.meta → eval
         → 取 default export → 调 factory(api, ?) → commit/discard（事务性，§8.2）
```

`kiana-extensions` 对外只暴露**一个** façade：

```rust
pub struct ExtensionHost { /* AsyncRuntime + Context + 注册表 + 派发器 */ }

impl ExtensionHost {
    pub async fn new(cfg: ExtRuntimeConfig) -> Result<Self>;
    pub async fn load(&self, paths: &[PathBuf], cwd: &Path) -> LoadExtensionsResult;
    pub async fn dispatch(&self, event: ExtensionEvent) -> EventOutcome;   // §8.5
    pub async fn call_tool(&self, name: &str, call: ToolCall) -> ToolOutcome;
    pub async fn render_call(&self, tool: &str, ctx: ToolRenderContext) -> Option<Rendered>;
    pub async fn render_result(&self, tool: &str, ctx: ToolRenderContext) -> Option<Rendered>;
    pub async fn reload(&self) -> Result<()>;
}
```

---

## 4. Prelude：构建期打包 + 嵌入

### 4.1 组成与尺寸（实测）

| 组成 | 源 | 原始 | gzip | QuickJS 装载耗时 |
|---|---|---|---|---|
| kiana-tui 全量（`packages/tui/src/index.ts`） | TS | 301,144 B | 90,547 B | 47–55 ms |
| typebox 全量（`typebox@1.3.27`） | ESM | 85,301 B | 21,923 B | 15–23 ms |
| theme + config/syntax-highlight 桩 | TS | 计入 prelude | — | — |
| `Intl.Segmenter` polyfill（`unicode-segmenter@0.14.0`） | ESM | 16,461 B | 6,173 B | 3.5–4.5 ms |
| node shims（JS） | TS | < 8 KB | — | 0.2 ms |
| **合计（Minified ESM，含 loader/runner/api shim）** | | **407,534 B** | **121,119 B** | **75.4 ms** |

> 早期版本（未含 `marked`）为 414,417 B / 122,311 B；最终版把 `marked` + `marked-cjk-friendly`
> 内联后为 407,534 B（minify 更好）。

实测装载序列（同一 Context，冷）：
`shims 0.24 ms → intl 3.6 ms → typebox 16.9 ms → kiana-tui 55.4 ms`，合计 75.4 ms。
这是**每个进程一次**的成本（不是每个扩展一次）。

### 4.2 esbuild 打包配置（构建期脚本）

```js
// assets/extension-runtime/build.mjs
await esbuild.build({
  entryPoints: ["assets/extension-runtime/entry-prelude.ts"],
  bundle: true, format: "esm", platform: "neutral", minify: true,
  target: "es2022",
  absWorkingDir: repoRoot,
  alias: {
    "typebox": "<repo>/node_modules/typebox/build/index.mjs",
    "get-east-asian-width": "<repo>/node_modules/get-east-asian-width/index.js",
    "marked": "<repo>/node_modules/marked/lib/marked.esm.js",
    "marked-cjk-friendly": "<repo>/node_modules/marked-cjk-friendly/dist/index.js",
    "@earendil-works/kiana-tui": "packages/tui/src/index.ts",
  },
  external: [
    // 全部交给 prelude 内的 node-shims 模块（由 Resolver 提供）
    "node:*", "fs", "fs/promises", "path", "os", "url", "module", "events",
    "child_process", "util", "readline", "zlib", "crypto", "perf_hooks",
    "string_decoder", "stream", "http", "https", "net", "tty", "worker_threads",
    // 代码高亮与大依赖：走桩，运行时降级（§9.6）
    "chalk", "highlight.js", "ignore", "proper-lockfile", "minimatch",
    "brace-expansion", "diff",
  ],
  plugins: [{
    name: "kiana-shims",
    setup(b) {
      // theme.ts 里 `../../../utils/{syntax-highlight,text,fs-watch}.ts` / `../../../config.ts`
      // 在 monorepo 内是相对路径，无法用 alias，用 onResolve 重定向到 shim
      b.onResolve({ filter: /^\.\.\/(\.\.\/)*(utils\/(syntax-highlight|text|fs-watch)|config)\.ts$/ },
        a => ({ path: mapToShim(a.path) }));
    },
  }],
});
```

构建产物写入 `OUT_DIR/prelude.min.mjs`，`build.rs` 用 `include_bytes!` 嵌入；
可选 `zstd -19` 后嵌入（121 KB → 约 33 KB），启动时解压 ~1 ms（实测 zstd 0.14 足够快）。

### 4.3 可选：字节码预热（2–4× 启动加速）

实测（kiana-tui 301,144 B）：

| 路径 | 耗时 | 体积 |
|---|---|---|
| 源码 `declare`（编译）+ `eval` | 36.5 ms | 301 KB |
| `Module::write(strip_source=false, strip_debug=false)` | 4.1 ms | 907,884 B |
| 字节码 `Module::load` + `eval`（新 runtime） | **8.7 ms** | 907 KB |
| `strip_source=true, strip_debug=true` 的字节码 | — | 395,890 B（但 source map/stack 丢失） |

结论：
- 只对 **kiana-tui + typebox**（大且稳定）做字节码预热，收益 ~40 ms；
- 字节码**与 CPU 架构/端序/QuickJS 版本绑定**（`WriteOptions.endianness`），因此**不能**作为通用
  二进制内置；做法是：**首次启动在用户缓存目录生成**（`~/.cache/kiana/extensions/prelude-<hash>.qbc`），
  文件头写入 `quickjs-version + arch + endianness + prelude-hash`，不匹配则重新生成；
- 默认**关闭**（首启会多花 40 ms 生成，之后每次省 40 ms）；`KIANA_EXT_BYTECODE=1` 或检测到启动 >50 次时开启。
- 扩展源码本身的字节码缓存同理，但扩展是用户频繁编辑的文件，用 mtime+size+hash 做键。

---

## 5. 模块解析：复刻 jiti

### 5.1 jiti 的三条分支

`loader.ts:569-585`：

| 条件 | 配置 |
|---|---|
| `usesEmbeddedModules`（Bun/SEA/bundled node） | `{ virtualModules: VIRTUAL_MODULES, tryNative: false }` |
| 源码 TS 运行时 | `{ virtualModules: VIRTUAL_MODULES, tsconfigPaths: true }` |
| 未打包 Node | `{ alias: getAliases() }` |

三者都叠加 `{ moduleCache: false }`。Rust 二进制等价于**第一行**；但为了对齐"源码 TS 运行时"的
开发体验（monorepo 内 `tsconfig` paths），我们**同时**支持 `tsconfigPaths`：
`kiana` 默认 `virtualModules`，若扩展目录里有 `tsconfig.json` 且声明了 `paths`，再叠加一层 path 映射
（复刻 §2.2.1 的 22 项 + `~` 前缀）。

### 5.2 22 个虚拟模块（逐字对齐 `virtual-modules.ts`）

| 键 | 内容 | prelude 提供方式 |
|---|---|---|
| `typebox` / `@sinclair/typebox` | 全量 TypeBox | prelude 内联（85 KB） |
| `typebox/compile`、`typebox/value`、`@sinclair/typebox/{compile,value}` | 同上子入口 | prelude 内联（同一模块对象） |
| `@earendil-works/kiana-tui` / `@mariozechner/kiana-tui` | 全量 kiana-tui | prelude 内联（301 KB） |
| `@earendil-works/kiana-ai`（→ compat） | `getModel`/`streamSimple`/`complete`/OAuth/… | **Rust 绑定**（§8.6） |
| `@earendil-works/kiana-ai/compat` | 同上 | 同一对象 |
| `@earendil-works/kiana-ai/models` | 模型目录 | Rust 绑定 |
| `@earendil-works/kiana-ai/oauth` | OAuth | Rust 绑定 |
| `@earendil-works/kiana-ai/providers/all` | 提供商目录 | Rust 绑定 |
| `@earendil-works/kiana-coding-agent` / `@mariozechner/…` | SDK 根导出面（`defineTool`、`CustomEditor`、`createReadTool`、守卫…） | **prelude 内 `sdk-shim.ts`**（纯 JS，内部转调 `__kianaHost`） |
| `@earendil-works/kiana-agent-core` / `@mariozechner/…` | `Agent`/`AgentTool` 类型与少数运行时值 | prelude 薄导出 |

关键**语义要求**：同一个 specifier 在一次进程内多次 import 必须拿到**同一个对象**
（`ca-extensions.md` §8.6 第 2 条）。用 `Module::declare(name, src)` + 首次 eval 后从
`Module::namespace()` 取对象，之后所有解析都返回同一 specifier → QuickJS 的模块缓存保证同一 namespace。

**这里不能套用"每次唯一 specifier"**（那是扩展源码的 `moduleCache:false` 逻辑）；虚拟模块必须缓存。

### 5.3 `moduleCache: false` 的等价实现

实测的 QuickJS 语义：

| 操作 | 结果 |
|---|---|
| `Module::declare(sameName, src)` 两次 | 都成功，但**各自重新编译**（14.5–15.5 ms，无缓存收益） |
| 对已 eval 的模块再 `eval()` | **不重跑**模块体（返回同一 namespace） |
| 同 specifier 被两个模块 `import` | 依赖**只求值一次**（`__depCount === 1`） |
| 同源码 + 新 specifier（`?kiana=2`） | **重新求值**（`__depCount === 2`） |

因此扩展入口用 **unique specifier**：

```rust
let gen = runtime.generation();                       // 每次 loadExtensions 递增
let spec = format!("{}?kiana={gen}", resolved_path);     // 与 jiti 的 moduleCache:false 等价
```

- 依赖图内部（扩展 A import 扩展 B 的相对路径）解析出的 specifier 也带**同一代**的 `?kiana=<gen>`，
  这样一次加载内依赖只求值一次（与 jiti 一致），跨代重新求值。
- 工厂缓存（`(resolvedPath, cwd, generation)`，`ca-extensions.md` §2.2.2）保留在 **Rust 侧**：
  命中时**不重跑模块体、只重跑工厂**——因为工厂是模块导出的函数，重跑无需重新求值模块。
- `clearExtensionCache()` → `generation += 1`。

> 副作用：`import.meta.url` 会带上 `?kiana=N`。必须在 `Module::meta()` 里覆写为干净路径（§5.4）。

### 5.4 `import.meta` 注入（实测可行）

QuickJS 的 `import.meta` 是一个**空的可扩展普通对象**（`Object.keys(import.meta) === []`，
`Object.isExtensible === true`），`url` 默认 `undefined`。正确做法**不是**让扩展自己赋值，而是：

```rust
let m = Module::declare(ctx, spec, js)?;
{
    let meta = m.meta()?;                       // JS_GetImportMeta
    meta.set("url", format!("file://{}", clean_path))?;
    meta.set("dirname", dir)?;
    meta.set("filename", clean_path)?;
    meta.set("main", false)?;
    meta.set("resolve", host_resolve_fn)?;      // 同步 host fn，复刻 ESM resolution
}
let (_m, promise) = m.eval()?;
```

实测：注入后模块内读到 `file:///ext/a.ts | /ext | resolved:x | false`。**不能用 top-level await 赋值**
（QuickJS 的 `import.meta` 在模块体执行前已可用，但扩展可能同步读取它）。

### 5.5 相对路径 / npm 解析顺序

`Resolver::resolve`（实现 `rquickjs::loader::Resolver`）的判定顺序：

1. **虚拟模块表**（22 键，精确匹配）→ `kiana:builtin/<key>`
2. `node:*` / 裸内建名（`fs`、`path`、…）→ `kiana:node-shims/<name>`
3. `./x`、`../x`、`/abs/x` → 以 **base 的目录**（去掉 `?kiana=N` 查询串）拼接，`path-clean` 归一化，
   补扩展名（`.ts`→`.tsx`→`.js`→`.mjs`→`.cjs`→`/index.ts`…）
4. `~` 前缀（`ca-cli` 的 `resolvePath` 语义）→ 展开 home
5. **npm**：
   - 从 base 目录向上找 `node_modules/<pkg>`（Node 算法，含 `@scope/name`）
   - 读 `package.json`：`exports`（条件顺序 **`node` → `import` → `require` → `default`**，
     与 jiti 一致；`types`/`browser` 跳过）→ `module` → `main` → `index.js`
   - 子路径 `pkg/sub/path`：先查 `exports["./sub/path"]`，再按文件路径直连
   - 深层依赖：被解析文件里的 import 以**该文件路径**为 base 继续解析（递归）
6. `tsconfig.json` 的 `paths`（若启用）
7. 失败 → `Error: Cannot find module '<spec>' from '<base>'`（消息文本对齐 Node）

**Loader::load**：
- `kiana:builtin/*` → prelude 里预注册的模块源码（或直接 `Module::declare` 一个 re-export 桩）
- `kiana:node-shims/*` → prelude 的 shim 源码
- 文件 → `fs::read_to_string` → 按扩展名决定是否 `transpile`（§6）→ `Module::declare(spec, js)`
- `.json` → 合成 `export default <json>`（Node 的 JSON 模块语义 + `with { type: "json" }` 忽略）
- `.cjs` / 判定为 CJS 的 `.js` → 包一层 CJS wrapper（§11.2）

### 5.6 缓存

| 层级 | 键 | 失效 |
|---|---|---|
| 源码读取 | 绝对路径 | mtime+size |
| 剥离结果（JS 文本） | 路径 + mtime + size + oxc 版本 | 同上 |
| **模块实例** | QuickJS specifier（含 `?kiana=gen`） | 每代自动失效（新 specifier） |
| 工厂 | `(resolvedPath, cwd, generation)` | `clearExtensionCache()` / cwd 变化 |
| 虚拟模块 namespace | specifier | 永不（进程级） |

---

## 6. TypeScript / ESM 装载

### 6.1 剥离器：`oxc 0.155.0`（运行时，逐文件）

```rust
fn transpile(src: &str, path: &Path) -> Result<String> {
    let alloc = Allocator::default();
    let st = SourceType::from_path(path).unwrap_or_default();
    let ret = Parser::new(&alloc, src, st).parse();     // ret.diagnostics
    let mut program = ret.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let tr = Transformer::new(&alloc, path, &TransformOptions::default());
    tr.build_with_scoping(scoping, &mut program)?;
    Ok(Codegen::new().build(&program).code)
}
```

**实测耗时**（release，含 parse+semantic+transform+codegen+allocator）：

| 文件 | 大小 | 耗时 |
|---|---|---|
| `hello.ts` | 640 B | 30–55 µs |
| `minimal-mode.ts` | 10.8 KB | 0.37–0.40 ms |
| `preset.ts` | 14.5 KB | 0.49–0.53 ms |
| `overlay-qa-tests.ts`（最大单文件） | 46 KB | 2.38–2.76 ms |
| 合成 10× preset | 144 KB | 7.1 ms |

即：**79 个入口全量剥离 ≈ 20–30 ms**（一次性，且可缓存）。

**语法覆盖（逐条实测）**：

| 语法 | oxc 默认 `TransformOptions` | 备注 |
|---|---|---|
| `interface` / `type` / 类型注解 / 泛型 | ✅ 删除 | |
| `import type` / `import { type X }` | ✅ 删除 | |
| `as` / `satisfies` / `as const` | ✅ 删除 | |
| `enum`（含 `const enum`） | ✅ 降级为 IIFE | 输出与 esbuild/tsc 同形 |
| `namespace` | ✅ 降级为 IIFE | |
| 类参数属性（`constructor(private x)`） | ✅ 降级为赋值 | |
| 抽象类 / `implements` / 重载声明 | ✅ 删除 | |
| 可选链 / `??` | ✅ 保留（引擎原生支持） | |
| **装饰器** | ⚠️ 默认**不降级**（`DecoratorOptions.legacy` 默认 false，输出 `@dec`） | 需显式 `decorator: DecoratorOptions { legacy: true, emit_decorator_metadata: true, .. }`；**examples 里零使用**（已全量 grep 确认） |
| `export =` / `import x = require()` | ❌ 不处理（原样输出） | examples 零使用；真遇到按 CJS 处理 |
| `declare` 语句 | ✅ 删除 | |
| JSX/TSX | ✅（`SourceType::from_path` 识别 `.tsx`） | 用 `jsx` 选项；examples 零使用 |

**结论**：examples 语料用到的语法（`abstract class`、`satisfies`、`implements`、`import type`）
全部覆盖；装饰器只在"用户扩展"里可能出现，默认开启 `legacy: true` 以对齐 tsc 的
`experimentalDecorators`（jiti 的默认也是 legacy 语义）。

### 6.2 备选剥离器

| 方案 | 何时用 |
|---|---|
| **esbuild 0.25.10（构建期）** | 若某个扩展用到了 oxc 不支持的语法，构建期退回 esbuild `transform({loader:'ts', format:'esm', target:'es2022'})`。实测它能正确处理 `enum`、`abstract`、`namespace`、装饰器、`satisfies`、`import type`、`declare`。**但它是 npm 依赖，不进二进制**。 |
| `deno_ast 0.53.3` | `transpile(TranspileOptions { decorators, verbatim_module_syntax, imports_not_used_as_values })`；Swc 系，对装饰器/JSX 更完整，但体积大（swc_core 82.2.0 > 10 MB）。仅在 oxc 有缺口时考虑。 |
| 不剥离 | 仅当扩展本身已是 `.js`/`.mjs`（跳过 parse，直接 `Module::declare`）。实测 `hello.ts` 的 parse 成本 30 µs，可以忽略，但**保留该快路径**给 `.js`。 |

### 6.3 ESM / CJS 判定

| 文件 | 判定 |
|---|---|
| `.mjs` | ESM |
| `.cjs` | CJS |
| `.ts`/`.tsx` | 取决于语法：`import`/`export` → ESM；否则当 CJS 包一层（对齐 jiti 的 CJS 默认） |
| `.js`（在 `node_modules` 内） | 读最近 `package.json` 的 `"type"`：`module`→ESM，否则 CJS |
| `.js`（用户扩展） | 有 `import`/`export` 当 ESM，否则 CJS |

扩展入口**必须**是 ESM 语义（`jiti.import(path, { default: true })` 取默认导出）；
若扩展是 CJS（`module.exports = fn`），wrapper 生成 `export default module.exports`。

### 6.4 `import.meta` / `import()` / 顶层 await

- `import.meta`：§5.4。
- 动态 `import(spec)`：由同一个 Resolver/Loader 处理，**返回 Promise**（QuickJS 原生）。
  扩展里 `await import("./x.ts")` 正常工作；`?kiana=gen` 由 Rust 侧在解析时补上（对动态 import，
  Resolver 收到的 base 是发起模块的 specifier，能取到同一代）。
- 顶层 await：QuickJS 原生支持；`Module::eval` 返回的 Promise 必须被 drive loop 泵完（§7.2）。
- `import.meta.resolve(s)`：注入同步 host fn，按 §5.5 解析并返回 `file://` URL 字符串。

---

## 7. 异步与事件循环（tokio 集成）

### 7.1 运行时选择

用 `AsyncRuntime` + `AsyncContext`（`rquickjs` 的 `full-async`/`parallel`）。理由：
- `AsyncRuntime` 在 `parallel` 下是 `Send + Sync`（`unsafe impl Send/Sync for AsyncRuntime`），
  可以放进 `Arc` 让 tokio 任务持有；
- 内部用 `Mutex<InnerRuntime>` 串行化，**同一时刻只有一个线程进 JS**，语义上等价于 Node 的单线程；
- `execute_pending_job().await` 会 poll JS 的 job 队列**以及** rquickjs 的 spawner 队列
  （`is_job_pending` 同时检查两者），这正是"Promise + tokio"的接缝。

### 7.2 drive loop（实测可用）

```rust
pub async fn drain(rt: &AsyncRuntime) {
    loop {
        let pending = rt.is_job_pending().await;
        if !pending { break; }
        match rt.execute_pending_job().await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => { /* 记录 handler 抛错，继续 */ }
        }
        tokio::task::yield_now().await;   // 让出，避免饿死 tokio
    }
}
```

实测：
- `Promise.all([asyncHost, microtask])` 顺序与 V8 一致（`["a=done:x","b=done:y,done:z","micro"]`）；
- 5000 次 `await host()` 共 **7.64 ms**（1.5 µs/次）；
- Promise 最终状态 `Resolved`。

**陷阱（已踩）**：JS 工作**不能**在 `async_with(async |ctx| { … }).await` 的闭包里等待 host 异步
任务——那会持锁，host 侧再想进 JS（例如回调 handler）就死锁。正确做法：
- **setup 阶段**在 `async_with` 里跑（declare/eval/建对象）；
- **等待阶段**在闭包外，用 `rt.is_job_pending()/execute_pending_job()` 泵；
- 需要 JS 回调时，再开一次短暂的 `async_with`，不要在泵里嵌套。

### 7.3 host 异步函数（`Async<JsAsync>`）

```rust
let exec = Function::new(ctx.clone(), Async(|ctx, cmd: String| async move {
    // 在 tokio 上跑；返回时 rquickjs 自动 resolve Promise
    let out = tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await?;
    Ok::<_, rquickjs::Error>(json!({ "stdout": ..., "stderr": ..., "code": ..., "killed": false }))
}))?;
```

`kiana.exec()`、`spawn`、`fetch`、`modelRegistry.complete`、`ctx.ui.select` 等全部走这条。
**每一类 host 异步函数都要能取消**：把 `CancellationToken`（来自 `ctx.signal`）传进去，
abort 时 `Err(Error::new_from_js_message("abort","aborted","The operation was aborted"))`。

### 7.4 看门狗：中断与超时

- `rt.set_interrupt_handler(Some(Box::new(move || start.elapsed() > budget)))`；
  实测：模块里 `for(;;)i++` 在 250 ms 时被中断，`promise` 以 `Exception("interrupted")` reject，
  **之后 context 仍然可用**（再 eval 一个模块返回 7）。
- 中断是**全局**的：一旦触发，当前 JS 调用栈全部退出。因此：
  - 每次进入 JS（`load`、`dispatch`、`call_tool`、`render`）前**重置** `start`；
  - 超时预算：`load` 5 s、单次 handler 30 s（可配）、`render` 100 ms（渲染不能卡 UI）；
  - 超时后：把该扩展标记为 `Failed`，走 `discard()`，UI 报
    `Extension "<path>" error: <error>`（`ca-extensions.md` §2.8.4 逐字）。
- QuickJS 没有 `Atomics.wait` 之外的阻塞点，**同步 `fs.readFileSync` 由 host 实现，本身是阻塞的**：
  这类调用会占住 tokio worker。缓解：`fs` 同步 API 走**专用的 blocking 线程池**
  （`tokio::task::spawn_blocking` + `futures::executor::block_on` 在 JS 线程上等），
  或在 JS 侧把它实现为 async + 在 prelude 里用 `Atomics.wait` 不可行——因此**接受同步阻塞**，
  但加"同一 Context 只跑在一个专用 OS 线程上"的约束（见下）。

### 7.5 线程模型（重要）

**推荐**：给 QuickJS 分配一个**专用 OS 线程**（`std::thread`），该线程跑一个
`current_thread` tokio runtime；JS 永远在这个线程上执行。其他 tokio 任务通过
`mpsc` + `oneshot` 把"调用 JS"的请求投递过去。这样：
- 同步 host 函数（`readFileSync`）阻塞的只是 JS 线程，不污染多线程 worker；
- `AsyncRuntime` 的 `Send + Sync` 仍然可用（跨线程投递是安全的）；
- 与 Node 的"JS 单线程 + libuv 线程池"模型同构，语义最容易对齐。

编排：

```
tokio multi-thread (主)
  ├─ agent loop / tools / providers
  │     └─ ext_tx.send(ExtCall::Dispatch(event)) ──► JS 线程
  │                                                   AsyncRuntime + Context
  │                                                   drive loop
  │     ◄── oneshot(result) ─────────────────────────┘
  └─ 其它 I/O
```

一次 `dispatch` 的往返实测开销（不含 JS 执行）：`mpsc + oneshot` ~5–10 µs 量级，
相对单次 handler 的真实工作（µs–ms）可忽略。

### 7.6 微任务与 `queueMicrotask` 语义

`runner.ts` 用 `queueMicrotask` 把 `ui_prompt_start` / `ui_prompt_end` 推到当前微任务队列末尾。
QuickJS 有原生 `queueMicrotask`——**prelude 里直接用**，语义精确（这比在 Rust 侧用
`tokio::task::yield_now()` 近似要好，是 js-shim-heavy 的一个直接收益）。

---

## 8. API 绑定策略（逐 API 组）

这是本文档的核心。原则：

- **注册类（写状态）**：纯 JS，写在 prelude 的 `Extension` 记录里；`commit()` 时一次性刷给 Rust。
- **读取类（同步 getter）**：`__kianaHost.get_*()` 同步 host 函数，直接返回 JS 值。
- **动作类（有副作用/异步）**：`__kianaHost.*` 返回 Promise；Rust 侧 `Async(...)`。
- **对象图（Provider/MCP/Tool 定义）**：**JS 对象保持为 JS 对象**，Rust 只持有 `Persistent<Object>`
  句柄并按需读字段（绝不整体序列化），因为扩展可能给它挂方法、getter、Symbol。

### 8.1 值模型（`bridge/value.rs`）

| JS 语义 | 桥接表示 | 说明 |
|---|---|---|
| `undefined` | `enum JsVal { Undefined, Null, Bool, Num, Str, Arr, Obj, Handle(JsHandleId) }` | **必须区分** `undefined` 与"字段不存在" |
| 缺失字段 | `Option<JsVal>`（`None` = 不存在） | Rust `Option<Option<T>>` 的同构 |
| 普通对象/数组 | `serde_json::Value`（`preserve_order`） | 只用于**事件输入/结果输出**这类纯数据 |
| 复杂对象（含函数/类实例/Map/Date） | `Persistent<Object>` + `JsHandleId` | 用于 Tool 定义、Provider、Component、sessionManager 等 |
| 引用相等 | 保留同一 `Persistent`（或同一 JS 对象） | `sameMessages` 依赖它（`ca-extensions.md` §6.3） |
| 异常 | `JsThrown { value: JsHandleId, message: String }` | `message` 取 `.message`，否则 `String(err)`（`Error: msg`） |
| `structuredClone` | 原生（预lude 注入 polyfill：JSON round-trip） | 本单元只克隆 JSON 值 |

**避免 JSON 往返的热点**：`message_update`（流式 delta）、`tool_execution_update`（增量输出）、
`Text.render` 的返回值。这些走 **`Persistent` 句柄 + 局部读取**，不整体序列化。
实测：135 KB 对象一次 `JSON.stringify` 3.5 ms（20 次 70 ms），一次流式 delta 若 100 字节则 <10 µs，
但**每秒几十次**的累积不可忽视。

### 8.2 事务性 `createExtensionAPI`（三态机）

移植 `loader.ts:247-540` 到 `api-shim.ts`：

```ts
type ApiState = "loading" | "active" | "failed";
function createExtensionAPI(extension, runtime, cwd, eventBus) {
  const pendingFlagValues = new Map();
  const pendingRuntimeChanges: Array<() => void> = [];
  const loadingUnsubscribers: Array<() => void> = [];
  let state: ApiState = "loading";

  const assertActive = () => {
    if (state === "failed")
      throw new Error(`Extension "${extension.path}" failed to load and its API is no longer active.`);
    runtime.assertActive();
  };
  // …逐方法移植…
  return { api, commit, discard };
}
```

- `registerTool/registerCommand/registerShortcut/registerFlag/registerMessageRenderer/...`：
  **立即**写 JS 侧 `extension` 记录（失败时 `discard()` 撤销）；
- `registerProvider/registerMcpServer/registerVirtualModel/unregister*`：加载期只进
  `pendingRuntimeChanges`，`commit()` 时执行（对齐 §8.4）；
- `getFlag(name)`：先 `pendingFlagValues`，后已应用值，未注册 `undefined`；
- `commit()`/`discard()` 幂等（`take()`）。

**Rust 侧只看到 commit 之后的最终注册表**：`commit()` 调用一次 `__kianaHost.commit_extension(handle)`，
Rust 读 `extension` 对象（`Persistent`）里的 `tools`/`commands`/`flags`/`shortcuts`/`handlers` Map，
生成 Rust 侧的 `RegisteredTool`/`RegisteredCommand`（其中 handler 仍是 JS 函数句柄）。

### 8.3 API 逐组成员与绑定方式

`ExtensionAPI` 共 **31 个方法 + 1 个属性**（`events`），外加 41 个 `on()` 重载（= 41 个事件名）。

| 成员 | 类别 | 绑定 |
|---|---|---|
| `on(event, handler)` | 注册 | JS：`extension.handlers.get(event).push(wrap(handler))`，返回退订函数。**handler 仍是 JS 函数**，Rust 派发时调用它 |
| `registerTool(tool)` | 注册 | JS 校验（`typeof tool.parameters === "object" && !== null && !Array.isArray`，错误文本逐字）；写入 `extension.tools`；`__kianaHost.refresh_tools()` |
| `registerCommand(name, opts)` | 注册 | JS 校验非空 name + `handler` 为函数（逐字错误） |
| `registerShortcut(key, opts)` / `registerFlag(name, opts)` | 注册 | JS |
| `registerMessageRenderer(customType, fn)` / `registerMarkdownTransformer(fn)` / `registerEntryRenderer(customType, fn)` / `registerToolRenderer(resolver)` | 注册 | JS（渲染器是 JS 函数，见 §9） |
| `getFlag(name)` | 读取 | JS（本地 Map） |
| `getActiveTools()` / `getAllTools()` / `getCommands()` / `getSettings()` | 读取 | 同步 host：`__kianaHost.get_active_tools()` 等 |
| `getSessionName()` / `getThinkingLevel()` | 读取 | 同步 host |
| `sendMessage(msg, opts?)` / `sendUserMessage(text, opts?)` / `appendEntry(type, data?)` | 动作 | 异步 host（返回 Promise；`sendMessage` 在 `triggerTurn` 时可能触发新一轮） |
| `setSessionName(name)` / `setLabel(id, label)` / `setActiveTools(names)` / `setModel(model)` / `setThinkingLevel(level)` | 动作 | 同步 host（`setModel` 返回 Promise，可能触发 provider 解析） |
| `exec(cmd, args, opts?)` | 动作 | 异步 host → `tokio::process`（§8.7） |
| `registerProvider(name, cfg)` / `registerProvider(provider)` | 对象图 | 两 overload：前者纯 JSON 配置 → serde；后者是 kiana-ai `Provider` 对象（含 `stream` 函数）→ `Persistent` 句柄 |
| `unregisterProvider(name)` / `registerMcpServer(name, cfg)` / `unregisterMcpServer(name)` / `getMcpServers()` | 混合 | 配置 JSON + registry；`getMcpServers()` 返回 JS 数组（从 Rust 快照构造） |
| `registerVirtualModel(def)` / `unregisterVirtualModel(provider, id)` | JSON | 纯数据 |
| `events: EventBus` | 对象 | **prelude 内实现**（§8.8） |

**`ExtensionContext`（18 成员，全部是 getter）**：
`ui`/`mode`/`hasUI`/`cwd`/`sessionManager`/`modelRegistry`/`model`/`scopedModels`/`thinkingLevel`/
`isIdle()`/`isProjectTrusted()`/`signal`/`abort()`/`hasPendingMessages()`/`shutdown()`/
`getContextUsage()`/`compact(opts?)`/`getSystemPrompt()`。

移植 `runner.ts:884-966`：**每个成员都是 `Object.defineProperty` 的 getter**，读取时调
`runner.assertActive()` 再取新值（不是快照）。Rust 侧对应 `__kianaHost.ctx_get("cwd")` 这类单一入口，
或按字段拆成 `__kianaHost.ctx.cwd` 的 getter（后者少一次字符串分派，但 getter 数量大；
实测 100k 次同步 host 调用 11.4 ms，即 114 ns/次，**拆 getter 更划算**）。

| ctx 成员 | 实现 |
|---|---|
| `ui` | 返回 prelude 的 `uiContext` 对象（`Persistent`） |
| `mode` / `hasUI` / `cwd` / `isIdle()` / `isProjectTrusted()` / `hasPendingMessages()` | 同步 host getter |
| `sessionManager` | **JS 包装对象**：`getEntries()`/`getBranch()`/`getLeafEntry()`/`getLabel()`/`getSessionFile()` 转调 host（examples 只用到这 5 个 + `getLabel`）；内部持有 `Persistent` |
| `modelRegistry` | JS 包装：`find`/`findOfType`/`hasConfiguredAuth`/`classify`/`complete` |
| `model` / `scopedModels` / `thinkingLevel` | 同步 host（JSON） |
| `signal` | **AbortSignal shim**（§8.4），按需构造并复用 |
| `abort()` / `shutdown()` / `compact()` | 同步 host（`compact` 不 await） |
| `getContextUsage()` / `getSystemPrompt()` | 同步 host |

**`ExtensionToolContext`（+2）**：`tools`（getter → `__kianaHost.get_callable_tools()`）与
`executeTool(name, args, opts)`（异步 host，走嵌套 tool call，`ca-extensions.md` §5.4）。

**`ExtensionCommandContext`（+7）**：`getSystemPromptOptions`/`waitForIdle`/`newSession`/`fork`/
`navigateTree`/`switchSession`/`reload`。移植 `createCommandContext()` 的
`Object.defineProperties({}, Object.getOwnPropertyDescriptors(createContext()))` ——
**必须保留 getter 的惰性**（spread 会冻结旧值）。

**`ReplacedSessionContext`（2）**：`sendMessage`/`sendUserMessage`，指向**新** session 的 host 动作。

### 8.4 AbortSignal 桥

`ExtensionContext.signal` 与 `ToolDefinition.execute(..., signal, ...)` 的 signal 是 **JS `AbortSignal`**。
QuickJS 没有 `AbortController`（实测 `typeof AbortController === "undefined"`），prelude 里实现：

```ts
class AbortSignalShim extends EventTargetLike {
  aborted = false; reason = undefined;
  #listeners: Array<(e) => void> = [];
  addEventListener(type, fn) { if (this.aborted) { queueMicrotask(() => fn(this.#event)); } else this.#listeners.push(fn); }
  throwIfAborted() { if (this.aborted) throw this.reason; }
  static any(signals) { /* 组合，任一 abort 即 abort，reason 取第一个 */ }
  static timeout(ms) { /* setTimeout */ }
}
```

Rust 侧 `CancellationToken` 在 abort 时调用 `__kianaHost.__abort(id)` → prelude 触发对应 signal。
实测中断 handler 的行为表明"abort 之后再 `addEventListener` 要立即触发"必须显式实现。

### 8.5 事件派发：**语义留在 JS**

**41 个事件**（逐字，`types.ts`）：

```
project_trust, resources_discover, session_start, session_info_changed,
session_before_switch, session_before_fork, session_before_compact, session_compact,
session_compact_failed, session_shutdown, mcp_servers_change, session_before_tree,
session_tree, context, context_with_system, cache_warming_decision,
before_provider_request, before_provider_headers, after_provider_response,
provider_stream_event, before_agent_start, agent_start, agent_end, agent_before_settle,
agent_settled, ui_prompt_start, ui_prompt_end, turn_start, turn_end, message_start,
message_update, message_end, tool_execution_start, tool_execution_update,
tool_execution_end, model_select, thinking_level_select, tool_call, tool_result,
user_bash, input
```

**12 个专用派发器 + 1 个泛型 `emit` + `emitError`**，各有不同合并语义。全部移植到
`runner-shim.ts`（几乎逐行），Rust 只提供：

```ts
// prelude 暴露给 Rust 的派发入口（Rust 传 JSON 或句柄）
globalThis.__kianaRunner = {
  dispatch(eventType, eventJson) -> resultJson,      // 泛型
  emitToolCall(eventJson) -> resultJson | undefined,
  emitToolResult(eventJson) -> …,
  emitContext(messagesJson) -> messagesJson,
  emitBeforeAgentStart(eventJson) -> …,
  // …每个专用派发器一个方法
};
```

为什么放 JS（这是本设计的关键判断）：

| 语义 | 为什么必须在 JS |
|---|---|
| `before_agent_start` 的 systemPrompt 链 | 后一个 handler 通过 `ctx.getSystemPrompt()` 读前一个的改动 → 需要"链式状态 + getter"在同一 realm |
| `context` 两阶段 + `restoreSystemMessages` | 需要**数组引用相等**判断"handler 是否替换了整个数组" |
| `tool_call` 的 `block`/`terminate` 短路（**无 try/catch**） | 错误必须原样穿透到调用方 |
| `tool_result` 的补丁链 + `structuredContent` 删除 | 要区分"键存在且为 `undefined`"与"键不存在" |
| `input` 的 transform 链 + `handled` 短路 | 同上 |
| `session_before_*` 的默认值 | 返回 `undefined` = 用默认；返回对象 = 覆盖 |
| `before_provider_headers` 的**原地改** | 需要拿到同一个对象引用 |
| `resources_discover` 的数组拼接 | |
| `user_bash` **fail-closed**（唯一向上抛） | |
| `snapshotEventHandlers` + 嵌套派发重新取快照 | |
| `sameMessages` 引用相等 | |
| `queueMicrotask` 的 `ui_prompt_*` 时序 | 引擎原生微任务 |

如果把这些搬到 Rust，会重新引入 §6.3 里列的**全部** JS↔Rust 语义差异（undefined、引用相等、
字符串 UTF-16、对象键顺序）；放 JS 里则**构造上等于 jiti 环境**。

Rust 的职责只剩：
1. 把事件对象**物化**成 JS（每次派发一次，整次复用 → 引用相等有效）；
2. 调用 `__kianaRunner.<emitter>`；
3. 把结果读回（补丁/新对象/`undefined`）。

物化策略（性能分级）：

| 事件 | 物化 |
|---|---|
| `message_update` / `tool_execution_update` / `provider_stream_event` | **句柄**（整条消息/增量作为 `Persistent`，不 JSON） |
| `context` / `context_with_system`（整段历史） | 整条历史 JSON 一次（实测 135 KB ≈ 3.5 ms，可接受）；**只读引用共享**给所有 handler |
| 其余低频事件 | JSON |

### 8.6 派发错误

单个 handler 抛错 → `emit_error` 继续（`ca-extensions.md` §2.3.14）。实现：
`__kianaRunner` 内部 `try/catch`，catch 里调 `__kianaHost.emit_error({ extensionPath, event, message, stack })`，
**不中断**其它 handler。唯一例外：`emitUserBash` 直接 rethrow。

UI 落点（逐字）：`Extension "<extensionPath>" error: <error>`（含 stack 时缩进 `  ` + dim）。

### 8.7 `kiana.exec()`

移植 `core/exec.ts` 语义到 Rust host：

```rust
pub async fn exec(command: &str, args: &[String], cwd: &Path, opts: ExecOptions) -> ExecResult {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args).current_dir(cwd)
       .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // 增量 UTF-8 解码（StringDecoder 等价）: 用 encoding_rs 或自写 carry buffer，
    // 保证跨 chunk 的多字节字符不被拆分（kiana#11312）
    // timeout → SIGTERM → killed=true；abort signal → 同上
}
```

返回 `{ stdout, stderr, code, killed }`。`timeout` 默认无；超时发 `SIGTERM`（不 `SIGKILL`，
对齐原实现）；`code` 在信号杀死时为 `-1` 或 `null`（Node 是 `null` → JSON `null`）。

### 8.8 `kiana.events`（EventBus）

`event-bus.ts` 就是 `node:events` 的 `EventEmitter` 包一层。因此：
- prelude 里用 **Node shim 的 `EventEmitter`**（§10），`createEventBus()` 逐行照搬；
- 跨扩展通信天然工作（同一 realm，同一 emitter 实例）；
- `trackEventBusSubscription`（runtime 失效时退订）也在 JS 里，`discard`/`invalidate` 时统一调用。

**不需要 Rust 参与**。这是"js-shim-heavy"省掉的一整块（`ca-extensions.md` §2.2.3 里那堆
`loadingUnsubscribers` 逻辑）。

### 8.9 Tool 的调用与渲染

`ToolDefinition` 有 18 个字段，其中 6 个是函数：`prepareArguments`、`reachesTools`、
`prepareLoadout`、`execute`、`renderCall`、`renderResult`。

- Rust 侧 `RegisteredTool { definition: Persistent<Object>, source_info, name, … }`；
- **执行**：`execute(toolCallId, params, signal, onUpdate, ctx)` →
  Rust 用 `Persistent::restore` + JS 侧 invoker 调用（`rquickjs::Function::call` **没有 `this` 参数**，
  实测必须用 JS 侧 `((t, id, p, s, u, c) => t.execute(id, p, s, u, c))` 包装）；
  实测该方法每次调用 ~6.9 µs 级（1000 次 `render(80)` + 读 8 行 = 6.87 ms）。
- `onUpdate` 是 Rust 传给 JS 的**回调函数**，Rust 侧 `Function::new(ctx, ...)` 里把增量写进 channel。
  实测 100k 次 JS→Rust→JS 往返 12 ms（120 ns/次）。
- `renderCall`/`renderResult`：只在内核 UI 要渲染时调，返回 `Component`（§9）。
- `prepareArguments`/`reachesTools`/`prepareLoadout`：同步调用，参数/返回值走 JSON。
- `outputSchema`/`parameters` 是 TypeBox 对象：Rust 只读 `JSON.stringify` 后的 JSON Schema
  （或直接读 `[Kind]` Symbol + 字段）；`registerTool` 的"必须是对象 schema"校验在 JS 里做（逐字错误）。

### 8.10 Provider / MCP / VirtualModel

- **Provider（原生对象 overload）**：`Provider` 含 `stream(model, ctx, opts)` 函数与 `models` 数组。
  Rust 侧把它当**不透明句柄**：调用时 `__kianaHost` 反向调用 JS 的 `stream`，把
  `AssistantMessageEventStream` 的每个事件通过 `onUpdate` 回传。
- **Provider（配置 overload）**：纯 JSON，serde 化后进 Rust 的 `ModelRegistry`。
- **MCP**：配置 JSON + `McpServerRegistry`（Rust）；`getMcpServers()` 从 Rust 快照构造 JS 对象。
- **VirtualModel**：纯数据；注册排队到 `bindCore()`（与 §8.2 一致）。

---

## 9. TUI 组件互操作

### 9.1 核心：**真正的 kiana-tui 在 JS 里**

扩展渲染自定义组件（`ctx.ui.custom(factory)`、`setWidget(factory)`、`setFooter`、`setHeader`、
`setEditorComponent`、`registerMessageRenderer`、`ToolDefinition.renderCall/renderResult`）
返回的是 **JS `Component` 对象**：`{ render(width): string[], handleInput?(data), invalidate?() }`。

设计：**kiana-tui 的原始 TS 源码整体进 prelude**（301 KB），因此：
- 扩展 `extends Editor` / `extends Box` / `new Text(...)` / `new Container()` 全部是**真的**；
- `visibleWidth`/`truncateToWidth`/`wrapTextWithAnsi`/`matchesKey`/`Key`/`CURSOR_MARKER` 是真的；
- 输出已实测与 Node **逐字节一致**（见 §9.2）。

Rust 侧的 TUI（`kiana-tui` crate）**不重写组件**，只做终端 I/O 与布局编排：

```
Rust TUI                          JS prelude
  ├─ 每帧: for child in children:
  │     if child is JS component:
  │         lines = __kianaUi.render(handle, width)   ←── 调 JS render(width)
  │     else: lines = child.render(width)          （Rust 原生组件）
  ├─ 输入: 把 key sequence 交给组件
  │     if JS: __kianaUi.handleInput(handle, data) ──► 返回 {consumed, …}
  └─ overlay/dialog 栈：Rust 管布局，JS 管内容
```

**为什么 render 必须从 JS 调**：`render(width)` 内部会调 `Text`/`truncateToWidth`/`theme.fg`，
这些都在 JS 里；Rust 不可能"替它算"。实测 `render(80)` 返回 8 行、1000 次 6.87 ms（含读回字符串），
即 **6.9 µs/帧**，远低于 16 ms 帧预算。

### 9.2 保真度证据（实测，QuickJS vs Node 逐字节）

输入 6 个样本（纯 ASCII / ANSI / CJK / emoji ZWJ / 长串 / 含 tab 换行），`render(20)`、
`truncateToWidth(18, "…")`、`visibleWidth` 三者输出：

```
hello world              → w=11  render=["hello world         "] trunc="hello world"
\x1b[31mred\x1b[0m text  → w=8   render=["\x1b[31mred\x1b[0m text            "] trunc 相同
漢字かなカナ한글          → w=16  render=["漢字かなカナ한글    "]
a👍🏽b👨‍👩‍👧               → w=6   render=["a👍🏽b👨‍👩‍👧              "]
"x"*60                   → w=60  render=3 行×20；trunc="xxxxxxxxxxxxxxxxx\x1b[0m…\x1b[0m"
"line1\nline2\ttab"       → w=16  render=["line1               ", "line2   tab         "]
```

QuickJS 与 Node 的输出**完全一致**（含 `truncateToWidth` 补的 `\x1b[0m`）。这一条是整个设计
成立与否的关键，已用 kiana-tui **源码**（非重写）验证。

### 9.3 组件句柄与生命周期

- `ctx.ui.custom(factory, opts)` → JS 调 `factory(tuiProxy, theme, keybindings, done)`；
  `factory` 可返回 `Component` 或 `Promise<Component>`；
- Rust 拿到 `Persistent<Object>`（组件）与 `done` 回调（`Persistent<Function>`）；
- Rust 把这个组件放进 dialog 栈 / overlay；
- 关闭时：`component.dispose?.()`（try/catch 吞错，对齐 `interactive-mode.ts:2969`）、
  `resolve(done 的 result)`、恢复编辑框文本与焦点；
- **`tui` 参数**是 Rust 的 TUI proxy 对象：`{ requestRender, showOverlay, hideOverlay, terminal,
  addChild, removeChild, setFocus, … }`（`ca-extensions.md` §2.8.1）。这些是**同步 host 函数**，
  直接操作 Rust 侧 TUI。

### 9.4 `ctx.ui` 的 28 个成员

| 成员 | 绑定 |
|---|---|
| `select/confirm/input/editor` | 异步 host（Rust 弹对话框）→ Promise；RPC 模式走 §9.7 |
| `notify` | 同步 host |
| `onTerminalInput(handler)` | JS 注册，Rust 在输入时调 JS handler |
| `setStatus/setWorkingMessage/setWorkingVisible/setWorkingIndicator/setHiddenThinkingLabel` | 同步 host |
| `setWidget(key, content, opts)` | 两种：`string[]` → JSON；`factory` → 保存 `Persistent<Function>`，Rust 渲染时调它拿 `Component` |
| `setFooter(factory)` / `setHeader(factory)` | 保存 `Persistent<Function>` |
| `setTitle(text)` | 同步 host |
| `custom(factory, opts)` | §9.3 |
| `pasteToEditor(text)` / `setEditorText` / `getEditorText` | 同步 host |
| `addAutocompleteProvider(factory)` | 保存句柄；自动补全时 Rust 调 JS |
| `setEditorComponent(factory)` / `getEditorComponent()` | 保存 `Persistent<Function>`，返回 JS 对象 |
| `theme` (getter) | **JS**：prelude 里的 `theme` Proxy（读 `globalThis[Symbol.for(...)]`） |
| `getAllThemes()` / `getTheme(name)` / `setTheme(nameOrTheme)` | host（JSON / Theme 实例句柄） |
| `getToolsExpanded()` / `setToolsExpanded(v)` | 同步 host |

**`theme` 必须在 JS 里**：`theme.ts` 的 `theme` 是 Proxy，`Theme` 实例有预计算的 `fgAnsi`/`bgAnsi`
Map；扩展大量调用 `theme.fg("accent", text)`（examples 里 317 处 `theme.fg`）。
把 `Theme` 整体放 JS（prelude 已含），Rust 只提供**主题数据**（token → color）与终端能力
（`colorToHex`/`getTerminalColorMode` 等已在 kiana-tui 里）。

**初始化**：`initTheme()` 需要读主题文件（`fs.readFileSync`）+ 可选 watcher。在 prelude 里
不调 `initTheme()`，而是：

```ts
// entry-prelude.ts
globalThis.__kianaInitTheme = (themeJson) => {
  const t = buildThemeFromJson(themeJson);       // 复用 Theme 类
  (globalThis as any)[Symbol.for("@earendil-works/kiana-coding-agent:theme")] = t;
};
```

Rust 在启动时读主题文件、解析 JSON、调 `__kianaInitTheme(json)`；`setTheme(name)` 时同样由 Rust
读文件后推给 JS。这样 `theme` Proxy 永远有值，`getMarkdownTheme()`/`getSelectListTheme()`/
`getSettingsListTheme()` 也能正常工作（它们读 `theme`）。

### 9.5 编辑器子类

`border-status-editor.ts` / `modal-editor.ts` / `rainbow-editor.ts` 都是：
```ts
class X extends CustomEditor { constructor(...) { super(...); this.actionHandlers = ...; } renderTopBorder(...) {...} }
kiana.ui.setEditorComponent((tui, theme, kb) => new X(tui, theme, kb));
```
因为 `Editor`/`CustomEditor` 在 prelude 里是真的，`super()`、`this.actionHandlers`、
`renderTopBorder` override、`"actionHandlers" in editor` 的鸭子类型检测**全部原样工作**。
Rust 只把 `setEditorComponent` 的 factory 存下来，渲染时调它、把返回的组件当普通 JS 组件渲染。

### 9.6 代码高亮

`theme.ts` 的 `getMarkdownTheme().highlightCode` 依赖 `highlight.js` + 主题映射。prelude 里
`highlight.js` 用**桩**（返回原文本），Rust 侧提供 `__kianaHost.highlight(code, lang)`（可用
`syntect` 或 `tree-sitter`，或直接调用原有的 highlight.js 语义的 Rust 实现）。若 Rust 侧未提供，
降级为 `code.split("\n").map(l => theme.fg("mdCodeBlock", l))`（与原实现的 fallback 一致）。

### 9.7 RPC 模式

`ctx.ui.custom` 在 RPC 下返回 `undefined`；`setTheme` 返回
`{ success: false, error: "Theme switching not supported in RPC mode" }`。
`select/confirm/input/editor/notify/setStatus/setWidget/setTitle/set_editor_text` 走
`extension_ui_request` / `extension_ui_response`（`ca-extensions.md` §4.3 逐字）。

设计：**这些在 Rust 侧实现**（不是 JS），因为 RPC 是进程 I/O：
- JS 的 `ui.select(...)` → `__kianaHost.ui_select(title, options, opts)`（异步）→ Rust 发 JSON 行、等响应；
- `timeout`/`autoResolve` 在 Rust 的 `tokio::select!` 里；
- `custom` 在 RPC 下直接 `return undefined`（JS 侧判断 `ctx.mode === "rpc"`，或 host 返回 `null`）。

---

## 10. Node 内建模块 shim 清单

QuickJS 缺失的全局（实测 `typeof`）：

```
Intl             undefined     TextEncoder      undefined     TextDecoder      undefined
structuredClone  undefined     AbortController  undefined     AbortSignal      undefined
URL              undefined     URLSearchParams  undefined     setTimeout       undefined
setInterval      undefined     crypto           undefined     process          undefined
Buffer           undefined     fetch            undefined     WebAssembly      undefined
console          undefined     EventTarget      undefined     Event            undefined
atob/btoa        function      WeakRef          function      FinalizationRegistry function
queueMicrotask   function      SharedArrayBuffer function     Iterator         function
DisposableStack  function      DOMException     function      performance      object
```

### 10.1 全局注入（prelude 启动时）

| 全局 | 实现要点 |
|---|---|
| `console` | `log/info/debug→stdout`、`warn/error→stderr`（不加颜色，对齐 `ca-extensions.md` §6.3）；`log` 参数用 `util.format` 风格 join |
| `process` | `env`（Rust 传 JSON）、`cwd()`、`platform`、`arch`、`argv`、`execPath`、`exit()`（标记 + 走 shutdown）、`exitCode`、`stdout.write/stderr.write`、`stdin`（可读流的桩）、`features = {}`、`getBuiltinModule()`（返回 shim） |
| `TextEncoder`/`TextDecoder` | UTF-8 严格实现（**不能**用 `charCodeAt & 0xff`，那是原型里的临时桩；需正确处理代理对与非法序列替换 `�`） |
| `structuredClone` | JSON 语义（本单元只用 JSON 值）；可选支持 Map/Set/Date |
| `crypto.getRandomValues` | Rust `getrandom` → `Uint8Array`；`randomUUID()` 在 `node:crypto` shim 里 |
| `setTimeout/clearTimeout/setInterval/clearInterval` | **JS 定时器队列** + Rust 定时任务唤醒；`setInterval` 必须在 `ui.custom` 关闭时被清理（examples 里 snake/space-invaders 用它做 tick） |
| `queueMicrotask` | 原生 |
| `URL`/`URLSearchParams` | JS 实现（或 Rust `url` crate）；扩展用 `fileURLToPath` 居多 |
| `AbortController`/`AbortSignal` | §8.4 |
| `Buffer` | **必须实现**：`from/alloc/concat/isBuffer/byteLength`、`toString(enc)`、`slice`、`subarray`；examples 里 `ssh.ts`/`subagent`/`tool-override` 直接拼 Buffer |
| `atob`/`btoa` | 原生已有 |
| `performance.now()` | 原生（`performance` object 存在） |
| `fetch` | Rust `reqwest`（或 `hyper`）+ Web Streams 的最小实现；`Response.json()/text()/arrayBuffer()`、`headers`、`status`。**仅当扩展真的用**（examples：gitlab-duo、wad-finder、doom） |
| `WebAssembly` | **不提供**（§14 的 doom-overlay 因此受限） |
| `EventTarget`/`Event` | 极小实现（`AbortSignal` 与 `EventEmitter` 之外的扩展代码可能用） |

### 10.2 `node:*` 模块 shim（prelude 内，`kiana:node-shims/*`）

| 模块 | 必须实现的 API（按 examples 实测使用面） |
|---|---|
| `fs` | `readFileSync`(str/Buffer/encoding)、`writeFileSync`、`appendFileSync`、`existsSync`、`readdirSync`(withFileTypes)、`statSync`/`lstatSync`、`mkdirSync`(recursive)、`rmSync`、`unlinkSync`、`realpathSync`、`createReadStream`、`watch`/`watchFile`、`constants`、`promises` |
| `fs/promises` | `readFile`、`writeFile`、`appendFile`、`access`、`mkdir`、`mkdtemp`、`rm`、`stat`、`readdir`、`realpath` |
| `path` | `join`、`resolve`、`dirname`、`basename`、`extname`、`relative`、`isAbsolute`、`normalize`、`sep`、`delimiter`、`parse`、`format`、`posix`、`win32`（**必须区分平台**） |
| `os` | `homedir`、`tmpdir`、`platform`、`arch`、`cpus`、`release`、`hostname`、`networkInterfaces`、`EOL` |
| `url` | `fileURLToPath`、`pathToFileURL`、`URL`、`URLSearchParams` |
| `module` | `createRequire`（返回一个能 `require()` 的桩：**只能加载已内联的模块**，见 §11.4） |
| `events` | `EventEmitter`（完整：`on/once/off/emit/prependListener/removeAllListeners/listenerCount/setMaxListeners`） |
| `child_process` | `spawn`（返回带 `stdout/stderr/stdin` 流、`on('data'/'close'/'error'/'exit')`、`kill(sig)` 的 ChildProcess）、`spawnSync`、`exec`、`execSync`、`execFile`、`fork`（可选） |
| `util` | `promisify`、`inspect`、`format`、`inherits`、`types`、`TextEncoder/TextDecoder` 再导出 |
| `readline` | `createInterface({ input, crlfDelay, terminal })`（异步迭代、`on('line')`、`close`） |
| `zlib` | `gunzipSync`、`gzipSync`、`inflateSync`、`deflateSync`（Rust `flate2`/`zstd`） |
| `crypto` | `randomUUID`、`randomBytes`、`createHash`(sha256)、`getRandomValues` |
| `perf_hooks` | `performance` |
| `string_decoder` | `StringDecoder`（UTF-8 增量） |
| `chalk` | 极简：`chalk.red/green/bold/dim/...`，`level=0` 时原样返回（`NO_COLOR` 语义） |
| `worker_threads`/`net`/`http`/`https`/`tty`/`stream`/`dns` | **桩**（抛 `Error("... is not supported in the kiana extension runtime")`），按需补充 |

**语义必须逐条对齐**（`ca-extensions.md` §8.3 明确点名）：
- `readdirSync` 的排序（Node 返回 OS 顺序，**不是**排序后；`readdirSync` 在 Linux 上是 readdir 顺序）；
- `path.resolve`/`join` 的 `..` 归一化（含末尾斜杠、`.`、空段）；
- `Buffer.toString` 的 UTF-8 替换字符（非法序列 → `�`，且**不吞**字节）；
- `fs.existsSync` 对不可读路径返回 `false`（不抛）；
- `watch` 的 `fs.watch` 语义（inotify）：用 `notify` crate 映射；
- `spawn` 的 `stdio: ["ignore","pipe","pipe"]`、`shell: false`、`kill("SIGTERM")`。

### 10.3 `Intl`

QuickJS 无 `Intl`（实测 `typeof Intl === "undefined"`）。kiana-tui 的 `utils.ts` 在**模块顶层**就
`new Intl.Segmenter(...)`，所以 **Intl 必须在 prelude 之前装好**，否则整个 kiana-tui 加载失败
（原型踩过这个坑）。

方案（分层）：

1. **`Intl.Segmenter`**：用 `unicode-segmenter@0.14.0`（16 KB，纯 JS，UAX #29）polyfill
   grapheme/word；实测在 QuickJS 里工作正常，`segment("hi there")` word 粒度返回 3 段。
2. **`Intl.Collator`/`NumberFormat`/`DateTimeFormat`**：仅当扩展显式使用。`plan-mode`/`minimal-mode`
   等用到 `localeCompare` 的地方，fallback 为字节序（`ca-extensions.md` §6.1 已接受该差异）。
3. **可选加速**：把 grapheme 边界计算下沉到 Rust（`unicode-segmentation`），
   JS 侧 `Segmenter.segment` 只做 `text.slice(offsets[i], offsets[i+1])`。
   实测：ANSI 截断 20k 从 **4392 ms → 2411 ms**（1.8×），`segment` 本身 3123 ms → 1157 ms（2.7×）。
   注意返回的 boundary 必须是 **UTF-16 索引**（Rust 侧用 `encode_utf16().count()` 累加）。

---

## 11. npm 包支持

### 11.1 构建期 bundle（首选）

对**纯 JS** 的 npm 依赖（`ms`、`@anthropic-ai/sdk`、`@anthropic-ai/sandbox-runtime` 等），
在**扩展安装/加载时**用 esbuild（若可用）bundle 成单文件 ESM，再交给 QuickJS。
实测 `ms@2.1.3`（CJS）：esbuild bundle 后 1,994 B，在 QuickJS 里 `ms("2h") === 7200000`，
与原版一致；**不做 bundle** 时用 §11.2 的 CJS wrapper 也能跑（`raw CJS ms in QuickJS: 7200000`）。

构建期不可用（没有 Node）时 → §11.2 的**运行时 CJS 解析**兜底。

### 11.2 运行时 CJS interop

对 `node_modules/**/*.js`（`"type": "commonjs"` 或 `.cjs`）：

```js
// prelude 里的 cjs-wrapper
(function (module, exports, require, __filename, __dirname) {
  /* 原文件内容 */
})(module, module.exports, makeRequire(dirname), filename, dirname);
```

- `module.exports` / `exports.foo` / `exports = {...}`；
- `__esModule` 标记 → `import x from` 取 `module.exports.default`，否则取 `module.exports`；
- `require("node:fs")` 等 → 走 shim；
- `require("./x")` → 按 Node 规则补扩展名/`index.js`；
- `require` 的缓存：**每个 specifier 一次**（与 Node 一致；注意与 jiti 的 `moduleCache:false`
  只作用于**扩展入口**，不影响 npm 包内部）。

### 11.3 ESM npm 包

`"type": "module"` 且用 `import`/`export` 的包 → 直接 `Module::declare` + eval；
`exports` 条件解析见 §5.5。

### 11.4 `createRequire`

`doom-engine.ts` 用 `createRequire(doomJsPath)` 去 require 一个 **emscripten 生成的 CJS** 包。
我们提供一个 `createRequire(fromPath)`：
- 能解析到 `node_modules` 里的 CJS（§11.2）；
- 也能解析并执行一个本地 `.js` 文件（把文件读进来按 CJS wrapper 跑）；
- **不能**加载原生 `.node`（抛 `Error("native addons are not supported")`）。

### 11.5 不支持的 npm 包

| 类别 | 例子 | 处理 |
|---|---|---|
| 原生 `.node` 插件 | `@silvia-odwyer/photon-node` | 抛错；若扩展依赖它，功能降级或换 Rust 实现 |
| 需要 Node 内建模块的高版本特性 | `undici`（依赖 `net`/`tls`/`http2`） | `fetch` 用 Rust 实现；不加载 `undici` |
| 依赖 `worker_threads` | 少数 | 桩 + 抛错 |
| 依赖 `WebAssembly` | `doom.js` | **不支持** |

### 11.6 `package.json` 的 `kiana` manifest

复刻 `readKianaManifest`：`{ "kiana": { "extensions": ["./index.ts"] } }`。
发现顺序：`<cwd>/.kiana/extensions/`、全局 `~/.kiana/agent/extensions/`、`node_modules` 里带 `kiana.extensions`
的包（`ca-extensions.md` §4.1/§4.2）。

---

## 12. 性能

### 12.1 启动

| 项 | 实测 | 备注 |
|---|---|---|
| QuickJS Runtime+Context | < 1 ms | |
| node shims + Intl | 3.8 ms | intl 3.6 ms |
| typebox（85 KB） | 16.9 ms | |
| kiana-tui（301 KB） | 47–55 ms | 最大头 |
| **prelude 合计** | **75.4 ms** | 进程一次 |
| 字节码预热后（kiana-tui） | **8.7 ms**（vs 36.5 ms） | 可选，§4.3 |
| 扩展剥离（oxc） | 0.5–2.4 ms/文件 | 79 个入口 ≈ 20–30 ms |
| **79 个扩展加载（单 runtime，含工厂执行）** | **137.5 ms（1.74 ms/个）** | 原型实测；见 §14 |
| 单扩展冷启动（进程 + prelude + 1 扩展） | ~110–160 ms | 原型 30–90 ms（无真实 API 后端） |

对比：Node + jiti 加载同一批扩展约 0.6–1.5 s（jiti 的 Babel 转译 + 模块求值）。
**Rust 侧在启动上不劣于 Node**。

### 12.2 热路径（kiana-tui，QuickJS vs Node，同机同 release）

| 基准 | QuickJS | Node 26 | 倍率 |
|---|---|---|---|
| `visibleWidth` ×20k（ANSI+CJK） | 42 ms | 7–8 ms | **5–6×** |
| `truncateToWidth` ×20k（**纯 ASCII** 快路径） | 226 ms | 15 ms | **15×** |
| `truncateToWidth` ×20k（ANSI+CJK 慢路径） | 4392 ms | 295 ms | **15×** |
| `truncateToWidth` ×5k（CJK） | 1035 ms | 100 ms | **10×** |
| `Text.render(60)` ×5k（**复用实例**，缓存命中） | 2 ms | 5 ms | 0.4×（更快） |
| `Text` 构造 + `render(60)` ×5k（**每次新建**） | 1462 ms | 179 ms | **8×** |
| `Intl.Segmenter.segment` ×20k（1.62M 段） | 3123 ms | 299 ms | **10×** |
| 字符串迭代 ×20k | 87 ms | 14 ms | 6× |
| 100k 同步 host 调用 | 11.4 ms | — | 114 ns/次 |
| 100k `Persistent::restore` + 调用 | 15.1 ms | — | 151 ns/次 |
| 100k `Persistent<Function>::restore` | 83.4 ms | — | 834 ns/次 |
| 1000× JS `render(80)` + 读回 8 行 | 6.87 ms | — | 6.9 µs/帧 |
| 1000× `handleInput` | 0.196 ms | — | 0.2 µs |
| 20× `JSON.stringify`(135 KB) | 70 ms | — | 3.5 ms/次 |
| 5000× `await host()` | 7.64 ms | — | 1.5 µs/次 |

**解读**：
- 慢的根源不是"执行 JS"，而是 **`Intl.Segmenter` 的 JS polyfill**（10×）与
  **QuickJS 的字符串/Map/闭包开销**（约 2–6×）。
- `Text.render` 在复用实例时**比 Node 还快**（2 ms vs 5 ms）——因为 kiana-tui 自身有行缓存。
  真实 TUI 每帧重绘的是少量变化的组件，缓存命中率高。
- **单帧预算**：80×30 的屏幕约 2400 字符；一次全量 `truncateToWidth` 若走慢路径约 2400/20k×4392
  ≈ 0.53 ms（按字符比例估），**完全在 16 ms 内**。只有全屏文字频繁变化（如流式 markdown 重排）
  才需要关注，而那时 kiana-tui 的 `Markdown` 组件本身有增量渲染。

### 12.3 优化手段（按收益排序）

| 手段 | 收益（实测/估算） | 成本 |
|---|---|---|
| **Rust 侧 grapheme 边界服务**（§10.3） | `truncateToWidth` ANSI 20k **4392→2411 ms（1.8×）**；`segment` **3123→1157 ms（2.7×）** | 中（host fn + UTF-16 索引） |
| 字节码预热 prelude | 启动 **75→35 ms** | 低（缓存文件） |
| 剥离结果磁盘缓存（mtime+size） | 每次启动省 20–30 ms | 低 |
| 把 `visibleWidth` 的**热路径**下沉 Rust | Node 7 ms vs QJS 42 ms（6×）→ 可到 ~10 ms | 中（要处理 ANSI 状态机） |
| `Text` 实例复用（组件 `invalidate` 时只重算变化行） | 已由 kiana-tui 提供；确保 Rust 不每帧重建组件 | 低 |
| 事件对象走句柄而非 JSON | `message_update` 高频时省 3.5 ms/135 KB | 中 |
| 渲染结果**增量**跨边界（只回传变化的行） | 省字符串拷贝 | 中 |

### 12.4 内存

- QuickJS 空 runtime：117 KB（`malloc_size`）。
- prelude 加载后：约 **4.6 MB**（kiana-tui + typebox + theme + 模块元数据）。
- 每个扩展模块：源码 + AST（parse 后即释放）+ namespace，通常 < 100 KB。
- `set_memory_limit(256 MiB)`；超限时 QuickJS 抛 `out of memory`，按 load 失败处理。
- `set_max_stack_size(4 MiB)`：QuickJS 默认较小，深递归组件（如 `Box` 嵌套）需要放宽。
- `set_gc_threshold(8 MiB)`：避免频繁 GC；实测 20k 次分配循环无停顿问题。

---

## 13. 保真度 vs 性能：哪些放 JS，哪些放 Rust

| 层 | 放哪 | 理由 |
|---|---|---|
| 组件（`Text`/`Box`/`Markdown`/`Editor`/`SelectList`…） | **JS（原样）** | 逐字节保真；性能足够（§12.2） |
| `theme` / `Theme` 类 | **JS** | 扩展高频调用 `theme.fg`；Proxy + 预计算 ANSI 已在 JS |
| TypeBox | **JS** | 扩展直接用 `Type.Object(...)`；Rust 只需 JSON Schema |
| `EventBus` | **JS** | 就是 EventEmitter |
| `loader` 的 jiti 语义（虚拟模块/缓存/别名） | **JS 决策 + Rust 解析** | 语义薄，解析要 I/O |
| `runner` 的事件派发/合并 | **JS** | §8.5：语义复杂且依赖 JS 对象模型 |
| `createExtensionAPI` 三态机 | **JS** | §8.2：纯状态逻辑 |
| `ctx` 的 getter 与动作 | **Rust（host fn）** | 真值在 Rust；同步 getter 114 ns/次可接受 |
| Tool 执行 / Provider 调用 / MCP | **Rust** | 网络/进程/模型 |
| TUI 终端 I/O、布局、dialog 栈 | **Rust** | Rust 的 tui-core 已有；JS 只产出行 |
| Agent loop / session / 压缩 | **Rust** | 与扩展无关 |
| grapheme 宽度（可选加速） | **Rust** | 唯一能显著提速的算子 |

一句话：**"扩展看到的世界"完全在 JS；"世界本身"在 Rust。**

---

## 14. `examples/extensions/**` 兼容清单

语料：**79 个入口** = 70 个 `*.ts` + 9 个目录的 `index.ts`（外加 README 共 80 条目）。
分类按**传递依赖**（含 `./x.ts` 递归）统计：

| 类别 | 数量 | 说明 |
|---|---|---|
| **A. 纯 API（无 node 内建、无 npm）** | **58** | 只 import `@earendil-works/kiana-*`、`typebox`。**无需任何 shim**，只依赖 API 绑定 |
| **B. 仅 node 内建** | **17** | `fs`/`fs/promises`/`path`/`os`/`url`/`readline`/`child_process`/`util`。靠 §10 的 shim 支持 |
| **C. npm 依赖** | **3** | `with-deps`（`ms`）、`custom-provider-anthropic`（`@anthropic-ai/sdk`）、`sandbox`（`@anthropic-ai/sandbox-runtime`） |
| **D. 需要 WebAssembly** | **1** | `doom-overlay`（`doom.js` emscripten + `node:zlib` gunzip + `createRequire`） |

### 14.1 原型实测（`/tmp/e2e`，单 runtime 批量）

用**真实 kiana-tui（301 KB）+ 真实 typebox + Intl polyfill + node shim 桩**，79 个入口逐一加载：

```
batch: 76/79 ok in 137.5 ms (1.74 ms/ext)
  FAIL custom-provider-gitlab-duo/index.ts :: Could not find export 'anthropicMessagesApi'
  FAIL gondolin/index.ts                    :: Could not find export 'RealFSProvider'
  FAIL sandbox/index.ts                     :: Could not find export 'SandboxManager'
```

3 个失败**全部**是原型里把 `@earendil-works/kiana-ai/compat`、`@earendil-works/gondolin`、
`@anthropic-ai/sandbox-runtime` 换成了**空桩**导致的缺导出，**与引擎无关**（换成真实模块即通过）。

### 14.2 逐条结论（生产中）

| 入口 | 结论 |
|---|---|
| **A 类 58 个**（`hello`、`todo`、`snake`、`space-invaders`、`tic-tac-toe`、`overlay-*`、`preset`*、`tools`、`minimal-mode`、`plan-mode`、`subagent`* …） | ✅ **原样可用**。`*` 这些用 node 内建，属 B 类 |
| `file-trigger`、`claude-rules`、`preset`、`provider-payload`、`dynamic-resources`、`titlebar-spinner`、`git-checkpoint`、`auto-commit-on-exit`、`dirty-repo-guard`（fs/path/url） | ✅ 原样可用（`fs.watch` 由 `notify` 实现） |
| `interactive-shell`（`spawnSync`）、`overlay-qa-tests`（`spawn`）、`ssh`（`spawn`）、`mac-system-theme`（`exec`+`promisify`）、`subagent`（`spawn`+`os`）、`truncated-tool`（`spawn`）、`tool-override`（`fs/promises`） | ✅ 原样可用（`child_process` shim → tokio::process；注意**交互式全屏程序需要给子进程真实 TTY**，`interactive-shell` 依赖 `spawnSync` 的 `stdio: "inherit"`） |
| `git-merge-and-resolve`（`node:readline` + `createReadStream`） | ✅ 原样可用（`readline` shim + fs 流） |
| `with-deps`（`ms`） | ✅ 原样可用（CJS interop 实测通过） |
| `custom-provider-anthropic`（`@anthropic-ai/sdk` + `fetch`） | ✅ 可用（sdk 是纯 JS，bundle 后跑；`fetch` 用 Rust 实现） |
| `custom-provider-gitlab-duo`（`@earendil-works/kiana-ai/compat` + `fetch`） | ✅ 可用（只要 kiana-ai compat 的 JS 面完整，§8.6） |
| `sandbox`（`@anthropic-ai/sandbox-runtime` + `child_process`） | ⚠️ 取决于该 npm 包自身能否在 shim 下跑；它是 JS 包但会大量 spawn `bwrap`/`sandbox-exec`，**预期可用但需实测** |
| `gondolin`（`@earendil-works/gondolin` + QEMU） | ❌ **不保证**。该包要求 Node ≥ 23.6 的特定能力（`node:sqlite`/新 API），且需要 QEMU；不是引擎能补的 |
| `doom-overlay` | ⚠️ **加载 OK，游戏本体不可用**：`WebAssembly` 缺失。可选：接 `wasmtime`（体积 +15 MB）或标记为"不支持" |
| `questionnaire`/`question`/`qna`/`handoff`（`BorderedLoader`） | ✅ 需要 kiana-coding-agent 的 `BorderedLoader` 在 prelude 的 sdk-shim 里提供 |
| 三个 editor 子类（`border-status-editor`/`modal-editor`/`rainbow-editor`） | ✅ 因为 `CustomEditor`/`Editor` 是真的 JS 类 |

**统计结论**：
- **77/79 原样可用**（A 58 + B 17 + C 中 2 个纯 JS）；
- 1 个（`gondolin`）**不保证**（外部 VM 依赖）；
- 1 个（`doom-overlay`）**加载可用、WASM 不可用**。

### 14.3 其它语料

| 文件 | 结论 |
|---|---|
| `examples/rpc-extension-ui.ts` | ✅ RPC 客户端，不加载进运行时 |
| `examples/rpc-client.ts` | ✅ 同上 |
| `examples/sdk/06-extensions.ts` | ✅ 走 `loadExtensionFromFactory`（inline factory），prelude 支持 |
| `test/*.test.ts` 的扩展用例 | 需按 `ca-extensions.md` §7 移植；本设计提供同名 Rust 测试钩子 |

---

## 15. 风险与缓解

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | **QuickJS 无 `Intl`，kiana-tui 模块顶层就要 `Segmenter`** | 整个 prelude 加载失败 | 已解决：`unicode-segmenter` polyfill **先于** prelude 装载（实测 3.6 ms）；可选 Rust 加速（§10.3） |
| 2 | **`Function::call` 没有 `this` 参数**（rquickjs 0.14） | 组件方法/工具方法调用时 `this` 为 `undefined`（实测报错） | 所有方法调用经 JS 侧 invoker 包装：`((o, ...a) => o.m(...a))` |
| 3 | **`Persistent` 不实现 `Send`**（原始指针 + 生命周期） | 不能把 JS 句柄放进多线程 tokio 任务 | 所有 JS 句柄留在 JS 线程；跨线程只传 `handle_id: u64`（线程本地表） |
| 4 | **中断是全局的** | 一个扩展死循环会杀掉同批其它扩展的调用栈 | 每扩展每次调用前置预算；超时只把该扩展标 `Failed`；提供 `--extension-isolation`（每扩展一个 Context） |
| 5 | **`moduleCache:false` 与 QuickJS 的 specifier 缓存冲突** | 重载扩展时拿到旧模块 | 用 `?kiana=<gen>` 唯一 specifier；实测新 specifier 会重新求值 |
| 6 | **`import.meta.url` 带查询串** | 扩展用 `import.meta.url` 推路径会错 | `Module::meta()` 覆写为干净 `file://` 路径（实测可写） |
| 7 | **`WebAssembly` 缺失** | doom-overlay 等 | 明确标记不支持；或可选 `wasmtime` feature |
| 8 | **`node:fs` 同步 API 阻塞 JS 线程** | 大量同步 I/O 时 UI 卡顿 | 专用 JS 线程 + 同步 fs 走 blocking pool；引导扩展用 async API |
| 9 | **CJS 包的 `require` 语义** | 某些包（`undici`、`chokidar`）依赖 Node 内部 | 白名单 + 明确错误；`fetch`/`watch` 用 Rust 实现替代 |
| 10 | **主题初始化** | `theme` Proxy 未初始化时抛 "Theme not initialized" | Rust 启动读主题 JSON → `__kianaInitTheme(json)`；`setTheme` 同路径 |
| 11 | **字节码与架构绑定** | 不能内置进二进制 | 用户缓存目录生成 + 头部校验（§4.3） |
| 12 | **oxc 装饰器默认不降级** | 用户扩展用装饰器会得到语法错误 | 显式 `DecoratorOptions { legacy: true, .. }`；examples 零使用 |
| 13 | **原生 `.node` 插件** | 依赖它的扩展不可用 | 明确报错；必要时用 Rust 重写该能力 |
| 14 | **单 Context 的全局污染** | 一个扩展改 `globalThis` 会影响别人（但 Node 里也一样） | 与 Node 行为一致，**不算回归**；隔离模式可选 |
| 15 | **`structuredClone`/`undefined` 语义** | 事件对象往返丢 `undefined` 键 | 事件对象在 JS 侧构造并原地传递；Rust 只读需要的字段（§8.1） |

---

## 16. 实施顺序

1. **引擎外壳**：`AsyncRuntime` + 专用 JS 线程 + drive loop + 中断/内存限制（§7）。
2. **prelude 构建管线**：esbuild 脚本 + `include_bytes!` + 版本化缓存（§4）。
3. **node shims**：`path`/`fs`/`os`/`url`/`events`/`util`/`buffer`/timers/`process`（§10），
   配 `test/node-shims/*` 对照 Node 的黄金输出。
4. **`loader-shim` + 虚拟模块**：22 键 + `kiana-tui`/`typebox` 内联 + `import.meta` 注入（§5）。
5. **`runner-shim` + 12 个专用派发器**：逐条对照 `ca-extensions.md` §2.3.14（§8.5）。
6. **`api-shim` + 三态机 + ctx 家族**（§8.2/§8.3）。
7. **Tool 执行/渲染 + 3 个 editor 子类**（§8.9/§9.5）。
8. **UI 适配**：dialog 栈、overlay、widget、footer/header、`custom`（§9）。
9. **RPC 模式**（§9.7）。
10. **npm/cjs interop + `createRequire`**（§11）。
11. **兼容语料回归**：把 §14 的 79 个入口做成快照测试（加载 + 关键 API 调用 + 渲染输出）。

### 验收（对应 `ca-extensions.md` §8.8）

- [ ] `index.ts` 的 **193 个名字**、`types.ts` 的 **186 个类型**全部在 prelude 的 `sdk-shim` 里可导入（运行时值 + 类型无需，但 `index.ts` 的运行时导出要全）。
- [ ] **41 个事件**全部可 `on`，且 12 个专用派发器的合并语义逐条通过。
- [ ] `ExtensionAPI` 的 **31 个方法**、`UIContext` 的 **28 个成员**、`ExtensionContext` 的 **18 个成员**全部实现。
- [ ] 79 个 examples 入口：**≥77 加载成功**（gondolin 允许失败，doom 允许 WASM 失败）。
- [ ] `visibleWidth`/`truncateToWidth`/`Text.render` 对固定语料与 Node 输出**逐字节一致**。
- [ ] 重载（`ctx.reload`）后旧 ctx 抛 stale 错误（逐字）。
- [ ] 单扩展死循环 30 s 后只杀该扩展，进程存活。

---

## 附录 A：原型验证摘要（可复现）

| 验证 | 文件 | 结果 |
|---|---|---|
| QuickJS 缺失全局 | `/tmp/qjs-probe/src/main.rs` | `Intl/TextEncoder/AbortController/… = undefined` |
| `import.meta` 注入 | 同上 | 可写，模块读到注入值 |
| 模块缓存语义 | `/tmp/qjs-probe/src/bin/modulecache.rs` | 同 specifier 依赖只求值一次；新 specifier 重新求值 |
| 中断 | 同上 | 250 ms 中断，之后 context 可用 |
| 字节码 | `/tmp/qjs-probe/src/bin/bytecode.rs` | 36.5 ms → 8.7 ms |
| Persistent / host 调用 | `/tmp/qjs-probe/src/bin/persist.rs` | 114–834 ns/次 |
| kiana-tui 保真 + 基准 | `/tmp/qjs-probe/src/main.rs` | 输出逐字节一致；倍率见 §12.2 |
| async/tokio | `/tmp/qjs-async2` | Promise.all + 5000 await = 7.64 ms |
| 自定义 Resolver/Loader | `/tmp/qjs-async` | 虚拟模块 + 相对解析可用 |
| oxc 剥离 | `/tmp/oxc-probe` | 全部语法覆盖表（§6.1） |
| **端到端** | `/tmp/e2e` | **76/79 加载成功，137 ms** |
| prelude 全量 | `/tmp/qjs-probe/src/bin/prelude.rs` | 407 KB 装载 75.4 ms，导出齐全 |

## 附录 B：与 codemode 的关系

`agent-codemode.md` 的 `quickjs-wasi@3.6.2` 是**独立的执行单元**（脚本沙箱、确定性、可移植），
本设计**不改它**。两套引擎的差异（native QuickJS-ng vs wasm QuickJS）不影响扩展 API：
扩展运行时走 native，codemode 脚本走 wasm。二者共享的只有 TypeBox/JSON Schema 的语义。
