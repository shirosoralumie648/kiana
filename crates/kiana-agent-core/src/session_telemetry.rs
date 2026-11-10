//! 类型化 `SessionTelemetry`：turn / API / SSE / tool / compaction / hook / startup 埋点。
//!
//! 这个类型把「会话层关心的事件」翻译成统一的 span + 指标：
//!
//! | 事件 | span 名 | 指标 |
//! | --- | --- | --- |
//! | 一次 agent run | `invoke_agent <agent>` | — |
//! | 一轮 | `kiana.session.turn` | `kiana.session.turns`、TTFT/TTFM 直方图 |
//! | 一次模型请求 | `chat <model>` | API 延迟、token、cost |
//! | 一个 SSE 事件 | span event `sse.<type>` | `kiana.api.sse.events` |
//! | 一次工具调用 | `execute_tool <tool>` | 工具计数/延迟（按 status/kind） |
//! | 压缩 | `kiana.session.compaction` | 压缩计数/延迟 |
//! | hook | `kiana.hook.run` | hook 计数/延迟 |
//! | 启动阶段 | `kiana.session.startup` | 阶段延迟 |
//!
//! # 隐私与基数
//!
//! - 内容捕获默认关闭（[`ContentCapture`]，环境变量
//!   `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true` 才打开）；
//! - **指标标签**只放低基数/脱敏值：工具名只出现在 span 属性
//!   （`gen_ai.tool.name`），指标用 `tool.kind` 三分类；hook 指标只放 hook event；
//! - 关闭捕获时 prompt / tool 参数/结果不会写进任何 span 属性或事件。
//!
//! # 生命周期
//!
//! `SessionTelemetry` 内部持有一个遥测 context（默认内存实现，可注入）。span 采用
//! 「先启动、后收尾」的显式句柄 [`SessionSpan`]：`start_*` 返回句柄，异步操作结束后
//! `finish().await` 结算；`Drop` 时也会发送收尾信号（但不等待）。这样 agent 主流程不会被
//! 遥测阻塞，也不会留下未结算的 span。
//!
//! `flush()` 把内部记录（若有内存 store）转交给可选的 [`OtlpExporter`] 并导出；导出失败
//! 只体现在 [`ExportOutcome`] 里，不影响调用方。

use std::sync::{Arc, Mutex};

use kiana_ai::types::Usage;
use kiana_telemetry::names;
use kiana_telemetry::otel::{ExportOutcome, LogRecord, OtlpExporter, SeverityNumber};
use kiana_telemetry::{
    AttributeValue, ContentCapture, InMemoryTelemetryContext, SpanAttributes, SpanOptions,
    SpanStatus, StatusError, TelemetryContext, TelemetryContextExt, TelemetryError,
    TelemetryMetrics, TelemetrySpan, TraceQuery,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::run_collector::ToolStatus;

/// 一次 API 调用的埋点输入。
#[derive(Debug, Clone, PartialEq)]
pub struct ApiCallRecord {
    /// provider id。
    pub provider: String,
    /// 请求的模型 id。
    pub model: String,
    /// provider 实际返回的模型 id。
    pub response_model: Option<String>,
    /// 请求耗时（毫秒）。
    pub duration_ms: f64,
    /// 首 token 延迟（毫秒）。
    pub ttft_ms: Option<f64>,
    /// 首个有意义 token 延迟（毫秒）。
    pub ttfm_ms: Option<f64>,
    /// token 用量。
    pub usage: Usage,
    /// 费用来源（`canonical` / `user_declared` / `provider_reported`）。
    pub cost_source: Option<String>,
    /// stop reason 文本。
    pub stop_reason: Option<String>,
    /// 失败类型。
    pub error_type: Option<String>,
}

/// 一次工具调用的埋点输入。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    /// 工具名（只进 span 属性）。
    pub tool_name: String,
    /// tool call id（进 span 属性）。
    pub tool_call_id: String,
    /// 终态。
    pub status: ToolStatus,
    /// 耗时（毫秒）。
    pub duration_ms: f64,
    /// 错误类型。
    pub error_type: Option<String>,
}

/// 一次压缩的埋点输入。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionRecord {
    /// 触发方式（`auto` / `manual` / `overflow`）。
    pub trigger: String,
    /// 耗时（毫秒）。
    pub duration_ms: f64,
    /// 压缩前 token。
    pub input_tokens: u64,
    /// 压缩后 token。
    pub output_tokens: u64,
    /// 结果（`ok` / `error` / `skipped`）。
    pub outcome: String,
}

