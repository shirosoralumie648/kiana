//! 可选 `WebAssembly` 宿主绑定（`feature = "webassembly"`，wasmtime 后端）。
//!
//! 设计文档 §7.4 第 3 条 / §11.3：默认不提供 `WebAssembly`；启用该特性时用 wasmtime
//! 绑定 `WebAssembly.Module` / `WebAssembly.Instance` / `WebAssembly.instantiate`
//! （doom-overlay 示例的引擎）。TS 的 Node 运行时由 V8 提供同一组 API。
//!
//! # 覆盖范围
//!
//! - `new WebAssembly.Module(bytes)`、`WebAssembly.compile`、`WebAssembly.validate`、
//!   `WebAssembly.Module.exports/imports`；
//! - `new WebAssembly.Instance(module, imports)`、`WebAssembly.instantiate(moduleOrBytes, imports)`；
//! - 导出/导入函数（i32/i64/f32/f64），导出内存（`buffer` / `grow`）与全局变量；
//! - 导入的 memory/table/global 按模块声明类型分配（JS 传入的对象不共享，
//!   见下文“已知限制”）。
//!
//! # 已知限制（Emscripten 语料）
//!
//! - **内存不共享**：`Memory.buffer` 每次读取返回当前 wasm 内存的**拷贝**（QuickJS 的
//!   `ArrayBuffer` 不能指向 wasmtime 的可增长线性内存）。Emscripten 的 `HEAPU8` 因此
//!   不会写回 wasm；doom-overlay 的 `_doomgeneric_Tick` 仍会读到零页。设计文档已注明
//!   `doom-overlay` 即使有本特性“大概率仍失败”。模块**编译/实例化**本身是真实的。
//! - 导入的 `WebAssembly.Memory` 不跨 store 共享：按模块声明类型在实例 store 内新分配。
//! - 多返回值函数把结果数组转成 JS 数组；引用类型（externref/anyref/GC）不支持。
//!
//! 这些限制只影响可选的 doom 示例；`WebAssembly` 的 API 形状与错误文案对齐 Node。
//!
//! 未提供：`WebAssembly.Module.customSections`、`WebAssembly.Global` / `WebAssembly.Table`
//! 的构造器，以及 `WebAssembly.CompileError` / `LinkError` / `RuntimeError` 三个全局错误
//! 构造器（错误对象由 `__kiana_wasm.error` 现场创建）；`new WebAssembly.Memory(...)`
//! 恒抛 `LinkError`。
//!
//! # 在系统中的位置
//!
//! 本模块由 `loader/mod.rs` 以 `#[cfg(feature = "webassembly")] pub mod wasm;` 挂载，
//! 唯一调用方是 `loader::js_host::install`（在该函数末尾、Node 全局安装完成之后）。
//! 装配器 `Generation::with_config` 的第 4 步调用 `js_host::install`，因此未启用
//! `webassembly` 的构建里连这个模块都不存在，扩展看到的 `WebAssembly` 是未定义全局。
//!
//! # 两层实现：Rust 闭包 + 内嵌 JS
//!
//! wasmtime 的 `Store` / `Func` / `Memory` / `Global` 是宿主对象，不能直接变成 JS 值，
//! 因此本文件分成两层：
//!
//! 1. Rust 侧负责编译（`Module::new`）、实例化（`Instance::new`）、值转换（`Val` 与
//!    JS Number/BigInt 互转）与 store 借用；
//! 2. `HELPERS_JS` 求值出全局 `__kiana_wasm`，提供真正的 JS `class`（`Instance`、
//!    `Memory`）与桥接工厂（`memory` / `global` / `table` / `makeInstance`）。
//!    Rust 闭包不是 JS constructor，`new WebAssembly.Instance(...)` 只能由 JS class
//!    承接，构造器再回调 `__kiana_wasm.instance` 里的 Rust 实现。
//!
//! # 一次调用的执行顺序
//!
//! - **安装**：`install` → `install_in_ctx`：幂等检查 → 求值 `HELPERS_JS` → 建
//!   `WebAssembly` 对象 → 定义 `Module` 类与静态 `exports` / `imports` →
//!   挂 `validate` / `compile` / `instantiate` → 挂 JS class `Instance` / `Memory`。
//! - **编译**：`new WebAssembly.Module(bytes)` 或 `compile(bytes)`：JS 侧把 BufferSource
//!   转成字节数组 → `Module::new(ENGINE, bytes)` → 包成 `JsWasmModule`。
//! - **实例化**：`instantiate`：新建 `Store` → 按 `module.imports()` 顺序解析导入
//!   （函数登记为 `JsCallback`；memory/table/global 按声明类型新分配）→ `Instance::new`
//!   → 遍历 `module.exports()` 为每个导出生成 JS 函数/对象。
//! - **JS 调导出**：导出的 JS 函数借出 store 并同步调用 wasm。
//! - **wasm 调导入**：wasmtime 宿主闭包经线程本地 `current_engine` 重新进入 JS，
//!   调用实例化时登记的 JS 函数。
//!
//! # 线程与重入约束
//!
//! - 引擎是进程级 `static`；实例状态挂在 `Rc<InstanceData>` 上（`Store` 在 `RefCell`
//!   里），由导出的函数/内存/全局对象的 JS 闭包共享。因此实例只能在其创建线程上使用，
//!   与 `JsEngine` 的单线程模型一致。
//! - **重入**：导出的 JS 函数在整次 wasm 调用期间持有 store 的可变借用。若被调用的
//!   wasm 同步回调导入的 JS，且该 JS 再调用同一实例的导出或读 `memory.buffer`，
//!   `try_borrow(_mut)` 会失败并抛 `RuntimeError`（文案见各包装函数），而不是 panic。
//! - 每个实例有自己的 `Store`：导入的 memory/table/global 不会与 JS 传入的对象或其它
//!   实例共享状态。
//!
//! # 错误传播
//!
//! 错误由 `throw_wasm_error` 造成带 `name` 的 JS `Error`：`CompileError`（编译失败）、
//! `LinkError`（导入缺失、类型不符、签名不支持）、`RuntimeError`（wasm 执行失败、
//! 借用冲突、不支持的返回值类型）；参数类型错误另有 `TypeError`。`compile` /
//! `instantiate` 的失败是**同步抛出**的，不会变成 rejected promise——返回的 Promise
//! 只在成功路径上由 `Promise.resolve` 包装。
//!
//! # 阅读顺序
//!
//! 1. `HELPERS_JS`：JS 侧接口的形状；
//! 2. `install` / `install_in_ctx`：全局怎么装出来；
//! 3. `instantiate`：导入解析与导出包装；
//! 4. `export_func` / `export_memory` / `export_global` 与 `call_js_import`：两个方向的
//!    调用、store 借用与错误转换。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;

