//! rquickjs 宿主：`__kiana_host.*` 原语、`Function::call` 的 `This` 纪律、错误转换。
//!
//! 设计见 `spec/design-extension-runtime.md` §6（Node 内建与全局）、§7.3（真实 TCP）、
//! §5.8（Intl）与 `ARCHITECTURE.md` §8 第 27、29 条。本文件是 **loader-host** 单元：
//!
//! - [`install_intl`]：安装 `Intl`（Segmenter/NumberFormat/DateTimeFormat）、`toLocale*`
//!   覆盖与 `__kiana_host.segment`/`intlNumber`/`intlDate` 原语。**必须先于任何模块**（§9.2）。
//! - [`install`]：安装全局（console/process/Buffer/TextEncoder/TextDecoder/URL/fetch/
//!   crypto/structuredClone/Web Streams…）与 `node:*` shim，填充 `__kiana_node_shims`。
//!
//! 规则（跨单元契约）：
//!
//! - 原语一律是 `__kiana_host.*` 上的函数，接收 `Ctx`；宿主异步走 `__kiana.promise(kind, payload)`
//!   的 kind 注册表（`engine.register_host_kind` + `spawn_local` + 完成队列）。
//! - JS 侧 `node:*` 模块是**真正的对象**（`js/node/*.js`），Rust 只提供同步原语与流式事件。
//! - 宿主错误物化成**抛出**的 JS 异常；Node 暴露的 `code`/`errno`/`syscall`/`path` 属性逐条
//!   附上，消息文本对齐 Node（`ENOENT: no such file or directory, open '/x'`）。
//!
//! # node:path 的重复实现（已知偏差）
//!
//! `ARCHITECTURE.md` §2.2 不允许本 crate 依赖 `kiana-durable`，所以 `GAPS.md` G4.3 的
//! “`node:path` 唯一实现在 `kiana-durable::env::path`” 在这里无法复用。本单元在
//! `js/node/path.js` 里独立实现 POSIX 与 win32 两套语义（默认按 `process.platform` 选择）。
//! 该重复已上报架构者/评审（见交付说明）。
//!
//! # 这个文件的边界：JS 语义与宿主能力
//!
//! 它只做两件事：把 QuickJS 没有的能力（真实文件系统、进程、TCP、HTTP、ICU 数据）以
//! `__kiana_host.*` 同步函数暴露给 JS；把需要等待的能力登记成 host kind，由 JS 侧的
//! `__kiana.promise(kind, payload)` 驱动。Node 的可观察语义（参数校验、事件/流、部分错误文案）
//! 尽量留在 `js/prelude_node.js` 与 `js/node/*.js`，Rust 侧只保证原语行为与错误形状。
//!
//! 分工带来三条硬约束：
//!
//! - 同步原语在 JS 调用栈内执行，不能等待，也不能再进 `JsEngine::with_js`
//!   （`engine` 的 `HostKindHandler` 文档：处理器在 JS 调用栈里运行）；
//! - host kind 的处理器必须在 `spawn_local` 之前把 payload 解析成 Rust 类型，完成后用
//!   `completion_sender()` 回投；跨 `await` 持有的 JS 回调必须用 `capture()` 包起来；
//! - 二进制一律用 base64 字符串过界：`b64_decode` 对非法输入返回空 `Vec` 而不是报错
//!   （`unwrap_or_default`），因此坏数据进来时下游看到的是空字节序列。
//!
//! # 执行流程
//!
//! `install_intl`：装 Rust 原语（`locale`/`timeZone`/`intlValidTimeZone`/`segment`/
//! `intlNumber`/`intlNumberBig`/`intlDate`）→ 求值 `js/intl.js`。
//!
//! `install`：注册 6 个 host kind（`fs`/`watch`/`child.spawn`/`net.listen`/`net.connect`/
//! `fetch`）→ 求值 `js/prelude_node.js` → 按 `NODE_SHIMS` 的固定顺序求值 25 个
//! `js/node/*.js` → 补齐 `node:`/裸名别名 → 用 `NODE_MODULE_EXPORTS` 生成并登记 `node:*`
//! 的 ESM 包装源。先注册 kind 再求值 JS：`__kiana.promise` 在 `engine::install`（prelude_core）
//! 时已经存在，求值期间发生的任何宿主调用都必须命中已注册的 kind。
//!
//! # 失败模式与幂等
//!
//! 两个安装函数返回 `EngineResult`；任何一步失败由装配器（`Generation::with_config`）负责
//! 丢弃半装的引擎。运行期错误分两条路：同步原语抛出 Node 形状的 JS 异常（见 `error` 模块），
//! 异步 kind 通过 `resolve`/`reject` 投递 JSON，由 JS 侧 `__kiana_node_internal.promise`
//! 把带 `__kianaError` 标记的对象物化成真正的 `Error` 并复制 `code`/`errno`/`syscall`/`path`。
//!
//! 幂等标记分别是全局 `Intl` 与 `globalThis.__kiana_node_shims`，建立时机不同：
//! `js/intl.js` 在定义成员之前就发布 `Intl`；`js/prelude_node.js` 在文件末尾才创建
//! `__kiana_node_shims`。因此脚本求值中途失败时，标记可能已经存在，再次调用会直接返回，
//! 不会补齐失败的那部分（`Intl` 会留下缺成员的半成品）。
//!
//! # 阅读顺序
//!
//! `install_intl`/`install` → `install_sync_primitives`（原语清单）→ `error`（错误形状）→
//! `paths`/`fs`/`child`/`net`/`fetch`（各能力）→ `intl`（格式化）。`sys` 是纯平台查询，可最后看。
#![cfg(feature = "runtime")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Object, Value};

use crate::engine::{EngineResult, HostCall, JsEngine};

/// 全局（console/process/Buffer/Web Streams/fetch/…）源文件。
const PRELUDE_NODE: &str = include_str!("../../js/prelude_node.js");
/// `Intl` 与 `toLocale*` 覆盖（§5.8、§6.4）。
const INTL_JS: &str = include_str!("../../js/intl.js");

// ---------------------------------------------------------------------------
// 公共安装入口
// ---------------------------------------------------------------------------

/// 安装 `Intl` 与 `toLocale*` 覆盖（§5.8）。**必须在任何可能捕获 `Intl` 的模块之前**调用
/// （§9.2 第 3 步）。
///
/// 安装内容分两半：先装 Rust 原语（`locale`/`timeZone`/`intlValidTimeZone`/`segment`/
/// `intlNumber`/`intlNumberBig`/`intlDate`），再求值 `js/intl.js`（ECMA-402 表面：构造器
/// 参数校验、`toLocale*` 覆盖；`Intl.Collator`/`PluralRules`/`RelativeTimeFormat` 不支持并抛
/// `RangeError`）。原语必须先于脚本，脚本顶层立即读取 `__kiana_host`。
///
/// 幂等判断只看全局 `Intl` 是否存在且不是 `undefined`/`null`。`js/intl.js` 在定义成员之前
/// 就发布了 `Intl`，所以求值中途失败会让下一次调用直接返回，留下缺成员的 `Intl`。
pub fn install_intl(engine: &JsEngine) -> EngineResult<()> {
    if is_installed(engine, "Intl") {
        return Ok(());
    }
    intl::install_primitives(engine)?;
    eval_source(engine, INTL_JS, "<intl>")
}

/// 安装 Node 全局与 `node:*` shim（§6、§9.2 第 4–5 步）。
///
/// 调用顺序：`install_intl` → `engine::install`（prelude_core）→ 本函数。本函数可重复调用
/// （第二次直接返回）。
///
/// 执行顺序与副作用（改动会影响所有 `js/node/*.js` 与 `js/cjs.js`）：
///
/// 1. 建立 4 份单线程状态（`Rc<RefCell<…>>`：`net::NetState`/`fetch::FetchState`/
///    `child::ChildState`/`fs::FsWatchState`）。
/// 2. 注册 `__kiana_host.*` 同步原语（见 `install_sync_primitives`）。
/// 3. 注册 6 个 host kind，把上一步的状态以 `Rc` 共享进处理器（同步原语仍持有同一份）；
///    kind 名必须与 JS 侧 `__kiana.promise(kind, …)` 的字符串一致，否则运行期报
///    `unknown host kind`。
/// 4. 求值 `js/prelude_node.js`，再按 `NODE_SHIMS` 的顺序求值每个 `js/node/*.js`。该顺序是
///    依赖顺序：后面的 shim 在顶层读取前面 shim 写进 `__kiana_node_shims` 的对象。
/// 5. 补齐 `node:fs/promises` ↔ `fs/promises`、`node:path/posix` ↔ `path/posix` 等别名。
///    每个 shim 自己通常已同时注册 `x` 与 `node:x` 两种拼写，这一步只兜底子路径。
/// 6. 登记 `node:*` 的 ESM 包装源，供 loader 在 `Module::declare` 时取用。
///
/// `webassembly` 特性开启时最后调用 `loader::wasm::install`。
///
/// 失败：任何一步的错误原样返回。调用方（装配器）应丢弃引擎，不要在同一引擎上重试——
/// `__kiana_node_shims` 由 prelude 在文件末尾创建，重试会被幂等判断挡掉，留下半装状态。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    if is_installed(engine, "__kiana_node_shims") {
        return Ok(());
    }

    let net_state = Rc::new(RefCell::new(net::NetState::default()));
    let fetch_state = Rc::new(RefCell::new(fetch::FetchState::default()));
    let child_state = Rc::new(RefCell::new(child::ChildState::default()));
    let fs_state = Rc::new(RefCell::new(fs::FsWatchState::default()));

    install_sync_primitives(
        engine,
        Rc::clone(&net_state),
        Rc::clone(&fetch_state),
        Rc::clone(&child_state),
        Rc::clone(&fs_state),
    )?;
    engine.register_host_kind("fs", fs::handle_fs_kind);
    {
        let state = Rc::clone(&fs_state);
        engine.register_host_kind("watch", move |call: HostCall<'_>| -> EngineResult<()> {
            fs::handle_watch_kind(call, &state)
        });
    }
    engine.register_host_kind(
        "child.spawn",
        move |call: HostCall<'_>| -> EngineResult<()> {
            child::handle_spawn_kind(call, &child_state)
        },
    );
    {
        let state = Rc::clone(&net_state);
        engine.register_host_kind(
            "net.listen",
            move |call: HostCall<'_>| -> EngineResult<()> { net::handle_listen_kind(call, &state) },
        );
    }
    {
        let state = Rc::clone(&net_state);
        engine.register_host_kind(
            "net.connect",
            move |call: HostCall<'_>| -> EngineResult<()> {
                net::handle_connect_kind(call, &state)
            },
        );
    }
    engine.register_host_kind("fetch", move |call: HostCall<'_>| -> EngineResult<()> {
        fetch::handle_fetch_kind(call, &fetch_state)
    });

    eval_source(engine, PRELUDE_NODE, "<prelude_node>")?;

    for (name, source) in NODE_SHIMS {
        let filename = format!("<node:{name}>");
        eval_source(engine, source, &filename)?;
    }

    // `__kiana_node_shims` 的键补齐（每个 shim 自己注册 `node:x` 与 `x` 两种拼写；这里只做
    // 别名兜底，例如 `fs/promises`）。
    eval_source(
        engine,
        r#"
        (function () {
          "use strict";
          var shims = globalThis.__kiana_node_shims;
          if (!shims) return;
          var aliases = [
            ["node:fs/promises", "fs/promises"],
            ["node:path/posix", "path/posix"],
            ["node:path/win32", "path/win32"],
            ["node:util/types", "util/types"],
            ["node:stream/promises", "stream/promises"],
            ["node:stream/web", "stream/web"],
            ["node:timers/promises", "timers/promises"]
          ];
          for (var i = 0; i < aliases.length; i++) {
            var from = aliases[i][0];
            var to = aliases[i][1];
            if (shims[from] === undefined && shims[to] !== undefined) shims[from] = shims[to];
            if (shims[to] === undefined && shims[from] !== undefined) shims[to] = shims[from];
          }
        })();
        "#,
        "<node-shims-aliases>",
    )?;

    register_node_module_sources(engine);
    // 可选的 `WebAssembly`（wasmtime）；默认构建不安装这个全局（设计文档 §7.4）。
    #[cfg(feature = "webassembly")]
    crate::loader::wasm::install(engine)?;
    Ok(())
}

/// 求值一段全局脚本，失败时返回带文件名与 JS 异常文本的 loader 错误。
///
/// 求值选项：`global = true`（全局脚本，不是模块）、`strict = true`（避免 sloppy 语义）、
/// `backtrace_barrier = true`（错误栈不显示宿主调用帧）、`filename` 只影响错误文本。
///
/// 失败时先用 `ctx.catch()` 取走挂起的异常：对象有 `message` 就当消息，否则接受字符串值，
/// 再否则用 `Debug` 文本。`catch()` 同时清空异常槽，所以返回 `Err` 时上下文是干净的
/// （与 `JsEngine::eval_script` 的约定相同：残留异常会被后续模块加载误读）。
fn eval_source(engine: &JsEngine, source: &str, filename: &str) -> EngineResult<()> {
    engine.with_js(|ctx| {
        let mut options = rquickjs::context::EvalOptions::default();
        options.global = true;
        options.strict = true;
        options.backtrace_barrier = true;
        options.filename = Some(filename.to_owned());
        match ctx.eval_with_options::<(), _>(source, options) {
            Ok(()) => Ok(()),
            Err(error) => {
                let value = ctx.catch();
                let message = value
                    .as_object()
                    .and_then(|object| object.get::<_, Option<String>>("message").ok().flatten())
                    .or_else(|| value.as_string().and_then(|text| text.to_string().ok()))
                    .unwrap_or_else(|| format!("{error:?}"));
                Err(rquickjs::Error::new_loading_message(
                    filename.to_owned(),
                    message,
                ))
            }
        }
    })
}

/// 全局是否“已安装”：存在且既不是 `undefined` 也不是 `null`。
///
/// 只服务于幂等判断，因此 `false`/`0`/空对象都算已安装，也不检查对象成员是否齐全；
/// 求值中途失败留下的半成品全局同样会被判为“已安装”（见 `install_intl` 的说明）。
fn is_installed(engine: &JsEngine, global: &str) -> bool {
    engine.with_js(|ctx| {
        ctx.globals()
            .get::<_, Option<Value>>(global)
            .ok()
            .flatten()
            .is_some_and(|value| !value.is_undefined() && !value.is_null())
    })
}

/// prelude 源文本（体积断言用）。
pub fn prelude_node_source() -> &'static str {
    PRELUDE_NODE
}

/// `Intl` 源文本（体积断言用）。
pub fn intl_source() -> &'static str {
    INTL_JS
}

// ---------------------------------------------------------------------------
// 同步原语
// ---------------------------------------------------------------------------

/// `process.arch` / `os.arch()` 的 Node 名称（Rust 的 `x86_64` 是 Node 的 `x64`）。
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "ia32",
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// 把全部同步原语挂到 `__kiana_host` 上（`install` 的第 2 步）。
///
/// `JsEngine::with_config` 已经创建了 `__kiana_host` 对象（`engine::install_host_primitives`），
/// 这里只往里加成员；取不到对象说明调用方用错了引擎或顺序，直接返回引擎错误。
///
/// 跨语言的类型约定（改签名会同时破坏 JS 调用点）：
///
/// - JS `number` ↔ Rust `f64`：进程号、fd、句柄 id 都走这里，超过 2^53 会丢精度；
/// - JS `string` ↔ Rust `String`；缺省参数用 `Opt<T>`（`undefined` 视同缺省）；
/// - 二进制用 base64 字符串（见文件头）；
/// - 返回 `Result<_, rquickjs::Error>` 的闭包失败时：实现统一用 `Exception::throw_*` /
///   `error::throw_node_error` 挂起 JS 异常，返回的 `Error` 只是失败信号。
///
/// 失败模式：进程/环境类查询失败一律降级而不抛错（`cwd` → `/`、`execPath` → `kiana`、
/// 环境变量按 lossy 字符串快照）。会抛 JS 异常的：`chdir`/`kill`/`resolve`/`read`/`fsSync`/
/// `fileURLToPath` 抛 Node 形状的错误对象，`urlParse` 抛 `TypeError`（消息 `Invalid URL: …`）。
/// `spawnSync` 不抛错，启动失败放在返回 JSON 的 `error` 字段（见该函数）。
fn install_sync_primitives(
    engine: &JsEngine,
    net_state: Rc<RefCell<net::NetState>>,
    fetch_state: Rc<RefCell<fetch::FetchState>>,
    child_state: Rc<RefCell<child::ChildState>>,
    fs_state: Rc<RefCell<fs::FsWatchState>>,
) -> EngineResult<()> {
    engine.with_js(|ctx| {
        let host: Object<'_> = ctx.globals().get("__kiana_host")?;

        // 宏只做两件事：把 Rust 闭包变成绑定到当前 `Ctx` 的 JS 函数，再挂到 `__kiana_host`
        // 的同名属性。需要共享状态的原语不走宏，而是在下面用手写块捕获 `Rc<RefCell<…>>`。
        macro_rules! set_fn {
            ($name:literal, $f:expr) => {
                host.set($name, Function::new(ctx.clone(), $f)?)?;
            };
        }

        // ---- 进程 / 环境 ---------------------------------------------------
        set_fn!("pid", || -> f64 { f64::from(std::process::id()) });
        set_fn!("platform", || -> String {
            match std::env::consts::OS {
                "linux" => "linux".to_owned(),
                "macos" => "darwin".to_owned(),
                // Node (and design §6.2) call Windows "win32", not "windows".
                "windows" => "win32".to_owned(),
                other => other.to_owned(),
            }
        });
        set_fn!("arch", || -> String { node_arch().to_owned() });
        set_fn!("machine", || -> String {
            match std::env::consts::ARCH {
                "aarch64" if cfg!(target_os = "macos") => "arm64".to_owned(),
                other => other.to_owned(),
            }
        });
        set_fn!("cwd", || -> String {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_owned())
        });
        set_fn!("chdir", |ctx: Ctx<'_>,
                          path: String|
         -> Result<(), rquickjs::Error> {
            std::env::set_current_dir(&path).map_err(|error| {
                error::throw_node_error(&ctx, error::io_error("chdir", &path, &error))
            })
        });
        // 每次调用都重新读取当前进程环境并整体序列化成 JSON；JS 侧解析成 `process.env`，
        // 因此是“调用时刻的快照”，之后的环境变化不会自动同步到 JS 对象。
        set_fn!("env", || -> String {
            let mut map = serde_json::Map::new();
            // `vars()` panics on non-Unicode values; Node exposes a best-effort
            // string copy instead (lossy on Unix).
            for (key, value) in std::env::vars_os() {
                let key = key.to_string_lossy().into_owned();
                let value = value.to_string_lossy().into_owned();
                map.insert(key, serde_json::Value::String(value));
            }
            serde_json::Value::Object(map).to_string()
        });
        set_fn!("execPath", || -> String {
            std::env::current_exe()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "kiana".to_owned())
        });
        set_fn!("homedir", || -> String { sys::home_dir() });
        set_fn!("tmpdir", || -> String {
            std::env::temp_dir().to_string_lossy().into_owned()
        });
        set_fn!("hostname", || -> String { sys::hostname() });
        set_fn!("osRelease", || -> String { sys::os_release() });
        set_fn!("cpuCount", || -> f64 { sys::cpu_count() });
        set_fn!("totalMemory", || -> f64 { sys::total_memory() });
        set_fn!("freeMemory", || -> f64 { sys::free_memory() });
        set_fn!("cpuModel", || -> String { sys::cpu_model() });

        // ---- 输出 -----------------------------------------------------------
        set_fn!("write", |fd: f64, text: String| {
            sys::write_fd(fd as i32, &text);
        });
        set_fn!("log", |level: String, text: String| {
            sys::write_log(&level, &text);
        });

        // ---- 时间 / 随机 / 摘要 --------------------------------------------
        set_fn!("now", || -> f64 { sys::monotonic_ms() });
        set_fn!("nowEpochMs", || -> f64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64() * 1000.0)
                .unwrap_or(0.0)
        });
        set_fn!("randomBytes", |count: f64| -> String {
            let mut bytes = vec![0u8; count.max(0.0) as usize];
            if getrandom::fill(&mut bytes).is_err() {
                // getrandom 不可用时退回时间/计数器（与 Node 的 `randomFillSync` 不同，
                // 但这里只在系统 RNG 完全不可用时发生）。
                let mut seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_nanos() as u64)
                    .unwrap_or(1);
                for byte in &mut bytes {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    *byte = seed as u8;
                }
            }
            b64_encode(&bytes)
        });
        set_fn!("randomUuid", || -> String {
            uuid::Uuid::new_v4().to_string()
        });
        set_fn!("sha256", |data: String| -> String {
            use sha2::Digest as _;
            let bytes = b64_decode(&data);
            let digest = sha2::Sha256::digest(&bytes);
            b64_encode(&digest)
        });
        set_fn!("sha512", |data: String| -> String {
            use sha2::Digest as _;
            let bytes = b64_decode(&data);
            let digest = sha2::Sha512::digest(&bytes);
            b64_encode(&digest)
        });
        set_fn!("hash", |ctx: Ctx<'_>,
                         algorithm: String,
                         data: String|
         -> Result<String, rquickjs::Error> {
            use sha2::Digest as _;
            let bytes = b64_decode(&data);
            let digest: Vec<u8> = match algorithm.to_ascii_uppercase().as_str() {
                "SHA-256" | "SHA256" => sha2::Sha256::digest(&bytes).to_vec(),
                "SHA-384" | "SHA384" => sha2::Sha384::digest(&bytes).to_vec(),
                "SHA-512" | "SHA512" => sha2::Sha512::digest(&bytes).to_vec(),
                _ => {
                    return Err(rquickjs::Exception::throw_message(
                        &ctx,
                        &format!("Unsupported digest algorithm: {algorithm}"),
                    ));
                }
            };
            Ok(b64_encode(&digest))
        });

        // ---- zlib -----------------------------------------------------------
        set_fn!("zlib", |ctx: Ctx<'_>,
                         operation: String,
                         data: String|
         -> Result<String, rquickjs::Error> {
            let input = b64_decode(&data);
            let output = sys::zlib_op(&operation, &input).map_err(|error| {
                rquickjs::Exception::throw_message(&ctx, &format!("{operation} failed: {error}"))
            })?;
            Ok(b64_encode(&output))
        });

        // ---- 进程信号 -------------------------------------------------------
        set_fn!("kill", |ctx: Ctx<'_>,
                         pid: f64,
                         signal: Opt<String>|
         -> Result<bool, rquickjs::Error> {
            let signal = signal.0.unwrap_or_else(|| "SIGTERM".to_owned());
            match child::send_signal(pid as i32, &signal) {
                Ok(()) => Ok(true),
                Err((errno, code)) => Err(error::throw_node_error(
                    &ctx,
                    error::node_error(
                        "Error",
                        format!("kill {code}"),
                        Some(code),
                        Some(errno),
                        Some("kill"),
                        None,
                    ),
                )),
            }
        });

        // ---- 模块解析 / 读取（loader 与 cjs.js 的跨单元契约）----------------
        // `cjs.js` 的同步 require 桥：失败时 `code` 必须是 `MODULE_NOT_FOUND`，消息与
        // Require stack 由 `paths::resolve_specifier` 生成，`cjs.js` 只按 code 分类再抛。
        set_fn!("resolve", |ctx: Ctx<'_>,
                            from: String,
                            spec: String|
         -> Result<String, rquickjs::Error> {
            match paths::resolve_specifier(&from, &spec) {
                Ok(path) => Ok(path),
                Err(message) => Err(error::throw_node_error(
                    &ctx,
                    error::node_error("Error", message, Some("MODULE_NOT_FOUND"), None, None, None),
                )),
            }
        });
        // `Option<String>` 的 `None` 在 JS 侧是 `undefined`（rquickjs 对 Option 的转换），
        // `cjs.js` 据此判定“这是 ESM 或宿主拒绝按 CJS 读取”，随后抛 `ERR_REQUIRE_ESM`。
        set_fn!("read", |ctx: Ctx<'_>,
                         path: String|
         -> Result<Option<String>, rquickjs::Error> {
            match fs::read_module_source(&path) {
                Ok(Some(source)) => Ok(Some(source)),
                Ok(None) => Ok(None),
                Err(error_value) => Err(error::throw_node_error(&ctx, error_value)),
            }
        });
        set_fn!("urlParse", |ctx: Ctx<'_>,
                             input: String,
                             base: Opt<String>|
         -> Result<String, rquickjs::Error> {
            paths::url_parse(&input, base.0.as_deref())
                .map_err(|message| rquickjs::Exception::throw_type(&ctx, &message))
        });
        set_fn!(
            "fileURLToPath",
            |ctx: Ctx<'_>, url: String| -> Result<String, rquickjs::Error> {
                paths::file_url_to_path(&url).map_err(|value| error::throw_node_error(&ctx, value))
            }
        );
        set_fn!("pathToFileURL", |path: String| -> String {
            paths::path_to_file_url(&path)
        });
        set_fn!("pathToFileURLObject", |path: String| -> String {
            paths::path_to_file_url(&path)
        });

        // ---- fs（同步；`js/node/fs.js` 的薄包装）-----------------------------
        set_fn!("fsSync", |ctx: Ctx<'_>,
                           operation: String,
                           args: String|
         -> Result<String, rquickjs::Error> {
            match fs::fs_sync(&operation, &args) {
                Ok(value) => Ok(value.to_string()),
                Err(error_value) => Err(error::throw_node_error(&ctx, error_value)),
            }
        });

        // ---- child_process（同步）-------------------------------------------
        set_fn!("spawnSync", |ctx: Ctx<'_>,
                              args: String|
         -> Result<String, rquickjs::Error> {
            match child::spawn_sync(&args) {
                Ok(value) => Ok(value.to_string()),
                Err(error_value) => Err(error::throw_node_error(&ctx, error_value)),
            }
        });

        // ---- net（同一线程的写端注册表）--------------------------------------
        {
            let state = Rc::clone(&net_state);
            host.set(
                "netWrite",
                Function::new(ctx.clone(), move |socket_id: f64, data: String| -> bool {
                    net::write_socket(&state, socket_id as u64, b64_decode(&data))
                })?,
            )?;
        }
        {
            let state = Rc::clone(&net_state);
            host.set(
                "netEnd",
                Function::new(ctx.clone(), move |socket_id: f64| {
                    net::end_socket(&state, socket_id as u64);
                })?,
            )?;
        }
        {
            let state = Rc::clone(&net_state);
            host.set(
                "netDestroy",
                Function::new(ctx.clone(), move |socket_id: f64| {
                    net::destroy_socket(&state, socket_id as u64);
                })?,
            )?;
        }
        {
            let state = Rc::clone(&net_state);
            host.set(
                "netClose",
                Function::new(ctx.clone(), move |handle: f64| {
                    net::close_server(&state, handle as u64);
                })?,
            )?;
        }

        // ---- fetch ----------------------------------------------------------
        {
            let state = Rc::clone(&fetch_state);
            host.set(
                "fetchAbort",
                Function::new(ctx.clone(), move |id: f64| {
                    fetch::abort(&state, id as u64);
                })?,
            )?;
        }

        // ---- fs.watch 的 close ----------------------------------------------
        {
            let state = Rc::clone(&fs_state);
            host.set(
                "watchClose",
                Function::new(ctx.clone(), move |handle: f64| {
                    fs::close_watch(&state, handle as u64);
                })?,
            )?;
        }

        // ---- child.kill 的注册表 --------------------------------------------
        {
            let state = Rc::clone(&child_state);
            host.set(
                "spawnKill",
                Function::new(
                    ctx.clone(),
                    move |handle: f64, signal: Opt<String>| -> bool {
                        child::kill_child(&state, handle as u64, signal.0.as_deref())
                    },
                )?,
            )?;
        }
        {
            let state = Rc::clone(&child_state);
            host.set(
                "childWrite",
                Function::new(ctx.clone(), move |handle: f64, data: String| -> bool {
                    child::write_child(&state, handle as u64, b64_decode(&data))
                })?,
            )?;
        }
        {
            let state = Rc::clone(&child_state);
            host.set(
                "childEnd",
                Function::new(ctx.clone(), move |handle: f64| -> bool {
                    child::end_child_stdin(&state, handle as u64)
                })?,
            )?;
        }

        // ---- Intl 之外的小工具 ----------------------------------------------
        set_fn!("decodeUtf8", |data: String| -> String {
            String::from_utf8_lossy(&b64_decode(&data)).into_owned()
        });
        set_fn!("encodeUtf8", |text: String| -> String {
            b64_encode(text.as_bytes())
        });

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 共享工具
// ---------------------------------------------------------------------------

