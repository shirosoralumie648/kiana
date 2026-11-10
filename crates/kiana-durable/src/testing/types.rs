//! `testing/types.ts`：断言接口、provider、用例与选项。
//!
//! # 与 TS 的对应
//!
//! - TS 的 `unknown` 在 Rust 侧统一为 [`JsonValue`]：conformance 只比较可 JSON 化的值，`undefined`
//!   用 [`JsonValue::Null`] 表示（`toBeUndefined` 对应 `None` 的断言，见各套件的辅助函数）。
//! - TS 的函数类型别名（`StorageConformanceProvider` 等）在 Rust 里是 `Arc<dyn Fn …>`。
//! - `EnvConformanceAssertions` 在 TS 里是 `StorageConformanceAssertions` 的别名；Rust 用
//!   `pub use … as …` 保持“同一个类型”的语义。
//!
//! # 这组类型如何被使用
//!
//! 两个一致性套件只依赖这里的抽象：`create_storage_conformance` / `create_env_conformance`
//! 把 `assertions` 与 provider 存进每个 [`CaseRun`]；执行用例时通过 provider 取得实例、通过
//! 断言集合报告失败。runner 适配层再把这些用例交给测试框架。因此新增后端只需实现 provider
//! （以及可选的断言适配），不必修改套件本身。
//!
//! # 生命周期与所有权
//!
//! - [`ConformanceFuture`] 借用调用栈上的数据（待执行的 operation 与期望错误消息），只在
//!   `rejects` 内被立即 await；[`CaseFuture`] 是 `'static`，因为 runner 可能先收集用例、
//!   稍后才执行。
//! - provider 与用例执行函数都是 `Arc<dyn Fn …>`：同一份闭包可以被多个用例和多次执行共享。
//!   用 `Fn` 允许按共享引用重复调用，但闭包仍可通过内部可变性（Mutex/Atomic 等）携带状态；
//!   套件只要求调用可重复，重复调用需由实现自行同步。
//! - [`CaseRun`] 每次调用返回一个新 future；它不保证幂等，副作用由 provider 和用例体决定。
//!
//! # 失败模式
//!
//! 断言集合的失败通道就是方法本身（默认实现 [`crate::testing::assertions::PanicAssertions`]
//! 会 panic）；[`CaseFuture`] 的输出是 `()`，没有错误类型，所以用例体的 IO 错误必须由用例
//! 自己转成 panic 或断言。provider 若没有调用回调，用例体不会执行，执行方也收不到错误信号。
//!
//! # 其他约定
//!
//! 几个选项结构体的 `Debug` 是手写的：`Arc<dyn Trait>` 与闭包不实现 `Debug`，因此只打印
//! 可打印字段并用 `finish_non_exhaustive` 标出省略。本模块的 `Option` 字段通常表示“未指定”，
//! 由构造套件的函数填入 TS 默认值，而不是关闭某个开关。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::env::ExecutionEnv;
use crate::types::{BoxError, JsonValue, Storage};

/// 一个装箱的异步结果（TS 里 `rejects(operation: Promise<unknown>, …)` 的 operation）。
///
/// `Err` 的错误消息就是 `rejects` 要匹配的消息：断言实现按“包含子串”检查（Vitest
/// `toThrow(string)`），而不是精确相等。
pub type ConformanceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonValue, BoxError>> + Send + 'a>>;

/// 用例返回的 future（TS 的 `run(): Promise<void>`）。
///
/// 输出类型是 `()`：用例没有错误通道，失败必须通过 panic（断言或 `unwrap` 辅助函数）
/// 表现出来；`'static` 允许 runner 先保存再执行。
pub type CaseFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// 一个用例的执行函数；可重复调用。
///
/// 一致性套件构造的 `CaseRun` 每次调用都会重新向 provider 申请实例并创建新的 future，因此
/// 同一用例的多次执行互不共享实例状态；调用方决定执行一次还是多次。
pub type CaseRun = Arc<dyn Fn() -> CaseFuture + Send + Sync>;

/// 测试代码可注入的断言集合（TS: `StorageConformanceAssertions`）。
///
/// 只有 [`StorageConformanceAssertions::ok`] 接受 message；`rejects` 的第二个参数必填，语义是
/// “错误消息包含该子串”（Vitest `toThrow(string)`）。
///
/// 实现通过 `Arc` 与所有用例共享，因此要求 `Send + Sync`；断言失败如何表示由实现决定
/// （仓库测试用的 `PanicAssertions` 直接 panic，记录型实现可以只记录并在事后汇总）。
#[async_trait]
pub trait StorageConformanceAssertions: Send + Sync {
    /// `expect(value).toBeTruthy()`。
    fn ok(&self, value: bool, message: Option<&str>);
    /// `expect(actual).toBe(expected)`。
    fn strict_equal(&self, actual: &JsonValue, expected: &JsonValue);
    /// `expect(actual).toEqual(expected)`。
    fn deep_equal(&self, actual: &JsonValue, expected: &JsonValue);
    /// `expect(actual).toMatchObject(expected)`。
    fn partial_deep_equal(&self, actual: &JsonValue, expected: &JsonValue);
    /// `expect(actual).toBeGreaterThan(expected)`。
    fn greater_than(&self, actual: f64, expected: f64);
    /// `await expect(operation).rejects.toThrow(messageIncludes)`。
    async fn rejects<'a>(&self, operation: ConformanceFuture<'a>, message_includes: &str);
}

