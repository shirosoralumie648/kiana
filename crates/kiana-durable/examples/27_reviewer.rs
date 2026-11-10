//! 移植自 `packages/durable/test/examples/27-reviewer.ts`：主 agent 旁边一个只读的 reviewer。
//!
//! TS 示例给 reviewer 一个更便宜的模型、只读工具和它自己的 checkout，并用 GenerationTask 的
//! `onYield` 做二轮审查。Rust 的 harness 单元尚未实现，本示例展示它配置里属于本单元的部分：
//! 只有 `read` 的工具集，以及在独立目录里读到的文件。

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{FileContent, FileSystem};
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let env = NodeExecutionEnv::new(NodeEnvOptions::new(temp));
    let worktree = env
        .create_temp_dir(Some("kiana-durable-review-"), context)
        .await?;
    env.write_file(
        &format!("{worktree}/user.ts"),
        FileContent::Text("export const name = (user) => user.name;\n".to_owned()),
        context,
    )
    .await?;

    let reviewer_tools = [create_read_tool(ReadToolOptions::default())];
    println!(
        "reviewer: small {:?} true",
        reviewer_tools
            .iter()
            .map(|tool| tool.tool.name.as_str())
            .collect::<Vec<_>>()
    );
    println!(
        "> Read user.ts: {}",
        env.read_text_file(&format!("{worktree}/user.ts"), context)
            .await?
            .trim_end()
    );
    println!("reviewer: 1. `name` does not handle a missing user.");
    println!("reviewer: 2. `user` has no type. No further findings.");
    Ok(())
}
