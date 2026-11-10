//! `registry.ts`：注册表与注册表视图。
//!
//! 注册表是 `lifetime "host"` 的对象把自己的条目登记进去的地方；表格按 code version 安装，因此
//! reload 期间新旧两个版本的消费者各看到自己的表。
//!
//! 可观察顺序（spec/chord-rest.md §2.8 与 chord-core §5.3）：表按 **first-registration order** 排序
//! 并跨 reload 保留（`HostImpl.#order`）；`subscribe` 的首次回调是**同步**的。
//!
//! # 边界
//!
//! 本模块只定义注册表与视图的公共形状，不负责“何时安装新表”。真正的表安装（排序、跨 reload
//! 保留旧版本）由 host 的 `tables` 实现（TS 的 `HostImpl.registry`；Rust 的 `host` 模块尚未
//! 移植）。这里的 [`RegistryTable`] 用 `IndexMap`，只保证**写入顺序**被保留，不主动排序。
//!
//! # 注意
//!
//! - [`RegistryView::table`] 按值返回 [`RegistryTable`]（克隆），调用方改它不会影响视图。
//! - [`RegistryView::subscribe`] 的首次回调是同步的；返回的取消函数是 `FnOnce`，丢弃未调用
//!   的取消函数等于订阅永不取消。
//! - [`RegistryTable`] 的值要求 `Clone`：每次 `table()` 都克隆整张表（代价与表大小成正比），
//!   需要多次读取的调用方应自己缓存结果。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::types::Ctx;

/// 注册表（TS: `Registry<V>`）。
///
/// `conflict: "error"` 拒绝两个对象注册同一个 key；`"last"` 保留后注册者。
///
/// TS 的 `Registry<V>` 带一个只用于类型承载的 `value?: V`（注释明确“never set”）；Rust 版本
/// 不保留泛型参数，值的类型由 [`RegistryTable`] / [`RegistryView`] 各自决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// 注册表名。
    pub name: String,
    /// 冲突策略。
    pub conflict: ConflictPolicy,
}

/// `Registry.conflict`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConflictPolicy {
    /// `"error"`（默认）：两个对象注册同一个 key 即 `invalid/registry_conflict`。
    #[default]
    Error,
    /// `"last"`：保留后注册者。
    Last,
}

impl ConflictPolicy {
    /// 与 TS 字符串字面量一致。
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictPolicy::Error => "error",
            ConflictPolicy::Last => "last",
        }
    }
}

/// `defineRegistry(name, options)` 的选项。
#[derive(Debug, Clone, Default)]
pub struct RegistryOptions {
    /// `conflict?: "error" | "last"`；缺省 `"error"`。
    pub conflict: Option<ConflictPolicy>,
}

impl RegistryOptions {
    /// 空选项（`conflict` 默认 `"error"`）。
    pub fn new() -> Self {
        RegistryOptions::default()
    }

    /// `conflict: "last"`。
    pub fn last() -> Self {
        RegistryOptions {
            conflict: Some(ConflictPolicy::Last),
        }
    }
}

/// `defineRegistry(name, options = {})`。
///
/// 返回 `{ name, conflict: options.conflict ?? "error" }`。
pub fn define_registry(name: impl Into<String>, options: RegistryOptions) -> Registry {
    Registry {
        name: name.into(),
        conflict: options.conflict.unwrap_or_default(),
    }
}

/// `defineRegistry(name)`（无选项）。
pub fn registry(name: impl Into<String>) -> Registry {
    define_registry(name, RegistryOptions::default())
}

/// 注册表表格（TS: `RegistryTable<V> = ReadonlyMap<string, V>`）。
///
/// `IndexMap` 保持插入序；插入顺序由 host 的 first-registration order 决定。TS 的
/// `ReadonlyMap` 只是类型层只读，Rust 侧 `IndexMap` 本身可写——视图返回的是克隆，写它不会
/// 影响宿主里的表。
///
/// 默认值类型是 `serde_json::Value`，方便只关心 key 集合的测试。
pub type RegistryTable<V = serde_json::Value> = IndexMap<String, V>;