/// 标准 base64（带 `=` 填充）；JS 侧配对函数是 `__kiana_node_internal.base64Encode`。
pub(crate) fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 标准 base64 解码；非法输入返回空 `Vec`（`unwrap_or_default`，不报错）。
///
/// 这是静默降级：所有 `__kiana_host.*` 的二进制参数都经这里解码，坏数据会变成空字节序列
/// 继续往下走（例如 `sha256` 会得到空输入的摘要），调用方不能依赖它检测格式错误。
pub(crate) fn b64_decode(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}

/// 从 `HostCall` 的 payload 里取一个函数字段。
///
/// payload 不是对象时抛 JS `TypeError`（固定文案 `host payload must be an object`）；
/// 字段缺失或不是函数时由 rquickjs 的 `Function` 转换错误决定文案。必须在 `spawn_local`
/// 之前调用——返回的 `Function<'js>` 不能跨出当前 `with_js` 事务。
fn payload_function<'js>(call: &HostCall<'js>, name: &str) -> EngineResult<Function<'js>> {
    let payload = call.payload().as_object().ok_or_else(|| {
        rquickjs::Exception::throw_type(call.ctx(), "host payload must be an object")
    })?;
    payload.get::<_, Function>(name)
}

// ---------------------------------------------------------------------------
// sys：home/tmp/CPU/输出/zlib
// ---------------------------------------------------------------------------

mod sys {
    // 平台查询与小工具。全部同步、可能阻塞（读 `/proc`、读文件系统），在 JS 调用栈里直接执行。
    // 约定：只读查询失败一律降级为默认值（`0`/`"unknown"`/空串/"localhost"），不抛错；写
    // stdout/stderr 的错误被忽略。本模块唯一的 unsafe 是 `os_release` 里的 `uname` 块，
    // 安全前提见该函数内注释。

    use std::io::Write as _;
    use std::time::Instant;

    use base64::Engine as _;

    /// `os.homedir()`：Unix 读 `HOME`，Windows 读 `USERPROFILE` 再退 `HOMEDRIVE`+`HOMEPATH`，
    /// 都取不到时返回当前目录，再失败返回 `/`。空字符串视为未设置。
    pub fn home_dir() -> String {
        #[cfg(unix)]
        {
            if let Ok(home) = std::env::var("HOME")
                && !home.is_empty()
            {
                return home;
            }
        }
        #[cfg(windows)]
        {
            if let Ok(home) = std::env::var("USERPROFILE")
                && !home.is_empty()
            {
                return home;
            }
            if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
                return format!("{drive}{path}");
            }
        }
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_owned())
    }

    /// `os.hostname()`：Unix 优先读 `/proc/sys/kernel/hostname`，再退环境变量
    /// `HOSTNAME`/`COMPUTERNAME`，最后返回 `"localhost"`。
    pub fn hostname() -> String {
        #[cfg(unix)]
        {
            if let Ok(mut host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
                let trimmed = host.trim_end().to_owned();
                if !trimmed.is_empty() {
                    return trimmed;
                }
                host.clear();
            }
        }
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "localhost".to_owned())
    }

    /// `os.release()`：Unix 优先读 `/proc/sys/kernel/osrelease`，读不到时用 `uname(2)`；
    /// 非 Unix 平台返回空串。
    pub fn os_release() -> String {
        #[cfg(unix)]
        {
            if let Ok(mut release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
                let trimmed = release.trim_end().to_owned();
                if !trimmed.is_empty() {
                    return trimmed;
                }
                release.clear();
            }
            // SAFETY: `utsname` 是纯 POD 的 C 结构体，全零是合法初值；成功返回时 `uname`
            // 会填写它的全部字段。
            let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
            // SAFETY: `uname` fills the caller-provided struct.
            if unsafe { libc::uname(&mut uts) } == 0 {
                // SAFETY: `uts.release` 是 `uname` 填好的 NUL 结尾 C 字符串，`uts` 在本作用域
                // 仍然存活，`CStr` 不越过它。
                let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) };
                return release.to_string_lossy().into_owned();
            }
        }
        String::new()
    }

    /// `os.freemem()`：Linux 用 `/proc/meminfo` 的 `MemAvailable`，其它平台 0。
    pub fn free_memory() -> f64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                for line in meminfo.lines() {
                    if let Some(rest) = line.strip_prefix("MemAvailable:")
                        && let Some(value) = rest.split_whitespace().next()
                        && let Ok(kib) = value.parse::<f64>()
                    {
                        return kib * 1024.0;
                    }
                }
            }
        }
        0.0
    }

    /// `os.cpus()[i].model`：Linux 读 `/proc/cpuinfo`，其它平台 `"unknown"`。
    pub fn cpu_model() -> String {
        #[cfg(target_os = "linux")]
        {
            if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
                for line in cpuinfo.lines() {
                    if let Some((key, value)) = line.split_once(':')
                        && key.trim() == "model name"
                    {
                        return value.trim().to_owned();
                    }
                }
            }
        }
        "unknown".to_owned()
    }

    /// `os.cpus().length`：`available_parallelism` 失败时按 1 个 CPU 返回。
    pub fn cpu_count() -> f64 {
        std::thread::available_parallelism()
            .map(|count| count.get() as f64)
            .unwrap_or(1.0)
    }

    /// `os.totalmem()`：Linux 读 `/proc/meminfo` 的 `MemTotal`（kB 换算成字节），其它平台 0。
    pub fn total_memory() -> f64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                for line in meminfo.lines() {
                    if let Some(rest) = line.strip_prefix("MemTotal:")
                        && let Some(value) = rest.split_whitespace().next()
                        && let Ok(kib) = value.parse::<f64>()
                    {
                        return kib * 1024.0;
                    }
                }
            }
        }
        0.0
    }

    /// `__kiana_host.write(fd, text)`：fd 1 -> stdout，fd 2 -> stderr。
    ///
    /// 其它 fd（3 及以上）同样写 stdout。每次写后立即 flush；写入与 flush 的错误都被忽略，
    /// 调用方无法知道写失败。
    pub fn write_fd(fd: i32, text: &str) {
        let result = match fd {
            2 => std::io::stderr().write_all(text.as_bytes()),
            _ => std::io::stdout().write_all(text.as_bytes()),
        };
        let _ = result.and_then(|()| match fd {
            2 => std::io::stderr().flush(),
            _ => std::io::stdout().flush(),
        });
    }

    /// `console.*`：TUI 模式下全部走 stderr（设计 §7.4 第 8 条）。
    ///
    /// `level` 参数当前没有参与输出（函数末尾 `let _ = level`）；写 stderr 的错误同样被忽略。
    pub fn write_log(level: &str, text: &str) {
        let mut stderr = std::io::stderr();
        let _ = writeln!(stderr, "{text}");
        let _ = stderr.flush();
        let _ = level;
    }

    /// 单调时钟毫秒（`performance.now()`）。
    ///
    /// 基准是进程内**首次调用**时创建的 `Instant`（`OnceLock`），所以返回值从接近 0 开始；
    /// 它不是 Unix 时间，也不能跨进程比较。
    pub fn monotonic_ms() -> f64 {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_secs_f64() * 1000.0
    }

    /// `node:zlib` 的同步后端：`gunzip`/`gzip`/`inflate`/`deflate`/`inflate-raw`/
    /// `deflate-raw`/`base64`。
    ///
    /// 压缩错误以 `String` 返回（调用点把它转成 JS 异常，文案是 flate2 的英文描述）。
    /// `base64` 是解码操作：输入字节先按 UTF-8 解释（非 UTF-8 会变成空串，于是得到空 `Vec`），
    /// 合法 UTF-8 但不是合法 base64 时返回解码错误字符串。
    pub fn zlib_op(operation: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        use std::io::Read as _;
        match operation {
            "gunzip" => {
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(input)
                    .read_to_end(&mut out)
                    .map_err(|error| error.to_string())?;
                Ok(out)
            }
            "gzip" => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder
                    .write_all(input)
                    .map_err(|error| error.to_string())?;
                encoder.finish().map_err(|error| error.to_string())
            }
            "inflate" => {
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(input)
                    .read_to_end(&mut out)
                    .map_err(|error| error.to_string())?;
                Ok(out)
            }
            "deflate" => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder
                    .write_all(input)
                    .map_err(|error| error.to_string())?;
                encoder.finish().map_err(|error| error.to_string())
            }
            "inflate-raw" => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(input)
                    .read_to_end(&mut out)
                    .map_err(|error| error.to_string())?;
                Ok(out)
            }
            "deflate-raw" => {
                let mut encoder =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                encoder
                    .write_all(input)
                    .map_err(|error| error.to_string())?;
                encoder.finish().map_err(|error| error.to_string())
            }
            "base64" => Ok(base64::engine::general_purpose::STANDARD
                .decode(std::str::from_utf8(input).unwrap_or_default())
                .map_err(|error| error.to_string())?),
            other => Err(format!("unknown zlib operation: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// error：Node 形状的异常
// ---------------------------------------------------------------------------

pub(crate) mod error {
    // Node 形状错误的单一来源。同一个 JSON 对象走两条路：
    //
    // - 同步原语：`throw_node_error` 用 `make_error_object` 直接物化成 Error 实例并 `ctx.throw`；
    // - 异步 kind：`HostValue::Json(node_error(...))` 投给 promise，JS 侧
    //   `__kiana_node_internal.promise` 看到 `__kianaError` 标记后调用 `nodeError` 物化。
    //
    // 两条路径必须字段一致（`code`/`errno`/`syscall`/`path`/`dest` 等），否则同步与异步 API
    // 抛出的错误形状会分叉；`js/prelude_node.js` 的 `nodeError` 是异步侧的字段清单。
    use rquickjs::{Ctx, Function, Object, Value};

    /// 构造 Node 错误对象（`Error`/`TypeError`/`RangeError`…）并抛出。
    ///
    /// 物化失败时返回 `make_error_object` 的错误（此时没有挂起 JS 异常，调用方看到的是
    /// rquickjs 层的失败）；成功时用 `ctx.throw` 挂起异常并返回错误信号。
    pub fn throw_node_error(ctx: &Ctx<'_>, json: serde_json::Value) -> rquickjs::Error {
        match make_error_object(ctx, &json) {
            Ok(value) => ctx.throw(value),
            Err(error) => error,
        }
    }

    /// Node 错误值的 JSON 形状。
    ///
    /// `__kianaError: true` 是给 JS 侧识别“这是一条宿主错误”的标记；`name` 决定同步路径用哪个
    /// 构造器（`TypeError`/`RangeError`…），其余可选字段原样成为错误对象的同名属性。
    /// 未提供的字段不会出现在 JSON 里，JS 侧也就不会设置对应属性（`undefined` 与“没有该属性”
    /// 在 Node 的检查里通常等价）。
    pub fn node_error(
        name: &str,
        message: String,
        code: Option<&str>,
        errno: Option<i32>,
        syscall: Option<&str>,
        path: Option<&str>,
    ) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("__kianaError".into(), serde_json::Value::Bool(true));
        map.insert("name".into(), serde_json::Value::String(name.to_owned()));
        map.insert("message".into(), serde_json::Value::String(message));
        if let Some(code) = code {
            map.insert("code".into(), serde_json::Value::String(code.to_owned()));
        }
        if let Some(errno) = errno {
            map.insert("errno".into(), serde_json::Value::Number(errno.into()));
        }
        if let Some(syscall) = syscall {
            map.insert(
                "syscall".into(),
                serde_json::Value::String(syscall.to_owned()),
            );
        }
        if let Some(path) = path {
            map.insert("path".into(), serde_json::Value::String(path.to_owned()));
        }
        serde_json::Value::Object(map)
    }

    /// 把 [`node_error`] 的形状物化成 JS Error（带 code/errno/syscall/path）。
    ///
    /// 细节与边界：
    /// - `name` 是 `TypeError`/`RangeError`/`SyntaxError`/`ReferenceError`/`URIError`/`EvalError`
    ///   时用对应构造器；`AbortError` 与其它未知名字用 `Error` 构造器再补 `name`；
    /// - `code` 为 `ABORT_ERR` 时强制 `name = "AbortError"`；
    /// - `message`/`name`/`__kianaError` 不写进属性循环（前两个已由构造器/上面的分支处理）；
    /// - `path`/`dest`/`src`/`host`/`address`/`port` 与其它未知键：字符串或 i64 才复制，
    ///   其它 JSON 类型被忽略；未知键的属性写入失败也被忽略（`let _`），已知键则向上传播。
    pub fn make_error_object<'js>(
        ctx: &Ctx<'js>,
        json: &serde_json::Value,
    ) -> rquickjs::Result<Value<'js>> {
        let name = json
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("Error");
        let message = json
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let ctor: Function<'js> = match name {
            "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" | "URIError"
            | "EvalError" => ctx
                .globals()
                .get::<_, Option<Function<'js>>>(name)?
                .unwrap_or(ctx.globals().get("Error")?),
            "AbortError" => ctx.globals().get("Error")?,
            _ => ctx.globals().get("Error")?,
        };
        let object: Object<'js> = ctor.call((message.to_owned(),))?;
        if name != "Error"
            && name != "TypeError"
            && name != "RangeError"
            && name != "SyntaxError"
            && name != "ReferenceError"
            && name != "URIError"
            && name != "EvalError"
        {
            object.set("name", name)?;
        }
        if let Some(map) = json.as_object() {
            for (key, value) in map {
                match key.as_str() {
                    "__kianaError" | "message" | "name" => {}
                    "code" => {
                        if let Some(text) = value.as_str() {
                            object.set("code", text)?;
                            if text == "ABORT_ERR" {
                                object.set("name", "AbortError")?;
                            }
                        }
                    }
                    "errno" => {
                        if let Some(number) = value.as_i64() {
                            object.set("errno", number)?;
                        }
                    }
                    "syscall" | "path" | "dest" | "src" | "host" | "address" | "port" => {
                        if let Some(text) = value.as_str() {
                            object.set(key.as_str(), text)?;
                        } else if let Some(number) = value.as_i64() {
                            object.set(key.as_str(), number)?;
                        }
                    }
                    _ => {
                        if let Some(text) = value.as_str() {
                            let _ = object.set(key.as_str(), text);
                        } else if let Some(number) = value.as_i64() {
                            let _ = object.set(key.as_str(), number);
                        }
                    }
                }
            }
        }
        Ok(object.into_value())
    }

    /// `std::io::Error` → Node 错误 JSON（`ENOENT: no such file or directory, open '/x'`）。
    ///
    /// 消息固定为 `{code}: {description}, {syscall} '{path}'`；`name` 恒为 `"Error"`。
    /// `syscall` 由调用方按 Node 的选择传入（open/read/write/scandir/unlink/…），它同时进入
    /// 消息与 `syscall` 属性。
    pub fn io_error(syscall: &str, path: &str, error: &std::io::Error) -> serde_json::Value {
        let (errno, code, description) = errno_info(error);
        let message = format!("{code}: {description}, {syscall} '{path}'");
        node_error(
            "Error",
            message,
            Some(code),
            Some(errno),
            Some(syscall),
            Some(path),
        )
    }

    /// `std::io::Error` → `(errno, code, description)`，errno 采用 Node/libuv 的**负值**约定。
    ///
    /// - Unix：`raw_os_error()` 命中 `ERRNO_TABLE` 时返回 `-raw` 与该平台的 libc 常量；
    /// - 否则（所有非 Unix 平台，以及表中没有的 errno）：按 `ErrorKind` 映射到硬编码表，
    ///   未知 kind 返回 `(-1, "UNKNOWN", "unknown error")`。
    ///
    /// 非 Unix 分支的数值是 POSIX 的常见值，不是从操作系统读到的。
    pub fn errno_info(error: &std::io::Error) -> (i32, &'static str, &'static str) {
        #[cfg(unix)]
        {
            if let Some(raw) = error.raw_os_error()
                && let Some(entry) = ERRNO_TABLE.iter().find(|entry| entry.0 == raw)
            {
                // Node/libuv 在 Unix 上把 errno 取负后暴露（`{ errno: -2, code: 'ENOENT' }`）。
                return (-raw, entry.1, entry.2);
            }
        }
        match error.kind() {
            std::io::ErrorKind::NotFound => (-2, "ENOENT", "no such file or directory"),
            std::io::ErrorKind::PermissionDenied => (-13, "EACCES", "permission denied"),
            std::io::ErrorKind::AlreadyExists => (-17, "EEXIST", "file already exists"),
            std::io::ErrorKind::InvalidInput => (-22, "EINVAL", "invalid argument"),
            std::io::ErrorKind::BrokenPipe => (-32, "EPIPE", "broken pipe"),
            std::io::ErrorKind::TimedOut => (-110, "ETIMEDOUT", "connection timed out"),
            std::io::ErrorKind::ConnectionRefused => (-111, "ECONNREFUSED", "connection refused"),
            std::io::ErrorKind::ConnectionReset => (-104, "ECONNRESET", "connection reset by peer"),
            std::io::ErrorKind::AddrInUse => (-98, "EADDRINUSE", "address already in use"),
            std::io::ErrorKind::AddrNotAvailable => (-99, "EADDRNOTAVAIL", "address not available"),
            std::io::ErrorKind::NotADirectory => (-20, "ENOTDIR", "not a directory"),
            std::io::ErrorKind::IsADirectory => (-21, "EISDIR", "illegal operation on a directory"),
            std::io::ErrorKind::DirectoryNotEmpty => (-39, "ENOTEMPTY", "directory not empty"),
            _ => (-1, "UNKNOWN", "unknown error"),
        }
    }

    /// Unix errno → Node 的 `(code, description)` 对照表（按线性查找，表很小）。
    ///
    /// 只覆盖常见错误；`libc::E*` 保证数值与目标平台一致。描述文本是 Node 风格的英文短语，
    /// 测试断言的只是 `code`/`errno`/`syscall`/`path` 组合，不断言这里的英文。
    #[cfg(unix)]
    const ERRNO_TABLE: &[(i32, &str, &str)] = &[
        (libc::E2BIG, "E2BIG", "argument list too long"),
        (libc::EACCES, "EACCES", "permission denied"),
        (libc::EADDRINUSE, "EADDRINUSE", "address already in use"),
        (
            libc::EADDRNOTAVAIL,
            "EADDRNOTAVAIL",
            "address not available",
        ),
        (libc::EBADF, "EBADF", "bad file descriptor"),
        (libc::EBUSY, "EBUSY", "resource busy or locked"),
        (
            libc::ECONNABORTED,
            "ECONNABORTED",
            "software caused connection abort",
        ),
        (libc::ECONNREFUSED, "ECONNREFUSED", "connection refused"),
        (libc::ECONNRESET, "ECONNRESET", "connection reset by peer"),
        (libc::EEXIST, "EEXIST", "file already exists"),
        (libc::EISDIR, "EISDIR", "illegal operation on a directory"),
        (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
        (libc::EMFILE, "EMFILE", "too many open files"),
        (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
        (libc::ENOENT, "ENOENT", "no such file or directory"),
        (libc::ENOMEM, "ENOMEM", "not enough memory"),
        (libc::ENOSPC, "ENOSPC", "no space left on device"),
        (libc::ENOTDIR, "ENOTDIR", "not a directory"),
        (libc::ENOTEMPTY, "ENOTEMPTY", "directory not empty"),
        (libc::EPERM, "EPERM", "operation not permitted"),
        (libc::EPIPE, "EPIPE", "broken pipe"),
        (libc::EROFS, "EROFS", "read-only file system"),
        (libc::ESPIPE, "ESPIPE", "invalid seek"),
        (libc::ESRCH, "ESRCH", "no such process"),
        (libc::ETIMEDOUT, "ETIMEDOUT", "connection timed out"),
        (libc::EINVAL, "EINVAL", "invalid argument"),
        (libc::ENOSYS, "ENOSYS", "function not implemented"),
        (libc::ENOTSUP, "ENOTSUP", "operation not supported"),
        (libc::EAGAIN, "EAGAIN", "resource temporarily unavailable"),
        (libc::EINTR, "EINTR", "interrupted system call"),
        (libc::EIO, "EIO", "i/o error"),
    ];
}

// ---------------------------------------------------------------------------
// node:* ESM 包装源
// ---------------------------------------------------------------------------

/// 每个内建模块的 ESM 包装源（`import { readFileSync } from "node:fs"` 需要具名导出）。
///
/// 源文本只生成一次（`OnceLock`），以 `&'static str` 注册进引擎的模块源表。
///
/// 生成的包装形如：
///
/// ```text
/// const m = globalThis.__kiana_node_shims["node:fs"];
/// export default m;
/// export const readFileSync = m["readFileSync"];
/// ```
///
/// 具名导出是模块求值时读取一次的 `const` 绑定（`export const x = m["x"]`），所以包装模块
/// 求值之后再替换 shim 成员不会同步到已声明的导出上；默认导出持有对象引用，能看到成员变化。
/// 表里的 `"default"` 条目被跳过（`export default m` 已经覆盖），否则会生成非法的
/// `export const default = …`。
fn node_module_sources() -> &'static [(&'static str, &'static str)] {
    use std::sync::OnceLock;
    static SOURCES: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let mut sources = Vec::new();
        for (module, exports) in NODE_MODULE_EXPORTS {
            let key: String = format!("node:{module}");
            let mut source = String::new();
            source.push_str("const m = globalThis.__kiana_node_shims[");
            source.push_str(&js_string_literal(&key));
            source.push_str("];\nexport default m;\n");
            for name in *exports {
                if *name == "default" {
                    continue;
                }
                source.push_str("export const ");
                source.push_str(name);
                source.push_str(" = m[");
                source.push_str(&js_string_literal(name));
                source.push_str("];\n");
            }
            let leaked_source: &'static str = Box::leak(source.into_boxed_str());
            let leaked_key: &'static str = Box::leak(key.into_boxed_str());
            sources.push((leaked_key, leaked_source));
        }
        sources
    })
}

/// 包装源用的最小字符串转义（反斜杠、双引号、换行、回车）。
///
/// 只被 `node_module_sources` 使用，输入是 `node:<模块名>` 与导出名（ASCII 标识符），
/// 因此不做 Unicode/控制字符转义。
fn js_string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// 把 `node:*` 的 ESM 包装注册进引擎模块源表（loader 在 `Module::declare` 时取用）。
///
/// 包装在**运行时**从 `globalThis.__kiana_node_shims["node:<name>"]` 取值，所以 shim 对象缺
/// 某个名字时对应具名导出是 `undefined`，加载阶段不会报错。`NODE_MODULE_EXPORTS` 与
/// `js/node/*.js` 的对象因此必须同步修改；`tests/node_shims.rs` 的
/// `node_esm_named_exports_are_defined_on_the_shim_objects` 会逐个断言这些导出非 `undefined`。
///
/// 源文本需要 `&'static str`（`JsEngine::register_module_source` 的签名），生成时用
/// `Box::leak`；`node_module_sources` 的 `OnceLock` 保证整个进程只生成并泄漏一次。
pub fn register_node_module_sources(engine: &JsEngine) {
    for (name, source) in node_module_sources() {
        engine.register_module_source(*name, source);
    }
}

