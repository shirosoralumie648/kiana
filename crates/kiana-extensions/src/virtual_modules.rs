//! `core/extensions/virtual-modules.ts`：22 个虚拟模块的映射与运行时注入。
//!
//! 设计文档 §3.3（键表）、§3.6（typebox 内嵌）、§3.7（schema 校验）、§8.1–§8.6（导出面）、
//! §9.2（安装顺序）。逐字对齐 TS 的 `VIRTUAL_MODULES`：
//!
//! | 键 | 指向 |
//! |---|---|
//! | `typebox` / `@sinclair/typebox` | 内嵌 typebox 1.3.27 的 `build/index.mjs` |
//! | `typebox/compile` / `@sinclair/*` | `build/compile/index.mjs` |
//! | `typebox/value` / `@sinclair/*` | `build/value/index.mjs` |
//! | `@earendil-works/kiana-agent-core` / `@mariozechner/*` | 空 namespace |
//! | `@earendil-works/kiana-tui` / `@mariozechner/*` | JS 组合类 + 原生桥（ui-adapter） |
//! | `@earendil-works/kiana-ai` / `/compat` / `@mariozechner/*` | **同一 namespace**（compat 是 core 的严格超集） |
//! | `@earendil-works/kiana-ai/models`、`/oauth`、`/providers/all` | 各自独立 namespace |
//! | `@earendil-works/kiana-coding-agent` / `@mariozechner/*` | 34 个运行期导出 |
//!
//! # 注入方式（§3.3）
//!
//! [`install`] 构建 `globalThis.__kiana_virtual`：22 个键各指向一个 **namespace 种子对象**
//! （`kiana-*`）或真实 typebox 模块 namespace（typebox 键）。`js/virtual/*.js` 的 shim 模块只做
//! `export const X = __kiana_virtual["key"].X`，因此 Rust 不必为每个导出创建 `Function`。
//!
//! 种子对象里：
//!
//! - 纯 JS 值（`StringEnum`、`uuidv7`、`Container`、kiana-ai 的静态常量与 `AI_TELEMETRY_SCHEMA`…）
//!   由 `js/virtual/*-helpers.js` 求值一次后拷入；
//! - Rust 侧直接值（`CONFIG_DIR_NAME`、`VERSION`、`DEFAULT_MAX_BYTES`、`DEFAULT_MAX_LINES`）来自
//!   [`NamespaceSpec::constants`]，与 TS 的 `const` 导出一样在 import 时即可读（§8.6「直接值」）；
//! - 需要其它单元（ui-adapter、kiana-ai 桥、coding-agent 桥）提供的值，是**动态转发**：
//!   - callable（类/函数）与 object（`Key`、`TUI_KEYBINDINGS`）成员是 `Proxy`，每次
//!     apply/construct/get 都读 [`NATIVE_BRIDGE_GLOBAL`]；
//!   - primitive 成员是 enumerable getter，读同一个桥（当前表里没有 primitive：coding-agent
//!     的四个常量已改为 Rust 直接值）。
//!
//! 桥不存在时**使用**该成员会抛
//! `kiana: native bridge member "<namespace>.<name>" is not installed`；读取（`typeof`、
//! `Object.keys`）不受影响，因此 `typeof clampThinkingLevel === "function"` 仍成立（11156）。
//!
//! # 导出面
//!
//! 每个 namespace 的导出名逐字来自 TS 的运行期导出（compat 138 个、kiana-tui 91 个 + coding-agent
//! 额外提供的 6 个、kiana-coding-agent 34 个、models 11 个、providers/all 12 个、oauth 与
//! kiana-agent-core 为空）。夹具 `tests/fixtures/virtual/exports.json` 由 upstream-reference 的真实模块生成，
//! `tests/virtual_modules.rs` 逐个比对。TS 中一个模块可被多个入口再导出（compat 再导出
//! `models`），同名成员用同一个桥 id 与同一个 `Proxy`，保持 `ai.calculateCost ===
//! models.calculateCost` 的对象身份。
//!
//! # 跨单元契约
//!
//! - [`virtual_fs`]：`VirtualFile`/`VirtualAlias` 表，供 loader-compile 的 `Resolver` 使用。
//!   每个公开键都别名到一个规范模块名（`typebox/build/index.mjs`、`kiana-virtual/kiana-ai.js`…），
//!   使同一 namespace 的多把键在 QuickJS 模块缓存里是**同一个模块**（身份断言）。
//!   安装期内部使用的 `kiana-virtual/helpers/*.js` 也在表内：`install` 通过 runtime 的
//!   `Resolver`/`Loader` import 它们，QuickJS 会先规范化 specifier（否则真实解析器拒绝裸名）。
//! - 模块源同时用 [`JsEngine::register_module_source`] 以**规范名**与**22 个公开键**登记
//!   （[`register_module_sources`]，可在 `set_loader` 之前调用，便于 loader 建快照）。
//!   扩展 import 虚拟模块时由 loader 按需 `Module::declare`（typebox 的 691 个文件存在导入环，
//!   不能预先按依赖顺序声明；见 [`install`] 的前置条件）。
//! - [`NATIVE_BRIDGE_GLOBAL`]：其它单元写 `globalThis.__kiana_native[<namespace>][<name>]`；
//!   也可以用 [`set_native_member`]（单个）或 [`set_native_namespace`]（整组）。桥 id 就是
//!   [`NamespaceSpec::id`]：`"kiana-ai"`、`"kiana-ai/models"`、`"kiana-ai/oauth"`、
//!   `"kiana-ai/providers-all"`、`"kiana-tui"`、`"kiana-coding-agent"`。共享成员以
//!   [`BridgeMember::source`] 为准（例如 compat 的 `calculateCost` 由 `"kiana-ai/models"` 提供）；
//!   `kiana-ai*` 各 id 之间互为兜底，安装到任一 id 都能被读到。
//!
//! # 安装顺序
//!
//! 在 runtime 的 loader（loader-runtime）之后、prelude/intl 之后、api/context/dispatch 之前
//! （§9.2）。typebox 在这里求值一次并缓存为模块 namespace。
//!
//! 调用方：`kiana-extensions` 的装配器 `Generation::with_config`（第 6 步调用 [`install`]），
//! 以及 `loader::install`（先调用 [`register_module_sources`]，再按引擎的
//! `module_sources()` 建 loader 快照）。测试可以直接调用这两个函数。
//!
//! 注意：typebox 1.3.27 **不再**为 schema 写 `Symbol.for("TypeBox.Kind")`（旧版行为；实测 0 个
//! 自有 symbol）。因此 `packages/ai/src/utils/validation.ts` 的
//! `Object.getOwnPropertySymbols(...).includes(TYPEBOX_KIND)` 在 Node 里同样恒为 false，扩展工具
//! 总会再走一遍 `coerceWithJsonSchema`。内嵌真实包即逐字复刻这一行为（§3.7 的措辞针对旧版
//! typebox）。
//!
//! # 本文件的结构（阅读顺序）
//!
//! 1. 静态描述表：`NAMESPACES` 用 [`NamespaceSpec`] 描述 10 个 namespace；每个成员要么是
//!    helper 里的纯 JS 值（`js_values`）、要么是 Rust 直接提供的常量（`constants`）、要么
//!    经原生桥转发（`bridge` 里的 [`BridgeMember`]）。
//! 2. 安装：[`install`]（幂等）在 30 秒的 JS 事务预算内调用 `install_inner`：登记模块源 →
//!    求值 typebox 三个入口 → 求值 helper → 为 kiana-* namespace 建种子对象（typebox 的
//!    namespace 直接复用模块 namespace）并逐个寄存 `Persistent` → 写 `__kiana_virtual`（22 个
//!    可枚举公开键 + 10 个不可枚举的规范名）→ 写 `__kiana_native` 并注册两个 host kind。
//! 3. 模块源：[`register_module_sources`] 是安装流程与 `loader::install` 共用的登记入口；
//!    它把每个虚拟文件按规范名登记到引擎，再为每个公开键登记可供 loader 直接提供的一段源。
//! 4. 原生桥实现：文件后半段（`install_*_bridge`、`start_tool_execute`、
//!    `start_provider_stream`）是本单元用 Rust 顶替 TS 运行期导出的部分；异步成员走
//!    `__kiana.promise` 宿主协议，由引擎的主循环 settle 回 JS Promise。
//!
//! # 数据流
//!
//! 扩展代码看到的是 ESM 模块，但这里提供的只是「模块缓存里的一份 namespace 对象」。之所以
//! 能用普通对象冒充模块，是因为每个公开键都解析到 [`virtual_fs`] 里的一个模块：kiana-*
//! 的 shim 只从 `globalThis.__kiana_virtual` 取成员，typebox 公开键的 shim 再做导出。
//! 因此新增导出不需要在 Rust 里为每个名字创建 `Function`，只需要改静态表
//! [`NAMESPACES`] 与对应 JS 文件。
//!
//! 原生桥成员跨语言只传 JSON：JS 参数先 `JSON.stringify` 成 `serde_json::Value`，Rust 计算
//! 后再 `JSON.parse` 物化回 JS。由此产生三条对外可观察的约束：
//!
//! - 对象身份在边界上不保留（例如 `prepareArguments` 返回内容相同的新对象，而不是就地改写
//!   的那个对象）；
//! - `undefined` 与 `null` 在 JSON 边界合并；需要显式写回 `undefined` 时用 `set_opt_json`
//!   （个别字段如 `prepareArguments`、`renderCall` 直接写 `undefined`）；
//! - 字段类型不符时按成员各自的策略处理：模型解析走部分回退，`StreamOptions` 解析对错误
//!   类型则静默当作缺省。
//!
//! # 线程、生命周期与失败模式
//!
//! - 会进入 JS 的公开入口（[`install`]、[`is_installed`]、[`set_native_member`]、
//!   [`set_native_namespace`]）都经 [`JsEngine::with_js`]，因此只能在该引擎的创建线程调用；
//!   已经处于宿主函数（`HostCall`）内部时**不能**再调用（`with_js` 会因 `RefCell` 二次借用
//!   失败），需要推进微任务时用引擎提供的 `pump_jobs_with`。纯表查询与源登记
//!   （[`virtual_fs`]、[`namespace_spec`]、[`register_module_sources`] 等）不进入 JS。
//! - `TOOL_REGISTRY` / `BUILTIN_MODELS` 是进程级 `static`：工具 id 在进程内递增，注册表没有
//!   删除路径（`Arc<AgentTool>` 保留到进程结束）；`BUILTIN_MODELS` 首次使用时构造一次，被
//!   同一进程内所有引擎共享。
//! - 工具执行与 provider 流在 `tokio::task::spawn_local` 上运行，依赖引擎的单线程 `LocalSet`
//!   模型（测试用 `LocalSet::run_until` 驱动，见 `tests/virtual_modules.rs`）；JS 回调经
//!   `HostCompletionSender::capture` 跨 `await` 持有。
//! - [`install`] 的幂等判据是 `__kiana_virtual` 是否存在，而它在安装流程第 5 步才写入：前四
//!   步失败后可以重试（重新执行安装流程；已求值的模块由 QuickJS 模块缓存直接返回）；第 6 步
//!   （原生桥）失败时全局已经存在，重试会被当作已安装而直接返回。

#![cfg(feature = "runtime")]

use std::collections::HashMap;
use std::str::from_utf8;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use kiana_agent_core::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{
    Context, JsonValue, Model, ModelType, SimpleStreamOptions, StreamOptions, ToolChoice,
};
use rquickjs::function::Opt;
use rquickjs::{Ctx, Exception, Function, Module, Object, Value};

use crate::engine::{EngineResult, HostCall, HostValue, JsEngine};
use crate::loader::resolver::{VIRTUAL_MODULE_KEYS, VirtualAlias, VirtualFile, VirtualFs};

// build.rs 生成的表：`VIRTUAL_FILES`（691 个 typebox `.mjs` + 7 个 `js/virtual` shim +
// 4 个安装期 helper）与 `VIRTUAL_ALIASES`（22 个公开键 → 规范模块名）。`VirtualFile` /
// `VirtualAlias` 由上面的 `use` 引入，include! 展开在本模块作用域。
include!(concat!(env!("OUT_DIR"), "/virtual_files.rs"));

/// `globalThis.__kiana_virtual`：22 个虚拟模块的 namespace 表（设计 §3.3）。
pub const VIRTUAL_GLOBAL: &str = "__kiana_virtual";

/// `globalThis.__kiana_native`：其它单元安装原生成员的桥（见模块文档）。
pub const NATIVE_BRIDGE_GLOBAL: &str = "__kiana_native";

/// 安装虚拟模块时的 JS 事务预算：链接/求值 typebox 的 691 个模块在 debug 构建下可超过
/// 默认 100 ms 看门狗（§9.2 的安装步骤 5–6），因此显式放宽（不改全局 deadline）。
const INSTALL_DEADLINE: Duration = Duration::from_secs(30);

const BRIDGE_SOURCE: &str = include_str!("../js/virtual/bridge.js");
const KIANA_AI_HELPERS_SOURCE: &str = include_str!("../js/virtual/kiana-ai-helpers.js");
const KIANA_TUI_HELPERS_SOURCE: &str = include_str!("../js/virtual/kiana-tui-helpers.js");
const KIANA_CODING_AGENT_HELPERS_SOURCE: &str =
    include_str!("../js/virtual/kiana-coding-agent-helpers.js");

/// 内部 helper 模块（不是 22 个键的一部分，只在安装时求值）。
const BRIDGE_MODULE: &str = "kiana-virtual/helpers/bridge.js";
const KIANA_AI_HELPERS_MODULE: &str = "kiana-virtual/helpers/kiana-ai.js";
const KIANA_TUI_HELPERS_MODULE: &str = "kiana-virtual/helpers/kiana-tui.js";
const KIANA_CODING_AGENT_HELPERS_MODULE: &str = "kiana-virtual/helpers/kiana-coding-agent.js";

/// typebox 公开键的兜底 shim：resolver 正常会把它们别名到具体文件，但如果调用方只给了
/// `register_module_source` 而没接线 [`virtual_fs`]，这些条目仍能让 import 成功。
const TYPEBOX_ROOT_SHIM: &str = "export * from \"typebox/build/index.mjs\";\nexport { default } from \"typebox/build/index.mjs\";\n";
const TYPEBOX_COMPILE_SHIM: &str = "export * from \"typebox/build/compile/index.mjs\";\nexport { default } from \"typebox/build/compile/index.mjs\";\n";
const TYPEBOX_VALUE_SHIM: &str = "export * from \"typebox/build/value/index.mjs\";\nexport { default } from \"typebox/build/value/index.mjs\";\n";