use rquickjs::class::Trace;
use rquickjs::function::{Opt, Rest};
use rquickjs::{ArrayBuffer, Class, Ctx, Function, JsLifetime, Object, Value};
use wasmtime::{
    Engine, Extern, ExternType, Func, FuncType, Global, Instance, Memory, Module, Ref, Store,
    Table, Val, ValType,
};

use crate::engine::{EngineResult, JsEngine, JsErrorKind, take_exception};

/// wasmtime 引擎（进程级；编译出的 `Module` 绑定到它）。
///
/// `LazyLock` 在第一次需要引擎时创建（编译、校验或建 `Store`）；之后所有 `Module` 与
/// 每个实例的 `Store` 都绑定到这一份引擎。`Engine::default()` 表示没有自定义 `Config`，
/// wasmtime 的默认设置原样生效。
static ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);

/// JS 侧辅助函数（全局 `__kiana_wasm`）。
///
/// 需要 JS 这一层的原因：Rust 闭包不能充当 JS constructor；访问器属性（`buffer`、
/// `value`）、`Promise.resolve` 与多返回值数组用 JS 表达最直接。成员：
///
/// - `bytes`：`ArrayBuffer` / `ArrayBufferView` → 普通数组（Rust 侧再收成 `Vec<u8>`）；
/// - `resolve`：`Promise.resolve`，给 `compile` / `instantiate` 提供 thenable 返回值；
/// - `memory` / `global`：用 Rust 闭包拼出导出对象；`table`：只接收长度快照（数字），
///   返回占位对象；
/// - `error`：按名字构造 `Error`（见 `throw_wasm_error`）；
/// - `Instance` / `Memory`：`WebAssembly.Instance` / `WebAssembly.Memory` 的 JS class；
/// - `instance`：`Instance` 构造器调用的 Rust 实例化入口（由 `install_in_ctx` 填充）；
/// - `makeInstance`：用 `Object.create` 造出 `instanceof Instance` 的实例（给
///   `WebAssembly.instantiate` 用，不经过构造器）。
///
/// `Memory` 的构造器总是抛 `LinkError`（独立内存不支持）；`table()` 返回占位对象：
/// `get` 恒 `undefined`、`set` 空操作、`grow` 返回创建时的长度。
const HELPERS_JS: &str = r#"
globalThis.__kiana_wasm = {
    bytes(value) {
        if (value instanceof ArrayBuffer) {
            return Array.from(new Uint8Array(value));
        }
        if (ArrayBuffer.isView(value)) {
            return Array.from(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
        }
        throw new TypeError("WebAssembly: Argument 0 must be a BufferSource");
    },
    resolve(value) {
        return Promise.resolve(value);
    },
    memory(getBuffer, grow) {
        return {
            get buffer() { return getBuffer(); },
            grow,
        };
    },
    global(getValue, setValue) {
        return {
            get value() { return getValue(); },
            set value(v) { setValue(v); },
        };
    },
    table(length) {
        return {
            length,
            get() { return undefined; },
            set() {},
            grow() { return length; },
        };
    },
    error(name, message) {
        const error = new Error(message);
        error.name = name;
        return error;
    },
    Instance: class Instance {
        constructor(module, imports) {
            this.exports = globalThis.__kiana_wasm.instance(module, imports);
        }
    },
    makeInstance(exports) {
        const instance = Object.create(globalThis.__kiana_wasm.Instance.prototype);
        instance.exports = exports;
        return instance;
    },
    Memory: class Memory {
        constructor(_descriptor) {
            const error = new Error("WebAssembly.Memory: standalone memories are not supported");
            error.name = "LinkError";
            throw error;
        }
    },
};
"#;

/// 可数值化的 wasm 值类型（引用类型不支持）。
///
/// 这是本文件内部对 `wasmtime::ValType` 的窄化：`I32` / `I64` / `F32` / `F64` 以外的
/// 类型（`V128`、`Ref(_)`）没有对应变体。导入签名在链接期、导出签名在包装期被检查，
/// 任何一侧出现不支持的类型都变成 `LinkError`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumType {
    I32,
    I64,
    F32,
    F64,
}

/// `ValType` → `NumType`；`V128` 与引用类型返回 `None`。
///
/// `None` 的语义是“这个签名不能跨 JS 边界”，由调用方在链接期或包装期转成 `LinkError`。
fn num_type(ty: &ValType) -> Option<NumType> {
    match ty {
        ValType::I32 => Some(NumType::I32),
        ValType::I64 => Some(NumType::I64),
        ValType::F32 => Some(NumType::F32),
        ValType::F64 => Some(NumType::F64),
        _ => None,
    }
}

/// 某个类型的零值，用作**导入全局**的初值。
///
/// 数值类型给 0，`V128` 给全零向量；其余类型退回 `Val::FuncRef(None)`（null 函数引用）。
fn zero_val(ty: &ValType) -> Val {
    match ty {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        ValType::V128 => Val::V128(0u128.into()),
        _ => Val::FuncRef(None),
    }
}

/// `ExternType` → `WebAssembly.Module.imports()` / `exports()` 条目里的 `kind` 字符串。
///
/// 五个取值同时用于 JS 侧结果对象的 `kind` 字段与 LinkError 文案。
fn kind_name(ty: &ExternType) -> &'static str {
    match ty {
        ExternType::Func(_) => "function",
        ExternType::Global(_) => "global",
        ExternType::Table(_) => "table",
        ExternType::Memory(_) => "memory",
        ExternType::Tag(_) => "tag",
    }
}

