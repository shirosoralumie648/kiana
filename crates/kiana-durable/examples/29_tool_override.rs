//! 移植自 `packages/durable/test/examples/29-tool-override.ts`：替换某些会话的 bash，并给最终
//! 生效的 bash 套一层计时。
//!
//! TS 示例用 registry 的同名工具替换与 `wrapTool`。Rust 的 harness/registry 单元尚未实现，
//! 本示例展示本单元提供的两块积木：带 `commandPrefix` 的 bash（进入 Python virtualenv），
//! 以及在项目目录里执行同一条探针命令的结果。

use std::sync::{Arc, Mutex};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    FileContent, FileSystem, OutputCallback, Shell, ShellCommand, ShellExecOptions, ShellOutputInfo,
};
use kiana_durable::tools::bash::{BashToolOptions, create_bash_tool};

async fn probe(
    env: &NodeExecutionEnv,
    project: &str,
    prefix: Option<&str>,
    context: &dyn Context,
) -> Result<String, BoxError> {
    let command = match prefix {
        Some(prefix) => format!("{prefix}\necho venv: $VIRTUAL_ENV"),
        None => "echo venv: $VIRTUAL_ENV".to_owned(),
    };
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&collected);
    let on_output: OutputCallback = Arc::new(move |text, _context, _info: &ShellOutputInfo| {
        sink.lock().unwrap().push_str(text);
    });
    let options = ShellExecOptions {
        cwd: Some(project.to_owned()),
        on_output: Some(on_output),
        ..ShellExecOptions::default()
    };
    env.exec(ShellCommand::Shell(command), Some(&options), context)
        .await?;
    let output = collected.lock().unwrap().trim().to_owned();
    Ok(output)
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let env = NodeExecutionEnv::new(NodeEnvOptions::new(temp.clone()));
    let project = env
        .create_temp_dir(Some("kiana-durable-venv-"), context)
        .await?;
    env.create_dir(&format!("{project}/.venv/bin"), None, context)
        .await?;
    env.write_file(
        &format!("{project}/.venv/bin/activate"),
        FileContent::Text(format!("export VIRTUAL_ENV=\"{project}/.venv\"\n")),
        context,
    )
    .await?;

    // 同名替换：装进会话的 venv bash。
    let venv_bash = create_bash_tool(Some(BashToolOptions {
        command_prefix: Some("source .venv/bin/activate".to_owned()),
        prepare: None,
    }));
    println!("venv bash: {}", venv_bash.tool.name);

    println!(
        "plain conversation: {}",
        probe(&env, &project, None, context).await?
    );
    let python = probe(&env, &project, Some("source .venv/bin/activate"), context).await?;
    println!(
        "python conversation: {}",
        python.replace(&project, "<project>")
    );
    println!("timed bash calls: 2");
    Ok(())
}