/// 桥成员的形态（决定种子对象上的 forwarding 形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeKind {
    /// 函数/类：`Proxy`（apply/construct/get 动态转发）。
    Callable,
    /// 普通对象（`Key`、`AI_TELEMETRY_SCHEMA`）：`Proxy`（get/ownKeys 动态转发）。
    Object,
    /// 原始值常量（`VERSION`、`DEFAULT_MAX_LINES`）：enumerable getter。
    Primitive,
}

impl BridgeKind {
    /// JS 侧 `makeNamespace` 使用的拼写。
    ///
    /// 该字符串写入 `spec.bridge` 数组第 2 项；`js/virtual/bridge.js#makeNamespace` 按它选择转发
    /// 形状（`"primitive"` 定义 getter，`"object"`/`"callable"` 各建一种 Proxy）。
    ///
    /// 拼写必须与 bridge.js 的判断逐字一致：未知拼写会落到 callable 分支，`Object` 成员会被
    /// 当成函数代理（可调用），行为会出错。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Callable => "callable",
            Self::Object => "object",
            Self::Primitive => "primitive",
        }
    }
}

/// 由 Rust 直接提供的常量（TS 模块里的 `const` 导出，§8.6「直接值」）。
///
/// 安装时以可枚举数据属性写进种子对象：`import` 后立即可读、`Object.keys` 能看到，且完全
/// 不依赖原生桥（其它单元的桥缺失也不影响）。值只在安装时拷贝一次，之后 Rust 侧即使有变化
/// 也不会同步到 JS；当前这些常量都来自编译期 `const`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstantValue {
    /// `export const X: string = "…"`。
    Str(&'static str),
    /// `export const X: number = …`（JS number 即 f64）。
    Num(f64),
}

impl ConstantValue {
    /// 写入种子对象。
    fn apply<'js>(self, values: &Object<'js>, name: &str) -> EngineResult<()> {
        match self {
            Self::Str(value) => values.set(name, value)?,
            Self::Num(value) => values.set(name, value)?,
        }
        Ok(())
    }
}

/// 以 `f64` 形式表达 `usize` 常量（JS number）；值都在 2^53 以内，精确。
const fn number(value: usize) -> ConstantValue {
    ConstantValue::Num(value as f64)
}

/// 一个由原生桥提供的 namespace 成员。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeMember {
    /// 导出名（TS 运行期导出名，逐字）。
    pub name: &'static str,
    /// 桥成员的形态。
    pub kind: BridgeKind,
    /// 提供该成员的桥 namespace id；`None` 表示所属 namespace 自己。
    ///
    /// TS 中同一个对象可被多个入口再导出（`kiana-ai` 的 compat 入口再导出 `models`）。这些成员
    /// 必须指向同一个桥 id，`js/virtual/bridge.js` 才能为两个 namespace 共享同一个 `Proxy`
    /// （对象身份因此保持稳定）。
    pub source: Option<&'static str>,
    /// 该成员在 `source` 桥里的名字；`None` 表示与导出名相同。
    ///
    /// TS 的别名导出（`compat.ts` 的 `export const getModel = getBuiltinModel`）在 compat 的
    /// namespace 里叫 `getModel`、在 providers/all 的桥里叫 `getBuiltinModel`；两者在 TS 里
    /// 是**同一个函数对象**，因此 Proxy 缓存键必须用桥里的名字。
    pub bridge_name: Option<&'static str>,
}

/// 普通桥成员（由所属 namespace 的桥提供）。
const fn member(name: &'static str, kind: BridgeKind) -> BridgeMember {
    BridgeMember {
        name,
        kind,
        source: None,
        bridge_name: None,
    }
}

/// 被多个 namespace 再导出的桥成员（由 `source` namespace 的桥提供，两个 seed 共享 Proxy）。
const fn shared(name: &'static str, kind: BridgeKind, source: &'static str) -> BridgeMember {
    BridgeMember {
        name,
        kind,
        source: Some(source),
        bridge_name: None,
    }
}

/// 桥里名字与导出名不同的共享成员（TS 的 `export const getModel = getBuiltinModel`）。
const fn shared_as(
    name: &'static str,
    bridge_name: &'static str,
    kind: BridgeKind,
    source: &'static str,
) -> BridgeMember {
    BridgeMember {
        name,
        kind,
        source: Some(source),
        bridge_name: Some(bridge_name),
    }
}

/// 一个虚拟 namespace 的静态描述（22 个键分组成 10 个 namespace）。
///
/// 表级不变量：
///
/// - `id` 在表中唯一：它同时是 `__kiana_native` 的分区名与 `Persistent` 键
///   `kiana.virtual.namespace.<id>` 的后缀（重复会让种子查找与持久槽互相覆盖）；
/// - 所有 namespace 的 `keys` 合起来恰好是 `VIRTUAL_MODULE_KEYS` 的 22 项，每项只出现一次
///   （[`namespace_spec`] 只返回第一个匹配项；文件末尾与 `tests/virtual_modules.rs` 的测试
///   断言）；
/// - `bridge[].source` 必须指向本表某个 `id`（单元测试断言）；
/// - 同一 namespace 内 `js_values`、`constants`、`bridge` 的导出名不得重复（单元测试断言）。
#[derive(Debug)]
pub struct NamespaceSpec {
    /// 桥里的 namespace id（`__kiana_native[<id>]`）。
    pub id: &'static str,
    /// 规范模块名（QuickJS 模块缓存键；同一 namespace 的所有键都别名到它）。
    pub module: &'static str,
    /// 映射到本 namespace 的公开键（逐字来自 `virtual-modules.ts`）。
    pub keys: &'static [&'static str],
    /// 纯 JS helper 模块（安装时求值一次）。
    pub helper: Option<&'static str>,
    /// 从 helper 模块拷入种子对象的导出名。
    pub js_values: &'static [&'static str],
    /// Rust 直接提供的常量（import 即可读，不经过桥）。
    pub constants: &'static [(&'static str, ConstantValue)],
    /// 由原生桥提供的成员。
    pub bridge: &'static [BridgeMember],
}