/// 构造一个带名字的 JS 错误并作为异常抛出（`CompileError` / `LinkError` / `RuntimeError`）。
///
/// 名字由 `__kiana_wasm.error(name, message)` 设到 `Error.name` 上，因此 JS 侧
/// `error instanceof Error` 成立、`error.name` 可区分；helper 不可用时退回
/// `Exception::throw_message`（只剩 message）。调用点还使用 `TypeError`（参数类型错误，
/// 见 `module_from_value`）。
///
/// 返回 `rquickjs::Error::Exception`（成功路径上 `ctx.throw` 已调用 `JS_Throw`）；
/// 调用方用 `?` 返回它，rquickjs 再把它变成抛给 JS 的异常。
fn throw_wasm_error<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    message: impl Into<String>,
) -> rquickjs::Error {
    let message = message.into();
    let make = ctx
        .globals()
        .get::<_, Object>("__kiana_wasm")
        .and_then(|helpers| helpers.get::<_, Function>("error"));
    let Ok(make) = make else {
        return rquickjs::Exception::throw_message(ctx, &message);
    };
    match make.call::<_, Value>((name, message)) {
        Ok(value) => ctx.throw(value),
        Err(error) => error,
    }
}

/// 把 JS 的 BufferSource 转成 wasm 字节。
///
/// 转换在 JS 里做（`__kiana_wasm.bytes`）：`ArrayBuffer` 取整体，`ArrayBufferView` 按
/// `byteOffset` / `byteLength` 取视图，其它值抛 `TypeError`。返回的 JS 数组在这里被收成
/// `Vec<u8>`；helper 抛出的异常沿返回值上抛给调用方。
fn wasm_module_bytes<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Vec<u8>> {
    let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
    let convert: Function<'js> = helpers.get("bytes")?;
    convert.call((value,))
}

/// 把值包成已完成的 Promise（`WebAssembly.compile` / `instantiate` 的返回形状）。
///
/// 只是 `Promise.resolve(value)`：编译与实例化都已在调用线程同步完成，所以返回的
/// Promise 一被 await 就得到结果。
fn resolved<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Value<'js>> {
    let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
    let resolve: Function<'js> = helpers.get("resolve")?;
    resolve.call((value,))
}

/// `WebAssembly.Module` 的 JS 类。
///
/// 每个实例只是编译产物的一个句柄：字段 `Rc<Module>` 让多个 JS 包装对象可以指向同一份
/// wasmtime 模块。`Trace` 派生负责告诉 GC 哪些字段是 JS 值，`#[qjs(skip_trace)]` 表明
/// `module` 不是（它是 Rust 侧的 `Rc`，生命周期跟着这个对象走）。
///
/// Rust 侧公开面只有构造器；`exports` / `imports` 两个静态方法是在 `install_in_ctx`
/// 里以函数属性挂到构造器上的，不是 Rust 方法。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Module")]
pub struct JsWasmModule {
    /// 编译产物（wasmtime `Module` 的共享句柄）；GC 不追踪，随本对象存活。
    #[qjs(skip_trace)]
    module: Rc<Module>,
}

#[rquickjs::methods]
impl JsWasmModule {
    /// `new WebAssembly.Module(bufferSource)`。
    ///
    /// 参数是任意 BufferSource；先经 `wasm_module_bytes` 取字节，再编译到进程级 `ENGINE`。
    /// 编译失败抛 `CompileError`，message 用 wasmtime 的 `{error:#}`（带错误链）。构造是
    /// 同步的，不返回 Promise。
    #[qjs(constructor)]
    fn new<'js>(ctx: Ctx<'js>, bytes: Value<'js>) -> EngineResult<Self> {
        let bytes = wasm_module_bytes(&ctx, bytes)?;
        let module = Module::new(&ENGINE, &bytes)
            .map_err(|error| throw_wasm_error(&ctx, "CompileError", format!("{error:#}")))?;
        Ok(JsWasmModule {
            module: Rc::new(module),
        })
    }
}

/// 从 JS 值取回 `WebAssembly.Module` 背后的 wasmtime 模块。
///
/// 只有本类实例能取回；其它值（含 `undefined`、普通对象、其它 class 的实例）统一抛
/// `TypeError`。错误文案是固定的 `WebAssembly.Instance: Argument 0 must be a
/// WebAssembly.Module`：`Module.exports`、`Module.imports`、`WebAssembly.instantiate`
/// （source 已经是模块时）、`new WebAssembly.Instance(...)` 都会走到这里。
fn module_from_value<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> EngineResult<Rc<Module>> {
    match Class::<JsWasmModule>::from_value(value) {
        Ok(module) => Ok(Rc::clone(&module.borrow().module)),
        Err(_) => Err(throw_wasm_error(
            ctx,
            "TypeError",
            "WebAssembly.Instance: Argument 0 must be a WebAssembly.Module",
        )),
    }
}

/// 一个已实例化模块的存储 + 实例句柄（按值导出）。
///
/// 每个实例一份 `Store<()>`：wasm 的线性内存、表、全局都在 store 里，因此实例之间不
/// 共享这些状态。`instance` 是 store 里的实例句柄。
///
/// 为什么是 `RefCell`：导出函数/内存/全局的 JS 闭包只持有共享的 `Rc<InstanceData>`，
/// 却需要在调用时拿到 `&mut Store`（wasmtime 的 `func.call` / `memory.grow` /
/// `global.set` 都要 `&mut`）。闭包路径一律用 `try_borrow(_mut)`，把重入冲突转成
/// `RuntimeError`；实例化期间的几处（`export_global` 读声明类型、`get_export`、表导出
/// 读长度快照）用普通 `borrow` / `borrow_mut`。
///
/// 只能在同一线程使用：`Rc` + `RefCell` + JS 值都不是 `Send`。
struct InstanceData {
    store: RefCell<Store<()>>,
    instance: Instance,
}

