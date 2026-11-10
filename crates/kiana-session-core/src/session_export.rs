//! 会话 JSONL 导出。
//!
//! TS: `packages/coding-agent/src/core/session-export.ts`。
//!
//! # 这个文件负责什么
//!
//! 会话文件本身是一棵树（每条记录带 `id`/`parentId`，leaf 指针标记当前位置），而导出格式
//! 是**线性**的 JSONL：只保留当前 leaf 到 root 的路径，并把每条的 `parentId` 重写成上一条
//! 的 `id`。因此导出结果只包含当前分支，分享或归档时不会带出其它分支。
//!
//! 每一行必须保持**逐字节**稳定：未知字段、条目顺序与对象键序原样保留（当前稳定契约，
//! 改动需同步测试）。[`crate::session_manager::SessionManager::get_branch_json`] 对带原始对象的条目
//! （从会话文件加载的条目即 `JSON.parse` 的结果；fork 与 append 时 leaf 为 `undefined`
//! 的路径也会生成原始对象）逐字回放；对没有原始对象的运行期新建条目（正常的 append
//! 路径）则用 `to_wire_value` + `reorder_wire_keys` 从 typed 视图按 TS 键序重建。
//! `tests/session_export.rs` 用 TS 参考实现生成的 golden fixture 锁定这一点。
//!
//! # 主要流程
//!
//! 1. 构造 header（`type:"session"`、当前版本、会话 id、导出时刻、cwd，**不含**
//!    `parentSession`）。
//! 2. `get_branch_json(None)` 取当前分支的原始条目。
//! 3. 逐条覆盖 `parentId` 指向前一条 id（首条为 `null`），得到线性链。
//! 4. 可选地追加调用方提供的尾随条目（分享与缺陷报告用的元数据）。
//! 5. 每行 `json_stringify_js` 后接 `\n`，文件末尾同样有换行。
//!
//! # 调用方
//!
//! - `kiana-session` 的 `AgentSession::export_to_jsonl`（交互式 `/export`）。
//! - `kiana-interactive` 的会话分享（尾随条目携带系统提示与工具 schema）与 bug report。
//!
//! # 注意
//!
//! 导出读取调用方传入的 `&SessionManager`；并发追加由调用方自己排除（`kiana-session` 在
//! 调用前先锁住会话管理器）。

use std::path::Path;
use std::sync::Arc;

use kiana_ai::types::JsonValue;

use crate::session_manager::{
    CURRENT_SESSION_VERSION, SessionHeader, SessionManager, SessionManagerError, SessionTypeMarker,
    entry_json_string, json_stringify_js,
};

/// 追加导出专用条目的回调：`(parentId, timestamp) => entries`。
///
/// `parentId` 是最后一个导出条目的 `id`；分支为空时是 `None`（TS 类型为 `null`）。
/// `timestamp` 是 header 使用的时间戳。返回的条目按原样（经 `JSON.stringify`）追加在分支
/// 之后，函数不会改写它们的 `parentId`——父指针由回调自己写。
///
/// `kiana-interactive` 用它写入 `kiana.share` 条目（系统提示与工具 schema），供 session
/// viewer 渲染。回调放在 `Arc` 里，因此类型要求 `Send + Sync`。
///
/// TS: `session-export.ts#TrailingEntries`
pub type TrailingEntries = Arc<dyn Fn(Option<&str>, &str) -> Vec<JsonValue> + Send + Sync>;

