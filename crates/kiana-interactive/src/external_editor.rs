//! modes/interactive/external-editor.ts。
//!
//! 在 `$VISUAL` / `$EDITOR` 指定的外部编辑器里编辑一段文本：临时目录前缀 `kiana-editor-`、
//! 文件名 `prompt.md`、stdio 继承、退出码 0 才读回内容（去 BOM + 恰好一个结尾换行）。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 里 `fs` / `spawn` 的异常表现为 Promise reject；Rust 用 [`std::io::Result`] 表达
//!   同一错误通道（**不是** `anyhow`）。子进程启动失败在 TS 里 resolve 为 `null` 退出码，
//!   Rust 同样映射为 [`ExternalEditorResult::Failed`]。
//! * `process.stdout.write` → `std::io::stdout().write_all`（错误静默，TS 写 stdout 失败会
//!   抛异常，但随后的 finally 仍会清理目录）。
//! * Windows 的 `shell: true` 用 `cmd /d /s /c` 复刻（Node 的 `child_process` 在 win32 上
//!   用 `cmd.exe /d /s /c`）。
//!
//! ## 执行流程
//! 1. 在系统临时目录创建 `kiana-editor-*` 目录（Unix 上显式 0700），写入 `prompt.md`；
//! 2. 向 stdout 打印进入/恢复提示（写失败被忽略）；
//! 3. 以继承的 stdio 启动编辑器并等待退出；
//! 4. 退出码为 0 时读回文件、按 UTF-8 有损解码、去 BOM 与恰好一个结尾换行；
//! 5. 返回时 `TempDir` drop，best-effort 删除临时目录。
//!
//! ## 被谁调用
//! `interactive_mode` 的编辑器动作与 [`crate::components::extension_editor`] 都经由
//! [`edit_in_external_editor`] 复用这条流程。调用方负责在等待前后停/启 TUI 并回填编辑器
//! 内容——本模块不碰 TUI。
//!
//! ## 失败模式
//! - 临时目录/文件 I/O 失败、读回失败：返回 [`std::io::Result`] 的 `Err`；
//! - 子进程启动失败、编辑器以非 0 退出码结束、或被信号终止（`ExitStatus::code()` 为
//!   `None`）：返回 [`ExternalEditorResult::Failed`]，调用方应保留原文本；
//! - 无论成功失败，临时目录都在返回前删除；删除失败不改变返回值。

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use tempfile::Builder;

/// `ExternalEditorOptions`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalEditorOptions {
    /// 编辑器命令（按空格切分：`[editor, ...args]`）。
    pub command: String,
    /// 写入 `prompt.md` 的初始内容。
    pub content: String,
}

/// `ExternalEditorResult`（TS 的可辨识联合 `{status:"complete";content} | {status:"failed"}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEditorResult {
    /// 编辑器退出码 0：`content` 是读回并清理后的文本。
    Complete {
        /// 去 BOM、去一个结尾换行后的内容。
        content: String,
    },
    /// 退出码非 0 或无法启动。
    Failed,
}

/// `editInExternalEditor(options)`。
///
/// 输入：`options.command` 是完整命令行字符串，按**单个空格**切分（与 TS 的 `split(" ")`
/// 一致，不支持带空格的路径或引号语法）；`options.content` 写入临时文件作为初始内容。
///
/// 输出：`Complete` 携带读回并清理后的文本（可能为空串），`Failed` 表示编辑器没有以
/// 退出码 0 结束。
///
/// 错误：只有创建目录、写入、读回文件会返回 `Err`；子进程相关失败一律折叠为 `Failed`。
/// 本函数不接收取消令牌，编辑器退出前一直等待，因此调用方通常先停掉 TUI 再 await。
///
/// 临时目录在返回前 best-effort 删除（`TempDir` 的 drop 忽略错误，与 TS 的
/// `try { rmSync(...) } catch {}` 等价）。
pub async fn edit_in_external_editor(
    options: ExternalEditorOptions,
) -> std::io::Result<ExternalEditorResult> {
    // `mkdtempSync(join(tmpdir(), "kiana-editor-"))`：mkdtemp 固定创建 0700 目录（不受 umask 影响），
    // tempfile 默认走 `create_dir`（受 umask 影响），因此显式要求 0700。
    let mut builder = Builder::new();
    builder.prefix("kiana-editor-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let directory = builder.tempdir()?;
    let file_path: PathBuf = directory.path().join("prompt.md");
    std::fs::write(&file_path, options.content.as_bytes())?;

    // `process.stdout.write("Launching external editor: ...\nKiana will resume when the editor exits.\n")`
    let announcement = format!(
        "Launching external editor: {}\nKiana will resume when the editor exits.\n",
        options.command
    );
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(announcement.as_bytes());
    let _ = stdout.flush();

    // 非 0 退出码、spawn 失败、被信号终止都走同一条 Failed 分支：不读回文件。
    let exit_code = spawn_editor(&options.command, &file_path).await;
    if exit_code != Some(0) {
        return Ok(ExternalEditorResult::Failed);
    }

    // `readFileSync(filePath, "utf-8")`：Node 用 U+FFFD 替换非法字节。
    // 读回失败（例如编辑器删除了临时文件）以 `Err` 返回，调用方走崩溃/错误提示路径。
    let bytes = std::fs::read(&file_path)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // `stripBom(...).replace(/\n$/, "")`：只去掉最后一个 `\n`。
    let stripped = kiana_core::text::strip_bom(&text);
    let content = stripped.strip_suffix('\n').unwrap_or(&stripped).to_string();
    Ok(ExternalEditorResult::Complete { content })
}

/// `const [editor, ...editorArgs] = command.split(" "); spawn(editor, [...args, filePath], {stdio:"inherit", shell:win32})`。
///
/// 返回 `null`（`None`）表示 spawn 出错或子进程被信号终止（TS 的 `exitCode` 为 `null`）。
async fn spawn_editor(command: &str, file_path: &PathBuf) -> Option<i32> {
    let mut parts = command.split(' ');
    let editor = parts.next().unwrap_or("");
    let editor_args: Vec<&str> = parts.collect();

    let mut cmd = if cfg!(windows) {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/d")
            .arg("/s")
            .arg("/c")
            .arg(format!("{} {}", command, file_path.display()));
        cmd
    } else {
        let mut cmd = tokio::process::Command::new(editor);
        cmd.args(&editor_args).arg(file_path);
        cmd
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status().await {
        Ok(status) => status.code(),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    // 测试目标：只覆盖结果枚举的形状匹配（构造后能解构出 `Complete` 的 `content`）。
    // 真实子进程/临时目录流程由 tests/external_editor.rs（用 fixtures/fake-external-editor.mjs
    // 走完整流程，断言目录前缀、权限、文件名、内容清理与失败路径）与
    // tests/components_extension_dialogs.rs 的 ctrl-G 测试覆盖；interactive_mode::input 侧
    // 只覆盖 finish_open_external_editor 的 I/O 失败崩溃路径（tests/interactive_mode_lifecycle.rs
    // 的 external_editor_io_failures_follow_the_crash_path）。
    use super::*;

    #[test]
    fn complete_result_carries_the_edited_content() {
        let result = ExternalEditorResult::Complete {
            content: "edited".into(),
        };
        match result {
            ExternalEditorResult::Complete { content } => assert_eq!(content, "edited"),
            ExternalEditorResult::Failed => panic!("expected complete"),
        }
    }
}