/// `export type EnvConformanceAssertions = StorageConformanceAssertions;`。
pub use StorageConformanceAssertions as EnvConformanceAssertions;

/// 一个把 storage 交给测试体的回调（TS 的 `use: (storage: Storage) => Promise<void>`）。
///
/// 由 provider 调用；套件本身不创建、持有或清理 storage。
pub type StorageUse = Arc<dyn Fn(Arc<dyn Storage>) -> CaseFuture + Send + Sync>;

/// TS: `StorageConformanceProvider = (use: (storage: Storage) => Promise<void>) => Promise<void>`。
///
/// provider 必须恰好调用并等待 `use` 一次；它负责为每次调用准备干净的 storage 并在回调结束后
/// 清理。这个契约无法由类型系统强制。
pub type StorageConformanceProvider = Arc<dyn Fn(StorageUse) -> CaseFuture + Send + Sync>;

/// 一个把 environment 交给测试体的回调（TS 的 `use: (env: ExecutionEnv) => Promise<void>`）。
///
/// 由 provider 调用；套件只在回调存续期间使用这个 environment。
pub type EnvUse = Arc<dyn Fn(Arc<dyn ExecutionEnv>) -> CaseFuture + Send + Sync>;

/// TS: `EnvConformanceProvider = (use: (env: ExecutionEnv) => Promise<void>) => Promise<void>`。
///
/// 与 [`StorageConformanceProvider`] 一样，provider 必须恰好调用并等待 `use` 一次；它交给回调的
/// environment 必须满足 [`EnvConformanceOptions::with_env`] 描述的目录约束，并负责清理。
pub type EnvConformanceProvider = Arc<dyn Fn(EnvUse) -> CaseFuture + Send + Sync>;

/// `StorageConformanceOptions`。
///
/// 套件构造所需的全部依赖，都由调用方注入；`create_storage_conformance` 不读取其他全局状态，
/// 也不在构造期调用 provider。
#[derive(Clone)]
pub struct StorageConformanceOptions {
    /// 断言集合。
    pub assertions: Arc<dyn StorageConformanceAssertions>,
    /// 每个用例拿到的 storage。
    pub with_storage: StorageConformanceProvider,
}

impl std::fmt::Debug for StorageConformanceOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageConformanceOptions")
            .finish_non_exhaustive()
    }
}

/// 一个与测试 runner 无关的 storage 一致性用例。
///
/// `run` 由 runner 在执行时调用；`name` 通常被 runner 用作测试标题或失败定位信息。
#[derive(Clone)]
pub struct StorageConformanceCase {
    /// 用例名（逐字来自 TS 的 `it` 标题）。
    pub name: String,
    /// 执行体。
    pub run: CaseRun,
}

impl std::fmt::Debug for StorageConformanceCase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageConformanceCase")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// `EnvConformanceOptions`。
///
/// 三个可选字段的 `None` 表示“采用 TS 默认值”，由 `create_env_conformance` 落实；
/// 要覆盖默认值（例如在不能创建符号链接的环境里关闭 symlink 用例）必须显式传 `Some(false)`，
/// 传 `None` 不会关闭。
#[derive(Clone)]
pub struct EnvConformanceOptions {
    /// 断言集合。
    pub assertions: Arc<dyn StorageConformanceAssertions>,
    /// 每个用例拿到的环境；`cwd` 必须是全新的空可写目录。
    pub with_env: EnvConformanceProvider,
    /// 运行 POSIX shell 脚本的程序与标志；默认 `["sh", "-c"]`。
    pub shell: Option<Vec<String>>,
    /// shell 的 `ln -s` 是否创建符号链接；默认 true。
    pub symlinks: Option<bool>,
    /// 环境是否实现了可选的 `listFiles`；默认 false。
    pub list_files: Option<bool>,
}

impl std::fmt::Debug for EnvConformanceOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvConformanceOptions")
            .field("shell", &self.shell)
            .field("symlinks", &self.symlinks)
            .field("list_files", &self.list_files)
            .finish_non_exhaustive()
    }
}

/// 一个与测试 runner 无关的 `ExecutionEnv` 一致性用例。
#[derive(Clone)]
pub struct EnvConformanceCase {
    /// 用例名（逐字来自 TS 的 `it` 标题）。
    pub name: String,
    /// 等待环境 watch 延迟的用例需要比 runner 默认更长的超时；单位毫秒，`None` 表示不指定。
    ///
    /// 这只是元数据：套件本身不计时，是否应用、如何应用由 runner 或直接执行用例的调用方决定。
    pub timeout_ms: Option<u64>,
    /// 执行体。
    pub run: CaseRun,
}

impl std::fmt::Debug for EnvConformanceCase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvConformanceCase")
            .field("name", &self.name)
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}
