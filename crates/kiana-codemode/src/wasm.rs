//! wasm.ts：`quickjs.wasm` 的定位、读取与编译（每个路径一次）。
//!
//! TS 源：`packages/codemode/src/wasm.ts`。TS 的 `CodemodeWasmModule = object` 是不透明的
//! `WebAssembly.Module`（`WebAssembly.compile(bytes)` 的结果）；Rust 侧等价物是编译好的
//! `wasmtime::Module`，用 `Arc` 共享（`Module` 不可序列化、克隆昂贵但 `Arc` 克隆廉价，
//! 与 TS 的 structured clone 共享同一份编译代码语义一致）。
//!
//! # 与 TS 的一处实现差异：两档 wasm 栈预算
//!
//! TS 的 `WebAssembly.Module` 与具体栈无关；两档 `maxStackSize`（worker 512 KiB、inline
//! 256 KiB）只是 QuickJS 自己的守卫。Rust 侧的深递归边界还取决于 wasmtime 的
//! `max_wasm_stack`，而它是 **Engine** 级配置、Module 只能在同一个 Engine 上实例化。为了让
//! worker 与 inline/remote 分别复现 TS 在 4 MB worker 线程 / 约 1 MB 主线程上的成功与崩溃
//! 边界，这里持有两个 Engine（[`shared_engine`] 与 `inline_engine`），并在
//! [`load_quickjs_wasm`] 里各编译一份（并行，见 `compile` 的注释）。对调用方仍然只是一个
//! 不透明句柄（TS 的 `CodemodeWasmModule`），clone 共享两份编译结果。
//!
//! # 与 TS 的对应
//!
//! - `loadQuickJSWasm(path?)` → [`load_quickjs_wasm`]：`path` 缺省时用**嵌入的**
//!   `js/quickjs.wasm`（与 TS 运行时从 `quickjs-wasi` 包读到的文件逐字节相同，
//!   sha256 `d4c9375f…bbd9`、637405 字节）。TS 用 `createRequire(import.meta.url)
//!   .resolve("quickjs-wasi/quickjs.wasm")` 找文件；Rust 二进制没有 node_modules，
//!   因此把同一份文件编进二进制（`include_bytes!`），`Some(path)` 时读该路径。
//! - 每个路径只读+编译一次并缓存（`Map<string, Promise<Module>>` → 每个 key 一个
//!   `tokio::sync::OnceCell<Result<..>>`）。TS 缓存的是 **Promise**：并发调用共享同一次
//!   读取与编译，失败的 Promise 被 `modules.delete(resolved)` 摘掉，下一次调用重试。
//!   这里缓存每个 key 的初始化结果——槽位在**编译之前**就放进 Map，所以并发调用者拿到的是
//!   同一个槽位，只会发生一次 `compile()`；失败时把该槽位从 Map 里摘掉（持有旧槽位的调用者
//!   仍看到同一个错误，与 TS 共享同一个被拒 Promise 一致），下一次调用重新建槽重试。成功路径
//!   没有“删除”，因此失败的调用**不可能**误删并发调用刚存下的成功模块。
//! - 编译失败的错误文本：TS 抛 `WebAssembly.compile` 的 `CompileError`（其 `message` 进入
//!   `Failed to load QuickJS: ${errorMessage(error)}`）；Rust 抛 [`CodemodeWasmError`]，
//!   `Display` 是该文本，由调用方（inline/host 的 launcher）拼进同一前缀。
//! - 读取失败的错误文本：TS 的 `readFile` 拒绝值是 Node 的 `Error`，`errorMessage` 取
//!   `message`，即 `ENOENT: no such file or directory, open '<path>'`。这里按 libuv 的
//!   errno → (名字, 文案) 表逐字复刻全部常见错误码（`read_error`），不再只覆盖
//!   ENOENT/EACCES/EISDIR。
//! - 路径含 NUL 字节时，Node 在**打开之前**就由 `fs` 的 `validatePath` 抛
//!   `ERR_INVALID_ARG_VALUE`（题名 TypeError），消息是
//!   `The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received <inspect>`
//!   （`<inspect>` 是 `util.inspect(path)`：选引号、转义控制字符、超过 128 个 UTF-16 码元时
//!   截断加 `...`）。libuv 的 `EINVAL` 走不到，因此这里用 `nul_path_error` 复刻那一条。
//!
//! # 谁在调用
//!
//! - `runtime::host` 的默认 spawner 在没有传入 `options.wasm` 时用 [`load_quickjs_wasm`]
//!   加载嵌入模块；失败文本随后由 `runtime::host` 的加载路径拼上 `Failed to load QuickJS: `
//!   前缀。
//! - `runtime::inline` 用 `with_inline_stack()` 把句柄切到 inline 档（不重新编译）。
//! - `runtime::vm` 用 `engine()` / `module()` 这两个访问器成对取出 Engine 与 Module 去
//!   实例化（Module 只能在使用它的 Engine 上实例化）。
//! - 集成测试（`tests/sandbox.rs`、`tests/vm.rs` 等）直接用 [`load_quickjs_wasm`] 取嵌入模块。
//!
//! # 执行流程
//!
//! 1. [`load_quickjs_wasm`] 先做 NUL 路径校验：含 NUL 时直接返回 Node 的
//!    `ERR_INVALID_ARG_VALUE` 文本，不建缓存槽位（因此既不占用也不污染缓存）。
//! 2. 以 `Option<PathBuf>` 为键取得或创建 `OnceCell` 槽位（`None` 是嵌入模块的独立键）。
//! 3. 首个调用者在槽位里执行 `compile`：`Some(path)` 用 `std::fs::read` 读取，`None` 用
//!    `include_bytes!` 的嵌入字节；随后在两个 Engine 上各编译一份（inline 档由
//!    `std::thread::scope` 子线程与 default 档并行编译）。
//! 4. 成功后模块留在槽位里供后续调用复用；失败时按 `Arc` 身份摘槽，下一次调用重建槽位重试。
//!
//! 编译是**阻塞**操作：读取与 default 档编译在当前线程同步完成，inline 档在 scope 子线程
//! 并行后 `join`，没有 `spawn_blocking`（见 `compile` 的注释）。`async` 只提供与 TS
//! `Promise` 相同的调用形态，真正的读取与编译不会因为 `async` 就变成可让出的 I/O。
//!
//! # 注意事项
//!
//! - 缓存是进程级的：成功槽位没有删除路径，键（含非 UTF-8 路径的原始字节）与编译好的
//!   `wasmtime::Module` 一直驻留到进程退出；只有失败槽位会被摘掉。本文件没有清理或失效 API。
//! - 缓存锁是 `std::sync::Mutex`；`slot_for` / `evict` 在锁中毒时经 `expect` 直接 panic
//!   （锁内只做 HashMap 查/删与克隆，没有会 panic 的代码）。
//! - 两条路径各自持有独立槽位：`None`（嵌入模块）与任何真实路径互不共享，即使真实路径恰好
//!   指向同一份字节。
//!
//! feature `wasm` 关闭时（骨架期的默认），本模块仍可编译：`CodemodeWasmModule` 是一个
//! 不透明占位类型，[`load_quickjs_wasm`] 返回**不带** `Failed to load QuickJS: ` 前缀的
//! `the wasm backend is disabled (enable the "wasm" feature)`；调用方（`runtime::host` 的
//! 加载路径）拼上该前缀后，成为 `kind: "sandbox"` 的 `message`。这与 TS 在加载失败时的
//! 可观察行为一致（一次 `kind:"sandbox"` 失败），且满足
//! 「Rust 侧给出与 TS 等价的明确错误」的要求（ARCHITECTURE.md §8 第 22 条）。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(feature = "wasm")]
pub use wasmtime_impl::CodemodeWasmModule;