/// 内建模块名（`node:<name>`）与具名导出表。
///
/// 这张表同时约束运行期的 `node:*` ESM 包装（见 `node_module_sources`）与 `js/node/*.js`
/// 对象成员：名字写错不会在加载期报错，只会让对应导入得到 `undefined`。新增具名导出时必须
/// 同时改 shim 对象与本表；`tests/node_shims.rs` 的
/// `node_esm_named_exports_are_defined_on_the_shim_objects` 会遍历这里生成的
/// `export const` 行并断言 shim 上同名属性非 `undefined`。
const NODE_MODULE_EXPORTS: &[(&str, &[&str])] = &[
    (
        "assert",
        &[
            "ok",
            "equal",
            "notEqual",
            "strictEqual",
            "notStrictEqual",
            "deepEqual",
            "deepStrictEqual",
            "notDeepEqual",
            "throws",
            "doesNotThrow",
            "rejects",
            "fail",
            "AssertionError",
            "strict",
        ],
    ),
    (
        "buffer",
        &[
            "Buffer",
            "SlowBuffer",
            "constants",
            "kMaxLength",
            "INSPECT_MAX_BYTES",
            "isUtf8",
            "transcode",
            "atob",
            "btoa",
            "Blob",
            "File",
        ],
    ),
    (
        "child_process",
        &[
            "spawn",
            "spawnSync",
            "exec",
            "execSync",
            "execFile",
            "execFileSync",
            "fork",
        ],
    ),
    (
        "console",
        &[
            "Console", "log", "info", "warn", "error", "debug", "trace", "dir", "assert", "table",
            "time", "timeEnd",
        ],
    ),
    (
        "crypto",
        &[
            "webcrypto",
            "randomUUID",
            "randomBytes",
            "randomFillSync",
            "randomInt",
            "getRandomValues",
            "subtle",
            "createHash",
            "createHmac",
            "timingSafeEqual",
            "getHashes",
            "getCurves",
        ],
    ),
    (
        "events",
        &[
            "EventEmitter",
            "once",
            "on",
            "addAbortListener",
            "getEventListeners",
            "getMaxListeners",
            "setMaxListeners",
            "listenerCount",
            "captureRejections",
            "defaultMaxListeners",
            "errorMonitor",
        ],
    ),
    (
        "fs",
        &[
            "readFileSync",
            "writeFileSync",
            "appendFileSync",
            "existsSync",
            "readdirSync",
            "statSync",
            "lstatSync",
            "accessSync",
            "access",
            "readFile",
            "writeFile",
            "appendFile",
            "mkdtemp",
            "mkdtempSync",
            "unlinkSync",
            "unlink",
            "rmdirSync",
            "rmSync",
            "rm",
            "mkdir",
            "mkdirSync",
            "renameSync",
            "realpathSync",
            "chmodSync",
            "copyFileSync",
            "readlinkSync",
            "symlinkSync",
            "createReadStream",
            "createWriteStream",
            "watch",
            "watchFile",
            "unwatchFile",
            "openSync",
            "closeSync",
            "readSync",
            "writeSync",
            "fstatSync",
            "constants",
            "promises",
            "Dirent",
            "Stats",
            "FSWatcher",
            "readvSync",
            "writevSync",
        ],
    ),
    (
        "fs/promises",
        &[
            "readFile",
            "writeFile",
            "appendFile",
            "readdir",
            "stat",
            "lstat",
            "access",
            "mkdir",
            "rmdir",
            "rm",
            "unlink",
            "rename",
            "realpath",
            "chmod",
            "copyFile",
            "mkdtemp",
            "readlink",
            "symlink",
            "open",
            "constants",
        ],
    ),
    (
        "http",
        &[
            "createServer",
            "Server",
            "IncomingMessage",
            "ServerResponse",
            "request",
            "get",
            "Agent",
            "STATUS_CODES",
            "METHODS",
            "globalAgent",
            "validateHeaderName",
            "validateHeaderValue",
            "setMaxIdleHTTPParsers",
        ],
    ),
    (
        "https",
        &[
            "createServer",
            "Server",
            "request",
            "get",
            "Agent",
            "globalAgent",
        ],
    ),
    (
        "module",
        &[
            "createRequire",
            "builtinModules",
            "isBuiltin",
            "Module",
            "syncBuiltinESMExports",
            "findSourceMap",
            "SourceMap",
            "_resolveFilename",
            "_nodeModulePaths",
            "register",
            "registerHooks",
        ],
    ),
    (
        "net",
        &[
            "createServer",
            "Server",
            "Socket",
            "connect",
            "createConnection",
            "isIP",
            "isIPv4",
            "isIPv6",
            "Stream",
            "BlockList",
            "SocketAddress",
        ],
    ),
    (
        "os",
        &[
            "homedir",
            "tmpdir",
            "platform",
            "arch",
            "type",
            "release",
            "hostname",
            "cpus",
            "totalmem",
            "freemem",
            "uptime",
            "loadavg",
            "networkInterfaces",
            "userInfo",
            "endianness",
            "EOL",
            "constants",
            "devNull",
            "machine",
            "getPriority",
            "setPriority",
        ],
    ),
    (
        "path",
        &[
            "join",
            "resolve",
            "dirname",
            "basename",
            "relative",
            "isAbsolute",
            "normalize",
            "extname",
            "parse",
            "format",
            "toNamespacedPath",
            "sep",
            "delimiter",
            "posix",
            "win32",
        ],
    ),
    (
        "path/posix",
        &[
            "join",
            "resolve",
            "dirname",
            "basename",
            "relative",
            "isAbsolute",
            "normalize",
            "extname",
            "parse",
            "format",
            "sep",
            "delimiter",
        ],
    ),
    (
        "path/win32",
        &[
            "join",
            "resolve",
            "dirname",
            "basename",
            "relative",
            "isAbsolute",
            "normalize",
            "extname",
            "parse",
            "format",
            "sep",
            "delimiter",
        ],
    ),
    (
        "perf_hooks",
        &[
            "performance",
            "PerformanceObserver",
            "PerformanceEntry",
            "PerformanceMark",
            "PerformanceMeasure",
            "constants",
        ],
    ),
    (
        "process",
        &[
            "env",
            "argv",
            "execPath",
            "platform",
            "arch",
            "version",
            "versions",
            "cwd",
            "exit",
            "stdout",
            "stderr",
            "stdin",
            "nextTick",
            "on",
            "once",
            "off",
            "emit",
            "hrtime",
            "uptime",
            "memoryUsage",
            "pid",
            "title",
            "kill",
            "umask",
            "getuid",
            "getgid",
            "geteuid",
            "getegid",
            "chdir",
            "release",
            "features",
            "binding",
        ],
    ),
    (
        "readline",
        &[
            "createInterface",
            "Interface",
            "cursorTo",
            "moveCursor",
            "clearLine",
            "clearScreenDown",
            "emitKeypressEvents",
        ],
    ),
    (
        "stream",
        &[
            "Stream",
            "Readable",
            "Writable",
            "Duplex",
            "Transform",
            "PassThrough",
            "pipeline",
            "finished",
            "addAbortSignal",
            "isReadable",
            "isWritable",
            "ReadableStream",
            "WritableStream",
            "default",
        ],
    ),
    ("stream/promises", &["pipeline", "finished"]),
    (
        "stream/web",
        &[
            "ReadableStream",
            "WritableStream",
            "TransformStream",
            "ByteLengthQueuingStrategy",
            "CountQueuingStrategy",
        ],
    ),
    ("string_decoder", &["StringDecoder"]),
    (
        "timers",
        &[
            "setTimeout",
            "clearTimeout",
            "setInterval",
            "clearInterval",
            "setImmediate",
            "clearImmediate",
            "promises",
        ],
    ),
    (
        "timers/promises",
        &["setTimeout", "setImmediate", "setInterval", "scheduler"],
    ),
    ("tty", &["isatty", "isTTY", "ReadStream", "WriteStream"]),
    (
        "url",
        &[
            "URL",
            "URLSearchParams",
            "fileURLToPath",
            "pathToFileURL",
            "urlToHttpOptions",
            "format",
            "parse",
            "resolve",
            "domainToASCII",
            "domainToUnicode",
        ],
    ),
    (
        "util",
        &[
            "promisify",
            "inspect",
            "format",
            "formatWithOptions",
            "inherits",
            "deprecate",
            "isDeepStrictEqual",
            "types",
            "TextEncoder",
            "TextDecoder",
            "callbackify",
            "debuglog",
            "getSystemErrorName",
            "getSystemErrorMap",
            "parseArgs",
            "stripVTControlCharacters",
            "toUSVString",
            "transferableAbortSignal",
            "aborted",
        ],
    ),
    (
        "util/types",
        &[
            "isPromise",
            "isDate",
            "isRegExp",
            "isTypedArray",
            "isArrayBuffer",
            "isArrayBufferView",
            "isMap",
            "isSet",
            "isWeakMap",
            "isWeakSet",
            "isNativeError",
            "isError",
            "isBoxedPrimitive",
            "isBigIntObject",
            "isSymbolObject",
            "isNumberObject",
            "isStringObject",
            "isBooleanObject",
            "isAsyncFunction",
            "isGeneratorFunction",
            "isGeneratorObject",
            "isProxy",
        ],
    ),
    (
        "zlib",
        &[
            "gunzipSync",
            "gzipSync",
            "inflateSync",
            "deflateSync",
            "unzipSync",
            "inflateRawSync",
            "deflateRawSync",
            "createGunzip",
            "constants",
        ],
    ),
    (
        "tls",
        &[
            "connect",
            "createServer",
            "TLSSocket",
            "createSecureContext",
            "DEFAULT_MIN_VERSION",
            "DEFAULT_MAX_VERSION",
        ],
    ),
];

/// `js/node/*.js` 的安装表（顺序 = 依赖顺序）。
///
/// 每个文件是一段 IIFE：顶层取 `globalThis.__kiana_node_shims` 与
/// `__kiana_node_internal`，末尾写入 `shims.x` 与 `shims["node:x"]`。因此排在前面的模块
/// 的对象必须在后面的模块求值前就绪；`node:*` 的 ESM 包装不在这里，见 `NODE_MODULE_EXPORTS`。
const NODE_SHIMS: &[(&str, &str)] = &[
    ("events", include_str!("../../js/node/events.js")),
    ("stream", include_str!("../../js/node/stream.js")),
    ("util", include_str!("../../js/node/util.js")),
    ("path", include_str!("../../js/node/path.js")),
    ("os", include_str!("../../js/node/os.js")),
    ("url", include_str!("../../js/node/url.js")),
    ("crypto", include_str!("../../js/node/crypto.js")),
    ("zlib", include_str!("../../js/node/zlib.js")),
    ("fs", include_str!("../../js/node/fs.js")),
    ("fs/promises", include_str!("../../js/node/fs_promises.js")),
    (
        "child_process",
        include_str!("../../js/node/child_process.js"),
    ),
    ("net", include_str!("../../js/node/net.js")),
    ("http", include_str!("../../js/node/http.js")),
    ("https", include_str!("../../js/node/https.js")),
    ("readline", include_str!("../../js/node/readline.js")),
    ("tls", include_str!("../../js/node/tls.js")),
    ("module", include_str!("../../js/node/module.js")),
    ("assert", include_str!("../../js/node/assert.js")),
    ("buffer", include_str!("../../js/node/buffer.js")),
    ("process", include_str!("../../js/node/process.js")),
    ("console", include_str!("../../js/node/console.js")),
    ("timers", include_str!("../../js/node/timers.js")),
    ("perf_hooks", include_str!("../../js/node/perf_hooks.js")),
    (
        "string_decoder",
        include_str!("../../js/node/string_decoder.js"),
    ),
    ("tty", include_str!("../../js/node/tty.js")),
];

// 其余模块：fs / child / net / fetch / intl（内联，见文件末尾）。
mod paths {
    // 模块解析与 URL 工具。消费者是四个宿主原语：
    // - `__kiana_host.resolve`（CJS `require` 桥，固定 `ResolutionKind::Require`）；
    // - `__kiana_host.urlParse` / `fileURLToPath`（`node:url` shim）；
    // - `__kiana_host.pathToFileURL` / `pathToFileURLObject`（`node:url` shim）。
    //
    // 解析器按线程寄存：引擎是单线程的（设计 §2.2），同一线程换引擎时用
    // `set_module_resolver` 覆盖。`RESOLVER` 没有清理入口，默认解析器一旦惰性创建就会
    // 留在线程上（它不持有引擎，只是缓存）。

    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::super::resolver::{ModuleResolver, ResolutionKind, ResolverOptions};
    use super::error;

    thread_local! {
        /// 当前线程的解析器（单线程所有权；loader 可用 [`set_module_resolver`] 覆盖）。
        static RESOLVER: RefCell<Option<Rc<ModuleResolver>>> = const { RefCell::new(None) };
    }

    /// 覆盖 `__kiana_host.resolve` 使用的模块解析器（loader 单元的注入点）。
    ///
    /// 引擎是单线程的（设计 §2.2），因此解析器按线程寄存；同一线程换引擎时重新设置即可。
    /// 当前仓库内没有调用方：未设置时 `resolver()` 会惰性创建 `ResolverOptions::default()`
    /// 的解析器（没有虚拟文件表，也没有 `tsconfig` 的 paths）。
    pub fn set_module_resolver(resolver: Rc<ModuleResolver>) {
        RESOLVER.with(|slot| *slot.borrow_mut() = Some(resolver));
    }

    /// 取当前解析器；未设置时创建一个没有虚拟文件表的默认解析器（相对路径与
    /// `node_modules` 仍可解析）。
    fn resolver() -> Rc<ModuleResolver> {
        RESOLVER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(resolver) = slot.as_ref() {
                return Rc::clone(resolver);
            }
            let resolver = Rc::new(ModuleResolver::new(ResolverOptions::default()));
            *slot = Some(Rc::clone(&resolver));
            resolver
        })
    }

    /// Node's CJS directory resolution reads `package.json` ("exports" first,
    /// then "main") before falling back to `index.js`. The module resolver's
    /// directory path returns `index.js` unconditionally, so the host fixes
    /// that up here (loader-host owns this primitive).
    ///
    /// 细节：`exports` 只处理根条目 `"."`（字符串或条件对象），条件按
    /// require → node → default → import 取第一个字符串；`main` 只在没有可用 exports 时使用；
    /// 结果去掉前导 `./` 后必须指向已存在的文件，否则返回 `None` 交回 `ModuleResolver`
    /// （由它退回 `index.js`）。`package.json` 缺失或不是合法 JSON 时同样返回 `None`。
    fn package_directory_entry(directory: &Path) -> Option<String> {
        if !directory.is_dir() {
            return None;
        }
        let manifest = std::fs::read_to_string(directory.join("package.json")).ok()?;
        let json: serde_json::Value = serde_json::from_str(&manifest).ok()?;
        let mut relative: Option<String> = None;
        if let Some(exports) = json.get("exports") {
            let root = match exports {
                serde_json::Value::Object(map) => map.get("."),
                other => Some(other),
            };
            fn pick(value: &serde_json::Value) -> Option<String> {
                match value {
                    serde_json::Value::String(text) => Some(text.clone()),
                    serde_json::Value::Object(map) => {
                        for key in ["require", "node", "default", "import"] {
                            if let Some(found) = map.get(key).and_then(pick) {
                                return Some(found);
                            }
                        }
                        None
                    }
                    _ => None,
                }
            }
            relative = root.and_then(pick);
        }
        if relative.is_none()
            && let Some(serde_json::Value::String(main)) = json.get("main")
        {
            relative = Some(main.clone());
        }
        let relative = relative?;
        let relative = relative.trim_start_matches("./");
        let candidate = directory.join(relative);
        if candidate.is_file() {
            return candidate.to_str().map(str::to_owned);
        }
        None
    }

    /// 判断 specifier 是否指向一个**已存在的目录**。
    ///
    /// 只接受绝对路径与 `./`/`../`/`.`/`..` 开头的相对路径；`node:`/`data:`/`file:` 前缀与
    /// 裸包名一律返回 `None`（交给 `ModuleResolver`）。返回值是目录本身，供
    /// `package_directory_entry` 读取 `package.json`。
    fn directory_specifier(from: &str, spec: &str) -> Option<PathBuf> {
        if spec.is_empty() || spec.starts_with("node:") || spec.starts_with("data:") {
            return None;
        }
        if spec.starts_with("file:") {
            return None;
        }
        let candidate = Path::new(spec);
        if candidate.is_absolute()
            || spec.starts_with("./")
            || spec.starts_with("../")
            || spec == "."
            || spec == ".."
        {
            let joined = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                Path::new(from)
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(candidate)
            };
            return joined.is_dir().then_some(joined);
        }
        None
    }

    /// `__kiana_host.resolve(from, spec)`：jiti/Node 顺序的同步解析。
    ///
    /// 先处理“相对路径 + 目录 + `package.json`”这一 CJS 特例，再交给 `ModuleResolver`
    /// （`ResolutionKind::Require`）。所有失败统一成 Node 的
    /// `Cannot find module '<spec>'\nRequire stack:\n- <from>` 文案，底层原因被丢弃；
    /// `MODULE_NOT_FOUND` 这个 `code` 由调用点（`install_sync_primitives`）补上。
    pub fn resolve_specifier(from: &str, spec: &str) -> Result<String, String> {
        if let Some(directory) = directory_specifier(from, spec)
            && let Some(entry) = package_directory_entry(&directory)
        {
            return Ok(entry);
        }
        let resolver = resolver();
        match resolver.resolve_specifier(from, spec, ResolutionKind::Require) {
            Ok(path) => Ok(path),
            Err(_) => Err(format!(
                "Cannot find module '{}'\nRequire stack:\n- {}",
                spec, from
            )),
        }
    }

    /// `fileURLToPath`：与 Node 的 `url.fileURLToPath` 对齐（`file:`、百分号解码、Windows 盘符）。
    ///
    /// 顺序：解析 URL → 校验 scheme 是 `file` → 校验 host（Windows 支持 UNC；Unix 只允许空或
    /// `localhost`）→ 拒绝编码过的路径分隔符（Unix `%2F`，Windows 还有 `%5C`）→ 校验百分号
    /// 转义合法 → 解码 → Windows 盘符路径转换。
    ///
    /// 错误都是 Node 错误 JSON：`ERR_INVALID_URL`、`ERR_INVALID_URL_SCHEME`、
    /// `ERR_INVALID_FILE_URL_HOST`、`ERR_INVALID_FILE_URL_PATH`；非法转义抛 `URIError`
    /// （没有 `code`）。`tests/node_shims.rs` 的
    /// `file_url_to_path_reports_node_error_shapes` 覆盖这些分支。
    pub fn file_url_to_path(input: &str) -> Result<String, serde_json::Value> {
        let invalid = |message: &str, code: &str, name: &str| {
            error::node_error(name, message.to_owned(), Some(code), None, None, None)
        };
        let url = url::Url::parse(input)
            .map_err(|_| invalid("Invalid URL", "ERR_INVALID_URL", "TypeError"))?;
        if url.scheme() != "file" {
            return Err(invalid(
                "The URL must be of scheme file",
                "ERR_INVALID_URL_SCHEME",
                "TypeError",
            ));
        }
        if let Some(host) = url.host_str()
            && !host.is_empty()
            && host != "localhost"
        {
            // UNC：file://server/share -> \\server\share（Windows）或 //server/share。
            #[cfg(windows)]
            {
                let path = url.path().replace('/', "\\");
                return Ok(format!("\\\\{host}{path}"));
            }
            #[cfg(not(windows))]
            {
                let platform = if cfg!(target_os = "macos") {
                    "darwin"
                } else {
                    std::env::consts::OS
                };
                return Err(invalid(
                    &format!("File URL host must be \"localhost\" or empty on {platform}"),
                    "ERR_INVALID_FILE_URL_HOST",
                    "TypeError",
                ));
            }
        }
        let raw = url.path();
        // Node's fileURLToPath rejects encoded path separators before decoding:
        // POSIX rejects `%2F`; Windows rejects `%2F` and `%5C` as well.
        let bytes = raw.as_bytes();
        let mut index = 0;
        while index + 2 < bytes.len() {
            if bytes[index] == b'%' {
                let second = bytes[index + 1] | 0x20;
                let third = bytes[index + 2] | 0x20;
                #[cfg(windows)]
                let encoded_separator =
                    (second == b'2' && third == b'f') || (second == b'5' && third == b'c');
                #[cfg(not(windows))]
                let encoded_separator = second == b'2' && third == b'f';
                if encoded_separator {
                    #[cfg(windows)]
                    let message = "File URL path must not include encoded \\ or / characters";
                    #[cfg(not(windows))]
                    let message = "File URL path must not include encoded / characters";
                    return Err(invalid(message, "ERR_INVALID_FILE_URL_PATH", "TypeError"));
                }
            }
            index += 1;
        }
        // Node's percent-decoder throws URIError on malformed input.
        let bytes = raw.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let valid = index + 2 < bytes.len()
                    && bytes[index + 1].is_ascii_hexdigit()
                    && bytes[index + 2].is_ascii_hexdigit();
                if !valid {
                    return Err(error::node_error(
                        "URIError",
                        "URI malformed".to_owned(),
                        None,
                        None,
                        None,
                        None,
                    ));
                }
                index += 3;
                continue;
            }
            index += 1;
        }
        let decoded = percent_encoding::percent_decode_str(raw)
            .decode_utf8()
            .map_err(|_| {
                error::node_error(
                    "URIError",
                    "URI malformed".to_owned(),
                    None,
                    None,
                    None,
                    None,
                )
            })?
            .into_owned();
        #[cfg(windows)]
        {
            // file:///C:/dir -> C:\dir
            let bytes = decoded.as_bytes();
            if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
                return Ok(decoded[1..].replace('/', "\\"));
            }
            Ok(decoded.replace('/', "\\"))
        }
        #[cfg(not(windows))]
        {
            Ok(decoded)
        }
    }

    /// `new URL(input, base)` 的 Rust 后端（WHATWG URL，`url` crate）。
    ///
    /// 返回 JSON 字符串：`href`/`protocol`/`username`/`password`/`host`/`hostname`/`port`/
    /// `pathname`/`search`/`hash`/`origin`，由 `js/node/url.js` 组装成 JS 对象。
    /// `host` 带非默认端口时含 `:port`，默认端口时 `port` 是空串。解析失败返回
    /// `Invalid URL: ...` 字符串，调用点把它转成 JS `TypeError`（不是 Node 错误对象）。
    pub fn url_parse(input: &str, base: Option<&str>) -> Result<String, String> {
        let parsed = match base {
            Some(base) => match url::Url::parse(base) {
                Ok(base) => base
                    .join(input)
                    .map_err(|error| format!("Invalid URL: {error}")),
                Err(error) => Err(format!("Invalid URL: {error}")),
            },
            None => url::Url::parse(input).map_err(|error| format!("Invalid URL: {error}")),
        }?;
        Ok(serde_json::json!({
            "href": parsed.as_str(),
            "protocol": parsed.scheme().to_owned() + ":",
            "username": parsed.username().to_owned(),
            "password": parsed.password().unwrap_or("").to_owned(),
            "host": parsed.host_str().map(|host| match parsed.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            }).unwrap_or_default(),
            "hostname": parsed.host_str().unwrap_or("").to_owned(),
            "port": parsed.port().map(|port| port.to_string()).unwrap_or_default(),
            "pathname": parsed.path().to_owned(),
            "search": match parsed.query() {
                Some(query) => format!("?{query}"),
                None => String::new(),
            },
            "hash": match parsed.fragment() {
                Some(fragment) => format!("#{fragment}"),
                None => String::new(),
            },
            "origin": parsed.origin().ascii_serialization(),
        })
        .to_string())
    }

    /// `pathToFileURL`：Node 的 `url.pathToFileURL`。
    ///
    /// 先转成绝对路径（相对路径拼当前目录，不要求存在），做词法 `.`/`..` 归一化，再用
    /// `url` crate 转成 file URL。`Url::from_file_path` 失败时走手写兜底：`file://` 加上对
    /// 整串路径的 `NON_ALPHANUMERIC` 百分号编码——这条兜底会把 `/` 等字符也编码。
    pub fn path_to_file_url(path: &str) -> String {
        let path = Path::new(path);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        };
        // Node 先解析成绝对路径（不要求存在），再 percent-encode。
        let normalized = normalize_absolute(&absolute);
        match url::Url::from_file_path(&normalized) {
            Ok(url) => url.to_string(),
            Err(()) => {
                let text = normalized.to_string_lossy().replace('\\', "/");
                let encoded = percent_encoding::utf8_percent_encode(
                    &text,
                    percent_encoding::NON_ALPHANUMERIC,
                )
                .to_string();
                format!("file://{encoded}")
            }
        }
    }

    /// 词法规范化（`.`/`..`），不要求路径存在。
    pub fn normalize_absolute(path: &Path) -> PathBuf {
        use std::path::Component;
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        out.push("..");
                    }
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
}

/// 覆盖 `__kiana_host.resolve` 使用的模块解析器（loader 单元的注入点）。
///
/// 解析器按线程寄存；完整说明见 `paths::set_module_resolver` 的文档（该模块是私有的）。
pub use paths::set_module_resolver;

/// 测试缝：`LC_ALL > LANG > en-US` 的纯函数（不读进程环境）。
///
/// 参数顺序即优先级：`lc_all` 为空或非法时看 `lang`，两者都不可用返回 `en-US`。只取
/// `ll_CC`/`ll-CC` 前缀，编码（`.UTF-8`）与修饰符（`@euro`）被剥离；`C`/`POSIX` 归一化为
/// `en-US`；`LANGUAGE` 不参与。`tests/intl.rs` 的 locale 解析用例锁定了这些行为。
pub fn resolve_locale_for_test(lc_all: Option<&str>, lang: Option<&str>) -> String {
    intl::resolve_locale(lc_all, lang)
}

// ---------------------------------------------------------------------------
// fs：同步原语、fs.promises 的宿主 kind、fs.watch
// ---------------------------------------------------------------------------

mod fs {
    // 同步文件系统原语、`fs.promises` 的 host kind、`fs.watch`。三层结构：
    //
    // 1. `fs_sync`：一个大的 `match operation`。参数来自 JS 侧的位置数组（JSON），返回 JSON；
    //    错误是 Node 错误 JSON（`error` 模块）。Node 的 syscall 选择与文案差异都在这里。
    // 2. `handle_fs_kind`：把 `fs_sync` 包成 `spawn_local` 任务，让 JS 拿到 Promise。
    //    任务里没有 `.await`，所以同步 IO 会一直占用 LocalSet 线程直到调用返回。
    // 3. `handle_watch_kind`：notify 监听 + 5ms 轮询把事件转给 JS 回调；`close_watch` 置位
    //    停止标志。
    //
    // 本模块不做路径沙箱或权限检查：路径字符串直接交给 `std::fs`，相对路径按进程当前工作
    // 目录解释。二进制内容一律 base64。

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use rquickjs::Function;

    use crate::engine::{EngineResult, HostCall, HostValue};

    use super::error;
    use super::{b64_decode, b64_encode};

    /// `fs.watch` 的句柄表（close 用）。
    #[derive(Default)]
    pub struct FsWatchState {
        closed: RefCell<HashMap<u64, Arc<AtomicBool>>>,
        next: AtomicU64,
    }