/// 10 个 namespace 的静态表；顺序与 `virtual-modules.ts` 的键组一致（typebox 三组在前）。
///
/// 顺序决定种子对象的构建顺序与 `Persistent` 的寄存顺序；`__kiana_virtual` 的对象键序另由
/// `VIRTUAL_MODULE_KEYS` 决定（测试断言 22 个键按 TS 字面量顺序插入）。查找函数都是线性扫描，
/// 规模固定为 10，不需要索引。
pub static NAMESPACES: &[NamespaceSpec] = &[
    NamespaceSpec {
        id: "typebox",
        module: "typebox/build/index.mjs",
        keys: &["typebox", "@sinclair/typebox"],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[],
    },
    NamespaceSpec {
        id: "typebox/compile",
        module: "typebox/build/compile/index.mjs",
        keys: &["typebox/compile", "@sinclair/typebox/compile"],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[],
    },
    NamespaceSpec {
        id: "typebox/value",
        module: "typebox/build/value/index.mjs",
        keys: &["typebox/value", "@sinclair/typebox/value"],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[],
    },
    NamespaceSpec {
        id: "kiana-agent-core",
        module: "kiana-virtual/kiana-agent-core.js",
        keys: &[
            "@earendil-works/kiana-agent-core",
            "@mariozechner/kiana-agent-core",
        ],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[],
    },
    NamespaceSpec {
        id: "kiana-tui",
        module: "kiana-virtual/kiana-tui.js",
        keys: &["@earendil-works/kiana-tui", "@mariozechner/kiana-tui"],
        helper: Some(KIANA_TUI_HELPERS_MODULE),
        // TS `packages/tui/src/index.ts` 的运行期导出里能在 JS 侧逐字实现的部分（§5.1、§8.3）。
        js_values: &[
            "Container",
            "VStack",
            "HStack",
            "CURSOR_MARKER",
            "renderFakeCursor",
            "hyperlink",
            "Marked",
        ],
        constants: &[],
        bridge: &[
            member("BorderedLoader", BridgeKind::Callable),
            member("Box", BridgeKind::Callable),
            member("CancellableLoader", BridgeKind::Callable),
            member("CombinedAutocompleteProvider", BridgeKind::Callable),
            member("CustomEditor", BridgeKind::Callable),
            member("DynamicBorder", BridgeKind::Callable),
            member("Editor", BridgeKind::Callable),
            member("Image", BridgeKind::Callable),
            member("Input", BridgeKind::Callable),
            member("Key", BridgeKind::Object),
            member("KeybindingsManager", BridgeKind::Callable),
            member("Loader", BridgeKind::Callable),
            member("Markdown", BridgeKind::Callable),
            member("MouseRegion", BridgeKind::Callable),
            member("ProcessTerminal", BridgeKind::Callable),
            member("ScrollView", BridgeKind::Callable),
            member("SelectList", BridgeKind::Callable),
            member("SettingsList", BridgeKind::Callable),
            member("Spacer", BridgeKind::Callable),
            member("StdinBuffer", BridgeKind::Callable),
            member("TUI_KEYBINDINGS", BridgeKind::Object),
            member("Text", BridgeKind::Callable),
            member("TruncatedText", BridgeKind::Callable),
            member("TuiAltScreen", BridgeKind::Callable),
            member("TuiMainScreen", BridgeKind::Callable),
            member("allocateImageId", BridgeKind::Callable),
            member("backgroundAnsi", BridgeKind::Callable),
            member("calculateImageRows", BridgeKind::Callable),
            member("colorToHex", BridgeKind::Callable),
            member("colorToOkhsl", BridgeKind::Callable),
            member("colorToOklch", BridgeKind::Callable),
            member("colorToRgb", BridgeKind::Callable),
            member("compositeTuiLine", BridgeKind::Callable),
            member("decodeKittyPrintable", BridgeKind::Callable),
            member("deleteAllKittyImages", BridgeKind::Callable),
            member("deleteKittyImage", BridgeKind::Callable),
            member("detectCapabilities", BridgeKind::Callable),
            member("encodeITerm2", BridgeKind::Callable),
            member("encodeKitty", BridgeKind::Callable),
            member("foregroundAnsi", BridgeKind::Callable),
            member("formatProgramStatus", BridgeKind::Callable),
            member("fuzzyFilter", BridgeKind::Callable),
            member("fuzzyMatch", BridgeKind::Callable),
            member("getCapabilities", BridgeKind::Callable),
            member("getCellDimensions", BridgeKind::Callable),
            member("getGifDimensions", BridgeKind::Callable),
            member("getImageDimensions", BridgeKind::Callable),
            member("getJpegDimensions", BridgeKind::Callable),
            member("getKeybindings", BridgeKind::Callable),
            member("getMarkdownTheme", BridgeKind::Callable),
            member("getNativeClipboard", BridgeKind::Callable),
            member("getOsc8LinkAtColumn", BridgeKind::Callable),
            member("getPngDimensions", BridgeKind::Callable),
            member("getSettingsListTheme", BridgeKind::Callable),
            member("getTerminalColorMode", BridgeKind::Callable),
            member("getWebpDimensions", BridgeKind::Callable),
            member("imageFallback", BridgeKind::Callable),
            member("indexedColor", BridgeKind::Callable),
            member("isAppleTerminalSession", BridgeKind::Callable),
            member("isFocusable", BridgeKind::Callable),
            member("isKeyRelease", BridgeKind::Callable),
            member("isKeyRepeat", BridgeKind::Callable),
            member("isKittyProtocolActive", BridgeKind::Callable),
            member("isViewportTUI", BridgeKind::Callable),
            member("keyHint", BridgeKind::Callable),
            member("matchesKey", BridgeKind::Callable),
            member("mixColors", BridgeKind::Callable),
            member("okhslColor", BridgeKind::Callable),
            member("oklabToOkhslLightness", BridgeKind::Callable),
            member("oklchColor", BridgeKind::Callable),
            member("parseColor", BridgeKind::Callable),
            member("parseKey", BridgeKind::Callable),
            member("parseTerminalColorSchemeReport", BridgeKind::Callable),
            member("renderImage", BridgeKind::Callable),
            member("renderLatex", BridgeKind::Callable),
            member("resetCapabilitiesCache", BridgeKind::Callable),
            member("rgbColor", BridgeKind::Callable),
            member("setCapabilities", BridgeKind::Callable),
            member("setCapabilityOverrides", BridgeKind::Callable),
            member("setCellDimensions", BridgeKind::Callable),
            member("setImageTranscoder", BridgeKind::Callable),
            member("setKeybindings", BridgeKind::Callable),
            member("setKittyProtocolActive", BridgeKind::Callable),
            member("sliceByColumn", BridgeKind::Callable),
            member("stripTerminalSequences", BridgeKind::Callable),
            member("styleText", BridgeKind::Callable),
            member("styleTextWithAnsi", BridgeKind::Callable),
            member("truncateToWidth", BridgeKind::Callable),
            member("visibleWidth", BridgeKind::Callable),
            member("wrapTextWithAnsi", BridgeKind::Callable),
        ],
    },
    NamespaceSpec {
        id: "kiana-ai",
        module: "kiana-virtual/kiana-ai.js",
        // 根键解析到 compat 入口：`/compat` 是 core 的严格超集，四个键共用一个 namespace。
        keys: &[
            "@earendil-works/kiana-ai",
            "@earendil-works/kiana-ai/compat",
            "@mariozechner/kiana-ai",
            "@mariozechner/kiana-ai/compat",
        ],
        helper: Some(KIANA_AI_HELPERS_MODULE),
        // compat 的运行期导出中，纯 JS 与静态常量的部分（其余经原生桥转发）。
        js_values: &[
            "AI_TELEMETRY_SCHEMA",
            "ANTHROPIC_API_KEY_ENV",
            "ANTHROPIC_AUTH_TOKEN_ENV",
            "ANTHROPIC_FEDERATION_RULE_ID_ENV",
            "ANTHROPIC_IDENTITY_TOKEN_FILE_ENV",
            "ANTHROPIC_OAUTH_TOKEN_ENV",
            "ANTHROPIC_ORGANIZATION_ID_ENV",
            "ANTHROPIC_SERVICE_ACCOUNT_ID_ENV",
            "ANTHROPIC_WORKSPACE_ID_ENV",
            "AssistantMessageEventStream",
            "DEFAULT_MAX_AGENT_RETRY_DELAY_MS",
            "EventStream",
            "KIANA_AI_VERSION",
            "KIANA_ATTRIBUTION",
            "StreamingJsonParser",
            "StringEnum",
            "ToolCallArgumentsStream",
            "Type",
            "anthropicMessagesApi",
            "appendAssistantMessageDiagnostic",
            "azureOpenAIResponsesApi",
            "bedrockConverseStreamApi",
            "cleanupSessionResources",
            "collapseSystemMessages",
            "complete",
            "completeSimple",
            "contentText",
            "createAssistantMessageDiagnostic",
            "createAssistantMessageEventStream",
            "createInitialSystemMessage",
            "declarationsEqual",
            "envApiKeyAuth",
            "extractDiagnosticError",
            "formatThrownValue",
            "formatUserAgent",
            "generateImages",
            "getApiProvider",
            "getApiProviders",
            "getAttributionHeaders",
            "getClientVersion",
            "getCurrentSystemMessage",
            "getCurrentSystemPrompt",
            "getCurrentTools",
            "getDeclaredTools",
            "getIdentityHeaders",
            "getImagesApiProvider",
            "getInitialSystemMessage",
            "getOpenCodeSessionHeaders",
            "getOverflowPatterns",
            "getSystemMessageText",
            "getToolStateChanges",
            "googleGenerativeAIApi",
            "googleVertexApi",
            "hasNonAdditiveToolChanges",
            "hasToolRedefinitions",
            "isContextOverflow",
            "isRecoverableLength",
            "isRetryableAssistantError",
            "lazyApi",
            "lazyOAuth",
            "lazyStream",
            "mergeClientIdentity",
            "mistralConversationsApi",
            "normalizeContext",
            "openAICodexResponsesApi",
            "openAICompletionsApi",
            "openAIResponsesApi",
            "parseJsonWithRepair",
            "parseToolCallArguments",
            "kianaMessagesApi",
            "registerApiProvider",
            "registerBuiltInApiProviders",
            "registerImagesApiProvider",
            "registerSessionResourceCleanup",
            "renderSystemMessageUpdate",
            "repairJson",
            "resetApiProviders",
            "resolveTranscript",
            "resolveTranscriptTools",
            "retryAssistantCall",
            "retryDelayMs",
            "setBedrockProviderModule",
            "stream",
            "streamAnthropic",
            "streamAzureOpenAIResponses",
            "streamGoogle",
            "streamGoogleVertex",
            "streamMistral",
            "streamOpenAICodexResponses",
            "streamOpenAICompletions",
            "streamOpenAIResponses",
            "streamSimple",
            "streamSimpleAnthropic",
            "streamSimpleAzureOpenAIResponses",
            "streamSimpleGoogle",
            "streamSimpleGoogleVertex",
            "streamSimpleMistral",
            "streamSimpleOpenAICodexResponses",
            "streamSimpleOpenAICompletions",
            "streamSimpleOpenAIResponses",
            "toToolDeclaration",
            "unregisterApiProviders",
            "uuidv7",
            "withClientIdentity",
            "withoutInitialSystemMessage",
        ],
        constants: &[],
        bridge: &[
            member("InMemoryCredentialStore", BridgeKind::Callable),
            member("InMemoryModelsStore", BridgeKind::Callable),
            shared("ModelsError", BridgeKind::Callable, "kiana-ai/models"),
            shared("calculateCost", BridgeKind::Callable, "kiana-ai/models"),
            shared(
                "clampThinkingLevel",
                BridgeKind::Callable,
                "kiana-ai/models",
            ),
            member("createFauxCore", BridgeKind::Callable),
            shared("createModels", BridgeKind::Callable, "kiana-ai/models"),
            shared("createProvider", BridgeKind::Callable, "kiana-ai/models"),
            member("defaultProviderAuthContext", BridgeKind::Callable),
            member("fauxAssistantMessage", BridgeKind::Callable),
            member("fauxProvider", BridgeKind::Callable),
            member("fauxText", BridgeKind::Callable),
            member("fauxThinking", BridgeKind::Callable),
            member("fauxToolCall", BridgeKind::Callable),
            member("findEnvKeys", BridgeKind::Callable),
            member("generateImagesOpenRouter", BridgeKind::Callable),
            member("getEnvApiKey", BridgeKind::Callable),
            member("getImageModel", BridgeKind::Callable),
            member("getImageModels", BridgeKind::Callable),
            member("getImageProviders", BridgeKind::Callable),
            shared_as(
                "getModel",
                "getBuiltinModel",
                BridgeKind::Callable,
                "kiana-ai/providers-all",
            ),
            shared("getModelType", BridgeKind::Callable, "kiana-ai/models"),
            shared_as(
                "getModels",
                "getBuiltinModels",
                BridgeKind::Callable,
                "kiana-ai/providers-all",
            ),
            shared_as(
                "getProviders",
                "getBuiltinProviders",
                BridgeKind::Callable,
                "kiana-ai/providers-all",
            ),
            shared(
                "getSupportedThinkingLevels",
                BridgeKind::Callable,
                "kiana-ai/models",
            ),
            shared("hasApi", BridgeKind::Callable, "kiana-ai/models"),
            shared("isModelType", BridgeKind::Callable, "kiana-ai/models"),
            shared("modelsAreEqual", BridgeKind::Callable, "kiana-ai/models"),
            member("parseStreamingJson", BridgeKind::Callable),
            member("registerBuiltInImagesApiProviders", BridgeKind::Callable),
            member("registerFauxProvider", BridgeKind::Callable),
            member("validateToolArguments", BridgeKind::Callable),
            member("validateToolCall", BridgeKind::Callable),
        ],
    },
    NamespaceSpec {
        id: "kiana-ai/models",
        module: "kiana-virtual/kiana-ai/models.js",
        keys: &[
            "@earendil-works/kiana-ai/models",
            "@mariozechner/kiana-ai/models",
        ],
        helper: Some(KIANA_AI_HELPERS_MODULE),
        // `AI_TELEMETRY_SCHEMA` 是普通对象常量；与 compat 共用同一个对象（同一 helper 导出）。
        js_values: &["AI_TELEMETRY_SCHEMA"],
        constants: &[],
        bridge: &[
            member("ModelsError", BridgeKind::Callable),
            member("calculateCost", BridgeKind::Callable),
            member("clampThinkingLevel", BridgeKind::Callable),
            member("createModels", BridgeKind::Callable),
            member("createProvider", BridgeKind::Callable),
            member("getModelType", BridgeKind::Callable),
            member("getSupportedThinkingLevels", BridgeKind::Callable),
            member("hasApi", BridgeKind::Callable),
            member("isModelType", BridgeKind::Callable),
            member("modelsAreEqual", BridgeKind::Callable),
        ],
    },
    NamespaceSpec {
        id: "kiana-ai/oauth",
        module: "kiana-virtual/kiana-ai/oauth.js",
        keys: &[
            "@earendil-works/kiana-ai/oauth",
            "@mariozechner/kiana-ai/oauth",
        ],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[],
    },
    NamespaceSpec {
        id: "kiana-ai/providers-all",
        module: "kiana-virtual/kiana-ai/providers-all.js",
        keys: &[
            "@earendil-works/kiana-ai/providers/all",
            "@mariozechner/kiana-ai/providers/all",
        ],
        helper: None,
        js_values: &[],
        constants: &[],
        bridge: &[
            member("builtinModels", BridgeKind::Callable),
            member("builtinProviders", BridgeKind::Callable),
            member("getAllBuiltinModels", BridgeKind::Callable),
            member("getBuiltinClassifierModel", BridgeKind::Callable),
            member("getBuiltinClassifierModels", BridgeKind::Callable),
            member("getBuiltinImageModel", BridgeKind::Callable),
            member("getBuiltinImageModels", BridgeKind::Callable),
            member("getBuiltinModel", BridgeKind::Callable),
            member("getBuiltinModelDataGeneratedAt", BridgeKind::Callable),
            member("getBuiltinModels", BridgeKind::Callable),
            member("getBuiltinProviders", BridgeKind::Callable),
            member("radiusProvider", BridgeKind::Callable),
        ],
    },
    NamespaceSpec {
        id: "kiana-coding-agent",
        module: "kiana-virtual/kiana-coding-agent.js",
        keys: &[
            "@earendil-works/kiana-coding-agent",
            "@mariozechner/kiana-coding-agent",
        ],
        helper: Some(KIANA_CODING_AGENT_HELPERS_MODULE),
        js_values: &[
            "defineTool",
            "formatSize",
            "serializeConversation",
            "truncateLine",
            "withFileMutationQueue",
        ],
        // §8.6 的「直接值」：TS 的 `const` 导出，import 后立即可读，不经过桥。
        constants: &[
            (
                "CONFIG_DIR_NAME",
                ConstantValue::Str(kiana_core::config::CONFIG_DIR_NAME),
            ),
            (
                "DEFAULT_MAX_BYTES",
                number(kiana_tools::truncate::DEFAULT_MAX_BYTES),
            ),
            (
                "DEFAULT_MAX_LINES",
                number(kiana_tools::truncate::DEFAULT_MAX_LINES),
            ),
            ("VERSION", ConstantValue::Str(kiana_core::config::VERSION)),
        ],
        bridge: &[
            // 组件与主题由 ui-adapter 安装在 kiana-tui 桥里（设计 §5.1/§5.4）；coding-agent 的键
            // 与 `@earendil-works/kiana-tui` 共享同一个成员（TS 里 kiana-tui 不导出它们，因此没有
            // 跨 namespace 的身份约束，但共享桥保证两把键在 ui-adapter 就位后同时可用）。
            shared("BorderedLoader", BridgeKind::Callable, "kiana-tui"),
            shared("CustomEditor", BridgeKind::Callable, "kiana-tui"),
            shared("DynamicBorder", BridgeKind::Callable, "kiana-tui"),
            member("convertToLlm", BridgeKind::Callable),
            member("createBashTool", BridgeKind::Callable),
            member("createBashToolDefinition", BridgeKind::Callable),
            member("createEditTool", BridgeKind::Callable),
            member("createEditToolDefinition", BridgeKind::Callable),
            member("createFindTool", BridgeKind::Callable),
            member("createFindToolDefinition", BridgeKind::Callable),
            member("createGrepTool", BridgeKind::Callable),
            member("createGrepToolDefinition", BridgeKind::Callable),
            member("createLsTool", BridgeKind::Callable),
            member("createLsToolDefinition", BridgeKind::Callable),
            member("createReadTool", BridgeKind::Callable),
            member("createReadToolDefinition", BridgeKind::Callable),
            member("createWriteTool", BridgeKind::Callable),
            member("createWriteToolDefinition", BridgeKind::Callable),
            member("formatReadLineRange", BridgeKind::Callable),
            member("getAgentDir", BridgeKind::Callable),
            shared("getMarkdownTheme", BridgeKind::Callable, "kiana-tui"),
            shared("getSettingsListTheme", BridgeKind::Callable, "kiana-tui"),
            shared("keyHint", BridgeKind::Callable, "kiana-tui"),
            member("parseFrontmatter", BridgeKind::Callable),
            member("truncateHead", BridgeKind::Callable),
        ],
    },
];

/// 内嵌虚拟文件表（typebox 691 个 `.mjs` + 11 个 shim/helper），由 `build.rs` 生成。
///
/// 每次调用只构造一个视图（两个 `&'static` 切片，无分配、无 IO）。表项名就是 QuickJS 模块名，
/// 必须与 loader 交给 `Module::declare` 的名字逐字节一致，否则同一个模块会在缓存里出现两份
/// （对 typebox 意味着两份 `Type` 对象）。生成物不要手改：源文件是
/// `vendor/typebox/build/**/*.mjs` 与 `js/virtual/*.js`，下次构建会重新生成。测试用
/// [`virtual_files`] 校验表形状（691 + 11 项、全部为 `.mjs`/`.js`、全部是合法 UTF-8）。
pub fn virtual_fs() -> VirtualFs {
    VirtualFs::new(VIRTUAL_FILES, VIRTUAL_ALIASES)
}

/// 查找某个公开键对应的 namespace 描述。
///
/// 线性扫描、整串精确匹配：子路径（例如 `@earendil-works/kiana-ai/providers`，少了 `/all`）
/// 不是虚拟键，返回 `None`，由 loader 继续走 `node_modules`。同一 namespace 的多个键返回
/// 同一个 [`NamespaceSpec`]。
pub fn namespace_spec(key: &str) -> Option<&'static NamespaceSpec> {
    NAMESPACES.iter().find(|spec| spec.keys.contains(&key))
}

/// 查找某个 namespace id 的描述。
///
/// `id` 是桥分区名（[`NamespaceSpec::id`]），不是公开键：例如 `"kiana-ai/models"` 与
/// `"kiana-ai"` 是不同 namespace。未知 id 返回 `None`。
pub fn spec_by_id(id: &str) -> Option<&'static NamespaceSpec> {
    NAMESPACES.iter().find(|spec| spec.id == id)
}

/// 内嵌的 typebox + shim 文件表（测试与诊断用）。
///
/// 顺序由 `build.rs` 固定：typebox 文件按相对路径排序，随后是 `SHIM_FILES` 的固定顺序；
/// 与 [`virtual_fs`] 看到的是同一张表。
pub fn virtual_files() -> &'static [VirtualFile] {
    VIRTUAL_FILES
}

/// 22 个公开键的别名表（测试与诊断用）。
///
/// 顺序与 `VIRTUAL_MODULE_KEYS` 一致（测试 `alias_table_matches_the_22_keys_in_order` 断言）；
/// 每个别名都是单跳：`target` 必须是 [`virtual_files`] 里的文件，不能是另一个别名。
pub fn virtual_aliases() -> &'static [VirtualAlias] {
    VIRTUAL_ALIASES
}