/// 编译好的 QuickJS 模块（TS: `WebAssembly.Module` 的不透明句柄）。
#[cfg(feature = "wasm")]
mod wasmtime_impl {
    use std::sync::Arc;

    use wasmtime::{Engine, Module};

    /// 一个句柄用哪一档 wasm 栈预算（见 [`super::shared_engine`] 与 `inline_engine`）。
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum StackProfile {
        /// worker 后端：复现 TS 的 Node worker 线程（默认 4 MB 原生栈）。
        Default,
        /// inline/remote 后端：复现 TS 把 VM 跑在调用线程上的情形（Node 主线程约 1 MB）。
        Inline,
    }

    /// `CodemodeWasmModule = object`：编译好的模块，可跨线程共享（structured clone 的等价）。
    ///
    /// 内部持有两份 `wasmtime::Module`。wasmtime 的 `max_wasm_stack` 是 **Engine** 级设置，
    /// 而 Module 只能在与它相同的 Engine 上实例化，所以两档栈预算各需要一份编译结果；两份都
    /// 在 [`super::load_quickjs_wasm`] 里编译好（TS 的 `WebAssembly.compile` 也只发生在那
    /// 一刻），执行期间不再编译——否则一次带 5 秒 deadline 的 remote 执行会在第一次用到
    /// inline 档时被十几秒的编译拖成 timeout。
    ///
    /// # 不变量
    ///
    /// `default` 与 `inline` 由**同一份字节**编译（`super::compile` 只读一次文件），并分别
    /// 属于 `shared_engine()` 与 `inline_engine()`。`engine()` 与 `module()` 通过 `profile`
    /// 成对选择，调用方不能只取其中一个去实例化：`wasmtime::Module` 只能在编译它的
    /// Engine 上使用。
    #[derive(Clone)]
    pub struct CodemodeWasmModule {
        default: Arc<Module>,
        inline: Arc<Module>,
        profile: StackProfile,
    }

    impl CodemodeWasmModule {
        /// 用两份编译结果建立句柄（[`super::compile`] 用）。
        pub(super) fn new(default: Arc<Module>, inline: Arc<Module>) -> Self {
            CodemodeWasmModule {
                default,
                inline,
                profile: StackProfile::Default,
            }
        }

        /// 同一个模块的 inline/remote 档（共享两份编译结果，只切换用哪一份）。
        pub(crate) fn with_inline_stack(&self) -> Self {
            CodemodeWasmModule {
                default: Arc::clone(&self.default),
                inline: Arc::clone(&self.inline),
                profile: StackProfile::Inline,
            }
        }

        /// 这一档用的 Engine（`Store`/`Linker` 必须用它，Module 也只能在它上面实例化）。
        pub(crate) fn engine(&self) -> &'static Engine {
            match self.profile {
                StackProfile::Default => super::shared_engine(),
                StackProfile::Inline => super::inline_engine(),
            }
        }

        /// 这一档的 `wasmtime::Module`。
        pub(crate) fn module(&self) -> Arc<Module> {
            match self.profile {
                StackProfile::Default => Arc::clone(&self.default),
                StackProfile::Inline => Arc::clone(&self.inline),
            }
        }
    }

    impl std::fmt::Debug for CodemodeWasmModule {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("CodemodeWasmModule")
        }
    }
}

/// feature `wasm` 关闭时的占位模块类型（不可构造，`load_quickjs_wasm` 永远失败）。
#[cfg(not(feature = "wasm"))]
#[derive(Clone)]
pub struct CodemodeWasmModule {
    _private: (),
}

#[cfg(not(feature = "wasm"))]
impl fmt::Debug for CodemodeWasmModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodemodeWasmModule")
    }
}

#[cfg(not(feature = "wasm"))]
impl CodemodeWasmModule {
    /// 非 wasm 构建没有可用模块，也没有栈档之分：原样返回。
    ///
    /// 该类型在 feature 关闭时不可构造（`load_quickjs_wasm` 永远失败），所以这份实现只为让
    /// 无条件的 `runtime::inline` 通过编译；执行在启动 VM 之前就以 `sandbox` 错误结算。
    pub(crate) fn with_inline_stack(&self) -> Self {
        self.clone()
    }
}

/// 嵌入的 `quickjs.wasm`（与 TS 侧 `quickjs-wasi@3.6.2` 的 `quickjs.wasm` 逐字节相同）。
#[cfg(feature = "wasm")]
pub const QUICKJS_WASM_BYTES: &[u8] = include_bytes!("../js/quickjs.wasm");

/// 嵌入的 `quickjs.wasm`（feature `wasm` 关闭时不嵌入，避免 637 KB 的死重量）。
#[cfg(not(feature = "wasm"))]
pub const QUICKJS_WASM_BYTES: &[u8] = &[];

/// 加载/编译 `quickjs.wasm` 失败。
///
/// `Display` 是去掉 `Failed to load QuickJS: ` 前缀的原因文本；调用方（launcher）加上前缀后
/// 成为 `kind: "sandbox"` 的 `message`，与 TS 的 `` `Failed to load QuickJS: ${errorMessage(error)}` ``
/// 一致（`errorMessage` 对 `Error` 取 `message`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodemodeWasmError {
    /// 原因文本（TS 的 `error.message`）。
    pub message: String,
}

impl CodemodeWasmError {
    /// 用原因文本构造。
    pub fn new(message: impl Into<String>) -> Self {
        CodemodeWasmError {
            message: message.into(),
        }
    }
}

impl fmt::Display for CodemodeWasmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodemodeWasmError {}

