//! finality 账本测试：active 永不退休、容量压力只退休最短 settled 前缀、
//! batch 在 ack 前原样重发、replay 与 flush。

use kiana_interactive::render_core::history::{Finality, TranscriptLedger};

fn block(rows: usize) -> Vec<String> {
    (0..rows).map(|index| format!("row-{index}")).collect()
}

#[test]
fn active_blocks_are_never_retired_even_under_pressure() {
    let mut ledger = TranscriptLedger::new();
    ledger.add_block(block(30));
    assert!(ledger.peek_retirement(80, 10).is_none());
    assert_eq!(ledger.finality(1), Some(Finality::Active));
}

#[test]
fn pressure_retires_shortest_settled_prefix() {
    let mut ledger = TranscriptLedger::new();
    let first = ledger.add_block(block(10));
    let second = ledger.add_block(block(10));
    let third = ledger.add_block(block(10));
    ledger.settle(first);
    ledger.settle(second);
    // live = 10 + 1 + 10 + 1 + 10 = 32；capacity 15：
    // 退休第 1 块剩 21 > 15；退休前两块剩 10 <= 15 → 最短前缀 2。
    let batch = ledger.peek_retirement(80, 15).expect("pressure");
    assert_eq!(batch.rows.iter().filter(|row| !row.is_empty()).count(), 20);
    assert_eq!(batch.rows.last(), Some(&String::new()));
    assert_eq!(ledger.finality(third), Some(Finality::Active));

    // ack 前重发同一个 id 与内容。
    let again = ledger.peek_retirement(80, 15).unwrap();
    assert_eq!(again.id, batch.id);
    assert_eq!(again.rows, batch.rows);
    assert!(ledger.acknowledge_batch(batch.id));
    assert_eq!(ledger.finality(first), Some(Finality::Committed));
    assert_eq!(ledger.finality(second), Some(Finality::Committed));
    assert_eq!(ledger.live_row_count(80), 10);
}

#[test]
fn no_retirement_when_live_content_fits() {
    let mut ledger = TranscriptLedger::new();
    for _ in 0..3 {
        let id = ledger.add_block(block(3));
        ledger.settle(id);
    }
    assert!(ledger.peek_retirement(80, 100).is_none());
}

#[test]
fn first_active_block_blocks_retirement_of_later_settled_blocks() {
    let mut ledger = TranscriptLedger::new();
    ledger.add_block(block(20)); // active
    let settled = ledger.add_block(block(20));
    ledger.settle(settled);
    assert!(
        ledger.peek_retirement(80, 10).is_none(),
        "不能越过 active 前缀"
    );
}

#[test]
fn retirement_budget_bounds_each_batch_but_makes_progress() {
    let mut ledger = TranscriptLedger::new().with_max_retire_rows(5);
    for _ in 0..5 {
        let id = ledger.add_block(block(3));
        ledger.settle(id);
    }
    let batch = ledger.peek_retirement(80, 1).expect("pressure");
    // 至少一块，最多预算附近；不能一次把所有块都退休。
    assert!(!batch.rows.is_empty());
    let mut total = 0usize;
    loop {
        let Some(batch) = ledger.peek_retirement(80, 1) else {
            break;
        };
        total += batch.rows.iter().filter(|row| !row.is_empty()).count();
        assert!(ledger.acknowledge_batch(batch.id));
        assert!(total <= 15);
    }
    assert_eq!(total, 15);
}

#[test]
fn ack_with_wrong_id_is_noop_and_committed_is_final() {
    let mut ledger = TranscriptLedger::new();
    let id = ledger.add_block(block(3));
    ledger.settle(id);
    let batch = ledger.peek_retirement(80, 0).unwrap();
    assert!(!ledger.acknowledge_batch(batch.id + 100));
    assert_eq!(ledger.finality(id), Some(Finality::Settled));
    assert!(ledger.acknowledge_batch(batch.id));
    assert_eq!(ledger.finality(id), Some(Finality::Committed));
    assert!(!ledger.settle(id), "committed 不能回退");
    assert!(ledger.committed_rows().len() >= 3);
}

#[test]
fn replay_returns_complete_committed_ledger_with_fresh_id() {
    let mut ledger = TranscriptLedger::new();
    let first = ledger.add_block(block(3));
    ledger.settle(first);
    let batch = ledger.peek_retirement(80, 0).unwrap();
    ledger.acknowledge_batch(batch.id);
    let committed = ledger.committed_rows().to_vec();
    assert!(!committed.is_empty());

    ledger.begin_replay();
    let replay = ledger.peek_replay().expect("replay");
    assert_eq!(
        replay.kind,
        kiana_interactive::render_core::frame::HistoryBatchKind::Replay
    );
    assert_eq!(replay.rows, committed);
    assert!(replay.id > batch.id, "id 必须单调");
    assert!(ledger.acknowledge_batch(replay.id));
    assert_eq!(ledger.committed_rows(), committed.as_slice());
    assert!(!ledger.replay_requested());
}

#[test]
fn flush_retires_all_consecutive_settled_blocks() {
    let mut ledger = TranscriptLedger::new();
    for _ in 0..3 {
        let id = ledger.add_block(block(2));
        ledger.settle(id);
    }
    ledger.begin_flush();
    let batch = ledger.peek_flush(80).expect("flush batch");
    assert_eq!(batch.rows.iter().filter(|row| !row.is_empty()).count(), 6);
    assert!(ledger.acknowledge_batch(batch.id));
    assert_eq!(ledger.live_row_count(80), 0);
}

#[test]
fn batch_ids_are_monotonic_across_rounds() {
    let mut ledger = TranscriptLedger::new();
    let first = ledger.add_block(block(3));
    ledger.settle(first);
    let batch1 = ledger.peek_retirement(80, 0).unwrap();
    ledger.acknowledge_batch(batch1.id);
    let second = ledger.add_block(block(3));
    ledger.settle(second);
    let batch2 = ledger.peek_retirement(80, 0).unwrap();
    assert!(batch2.id > batch1.id);
}
