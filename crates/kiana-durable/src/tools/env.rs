//! `tools/env.ts`：工具从调用里取执行环境，没有就报普通错误。
//!
//! # 为什么单独有这个模块
//! 每个编码工具（read/write/edit/bash）都要通过 `ToolExecutionApi::env()` 拿本次调用的
//! `ExecutionEnv`。没有绑定执行环境是正常状态而不是 bug，所以不能 `unwrap`：这里把
//! `Option` 转成 `Err`，由 `harness/tool.rs` 把 `execute` 的错误折算成工具错误结果，任务
//! 本身不会 panic。
//!
//! 返回的是 `Arc<dyn ExecutionEnv>` 的克隆，调用方可以在自己的读写流程里持有它；错误文本
//! `No execution environment is configured` 是当前稳定契约，改动需同步测试。
//!
//! # 边界
//! 这里只做"取不到就报错"，不检查环境能力（shell、文件系统权限等）：那些失败由环境自己
//! 在具体操作上返回 `FileError`/`ExecutionError`。错误类型是普通的 `std::io::Error`，
//! 没有机器可读的错误码。

use std::sync::Arc;

use crate::env::ExecutionEnv;
use crate::harness::types::ToolExecutionApi;
use crate::types::BoxError;

/// 本次调用的执行环境；没有配置环境时返回 `Error("No execution environment is configured")`。
///
/// # 参数与返回
/// `api` 是本次工具调用的 API；成功时返回 `api.env()` 的 `Arc` 克隆，失败时返回
/// `BoxError`（`Box<dyn Error + Send + Sync>`），文本与 TS 的 `Error(...)` 相同。
///
/// # 使用约定
/// `None` 表示这次调用本来就没有执行环境。本 crate 的工具调用方都直接 `?` 上抛，由
/// harness 折算成工具错误结果；这里没有"默认环境"可以降级。
pub fn require_env(api: &dyn ToolExecutionApi) -> Result<Arc<dyn ExecutionEnv>, BoxError> {
    match api.env() {
        Some(env) => Ok(env),
        None => Err(std::io::Error::other("No execution environment is configured").into()),
    }
}
