//! 移植自 `packages/durable/test/examples/25-coding-agent.ts`：一个小型编码 agent 的工作目录
//! 行为。
//!
//! TS 示例通过 Harness 的 `env({ cwd })` 在每次工具调用时选择目录，并用 settings 切换并行
//! 工具。Rust 的 harness 单元尚未实现，本示例直接演示它依赖的环境层：同一组编码工具，以及
//! `pwd` 在工作区被切换后报出的目录。

use std::sync::{Arc, Mutex};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    FileSystem, OutputCallback, Shell, ShellCommand, ShellExecOptions, ShellOutputInfo,
};
use kiana_durable::tools::create_coding_tools;
use kiana_durable::tools::read::ReadToolOptions;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let env = NodeExecutionEnv::new(NodeEnvOptions::new(temp));
    let workspace = env
        .create_temp_dir(Some("kiana-durable-agent-"), context)
        .await?;
    env.create_dir(&format!("{workspace}/app"), None, context)
        .await?;

    // 内置编码工具。
    let tools = create_coding_tools(ReadToolOptions::default()).tools();
    println!(
        "tools: {:?}",
        tools
            .iter()
            .map(|tool| tool.tool.name.as_str())
            .collect::<Vec<_>>()
    );

    // Harness 对每次调用都用会话的 `cwd` 建环境；切换目录不需要重启。
    for cwd in [workspace.clone(), format!("{workspace}/app")] {
        let collected = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&collected);
        let on_output: OutputCallback = Arc::new(move |text, _context, _info: &ShellOutputInfo| {
            sink.lock().unwrap().push_str(text);
        });
        let options = ShellExecOptions {
            cwd: Some(cwd),
            on_output: Some(on_output),
            ..ShellExecOptions::default()
        };
        env.exec(
            ShellCommand::Shell("pwd".to_owned()),
            Some(&options),
            context,
        )
        .await?;
        let output = collected.lock().unwrap().trim().to_owned();
        println!("bash ran in: {output}");
    }

    // 用户关掉并行工具后，下一次使用起生效（环境层没有对应开关，这里记录同一时刻的设置）。
    println!("settings: parallelTools=false applied from the next call");
    Ok(())
}
