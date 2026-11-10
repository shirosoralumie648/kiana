//! `core/settings-manager.ts` 的 `SettingsManager` / `SettingsStorage` 门面。
//!
//! TS: `packages/coding-agent/src/core/settings-manager.ts`（1,443 行）。
//!
//! `SettingsManager` 是单实例、同步 getter、异步写队列（`ca-config-models.md` §8.2）：
//!
//! - getter 直接读内存快照（`RwLock` 读锁）；
//! - setter 同步改全局内存、标记本会话修改过的字段，然后 enqueue 一个写任务；
//! - 写队列按 FIFO 串行执行，写失败只记录到 [`SettingsManager::drain_errors`]（与 TS 的
//!   `writeQueue.catch(recordError)` 一致），不向调用方抛出；
//! - [`SettingsManager::flush`] 等待此前入队的写任务全部完成；
//! - [`SettingsManager::reload`] 先 flush，再重新读两个作用域的文件；某个作用域读取失败时
//!   **保留**该作用域上一次的值，只记录错误；
//! - 写队列的生命周期与 TS 的 Promise 链一致：已入队的任务不再依赖调用方的句柄，最后一个
//!   `SettingsManager` 被 drop 后 worker 会先排空队列再退出。
//!
//! **类型不符的已知设置**：TS 的 getter 直接读 `JSON.parse` 的原始对象，字段类型与 schema
//! 不符时仍返回原值（`??` 只在 `null`/`undefined` 时回落）。Rust 的类型化字段容纳不了的
//! 原始值保留在 [`Settings::extra`] 中，getter 在类型化字段缺失时回落到原始值：布尔按 JS
//! truthiness，数字按 `Number(value)`（`ToNumber`，含数组的 `join(",")` 语义），数组按
//! 元素解析并保留合法元素，字符串型的数组设置按码点展开（`[...\"abc\"]`）。
//!
//! 少数返回类型是标量/枚举的 getter 无法携带 TS 返回的原始 JSON（例如
//! `getTransport()` 在文件里是 `\"bogus\"` 时 TS 原样返回 `\"bogus\"`，Rust 只能返回枚举
//! 默认值；`getRetrySettings().maxRetries` 在 TS 里可能是字符串 `\"4\"`，Rust 是 `f64`）。
//! 这些是类型化 API 的固有偏差，无法在不破坏跨 crate 冻结签名的前提下复刻；消费方做
//! 真假判断/算术时按解析后的值计算，结果与原始 JSON 值等价。
//!
//! 写盘是**选择性持久化**：只把本会话修改过的字段（`modifiedFields` /
//! `modifiedNestedFields`，及其 project 变体）覆盖到磁盘当前内容上，因此外部对其它字段的
//! 编辑会被保留（`settings-manager-bug.test.ts`）。写出的字节是
//! `JSON.stringify(value, null, 2)`：键序 = 当前文件键序（JS 整数索引键规则）+ 修改字段的
//! Set 插入序，无结尾换行（[`kiana_json::js_object::stringify_pretty`]）。
//!
//! 字段身份用 JSON 属性名（`&str`）表示，与 TS 的 `keyof Settings` 一一对应。
//!
//! 嵌套 setter（`terminal.*`、`compaction.enabled`、`retry.enabled`、`images.*`、
//! `markdown.mermaid`、`modelThinkingLevels.*`）返回 `Result<(), SettingsValueError>`：
//! 父值是原始值（数字/字符串/布尔）时，TS 的严格模式属性赋值会同步抛 `TypeError`，
//! Rust 侧返回逐字文案（`Cannot create property '…' on …`），内存与磁盘均不变。
//!
//! # 这个文件负责什么（边界）
//!
//! 这里是 `SettingsManager` 门面：把「两个作用域的内存快照 + 选择性写盘 + 加载/迁移」
//! 编排成一个可克隆、可从任意线程同步读取的对象。字段类型与宽松解析在 `schema`，
//! 文件锁与 I/O 在 `storage`，默认值在 `defaults`，旧格式迁移在 `migrations`；本文件
//! 自己不做这些。所以「字段为什么是这个类型」查 `schema`，「文件为什么没写成」先看本
//! 文件的写队列与 `global_settings_load_error` 抑制逻辑，再看 `storage`。
//!
//! # 一次典型调用
//!
//! 1. `create*` / `from_storage*` / `in_memory*` 都汇入 `from_storage_with_paths`：按
//!    global → project 顺序各加载一次（`try_load_from_storage`），失败的作用域用
//!    `Settings::default()` 占位并把错误压入 `errors`，最后 [`deep_merge_settings`]
//!    生成有效设置快照。
//! 2. getter 走 `with_settings`（读锁内执行闭包，不克隆整份设置）或 `get_settings`
//!    （克隆）；类型化字段缺失时回落到 `extra` 里的原始 JSON（见上文「类型不符」）。
//! 3. setter 走 `mutate_global_and_save` / `mutate_global_nested_and_save` /
//!    `mutate_project_and_save`：在**同一把写锁**内完成改内存、标记 modified、重算合并
//!    快照、入队；出锁后才 `ensure_worker`，因为同步上下文的就地排空会在持锁时自锁。
//! 4. worker 串行执行 `Persist`：读磁盘当前内容并迁移 → 只覆盖 modified 字段 →
//!    `JSON.stringify(..., null, 2)` 写回；失败只记录到 `errors`。
//! 5. `flush` 入队 `Barrier`，等此前入队的写任务全部结束；`reload` 先 `flush` 再重新
//!    加载两个作用域。
//!
//! # 并发与锁
//!
//! - `ManagerState` 由 `std::sync::RwLock` 保护：getter 持读锁，setter/写路径持写锁。
//! - 锁中毒（持锁线程 panic）后统一 `into_inner()` 继续使用，不把 panic 传播给调用方。
//! - 写队列是无界 `tokio::sync::mpsc`，`send` 不阻塞，因此可以在写锁内发送；这也保证
//!   「修改顺序 == 快照顺序 == 入队顺序」。若把发送挪到锁外，后创建的旧快照可能后入队
//!   并覆盖新值。
//! - worker 惰性启动：tokio 运行时内 `spawn`；没有运行时的同步上下文就地 `try_recv`
//!   排空队列。`worker_started` 只防重复 spawn，不保护 `ManagerState`。
//! - 每个 `SettingsManager` 句柄各持一份 `write_tx`；最后一个句柄销毁后 `recv()` 返回
//!   `None`，worker 排空队列后退出。
//!
//! # 错误模型
//!
//! - **同步返回**：嵌套 setter 在父值不可赋值时返回 [`SettingsValueError`]（逐字 TS
//!   `TypeError` 文案）；项目未受信时项目写 setter 返回
//!   [`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`]。这两种情况内存与磁盘都不变。
//! - **记录型**：加载/写盘失败不抛给调用方，而是进入 `errors`，由
//!   [`SettingsManager::drain_errors`] 取走。失败的任务本身不清除 modified 标记；但任何
//!   同作用域的成功写都会清空该作用域的**全部**标记（见 `clear_modified_scope`），因此若
//!   失败之前或之后出现了其它成功写，失败字段不保证被后续写重试。某作用域加载失败期间，
//!   写盘路径跳过该文件，避免覆盖读不到或损坏的内容。
//! - `reload` 失败只更新错误字段，**保留**该作用域此前的内存值。
//!
//! # 阅读顺序
//!
//! [`SettingsScope`] / [`SettingsError`] → 深度合并宏（`impl_settings_merge!`）→
//! `ManagerState` 与 `Shared`（锁与队列）→ `SettingsManager` 的构造 → `mutate_*` 写路径
//! → `persist_scoped_settings` → 文件末尾的原始值回落 helper。
//!
//! # 修改这里会影响谁
//!
//! `kiana-session`、`kiana-interactive`、`kiana-coding-agent` 等 crate 持有
//! [`SettingsManager`]，通过公开 API（构造、getter/setter、`reload`/`flush`）使用它；
//! `settings.json` 的键序与「无结尾换行」被 `settings_manager_bug.rs` 的逐字节黄金用例
//! 锁定，改「选择性持久化」会破坏外部编辑保留测试。

pub mod compaction;
pub mod defaults;
pub mod diagnostics;
pub mod key_path;
pub mod layers;
pub mod migrations;
pub mod permissions;
pub mod schema;
pub mod storage;
pub mod strict;
pub mod tools;

pub use compaction::{CompactionModelRef, ResolvedCompactionSettings};
pub use diagnostics::{
    DiagnosticKind, DiagnosticLike, SettingsDiagnostic, collect_settings_diagnostics,
    deduplicate_diagnostics,
};
pub use storage::{
    FileSettingsStorage, InMemorySettingsStorage, SettingsLockCallback, SettingsStorage,
    SettingsStorageError,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::settings::defaults::SETTINGS_DEFAULTS;
use crate::settings::migrations::migrate_settings;
// `settings-manager.ts` 的 `export type { ... } from "./settings-schema.ts"` 面：
// 下游可以写 `kiana_config::settings::Settings`（与 TS 同名），也可以走 `settings::schema`。
pub use crate::settings::schema::{
    BranchSummarySettings, CacheWarmingMode, CodemodeMode, CodemodeSettings,
    CompactionModelOverride, CompactionSettings, DefaultProjectTrust, DoubleEscapeAction,
    FullscreenExitOutput, FullscreenScrollbar, FullscreenWheelScrollLines, ImageSettings,
    MarkdownSettings, MermaidRenderingMode, ModelThinkingLevel, PackageSource,
    ProviderRetrySettings, QuietStartup, RetrySettings, SessionShareDestination, Settings,
    SteeringMode, TerminalImagesSetting, TerminalSettings, ThinkingBudgetsSettings, TimeoutSetting,
    TransportSetting, TreeFilterMode, TuiMode, UiSettings, WarningSettings,
};
use crate::settings::tools::resolve_default_tools;
use kiana_core::config::CONFIG_DIR_NAME;
use kiana_core::http_dispatcher::{DEFAULT_HTTP_IDLE_TIMEOUT_MS, parse_http_idle_timeout_ms};
use kiana_core::paths::{PathInputOptions, normalize_path};
use kiana_core::text::strip_bom;

/// `CACHE_WARMING_MODES`（合法值的顺序即 `includes` 检查的集合）。
pub const CACHE_WARMING_MODES: [CacheWarmingMode; 3] = [
    CacheWarmingMode::Off,
    CacheWarmingMode::Streaming,
    CacheWarmingMode::Idle,
];

/// 项目未受信时写项目设置抛出的逐字文案。
pub const PROJECT_NOT_TRUSTED_WRITE_MESSAGE: &str =
    "Project is not trusted; refusing to write project settings";

// ---------------------------------------------------------------------------
// 基础类型
// ---------------------------------------------------------------------------

/// `SettingsScope`：`"global" | "project"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SettingsScope {
    /// `<agent-dir>/settings.json`。
    Global,
    /// `<cwd>/.kiana/settings.json`。
    Project,
}

impl SettingsScope {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsScope::Global => "global",
            SettingsScope::Project => "project",
        }
    }
}

impl std::fmt::Display for SettingsScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `SettingsManagerCreateOptions`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsManagerCreateOptions {
    /// `projectTrusted`（缺省 `true`）。
    pub project_trusted: Option<bool>,
}

/// `SettingsError`：一次加载/写盘失败。
///
/// TS 里 `error` 是 `Error` 对象；Rust 侧只保留 `error.message`（`Display` 文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    /// 出错的作用域。
    pub scope: SettingsScope,
    /// 文件路径（`create` 创建的 manager 才有；`fromStorage` 没有）。
    pub path: Option<String>,
    /// `error.message`。
    pub error: String,
}

impl SettingsError {
    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.error
    }
}

/// 设置值非法（TS getter 抛出的 `Error`）。
///
/// 也用于两类同步失败：嵌套 setter 的严格模式 `TypeError` 文案，以及项目未受信时的
/// 写拒绝（[`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`]）。`Display` 逐字等于 TS 的
/// `error.message`，测试按字符串精确比对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsValueError {
    message: String,
}

impl SettingsValueError {
    /// 用逐字文案构造。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for SettingsValueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SettingsValueError {}

/// `String(number)`（ECMAScript `Number::toString`；`NaN`/`Infinity` 有专门拼写）。
pub(crate) fn js_string_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::js_num::format_js_number(value)
    }
}

/// `String(value)`（JSON 值版）。
fn js_string_value(value: &Value) -> String {
    kiana_json::js_string::js_string(Some(value))
}

/// `Math.floor`。
fn js_floor(value: f64) -> f64 {
    value.floor()
}

/// `Math.max`：任一参数为 `NaN` 时结果为 `NaN`（Rust 的 `f64::max` 会忽略 `NaN`）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// `Math.min`：任一参数为 `NaN` 时结果为 `NaN`。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

// ---------------------------------------------------------------------------
// 深度合并（deepMergeObjects / deepMergeSettings）
// ---------------------------------------------------------------------------

/// `deepMergeObjects` 的「可合并对象」判定：`typeof === "object" && !== null && !Array`。
fn is_mergeable_object(value: &Value) -> bool {
    value.is_object()
}

/// 两个普通 JSON 对象的递归合并：base 键序在前，override 新增键按遍历序追加，
/// 两边都是对象时递归，否则整体替换（数组也整体替换）。
fn merge_value_objects(
    base: &serde_json::Map<String, Value>,
    overrides: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut result = base.clone();
    for (key, override_value) in overrides {
        let merged = match (result.get(key), override_value) {
            (Some(base_value), override_value)
                if is_mergeable_object(base_value) && is_mergeable_object(override_value) =>
            {
                match (base_value, override_value) {
                    (Value::Object(base_object), Value::Object(override_object)) => {
                        Value::Object(merge_value_objects(base_object, override_object))
                    }
                    _ => override_value.clone(),
                }
            }
            _ => override_value.clone(),
        };
        result.insert(key.clone(), merged);
    }
    result
}

/// 设置对象的深度合并（每个参与嵌套合并的 `settings_object!` 结构体实现一次；
/// 输入/迁移用的 `SettingsInput`、`RetrySettingsInput`、`SkillsSettings` 与枚举载荷
/// `PackageSourceObject` 不实现）。
trait SettingsMerge: Clone {
    fn merge(base: &Self, over: &Self) -> Self;
}

/// 合并两侧的 `extra`：只处理**未知键**；已知键（含类型不符的原值）由字段分支完整处理。
/// 未知键按普通对象递归合并，其余情况 override 直接覆盖。
fn merge_extra_values(
    target: &mut IndexMap<String, Value>,
    overrides: &IndexMap<String, Value>,
    known_keys: &[&str],
) {
    for (key, value) in overrides {
        if known_keys.contains(&key.as_str()) {
            continue;
        }
        let merged = match (target.get(key), value) {
            (Some(Value::Object(base_object)), Value::Object(override_object)) => {
                Value::Object(merge_value_objects(base_object, override_object))
            }
            _ => value.clone(),
        };
        target.insert(key.clone(), merged);
    }
}

/// `deepMergeObjects` 对单个字段值的语义：双方都是可合并对象时逐键递归，否则 override 覆盖。
fn merge_json_value(base: Option<&Value>, overrides: &Value) -> Value {
    match (base, overrides) {
        (Some(Value::Object(base_object)), Value::Object(override_object)) => {
            Value::Object(merge_value_objects(base_object, override_object))
        }
        _ => overrides.clone(),
    }
}

/// 把合并后的原始 JSON 解析成类型化容器；失败时把原值放进 `extra`（getter 会回落读它）。
fn deserialize_merged<T: serde::de::DeserializeOwned>(
    merged: Value,
    key: &str,
    extra: &mut IndexMap<String, Value>,
) -> Option<T> {
    match serde_json::from_value::<T>(merged.clone()) {
        Ok(value) => Some(value),
        Err(_) => {
            extra.insert(key.to_string(), merged);
            None
        }
    }
}