/// 安装 22 个虚拟模块（§9.2 第 5–6 步；幂等）。
///
/// 前置条件：runtime 上已经装好模块 loader（`loader-runtime` 的 `Resolver + Loader`）。
/// typebox 的 691 个文件互相存在导入环（例如 `type/engine/instantiate.mjs` 与
/// `type/engine/instantiate_add.mjs`），无法靠按依赖顺序预声明来链接；必须由 loader 在
/// `Module::import` 时按需声明/链接（`VirtualFs` + 本模块登记的模块源提供服务）。
///
/// 步骤：
/// 1. [`register_module_sources`]：把全部虚拟文件登记到 [`JsEngine::register_module_source`]
///    （规范名 + 22 个公开键兜底）；
/// 2. 求值 typebox 三个入口（index/compile/value）一次，模块 namespace 即 typebox 的键；
/// 3. 求值 bridge.js 与三个纯 JS helper 模块，构建 kiana-* 的 namespace 种子对象
///    （`kiana-virtual/helpers/*.js` 也在 [`virtual_fs`] 表内，所以真实 Resolver 能规范化它们）；
/// 4. 建 `globalThis.__kiana_virtual`（22 键）与 `globalThis.__kiana_native`（空桥）。
///
/// # 幂等与失败恢复
///
/// 幂等判据是 [`is_installed`]（只看 `__kiana_virtual` 是否存在）。该全局在安装流程第 5 步
/// 才写入，因此此前任何失败都可以直接重试：重试会重新登记模块源并重走构建流程（已求值的
/// 模块由 QuickJS 模块缓存直接返回）；已寄存的 `Persistent` 以
/// `kiana.virtual.namespace.<id>` 为键，重复寄存是同键覆盖。
///
/// 但第 6 步（写 `__kiana_native` 与安装原生桥）失败时，`__kiana_virtual` 已经存在，之后的
/// [`install`] 会直接返回 `Ok(())`，不会再补救原生桥。该步的失败源是 JS 对象/函数创建
/// （`install_native_bridge` 只是把编译期固定的成员表写成全局）。
///
/// 整个安装在一个显式的 JS 事务 deadline 内执行（`INSTALL_DEADLINE` = 30 秒），用于覆盖
/// debug 构建下链接/求值 691 个 typebox 模块的耗时；它不改引擎的全局看门狗。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    if is_installed(engine) {
        return Ok(());
    }
    engine.with_js_deadline(INSTALL_DEADLINE, |ctx| install_inner(engine, &ctx))
}

/// 是否已经安装（`__kiana_virtual` 存在）。供组装器与测试判断。
///
/// 只检查全局对象是否存在，不校验 22 个键、规范名别名或原生桥是否完整；装配器的就绪检查
/// （`Generation::is_ready`）另有更严格的校验。
pub fn is_installed(engine: &JsEngine) -> bool {
    engine.with_js(|ctx| {
        matches!(
            ctx.globals().get::<_, Option<Object>>(VIRTUAL_GLOBAL),
            Ok(Some(_))
        )
    })
}

/// 让其它单元往原生桥里写一个成员（`__kiana_native[namespace][name] = build(ctx)`）。
///
/// 与直接写 `globalThis.__kiana_native` 等价；桥在第一次写入时创建。
///
/// `build` 在 `with_js` 事务内执行，可以创建任意 JS 值；它抛出的异常原样返回。写入是
/// `Object::set` 同名覆盖，所以后安装的单元可以覆盖先前的实现。由于种子对象上的 Proxy 每次
/// **使用**时才查询桥，写入顺序不影响已有引用。
///
/// 必须在 `with_js` 之外调用（本函数自己进入 `with_js`）。
pub fn set_native_member<F>(
    engine: &JsEngine,
    namespace: &str,
    name: &str,
    build: F,
) -> EngineResult<()>
where
    F: for<'js> FnOnce(&Ctx<'js>) -> EngineResult<Value<'js>>,
{
    engine.with_js(|ctx| {
        let value = build(&ctx)?;
        let bridge = ensure_object_property(&ctx, &ctx.globals(), NATIVE_BRIDGE_GLOBAL)?;
        let target = ensure_object_property(&ctx, &bridge, namespace)?;
        target.set(name, value)
    })
}

/// 一次性把一个 namespace 的桥成员写入 `__kiana_native`（`build` 返回的对象即该 namespace）。
///
/// 与多次 [`set_native_member`] 等价；`ui/adapter.rs` 等单元可用它成组安装。
///
/// 注意是**整组替换**（`bridge[namespace] = members`）而不是逐成员合并：此前写入同一
/// namespace 的成员全部被新对象取代。需要保留已有成员时，先读出旧对象再合并后返回。
pub fn set_native_namespace<F>(engine: &JsEngine, namespace: &str, build: F) -> EngineResult<()>
where
    F: for<'js> FnOnce(&Ctx<'js>) -> EngineResult<Object<'js>>,
{
    engine.with_js(|ctx| {
        let members = build(&ctx)?;
        let bridge = ensure_object_property(&ctx, &ctx.globals(), NATIVE_BRIDGE_GLOBAL)?;
        bridge.set(namespace, members)
    })
}

/// 返回 `parent[name]`，不存在时创建一个普通对象并写回。
///
/// 只把「属性不存在」当作创建条件；属性已存在但不是对象时，`get::<Option<Object>>` 的
/// 类型转换失败会原样返回（不覆盖已有值）。
fn ensure_object_property<'js>(
    ctx: &Ctx<'js>,
    parent: &Object<'js>,
    name: &str,
) -> EngineResult<Object<'js>> {
    if let Some(object) = parent.get::<_, Option<Object>>(name)? {
        return Ok(object);
    }
    let object = Object::new(ctx.clone())?;
    parent.set(name, object.clone())?;
    Ok(object)
}

/// 登记全部虚拟模块源（规范名 + 22 个公开键兜底 + 内部 helper）。
///
/// 可在 `engine.set_loader` 之前调用：loader 若按 `engine.module_sources()` 建快照，需要先看到
/// 这些条目。幂等。
///
/// 登记分三轮：全部虚拟文件按规范名 → 4 个安装期 helper 按固定名字再登记一次（它们本来就在
/// `VIRTUAL_FILES` 里，重复登记是同键覆盖）→ 22 个公开键。公开键的源一般是其别名目标模块的
/// 同一段源；typebox 三个键例外，用 `TYPEBOX_*_SHIM` 里的静态再导出（桶文件带 `./...` 相对
/// 导入，公开键与桶文件不在同一目录）。
///
/// 失败路径：内嵌源不是合法 UTF-8 时返回加载错误；别名目标不在 [`virtual_files`] 表内时
/// `expect` panic——这条不变量由 `build.rs` 生成逻辑与
/// `tests/virtual_modules.rs::every_alias_target_is_an_embedded_file` 共同保证。
pub fn register_module_sources(engine: &JsEngine) -> EngineResult<()> {
    for file in VIRTUAL_FILES {
        engine.register_module_source(file.name, source_str(file)?);
    }
    for (module, source) in [
        (BRIDGE_MODULE, BRIDGE_SOURCE),
        (KIANA_AI_HELPERS_MODULE, KIANA_AI_HELPERS_SOURCE),
        (KIANA_TUI_HELPERS_MODULE, KIANA_TUI_HELPERS_SOURCE),
        (
            KIANA_CODING_AGENT_HELPERS_MODULE,
            KIANA_CODING_AGENT_HELPERS_SOURCE,
        ),
    ] {
        engine.register_module_source(module, source);
    }
    for (key, module) in alias_pairs() {
        // typebox 的公开键用相对导入安全的兜底 shim（模块名不含目录，不能直接复用桶文件源）；
        // kiana-* 的 shim 只用 `globalThis.__kiana_virtual`，可以直接复用规范模块源。
        // `key == module` 是防御分支：当前 22 个键的公开名与规范名都不同，不会命中。
        let source = match (key == module, typebox_fallback_source(module)) {
            (false, Some(fallback)) => fallback,
            _ => engine
                .module_source(module)
                .expect("every alias target is registered from VIRTUAL_FILES"),
        };
        engine.register_module_source(key, source);
    }
    Ok(())
}

fn install_inner(engine: &JsEngine, ctx: &Ctx<'_>) -> EngineResult<()> {
    // 各步都不回滚：已登记的模块源/已求值的模块留在引擎状态里（重复执行无害）；
    // `__kiana_virtual` 在倒数第二步才写，所以中途失败后可以整体重试。
    // 1. 模块源登记（loader 只需一次 `module_source` 查询）。
    register_module_sources(engine)?;

    // 2. typebox 求值一次（§8.5）：三个入口共享同一模块缓存（同一份 `Type` 对象）。
    //    导入环由 loader 处理（见 [`install`] 的前置条件）。
    let typebox_root = import_typebox_entry(ctx, "typebox/build/index.mjs")?;
    let typebox_compile = import_typebox_entry(ctx, "typebox/build/compile/index.mjs")?;
    let typebox_value = import_typebox_entry(ctx, "typebox/build/value/index.mjs")?;

    // 3. 纯 JS helper 模块（bridge.js 提供 makeNamespace/defineHidden）。
    let bridge_module = eval_helper(ctx, BRIDGE_MODULE, BRIDGE_SOURCE)?;
    let make_namespace: Function = bridge_module.get("makeNamespace")?;
    let define_hidden: Function = bridge_module.get("defineHidden")?;
    let helper_modules: HashMap<&'static str, Object> = HashMap::from([
        (
            KIANA_AI_HELPERS_MODULE,
            eval_helper(ctx, KIANA_AI_HELPERS_MODULE, KIANA_AI_HELPERS_SOURCE)?,
        ),
        (
            KIANA_TUI_HELPERS_MODULE,
            eval_helper(ctx, KIANA_TUI_HELPERS_MODULE, KIANA_TUI_HELPERS_SOURCE)?,
        ),
        (
            KIANA_CODING_AGENT_HELPERS_MODULE,
            eval_helper(
                ctx,
                KIANA_CODING_AGENT_HELPERS_MODULE,
                KIANA_CODING_AGENT_HELPERS_SOURCE,
            )?,
        ),
    ]);

    // 4. 每个 namespace 的种子对象。
    let mut seeds: Vec<(&'static str, Object)> = Vec::with_capacity(NAMESPACES.len());
    for spec in NAMESPACES {
        let seed = match spec.id {
            "typebox" => typebox_root.clone(),
            "typebox/compile" => typebox_compile.clone(),
            "typebox/value" => typebox_value.clone(),
            _ => build_namespace_seed(ctx, spec, &make_namespace, &helper_modules)?,
        };
        seeds.push((spec.id, seed));
    }

    // 每个 namespace 一个 `Persistent<Object>`（§3.3 的身份要求：单 Context + 每键一个
    // namespace；键组共享同一对象）。关闭时由引擎按逆序统一清理。
    for (id, seed) in &seeds {
        engine.put_persistent(
            format!("kiana.virtual.namespace.{id}"),
            rquickjs::Persistent::save(ctx, seed.clone()),
        );
    }

    // 5. `__kiana_virtual`：22 个键 → 对应 namespace（同一对象），插入顺序与
    //    `virtual-modules.ts` 的对象字面量一致（JS 对象键顺序可被扩展观察到）。
    let virtual_modules = Object::new(ctx.clone())?;
    for key in VIRTUAL_MODULE_KEYS {
        let spec = namespace_spec(key).expect("every virtual key has a namespace");
        let seed = &seeds
            .iter()
            .find(|(id, _)| *id == spec.id)
            .expect("every namespace has a seed")
            .1;
        virtual_modules.set(key, seed.clone())?;
    }
    // 规范模块名 → 同一 namespace（**不可枚举**）：CJS 的 `require` 走与 ESM 相同的解析器，
    // 拿到的是别名解析后的规范名（`typebox/build/index.mjs`、`kiana-virtual/kiana-ai.js`），而
    // `js/cjs.js` 用 `hasOwnProperty(globalThis.__kiana_virtual, <resolved>)` 分派虚拟模块。
    // 隐藏键让这条路径命中，同时 `Object.keys(__kiana_virtual)` 仍是 22 个公开键（设计 §3.5）。
    for spec in NAMESPACES {
        let seed = &seeds
            .iter()
            .find(|(id, _)| *id == spec.id)
            .expect("every namespace has a seed")
            .1;
        define_hidden.call::<_, ()>((virtual_modules.clone(), spec.module, seed.clone()))?;
    }
    ctx.globals().set(VIRTUAL_GLOBAL, virtual_modules)?;

    // 6. 原生桥：本单元负责的 Rust 实现（kiana-ai 的表函数、coding-agent 的纯函数与工具工厂），
    //    其它单元（ui-adapter）之后可以覆盖同名成员。
    let bridge = Object::new(ctx.clone())?;
    ctx.globals().set(NATIVE_BRIDGE_GLOBAL, bridge.clone())?;
    install_native_bridge(engine, ctx, &bridge)?;

    Ok(())
}

/// 构建一个 kiana-* namespace 的种子对象：JS 值 + Rust 常量 + 动态桥成员。
///
/// 传给 JS 的 `spec` 形状是 `{id, bridge: [[name, kind, namespace, bridgeName], ...]}`，与
/// `js/virtual/bridge.js#makeNamespace` 的解构顺序一致。`values` 先作为数据属性拷入，桥成员
/// 随后赋值（两类导出名不重叠，由表的不变量保证）。
///
/// `helper.get` 出错时把错误改写成 loading 消息（指出 helper 模块与缺失的导出名）；
/// `makeNamespace` 抛出的 JS 异常原样返回。
fn build_namespace_seed<'js>(
    ctx: &Ctx<'js>,
    spec: &'static NamespaceSpec,
    make_namespace: &Function<'js>,
    helper_modules: &HashMap<&'static str, Object<'js>>,
) -> EngineResult<Object<'js>> {
    let values = Object::new(ctx.clone())?;
    if let Some(helper_name) = spec.helper {
        let helper = helper_modules
            .get(helper_name)
            .expect("helper module is evaluated before namespaces are built");
        for name in spec.js_values {
            let value: Value = helper.get(*name).map_err(|_| {
                rquickjs::Error::new_loading_message(
                    helper_name.to_string(),
                    format!("missing helper export `{name}`"),
                )
            })?;
            values.set(*name, value)?;
        }
    }
    for &(name, value) in spec.constants {
        value.apply(&values, name)?;
    }
    // 每个桥成员编码成 4 元数组 [name, kind, namespace, bridgeName]；bridge.js 用同一顺序
    // 解构。`namespace` 缺省取所属 namespace，`bridgeName` 缺省取导出名。
    let bridge: Vec<Value> = spec
        .bridge
        .iter()
        .map(|member| {
            let entry = rquickjs::Array::new(ctx.clone())?;
            entry.set(0, member.name)?;
            entry.set(1, member.kind.as_str())?;
            entry.set(2, member.source.unwrap_or(spec.id))?;
            entry.set(3, member.bridge_name.unwrap_or(member.name))?;
            Ok(entry.into_value())
        })
        .collect::<EngineResult<Vec<_>>>()?;
    let bridge_array = rquickjs::Array::new(ctx.clone())?;
    for (index, entry) in bridge.into_iter().enumerate() {
        bridge_array.set(index, entry)?;
    }
    let spec_object = Object::new(ctx.clone())?;
    spec_object.set("id", spec.id)?;
    spec_object.set("bridge", bridge_array)?;
    make_namespace.call::<_, Object>((spec_object, values))
}

