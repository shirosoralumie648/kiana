//! `PathMetadata`（`kiana_core::source_info::PathMetadata` 的 re-export）/ `ResolvedResource` / `ResolvedPaths` / `ProgressEvent`。
//!
//! TS: `packages/coding-agent/src/core/package-manager.ts` 的类型声明段（`ResolvedResource`、
//! `ResolvedPaths`、`PackageInstallFailure`、`MissingSourceAction`、`ProgressEvent`、
//! `ProgressCallback`、`PackageUpdate`、`ConfiguredPackage`）。
//!
//! 本文件还从 `kiana_core::source_info` 再导出 [`PathMetadata`] / [`SourceOrigin`] /
//! [`SourceScope`] 三个类型：包管理子系统的公开 API（`ResolvedPaths`、`PackageInstallFailure`
//! 等）在字段里直接用到它们，再导出让调用方不必依赖 `kiana-core`。`SourceScope` 是三值
//! 的内部作用域，公开 API 的作用域用本文件的 [`InstallScope`]（见该类型文档）。
//!
//! `PathMetadata` 定义在 `kiana_core::source_info`（`ARCHITECTURE.md` §3.5），这里只再导出，
//! 不新增字段。字段顺序固定（serde 按声明顺序输出，测试逐字断言）：`source`、`scope`、
//! `origin`、`baseDir`、`packageRoot`。
//!
//! # 这个文件解决什么问题
//! 包管理子系统的输入/输出类型集中在这里，避免 `resolve`、`settings_io`、`loader` 与
//! 下游 crate 各自定义一份。`ResolvedPaths` 是 `resolve()` / `resolve_extension_sources()`
//! 的返回值，`PackageInstallFailure` 是其中的启动警告条目，进度与更新通知类型则供调用方
//! 注册回调和展示更新提示（例如 `kiana-interactive` 的 `check_for_available_updates`
//! 只消费 `PackageUpdate::display_name`）。
//!
//! # 序列化是公开契约
//! 这些类型多数派生 `Serialize` / `Deserialize`，字段上的 `camelCase`、`rename` 与
//! `skip_serializing_if` 决定了 JSON 形状，本文件末尾的测试与 `tests/` 下的用例按字段名、
//! 顺序逐字断言。因此：
//! - 结构体字段顺序不是随意的（与 TS 接口声明顺序一致，serde 按声明顺序输出）；
//! - `Option` 字段为 `None` 时可能被省略（见各字段的 `skip_serializing_if`），改动会
//!   改变 JSON；
//! - 枚举的 `rename_all` 决定线上取值（`user` / `project`、`npm` / `git` 等）。
//!
//! # 阅读顺序
//! 先看 [`ResolvedPaths`] 与 [`ResolvedResource`] 理解 `resolve` 的输出，再看
//! [`MissingSourceAction`] / [`ProgressEvent`] 理解回调协议，最后看 [`PackageUpdate`] /
//! [`ConfiguredPackage`] 这两个 CLI 展示用的类型。`InstallScope` 是公开 API 的作用域，
//! 与三值的 `SourceScope` 刻意区分。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use kiana_core::source_info::{PathMetadata, SourceOrigin, SourceScope};

/// 带 `enabled` 标记的已解析资源。TS: `package-manager.ts#ResolvedResource`。
///
/// 字段顺序：`path`、`enabled`、`metadata`。
///
/// `path` 用 `String` 而不是 `PathBuf`：内置资源是 `builtin:<name>` 形式的合成路径，
/// 不能交给文件系统 API。`enabled` 与 `path` 分开，是因为过滤规则可能保留一个资源条目
/// 但把它标记为禁用；加载方应只使用 `enabled == true` 的条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedResource {
    /// 资源路径（可能是合成路径 `builtin:<name>`）。
    pub path: String,
    /// 该资源是否被启用（过滤规则可以保留路径但禁用）。
    pub enabled: bool,
    /// 来源元数据。
    pub metadata: PathMetadata,
}

/// 一个无法安装的已配置包。TS: `package-manager.ts#PackageInstallFailure`。
///
/// 字段顺序：`source`、`scope`、`message`。
///
/// 安装失败不会中止整个 `resolve`：它被收集进 [`ResolvedPaths::install_failures`]，
/// 由 `crate::loader` 转成启动警告（`loader::warnings` 读取 `source` / `scope` /
/// `message` 三个字段）。`source` 记录的是**实际尝试解析/安装**的源字符串；项目侧
/// `autoload: false` 的增量条目可能改用同 identity 的用户源，此时这里也是用户源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInstallFailure {
    /// 配置里的源字符串（`resolvePackageSources` 使用的是实际解析后的源）。
    pub source: String,
    /// 作用域。
    pub scope: SourceScope,
    /// 安装错误文本（`error.message`）。
    pub message: String,
}