// 合并宏按字段种类分派（`$kind` 由下面的 `impl_settings_merge!` 调用点给出）：
//
// - `(scalar)`：override 有类型化值则整体替换；没有时若 override 的 `extra` 保留了该键，
//   说明它在 override 层是 `null` 或类型不符，必须用原始值覆盖 base，并把 base 的类型化
//   值清成 `None`。
// - `(object T)` / `(record T)` / `(map T)`：尽量递归合并类型化值；只要一侧无法整体解析，
//   就退回原始 JSON 逐键合并再尝试反序列化，让合法条目不被另一侧的非法值吞掉。
//
// 每个分支都同步维护 `extra`：类型化成功则删除同名原始值，失败则把合并后的原始 JSON
// 放回 `extra`。因此 `Settings::raw(key)`（类型化优先、其次 extra）始终返回合并后的当前
// 值，getter 的原始值回落才不会读到 base 的旧值。
macro_rules! impl_settings_merge {
    (
        $name:ident {
            $(
                $field:ident : $key:literal : $kind:tt
            ),* $(,)?
        }
    ) => {
        impl SettingsMerge for $name {
            fn merge(base: &Self, over: &Self) -> Self {
                let mut result = base.clone();
                $(
                    impl_settings_merge!(@field result, base, over, $field, $key, $kind);
                )*
                merge_extra_values(&mut result.extra, &over.extra, $name::known_keys());
                result
            }
        }
    };

    (@field $result:ident, $base:ident, $over:ident, $field:ident, $key:literal, (scalar)) => {
        if let Some(value) = &$over.$field {
            $result.$field = Some(value.clone());
            $result.extra.shift_remove($key);
        } else if let Some(raw) = $over.extra.get($key) {
            // `null` / 类型不符的原始值整体覆盖（getter 读 `extra` 时按 JS 语义回落）。
            $result.$field = None;
            $result.extra.insert($key.to_string(), raw.clone());
        }
    };

    // 嵌套对象：两侧都能整体解析时逐字段合并；任一侧无法整体解析时退回原始 JSON 逐键
    // 合并（TS `deepMergeObjects` 只让真正非法的条目失去类型化表示，合法条目必须保留）。
    (@field $result:ident, $base:ident, $over:ident, $field:ident, $key:literal, (object $ty:ty)) => {
        match (&$over.$field, &$base.$field) {
            (Some(over_value), Some(base_value)) => {
                $result.$field = Some(<$ty as SettingsMerge>::merge(base_value, over_value));
                $result.extra.shift_remove($key);
            }
            (Some(over_value), None) => {
                if let Some(base_raw) = $base.extra.get($key) {
                    let over_raw = serde_json::to_value(over_value).unwrap_or(Value::Null);
                    let merged = merge_json_value(Some(base_raw), &over_raw);
                    $result.$field =
                        deserialize_merged::<$ty>(merged, $key, &mut $result.extra);
                    if $result.$field.is_some() {
                        $result.extra.shift_remove($key);
                    }
                } else {
                    $result.$field = Some(over_value.clone());
                    $result.extra.shift_remove($key);
                }
            }
            (None, _) => {
                if let Some(over_raw) = $over.extra.get($key) {
                    let base_raw = $base.raw($key);
                    let merged = merge_json_value(base_raw.as_ref(), over_raw);
                    $result.$field =
                        deserialize_merged::<$ty>(merged, $key, &mut $result.extra);
                    if $result.$field.is_some() {
                        $result.extra.shift_remove($key);
                    }
                }
            }
        }
    };

    // record（`IndexMap<String, 嵌套对象>`）：两侧都能整体解析时逐条目合并类型化值；
    // 任一侧整张 record 无法解析时退回原始 JSON 逐键合并，保留另一层的合法条目。
    (@field $result:ident, $base:ident, $over:ident, $field:ident, $key:literal, (record $ty:ty)) => {
        if let Some(over_map) = &$over.$field {
            if $base.$field.is_none() && $base.extra.contains_key($key) {
                let base_raw = $base.raw($key).unwrap_or(Value::Null);
                let over_raw = serde_json::to_value(over_map).unwrap_or(Value::Null);
                let merged = merge_json_value(Some(&base_raw), &over_raw);
                $result.$field =
                    deserialize_merged::<IndexMap<String, $ty>>(merged, $key, &mut $result.extra);
                if $result.$field.is_some() {
                    $result.extra.shift_remove($key);
                }
            } else {
                let mut merged = $base.$field.clone().unwrap_or_default();
                for (entry_key, value) in over_map {
                    let merged_value = match merged.get(entry_key) {
                        Some(base_value) => <$ty as SettingsMerge>::merge(base_value, value),
                        None => value.clone(),
                    };
                    merged.insert(entry_key.clone(), merged_value);
                }
                $result.$field = Some(merged);
                $result.extra.shift_remove($key);
            }
        } else if let Some(over_raw) = $over.extra.get($key) {
            let base_raw = $base.raw($key);
            let merged = merge_json_value(base_raw.as_ref(), over_raw);
            $result.$field =
                deserialize_merged::<IndexMap<String, $ty>>(merged, $key, &mut $result.extra);
            if $result.$field.is_some() {
                $result.extra.shift_remove($key);
            }
        }
    };

    // map（`IndexMap<String, 标量>`，如 modelThinkingLevels）：语义与 record 相同。
    (@field $result:ident, $base:ident, $over:ident, $field:ident, $key:literal, (map $ty:ty)) => {
        if let Some(over_map) = &$over.$field {
            if $base.$field.is_none() && $base.extra.contains_key($key) {
                let base_raw = $base.raw($key).unwrap_or(Value::Null);
                let over_raw = serde_json::to_value(over_map).unwrap_or(Value::Null);
                let merged = merge_json_value(Some(&base_raw), &over_raw);
                $result.$field =
                    deserialize_merged::<IndexMap<String, $ty>>(merged, $key, &mut $result.extra);
                if $result.$field.is_some() {
                    $result.extra.shift_remove($key);
                }
            } else {
                let mut merged = $base.$field.clone().unwrap_or_default();
                for (entry_key, value) in over_map {
                    merged.insert(entry_key.clone(), value.clone());
                }
                $result.$field = Some(merged);
                $result.extra.shift_remove($key);
            }
        } else if let Some(over_raw) = $over.extra.get($key) {
            let base_raw = $base.raw($key);
            let merged = merge_json_value(base_raw.as_ref(), over_raw);
            $result.$field =
                deserialize_merged::<IndexMap<String, $ty>>(merged, $key, &mut $result.extra);
            if $result.$field.is_some() {
                $result.extra.shift_remove($key);
            }
        }
    };
}

impl_settings_merge!(CompactionModelOverride {
    reserve_tokens: "reserveTokens": (scalar),
    keep_recent_tokens: "keepRecentTokens": (scalar),
});

impl_settings_merge!(CompactionSettings {
    enabled: "enabled": (scalar),
    reserve_tokens: "reserveTokens": (scalar),
    keep_recent_tokens: "keepRecentTokens": (scalar),
    model_overrides: "modelOverrides": (record CompactionModelOverride),
});

impl_settings_merge!(BranchSummarySettings {
    reserve_tokens: "reserveTokens": (scalar),
    skip_prompt: "skipPrompt": (scalar),
});

impl_settings_merge!(ProviderRetrySettings {
    timeout_ms: "timeoutMs": (scalar),
    max_retries: "maxRetries": (scalar),
    max_retry_delay_ms: "maxRetryDelayMs": (scalar),
});

impl_settings_merge!(RetrySettings {
    enabled: "enabled": (scalar),
    max_retries: "maxRetries": (scalar),
    base_delay_ms: "baseDelayMs": (scalar),
    max_agent_delay_ms: "maxAgentDelayMs": (scalar),
    provider: "provider": (object ProviderRetrySettings),
});

impl_settings_merge!(TerminalSettings {
    show_images: "showImages": (scalar),
    image_width_cells: "imageWidthCells": (scalar),
    clear_on_shrink: "clearOnShrink": (scalar),
    show_terminal_progress: "showTerminalProgress": (scalar),
    hyperlinks: "hyperlinks": (scalar),
    images: "images": (scalar),
    true_color: "trueColor": (scalar),
});

impl_settings_merge!(ImageSettings {
    auto_resize: "autoResize": (scalar),
    block_images: "blockImages": (scalar),
});

impl_settings_merge!(UiSettings {
    locale: "locale": (scalar),
});

impl_settings_merge!(ThinkingBudgetsSettings {
    minimal: "minimal": (scalar),
    low: "low": (scalar),
    medium: "medium": (scalar),
    high: "high": (scalar),
});

impl_settings_merge!(MarkdownSettings {
    code_block_indent: "codeBlockIndent": (scalar),
    mermaid: "mermaid": (scalar),
});

impl_settings_merge!(WarningSettings {
    anthropic_extra_usage: "anthropicExtraUsage": (scalar),
});

impl_settings_merge!(CodemodeSettings {
    mode: "mode": (scalar),
    inline_budget: "inlineBudget": (scalar),
});

impl_settings_merge!(Settings {
    schema: "$schema": (scalar),
    last_changelog_version: "lastChangelogVersion": (scalar),
    default_provider: "defaultProvider": (scalar),
    default_model: "defaultModel": (scalar),
    default_thinking_level: "defaultThinkingLevel": (scalar),
    model_thinking_levels: "modelThinkingLevels": (map ModelThinkingLevel),
    transport: "transport": (scalar),
    session_share_destination: "sessionShareDestination": (scalar),
    steering_mode: "steeringMode": (scalar),
    follow_up_mode: "followUpMode": (scalar),
    theme: "theme": (scalar),
    compaction: "compaction": (object CompactionSettings),
    branch_summary: "branchSummary": (object BranchSummarySettings),
    retry: "retry": (object RetrySettings),
    hide_thinking_block: "hideThinkingBlock": (scalar),
    show_cache_miss_notices: "showCacheMissNotices": (scalar),
    external_editor: "externalEditor": (scalar),
    shell_path: "shellPath": (scalar),
    quiet_startup: "quietStartup": (scalar),
    default_project_trust: "defaultProjectTrust": (scalar),
    shell_command_prefix: "shellCommandPrefix": (scalar),
    npm_command: "npmCommand": (scalar),
    collapse_changelog: "collapseChangelog": (scalar),
    enable_install_telemetry: "enableInstallTelemetry": (scalar),
    enable_analytics: "enableAnalytics": (scalar),
    tracking_id: "trackingId": (scalar),
    packages: "packages": (scalar),
    extensions: "extensions": (scalar),
    device_id: "deviceId": (scalar),
    skills: "skills": (scalar),
    prompts: "prompts": (scalar),
    themes: "themes": (scalar),
    enable_skill_commands: "enableSkillCommands": (scalar),
    terminal: "terminal": (object TerminalSettings),
    images: "images": (object ImageSettings),
    ui: "ui": (object UiSettings),
    enabled_models: "enabledModels": (scalar),
    default_tools: "defaultTools": (scalar),
    double_escape_action: "doubleEscapeAction": (scalar),
    tree_filter_mode: "treeFilterMode": (scalar),
    thinking_budgets: "thinkingBudgets": (object ThinkingBudgetsSettings),
    editor_padding_x: "editorPaddingX": (scalar),
    output_pad: "outputPad": (scalar),
    autocomplete_max_visible: "autocompleteMaxVisible": (scalar),
    show_hardware_cursor: "showHardwareCursor": (scalar),
    markdown: "markdown": (object MarkdownSettings),
    warnings: "warnings": (object WarningSettings),
    codemode: "codemode": (object CodemodeSettings),
    session_dir: "sessionDir": (scalar),
    http_proxy: "httpProxy": (scalar),
    http_idle_timeout_ms: "httpIdleTimeoutMs": (scalar),
    cache_warming: "cacheWarming": (scalar),
    websocket_connect_timeout_ms: "websocketConnectTimeoutMs": (scalar),
    tui_mode: "tuiMode": (scalar),
    fullscreen_exit_output: "fullscreenExitOutput": (scalar),
    fullscreen_scrollbar: "fullscreenScrollbar": (scalar),
    fullscreen_copy_on_select: "fullscreenCopyOnSelect": (scalar),
    fullscreen_wheel_scroll_lines: "fullscreenWheelScrollLines": (scalar),
});