    impl FsWatchState {
        fn allocate(&self, flag: Arc<AtomicBool>) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            self.closed.borrow_mut().insert(id, flag);
            id
        }
    }

    fn arg_str(args: &serde_json::Value, index: usize) -> Option<String> {
        args.get(index)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    }

    fn arg_bool(args: &serde_json::Value, index: usize) -> Option<bool> {
        args.get(index).and_then(|value| value.as_bool())
    }

    fn arg_num(args: &serde_json::Value, index: usize) -> Option<f64> {
        args.get(index).and_then(|value| value.as_f64())
    }

    /// `__kiana_host.fsSync(operation, argsJson)`：返回 JSON，错误是 Node 错误 JSON。
    ///
    /// `args` 是位置参数数组（索引与 `js/node/fs.js` 的调用一致），缺项按空串/默认值处理；
    /// `argsJson` 解析失败按 `Null` 处理而不是报错。返回的 `Value` 由调用点序列化给 JS。
    ///
    /// 各分支的主要复杂度是“Node 在失败时用哪个 syscall、消息里带不带 path”：
    /// `open`/`read`/`write`/`scandir`/`unlink`/`rmdir`/`rename`/`realpath`/`chmod`/
    /// `copyfile`/`readlink`/`symlink`/`mkdir`/`lstat`/`rm` 的选择被测试断言锁定，不要随手
    /// 改成统一的 `open`。
    pub fn fs_sync(
        operation: &str,
        args_json: &str,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let args: serde_json::Value =
            serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null);
        match operation {
            "readFile" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                // Node reports a directory as `EISDIR ... read` (not `open`).
                if let Ok(metadata) = std::fs::metadata(&path)
                    && metadata.is_dir()
                {
                    // Node: `EISDIR: illegal operation on a directory, read`
                    // with no path on the message or the error object.
                    return Err(error::node_error(
                        "Error",
                        "EISDIR: illegal operation on a directory, read".to_owned(),
                        Some("EISDIR"),
                        Some(-21),
                        Some("read"),
                        None,
                    ));
                }
                let bytes =
                    std::fs::read(&path).map_err(|error| error::io_error("open", &path, &error))?;
                Ok(serde_json::json!({ "b64": b64_encode(&bytes) }))
            }
            // `writeFile` 与 `appendFile` 共用这条分支：区别只由第 3 个参数 flag 决定
            // （`js/node/fs.js` 里 appendFile 默认传 "a"，writeFile 默认传 "w"）。
            "writeFile" | "appendFile" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let data = b64_decode(&arg_str(&args, 1).unwrap_or_default());
                let flag = arg_str(&args, 2).unwrap_or_else(|| "w".to_owned());
                let mode = arg_num(&args, 3).unwrap_or(0.0) as u32;
                let mut options = std::fs::OpenOptions::new();
                match flag.as_str() {
                    "w" => {
                        options.write(true).create(true).truncate(true);
                    }
                    "wx" | "xw" => {
                        options.write(true).create_new(true);
                    }
                    "w+" => {
                        options.read(true).write(true).create(true).truncate(true);
                    }
                    "wx+" | "xw+" => {
                        options.read(true).write(true).create_new(true);
                    }
                    "a" | "as" | "sa" => {
                        options.append(true).create(true);
                    }
                    "ax" | "xa" => {
                        options.append(true).create_new(true);
                    }
                    "a+" | "as+" | "sa+" => {
                        options.read(true).append(true).create(true);
                    }
                    "ax+" | "xa+" => {
                        options.read(true).append(true).create_new(true);
                    }
                    "r+" => {
                        options.read(true).write(true);
                    }
                    "r" | "rs" | "sr" => {
                        // Write attempts then fail like Node (EBADF, syscall "write").
                        options.read(true);
                    }
                    other => {
                        return Err(error::node_error(
                            "TypeError",
                            format!("The argument 'flag' is invalid. Received {other}"),
                            Some("ERR_INVALID_ARG_VALUE"),
                            None,
                            None,
                            None,
                        ));
                    }
                }
                // `mode` 只在 Unix 且非 0 时设置；0 表示沿用 std 的默认权限（再受 umask 影响）。
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    if mode != 0 {
                        options.mode(mode);
                    }
                }
                #[cfg(not(unix))]
                let _ = mode;
                let mut file = options
                    .open(&path)
                    .map_err(|error| error::io_error("open", &path, &error))?;
                use std::io::Write as _;
                file.write_all(&data)
                    .map_err(|error| error::io_error("write", &path, &error))?;
                Ok(serde_json::Value::Null)
            }
            "exists" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                Ok(serde_json::Value::Bool(Path::new(&path).exists()))
            }
            "readdir" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let with_types = arg_bool(&args, 1).unwrap_or(false);
                let entries = std::fs::read_dir(&path)
                    .map_err(|error| error::io_error("scandir", &path, &error))?;
                let mut out = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|error| error::io_error("scandir", &path, &error))?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if with_types {
                        let kind = entry
                            .file_type()
                            .map(|kind| dirent_type(&kind))
                            .unwrap_or("other");
                        out.push(serde_json::json!({ "name": name, "type": kind }));
                    } else {
                        out.push(serde_json::Value::String(name));
                    }
                }
                Ok(serde_json::Value::Array(out))
            }
            "stat" | "lstat" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let follow = operation == "stat";
                let metadata = if follow {
                    std::fs::metadata(&path)
                } else {
                    std::fs::symlink_metadata(&path)
                }
                .map_err(|error| error::io_error(operation, &path, &error))?;
                Ok(stat_json(&metadata))
            }
            "access" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let mode = arg_num(&args, 1).unwrap_or(0.0) as i32;
                check_access(&path, mode)?;
                Ok(serde_json::Value::Null)
            }
            "mkdtemp" => {
                let prefix = arg_str(&args, 0).unwrap_or_default();
                Ok(serde_json::Value::String(mkdtemp(&prefix)?))
            }
            "unlink" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                std::fs::remove_file(&path)
                    .map_err(|error| error::io_error("unlink", &path, &error))?;
                Ok(serde_json::Value::Null)
            }
            "rmdir" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let recursive = arg_bool(&args, 1).unwrap_or(false);
                let result = if recursive {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_dir(&path)
                };
                result.map_err(|error| error::io_error("rmdir", &path, &error))?;
                Ok(serde_json::Value::Null)
            }
            "rm" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let recursive = arg_bool(&args, 1).unwrap_or(false);
                let force = arg_bool(&args, 2).unwrap_or(false);
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(serde_json::Value::Null);
                    }
                    // Node's rm stats missing paths with the `lstat` syscall.
                    Err(error) => {
                        return Err(error::io_error("lstat", &path, &error));
                    }
                };
                if metadata.is_dir() && !recursive {
                    // Node: SystemError ERR_FS_EISDIR with a positive errno.
                    return Err(error::node_error(
                        "SystemError",
                        format!("Path is a directory: rm returned EISDIR (is a directory) {path}"),
                        Some("ERR_FS_EISDIR"),
                        Some(21),
                        Some("rm"),
                        Some(path.as_str()),
                    ));
                }
                let result = if metadata.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                result.map_err(|error| error::io_error("rm", &path, &error))?;
                Ok(serde_json::Value::Null)
            }
            "mkdir" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let recursive = arg_bool(&args, 1).unwrap_or(false);
                let mode = arg_num(&args, 2).unwrap_or(511.0) as u32;
                // Node's recursive mkdir returns the first directory it created,
                // not the requested path; find the deepest existing ancestor first.
                let first_created = if recursive {
                    first_missing_component(&path)
                } else {
                    None
                };
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(recursive);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    builder.mode(mode);
                }
                #[cfg(not(unix))]
                let _ = mode;
                builder
                    .create(&path)
                    .map_err(|error| error::io_error("mkdir", &path, &error))?;
                Ok(first_created
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null))
            }
            "rename" => {
                let from = arg_str(&args, 0).unwrap_or_default();
                let to = arg_str(&args, 1).unwrap_or_default();
                std::fs::rename(&from, &to)
                    .map_err(|error| error::io_error("rename", &from, &error))?;
                Ok(serde_json::Value::Null)
            }
            "realpath" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let resolved = std::fs::canonicalize(&path)
                    .map_err(|error| error::io_error("realpath", &path, &error))?;
                Ok(serde_json::Value::String(
                    resolved.to_string_lossy().into_owned(),
                ))
            }
            "chmod" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let mode = arg_num(&args, 1).unwrap_or(0.0) as u32;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                        .map_err(|error| error::io_error("chmod", &path, &error))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = mode;
                    if !Path::new(&path).exists() {
                        return Err(error::io_error(
                            "chmod",
                            &path,
                            &std::io::Error::from(std::io::ErrorKind::NotFound),
                        ));
                    }
                }
                Ok(serde_json::Value::Null)
            }
            // 第 3 个参数是 Node 的 mode 位掩码，`COPYFILE_EXCL` 的值为 1（`js/node/fs.js`
            // 从 `fs.constants` 取）。走排他分支时手工复制，并保留 Node 的 `dest` 属性。
            "copyFile" => {
                let from = arg_str(&args, 0).unwrap_or_default();
                let to = arg_str(&args, 1).unwrap_or_default();
                let exclusive = arg_num(&args, 2).unwrap_or(0.0) as i32 & 1 != 0;
                if exclusive {
                    use std::io::Write as _;
                    let mut source = std::fs::File::open(&from)
                        .map_err(|error| error::io_error("open", &from, &error))?;
                    let mut target = match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&to)
                    {
                        Ok(target) => target,
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            let mut value = error::node_error(
                                "Error",
                                format!("EEXIST: file already exists, copyfile '{from}' -> '{to}'"),
                                Some("EEXIST"),
                                Some(-17),
                                Some("copyfile"),
                                Some(from.as_str()),
                            );
                            if let Some(map) = value.as_object_mut() {
                                map.insert(
                                    "dest".to_owned(),
                                    serde_json::Value::String(to.clone()),
                                );
                            }
                            return Err(value);
                        }
                        Err(error) => return Err(error::io_error("open", &to, &error)),
                    };
                    std::io::copy(&mut source, &mut target)
                        .map_err(|error| error::io_error("copyfile", &to, &error))?;
                    let _ = target.flush();
                } else {
                    std::fs::copy(&from, &to)
                        .map_err(|error| error::io_error("copyfile", &from, &error))?;
                }
                Ok(serde_json::Value::Null)
            }
            "readlink" => {
                let path = arg_str(&args, 0).unwrap_or_default();
                let target = std::fs::read_link(&path)
                    .map_err(|error| error::io_error("readlink", &path, &error))?;
                Ok(serde_json::Value::String(
                    target.to_string_lossy().into_owned(),
                ))
            }
            "symlink" => {
                let target = arg_str(&args, 0).unwrap_or_default();
                let path = arg_str(&args, 1).unwrap_or_default();
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&target, &path)
                        .map_err(|error| error::io_error("symlink", &path, &error))?;
                }
                #[cfg(windows)]
                {
                    let is_dir = Path::new(&target).is_dir();
                    let result = if is_dir {
                        std::os::windows::fs::symlink_dir(&target, &path)
                    } else {
                        std::os::windows::fs::symlink_file(&target, &path)
                    };
                    result.map_err(|error| error::io_error("symlink", &path, &error))?;
                }
                Ok(serde_json::Value::Null)
            }
            // 空实现：不修改任何时间戳，直接返回 null。需要真实 utimes 的扩展会静默失效。
            "utimes" => Ok(serde_json::Value::Null),
            other => Err(error::node_error(
                "Error",
                format!("fs operation not supported in this runtime: {other}"),
                Some("ERR_UNSUPPORTED"),
                None,
                None,
                None,
            )),
        }
    }

    /// `mkdirSync(path, {recursive: true})` 的返回值：第一个被创建的目录（Node 语义）。
    /// 路径已存在时返回 `None`。
    ///
    /// 算法：从目标向上收集“尚不存在”的组件（`Path::exists`，跟随符号链接），直到遇到已
    /// 存在的祖先或走到根；`missing` 的最后一项就是最靠上的缺失目录（也就是 Node 返回的
    /// 那一个）。只查询、不创建——真正创建发生在调用方的 `DirBuilder::create`。
    fn first_missing_component(path: &str) -> Option<String> {
        let mut current = PathBuf::from(path);
        let mut missing: Vec<PathBuf> = Vec::new();
        loop {
            if current.as_os_str().is_empty() || current.exists() {
                break;
            }
            missing.push(current.clone());
            match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => current = parent.to_path_buf(),
                _ => break,
            }
        }
        missing
            .last()
            .map(|entry| entry.to_string_lossy().into_owned())
    }

    fn dirent_type(kind: &std::fs::FileType) -> &'static str {
        if kind.is_symlink() {
            "symlink"
        } else if kind.is_dir() {
            "dir"
        } else if kind.is_file() {
            "file"
        } else {
            "other"
        }
    }

    fn stat_json(metadata: &std::fs::Metadata) -> serde_json::Value {
        let kind = metadata.file_type();
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let atime = metadata
            .accessed()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let birthtime = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        // Node's `ctimeMs` is the inode change time on Unix, not the birth time.
        #[cfg(unix)]
        let ctime = {
            use std::os::unix::fs::MetadataExt as _;
            metadata.ctime() as f64 * 1000.0 + metadata.ctime_nsec() as f64 / 1_000_000.0
        };
        #[cfg(not(unix))]
        let ctime = birthtime;
        #[cfg(unix)]
        let (mode, uid, gid, ino, dev, nlink) = {
            use std::os::unix::fs::MetadataExt as _;
            (
                metadata.mode(),
                metadata.uid(),
                metadata.gid(),
                metadata.ino(),
                metadata.dev(),
                metadata.nlink(),
            )
        };
        #[cfg(not(unix))]
        let (mode, uid, gid, ino, dev, nlink) = (
            if kind.is_dir() { 0o40755 } else { 0o100644 },
            0,
            0,
            0,
            0,
            1,
        );
        #[cfg(unix)]
        let blocks = {
            use std::os::unix::fs::MetadataExt as _;
            metadata.blocks()
        };
        #[cfg(not(unix))]
        let blocks = metadata.len().div_ceil(512);
        serde_json::json!({
            "size": metadata.len(),
            "blocks": blocks,
            "blksize": 4096,
            "mode": mode,
            "uid": uid,
            "gid": gid,
            "ino": ino,
            "dev": dev,
            "nlink": nlink,
            "mtimeMs": mtime,
            "atimeMs": atime,
            "ctimeMs": ctime,
            "birthtimeMs": birthtime,
            "isFile": kind.is_file(),
            "isDirectory": kind.is_dir(),
            "isSymbolicLink": kind.is_symlink(),
            "type": dirent_type(&kind),
        })
    }

    /// `fs.accessSync(path, mode)`：Unix 直接用 `access(2)`，因此检查的是**真实权限**，
    /// 而不是“文件是否存在”；`mode` 是 `R_OK`/`W_OK`/`X_OK` 的位组合。
    ///
    /// 路径含 NUL 字节时 `CString::new` 失败，返回 `ERR_INVALID_ARG_VALUE` 的 TypeError；
    /// 其它失败用 `last_os_error()` 转成 Node 错误。非 Unix 平台退化成存在性检查。
    fn check_access(path: &str, mode: i32) -> Result<(), serde_json::Value> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            let Ok(c_path) = CString::new(path) else {
                return Err(error::node_error(
                    "TypeError",
                    "The argument 'path' must be a string without null bytes".to_owned(),
                    Some("ERR_INVALID_ARG_VALUE"),
                    None,
                    Some("access"),
                    Some(path),
                ));
            };
            // SAFETY: `c_path` 是本函数内有效的 NUL 结尾 C 字符串，指针只在调用期间使用。
            let result = unsafe { libc::access(c_path.as_ptr(), mode) };
            if result == 0 {
                Ok(())
            } else {
                Err(error::io_error(
                    "access",
                    path,
                    &std::io::Error::last_os_error(),
                ))
            }
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            if Path::new(path).exists() {
                Ok(())
            } else {
                Err(error::io_error(
                    "access",
                    path,
                    &std::io::Error::from(std::io::ErrorKind::NotFound),
                ))
            }
        }
    }

    /// `fs.mkdtempSync(prefix)`：在 prefix 后拼 6 个随机字母数字，最多重试 10 次。
    ///
    /// 每次都先 `create_dir` 再尝试设成 0700（Unix；设置失败被忽略）。重试只针对 `EEXIST`；
    /// 10 次都撞名字时返回合成的 `EACCES ... mkdtemp` 错误。
    fn mkdtemp(prefix: &str) -> Result<String, serde_json::Value> {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for _ in 0..10 {
            let mut random = [0u8; 6];
            let _ = getrandom::fill(&mut random);
            let suffix: String = random
                .iter()
                .map(|byte| CHARS[*byte as usize % CHARS.len()] as char)
                .collect();
            let candidate = format!("{prefix}{suffix}");
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        let _ = std::fs::set_permissions(
                            &candidate,
                            std::fs::Permissions::from_mode(0o700),
                        );
                    }
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error::io_error("mkdtemp", prefix, &error)),
            }
        }
        Err(error::node_error(
            "Error",
            format!("EACCES: permission denied, mkdtemp '{prefix}'"),
            Some("EACCES"),
            Some(-13),
            Some("mkdtemp"),
            Some(prefix),
        ))
    }

    /// `__kiana_host.read(path)`：CJS 源码；ESM（`.mjs` 或 `type: module` 的 `.js`）返回 `None`。
    ///
    /// - `node:<name>` 一律返回 `None`：内建模块从 `__kiana_node_shims` 取，不按文件读；
    /// - 模块名可能带 `?v=<gen>-<seq>` 缓存键（`loader_impl` 的入口命名），读取前截掉；
    /// - 读取失败返回 Node 错误（syscall `open`），由调用点抛出；
    /// - 内容用 `from_utf8_lossy`：非 UTF-8 字节变成 U+FFFD，再交给 JS 求值；
    /// - `None` 让 `cjs.js` 按 ESM 处理（直接 `require` 时抛 `ERR_REQUIRE_ESM`）。
    pub fn read_module_source(path: &str) -> Result<Option<String>, serde_json::Value> {
        if path.starts_with("node:") {
            return Ok(None);
        }
        let cleaned = match path.split_once("?v=") {
            Some((base, _)) => base,
            None => path,
        };
        let bytes =
            std::fs::read(cleaned).map_err(|error| error::io_error("open", cleaned, &error))?;
        if is_esm_path(cleaned) {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// ESM 判定：`.mjs`/`.mts`/`.mjsx` 一定是 ESM；`.cjs`/`.cts`/`.json`/`.node` 一定不是；
    /// 其它扩展名（含 `.js`/`.ts`）向上找最近的 `package.json`，看 `"type": "module"`。
    /// 只看扩展名与 package.json，不解析源码，也不嗅探 `import` 语法。
    fn is_esm_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".mjs") || lower.ends_with(".mts") || lower.ends_with(".mjsx") {
            return true;
        }
        if lower.ends_with(".cjs")
            || lower.ends_with(".cts")
            || lower.ends_with(".json")
            || lower.ends_with(".node")
        {
            return false;
        }
        package_is_module(path)
    }

    /// 最近的 `package.json` 的 `"type": "module"`（Node 的 ESM 判定）。
    ///
    /// 从文件所在目录逐级向上：第一个存在的 `package.json` 决定结果——即使它没有 `"type"`
    /// 字段或不是合法 JSON，也返回 `false` 并停止；到根仍未找到同样返回 `false`。
    /// 没有缓存，每次判断都会重新读文件。
    fn package_is_module(path: &str) -> bool {
        let mut directory = PathBuf::from(path);
        directory.pop();
        loop {
            let manifest = directory.join("package.json");
            if let Ok(text) = std::fs::read_to_string(&manifest) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    return json.get("type").and_then(|value| value.as_str()) == Some("module");
                }
                return false;
            }
            if !directory.pop() {
                return false;
            }
        }
    }

    // ---- fs.promises 的宿主 kind -----------------------------------------

    /// `__kiana.promise("fs", { op, ... })`：结果 resolve，错误 reject 成 Node 错误 JSON。
    ///
    /// 处理器只解析 payload，然后把同步调用放进 `spawn_local` 并立即返回；promise 的
    /// resolve/reject 由任务通过 `completion_sender()` 投递。任务里没有 `.await`，所以一次
    /// 大文件读写会占住 LocalSet 线程直到完成，期间其它本地任务无法推进。
    pub fn handle_fs_kind(call: HostCall<'_>) -> EngineResult<()> {
        let payload: serde_json::Value = call.payload_json()?;
        let operation = payload
            .get("op")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let args = payload
            .get("args")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let sender = call.completion_sender();
        let id = call.id();
        tokio::task::spawn_local(async move {
            let args_text = args.to_string();
            match fs_sync(&operation, &args_text) {
                Ok(value) => {
                    sender.resolve(id, HostValue::Json(value));
                }
                Err(error_value) => {
                    sender.reject(id, HostValue::Json(error_value));
                }
            }
        });
        Ok(())
    }

    /// `__kiana.promise("watch", { path, onEvent })`。
    ///
    /// 成功时立即 resolve `{ handle }`，之后事件由 `onEvent` 推送：`{type:"change",
    /// eventType, filename}`（`eventType` 是 `"change"`/`"rename"`）。notify 的监听回调把
    /// 事件写进 `std::sync::mpsc` 通道，由 `spawn_local` 任务里的轮询循环读取
    /// （每 5ms `try_recv` 一次），因此投递延迟受这个周期影响。
    ///
    /// 失败路径：`start_watcher` 在创建失败时会**同步**调用错误回调（往 `tx` 里写一条
    /// error 事件），但此时接收端 `rx` 尚未移入任务、随函数返回被丢弃，所以 JS 只能看到
    /// promise 的 reject，看不到事件。reject 的错误固定是 `ENOENT ... watch`，不区分真实原因。
    pub fn handle_watch_kind(
        call: HostCall<'_>,
        state: &Rc<RefCell<FsWatchState>>,
    ) -> EngineResult<()> {
        let path = call
            .payload()
            .as_object()
            .and_then(|payload| payload.get::<_, Option<String>>("path").ok().flatten())
            .unwrap_or_default();
        let on_event: Function<'_> = super::payload_function(&call, "onEvent")?;
        let callback = call.completion_sender().capture(on_event);
        let sender = call.completion_sender();
        let id = call.id();
        let closed = Arc::new(AtomicBool::new(false));
        let handle = state.borrow_mut().allocate(Arc::clone(&closed));

        let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
        let event_tx = tx.clone();
        let error_tx = tx.clone();
        let watcher = start_watcher(
            &path,
            move |event_type, filename| {
                let _ = event_tx.send(serde_json::json!({
                    "type": "change",
                    "eventType": event_type.as_str(),
                    "filename": filename,
                }));
            },
            move || {
                let _ = error_tx.send(serde_json::json!({
                    "type": "error",
                    "message": "ENOENT: no such file or directory, watch",
                    "code": "ENOENT",
                }));
            },
        );

        let Some(watcher) = watcher else {
            let _ = tx.send(serde_json::json!({
                "type": "error",
                "message": "ENOENT: no such file or directory, watch",
                "code": "ENOENT",
            }));
            sender.reject(
                id,
                HostValue::Json(error::node_error(
                    "Error",
                    format!("ENOENT: no such file or directory, watch '{path}'"),
                    Some("ENOENT"),
                    None,
                    Some("watch"),
                    Some(path.as_str()),
                )),
            );
            return Ok(());
        };

        sender.resolve(id, HostValue::Json(serde_json::json!({ "handle": handle })));
        let state = Rc::clone(state);
        // 轮询循环必须持有 `watcher`：它一旦被丢弃，notify 的底层监听就停止。`try_recv`
        // 一次排空所有已到达事件；没有事件时睡 5ms，避免忙转。
        tokio::task::spawn_local(async move {
            let _watcher = watcher;
            loop {
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                let mut drained = false;
                while let Ok(event) = rx.try_recv() {
                    drained = true;
                    callback.invoke(vec![HostValue::Json(event)]);
                }
                if drained {
                    continue;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            state.borrow_mut().closed.borrow_mut().remove(&handle);
        });
        Ok(())
    }

    /// `fs.watch` 的 close：置位停止标志，返回该 handle 是否存在。
    ///
    /// 登记项在这里就被移除（轮询任务结束时还会再移除一次，幂等），所以重复 close 返回
    /// `false`。任务可能正处在 5ms 的 sleep 中，最多再等一个周期才观察到标志。
    pub fn close_watch(state: &Rc<RefCell<FsWatchState>>, handle: u64) -> bool {
        let state_ref = state.borrow();
        let mut watchers = state_ref.closed.borrow_mut();
        match watchers.remove(&handle) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// 对 `kiana_core::fs_watch::watch_with_error_handler` 的薄转发（非递归监听）。
    ///
    /// 该函数在创建/注册失败时会**先同步调用 `on_error`** 再返回 `None`，调用方需要知道
    /// 这个时序（见 `handle_watch_kind` 的失败路径）。
    fn start_watcher<L, E>(
        path: &str,
        listener: L,
        on_error: E,
    ) -> Option<notify::RecommendedWatcher>
    where
        L: FnMut(kiana_core::fs_watch::WatchEventType, Option<String>) + Send + 'static,
        E: FnMut() + Send + 'static,
    {
        kiana_core::fs_watch::watch_with_error_handler(path, listener, on_error)
    }
}

// ---------------------------------------------------------------------------
// child_process
// ---------------------------------------------------------------------------

mod child {
    // child_process 的宿主侧：同步 `spawnSync` 与异步 `spawn`（host kind `child.spawn`）。
    //
    // 异步侧状态机：
    // 1. 处理器解析 payload（command/args/options/onEvent），用
    //    `kiana_core::child_process::spawn_process` 启动进程；
    // 2. 成功后**立刻** resolve `{handle, pid}`，随后 `spawn_local` 任务同时驱动 stdin 命令
    //    通道、stdout/stderr 读取与 `child.wait()`；
    // 3. 事件依次推给 JS：`{type:"stdout"|"stderr", b64}`、`{type:"error"}`，最后
    //    `{type:"close", code, signal}`；
    // 4. 退出且两个输出流都关闭后才发 `close`，避免截断最后一段输出。
    //
    // `ChildState` 保存 handle → 命令发送端，供 `child.kill` / `stdin.write` / `stdin.end`
    // 使用；任务结束时移除。
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use kiana_core::child_process::{SpawnOptions, SpawnSyncOptions, StdioMode};
    use rquickjs::Function;
    use tokio::io::AsyncReadExt as _;
    use tokio::sync::mpsc;

    use crate::engine::{EngineResult, HostCall, HostValue};

    use super::{b64_encode, error};

    enum ChildCommand {
        Signal(String),
        Write(Vec<u8>),
        EndStdin,
    }

    /// 子进程句柄表：handle → 命令发送端（kill 与 stdin 写端共用一条通道）。
    ///
    /// id 从 1 开始单调递增；任务结束时由任务自己 `remove`。发送端被移除后，
    /// `kill_child`/`write_child`/`end_child_stdin` 返回 `false`。
    #[derive(Default)]
    pub struct ChildState {
        senders: RefCell<HashMap<u64, mpsc::UnboundedSender<ChildCommand>>>,
        next: AtomicU64,
    }

    impl ChildState {
        fn allocate(&self, sender: mpsc::UnboundedSender<ChildCommand>) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            self.senders.borrow_mut().insert(id, sender);
            id
        }

        fn remove(&self, id: u64) {
            self.senders.borrow_mut().remove(&id);
        }

        fn write_stdin(&self, id: u64, bytes: Vec<u8>) -> bool {
            let sender = self.senders.borrow().get(&id).cloned();
            match sender {
                Some(sender) => sender.send(ChildCommand::Write(bytes)).is_ok(),
                None => false,
            }
        }

        fn end_stdin(&self, id: u64) -> bool {
            let sender = self.senders.borrow().get(&id).cloned();
            match sender {
                Some(sender) => sender.send(ChildCommand::EndStdin).is_ok(),
                None => false,
            }
        }
    }

    /// 解析 `stdio` 选项：数组逐项取，字符串一般化到三项，其它情况默认全 `pipe`。
    ///
    /// 只识别 `ignore`/`pipe`/`inherit`；`ipc`、数字 fd、流对象等写法都会落到默认值
    /// （`pipe`），不会报错。
    fn stdio_modes(value: &serde_json::Value) -> [StdioMode; 3] {
        fn one(value: Option<&serde_json::Value>, default: StdioMode) -> StdioMode {
            match value.and_then(|value| value.as_str()) {
                Some("ignore") => StdioMode::Ignore,
                Some("pipe") => StdioMode::Pipe,
                Some("inherit") => StdioMode::Inherit,
                _ => default,
            }
        }
        match value {
            serde_json::Value::Array(items) => [
                one(items.first(), StdioMode::Pipe),
                one(items.get(1), StdioMode::Pipe),
                one(items.get(2), StdioMode::Pipe),
            ],
            serde_json::Value::String(mode) => {
                let mode = match mode.as_str() {
                    "ignore" => StdioMode::Ignore,
                    "inherit" => StdioMode::Inherit,
                    _ => StdioMode::Pipe,
                };
                [mode, mode, mode]
            }
            _ => [StdioMode::Pipe, StdioMode::Pipe, StdioMode::Pipe],
        }
    }

    /// `env` 对象 → 子进程环境变量列表。
    ///
    /// 只接受字符串/数字/布尔值，其它类型跳过；`Some(vec![])` 表示“显式空环境”，与 `None`
    /// （没传 `env`，由底层决定继承）不同。`js/node/child_process.js` 在没传 `env` 时会把
    /// 当前 `process.env` 快照进来，所以正常路径总是 `Some`。
    fn env_from(
        value: &serde_json::Value,
    ) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
        let object = value.as_object()?;
        let mut env = Vec::new();
        for (key, value) in object {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => continue,
            };
            env.push((key.into(), text.into()));
        }
        Some(env)
    }

    /// `shell: true` / `shell: "<path>"` 时把命令与参数拼成一行交给 shell。
    ///
    /// 参数按空格直接拼接，**不做引号或转义**：带空格/元字符的参数需要调用方自己加引号。
    /// 默认 shell：Windows `cmd.exe /d /s /c <line>`，其它平台 `/bin/sh -c <line>`。
    fn shell_command(command: &str, args: &[String], shell: Option<&str>) -> (String, Vec<String>) {
        #[cfg(windows)]
        let default_shell = "cmd.exe";
        #[cfg(not(windows))]
        let default_shell = "/bin/sh";
        let shell = shell.unwrap_or(default_shell);
        let mut line = command.to_owned();
        for arg in args {
            line.push(' ');
            line.push_str(arg);
        }
        #[cfg(windows)]
        {
            (
                shell.to_owned(),
                vec!["/d".to_owned(), "/s".to_owned(), "/c".to_owned(), line],
            )
        }
        #[cfg(not(windows))]
        {
            (shell.to_owned(), vec!["-c".to_owned(), line])
        }
    }

    /// `__kiana.promise("child.spawn", { command, args, options, onEvent })`。
    ///
    /// 成功：先 resolve `{handle, pid}`，再开始推事件。失败：reject 成 `Error`
    /// （消息 `spawn <program> <code>`，`syscall` 与 `path` 都指向程序名），不发事件。
    ///
    /// 事件协议见模块头；`stdout`/`stderr` 由同一个 `select!` 的两个分支读取，两者同时就绪时
    /// 先处理哪个由 `select!` 的选择决定，因此两个流之间的交错顺序不保证。stdout 读错误
    /// （除 `BrokenPipe`）会发一条 `stderrError` 诊断事件——Node 没有对应事件，
    /// `js/node/child_process.js` 明确忽略它。
    pub fn handle_spawn_kind(
        call: HostCall<'_>,
        state: &Rc<RefCell<ChildState>>,
    ) -> EngineResult<()> {
        let payload: serde_json::Value = call.payload_json()?;
        let command = payload
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let raw_args: Vec<String> = payload
            .get("args")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let options = payload
            .get("options")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let on_event: Function<'_> = super::payload_function(&call, "onEvent")?;
        let callback = call.completion_sender().capture(on_event);
        let sender = call.completion_sender();
        let id = call.id();

        let shell = match options.get("shell") {
            Some(serde_json::Value::String(text)) => Some(text.clone()),
            Some(serde_json::Value::Bool(true)) => Some(String::new()),
            _ => None,
        };
        let (program, args) = match shell {
            Some(ref text) => {
                let shell_name = if text.is_empty() {
                    None
                } else {
                    Some(text.as_str())
                };
                shell_command(&command, &raw_args, shell_name)
            }
            None => (command.clone(), raw_args.clone()),
        };

        let stdio = stdio_modes(options.get("stdio").unwrap_or(&serde_json::Value::Null));
        let spawn_options = SpawnOptions {
            cwd: options
                .get("cwd")
                .and_then(|value| value.as_str())
                .map(PathBuf::from),
            stdin: stdio[0],
            stdout: stdio[1],
            stderr: stdio[2],
            detached: options
                .get("detached")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            windows_hide: options
                .get("windowsHide")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            env: options.get("env").and_then(env_from),
        };

        match kiana_core::child_process::spawn_process(&program, &args, &spawn_options) {
            Ok(mut child) => {
                let pid = child.id().unwrap_or(0);
                let (command_tx, mut command_rx) = mpsc::unbounded_channel();
                let handle = state.borrow_mut().allocate(command_tx);
                sender.resolve(
                    id,
                    HostValue::Json(serde_json::json!({ "handle": handle, "pid": pid })),
                );

                let state = Rc::clone(state);
                tokio::task::spawn_local(async move {
                    let mut stdout = child.stdout.take();
                    let mut stderr = child.stderr.take();
                    let mut stdin = child.stdin.take();
                    let mut out_buf = vec![0u8; 65536];
                    let mut err_buf = vec![0u8; 65536];
                    // 子进程可能先退出、管道里仍有未读数据，所以必须等两个输出流都读到 EOF
                    // 才发 close；`exited && !out_open && !err_open` 是唯一的退出条件。
                    let mut out_open = stdout.is_some();
                    let mut err_open = stderr.is_some();
                    let mut exited = false;
                    let mut exit_status: Option<std::process::ExitStatus> = None;

                    loop {
                        if exited && !out_open && !err_open {
                            break;
                        }
                        tokio::select! {
                            command = command_rx.recv() => {
                                match command {
                                    Some(ChildCommand::Signal(signal)) => {
                                        send_signal(pid as i32, &signal).ok();
                                    }
                                    Some(ChildCommand::Write(bytes)) => {
                                        use tokio::io::AsyncWriteExt as _;
                                        if let Some(stream) = stdin.as_mut() {
                                            let _ = stream.write_all(&bytes).await;
                                            let _ = stream.flush().await;
                                        }
                                    }
                                    Some(ChildCommand::EndStdin) => {
                                        use tokio::io::AsyncWriteExt as _;
                                        if let Some(mut stream) = stdin.take() {
                                            let _ = stream.shutdown().await;
                                        }
                                    }
                                    None => {}
                                }
                            }
                            result = async {
                                match stdout.as_mut() {
                                    Some(stream) => stream.read(&mut out_buf).await,
                                    None => Ok(0),
                                }
                            }, if out_open => {
                                match result {
                                    Ok(0) => {
                                        out_open = false;
                                        stdout = None;
                                    }
                                    Ok(count) => {
                                        callback.invoke(vec![HostValue::Json(serde_json::json!({
                                            "type": "stdout",
                                            "b64": b64_encode(&out_buf[..count]),
                                        }))]);
                                    }
                                    Err(error) => {
                                        out_open = false;
                                        stdout = None;
                                        if error.kind() != std::io::ErrorKind::BrokenPipe {
                                            callback.invoke(vec![HostValue::Json(serde_json::json!({
                                                "type": "stderrError",
                                                "message": error.to_string(),
                                            }))]);
                                        }
                                    }
                                }
                            }
                            result = async {
                                match stderr.as_mut() {
                                    Some(stream) => stream.read(&mut err_buf).await,
                                    None => Ok(0),
                                }
                            }, if err_open => {
                                match result {
                                    Ok(0) => {
                                        err_open = false;
                                        stderr = None;
                                    }
                                    Ok(count) => {
                                        callback.invoke(vec![HostValue::Json(serde_json::json!({
                                            "type": "stderr",
                                            "b64": b64_encode(&err_buf[..count]),
                                        }))]);
                                    }
                                    Err(_) => {
                                        err_open = false;
                                        stderr = None;
                                    }
                                }
                            }
                            status = child.wait(), if !exited => {
                                match status {
                                    Ok(status) => {
                                        exited = true;
                                        exit_status = Some(status);
                                        if !out_open && !err_open {
                                            break;
                                        }
                                    }
                                    Err(error) => {
                                        exited = true;
                                        callback.invoke(vec![HostValue::Json(serde_json::json!({
                                            "type": "error",
                                            "message": error.to_string(),
                                            "code": "EUNKNOWN",
                                        }))]);
                                        if !out_open && !err_open {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // `code` 在信号终止时是 null；`signal` 只在 Unix 有值（见 status_signal）。
                    let (code, signal) = match exit_status {
                        Some(status) => (
                            status
                                .code()
                                .map_or(serde_json::Value::Null, |code| code.into()),
                            status_signal(&status),
                        ),
                        None => (serde_json::Value::Null, serde_json::Value::Null),
                    };
                    callback.invoke(vec![HostValue::Json(serde_json::json!({
                        "type": "close",
                        "code": code,
                        "signal": signal,
                    }))]);
                    state.borrow_mut().remove(handle);
                });
            }
            Err(failure) => {
                let (errno, code, _) = error::errno_info(&failure);
                let message = format!("spawn {program} {code}");
                let syscall = format!("spawn {program}");
                sender.reject(
                    id,
                    HostValue::Json(error::node_error(
                        "Error",
                        message,
                        Some(code),
                        Some(errno),
                        Some(syscall.as_str()),
                        Some(program.as_str()),
                    )),
                );
            }
        }
        Ok(())
    }

    /// 被信号终止时返回 Unix 信号名，否则 `null`（Windows 与正常退出都走 `null`）。
    fn status_signal(status: &std::process::ExitStatus) -> serde_json::Value {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            if let Some(signal) = status.signal() {
                return serde_json::Value::String(signal_name(signal).to_owned());
            }
        }
        serde_json::Value::Null
    }

    /// 信号编号 → 名字。表是硬编码的常用信号，不来自 libc 常量；表外编号返回
    /// `"SIGUNKNOWN"`（这个字符串会原样出现在 `close` 事件的 `signal` 里）。
    fn signal_name(signal: i32) -> &'static str {
        match signal {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            6 => "SIGABRT",
            9 => "SIGKILL",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            18 => "SIGCONT",
            19 => "SIGSTOP",
            10 => "SIGUSR1",
            12 => "SIGUSR2",
            _ => "SIGUNKNOWN",
        }
    }

    /// 信号名/数字字符串 → 编号。纯数字（含负数）原样解析；表外名字返回 `None`，调用方
    /// 把它变成 `EINVAL`。与 `signal_name` 一样是硬编码表，不使用 libc 常量。
    fn signal_number(signal: &str) -> Option<i32> {
        let name = signal.trim();
        let name = if let Ok(number) = name.parse::<i32>() {
            return Some(number);
        } else {
            name
        };
        Some(match name {
            "SIGHUP" => 1,
            "SIGINT" => 2,
            "SIGQUIT" => 3,
            "SIGILL" => 4,
            "SIGTRAP" => 5,
            "SIGABRT" => 6,
            "SIGBUS" => 7,
            "SIGFPE" => 8,
            "SIGKILL" => 9,
            "SIGUSR1" => 10,
            "SIGSEGV" => 11,
            "SIGUSR2" => 12,
            "SIGPIPE" => 13,
            "SIGALRM" => 14,
            "SIGTERM" => 15,
            "SIGCONT" => 18,
            "SIGSTOP" => 19,
            _ => return None,
        })
    }

    /// `libc::kill` / `TerminateProcess`，错误按 Node 的 `(errno, code)` 返回。
    ///
    /// 未知信号名在系统调用之前就返回 `EINVAL`。Unix 直接 `kill(2)`；Windows 用
    /// `OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess`，**任何**信号名都等同于终止
    /// 进程，`pid <= 0` 返回 `ESRCH`。错误码经 `error::errno_info` 转换（Unix 是负值）。
    pub fn send_signal(pid: i32, signal: &str) -> Result<(), (i32, &'static str)> {
        let Some(number) = signal_number(signal) else {
            return Err((-22, "EINVAL"));
        };
        #[cfg(unix)]
        {
            // SAFETY: `kill(2)` 只读取两个整数参数，不解引用指针，也不访问进程内内存。
            let result = unsafe { libc::kill(pid, number) };
            if result == 0 {
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                let (errno, code, _) = error::errno_info(&error);
                Err((errno, code))
            }
        }
        #[cfg(windows)]
        {
            let _ = number;
            if pid <= 0 {
                return Err((-3, "ESRCH"));
            }
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_TERMINATE, TerminateProcess,
            };
            // SAFETY: process handles returned by OpenProcess are closed below.
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
                if handle.is_null() {
                    let error = std::io::Error::last_os_error();
                    let (errno, code, _) = error::errno_info(&error);
                    return Err((errno, code));
                }
                let result = TerminateProcess(handle, 1);
                CloseHandle(handle);
                if result == 0 {
                    let error = std::io::Error::last_os_error();
                    let (errno, code, _) = error::errno_info(&error);
                    return Err((errno, code));
                }
            }
            Ok(())
        }
    }

    /// `child.kill([signal])`：把信号命令投给子进程任务，返回是否投递成功。
    ///
    /// 只是“命令入队成功”，不代表进程真的收到了信号；handle 未知（已退出并清理）或通道
    /// 关闭时返回 `false`。默认 `SIGTERM`。
    pub fn kill_child(state: &Rc<RefCell<ChildState>>, handle: u64, signal: Option<&str>) -> bool {
        let sender = state.borrow().senders.borrow().get(&handle).cloned();
        if let Some(sender) = sender {
            sender
                .send(ChildCommand::Signal(signal.unwrap_or("SIGTERM").to_owned()))
                .is_ok()
        } else {
            false
        }
    }

    /// `stdin.write(...)`：把字节投给子进程任务，返回命令是否入队。
    ///
    /// 同样只表示入队成功；子进程没开 stdin 管道时任务内会静默丢弃（`stdin` 为 `None`）。
    pub fn write_child(state: &Rc<RefCell<ChildState>>, handle: u64, bytes: Vec<u8>) -> bool {
        state.borrow().write_stdin(handle, bytes)
    }

    /// `stdin.end()`：投递关闭命令，返回命令是否入队；重复调用只会让第一次生效
    /// （任务里 `stdin.take()` 之后为 `None`）。
    pub fn end_child_stdin(state: &Rc<RefCell<ChildState>>, handle: u64) -> bool {
        state.borrow().end_stdin(handle)
    }

    /// `__kiana_host.spawnSync(argsJson)`：`spawn_process_sync` 的 JSON 包装。
    ///
    /// 即使启动失败也返回 `Ok`：错误放在 JSON 的 `error` 字段（Node 的 `spawnSync` 语义，
    /// 不抛异常）。`stdout`/`stderr` 是原始字节的 base64（Node 在没指定 `encoding` 时给
    /// Buffer）；`signal` 字段目前恒为 `null`。`argsJson` 解析失败按 `Null` 处理。
    pub fn spawn_sync(args_json: &str) -> Result<serde_json::Value, serde_json::Value> {
        let payload: serde_json::Value =
            serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null);
        let command = payload
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let raw_args: Vec<String> = payload
            .get("args")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let options = payload
            .get("options")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let shell = match options.get("shell") {
            Some(serde_json::Value::String(text)) => Some(text.clone()),
            Some(serde_json::Value::Bool(true)) => Some(String::new()),
            _ => None,
        };
        let (program, args) = match shell {
            Some(ref text) => {
                let shell_name = if text.is_empty() {
                    None
                } else {
                    Some(text.as_str())
                };
                shell_command(&command, &raw_args, shell_name)
            }
            None => (command.clone(), raw_args.clone()),
        };
        let stdio = stdio_modes(options.get("stdio").unwrap_or(&serde_json::Value::Null));
        let sync_options = SpawnSyncOptions {
            cwd: options
                .get("cwd")
                .and_then(|value| value.as_str())
                .map(PathBuf::from),
            stdin: stdio[0],
            stdout: stdio[1],
            stderr: stdio[2],
            windows_hide: options
                .get("windowsHide")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            timeout_ms: options
                .get("timeout")
                .and_then(|value| value.as_f64())
                .filter(|value| *value > 0.0)
                .map(|value| value as u64),
            // Node inherits the *live* `process.env` when `env` is omitted; the JS
            // shim snapshots it into the payload, so we always receive an object.
            env: options.get("env").and_then(env_from),
        };
        let result =
            kiana_core::child_process::spawn_process_sync_bytes(&program, &args, &sync_options);
        let error_value = result.error.as_ref().map(|failure| {
            let (errno, code, _) = error::errno_info(failure);
            let syscall = format!("spawnSync {program}");
            error::node_error(
                "Error",
                failure.to_string(),
                Some(code),
                Some(errno),
                Some(syscall.as_str()),
                Some(program.as_str()),
            )
        });
        Ok(serde_json::json!({
            "pid": result.pid,
            "status": result.status,
            "signal": serde_json::Value::Null,
            // Raw bytes: Node returns a Buffer when `encoding` is not set.
            "stdout": b64_encode(&result.stdout),
            "stderr": b64_encode(&result.stderr),
            "error": error_value,
        }))
    }
}

