//! 扩展工厂缓存：键 `(resolvedPath, cwd, generation)`。
//!
//! TS 源：`packages/coding-agent/src/core/extensions/loader.ts` 的模块级
//! `extensionCache` / `extensionCacheCwd` / `extensionCacheGeneration` 与
//! `clearExtensionCache` / `useExtensionCacheCwd` / `isCurrentCacheToken`。
//! 规范：`spec/ca-extensions.md` §2.2.2。
//!
//! # 语义
//!
//! - 缓存键是**已 resolve 的扩展路径**；值是**工厂**。命中时跳过模块执行（不重跑模块体），
//!   但工厂体仍会重跑（`initializeExtension` 每次调用工厂）。
//! - `clear_extension_cache()` 清空并把 generation +1，先前的 token 全部失效
//!   （`extensionCacheCwd` 同时清空）。
//! - `use_extension_cache_cwd(cwd)` 先 resolve cwd；与已记录的 cwd 不同则先清缓存，然后记录
//!   当前 cwd 并返回 `{cwd, generation}` token。
//! - `is_current_cache_token` 同时比较 cwd 与 generation。
//! - **不缓存 `NULL`**：模块没有有效默认导出时调用方不会写入缓存。
//! - 缓存只在 `loadExtensionsCached` 中启用；`loadExtensions` / `discoverAndLoadExtensions`
//!   不传 token，`get`/`put` 直接是 no-op。
//!
//! # Rust 形态
//!
//! TS 的模块级状态在 JS 里是进程全局；`kiana-extensions` 必须跑在单线程 `LocalSet` 上
//! （`ARCHITECTURE.md` §4.2），因此状态放在 `thread_local!`。这也让并行测试天然隔离。
//! 值用 `Rc<dyn Any>` 存（JS 工厂是 `rquickjs::Persistent<Function>`，`!Send`，不能进
//! `static`）；调用方用 [`get_cached`] 的泛型参数取回具体类型。
//!
//! # 调用方与生命周期
//!
//! - 读写：`loader::load_extension_module`（`loader/mod.rs`）在缓存未命中时 [`put_cached`]、
//!   命中时 [`get_cached`]；只有 `load_extensions_cached` 会创建 token，`load_extensions` /
//!   `discover_and_load_extensions` 传 `None`，因此完全不碰缓存。
//! - 清空：[`clear_extension_cache`] 由 `crate::Generation::with_config`（新引擎装配前）与
//!   `kiana-resources` 的 `reload()` 调用；cwd 变化由 [`use_extension_cache_cwd`] 内部清空。
//! - 缓存值持有 JS 回调的持久槽：`loader/mod.rs` 的 `CachedJsFactory::drop` 会在条目释放时
//!   移除它。因此清空必须发生在**旧引擎仍是当前引擎**时（`Generation::with_config` 在
//!   `loader::install` 之前调用本模块的 `clear`），否则 Drop 会去新引擎的持久槽里释放旧 id。
//! - 每个线程只有一份状态：同一线程交替使用两个 cwd 调 [`use_extension_cache_cwd`] 会反复
//!   清空缓存（每次 cwd 变化 generation +1），不会同时保留两个 cwd 的条目。

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kiana_core::paths::{PathInputOptions, resolve_path};

/// 缓存代际 token。TS: `loader.ts#ExtensionCacheToken`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCacheToken {
    /// resolve 后的 cwd（`resolvePath(cwd)`）。
    pub cwd: String,
    /// 取 token 时的 generation。
    pub generation: u64,
}

#[derive(Default)]
struct FactoryCacheState {
    cwd: Option<String>,
    generation: u64,
    entries: HashMap<String, Rc<dyn Any>>,
}

thread_local! {
    /// 与 TS 模块级变量一一对应的单线程状态。
    static CACHE: RefCell<FactoryCacheState> = RefCell::new(FactoryCacheState::default());
}

/// 清空工厂缓存：清条目、清 cwd、generation +1（此前所有 token 失效）。
/// TS: `loader.ts#clearExtensionCache`。
///
/// 只清本线程的 thread-local 状态；条目被移除时立即 `Drop`（缓存值因此释放它持有的 JS
/// 持久槽，见模块文档的调用顺序约束）。`generation` 用 `wrapping_add`，溢出时回绕。
pub fn clear_extension_cache() {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.entries.clear();
        cache.cwd = None;
        cache.generation = cache.generation.wrapping_add(1);
    });
}

/// `resolvePath(cwd)`：基准是 `process.cwd()`，默认选项（`~` 展开、其它规范化关闭）。
///
/// `std::env::current_dir()` 失败时不报错，基准退化为空串；因此本函数在异常环境下仍返回
/// 字符串（`resolve_path` 不会替调用方报告 cwd 不可用）。
fn resolve_cwd(cwd: &str) -> String {
    let process_cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    resolve_path(cwd, &process_cwd, &PathInputOptions::default())
}