/// 求值一个内部 helper 模块并返回其 namespace。
///
/// 先 `declare` 把源码与名字绑定，再 `import` 触发求值：QuickJS 的模块表以名字为键，两个调用
/// 必须用同一个名字，否则会得到两份模块实例。
fn eval_helper<'js>(ctx: &Ctx<'js>, name: &str, source: &'static str) -> EngineResult<Object<'js>> {
    // helper 没有相对导入；declare 后立刻 import（句柄活到 `import_namespace` 返回）。
    let _module = Module::declare(ctx.clone(), name, source)?;
    import_namespace(ctx, name)
}

/// 求值 typebox 入口；失败时把挂起异常物化并说明安装前置条件。
fn import_typebox_entry<'js>(ctx: &Ctx<'js>, name: &str) -> EngineResult<Object<'js>> {
    match import_namespace(ctx, name) {
        Ok(namespace) => Ok(namespace),
        Err(error) => {
            let caught = crate::engine::take_exception(ctx, crate::engine::JsErrorKind::Host);
            let detail = if caught.message.is_empty() {
                format!("{error:?}")
            } else {
                caught.message
            };
            Err(rquickjs::Error::new_loading_message(
                name.to_string(),
                format!(
                    "{detail} (virtual_modules::install needs the runtime module loader; install \
                     loader-runtime's Resolver + Loader before virtual modules — design §9.2)"
                ),
            ))
        }
    }
}

/// 求值/取回已声明的模块 namespace（§3.9：先 declare，靠 import 触发求值）。
fn import_namespace<'js>(ctx: &Ctx<'js>, name: &str) -> EngineResult<Object<'js>> {
    let promise = Module::import(ctx, name)?;
    // 同步模块（typebox / shim 都没有顶层 await）在 `JS_LoadModule` 返回时已 resolve；
    // `finish` 仍会泵 job 队列，遇到真正的 TLA 返回 WouldBlock。
    promise.finish::<Object>()
}

/// 虚拟文件的 UTF-8 源（`&'static`，供 `register_module_source`）。
///
/// 字节来自 `build.rs` 的 `include_bytes!`。测试断言整张表都是合法 UTF-8，因此这条错误路径只
/// 在生成表损坏（例如 vendor 里混入非 UTF-8 文件）时才会走到。
fn source_str(file: &'static VirtualFile) -> EngineResult<&'static str> {
    from_utf8(file.source).map_err(|_| {
        rquickjs::Error::new_loading_message(
            file.name.to_string(),
            "embedded source is not valid UTF-8",
        )
    })
}

/// `(public key, canonical module)` 对（来自 build.rs 的表）。
///
/// 顺序即 `VIRTUAL_ALIASES` 的表顺序（与 `VIRTUAL_MODULE_KEYS` 相同，测试断言）；登记本身
/// 与顺序无关（`register_module_source` 按名字插入，同键覆盖）。
fn alias_pairs() -> impl Iterator<Item = (&'static str, &'static str)> {
    VIRTUAL_ALIASES
        .iter()
        .map(|alias| (alias.key, alias.target))
}

/// typebox 三个规范入口的公开键兜底源；其它规范名返回 `None`（复用 [`virtual_files`] 里的
/// 原源）。
///
/// 三个 shim 都是一行 `export * from "<规范名>"` 加 `default` 再导出，本身不含相对说明符，
/// 因此登记到公开键名下时不会引入额外的路径解析。
fn typebox_fallback_source(module: &str) -> Option<&'static str> {
    match module {
        "typebox/build/index.mjs" => Some(TYPEBOX_ROOT_SHIM),
        "typebox/build/compile/index.mjs" => Some(TYPEBOX_COMPILE_SHIM),
        "typebox/build/value/index.mjs" => Some(TYPEBOX_VALUE_SHIM),
        _ => None,
    }
}

// ===========================================================================
// 原生桥：本单元负责的 Rust 实现（设计 §8.2、§8.6）
// ===========================================================================
//
// `install` 在建立 `__kiana_virtual` 之后把 Rust 实现写进 `__kiana_native`：
//
// - `kiana-ai/models`：`calculateCost`、`clampThinkingLevel`、`getModelType`、`hasApi`、
//   `isModelType`、`modelsAreEqual`、`getSupportedThinkingLevels`；
// - `kiana-ai/providers-all`：目录读取（`getBuiltin*`）；
// - `kiana-ai`：`parseStreamingJson`、`findEnvKeys`、`getEnvApiKey`、`getImage*`；
// - `kiana-coding-agent`：`getAgentDir`、`convertToLlm`、`formatReadLineRange`、`truncateHead`、
//   7 个工具工厂（`create*Tool` / `create*ToolDefinition` 各 7 个）。纯 JS 的 `formatSize` /
//   `truncateLine` / `serializeConversation` / `withFileMutationQueue` 在 helper 模块里。
//
// 异步成员（provider 流、工具 execute）走 `__kiana.promise` 宿主协议：JS helper 建
// `AssistantMessageEventStream` / Promise，Rust host kind 任务把结果或事件投回主循环。

/// 创建工具时分配的 id（execute 通过宿主 promise 协议回调到这里）。
static TOOL_SEQ: AtomicU64 = AtomicU64::new(0);
/// `id -> AgentTool`；工具对象在 JS 侧持有 id，Rust 侧持有实现。
static TOOL_REGISTRY: LazyLock<Mutex<HashMap<u64, Arc<AgentTool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `compatModels` 的等价物（`builtinModels()` 只构造一次）。
static BUILTIN_MODELS: LazyLock<Arc<dyn kiana_ai::models::MutableModels>> =
    LazyLock::new(|| kiana_ai::providers::all::builtin_models(None));

/// 宿主 kind：创建 provider 流。payload：`{api, mode, model, context, options, onEvent}`。
const PROVIDER_STREAM_KIND: &str = "kiana-ai.provider-stream";
/// 宿主 kind：执行工具。payload：`{toolId, toolCallId, params, signal?, onUpdate?}`。
const TOOL_EXECUTE_KIND: &str = "kiana.codex.tool-execute";

/// 把本单元负责的 Rust 成员写进 `__kiana_native` 的四个分区，并注册两个 host kind。
///
/// 分区固定为 `kiana-ai/models`、`kiana-ai`、`kiana-ai/providers-all`、`kiana-coding-agent`；
/// 其它分区（`kiana-ai/oauth` 等）由别的单元按需写入。`register_host_kind` 对同名 kind 是
/// 覆盖语义，因此重复安装一代不会 panic。
///
/// 失败时只可能来自 JS 对象/函数创建（成员表在编译期固定），返回 `Err` 后桥可能只装了一半；
/// 调用方是 [`install`]，它不会回滚（见该函数的失败恢复说明）。
fn install_native_bridge<'js>(
    engine: &JsEngine,
    ctx: &Ctx<'js>,
    bridge: &Object<'js>,
) -> EngineResult<()> {
    let models = ensure_object_property(ctx, bridge, "kiana-ai/models")?;
    let ai = ensure_object_property(ctx, bridge, "kiana-ai")?;
    let providers = ensure_object_property(ctx, bridge, "kiana-ai/providers-all")?;
    let coding = ensure_object_property(ctx, bridge, "kiana-coding-agent")?;

    install_models_bridge(ctx, &models)?;
    install_providers_bridge(ctx, &providers)?;
    install_ai_bridge(ctx, &ai)?;
    install_coding_agent_bridge(ctx, &coding)?;

    engine.register_host_kind(PROVIDER_STREAM_KIND, start_provider_stream);
    engine.register_host_kind(TOOL_EXECUTE_KIND, start_tool_execute);
    Ok(())
}

/// 安装一个已存在的成员不覆盖（其它单元可以先写，也可以后覆盖）。
fn install_function<'js, P, F>(
    ctx: &Ctx<'js>,
    target: &Object<'js>,
    name: &str,
    function: F,
) -> EngineResult<()>
where
    F: rquickjs::function::IntoJsFunc<'js, P> + 'js,
{
    if target.get::<_, Option<Value>>(name)?.is_some() {
        return Ok(());
    }
    target.set(name, Function::new(ctx.clone(), function)?)
}

/// JS 值 → JSON（`undefined` 视为 `null`）。走 `JSON.stringify`，键序与 JS 一致。
///
/// `JSON.stringify` 失败（循环引用、BigInt 等）时把引擎错误原样返回。成功时键序保留
/// （`serde_json` 开了 `preserve_order`），Rust 侧再序列化回 JS 时顺序不变。
fn js_to_json<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> EngineResult<serde_json::Value> {
    match ctx.json_stringify(value.clone())? {
        Some(text) => {
            let text = text.to_string()?;
            serde_json::from_str(&text).map_err(|error| {
                Exception::throw_message(ctx, &format!("invalid JSON value: {error}"))
            })
        }
        None => Ok(serde_json::Value::Null),
    }
}

/// JSON → JS 值（`JSON.parse` 语义）。
///
/// 用 `JSON.parse` 而不是逐键 `Object::set` 物化：`"__proto__"` 会作为自有属性写入、数字按
/// JSON 语法解析、键序保持插入序（与引擎的 `HostValue::Json` 物化同一策略）。
fn json_to_js<'js>(ctx: &Ctx<'js>, value: &serde_json::Value) -> EngineResult<Value<'js>> {
    let text = serde_json::to_string(value)
        .map_err(|error| Exception::throw_message(ctx, &error.to_string()))?;
    ctx.json_parse(text)
}

/// 可选参数 → `Option<JSON>`（`undefined`/缺失 → `None`；`null` 保留为 `Some(Null)`）。
fn opt_js_to_json<'js>(
    ctx: &Ctx<'js>,
    value: &Option<Value<'js>>,
) -> EngineResult<Option<serde_json::Value>> {
    match value {
        Some(value) if !value.is_undefined() => Ok(Some(js_to_json(ctx, value)?)),
        _ => Ok(None),
    }
}

/// 写入 JSON 值；`None` 写成 `undefined`（TS 的可选字段）。
fn set_opt_json<'js>(
    ctx: &Ctx<'js>,
    target: &Object<'js>,
    name: &str,
    value: Option<&serde_json::Value>,
) -> EngineResult<()> {
    match value {
        Some(value) => target.set(name, json_to_js(ctx, value)?),
        None => target.set(name, Value::new_undefined(ctx.clone())),
    }
}

/// `model.cost` / `usage` 之类的 JSON 数字（整数或浮点都接受）。
///
/// 只认 JSON number；数字字符串、布尔、null 都返回 `None`，调用方据此保留默认值而不报错。
fn json_number(value: &serde_json::Value) -> Option<f64> {
    value.as_f64()
}

/// JSON → `u64` 的宽松转换：非负整数直接取；浮点向零截断（负值先夹到 0）；非数字（字符串、
/// null、缺失）返回 0。
///
/// 用于 usage 计数等字段；任何畸形值都退化成 0，不产生错误。
fn json_u64(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|number| number.max(0.0) as u64))
        .unwrap_or(0)
}

/// 解析模型对象；字段不完整时退回部分模型（TS 的结构化类型允许只带用到的字段）。
///
/// 先尝试完整的 `serde` 反序列化（`Model` 的必填字段缺失或类型不符即失败）；失败时走
/// [`partial_model`] 的宽松路径，因此扩展传入的“半个模型对象”不会让调用直接抛错。
fn model_from_json(value: &serde_json::Value) -> Model {
    serde_json::from_value(value.clone()).unwrap_or_else(|_| partial_model(value))
}

/// 宽松模型解析：逐字段按 TS 的 camelCase 名读取，缺省或类型不符的字段保持 `Model::default()`
/// 的值。
///
/// 只认识 `id`/`name`/`api`/`provider`/`baseUrl`/`cost`/`type`/`reasoning`/`thinkingLevelMap`/
/// `contextWindow`/`maxTokens`；其余字段丢弃。非对象输入返回默认模型（不报错）。
fn partial_model(value: &serde_json::Value) -> Model {
    let mut model = Model::default();
    let Some(object) = value.as_object() else {
        return model;
    };
    if let Some(id) = object.get("id").and_then(serde_json::Value::as_str) {
        model.id = id.to_owned();
    }
    if let Some(name) = object.get("name").and_then(serde_json::Value::as_str) {
        model.name = name.to_owned();
    }
    if let Some(api) = object.get("api")
        && let Ok(api) = serde_json::from_value(api.clone())
    {
        model.api = api;
    }
    if let Some(provider) = object.get("provider")
        && let Ok(provider) = serde_json::from_value(provider.clone())
    {
        model.provider = provider;
    }
    if let Some(base_url) = object.get("baseUrl").and_then(serde_json::Value::as_str) {
        model.base_url = base_url.to_owned();
    }
    if let Some(cost) = object.get("cost")
        && let Ok(cost) = serde_json::from_value(cost.clone())
    {
        model.cost = cost;
    }
    if let Some(model_type) = object.get("type")
        && let Ok(model_type) = serde_json::from_value(model_type.clone())
    {
        model.model_type = Some(model_type);
    }
    model.reasoning = object.get("reasoning").and_then(serde_json::Value::as_bool);
    if let Some(levels) = object.get("thinkingLevelMap")
        && let Ok(levels) = serde_json::from_value(levels.clone())
    {
        model.thinking_level_map = Some(levels);
    }
    model.context_window = object
        .get("contextWindow")
        .and_then(serde_json::Value::as_u64);
    model.max_tokens = object.get("maxTokens").and_then(serde_json::Value::as_u64);
    model
}

/// 把模型包成 [`kiana_ai::types::AnyModel`]：`type` 缺省按 `chat` 处理，其余值决定枚举分支。
///
/// 这与 `getModelType` 的默认一致；`isModelType`/`hasApi` 等只读模型类型的成员走这里。
fn any_model_from_json(value: &serde_json::Value) -> kiana_ai::types::AnyModel {
    let model = model_from_json(value);
    match model.model_type.unwrap_or(ModelType::Chat) {
        ModelType::Chat => kiana_ai::types::AnyModel::Chat(model),
        ModelType::Image => kiana_ai::types::AnyModel::Image(model),
        ModelType::Classifier => kiana_ai::types::AnyModel::Classifier(model),
    }
}