/// 把 wasmtime 值转成 JS 值。
///
/// `I32` / `F32` / `F64` → JS Number（`F32` 先用 `from_bits` 还原，再无损加宽成 `f64`）；
/// `I64` → BigInt（`BigInt::from_i64`）。其余 `Val` 变体（`V128`、引用）抛 `RuntimeError`。
/// 正常路径不会走到错误分支：三个调用方（`export_func` / `export_global` /
/// `call_js_import`）的签名都已先用 `num_type` 过滤过。
fn val_to_js<'js>(ctx: &Ctx<'js>, value: &Val) -> EngineResult<Value<'js>> {
    match value {
        Val::I32(number) => Ok(Value::new_int(ctx.clone(), *number)),
        Val::I64(number) => Ok(rquickjs::BigInt::from_i64(ctx.clone(), *number)?.into_value()),
        Val::F32(bits) => Ok(Value::new_float(ctx.clone(), f32::from_bits(*bits) as f64)),
        Val::F64(bits) => Ok(Value::new_float(ctx.clone(), f64::from_bits(*bits))),
        _ => Err(throw_wasm_error(
            ctx,
            "RuntimeError",
            "unsupported WebAssembly value type",
        )),
    }
}

/// JS 值 → wasmtime 值（按目标类型；数字按 JS `ToNumber`/`ToBigInt` 语义）。
///
/// `I32` / `F32` 先调用全局 `Number(value)`，再用 Rust 的 `as` 转换（`I32` 是饱和
/// 整数转换、`F32` 是 f64→f32）；`F64` 直接 `to_bits`，无转换损失。两个后果
/// 值得注意：
///
/// - 字符串、布尔、`null`、`undefined` 都会先变成数字（`undefined` → `NaN`）；
/// - Rust 的浮点→整数 `as` 是**饱和**转换、`NaN` 变 0，所以超出范围的值会夹到类型
///   边界，而不是回绕。
///
/// `I64` 分支优先接受 BigInt（`BigInt::to_i64`），否则同样走 `Number` 再 `as i64`；
/// 后者会先经过 `f64`，超过 2^53 的整数已经失真。
fn js_to_val<'js>(ctx: &Ctx<'js>, value: &Value<'js>, ty: NumType) -> EngineResult<Val> {
    match ty {
        NumType::I32 => {
            let number: f64 = ctx
                .globals()
                .get::<_, Function>("Number")?
                .call((value.clone(),))?;
            Ok(Val::I32(number as i32))
        }
        NumType::I64 => {
            if let Some(big) = value.as_big_int().cloned() {
                Ok(Val::I64(big.to_i64()?))
            } else {
                let number: f64 = ctx
                    .globals()
                    .get::<_, Function>("Number")?
                    .call((value.clone(),))?;
                Ok(Val::I64(number as i64))
            }
        }
        NumType::F32 => {
            let number: f64 = ctx
                .globals()
                .get::<_, Function>("Number")?
                .call((value.clone(),))?;
            Ok(Val::F32((number as f32).to_bits()))
        }
        NumType::F64 => {
            let number: f64 = ctx
                .globals()
                .get::<_, Function>("Number")?
                .call((value.clone(),))?;
            Ok(Val::F64(number.to_bits()))
        }
    }
}

/// 从 import 对象里取 `imports[module][name]`。
///
/// 任一步不成立都返回 `None`：`imports` 缺失或不是对象、命名空间不是对象、读取属性
/// 失败、属性值是 `undefined`。调用方把 `None` 一律当“导入缺失”，所以命名空间是原始
/// 值时也会报 `import is missing`，而不是参数类型错误。
fn import_value<'js>(
    imports: Option<&Object<'js>>,
    module: &str,
    name: &str,
) -> Option<Value<'js>> {
    let namespace: Option<Object<'js>> = imports?.get(module).ok().flatten();
    let namespace = namespace?;
    let value: Value<'js> = namespace.get(name).ok()?;
    if value.is_undefined() {
        None
    } else {
        Some(value)
    }
}

/// 一个 JS 导入函数的签名（用于跨 wasmtime 边界转换）。
///
/// `signature` 原样交给 `Func::new`；`results` 是给 `call_js_import` 用的窄化结果类型，
/// 与 `signature.results()` 一一对应（由同一份 `FuncType` 收集而来）。
#[derive(Clone)]
struct ImportSig {
    results: Vec<NumType>,
    signature: FuncType,
}

/// 把 wasmtime 的函数类型窄化成可跨 JS 边界的签名；不可转换时返回 `None`。
///
/// 参数只要出现一个非数值类型就立即拒绝；结果统一 `collect::<Option<Vec<_>>>()`，任一
/// 结果不可数值化也拒绝。调用方把 `None` 映射为 `LinkError ... unsupported signature`，
/// 即拒绝发生在**链接期**，而不是调用期。
fn import_sig(ty: &FuncType) -> Option<ImportSig> {
    // 引用类型/V128 的导入签名在 JS 边界上无法转换：链接期直接拒绝（LinkError）。
    if ty.params().any(|ty| num_type(&ty).is_none()) {
        return None;
    }
    let results: Option<Vec<NumType>> = ty.results().map(|ty| num_type(&ty)).collect();
    Some(ImportSig {
        results: results?,
        signature: ty.clone(),
    })
}

