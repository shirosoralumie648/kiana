//! 执行所有权（`execution.rs`）测试。
//!
//! 覆盖 DoD：
//! - 两个进程同时 acquire 同一 session 只有一个运行；
//! - busy period 在同一事务写 ExecutionClaim + Execution.Started；
//! - terminal 释放 claim，terminal 事件只发布一次；
//! - clean shutdown 保留 claim 且不消耗 attempt；
//! - 崩溃 sweep durably 计数 attempt，超限 terminalize 且有原因；
//! - 文件存储重开后 attempt 仍保留。

mod common;

use std::sync::{Arc, Barrier};

use kiana_session::execution::{
    AcquireOutcome, AcquireResult, ExecutionController, ExecutionEvent, ExecutionStatus,
    ExecutionStore, FileExecutionStore, MemoryExecutionStore, RecoverResult, SweepAction,
};

#[test]
fn only_one_execution_can_hold_a_session() {
    let store = Arc::new(MemoryExecutionStore::new());
    let first = ExecutionController::new(Arc::clone(&store));
    let second = ExecutionController::new(Arc::clone(&store));

    let lease = match first.acquire("session-1", "process-a").expect("acquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    assert!(!lease.execution_id().is_empty());

    match second.acquire("session-1", "process-b").expect("second") {
        AcquireResult::Busy(claim) => assert_eq!(claim.execution_id, lease.execution_id()),
        other => panic!("expected busy, got {other:?}"),
    }

    // 不同 session 互不影响。
    match second.acquire("session-2", "process-b").expect("third") {
        AcquireResult::Acquired(_) => {}
        other => panic!("expected acquired, got {other:?}"),
    }
}

#[test]
fn busy_period_writes_claim_and_started_together() {
    let store = Arc::new(MemoryExecutionStore::new());
    let controller = ExecutionController::new(Arc::clone(&store));
    let _lease = match controller.acquire("session", "owner").expect("acquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    let state = store.load("session").expect("load").expect("state");
    let claim = state.claim.expect("claim");
    assert_eq!(claim.attempts, 1);
    assert_eq!(claim.owner, "owner");
    assert_eq!(state.events.len(), 1);
    match &state.events[0] {
        ExecutionEvent::Started {
            seq,
            execution_id,
            attempt,
            ..
        } => {
            assert_eq!(*seq, 1);
            assert_eq!(execution_id, &claim.execution_id);
            assert_eq!(*attempt, 1);
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

#[test]
fn terminal_releases_claim_and_is_published_once() {
    let store = Arc::new(MemoryExecutionStore::new());
    let controller = ExecutionController::new(Arc::clone(&store));
    let mut lease = match controller.acquire("session", "owner").expect("acquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    let first = lease
        .finish(ExecutionStatus::Completed, None)
        .expect("finish");
    assert!(first.released);
    assert!(first.terminal_event.is_some());

    let second = lease
        .finish(ExecutionStatus::Failed, Some("late".to_string()))
        .expect("second finish");
    assert!(!second.released);
    assert!(second.terminal_event.is_none());

    let state = store.load("session").expect("load").expect("state");
    assert!(state.claim.is_none());
    assert_eq!(state.terminal_seq, 1);
    let terminals = state
        .events
        .iter()
        .filter(|event| matches!(event, ExecutionEvent::Terminal { .. }))
        .count();
    assert_eq!(terminals, 1);

    // 终态后可重新 acquire，新 execution 有新的 id。
    let mut next = match controller.acquire("session", "owner").expect("reacquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    let third = next
        .finish(ExecutionStatus::Completed, None)
        .expect("finish second execution");
    assert!(third.released);
    assert_eq!(
        store
            .load("session")
            .expect("load")
            .expect("state")
            .terminal_seq,
        2
    );
}

#[test]
fn clean_shutdown_keeps_the_claim_without_spending_an_attempt() {
    let store = Arc::new(MemoryExecutionStore::new());
    let controller = ExecutionController::new(Arc::clone(&store));
    let mut lease = match controller.acquire("session", "owner").expect("acquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    let execution_id = lease.execution_id().to_string();
    lease.clean_shutdown().expect("clean shutdown");

    // claim 仍在，直接 acquire 仍然 Busy。
    match controller.acquire("session", "owner-2").expect("acquire") {
        AcquireResult::Busy(claim) => {
            assert_eq!(claim.execution_id, execution_id);
            assert!(claim.clean_shutdown);
        }
        other => panic!("expected busy, got {other:?}"),
    }

    // sweep 不增加 attempt，返回 Resume。
    let store_sweep = store.sweep("session", 123, 10).expect("sweep");
    assert_eq!(store_sweep.actions.len(), 1);
    match &store_sweep.actions[0] {
        SweepAction::Resume { claim, attempt } => {
            assert_eq!(claim.execution_id, execution_id);
            assert_eq!(*attempt, 1);
        }
        other => panic!("expected resume, got {other:?}"),
    }

    // recover 接管原 claim（不 acquire 新的 execution）。
    let controller2 = ExecutionController::new(Arc::clone(&store));
    let mut resumed = match controller2.recover("session", "owner-2").expect("recover") {
        RecoverResult::Resumed(lease) => lease,
        other => panic!("expected resumed, got {other:?}"),
    };
    assert_eq!(resumed.execution_id(), execution_id);
    resumed
        .finish(ExecutionStatus::Completed, None)
        .expect("finish");
}

#[test]
fn crash_sweeps_increment_attempts_and_terminalize_after_the_limit() {
    let store = Arc::new(MemoryExecutionStore::new());
    let controller = ExecutionController::new(Arc::clone(&store)).with_max_attempts(3);

    let lease = match controller.acquire("session", "owner").expect("acquire") {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    let execution_id = lease.execution_id().to_string();
    drop(lease); // 崩溃：claim 保留

    // attempt 1 -> 2
    let first = store.sweep("session", 1, 3).expect("sweep 1");
    assert_eq!(first.state.claim.as_ref().expect("claim").attempts, 2);
    assert!(matches!(first.actions[0], SweepAction::Resume { .. }));

    // 接管后再次崩溃 -> 3（adopt 本身不消耗 attempt）。
    let lease = store
        .adopt("session", &execution_id, "owner-2", None)
        .expect("adopt");
    drop(lease);
    let second = store.sweep("session", 2, 3).expect("sweep 2");
    assert_eq!(second.state.claim.as_ref().expect("claim").attempts, 3);

    // 再接管一次后崩溃；attempts == max -> terminalize，原因明确，terminal 只发布一次。
    let lease = store
        .adopt("session", &execution_id, "owner-3", None)
        .expect("adopt again");
    drop(lease);
    let third = store.sweep("session", 3, 3).expect("sweep 3");
    assert!(third.state.claim.is_none());
    assert_eq!(third.state.terminal_seq, 1);
    match &third.actions[0] {
        SweepAction::Terminalize {
            execution_id: id,
            reason,
            terminal_event,
        } => {
            assert_eq!(id, &execution_id);
            assert_eq!(reason, "execution_attempts_exhausted");
            assert!(matches!(
                terminal_event,
                Some(ExecutionEvent::Terminal {
                    status: ExecutionStatus::Failed,
                    ..
                })
            ));
        }
        other => panic!("expected terminalize, got {other:?}"),
    }

    // 重复 sweep 不再产生动作/事件。
    let fourth = store.sweep("session", 4, 3).expect("sweep 4");
    assert!(fourth.actions.is_empty());
    assert_eq!(fourth.state.terminal_seq, 1);
}

#[test]
fn file_store_persists_attempts_across_reopen_and_serializes_processes() {
    let temp = tempfile::tempdir().expect("temp");
    let store = Arc::new(FileExecutionStore::new(temp.path().join("executions")));
    let controller = ExecutionController::new(Arc::clone(&store)).with_max_attempts(10);

    let lease = match controller
        .acquire("session-file", "owner")
        .expect("acquire")
    {
        AcquireResult::Acquired(lease) => lease,
        other => panic!("expected acquired, got {other:?}"),
    };
    drop(lease);

    // 重新打开同一个目录，attempt 从磁盘读取并 +1。
    let reopened = Arc::new(FileExecutionStore::new(temp.path().join("executions")));
    let sweep = reopened
        .sweep("session-file", 1, 10)
        .expect("reopened sweep");
    match &sweep.actions[0] {
        SweepAction::Resume { claim, attempt } => {
            assert_eq!(claim.attempts, 2);
            assert_eq!(*attempt, 2);
        }
        other => panic!("expected resume, got {other:?}"),
    }

    // 两个线程（模拟两个进程）对同一 session 竞争：只能有一个 Acquired。
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for index in 0..2 {
        let store = Arc::new(FileExecutionStore::new(temp.path().join("executions")));
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let controller = ExecutionController::new(store);
            barrier.wait();
            matches!(
                controller
                    .acquire("contended", format!("owner-{index}"))
                    .expect("acquire"),
                AcquireResult::Acquired(_)
            )
        }));
    }
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .filter(|acquired| *acquired)
        .count();
    assert_eq!(winners, 1);
}

#[test]
fn finish_with_a_stale_execution_id_does_not_release_the_claim() {
    let store = Arc::new(MemoryExecutionStore::new());
    let claim = kiana_session::execution::ExecutionClaim {
        session_id: "session".to_string(),
        execution_id: "exec_one".to_string(),
        owner: "owner".to_string(),
        pid: None,
        started_at_ms: 1,
        attempts: 1,
        clean_shutdown: false,
    };
    assert!(matches!(
        store.begin(claim).expect("begin"),
        AcquireOutcome::Acquired(_)
    ));
    let outcome = store
        .finish("session", "exec_stale", ExecutionStatus::Completed, None)
        .expect("finish");
    assert!(!outcome.released);
    assert!(outcome.terminal_event.is_none());
    assert!(
        store
            .load("session")
            .expect("load")
            .expect("state")
            .claim
            .is_some()
    );
}
