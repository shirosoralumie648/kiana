//! 移植自 `packages/durable/test/examples/26-plan-mode.ts`：会话进入只读计划模式，再切回完整
//! 工具集。
//!
//! TS 示例通过 Harness 的 `configure({ extensions, tools })` 切换选择。Rust 的 harness 单元
//! 尚未实现，本示例直接展示它选择的两份工具清单：计划模式只有 `read` 与 `submit_plan`，
//! 离开后恢复 CodingTools 的全部工具。

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{FileContent, FileSystem};
use kiana_durable::harness::define::define_tool;
use kiana_durable::harness::types::ToolRegistration;
use kiana_durable::tools::create_coding_tools;
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use serde_json::json;

/// `submit_plan`：把计划作为步骤列表提交。
fn submit_plan_tool() -> ToolRegistration {
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "submit_plan".to_owned(),
            description: "Submit the plan as a list of steps.".to_owned(),
            parameters: json!({
                "type": "object",
                "required": ["steps"],
                "properties": {
                    "steps": { "type": "array", "items": { "type": "string" } }
                }
            }),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: None,
    })
}

fn names(tools: &[ToolRegistration]) -> Vec<&str> {
    tools.iter().map(|tool| tool.tool.name.as_str()).collect()
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let env = NodeExecutionEnv::new(NodeEnvOptions::new(temp));
    let directory = env
        .create_temp_dir(Some("kiana-durable-plan-"), context)
        .await?;
    env.write_file(
        &format!("{directory}/server.ts"),
        FileContent::Text("app.listen(3000);\n".to_owned()),
        context,
    )
    .await?;

    let full = create_coding_tools(ReadToolOptions::default()).tools();
    let plan = vec![
        create_read_tool(ReadToolOptions::default()),
        submit_plan_tool(),
    ];
    println!("tools: {:?}", names(&full));
    println!("plan mode tools: {:?}", names(&plan));

    // 计划模式里模型可以读取代码，但不能修改。
    println!(
        "read server.ts: {:?}",
        env.read_text_file(&format!("{directory}/server.ts"), context)
            .await?
    );
    println!("plan: [\"Read PORT from the environment\", \"Default to 3000\"]");

    println!("tools again: {:?}", names(&full));
    Ok(())
}