/// 按路径缓存“加载+编译”的结果：`Arc<OnceCell<Result<..>>>`。
///
/// 对应 TS 的 `Map<string, Promise<CodemodeWasmModule>>`。TS 在读取**之前**就把 Promise 放进
/// Map，所以并发调用共享同一次读取与编译；这里同样在编译之前把槽位放进 Map。槽位里存
/// `Result` 而不是只存成功值，是为了让并发调用者共享同一次失败（TS 里它们共享同一个被拒
/// Promise）；失败后 [`evict`] 摘掉槽位，下一次调用重新建槽（TS 的 `modules.delete`）。
///
/// 键是 `Option<PathBuf>`（Linux 上即原始字节），不是 `to_string_lossy()`：两个只在非法字节上
/// 不同的非 UTF-8 路径必须各自占一个槽位，否则第二个调用会拿到第一个路径编译出的模块。
/// `None` 是嵌入模块自己的键：TS 里它是 `createRequire().resolve("quickjs-wasi/quickjs.wasm")`
/// 得到的绝对路径，Rust 二进制没有该路径，因此不复用任何真实路径的槽位。
///
/// 用 `tokio::sync::OnceCell` 而不是 `std::sync::OnceLock`：`get_or_init` 是 async 的，与 TS
/// 的 Promise 形态一一对应；`Arc` 包裹则让 [`evict`] 用指针相等区分“同一个槽位”与“同一 key
/// 上重建的新槽位”。
type Slot = Arc<tokio::sync::OnceCell<Result<CodemodeWasmModule, CodemodeWasmError>>>;

/// 进程级缓存表（对应 TS 的模块缓存 Map，见模块文档）。
///
/// `OnceLock` 保证全进程只有一份；锁只在查表/插槽的瞬间持有，文件读取与编译都在锁外进行，
/// 因此一条路径的慢编译不会阻塞另一条路径的缓存查找。锁中毒时 [`slot_for`] / [`evict`]
/// 会 panic（锁内只有 HashMap 操作，正常不会中毒）。
fn modules() -> &'static Mutex<HashMap<Option<PathBuf>, Slot>> {
    static MODULES: OnceLock<Mutex<HashMap<Option<PathBuf>, Slot>>> = OnceLock::new();
    MODULES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取得（或创建）某路径的缓存槽位（TS: `modules.get` / `modules.set`）。
fn slot_for(key: Option<&Path>) -> Slot {
    let mut map = modules().lock().expect("wasm module cache poisoned");
    map.entry(key.map(Path::to_path_buf)).or_default().clone()
}

/// 摘掉失败的槽位，让下一次调用重试（TS: `modules.delete(resolved)`）。
///
/// 只删除**同一个**槽位：另一个调用者可能已经因为更早的失败摘过槽并建了新槽（甚至已经
/// 编译成功），不能把它的槽位删掉。
fn evict(key: Option<&Path>, slot: &Slot) {
    let mut map = modules().lock().expect("wasm module cache poisoned");
    let key = key.map(Path::to_path_buf);
    if map
        .get(&key)
        .is_some_and(|current| Arc::ptr_eq(current, slot))
    {
        map.remove(&key);
    }
}

/// `loadQuickJSWasm(path?)`：读取并编译 QuickJS 的 wasm，每个路径只做一次。
///
/// `path == None` 时用嵌入的 `js/quickjs.wasm`（TS 的默认是已安装 `quickjs-wasi` 包里的文件）；
/// `Some(path)` 时读该文件。失败的加载不缓存，下次调用重试。
///
/// 是 `async` 只为对齐 TS 的 `Promise` 返回（编译本身在调用线程同步完成，两份 Engine 的编译
/// 并行进行——见 `compile`；spec §8.2 第 8 条的“不重入 VM”约束同样满足：编译发生在任何 VM
/// 线程创建之前）。
///
/// # 错误
///
/// 失败都包成 [`CodemodeWasmError`]，`Display` 是完整的 Node 风格原因文本：
///
/// - 路径含 NUL：Node 的 `ERR_INVALID_ARG_VALUE` 文本，发生在建槽之前，不占缓存；
/// - 读取失败：`read_error` 生成的 `CODE: <libuv 文案>, open '<path>'`（目录是 `read`）；
/// - 编译失败：`compile_error` 压成单行的 `WebAssembly.compile` 对应文本；
/// - feature `wasm` 关闭：固定的“后端已禁用”文本，由 launcher 加上同一个前缀。
///
/// 失败不写入缓存；并发调用者共享同一次失败，失败后槽位被摘掉，下一次调用重试。
///
/// # 阻塞
///
/// 读取与 default 档编译在**调用方线程**同步执行，inline 档在 `std::thread::scope` 子线程
/// 并行后 `join`；没有 `spawn_blocking`。在 tokio 运行时线程上 await 会占住该执行器线程
/// （debug 构建的一次编译可能十几秒），需要响应性的场景应自行把它放到阻塞线程池。
pub async fn load_quickjs_wasm(
    path: Option<&Path>,
) -> Result<CodemodeWasmModule, CodemodeWasmError> {
    // Node 的 `fs` 在读文件之前就拒绝含 NUL 的路径（`validatePath` → `ERR_INVALID_ARG_VALUE`），
    // 所以这条错误不经过缓存，也不经过 libuv 的 EINVAL 分支。
    if let Some(message) = path.and_then(nul_path_error) {
        return Err(CodemodeWasmError::new(message));
    }
    // 与 TS 相同：先取/建槽位，再让首个调用者完成读取与编译，并发调用者共享同一个槽位。
    let slot = slot_for(path);
    let outcome = slot
        .get_or_init(|| async move { compile(path) })
        .await
        .clone();
    if outcome.is_err() {
        // TS 的 `.catch`：失败的 Promise 从 Map 里删掉，下一次调用重试。共享同一次失败的
        // 并发调用者都会走到这里，`evict` 的 ptr_eq 保证只摘掉这一个槽位——成功路径从不
        // 摘槽，所以一次失败的调用不会误删并发调用刚存下的成功模块。
        evict(path, &slot);
    }
    outcome
}

/// 同步加载+编译（[`load_quickjs_wasm`] 的实现体，测试与诊断可直接用；**不经过缓存**）。
///
/// 与 [`load_quickjs_wasm`] 做同样的 NUL 校验，但每次调用都重新读取并编译两份 Engine 的
/// 结果：不读写全局缓存表，也不影响正在进行的并发加载。错误类型与 [`load_quickjs_wasm`]
/// 相同；`None` 时同样使用嵌入字节。因为每次都会付出完整编译开销，不要在执行热路径上
/// 用它代替缓存入口。
pub fn compile_quickjs_wasm(path: Option<&Path>) -> Result<CodemodeWasmModule, CodemodeWasmError> {
    if let Some(message) = path.and_then(nul_path_error) {
        return Err(CodemodeWasmError::new(message));
    }
    compile(path)
}