/// `deepMergeSettings(base, overrides)`：project/overrides 优先，嵌套对象递归合并。
///
/// 合并语义：只有两侧都是 JSON 对象时才递归，数组、标量与 `null` 都整体替换。类型化字段
/// 的序列化顺序由 schema 结构体的声明顺序决定；`extra` 里的未知键保持 base 的插入顺序，
/// override 的新增键按遍历顺序追加。
///
/// `defaultTools` 不走上面的普通合并：它先取自 `raw("defaultTools")` 的原始值，交给
/// `tools::merge_default_tools_raw` 处理——纯 `+name` / `-name` 修饰符列表会追加到继承
/// 列表，其它值（含非法值）整体替换；合并结果能解析成 `Vec<String>` 才写回类型化字段，
/// 否则放进 `extra`。原因是 settings 文件内容未经校验，`defaultTools` 可能是无法解析成
/// `Vec<String>` 的任意 JSON（混合数组、字符串等，见 `tools::merge_default_tools_raw` 的
/// 文档说明）；这类值只存在于 `extra`，因此统一从 `raw("defaultTools")` 按 TS 的
/// `Array.isArray` / 全修饰符规则处理。
pub fn deep_merge_settings(base: &Settings, overrides: &Settings) -> Settings {
    let mut merged = SettingsMerge::merge(base, overrides);
    let base_tools = base.raw("defaultTools");
    let override_tools = overrides.raw("defaultTools");
    if let Some(default_tools) =
        tools::merge_default_tools_raw(base_tools.as_ref(), override_tools.as_ref())
    {
        match serde_json::from_value::<Vec<String>>(default_tools.clone()) {
            Ok(list) => {
                merged.default_tools = Some(list);
                merged.extra.shift_remove("defaultTools");
            }
            Err(_) => {
                merged.default_tools = None;
                merged
                    .extra
                    .insert("defaultTools".to_string(), default_tools);
            }
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// 内部状态与写队列
// ---------------------------------------------------------------------------

/// 诊断用的两个 `settings.json` 路径。
///
/// `create*` 会填入真实路径；`from_storage*` 不知道后端对应哪个文件，两者都是 `None`，
/// 因此这些 manager 记录的错误条目 `SettingsError::path` 也为 `None`。
#[derive(Debug, Clone, Default)]
struct SettingsPaths {
    global: Option<String>,
    project: Option<String>,
}

/// `RwLock` 保护的完整可变状态。
///
/// 不变量：
/// - `settings` 是供 getter 读取的有效快照，由 `global_settings` 与 `project_settings`
///   合并而成（`apply_overrides` 会在其上再叠一层临时覆盖）；
/// - `*_load_error` 非 `None` 表示该作用域最近一次加载失败（读文件、锁、解析或迁移），
///   写盘路径必须跳过它，避免把读不到/损坏的文件覆盖成内存里的空设置；
/// - 四个 modified 集合记录**本会话**改过的 JSON 属性名：顶层集合给出写盘时的字段顺序
///   （TS `Set` 的插入序），嵌套集合给出该字段下要逐键合并的子键。
struct ManagerState {
    global_settings: Settings,
    project_settings: Settings,
    settings: Settings,
    project_trusted: bool,
    /// 本会话修改过的全局顶层字段（JS `Set` 顺序）。
    modified_fields: IndexSet<String>,
    /// 本会话修改过的全局嵌套字段。
    modified_nested_fields: IndexMap<String, IndexSet<String>>,
    /// 本会话修改过的项目顶层字段。
    modified_project_fields: IndexSet<String>,
    /// 本会话修改过的项目嵌套字段。
    modified_project_nested_fields: IndexMap<String, IndexSet<String>>,
    global_settings_load_error: Option<String>,
    project_settings_load_error: Option<String>,
    errors: Vec<SettingsError>,
    settings_paths: SettingsPaths,
}

/// 一次待写的持久化任务（TS 的 `persistScopedSettings` 参数快照）。
struct PendingPersist {
    scope: SettingsScope,
    snapshot: Settings,
    modified_fields: IndexSet<String>,
    modified_nested_fields: IndexMap<String, IndexSet<String>>,
}

/// `save()` 的锁内部分：重算有效设置，返回 global 的写快照。
///
/// 无论是否返回快照都会先重算 `state.settings`，所以 `apply_overrides` 叠加的临时覆盖
/// 在任何一次全局写路径（即使因加载失败而没有真正写盘）中都会被丢弃。
///
/// 文件加载失败时返回 `None`（TS 直接 `return`，不覆盖损坏的文件）。
fn snapshot_global_locked(state: &mut ManagerState) -> Option<PendingPersist> {
    let merged = deep_merge_settings(&state.global_settings, &state.project_settings);
    state.settings = merged;
    if state.global_settings_load_error.is_some() {
        return None;
    }
    Some(PendingPersist {
        scope: SettingsScope::Global,
        snapshot: state.global_settings.clone(),
        modified_fields: state.modified_fields.clone(),
        modified_nested_fields: state.modified_nested_fields.clone(),
    })
}

/// `saveProjectSettings(settings)` 的锁内部分：返回 project 的写快照。
fn snapshot_project_locked(state: &ManagerState) -> Option<PendingPersist> {
    if state.project_settings_load_error.is_some() {
        return None;
    }
    Some(PendingPersist {
        scope: SettingsScope::Project,
        snapshot: state.project_settings.clone(),
        modified_fields: state.modified_project_fields.clone(),
        modified_nested_fields: state.modified_project_nested_fields.clone(),
    })
}

/// 写队列上的两类消息：持久化任务，或 `flush()` 的完成屏障。
enum WriteMessage {
    Persist(Box<PendingPersist>),
    /// `flush()` 的屏障：在它之前入队的任务都执行完后回信。
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// 所有 `SettingsManager` 克隆共享的内部状态。
///
/// `write_rx` 只被 worker 取走一次；`worker_started` 是启动标记而不是状态锁。
struct Shared {
    storage: Arc<dyn SettingsStorage>,
    state: RwLock<ManagerState>,
    // 发送端**不**放在 `Shared` 里：每个 [`SettingsManager`] 句柄各持一份。最后一个句柄
    // 销毁时所有 sender 一起 drop，worker 的 `recv()` 返回 `None` 后退出（此时它仍会在退出
    // 前排空队列里的写任务，对应 TS 的写队列在调用方丢失引用后继续执行）。
    write_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<WriteMessage>>>,
    worker_started: AtomicBool,
}

impl Shared {
    /// 惰性启动写 worker；没有 tokio 运行时的同步上下文改为就地排空队列。
    ///
    /// 调用点通常在 `send` 之后调用；条件写路径（闭包返回 `false` 或加载失败抑制入队，
    /// 见 `mutate_global_and_save_if` 与 `mutate_project_and_save`）也会在队列为空时调用，
    /// 本函数对此是安全的：同步分支发现队列里没有消息就直接返回，tokio 分支只确保 worker
    /// 已启动。同步分支持有 `write_rx` 互斥锁，
    /// 而 `handle_message` → `run_persist` 会再取 `ManagerState` 的写锁，所以本函数
    /// **不能**在持有 `state` 写锁时调用（std `RwLock` 不可重入，会自锁）。
    fn ensure_worker(self: &Arc<Self>) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if self.worker_started.swap(true, Ordering::SeqCst) {
                return;
            }
            let receiver = self
                .write_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(mut receiver) = receiver {
                // worker 持有强引用：即使所有 manager 句柄都已销毁，已入队的写任务仍会执行。
                // 最后一个 sender 销毁后 `recv()` 返回 `None`，任务退出并释放 `Shared`。
                let shared = Arc::clone(self);
                handle.spawn(async move {
                    while let Some(message) = receiver.recv().await {
                        shared.handle_message(message);
                    }
                });
            }
            return;
        }

        // 同步上下文（没有 tokio 运行时）：就地按 FIFO 执行。
        let mut guard = self
            .write_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(receiver) = guard.as_mut() {
            while let Ok(message) = receiver.try_recv() {
                self.handle_message(message);
            }
        }
    }

    fn handle_message(&self, message: WriteMessage) {
        match message {
            WriteMessage::Persist(pending) => self.run_persist(*pending),
            WriteMessage::Barrier(sender) => {
                let _ = sender.send(());
            }
        }
    }

    /// 执行一个持久化任务。
    ///
    /// 信任检查放在 worker 里（而不是入队时）：任务按 FIFO 执行，检查看到的是执行时刻的
    /// `project_trusted`。失败的任务本身不清除 modified 标记；但任何同作用域的成功写都会
    /// 清空该作用域的全部标记，因此若期间出现其它成功写，失败字段不保证被后续写重试
    /// （对应 TS `writeQueue.catch(recordError)`）。
    fn run_persist(&self, pending: PendingPersist) {
        let scope = pending.scope;
        if scope == SettingsScope::Project && !self.is_project_trusted() {
            // TS: `enqueueWrite` 的 `.then` 里同步抛 → `.catch` 记录错误；modified 标记不清除。
            self.record_error(SettingsScope::Project, PROJECT_NOT_TRUSTED_WRITE_MESSAGE);
            return;
        }
        match self.persist_scoped_settings(&pending) {
            Ok(()) => self.clear_modified_scope(scope),
            Err(message) => self.record_error(scope, &message),
        }
    }

    /// 选择性持久化的核心：在存储锁内读当前文件 → 迁移 → 只覆盖 modified 字段 → 写回。
    ///
    /// 输入 `pending` 是**入队时**的快照，不是当前内存状态：worker 执行时内存可能已被
    /// 后续 setter 改过，但本次任务落盘的仍是提交顺序对应的那份快照。磁盘内容会先经过
    /// `migrate_settings`，所以保存会顺带把旧格式迁移成新格式写回。
    ///
    /// 失败返回可直接展示的错误文案：序列化失败、读文件失败、`JSON.parse`/迁移失败、
    /// 写文件失败都会变成队列里的 `SettingsError`，不会 panic。
    fn persist_scoped_settings(&self, pending: &PendingPersist) -> Result<(), String> {
        let snapshot = serde_json::to_value(&pending.snapshot)
            .map_err(|error| format!("Failed to serialize settings: {error}"))?;
        self.storage
            .with_lock(pending.scope, &mut |current| {
                let current_value = match current {
                    Some(content) if !content.is_empty() => {
                        let mut value = kiana_json::js_parse::parse(&strip_bom(content))
                            .map_err(|error| -> SettingsStorageError { Box::new(error) })?;
                        migrate_settings(&mut value)
                            .map_err(|error| -> SettingsStorageError { Box::new(error) })?;
                        value
                    }
                    _ => Value::Object(serde_json::Map::new()),
                };
                let current_map = spread_object(current_value);
                let mut merged = current_map.clone();

                let snapshot_object = match &snapshot {
                    Value::Object(object) => object.clone(),
                    _ => serde_json::Map::new(),
                };

                for field in &pending.modified_fields {
                    let value = snapshot_object.get(field);
                    let nested_modified = pending.modified_nested_fields.get(field);
                    // TS: `typeof value === "object" && value !== null`（数组也算对象）。
                    let nested_object = match value {
                        Some(Value::Object(_)) | Some(Value::Array(_)) => {
                            value.map(|value| spread_object(value.clone()))
                        }
                        _ => None,
                    };
                    if let (Some(nested_modified), Some(in_memory_nested)) =
                        (nested_modified, nested_object)
                    {
                        // `currentFileSettings[field] ?? {}`（对象或数组都用 JS 展开语义）。
                        let base_nested = match current_map.get(field) {
                            Some(value) => spread_object(value.clone()),
                            None => serde_json::Map::new(),
                        };
                        let mut merged_nested = base_nested;
                        for nested_key in nested_modified {
                            match in_memory_nested.get(nested_key) {
                                Some(nested_value) => {
                                    merged_nested.insert(nested_key.clone(), nested_value.clone());
                                }
                                None => {
                                    merged_nested.shift_remove(nested_key);
                                }
                            }
                        }
                        merged.insert(field.clone(), Value::Object(merged_nested));
                    } else {
                        match value {
                            Some(value) => {
                                merged.insert(field.clone(), value.clone());
                            }
                            None => {
                                merged.shift_remove(field);
                            }
                        }
                    }
                }

                Ok(Some(kiana_json::js_object::stringify_pretty(
                    &Value::Object(merged),
                    2,
                )))
            })
            .map_err(|error| error.to_string())
    }

    fn is_project_trusted(&self) -> bool {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .project_trusted
    }

    fn record_error(&self, scope: SettingsScope, message: &str) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        record_error_locked(&mut state, scope, message);
    }

    /// 一次写成功后清空该作用域的**全部** modified 标记——包括其它在途任务覆盖的字段；
    /// 失败的任务本身不清除标记。因此若一个失败任务期间出现了同作用域的其它成功写，
    /// 失败字段不保证被后续写重试。
    fn clear_modified_scope(&self, scope: SettingsScope) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match scope {
            SettingsScope::Global => {
                state.modified_fields.clear();
                state.modified_nested_fields.clear();
            }
            SettingsScope::Project => {
                state.modified_project_fields.clear();
                state.modified_project_nested_fields.clear();
            }
        }
    }
}

/// `{...value}`（JS 对象展开）：对象原样；数组展开成下标键；字符串按索引展开成字符
/// （JS 的 String 对象每个 UTF-16 code unit 是一个索引属性；这里按 Unicode 码点近似，
/// 非 BMP 字符在 JS 里是两个代理半区，无法用 UTF-8 字符串表示）；其它类型得到空对象。
///
/// 两个调用点都依赖这一语义：`persist_scoped_settings` 用它合并嵌套字段
/// （TS `{...currentFileSettings[field]}`），`try_load_from_storage` 用它把顶层数组变成
/// 下标键对象。
fn spread_object(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(object) => object,
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item))
            .collect(),
        Value::String(text) => text
            .chars()
            .enumerate()
            .map(|(index, character)| (index.to_string(), Value::String(character.to_string())))
            .collect(),
        _ => serde_json::Map::new(),
    }
}

/// 追加一条错误（保持产生顺序）；路径取构造时记录的诊断路径（`fromStorage` 为 `None`）。
fn record_error_locked(state: &mut ManagerState, scope: SettingsScope, message: &str) {
    let path = match scope {
        SettingsScope::Global => state.settings_paths.global.clone(),
        SettingsScope::Project => state.settings_paths.project.clone(),
    };
    state.errors.push(SettingsError {
        scope,
        path,
        error: message.to_string(),
    });
}

/// 记录「本会话改过这个全局字段」；嵌套 setter 额外记录具体子键。
///
/// 顶层集合用 `IndexSet`，因为写盘时新增键按它的插入顺序追加（对应 TS `Set` 的迭代序）。
fn mark_modified(state: &mut ManagerState, field: &str, nested_key: Option<&str>) {
    state.modified_fields.insert(field.to_string());
    if let Some(nested_key) = nested_key {
        state
            .modified_nested_fields
            .entry(field.to_string())
            .or_default()
            .insert(nested_key.to_string());
    }
}

/// `mark_modified` 的项目作用域版本（两套集合互不影响）。
fn mark_project_modified(state: &mut ManagerState, field: &str, nested_key: Option<&str>) {
    state.modified_project_fields.insert(field.to_string());
    if let Some(nested_key) = nested_key {
        state
            .modified_project_nested_fields
            .entry(field.to_string())
            .or_default()
            .insert(nested_key.to_string());
    }
}

fn invalid_setting_error(setting_name: &str, value: &str) -> SettingsValueError {
    SettingsValueError::new(format!("Invalid {setting_name} setting: {value}"))
}

// ---------------------------------------------------------------------------
// SettingsManager
// ---------------------------------------------------------------------------

/// `SettingsManager`：单实例、同步 getter、异步写队列。
///
/// 所有方法只借用 `&self`（内部 `RwLock` + 写队列），可以放进 `Arc` 共享。
///
/// `Clone` 是**浅克隆**：克隆出的句柄与原来共享同一份 `Shared`（同一份设置、同一个写
/// 队列），只是各自持有一个发送端；不会复制设置快照，也不会新建 worker。
#[derive(Clone)]
pub struct SettingsManager {
    shared: Arc<Shared>,
    /// 写队列的发送端（每个句柄一份；最后一个句柄销毁时队列关闭）。
    write_tx: tokio::sync::mpsc::UnboundedSender<WriteMessage>,
}

impl std::fmt::Debug for SettingsManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsManager")
            .finish_non_exhaustive()
    }
}

impl SettingsManager {
    // -- 构造 ---------------------------------------------------------------

    /// `SettingsManager.create(cwd, agentDir)`：从文件加载（默认信任项目）。
    pub fn create(cwd: &str, agent_dir: &str) -> Self {
        Self::create_with_options(cwd, agent_dir, SettingsManagerCreateOptions::default())
    }

    /// `SettingsManager.create(cwd, getAgentDir())`。
    pub fn create_default(cwd: &str) -> Self {
        let agent_dir = kiana_core::config::get_agent_dir();
        Self::create_with_options(
            cwd,
            &agent_dir.to_string_lossy(),
            SettingsManagerCreateOptions::default(),
        )
    }

    /// `SettingsManager.create(cwd, agentDir, options)`。
    ///
    /// `cwd` 与 `agentDir` 先经 `resolve_from_cwd`（相对路径按进程当前目录解析），再分别
    /// 拼出 `<cwd>/.kiana/settings.json` 与 `<agentDir>/settings.json`。这两个字符串只用于
    /// 错误诊断；真正的读写由 `FileSettingsStorage` 按同样的路径完成。
    pub fn create_with_options(
        cwd: &str,
        agent_dir: &str,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        let resolved_cwd = storage::resolve_from_cwd(cwd);
        let resolved_agent_dir = storage::resolve_from_cwd(agent_dir);
        let storage = Arc::new(FileSettingsStorage::new(&resolved_cwd, &resolved_agent_dir));
        let paths = SettingsPaths {
            global: Some(storage::join_path(&resolved_agent_dir, &["settings.json"])),
            project: Some(storage::join_path(
                &resolved_cwd,
                &[CONFIG_DIR_NAME, "settings.json"],
            )),
        };
        Self::from_storage_with_paths(storage, options, paths)
    }

    /// `SettingsManager.fromStorage(storage)`：任意后端，默认信任项目。
    pub fn from_storage(storage: Arc<dyn SettingsStorage>) -> Self {
        Self::from_storage_with_options(storage, SettingsManagerCreateOptions::default())
    }

    /// `SettingsManager.fromStorage(storage, options)`。
    ///
    /// 不知道后端对应哪个文件，因此错误条目的 `path` 为 `None`（与 TS 的
    /// `fromStorage` 一致）。
    pub fn from_storage_with_options(
        storage: Arc<dyn SettingsStorage>,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        Self::from_storage_with_paths(storage, options, SettingsPaths::default())
    }

