//! Work Graph：Work 账本的只读图投影（Phase 3）。
//!
//! CQRS 边界不变：**权威状态是 `WorkRecord`**，图只是从账本快照构建的投影。
//! `resumedFrom` 是图里的依赖边（原记录 → 恢复它的后继）；`parentSessionId` 是
//! 所有权域，`depth` 是嵌套深度。同一父会话的 `WorkId` 唯一，因此图按父会话构建；
//! 跨会话用 [`WorkGraphSet`] 分组，避免不同会话的同号 WorkId 混在同一张图里。
//!
//! 这里没有引入调度器或队列：图不执行任何 Work。`runnable()` 只是标出“现在还能
//! 推进”的节点（Running，或仍可被 `resume_work` 接受的 lineage tip）；并行由互相
//! 独立的可运行分支自然获得，不需要额外的线程池。
//!
//! durable 的 `TaskGraph` 绑定 `Harness`、数值 `ConversationId` 与 `TaskRecord`，
//! Work 账本使用字符串父会话与 per-session `WorkId`，直接复用会把两套身份混在
//! 一起；本模块只借用它的图/视图词汇，权威数据仍来自 Work 账本。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::subagent::work::{RunId, WorkId, WorkPlanId, WorkStoreError};
use crate::subagent::work_store::{DurableWorkStore, WorkRecord};

/// 图节点的生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkGraphState {
    /// 正在运行或等待恢复落定。
    Running,
    /// 已终态。
    Terminal {
        /// `WorkTerminal::status()`。
        status: String,
        /// `Failed.stopReason`（其它终态为 `None`）。
        stop_reason: Option<String>,
    },
}

impl WorkGraphState {
    /// 稳定状态字符串。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Terminal { status, .. } => status,
        }
    }

    /// 是否终态。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }
}

/// Work 图中的一个节点（不可变快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkGraphNode {
    /// Work 身份（在所属父会话内唯一）。
    pub work_id: WorkId,
    /// 所有权域。
    pub parent_session_id: String,
    /// 本次执行身份。
    pub run_id: RunId,
    /// profile 名。
    pub profile: String,
    /// 生命周期状态。
    pub state: WorkGraphState,
    /// 依赖边来源：恢复时指向的原记录。
    pub resumed_from: Option<WorkId>,
    /// 嵌套深度。
    pub depth: u32,
    /// 关联的后台 attempt。
    pub background_task_id: Option<String>,
    /// 请求是否声明可恢复。
    pub resumable: bool,
    /// 计划身份。
    pub plan_id: Option<WorkPlanId>,
    /// 持久会话文件。
    pub session_file: Option<String>,
    /// 已尝试次数。
    pub attempts: u32,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: u64,
    /// 更新时间（Unix 毫秒）。
    pub updated_at_ms: u64,
}

impl WorkGraphNode {
    fn from_record(record: &WorkRecord) -> Self {
        let state = match record.terminal_result() {
            None => WorkGraphState::Running,
            Some(result) => {
                let stop_reason = match &result.terminal {
                    crate::subagent::WorkTerminal::Failed { stop_reason, .. } => {
                        stop_reason.clone()
                    }
                    _ => None,
                };
                WorkGraphState::Terminal {
                    status: result.terminal.status().to_string(),
                    stop_reason,
                }
            }
        };
        Self {
            work_id: record.work_id,
            parent_session_id: record.parent_session_id.clone(),
            run_id: record.run_id.clone(),
            profile: record.request.profile.clone(),
            state,
            resumed_from: record.resumed_from,
            depth: record.request.depth,
            background_task_id: record.background_task_id.clone(),
            resumable: record.request.resumable,
            plan_id: record.request.plan_id.clone(),
            session_file: record.session_file().map(str::to_string),
            attempts: record.attempts,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
        }
    }

    /// 是否是仍可被 `resume_work` 接受的 interrupted 记录（lineage tip 判定见
    /// [`WorkGraph::runnable`]）。
    fn is_resumable_interrupted(&self) -> bool {
        matches!(
            &self.state,
            WorkGraphState::Terminal {
                status,
                stop_reason: Some(reason),
            } if status == "failed" && reason == "interrupted"
        ) && self.resumable
            && self.session_file.is_some()
    }
}

/// 一条依赖边：`from` 是原记录，`to` 是恢复它产生的后继。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkGraphEdge {
    /// 原 WorkId（`to.resumedFrom`）。
    pub from: WorkId,
    /// 后继 WorkId。
    pub to: WorkId,
}