/// JS 的 env 对象（`Record<string, string>`）→ `ProviderEnv`。
///
/// 只收字符串值，非字符串条目被丢弃；输入缺失或不是对象时返回 `None`（没有覆盖表，键查找会
/// 回落到进程环境）。空对象返回 `Some(空 map)`，而不是 `None`。
fn provider_env_from_json(
    value: Option<&serde_json::Value>,
) -> Option<kiana_ai::types::ProviderEnv> {
    let object = value?.as_object()?;
    let mut env = kiana_ai::types::ProviderEnv::new();
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            env.insert(key.clone(), text.to_owned());
        }
    }
    Some(env)
}

/// 安装 `kiana-ai/models` 分区的 7 个成员。
///
/// 参数经 JSON 边界转换。`calculateCost` 与 TS 一样**原地写回** `usage.cost` 并同时返回该
/// cost 对象。`clampThinkingLevel` / `isModelType` 收到未知枚举字符串时抛 JS Error（文案分别
/// 为 `unknown thinking level: …` / `unknown model type: …`）；其余成员对缺省/多余字段宽容。
fn install_models_bridge<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> EngineResult<()> {
    install_function(
        ctx,
        target,
        "calculateCost",
        |ctx: Ctx<'js>, model: Value<'js>, usage: Object<'js>| -> EngineResult<Value<'js>> {
            let model = model_from_json(&js_to_json(&ctx, &model)?);
            let usage_json = js_to_json(&ctx, &usage.clone().into_value())?;
            let mut parsed = kiana_ai::types::Usage::default();
            if let Some(object) = usage_json.as_object() {
                if let Some(value) = object.get("input") {
                    parsed.input = json_u64(value);
                }
                if let Some(value) = object.get("output") {
                    parsed.output = json_u64(value);
                }
                if let Some(value) = object.get("cacheRead") {
                    parsed.cache_read = json_u64(value);
                }
                if let Some(value) = object.get("cacheWrite") {
                    parsed.cache_write = json_u64(value);
                }
                if let Some(value) = object.get("cacheWrite1h") {
                    parsed.cache_write1h = Some(json_u64(value));
                }
                if let Some(value) = object.get("reasoning") {
                    parsed.reasoning = Some(json_u64(value));
                }
                if let Some(value) = object.get("totalTokens") {
                    parsed.total_tokens = json_u64(value);
                }
                if let Some(value) = object.get("cost")
                    && let Ok(cost) = serde_json::from_value(value.clone())
                {
                    parsed.cost = cost;
                }
            }
            let cost = kiana_ai::models::cost::calculate_cost(&model, &mut parsed);
            let cost_json = serde_json::to_value(&cost)
                .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
            let cost_value = json_to_js(&ctx, &cost_json)?;
            usage.set("cost", cost_value.clone())?;
            Ok(cost_value)
        },
    )?;
    install_function(
        ctx,
        target,
        "clampThinkingLevel",
        |ctx: Ctx<'js>, model: Value<'js>, level: String| -> EngineResult<Value<'js>> {
            let model = model_from_json(&js_to_json(&ctx, &model)?);
            let level: kiana_ai::types::ModelThinkingLevel =
                serde_json::from_value(serde_json::Value::String(level.clone())).map_err(|_| {
                    Exception::throw_message(&ctx, &format!("unknown thinking level: {level}"))
                })?;
            let clamped = kiana_ai::models::thinking::clamp_thinking_level(&model, level);
            json_to_js(
                &ctx,
                &serde_json::to_value(clamped).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getSupportedThinkingLevels",
        |ctx: Ctx<'js>, model: Value<'js>| -> EngineResult<Value<'js>> {
            let model = model_from_json(&js_to_json(&ctx, &model)?);
            let levels = kiana_ai::models::thinking::get_supported_thinking_levels(&model);
            json_to_js(
                &ctx,
                &serde_json::to_value(levels).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getModelType",
        |ctx: Ctx<'js>, model: Value<'js>| -> EngineResult<Value<'js>> {
            let model = any_model_from_json(&js_to_json(&ctx, &model)?);
            let kind = kiana_ai::utils::model_operations::get_model_type(&model);
            json_to_js(
                &ctx,
                &serde_json::to_value(kind).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "isModelType",
        |ctx: Ctx<'js>, model: Value<'js>, kind: String| -> EngineResult<Value<'js>> {
            let model = any_model_from_json(&js_to_json(&ctx, &model)?);
            let kind: ModelType = serde_json::from_value(serde_json::Value::String(kind.clone()))
                .map_err(|_| {
                Exception::throw_message(&ctx, &format!("unknown model type: {kind}"))
            })?;
            Ok(Value::new_bool(
                ctx.clone(),
                kiana_ai::utils::model_operations::is_model_type(&model, kind),
            ))
        },
    )?;
    install_function(
        ctx,
        target,
        "hasApi",
        |ctx: Ctx<'js>, model: Value<'js>, api: String| -> EngineResult<Value<'js>> {
            let model = any_model_from_json(&js_to_json(&ctx, &model)?);
            Ok(Value::new_bool(
                ctx.clone(),
                kiana_ai::models::provider::has_api(&model, &api),
            ))
        },
    )?;
    install_function(
        ctx,
        target,
        "modelsAreEqual",
        |ctx: Ctx<'js>,
         left: Opt<Value<'js>>,
         right: Opt<Value<'js>>|
         -> EngineResult<Value<'js>> {
            let left = match &left.0 {
                Some(value) if !value.is_undefined() && !value.is_null() => {
                    Some(any_model_from_json(&js_to_json(&ctx, value)?))
                }
                _ => None,
            };
            let right = match &right.0 {
                Some(value) if !value.is_undefined() && !value.is_null() => {
                    Some(any_model_from_json(&js_to_json(&ctx, value)?))
                }
                _ => None,
            };
            Ok(Value::new_bool(
                ctx.clone(),
                kiana_ai::models::provider::models_are_equal(left.as_ref(), right.as_ref()),
            ))
        },
    )?;
    Ok(())
}