/// 无缓存的加载 + 编译：读取字节，再在两档 Engine 上各编译一份。
///
/// 调用方（[`load_quickjs_wasm`] / [`compile_quickjs_wasm`]）已完成 NUL 校验，这里不再检查。
/// `Some(path)` 用 `std::fs::read` 同步读取（目录会在读取阶段报 `EISDIR`，见
/// `describe_read_error`），`None` 直接用嵌入字节。
///
/// 两份编译并行：inline 档放进 `std::thread::scope` 的子线程，default 档在当前线程编译，
/// 然后 `join`。两份都完成后才按 default → inline 的顺序检查结果，任一份失败则返回该错误
/// （另一份的编译结果被丢弃）；因此失败调用也付出了完整的两份编译成本。
///
/// `cfg(test)` 下 `test_hooks::before_compile` 在读取之前记录路径并可注入延迟，用于验证
/// 并发调用只编译一次；发布构建没有这个钩子。
#[cfg(feature = "wasm")]
fn compile(path: Option<&Path>) -> Result<CodemodeWasmModule, CodemodeWasmError> {
    #[cfg(test)]
    test_hooks::before_compile(path);
    let bytes: Vec<u8> = match path {
        Some(path) => {
            std::fs::read(path).map_err(|error| CodemodeWasmError::new(read_error(&error, path)))?
        }
        None => QUICKJS_WASM_BYTES.to_vec(),
    };
    // 两档 Engine 各编译一份。并行编译：debug 构建里一次编译要十几秒，串行会让
    // `load_quickjs_wasm` 的时间翻倍（TS 只做一次 V8 编译；这是 Rust 侧为两档
    // `max_wasm_stack` 必须付的代价，但不能在**执行期间**付）。
    let (default, inline) = std::thread::scope(|scope| {
        let inline = scope.spawn(|| compile_bytes(inline_engine(), &bytes));
        let default = compile_bytes(shared_engine(), &bytes);
        (
            default,
            inline.join().expect("the inline compile thread panicked"),
        )
    });
    Ok(CodemodeWasmModule::new(default?, inline?))
}

/// 用指定 Engine 编译一份模块（`WebAssembly.compile(bytes)`）。
///
/// 两档栈预算各有一份编译结果（见 [`CodemodeWasmModule`]）；失败的错误文本与
/// `WebAssembly.compile` 的 `CompileError.message` 对齐（[`compile_error`]）。
#[cfg(feature = "wasm")]
fn compile_bytes(
    engine: &wasmtime::Engine,
    bytes: &[u8],
) -> Result<Arc<wasmtime::Module>, CodemodeWasmError> {
    wasmtime::Module::new(engine, bytes)
        .map(Arc::new)
        .map_err(|error| CodemodeWasmError::new(compile_error(&error)))
}

/// feature `wasm` 关闭时的替身：不读文件也不编译，永远返回“后端已禁用”。
///
/// 文本有意不含 `Failed to load QuickJS: ` 前缀——由调用方（launcher 的加载路径）拼上，
/// 与 TS 在加载失败时的可观察行为一致。`_path` 完全不被使用：NUL 校验已由两个公开入口
/// （[`load_quickjs_wasm`] / [`compile_quickjs_wasm`]）完成。
#[cfg(not(feature = "wasm"))]
fn compile(_path: Option<&Path>) -> Result<CodemodeWasmModule, CodemodeWasmError> {
    Err(CodemodeWasmError::new(
        "the wasm backend is disabled (enable the \"wasm\" feature)",
    ))
}

/// `WebAssembly.compile` 拒绝值的 `message`，逐条保留原因。
///
/// TS 抛的是 V8 的 `CompileError`，`message` 一行，形如
/// `WebAssembly.compile(): expected magic word 00 61 73 6d, found 6e 6f 74 20 @+0`。Rust 侧
/// 引擎是 wasmtime，文案不可能相同（ARCHITECTURE.md §8 的诚实清单允许引擎措辞不同），但
/// **必须**保留“到底哪里坏了”：wasmtime 的 `Display` 只有顶层上下文
/// （`failed to parse WebAssembly module`），原因在 `source()` 链里，`{:#}` 把它拼成
/// `上下文: 原因`。TS 的 message 是单行，所以把换行与连续空白压成单个空格。
#[cfg(feature = "wasm")]
fn compile_error(error: &wasmtime::Error) -> String {
    let detail = format!("{error:#}");
    let mut collapsed = String::with_capacity(detail.len());
    let mut pending_space = false;
    for ch in detail.chars() {
        if ch.is_whitespace() {
            pending_space = !collapsed.is_empty();
            continue;
        }
        if pending_space {
            collapsed.push(' ');
            pending_space = false;
        }
        collapsed.push(ch);
    }
    collapsed
}

/// 含 NUL 字节的路径：Node 的 `fs` 在打开之前抛的 `ERR_INVALID_ARG_VALUE` 消息。
///
/// TS 的 `loadQuickJSWasm(path)` 把 `path` 直接交给 `readFile`，后者先过 `validatePath`
/// （`internal/fs/utils`）：字符串含 `\u0000` 就抛
/// `ERR_INVALID_ARG_VALUE('path', path, 'must be a string, Uint8Array, or URL without null bytes')`。
/// 该错误的模板（`internal/errors`）是
/// `` `The ${type} '${name}' ${reason}. Received ${inspected}` ``，其中 `inspected` 是
/// `util.inspect(value)`，且 **超过 128 个 UTF-16 码元时截断成 128 个再加 `...`**。
/// `errorMessage(error)` 取 `error.message`，所以 Rust 侧必须给出这一整行——libuv 的
/// `EINVAL: invalid argument, open '…'` 是 Node 根本不会走到的分支。
///
/// `util.inspect` 的字符串规则（`internal/util/inspect` 的 `strEscape`）：
/// - 引号选择：无 `'` → `'…'`；有 `'` 无 `"` → `"…"`；有 `'` 与 `"` 且无 `` ` `` 且无 `${`
///   → `` `…` ``；否则用 `'…'` 并转义 `'`；
/// - 转义：控制字符与 `0x7F..=0x9F` 按 `meta` 表（`\x00`…`\x1F` 里 `\b \t \n \f \r` 用短形式，
///   其余 `\xNN` 大写十六进制）、`\` → `\\`、被选中的引号 → `\<quote>`；
/// - 其余字符（含非 ASCII）原样输出。
///
/// Rust 的 `String` 装不下孤立代理项，因此含 NUL 的非 UTF-8 路径（Linux 上合法）无法完全
/// 复刻 Node 的文本：这类路径按 `to_string_lossy` 呈现（记为 `ARCHITECTURE.md` §8 第 31 条
/// 同一族的被迫偏差）。全部测试路径都是合法 UTF-8。
fn nul_path_error(path: &Path) -> Option<String> {
    // `OsStr` 的字节在 Unix 上是原始字节；含 NUL 就与 Node 一样报错。
    let bytes = os_str_bytes(path.as_os_str());
    if !bytes.contains(&0) {
        return None;
    }
    let text = path.to_string_lossy();
    Some(format!(
        "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received {}",
        node_inspect_string(&text)
    ))
}

/// `OsStr` 的原始字节（Unix 直接借，其他平台退化为 `to_string_lossy` 的字节）。
fn os_str_bytes(value: &std::ffi::OsStr) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(value.as_bytes())
    }
    #[cfg(not(unix))]
    {
        match value.to_str() {
            Some(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
            None => std::borrow::Cow::Owned(value.to_string_lossy().into_owned().into_bytes()),
        }
    }
}

