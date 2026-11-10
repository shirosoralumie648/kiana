//! `Persistent` 逆序关闭与代际重载（设计文档 §2.8；顺序错误会 abort 进程）。
//!
//! TS 对应物：无（Node 的 `vm` context 由 GC 回收）。设计文档 §2.8、§10.5。
//!
//! **硬约束**：任何一个 [`rquickjs::Persistent`] 活过 `Runtime` 会触发
//! `JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed` 并 **abort 进程**。
//! 所以 `Drop` 必须显式按以下顺序执行：
//!
//! 1. 清掉引擎持有的全部 `Persistent`（pending / handles / 通用槽 / 错误队列 /
//!    host kind handler / 完成队列）；
//! 2. 在 `with_js` 内泵空 job 队列（断开 JS 侧的引用链），**再清一遍**这些容器：
//!    job 里的未处理拒绝 / `__kiana.recordError` 会在这里生成新的 `Persistent`；
//!    `shutting_down` 标志同时让 rejection tracker 在关闭期间不再登记；
//! 3. `run_gc()` —— 必须在 `ctx.with` 之外（§2.2 第 3 条）；
//! 4. 字段按**声明顺序**析构：`ctx` 在 `rt` 之前（Rust 的字段析构顺序与声明顺序一致，
//!    不是设计文档 §2.8 注释里说的“逆序”）。
//!
//! 代际重载（§2.8）：整代销毁再建，新代的 `EngineConfig::generation` +1；
//! 旧代的 `kiana`/`ctx` 句柄由 `api.js` 的 `invalidate()` 置 `Failed` 并抛【逐字】stale 文案
//! （§4.6），引擎侧的 [`JsEngine::invalidate`] 只按 TS 语义记录 stale 文案（幂等、
//! 不打断在飞行中的宿主工作），以及模块名里的 `<gen>`
//! （[`JsEngine::scoped_module_name`]）。
//!
//! # 实现注记
//!
//! - `Drop` 里清理完成队列用的是 `try_lock`：拿不到锁也不影响正确性——`completions` 是
//!   [`JsEngine`] 的**第一个**字段，即使这里没清，字段析构也会在 `ctx`/`rt` 之前丢弃
//!   接收端与排队事件（字段顺序见 `engine/mod.rs`）。
//! - 第 0 步把 `deadline_ms` 置 0 只是清掉显式 deadline；随后 `with_js` 仍会安装默认
//!   看门狗（`limits::enter_watchdog` 在 previous 为 0 且 `EngineConfig::watchdog` 非 0
//!   时安装，默认 100 ms），所以第 2 步的清理泵本身受单次事务预算约束，不是无限时间。
//! - 第 1、2 步清空的 `timers` 不含 `Persistent`（JS 回调留在 JS 的 `Map` 里），清它只为
//!   避免跨代残留；`host_kinds` 清的是 `'static` handler 闭包（它们自身不持有 JS 值，
//!   但可以捕获其它 Rust 状态）。

use std::sync::atomic::Ordering;

use super::{EngineResult, JsEngine};

impl Drop for JsEngine {
    fn drop(&mut self) {
        // 0. 关闭期间不再接受中断（否则清理用的 job 泵会被立刻打断），并把 rejection
        //    tracker 静音：下面清空 `errors` 之后再泵 job 时，新的未处理拒绝不得再塞进
        //    一个 `Persistent`（它会在 `rt` 之后才析构 → `JS_FreeRuntime` abort）。
        self.shared.shutting_down.set(true);
        self.shared
            .interrupt
            .interrupted
            .store(false, Ordering::Relaxed);
        self.shared
            .interrupt
            .deadline_ms
            .store(0, Ordering::Relaxed);

        // 1. 清掉所有 Persistent。顺序无关紧要，但不能漏：
        //
        //    完成队列里排队的 `HostCompletion::Invoke` 持有 `Persistent<Function>`，
        //    这里先丢弃接收端（含队列），否则它们会活过 runtime（字段是第一个，见
        //    `engine/mod.rs` 的声明顺序说明）。`try_lock` 失败时由字段析构兜底。
        if let Ok(mut guard) = self.completions.try_lock() {
            *guard = None;
        }
        self.shared.pending.borrow_mut().clear();
        self.shared.handles.borrow_mut().clear();
        self.shared.persistents.borrow_mut().clear();
        self.shared.errors.borrow_mut().clear();
        self.shared.host_kinds.borrow_mut().clear();
        self.shared.timers.borrow_mut().clear();

        // 2. 泵空 job 队列，断开 JS 侧的引用链（此时 ctx 仍有效）。
        //
        //    job（例如 `Promise.resolve().then(() => { throw ... })`）会产生新的未处理
        //    拒绝；`__kiana.recordError` 也可能被调用。即使 tracker 已静音，宿主原语仍可能
        //    往这些容器里放 Persistent，所以第 3 步之前要再清一遍。
        self.with_js(|ctx| while ctx.execute_pending_job() {});
        self.shared.pending.borrow_mut().clear();
        self.shared.handles.borrow_mut().clear();
        self.shared.persistents.borrow_mut().clear();
        self.shared.errors.borrow_mut().clear();
        self.shared.host_kinds.borrow_mut().clear();
        self.shared.timers.borrow_mut().clear();

        // 3. GC 必须在 ctx.with 之外。
        self.rt.run_gc();

        // 4. 剩下的交给字段析构：completions → ctx → rt → shared
        //    （`JsEngine` 的字段声明顺序见 `engine/mod.rs`）。
    }
}