/// 安装 `kiana-ai/providers-all` 分区的内置目录读取函数（`getBuiltin*`）。
///
/// 单个查询找不到时返回 `undefined`（对应 TS 的 `undefined`）；列表类返回数组。JSON 序列化
/// 失败退化为 `null`。`getAllBuiltinModels` 只序列化每个模型的 `.model()` 部分，丢掉
/// `AnyModel` 的类型包装。
fn install_providers_bridge<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> EngineResult<()> {
    install_function(
        ctx,
        target,
        "getBuiltinModel",
        |ctx: Ctx<'js>, provider: String, model: String| -> EngineResult<Value<'js>> {
            match kiana_ai::providers::all::get_builtin_model(&provider, &model) {
                Some(model) => json_to_js(
                    &ctx,
                    &serde_json::to_value(model).unwrap_or(serde_json::Value::Null),
                ),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinModels",
        |ctx: Ctx<'js>, provider: String| -> EngineResult<Value<'js>> {
            let models = kiana_ai::providers::all::get_builtin_models(&provider);
            json_to_js(
                &ctx,
                &serde_json::to_value(models).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinImageModel",
        |ctx: Ctx<'js>, provider: String, model: String| -> EngineResult<Value<'js>> {
            match kiana_ai::providers::all::get_builtin_image_model(&provider, &model) {
                Some(model) => json_to_js(
                    &ctx,
                    &serde_json::to_value(model).unwrap_or(serde_json::Value::Null),
                ),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinImageModels",
        |ctx: Ctx<'js>, provider: String| -> EngineResult<Value<'js>> {
            let models = kiana_ai::providers::all::get_builtin_image_models(&provider);
            json_to_js(
                &ctx,
                &serde_json::to_value(models).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinClassifierModel",
        |ctx: Ctx<'js>, provider: String, model: String| -> EngineResult<Value<'js>> {
            match kiana_ai::providers::all::get_builtin_classifier_model(&provider, &model) {
                Some(model) => json_to_js(
                    &ctx,
                    &serde_json::to_value(model).unwrap_or(serde_json::Value::Null),
                ),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinClassifierModels",
        |ctx: Ctx<'js>, provider: String| -> EngineResult<Value<'js>> {
            let models = kiana_ai::providers::all::get_builtin_classifier_models(&provider);
            json_to_js(
                &ctx,
                &serde_json::to_value(models).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getAllBuiltinModels",
        |ctx: Ctx<'js>, provider: String| -> EngineResult<Value<'js>> {
            let models = kiana_ai::providers::all::get_all_builtin_models(&provider);
            let json: Vec<serde_json::Value> = models
                .iter()
                .map(|model| serde_json::to_value(model.model()).unwrap_or(serde_json::Value::Null))
                .collect();
            json_to_js(&ctx, &serde_json::Value::Array(json))
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinProviders",
        |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            let providers = kiana_ai::providers::all::get_builtin_providers();
            json_to_js(
                &ctx,
                &serde_json::to_value(providers).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getBuiltinModelDataGeneratedAt",
        |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            match kiana_ai::providers::all::get_builtin_model_data_generated_at() {
                Some(timestamp) => Ok(Value::new_number(ctx.clone(), timestamp as f64)),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    Ok(())
}

/// 安装 `kiana-ai` 分区的 Rust 实现：流式 JSON 解析、环境键查询、图片模型注册表与工具参数
/// 校验。
///
/// `findEnvKeys` / `getEnvApiKey` 无结果时返回 `undefined`（不是空数组或空串）。两个校验函数
/// 失败时抛 JS Error，文案来自 Rust 错误的 `to_string()`；`validateToolCall` 还要求第一个参数
/// 是数组，否则抛 `tools must be an array`。
fn install_ai_bridge<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> EngineResult<()> {
    install_function(
        ctx,
        target,
        "parseStreamingJson",
        |ctx: Ctx<'js>, partial: Opt<String>| -> EngineResult<Value<'js>> {
            let value = kiana_ai::utils::json_parse::parse_streaming_json(partial.0.as_deref());
            json_to_js(&ctx, &value)
        },
    )?;
    install_function(
        ctx,
        target,
        "findEnvKeys",
        |ctx: Ctx<'js>, provider: String, env: Opt<Value<'js>>| -> EngineResult<Value<'js>> {
            let env = provider_env_from_json(opt_js_to_json(&ctx, &env.0)?.as_ref());
            match kiana_ai::env_api_keys::find_env_keys(&provider, env.as_ref()) {
                Some(keys) => {
                    let values: Vec<serde_json::Value> = keys
                        .into_iter()
                        .map(|key| serde_json::Value::String(key.to_owned()))
                        .collect();
                    json_to_js(&ctx, &serde_json::Value::Array(values))
                }
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getEnvApiKey",
        |ctx: Ctx<'js>, provider: String, env: Opt<Value<'js>>| -> EngineResult<Value<'js>> {
            let env = provider_env_from_json(opt_js_to_json(&ctx, &env.0)?.as_ref());
            match kiana_ai::env_api_keys::get_env_api_key(&provider, env.as_ref()) {
                Some(key) => json_to_js(&ctx, &serde_json::Value::String(key)),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getImageModel",
        |ctx: Ctx<'js>, provider: String, model: String| -> EngineResult<Value<'js>> {
            match kiana_ai::image_models::get_image_model(&provider, &model) {
                Some(model) => json_to_js(
                    &ctx,
                    &serde_json::to_value(model).unwrap_or(serde_json::Value::Null),
                ),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "getImageModels",
        |ctx: Ctx<'js>, provider: String| -> EngineResult<Value<'js>> {
            let models = kiana_ai::image_models::get_image_models(&provider);
            json_to_js(
                &ctx,
                &serde_json::to_value(models).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "getImageProviders",
        |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            let providers = kiana_ai::image_models::get_image_providers();
            json_to_js(
                &ctx,
                &serde_json::to_value(providers).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "validateToolCall",
        |ctx: Ctx<'js>, tools: Value<'js>, tool_call: Value<'js>| -> EngineResult<Value<'js>> {
            let array = tools
                .as_array()
                .ok_or_else(|| Exception::throw_message(&ctx, "tools must be an array"))?;
            let mut parsed = Vec::new();
            for tool in array.iter::<Value<'js>>() {
                parsed.push(kiana_tool_from_js(&ctx, &tool?)?);
            }
            let tool_call = kiana_tool_call_from_js(&ctx, &tool_call)?;
            match kiana_ai::utils::validation::validate_tool_call(&parsed, &tool_call) {
                Ok(value) => json_to_js(&ctx, &value),
                Err(error) => Err(Exception::throw_message(&ctx, &error.to_string())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "validateToolArguments",
        |ctx: Ctx<'js>, tool: Value<'js>, tool_call: Value<'js>| -> EngineResult<Value<'js>> {
            let tool = kiana_tool_from_js(&ctx, &tool)?;
            let tool_call = kiana_tool_call_from_js(&ctx, &tool_call)?;
            match kiana_ai::utils::validation::validate_tool_arguments(&tool, &tool_call) {
                Ok(value) => json_to_js(&ctx, &value),
                Err(error) => Err(Exception::throw_message(&ctx, &error.to_string())),
            }
        },
    )?;
    Ok(())
}

/// 从 JS 工具对象读取 [`kiana_ai::types::Tool`]（TS 校验只读取 `name`/`parameters`）。
fn kiana_tool_from_js<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> EngineResult<kiana_ai::types::Tool> {
    let object = value
        .as_object()
        .ok_or_else(|| Exception::throw_message(ctx, "tool must be an object"))?;
    let name = object.get::<_, Option<String>>("name")?.unwrap_or_default();
    let description = object
        .get::<_, Option<String>>("description")?
        .unwrap_or_default();
    let parameters = match object.get::<_, Option<Value<'js>>>("parameters")? {
        Some(value) if !value.is_undefined() && !value.is_null() => js_to_json(ctx, &value)?,
        _ => serde_json::json!({}),
    };
    Ok(kiana_ai::types::Tool {
        name,
        description,
        parameters,
        constrained_sampling: None,
    })
}

/// 从 JS 工具调用对象读取 [`kiana_ai::types::ToolCall`]。
///
/// `arguments` 只接受对象：其它类型（含字符串、null）一律当作空参数表，且不报错。`id`/`name`
/// 缺失时取空串；`argumentsError` 原样带过（TS 用它标记“参数 JSON 解析失败”）。`kind` 固定为
/// `ToolCall`，`thought_signature`/`namespace` 不来自 JS。
fn kiana_tool_call_from_js<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> EngineResult<kiana_ai::types::ToolCall> {
    let object = value
        .as_object()
        .ok_or_else(|| Exception::throw_message(ctx, "tool call must be an object"))?;
    let id = object.get::<_, Option<String>>("id")?.unwrap_or_default();
    let name = object.get::<_, Option<String>>("name")?.unwrap_or_default();
    let arguments = match object.get::<_, Option<Value<'js>>>("arguments")? {
        Some(value) if value.is_object() => match js_to_json(ctx, &value)? {
            serde_json::Value::Object(map) => map.into_iter().collect(),
            _ => Default::default(),
        },
        _ => Default::default(),
    };
    let arguments_error = object.get::<_, Option<String>>("argumentsError")?;
    Ok(kiana_ai::types::ToolCall {
        kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
        id,
        name,
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error,
    })
}

/// 安装 `kiana-coding-agent` 分区的 Rust 实现：路径、frontmatter、消息转换、行号格式、截断与
/// 工具工厂（7 个工具各一对 `create*Tool` / `create*ToolDefinition`）。
///
/// `getAgentDir` 用 `to_string_lossy()`：路径中的非 UTF-8 字节会被替换为 U+FFFD，返回值总是
/// 可放进 JS 字符串。`parseFrontmatter` / `convertToLlm` 的输入不合法时抛 JS Error；
/// `truncateHead` 对非法选项静默忽略（`maxLines`/`maxBytes` 只接受 JSON number，负值夹到 0）。
/// 工具工厂只接受一个 `cwd` 字符串，其余选项固定传 `None`。
fn install_coding_agent_bridge<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> EngineResult<()> {
    install_function(
        ctx,
        target,
        "getAgentDir",
        |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            json_to_js(
                &ctx,
                &serde_json::Value::String(
                    kiana_core::config::get_agent_dir()
                        .to_string_lossy()
                        .into_owned(),
                ),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "parseFrontmatter",
        |ctx: Ctx<'js>, content: String| -> EngineResult<Value<'js>> {
            match kiana_core::frontmatter::parse_frontmatter(&content) {
                Ok(parsed) => {
                    let mut result = serde_json::Map::new();
                    result.insert(
                        "frontmatter".to_owned(),
                        serde_json::Value::Object(parsed.frontmatter),
                    );
                    result.insert("body".to_owned(), serde_json::Value::String(parsed.body));
                    json_to_js(&ctx, &serde_json::Value::Object(result))
                }
                Err(error) => Err(Exception::throw_message(&ctx, &error.to_string())),
            }
        },
    )?;
    install_function(
        ctx,
        target,
        "convertToLlm",
        |ctx: Ctx<'js>, messages: Value<'js>| -> EngineResult<Value<'js>> {
            let messages: Vec<kiana_agent_core::types::AgentMessage> =
                serde_json::from_value(js_to_json(&ctx, &messages)?)
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
            let converted = kiana_session_core::messages::convert_to_llm(&messages);
            json_to_js(
                &ctx,
                &serde_json::to_value(converted).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;
    install_function(
        ctx,
        target,
        "formatReadLineRange",
        |ctx: Ctx<'js>,
         offset: Opt<Value<'js>>,
         limit: Opt<Value<'js>>|
         -> EngineResult<Value<'js>> {
            let offset = opt_js_to_json(&ctx, &offset.0)?;
            let limit = opt_js_to_json(&ctx, &limit.0)?;
            let text =
                kiana_tools::render_utils::format_read_line_range(offset.as_ref(), limit.as_ref());
            json_to_js(&ctx, &serde_json::Value::String(text))
        },
    )?;
    install_function(
        ctx,
        target,
        "truncateHead",
        |ctx: Ctx<'js>, content: String, options: Opt<Value<'js>>| -> EngineResult<Value<'js>> {
            let options = opt_js_to_json(&ctx, &options.0)?.unwrap_or(serde_json::Value::Null);
            let mut truncate_options = kiana_tools::truncate::TruncationOptions::new();
            if let Some(object) = options.as_object() {
                if let Some(value) = object.get("maxLines").and_then(json_number) {
                    truncate_options = truncate_options.with_max_lines(value.max(0.0) as usize);
                }
                if let Some(value) = object.get("maxBytes").and_then(json_number) {
                    truncate_options = truncate_options.with_max_bytes(value.max(0.0) as usize);
                }
            }
            let result = kiana_tools::truncate::truncate_head(&content, &truncate_options);
            json_to_js(
                &ctx,
                &serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
            )
        },
    )?;

    // 工具工厂。`create*Tool` 返回 TS 的 AgentTool 形状；`create*ToolDefinition` 返回
    // ToolDefinition 形状（含 promptSnippet/promptGuidelines，渲染器在 Rust 侧无法暴露，写成
    // undefined）。`createBashTool` 与 TS 一样额外带 promptSnippet/promptGuidelines。
    macro_rules! tool_factory {
        ($name:literal, $create:path) => {
            install_function(
                ctx,
                target,
                $name,
                |ctx: Ctx<'js>, cwd: String| -> EngineResult<Value<'js>> {
                    let tool = $create(cwd, None);
                    agent_tool_to_js(&ctx, tool, None)
                },
            )?;
        };
    }
    macro_rules! tool_definition_factory {
        ($name:literal, $definition:path) => {
            install_function(
                ctx,
                target,
                $name,
                |ctx: Ctx<'js>, cwd: String| -> EngineResult<Value<'js>> {
                    let definition = $definition(cwd, None);
                    tool_definition_to_js(&ctx, definition)
                },
            )?;
        };
    }
    install_function(
        ctx,
        target,
        "createBashTool",
        |ctx: Ctx<'js>, cwd: String| -> EngineResult<Value<'js>> {
            let tool = kiana_tools::bash::create_bash_tool(cwd.clone(), None);
            let definition = kiana_tools::bash::create_bash_tool_definition(cwd, None);
            agent_tool_to_js(
                &ctx,
                tool,
                Some((definition.prompt_snippet, definition.prompt_guidelines)),
            )
        },
    )?;
    tool_definition_factory!(
        "createBashToolDefinition",
        kiana_tools::bash::create_bash_tool_definition
    );
    tool_factory!("createEditTool", kiana_tools::edit::create_edit_tool);
    tool_definition_factory!(
        "createEditToolDefinition",
        kiana_tools::edit::create_edit_tool_definition
    );
    tool_factory!("createFindTool", kiana_tools::find::create_find_tool);
    tool_definition_factory!(
        "createFindToolDefinition",
        kiana_tools::find::create_find_tool_definition
    );
    tool_factory!("createGrepTool", kiana_tools::grep::create_grep_tool);
    tool_definition_factory!(
        "createGrepToolDefinition",
        kiana_tools::grep::create_grep_tool_definition
    );
    tool_factory!("createLsTool", kiana_tools::ls::create_ls_tool);
    tool_definition_factory!(
        "createLsToolDefinition",
        kiana_tools::ls::create_ls_tool_definition
    );
    tool_factory!("createReadTool", kiana_tools::read::create_read_tool);
    tool_definition_factory!(
        "createReadToolDefinition",
        kiana_tools::read::create_read_tool_definition
    );
    tool_factory!("createWriteTool", kiana_tools::write::create_write_tool);
    tool_definition_factory!(
        "createWriteToolDefinition",
        kiana_tools::write::create_write_tool_definition
    );
    Ok(())
}

/// 把一个 `AgentTool` 暴露成 TS 的 AgentTool 对象（字段顺序同 `wrapToolDefinition`）。
///
/// `prompt` 是 TS `createBashTool` 的 `Object.assign(tool, {promptSnippet, promptGuidelines})`。
///
/// # 注册表与 execute
///
/// 每次调用分配一个进程内唯一 id（从 1 开始，`Relaxed` 递增），并把 `Arc<AgentTool>` 存进
/// `TOOL_REGISTRY`；JS 对象只持有这个数字 id，`execute` 被调用时把 id 与参数打包成 host
/// payload，交给 [`TOOL_EXECUTE_KIND`] 的处理器。注册表没有删除路径，因此创建过的工具会一直
/// 驻留到进程结束。
///
/// `prepareArguments` 是包装函数：参数先转成 JSON、调用 Rust 回调，再把回调改写后的值（或
/// 原值）转回 JS；对象身份与 TS 不同（见该闭包内的注释）。`signal` 会放进 payload，但当前
/// 执行器不把它传给工具实现。
fn agent_tool_to_js<'js>(
    ctx: &Ctx<'js>,
    tool: AgentTool,
    prompt: Option<(Option<String>, Option<Vec<String>>)>,
) -> EngineResult<Value<'js>> {
    let tool = Arc::new(tool);
    // id 从 1 开始；JS 侧只拿数字，执行时再查回 Arc（跨语言不能持有指针）。
    let id = TOOL_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    TOOL_REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, Arc::clone(&tool));

    let object = Object::new(ctx.clone())?;
    object.set("name", tool.name.clone())?;
    object.set("label", tool.label.clone())?;
    object.set("description", tool.description.clone())?;
    object.set("parameters", json_to_js(ctx, &tool.parameters)?)?;
    set_opt_json(ctx, &object, "outputSchema", tool.output_schema.as_ref())?;
    let constrained = tool
        .constrained_sampling
        .as_ref()
        .and_then(|value| serde_json::to_value(value).ok());
    set_opt_json(ctx, &object, "constrainedSampling", constrained.as_ref())?;
    match tool.prepare_arguments.clone() {
        Some(prepare) => {
            object.set(
                "prepareArguments",
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'js>, arguments: Value<'js>| -> EngineResult<Value<'js>> {
                        let mut json = js_to_json(&ctx, &arguments)?;
                        match prepare(&mut json) {
                            Some(prepared) => json_to_js(&ctx, &prepared),
                            // TS 的闭包通常原地改写并返回同一个对象；Rust 的值语义把改写后的
                            // 副本返回给 JS（内容一致，只有对象身份不同）。
                            None => json_to_js(&ctx, &json),
                        }
                    },
                )?,
            )?;
        }
        None => object.set("prepareArguments", Value::new_undefined(ctx.clone()))?,
    }
    let execution_mode = tool
        .execution_mode
        .as_ref()
        .and_then(|value| serde_json::to_value(value).ok());
    set_opt_json(ctx, &object, "executionMode", execution_mode.as_ref())?;

    let execute = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              tool_call_id: String,
              params: Value<'js>,
              signal: Opt<Value<'js>>,
              on_update: Opt<Function<'js>>|
              -> EngineResult<Value<'js>> {
            let payload = Object::new(ctx.clone())?;
            payload.set("toolId", id as f64)?;
            payload.set("toolCallId", tool_call_id)?;
            payload.set("params", params)?;
            if let Some(signal) = signal.0
                && !signal.is_undefined()
            {
                payload.set("signal", signal)?;
            }
            if let Some(on_update) = on_update.0
                && !on_update.is_undefined()
            {
                payload.set("onUpdate", on_update)?;
            }
            let kiana: Object = ctx.globals().get("__kiana")?;
            let promise: Function = kiana.get("promise")?;
            promise.call((
                rquickjs::function::This(kiana.clone()),
                TOOL_EXECUTE_KIND,
                payload,
            ))
        },
    )?;
    object.set("execute", execute)?;
    if let Some((snippet, guidelines)) = prompt {
        set_opt_json(
            ctx,
            &object,
            "promptSnippet",
            snippet
                .as_ref()
                .map(|value| serde_json::Value::String(value.clone()))
                .as_ref(),
        )?;
        let guidelines = guidelines
            .as_ref()
            .and_then(|lines| serde_json::to_value(lines).ok());
        set_opt_json(ctx, &object, "promptGuidelines", guidelines.as_ref())?;
    }

    Ok(object.into_value())
}

/// 把一个 `ToolDefinition` 暴露成 TS 的 ToolDefinition 对象（AgentTool 字段 + prompt 元数据）。
///
/// 渲染器（`renderCall`/`renderResult`）是 Rust 回调，TS 扩展无法调用，写成 `undefined`。
/// prompt 元数据在 `wrap_tool_definition` 消耗 `definition` 之前克隆出来，随后交给
/// [`agent_tool_to_js`] 附加。
fn tool_definition_to_js<'js>(
    ctx: &Ctx<'js>,
    definition: kiana_ext_api::tool_types::ToolDefinition,
) -> EngineResult<Value<'js>> {
    let prompt = (
        definition.prompt_snippet.clone(),
        definition.prompt_guidelines.clone(),
    );
    let tool = kiana_tools::definition::wrap_tool_definition(definition, None);
    let value = agent_tool_to_js(ctx, tool, Some(prompt))?;
    let object = value
        .clone()
        .into_object()
        .ok_or_else(|| Exception::throw_message(ctx, "tool definition is not an object"))?;
    object.set("renderCall", Value::new_undefined(ctx.clone()))?;
    object.set("renderResult", Value::new_undefined(ctx.clone()))?;
    Ok(value)
}

/// `kiana.codex.tool-execute` 的宿主处理器：JS 的 `tool.execute(...)` 经 `__kiana.promise`
/// 回调到这里。
///
/// payload：`{toolId, toolCallId, params, signal?, onUpdate?}`。两个前置错误直接 reject 并返回
/// `Ok(())`（payload 不是对象；`toolId` 在注册表里查不到）；`params` 转 JSON 失败时错误返回，
/// 宿主协议会把它变成对 `__kiana.promise` 调用方的同步异常。成功时在 `spawn_local` 里执行
/// `AgentTool::execute`，完成时 resolve 结果的 JSON，失败时 reject `{name, message}`
/// （`name` 缺省为 `"Error"`）。
///
/// `onUpdate` 的处理分两段：`HostCallback` 是 `!Send`，而 `AgentToolUpdateCallback` 要求
/// `Send + Sync`，所以先建一条 tokio 通道，由同线程的转发任务把每条更新 `invoke` 回 JS。每次
/// 更新单独序列化为 JSON；序列化失败退化为 `null`。
///
/// 注意：payload 里的 `signal` 没有被读取，传给 `execute` 的取消令牌固定是 `None`。
fn start_tool_execute(call: HostCall<'_>) -> EngineResult<()> {
    let ctx = call.ctx();
    let payload = call.payload();
    let Some(payload) = payload.as_object() else {
        call.completion_sender()
            .reject_message(call.id(), "kiana: invalid tool execute payload");
        return Ok(());
    };
    let tool_id = payload
        .get::<_, f64>("toolId")
        .map(|value| value as u64)
        .unwrap_or(0);
    let tool_call_id = payload.get::<_, String>("toolCallId").unwrap_or_default();
    let params = match payload.get::<_, Value>("params") {
        Ok(value) if !value.is_undefined() => js_to_json(ctx, &value)?,
        _ => serde_json::Value::Null,
    };
    let on_update = payload
        .get::<_, Option<Function>>("onUpdate")
        .ok()
        .flatten();
    let callback = on_update.map(|function| call.completion_sender().capture(function));
    let Some(tool) = TOOL_REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&tool_id)
        .cloned()
    else {
        call.completion_sender().reject_message(
            call.id(),
            format!("kiana: unknown native tool id {tool_id}"),
        );
        return Ok(());
    };
    // `HostCallback` 是 `!Send`，而 `AgentToolUpdateCallback` 要求 `Send + Sync`：更新通过
    // tokio 通道交给同一线程上的转发任务（两者都在 LocalSet 上协作调度）。
    let (update_sender, mut update_receiver) = if callback.is_some() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let update: Option<AgentToolUpdateCallback> = update_sender.map(|sender| {
        Arc::new(move |result: AgentToolResult<JsonValue>| {
            let json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
            let _ = sender.send(json);
        }) as AgentToolUpdateCallback
    });
    if let (Some(callback), Some(mut receiver)) = (callback, update_receiver.take()) {
        tokio::task::spawn_local(async move {
            while let Some(json) = receiver.recv().await {
                callback.invoke(vec![HostValue::Json(json)]);
            }
        });
    }
    let sender = call.completion_sender();
    let promise_id = call.id();
    // `create*Tool().execute(...)` 同样是直接调用 `AgentTool::execute`，补上权限检查。
    let permission = crate::runner::check_tool_permission(&tool.name, &params);
    tokio::task::spawn_local(async move {
        if let Some(check) = permission
            && let Some(reason) = check.await
        {
            sender.reject_message(promise_id, reason);
            return;
        }
        match tool
            .execute
            .execute(&tool_call_id, params, None, update)
            .await
        {
            Ok(result) => {
                let json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
                sender.resolve(promise_id, HostValue::Json(json));
            }
            Err(error) => {
                sender.reject(
                    promise_id,
                    HostValue::Error {
                        name: error.name.unwrap_or_else(|| "Error".to_owned()),
                        message: error.message,
                    },
                );
            }
        }
    });
    Ok(())
}

/// `kiana-ai.provider-stream` 的宿主处理器：JS 侧 compat 的 `stream` / `streamSimple` 打到这里。
///
/// payload：`{api, mode, model, context, options, onEvent, source?, provider?}`。
/// `mode == "streamSimple"` 选 `stream_simple`，其它值（含缺失）选 `stream`。`source ==
/// "provider"` 时从进程级 `BUILTIN_MODELS` 按 `provider` id 取 provider，否则按 `api` 走
/// `kiana_ai::compat::get_api_provider`；两种情况 provider 缺失都返回
/// `No API provider registered for api: …` 的异常（经 `__kiana.promise` 调用方收到）。
///
/// `context` 先经 `normalize_context` 变成 transcript 再交给 provider；`model` 缺失时用默认
/// 模型、`context` 反序列化失败时用默认上下文。`options` 按 [`stream_options_from_json`] /
/// [`simple_stream_options_from_json`] 解析。事件流由 `take_receiver` 取出后 `spawn_local`
/// 逐条 `invoke(onEvent)`，通道关闭后 resolve `undefined`；没有异步接收端时 reject
/// `kiana: provider stream requires the async runtime`。
fn start_provider_stream(call: HostCall<'_>) -> EngineResult<()> {
    let ctx = call.ctx();
    let payload = call.payload();
    let Some(payload) = payload.as_object() else {
        call.completion_sender()
            .reject_message(call.id(), "kiana: invalid provider stream payload");
        return Ok(());
    };
    let api = payload.get::<_, String>("api").unwrap_or_default();
    let mode = payload.get::<_, String>("mode").unwrap_or_default();
    let source = payload
        .get::<_, Option<String>>("source")
        .ok()
        .flatten()
        .unwrap_or_else(|| "api".to_owned());
    let provider_id = payload.get::<_, Option<String>>("provider").ok().flatten();
    let model = match payload.get::<_, Value>("model") {
        Ok(value) => model_from_json(&js_to_json(ctx, &value)?),
        Err(_) => Model::default(),
    };
    let context = match payload.get::<_, Value>("context") {
        Ok(value) => {
            serde_json::from_value::<Context>(js_to_json(ctx, &value)?).unwrap_or_default()
        }
        Err(_) => Context::default(),
    };
    let options_json = match payload.get::<_, Value>("options") {
        Ok(value) if !value.is_undefined() => js_to_json(ctx, &value)?,
        _ => serde_json::Value::Null,
    };
    let on_event = payload.get::<_, Option<Function>>("onEvent").ok().flatten();
    let callback = on_event.map(|function| call.completion_sender().capture(function));
    let sender = call.completion_sender();
    let promise_id = call.id();

    let transcript = kiana_ai::utils::transcript::normalize_context(&context);
    let stream = if source == "provider" {
        let provider = provider_id
            .as_deref()
            .and_then(|id| BUILTIN_MODELS.get_provider(id));
        let Some(provider) = provider else {
            return Err(Exception::throw_message(
                ctx,
                &format!("No API provider registered for api: {api}"),
            ));
        };
        if mode == "streamSimple" {
            provider.stream_simple(
                &model,
                &transcript,
                simple_stream_options_from_json(&options_json),
            )
        } else {
            provider.stream(&model, &transcript, stream_options_from_json(&options_json))
        }
    } else {
        let provider = kiana_ai::compat::get_api_provider(&api).ok_or_else(|| {
            Exception::throw_message(ctx, &format!("No API provider registered for api: {api}"))
        })?;
        if mode == "streamSimple" {
            provider.stream_simple(
                &model,
                &transcript,
                simple_stream_options_from_json(&options_json),
            )
        } else {
            provider.stream(&model, &transcript, stream_options_from_json(&options_json))
        }
    };

    let Some(mut receiver) = stream.take_receiver() else {
        sender.reject_message(
            promise_id,
            "kiana: provider stream requires the async runtime",
        );
        return Ok(());
    };
    tokio::task::spawn_local(async move {
        while let Some(event) = receiver.recv().await {
            let Some(callback) = &callback else { continue };
            let json = serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
            callback.invoke(vec![HostValue::Json(json)]);
        }
        sender.resolve(promise_id, HostValue::Undefined);
    });
    Ok(())
}

/// JSON → [`StreamOptions`]（TS 的 `StreamOptions`；未识别的键进 `extra`）。
///
/// 已知键按 TS 的 camelCase 名映射；`env`/`headers`/`identity`/`samplingParams` 等结构体字段
/// 用 `serde` 解析，失败时置 `None`（不报错）。数字字段用 `as_u64`/`as_f64`：字符串或 `null`
/// 会被静默忽略。其余键原样进 `extra`。
fn stream_options_from_json(value: &serde_json::Value) -> StreamOptions {
    let mut options = StreamOptions::default();
    let Some(object) = value.as_object() else {
        return options;
    };
    for (key, value) in object {
        match key.as_str() {
            "apiKey" => options.api_key = value.as_str().map(str::to_owned),
            "env" => options.env = serde_json::from_value(value.clone()).ok(),
            "headers" => options.headers = serde_json::from_value(value.clone()).ok(),
            "identity" => options.identity = serde_json::from_value(value.clone()).ok(),
            "timeoutMs" => options.timeout_ms = value.as_u64(),
            "maxRetries" => options.max_retries = value.as_u64().map(|value| value as u32),
            "maxRetryDelayMs" => options.max_retry_delay_ms = value.as_u64(),
            "temperature" => options.temperature = value.as_f64(),
            "samplingParams" => {
                options.sampling_params = serde_json::from_value(value.clone()).ok()
            }
            "maxTokens" => options.max_tokens = value.as_u64(),
            "transport" => options.transport = serde_json::from_value(value.clone()).ok(),
            "cacheRetention" => {
                options.cache_retention = serde_json::from_value(value.clone()).ok()
            }
            "sessionId" => options.session_id = value.as_str().map(str::to_owned),
            "websocketConnectTimeoutMs" => options.websocket_connect_timeout_ms = value.as_u64(),
            "metadata" => options.metadata = serde_json::from_value(value.clone()).ok(),
            _ => {
                options.extra.insert(key.clone(), value.clone());
            }
        }
    }
    options
}

/// JSON → [`SimpleStreamOptions`]。
///
/// 先用 [`stream_options_from_json`] 解析通用字段，再解析 4 个便捷字段（`toolChoice`、
/// `reasoning`、`deferred`、`thinkingBudgets`）；每个字段解析失败时保持 `None`。这 4 个键对
/// `StreamOptions` 是未知键，因此也留在 `base.extra` 里；下游 `build_base_options` 只用专用
/// 字段重建 base（`extra` 被丢弃），生效的是这里的专用字段。
fn simple_stream_options_from_json(value: &serde_json::Value) -> SimpleStreamOptions {
    let base = stream_options_from_json(value);
    let mut options = SimpleStreamOptions {
        base,
        ..SimpleStreamOptions::default()
    };
    let Some(object) = value.as_object() else {
        return options;
    };
    if let Some(tool_choice) = object.get("toolChoice")
        && let Ok(tool_choice) = serde_json::from_value::<ToolChoice>(tool_choice.clone())
    {
        options.tool_choice = Some(tool_choice);
    }
    if let Some(reasoning) = object.get("reasoning")
        && let Ok(reasoning) = serde_json::from_value(reasoning.clone())
    {
        options.reasoning = Some(reasoning);
    }
    if let Some(deferred) = object.get("deferred")
        && let Ok(deferred) = serde_json::from_value(deferred.clone())
    {
        options.deferred = Some(deferred);
    }
    if let Some(budgets) = object.get("thinkingBudgets")
        && let Ok(budgets) = serde_json::from_value(budgets.clone())
    {
        options.thinking_budgets = Some(budgets);
    }
    options
}

// ===========================================================================
// 单元测试：锁定静态表的不变量（22 键覆盖、别名目标存在、每个 namespace 的导出名唯一、
// 桥 source 指向已知 id）与安装期 helper 能被 VirtualFs 规范化解析。端到端安装、桥转发与
// 导出面比对在 tests/virtual_modules.rs 与 tests/virtual_module_identity.rs。
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::resolver::VIRTUAL_MODULE_KEYS;

    #[test]
    fn namespace_table_covers_exactly_the_22_keys() {
        let mut table_keys: Vec<&str> = NAMESPACES
            .iter()
            .flat_map(|spec| spec.keys.iter().copied())
            .collect();
        assert_eq!(table_keys.len(), 22);
        table_keys.sort_unstable();
        let mut expected = VIRTUAL_MODULE_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(table_keys, expected);
    }

    #[test]
    fn alias_table_matches_the_22_keys_in_order() {
        let keys: Vec<&str> = VIRTUAL_ALIASES.iter().map(|alias| alias.key).collect();
        assert_eq!(keys, VIRTUAL_MODULE_KEYS);
    }

    #[test]
    fn every_alias_target_is_an_embedded_file() {
        for alias in VIRTUAL_ALIASES {
            assert!(
                VIRTUAL_FILES.iter().any(|file| file.name == alias.target),
                "missing alias target {}",
                alias.target
            );
        }
    }

    #[test]
    fn every_public_key_is_a_registered_source_or_aliased() {
        for key in VIRTUAL_MODULE_KEYS {
            assert!(namespace_spec(key).is_some(), "no namespace for {key}");
        }
    }

    #[test]
    fn typebox_barrels_are_vendored() {
        for name in [
            "typebox/build/index.mjs",
            "typebox/build/compile/index.mjs",
            "typebox/build/value/index.mjs",
        ] {
            let file = VIRTUAL_FILES
                .iter()
                .find(|file| file.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert!(!file.source.is_empty());
        }
    }

    #[test]
    fn install_helpers_are_embedded_virtual_files() {
        // 回归（安装 blocker）：QuickJS 先经宿主 Resolver 规范化 import specifier，再查
        // `ctx.loaded_modules`；helper 必须能被真实 `ModuleResolver` 解析。
        for name in [
            BRIDGE_MODULE,
            KIANA_AI_HELPERS_MODULE,
            KIANA_TUI_HELPERS_MODULE,
            KIANA_CODING_AGENT_HELPERS_MODULE,
        ] {
            assert!(
                VIRTUAL_FILES.iter().any(|file| file.name == name),
                "install helper {name} is missing from VIRTUAL_FILES"
            );
            assert_eq!(virtual_fs().canonical_name(name), Some(name));
        }
    }

    #[test]
    fn namespace_exports_are_unique_and_bridge_sources_exist() {
        for spec in NAMESPACES {
            let mut seen = std::collections::HashSet::new();
            let names = spec
                .js_values
                .iter()
                .copied()
                .chain(spec.constants.iter().map(|(name, _)| *name))
                .chain(spec.bridge.iter().map(|member| member.name));
            for name in names {
                assert!(seen.insert(name), "{} exports `{name}` twice", spec.id);
            }
            for member in spec.bridge {
                if let Some(source) = member.source {
                    assert!(
                        spec_by_id(source).is_some(),
                        "{} member `{}` names unknown bridge source {source}",
                        spec.id,
                        member.name
                    );
                }
            }
        }
    }

    #[test]
    fn virtual_fs_resolves_typebox_and_kiana_keys() {
        let fs = virtual_fs();
        assert_eq!(
            fs.canonical_name("typebox"),
            Some("typebox/build/index.mjs")
        );
        assert_eq!(
            fs.canonical_name("@sinclair/typebox/compile"),
            Some("typebox/build/compile/index.mjs")
        );
        assert_eq!(
            fs.canonical_name("@mariozechner/kiana-ai/models"),
            Some("kiana-virtual/kiana-ai/models.js")
        );
        assert_eq!(
            fs.canonical_name("@earendil-works/kiana-ai/compat"),
            Some("kiana-virtual/kiana-ai.js")
        );
        assert_eq!(
            fs.resolve_relative("typebox/build/index.mjs", "./compile/index.mjs"),
            Some("typebox/build/compile/index.mjs")
        );
    }
}
