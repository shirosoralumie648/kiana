//! 移植自 `packages/durable/test/examples/16-coding-tools.ts`：编码工具的一轮
//! （read → edit → bash）。
//!
//! TS 示例用 faux 模型与 Harness 驱动这组工具；本示例直接调用同一组工具背后的文件与 shell
//! 原语（同样的 `notes.txt`、同一次 `world` → `durable` 编辑、同一条 `cat`），输出可用于核对
//! 工具链的实际行为。

use std::sync::{Arc, Mutex};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    FileContent, FileSystem, OutputCallback, Shell, ShellCommand, ShellExecOptions, ShellOutputInfo,
};
use kiana_durable::tools::create_coding_tools;
use kiana_durable::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, generate_unified_patch,
};
use kiana_durable::tools::read::ReadToolOptions;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let env = NodeExecutionEnv::new(NodeEnvOptions::new(temp));
    let directory = env
        .create_temp_dir(Some("kiana-durable-example-"), context)
        .await?;
    env.write_file(
        &format!("{directory}/notes.txt"),
        FileContent::Text("hello world\n".to_owned()),
        context,
    )
    .await?;

    // CodingTools 带来 read、write、edit 与 bash。
    let tools = create_coding_tools(ReadToolOptions::default()).tools();
    println!(
        "tools: {:?}",
        tools
            .iter()
            .map(|tool| tool.tool.name.as_str())
            .collect::<Vec<_>>()
    );

    // edit: world → durable。
    let original = env.read_text_file("notes.txt", context).await?;
    let applied = apply_edits_to_normalized_content(
        &original,
        &[Edit {
            old_text: "world".to_owned(),
            new_text: "durable".to_owned(),
        }],
        "notes.txt",
    )?;
    env.write_file(
        &format!("{directory}/notes.txt"),
        FileContent::Text(applied.new_content.clone()),
        context,
    )
    .await?;
    println!(
        "patch:\n{}",
        generate_unified_patch("notes.txt", &original, &applied.new_content, 4)
    );

    // bash: cat notes.txt。
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&collected);
    let on_output: OutputCallback = Arc::new(move |text, _context, _info: &ShellOutputInfo| {
        sink.lock().unwrap().push_str(text);
    });
    let options = ShellExecOptions {
        cwd: Some(directory.clone()),
        on_output: Some(on_output),
        ..ShellExecOptions::default()
    };
    let result = env
        .exec(
            ShellCommand::Shell("cat notes.txt".to_owned()),
            Some(&options),
            context,
        )
        .await?;
    println!("bash: {}", collected.lock().unwrap().trim_end());
    println!("exit: {}", result.exit_code);

    println!(
        "file: {:?}",
        env.read_text_file(&format!("{directory}/notes.txt"), context)
            .await?
    );
    println!("storage: {directory}");
    Ok(())
}