/// wasm → JS 的调用：在已激活的 JS 事务里执行保存的函数。
///
/// 步骤：
///
/// 1. 取线程本地的当前引擎；没有引擎则返回错误字符串（最终变成 wasmtime 错误）；
/// 2. 进入 `with_js`，用 `JsCallback::with_function` 从引擎持久槽恢复 JS 函数；
/// 3. 实参逐个 `val_to_js` 推进 `Args` 后调用；异常用 `take_exception` 取文案
///    （同时清空异常槽，后续 JS 调用不会误读旧异常）；
/// 4. 结果：多个要求 JS 返回数组并按下标转换，单个直接转换，零个忽略返回值。
///
/// 所有失败都压成 `String`（wasmtime 宿主函数只能返回 `wasmtime::Error`），因此原始
/// JS 错误对象不会穿过边界；外层 `export_func` 再把它包成 `RuntimeError: ...`。
fn call_js_import(
    callback: &super::JsCallback,
    params: &[Val],
    result_types: &[NumType],
    results: &mut [Val],
) -> Result<(), String> {
    let engine = super::current_engine().ok_or_else(|| "no JS engine is active".to_string())?;
    engine.with_js(|ctx| -> Result<(), String> {
        callback
            .with_function(&ctx, |ctx, function| -> Result<(), String> {
                let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
                for value in params {
                    let value = val_to_js(ctx, value).map_err(|error| error.to_string())?;
                    args.push_arg(value).map_err(|error| error.to_string())?;
                }
                let value: Value<'_> = function
                    .call_arg(args)
                    .map_err(|_| take_exception(ctx, JsErrorKind::Host).message)?;
                if result_types.len() > 1 {
                    let array = value.as_array().cloned().ok_or_else(|| {
                        "WebAssembly: imported function must return an array for multi-value results"
                            .to_string()
                    })?;
                    for (index, ty) in result_types.iter().enumerate() {
                        let item: Value<'_> =
                            array.get(index).map_err(|error| error.to_string())?;
                        results[index] =
                            js_to_val(ctx, &item, *ty).map_err(|error| error.to_string())?;
                    }
                } else if let Some(ty) = result_types.first() {
                    results[0] =
                        js_to_val(ctx, &value, *ty).map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .map_err(|error| error.message)?
    })
}

/// 导出函数 → JS 函数。
///
/// 包装期先检查签名：参数与结果都必须是数值类型，否则抛 `LinkError`。返回的 JS 函数：
///
/// 1. 从 `Rest` 取前 N 个实参（缺省补 `undefined`，多余忽略），按参数类型 `js_to_val`；
/// 2. 准备与结果个数等长的输出槽（初值只为占位，wasmtime 会覆盖）；
/// 3. **借出 store 并同步调用 wasm**。整次调用期间 store 都是可变借用：如果 wasm 回调
///    导入的 JS，而该 JS 又调用本实例的导出或读 `memory.buffer`，`try_borrow_mut` /
///    `try_borrow` 失败，抛 `RuntimeError`（`recursive WebAssembly call` /
///    `WebAssembly memory is busy`）；
/// 4. 结果 0 个 → `undefined`，1 个 → Number/BigInt，多个 → JS 数组。
fn export_func<'js>(
    ctx: &Ctx<'js>,
    data: &Rc<InstanceData>,
    func: Func,
    signature: FuncType,
) -> EngineResult<Function<'js>> {
    let params: Vec<NumType> = signature
        .params()
        .map(|ty| num_type(&ty))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| throw_wasm_error(ctx, "LinkError", "unsupported function signature"))?;
    let results: Vec<NumType> = signature
        .results()
        .map(|ty| num_type(&ty))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| throw_wasm_error(ctx, "LinkError", "unsupported function signature"))?;
    let data = Rc::clone(data);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> EngineResult<Value<'js>> {
            let mut values = Vec::with_capacity(params.len());
            for (index, ty) in params.iter().enumerate() {
                let value = args
                    .0
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                values.push(js_to_val(&ctx, &value, *ty)?);
            }
            let mut outputs: Vec<Val> = results.iter().map(|_| Val::I32(0)).collect();
            {
                let mut store = data.store.try_borrow_mut().map_err(|_| {
                    throw_wasm_error(&ctx, "RuntimeError", "recursive WebAssembly call")
                })?;
                func.call(&mut *store, &values, &mut outputs)
                    .map_err(|error| {
                        throw_wasm_error(&ctx, "RuntimeError", format!("{error:#}"))
                    })?;
            }
            match outputs.len() {
                0 => Ok(Value::new_undefined(ctx.clone())),
                1 => val_to_js(&ctx, &outputs[0]),
                _ => {
                    let array = rquickjs::Array::new(ctx.clone())?;
                    for (index, value) in outputs.iter().enumerate() {
                        array.set(index, val_to_js(&ctx, value)?)?;
                    }
                    Ok(array.into_value())
                }
            }
        },
    )
}

/// 导出内存 → JS 对象（`buffer` 读取当前内存的拷贝；`grow` 增长页数）。
///
/// `__kiana_wasm.memory` 生成带 `buffer` 访问器的对象。每次读 `buffer` 都把
/// `memory.data(&store)` 的当前内容 `to_vec()` 进新的 `ArrayBuffer`，因此：
///
/// - 写 `buffer` 不回写 wasm 内存（即模块文档的“内存不共享”）；
/// - 每次读到的是新对象，引用不会相等；
/// - 读的时候 store 被借出，wasm 调用期间读会抛
///   `RuntimeError: WebAssembly memory is busy`。
///
/// `grow(delta)`：非有限或 ≤ 0 的 delta 当作 0 页，其余截断成 `u64`；`Memory::grow`
/// 返回错误时抛 `RuntimeError`；成功返回**增长前**的页数（`f64`）。
fn export_memory<'js>(
    ctx: &Ctx<'js>,
    data: &Rc<InstanceData>,
    memory: Memory,
) -> EngineResult<Object<'js>> {
    let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
    let make: Function<'js> = helpers.get("memory")?;
    let get_buffer = {
        let data = Rc::clone(data);
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>| -> EngineResult<ArrayBuffer<'js>> {
                let store = data.store.try_borrow().map_err(|_| {
                    throw_wasm_error(&ctx, "RuntimeError", "WebAssembly memory is busy")
                })?;
                ArrayBuffer::new(ctx.clone(), memory.data(&*store).to_vec())
            },
        )?
    };
    let grow = {
        let data = Rc::clone(data);
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, delta: f64| -> EngineResult<f64> {
                let mut store = data.store.try_borrow_mut().map_err(|_| {
                    throw_wasm_error(&ctx, "RuntimeError", "WebAssembly memory is busy")
                })?;
                let pages = if delta.is_finite() && delta > 0.0 {
                    delta as u64
                } else {
                    0
                };
                memory
                    .grow(&mut *store, pages)
                    .map(|old| old as f64)
                    .map_err(|error| throw_wasm_error(&ctx, "RuntimeError", format!("{error:#}")))
            },
        )?
    };
    make.call((get_buffer, grow))
}

