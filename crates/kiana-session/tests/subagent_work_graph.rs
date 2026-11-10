//! Phase 3：Work Graph 从 durable 账本投影。

mod common;

use common::{HarnessOptions, create_harness, run};
use kiana_session::agent_definition::builtin_agent_definitions;
use kiana_session::continuity::ContinuityOptions;
use kiana_session::subagent::{
    RunId, SubagentRuntimeOptions, WorkId, WorkOptions, WorkRecord, WorkStore, build_work_request,
};

fn work_continuity(state_dir: &std::path::Path) -> ContinuityOptions {
    ContinuityOptions {
        execution: false,
        checkpoints: false,
        background: false,
        ..ContinuityOptions::new(state_dir)
    }
}

fn general_purpose() -> kiana_session::agent_definition::AgentDefinition {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin general-purpose")
}

#[tokio::test]
async fn session_work_graph_projects_the_durable_ledger() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        for (work_id, resumed_from) in [(31, None), (32, Some(31))] {
            let request = build_work_request(
                &general_purpose(),
                &SubagentRuntimeOptions::default(),
                "graph",
                state.path(),
                &WorkOptions {
                    work_id: Some(WorkId(work_id)),
                    run_id: Some(RunId::new(format!("run-{work_id}"))),
                    ..Default::default()
                },
            );
            let mut record =
                WorkRecord::new(request, session_id.clone(), None, None, work_id as u64);
            if let Some(origin) = resumed_from {
                record = record.with_resumed_from(WorkId(origin));
            }
            store.begin(record).await.expect("begin");
        }

        let graph = harness
            .session
            .work_graph()
            .await
            .expect("graph")
            .expect("durable store");
        assert_eq!(graph.nodes().len(), 2);
        assert_eq!(graph.edges().len(), 1);
        assert_eq!(
            graph
                .roots()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(31)]
        );
        assert_eq!(
            graph
                .children(WorkId(31))
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(32)]
        );
        assert_eq!(
            graph
                .runnable()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(31), WorkId(32)]
        );

        // 跨会话视图按父会话分组，WorkId 不会串号。
        let set_view = harness.session.work_graph_set_view().expect("set view");
        let refresh = set_view.refresh().await.expect("refresh");
        let set = refresh.set().expect("set");
        assert_eq!(set.graphs.len(), 1);
        assert_eq!(set.skipped_sessions, 0);

        harness.cleanup();
    })
    .await;
}