    /// 所有构造函数的公共路径：加载两个作用域 → 记录初始错误 → 合并 → 建队列。
    ///
    /// 加载顺序固定 global 在前、project 在后，`errors` 的初始顺序也据此排列。`project_trusted`
    /// 缺省为 `true`；未受信时 project 作用域直接当作空设置且**不**产生错误（没有解析就
    /// 谈不上解析失败）。加载失败的作用域用 `Settings::default()` 占位，错误同时进入
    /// `errors` 与该作用域的 `*_settings_load_error`（后者用于抑制之后的写盘）。
    fn from_storage_with_paths(
        storage: Arc<dyn SettingsStorage>,
        options: SettingsManagerCreateOptions,
        settings_paths: SettingsPaths,
    ) -> Self {
        let project_trusted = options.project_trusted.unwrap_or(true);
        let global_load = try_load_from_storage(&*storage, SettingsScope::Global, true);
        let project_load =
            try_load_from_storage(&*storage, SettingsScope::Project, project_trusted);

        let mut initial_errors = Vec::new();
        let (global_settings, global_error) = match global_load {
            Ok(settings) => (settings, None),
            Err(error) => {
                initial_errors.push(SettingsError {
                    scope: SettingsScope::Global,
                    path: settings_paths.global.clone(),
                    error: error.message().to_string(),
                });
                (Settings::default(), Some(error.message().to_string()))
            }
        };
        let (project_settings, project_error) = match project_load {
            Ok(settings) => (settings, None),
            Err(error) => {
                initial_errors.push(SettingsError {
                    scope: SettingsScope::Project,
                    path: settings_paths.project.clone(),
                    error: error.message().to_string(),
                });
                (Settings::default(), Some(error.message().to_string()))
            }
        };

        let settings = deep_merge_settings(&global_settings, &project_settings);
        let (write_tx, write_rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            storage,
            state: RwLock::new(ManagerState {
                global_settings,
                project_settings,
                settings,
                project_trusted,
                modified_fields: IndexSet::new(),
                modified_nested_fields: IndexMap::new(),
                modified_project_fields: IndexSet::new(),
                modified_project_nested_fields: IndexMap::new(),
                global_settings_load_error: global_error,
                project_settings_load_error: project_error,
                errors: initial_errors,
                settings_paths,
            }),
            write_rx: Mutex::new(Some(write_rx)),
            worker_started: AtomicBool::new(false),
        });
        Self { shared, write_tx }
    }

    /// `SettingsManager.inMemory(settings)`（无文件 I/O）。
    pub fn in_memory(settings: Settings) -> Self {
        Self::in_memory_with_options(settings, SettingsManagerCreateOptions::default())
    }

    /// `SettingsManager.inMemory(settings, options)`。
    ///
    /// 先做一次 `serialize → migrate → stringify`，把调用方传入的对象变成与磁盘文件
    /// 相同的 JSON 形态再交给后端，后续读取路径因此与文件后端完全一致。两个 `expect`
    /// 编码了这一假设：`Settings` 一定能序列化成 JSON 对象，`migrate_settings` 对对象
    /// 不会失败（顶层不是对象时才会返回错误，而这里序列化结果必是对象）。
    pub fn in_memory_with_options(
        settings: Settings,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        let storage = Arc::new(InMemorySettingsStorage::new());
        let mut value = serde_json::to_value(&settings).expect("Settings must serialize to JSON");
        migrate_settings(&mut value).expect("a Settings object is always a JSON object");
        let text = kiana_json::js_object::stringify_pretty(&value, 2);
        storage
            .with_lock(SettingsScope::Global, &mut |_| Ok(Some(text.clone())))
            .expect("in-memory storage cannot fail");
        Self::from_storage_with_options(storage, options)
    }

    // -- 读 ----------------------------------------------------------------

    /// 取状态读锁；锁中毒时继续使用内部值（持锁线程 panic 不应让后续调用全部失败）。
    fn read_state(&self) -> RwLockReadGuard<'_, ManagerState> {
        self.shared
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 取状态写锁；中毒处理同 `read_state`。
    fn write_state(&self) -> RwLockWriteGuard<'_, ManagerState> {
        self.shared
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 在有效设置快照上执行 `f`（不克隆整份设置）。
    pub(crate) fn with_settings<R>(&self, f: impl FnOnce(&Settings) -> R) -> R {
        f(&self.read_state().settings)
    }

    /// `getSettings()`：global + project 合并后的有效设置（深拷贝）。
    pub fn get_settings(&self) -> Settings {
        self.read_state().settings.clone()
    }

    /// `getGlobalSettings()`。
    pub fn get_global_settings(&self) -> Settings {
        self.read_state().global_settings.clone()
    }

    /// `getProjectSettings()`。
    pub fn get_project_settings(&self) -> Settings {
        self.read_state().project_settings.clone()
    }

    /// `isProjectTrusted()`。
    pub fn is_project_trusted(&self) -> bool {
        self.read_state().project_trusted
    }

    // -- 重载 / 覆盖 / 错误 -------------------------------------------------

    /// `async reload()`：先等写队列，再按 global → project 顺序重新加载。
    ///
    /// 某个作用域加载失败时**保留**上一次的值，只记录错误。四个 modified 集合在两次
    /// 加载之间无条件清空：此前写失败遗留的「待重试」标记也会一起丢弃。
    pub async fn reload(&self) {
        self.flush().await;

        let global_load = try_load_from_storage(&*self.shared.storage, SettingsScope::Global, true);
        let project_trusted = self.is_project_trusted();
        let project_load = try_load_from_storage(
            &*self.shared.storage,
            SettingsScope::Project,
            project_trusted,
        );

        let mut state = self.write_state();
        match global_load {
            Ok(settings) => {
                state.global_settings = settings;
                state.global_settings_load_error = None;
            }
            Err(error) => {
                state.global_settings_load_error = Some(error.message().to_string());
                record_error_locked(&mut state, SettingsScope::Global, error.message());
            }
        }

        state.modified_fields.clear();
        state.modified_nested_fields.clear();
        state.modified_project_fields.clear();
        state.modified_project_nested_fields.clear();

        match project_load {
            Ok(settings) => {
                state.project_settings = settings;
                state.project_settings_load_error = None;
            }
            Err(error) => {
                state.project_settings_load_error = Some(error.message().to_string());
                record_error_locked(&mut state, SettingsScope::Project, error.message());
            }
        }

        let merged = deep_merge_settings(&state.global_settings, &state.project_settings);
        state.settings = merged;
    }

    /// `applyOverrides(overrides)`：在有效设置上再叠一层覆盖（不标记 modified、不写盘）。
    ///
    /// 只改 `state.settings`，不动 `global_settings` / `project_settings`。因此下一次成功的
    /// 写盘（`snapshot_global_locked` 会重新合并两个作用域）或 `reload` 都会让这层覆盖
    /// 消失；它适合启动期的临时覆盖，不能当作持久设置。
    pub fn apply_overrides(&self, overrides: Settings) {
        let mut state = self.write_state();
        let merged = deep_merge_settings(&state.settings, &overrides);
        state.settings = merged;
    }

    /// 原子更新项目 settings 里的 `permissions` 对象。
    ///
    /// 供第 1 周的「总是允许（项目）」写回使用：只修改 `permissions` 这一个顶层键，
    /// 磁盘上的其它字段与键序由选择性持久化保留。
    ///
    /// 项目未受信时返回 [`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`]；项目 settings 文件
    /// 加载失败（损坏 JSON 等）时返回错误且不更新内存/磁盘——调用方必须 fail-closed，
    /// 不能把“写不进去”报告成“已记住”。
    pub fn update_project_permissions(
        &self,
        update: impl FnOnce(Option<&Value>) -> Value,
    ) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("permissions", |settings| {
            let current = settings.raw("permissions");
            let next = update(current.as_ref());
            settings.extra.insert("permissions".to_string(), next);
        })
    }

    /// 当前项目作用域里的 `permissions` 对象（不做 global 合并）。
    pub fn project_permissions(&self) -> Option<Value> {
        self.read_state().project_settings.raw("permissions")
    }

    /// 原子更新全局 settings 里的 `permissions` 对象（`/permissions` 面板的 User 作用域）。
    ///
    /// 与 [`Self::update_project_permissions`] 一样只修改 `permissions` 这一个顶层键，
    /// 磁盘上的其它字段与键序由选择性持久化保留。全局 settings 文件加载失败时返回错误
    /// 且不更新内存/磁盘——写回路径必须 fail-closed，不能覆盖损坏的文件。
    pub fn update_global_permissions(
        &self,
        update: impl FnOnce(Option<&Value>) -> Value,
    ) -> Result<(), SettingsValueError> {
        let load_error = {
            let state = self.read_state();
            state.global_settings_load_error.clone()
        };
        if let Some(error) = load_error {
            return Err(SettingsValueError::new(format!(
                "Cannot save global settings because the global settings file failed to load: {error}"
            )));
        }
        self.mutate_global_and_save(|state| {
            let current = state.global_settings.raw("permissions");
            let next = update(current.as_ref());
            state
                .global_settings
                .extra
                .insert("permissions".to_string(), next);
            mark_modified(state, "permissions", None);
        });
        Ok(())
    }

    /// `async flush()`：等待此前入队的写任务全部完成。
    ///
    /// 屏障本身也走写队列，因此天然排在已入队任务之后；屏障之后再入队的任务不在本次
    /// 等待范围内。同步上下文里 `ensure_worker` 会在 `receiver.await` 之前就地执行屏障，
    /// 所以返回时队列已排空。
    pub async fn flush(&self) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.enqueue_message(WriteMessage::Barrier(sender));
        let _ = receiver.await;
    }

    /// `drainErrors()`：返回并清空错误列表（保持顺序）。
    pub fn drain_errors(&self) -> Vec<SettingsError> {
        let mut state = self.write_state();
        std::mem::take(&mut state.errors)
    }

    // -- 写盘内部 -----------------------------------------------------------

    /// 先入队，再确保 worker 在消费。
    ///
    /// `send` 失败说明接收端已 drop（例如 worker 所在运行时已关闭），此时没有可写的
    /// 消费者，静默丢弃——调用方无法从同步 setter 返回这个错误，它也不该阻止内存更新。
    fn enqueue_message(&self, message: WriteMessage) {
        if self.write_tx.send(message).is_err() {
            // 接收端只在 worker 与 `Shared` 都已销毁后关闭；此时没有可写的目标。
            return;
        }
        self.shared.ensure_worker();
    }

    /// 在**同一把写锁**内完成「改内存 + 标记 modified + 快照 + 入队」，对应 TS 里
    /// 同一个同步 turn 内完成的 `markModified(...)` + `save()`。
    ///
    /// 若把标记与快照拆成两段临界区，写 worker 的 `clearModifiedScope` 可能恰好落在中间：
    /// 这次修改会以空的 `modified_fields` 入队，只把磁盘上的当前内容重新序列化一遍，
    /// 修改永远不落盘。入队也放在锁内，使队列顺序与快照顺序一致（后创建的快照不会被
    /// 先创建、后入队的旧快照覆盖）。
    fn mutate_global_and_save(&self, apply: impl FnOnce(&mut ManagerState)) {
        self.mutate_global_and_save_if(|state| {
            apply(state);
            true
        });
    }

    /// [`Self::mutate_global_and_save`] 的条件版本：闭包返回 `false` 表示不写盘
    /// （对应 TS 里提前 `return`、没有调用 `save()` 的分支）。
    fn mutate_global_and_save_if(&self, apply: impl FnOnce(&mut ManagerState) -> bool) -> bool {
        let should_write = {
            let mut state = self.write_state();
            if !apply(&mut state) {
                false
            } else {
                match snapshot_global_locked(&mut state) {
                    Some(pending) => {
                        // 无界通道的 `send` 不会阻塞；在锁内发送保证 FIFO 顺序 == 快照顺序
                        // （后创建的快照不会被先创建、后入队的旧快照覆盖）。
                        let _ = self.write_tx.send(WriteMessage::Persist(Box::new(pending)));
                        true
                    }
                    // 文件加载失败：只更新内存，不覆盖损坏的文件。
                    None => true,
                }
            }
        };
        // 同步上下文（没有 tokio 运行时）在这里就地排空队列；不能在持锁时调用（会自锁）。
        self.shared.ensure_worker();
        should_write
    }

    /// 嵌套 setter 的原子版本：在写锁内先按 JS 严格模式检查父值是否可赋值，再改内存、
    /// 标记 modified、快照并入队。
    ///
    /// TS 里父值是 数字/字符串/布尔 时，`settings[parent][property] = …` 会同步抛
    /// `TypeError`，因此 setter 在调用方观察到的结果是一条错误（这里用
    /// [`SettingsValueError`] 表达），且内存与磁盘都不变。
    fn mutate_global_nested_and_save(
        &self,
        parent_key: &str,
        property_key: &str,
        nullish_replace: bool,
        apply: impl FnOnce(&mut ManagerState),
    ) -> Result<(), SettingsValueError> {
        let result = {
            let mut state = self.write_state();
            if let Some(error) = nested_parent_assignment_error(
                &state.global_settings,
                parent_key,
                property_key,
                nullish_replace,
            ) {
                Err(error)
            } else {
                apply(&mut state);
                if let Some(pending) = snapshot_global_locked(&mut state) {
                    // 无界通道的 `send` 不会阻塞；在锁内发送保证 FIFO 顺序 == 快照顺序。
                    let _ = self.write_tx.send(WriteMessage::Persist(Box::new(pending)));
                }
                Ok(())
            }
        };
        if result.is_ok() {
            // 同步上下文（没有 tokio 运行时）在这里就地排空队列；不能在持锁时调用（会自锁）。
            self.shared.ensure_worker();
        }
        result
    }

    /// `saveProjectSettings(settings)` + `updateProjectSettings(field, update)` 的原子版本。
    ///
    /// 在写锁内完成信任检查、在 `project_settings` 的克隆上执行 `update`、替换、标记
    /// modified、重算有效设置并入队；因此调用方看到的项目快照要么完全没变，要么包含本次
    /// 修改。这里只标记顶层字段（`update` 是整字段替换，没有嵌套合并）。
    ///
    /// 返回 `Err` 的两种情况（都是 fail-closed）：
    /// - 项目未受信（[`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`]）；
    /// - 项目 settings 文件加载失败（损坏 JSON 等）。此时磁盘上的文件不可解析，写回会
    ///   覆盖用户内容，所以**不写盘、也不更新内存**，并把错误交给调用方。静默返回
    ///   `Ok` 会让「总是允许（项目）」报告成功但重启后规则消失。
    fn mutate_project_and_save(
        &self,
        field: &str,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsValueError> {
        {
            let mut state = self.write_state();
            if !state.project_trusted {
                return Err(SettingsValueError::new(PROJECT_NOT_TRUSTED_WRITE_MESSAGE));
            }
            if let Some(error) = &state.project_settings_load_error {
                return Err(SettingsValueError::new(format!(
                    "Cannot save project settings because the project settings file failed to load: {error}"
                )));
            }
            let mut project_settings = state.project_settings.clone();
            update(&mut project_settings);
            state.project_settings = project_settings;
            mark_project_modified(&mut state, field, None);
            let merged = deep_merge_settings(&state.global_settings, &state.project_settings);
            state.settings = merged;
            if let Some(pending) = snapshot_project_locked(&state) {
                let _ = self.write_tx.send(WriteMessage::Persist(Box::new(pending)));
            }
        }
        self.shared.ensure_worker();
        Ok(())
    }

    /// `setProjectTrusted(trusted)`：切换项目信任状态并同步内存中的项目设置。
    ///
    /// - 值未变化时是空操作（不重载文件、不产生错误）。
    /// - 变为不可信：丢弃内存中的项目设置（置空对象）、清空项目 modified 标记与加载错误，
    ///   不记错误；之后项目写 setter 返回 [`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`]。
    /// - 变为可信：重新从磁盘加载项目作用域；失败时保留当前内存值并记录错误（与 `reload`
    ///   的失败语义一致）。
    pub fn set_project_trusted(&self, trusted: bool) {
        if self.is_project_trusted() == trusted {
            return;
        }

        {
            let mut state = self.write_state();
            state.project_trusted = trusted;
            state.modified_project_fields.clear();
            state.modified_project_nested_fields.clear();
            if !trusted {
                state.project_settings = Settings::default();
                state.project_settings_load_error = None;
                let merged = deep_merge_settings(&state.global_settings, &state.project_settings);
                state.settings = merged;
                return;
            }
        }

        let project_load =
            try_load_from_storage(&*self.shared.storage, SettingsScope::Project, true);
        let mut state = self.write_state();
        match project_load {
            Ok(settings) => {
                state.project_settings = settings;
                state.project_settings_load_error = None;
            }
            Err(error) => {
                state.project_settings_load_error = Some(error.message().to_string());
                record_error_locked(&mut state, SettingsScope::Project, error.message());
            }
        }
        let merged = deep_merge_settings(&state.global_settings, &state.project_settings);
        state.settings = merged;
    }

    // -- changelog / 会话目录 / 默认模型 ------------------------------------

    /// `getLastChangelogVersion()`。
    pub fn get_last_changelog_version(&self) -> Option<String> {
        self.with_settings(|settings| settings.last_changelog_version.clone())
    }

    /// `setLastChangelogVersion(version)`。
    pub fn set_last_changelog_version(&self, version: String) {
        self.mutate_global_and_save(|state| {
            state.global_settings.last_changelog_version = Some(version);
            state
                .global_settings
                .extra
                .shift_remove("lastChangelogVersion");
            mark_modified(state, "lastChangelogVersion", None);
        });
    }

    /// `getSessionDir()`：非空值经 `normalizePath`（展开 `~`）。
    pub fn get_session_dir(&self) -> Option<String> {
        let session_dir = self.with_settings(|settings| settings.session_dir.clone());
        match session_dir {
            Some(value) if !value.is_empty() => {
                Some(normalize_path(&value, &PathInputOptions::default()))
            }
            other => other,
        }
    }

    /// `getDefaultProvider()`。
    pub fn get_default_provider(&self) -> Option<String> {
        self.with_settings(|settings| settings.default_provider.clone())
    }

    /// `getDefaultModel()`。
    pub fn get_default_model(&self) -> Option<String> {
        self.with_settings(|settings| settings.default_model.clone())
    }

    /// `setDefaultProvider(provider)`。
    pub fn set_default_provider(&self, provider: String) {
        self.mutate_global_and_save(|state| {
            state.global_settings.default_provider = Some(provider);
            state.global_settings.extra.shift_remove("defaultProvider");
            mark_modified(state, "defaultProvider", None);
        });
    }

    /// `setDefaultModel(modelId)`。
    pub fn set_default_model(&self, model_id: String) {
        self.mutate_global_and_save(|state| {
            state.global_settings.default_model = Some(model_id);
            state.global_settings.extra.shift_remove("defaultModel");
            mark_modified(state, "defaultModel", None);
        });
    }

    /// `setDefaultModelAndProvider(provider, modelId)`。
    pub fn set_default_model_and_provider(&self, provider: String, model_id: String) {
        self.mutate_global_and_save(|state| {
            state.global_settings.default_provider = Some(provider);
            state.global_settings.extra.shift_remove("defaultProvider");
            state.global_settings.default_model = Some(model_id);
            state.global_settings.extra.shift_remove("defaultModel");
            mark_modified(state, "defaultProvider", None);
            mark_modified(state, "defaultModel", None);
        });
    }

    // -- steering / follow-up ----------------------------------------------

    /// `getSteeringMode()`。
    pub fn get_steering_mode(&self) -> SteeringMode {
        self.with_settings(|settings| {
            settings
                .steering_mode
                .unwrap_or(SETTINGS_DEFAULTS.steering_mode)
        })
    }

    /// `setSteeringMode(mode)`。
    pub fn set_steering_mode(&self, mode: SteeringMode) {
        self.mutate_global_and_save(|state| {
            state.global_settings.steering_mode = Some(mode);
            state.global_settings.extra.shift_remove("steeringMode");
            mark_modified(state, "steeringMode", None);
        });
    }

    /// `getFollowUpMode()`。
    pub fn get_follow_up_mode(&self) -> SteeringMode {
        self.with_settings(|settings| {
            settings
                .follow_up_mode
                .unwrap_or(SETTINGS_DEFAULTS.follow_up_mode)
        })
    }

    /// `setFollowUpMode(mode)`。
    pub fn set_follow_up_mode(&self, mode: SteeringMode) {
        self.mutate_global_and_save(|state| {
            state.global_settings.follow_up_mode = Some(mode);
            state.global_settings.extra.shift_remove("followUpMode");
            mark_modified(state, "followUpMode", None);
        });
    }

    // -- theme -------------------------------------------------------------

    /// `getThemeSetting()`：只返回字符串值。
    pub fn get_theme_setting(&self) -> Option<String> {
        self.with_settings(|settings| settings.theme.clone())
    }

    /// `getTheme()`：含 `/` 的自动主题设置不当作固定主题。
    pub fn get_theme(&self) -> Option<String> {
        let theme = self.get_theme_setting();
        theme.filter(|theme| !theme.contains('/'))
    }

    /// `setTheme(theme)`。
    pub fn set_theme(&self, theme: String) {
        self.mutate_global_and_save(|state| {
            state.global_settings.theme = Some(theme);
            state.global_settings.extra.shift_remove("theme");
            mark_modified(state, "theme", None);
        });
    }

    // -- ui ----------------------------------------------------------------

    /// `getUiLocale()`：界面语言标识。
    ///
    /// 优先取合并后可类型化的 `ui.locale`（`ui` 整体无法解析时回落到原始对象里的
    /// 字符串 `locale`）；空串或纯空白视为未设置。其次回退旧键 `language`（不在
    /// schema 中，只存在于 `extra`）；都没有时返回 `"en"`。
    pub fn get_ui_locale(&self) -> String {
        self.with_settings(|settings| {
            let locale = settings
                .ui
                .as_ref()
                .and_then(|ui| ui.locale.clone())
                .or_else(|| match settings.raw("ui") {
                    Some(Value::Object(ui)) => {
                        ui.get("locale").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                });
            if let Some(locale) = locale
                && !locale.trim().is_empty()
            {
                return locale;
            }
            match settings.raw("language") {
                Some(Value::String(language)) if !language.trim().is_empty() => language,
                _ => "en".to_string(),
            }
        })
    }

    /// `setUiLocale(locale)`：写全局 `ui.locale`，保留 `ui` 的其它字段。
    ///
    /// `ui` 之前是非法原始值（`null`/字符串/数组等）时，它保存在
    /// `global_settings.extra["ui"]`；这里随类型化写入一起删除，避免旧值继续写回。
    /// 整个 `ui` 对象按顶层字段写盘（不是逐子键合并），因此旧的非法值不会残留成
    /// 下标键。
    pub fn set_ui_locale(&self, locale: &str) {
        self.mutate_global_and_save(|state| {
            state
                .global_settings
                .ui
                .get_or_insert_with(UiSettings::default)
                .locale = Some(locale.to_string());
            state.global_settings.extra.shift_remove("ui");
            mark_modified(state, "ui", None);
        });
    }

    // -- thinking level ----------------------------------------------------

    /// `getDefaultThinkingLevel()`。
    pub fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel> {
        self.with_settings(|settings| settings.default_thinking_level)
    }

    /// `setDefaultThinkingLevel(level)`。
    pub fn set_default_thinking_level(&self, level: ModelThinkingLevel) {
        self.mutate_global_and_save(|state| {
            state.global_settings.default_thinking_level = Some(level);
            state
                .global_settings
                .extra
                .shift_remove("defaultThinkingLevel");
            mark_modified(state, "defaultThinkingLevel", None);
        });
    }

    /// `getModelThinkingLevel(provider, modelId)`。
    ///
    /// 整张记录里只要有一个非法值，`modelThinkingLevels` 就整体进入 `extra`；此时回落到
    /// 原始记录，逐项解析（合法项仍可用，非法项按 `undefined` 处理）。
    pub fn get_model_thinking_level(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Option<ModelThinkingLevel> {
        let key = format!("{provider}/{model_id}");
        self.with_settings(|settings| {
            if let Some(levels) = &settings.model_thinking_levels {
                return levels.get(&key).copied();
            }
            match settings.raw("modelThinkingLevels") {
                Some(Value::Object(levels)) => levels
                    .get(&key)
                    .and_then(|value| serde_json::from_value(value.clone()).ok()),
                _ => None,
            }
        })
    }

    /// `getAllModelThinkingLevels()`（浅拷贝）。
    ///
    /// 原始记录无法整体解析时，只保留能解析为 [`ModelThinkingLevel`] 的条目（Rust 的
    /// 枚举容纳不了 TS 里原样返回的非法值）。
    pub fn get_all_model_thinking_levels(&self) -> IndexMap<String, ModelThinkingLevel> {
        self.with_settings(|settings| {
            if let Some(levels) = &settings.model_thinking_levels {
                return levels.clone();
            }
            match settings.raw("modelThinkingLevels") {
                Some(Value::Object(levels)) => levels
                    .iter()
                    .filter_map(|(key, value)| {
                        serde_json::from_value(value.clone())
                            .ok()
                            .map(|level| (key.clone(), level))
                    })
                    .collect(),
                _ => IndexMap::new(),
            }
        })
    }

    /// `setModelThinkingLevel(provider, modelId, level)`。
    ///
    /// 整张记录里存在非法值时记录只存在于 `extra`：此时在原始对象上写入，避免把其它模型
    /// 的条目（可能非法）丢掉。
    ///
    /// # Errors
    ///
    /// `modelThinkingLevels` 是 truthy 的原始值时，严格模式下的属性赋值抛
    /// `TypeError: Cannot create property '<provider>/<modelId>' on …`；返回同一文案，
    /// 内存与磁盘均不变。
    pub fn set_model_thinking_level(
        &self,
        provider: &str,
        model_id: &str,
        level: ModelThinkingLevel,
    ) -> Result<(), SettingsValueError> {
        let key = format!("{provider}/{model_id}");
        let property_key = key.clone();
        self.mutate_global_nested_and_save("modelThinkingLevels", &property_key, false, |state| {
            if state.global_settings.model_thinking_levels.is_none() {
                match state
                    .global_settings
                    .extra
                    .get("modelThinkingLevels")
                    .cloned()
                {
                    // 记录无法整体解析：在原始对象上写入，避免丢掉其它（可能非法的）条目。
                    Some(Value::Object(mut levels)) => {
                        levels.insert(key, serde_json::to_value(level).unwrap_or(Value::Null));
                        state
                            .global_settings
                            .extra
                            .insert("modelThinkingLevels".to_string(), Value::Object(levels));
                        mark_modified(state, "modelThinkingLevels", None);
                        return;
                    }
                    // TS：数组也是 truthy 对象，赋值只给数组加一个属性（`JSON.stringify`
                    // 忽略非索引属性），保存后文件仍是原数组；这里保留原数组并写盘。
                    Some(Value::Array(_)) => {
                        mark_modified(state, "modelThinkingLevels", None);
                        return;
                    }
                    // 键缺失 / `null` / falsy 原始值：替换成新对象。
                    _ => {}
                }
            }
            state
                .global_settings
                .model_thinking_levels
                .get_or_insert_with(IndexMap::new)
                .insert(key, level);
            state
                .global_settings
                .extra
                .shift_remove("modelThinkingLevels");
            mark_modified(state, "modelThinkingLevels", None);
        })
    }

    /// `removeModelThinkingLevel(provider, modelId)`：没有对象时直接返回（不写盘）。
    pub fn remove_model_thinking_level(&self, provider: &str, model_id: &str) {
        let key = format!("{provider}/{model_id}");
        self.mutate_global_and_save_if(|state| {
            if state.global_settings.model_thinking_levels.is_none() {
                // 记录无法整体解析时在原始对象上删除（TS 直接 `delete` 属性）。
                let Some(Value::Object(levels)) =
                    state.global_settings.extra.get("modelThinkingLevels")
                else {
                    return false;
                };
                let mut levels = levels.clone();
                levels.shift_remove(&key);
                if levels.is_empty() {
                    state
                        .global_settings
                        .extra
                        .shift_remove("modelThinkingLevels");
                } else {
                    state
                        .global_settings
                        .extra
                        .insert("modelThinkingLevels".to_string(), Value::Object(levels));
                }
                mark_modified(state, "modelThinkingLevels", None);
            } else {
                let Some(mut levels) = state.global_settings.model_thinking_levels.take() else {
                    return false;
                };
                levels.shift_remove(&key);
                state.global_settings.model_thinking_levels = if levels.is_empty() {
                    None
                } else {
                    Some(levels)
                };
                mark_modified(state, "modelThinkingLevels", None);
            }
            true
        });
    }

    // -- transport / share -------------------------------------------------

    /// `getTransport()`。
    pub fn get_transport(&self) -> TransportSetting {
        self.with_settings(|settings| settings.transport.unwrap_or(SETTINGS_DEFAULTS.transport))
    }

    /// `setTransport(transport)`。
    pub fn set_transport(&self, transport: TransportSetting) {
        self.mutate_global_and_save(|state| {
            state.global_settings.transport = Some(transport);
            state.global_settings.extra.shift_remove("transport");
            mark_modified(state, "transport", None);
        });
    }

    /// `getSessionShareDestination()`：仅 `"gist"` / `"radius"` 原样，其余 `"auto"`。
    pub fn get_session_share_destination(&self) -> SessionShareDestination {
        self.with_settings(|settings| match settings.session_share_destination {
            Some(SessionShareDestination::Gist) => SessionShareDestination::Gist,
            Some(SessionShareDestination::Radius) => SessionShareDestination::Radius,
            _ => SETTINGS_DEFAULTS.session_share_destination,
        })
    }

    /// `setSessionShareDestination(destination)`。
    pub fn set_session_share_destination(&self, destination: SessionShareDestination) {
        self.mutate_global_and_save(|state| {
            state.global_settings.session_share_destination = Some(destination);
            state
                .global_settings
                .extra
                .shift_remove("sessionShareDestination");
            mark_modified(state, "sessionShareDestination", None);
        });
    }

    // -- compaction --------------------------------------------------------

    /// `getCompactionEnabled()`。
    pub fn get_compaction_enabled(&self) -> bool {
        self.with_settings(|settings| {
            let typed = settings
                .compaction
                .as_ref()
                .and_then(|compaction| compaction.enabled);
            nested_boolean_setting(
                settings,
                &["compaction"],
                "enabled",
                typed,
                SETTINGS_DEFAULTS.compaction.enabled,
            )
        })
    }

    /// `setCompactionEnabled(enabled)`。
    ///
    /// # Errors
    ///
    /// `compaction` 是 truthy 的原始值时返回
    /// `Cannot create property 'enabled' on …`（TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_compaction_enabled(&self, enabled: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("compaction", "enabled", false, |state| {
            state
                .global_settings
                .compaction
                .get_or_insert_with(CompactionSettings::default)
                .enabled = Some(enabled);
            state.global_settings.extra.shift_remove("compaction");
            mark_modified(state, "compaction", Some("enabled"));
        })
    }

    // -- branch summary ----------------------------------------------------

    /// `getBranchSummarySettings()`。
    pub fn get_branch_summary_settings(&self) -> ResolvedBranchSummarySettings {
        self.with_settings(|settings| {
            let branch_summary = settings.branch_summary.as_ref();
            ResolvedBranchSummarySettings {
                reserve_tokens: nested_number_setting(
                    settings,
                    &["branchSummary"],
                    "reserveTokens",
                    branch_summary.and_then(|summary| summary.reserve_tokens),
                    SETTINGS_DEFAULTS.branch_summary.reserve_tokens,
                ),
                skip_prompt: nested_boolean_setting(
                    settings,
                    &["branchSummary"],
                    "skipPrompt",
                    branch_summary.and_then(|summary| summary.skip_prompt),
                    SETTINGS_DEFAULTS.branch_summary.skip_prompt,
                ),
            }
        })
    }

    /// `getBranchSummarySkipPrompt()`。
    pub fn get_branch_summary_skip_prompt(&self) -> bool {
        self.with_settings(|settings| {
            nested_boolean_setting(
                settings,
                &["branchSummary"],
                "skipPrompt",
                settings
                    .branch_summary
                    .as_ref()
                    .and_then(|summary| summary.skip_prompt),
                SETTINGS_DEFAULTS.branch_summary.skip_prompt,
            )
        })
    }

    // -- retry -------------------------------------------------------------

    /// `getRetryEnabled()`。
    pub fn get_retry_enabled(&self) -> bool {
        self.with_settings(|settings| {
            let typed = settings.retry.as_ref().and_then(|retry| retry.enabled);
            nested_boolean_setting(
                settings,
                &["retry"],
                "enabled",
                typed,
                SETTINGS_DEFAULTS.retry.enabled,
            )
        })
    }

    /// `setRetryEnabled(enabled)`。
    ///
    /// # Errors
    ///
    /// `retry` 是 truthy 的原始值时返回 `Cannot create property 'enabled' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_retry_enabled(&self, enabled: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("retry", "enabled", false, |state| {
            state
                .global_settings
                .retry
                .get_or_insert_with(RetrySettings::default)
                .enabled = Some(enabled);
            state.global_settings.extra.shift_remove("retry");
            mark_modified(state, "retry", Some("enabled"));
        })
    }

    /// `getRetrySettings()`。
    pub fn get_retry_settings(&self) -> ResolvedRetrySettings {
        self.with_settings(|settings| {
            let retry = settings.retry.as_ref();
            ResolvedRetrySettings {
                enabled: nested_boolean_setting(
                    settings,
                    &["retry"],
                    "enabled",
                    retry.and_then(|retry| retry.enabled),
                    SETTINGS_DEFAULTS.retry.enabled,
                ),
                max_retries: nested_number_setting(
                    settings,
                    &["retry"],
                    "maxRetries",
                    retry.and_then(|retry| retry.max_retries),
                    SETTINGS_DEFAULTS.retry.max_retries,
                ),
                base_delay_ms: nested_number_setting(
                    settings,
                    &["retry"],
                    "baseDelayMs",
                    retry.and_then(|retry| retry.base_delay_ms),
                    SETTINGS_DEFAULTS.retry.base_delay_ms,
                ),
                max_agent_delay_ms: nested_number_setting(
                    settings,
                    &["retry"],
                    "maxAgentDelayMs",
                    retry.and_then(|retry| retry.max_agent_delay_ms),
                    SETTINGS_DEFAULTS.retry.max_agent_delay_ms,
                ),
            }
        })
    }

    /// `getProviderRetrySettings()`：`timeoutMs` / `maxRetries` 不填默认值。
    pub fn get_provider_retry_settings(&self) -> ResolvedProviderRetrySettings {
        self.with_settings(|settings| {
            let provider = settings
                .retry
                .as_ref()
                .and_then(|retry| retry.provider.as_ref());
            ResolvedProviderRetrySettings {
                timeout_ms: optional_nested_number_setting(
                    settings,
                    &["retry", "provider"],
                    "timeoutMs",
                    provider.and_then(|provider| provider.timeout_ms),
                ),
                max_retries: optional_nested_number_setting(
                    settings,
                    &["retry", "provider"],
                    "maxRetries",
                    provider.and_then(|provider| provider.max_retries),
                ),
                max_retry_delay_ms: nested_number_setting(
                    settings,
                    &["retry", "provider"],
                    "maxRetryDelayMs",
                    provider.and_then(|provider| provider.max_retry_delay_ms),
                    SETTINGS_DEFAULTS.retry.provider.max_retry_delay_ms,
                ),
            }
        })
    }

    /// `getHttpIdleTimeoutMs()`。
    ///
    /// # Errors
    ///
    /// 配置值非法时返回 `Invalid httpIdleTimeoutMs setting: ${String(value)}`。
    pub fn get_http_idle_timeout_ms(&self) -> Result<f64, SettingsValueError> {
        let configured =
            self.with_settings(|settings| timeout_setting(settings, "httpIdleTimeoutMs"));
        Ok(configured?.unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS as f64))
    }

    /// `setHttpIdleTimeoutMs(timeoutMs)`。
    ///
    /// # Errors
    ///
    /// `timeoutMs` 非有限或为负时返回 `Invalid httpIdleTimeoutMs setting: ${String(ms)}`。
    pub fn set_http_idle_timeout_ms(&self, timeout_ms: f64) -> Result<(), SettingsValueError> {
        if !timeout_ms.is_finite() || timeout_ms < 0.0 {
            return Err(invalid_setting_error(
                "httpIdleTimeoutMs",
                &js_string_number(timeout_ms),
            ));
        }
        self.mutate_global_and_save(|state| {
            state.global_settings.http_idle_timeout_ms =
                Some(TimeoutSetting::Millis(js_floor(timeout_ms)));
            state
                .global_settings
                .extra
                .shift_remove("httpIdleTimeoutMs");
            mark_modified(state, "httpIdleTimeoutMs", None);
        });
        Ok(())
    }

    /// `getCacheWarmingMode()`：**只读全局设置**（预热要花钱）。
    pub fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        self.read_state()
            .global_settings
            .cache_warming
            .filter(|mode| CACHE_WARMING_MODES.contains(mode))
            .unwrap_or(SETTINGS_DEFAULTS.cache_warming)
    }

    /// `setCacheWarmingMode(mode)`。
    pub fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        self.mutate_global_and_save(|state| {
            state.global_settings.cache_warming = Some(mode);
            state.global_settings.extra.shift_remove("cacheWarming");
            mark_modified(state, "cacheWarming", None);
        });
    }

    /// `getWebSocketConnectTimeoutMs()`。
    ///
    /// # Errors
    ///
    /// 配置值非法时返回 `Invalid websocketConnectTimeoutMs setting: ${String(value)}`。
    pub fn get_websocket_connect_timeout_ms(&self) -> Result<Option<f64>, SettingsValueError> {
        self.with_settings(|settings| timeout_setting(settings, "websocketConnectTimeoutMs"))
    }

    // -- 布尔 / 字符串设置 --------------------------------------------------

    /// `getHideThinkingBlock()`。
    pub fn get_hide_thinking_block(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "hideThinkingBlock",
                settings.hide_thinking_block,
                SETTINGS_DEFAULTS.hide_thinking_block,
            )
        })
    }

    /// `getShowCacheMissNotices()`。
    pub fn get_show_cache_miss_notices(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "showCacheMissNotices",
                settings.show_cache_miss_notices,
                SETTINGS_DEFAULTS.show_cache_miss_notices,
            )
        })
    }

    /// `setHideThinkingBlock(hide)`。
    pub fn set_hide_thinking_block(&self, hide: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.hide_thinking_block = Some(hide);
            state
                .global_settings
                .extra
                .shift_remove("hideThinkingBlock");
            mark_modified(state, "hideThinkingBlock", None);
        });
    }

    /// `setShowCacheMissNotices(show)`。
    pub fn set_show_cache_miss_notices(&self, show: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.show_cache_miss_notices = Some(show);
            state
                .global_settings
                .extra
                .shift_remove("showCacheMissNotices");
            mark_modified(state, "showCacheMissNotices", None);
        });
    }

    /// `getExternalEditorCommand()`：配置值（非空）→ `$VISUAL` → `$EDITOR` → 平台默认。
    pub fn get_external_editor_command(&self) -> String {
        let configured = self.with_settings(|settings| settings.external_editor.clone());
        if let Some(editor) = configured
            && !kiana_json::js_trim(&editor).is_empty()
        {
            return editor;
        }
        let environment = std::env::var("VISUAL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|value| !value.is_empty())
            });
        if let Some(editor) = environment {
            return editor;
        }
        if cfg!(windows) {
            "notepad".to_string()
        } else {
            "nano".to_string()
        }
    }

    /// `getShellPath()`：非空值经 `normalizePath`（展开 `~`）。
    pub fn get_shell_path(&self) -> Option<String> {
        let shell_path = self.with_settings(|settings| settings.shell_path.clone());
        match shell_path {
            Some(value) if !value.is_empty() => {
                Some(normalize_path(&value, &PathInputOptions::default()))
            }
            other => other,
        }
    }

    /// `setShellPath(path)`（允许 `undefined`：写盘时该键被删除）。
    pub fn set_shell_path(&self, path: Option<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.shell_path = path;
            state.global_settings.extra.shift_remove("shellPath");
            mark_modified(state, "shellPath", None);
        });
    }

    /// `getQuietStartup()`：`true` / `"header"` 原样，其余 `false`。
    pub fn get_quiet_startup(&self) -> QuietStartup {
        self.with_settings(|settings| match settings.quiet_startup {
            Some(QuietStartup::Header) => QuietStartup::Header,
            Some(QuietStartup::Boolean(true)) => QuietStartup::Boolean(true),
            _ => SETTINGS_DEFAULTS.quiet_startup,
        })
    }

    /// `setQuietStartup(quiet)`。
    pub fn set_quiet_startup(&self, quiet: QuietStartup) {
        self.mutate_global_and_save(|state| {
            state.global_settings.quiet_startup = Some(quiet);
            state.global_settings.extra.shift_remove("quietStartup");
            mark_modified(state, "quietStartup", None);
        });
    }

    /// `getDefaultProjectTrust()`：**只读全局设置**，`"always"` / `"never"` 原样，其余 `"ask"`。
    pub fn get_default_project_trust(&self) -> DefaultProjectTrust {
        self.read_state()
            .global_settings
            .default_project_trust
            .filter(|trust| {
                matches!(
                    trust,
                    DefaultProjectTrust::Always | DefaultProjectTrust::Never
                )
            })
            .unwrap_or(SETTINGS_DEFAULTS.default_project_trust)
    }

    /// `setDefaultProjectTrust(defaultProjectTrust)`。
    pub fn set_default_project_trust(&self, default_project_trust: DefaultProjectTrust) {
        self.mutate_global_and_save(|state| {
            state.global_settings.default_project_trust = Some(default_project_trust);
            state
                .global_settings
                .extra
                .shift_remove("defaultProjectTrust");
            mark_modified(state, "defaultProjectTrust", None);
        });
    }

    /// `getShellCommandPrefix()`。
    pub fn get_shell_command_prefix(&self) -> Option<String> {
        self.with_settings(|settings| settings.shell_command_prefix.clone())
    }

    /// `setShellCommandPrefix(prefix)`。
    pub fn set_shell_command_prefix(&self, prefix: Option<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.shell_command_prefix = prefix;
            state
                .global_settings
                .extra
                .shift_remove("shellCommandPrefix");
            mark_modified(state, "shellCommandPrefix", None);
        });
    }

    /// `getNpmCommand()`。
    pub fn get_npm_command(&self) -> Option<Vec<String>> {
        self.with_settings(npm_command_setting)
    }

    /// `setNpmCommand(command)`。
    pub fn set_npm_command(&self, command: Option<Vec<String>>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.npm_command = command;
            state.global_settings.extra.shift_remove("npmCommand");
            mark_modified(state, "npmCommand", None);
        });
    }

    /// `getCollapseChangelog()`。
    pub fn get_collapse_changelog(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "collapseChangelog",
                settings.collapse_changelog,
                SETTINGS_DEFAULTS.collapse_changelog,
            )
        })
    }

    /// `setCollapseChangelog(collapse)`。
    pub fn set_collapse_changelog(&self, collapse: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.collapse_changelog = Some(collapse);
            state
                .global_settings
                .extra
                .shift_remove("collapseChangelog");
            mark_modified(state, "collapseChangelog", None);
        });
    }

    // -- telemetry / analytics ---------------------------------------------

    /// `getEnableInstallTelemetry()`。
    pub fn get_enable_install_telemetry(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "enableInstallTelemetry",
                settings.enable_install_telemetry,
                SETTINGS_DEFAULTS.enable_install_telemetry,
            )
        })
    }

    /// `setEnableInstallTelemetry(enabled)`。
    pub fn set_enable_install_telemetry(&self, enabled: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.enable_install_telemetry = Some(enabled);
            state
                .global_settings
                .extra
                .shift_remove("enableInstallTelemetry");
            mark_modified(state, "enableInstallTelemetry", None);
        });
    }

    /// `getEnableAnalytics()`。
    pub fn get_enable_analytics(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "enableAnalytics",
                settings.enable_analytics,
                SETTINGS_DEFAULTS.enable_analytics,
            )
        })
    }

    /// `getTrackingId()`。
    ///
    /// 类型化字段存在时原样返回（空串也是 `Some("")`）；字段类型不符时按 TS 的属性读取：
    /// `null` 与缺失返回 `None`，其它原始值按 `String(value)` 返回。
    pub fn get_tracking_id(&self) -> Option<String> {
        self.with_settings(|settings| match &settings.tracking_id {
            Some(value) => Some(value.clone()),
            None => match settings.raw("trackingId") {
                Some(Value::Null) | None => None,
                Some(raw) => Some(js_string_value(&raw)),
            },
        })
    }

    /// `setEnableAnalytics(enabled)`：首次 opt-in 时生成 tracking id。
    ///
    /// TS 的判定是 `if (enabled && !this.globalSettings.trackingId)`：空串、`0`、
    /// `false`、`null` 都是 falsy，会生成新的 UUID。
    pub fn set_enable_analytics(&self, enabled: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.enable_analytics = Some(enabled);
            state.global_settings.extra.shift_remove("enableAnalytics");
            mark_modified(state, "enableAnalytics", None);
            let has_tracking_id = truthy_string_setting(
                &state.global_settings,
                "trackingId",
                &state.global_settings.tracking_id,
            )
            .is_some();
            if enabled && !has_tracking_id {
                let tracking_id = uuid::Uuid::new_v4().to_string();
                state.global_settings.tracking_id = Some(tracking_id);
                state.global_settings.extra.shift_remove("trackingId");
                mark_modified(state, "trackingId", None);
            }
        });
    }

    /// `getOrCreateDeviceId()`：**只用全局设置**，首次调用生成并写盘。
    ///
    /// TS 的判定是 `if (!this.globalSettings.deviceId)`：空串是 falsy，会被替换成新 UUID。
    /// 这里用写锁把「检查 → 生成 → 标记 → 入队」做成一个原子步骤，两个并发调用不会各自
    /// 生成一个 UUID；`ensure_worker` 放在锁外，避免同步上下文就地排空时自锁。
    pub fn get_or_create_device_id(&self) -> String {
        let device_id = {
            let mut state = self.write_state();
            if let Some(device_id) = truthy_string_setting(
                &state.global_settings,
                "deviceId",
                &state.global_settings.device_id,
            ) {
                return device_id;
            }
            let device_id = uuid::Uuid::new_v4().to_string();
            state.global_settings.device_id = Some(device_id.clone());
            state.global_settings.extra.shift_remove("deviceId");
            mark_modified(&mut state, "deviceId", None);
            if let Some(pending) = snapshot_global_locked(&mut state) {
                let _ = self.write_tx.send(WriteMessage::Persist(Box::new(pending)));
            }
            device_id
        };
        self.shared.ensure_worker();
        device_id
    }

    // -- 资源路径 -----------------------------------------------------------

    /// `getPackages()`。
    pub fn get_packages(&self) -> Vec<PackageSource> {
        self.with_settings(|settings| {
            package_source_array_setting(settings, "packages", &settings.packages)
        })
    }

    /// `setPackages(packages)`。
    pub fn set_packages(&self, packages: Vec<PackageSource>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.packages = Some(packages);
            state.global_settings.extra.shift_remove("packages");
            mark_modified(state, "packages", None);
        });
    }

    /// `setProjectPackages(packages)`。
    ///
    /// # Errors
    ///
    /// 项目未受信时返回 `Project is not trusted; refusing to write project settings`。
    pub fn set_project_packages(
        &self,
        packages: Vec<PackageSource>,
    ) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("packages", |settings| {
            settings.packages = Some(packages);
        })
    }

    /// `getExtensionPaths()`。
    pub fn get_extension_paths(&self) -> Vec<String> {
        self.with_settings(|settings| {
            string_array_setting(settings, "extensions", &settings.extensions)
        })
    }

    /// `setExtensionPaths(paths)`。
    pub fn set_extension_paths(&self, paths: Vec<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.extensions = Some(paths);
            state.global_settings.extra.shift_remove("extensions");
            mark_modified(state, "extensions", None);
        });
    }

    /// `setProjectExtensionPaths(paths)`。
    ///
    /// # Errors
    ///
    /// 项目未受信时返回逐字错误。
    pub fn set_project_extension_paths(
        &self,
        paths: Vec<String>,
    ) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("extensions", |settings| {
            settings.extensions = Some(paths);
        })
    }

    /// `getSkillPaths()`。
    pub fn get_skill_paths(&self) -> Vec<String> {
        self.with_settings(|settings| string_array_setting(settings, "skills", &settings.skills))
    }

    /// `setSkillPaths(paths)`。
    pub fn set_skill_paths(&self, paths: Vec<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.skills = Some(paths);
            state.global_settings.extra.shift_remove("skills");
            mark_modified(state, "skills", None);
        });
    }

    /// `setProjectSkillPaths(paths)`。
    ///
    /// # Errors
    ///
    /// 项目未受信时返回逐字错误。
    pub fn set_project_skill_paths(&self, paths: Vec<String>) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("skills", |settings| {
            settings.skills = Some(paths);
        })
    }

    /// `getPromptTemplatePaths()`。
    pub fn get_prompt_template_paths(&self) -> Vec<String> {
        self.with_settings(|settings| string_array_setting(settings, "prompts", &settings.prompts))
    }

    /// `setPromptTemplatePaths(paths)`。
    pub fn set_prompt_template_paths(&self, paths: Vec<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.prompts = Some(paths);
            state.global_settings.extra.shift_remove("prompts");
            mark_modified(state, "prompts", None);
        });
    }

    /// `setProjectPromptTemplatePaths(paths)`。
    ///
    /// # Errors
    ///
    /// 项目未受信时返回逐字错误。
    pub fn set_project_prompt_template_paths(
        &self,
        paths: Vec<String>,
    ) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("prompts", |settings| {
            settings.prompts = Some(paths);
        })
    }

    /// `getThemePaths()`。
    pub fn get_theme_paths(&self) -> Vec<String> {
        self.with_settings(|settings| string_array_setting(settings, "themes", &settings.themes))
    }

    /// `setThemePaths(paths)`。
    pub fn set_theme_paths(&self, paths: Vec<String>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.themes = Some(paths);
            state.global_settings.extra.shift_remove("themes");
            mark_modified(state, "themes", None);
        });
    }

    /// `setProjectThemePaths(paths)`。
    ///
    /// # Errors
    ///
    /// 项目未受信时返回逐字错误。
    pub fn set_project_theme_paths(&self, paths: Vec<String>) -> Result<(), SettingsValueError> {
        self.mutate_project_and_save("themes", |settings| {
            settings.themes = Some(paths);
        })
    }

    /// `getEnableSkillCommands()`。
    pub fn get_enable_skill_commands(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "enableSkillCommands",
                settings.enable_skill_commands,
                SETTINGS_DEFAULTS.enable_skill_commands,
            )
        })
    }

    /// `setEnableSkillCommands(enabled)`。
    pub fn set_enable_skill_commands(&self, enabled: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.enable_skill_commands = Some(enabled);
            state
                .global_settings
                .extra
                .shift_remove("enableSkillCommands");
            mark_modified(state, "enableSkillCommands", None);
        });
    }

    /// `getThinkingBudgets()`。
    pub fn get_thinking_budgets(&self) -> Option<ThinkingBudgetsSettings> {
        self.with_settings(|settings| settings.thinking_budgets.clone())
    }

    // -- terminal ----------------------------------------------------------

    /// `getTerminalCapabilityOverrides()`：`"auto"` 与未设置被省略；`false` → `images: null`。
    pub fn get_terminal_capability_overrides(
        &self,
    ) -> kiana_tui::terminal_image::CapabilityOverrides {
        use kiana_tui::{ImageProtocol, terminal_image::CapabilityOverrides};

        self.with_settings(|settings| {
            let mut overrides = CapabilityOverrides::default();
            let Some(terminal) = settings.terminal.as_ref() else {
                return overrides;
            };
            match terminal.images {
                Some(TerminalImagesSetting::Protocol(protocol)) => match protocol {
                    schema::TerminalImageProtocol::Kitty => {
                        overrides.images = Some(Some(ImageProtocol::Kitty));
                    }
                    schema::TerminalImageProtocol::Iterm2 => {
                        overrides.images = Some(Some(ImageProtocol::Iterm2));
                    }
                    schema::TerminalImageProtocol::Auto => {}
                },
                Some(TerminalImagesSetting::Disabled) => {
                    overrides.images = Some(None);
                }
                None => {}
            }
            if let Some(schema::BooleanOrAuto::Boolean(value)) = terminal.true_color {
                overrides.true_color = Some(value);
            }
            if let Some(schema::BooleanOrAuto::Boolean(value)) = terminal.hyperlinks {
                overrides.hyperlinks = Some(value);
            }
            overrides
        })
    }

    /// `getShowImages()`。
    pub fn get_show_images(&self) -> bool {
        self.with_settings(|settings| {
            nested_boolean_setting(
                settings,
                &["terminal"],
                "showImages",
                settings
                    .terminal
                    .as_ref()
                    .and_then(|terminal| terminal.show_images),
                SETTINGS_DEFAULTS.terminal.show_images,
            )
        })
    }

    /// `setShowImages(show)`。
    ///
    /// # Errors
    ///
    /// `terminal` 是 truthy 的原始值时返回 `Cannot create property 'showImages' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_show_images(&self, show: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("terminal", "showImages", false, |state| {
            state
                .global_settings
                .terminal
                .get_or_insert_with(TerminalSettings::default)
                .show_images = Some(show);
            state.global_settings.extra.shift_remove("terminal");
            mark_modified(state, "terminal", Some("showImages"));
        })
    }

    /// `getImageWidthCells()`：非有限数 → 默认 60；否则 `max(1, floor(w))`。
    pub fn get_image_width_cells(&self) -> f64 {
        self.with_settings(|settings| {
            let width = settings
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.image_width_cells);
            match width {
                Some(width) if width.is_finite() => js_max(1.0, js_floor(width)),
                _ => SETTINGS_DEFAULTS.terminal.image_width_cells,
            }
        })
    }

    /// `setImageWidthCells(width)`。
    ///
    /// # Errors
    ///
    /// `terminal` 是 truthy 的原始值时返回 `Cannot create property 'imageWidthCells' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_image_width_cells(&self, width: f64) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("terminal", "imageWidthCells", false, |state| {
            state
                .global_settings
                .terminal
                .get_or_insert_with(TerminalSettings::default)
                .image_width_cells = Some(js_max(1.0, js_floor(width)));
            state.global_settings.extra.shift_remove("terminal");
            mark_modified(state, "terminal", Some("imageWidthCells"));
        })
    }

    /// `getClearOnShrink()`：设置优先，其次 `KIANA_CLEAR_ON_SHRINK === "1"`，最后默认 `false`。
    pub fn get_clear_on_shrink(&self) -> bool {
        let configured = self.with_settings(|settings| {
            if let Some(value) = settings
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.clear_on_shrink)
            {
                return Some(value);
            }
            // `terminal?.clearOnShrink !== undefined`：原始值（含 `null`）参与真假判定，
            // 只有父对象/字段不存在时才看环境变量。
            nested_raw(settings, "terminal", "clearOnShrink").map(|raw| js_truthy(&raw))
        });
        if let Some(value) = configured {
            return value;
        }
        if std::env::var("KIANA_CLEAR_ON_SHRINK").is_ok_and(|value| value == "1") {
            return true;
        }
        SETTINGS_DEFAULTS.terminal.clear_on_shrink
    }

    /// `setClearOnShrink(enabled)`。
    ///
    /// # Errors
    ///
    /// `terminal` 是 truthy 的原始值时返回 `Cannot create property 'clearOnShrink' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_clear_on_shrink(&self, enabled: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("terminal", "clearOnShrink", false, |state| {
            state
                .global_settings
                .terminal
                .get_or_insert_with(TerminalSettings::default)
                .clear_on_shrink = Some(enabled);
            state.global_settings.extra.shift_remove("terminal");
            mark_modified(state, "terminal", Some("clearOnShrink"));
        })
    }

    /// `getShowTerminalProgress()`。
    pub fn get_show_terminal_progress(&self) -> bool {
        self.with_settings(|settings| {
            nested_boolean_setting(
                settings,
                &["terminal"],
                "showTerminalProgress",
                settings
                    .terminal
                    .as_ref()
                    .and_then(|terminal| terminal.show_terminal_progress),
                SETTINGS_DEFAULTS.terminal.show_terminal_progress,
            )
        })
    }

    /// `setShowTerminalProgress(enabled)`。
    ///
    /// # Errors
    ///
    /// `terminal` 是 truthy 的原始值时返回
    /// `Cannot create property 'showTerminalProgress' on …`（TS 严格模式的 `TypeError`），
    /// 内存与磁盘不变。
    pub fn set_show_terminal_progress(&self, enabled: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("terminal", "showTerminalProgress", false, |state| {
            state
                .global_settings
                .terminal
                .get_or_insert_with(TerminalSettings::default)
                .show_terminal_progress = Some(enabled);
            state.global_settings.extra.shift_remove("terminal");
            mark_modified(state, "terminal", Some("showTerminalProgress"));
        })
    }

    // -- TUI 模式 / 全屏 ---------------------------------------------------

    /// `getTuiMode()`：仅 `"regular"` 原样，其余 `"fullscreen"`。
    pub fn get_tui_mode(&self) -> TuiMode {
        self.with_settings(|settings| match settings.tui_mode {
            Some(TuiMode::Regular) => TuiMode::Regular,
            _ => SETTINGS_DEFAULTS.tui_mode,
        })
    }

    /// `setTuiMode(mode)`。
    pub fn set_tui_mode(&self, mode: TuiMode) {
        self.mutate_global_and_save(|state| {
            state.global_settings.tui_mode = Some(mode);
            state.global_settings.extra.shift_remove("tuiMode");
            mark_modified(state, "tuiMode", None);
        });
    }

    /// `getFullscreenExitOutput()`：仅 `"resume-hint"` 原样，其余 `"transcript"`。
    pub fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput {
        self.with_settings(|settings| match settings.fullscreen_exit_output {
            Some(FullscreenExitOutput::ResumeHint) => FullscreenExitOutput::ResumeHint,
            _ => SETTINGS_DEFAULTS.fullscreen_exit_output,
        })
    }

    /// `setFullscreenExitOutput(output)`。
    pub fn set_fullscreen_exit_output(&self, output: FullscreenExitOutput) {
        self.mutate_global_and_save(|state| {
            state.global_settings.fullscreen_exit_output = Some(output);
            state
                .global_settings
                .extra
                .shift_remove("fullscreenExitOutput");
            mark_modified(state, "fullscreenExitOutput", None);
        });
    }

    /// `getFullscreenScrollbar()`：`"always"` / `"hidden"` 原样，其余 `"auto"`。
    pub fn get_fullscreen_scrollbar(&self) -> kiana_tui::ScrollViewScrollbar {
        use kiana_tui::ScrollViewScrollbar;

        self.with_settings(|settings| match settings.fullscreen_scrollbar {
            Some(FullscreenScrollbar::Always) => ScrollViewScrollbar::Always,
            Some(FullscreenScrollbar::Hidden) => ScrollViewScrollbar::Hidden,
            _ => ScrollViewScrollbar::Auto,
        })
    }

    /// `setFullscreenScrollbar(mode)`。
    pub fn set_fullscreen_scrollbar(&self, mode: kiana_tui::ScrollViewScrollbar) {
        use kiana_tui::ScrollViewScrollbar;

        let mode = match mode {
            ScrollViewScrollbar::Always => FullscreenScrollbar::Always,
            ScrollViewScrollbar::Hidden => FullscreenScrollbar::Hidden,
            ScrollViewScrollbar::Auto => FullscreenScrollbar::Auto,
        };
        self.mutate_global_and_save(|state| {
            state.global_settings.fullscreen_scrollbar = Some(mode);
            state
                .global_settings
                .extra
                .shift_remove("fullscreenScrollbar");
            mark_modified(state, "fullscreenScrollbar", None);
        });
    }

    /// `getFullscreenCopyOnSelect()`。
    pub fn get_fullscreen_copy_on_select(&self) -> bool {
        self.with_settings(|settings| {
            boolean_setting(
                settings,
                "fullscreenCopyOnSelect",
                settings.fullscreen_copy_on_select,
                SETTINGS_DEFAULTS.fullscreen_copy_on_select,
            )
        })
    }

    /// `setFullscreenCopyOnSelect(enabled)`。
    pub fn set_fullscreen_copy_on_select(&self, enabled: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.fullscreen_copy_on_select = Some(enabled);
            state
                .global_settings
                .extra
                .shift_remove("fullscreenCopyOnSelect");
            mark_modified(state, "fullscreenCopyOnSelect", None);
        });
    }

    /// `getFullscreenWheelScrollLines()`：有限数 → `max(1, min(100, floor))`；否则 `"auto"`。
    pub fn get_fullscreen_wheel_scroll_lines(&self) -> kiana_tui::WheelScrollLines {
        use kiana_tui::WheelScrollLines;

        self.with_settings(|settings| match settings.fullscreen_wheel_scroll_lines {
            Some(FullscreenWheelScrollLines::Lines(lines)) if lines.is_finite() => {
                WheelScrollLines::Fixed(js_max(1.0, js_min(100.0, js_floor(lines))))
            }
            _ => match SETTINGS_DEFAULTS.fullscreen_wheel_scroll_lines {
                FullscreenWheelScrollLines::Auto => WheelScrollLines::Auto,
                FullscreenWheelScrollLines::Lines(lines) => WheelScrollLines::Fixed(lines),
            },
        })
    }

    /// `setFullscreenWheelScrollLines(lines)`。
    pub fn set_fullscreen_wheel_scroll_lines(&self, lines: kiana_tui::WheelScrollLines) {
        use kiana_tui::WheelScrollLines;

        let value = match lines {
            WheelScrollLines::Auto => FullscreenWheelScrollLines::Auto,
            WheelScrollLines::Fixed(lines) => {
                FullscreenWheelScrollLines::Lines(js_max(1.0, js_min(100.0, js_floor(lines))))
            }
        };
        self.mutate_global_and_save(|state| {
            state.global_settings.fullscreen_wheel_scroll_lines = Some(value);
            state
                .global_settings
                .extra
                .shift_remove("fullscreenWheelScrollLines");
            mark_modified(state, "fullscreenWheelScrollLines", None);
        });
    }

    // -- images ------------------------------------------------------------

    /// `getImageAutoResize()`。
    pub fn get_image_auto_resize(&self) -> bool {
        self.with_settings(|settings| {
            nested_boolean_setting(
                settings,
                &["images"],
                "autoResize",
                settings
                    .images
                    .as_ref()
                    .and_then(|images| images.auto_resize),
                SETTINGS_DEFAULTS.images.auto_resize,
            )
        })
    }

    /// `setImageAutoResize(enabled)`。
    ///
    /// # Errors
    ///
    /// `images` 是 truthy 的原始值时返回 `Cannot create property 'autoResize' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_image_auto_resize(&self, enabled: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("images", "autoResize", false, |state| {
            state
                .global_settings
                .images
                .get_or_insert_with(ImageSettings::default)
                .auto_resize = Some(enabled);
            state.global_settings.extra.shift_remove("images");
            mark_modified(state, "images", Some("autoResize"));
        })
    }

    /// `getBlockImages()`。
    pub fn get_block_images(&self) -> bool {
        self.with_settings(|settings| {
            nested_boolean_setting(
                settings,
                &["images"],
                "blockImages",
                settings
                    .images
                    .as_ref()
                    .and_then(|images| images.block_images),
                SETTINGS_DEFAULTS.images.block_images,
            )
        })
    }

    /// `setBlockImages(blocked)`。
    ///
    /// # Errors
    ///
    /// `images` 是 truthy 的原始值时返回 `Cannot create property 'blockImages' on …`
    /// （TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_block_images(&self, blocked: bool) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("images", "blockImages", false, |state| {
            state
                .global_settings
                .images
                .get_or_insert_with(ImageSettings::default)
                .block_images = Some(blocked);
            state.global_settings.extra.shift_remove("images");
            mark_modified(state, "images", Some("blockImages"));
        })
    }

    // -- 模型过滤 / 工具 ----------------------------------------------------

    /// `getEnabledModels()`。
    pub fn get_enabled_models(&self) -> Option<Vec<String>> {
        self.with_settings(|settings| {
            optional_string_array_setting(settings, "enabledModels", &settings.enabled_models)
        })
    }

    /// `setEnabledModels(patterns)`。
    pub fn set_enabled_models(&self, patterns: Option<Vec<String>>) {
        self.mutate_global_and_save(|state| {
            state.global_settings.enabled_models = patterns;
            state.global_settings.extra.shift_remove("enabledModels");
            mark_modified(state, "enabledModels", None);
        });
    }

    /// `getDefaultTools()`：`undefined` 当且仅当没有任何设置层定义 `defaultTools`。
    ///
    /// TS 逐字：`const tools = this.settings.defaultTools; if (tools === undefined) return
    /// undefined; return resolveDefaultTools(Array.isArray(tools) ? tools.filter(t =>
    /// typeof t === "string") : []);`
    pub fn get_default_tools(&self) -> Option<Vec<String>> {
        self.with_settings(|settings| {
            let raw = settings.raw("defaultTools")?;
            let entries: Vec<String> = match raw {
                Value::Array(items) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            };
            Some(resolve_default_tools(&entries))
        })
    }

    // -- 杂项 --------------------------------------------------------------

    /// `getDoubleEscapeAction()`。
    pub fn get_double_escape_action(&self) -> DoubleEscapeAction {
        self.with_settings(|settings| {
            settings
                .double_escape_action
                .unwrap_or(SETTINGS_DEFAULTS.double_escape_action)
        })
    }

    /// `setDoubleEscapeAction(action)`。
    pub fn set_double_escape_action(&self, action: DoubleEscapeAction) {
        self.mutate_global_and_save(|state| {
            state.global_settings.double_escape_action = Some(action);
            state
                .global_settings
                .extra
                .shift_remove("doubleEscapeAction");
            mark_modified(state, "doubleEscapeAction", None);
        });
    }

    /// `getTreeFilterMode()`：合法值原样，其余 `"default"`。
    pub fn get_tree_filter_mode(&self) -> TreeFilterMode {
        self.with_settings(|settings| {
            settings
                .tree_filter_mode
                .unwrap_or(SETTINGS_DEFAULTS.tree_filter_mode)
        })
    }

    /// `setTreeFilterMode(mode)`。
    pub fn set_tree_filter_mode(&self, mode: TreeFilterMode) {
        self.mutate_global_and_save(|state| {
            state.global_settings.tree_filter_mode = Some(mode);
            state.global_settings.extra.shift_remove("treeFilterMode");
            mark_modified(state, "treeFilterMode", None);
        });
    }

    /// `getShowHardwareCursor()`：设置值优先，否则 `KIANA_HARDWARE_CURSOR === "1"`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.with_settings(|settings| {
            let fallback = std::env::var("KIANA_HARDWARE_CURSOR").is_ok_and(|value| value == "1");
            boolean_setting(
                settings,
                "showHardwareCursor",
                settings.show_hardware_cursor,
                fallback,
            )
        })
    }

    /// `setShowHardwareCursor(enabled)`。
    pub fn set_show_hardware_cursor(&self, enabled: bool) {
        self.mutate_global_and_save(|state| {
            state.global_settings.show_hardware_cursor = Some(enabled);
            state
                .global_settings
                .extra
                .shift_remove("showHardwareCursor");
            mark_modified(state, "showHardwareCursor", None);
        });
    }

    /// `getEditorPaddingX()`：读取时不 clamp。
    pub fn get_editor_padding_x(&self) -> f64 {
        self.with_settings(|settings| {
            number_setting(
                settings,
                "editorPaddingX",
                settings.editor_padding_x,
                SETTINGS_DEFAULTS.editor_padding_x,
            )
        })
    }

    /// `setEditorPaddingX(padding)`：写 `max(0, min(3, floor))`。
    pub fn set_editor_padding_x(&self, padding: f64) {
        self.mutate_global_and_save(|state| {
            state.global_settings.editor_padding_x =
                Some(js_max(0.0, js_min(3.0, js_floor(padding))));
            state.global_settings.extra.shift_remove("editorPaddingX");
            mark_modified(state, "editorPaddingX", None);
        });
    }

    /// `getOutputPad()`：`0` 原样，其余 `1`。
    pub fn get_output_pad(&self) -> i64 {
        self.with_settings(|settings| {
            if settings.output_pad == Some(0.0) {
                0
            } else {
                SETTINGS_DEFAULTS.output_pad as i64
            }
        })
    }

    /// `setOutputPad(padding)`（`0` / `1`）。
    pub fn set_output_pad(&self, padding: i64) {
        self.mutate_global_and_save(|state| {
            state.global_settings.output_pad = Some(padding as f64);
            state.global_settings.extra.shift_remove("outputPad");
            mark_modified(state, "outputPad", None);
        });
    }

    /// `getAutocompleteMaxVisible()`。
    pub fn get_autocomplete_max_visible(&self) -> f64 {
        self.with_settings(|settings| {
            number_setting(
                settings,
                "autocompleteMaxVisible",
                settings.autocomplete_max_visible,
                SETTINGS_DEFAULTS.autocomplete_max_visible,
            )
        })
    }

    /// `setAutocompleteMaxVisible(maxVisible)`：写 `max(3, min(20, floor))`。
    pub fn set_autocomplete_max_visible(&self, max_visible: f64) {
        self.mutate_global_and_save(|state| {
            state.global_settings.autocomplete_max_visible =
                Some(js_max(3.0, js_min(20.0, js_floor(max_visible))));
            state
                .global_settings
                .extra
                .shift_remove("autocompleteMaxVisible");
            mark_modified(state, "autocompleteMaxVisible", None);
        });
    }

    /// `getCodeBlockIndent()`。
    pub fn get_code_block_indent(&self) -> String {
        self.with_settings(|settings| {
            settings
                .markdown
                .as_ref()
                .and_then(|markdown| markdown.code_block_indent.clone())
                .unwrap_or_else(|| SETTINGS_DEFAULTS.markdown.code_block_indent.to_string())
        })
    }

    /// `getMermaidRenderingMode()`：`"off"` / `"final"` 原样，其余 `"streaming"`。
    pub fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        self.with_settings(|settings| {
            match settings
                .markdown
                .as_ref()
                .and_then(|markdown| markdown.mermaid)
            {
                Some(MermaidRenderingMode::Off) => MermaidRenderingMode::Off,
                Some(MermaidRenderingMode::Final) => MermaidRenderingMode::Final,
                _ => SETTINGS_DEFAULTS.markdown.mermaid,
            }
        })
    }

    /// `setMermaidRenderingMode(mode)`。
    ///
    /// TS 用 `markdown ??= {}`（只替换 `null`/缺失），因此 falsy 的原始值也不会被替换。
    ///
    /// # Errors
    ///
    /// `markdown` 是任何原始值（数字/字符串/布尔，含 `0`、`""`、`false`）时返回
    /// `Cannot create property 'mermaid' on …`（TS 严格模式的 `TypeError`），内存与磁盘不变。
    pub fn set_mermaid_rendering_mode(
        &self,
        mode: MermaidRenderingMode,
    ) -> Result<(), SettingsValueError> {
        self.mutate_global_nested_and_save("markdown", "mermaid", true, |state| {
            state
                .global_settings
                .markdown
                .get_or_insert_with(MarkdownSettings::default)
                .mermaid = Some(mode);
            state.global_settings.extra.shift_remove("markdown");
            mark_modified(state, "markdown", Some("mermaid"));
        })
    }

    /// `getWarnings()`（浅拷贝）。
    pub fn get_warnings(&self) -> WarningSettings {
        self.with_settings(|settings| settings.warnings.clone().unwrap_or_default())
    }

    /// `setWarnings(warnings)`。
    pub fn set_warnings(&self, warnings: WarningSettings) {
        self.mutate_global_and_save(|state| {
            state.global_settings.warnings = Some(warnings);
            state.global_settings.extra.shift_remove("warnings");
            mark_modified(state, "warnings", None);
        });
    }
}