/// 导出全局变量 → JS 对象（`value` getter/setter）。
///
/// 先读声明的内容类型；非数值类型在包装期就抛 `LinkError`（不会等到读值）。getter /
/// setter 由 `__kiana_wasm.global` 挂成访问器，每次进出 store；setter 按声明类型
/// `js_to_val`，设置失败抛 `RuntimeError`。借用冲突时抛
/// `RuntimeError: WebAssembly instance is busy`。
fn export_global<'js>(
    ctx: &Ctx<'js>,
    data: &Rc<InstanceData>,
    global: Global,
) -> EngineResult<Value<'js>> {
    let content = {
        let store = data.store.borrow();
        global.ty(&*store).content().clone()
    };
    let Some(num) = num_type(&content) else {
        return Err(throw_wasm_error(
            ctx,
            "LinkError",
            "unsupported WebAssembly global type",
        ));
    };
    let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
    let make: Function<'js> = helpers.get("global")?;
    let getter = {
        let data = Rc::clone(data);
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
                let mut store = data.store.try_borrow_mut().map_err(|_| {
                    throw_wasm_error(&ctx, "RuntimeError", "WebAssembly instance is busy")
                })?;
                val_to_js(&ctx, &global.get(&mut *store))
            },
        )?
    };
    let setter = {
        let data = Rc::clone(data);
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: Value<'js>| -> EngineResult<()> {
                let val = js_to_val(&ctx, &value, num)?;
                let mut store = data.store.try_borrow_mut().map_err(|_| {
                    throw_wasm_error(&ctx, "RuntimeError", "WebAssembly instance is busy")
                })?;
                global
                    .set(&mut *store, val)
                    .map_err(|error| throw_wasm_error(&ctx, "RuntimeError", format!("{error:#}")))
            },
        )?
    };
    make.call((getter, setter))
}

/// 实例化模块：解析导入，返回 `(Store 数据, exports 对象)`。
///
/// 阶段：
///
/// 1. 每个实例新建一个 `Store<()>`（引擎仍是进程级 `ENGINE`）；
/// 2. 按 `module.imports()` 的声明顺序逐个解析（缺失导入的文案带
///    `Import #<index> module="..." <kind> "<name>"`），导入类型决定处理方式：
///    - function：JS 值必须是可调用函数，签名必须能数值化；然后把函数登记到引擎持久槽
///      （`JsCallback::new`），wasmtime 宿主闭包只持有 id；
///    - memory / table / global：**只检查 JS 侧提供了这个导入**，随后在 store 里按声明
///      类型新分配，完全不读 JS 对象的内容；
///    - tag：不支持，直接 `LinkError`。
/// 3. `Instance::new` 完成链接与实例化，失败统一转成 `LinkError`；
/// 4. 遍历 `module.exports()`，把每个导出包装成 JS 值写进 `exports`；不认识的导出跳过。
///
/// 返回值里的 `Rc<InstanceData>` 由调用方决定挂到哪里：`install_in_ctx` 的两个入口都只
/// 保留导出的 JS 值，实例的存活因此由这些值（函数/内存/全局对象里的闭包）的引用计数
/// 决定——模块没有任何此类导出时，store 在返回后即被释放。
///
/// 函数导入的另一侧不会随实例释放：`JsCallback::new` 把 JS 函数写进**引擎**的持久槽，
/// 本文件没有注销路径，这条登记要等引擎 `Drop` 才清除。反复实例化带函数导入的模块会
/// 累积持久槽条目。
fn instantiate<'js>(
    ctx: &Ctx<'js>,
    module: &Module,
    imports: Option<&Object<'js>>,
) -> EngineResult<(Rc<InstanceData>, Object<'js>)> {
    let mut store = Store::new(&ENGINE, ());
    let mut externs = Vec::new();
    // 按模块声明的导入顺序解析；错误文案里的 `#index` 就是这里的序号。
    for (index, import) in module.imports().enumerate() {
        let module_name = import.module().to_owned();
        let name = import.name().to_owned();
        let ty = import.ty();
        let value = import_value(imports, &module_name, &name);
        // 缺失导入统一走这个闭包：文案带序号、module/name 与 kind，方便扩展作者定位。
        let missing = || {
            throw_wasm_error(
                ctx,
                "LinkError",
                format!(
                    "WebAssembly.Instance(): Import #{index} module=\"{module_name}\" {} \"{name}\" error: import is missing",
                    kind_name(&ty)
                ),
            )
        };
        let extern_ = match &ty {
            ExternType::Func(func_ty) => {
                // 函数导入：先检查签名可数值化，再检查 JS 侧确实给了可调用值。
                let Some(sig) = import_sig(func_ty) else {
                    return Err(throw_wasm_error(
                        ctx,
                        "LinkError",
                        format!(
                            "WebAssembly.Instance(): Import #{index} module=\"{module_name}\" function \"{name}\" error: unsupported signature"
                        ),
                    ));
                };
                let Some(function) = value.and_then(|value| value.as_function().cloned()) else {
                    return Err(throw_wasm_error(
                        ctx,
                        "LinkError",
                        format!(
                            "WebAssembly.Instance(): Import #{index} module=\"{module_name}\" function \"{name}\" error: function import requires a callable"
                        ),
                    ));
                };
                let callback = super::JsCallback::new(ctx, function);
                let result_types = sig.results.clone();
                let func = Func::new(
                    &mut store,
                    sig.signature,
                    move |_caller, params, results| {
                        call_js_import(&callback, params, &result_types, results)
                            .map_err(wasmtime::Error::msg)
                    },
                );
                Extern::Func(func)
            }
            ExternType::Memory(memory_ty) => {
                // 导入内存与 JS 对象不共享（见模块文档）：按声明类型在实例 store 内分配。
                let _ = value.ok_or_else(missing)?;
                Extern::Memory(
                    Memory::new(&mut store, memory_ty.clone())
                        .map_err(|error| throw_wasm_error(ctx, "LinkError", error.to_string()))?,
                )
            }
            ExternType::Table(table_ty) => {
                let _ = value.ok_or_else(missing)?;
                // 表初值固定为 null 函数引用（`Ref::Func(None)`）；构造失败按 LinkError 报出。
                Extern::Table(
                    Table::new(&mut store, table_ty.clone(), Ref::Func(None)).map_err(|error| {
                        throw_wasm_error(ctx, "LinkError", format!("{error:#}"))
                    })?,
                )
            }
            ExternType::Global(global_ty) => {
                let _ = value.ok_or_else(missing)?;
                // 初值由 `zero_val` 按声明的内容类型给出。
                Extern::Global(
                    Global::new(&mut store, global_ty.clone(), zero_val(global_ty.content()))
                        .map_err(|error| {
                            throw_wasm_error(ctx, "LinkError", format!("{error:#}"))
                        })?,
                )
            }
            ExternType::Tag(_) => {
                // tag（异常处理）导入没有 JS 表示：即使提供了值也直接拒绝。
                let _ = value.ok_or_else(missing)?;
                return Err(throw_wasm_error(
                    ctx,
                    "LinkError",
                    format!(
                        "WebAssembly.Instance(): Import #{index} module=\"{module_name}\" tag \"{name}\" error: tag imports are not supported"
                    ),
                ));
            }
        };
        externs.push(extern_);
    }

    let instance = Instance::new(&mut store, module, &externs)
        .map_err(|error| throw_wasm_error(ctx, "LinkError", format!("{error:#}")))?;
    let data = Rc::new(InstanceData {
        store: RefCell::new(store),
        instance,
    });

    let exports = Object::new(ctx.clone())?;
    // 导出包装在实例化之后进行：`get_export` 需要 `&mut Store`，这里对每个导出各借用
    // 一次；此时还没有任何 JS 回调在读 store，不会有借用冲突。
    for export in module.exports() {
        let name = export.name().to_owned();
        let ty = export.ty();
        let Some(extern_) = data
            .instance
            .get_export(&mut *data.store.borrow_mut(), &name)
        else {
            continue;
        };
        let value: Value<'js> = match extern_ {
            Extern::Func(func) => {
                let ExternType::Func(signature) = ty else {
                    continue;
                };
                export_func(ctx, &data, func, signature)?.into_value()
            }
            Extern::Memory(memory) => export_memory(ctx, &data, memory)?.into_value(),
            Extern::Global(global) => export_global(ctx, &data, global)?,
            Extern::Table(table) => {
                // 导出的表只暴露长度快照；get/set/grow 是占位实现（见 HELPERS_JS.table）。
                let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
                let make: Function<'js> = helpers.get("table")?;
                let store = data.store.borrow();
                let length = table.size(&*store) as f64;
                make.call((length,))?
            }
            _ => continue,
        };
        exports.set(name, value)?;
    }
    Ok((data, exports))
}