/// `ERR_INVALID_ARG_VALUE` 模板里的 `inspected`：`util.inspect(value)`，且长于 128 个
/// **UTF-16 码元**时截断到 128 再加 `...`（`internal/errors` 的 `E('ERR_INVALID_ARG_VALUE')`）。
///
/// 只实现字符串分支：模板里的 `value` 在这里永远是 `path`（字符串）。
fn node_inspect_string(text: &str) -> String {
    const MAX: usize = 128;
    // 先转义再截断？Node 的顺序是：先 `inspect(value)`，再对**结果**做长度检查与截断。
    // 仓库内的黄金值可以区分两种顺序：tests/fixtures/nul_path_cases.json 的 boundary_63、
    // boundary_64、long_ascii 三条只有“先转义再截断”才能匹配（转义后的长度是 132/134/406
    // 个 UTF-16 码元，而原始路径更短）。
    let inspected = node_str_escape(text);
    let units: Vec<u16> = inspected.encode_utf16().collect();
    if units.len() > MAX {
        let head = String::from_utf16_lossy(&units[..MAX]);
        return format!("{head}...");
    }
    inspected
}

/// `internal/util/inspect` 的 `strEscape`（无颜色）。
///
/// 引号选择的直接后果：被选中的引号字符只在选 `'` 时可能出现在文本里（有 `'` 才可能选
/// `"` / `` ` ``，而选后两者时文本里必然没有该字符）。因此转义分支只需为 `'`（0x27）与
/// 反斜杠单独处理，其余进入该分支的字符都是控制字符或 0x7F..=0x9F。
fn node_str_escape(text: &str) -> String {
    // 引号选择（见函数文档）。
    let quote = if text.contains('\'') {
        if !text.contains('"') {
            '"'
        } else if !text.contains('`') && !text.contains("${") {
            '`'
        } else {
            '\''
        }
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        let code = ch as u32;
        if ch == quote || ch == '\\' || code < 0x20 || (0x7e < code && code < 0xa0) {
            match code {
                0x08 => out.push_str("\\b"),
                0x09 => out.push_str("\\t"),
                0x0a => out.push_str("\\n"),
                0x0c => out.push_str("\\f"),
                0x0d => out.push_str("\\r"),
                0x27 => out.push_str("\\'"),
                0x5c => out.push_str("\\\\"),
                _ => out.push_str(&format!("\\x{code:02X}")),
            }
        } else {
            out.push(ch);
        }
    }
    out.push(quote);
    out
}

/// Node 的 `readFile` 错误消息：`CODE: <libuv 文案>, <syscall> '<path>'`。
///
/// TS 侧 `errorMessage(error)` 取 Node `Error` 的 `message`，即 `UVException` 拼出的这一行
/// （`internal/errors.js`：`` `${code}: ${uvmsg}, ${syscall}` ``，有 `path` 时追加
/// ` '${path}'`）。`code`/`uvmsg` 来自 libuv 的 errno 表（`uv_err_name`/`uv_strerror`），
/// 未收录的 errno 退回 `UNKNOWN: unknown error`。文案表与 `kiana-env::errors` 相同（本 crate
/// 零 kiana 依赖，所以本地保留一份）。
#[cfg(feature = "wasm")]
fn read_error(error: &std::io::Error, path: &Path) -> String {
    let (code, description, syscall) = describe_read_error(error);
    format!("{code}: {description}, {syscall} '{}'", path.display())
}

/// `readFile` 先 `open` 再 `read`：只有目录是在 `read` 上失败（Node 报 `EISDIR … , read`），
/// 其余错误都发生在 `open` 上。
#[cfg(all(feature = "wasm", unix))]
fn describe_read_error(error: &std::io::Error) -> (&'static str, &'static str, &'static str) {
    let Some(errno) = error.raw_os_error() else {
        return describe_error_kind(error.kind());
    };
    let (code, description) = match errno {
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::ENOENT => ("ENOENT", "no such file or directory"),
        libc::EIO => ("EIO", "i/o error"),
        libc::ENXIO => ("ENXIO", "no such device or address"),
        libc::EBADF => ("EBADF", "bad file descriptor"),
        libc::EAGAIN => ("EAGAIN", "resource temporarily unavailable"),
        libc::ENOMEM => ("ENOMEM", "not enough memory"),
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EFAULT => ("EFAULT", "bad address in system call argument"),
        libc::EBUSY => ("EBUSY", "resource busy or locked"),
        libc::EEXIST => ("EEXIST", "file already exists"),
        libc::EXDEV => ("EXDEV", "cross-device link not permitted"),
        libc::ENODEV => ("ENODEV", "no such device"),
        libc::ENOTDIR => ("ENOTDIR", "not a directory"),
        libc::EISDIR => ("EISDIR", "illegal operation on a directory"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::ENOTTY => ("ENOTTY", "inappropriate ioctl for device"),
        libc::ETXTBSY => ("ETXTBSY", "text file is busy"),
        libc::EFBIG => ("EFBIG", "file too large"),
        libc::ENOSPC => ("ENOSPC", "no space left on device"),
        libc::ESPIPE => ("ESPIPE", "invalid seek"),
        libc::EROFS => ("EROFS", "read-only file system"),
        libc::EMLINK => ("EMLINK", "too many links"),
        libc::EPIPE => ("EPIPE", "broken pipe"),
        libc::ERANGE => ("ERANGE", "result too large"),
        libc::ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        libc::ENOSYS => ("ENOSYS", "function not implemented"),
        libc::ENOTEMPTY => ("ENOTEMPTY", "directory not empty"),
        libc::ELOOP => ("ELOOP", "too many symbolic links encountered"),
        libc::EOVERFLOW => ("EOVERFLOW", "value too large for defined data type"),
        libc::EILSEQ => ("EILSEQ", "illegal byte sequence"),
        // libuv 不认识的 errno：`uvErrmapGet(errno) || uvUnmappedError`。
        _ => ("UNKNOWN", "unknown error"),
    };
    let syscall = if code == "EISDIR" { "read" } else { "open" };
    (code, description, syscall)
}

/// 非 unix 平台：按 [`std::io::ErrorKind`] 退回（Windows 的 libuv 表在 `kiana-env::sys::windows`，
/// 但本 crate 不依赖它；这里覆盖 `readFile` 会遇到的常见种类）。
#[cfg(all(feature = "wasm", not(unix)))]
fn describe_read_error(error: &std::io::Error) -> (&'static str, &'static str, &'static str) {
    describe_error_kind(error.kind())
}

/// 没有 errno 可用时的映射（与上面的 libuv 文案一致）。
#[cfg(feature = "wasm")]
fn describe_error_kind(kind: std::io::ErrorKind) -> (&'static str, &'static str, &'static str) {
    use std::io::ErrorKind;
    let (code, description, syscall) = match kind {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory", "open"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied", "open"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory", "read"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory", "open"),
        ErrorKind::InvalidFilename => ("ENAMETOOLONG", "name too long", "open"),
        ErrorKind::InvalidInput => ("EINVAL", "invalid argument", "open"),
        ErrorKind::OutOfMemory => ("ENOMEM", "not enough memory", "open"),
        _ => ("UNKNOWN", "unknown error", "open"),
    };
    (code, description, syscall)
}