/// 一个父会话的 Work 图快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkGraph {
    nodes: Vec<WorkGraphNode>,
    edges: Vec<WorkGraphEdge>,
    skipped_sessions: usize,
}

impl WorkGraph {
    /// 从账本记录构建图。记录可以来自多个父会话时，调用方应先用
    /// [`WorkGraphSet`] 分组。
    pub fn from_records(records: impl IntoIterator<Item = WorkRecord>) -> Self {
        let mut nodes: BTreeMap<i64, WorkGraphNode> = BTreeMap::new();
        let mut edges = Vec::new();
        for record in records {
            if let Some(origin) = record.resumed_from {
                edges.push(WorkGraphEdge {
                    from: origin,
                    to: record.work_id,
                });
            }
            nodes.insert(record.work_id.0, WorkGraphNode::from_record(&record));
        }
        let mut nodes: Vec<WorkGraphNode> = nodes.into_values().collect();
        nodes.sort_by_key(|node| node.work_id.0);
        edges.sort_by_key(|edge| (edge.from.0, edge.to.0));
        Self {
            nodes,
            edges,
            skipped_sessions: 0,
        }
    }

    /// 本次构建中未能读取的会话目录数；非零表示图不完整（跨会话视图用）。
    pub fn skipped_sessions(&self) -> usize {
        self.skipped_sessions
    }

    /// 全部节点，按 `WorkId` 数值升序。
    pub fn nodes(&self) -> &[WorkGraphNode] {
        &self.nodes
    }

    /// 全部依赖边。
    pub fn edges(&self) -> &[WorkGraphEdge] {
        &self.edges
    }

    /// 按 id 取节点。
    pub fn node(&self, work_id: WorkId) -> Option<&WorkGraphNode> {
        self.nodes
            .binary_search_by_key(&work_id.0, |node| node.work_id.0)
            .ok()
            .map(|index| &self.nodes[index])
    }

    /// 没有 `resumedFrom` 的根节点。
    pub fn roots(&self) -> Vec<&WorkGraphNode> {
        self.nodes
            .iter()
            .filter(|node| node.resumed_from.is_none())
            .collect()
    }

    /// 由该记录恢复出的直接后继。
    pub fn children(&self, work_id: WorkId) -> Vec<&WorkGraphNode> {
        self.edges
            .iter()
            .filter(|edge| edge.from == work_id)
            .filter_map(|edge| self.node(edge.to))
            .collect()
    }

    /// 当前可推进的节点：Running 记录，或仍可被 `resume_work` 接受的 lineage
    /// tip（resumable + `sessionFile` + interrupted + 尚无后继）。互相独立的分支
    /// 会同时出现在这里；本方法不启动任何东西。
    pub fn runnable(&self) -> Vec<&WorkGraphNode> {
        let anchored: HashSet<i64> = self.edges.iter().map(|edge| edge.from.0).collect();
        self.nodes
            .iter()
            .filter(|node| match &node.state {
                WorkGraphState::Running => true,
                WorkGraphState::Terminal { .. } => {
                    node.is_resumable_interrupted() && !anchored.contains(&node.work_id.0)
                }
            })
            .collect()
    }

    /// 序列化成宿主可消费的 JSON 值。
    ///
    /// 身份字段写成十进制字符串（与 Work 账本一致），超过 2^53 的 `WorkId`
    /// 不会在 JS 侧丢精度。
    pub fn to_json(&self) -> Value {
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                let mut object = Map::new();
                object.insert(
                    "workId".to_string(),
                    Value::String(node.work_id.0.to_string()),
                );
                object.insert(
                    "parentSessionId".to_string(),
                    Value::String(node.parent_session_id.clone()),
                );
                object.insert("runId".to_string(), Value::String(node.run_id.0.clone()));
                object.insert("profile".to_string(), Value::String(node.profile.clone()));
                object.insert(
                    "state".to_string(),
                    match &node.state {
                        WorkGraphState::Running => json!({"status": "running"}),
                        WorkGraphState::Terminal {
                            status,
                            stop_reason,
                        } => {
                            let mut state = Map::new();
                            state.insert("status".to_string(), Value::String(status.clone()));
                            if let Some(reason) = stop_reason {
                                state.insert(
                                    "stopReason".to_string(),
                                    Value::String(reason.clone()),
                                );
                            }
                            Value::Object(state)
                        }
                    },
                );
                if let Some(origin) = node.resumed_from {
                    object.insert(
                        "resumedFrom".to_string(),
                        Value::String(origin.0.to_string()),
                    );
                }
                object.insert("depth".to_string(), Value::from(node.depth));
                if let Some(task_id) = &node.background_task_id {
                    object.insert(
                        "backgroundTaskId".to_string(),
                        Value::String(task_id.clone()),
                    );
                }
                object.insert("resumable".to_string(), Value::Bool(node.resumable));
                if let Some(plan_id) = &node.plan_id {
                    object.insert("planId".to_string(), Value::String(plan_id.0.clone()));
                }
                if let Some(session_file) = &node.session_file {
                    object.insert(
                        "sessionFile".to_string(),
                        Value::String(session_file.clone()),
                    );
                }
                object.insert("attempts".to_string(), Value::from(node.attempts));
                object.insert("createdAtMs".to_string(), Value::from(node.created_at_ms));
                object.insert("updatedAtMs".to_string(), Value::from(node.updated_at_ms));
                Value::Object(object)
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .map(|edge| {
                json!({
                    "from": edge.from.0.to_string(),
                    "to": edge.to.0.to_string(),
                    "kind": "resumed_from",
                })
            })
            .collect();
        let mut root = Map::new();
        root.insert("nodes".to_string(), Value::Array(nodes));
        root.insert("edges".to_string(), Value::Array(edges));
        if self.skipped_sessions > 0 {
            root.insert(
                "skippedSessions".to_string(),
                Value::from(self.skipped_sessions),
            );
        }
        Value::Object(root)
    }

