//! 引擎测试的共享 harness（设计文档 §10.5）。
//!
//! **本文件对后续单元只读**：loader / runner / ui 的测试都复用它，所以只放与具体单元
//! 无关的通用能力（建引擎、求值、驱动主循环、错误物化），不放业务断言。
//!
//! 用法：
//!
//! ```ignore
//! #[tokio::test(flavor = "current_thread", start_paused = true)]
//! async fn example() {
//!     let h = Harness::new();
//!     h.eval::<()>("globalThis.x = 1;");
//!     assert!(h.settle_all().await);
//! }
//! ```
//!
//! `start_paused` 让 tokio 的时钟可被 auto-advance（需要 dev-dependency 打开的
//! `tokio/test-util`），因此定时器测试不依赖墙钟（`ARCHITECTURE.md` §4.8 第 8 条）。

#![allow(dead_code)]

use std::time::Duration;

use kiana_extensions::engine::{self, EngineConfig, JsEngine, JsError};
use rquickjs::{Ctx, Function, Object};

/// 单个测试的默认超时（paused 时钟下是虚拟时间）。
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// 物化后的 JS 错误（与 [`kiana_extensions::engine::JsError`] 同形，但不带来源）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestError {
    /// `error.name`。
    pub name: String,
    /// `error.message`（或非 Error 值的 `String(value)`）。
    pub message: String,
    /// `error.stack`。
    pub stack: Option<String>,
}

impl TestError {
    /// 构造一个仅用于断言的错误。
    pub fn new(name: &str, message: &str) -> Self {
        Self {
            name: name.to_owned(),
            message: message.to_owned(),
            stack: None,
        }
    }
}

/// 测试 harness：一个装了 prelude 的引擎 + 常用求值/驱动方法。
pub struct Harness {
    /// 引擎本体（测试可直接用它注册 host kind、句柄、持久槽等）。
    pub engine: JsEngine,
}

impl Harness {
    /// 默认配置（512 MiB / 4 MiB / 100 ms 看门狗）。
    pub fn new() -> Self {
        Self::with_config(EngineConfig::new())
    }

    /// 指定配置（OOM / 短看门狗测试用）。
    pub fn with_config(config: EngineConfig) -> Self {
        let engine = JsEngine::with_config(config).expect("create engine");
        engine::install(&engine).expect("install prelude_core");
        Self { engine }
    }