/// 一次 hook 执行的埋点输入。
#[derive(Debug, Clone, PartialEq)]
pub struct HookRunRecord {
    /// hook 事件（`PreToolUse` 等；低基数）。
    pub hook_event: String,
    /// hook 名/脚本名（只进 span 属性）。
    pub hook_name: Option<String>,
    /// 耗时（毫秒）。
    pub duration_ms: f64,
    /// 结果（`allow` / `deny` / `error` / `noop`）。
    pub outcome: String,
}

/// 一轮的耗时跟踪器。
#[derive(Debug, Clone)]
pub struct TurnTracker {
    started: std::time::Instant,
    first_token: Option<std::time::Instant>,
    first_meaningful_token: Option<std::time::Instant>,
}

impl Default for TurnTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnTracker {
    /// 开始计时。
    pub fn new() -> Self {
        TurnTracker {
            started: std::time::Instant::now(),
            first_token: None,
            first_meaningful_token: None,
        }
    }

    /// 记录一个流事件；`meaningful` 表示非 thinking/非空文本增量。
    pub fn record_token(&mut self, meaningful: bool) {
        let now = std::time::Instant::now();
        self.first_token.get_or_insert(now);
        if meaningful {
            self.first_meaningful_token.get_or_insert(now);
        }
    }

    /// TTFT（毫秒）。
    pub fn ttft_ms(&self) -> Option<f64> {
        self.first_token
            .map(|instant| instant.duration_since(self.started).as_secs_f64() * 1_000.0)
    }

    /// TTFM（毫秒）。
    pub fn ttfm_ms(&self) -> Option<f64> {
        self.first_meaningful_token
            .map(|instant| instant.duration_since(self.started).as_secs_f64() * 1_000.0)
    }

    /// 整轮耗时（毫秒）。
    pub fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1_000.0
    }
}

/// 一个打开的 session span。
pub struct SessionSpan {
    span: Arc<dyn TelemetrySpan>,
    finish: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SessionSpan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionSpan")
            .field("span", &"<telemetry span>")
            .finish()
    }
}

impl SessionSpan {
    /// 底层 span 句柄。
    pub fn span(&self) -> &Arc<dyn TelemetrySpan> {
        &self.span
    }

    /// 在同一个 trace 下启动子 span。
    pub async fn start_child(
        &self,
        name: impl Into<String>,
        attributes: SpanAttributes,
    ) -> Result<SessionSpan, TelemetryError> {
        let context: Arc<dyn TelemetryContext> = self.span.clone();
        start_span_on(context, name.into(), attributes).await
    }

    /// 标记 span 失败并结算。
    pub async fn finish_with_error(mut self, error_type: &str, message: &str) {
        self.span.set_status(SpanStatus::Error {
            error: Some(StatusError {
                name: error_type.to_string(),
                message: message.to_string(),
            }),
        });
        self.finish_inner().await;
    }

    /// 结算 span（等待记录完成）。
    pub async fn finish(mut self) {
        self.finish_inner().await;
    }

    async fn finish_inner(&mut self) {
        if let Some(sender) = self.finish.take() {
            let _ = sender.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

impl Drop for SessionSpan {
    fn drop(&mut self) {
        if let Some(sender) = self.finish.take() {
            let _ = sender.send(());
        }
    }
}

async fn start_span_on(
    context: Arc<dyn TelemetryContext>,
    name: String,
    attributes: SpanAttributes,
) -> Result<SessionSpan, TelemetryError> {
    let options = SpanOptions {
        name,
        attributes: Some(attributes),
        links: None,
    };
    let (span_tx, span_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = context
            .start_span_typed(options, move |span| {
                Box::pin(async move {
                    let _ = span_tx.send(span);
                    let _ = finish_rx.await;
                })
            })
            .await;
    });
    let span = span_rx
        .await
        .map_err(|_| TelemetryError::new("session span task terminated before start"))?;
    Ok(SessionSpan {
        span,
        finish: Some(finish_tx),
        join: Some(join),
    })
}

async fn start_span_on_parent(
    parent: Arc<dyn TelemetrySpan>,
    name: String,
    attributes: SpanAttributes,
) -> Result<SessionSpan, TelemetryError> {
    let context: Arc<dyn TelemetryContext> = parent;
    start_span_on(context, name, attributes).await
}

/// 类型化会话遥测。
pub struct SessionTelemetry {
    context: Arc<dyn TelemetryContext>,
    memory: Option<Arc<InMemoryTelemetryContext>>,
    exporter: Option<Arc<OtlpExporter>>,
    capture: ContentCapture,
    /// 当前 agent run 的根 span（chat/tool/turn 等子 span 的父上下文）。
    active_root: Mutex<Option<Arc<dyn TelemetrySpan>>>,
    /// 当前 turn 的 span（优先于根 span 作为父上下文）。
    active_turn: Mutex<Option<Arc<dyn TelemetrySpan>>>,
    /// 当前 run 的 W3C trace context（bash `TRACEPARENT`、日志关联）。
    trace_context: Mutex<Option<kiana_telemetry::w3c::TraceContext>>,
    /// 待导出的日志记录（flush 时交给 exporter）。
    logs: Mutex<Vec<LogRecord>>,
}

impl std::fmt::Debug for SessionTelemetry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionTelemetry")
            .field("capture_content", &self.capture.is_enabled())
            .field("has_exporter", &self.exporter.is_some())
            .finish()
    }
}