    /// `kiana-json` 编码（JS 数字/键序语义）。
    pub fn stringify(&self) -> String {
        kiana_json::stringify(&kiana_json::normalize_numbers(self.to_json()))
    }
}

/// 跨父会话的图集合。`WorkId` 按父会话独立分配，同号记录属于不同 Work，
/// 因此不能合并成一张图。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkGraphSet {
    /// 每个父会话一张图，按会话 id 排序。
    pub graphs: Vec<WorkGraph>,
    /// 无法读取的会话目录数。
    pub skipped_sessions: usize,
}

impl WorkGraphSet {
    /// 取某个父会话的图。
    pub fn graph(&self, parent_session_id: &str) -> Option<&WorkGraph> {
        self.graphs.iter().find(|graph| {
            // 每个图内部节点都属于同一父会话；空图没有节点，按调用方传入的键无法
            // 区分，返回第一张空图也没有意义。
            graph
                .nodes
                .first()
                .is_some_and(|node| node.parent_session_id == parent_session_id)
        })
    }

    /// 序列化成 JSON 值。
    pub fn to_json(&self) -> Value {
        json!({
            "graphs": self.graphs.iter().map(WorkGraph::to_json).collect::<Vec<_>>(),
            "skippedSessions": self.skipped_sessions,
        })
    }

    /// `kiana-json` 编码。
    pub fn stringify(&self) -> String {
        kiana_json::stringify(&kiana_json::normalize_numbers(self.to_json()))
    }
}

/// 账本图的刷新视图。视图不缓存状态：每次 `refresh` 都从权威账本重建。
#[derive(Clone)]
pub struct WorkGraphView {
    store: Arc<DurableWorkStore>,
    parent_session_id: Option<String>,
}

impl WorkGraphView {
    /// 某个父会话的视图。
    pub fn for_session(store: Arc<DurableWorkStore>, parent_session_id: impl Into<String>) -> Self {
        Self {
            store,
            parent_session_id: Some(parent_session_id.into()),
        }
    }

    /// 所有可读会话的视图。
    pub fn all_sessions(store: Arc<DurableWorkStore>) -> Self {
        Self {
            store,
            parent_session_id: None,
        }
    }

    /// 从权威账本重建。
    pub async fn refresh(&self) -> Result<GraphRefresh, WorkStoreError> {
        match &self.parent_session_id {
            Some(parent_session_id) => Ok(GraphRefresh::Graph(
                self.store.graph_for_session(parent_session_id).await?,
            )),
            None => Ok(GraphRefresh::Set(self.store.graph_set().await?)),
        }
    }
}

/// 视图刷新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphRefresh {
    /// 单会话图。
    Graph(WorkGraph),
    /// 跨会话图集合。
    Set(WorkGraphSet),
}

impl GraphRefresh {
    /// 单会话图（集合视图返回 `None`）。
    pub fn graph(&self) -> Option<&WorkGraph> {
        match self {
            Self::Graph(graph) => Some(graph),
            Self::Set(_) => None,
        }
    }