/// 记录本轮缓存的 cwd 并返回 token；cwd 变化时先 [`clear_extension_cache`]。
/// TS: `loader.ts#useExtensionCacheCwd`。
///
/// 首次调用（尚无记录的 cwd）不清条目也不加 generation；同一 cwd 重复调用返回相同
/// generation，先前 token 仍然有效。只有“已记录 cwd 且不同”时才执行清空分支。
/// token 里的 `cwd` 是 resolve 后的值，调用方应直接用它作为后续路径解析基准
/// （`load_extensions_internal` 就是这么做的），否则缓存作用域与实际加载用的 cwd 会不一致。
pub fn use_extension_cache_cwd(cwd: &str) -> ExtensionCacheToken {
    let resolved_cwd = resolve_cwd(cwd);
    let generation = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.cwd.as_deref().is_some_and(|old| old != resolved_cwd) {
            cache.entries.clear();
            cache.cwd = None;
            cache.generation = cache.generation.wrapping_add(1);
        }
        cache.cwd = Some(resolved_cwd.clone());
        cache.generation
    });
    ExtensionCacheToken {
        cwd: resolved_cwd,
        generation,
    }
}

/// token 是否仍对应当前缓存（cwd 与 generation 都相等）。
/// TS: `loader.ts#isCurrentCacheToken`。
///
/// `None` token 恒为 `false`（未启用缓存的加载入口因此天然不命中）。比较是字符串逐字节相等，
/// 不做路径归一化：token 里的 cwd 在生成时已经 resolve。
pub fn is_current_cache_token(token: Option<&ExtensionCacheToken>) -> bool {
    let Some(token) = token else {
        return false;
    };
    CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.cwd.as_deref() == Some(token.cwd.as_str()) && cache.generation == token.generation
    })
}

/// 读取缓存工厂；token 无效时返回 `None`。
///
/// `T` 必须是写入时的具体类型（`Rc<dyn Any>::downcast` 失败按未命中处理）。
/// 未命中（无此路径或类型不符）不会移除已有条目：类型不符的条目仍留在缓存里，直到被覆盖
/// 或清空。
/// TS: `loadExtensionModule` 的 `extensionCache.get(extensionPath)` 分支。
pub fn get_cached<T: Any>(token: Option<&ExtensionCacheToken>, path: &str) -> Option<Rc<T>> {
    if !is_current_cache_token(token) {
        return None;
    }
    CACHE.with(|cache| {
        cache
            .borrow()
            .entries
            .get(path)
            .and_then(|value| Rc::clone(value).downcast::<T>().ok())
    })
}

/// 写入缓存工厂；token 无效或已存在同路径条目时按插入语义覆盖。
/// TS: `loadExtensionModule` 的 `extensionCache.set(extensionPath, factory)` 分支。
///
/// 同路径旧值会被 `HashMap::insert` 替换并立即 `Drop`——如果它持有 JS 持久槽，这次释放会
/// 发生在当前引擎上。当前调用方只在缓存未命中时写入，避免误释放仍在使用的缓存项。
pub fn put_cached<T: Any>(token: Option<&ExtensionCacheToken>, path: &str, factory: Rc<T>) {
    if !is_current_cache_token(token) {
        return;
    }
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entries
            .insert(path.to_string(), factory as Rc<dyn Any>);
    });
}

/// 当前缓存条目数（测试/诊断用）。
pub fn cached_len() -> usize {
    CACHE.with(|cache| cache.borrow().entries.len())
}

/// 当前 generation（测试/诊断用）。
pub fn cache_generation() -> u64 {
    CACHE.with(|cache| cache.borrow().generation)
}

/// 当前记录的 cwd（测试/诊断用；`None` 表示缓存已清空）。
pub fn cached_cwd() -> Option<String> {
    CACHE.with(|cache| cache.borrow().cwd.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试线程有独立的 thread-local 缓存，先清空再断言。
    fn reset() {
        clear_extension_cache();
    }

    #[test]
    fn clear_increments_generation_and_drops_tokens() {
        reset();
        let token = use_extension_cache_cwd("/tmp");
        assert!(is_current_cache_token(Some(&token)));
        clear_extension_cache();
        assert!(!is_current_cache_token(Some(&token)));
        assert_eq!(cached_len(), 0);
    }

    #[test]
    fn changing_cwd_clears_cached_entries() {
        reset();
        let first = use_extension_cache_cwd("/tmp/one");
        put_cached(Some(&first), "/x.ts", Rc::new(1_u32));
        assert_eq!(cached_len(), 1);
        let second = use_extension_cache_cwd("/tmp/two");
        assert!(!is_current_cache_token(Some(&first)));
        assert!(is_current_cache_token(Some(&second)));
        assert_eq!(cached_len(), 0);
    }

    #[test]
    fn put_and_get_are_noops_without_a_current_token() {
        reset();
        put_cached(None, "/x.ts", Rc::new(1_u32));
        assert_eq!(cached_len(), 0);
        assert!(get_cached::<u32>(None, "/x.ts").is_none());
    }

    #[test]
    fn get_roundtrips_the_written_value() {
        reset();
        let token = use_extension_cache_cwd("/tmp");
        put_cached(Some(&token), "/x.ts", Rc::new(7_u32));
        assert_eq!(
            *get_cached::<u32>(Some(&token), "/x.ts").expect("cached"),
            7
        );
        assert!(get_cached::<String>(Some(&token), "/x.ts").is_none());
        assert!(get_cached::<u32>(Some(&token), "/other.ts").is_none());
    }
}