impl Default for SessionTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTelemetry {
    /// 新建：内存 context + 环境变量决定的内容捕获开关。
    pub fn new() -> Self {
        let memory = Arc::new(InMemoryTelemetryContext::new());
        SessionTelemetry {
            context: memory.clone(),
            memory: Some(memory),
            exporter: None,
            capture: ContentCapture::from_env(),
            active_root: Mutex::new(None),
            active_turn: Mutex::new(None),
            trace_context: Mutex::new(None),
            logs: Mutex::new(Vec::new()),
        }
    }

    /// 注入自定义 context（例如生产环境的 OTel 适配器）。
    pub fn with_context(context: Arc<dyn TelemetryContext>) -> Self {
        SessionTelemetry {
            context,
            memory: None,
            exporter: None,
            capture: ContentCapture::from_env(),
            active_root: Mutex::new(None),
            active_turn: Mutex::new(None),
            trace_context: Mutex::new(None),
            logs: Mutex::new(Vec::new()),
        }
    }

    /// 设置内容捕获（测试或显式开启）。
    pub fn with_capture(mut self, capture: ContentCapture) -> Self {
        self.capture = capture;
        self
    }

    /// 设置 OTLP 导出器；`flush` 时使用。
    pub fn with_exporter(mut self, exporter: Arc<OtlpExporter>) -> Self {
        self.exporter = Some(exporter);
        self
    }

    /// 内容捕获是否开启。
    pub fn capture_content(&self) -> bool {
        self.capture.is_enabled()
    }

    /// 底层 context。
    pub fn context(&self) -> Arc<dyn TelemetryContext> {
        self.context.clone()
    }

    /// 内部内存 store（若使用默认构造）。
    pub fn memory(&self) -> Option<&Arc<InMemoryTelemetryContext>> {
        self.memory.as_ref()
    }