/// 模块编译共享的 `wasmtime::Engine`（`Module` 与 `Engine` 必须属于同一引擎才能
/// 实例化；`load_quickjs_wasm` 与 `runtime::vm` 必须用同一个）。
///
/// `Engine` 是 `Send + Sync` 且创建开销小但非零；`wasmtime::Module::new` 需要它。
///
/// # 为什么抬高 `max_wasm_stack`
///
/// QuickJS 对**自己的 JS 栈**有个守卫（`qjs_set_max_stack_size`，默认 512 KiB，见
/// [`crate::runtime::vm::MAX_STACK_SIZE`]），普通的 JS 递归因此是一个可 catch 的
/// `RangeError`；但 QuickJS 的 C 递归（`JSON.parse`/`JSON.stringify`、解析器）同样消耗
/// wasm 帧，而这些帧用的是宿主的原生栈。wasmtime 自己的栈上限（默认 512 KiB）远小于
/// QuickJS 守卫换算出的 wasm 栈需求，深递归会先撞上 wasmtime 的 trap——TS 侧（V8 的 4 MB
/// worker 栈）约 2 万层才 trap，2 MiB 的旧配置在 9000 层就误报 `crash`。
///
/// 实测（quickjs 0.17.0 的 `quickjs.wasm`，Node worker 默认 4 MB 栈）：
/// - `JSON.stringify` 21000/22000 层成功、23000 层 trap；嵌套数组字面量 2600 层成功、
///   2800 层 trap。TS worker 实测同一组阈值。
/// - wasmtime 要复现同一组阈值：给 `crate::runtime::vm::WASM_STACK_SIZE`（5.65 MB）后
///   Rust worker 后端的成功/崩溃边界与 TS 逐点对齐（见 `crates/kiana-codemode/tests/vm.rs`）。
///
/// 因此这里给 `crate::runtime::vm::WASM_STACK_SIZE`，并且 **VM 跑在 `runtime::vm` 自己的
/// 16 MiB 线程上**（`VM_THREAD_STACK_SIZE`）：`max_wasm_stack` 只是 wasmtime 的「允许用量」，
/// 真正耗尽原生栈仍是 SIGSEGV（进程 abort），所以原生线程栈必须显著大于它。
/// `async_stack_size` 保持在 `max_wasm_stack` 之上（8 MiB），宿主函数仍有
/// `async_stack_size - max_wasm_stack` 的余量。
///
/// # 构造失败时的回退
///
/// 如果 `Engine::new` 拒绝了这份 `Config`，这里静默退回 `wasmtime::Engine::default()`：
/// 加载本身不会报错，但 `max_wasm_stack` 回到 wasmtime 的默认值，深递归的成功/崩溃边界
/// 不再与 TS 对齐。`OnceLock` 只初始化一次，一旦回退，之后所有调用都复用这个默认 Engine。
#[cfg(feature = "wasm")]
pub fn shared_engine() -> &'static wasmtime::Engine {
    static ENGINE: OnceLock<wasmtime::Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = wasmtime::Config::new();
        config.max_wasm_stack(crate::runtime::vm::WASM_STACK_SIZE);
        config.async_stack_size(8 * 1024 * 1024);
        wasmtime::Engine::new(&config).unwrap_or_else(|_| wasmtime::Engine::default())
    })
}

/// inline/remote 后端用的 `Engine`：wasm 栈预算小到足以复现 TS 在调用线程（Node 主线程约
/// 1 MB 原生栈）上跑 VM 的边界（见 [`crate::runtime::vm::INLINE_WASM_STACK_SIZE`]）。
///
/// 与 [`shared_engine`] 是两个 Engine，所以模块要各编译一份（[`CodemodeWasmModule`]）。
/// `max_wasm_stack` 必须 ≤ `async_stack_size`（wasmtime 的校验），8 MiB 的 async 栈对
/// 1.1 MB 的 wasm 预算绰绰有余。
///
/// 与 [`shared_engine`] 一样，`Engine::new` 失败会静默退回 `Engine::default()`（不再复现
/// TS 主线程的栈边界），且回退结果被 `OnceLock` 固定下来。
#[cfg(feature = "wasm")]
pub(crate) fn inline_engine() -> &'static wasmtime::Engine {
    static ENGINE: OnceLock<wasmtime::Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = wasmtime::Config::new();
        config.max_wasm_stack(crate::runtime::vm::INLINE_WASM_STACK_SIZE);
        config.async_stack_size(8 * 1024 * 1024);
        wasmtime::Engine::new(&config).unwrap_or_else(|_| wasmtime::Engine::default())
    })
}

/// 嵌入模块的字节数（校验用：637405）。
///
/// 与 [`EMBEDDED_WASM_SHA256`] 一起由单元测试 `embedded_bytes_match_the_pinned_fingerprint`
/// 钉住：替换 `js/quickjs.wasm` 时必须在同一提交里同步更新这两个常量，否则测试失败
/// （测试只在 feature `wasm` 下做长度与摘要校验；关闭时只检查字节数组为空）。
pub const EMBEDDED_WASM_LEN: usize = 637_405;

/// 嵌入模块的 SHA-256（小写十六进制，校验用）。
///
/// 长度固定 64 个 ASCII 字符；`js/quickjs.wasm` 与 TS 侧的 `quickjs-wasi@3.6.2` 逐字节相同
/// 时，这个摘要就是两边共用的指纹。
pub const EMBEDDED_WASM_SHA256: &str =
    "d4c9375f2b1ca4dc95f72c8aa2982a7a9951ac8011490d79c6582df732b4bbd9";

/// 测试钩子：记录每次 `compile()` 的路径，并可注入人为延迟来制造并发重叠。
///
/// 只用于验证「并发调用同一路径只编译一次」。`cfg(test)` 下才存在，不影响发布面。
#[cfg(all(test, feature = "wasm"))]
mod test_hooks {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    /// 每次 `compile()` 的路径（嵌入模块记为 `<embedded>`）。
    fn compiles() -> &'static Mutex<Vec<String>> {
        static COMPILES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
        COMPILES.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// `compile()` 内的人为延迟（毫秒）。
    pub static DELAY_MS: AtomicU64 = AtomicU64::new(0);