/// `getBranchSummarySettings()` 的返回值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedBranchSummarySettings {
    /// `reserveTokens`。
    pub reserve_tokens: f64,
    /// `skipPrompt`。
    pub skip_prompt: bool,
}

/// `getRetrySettings()` 的返回值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedRetrySettings {
    /// `enabled`。
    pub enabled: bool,
    /// `maxRetries`。
    pub max_retries: f64,
    /// `baseDelayMs`。
    pub base_delay_ms: f64,
    /// `maxAgentDelayMs`。
    pub max_agent_delay_ms: f64,
}

/// `getProviderRetrySettings()` 的返回值（前两项没有默认值）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedProviderRetrySettings {
    /// `timeoutMs`。
    pub timeout_ms: Option<f64>,
    /// `maxRetries`。
    pub max_retries: Option<f64>,
    /// `maxRetryDelayMs`。
    pub max_retry_delay_ms: f64,
}

// ---------------------------------------------------------------------------
// 加载
// ---------------------------------------------------------------------------

/// `loadFromStorage` / `tryLoadFromStorage`：读取并迁移一个作用域。
///
/// 阶段：
/// 1. 项目未受信 → 直接返回空设置，不读文件，也不产生错误；
/// 2. 借 `storage.with_lock` 以只读方式取当前内容（回调返回 `Ok(None)`，不会写回）；
///    文件不存在或内容为空串 → 空设置；`with_lock` 本身失败（加锁/读文件）→ `Err`；
/// 3. 去 BOM、`JSON.parse`、`migrate_settings`；任一步失败 → `Err`，错误处理（记录还是
///    保留旧值）由调用方决定；
/// 4. 顶层是 JSON 数组时先展开成下标键对象，再宽松反序列化成 [`Settings`]：TS 的
///    `deepMergeSettings` 对顶层值做 `{...base}`，数组会变成 `{"0":…}` 这样的对象。
///
/// 本函数**不**记录错误，也不修改 manager 状态。
fn try_load_from_storage(
    storage: &dyn SettingsStorage,
    scope: SettingsScope,
    project_trusted: bool,
) -> Result<Settings, SettingsValueError> {
    if scope == SettingsScope::Project && !project_trusted {
        return Ok(Settings::default());
    }

    let mut content: Option<String> = None;
    storage
        .with_lock(scope, &mut |current| {
            content = current.map(str::to_string);
            Ok(None)
        })
        .map_err(|error| SettingsValueError::new(error.to_string()))?;

    let Some(content) = content else {
        return Ok(Settings::default());
    };
    if content.is_empty() {
        return Ok(Settings::default());
    }

    let mut value = kiana_json::js_parse::parse(&strip_bom(&content))
        .map_err(|error| SettingsValueError::new(error.message))?;
    migrate_settings(&mut value).map_err(|error| SettingsValueError::new(error.message))?;
    // TS 的 `deepMergeSettings` 对顶层值做 `{...base}`：顶层是数组时得到下标键对象
    // （`[1,2]` → `{"0":1,"1":2}`）。[`Settings`] 的宽松反序列化对数组返回空对象，
    // 因此在进入类型化视图前先做同样的展开。
    let value = if value.is_array() {
        Value::Object(spread_object(value))
    } else {
        value
    };
    serde_json::from_value::<Settings>(value)
        .map_err(|error| SettingsValueError::new(error.to_string()))
}