/// `resolve()` 的结果。TS: `package-manager.ts#ResolvedPaths`。
///
/// 字段顺序：`extensions`、`skills`、`prompts`、`themes`、`installFailures`。
/// `installFailures` 在 `None` 时**不序列化**（TS 里该字段在 `resolve()` 中总是被赋值，
/// 单独构造的结果可以没有）。
///
/// `resolve()` 与 `resolve_extension_sources()` 返回时都会把 `install_failures` 显式设为
/// `Some`（可能是空数组），因此“`None`”只出现在单独构造或 `Default` 的结果里。四个资源
/// 数组的顺序、去重与启用标记由 `resolve` 的累积/排序阶段决定；本类型只是结果的容器，
/// 不提供排序或合并逻辑。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPaths {
    /// 扩展。
    pub extensions: Vec<ResolvedResource>,
    /// 技能。
    pub skills: Vec<ResolvedResource>,
    /// prompt 模板。
    pub prompts: Vec<ResolvedResource>,
    /// 主题。
    pub themes: Vec<ResolvedResource>,
    /// 安装失败列表（`resolve()` / `resolveExtensionSources()` 总是给出，可能是空数组）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_failures: Option<Vec<PackageInstallFailure>>,
}

/// `onMissing` 回调的返回值。TS: `MissingSourceAction = "install" | "skip" | "error"`。
///
/// 只有某个已配置包在磁盘上缺失时才会询问回调；离线模式下根本不会调用回调（直接按
/// `Skip` 处理），因此实现方不能依赖回调一定被调用来做清理。`Error` 会让 `resolve`
/// 以 `Missing source: <source>` 失败，`Skip` 跳过该源，`Install` 继续安装。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissingSourceAction {
    /// 继续安装。
    Install,
    /// 跳过该源。
    Skip,
    /// 抛 `Missing source: ${source}`。
    Error,
}

/// `onMissing` 回调返回的 boxed future。
///
/// `Pin<Box<...>>` 把任意具体 future 擦除成同一类型，才能放进 `dyn Fn` 的返回位置；
/// `Send + 'static` 是 `async_trait` 生成的 future 的要求，也让回调可以跨 await 点使用。
pub type MissingSourceFuture = Pin<Box<dyn Future<Output = MissingSourceAction> + Send + 'static>>;

/// `resolve(onMissing?)` 的回调。TS: `ProgressCallback` 的同类形态，但参数是源字符串。
///
/// Rust 侧用 `Arc<dyn Fn>` 而不是 `&dyn Fn`：`async_trait` 的 future 必须是 `Send + 'static`
/// 友好的，拥有所有权的回调避免把调用方的借用带进 future。
///
/// 每个缺失的源调用一次，参数是该源的实际源字符串；返回的 future 由 `resolve` 立即
/// `await`，回调本身不会被长期保存，因此无需考虑回调生命周期与取消。
pub type MissingSourceCallback = Arc<dyn Fn(String) -> MissingSourceFuture + Send + Sync>;

/// 把一个异步函数包装成 [`MissingSourceCallback`]。
///
/// 包装只是 `Box::pin`；闭包按值捕获所需状态（`F: 'static`），因此调用方要自己保证
/// 捕获的值可跨线程共享（`Send + Sync`）。
pub fn missing_source_callback<F, Fut>(f: F) -> MissingSourceCallback
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = MissingSourceAction> + Send + 'static,
{
    Arc::new(move |source: String| Box::pin(f(source)))
}

/// 进度事件类型。TS: `ProgressEvent["type"]`。
///
/// 只有安装、卸载、更新路径会发事件；`resolve` 在自动安装缺失包时不发进度（见
/// `crate::package` 模块文档）。枚举值序列化为小写字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressEventType {
    /// 开始。
    Start,
    /// 进行中（本单元不产生）。
    Progress,
    /// 完成。
    Complete,
    /// 出错。
    Error,
}

/// 进度事件动作。TS: `ProgressEvent["action"]`。
///
/// 与事件类型正交：例如 `Start`/`Complete` 都可能带 `Install`、`Remove` 或 `Update`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressAction {
    /// 安装。
    Install,
    /// 删除。
    Remove,
    /// 更新。
    Update,
    /// 克隆（本单元不产生）。
    Clone,
    /// 拉取（临时 git 源刷新）。
    Pull,
}

/// 进度事件。TS: `package-manager.ts#ProgressEvent`。
///
/// 字段顺序：`type`、`action`、`source`、`message`。Rust 字段名是 `kind`，但通过
/// `#[serde(rename = "type")]` 序列化成 `type`（测试会逐字断言 JSON）。
///
/// `source` 是用户可见的源字符串（例如 `npm:foo`），不是解析后的文件路径；`message`
/// 只在 `Start` 与 `Error` 事件上通常有值，`Complete` 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    /// 事件类型。
    #[serde(rename = "type")]
    pub kind: ProgressEventType,
    /// 动作。
    pub action: ProgressAction,
    /// 源字符串。
    pub source: String,
    /// 可选消息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 进度回调。TS: `ProgressCallback`。
///
/// 回调在触发事件的任务线程上同步执行，manager 调用它时不持有内部锁；实现方应尽快
/// 返回，避免阻塞安装/更新任务。回调需要 `Send + Sync`，因为它可能被不同任务线程调用。
pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