/// 把 `Module` 包成 JS `WebAssembly.Module` 实例。
///
/// 克隆 `Module` 句柄放进新的 `Rc`，再用 `Class::instance` 创建真正的类实例：
/// `instanceof WebAssembly.Module` 成立。
fn module_class_value<'js>(ctx: &Ctx<'js>, module: &Module) -> EngineResult<Value<'js>> {
    let instance = Class::instance(
        ctx.clone(),
        JsWasmModule {
            module: Rc::new(module.clone()),
        },
    )?;
    Ok(instance.into_value())
}

/// 安装 `WebAssembly` 全局（幂等）。
///
/// 由 `loader::js_host::install` 在 Node shim 安装完成后调用（装配顺序第 4 步 /
/// §9.2 第 5 步）。此时 `prelude_core` 已求值，但本模块不依赖 `__kiana`，只用自己
/// 求值的 `__kiana_wasm`。
///
/// 幂等：全局 `WebAssembly` 已有定义（不是 `undefined` / `null`）时直接返回，不重复
/// 求值 `HELPERS_JS`。否则在一个 `with_js` 事务里完成 `install_in_ctx` 的全部安装。
///
/// 失败（`HELPERS_JS` 求值失败或属性设置失败）原样返回 `rquickjs::Error`；与其它安装
/// 步骤一样，调用方应丢弃引擎而不是重试。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    engine.with_js(|ctx| install_in_ctx(&ctx))
}