// ---------------------------------------------------------------------------
// 原始值回落（类型与 schema 不符的已知设置）
// ---------------------------------------------------------------------------
//
// TS 的 getter 直接在 `JSON.parse` 出来的对象上取属性：已知字段的值即使类型不对
// （例如 `compaction.enabled: 0`、`extensions: ["/a", 3]`），getter 仍返回该值，`??`
// 只在 `null`/`undefined` 时回落默认值。Rust 的类型化字段容纳不了这些值，它们的原始
// JSON 保留在 [`Settings::extra`] 里；下面这组 helper 让 getter 在类型化字段缺失时读
// 原始值，并按 JS 语义（`Boolean(value)` / `Number(value)`）解释。

/// `Boolean(value)`。
fn js_truthy(value: &Value) -> bool {
    kiana_json::js_string::is_truthy(Some(value))
}

/// `settings[key][nestedKey]` 的原始值：`key` 不是 JSON 对象时视为不存在（可选链）。
fn nested_raw(settings: &Settings, key: &str, nested_key: &str) -> Option<Value> {
    nested_raw_at(settings, &[key, nested_key])
}

/// TS 的嵌套 setter 在严格模式下给 `settings[parentKey][propertyKey]` 赋值时，父值不可
/// 赋值所抛出的 `TypeError` 的等价物（`None` 表示可赋值）。
///
/// TS 有两种初始化父对象的写法，决定了哪些原始值会被替换成 `{}`：
/// - `if (!settings[key]) settings[key] = {}`（`nullish_replace = false`）：`null`/缺失
///   以及 **falsy** 原始值（`0`、`""`、`false`）都会被替换成 `{}`，可赋值；
/// - `settings[key] ??= {}`（`nullish_replace = true`）：只有 `null`/缺失会被替换，其它
///   原始值（包括 falsy 的 `0`、`""`、`false`）都会在下一行赋值时抛错。
///
/// 对象和数组是合法的赋值目标（数组的命名属性会被 `JSON.stringify` 忽略）。
///
/// 判定读的是 `settings.raw(parent_key)`：已解析的字段会重新序列化成对象，所以「父对象
/// 已成功解析」时永远可赋值；只有原始值仍是标量且按上述初始化规则不会被替换时才报错。
fn nested_parent_assignment_error(
    settings: &Settings,
    parent_key: &str,
    property_key: &str,
    nullish_replace: bool,
) -> Option<SettingsValueError> {
    let raw = settings.raw(parent_key)?;
    match raw {
        Value::Null | Value::Object(_) | Value::Array(_) => None,
        raw => {
            if !nullish_replace && !js_truthy(&raw) {
                return None;
            }
            // V8: `Cannot create property 'showImages' on number '42'`（`typeof` + `String(v)`）。
            Some(SettingsValueError::new(format!(
                "Cannot create property '{property_key}' on {} '{}'",
                kiana_json::js_string::js_typeof(&raw).as_str(),
                js_string_value(&raw)
            )))
        }
    }
}

