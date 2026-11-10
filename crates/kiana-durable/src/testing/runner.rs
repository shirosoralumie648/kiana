//! `testing/runner.ts`：把与 runner 无关的用例注册到 Vitest/Jest 形状的测试框架。
//!
//! Rust 没有动态测试注册：`describe`/`it` 由调用方（或实现 [`StorageConformanceRunner`] 的
//! 收集器）驱动，两个 `register_*` 仍然逐个用例调用它们，并把用例数组返回给调用方，方便直接
//! 用 `#[test]` 循环并以 TS 的 `it` 标题作为断言消息。
//!
//! # 执行模型
//!
//! `register_*` 先构造完整用例数组，再调用一次 `runner.describe(name, suite)`；`suite` 内同步
//! 遍历数组并逐个调用 `runner.it`。`register_*` 自身不调用 `case.run()`；future 何时创建由
//! runner 的 `it` 决定：收集型 runner 延后执行，立即执行型会在登记期间触发 provider。
//!
//! # 返回值与错误
//!
//! 两个函数返回构造出的 `Vec<...Case>`，这是 Rust 侧相对 TS 的增量：没有动态注册能力时，
//! 调用方可以自己循环执行，并把用例名放进失败消息。注册本身没有错误通道；`runner.expect()`
//! 在每个 `register_*` 调用中只取一次，随后由 `create_expect_assertions` 在所有用例间共享。
//!
//! # 注意事项
//!
//! - `describe` 的实现必须同步执行传入的 `suite`，否则 `it` 不会被调用、用例也不会被登记；
//! - storage 用例统一不传 timeout（`None`），env 用例把 [`EnvConformanceCase::timeout_ms`]
//!   原样传给 `it`（只有 watch 用例非空，见 `env_conformance`）；
//! - runner 需要 `Send + Sync`；用例与断言都通过 `Arc` 共享；本模块自身不克隆 provider 的
//!   存储实例，是否在登记阶段申请实例取决于 runner 的 `it` 实现。

use std::sync::Arc;

use crate::testing::assertions::{ExpectLike, create_expect_assertions};
use crate::testing::env_conformance::create_env_conformance;
use crate::testing::storage_conformance::create_storage_conformance;
use crate::testing::types::{
    CaseRun, EnvConformanceCase, EnvConformanceOptions, EnvConformanceProvider,
    StorageConformanceCase, StorageConformanceOptions, StorageConformanceProvider,
};

/// Vitest/Jest 形状的测试 runner（TS: `StorageConformanceRunner`）。
///
/// 实现方负责 `describe`/`it` 的语义（例如收集用例或立即执行）；本 trait 不规定执行顺序或
/// 并发方式，只要求这三个方法同步返回。`it` 收到的 [`CaseRun`] 可以保存起来稍后重复执行。
pub trait StorageConformanceRunner: Send + Sync {
    /// `describe(name, suite)`；`suite` 内会逐个调用 [`StorageConformanceRunner::it`]。
    ///
    /// `register_*` 依赖实现同步调用一次 `suite`；不调用就不会注册任何用例。
    fn describe(&self, name: &str, suite: &mut dyn FnMut());
    /// `expect` 函数本身。
    fn expect(&self) -> Arc<dyn ExpectLike>;
    /// `it(name, test, timeoutMs?)`。
    ///
    /// `timeout_ms` 由 runner 决定是否应用；本模块不自行计时。
    fn it(&self, name: &str, test: CaseRun, timeout_ms: Option<u64>);
}

/// `registerStorageConformance(runner, name, withStorage)`。
///
/// 每个 case 用 `runner.expect` 适配出的断言集合执行；`it` 不传 timeout。返回的数组与
/// `suite` 中 `it` 的调用顺序一致，调用方可以丢弃它（仅注册），也可以自己循环执行。
pub fn register_storage_conformance(
    runner: &dyn StorageConformanceRunner,
    name: &str,
    with_storage: StorageConformanceProvider,
) -> Vec<StorageConformanceCase> {
    let cases = create_storage_conformance(&StorageConformanceOptions {
        assertions: create_expect_assertions(runner.expect()),
        with_storage,
    });
    // 只登记用例；provider 与断言要等 runner 执行 case.run() 时才会被使用。
    runner.describe(name, &mut || {
        for test_case in &cases {
            runner.it(&test_case.name, test_case.run.clone(), None);
        }
    });
    cases
}

/// `Pick<EnvConformanceOptions, "shell" | "symlinks" | "listFiles">`（TS 默认 `{}`）。
///
/// 字段与 [`EnvConformanceOptions`] 同名；`None` 表示“未指定”，由 `create_env_conformance`
/// 填入 TS 默认值（`["sh", "-c"]` / true / false），不是关闭开关。
#[derive(Debug, Clone, Default)]
pub struct EnvConformanceRegistration {
    /// 运行 POSIX shell 的程序与标志；默认 `["sh", "-c"]`。
    pub shell: Option<Vec<String>>,
    /// shell 的 `ln -s` 是否创建符号链接；默认 true。
    pub symlinks: Option<bool>,
    /// 环境是否实现了 `listFiles`；默认 false。
    pub list_files: Option<bool>,
}

/// `registerEnvConformance(runner, name, withEnv, options = {})`。
///
/// `options` 只透传 `shell`/`symlinks`/`listFiles`；每个 case 的 `timeoutMs` 原样传给 `it`
/// （`None` 表示不传）。返回的数组与 `suite` 中 `it` 的调用顺序一致。
pub fn register_env_conformance(
    runner: &dyn StorageConformanceRunner,
    name: &str,
    with_env: EnvConformanceProvider,
    options: EnvConformanceRegistration,
) -> Vec<EnvConformanceCase> {
    let cases = create_env_conformance(&EnvConformanceOptions {
        assertions: create_expect_assertions(runner.expect()),
        with_env,
        shell: options.shell,
        symlinks: options.symlinks,
        list_files: options.list_files,
    });
    runner.describe(name, &mut || {
        for test_case in &cases {
            runner.it(&test_case.name, test_case.run.clone(), test_case.timeout_ms);
        }
    });
    cases
}