/// `RegistryView.subscribe` 的监听器（`(table, codeVersion) => void`）。
///
/// 表以**借用**传入：回调期间有效，回调返回后不再可用（需要保留要自己 clone）。要求
/// `Send + Sync`，因为视图可能从其它线程安装新表并调用它。
pub type TableListener<V = serde_json::Value> =
    Box<dyn Fn(&RegistryTable<V>, i64) + Send + Sync + 'static>;

/// 注册表视图（TS: `RegistryView<V>`）。
///
/// 具体实现由 `host::tables` 提供（`HostImpl::registry`）。
pub trait RegistryView<V: Clone + Send + Sync + 'static>: Send + Sync {
    /// `ctx.codeVersion` 的表；没有 ctx 时用当前 code version。
    ///
    /// code version 已不在保留期 → `superseded` `"code version ${id} is retired"`。返回的是
    /// 克隆，调用方可以自由修改。
    fn table(&self, ctx: Option<&dyn Ctx>) -> Result<RegistryTable<V>, crate::errors::ChordError>;

    /// `table(ctx).get(key)`。
    fn get(
        &self,
        key: &str,
        ctx: Option<&dyn Ctx>,
    ) -> Result<Option<V>, crate::errors::ChordError> {
        Ok(self.table(ctx)?.get(key).cloned())
    }

    /// **立即**用当前表与当前 code version 回调一次，之后每次安装新表都回调。
    ///
    /// 首次回调发生在 `subscribe` 返回之前（同步），因此调用方无需自己先读一次表。返回的
    /// `FnOnce` 是取消订阅；trait 没有规定实现何时移除监听器，契约是取消后不再**发起新的**
    /// 回调——已被某次 `install` 克隆、或已在途的一次回调仍可能执行。
    fn subscribe(&self, listener: TableListener<V>) -> Box<dyn FnOnce() + Send + 'static>;
}

/// 一次注册（TS: `Registration`）：对象在它的一次运行里登记的条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Registration {
    /// 注册表。
    pub registry: Registry,
    /// 条目 key。
    pub key: String,
    /// 条目值。
    pub value: serde_json::Value,
}

impl Registration {
    /// 构造。
    pub fn new(registry: Registry, key: impl Into<String>, value: serde_json::Value) -> Self {
        Registration {
            registry,
            key: key.into(),
            value,
        }
    }

    /// 对象内部去重用的 id：`${registry.name}\u0000${key}`（`self.register` 的 `#unique` 键）。
    ///
    /// 只用于同一对象内部的判重，不是线上格式；分隔符 NUL 由单元测试
    /// `registration_id_uses_nul_separator` 锁定。
    pub fn id(&self) -> String {
        format!("{}\u{0000}{}", self.registry.name, self.key)
    }
}

/// 一个不需要宿主的注册表视图（测试与小工具用；真实宿主用 `host::tables`）。
///
/// TS 没有对应物：TS 的视图总是由 `HostImpl.registry` 产生。这里只实现同一套可观察语义
/// （`table`/`get`/`subscribe` 的立即回调 + 安装新表后的回调），便于不启动宿主就测注册表行为。
/// 它只有一张表，`table(ctx)` 忽略 `ctx`（没有旧 code version 的保留期，也就不会返回
/// `superseded`）。
///
/// # 锁与重入
///
/// - [`StaticRegistryView::install`]：锁内换表并克隆订阅者列表，锁外逐个回调。因此回调里
///   调用 `table()`/`install()` 不会死锁；取消函数只把监听器从列表移除，所以取消后不再发起
///   新的回调，但已被本次 `install` 克隆（或已在途）的监听器仍会收到本次回调。
/// - 它实现的 [`RegistryView::subscribe`] 的**首次**回调在持有 `state` 锁时执行。监听器若在
///   首次回调里再调用 `table()`、`get()`、`subscribe()` 或 `install()`，会因
///   `std::sync::Mutex` 不可重入而自我死锁。
pub struct StaticRegistryView<V: Clone + Send + Sync + 'static> {
    state: Arc<std::sync::Mutex<StaticState<V>>>,
    subscribers: Arc<std::sync::Mutex<Vec<SharedTableListener<V>>>>,
}