/// 沿 `path` 逐层取属性（每层都必须是 JSON 对象，对应 TS 的可选链）。
fn nested_raw_at(settings: &Settings, path: &[&str]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    let mut current = settings.raw(first)?;
    for key in rest {
        match current {
            Value::Object(object) => current = object.get(*key)?.clone(),
            _ => return None,
        }
    }
    Some(current)
}

/// `settings[key] ?? default`：类型化值缺失时用原始值做 JS truthiness。
fn boolean_setting(settings: &Settings, key: &str, typed: Option<bool>, default: bool) -> bool {
    if let Some(value) = typed {
        return value;
    }
    match settings.raw(key) {
        None | Some(Value::Null) => default,
        Some(raw) => js_truthy(&raw),
    }
}

/// `settings[path...][key] ?? default`。
fn nested_boolean_setting(
    settings: &Settings,
    path: &[&str],
    key: &str,
    typed: Option<bool>,
    default: bool,
) -> bool {
    if let Some(value) = typed {
        return value;
    }
    let mut full_path = path.to_vec();
    full_path.push(key);
    match nested_raw_at(settings, &full_path) {
        None | Some(Value::Null) => default,
        Some(raw) => js_truthy(&raw),
    }
}

/// `Number(value)`（ECMAScript `ToNumber`）。
///
/// TS 的 getter 返回原始 JSON 值；Rust 的类型化 getter 只能返回 `f64`，因此按 `ToNumber`
/// 转成消费方做算术时得到的同一个数字（`[1]` → 1、`[]` → 0、`{}` → `NaN`、`"4"` → 4）。
fn js_number_coerce(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::parse_js_number(text),
        Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Null => 0.0,
        // `ToPrimitive(array)` = `join(",")`（null/undefined 元素变成空串）。
        Value::Array(items) => {
            let joined = items
                .iter()
                .map(|item| match item {
                    Value::Null => String::new(),
                    other => js_string_value(other),
                })
                .collect::<Vec<_>>()
                .join(",");
            kiana_json::parse_js_number(&joined)
        }
        // `ToPrimitive(object)` = `"[object Object]"` → NaN。
        Value::Object(_) => f64::NAN,
    }
}

