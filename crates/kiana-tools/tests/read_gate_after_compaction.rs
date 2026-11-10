//! 第 6 周：压缩后 read gate 恢复的集成测试。
//!
//! - 压缩前快照最近读取；
//! - 压缩后只恢复预算内的文件；
//! - 未恢复路径的 write/edit 被 read-before-write 门禁拒绝；
//! - 文件被外部修改时恢复返回 `Changed`，不会伪装成“读过新版本”。

mod file_tools_test_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use file_tools_test_support::{TestContext, execute};
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_sandbox::{ExecutionPolicy, SandboxPolicy};
use kiana_tools::read::{ReadStateRegistry, RestoreOutcome, create_read_tool_definition};
use kiana_tools::write::{WriteToolOptions, create_write_tool_definition};
use serde_json::json;
use tempfile::TempDir;

struct Harness {
    _temp: TempDir,
    root: PathBuf,
    policy: Arc<ExecutionPolicy>,
    registry: Arc<ReadStateRegistry>,
}

impl Harness {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let policy = Arc::new(ExecutionPolicy::new(
            SandboxPolicy::danger_full_access(),
            root.clone(),
        ));
        Self {
            _temp: temp,
            root,
            policy,
            registry: Arc::new(ReadStateRegistry::new()),
        }
    }

    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn read_definition(&self) -> ToolDefinition {
        create_read_tool_definition(
            self.root.to_string_lossy(),
            Some(kiana_tools::read::ReadToolOptions {
                read_state: Some(Arc::clone(&self.registry)),
                ..Default::default()
            }),
        )
    }

    fn write_definition(&self, registry: &Arc<ReadStateRegistry>) -> ToolDefinition {
        create_write_tool_definition(
            self.root.to_string_lossy(),
            Some(WriteToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(registry)),
                ..WriteToolOptions::default()
            }),
        )
    }

    fn ctx(&self) -> Arc<dyn kiana_ext_api::types::ExtensionToolContextApi> {
        TestContext::new(&self.root)
    }
}

async fn read(harness: &Harness, path: &Path) {
    execute(
        &harness.read_definition(),
        json!({"path": path.to_string_lossy()}),
        &harness.ctx(),
    )
    .await
    .expect("read");
}

async fn write(
    definition: &ToolDefinition,
    path: &Path,
    content: &str,
    ctx: &Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
) -> Result<(), String> {
    execute(
        definition,
        json!({"path": path.to_string_lossy(), "content": content}),
        ctx,
    )
    .await
    .map(|_| ())
    .map_err(|error| error.message)
}

#[tokio::test]
async fn unrestored_paths_are_rejected_by_read_gate() {
    let harness = Harness::new();
    let a = harness.file("a.txt", "alpha\n");
    let b = harness.file("b.txt", "beta\n");
    read(&harness, &a).await;
    read(&harness, &b).await;

    let snapshots = harness.registry.snapshots(10);
    assert_eq!(snapshots.len(), 2);

    // 模拟压缩：read state 丢失，只恢复预算内的 a.txt。
    let restored_registry = Arc::new(ReadStateRegistry::new());
    let restored = snapshots
        .iter()
        .find(|snapshot| snapshot.path == a)
        .and_then(|snapshot| {
            (restored_registry.restore_snapshot(snapshot) == RestoreOutcome::Restored)
                .then_some(snapshot)
        });
    assert!(restored.is_some(), "a.txt should be restored");

    let definition = harness.write_definition(&restored_registry);
    let error = write(&definition, &b, "beta changed\n", &harness.ctx())
        .await
        .expect_err("b.txt was not restored; write must be rejected");
    assert!(
        error.contains("read") || error.contains("Read"),
        "unexpected gate error: {error}"
    );

    write(&definition, &a, "alpha changed\n", &harness.ctx())
        .await
        .expect("a.txt read state was restored");
    assert_eq!(fs::read_to_string(&a).unwrap(), "alpha changed\n");
}

#[tokio::test]
async fn changed_file_is_not_restored_as_if_read() {
    let harness = Harness::new();
    let a = harness.file("a.txt", "alpha\n");
    read(&harness, &a).await;
    let snapshot = harness.registry.snapshots(1).remove(0);
    // 外部修改文件后压缩恢复必须返回 Changed。
    fs::write(&a, "alpha changed\n").unwrap();
    let restored_registry = Arc::new(ReadStateRegistry::new());
    let outcome = restored_registry.restore_snapshot(&snapshot);
    assert!(matches!(outcome, RestoreOutcome::Changed(_)), "{outcome:?}");
    let definition = harness.write_definition(&restored_registry);
    let error = write(&definition, &a, "alpha changed again\n", &harness.ctx())
        .await
        .expect_err("changed file must not be writable without a fresh read");
    assert!(error.contains("changed") || error.contains("Changed") || error.contains("read"));
}

#[tokio::test]
async fn missing_file_restore_is_reported() {
    let harness = Harness::new();
    let a = harness.file("a.txt", "alpha\n");
    read(&harness, &a).await;
    let snapshot = harness.registry.snapshots(1).remove(0);
    fs::remove_file(&a).unwrap();
    let restored_registry = Arc::new(ReadStateRegistry::new());
    assert_eq!(
        restored_registry.restore_snapshot(&snapshot),
        RestoreOutcome::Missing
    );
}