/// 在一个 JS 事务里安装 `WebAssembly`（[`install`] 的 `'js` 具体化版本）。
///
/// 安装顺序（全部同步）：
///
/// 1. 幂等检查；
/// 2. 求值 `HELPERS_JS`，建立全局 `__kiana_wasm`；
/// 3. 建 `WebAssembly` 对象挂到全局，并用 `Class::<JsWasmModule>::define` 定义
///    `Module` 类（属性名来自 `#[rquickjs::class(rename = "Module")]`）；
/// 4. 给 `Module` 构造器挂静态 `exports` / `imports`：分别返回 `{name, kind}` 与
///    `{module, name, kind}` 数组；
/// 5. 挂 `validate`（同步 bool）、`compile` / `instantiate`（返回已完成的 Promise）；
/// 6. 把 `__kiana_wasm` 里的 JS class `Instance` / `Memory` 原样挂到 `WebAssembly` 上。
///
/// 构造器运行时按名字查 `__kiana_wasm.instance`；两步都在安装事务内同步完成，这里先
/// 注册只是与构造器的查找目标保持一致（防御性顺序，不产生可观察差异）。
fn install_in_ctx<'js>(ctx: &Ctx<'js>) -> EngineResult<()> {
    {
        if ctx
            .globals()
            .get::<_, Option<Value>>("WebAssembly")?
            .is_some_and(|value| !value.is_undefined())
        {
            return Ok(());
        }
        // 建立全局 `__kiana_wasm`；下面所有桥接函数都按名字从这里取。
        ctx.eval::<(), _>(HELPERS_JS)?;

        // 先挂全局对象，再把类与静态方法写进它（`Class::define` 的属性名来自 rename）。
        let wasm = Object::new(ctx.clone())?;
        ctx.globals().set("WebAssembly", wasm.clone())?;
        Class::<JsWasmModule>::define(&wasm)?;
        let module_constructor: Function<'js> = wasm.get("Module")?;
        // `WebAssembly.Module.exports(module)`：条目形状 `{ name, kind }`。
        module_constructor.set(
            "exports",
            Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>, module: Value<'js>| -> EngineResult<Value<'js>> {
                    let module = module_from_value(&ctx, &module)?;
                    let array = rquickjs::Array::new(ctx.clone())?;
                    for (index, export) in module.exports().enumerate() {
                        let entry = Object::new(ctx.clone())?;
                        entry.set("name", export.name().to_owned())?;
                        entry.set("kind", kind_name(&export.ty()))?;
                        array.set(index, entry)?;
                    }
                    Ok(array.into_value())
                },
            )?,
        )?;
        module_constructor.set(
            "imports",
            Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>, module: Value<'js>| -> EngineResult<Value<'js>> {
                    let module = module_from_value(&ctx, &module)?;
                    let array = rquickjs::Array::new(ctx.clone())?;
                    for (index, import) in module.imports().enumerate() {
                        let entry = Object::new(ctx.clone())?;
                        entry.set("module", import.module().to_owned())?;
                        entry.set("name", import.name().to_owned())?;
                        entry.set("kind", kind_name(&import.ty()))?;
                        array.set(index, entry)?;
                    }
                    Ok(array.into_value())
                },
            )?,
        )?;

        // `WebAssembly.validate(bytes)`。
        // 只回答“能否编译”，不抛错：连参数不是 BufferSource 的 TypeError 也被
        // `ctx.catch()` 取走清空并返回 false，调用方拿不到失败原因。
        wasm.set(
            "validate",
            Function::new(ctx.clone(), |ctx: Ctx<'js>, bytes: Value<'js>| -> bool {
                match wasm_module_bytes(&ctx, bytes) {
                    Ok(bytes) => Module::validate(&ENGINE, &bytes).is_ok(),
                    Err(_) => {
                        let _ = ctx.catch();
                        false
                    }
                }
            })?,
        )?;

        // `WebAssembly.compile(bytes)`：同步编译，成功路径返回已完成的 Promise；
        // 编译失败在这里同步抛 `CompileError`（`?` 在 `resolved` 之前返回）。
        wasm.set(
            "compile",
            Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>, bytes: Value<'js>| -> EngineResult<Value<'js>> {
                    let bytes = wasm_module_bytes(&ctx, bytes)?;
                    let module = Module::new(&ENGINE, &bytes).map_err(|error| {
                        throw_wasm_error(&ctx, "CompileError", format!("{error:#}"))
                    })?;
                    let value = module_class_value(&ctx, &module)?;
                    resolved(&ctx, value)
                },
            )?,
        )?;
        // `WebAssembly.instantiate(source, imports)`：source 是 `WebAssembly.Module`
        // 实例时复用它，否则按字节编译。返回形状按规范：Module → Instance 对象，字节 →
        // `{ module, instance }`。链接/实例化失败同样同步抛出。
        wasm.set(
            "instantiate",
            Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>,
                 source: Value<'js>,
                 imports: Opt<Value<'js>>|
                 -> EngineResult<Value<'js>> {
                    let imports = imports.0.and_then(|value| value.as_object().cloned());
                    // 判定只看“能否取回 JsWasmModule 实例”；其它任何值都当字节处理。
                    let from_bytes = Class::<JsWasmModule>::from_value(&source).is_err();
                    let module = if from_bytes {
                        let bytes = wasm_module_bytes(&ctx, source)?;
                        Module::new(&ENGINE, &bytes).map_err(|error| {
                            throw_wasm_error(&ctx, "CompileError", error.to_string())
                        })?
                    } else {
                        (*module_from_value(&ctx, &source)?).clone()
                    };
                    // Rust 侧只保留 exports：实例（含 store）由导出值里的 Rc 闭包续命。
                    let (_, exports) = instantiate(&ctx, &module, imports.as_ref())?;
                    let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
                    let make: Function<'js> = helpers.get("makeInstance")?;
                    let instance: Object<'js> = make.call((exports,))?;
                    let result = if from_bytes {
                        let wrapper = Object::new(ctx.clone())?;
                        wrapper.set("module", module_class_value(&ctx, &module)?)?;
                        wrapper.set("instance", instance)?;
                        wrapper.into_value()
                    } else {
                        instance.into_value()
                    };
                    resolved(&ctx, result)
                },
            )?,
        )?;

        // `Instance` / `Memory` 由 HELPERS_JS 的真实 JS class 提供（Rust 闭包不是
        // constructor，`new WebAssembly.Instance(...)` 会抛 "not a constructor"）。
        // 先注册 `__kiana_wasm.instance`（构造器运行时按名字查它），再暴露两个 class。
        let helpers: Object<'js> = ctx.globals().get("__kiana_wasm")?;
        helpers.set(
            "instance",
            Function::new(
                ctx.clone(),
                |ctx: Ctx<'js>,
                 module: Value<'js>,
                 imports: Opt<Value<'js>>|
                 -> EngineResult<Object<'js>> {
                    let module = module_from_value(&ctx, &module)?;
                    let imports = imports.0.and_then(|value| value.as_object().cloned());
                    let (_, exports) = instantiate(&ctx, &module, imports.as_ref())?;
                    Ok(exports)
                },
            )?,
        )?;
        wasm.set("Instance", helpers.get::<_, Function>("Instance")?)?;
        wasm.set("Memory", helpers.get::<_, Function>("Memory")?)?;
        Ok(())
    }
}