/// `settings[key] ?? default`（数值语境）。
fn number_setting(settings: &Settings, key: &str, typed: Option<f64>, default: f64) -> f64 {
    if let Some(value) = typed {
        return value;
    }
    match settings.raw(key) {
        None | Some(Value::Null) => default,
        Some(raw) => js_number_coerce(&raw),
    }
}

/// `settings[path...][key] ?? default`（数值语境）。
fn nested_number_setting(
    settings: &Settings,
    path: &[&str],
    key: &str,
    typed: Option<f64>,
    default: f64,
) -> f64 {
    if let Some(value) = typed {
        return value;
    }
    let mut full_path = path.to_vec();
    full_path.push(key);
    match nested_raw_at(settings, &full_path) {
        None | Some(Value::Null) => default,
        Some(raw) => js_number_coerce(&raw),
    }
}

/// `settings[path...][key]`（数值语境，没有默认值）。
fn optional_nested_number_setting(
    settings: &Settings,
    path: &[&str],
    key: &str,
    typed: Option<f64>,
) -> Option<f64> {
    if typed.is_some() {
        return typed;
    }
    let mut full_path = path.to_vec();
    full_path.push(key);
    match nested_raw_at(settings, &full_path) {
        None | Some(Value::Null) => None,
        Some(raw) => Some(js_number_coerce(&raw)),
    }
}

/// 字符串数组：类型化字段缺失时按元素逐个解析原始数组（保留合法元素）。
///
/// TS 是 `[...(settings[key] ?? [])]`：数组展开保留原始元素（含非字符串，Rust 的
/// `Vec<String>` 表示不了）；字符串按**码点**展开（与数组展开的字符串迭代器一致）。
fn string_array_setting(
    settings: &Settings,
    key: &str,
    typed: &Option<Vec<String>>,
) -> Vec<String> {
    if let Some(values) = typed {
        return values.clone();
    }
    match settings.raw(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(text)) => text
            .chars()
            .map(|character| character.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// `string[] | undefined` 的同款回落（`getEnabledModels`）。
///
/// TS 的 `getEnabledModels()` 直接返回原始值（不校验、也不展开）：非数组值（字符串/数字）
/// 会原样返回。Rust 的 `Option<Vec<String>>` 表示不了，非数组一律 `None`（类型级偏差）。
fn optional_string_array_setting(
    settings: &Settings,
    key: &str,
    typed: &Option<Vec<String>>,
) -> Option<Vec<String>> {
    if let Some(values) = typed {
        return Some(values.clone());
    }
    match settings.raw(key) {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// `getNpmCommand()` 的回落：TS 是 `command ? [...command] : undefined`。
///
/// `??` 不适用——这里用的是 **truthiness**：空串 / `0` / `false` / `null` 都得到
/// `undefined`；非空字符串按码点展开（`[...\"npm\"]` → `[\"n\",\"p\",\"m\"]`）。
fn npm_command_setting(settings: &Settings) -> Option<Vec<String>> {
    if let Some(values) = &settings.npm_command {
        return Some(values.clone());
    }
    match settings.raw("npmCommand") {
        None | Some(Value::Null) => None,
        Some(raw) if !js_truthy(&raw) => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        Some(Value::String(text)) => Some(
            text.chars()
                .map(|character| character.to_string())
                .collect(),
        ),
        // 其它可迭代不了的值在 TS 里抛 `TypeError: ... is not iterable`；Rust 侧无对应初值。
        Some(_) => None,
    }
}

/// `PackageSource[]` 的回落：逐个元素反序列化，非法元素丢弃；字符串按码点展开。
fn package_source_array_setting(
    settings: &Settings,
    key: &str,
    typed: &Option<Vec<PackageSource>>,
) -> Vec<PackageSource> {
    if let Some(values) = typed {
        return values.clone();
    }
    match settings.raw(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| serde_json::from_value(item.clone()).ok())
            .collect(),
        Some(Value::String(text)) => text
            .chars()
            .map(|character| PackageSource::Text(character.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// 字符串设置的真值语义（`if (!settings[key])`）：空串视为缺失。
fn truthy_string_setting(settings: &Settings, key: &str, typed: &Option<String>) -> Option<String> {
    match typed {
        Some(value) if !value.is_empty() => Some(value.clone()),
        Some(_) => None,
        None => match settings.raw(key) {
            Some(raw) if js_truthy(&raw) => Some(js_string_value(&raw)),
            _ => None,
        },
    }
}

/// `parseTimeoutSetting(value, settingName)`。
///
/// 先看类型化字段：`disabled` 映射成 `0`；`millis` 必须是有限非负数，否则生成逐字错误
/// `Invalid <name> setting: <String(value)>`。类型化字段缺失时读原始 JSON，解释规则复用
/// `http-dispatcher` 的 `parse_http_idle_timeout_ms`（字符串 `"disabled"`/数字等）。
/// 返回 `Ok(None)` 表示完全未设置：HTTP 空闲超时的调用方回落到
/// `DEFAULT_HTTP_IDLE_TIMEOUT_MS`，WebSocket 的调用方直接把它当 `Option` 返回。
///
/// `setting_name` 目前只允许 `"httpIdleTimeoutMs"` 与 `"websocketConnectTimeoutMs"`；
/// 映射用「非前者即后者」，新增设置项时必须同步修改这里。
fn timeout_setting(
    settings: &Settings,
    setting_name: &str,
) -> Result<Option<f64>, SettingsValueError> {
    let typed = match setting_name {
        "httpIdleTimeoutMs" => settings.http_idle_timeout_ms,
        _ => settings.websocket_connect_timeout_ms,
    };
    if let Some(typed) = typed {
        match typed {
            TimeoutSetting::Disabled => return Ok(Some(0.0)),
            TimeoutSetting::Millis(value) => {
                if value.is_finite() && value >= 0.0 {
                    return Ok(Some(js_floor(value)));
                }
                return Err(invalid_setting_error(
                    setting_name,
                    &js_string_number(value),
                ));
            }
        }
    }
    match settings.raw(setting_name) {
        Some(raw) => match parse_http_idle_timeout_ms(&raw) {
            Some(timeout_ms) => Ok(Some(timeout_ms)),
            None => Err(invalid_setting_error(setting_name, &js_string_value(&raw))),
        },
        None => Ok(None),
    }
}