    /// 启动根 span（`invoke_agent`）。
    pub async fn begin_agent_run(
        &self,
        agent_name: impl Into<String>,
    ) -> Result<SessionSpan, TelemetryError> {
        let agent_name = agent_name.into();
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "gen_ai.operation.name".into(),
            AttributeValue::String("invoke_agent".into()),
        );
        attributes.insert(
            "gen_ai.agent.name".into(),
            AttributeValue::String(agent_name.clone()),
        );
        let span = self
            .start_session_span(format!("invoke_agent {agent_name}"), attributes)
            .await?;
        *self
            .active_root
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(span.span().clone());
        *self
            .active_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let id = span.span().span_context_id();
        let derived = kiana_telemetry::w3c::TraceId::from_hex(&id.trace_id)
            .zip(kiana_telemetry::w3c::SpanId::from_hex(&id.span_id));
        *self
            .trace_context
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(match derived {
            Some((trace_id, span_id)) => kiana_telemetry::w3c::TraceContext {
                traceparent: kiana_telemetry::w3c::TraceParent {
                    version: 0,
                    trace_id,
                    span_id,
                    flags: 1,
                },
                tracestate: None,
            },
            // 内存适配器的 span id 不是 W3C hex：生成独立 root，仍保证同一次 run 内
            // 所有 bash 子进程共享同一个 trace id。
            None => kiana_telemetry::w3c::TraceContext::new_root(),
        });
        Ok(span)
    }

    /// 记录一轮的开始（span 名 `kiana.session.turn`）。
    pub async fn begin_turn(
        &self,
        turn_index: u64,
        model: &str,
    ) -> Result<SessionSpan, TelemetryError> {
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "gen_ai.operation.name".into(),
            AttributeValue::String("session_turn".into()),
        );
        attributes.insert(
            "kiana.turn.index".into(),
            AttributeValue::Number(turn_index as f64),
        );
        if !model.is_empty() {
            attributes.insert(
                "gen_ai.request.model".into(),
                AttributeValue::String(model.to_string()),
            );
        }
        let span = self
            .start_session_span(format!("kiana.session.turn {turn_index}"), attributes)
            .await?;
        *self
            .active_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(span.span().clone());
        Ok(span)
    }

    /// 结束当前 turn（清空 turn 父引用，下一次 span 回到 root）。
    pub fn end_turn(&self) {
        *self
            .active_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// 当前 trace 的 W3C 父上下文（根 span 的 trace/span id 能表示为标准 hex 时使用，
    /// 否则回落到独立生成的 root；bash 的 `TRACEPARENT` 与日志关联都用它）。
    pub fn current_trace_context(&self) -> Option<kiana_telemetry::w3c::TraceContext> {
        self.trace_context
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 当前 span 父上下文：优先 turn，其次 agent run 根；都没有时用内存 context。
    fn active_parent(&self) -> Option<Arc<dyn TelemetrySpan>> {
        self.active_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .or_else(|| {
                self.active_root
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            })
    }

    /// 以当前 turn/run 为父启动 span；没有活动 run 时是根 span。
    async fn start_session_span(
        &self,
        name: String,
        attributes: SpanAttributes,
    ) -> Result<SessionSpan, TelemetryError> {
        match self.active_parent() {
            Some(parent) => start_span_on_parent(parent, name, attributes).await,
            None => start_span_on(self.context.clone(), name, attributes).await,
        }
    }

    /// 结束当前 agent run：清空根/轮 span 的父引用（之后的 span 回到独立 root）。
    pub fn end_agent_run(&self) {
        *self
            .active_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *self
            .active_root
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *self
            .trace_context
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// 追加一条结构化日志；`flush()` 时随 traces/metrics 一起导出。
    ///
    /// 日志带上当前 trace/span id（可从 `current_trace_context` 推导）；缓冲上限 1024，
    /// 超出时丢弃最旧记录，避免长会话常驻内存无界增长。
    pub fn record_log(
        &self,
        severity: SeverityNumber,
        severity_text: &str,
        body: impl Into<String>,
        attributes: SpanAttributes,
    ) {
        let trace = self.current_trace_context();
        let record = {
            let mut record = kiana_telemetry::otel::log_record(
                body,
                kiana_telemetry::otel::attributes(&attributes),
                trace
                    .as_ref()
                    .map(|trace| trace.traceparent.trace_id.to_hex()),
                trace
                    .as_ref()
                    .map(|trace| trace.traceparent.span_id.to_hex()),
            );
            record.severity_number = severity;
            record.severity_text = severity_text.to_string();
            record
        };
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if logs.len() >= 1024 {
            logs.remove(0);
        }
        logs.push(record);
    }

    /// 当前缓冲的日志（诊断/测试）。
    pub fn recorded_logs(&self) -> Vec<LogRecord> {
        self.logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics>> {
        self.context.metrics()
    }

    fn record_histogram(&self, name: &str, unit: &str, value: f64, attributes: SpanAttributes) {
        let Some(metrics) = self.metrics() else {
            return;
        };
        let histogram = metrics.histogram(
            name,
            Some(kiana_telemetry::metrics::HistogramOptions {
                instrument: kiana_telemetry::metrics::InstrumentOptions {
                    description: None,
                    unit: Some(unit.to_string()),
                },
                boundaries: None,
            }),
        );
        histogram.record(value, Some(attributes), None);
    }

    fn record_counter(&self, name: &str, value: f64, attributes: SpanAttributes) {
        let Some(metrics) = self.metrics() else {
            return;
        };
        let counter = metrics.counter(
            name,
            Some(kiana_telemetry::metrics::InstrumentOptions {
                description: None,
                unit: None,
            }),
        );
        counter.add(value, Some(attributes));
    }

    /// 记录一轮的 TTFT/TTFM 与总耗时。
    pub async fn record_turn(
        &self,
        turn_index: u64,
        tracker: &TurnTracker,
        stop_reason: &str,
    ) -> Result<(), TelemetryError> {
        let turn = self.begin_turn(turn_index, "").await?;
        turn.span().set_attributes({
            let mut attributes = SpanAttributes::new();
            attributes.insert(
                "kiana.turn.stop_reason".into(),
                AttributeValue::String(stop_reason.to_string()),
            );
            attributes.insert(
                "kiana.turn.duration_ms".into(),
                AttributeValue::Number(tracker.elapsed_ms()),
            );
            attributes
        });
        if let Some(ttft) = tracker.ttft_ms() {
            turn.span().set_attributes({
                let mut attributes = SpanAttributes::new();
                attributes.insert("kiana.turn.ttft_ms".into(), AttributeValue::Number(ttft));
                attributes
            });
            self.record_histogram(names::TURN_TTFT_MS, "ms", ttft, SpanAttributes::new());
        }
        if let Some(ttfm) = tracker.ttfm_ms() {
            turn.span().set_attributes({
                let mut attributes = SpanAttributes::new();
                attributes.insert("kiana.turn.ttfm_ms".into(), AttributeValue::Number(ttfm));
                attributes
            });
            self.record_histogram(names::TURN_TTFM_MS, "ms", ttfm, SpanAttributes::new());
        }
        self.record_counter(names::SESSION_TURNS, 1.0, SpanAttributes::new());
        turn.finish().await;
        self.end_turn();
        Ok(())
    }

    /// 记录一次 API 调用：span `chat <model>` + 延迟/token/cost 指标。
    pub async fn record_api_call(&self, record: &ApiCallRecord) -> Result<(), TelemetryError> {
        // 结构化日志与 span 同源（trace id 由 current_trace_context 提供）。
        {
            let mut attributes = SpanAttributes::new();
            attributes.insert(
                "gen_ai.provider.name".into(),
                AttributeValue::String(record.provider.clone()),
            );
            attributes.insert(
                "gen_ai.request.model".into(),
                AttributeValue::String(record.model.clone()),
            );
            attributes.insert(
                "kiana.api.duration_ms".into(),
                AttributeValue::Number(record.duration_ms),
            );
            self.record_log(
                SeverityNumber::Info,
                "INFO",
                format!("chat {}", record.model),
                attributes,
            );
        }
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "gen_ai.operation.name".into(),
            AttributeValue::String("chat".into()),
        );
        attributes.insert(
            "gen_ai.provider.name".into(),
            AttributeValue::String(record.provider.clone()),
        );
        attributes.insert(
            "gen_ai.request.model".into(),
            AttributeValue::String(record.model.clone()),
        );
        if let Some(response_model) = &record.response_model {
            attributes.insert(
                "gen_ai.response.model".into(),
                AttributeValue::String(response_model.clone()),
            );
        }
        if let Some(stop_reason) = &record.stop_reason {
            attributes.insert(
                "gen_ai.response.finish_reasons".into(),
                AttributeValue::String(stop_reason.clone()),
            );
        }
        attributes.insert(
            "gen_ai.usage.input_tokens".into(),
            AttributeValue::Number(record.usage.input as f64),
        );
        attributes.insert(
            "gen_ai.usage.output_tokens".into(),
            AttributeValue::Number(record.usage.output as f64),
        );
        attributes.insert(
            "gen_ai.usage.cache_read.input_tokens".into(),
            AttributeValue::Number(record.usage.cache_read as f64),
        );
        attributes.insert(
            "gen_ai.usage.cache_creation.input_tokens".into(),
            AttributeValue::Number(record.usage.cache_write as f64),
        );
        attributes.insert(
            "kiana.api.duration_ms".into(),
            AttributeValue::Number(record.duration_ms),
        );
        if let Some(cost_source) = &record.cost_source {
            attributes.insert(
                "kiana.cost.source".into(),
                AttributeValue::String(cost_source.clone()),
            );
        }
        if let Some(ttft) = record.ttft_ms {
            attributes.insert("kiana.api.ttft_ms".into(), AttributeValue::Number(ttft));
        }
        if let Some(ttfm) = record.ttfm_ms {
            attributes.insert("kiana.api.ttfm_ms".into(), AttributeValue::Number(ttfm));
        }
        let span = self
            .start_session_span(format!("chat {}", record.model), attributes)
            .await?;

        self.record_histogram(
            names::API_CALL_DURATION_MS,
            "ms",
            record.duration_ms,
            SpanAttributes::new(),
        );
        for (token_type, value) in [
            ("input", record.usage.input),
            ("output", record.usage.output),
            ("cache_read", record.usage.cache_read),
            ("cache_write", record.usage.cache_write),
        ] {
            let mut attributes = SpanAttributes::new();
            attributes.insert(
                "type".into(),
                AttributeValue::String(token_type.to_string()),
            );
            self.record_counter(names::API_CALL_TOKENS, value as f64, attributes);
        }
        if record.usage.cost.total != 0.0 || record.cost_source.is_some() {
            let mut attributes = SpanAttributes::new();
            attributes.insert(
                "cost_source".into(),
                AttributeValue::String(
                    record
                        .cost_source
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
            );
            self.record_counter(
                names::API_CALL_COST_USD,
                record.usage.cost.total,
                attributes,
            );
        }
        if let Some(error_type) = &record.error_type {
            span.finish_with_error(error_type, "provider call failed")
                .await;
        } else {
            span.finish().await;
        }
        Ok(())
    }

    /// 记录一个 SSE 事件（span event + 计数）。`event_type` 必须是有界类别。
    pub fn record_sse_event(&self, span: Option<&SessionSpan>, event_type: &str) {
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "event.type".into(),
            AttributeValue::String(event_type.to_string()),
        );
        self.record_counter(names::SSE_EVENTS, 1.0, attributes.clone());
        if let Some(span) = span {
            span.span()
                .add_event(&format!("sse.{event_type}"), Some(attributes));
        }
    }

    /// 记录一次工具调用。工具名只进 span 属性；指标用 `tool.kind` 分类。
    pub async fn record_tool_call(&self, record: &ToolCallRecord) -> Result<(), TelemetryError> {
        let kind = names::tool_kind(&record.tool_name);
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "gen_ai.operation.name".into(),
            AttributeValue::String("execute_tool".into()),
        );
        attributes.insert(
            "gen_ai.tool.name".into(),
            AttributeValue::String(record.tool_name.clone()),
        );
        attributes.insert(
            "gen_ai.tool.call.id".into(),
            AttributeValue::String(record.tool_call_id.clone()),
        );
        attributes.insert(
            "kiana.tool.kind".into(),
            AttributeValue::String(kind.to_string()),
        );
        attributes.insert(
            "kiana.tool.status".into(),
            AttributeValue::String(record.status.as_str().to_string()),
        );
        attributes.insert(
            "kiana.tool.duration_ms".into(),
            AttributeValue::Number(record.duration_ms),
        );
        if let Some(error_type) = &record.error_type {
            attributes.insert(
                "kiana.tool.error_type".into(),
                AttributeValue::String(error_type.clone()),
            );
        }
        let span = self
            .start_session_span(format!("execute_tool {}", record.tool_name), attributes)
            .await?;

        let mut metric_attributes = SpanAttributes::new();
        metric_attributes.insert(
            "status".into(),
            AttributeValue::String(record.status.as_str().to_string()),
        );
        metric_attributes.insert("tool.kind".into(), AttributeValue::String(kind.to_string()));
        self.record_counter(names::TOOL_CALLS, 1.0, metric_attributes.clone());
        self.record_histogram(
            names::TOOL_CALL_DURATION_MS,
            "ms",
            record.duration_ms,
            metric_attributes,
        );
        if let Some(error_type) = &record.error_type {
            span.finish_with_error(error_type, "tool execution failed")
                .await;
        } else {
            span.finish().await;
        }
        Ok(())
    }

    /// 记录一次压缩。
    pub async fn record_compaction(&self, record: &CompactionRecord) -> Result<(), TelemetryError> {
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "kiana.compaction.trigger".into(),
            AttributeValue::String(record.trigger.clone()),
        );
        attributes.insert(
            "kiana.compaction.input_tokens".into(),
            AttributeValue::Number(record.input_tokens as f64),
        );
        attributes.insert(
            "kiana.compaction.output_tokens".into(),
            AttributeValue::Number(record.output_tokens as f64),
        );
        attributes.insert(
            "kiana.compaction.outcome".into(),
            AttributeValue::String(record.outcome.clone()),
        );
        let span = self
            .start_session_span("kiana.session.compaction".to_string(), attributes)
            .await?;
        let mut metric_attributes = SpanAttributes::new();
        metric_attributes.insert(
            "trigger".into(),
            AttributeValue::String(record.trigger.clone()),
        );
        metric_attributes.insert(
            "outcome".into(),
            AttributeValue::String(record.outcome.clone()),
        );
        self.record_counter(names::COMPACTIONS, 1.0, metric_attributes);
        self.record_histogram(
            names::COMPACTION_DURATION_MS,
            "ms",
            record.duration_ms,
            SpanAttributes::new(),
        );
        span.finish().await;
        Ok(())
    }

    /// 记录一次 hook 执行。hook 名只进 span 属性，指标只带事件名。
    pub async fn record_hook_run(&self, record: &HookRunRecord) -> Result<(), TelemetryError> {
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "kiana.hook.event".into(),
            AttributeValue::String(record.hook_event.clone()),
        );
        attributes.insert(
            "kiana.hook.outcome".into(),
            AttributeValue::String(record.outcome.clone()),
        );
        if let Some(name) = &record.hook_name {
            attributes.insert(
                "kiana.hook.name".into(),
                AttributeValue::String(name.clone()),
            );
        }
        let span = self
            .start_session_span("kiana.hook.run".to_string(), attributes)
            .await?;
        let mut metric_attributes = SpanAttributes::new();
        metric_attributes.insert(
            "hook.event".into(),
            AttributeValue::String(record.hook_event.clone()),
        );
        metric_attributes.insert(
            "outcome".into(),
            AttributeValue::String(record.outcome.clone()),
        );
        self.record_counter(names::HOOK_RUNS, 1.0, metric_attributes);
        self.record_histogram(
            names::HOOK_RUN_DURATION_MS,
            "ms",
            record.duration_ms,
            SpanAttributes::new(),
        );
        span.finish().await;
        Ok(())
    }

    /// 记录启动阶段耗时。
    pub async fn record_startup_phase(
        &self,
        phase: &str,
        duration_ms: f64,
    ) -> Result<(), TelemetryError> {
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "kiana.startup.phase".into(),
            AttributeValue::String(phase.to_string()),
        );
        attributes.insert(
            "kiana.startup.duration_ms".into(),
            AttributeValue::Number(duration_ms),
        );
        let span = self
            .start_session_span(format!("kiana.session.startup {phase}"), attributes)
            .await?;
        let mut metric_attributes = SpanAttributes::new();
        metric_attributes.insert("phase".into(), AttributeValue::String(phase.to_string()));
        self.record_histogram(
            names::STARTUP_PHASE_DURATION_MS,
            "ms",
            duration_ms,
            metric_attributes,
        );
        span.finish().await;
        Ok(())
    }

    /// 把 prompt / 工具参数写进 span——**只有捕获开启时才会写入**。
    pub fn set_content_attributes(
        &self,
        span: &SessionSpan,
        prompt: Option<&str>,
        tool_arguments: Option<&str>,
    ) {
        if !self.capture.is_enabled() {
            return;
        }
        let mut attributes = SpanAttributes::new();
        if let Some(prompt) = prompt {
            attributes.insert(
                "gen_ai.input.messages".into(),
                AttributeValue::String(prompt.to_string()),
            );
        }
        if let Some(tool_arguments) = tool_arguments {
            attributes.insert(
                "gen_ai.tool.call.arguments".into(),
                AttributeValue::String(tool_arguments.to_string()),
            );
        }
        if !attributes.is_empty() {
            span.span().set_attributes(attributes);
        }
    }

    /// 收集内部记录的全部已结算 span（仅默认内存 context 可用）。
    pub async fn recorded_spans(&self) -> Vec<kiana_telemetry::index::StoredSpan> {
        let Some(memory) = &self.memory else {
            return Vec::new();
        };
        let page = match memory
            .find_traces(Some(TraceQuery {
                limit: Some(100),
                ..TraceQuery::default()
            }))
            .await
        {
            Ok(page) => page,
            Err(_) => return Vec::new(),
        };
        page.traces
            .into_iter()
            .flat_map(|trace| trace.spans)
            .collect()
    }

    /// 把内部记录交给 exporter 并 flush。没有 exporter 时是 no-op。
    pub async fn flush(&self) -> ExportOutcome {
        let Some(exporter) = &self.exporter else {
            return ExportOutcome::default();
        };
        let spans = self.recorded_spans().await;
        if !spans.is_empty() {
            exporter.enqueue_spans(spans);
        }
        // 指标在内存 TelemetryMetrics 里累积；flush 时转交 exporter，否则 /v1/metrics 永远
        // 没有数据。日志同理（record_log 缓冲）。
        if let Some(memory) = &self.memory {
            let metrics = memory.get_metrics();
            if !metrics.is_empty() {
                exporter.enqueue_metrics(metrics);
            }
        }
        let logs = std::mem::take(
            &mut *self
                .logs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if !logs.is_empty() {
            exporter.enqueue_logs(logs);
        }
        exporter.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_ai::types::{StopReason, UsageCost};

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            total_tokens: input + output,
            cost: UsageCost {
                total: 0.25,
                ..UsageCost::default()
            },
            ..Usage::default()
        }
    }

    #[tokio::test]
    async fn records_turn_api_tool_compaction_hook_and_startup() {
        let telemetry = SessionTelemetry::new().with_capture(ContentCapture::disabled());
        let root = telemetry.begin_agent_run("kiana").await.expect("root");

        let mut tracker = TurnTracker::new();
        tracker.record_token(false);
        tracker.record_token(true);
        telemetry
            .record_turn(0, &tracker, "toolUse")
            .await
            .expect("turn");

        let chat = telemetry
            .record_api_call(&ApiCallRecord {
                provider: "openai".into(),
                model: "gpt-5".into(),
                response_model: Some("gpt-5-2026".into()),
                duration_ms: 120.0,
                ttft_ms: Some(20.0),
                ttfm_ms: Some(25.0),
                usage: usage(100, 25),
                cost_source: Some("canonical".into()),
                stop_reason: Some("toolUse".into()),
                error_type: None,
            })
            .await;
        chat.expect("api call");

        let tool_span = root
            .start_child("tool parent", SpanAttributes::new())
            .await
            .expect("child");
        telemetry.record_sse_event(Some(&tool_span), "delta");
        tool_span.finish().await;
        telemetry
            .record_tool_call(&ToolCallRecord {
                tool_name: "mcp__github__create_issue".into(),
                tool_call_id: "call_1".into(),
                status: ToolStatus::Ok,
                duration_ms: 30.0,
                error_type: None,
            })
            .await
            .expect("tool");
        telemetry
            .record_compaction(&CompactionRecord {
                trigger: "auto".into(),
                duration_ms: 50.0,
                input_tokens: 10_000,
                output_tokens: 2_000,
                outcome: "ok".into(),
            })
            .await
            .expect("compaction");
        telemetry
            .record_hook_run(&HookRunRecord {
                hook_event: "PreToolUse".into(),
                hook_name: Some("lint".into()),
                duration_ms: 5.0,
                outcome: "allow".into(),
            })
            .await
            .expect("hook");
        telemetry
            .record_startup_phase("config", 12.0)
            .await
            .expect("startup");
        root.finish().await;

        let spans = telemetry.recorded_spans().await;
        assert!(spans.len() >= 8, "spans: {spans:?}");
        let chat_span = spans
            .iter()
            .find(|span| span.name == "chat gpt-5")
            .expect("chat span");
        assert_eq!(
            chat_span.attributes.get("gen_ai.usage.input_tokens"),
            Some(&AttributeValue::Number(100.0))
        );
        let tool_span = spans
            .iter()
            .find(|span| span.name == "execute_tool mcp__github__create_issue")
            .expect("tool span");
        assert_eq!(
            tool_span.attributes.get("kiana.tool.kind"),
            Some(&AttributeValue::String("mcp".into()))
        );

        // 指标：工具标签不含工具名，只有 tool.kind 三分类。
        let tool_metrics: Vec<_> = telemetry
            .memory()
            .expect("memory")
            .get_metrics()
            .into_iter()
            .filter(|measurement| measurement.name == names::TOOL_CALLS)
            .collect();
        assert_eq!(tool_metrics.len(), 1);
        assert!(!tool_metrics[0].attributes.contains_key("tool.name"));
        assert_eq!(
            tool_metrics[0].attributes.get("tool.kind"),
            Some(&AttributeValue::String("mcp".into()))
        );
        assert_eq!(
            tool_metrics[0].attributes.get("status"),
            Some(&AttributeValue::String("ok".into()))
        );

        // 捕获关闭：没有任何 gen_ai.input.messages / tool arguments。
        assert!(!spans.iter().any(|span| {
            span.attributes.contains_key("gen_ai.input.messages")
                || span.attributes.contains_key("gen_ai.tool.call.arguments")
        }));
    }

    #[tokio::test]
    async fn content_capture_only_writes_when_enabled() {
        for (capture, expect) in [
            (ContentCapture::disabled(), false),
            (ContentCapture::enabled(), true),
        ] {
            let telemetry = SessionTelemetry::new().with_capture(capture);
            let span = telemetry.begin_agent_run("kiana").await.expect("root");
            telemetry.set_content_attributes(&span, Some("hello prompt"), Some("{\"x\":1}"));
            span.finish().await;
            let spans = telemetry.recorded_spans().await;
            let has_prompt = spans
                .iter()
                .any(|span| span.attributes.contains_key("gen_ai.input.messages"));
            assert_eq!(has_prompt, expect, "capture={capture:?}");
        }
    }

    #[tokio::test]
    async fn error_spans_are_marked_and_still_settled() {
        let telemetry = SessionTelemetry::new();
        let span = telemetry.begin_agent_run("kiana").await.expect("root");
        span.finish_with_error("ProviderError", "boom").await;
        let spans = telemetry.recorded_spans().await;
        assert_eq!(spans.len(), 1);
        assert!(matches!(spans[0].status, SpanStatus::Error { .. }));
    }

    #[tokio::test]
    async fn flush_without_exporter_is_a_noop() {
        let telemetry = SessionTelemetry::new();
        let outcome = telemetry.flush().await;
        assert_eq!(outcome, ExportOutcome::default());
    }

    #[test]
    fn turn_tracker_orders_ttft_and_ttfm() {
        let mut tracker = TurnTracker::new();
        assert_eq!(tracker.ttft_ms(), None);
        tracker.record_token(false);
        let ttft = tracker.ttft_ms().expect("ttft");
        assert!(ttft >= 0.0);
        assert_eq!(tracker.ttfm_ms(), None);
        tracker.record_token(true);
        let ttfm = tracker.ttfm_ms().expect("ttfm");
        assert!(ttfm >= ttft);
        assert!(tracker.elapsed_ms() >= 0.0);
    }

    #[test]
    fn stop_reason_and_usage_helpers_compile() {
        let _ = StopReason::Stop;
        let telemetry = SessionTelemetry::new();
        assert!(!telemetry.capture_content());
        let explicit = SessionTelemetry::new().with_capture(ContentCapture::enabled());
        assert!(explicit.capture_content());
    }

    #[test]
    fn a_custom_context_is_supported() {
        let memory: Arc<dyn TelemetryContext> = Arc::new(InMemoryTelemetryContext::new());
        let telemetry = SessionTelemetry::with_context(memory);
        assert!(telemetry.memory().is_none());
        let _ = telemetry.context();
    }
}