impl JsEngine {
    /// 引擎配置的只读引用（[`JsEngine::reload`] 以它派生下一代的配置）。
    pub fn config(&self) -> &super::EngineConfig {
        &self.shared.config
    }

    /// 当前代际号。模块名里的 `?v=<gen>-<seq>` 用它（设计文档 §2.8）。
    pub fn generation(&self) -> u64 {
        self.shared.config.generation
    }

    /// 生成带代际号的模块名：`<specifier>?v=<gen>-<seq>`（§3.4 的 `moduleCache:false` 等价）。
    ///
    /// `<seq>` 从 1 开始、每次调用 +1（`u64` 回绕），由本引擎独占；同一个 specifier
    /// 每次调用都会得到不同名字，所以模块缓存天然失效。同一个 specifier 在一次加载里应
    /// 只取一次名字（loader 负责缓存，见 `loader::load_extension_module`）。
    pub fn scoped_module_name(&self, specifier: &str) -> String {
        let seq = self.shared.module_seq.get().wrapping_add(1);
        self.shared.module_seq.set(seq);
        format!("{specifier}?v={}-{seq}", self.shared.config.generation)
    }

    /// prelude 是否已安装（crate-facade 的安装顺序见设计文档 §9.2）。
    pub fn is_installed(&self) -> bool {
        self.shared.installed.get()
    }

    /// 引擎直接持有的 `Persistent` 数量（泄漏测试用；不含宿主 handler 内部持有的）。
    ///
    /// 实现上是四个容器的**条目数**之和：`pending`（条目里没有 JS 值，只作计数）、
    /// `handles`、`persistents`、`errors`（一条错误最多带两个 `Persistent`）。它不含
    /// 完成队列里排队的 `HostCompletion::Invoke`，也不含 `host_kinds` handler 闭包内部
    /// 的持有；需要精确断言时以 `Drop` 不 abort 为准（`tests/engine_shutdown.rs`）。
    pub fn live_persistent_count(&self) -> usize {
        self.shared.pending.borrow().len()
            + self.shared.handles.borrow().len()
            + self.shared.persistents.borrow().len()
            + self.shared.errors.borrow().len()
    }

    /// 未完成的宿主 promise 数量。
    pub fn pending_count(&self) -> usize {
        self.shared.pending.borrow().len()
    }

    /// 失效当前代的旧 `kiana`/`ctx`（`/reload`、会话替换；设计文档 §2.8、§4.3）。
    ///
    /// 语义与 TS 的 `ExtensionRuntime.invalidate()`（`loader.ts:195`）/
    /// `ExtensionRunner.invalidate()`（`runner.ts:726`）逐条一致：
    ///
    /// - **幂等，首次生效**：已有 stale 文案时直接返回，后续文案被忽略；
    /// - 只记录 stale 文案（[`JsEngine::stale_message`]，`api.js` 的 `assertActive()` 用它）
    ///   并（在 JS 侧）退订事件总线——事件总线订阅归 `api.js` 管，引擎不持有；
    /// - **不** reject 已在飞行中的宿主 promise，也**不**清句柄/持久槽：TS 里
    ///   `kiana.exec`/`fetch`/tool 调用会继续跑并按真实结果 settle，只有之后新的
    ///   ctx/kiana 调用才抛 stale。宿主任务继续持有 runtime 锚（见
    ///   [`super::HostCompletionSender`]），引擎销毁时不会 abort 进程。
    ///
    /// 返回值是本次调用时仍在飞行中的宿主 promise 数量（诊断用；它们会正常完成）。
    ///
    /// 必须在 `with_js` 之外调用。
    pub fn invalidate(&self, message: impl Into<String>) -> usize {
        let mut stale = self.shared.stale.borrow_mut();
        if stale.is_none() {
            *stale = Some(message.into());
        }
        drop(stale);
        self.shared.pending.borrow().len()
    }

    /// 最近一次 [`JsEngine::invalidate`] 的 stale 文案（§4.6 的【逐字】文本由调用方传入）。
    pub fn stale_message(&self) -> Option<String> {
        self.shared.stale.borrow().clone()
    }

    /// 整代销毁再建（`/reload`）：drop 旧引擎（清理全部 `Persistent`），用
    /// `generation + 1` 重建；旧引擎若已安装 prelude，新引擎也安装。
    ///
    /// 旧代的 `kiana`/`ctx` 句柄由调用方（`api.js`）在 drop 之前 `invalidate`。
    ///
    /// 失败语义：旧引擎在调用任何可能失败的步骤**之前**就已 drop；`JsEngine::with_config`
    /// 或 [`super::install`] 失败时新引擎也随之丢弃，不会回滚旧代。调用方应把 `Err`
    /// 当作致命错误（`ExtensionHost::reload` 的文档有同样的约定）。
    pub fn reload(engine: JsEngine) -> EngineResult<JsEngine> {
        let config = engine.config().next_generation();
        let installed = engine.is_installed();
        drop(engine);
        let engine = JsEngine::with_config(config)?;
        if installed {
            super::install(&engine)?;
        }
        Ok(engine)
    }
}