    /// 图集合（单会话视图返回 `None`）。
    pub fn set(&self) -> Option<&WorkGraphSet> {
        match self {
            Self::Graph(_) => None,
            Self::Set(set) => Some(set),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::work::{
        WORK_PROTOCOL_VERSION, WorkMode, WorkRecovery, WorkResult, WorkStore as _,
    };
    use crate::subagent::work_store::WorkRecord;

    fn request(work_id: i64, run_id: &str) -> crate::subagent::WorkRequest {
        crate::subagent::WorkRequest {
            protocol: WORK_PROTOCOL_VERSION,
            work_id: WorkId(work_id),
            run_id: RunId::new(run_id),
            plan_id: Some(WorkPlanId::new("plan-1")),
            profile: "general-purpose".to_string(),
            prompt: "do the work".to_string(),
            mode: WorkMode::Task,
            approval: crate::subagent::WorkApproval::Headless,
            recovery: WorkRecovery::Resume,
            acceptance_policy: crate::subagent::WorkAcceptancePolicy::ReportOnly,
            acceptance_criteria: Vec::new(),
            workdir: None,
            isolation: None,
            depth: 1,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            max_calls: None,
            read_only: false,
            resumable: true,
        }
    }

    fn record(work_id: i64) -> WorkRecord {
        WorkRecord::new(
            request(work_id, &format!("run-{work_id}")),
            "session-1",
            None,
            None,
            10,
        )
    }

    #[test]
    fn lineage_edges_and_roots_follow_resumed_from() {
        let graph =
            WorkGraph::from_records([record(1), record(2).with_resumed_from(WorkId(1)), record(3)]);
        assert_eq!(
            graph
                .roots()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(1), WorkId(3)]
        );
        assert_eq!(
            graph
                .children(WorkId(1))
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(2)]
        );
        assert_eq!(graph.edges().len(), 1);
        assert_eq!(
            graph
                .runnable()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(1), WorkId(2), WorkId(3)],
            "independent running branches are all runnable"
        );
    }

    #[test]
    fn only_a_resumable_interrupted_tip_is_runnable_after_terminal() {
        let mut interrupted = record(4);
        let result = WorkResult::interrupted(
            &interrupted.request,
            "work interrupted: process lost",
            Some("/tmp/child.jsonl".to_string()),
            10,
            20,
        );
        interrupted.state = crate::subagent::WorkState::Terminal {
            result: Box::new(result),
            terminal_seq: 1,
        };
        let successor = record(5).with_resumed_from(WorkId(4));
        let graph = WorkGraph::from_records([interrupted.clone(), successor]);

        // W4 是 lineage tip 时本身可恢复；出现后继后只有运行中的 W5 可推进。
        assert!(graph.node(WorkId(4)).expect("w4").resumable);
        let tip_graph = WorkGraph::from_records([interrupted]);
        assert_eq!(
            tip_graph
                .runnable()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(4)]
        );
        assert_eq!(
            graph
                .runnable()
                .iter()
                .map(|node| node.work_id)
                .collect::<Vec<_>>(),
            vec![WorkId(5)]
        );
    }

    #[test]
    fn encoding_writes_identity_as_decimal_strings() {
        let graph = WorkGraph::from_records([record(9_007_199_254_740_993)]);
        let encoded = graph.stringify();
        assert!(
            encoded.contains("\"workId\":\"9007199254740993\""),
            "{encoded}"
        );
        assert!(!encoded.contains("9007199254740992"), "{encoded}");
        assert!(encoded.contains("\"planId\":\"plan-1\""), "{encoded}");
        assert!(encoded.contains("\"kind\":\"resumed_from\"") || graph.edges().is_empty());
    }

    #[tokio::test]
    async fn store_view_rebuilds_from_the_authoritative_ledger() {
        let temp = tempfile::tempdir().expect("temp");
        let store = Arc::new(DurableWorkStore::new(temp.path()));
        store.begin(record(11)).await.expect("begin");
        store
            .begin(record(12).with_resumed_from(WorkId(11)))
            .await
            .expect("begin successor");
        let view = WorkGraphView::for_session(store.clone(), "session-1");
        let refresh = view.refresh().await.expect("refresh");
        let graph = refresh.graph().expect("session graph");
        assert_eq!(graph.nodes().len(), 2);
        assert_eq!(graph.edges().len(), 1);
        assert_eq!(graph.skipped_sessions(), 0);

        let set = WorkGraphView::all_sessions(store)
            .refresh()
            .await
            .expect("refresh all");
        let set = set.set().expect("set");
        assert_eq!(set.graphs.len(), 1);
        assert_eq!(set.graphs[0].nodes().len(), 2);
    }
}