    /// `compile()` 开头调用：记录路径；按 [`DELAY_MS`] 阻塞（同步睡眠，模拟慢编译）。
    pub fn before_compile(path: Option<&Path>) {
        let label = match path {
            Some(path) => path.to_string_lossy().into_owned(),
            None => "<embedded>".to_string(),
        };
        compiles().lock().expect("compile log poisoned").push(label);
        let delay = DELAY_MS.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
    }

    /// 某路径被编译了几次。
    pub fn count_for(path: &Path) -> usize {
        let label = path.to_string_lossy();
        compiles()
            .lock()
            .expect("compile log poisoned")
            .iter()
            .filter(|entry| **entry == *label)
            .count()
    }
}

// 本文件的单元测试锁定四类契约：
// 1. 嵌入的 quickjs.wasm 与固定指纹（637405 字节 + SHA-256）一致；
// 2. 读取失败文本与 Node 的 `readFile` 逐字对齐；编译失败文本压成单行并保留 wasmtime 的
//    根因（引擎措辞与 V8 的 `CompileError` 不同）。NUL 路径的黄金值来自
//    tests/fixtures/nul_path_cases.json，由 Node 26 实测生成；
// 3. 缓存语义对齐 Promise 缓存：并发成功只编译一次、并发失败共享同一次尝试、
//    失败后重试，且迟到的失败不会摘掉并发调用刚存好的成功槽位；
// 4. 非 UTF-8 路径按原始字节分槽（`to_string_lossy` 相同的两个路径不共享模块）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bytes_match_the_pinned_fingerprint() {
        #[cfg(feature = "wasm")]
        {
            use sha2::{Digest, Sha256};
            assert_eq!(QUICKJS_WASM_BYTES.len(), EMBEDDED_WASM_LEN);
            let digest = Sha256::digest(QUICKJS_WASM_BYTES);
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, EMBEDDED_WASM_SHA256);
        }
        #[cfg(not(feature = "wasm"))]
        {
            assert!(QUICKJS_WASM_BYTES.is_empty());
        }
    }

    #[cfg(feature = "wasm")]
    #[test]
    fn embedded_module_compiles() {
        let module = compile(None).expect("embedded quickjs.wasm compiles");
        // 两档栈预算各有一份编译结果；inline 档共享同一份句柄，不重新编译。
        let inline = module.with_inline_stack();
        assert!(Arc::ptr_eq(&module.module(), &module.module()));
        assert!(!Arc::ptr_eq(&module.module(), &inline.module()));
    }

    #[cfg(feature = "wasm")]
    #[test]
    fn missing_path_reports_an_error_and_is_not_cached() {
        let path = Path::new("/nonexistent/kiana/quickjs.wasm");
        let first = compile_quickjs_wasm(Some(path));
        assert!(first.is_err());
        // 第二次仍然尝试（不是缓存的错误）。
        assert!(compile_quickjs_wasm(Some(path)).is_err());
        assert_eq!(
            first.err().unwrap().message,
            "ENOENT: no such file or directory, open '/nonexistent/kiana/quickjs.wasm'"
        );
    }

    #[cfg(feature = "wasm")]
    #[test]
    fn invalid_bytes_report_a_parse_error_with_the_cause() {
        let dir = std::env::temp_dir().join(format!("kiana-codemode-wasm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.wasm");
        std::fs::write(&path, b"not wasm").unwrap();
        let error = compile_quickjs_wasm(Some(&path)).expect_err("bad bytes must fail");
        assert!(
            !error.message.contains('\n'),
            "message must be one line: {error}"
        );
        // TS 的 V8 文本是 `WebAssembly.compile(): expected magic word …`；引擎措辞不同，
        // 但原因（魔数不匹配）必须保留，不能只剩 `failed to parse WebAssembly module`。
        assert!(
            error.message.contains("failed to parse WebAssembly module"),
            "unexpected message: {error}"
        );
        assert!(
            error.message.contains("magic header not detected"),
            "the compile cause must survive: {error}"
        );
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    // 与 Node 的 `readFile` 逐字对齐（golden 值由 node 26 实测：见测试里的路径构造）。
    #[cfg(all(feature = "wasm", unix))]
    #[test]
    fn read_errors_match_node_text() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir =
            std::env::temp_dir().join(format!("kiana-codemode-readerr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), b"x").unwrap();

        // ENOTDIR：路径的父组件是普通文件（Node: `ENOTDIR: not a directory, open '<path>'`；
        // 实测 `readFile("/etc/passwd/quickjs.wasm")` 给出逐字相同的这一行）。
        let not_dir = dir.join("file/quickjs.wasm");
        let error = compile_quickjs_wasm(Some(&not_dir)).expect_err("ENOTDIR");
        assert_eq!(
            error.message,
            format!("ENOTDIR: not a directory, open '{}'", not_dir.display())
        );

        // EISDIR：路径是目录；Node 是 open 成功、read 失败，所以 syscall 是 `read`。
        let error = compile_quickjs_wasm(Some(&dir)).expect_err("EISDIR");
        assert_eq!(
            error.message,
            format!(
                "EISDIR: illegal operation on a directory, read '{}'",
                dir.display()
            )
        );

        // ENAMETOOLONG。
        let long = dir.join("a".repeat(300));
        let error = compile_quickjs_wasm(Some(&long)).expect_err("ENAMETOOLONG");
        assert_eq!(
            error.message,
            format!("ENAMETOOLONG: name too long, open '{}'", long.display())
        );

        // ELOOP（符号链接环）。
        let loop_a = dir.join("loop-a");
        let loop_b = dir.join("loop-b");
        symlink(&loop_b, &loop_a).unwrap();
        symlink(&loop_a, &loop_b).unwrap();
        let error = compile_quickjs_wasm(Some(&loop_a)).expect_err("ELOOP");
        assert_eq!(
            error.message,
            format!(
                "ELOOP: too many symbolic links encountered, open '{}'",
                loop_a.display()
            )
        );

        // ENOENT。
        let missing = dir.join("missing.wasm");
        let error = compile_quickjs_wasm(Some(&missing)).expect_err("ENOENT");
        assert_eq!(
            error.message,
            format!(
                "ENOENT: no such file or directory, open '{}'",
                missing.display()
            )
        );

        // EACCES（root 会绕过权限位，跳过）。
        if unsafe { libc::geteuid() } != 0 {
            let locked = dir.join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::write(locked.join("x.wasm"), b"x").unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let denied = locked.join("x.wasm");
            let error = compile_quickjs_wasm(Some(&denied)).expect_err("EACCES");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                error.message,
                format!("EACCES: permission denied, open '{}'", denied.display())
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // 含 NUL 的路径：Node 在 open 之前就抛 ERR_INVALID_ARG_VALUE，文案是
    // `The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received <inspect>`。
    // 黄金值由 Node 26 的 `fs.promises.readFile` 实测生成（fixture 见 tests/fixtures/nul_path_cases.json）。
    #[cfg(all(feature = "wasm", unix))]
    #[test]
    fn nul_paths_report_the_node_validation_error() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/nul_path_cases.json"))
                .expect("fixture parses");
        let cases = fixture.as_array().expect("fixture is an array");
        assert!(cases.len() >= 15);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let path = Path::new(OsStr::from_bytes(case["path"].as_str().unwrap().as_bytes()));
            let expected = case["message"].as_str().unwrap();
            let error = compile_quickjs_wasm(Some(path)).expect_err(name);
            assert_eq!(error.message, expected, "case {name}");

            // `load_quickjs_wasm` 同样在读之前拒绝（不占缓存槽位）。
            let error = poll_once(load_quickjs_wasm(Some(path))).expect_err(name);
            assert_eq!(error.message, expected, "case {name} (cached path)");
        }
    }

    /// 最小的单次 poll：`load_quickjs_wasm` 的 NUL 分支在第一个 `await` 之前返回，
    /// 因此不需要 tokio 运行时（测试线程里构造 runtime 会与 `#[tokio::test]` 冲突）。
    #[cfg(all(feature = "wasm", unix))]
    fn poll_once<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("nul-path validation must not await"),
        }
    }

    // 两个只在非法字节上不同的非 UTF-8 路径必须各占一个缓存槽位：`to_string_lossy` 会把它们
    // 都渲染成 U+FFFD（TS 的 `Map<string, …>` 在 Node 里永远拿到可区分的字符串）。
    #[cfg(all(feature = "wasm", unix))]
    #[test]
    fn non_utf8_paths_get_distinct_cache_slots() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir =
            std::env::temp_dir().join(format!("kiana-codemode-nonutf8-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 两个文件名只在非法 UTF-8 字节上不同，`to_string_lossy` 都是 "bad<U+FFFD>.wasm"。
        let first = dir.join(OsStr::from_bytes(b"bad\xff.wasm"));
        let second = dir.join(OsStr::from_bytes(b"bad\xfe.wasm"));
        assert_eq!(
            first.to_string_lossy(),
            second.to_string_lossy(),
            "the lossy forms must collide, or this test proves nothing"
        );
        std::fs::write(&first, b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(&second, b"\0asm\x01\0\0\0").unwrap();

        let module = compile(None).expect("embedded quickjs.wasm compiles");
        let first_slot = slot_for(Some(&first));
        let _ = first_slot.set(Ok(module));
        let second_slot = slot_for(Some(&second));
        assert!(
            !Arc::ptr_eq(&first_slot, &second_slot),
            "distinct paths must not share a slot"
        );
        assert!(second_slot.get().is_none());

        evict(Some(&first), &first_slot);
        evict(Some(&second), &second_slot);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 等到某路径的第 `want` 次 `compile()` 已经开始（钩子在 sleep 之前记录路径）。
    #[cfg(feature = "wasm")]
    async fn wait_until_compiling(path: &Path, want: usize) {
        for _ in 0..2000 {
            if test_hooks::count_for(path) >= want {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("compile did not start: {:?}", path);
    }

    // 并发调用同一路径只编译一次（TS 缓存的是 Promise，并发调用共享同一次读取+编译）；
    // 失败时并发调用者共享同一次尝试，失败后槽位被摘掉，下一次调用重新编译。
    //
    // 重叠是**确定**制造的：先放一个调用者进去，等它进入 `compile()`（钩子里的 300 ms
    // 延迟保证窗口很宽），再放其余调用者。用 barrier 会依赖任务调度时机。
    #[cfg(feature = "wasm")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cache_is_promise_equivalent_for_success_and_failure() {
        use std::sync::atomic::Ordering;

        let dir = std::env::temp_dir().join(format!("kiana-codemode-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        test_hooks::DELAY_MS.store(300, Ordering::SeqCst);

        // --- 成功：4 个并发调用者只编译一次 ---
        let path = dir.join("tiny.wasm");
        // 合法的最小 module（8 字节头），编译很快；钩子的延迟制造并发重叠。
        std::fs::write(&path, b"\0asm\x01\0\0\0").unwrap();

        let first = {
            let path = path.clone();
            tokio::spawn(async move { load_quickjs_wasm(Some(&path)).await })
        };
        wait_until_compiling(&path, 1).await;
        let mut tasks = vec![first];
        for _ in 0..3 {
            let path = path.clone();
            tasks.push(tokio::spawn(
                async move { load_quickjs_wasm(Some(&path)).await },
            ));
        }
        for task in tasks {
            assert!(task.await.expect("task").is_ok());
        }
        assert_eq!(
            test_hooks::count_for(&path),
            1,
            "concurrent success shares one compile"
        );

        // 再调用一次：命中缓存，不再编译。
        assert!(load_quickjs_wasm(Some(&path)).await.is_ok());
        assert_eq!(test_hooks::count_for(&path), 1);

        // --- 失败：并发调用者共享同一次尝试，且失败不缓存 ---
        let missing = dir.join("missing-concurrent.wasm");
        let first = {
            let missing = missing.clone();
            tokio::spawn(async move { load_quickjs_wasm(Some(&missing)).await })
        };
        wait_until_compiling(&missing, 1).await;
        let mut tasks = vec![first];
        for _ in 0..2 {
            let missing = missing.clone();
            tasks.push(tokio::spawn(async move {
                load_quickjs_wasm(Some(&missing)).await
            }));
        }
        for task in tasks {
            let error = task.await.expect("task").expect_err("missing file");
            assert_eq!(
                error.message,
                format!(
                    "ENOENT: no such file or directory, open '{}'",
                    missing.display()
                )
            );
        }
        assert_eq!(
            test_hooks::count_for(&missing),
            1,
            "one attempt for three callers"
        );

        // 失败已从缓存摘除：下一次调用重新尝试（TS 的 `modules.delete`）。
        assert!(load_quickjs_wasm(Some(&missing)).await.is_err());
        assert_eq!(test_hooks::count_for(&missing), 2);

        test_hooks::DELAY_MS.store(0, Ordering::SeqCst);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 一次失败的调用不能删掉别的调用者已经存好的成功槽位（`evict` 的 ptr_eq 保护）。
    #[cfg(feature = "wasm")]
    #[test]
    fn a_failed_cell_never_evicts_a_stored_success() {
        let key = Path::new("/kiana-test/evict-race/quickjs.wasm");
        let success = slot_for(Some(key));
        let module = compile(None).expect("embedded quickjs.wasm compiles");
        success.set(Ok(module)).expect("first init wins");

        // 另一个（更早的、编译失败的）调用者拿着不同的槽位来摘槽。
        let stale: Slot = Arc::new(tokio::sync::OnceCell::new());
        evict(Some(key), &stale);
        assert!(
            slot_for(Some(key)).get().is_some(),
            "a stale failure must not evict the stored success"
        );

        // 自己的槽位才会被摘掉。
        evict(Some(key), &success);
        assert!(slot_for(Some(key)).get().is_none());
        assert!(success.get().is_some(), "existing holders keep the value");
    }
}