/// 一个可用的包更新。TS: `package-manager.ts#PackageUpdate`。
///
/// 字段顺序：`source`、`displayName`、`type`、`scope`。由
/// `DefaultPackageManager::check_for_available_updates` 产出；调用方通常只展示
/// `display_name`（`kiana-interactive` 的更新提示就是这样消费的）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageUpdate {
    /// 源字符串。
    pub source: String,
    /// 展示名（npm 是包名，git 是 `host/path`）。
    pub display_name: String,
    /// 包类型。
    #[serde(rename = "type")]
    pub package_type: PackageType,
    /// 作用域（只有 user / project）。
    pub scope: InstallScope,
}

/// `PackageUpdate["type"]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageType {
    /// `"npm"`。
    Npm,
    /// `"git"`。
    Git,
}

/// 已配置的包。TS: `package-manager.ts#ConfiguredPackage`。
///
/// 字段顺序：`source`、`scope`、`filtered`、`installedPath`。
///
/// `list_configured_packages` 按“先 user、后 project”的顺序产生这些条目；`filtered`
/// 表示 settings 里该条目是对象形式（带资源过滤），`installed_path` 只在安装路径存在时
/// 为 `Some`，序列化时缺失字段被省略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredPackage {
    /// 源字符串。
    pub source: String,
    /// 作用域。
    pub scope: InstallScope,
    /// settings 里是否为对象形式（带过滤）。
    pub filtered: bool,
    /// 已安装路径（不存在时省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
}

/// 只能取 `user` / `project` 的作用域。TS: `Exclude<SourceScope, "temporary">`、
/// `getInstalledPath(source, scope: "user" | "project")`。
///
/// 与三值的 `SourceScope` 分开，是为了让面向调用方的公开 API（安装路径、已配置包、
/// settings 增删）在类型上就排除 `temporary`；需要交给内部函数时用
/// [`InstallScope::as_source_scope`] 转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallScope {
    /// `"user"`。
    User,
    /// `"project"`。
    Project,
}

impl InstallScope {
    /// 转成 [`SourceScope`]。
    pub fn as_source_scope(self) -> SourceScope {
        match self {
            InstallScope::User => SourceScope::User,
            InstallScope::Project => SourceScope::Project,
        }
    }
}

impl From<InstallScope> for SourceScope {
    fn from(value: InstallScope) -> Self {
        value.as_source_scope()
    }
}

/// `install()` / `remove()` 的选项。TS: `options?: { local?: boolean }`。
///
/// 同一个选项也用于 `install_and_persist` / `remove_and_persist` 以及 settings 的
/// `add_source_to_settings` / `remove_source_from_settings`：`local == true` 选 project
/// 作用域，否则选 user。没有 `temporary` 选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// 为 true 时作用于项目作用域（`.kiana`）。
    pub local: bool,
}

/// `resolveExtensionSources()` 的选项。TS: `options?: { local?: boolean; temporary?: boolean }`。
///
/// 作用域优先级是 `temporary` → `local` → user：两个布尔同时为真时 `temporary` 生效。
/// 该作用域对本次调用的**所有**源统一生效，而不是逐源选择。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveExtensionSourcesOptions {
    /// 项目作用域。
    pub local: bool,
    /// 临时作用域（`-e` 的默认来源之一）。
    pub temporary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_core::source_info::PathMetadata;

    #[test]
    fn resolved_paths_omits_absent_install_failures() {
        let value = ResolvedPaths {
            extensions: vec![ResolvedResource {
                path: "builtin:mcp".into(),
                enabled: true,
                metadata: PathMetadata {
                    source: "builtin".into(),
                    scope: SourceScope::User,
                    origin: SourceOrigin::TopLevel,
                    base_dir: None,
                    package_root: None,
                },
            }],
            install_failures: None,
            ..ResolvedPaths::default()
        };
        let json = serde_json::to_string(&value).expect("serialize");
        assert_eq!(
            json,
            r#"{"extensions":[{"path":"builtin:mcp","enabled":true,"metadata":{"source":"builtin","scope":"user","origin":"top-level"}}],"skills":[],"prompts":[],"themes":[]}"#
        );
    }

    #[test]
    fn progress_event_uses_type_key() {
        let event = ProgressEvent {
            kind: ProgressEventType::Start,
            action: ProgressAction::Install,
            source: "npm:foo".into(),
            message: Some("Installing npm:foo...".into()),
        };
        assert_eq!(
            serde_json::to_string(&event).expect("serialize"),
            r#"{"type":"start","action":"install","source":"npm:foo","message":"Installing npm:foo..."}"#
        );
    }

    #[test]
    fn configured_package_field_order_and_scope() {
        let package = ConfiguredPackage {
            source: "git:github.com/u/r".into(),
            scope: InstallScope::Project,
            filtered: true,
            installed_path: Some("/tmp/x".into()),
        };
        assert_eq!(
            serde_json::to_string(&package).expect("serialize"),
            r#"{"source":"git:github.com/u/r","scope":"project","filtered":true,"installedPath":"/tmp/x"}"#
        );
    }
}