// ---------------------------------------------------------------------------
// net：真实 TCP 服务器 / 客户端（设计 §7.3）
// ---------------------------------------------------------------------------

mod net {
    // 真实 TCP 服务器/客户端（设计 §7.3）。所有 socket 都是 tokio 的 `TcpStream`，但读写
    // 任务跑在同一个 `LocalSet` 上，状态（写端通道、取消令牌）放在 `NetState` 的
    // `RefCell<HashMap>` 里，只能在同一线程访问。
    //
    // 事件协议（`onEvent`）：
    // - server：`{type:"connection", socketId, remoteAddress/Port/Family, localAddress/Port}`
    //   与 `{type:"error", message, code}`；
    // - socket：`{type:"connect"}`, `{type:"data", socketId, b64}`, `{type:"end"}`,
    //   `{type:"error"}`, 最后 `{type:"close", socketId, hadError}`。
    //
    // 关闭有两条路：`netEnd` 只关写半（发 `Shutdown` 命令），`netDestroy` 取消整个 socket
    // 任务；两者都不会立刻移除表项——清理由 `drive_socket` 退出时完成。
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rquickjs::Function;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use crate::engine::{EngineResult, HostCall, HostValue};

    use super::error;
    use super::{b64_encode, payload_function};

    /// 写端命令：`Write` 写一段字节，`Shutdown` 关闭写半（不发 RST，也不销毁 socket）。
    pub enum SocketCommand {
        Write(Vec<u8>),
        Shutdown,
    }

    /// 三张表各管一件事：
    ///
    /// - `writers`：socket id → 写命令发送端（`netWrite`/`netEnd` 用）；
    /// - `servers`：server handle → 取消令牌（`netClose` 用，取消 accept 循环）；
    /// - `sockets`：socket id → 取消令牌（`netDestroy` 用，取消读写任务）。
    ///
    /// socket id 与 server handle 是两套独立计数器（都从 1 开始），不能混用。
    #[derive(Default)]
    pub struct NetState {
        writers: RefCell<HashMap<u64, mpsc::UnboundedSender<SocketCommand>>>,
        servers: RefCell<HashMap<u64, CancellationToken>>,
        sockets: RefCell<HashMap<u64, CancellationToken>>,
        next_socket: AtomicU64,
        next_server: AtomicU64,
    }

    impl NetState {
        fn new_socket_id(&self) -> u64 {
            self.next_socket.fetch_add(1, Ordering::Relaxed) + 1
        }

        fn new_server_id(&self) -> u64 {
            self.next_server.fetch_add(1, Ordering::Relaxed) + 1
        }
    }

    /// 构造带 `address`/`port` 的 Node net 错误 JSON。
    ///
    /// `ECONNREFUSED` 的文案没有描述段（`connect ECONNREFUSED 127.0.0.1:1`），其它错误带
    /// `code: description`；`path` 字段在本模块恒不设置（网络错误用 address/port）。
    fn node_net_error(
        code: &str,
        description: &str,
        errno: i32,
        syscall: &str,
        address: &str,
        port: f64,
    ) -> serde_json::Value {
        // Node/libuv renders `connect ECONNREFUSED 127.0.0.1:1` (no description)
        // but `listen EADDRINUSE: address already in use 127.0.0.1:1`.
        let message = if code == "ECONNREFUSED" {
            format!("{syscall} {code} {address}:{port}")
        } else {
            format!("{syscall} {code}: {description} {address}:{port}")
        };
        let mut value = error::node_error(
            "Error",
            message,
            Some(code),
            Some(errno),
            Some(syscall),
            None,
        );
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "address".to_owned(),
                serde_json::Value::String(address.to_owned()),
            );
            map.insert("port".to_owned(), serde_json::Value::from(port));
        }
        value
    }

    /// 把 host/port 拼成 `tokio::net` 可解析的地址；裸 IPv6 字面量补方括号
    /// （`::1` → `[::1]:port`），已经带括号的保持原样。
    fn bind_address(host: &str, port: u16) -> String {
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }

    /// `__kiana.promise("net.listen", { port, host, onEvent })`。
    ///
    /// host 缺省 `0.0.0.0`，port 缺省 0（系统分配）。bind 成功后 resolve
    /// `{handle, port, address, family}`，其中 port/address 是**实际**监听地址（port 0 时
    /// 才有意义）。随后进入 accept 循环：每个连接分配新的 socket id、注册写端与取消令牌、
    /// 推一条 `connection` 事件，并为它启动一个 `drive_socket` 任务。
    ///
    /// accept 失败不会退出循环，而是推一条 `error` 事件后继续；只有 `netClose` 取消令牌
    /// 才会结束循环。令牌在 bind **之前**就登记，bind 失败会移除登记。
    pub fn handle_listen_kind(
        call: HostCall<'_>,
        state: &Rc<RefCell<NetState>>,
    ) -> EngineResult<()> {
        let payload: serde_json::Value = call.payload_json()?;
        let port = payload
            .get("port")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0) as u16;
        let host = payload
            .get("host")
            .and_then(|value| value.as_str())
            .filter(|host| !host.is_empty())
            .unwrap_or("0.0.0.0")
            .to_owned();
        let on_event: Function<'_> = payload_function(&call, "onEvent")?;
        let callback = Rc::new(call.completion_sender().capture(on_event));
        let sender = call.completion_sender();
        let id = call.id();
        let token = CancellationToken::new();
        let handle = state.borrow().new_server_id();
        state
            .borrow()
            .servers
            .borrow_mut()
            .insert(handle, token.clone());
        let state = Rc::clone(state);

        tokio::task::spawn_local(async move {
            let address = bind_address(&host, port);
            let listener = match tokio::net::TcpListener::bind(address.as_str()).await {
                Ok(listener) => listener,
                Err(failure) => {
                    let (errno, code, description) = error::errno_info(&failure);
                    sender.reject(
                        id,
                        HostValue::Json(node_net_error(
                            code,
                            description,
                            errno,
                            "listen",
                            &host,
                            f64::from(port),
                        )),
                    );
                    state.borrow().servers.borrow_mut().remove(&handle);
                    return;
                }
            };
            let local = match listener.local_addr() {
                Ok(local) => local,
                Err(failure) => {
                    let (errno, code, description) = error::errno_info(&failure);
                    sender.reject(
                        id,
                        HostValue::Json(node_net_error(
                            code,
                            description,
                            errno,
                            "listen",
                            &host,
                            f64::from(port),
                        )),
                    );
                    return;
                }
            };
            sender.resolve(
                id,
                HostValue::Json(serde_json::json!({
                    "handle": handle,
                    "port": local.port(),
                    "address": local.ip().to_string(),
                    "family": if local.is_ipv6() { "IPv6" } else { "IPv4" },
                })),
            );

            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, peer)) => {
                                let socket_id = state.borrow().new_socket_id();
                                let local = stream.local_addr().ok();
                                let (writer_tx, writer_rx) = mpsc::unbounded_channel();
                                state.borrow().writers.borrow_mut().insert(socket_id, writer_tx);
                                let socket_token = CancellationToken::new();
                                state.borrow().sockets.borrow_mut().insert(socket_id, socket_token.clone());
                                callback.invoke(vec![HostValue::Json(serde_json::json!({
                                    "type": "connection",
                                    "socketId": socket_id,
                                    "remoteAddress": peer.ip().to_string(),
                                    "remotePort": peer.port(),
                                    "remoteFamily": if peer.is_ipv6() { "IPv6" } else { "IPv4" },
                                    "localAddress": local.map(|value| value.ip().to_string()).unwrap_or_default(),
                                    "localPort": local.map(|value| value.port()).unwrap_or(0),
                                }))]);
                                let callback = Rc::clone(&callback);
                                let state = Rc::clone(&state);
                                let socket_token = socket_token.clone();
                                tokio::task::spawn_local(async move {
                                    drive_socket(
                                        socket_id,
                                        stream,
                                        callback,
                                        state,
                                        socket_token,
                                        Some(writer_rx),
                                    )
                                    .await;
                                });
                            }
                            Err(failure) => {
                                let (_, code, description) = error::errno_info(&failure);
                                callback.invoke(vec![HostValue::Json(serde_json::json!({
                                    "type": "error",
                                    "message": format!("{code}: {description}, accept"),
                                    "code": code,
                                }))]);
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// `__kiana.promise("net.connect", { port, host, onEvent })`。
    ///
    /// host 缺省 `localhost`。写端通道与取消令牌在连接**之前**就登记，连接失败时移除它们
    /// 并 reject。失败只 reject、不推事件：`js/node/net.js` 从 rejection 里同时产出
    /// `error` 与 `close`，避免重复。
    ///
    /// 成功时 resolve `{socketId, localAddress, localPort}` 并推一条 `connect` 事件，然后
    /// 同一任务进入 `drive_socket` 的读写循环。
    pub fn handle_connect_kind(
        call: HostCall<'_>,
        state: &Rc<RefCell<NetState>>,
    ) -> EngineResult<()> {
        let payload: serde_json::Value = call.payload_json()?;
        let port = payload
            .get("port")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0) as u16;
        let host = payload
            .get("host")
            .and_then(|value| value.as_str())
            .filter(|host| !host.is_empty())
            .unwrap_or("localhost")
            .to_owned();
        let on_event: Function<'_> = payload_function(&call, "onEvent")?;
        let callback = Rc::new(call.completion_sender().capture(on_event));
        let sender = call.completion_sender();
        let id = call.id();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let socket_id = state.borrow().new_socket_id();
        let token = CancellationToken::new();
        state
            .borrow()
            .writers
            .borrow_mut()
            .insert(socket_id, writer_tx);
        state
            .borrow()
            .sockets
            .borrow_mut()
            .insert(socket_id, token.clone());
        let state = Rc::clone(state);

        tokio::task::spawn_local(async move {
            let address = bind_address(&host, port);
            match tokio::net::TcpStream::connect(address.as_str()).await {
                Ok(stream) => {
                    let local = stream.local_addr().ok();
                    let peer = stream.peer_addr().ok();
                    sender.resolve(
                        id,
                        HostValue::Json(serde_json::json!({
                            "socketId": socket_id,
                            "localAddress": local.map(|value| value.ip().to_string()).unwrap_or_default(),
                            "localPort": local.map(|value| value.port()).unwrap_or(0),
                        })),
                    );
                    callback.invoke(vec![HostValue::Json(serde_json::json!({
                        "type": "connect",
                        "socketId": socket_id,
                        "remoteAddress": peer.map(|value| value.ip().to_string()).unwrap_or_else(|| host.clone()),
                        "remotePort": peer.map(|value| value.port()).unwrap_or(port),
                        "remoteFamily": peer.map(|value| if value.is_ipv6() { "IPv6" } else { "IPv4" }).unwrap_or("IPv4"),
                        "localAddress": local.map(|value| value.ip().to_string()).unwrap_or_default(),
                        "localPort": local.map(|value| value.port()).unwrap_or(0),
                    }))]);
                    drive_socket(socket_id, stream, callback, state, token, Some(writer_rx)).await;
                }
                Err(failure) => {
                    state.borrow().writers.borrow_mut().remove(&socket_id);
                    state.borrow().sockets.borrow_mut().remove(&socket_id);
                    let (errno, code, description) = error::errno_info(&failure);
                    // Only reject: the JS shim emits a single 'error' then
                    // 'close' from the rejection (Node emits each once).
                    sender.reject(
                        id,
                        HostValue::Json(node_net_error(
                            code,
                            description,
                            errno,
                            "connect",
                            &host,
                            f64::from(port),
                        )),
                    );
                }
            }
        });
        Ok(())
    }

    /// 单个 socket 的读写循环，直到取消、EOF、读错误或写错误。
    ///
    /// 退出时统一清理：从 `NetState` 移除写端与取消令牌登记，然后推一条
    /// `{type:"close", socketId, hadError}`。`hadError` 只在写失败或读失败时为 true；
    /// `destroy` 取消与正常 EOF 都是 false。读到 EOF 时会先 `writer.shutdown()` 关闭写半
    /// 再退出（正常时对端会收到 FIN；shutdown 的错误被忽略）。
    ///
    /// `writer_rx` 为 `None` 时用 `future::pending` 占位，不再处理写命令。
    async fn drive_socket(
        socket_id: u64,
        stream: tokio::net::TcpStream,
        callback: Rc<crate::engine::HostCallback>,
        state: Rc<RefCell<NetState>>,
        token: CancellationToken,
        mut writer_rx: Option<mpsc::UnboundedReceiver<SocketCommand>>,
    ) {
        let (mut reader, mut writer) = stream.into_split();
        let mut buffer = vec![0u8; 65536];
        let mut had_error = false;
        let reader_open = true;
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                command = async {
                    match writer_rx.as_mut() {
                        Some(receiver) => receiver.recv().await,
                        None => std::future::pending::<Option<SocketCommand>>().await,
                    }
                } => {
                    match command {
                        Some(SocketCommand::Write(bytes)) => {
                            if writer.write_all(&bytes).await.is_err() {
                                had_error = true;
                                break;
                            }
                            let _ = writer.flush().await;
                        }
                        Some(SocketCommand::Shutdown) => {
                            let _ = writer.shutdown().await;
                        }
                        None => {
                            writer_rx = None;
                        }
                    }
                }
                result = reader.read(&mut buffer), if reader_open => {
                    match result {
                        Ok(0) => {
                            callback.invoke(vec![HostValue::Json(serde_json::json!({
                                "type": "end",
                                "socketId": socket_id,
                            }))]);
                            let _ = writer.shutdown().await;
                            break;
                        }
                        Ok(count) => {
                            callback.invoke(vec![HostValue::Json(serde_json::json!({
                                "type": "data",
                                "socketId": socket_id,
                                "b64": b64_encode(&buffer[..count]),
                            }))]);
                        }
                        Err(failure) => {
                            let (_, code, description) = error::errno_info(&failure);
                            had_error = true;
                            callback.invoke(vec![HostValue::Json(serde_json::json!({
                                "type": "error",
                                "socketId": socket_id,
                                "message": format!("{code}: {description}, read"),
                                "code": code,
                            }))]);
                            break;
                        }
                    }
                }
            }
        }
        state.borrow().writers.borrow_mut().remove(&socket_id);
        state.borrow().sockets.borrow_mut().remove(&socket_id);
        callback.invoke(vec![HostValue::Json(serde_json::json!({
            "type": "close",
            "socketId": socket_id,
            "hadError": had_error,
        }))]);
    }

    /// `netWrite`：把字节投给 socket 任务，返回是否入队成功。
    ///
    /// 真正的 `write_all` 在任务里执行；写失败不会返回到这里，只体现在随后 `close` 事件的
    /// `hadError` 上。未知 id 或通道关闭返回 `false`。
    pub fn write_socket(state: &Rc<RefCell<NetState>>, socket_id: u64, bytes: Vec<u8>) -> bool {
        let sender = state.borrow().writers.borrow().get(&socket_id).cloned();
        match sender {
            Some(sender) => sender.send(SocketCommand::Write(bytes)).is_ok(),
            None => false,
        }
    }

    /// `netEnd`：投递 `Shutdown`（关闭写半）。未知 id 静默忽略，无返回值。
    pub fn end_socket(state: &Rc<RefCell<NetState>>, socket_id: u64) {
        let sender = state.borrow().writers.borrow().get(&socket_id).cloned();
        if let Some(sender) = sender {
            let _ = sender.send(SocketCommand::Shutdown);
        }
    }

    /// `netDestroy`：取消 socket 令牌。任务在下一次 `select!` 时退出并清理，随后推
    /// `close`（`hadError=false`）。未知 id 静默忽略。
    pub fn destroy_socket(state: &Rc<RefCell<NetState>>, socket_id: u64) {
        let token = state.borrow().sockets.borrow().get(&socket_id).cloned();
        if let Some(token) = token {
            token.cancel();
        }
    }

    /// `netClose`：移除并取消 server 的 accept 循环；已经接受的连接不受影响。
    /// 未知 handle 静默忽略。
    pub fn close_server(state: &Rc<RefCell<NetState>>, handle: u64) {
        let token = state.borrow().servers.borrow_mut().remove(&handle);
        if let Some(token) = token {
            token.cancel();
        }
    }
}