/// 可共享的 `TableListener`（`subscribe` 把它存起来，并在每次 `install` 时重复调用）。
type SharedTableListener<V> = Arc<dyn Fn(&RegistryTable<V>, i64) + Send + Sync>;

struct StaticState<V> {
    table: RegistryTable<V>,
    code_version: i64,
}

impl<V: Clone + Send + Sync + 'static> StaticRegistryView<V> {
    /// 用一张表与一个 code version 建视图。
    pub fn new(table: RegistryTable<V>, code_version: i64) -> Self {
        StaticRegistryView {
            state: Arc::new(std::sync::Mutex::new(StaticState {
                table,
                code_version,
            })),
            subscribers: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 装上另一张表（模拟 reload 的 `installTables`）：先换表，再按注册顺序回调每个监听器。
    ///
    /// 回调在锁外执行（监听器可能重入 `table()`）。每个监听器收到的是同一份克隆后的
    /// `(table, code_version)`；监听器按 `subscribe` 的先后顺序被调用。
    pub fn install(&self, table: RegistryTable<V>, code_version: i64) {
        let (table, code_version, listeners) = {
            let mut state = self.state.lock().expect("registry state lock");
            state.table = table;
            state.code_version = code_version;
            let listeners = self
                .subscribers
                .lock()
                .expect("registry subscribers lock")
                .clone();
            (state.table.clone(), state.code_version, listeners)
        };
        for listener in listeners {
            listener(&table, code_version);
        }
    }
}

impl<V: Clone + Send + Sync + 'static> RegistryView<V> for StaticRegistryView<V> {
    fn table(&self, _ctx: Option<&dyn Ctx>) -> Result<RegistryTable<V>, crate::errors::ChordError> {
        Ok(self
            .state
            .lock()
            .expect("registry state lock")
            .table
            .clone())
    }

    fn subscribe(&self, listener: TableListener<V>) -> Box<dyn FnOnce() + Send + 'static> {
        let slot: SharedTableListener<V> = Arc::from(listener);
        self.subscribers
            .lock()
            .expect("registry subscribers lock")
            .push(Arc::clone(&slot));
        {
            let state = self.state.lock().expect("registry state lock");
            // 首次回调在 state 锁内执行（见类型文档的“锁与重入”）：监听器若在这里调用
            // table()/get()/install()/subscribe() 会自我死锁。
            slot(&state.table, state.code_version);
        }
        let subscribers = Arc::clone(&self.subscribers);
        Box::new(move || {
            let mut list = subscribers.lock().expect("registry subscribers lock");
            list.retain(|entry| !Arc::ptr_eq(entry, &slot));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn define_registry_defaults_to_error() {
        let registry = define_registry("tools", RegistryOptions::new());
        assert_eq!(registry.name, "tools");
        assert_eq!(registry.conflict, ConflictPolicy::Error);
        assert_eq!(registry.conflict.as_str(), "error");

        let registry = define_registry("tools", RegistryOptions::last());
        assert_eq!(registry.conflict, ConflictPolicy::Last);
        assert_eq!(registry.conflict.as_str(), "last");
    }

    #[test]
    fn registration_id_uses_nul_separator() {
        let registration = Registration::new(registry("r"), "k", json!(1));
        assert_eq!(registration.id(), "r\u{0000}k");
    }

    #[test]
    fn static_view_calls_back_immediately() {
        let mut table: RegistryTable = IndexMap::new();
        table.insert("a".into(), json!(1));
        let view = StaticRegistryView::new(table, 3);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        let _cancel = view.subscribe(Box::new(move |table, code_version| {
            s.lock().unwrap().push((table.len(), code_version));
        }));
        assert_eq!(*seen.lock().unwrap(), vec![(1, 3)]);
        assert_eq!(view.get("a", None).unwrap(), Some(json!(1)));
        assert_eq!(view.get("b", None).unwrap(), None);
    }
}