/// 把当前分支（与可选导出条目）序列化成 JSONL。
///
/// header **不含** `parentSession`；分支被重新线性化（每条 `parentId` 覆盖为前一条 id，
/// 首条为 `null`）；末尾一个换行。
///
/// # 输入输出
///
/// - `session_manager`：取其 id、cwd 与**当前 leaf** 的分支（`get_branch_json(None)`）。
///   leaf 为空或不指向已知条目时分支为空，输出只剩 header（与 trailing 条目）。
/// - `create_trailing_entries`：可选的尾随条目生产者，见 [`TrailingEntries`]。
/// - 返回值是完整文件内容：每行一个 JSON，最后一行之后也有 `\n`。
///
/// # 保真约束
///
/// 分支条目来自 `get_branch_json`：带原始对象的条目逐字回放（未知字段与键序保留），
/// 没有原始对象的运行期新建条目由 typed 视图按 TS 键序重建。本函数对两者都只覆盖或删除
/// `parentId`。`parentId` 已存在时 `serde_json::Map::insert` 保持键的原位置（workspace
/// 启用了 `preserve_order`）；某个条目的 `id` 缺失或不是字符串时，下一条的 `parentId`
/// 会沿用该原始值（`id` 缺失则删除键，即省略该键）。
///
/// # 时间戳
///
/// header 的 `timestamp` 与 trailing 回调收到的时间戳来自同一次 `now` 调用；`export_session_to_jsonl`
/// 生成默认文件名时会再取一次当前时间，两者可能相差几毫秒。
///
/// TS: `session-export.ts#serializeSessionBranch`
pub fn serialize_session_branch(
    session_manager: &SessionManager,
    create_trailing_entries: Option<&TrailingEntries>,
) -> String {
    let timestamp =
        kiana_ai::model_catalog::js_date_to_iso_string(kiana_ai::model_catalog::now_epoch_millis());
    // header 由 typed `SessionHeader` 序列化：键序 type, version, id, timestamp, cwd；
    // `parent_session` 显式设为 None，序列化时整键省略（TS 的 header 没有这个字段）。
    let header = SessionHeader {
        kind: SessionTypeMarker::Marker,
        version: Some(CURRENT_SESSION_VERSION),
        id: session_manager.get_session_id().to_string(),
        timestamp: Some(timestamp.clone()),
        cwd: Some(session_manager.get_cwd().to_string()),
        parent_session: None,
    };
    let mut entries: Vec<JsonValue> = Vec::new();
    entries.push(
        serde_json::to_value(crate::session_manager::FileEntry::Header(header))
            .unwrap_or(JsonValue::Null),
    );
    // TS: `parentId` 初始为 `null`，每条之后变为上一条的 `entry.id`（可能是 undefined，
    // 展开时该键被省略）。原始条目携带未建模字段与键序。
    let mut parent_id: Option<JsonValue> = Some(JsonValue::Null);
    for entry in session_manager.get_branch_json(None) {
        let mut value = entry;
        let next_parent = if let JsonValue::Object(object) = &mut value {
            match &parent_id {
                Some(parent_id) => {
                    // `{ ...entry, parentId }`：键已存在时保持原位置（`IndexMap::insert` 语义）。
                    object.insert("parentId".to_string(), parent_id.clone());
                }
                // `parentId: undefined` 的键会被 `JSON.stringify` 省略。
                None => {
                    object.shift_remove("parentId");
                }
            }
            object.get("id").cloned()
        } else {
            None
        };
        entries.push(value);
        parent_id = next_parent;
    }
    if let Some(create_trailing_entries) = create_trailing_entries {
        // 只把字符串形式的父 id 交给回调；最后一个条目的 id 不是字符串时按 None 传给回调
        // （TS 运行期此时可能是 `undefined`）。
        let trailing_parent_id = parent_id.as_ref().and_then(JsonValue::as_str);
        entries.extend(create_trailing_entries(trailing_parent_id, &timestamp));
    }
    // 逐行 stringify 并追加 `\n`：换行只由每行自己的分隔符贡献，不会多出空行。
    let mut output = String::new();
    for entry in &entries {
        output.push_str(&json_stringify_js(entry));
        output.push('\n');
    }
    output
}

/// 把当前分支（与可选导出条目）写入 JSONL 文件，返回文件路径。
///
/// 默认文件名 `session-<ISO with : and . replaced by ->.jsonl`，相对 `process.cwd()` 解析；
/// 目录不存在则递归创建；已存在文件被覆盖。
///
/// # 细节
///
/// - `output_path` 为 `None` 时用默认文件名；`Some` 时交给 `kiana_core::paths::resolve_path`
///   解析（绝对路径忽略基准目录，相对路径相对当前进程 cwd；取不到 cwd 时基准回退为 `"."`）。
/// - 父目录非空且不存在时用 `create_dir_all` 递归创建；已存在则跳过。
/// - 内容是直接截断写入，没有临时文件或原子替换。
/// - 返回解析后的路径（基准是绝对路径时结果为绝对路径）。
///
/// # 错误
///
/// 目录创建或写入失败都通过 `?` 转成 `SessionManagerError::Io`（`From<std::io::Error>` 见
/// `session_manager`）。序列化本身没有错误通道。
///
/// TS: `session-export.ts#exportSessionToJsonl`
pub fn export_session_to_jsonl(
    session_manager: &SessionManager,
    output_path: Option<&str>,
    create_trailing_entries: Option<&TrailingEntries>,
) -> Result<String, SessionManagerError> {
    let default_name = format!(
        "session-{}.jsonl",
        kiana_ai::model_catalog::js_date_to_iso_string(kiana_ai::model_catalog::now_epoch_millis())
            .replace([':', '.'], "-")
    );
    // 基准目录取 `process.cwd()`；取不到时回退为 `"."`（与 `session_manager` 的
    // `current_dir_string` 回退一致）。
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let file_path = kiana_core::paths::resolve_path(
        output_path.unwrap_or(&default_name),
        &cwd,
        &Default::default(),
    );
    // 父目录非空且不存在时递归创建；路径本身没有父目录（如纯文件名）时跳过。
    if let Some(parent) = Path::new(&file_path).parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serialize_session_branch(session_manager, create_trailing_entries);
    std::fs::write(&file_path, contents)?;
    Ok(file_path)
}

/// 便捷助手：把任意可序列化条目按 TS 的 `JSON.stringify` 写出行（导出测试用）。
///
/// 返回单行文本、**不含**结尾换行；数字格式与键序由 `kiana_json` 的 JS 语义 stringify 保证
/// （例如 `3.0` 写成 `3`）。
/// TS: session-export.ts#jsonLine
pub fn json_line<T: serde::Serialize>(value: &T) -> String {
    entry_json_string(value)
}