// ---------------------------------------------------------------------------
// fetch：reqwest + 流式 chunk 事件（设计 §6.5、ARCHITECTURE §8 第 29 条）
// ---------------------------------------------------------------------------

mod fetch {
    // fetch 的宿主侧：reqwest 发请求，响应头一到就 resolve promise，响应体按 chunk 事件推给
    // JS（`js/prelude_node.js` 的 Response/ReadableStream 再把事件变成流）。
    //
    // 有两个 id，不能混用：payload 里的 `id` 是 JS 生成的流 id，用于 `fetchAbort(id)`；
    // `call.id()` 是引擎分配的 promise id，用于 resolve/reject。
    //
    // 取消在每个 await 点检查：响应头之前取消 → reject + 一条 `AbortError` 事件；
    // 响应体阶段取消 → 只推一条 `AbortError` 事件，不发 `end`。
    use std::cell::RefCell;
    use std::collections::HashMap;

    use futures::StreamExt as _;
    use rquickjs::Function;
    use tokio_util::sync::CancellationToken;

    use crate::engine::{EngineResult, HostCall, HostValue};

    use super::error;
    use super::{b64_decode, b64_encode, payload_function};

    /// 进行中的 fetch：流 id → 取消令牌。任务结束（成功/失败/取消）时移除对应项。
    #[derive(Default)]
    pub struct FetchState {
        tokens: RefCell<HashMap<u64, CancellationToken>>,
    }

    /// `__kiana.promise("fetch", { id, url, method, headers, body, onEvent })`。
    ///
    /// Promise 在**响应头**到达时 resolve（Node fetch 语义）；响应体通过 `onEvent` 的
    /// `chunk`/`end`/`error` 事件流式推送。
    ///
    /// 执行顺序与错误映射：
    /// 1. 解析 payload（`method`/`headers`/`body` 都是 JS 侧序列化好的形状，body 是 base64）；
    /// 2. 非法 method → reject `ERR_INVALID_HTTP_TOKEN` 的 TypeError（无事件）；
    ///    请求构造失败（URL 等）→ reject `ERR_INVALID_URL` 的 TypeError（无事件）；
    /// 3. `execute` 等响应头：超时 → `TimeoutError`，其它 → `TypeError`，同时推一条
    ///    `error` 事件；
    /// 4. resolve `{status, statusText, url, redirected, headers}`，然后逐 chunk 推
    ///    `{type:"chunk", b64}`，最后 `{type:"end"}`。
    ///
    /// 无法解析的 header 条目被静默跳过；响应头值用 lossy UTF-8（非 UTF-8 字节变成替换符）。
    pub fn handle_fetch_kind(
        call: HostCall<'_>,
        state: &std::rc::Rc<RefCell<FetchState>>,
    ) -> EngineResult<()> {
        let payload: serde_json::Value = call.payload_json()?;
        let id = payload
            .get("id")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0) as u64;
        let url = payload
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let method = payload
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or("GET")
            .to_owned();
        let headers: Vec<(String, String)> = payload
            .get("headers")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let pair = item.as_array()?;
                        Some((
                            pair.first()?.as_str()?.to_owned(),
                            pair.get(1)?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body = payload
            .get("body")
            .and_then(|value| value.as_str())
            .map(b64_decode);
        let on_event: Function<'_> = payload_function(&call, "onEvent")?;
        let callback = call.completion_sender().capture(on_event);
        let sender = call.completion_sender();
        let promise_id = call.id();
        let token = CancellationToken::new();
        state.borrow().tokens.borrow_mut().insert(id, token.clone());
        let state = std::rc::Rc::clone(state);

        tokio::task::spawn_local(async move {
            let client = kiana_core::http_dispatcher::shared_http_client();
            let method = match reqwest::Method::from_bytes(method.as_bytes()) {
                Ok(method) => method,
                Err(_) => {
                    sender.reject(
                        promise_id,
                        HostValue::Json(error::node_error(
                            "TypeError",
                            format!("Invalid HTTP method: {method}"),
                            Some("ERR_INVALID_HTTP_TOKEN"),
                            None,
                            None,
                            None,
                        )),
                    );
                    state.borrow().tokens.borrow_mut().remove(&id);
                    return;
                }
            };
            let mut builder = client.request(method, &url);
            for (name, value) in &headers {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                    reqwest::header::HeaderValue::from_str(value),
                ) {
                    builder = builder.header(name, value);
                }
            }
            if let Some(bytes) = body {
                builder = builder.body(reqwest::Body::from(bytes));
            }

            let request = match builder.build() {
                Ok(request) => request,
                Err(failure) => {
                    sender.reject(
                        promise_id,
                        HostValue::Json(error::node_error(
                            "TypeError",
                            format!("Failed to parse URL from {url}"),
                            Some("ERR_INVALID_URL"),
                            None,
                            None,
                            None,
                        )),
                    );
                    let _ = failure;
                    state.borrow().tokens.borrow_mut().remove(&id);
                    return;
                }
            };

            let response = tokio::select! {
                _ = token.cancelled() => {
                    sender.reject(
                        promise_id,
                        HostValue::Json(error::node_error(
                            "Error",
                            "This operation was aborted".to_owned(),
                            Some("ABORT_ERR"),
                            None,
                            None,
                            None,
                        )),
                    );
                    callback.invoke(vec![HostValue::Json(serde_json::json!({
                        "type": "error",
                        "name": "AbortError",
                        "message": "This operation was aborted",
                        "code": "ABORT_ERR",
                    }))]);
                    state.borrow().tokens.borrow_mut().remove(&id);
                    return;
                }
                result = client.execute(request) => {
                    match result {
                        Ok(response) => response,
                        Err(failure) => {
                            let name = if failure.is_timeout() { "TimeoutError" } else { "TypeError" };
                            sender.reject(
                                promise_id,
                                HostValue::Json(error::node_error(
                                    name,
                                    format!("fetch failed: {failure}"),
                                    None,
                                    None,
                                    None,
                                    None,
                                )),
                            );
                            callback.invoke(vec![HostValue::Json(serde_json::json!({
                                "type": "error",
                                "name": name,
                                "message": format!("fetch failed: {failure}"),
                            }))]);
                            state.borrow().tokens.borrow_mut().remove(&id);
                            return;
                        }
                    }
                }
            };

            let status = response.status();
            let status_text = status.canonical_reason().unwrap_or("").to_owned();
            let final_url = response.url().to_string();
            let redirected = response.url().as_str() != url;
            let mut header_pairs = Vec::new();
            for (name, value) in response.headers() {
                header_pairs.push(serde_json::json!([
                    name.as_str(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned()
                ]));
            }
            sender.resolve(
                promise_id,
                HostValue::Json(serde_json::json!({
                    "status": status.as_u16(),
                    "statusText": status_text,
                    "url": final_url,
                    "redirected": redirected,
                    "headers": header_pairs,
                })),
            );

            // 响应体循环：正常读完发 `end`；取消或流错误发 `error` 后直接 break，
            // 不会再补 `end`（终止事件只有一条）。
            let mut stream = response.bytes_stream();
            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        callback.invoke(vec![HostValue::Json(serde_json::json!({
                            "type": "error",
                            "name": "AbortError",
                            "message": "This operation was aborted",
                            "code": "ABORT_ERR",
                        }))]);
                        break;
                    }
                    chunk = stream.next() => {
                        match chunk {
                            Some(Ok(bytes)) => {
                                callback.invoke(vec![HostValue::Json(serde_json::json!({
                                    "type": "chunk",
                                    "b64": b64_encode(&bytes),
                                }))]);
                            }
                            Some(Err(failure)) => {
                                callback.invoke(vec![HostValue::Json(serde_json::json!({
                                    "type": "error",
                                    "name": "TypeError",
                                    "message": format!("fetch failed: {failure}"),
                                }))]);
                                break;
                            }
                            None => {
                                callback.invoke(vec![HostValue::Json(serde_json::json!({
                                    "type": "end",
                                }))]);
                                break;
                            }
                        }
                    }
                }
            }
            state.borrow().tokens.borrow_mut().remove(&id);
        });
        Ok(())
    }

    /// `__kiana_host.fetchAbort(id)`：取消对应流的令牌；未知 id 是静默 no-op。
    ///
    /// 只置位取消，不直接 reject/resolve——后续由请求任务自己按所处阶段发事件或 reject。
    pub fn abort(state: &std::rc::Rc<RefCell<FetchState>>, id: u64) {
        let token = state.borrow().tokens.borrow().get(&id).cloned();
        if let Some(token) = token {
            token.cancel();
        }
    }
}

// ---------------------------------------------------------------------------
// intl：Locale 解析、Segmenter、NumberFormat / DateTimeFormat（ICU4X 或 en-US）
// ---------------------------------------------------------------------------

mod intl {
    // ECMA-402 的 Rust 后端。分工：
    //
    // - `js/intl.js` 负责可观察的 JS 表面（构造器参数校验、locale 列表、`resolvedOptions`、
    //   `formatToParts` 的组装、`Date#toLocale*` 覆盖）；
    // - 本模块只做“给定 locale + options + 值，算出 JSON”的纯函数，经
    //   `__kiana_host.intlNumber`/`intlNumberBig`/`intlDate`/`segment` 暴露。
    //
    // 两条实现路径由 `locale-data` 特性切换：开启时数字用 ICU4X 的 `icu_decimal`，日期对
    // 非 en-US 用 `icu_datetime`（en-US 始终走手写实现，见 `date_output`）；关闭时全部走
    // 手写的 en-US 格式化。词粒度分段在开启时用 ICU4X 的 CJK 词典，关闭时退回
    // `unicode-segmentation` 的近似。
    //
    // 已知偏差集中在 `spec/design-extension-runtime.md` §7.4（零填充/分隔符、词典覆盖），
    // 测试对本地化文本按“已实测一致的 locale 才比字节”处理。
    use rquickjs::Function;
    use unicode_segmentation::UnicodeSegmentation as _;

    use crate::engine::{EngineResult, JsEngine};

    /// `LC_ALL > LANG > en-US`；只取 `ll_CC` / `ll-CC` 前缀（设计 §6.4）。
    ///
    /// 候选值依次尝试，第一个能通过 `normalize_locale` 的胜出；空串/全空白视为未设置。
    /// 不读 `LANGUAGE`（Node 不读它，`tests/intl.rs` 的 `language_variable_is_not_consulted`
    /// 锁定了这一点）。
    pub fn resolve_locale(lc_all: Option<&str>, lang: Option<&str>) -> String {
        for candidate in [lc_all, lang] {
            let Some(candidate) = candidate else { continue };
            let candidate = candidate.trim();
            if candidate.is_empty() {
                continue;
            }
            if let Some(normalized) = normalize_locale(candidate) {
                return normalized;
            }
        }
        "en-US".to_owned()
    }

    /// 从进程环境变量解析当前 locale。
    ///
    /// `std::env::var` 对非 Unicode 值返回 `Err`，这里视同未设置；每次调用都会重新读环境
    /// （`__kiana_host.locale` 由 JS 侧按需调用，不做缓存）。
    pub fn resolve_locale_from_env() -> String {
        let lc_all = std::env::var("LC_ALL").ok();
        let lang = std::env::var("LANG").ok();
        resolve_locale(lc_all.as_deref(), lang.as_deref())
    }

    /// 把 `zh_CN.UTF-8` / `en-US` / `de_DE@euro` 归一化成 `zh-CN` / `en-US` / `de-DE`。
    ///
    /// 先截掉 `.编码` 与 `@修饰符`；`C`/`POSIX`（大小写不敏感）映射成 `en-US`。语言段必须是
    /// 2-3 个 ASCII 字母，第二个子标签必须是 2 个字母或 3 个数字，否则返回 `None`（所以
    /// `zh-Hans-CN` 会因 `Hans` 是 4 个字母而失败）；第二个之后的子标签不再看。
    pub fn normalize_locale(input: &str) -> Option<String> {
        let mut base = input;
        if let Some((before, _)) = base.split_once('.') {
            base = before;
        }
        if let Some((before, _)) = base.split_once('@') {
            base = before;
        }
        let base = base.trim();
        if base.eq_ignore_ascii_case("c") || base.eq_ignore_ascii_case("posix") {
            return Some("en-US".to_owned());
        }
        let mut parts = base.split(['_', '-']);
        let language = parts.next()?.to_ascii_lowercase();
        if !(language.len() == 2 || language.len() == 3)
            || !language.chars().all(|ch| ch.is_ascii_alphabetic())
        {
            return None;
        }
        match parts.next() {
            Some(region) => {
                let region = region.to_ascii_uppercase();
                let is_alpha =
                    region.len() == 2 && region.chars().all(|ch| ch.is_ascii_alphabetic());
                let is_digits = region.len() == 3 && region.chars().all(|ch| ch.is_ascii_digit());
                if !is_alpha && !is_digits {
                    return None;
                }
                Some(format!("{language}-{region}"))
            }
            None => Some(language),
        }
    }

    /// 安装 `Intl` 用到的 7 个同步原语（`locale`/`timeZone`/`intlValidTimeZone`/`segment`/
    /// `intlNumber`/`intlNumberBig`/`intlDate`）。
    ///
    /// 要求 `__kiana_host` 已存在（由 `JsEngine::with_config` 注入）；返回值的字符串都是
    /// JSON，JS 侧 `js/intl.js` 负责解析与组装。必须在求值 `js/intl.js` 之前调用。
    pub fn install_primitives(engine: &JsEngine) -> EngineResult<()> {
        engine.with_js(|ctx| {
            let host: rquickjs::Object<'_> = ctx.globals().get("__kiana_host")?;
            host.set(
                "locale",
                Function::new(ctx.clone(), || -> String { resolve_locale_from_env() })?,
            )?;
            host.set(
                "timeZone",
                Function::new(ctx.clone(), || -> String { time_zone_name() })?,
            )?;
            host.set(
                "intlValidTimeZone",
                Function::new(ctx.clone(), |zone: String| -> bool {
                    valid_time_zone(&zone)
                })?,
            )?;
            host.set(
                "segment",
                Function::new(ctx.clone(), |text: String, granularity: String| -> String {
                    segment_json(&text, &granularity)
                })?,
            )?;
            host.set(
                "intlNumber",
                Function::new(
                    ctx.clone(),
                    |locale: String, options: String, value: f64| -> String {
                        number_format_json(&locale, &options, value)
                    },
                )?,
            )?;
            host.set(
                "intlNumberBig",
                Function::new(
                    ctx.clone(),
                    |locale: String, options: String, digits: String| -> String {
                        number_format_big_json(&locale, &options, &digits)
                    },
                )?,
            )?;
            host.set(
                "intlDate",
                Function::new(
                    ctx.clone(),
                    |locale: String, options: String, epoch_ms: f64| -> String {
                        date_format_json(&locale, &options, epoch_ms)
                    },
                )?,
            )?;
            Ok(())
        })
    }

    /// `Intl.Segmenter` 的原始分段：`[{segment, index, isWordLike?}]`（UTF-16 偏移）。
    ///
    /// `granularity` 只识别 `"word"`/`"sentence"`，其它值（含 `"grapheme"`）都按字素切分。
    /// `index` 是 **UTF-16 码元**偏移（`String#length` 的度量），不是字节偏移；只有 word
    /// 粒度会输出 `isWordLike` 字段。`input` 字段由 JS 侧补，不在 Rust 输出里。
    pub fn segment_json(text: &str, granularity: &str) -> String {
        let mut segments = Vec::new();
        match granularity {
            "word" => {
                word_segments(text, &mut segments);
            }
            "sentence" => {
                for (index, segment) in text.split_sentence_bound_indices() {
                    segments.push((segment.to_owned(), utf16_len(&text[..index]), None));
                }
            }
            _ => {
                for (index, segment) in text.grapheme_indices(true) {
                    segments.push((segment.to_owned(), utf16_len(&text[..index]), None));
                }
            }
        }
        let values: Vec<serde_json::Value> = segments
            .into_iter()
            .map(|(segment, index, word_like)| {
                let mut map = serde_json::Map::new();
                map.insert("segment".into(), serde_json::Value::String(segment));
                map.insert("index".into(), serde_json::Value::from(index));
                if let Some(word_like) = word_like {
                    map.insert("isWordLike".into(), serde_json::Value::Bool(word_like));
                }
                serde_json::Value::Object(map)
            })
            .collect();
        serde_json::Value::Array(values).to_string()
    }

    fn utf16_len(text: &str) -> usize {
        text.encode_utf16().count()
    }

    /// Word granularity: ICU4X's UAX#29 rules plus the CJK dictionary, matching
    /// Node/ICU segmentation for scripts without spaces. Falls back to
    /// `unicode-segmentation` when locale data is compiled out.
    #[cfg(feature = "locale-data")]
    fn word_segments(text: &str, segments: &mut Vec<(String, usize, Option<bool>)>) {
        use icu_segmenter::WordSegmenter;
        use icu_segmenter::options::WordBreakInvariantOptions;

        let segmenter = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
        let mut start = 0usize;
        let mut start_utf16 = 0usize;
        for (index, word_type) in segmenter.segment_str(text).iter_with_word_type() {
            if index == 0 {
                continue;
            }
            let segment = &text[start..index];
            segments.push((
                segment.to_owned(),
                start_utf16,
                Some(word_type.is_word_like()),
            ));
            start = index;
            start_utf16 += segment.encode_utf16().count();
        }
    }

    /// 无 `locale-data` 时的近似实现：按 `unicode-segmentation` 的 UAX#29 切词，
    /// `isWordLike` 用“片段里含任意字母或数字”近似 ICU 的判定（因此标点/空格是 false，
    /// 但 ICU 对某些脚本的更细分类可能不同）。
    #[cfg(not(feature = "locale-data"))]
    fn word_segments(text: &str, segments: &mut Vec<(String, usize, Option<bool>)>) {
        for (index, segment) in text.split_word_bound_indices() {
            let word_like = segment.chars().any(|ch| ch.is_alphanumeric());
            segments.push((
                segment.to_owned(),
                utf16_len(&text[..index]),
                Some(word_like),
            ));
        }
    }

    // ---- NumberFormat -----------------------------------------------------

    /// 解析后的 `Intl.NumberFormat` 选项：只保留 Rust 格式化需要的字段，字符串字段沿用
    /// JS 传入的规范值（`js/intl.js` 已做过取值校验）。
    #[derive(Default)]
    struct NumberOptions {
        style: String,
        currency: String,
        currency_display: String,
        unit: String,
        unit_display: String,
        notation: String,
        minimum_integer_digits: u32,
        minimum_fraction_digits: Option<u32>,
        maximum_fraction_digits: Option<u32>,
        use_grouping: String,
        sign_display: String,
    }