    /// 全局求值；panic on error（断言正常路径）。
    pub fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        match self.engine.eval_script::<T>(source, "<test>") {
            Ok(value) => value,
            Err(error) => panic!("eval failed: {error:?}"),
        }
    }

    /// 全局求值；错误物化成 [`TestError`]。
    pub fn try_eval<T>(&self, source: &str) -> Result<T, TestError>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.engine
            .eval_script_error::<T>(source, "<test>")
            .map_err(|error| TestError {
                name: error.name,
                message: error.message,
                stack: error.stack,
            })
    }

    /// 期望抛错，返回物化后的错误。
    ///
    /// 用 `()` 作为求值类型（`Value<'js>` 只实现 `FromJs<'js>` 本身，不能走 HRTB 绑定）。
    pub fn expect_error(&self, source: &str) -> TestError {
        match self.engine.eval_script_error::<()>(source, "<test>") {
            Ok(()) => panic!("expected a JS error, but the snippet succeeded"),
            Err(error) => TestError {
                name: error.name,
                message: error.message,
                stack: error.stack,
            },
        }
    }

    /// 求值一段带顶层 `await` 的脚本并泵到 resolve（同步 runtime 下的 2 轮语义）。
    pub fn eval_promise<T>(&self, source: &str) -> Result<T, TestError>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.engine.with_js(|ctx| {
            let promise = match ctx.eval_promise(source) {
                Ok(promise) => promise,
                Err(_) => return Err(catch_error(&ctx)),
            };
            loop {
                if let Some(result) = promise.result::<T>() {
                    return result.map_err(|_| catch_error(&ctx));
                }
                if !ctx.execute_pending_job() {
                    return Err(TestError::new("Error", "promise did not settle"));
                }
            }
        })
    }

    /// 取走并物化引擎错误队列（未处理拒绝 / 定时器异常）。
    pub fn take_errors(&self) -> Vec<JsError> {
        self.engine.take_errors()
    }

    /// 泵微任务。
    pub fn pump(&self) -> usize {
        self.engine.pump_jobs()
    }

    /// 驱动引擎直到 `pred` 为真或超时；处理完成队列、定时器与微任务。
    pub async fn drive_until(
        &self,
        timeout: Duration,
        mut pred: impl FnMut(&Self) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred(self) {
                return true;
            }
            if self.engine.try_drain_completions() > 0 {
                continue;
            }
            if self.engine.pump_jobs() > 0 {
                continue;
            }
            if tokio::time::Instant::now() >= deadline {
                return pred(self);
            }
            match self.engine.next_timer_deadline() {
                Some(due) if due <= deadline => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(due) => {
                            self.engine.fire_due_timers();
                        }
                        _ = self.engine.pump_completions() => {}
                        _ = tokio::time::sleep_until(deadline) => return pred(self),
                    }
                }
                _ => {
                    tokio::select! {
                        _ = self.engine.pump_completions() => {}
                        _ = tokio::time::sleep_until(deadline) => return pred(self),
                    }
                }
            }
        }
    }

    /// 驱动到“没有待处理完成事件、没有定时器、没有微任务”为止。
    ///
    /// 返回 `true` 表示达到空闲；`false` 表示超时。
    pub async fn settle_all(&self) -> bool {
        self.settle_all_within(TEST_TIMEOUT).await
    }

    /// [`Harness::settle_all`] 的超时版本。
    pub async fn settle_all_within(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.engine.try_drain_completions() > 0 {
                continue;
            }
            if self.engine.pump_jobs() > 0 {
                continue;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            match self.engine.next_timer_deadline() {
                Some(due) if due <= deadline => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(due) => {
                            self.engine.fire_due_timers();
                        }
                        _ = self.engine.pump_completions() => {}
                        _ = tokio::time::sleep_until(deadline) => return false,
                    }
                }
                _ => {
                    // 没有定时器：给 spawn_local 的任务一次被 poll 的机会；若一个 tick
                    // 之后仍无事件，视为空闲。
                    tokio::select! {
                        _ = self.engine.pump_completions() => {}
                        _ = tokio::time::sleep(Duration::from_millis(1)) => {
                            if self.engine.try_drain_completions() == 0 && self.engine.pump_jobs() == 0 {
                                return true;
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => return false,
                    }
                }
            }
        }
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// 把当前挂起的 JS 异常物化成 [`TestError`]（会清除挂起状态）。
///
/// 优先用 prelude 的 `__kiana.errorText`（`error instanceof Error ? error.message :
/// String(error)`）；prelude 不可用时退回属性读取。
pub fn catch_error(ctx: &Ctx<'_>) -> TestError {
    let value = ctx.catch();
    if let Ok(kiana) = ctx.globals().get::<_, Object>("__kiana")
        && let Ok(error_text) = kiana.get::<_, Function>("errorText")
    {
        match error_text.call::<_, Object>((value.clone(),)) {
            Ok(result) => {
                return TestError {
                    name: result.get("name").unwrap_or_default(),
                    message: result.get("message").unwrap_or_default(),
                    stack: result.get::<_, Option<String>>("stack").ok().flatten(),
                };
            }
            Err(_) => {
                let _ = ctx.catch();
            }
        }
    }
    if let Some(text) = value.as_string() {
        return TestError {
            name: "Error".to_owned(),
            message: text.to_string().unwrap_or_default(),
            stack: None,
        };
    }
    // OOM 之后 `__kiana.errorText` 可能因为再次分配失败而不可用；直接读属性。
    if let Some(object) = value.as_object() {
        return TestError {
            name: object.get::<_, String>("name").unwrap_or_else(|_| {
                let _ = ctx.catch();
                "Error".to_owned()
            }),
            message: object.get::<_, String>("message").unwrap_or_else(|_| {
                let _ = ctx.catch();
                String::new()
            }),
            stack: object.get::<_, Option<String>>("stack").ok().flatten(),
        };
    }
    TestError::new("Error", "<unprintable>")
}

/// 在 `LocalSet` 上运行 future：`spawn_local` 与 `!Send` 的引擎都需要它。
pub async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}