    /// 把 `JSON.stringify(options)` 解析成 `NumberOptions`。
    ///
    /// 缺省值：`style = decimal`、`currencyDisplay = symbol`、`unitDisplay = short`、
    /// `notation = standard`、`minimumIntegerDigits = 1`、`useGrouping = auto`、
    /// `signDisplay = auto`。`useGrouping` 的布尔值按 Intl 规范映射（`false`→`"false"`，
    /// `true`→`"always"`）。JSON 解析失败按 `Null` 处理，即全部取缺省值。
    fn parse_number_options(options_json: &str) -> NumberOptions {
        let value: serde_json::Value =
            serde_json::from_str(options_json).unwrap_or(serde_json::Value::Null);
        let text = |key: &str, fallback: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or(fallback)
                .to_owned()
        };
        let use_grouping = match value.get("useGrouping") {
            Some(serde_json::Value::Bool(false)) => "false".to_owned(),
            Some(serde_json::Value::Bool(true)) => "always".to_owned(),
            Some(serde_json::Value::String(text)) => text.clone(),
            _ => "auto".to_owned(),
        };
        NumberOptions {
            style: text("style", "decimal"),
            currency: text("currency", ""),
            currency_display: text("currencyDisplay", "symbol"),
            unit: text("unit", ""),
            unit_display: text("unitDisplay", "short"),
            notation: text("notation", "standard"),
            minimum_integer_digits: value
                .get("minimumIntegerDigits")
                .and_then(|value| value.as_f64())
                .map(|value| value.max(0.0) as u32)
                .unwrap_or(1),
            minimum_fraction_digits: value
                .get("minimumFractionDigits")
                .and_then(|value| value.as_f64())
                .map(|value| value.max(0.0) as u32),
            maximum_fraction_digits: value
                .get("maximumFractionDigits")
                .and_then(|value| value.as_f64())
                .map(|value| value.max(0.0) as u32),
            use_grouping,
            sign_display: text("signDisplay", "auto"),
        }
    }

    /// 取 locale 的语言段（第一个 `-`/`_` 之前）。不做大小写归一；调用方传进来的都是
    /// `js/intl.js` 规范化过的小写语言 + 大写区域。
    fn language_of(locale: &str) -> &str {
        locale.split(['-', '_']).next().unwrap_or("")
    }

    /// 追加一个 part；空值不产生 part，且相邻同 kind 的片段会被合并。
    ///
    /// 合并是必要的：`plain_decimal_parts` 里的分组循环会逐字符累积 `integer`，它们必须
    /// 合成一个 part，而不是每个字符一个 part。
    fn push_part(parts: &mut Vec<(String, String)>, kind: &str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        if let Some(last) = parts.last_mut()
            && last.0 == kind
        {
            last.1.push_str(&value);
            return;
        }
        parts.push((kind.to_owned(), value));
    }

    /// `[(kind, value)]` → `[[kind, value], …]`（JS 侧再包成 `{type, value}` 对象）。
    fn parts_to_json(parts: &[(String, String)]) -> Vec<serde_json::Value> {
        parts
            .iter()
            .map(|(kind, value)| serde_json::json!([kind, value]))
            .collect()
    }

    /// `__kiana_host.intlNumber(locale, options, value)`：`{ text, parts }`。
    ///
    /// `NaN`/`±Infinity` 在进入 locale 逻辑之前就短路：文本是 `NaN`/`∞`/`-∞`，`parts` 只有
    /// 一项 `["nan", text]`（不套符号、分组或货币）；`-0.0` 会走到 `format_number` 的
    /// `negative` 判断（`is_sign_negative() && number != 0.0` 为 false）。
    pub fn number_format_json(locale: &str, options_json: &str, value: f64) -> String {
        let options = parse_number_options(options_json);
        if !value.is_finite() {
            let text = if value.is_nan() {
                "NaN".to_owned()
            } else if value > 0.0 {
                "\u{221e}".to_owned()
            } else {
                "-\u{221e}".to_owned()
            };
            return serde_json::json!({ "text": text, "parts": [["nan", text]] }).to_string();
        }
        let (text, parts) = format_number(locale, &options, value);
        serde_json::json!({ "text": text, "parts": parts_to_json(&parts) }).to_string()
    }

    /// BigInt 路径：按十进制字符串格式化（保留全部精度，不经过 f64）。
    ///
    /// 与 `format_number` 的差别：不做舍入（BigInt 没有小数），percent 直接在整数后补两个
    /// `0`（等于 ×100），currency 按 ISO 小数位补零；**不应用** `minimumIntegerDigits`
    /// （这里没有调用 `pad_integer_digits`）。符号、分组与样式后缀与 f64 路径共用。
    pub fn number_format_big_json(locale: &str, options_json: &str, digits: &str) -> String {
        let options = parse_number_options(options_json);
        let negative = digits.starts_with('-');
        let mut integer = digits.trim_start_matches('-').to_owned();
        if integer.is_empty() {
            integer.push('0');
        }
        let mut fraction: Option<String> = None;
        match options.style.as_str() {
            "percent" => {
                integer.push_str("00");
            }
            "currency" => {
                let (min, max) = currency_fraction_digits(&options.currency);
                let count = max.max(min);
                if count > 0 {
                    fraction = Some("0".repeat(count as usize));
                }
            }
            _ => {}
        }
        let decimal_text = match &fraction {
            Some(fraction) => format!("{integer}.{fraction}"),
            None => integer.clone(),
        };
        let grouping = !matches!(options.use_grouping.as_str(), "false" | "never");
        let mut parts = Vec::new();
        if let Some(sign) = sign_text(locale, &options.sign_display, negative, negative) {
            parts.push(("minusSign".to_owned(), sign));
        }
        let number_parts = decimal_parts(locale, &decimal_text, grouping);
        let number_is_one = integer == "1";
        let styled = apply_style(locale, &options, number_is_one, number_parts, None);
        parts.extend(styled);
        let text = parts
            .iter()
            .map(|(_, value)| value.as_str())
            .collect::<String>();
        serde_json::json!({ "text": text, "parts": parts_to_json(&parts) }).to_string()
    }

    /// f64 主路径：percent 先 ×100；scientific/engineering 直接转交 `format_scientific`；
    /// compact 按 1e3/1e6/1e9/1e12 缩放并取语言后缀；随后按 style 决定默认小数位
    /// （percent 0/0、currency 按 ISO、其它 0/3，compact 时 0/1），用 `round_decimal` 对
    /// `f64::to_string()` 的十进制文本舍入，最后交给 `apply_style` 加符号与样式。
    ///
    /// 返回 `(完整文本, parts)`；文本就是 parts 的拼接，便于 JS 侧直接使用。
    fn format_number(
        locale: &str,
        options: &NumberOptions,
        value: f64,
    ) -> (String, Vec<(String, String)>) {
        let style = options.style.as_str();
        let mut number = if style == "percent" {
            value * 100.0
        } else {
            value
        };
        let negative = number.is_sign_negative() && number != 0.0;

        if options.notation == "scientific" || options.notation == "engineering" {
            return format_scientific(locale, options, number, negative);
        }

        let mut compact = None;
        let mut compact_max_frac = None;
        if options.notation == "compact" {
            let abs = number.abs();
            let size = if abs >= 1e12 {
                Some((1e12, compact_suffix(locale, 12)))
            } else if abs >= 1e9 {
                Some((1e9, compact_suffix(locale, 9)))
            } else if abs >= 1e6 {
                Some((1e6, compact_suffix(locale, 6)))
            } else if abs >= 1e3 {
                Some((1e3, compact_suffix(locale, 3)))
            } else {
                None
            };
            if let Some((divisor, suffix)) = size {
                number /= divisor;
                compact = suffix;
                // CLDR compact notation uses two significant digits.
                let abs = number.abs();
                if abs > 0.0 {
                    let magnitude = abs.log10().floor() as i32;
                    compact_max_frac = Some((1 - magnitude).max(0) as u32);
                }
            }
        }

        let (default_min, default_max) = match style {
            "percent" => (0, 0),
            "currency" => currency_fraction_digits(&options.currency),
            _ => {
                if compact.is_some() {
                    (0, 1)
                } else {
                    (0, 3)
                }
            }
        };
        let mut max_frac = options.maximum_fraction_digits.unwrap_or(default_max);
        let mut min_frac = options.minimum_fraction_digits.unwrap_or(default_min);
        if compact.is_some()
            && options.maximum_fraction_digits.is_none()
            && let Some(sig) = compact_max_frac
        {
            max_frac = sig;
        }
        min_frac = min_frac.min(max_frac);

        let decimal_text = round_decimal(&number.abs().to_string(), max_frac, min_frac);
        let decimal_text = pad_integer_digits(&decimal_text, options.minimum_integer_digits);
        let grouping = !matches!(options.use_grouping.as_str(), "false" | "never");
        let number_parts = decimal_parts(locale, &decimal_text, grouping);

        let mut parts: Vec<(String, String)> = Vec::new();
        if let Some(sign) = sign_text(locale, &options.sign_display, negative, number == 0.0) {
            parts.push(("minusSign".to_owned(), sign));
        }
        parts.extend(apply_style(
            locale,
            options,
            number.abs() == 1.0,
            number_parts,
            compact,
        ));
        let text = parts
            .iter()
            .map(|(_, value)| value.as_str())
            .collect::<String>();
        (text, parts)
    }

    /// scientific（指数 = floor(log10)）与 engineering（指数取 3 的倍数）共用。
    ///
    /// 尾数用 `round_decimal` 舍入；指数分隔符固定 `"E"`，负指数带 `-`、正指数不带 `+`；
    /// 尾数部分仍按 locale 分组（尽管科学计数法里通常只有一位整数）。
    fn format_scientific(
        locale: &str,
        options: &NumberOptions,
        number: f64,
        negative: bool,
    ) -> (String, Vec<(String, String)>) {
        let abs = number.abs();
        let mut exponent = if abs > 0.0 {
            abs.log10().floor() as i32
        } else {
            0
        };
        if options.notation == "engineering" {
            exponent = exponent.div_euclid(3) * 3;
        }
        let mantissa = if exponent == 0 {
            abs
        } else {
            abs / 10f64.powi(exponent)
        };
        let max_frac = options.maximum_fraction_digits.unwrap_or(3);
        let min_frac = options.minimum_fraction_digits.unwrap_or(0).min(max_frac);
        let decimal_text = round_decimal(&mantissa.to_string(), max_frac, min_frac);
        let grouping = !matches!(options.use_grouping.as_str(), "false" | "never");
        let mut parts: Vec<(String, String)> = Vec::new();
        if let Some(sign) = sign_text(locale, &options.sign_display, negative, number == 0.0) {
            parts.push(("minusSign".to_owned(), sign));
        }
        parts.extend(decimal_parts(locale, &decimal_text, grouping));
        parts.push(("exponentSeparator".to_owned(), "E".to_owned()));
        let exponent_text = if exponent < 0 {
            format!("-{}", -exponent)
        } else {
            exponent.to_string()
        };
        parts.push(("exponent".to_owned(), exponent_text));
        let text = parts
            .iter()
            .map(|(_, value)| value.as_str())
            .collect::<String>();
        (text, parts)
    }

    /// 数字部分之后套用 style（货币/单位/百分比）与 compact 后缀。
    ///
    /// 位置规则是硬编码的语言表：货币后缀语言（`locale_places_currency_suffix`）用 NBSP
    /// 连接，前缀语言把符号放最前（`currencyDisplay: "code"` 时符号后再加一个 NBSP）；
    /// percent 在 tr/fa 前置，其它语言按 `percent_uses_space` 决定是否加 NBSP；
    /// unit 与 compact 都追加在数字之后。
    fn apply_style(
        locale: &str,
        options: &NumberOptions,
        number_is_one: bool,
        mut number_parts: Vec<(String, String)>,
        compact: Option<String>,
    ) -> Vec<(String, String)> {
        match options.style.as_str() {
            "currency" => {
                let display = options.currency_display.as_str();
                if display == "name" {
                    number_parts.push(("literal".to_owned(), " ".to_owned()));
                    number_parts.push((
                        "currency".to_owned(),
                        currency_name(&options.currency, !number_is_one),
                    ));
                } else {
                    let token = currency_token(locale, options);
                    if locale_places_currency_suffix(locale) {
                        number_parts.push(("literal".to_owned(), "\u{a0}".to_owned()));
                        number_parts.push(("currency".to_owned(), token));
                    } else {
                        let mut prefixed = Vec::with_capacity(number_parts.len() + 2);
                        prefixed.push(("currency".to_owned(), token));
                        if display == "code" {
                            prefixed.push(("literal".to_owned(), "\u{a0}".to_owned()));
                        }
                        prefixed.extend(number_parts);
                        number_parts = prefixed;
                    }
                }
            }
            "unit" => {
                let label = unit_label(locale, options, number_is_one);
                number_parts.push(("literal".to_owned(), " ".to_owned()));
                number_parts.push(("unit".to_owned(), label));
            }
            "percent" => {
                if matches!(language_of(locale), "tr" | "fa") {
                    number_parts.insert(0, ("percent".to_owned(), percent_symbol(locale)));
                } else {
                    if percent_uses_space(locale) {
                        number_parts.push(("literal".to_owned(), "\u{a0}".to_owned()));
                    }
                    number_parts.push(("percent".to_owned(), percent_symbol(locale)));
                }
            }
            _ => {}
        }
        if let Some(suffix) = compact {
            number_parts.push(("compact".to_owned(), suffix));
        }
        number_parts
    }

    /// 百分号：`ar` 用 U+066A ARABIC PERCENT SIGN 加 U+061C ARABIC LETTER MARK，其它 `%`。
    fn percent_symbol(locale: &str) -> String {
        if language_of(locale) == "ar" {
            "\u{066a}\u{061c}".to_owned()
        } else {
            "%".to_owned()
        }
    }

    /// 百分号前是否加 NBSP：硬编码的语言名单（CLDR 的常见写法），不在名单里的语言不加。
    fn percent_uses_space(locale: &str) -> bool {
        matches!(
            language_of(locale),
            "de" | "fr"
                | "ru"
                | "sv"
                | "fi"
                | "pl"
                | "cs"
                | "da"
                | "nb"
                | "nn"
                | "es"
                | "it"
                | "pt"
                | "nl"
                | "uk"
                | "ca"
                | "ro"
                | "hu"
                | "sk"
                | "sl"
                | "lt"
                | "lv"
                | "et"
                | "is"
                | "el"
                | "bg"
                | "hr"
        )
    }

    /// `sv`/`fi` 等使用 U+2212 MINUS SIGN；`ar` 前置 U+061C ARABIC LETTER MARK。
    ///
    /// `display` 决定何时输出：`never` 从不、`always` 总是（正数给 `+`）、`exceptZero` 仅
    /// 非零、`negative`/`negativeZero` 仅负数、缺省同 `negative`。`is_zero` 由调用方按
    /// “数值等于 0”传入；`-0.0` 在 f64 主路径里不会被视为负数。
    fn sign_text(locale: &str, display: &str, negative: bool, is_zero: bool) -> Option<String> {
        let prefix = if negative { minus_char(locale) } else { "+" };
        match display {
            "never" => None,
            "always" => Some(prefix.to_owned()),
            "exceptZero" => {
                if is_zero {
                    None
                } else {
                    Some(prefix.to_owned())
                }
            }
            "negative" | "negativeZero" => {
                if negative {
                    Some(prefix.to_owned())
                } else {
                    None
                }
            }
            _ => {
                if negative {
                    Some(prefix.to_owned())
                } else {
                    None
                }
            }
        }
    }

    /// 负号字符：`ar` 是 U+061C + U+002D，`sv`/`fi`/`nb`/`nn`/`da` 是 U+2212，其它 U+002D。
    fn minus_char(locale: &str) -> &'static str {
        match language_of(locale) {
            "ar" => "\u{061c}-",
            "sv" | "fi" | "nb" | "nn" | "da" => "\u{2212}",
            _ => "-",
        }
    }

    /// compact 后缀：按语言查硬编码表（对应 1e3/1e6/1e9/1e12 四个量级），
    /// 未知语言回退 `K`/`M`/`B`/`T`；其它量级返回 `None`。
    fn compact_suffix(locale: &str, magnitude: i32) -> Option<String> {
        let lang = language_of(locale);
        let table: &[(&str, [&str; 4])] = &[
            ("en", ["K", "M", "B", "T"]),
            ("de", ["\u{202f}Tsd.", "Mio.", "Mrd.", "Bio."]),
            ("fr", ["\u{202f}k", "\u{202f}M", "\u{202f}Md", "\u{202f}Bn"]),
            ("es", ["\u{a0}mil", "\u{a0}M", "\u{a0}mil M", "\u{a0}B"]),
            ("it", ["\u{a0}min", "\u{a0}mln", "\u{a0}mld", "\u{a0}mln"]),
            ("pt", ["\u{a0}mil", "\u{a0}mi", "\u{a0}bi", "\u{a0}tri"]),
            (
                "ru",
                ["\u{a0}тыс.", "\u{a0}млн", "\u{a0}млрд", "\u{a0}трлн"],
            ),
            ("ja", ["万", "億", "兆", "京"]),
            ("zh", ["万", "亿", "万亿", "兆"]),
            ("ko", ["만", "억", "조", "경"]),
            ("hi", [" हज़ार", " लाख", " करोड़", " अरब"]),
            ("fi", [" t.", " milj.", " mrd.", " bilj."]),
            ("sv", [" tn", " mn", " md", " bn"]),
            ("da", ["\u{a0}t", "\u{a0}mia.", "\u{a0}mia.", "\u{a0}bn"]),
            (
                "nb",
                ["\u{a0}tis.", "\u{a0}mill.", "\u{a0}mrd.", "\u{a0}bill."],
            ),
            ("nl", ["\u{a0}K", "\u{a0}mln", "\u{a0}mld", "\u{a0}bln"]),
            ("pl", ["\u{a0}tys.", "\u{a0}mln", "\u{a0}mld", "\u{a0}bln"]),
        ];
        let index = match magnitude {
            3 => 0usize,
            6 => 1usize,
            9 => 2usize,
            12 => 3usize,
            _ => return None,
        };
        match table.iter().find(|(code, _)| *code == lang) {
            Some((_, suffixes)) => Some(suffixes[index].to_owned()),
            None => Some(["K", "M", "B", "T"][index].to_owned()),
        }
    }

    /// ISO 4217 的常用小数位：0 位与 3 位货币的硬编码名单，其余（含未知代码）默认 2 位。
    /// 返回 `(min, max)`；表内每对数值都相等，两个使用方（f64 与 BigInt 路径）据此决定
    /// 默认的最少/最多小数位。
    fn currency_fraction_digits(code: &str) -> (u32, u32) {
        match code {
            "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF"
            | "UGX" | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => (0, 0),
            "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => (3, 3),
            _ => (2, 2),
        }
    }

    /// 对 Rust 的十进制文本做 half-expand（Intl 的默认舍入）与补零/去零。
    ///
    /// 输入是 `f64::to_string()` 的最短十进制表示（`format_number` 与 `format_scientific`
    /// 都这样传）。算法只看第一个被丢弃的数字（`>= '5'` 进位），没有向后续位看的二次判断，
    /// 因此是十进制文本上的 half-up，而不是对二进制精确值做十进制舍入。
    /// 进位会一路传进整数部分并在需要时插入前导 `1`；最后把小数尾部的 0 删到 `min_frac`
    /// 位为止。
    fn round_decimal(text: &str, max_frac: u32, min_frac: u32) -> String {
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (integer, fraction) = match unsigned.split_once('.') {
            Some((integer, fraction)) => (integer, fraction),
            None => (unsigned, ""),
        };
        let integer = if integer.is_empty() { "0" } else { integer };
        let mut digits: Vec<u8> = fraction.bytes().take(max_frac as usize).collect();
        let round_up = if fraction.len() > max_frac as usize {
            fraction.as_bytes()[max_frac as usize] >= b'5'
        } else {
            false
        };
        while digits.len() < max_frac as usize {
            digits.push(b'0');
        }
        let mut integer_digits: Vec<u8> = integer.bytes().collect();
        if round_up {
            let mut index = digits.len();
            loop {
                if index == 0 {
                    let mut carry = true;
                    let mut position = integer_digits.len();
                    while carry && position > 0 {
                        position -= 1;
                        if integer_digits[position] == b'9' {
                            integer_digits[position] = b'0';
                        } else {
                            integer_digits[position] += 1;
                            carry = false;
                        }
                    }
                    if carry {
                        integer_digits.insert(0, b'1');
                    }
                    break;
                }
                index -= 1;
                if digits[index] == b'9' {
                    digits[index] = b'0';
                } else {
                    digits[index] += 1;
                    break;
                }
            }
        }
        while digits.len() > min_frac as usize && digits.last() == Some(&b'0') {
            digits.pop();
        }
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        out.push_str(std::str::from_utf8(&integer_digits).unwrap_or("0"));
        if !digits.is_empty() {
            out.push('.');
            out.push_str(std::str::from_utf8(&digits).unwrap_or(""));
        }
        out
    }

    /// 在整数部分左侧补 0 到 `minimum` 位（`minimum <= 1` 时原样返回）；小数部分保留。
    fn pad_integer_digits(text: &str, minimum: u32) -> String {
        if minimum <= 1 {
            return text.to_owned();
        }
        let (integer, rest) = match text.split_once('.') {
            Some((integer, rest)) => (integer, Some(rest)),
            None => (text, None),
        };
        let mut padded = String::new();
        for _ in integer.len()..minimum as usize {
            padded.push('0');
        }
        padded.push_str(integer);
        match rest {
            Some(rest) => format!("{padded}.{rest}"),
            None => padded,
        }
    }

    /// 用 ICU4X 的 `DecimalFormatter` 产生 parts；locale 解析失败、文本解析失败或格式化
    /// 失败时回退到 `plain_decimal_parts(text, '.', ',', grouping)`（即 en-US 的写法）。
    #[cfg(feature = "locale-data")]
    fn decimal_parts(locale: &str, text: &str, grouping: bool) -> Vec<(String, String)> {
        use icu_decimal::DecimalFormatter;
        use icu_decimal::input::Decimal;
        use icu_decimal::options::{DecimalFormatterOptions, GroupingStrategy};
        use std::str::FromStr as _;

        let Ok(decimal) = Decimal::from_str(text) else {
            return plain_decimal_parts(text, '.', ',', grouping);
        };
        let locale = icu_locale::Locale::try_from_str(locale)
            .unwrap_or_else(|_| "en-US".parse().expect("en-US"));
        let mut options = DecimalFormatterOptions::default();
        options.grouping_strategy = Some(if grouping {
            GroupingStrategy::Auto
        } else {
            GroupingStrategy::Never
        });
        match DecimalFormatter::try_new(locale.into(), options) {
            Ok(formatter) => {
                let formatted = formatter.format(&decimal);
                parts_from_writeable(&formatted)
                    .unwrap_or_else(|| plain_decimal_parts(text, '.', ',', grouping))
            }
            Err(_) => plain_decimal_parts(text, '.', ',', grouping),
        }
    }

    /// 无 `locale-data`：所有 locale 都按 en-US 的 `.` 与 `,` 处理。
    #[cfg(not(feature = "locale-data"))]
    fn decimal_parts(_locale: &str, text: &str, grouping: bool) -> Vec<(String, String)> {
        plain_decimal_parts(text, '.', ',', grouping)
    }

    /// 纯文本分组：从右往左每 3 位插一个 `group` part，小数用单独的 `decimal` part 分隔。
    ///
    /// 只按 ASCII 字节处理，输入来自上面的格式化路径（不会含非 ASCII 数字）；`integer` 的
    /// 相邻片段会由 `push_part` 合并。
    fn plain_decimal_parts(
        text: &str,
        decimal_separator: char,
        grouping_separator: char,
        grouping: bool,
    ) -> Vec<(String, String)> {
        let (integer, fraction) = match text.split_once('.') {
            Some((integer, fraction)) => (integer, Some(fraction)),
            None => (text, None),
        };
        let mut parts = Vec::new();
        if grouping {
            let bytes = integer.as_bytes();
            let mut chunk = String::new();
            for (index, byte) in bytes.iter().enumerate() {
                if index > 0 && (bytes.len() - index) % 3 == 0 {
                    push_part(&mut parts, "integer", std::mem::take(&mut chunk));
                    push_part(&mut parts, "group", grouping_separator.to_string());
                }
                chunk.push(*byte as char);
            }
            push_part(&mut parts, "integer", chunk);
        } else {
            push_part(&mut parts, "integer", integer.to_owned());
        }
        if let Some(fraction) = fraction {
            push_part(&mut parts, "decimal", decimal_separator.to_string());
            push_part(&mut parts, "fraction", fraction.to_owned());
        }
        parts
    }

    /// 把 `Writeable` 的 part 注解转成 `[[type, value], …]`。
    ///
    /// 做法：`write_to_parts` 收集所有 span 的字节区间，用区间端点把文本切成互不重叠的小段；
    /// 每段取“包含它且起点最大”的 span 的类型，找不到时是 `literal`（span 嵌套时取最内层，
    /// 同起点时后遍历到的覆盖先前的）。`write_to_parts` 失败返回 `None`，调用点回退到手写路径。
    ///
    /// 只在 `locale-data` 编译时使用。
    #[cfg(feature = "locale-data")]
    fn parts_from_writeable<W: writeable::Writeable + ?Sized>(
        value: &W,
    ) -> Option<Vec<(String, String)>> {
        use std::fmt;
        use writeable::PartsWrite;

        struct Sink {
            text: String,
            spans: Vec<(usize, usize, &'static str)>,
        }
        impl fmt::Write for Sink {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.text.push_str(text);
                Ok(())
            }
        }
        impl PartsWrite for Sink {
            type SubPartsWrite = Sink;
            fn with_part(
                &mut self,
                part: writeable::Part,
                mut f: impl FnMut(&mut Sink) -> fmt::Result,
            ) -> fmt::Result {
                let start = self.text.len();
                f(self)?;
                let end = self.text.len();
                if end > start {
                    self.spans.push((start, end, part.value));
                }
                Ok(())
            }
        }

        let mut sink = Sink {
            text: String::new(),
            spans: Vec::new(),
        };
        value.write_to_parts(&mut sink).ok()?;
        let mut cuts: Vec<usize> = vec![0, sink.text.len()];
        for (start, end, _) in &sink.spans {
            cuts.push(*start);
            cuts.push(*end);
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut parts = Vec::new();
        for window in cuts.windows(2) {
            let (start, end) = (window[0], window[1]);
            if start >= end {
                continue;
            }
            let mut kind = "literal";
            let mut best = 0usize;
            for (span_start, span_end, value) in &sink.spans {
                if *span_start <= start && end <= *span_end && *span_start >= best {
                    kind = value;
                    best = *span_start;
                }
            }
            let chunk = sink.text.get(start..end).unwrap_or("");
            push_part(&mut parts, kind, chunk);
        }
        Some(parts)
    }

    /// 货币符号的硬编码表（只覆盖常见货币与少量语言/地区特例）。
    ///
    /// 未知代码返回 `None`，调用方回退成代码本身；`USD`/`JPY` 有按语言区分的变体。
    fn currency_symbol(locale: &str, code: &str) -> Option<&'static str> {
        let language = language_of(locale);
        Some(match code {
            "USD" => {
                if locale.starts_with("en-GB") {
                    "US$"
                } else if language == "fr" {
                    "$US"
                } else {
                    "$"
                }
            }
            "JPY" => {
                if language == "zh" {
                    "JP\u{a5}"
                } else if language == "fr" {
                    "JPY"
                } else {
                    "\u{a5}"
                }
            }
            "EUR" => "\u{20ac}",
            "GBP" => "\u{a3}",
            "CNY" => "CN\u{a5}",
            "KRW" => "\u{20a9}",
            "INR" => "\u{20b9}",
            "RUB" => "\u{20bd}",
            "BRL" => "R$",
            "CAD" => "CA$",
            "AUD" => "A$",
            "CHF" => "CHF",
            "HKD" => "HK$",
            "SGD" => "SGD",
            "NZD" => "NZ$",
            "SEK" => "SEK",
            "NOK" => "NOK",
            "DKK" => "DKK",
            _ => return None,
        })
    }

    /// 货币英文全名（单/复数）；未知代码原样返回代码。
    fn currency_name(code: &str, plural: bool) -> String {
        let (singular, plural_form) = match code {
            "USD" => ("US dollar", "US dollars"),
            "EUR" => ("euro", "euros"),
            "GBP" => ("British pound", "British pounds"),
            "JPY" => ("Japanese yen", "Japanese yen"),
            "CNY" => ("Chinese yuan", "Chinese yuan"),
            "KRW" => ("South Korean won", "South Korean won"),
            "INR" => ("Indian rupee", "Indian rupees"),
            "RUB" => ("Russian ruble", "Russian rubles"),
            "CAD" => ("Canadian dollar", "Canadian dollars"),
            "AUD" => ("Australian dollar", "Australian dollars"),
            "CHF" => ("Swiss franc", "Swiss francs"),
            "BRL" => ("Brazilian real", "Brazilian reals"),
            other => return other.to_owned(),
        };
        if plural { plural_form } else { singular }.to_owned()
    }

    /// 按 `currencyDisplay` 选货币 token：`code` → 代码、`name` → 复数全名、
    /// `narrowSymbol`/其它 → 符号（未知代码回退成代码）。
    fn currency_token(locale: &str, options: &NumberOptions) -> String {
        match options.currency_display.as_str() {
            "code" => options.currency.clone(),
            "name" => currency_name(&options.currency, true),
            "narrowSymbol" => currency_symbol(locale, &options.currency)
                .map(str::to_owned)
                .unwrap_or_else(|| options.currency.clone()),
            _ => currency_symbol(locale, &options.currency)
                .map(str::to_owned)
                .unwrap_or_else(|| options.currency.clone()),
        }
    }

    /// 货币符号放在数字**之后**的语言名单（硬编码）：命中时 `apply_style` 在数字后加 NBSP
    /// 与符号，未命中的语言把符号放前面。
    fn locale_places_currency_suffix(locale: &str) -> bool {
        matches!(
            language_of(locale),
            "de" | "fr"
                | "es"
                | "it"
                | "pt"
                | "nl"
                | "ru"
                | "pl"
                | "sv"
                | "da"
                | "fi"
                | "cs"
                | "el"
                | "hu"
                | "ro"
                | "uk"
                | "nb"
                | "nn"
                | "ca"
                | "hr"
                | "sk"
                | "sl"
                | "lt"
                | "lv"
                | "et"
                | "is"
                | "bg"
        )
    }

    /// 单位标签：`unitDisplay: "long"` 只在 `en` 生效（单/复数全名）；其余情况走短标签表，
    /// `fi`/`sv`/`hi` 对少数单位有本地化覆盖。未知单位原样返回 `unit` 字符串。
    fn unit_label(locale: &str, options: &NumberOptions, singular: bool) -> String {
        let unit = options.unit.as_str();
        let language = language_of(locale);
        let long = options.unit_display == "long" && language == "en";
        // Localized wide/narrow labels for the units the corpus and tests use.
        let localized_short: Option<&str> = match (language, unit) {
            ("fi", "kilometer") => Some("km"),
            ("sv", "kilometer") => Some("km"),
            ("hi", "kilometer") => Some("\u{915}\u{93f}\u{970}\u{92e}\u{940}\u{970}"),
            ("fi", "meter") => Some("m"),
            ("sv", "meter") => Some("m"),
            ("hi", "meter") => Some("\u{92e}\u{940}"),
            ("fi", "kilogram") => Some("kg"),
            ("sv", "kilogram") => Some("kg"),
            ("hi", "kilogram") => Some("\u{915}\u{93f}\u{97}\u{917}\u{94d}\u{930}\u{93e}"),
            _ => None,
        };
        if !long && let Some(label) = localized_short {
            return label.to_owned();
        }
        let (one, many, short) = match unit {
            "kilometer" => ("kilometer", "kilometers", "km"),
            "meter" => ("meter", "meters", "m"),
            "centimeter" => ("centimeter", "centimeters", "cm"),
            "millimeter" => ("millimeter", "millimeters", "mm"),
            "mile" => ("mile", "miles", "mi"),
            "foot" => ("foot", "feet", "ft"),
            "inch" => ("inch", "inches", "in"),
            "yard" => ("yard", "yards", "yd"),
            "kilogram" => ("kilogram", "kilograms", "kg"),
            "gram" => ("gram", "grams", "g"),
            "milligram" => ("milligram", "milligrams", "mg"),
            "pound" => ("pound", "pounds", "lb"),
            "ounce" => ("ounce", "ounces", "oz"),
            "liter" => ("liter", "liters", "L"),
            "milliliter" => ("milliliter", "milliliters", "mL"),
            "second" => ("second", "seconds", "s"),
            "millisecond" => ("millisecond", "milliseconds", "ms"),
            "minute" => ("minute", "minutes", "min"),
            "hour" => ("hour", "hours", "h"),
            "day" => ("day", "days", "day"),
            "week" => ("week", "weeks", "wk"),
            "month" => ("month", "months", "mo"),
            "year" => ("year", "years", "yr"),
            "kilometer-per-hour" => ("kilometer per hour", "kilometers per hour", "km/h"),
            "meter-per-second" => ("meter per second", "meters per second", "m/s"),
            "celsius" => ("degree Celsius", "degrees Celsius", "\u{b0}C"),
            "fahrenheit" => ("degree Fahrenheit", "degrees Fahrenheit", "\u{b0}F"),
            "byte" => ("byte", "bytes", "byte"),
            "kilobyte" => ("kilobyte", "kilobytes", "kB"),
            "megabyte" => ("megabyte", "megabytes", "MB"),
            "gigabyte" => ("gigabyte", "gigabytes", "GB"),
            "terabyte" => ("terabyte", "terabytes", "TB"),
            "percent" => ("percent", "percent", "%"),
            other => return other.to_owned(),
        };
        if long {
            if singular { one } else { many }.to_owned()
        } else {
            short.to_owned()
        }
    }

    // ---- DateTimeFormat ---------------------------------------------------

    /// 日期格式规格，字段名与 `js/intl.js` 的 `buildDateSpec` 输出一一对应。
    /// `fields` 是 `YMD`/`YM`/`MD`/`Y`/`M`/`E`/`D`/`DE`/`MDE`/`YMDE` 这些字母组合；
    /// `time` 是 `"hour"`/`"minute"`/`"second"` 或 None。
    #[derive(Default)]
    struct DateSpec {
        fields: Option<String>,
        length: String,
        year2: bool,
        pad: bool,
        time: Option<String>,
        hour_cycle: Option<String>,
        time_zone: Option<String>,
        pad_hour: bool,
        minute_only: bool,
        fractional: Option<u32>,
        day_period: Option<String>,
        zone_name: Option<String>,
        has_minute: bool,
        has_second: bool,
        style_join: Option<String>,
    }

    /// 解析 `JSON.stringify(spec)`：`length` 缺省 `"short"`，布尔字段缺省 false，
    /// 其它字符串/数字字段缺省 None。JSON 无效时全部取缺省——此时既没有 `dateFields` 也没有
    /// `time`，手写路径会输出空字符串（ICU 路径也因无字段而拒绝并回退）。
    fn parse_date_spec(spec_json: &str) -> DateSpec {
        let value: serde_json::Value =
            serde_json::from_str(spec_json).unwrap_or(serde_json::Value::Null);
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        DateSpec {
            fields: text("dateFields"),
            length: text("length").unwrap_or_else(|| "short".to_owned()),
            year2: value
                .get("year2")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            pad: value
                .get("pad")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            time: text("time"),
            hour_cycle: text("hourCycle"),
            time_zone: text("timeZone"),
            pad_hour: value
                .get("padHour")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            minute_only: value
                .get("minuteOnly")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            fractional: value
                .get("fractional")
                .and_then(|value| value.as_f64())
                .map(|value| value.max(0.0) as u32),
            day_period: text("dayPeriod"),
            zone_name: text("zoneName"),
            has_minute: value
                .get("hasMinute")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            has_second: value
                .get("hasSecond")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            style_join: text("styleJoin"),
        }
    }

    impl DateSpec {
        /// `fields` 里是否含 `Y`；只有含年份时才向 ICU 传 `year_style`。
        fn has_year(&self) -> bool {
            self.fields
                .as_deref()
                .is_some_and(|fields| fields.contains('Y'))
        }
    }

    /// 已按目标时区拆好的本地时间字段（chrono 的取值为 1 起的月/日与 0 起的时/分/秒）。
    struct LocalParts {
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        millisecond: u32,
    }

    /// 命中名单的时区名按 UTC 处理，不交给 `chrono_tz` 解析。
    fn is_utc_zone(time_zone: &str) -> bool {
        matches!(
            time_zone,
            "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Etc/Universal" | "Etc/Zulu"
        )
    }

    /// 把 epoch 毫秒拆成本地字段，优先级：
    ///
    /// 1. 显式且非 UTC 名单、且 `chrono_tz` 能解析的时区 → 该时区的本地时间；
    /// 2. 显式 UTC 名单 → `chrono::Utc`；
    /// 3. 其它（无时区、或时区名解析失败）→ `chrono::Local`，即进程本地时区。
    ///
    /// 毫秒用 `rem_euclid(1000)` 取非负；时间戳无效时回退到 epoch 0 再计算。
    /// JS 侧在调用前已用 `intlValidTimeZone` 校验过显式时区，所以第 3 条主要发生在直接
    /// 调用原语或校验绕过时。
    fn local_parts(epoch_ms: f64, time_zone: Option<&str>) -> LocalParts {
        use chrono::{Datelike as _, TimeZone as _, Timelike as _};
        let millis = epoch_ms as i64;
        let millisecond = millis.rem_euclid(1000) as u32;
        let build = |year, month, day, hour, minute, second| LocalParts {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millisecond,
        };
        if let Some(zone) = time_zone
            && !is_utc_zone(zone)
            && let Ok(parsed) = zone.parse::<chrono_tz::Tz>()
            && let Some(local) = parsed.timestamp_millis_opt(millis).single()
        {
            return build(
                local.year(),
                local.month() as u8,
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8,
                local.second() as u8,
            );
        }
        let utc = time_zone.is_some_and(is_utc_zone);
        if utc {
            let local = chrono::Utc
                .timestamp_millis_opt(millis)
                .single()
                .or_else(|| chrono::Utc.timestamp_millis_opt(0).single())
                .unwrap_or_else(|| chrono::Utc.timestamp_millis_opt(0).unwrap());
            build(
                local.year(),
                local.month() as u8,
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8,
                local.second() as u8,
            )
        } else {
            let local = chrono::Local
                .timestamp_millis_opt(millis)
                .single()
                .or_else(|| chrono::Local.timestamp_millis_opt(0).single())
                .unwrap_or_else(|| chrono::Local.timestamp_millis_opt(0).unwrap());
            build(
                local.year(),
                local.month() as u8,
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8,
                local.second() as u8,
            )
        }
    }

    /// `Date#toLocale*String` / `Intl.DateTimeFormat` 的 Rust 后端。
    /// 返回 `{ text, parts }`（`parts` 与 Node 的 `formatToParts` 同形）。
    ///
    /// `spec_json` 来自 `js/intl.js` 的 `buildDateSpec`，`epoch_ms` 是 `Date#getTime()` 的
    /// 数值；时区解析与格式化见 `local_parts` / `date_output`。
    pub fn date_format_json(locale: &str, spec_json: &str, epoch_ms: f64) -> String {
        let spec = parse_date_spec(spec_json);
        let parts = local_parts(epoch_ms, spec.time_zone.as_deref());
        let (text, part_list) = date_output(locale, &spec, &parts, epoch_ms);
        serde_json::json!({ "text": text, "parts": parts_to_json(&part_list) }).to_string()
    }

    /// 选择输出路径：`en-US` 永远走手写实现（即使开了 `locale-data`）；其它 locale 在
    /// `locale-data` 且 ICU 能格式化时走 `icu_date_output`；否则回退手写实现（输出的是
    /// en-US 格式）。`epoch_ms` 只被手写路径的时区标签使用。
    fn date_output(
        locale: &str,
        spec: &DateSpec,
        parts: &LocalParts,
        epoch_ms: f64,
    ) -> (String, Vec<(String, String)>) {
        #[cfg(feature = "locale-data")]
        if !locale.eq_ignore_ascii_case("en-US")
            && let Some(output) = icu_date_output(locale, spec, parts)
        {
            return output;
        }
        let _ = epoch_ms;
        let part_list = en_us_date_parts(locale, spec, parts, epoch_ms);
        let text = part_list
            .iter()
            .map(|(_, value)| value.as_str())
            .collect::<String>();
        (text, part_list)
    }

    /// 英文星期名（Sunday 起）。日期字段非法（`NaiveDate` 构造失败）时返回空串。
    fn en_us_weekday(parts: &LocalParts) -> &'static str {
        const NAMES: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        use chrono::{Datelike as _, NaiveDate};
        NaiveDate::from_ymd_opt(parts.year, u32::from(parts.month), u32::from(parts.day))
            .map(|date| NAMES[date.weekday().num_days_from_sunday() as usize])
            .unwrap_or("")
    }

    /// 英文月份名（long/short 两套表）。月份索引先减 1 再钳到 0..=11，越界不会 panic。
    fn en_us_month(parts: &LocalParts, long: bool) -> &'static str {
        const LONG: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        const SHORT: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let index = usize::from(parts.month).saturating_sub(1).min(11);
        if long { LONG[index] } else { SHORT[index] }
    }

    fn literal(parts: &mut Vec<(String, String)>, text: &str) {
        push_part(parts, "literal", text);
    }

    /// 手写 en-US 日期部分：按 `fields` 的字母组合选择模板（`YMD`/`YM`/`MD`/`Y`/`M`/`E`/
    /// `D`/`DE`/`MDE`/`YMDE`），`length` 控制长/中/短格式，`pad` 控制两位补零，`year2`
    /// 只取年份后两位。未知组合回退成 `MM/DD/YYYY`；`fields` 为 `None` 时日期部分是空的。
    /// 有 `time` 时再追加时间部分（分隔符是 `styleJoin == "at"` 时的 `" at "`，否则 `, `），
    /// `zoneName` 只在同时有 `time` 时追加。
    fn en_us_date_parts(
        locale: &str,
        spec: &DateSpec,
        parts: &LocalParts,
        epoch_ms: f64,
    ) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let pad2 = |value: u8| -> String {
            if spec.pad {
                format!("{value:02}")
            } else {
                value.to_string()
            }
        };
        let year_text = if spec.year2 {
            format!("{:02}", parts.year.rem_euclid(100))
        } else {
            parts.year.to_string()
        };
        let day_text = pad2(parts.day);
        if let Some(fields) = spec.fields.as_deref() {
            let long_month = spec.length == "long";
            match fields {
                "YMD" => match spec.length.as_str() {
                    "long" => {
                        push_part(&mut out, "month", en_us_month(parts, true));
                        literal(&mut out, " ");
                        push_part(&mut out, "day", day_text.clone());
                        literal(&mut out, ", ");
                        push_part(&mut out, "year", year_text);
                    }
                    "medium" => {
                        push_part(&mut out, "month", en_us_month(parts, false));
                        literal(&mut out, " ");
                        push_part(&mut out, "day", day_text.clone());
                        literal(&mut out, ", ");
                        push_part(&mut out, "year", year_text);
                    }
                    _ => {
                        push_part(&mut out, "month", pad2(parts.month));
                        literal(&mut out, "/");
                        push_part(&mut out, "day", pad2(parts.day));
                        literal(&mut out, "/");
                        push_part(&mut out, "year", year_text);
                    }
                },
                "YM" => {
                    push_part(&mut out, "month", en_us_month(parts, long_month));
                    literal(&mut out, " ");
                    push_part(&mut out, "year", year_text);
                }
                "MD" => {
                    if long_month || spec.length == "medium" {
                        push_part(&mut out, "month", en_us_month(parts, long_month));
                        literal(&mut out, " ");
                        push_part(&mut out, "day", day_text.clone());
                    } else {
                        push_part(&mut out, "month", pad2(parts.month));
                        literal(&mut out, "/");
                        push_part(&mut out, "day", pad2(parts.day));
                    }
                }
                "Y" => push_part(&mut out, "year", year_text),
                "M" => push_part(&mut out, "month", en_us_month(parts, long_month)),
                "E" => push_part(&mut out, "weekday", en_us_weekday(parts)),
                "D" => push_part(&mut out, "day", pad2(parts.day)),
                "DE" => {
                    push_part(&mut out, "weekday", en_us_weekday(parts));
                    literal(&mut out, " ");
                    push_part(&mut out, "day", day_text.clone());
                }
                "MDE" => {
                    push_part(&mut out, "weekday", en_us_weekday(parts));
                    literal(&mut out, ", ");
                    push_part(&mut out, "month", en_us_month(parts, long_month));
                    literal(&mut out, " ");
                    push_part(&mut out, "day", day_text.clone());
                }
                "YMDE" => {
                    push_part(&mut out, "weekday", en_us_weekday(parts));
                    literal(&mut out, ", ");
                    push_part(&mut out, "month", en_us_month(parts, true));
                    literal(&mut out, " ");
                    push_part(&mut out, "day", day_text.clone());
                    literal(&mut out, ", ");
                    push_part(&mut out, "year", year_text);
                }
                _ => {
                    push_part(&mut out, "month", pad2(parts.month));
                    literal(&mut out, "/");
                    push_part(&mut out, "day", pad2(parts.day));
                    literal(&mut out, "/");
                    push_part(&mut out, "year", year_text);
                }
            }
        }
        if spec.time.is_some() {
            if !out.is_empty() {
                literal(
                    &mut out,
                    if spec.style_join.as_deref() == Some("at") {
                        " at "
                    } else {
                        ", "
                    },
                );
            }
            en_us_time_parts(locale, spec, parts, &mut out);
        }
        if let Some(style) = spec.zone_name.as_deref()
            && spec.time.is_some()
        {
            literal(&mut out, " ");
            push_part(
                &mut out,
                "timeZoneName",
                zone_label(spec.time_zone.as_deref(), style, epoch_ms),
            );
        }
        out
    }

    /// 手写 en-US 时间部分：`hourCycle == "h23"` 用 24 小时制，否则 12 小时制 + AM/PM；
    /// `minuteOnly` 只输出分钟；`fractional` 把毫秒截断/补零到指定小数位；`dayPeriod`
    /// 存在时用英文短语（`day_period_phrase`）替代 AM/PM。`locale` 参数当前未使用。
    fn en_us_time_parts(
        locale: &str,
        spec: &DateSpec,
        parts: &LocalParts,
        out: &mut Vec<(String, String)>,
    ) {
        let h23 = spec.hour_cycle.as_deref() == Some("h23");
        if spec.minute_only {
            let minute_text = if spec.pad {
                format!("{:02}", parts.minute)
            } else {
                parts.minute.to_string()
            };
            push_part(out, "minute", minute_text);
            return;
        }
        let (hour_value, meridiem) = if h23 {
            (u32::from(parts.hour), None)
        } else {
            let hour = if parts.hour.is_multiple_of(12) {
                12
            } else {
                u32::from(parts.hour % 12)
            };
            (hour, Some(if parts.hour < 12 { "AM" } else { "PM" }))
        };
        let hour_text = if spec.pad_hour || (h23 && spec.time.as_deref() != Some("hour")) {
            format!("{hour_value:02}")
        } else {
            hour_value.to_string()
        };
        push_part(out, "hour", hour_text);
        let show_minute = spec.has_minute || spec.time.as_deref() != Some("hour");
        let show_second = spec.has_second || spec.time.as_deref() == Some("second");
        if show_minute {
            literal(out, ":");
            push_part(out, "minute", format!("{:02}", parts.minute));
        }
        if show_second || spec.fractional.is_some() {
            literal(out, ":");
            push_part(out, "second", format!("{:02}", parts.second));
            if let Some(digits) = spec.fractional {
                literal(out, ".");
                let mut text = format!("{:03}", parts.millisecond);
                text.truncate(digits.min(3) as usize);
                while text.len() < digits as usize {
                    text.push('0');
                }
                push_part(out, "fractionalSecond", text);
            }
        }
        let _ = locale;
        match spec.day_period.as_deref() {
            Some(_) => {
                literal(out, " ");
                push_part(out, "dayPeriod", day_period_phrase(parts.hour));
            }
            None => {
                if let Some(meridiem) = meridiem {
                    literal(out, " ");
                    push_part(out, "dayPeriod", meridiem);
                }
            }
        }
    }

    /// en-US 的时段短语：0-5 深夜、6-11 上午、12-17 下午、18-23 晚上。
    fn day_period_phrase(hour: u8) -> &'static str {
        match hour {
            0..=5 => "at night",
            6..=11 => "in the morning",
            12..=17 => "in the afternoon",
            _ => "in the evening",
        }
    }

    /// `timeZoneName` 的文本：UTC 名单输出 `UTC`/`Coordinated Universal Time`；其它时区
    /// 输出 `GMT±H[:MM]`（`longOffset`/`longGeneric`/`full` 用带分钟的完整形式，`long`
    /// 对非 UTC 时区也用短偏移形式）。无时区返回空串；偏移查不到按 0 处理。
    fn zone_label(time_zone: Option<&str>, style: &str, epoch_ms: f64) -> String {
        let Some(zone) = time_zone else {
            return String::new();
        };
        let utc = is_utc_zone(zone);
        let offset = zone_offset_seconds(zone, epoch_ms);
        match style {
            "long" => {
                if utc {
                    "UTC".to_owned()
                } else {
                    format_offset(offset.unwrap_or(0), false)
                }
            }
            "shortOffset" | "shortGeneric" => format_offset(offset.unwrap_or(0), false),
            "longOffset" | "longGeneric" => format_offset(offset.unwrap_or(0), true),
            "full" => {
                if utc {
                    "Coordinated Universal Time".to_owned()
                } else {
                    format_offset(offset.unwrap_or(0), true)
                }
            }
            _ => {
                if utc {
                    "UTC".to_owned()
                } else {
                    format_offset(offset.unwrap_or(0), false)
                }
            }
        }
    }

    /// 该时刻目标时区相对 UTC 的偏移（秒）：`chrono_tz` 按时间戳取，正确处理 DST；
    /// UTC 名单直接 0；时区名或时间戳无效返回 `None`（调用方按 0 显示）。
    fn zone_offset_seconds(zone: &str, epoch_ms: f64) -> Option<i32> {
        use chrono::{Offset as _, TimeZone as _};
        if is_utc_zone(zone) {
            return Some(0);
        }
        let parsed: chrono_tz::Tz = zone.parse().ok()?;
        let utc = chrono::Utc.timestamp_millis_opt(epoch_ms as i64).single()?;
        Some(
            parsed
                .offset_from_utc_datetime(&utc.naive_utc())
                .fix()
                .local_minus_utc(),
        )
    }

    /// 格式化偏移：`long` 时总是 `GMT±HH:MM`，否则分钟为 0 时省略分钟（`GMT+2`）。
    fn format_offset(seconds: i32, long: bool) -> String {
        let sign = if seconds < 0 { "-" } else { "+" };
        let total = seconds.abs();
        let hours = total / 3600;
        let minutes = (total % 3600) / 60;
        if long {
            format!("GMT{sign}{hours:02}:{minutes:02}")
        } else if minutes == 0 {
            format!("GMT{sign}{hours}")
        } else {
            format!("GMT{sign}{hours}:{minutes:02}")
        }
    }

    /// ICU4X 的日期路径：`fields`/`time` 映射到 `FieldSetBuilder`，`hourCycle` 通过 locale
    /// 扩展 `-u-hc-…` 传入；构造字段集失败时去掉 `year_style` 重试一次。任一步失败返回
    /// `None`，由 `date_output` 回退到手写实现。
    ///
    /// 一个特例：`fields == "Y"` + `year2` + 无时间时直接输出两位年份（Node 的 `24`，
    /// 与 CLDR 的紧凑形式不同）。
    #[cfg(feature = "locale-data")]
    fn icu_date_output(
        locale: &str,
        spec: &DateSpec,
        parts: &LocalParts,
    ) -> Option<(String, Vec<(String, String)>)> {
        use icu_datetime::DateTimeFormatter;
        use icu_datetime::fieldsets::builder::{DateFields, FieldSetBuilder};
        use icu_datetime::input::{Date, DateTime, Time};
        use icu_datetime::options::{Alignment, Length, TimePrecision, YearStyle};

        // Node renders a bare 2-digit year as "24" (CLDR's compact "’24" differs).
        if spec.fields.as_deref() == Some("Y") && spec.year2 && spec.time.is_none() {
            let text = format!("{:02}", parts.year.rem_euclid(100));
            return Some((text.clone(), vec![("year".to_owned(), text)]));
        }

        let fields = spec.fields.as_deref().and_then(|name| match name {
            "YMD" => Some(DateFields::YMD),
            "YM" => Some(DateFields::YM),
            "MD" => Some(DateFields::MD),
            "Y" => Some(DateFields::Y),
            "M" => Some(DateFields::M),
            "E" => Some(DateFields::E),
            "D" => Some(DateFields::D),
            "DE" => Some(DateFields::DE),
            "MDE" => Some(DateFields::MDE),
            "YMDE" => Some(DateFields::YMDE),
            _ => None,
        });
        let time_precision = match spec.time.as_deref() {
            Some("hour") => Some(TimePrecision::Hour),
            Some("minute") => Some(TimePrecision::Minute),
            Some("second") => Some(TimePrecision::Second),
            _ => None,
        };
        if fields.is_none() && time_precision.is_none() {
            return None;
        }
        let mut builder = FieldSetBuilder::new();
        builder.date_fields = fields;
        builder.time_precision = time_precision;
        if fields.is_some() {
            builder.length = Some(match spec.length.as_str() {
                "long" => Length::Long,
                "medium" => Length::Medium,
                _ => Length::Short,
            });
            if spec.has_year() {
                builder.year_style = Some(if spec.year2 {
                    YearStyle::Auto
                } else {
                    YearStyle::Full
                });
            }
            if spec.pad {
                builder.alignment = Some(Alignment::Column);
            }
        }
        let field_set = match builder.clone().build_composite_datetime() {
            Ok(field_set) => field_set,
            Err(_) => {
                let mut retry = builder;
                retry.year_style = None;
                retry.build_composite_datetime().ok()?
            }
        };
        let locale_text = match spec.hour_cycle.as_deref() {
            Some(hour_cycle) => format!("{locale}-u-hc-{hour_cycle}"),
            None => locale.to_owned(),
        };
        let icu_locale = icu_locale::Locale::try_from_str(&locale_text)
            .or_else(|_| icu_locale::Locale::try_from_str(locale))
            .ok()?;
        let formatter = DateTimeFormatter::try_new(icu_locale.into(), field_set).ok()?;
        let date = Date::try_new_gregorian(parts.year, parts.month, parts.day).ok()?;
        let time = Time::try_new(parts.hour, parts.minute, parts.second, 0).ok()?;
        let formatted = formatter.format(&DateTime { date, time });
        let mut output_parts = parts_from_writeable(&formatted)?;
        for part in &mut output_parts {
            part.1 = normalize_icu_output(std::mem::take(&mut part.1));
        }
        let text: String = output_parts
            .iter()
            .map(|(_, value)| value.as_str())
            .collect();
        Some((text, output_parts))
    }

    /// ICU4X 用 U+202F（narrow no-break space）分隔 en-US 的 `AM`/`PM`；Node/V8 用
    /// 普通空格（实测）。其余零填充差异按 locale 记入 §7.4，不做字节级对齐。
    #[cfg(feature = "locale-data")]
    fn normalize_icu_output(text: String) -> String {
        text.replace('\u{202f}', " ")
    }

    /// 系统时区名（`Intl.DateTimeFormat#resolvedOptions().timeZone`）。
    ///
    /// 优先 `TZ` 环境变量（去掉前导 `:`），否则读 `/etc/localtime` 符号链接里 `zoneinfo/`
    /// 之后的部分，都拿不到时返回 `"UTC"`。不做时区名校验（`valid_time_zone` 才校验）。
    pub fn time_zone_name() -> String {
        if let Ok(zone) = std::env::var("TZ")
            && !zone.trim().is_empty()
        {
            return zone.trim_start_matches(':').to_owned();
        }
        #[cfg(unix)]
        {
            if let Ok(target) = std::fs::read_link("/etc/localtime")
                && let Some(text) = target.to_str()
                && let Some(index) = text.find("zoneinfo/")
            {
                return text[index + "zoneinfo/".len()..].to_owned();
            }
        }
        "UTC".to_owned()
    }

    /// 时区名是否有效：UTC 名单命中，或 `chrono_tz` 能解析。JS 侧在构造
    /// `Intl.DateTimeFormat` 时用它决定是否抛 `RangeError`。
    pub fn valid_time_zone(zone: &str) -> bool {
        is_utc_zone(zone) || zone.parse::<chrono_tz::Tz>().is_ok()
    }
}
