# 待核实与未修复项

本文件由 `tools/collect_uncertain.py` 从各批次工作流 journal 汇总生成；内容是**注释作者主动标记的待核实项**与**修复代理明确跳过的问题**，不代表代码缺陷。审阅发现的问题大部分已在同批次修复，未逐条列出。部分条目在后续人工修正中已处理，请以当前源码与注释为准。

## 1. 未修复的问题（fixer 明确跳过）

### u002-kiana-agent-core-large

- 为 ToolcallDeltaForNonToolCall 的 toolcall_start → toolcall_end → toolcall_delta 路径新增单测：审阅建议要求补测，但本任务绝对规则明确「不得改动测试」，故未添加（现有 tests/proxy.rs 只覆盖非 toolCall 下标/空洞上的 toolcall_delta，未覆盖 start→end→delta；见 tests/proxy.rs:833-845 与 1388-1394）。
- 未运行 cargo build/test/fmt/clippy 与任何写文件命令，符合规则；因此未做编译/文档验证。

### u115-kiana-coding-agent-group

- kiana/.rs 删除：文件已不存在，无需操作——检查 /home/ctyun/code/kiana_rev/kiana/ 目录确认无 .rs 文件，全仓库扫描未发现名为 .rs 或 0 字节的 .rs 文件；/home/ctyun/pi-rs-work/tools/check_comment_only_diff.py 已 exit 0（788 个修改的 .rs 全部为注释/空行改动，报告：通过：所有 .rs 变更仅涉及注释行）

### u166-kiana-durable-large

- events.rs 中 1006/1522/1532/1620 的 "kiana.generation" 字面量改为引用 GENERATION_TASK_KIND：这是代码改动，受“只允许插入或修改注释行”约束，未执行；已在 GENERATION_TASK_KIND 的文档中列全同步点并交由主代理处理。

### u221-kiana-export-root

- lib.rs:7 的歧义链接（基线已有，审阅明确不属本单元，未动）；tool_renderer.rs:20 同理。
- 未运行 cargo build/test/fmt/clippy（任务禁止）；注释纯度用 Edit 仅作用于 //! 行保证，未用工具脚本（tools/check_comment_only_diff.py 不存在于仓库）。

### u241-kiana-extensions-group

- transpile.rs:631 测试注释 'inner BOMs are legal identifier characters and must survive' 含与第 81 行相同的『标识符字符』不准确断言，但它是既有注释（不在本次审阅清单内），按『不扩大范围、保留已有注释』规则未改动；如需修改建议由主代理确认后处理

### u247-kiana-interactive-large

- 无法运行 cargo check -p kiana-interactive --all-targets / cargo doc 验证：依赖 crate kiana-resources 存在与本次改动无关的既有编译错误 — crates/kiana-resources/src/package/paths.rs:1067-1071 有 5 条 //! 内部文档注释被写在文件中部（mod tests 之前、其他 item 之后），触发 E0759 expected outer doc comment。该文件不在本单元负责范围内，按绝对规则未改动，请主代理处理。

### u253-kiana-interactive-large

- kiana-interactive 其余 6 个预先存在的 rustdoc 错误（custom_message.rs:16 `CustomEntryComponent`、earendil_announcement.rs:36 `has_image`、settings_selector.rs:11-12 私有项 `js_number_string`/`parse_int_js` 链接、interactive_mode.rs:1006 `InteractiveModeInner`、tui_renderer.rs:602 `TuiBase::focused_component`）——不在本单元指定的两项修复范围内，且不属于本注释修复者的修改权限（虽属注释行修改，但任务要求『不扩大范围』，应交主代理决策）。因此 `RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-interactive --no-deps` 在修复指定两项后仍有这些错误（exit 非 0），错误清单与修复前完全一致，未新增。

### u256-kiana-interactive-large

- armin.rs:272 fmt 问题 — 现状已符合要求：EffectState 只有枚举级 ///、无变体级 ///，cargo fmt -p kiana-interactive -- --check 已通过，无需改动

### u262-kiana-interactive-large

- paths.rs:1067 [code-change] — 无需修复：该 //! 块已不存在（现为普通 // 注释，1072 行 #[cfg(test)] 前），cargo check -p kiana-interactive --all-targets 与 cargo doc -p kiana-resources（-D warnings）均通过。
- armin.rs:272 [style] — 无需修复：磁盘上该文件已是修复后状态（enum EffectState 多行变体保留、289-291 行已有防 rustfmt 折叠的警告注释），cargo fmt -p kiana-interactive -- --check 零 diff。
- kiana-interactive 其余文件的 rustdoc 失败（settings_selector.rs / interactive_mode.rs / tui_renderer.rs / custom_message.rs / earendil_announcement.rs 的失效链接与私有项链接）— 不在本单元问题清单内，按要求不扩大范围，仅在此报告。

### u308-kiana-resources-large

- crates/kiana-resources/src/package/paths.rs:1067 [style] — 磁盘上已是修复状态：第 1067-1071 行已是普通 // 注释（cat -A 字节级核对），全文件第 53 行之后不再有任何 //! 行，E0753 触发条件已不存在，无需改动。该审阅意见所述错误状态已过时。

### u311-kiana-resources-group

- crates/kiana-resources/src/package/paths.rs:1067 [style] E0753 from trailing //! block before `mod tests` — not present in the current tree: lines 1067-1071 are already plain `//` comments, the only `//!` lines (1-53) sit in the file head before the first item, which is legal. Verified `cargo check -p kiana-resources` (workspace /home/ctyun/code/kiana_rev/kiana/Cargo.toml) compiles clean and `cargo fmt -p kiana-resources -- --check` reports no diffs. The issue was either filed against an older revision or already fixed by a prior step; no edit made, per the comments-only rule.

### u321-kiana-rpc-print-large

- u321-kiana-rpc-print-large: paths.rs:1067 的 5 行 `//!` 误置问题在磁盘上已是修复后状态（已改写为 `#[cfg(test)]` 前的普通 `//` 注释），无可修内容，未做任何编辑。

### u322-kiana-rpc-print-large

- rpc_mode.rs:915 的 [`crate::signal`]（私有方法 register_signal_handlers 的 /// 上的链接）未改：审查清单未列出该处，且 rustdoc 不对私有项文档发 private_intra_doc_links 警告；按“不扩大范围”保留原样。

### u323-kiana-rpc-print-large

- print_mode.rs:36 / rpc_mode.rs:33 的 [`crate::signal`] 链接修复——越界提示明示『请路由给对应单元处理』，不属本单元（rpc_types.rs）。且经核实定位不准：print_mode.rs:36 与 rpc_mode.rs:33 已是行内代码 `crate::signal`（反引号，非链接），无需修改；本 crate 中唯一真正的 [`crate::signal`] intra-doc 链接在 rpc_mode.rs:915（lib.rs:72 `mod signal;` 为私有，会触发 rustdoc::private_intra_doc_links）。建议主代理把该行路由给 rpc_mode 单元处理。

### u329-kiana-session-group

- paths.rs:1067 中部 //! 块（E0753）：核查发现该问题在当前文件中已不存在——//! 全部在文件头 1-53 行，测试前置说明已是普通 // 注释，无需修改（可能已被其他单元或主代理先行修复）

### u346-kiana-theme-group

- system_theme.rs:50,51,1371,1384,1387 — 无需修改：当前文件中这五处区间已全部写成反引号形式 `[0,1]`/`[0,2]`（grep 确认，与报告要求的修法一致），可能此前已被修复。唯一未加反引号的 [0,2] 在 system_theme.rs:1530 的普通 // 行注释里，rustdoc 不解析，不产生文档警告。若 rustdoc 报错仍在，请主代理重跑 RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-theme --no-deps 确认

## 2. 注释作者标记的待核实项

### u000-kiana-agent-core-large

- reset() 的 active_run 检查与 state 修改分两次取锁：并发 prompt/continue_ 可在检查后、修改前登记 run，随后 reset 仍会清空 transcript、把 is_streaming 置 false 并清空两个队列（run 仍在执行）。TS 单线程模型下不会出现；源码未修改，仅记录。
- continue_() 在 assistant 尾部时先 drain steering/follow-up，再由 begin_run 的 claim_run 占位；若另一线程在入口预检与 claim 之间抢先登记 run，drain 出的消息会随 conflict 错误一起丢失（调用方只看到 ContinueWhileProcessing）。仅并发路径存在；未修改源码。
- AgentError::AlreadyProcessing 与 AgentError::Loop 在当前工作区没有任何构造点（Loop 仅有 #[from]，代码路径把 AgentLoopError 转成了 String），是否为 API 对齐保留需要确认；注释按原样保留。
- AgentOptions.convert_to_llm 原有文档把私有函数 default_convert_to_llm 写成 intra-doc 链接（公开文档链接私有项会触发 rustdoc::private_intra_doc_links）；已改为行内代码，语义不变，这是本次唯一改动的既有注释行。
- begin_run 中 poll_once 返回 Ready 的快速路径按当前 executor（run_agent_loop 先 emit agent_start，event_sink 首个事件后 YieldOnce）似乎不会走到，注释按“防御性分支”描述；未运行编译/测试验证。

### u001-kiana-agent-core-large

- AgentLoopError::StreamEnded 的可达性：注释依据 crates/kiana-ai/src/event_stream.rs 的 result() 实现（仅在结果通道关闭时返回 Err；end_without_result 路径会永远 pending）撰写，未运行测试验证，也未声称该分支绝对不可达。
- prepare_tool_call_arguments 的“None + 闭包把 &mut 替换成非对象”分支：依据代码（_ => tool_call.clone() 与 None 分支使用 original.arguments）确认非对象值会被丢弃，并据此改写了原有注释；该边界在 TS 中没有直接对应，未实测。
- agent_loop/agent_loop_continue 的 tokio::spawn 任务内 panic 会终止任务、流不会 end 的描述：来自任务体结构与 tokio 语义，仓库内没有针对 sink/convertToLlm panic 的测试。
- pending_declares_no_tool_changes 的 `_ => true` 分支“不可达”：依据唯一调用点只传 role=="system" 的消息推断，没有测试直接覆盖该分支。
- AgentEventStream::recv 的“等待中的 future 被 drop 会导致下一条事件丢失”：依据 waiting 队列 + oneshot 投递实现推断，未写测试验证。
- “并行批次在同一任务里协作式轮询、工具内部同步阻塞会拖住整批”：依据 join_all_ordered 的 poll_fn 实现，属于调度事实而非实测性能结论。
- custom_system_projection 对字段类型不符时的静默降级（content 非字符串→空文本、sections/toolsAdded 反序列化失败→None、timestamp 非整数→0）逐项未测试，仅由代码可证。

### u002-kiana-agent-core-large

- 模块文档称调用方为 kiana-core/kiana-session，但全仓 grep 只在 tests/proxy.rs 找到 stream_proxy 的使用，未发现生产调用方；原文未改动，是否为规划中的装配点待核实。
- 代码中 `matches!(status.as_u16(), 204 | 205 | 304)` 的 304 分支不可达（304 已被 !is_success 拦截，测试 reports_the_null_body_type_error_for_no_content_responses 锁定其文案为状态行）。按规则只改注释、未动代码，建议维护者确认是否删除该分支。
- ProxyStreamFn 在仓库内只有定义、没有任何使用点；其与 types::StreamFn 的签名差异已据 types.rs:656-666 改写注释，但是否保留该别名待维护者确认。
- 任务禁止运行 cargo fmt --check；本次给 ProxyAssistantMessageEvent 的结构体变体新增了 ///，依据 kiana-ai/src/types.rs 同类带字段属性的多行变体判断不会触发 rustfmt 折叠，仍需在验证阶段实际确认。
- spawn_stream_task 在专用线程 runtime 创建失败时静默丢弃 future、消费侧永久等待，是源码阅读结论，没有测试覆盖。
- 未运行 cargo doc；hyper::ext::ReasonPhrase 等外部 intra-doc 链接未实际验证，但目标均为公开 API。

### u003-kiana-agent-core-group

- types.rs 的既有公开文档仍链接私有项：模块文档指向 [`serialize_js_json`]，AgentToolResult::details 指向 [`json_value_is_null`]、usage 指向 [`serialize_js_json`]。按“保留原注释”规则未改；RUSTDOCFLAGS='-D warnings' cargo doc 可能报 private_intra_doc_links，需确认是否为改动前已有的基线警告。
- AgentEvent 的 AgentEnd.messages、MessageStart.message、MessageEnd.message 位于单行变体内，单独加 /// 必须改动代码排版，故未加；目前由变体级文档覆盖这三个字段的含义。
- AgentTool.label 的“供 UI 展示”按 TS 同名语义描述；仓库内只验证到 kiana-tools 给 label 赋值、kiana-extensions 的 ctx.tools 把它投影给扩展脚本，未见到本 crate 内的 UI 消费。
- AgentMessage 的 untagged 反序列化结论“满足标准字段形状的自定义对象会被读成 Standard”由 derive 顺序与 kiana-ai 结构体未设置 deny_unknown_fields 推出，没有专门测试锁定该边界。
- AgentTool.replay 的业务语义（Never/Safe 何时可重放）未核实：本 crate 的 agent loop 不读取它，只有 kiana-extensions 原样投影为字符串；文档因此只陈述序列化字面量。
- 未运行 cargo check/doc/fmt/clippy（任务禁止）；新增 /// 后 rustfmt 是否改变多行枚举/结构体排版无法本地验证，请用 cargo fmt --all -- --check 复核。
- 仓库中找不到 docs/COMMENT_PROGRESS.md 提到的基线快照 /home/ctyun/code/kiana_rev/pi-rs-work/comment-baseline/kiana，无法运行 comment-only diff 脚本；自检依赖每次 Edit 的新旧文本对比（new_string 仅新增注释行，并已把误展开的 3 个单行枚举变体恢复原排版）。

### u004-kiana-agent-core-group

- AnyModel 标记分支疑似不可达：/home/ctyun/code/kiana_rev/kiana/crates/kiana-ai/src/types.rs:829 的手写 `impl Serialize for AnyModel` 对 `Remote` 直接 `json.serialize(serializer)`，不会产生 newtype variant；而 wire_keys 的拦截条件是 `name == "AnyModel" && variant == "Remote"`。/home/ctyun/code/kiana_rev/kiana/crates/kiana-config/tests/remote_catalog_provider.rs:938 断言 `to_wire_value(&AnyModel::Remote{..})` 等于原始对象（无内层包装），与“该分支不触发”一致。若属实，`REMOTE_MODEL_JSON_KEY` 的包装与 `reorder_wire_keys_inner` 的解包分支当前可能不会被真实调用产生；未运行测试验证。
- `TagRemote::serialize_newtype_variant` 对其余 newtype 变体只序列化内层值、丢弃外部变体名，与 `serde_json::to_value` 的外部标签形状不同。仓库内是否已有被 wire 序列化的派生枚举会走到这条路径未证实（grep 只找到 AnyModel 为手写 Serialize）。
- 既有公开文档中 `reorder_wire_keys` 链接了私有常量 `ASSISTANT_BASE_ORDER`/`ASSISTANT_TRAILING_ORDER`（本文件约 315 行）与 `MODEL_FROM_JSON_ORDER`（约 328 行）。按 COMMENT_STYLE 可能触发 `rustdoc::private_intra_doc_links`；禁止运行 cargo doc，未能确认，故保留原链接未改。
- 模块文档新增的“重排是幂等的”由代码推得（输出顺序只由键集与模板决定），仓库内没有专门测试直接锁定该性质。
- `reorder_model_object` 的条件 `has_type || !template_has_type` 允许“运行时对象带 `type`、目录模板省略 `type`”时套用模板；内置目录中哪些条目显式写 `type` 未逐条核对，该分支行为依赖 /home/ctyun/code/kiana_rev/kiana/crates/kiana-rpc-print/tests/json_event_parity.rs 的目录级逐字节测试。

### u005-kiana-agent-core-root

- 原注释称上层包含 `kiana-durable`，但 crates/kiana-durable/Cargo.toml 没有 `kiana-agent-core`，ARCHITECTURE.md §2.2 的边列表也没有该边；已据此把该句改为 kiana-session/kiana-tools/kiana-core 等。若存在尚未落地的计划依赖，请复核这一处改写。
- ARCHITECTURE.md §2.2 只列 `kiana-agent-core -> kiana-ai`，但 Cargo.toml 实际还直接依赖 `kiana-json`（types.rs 使用）以及 `reqwest`/`hyper`（proxy.rs 使用）；文档按 Cargo.toml 表述，§2.2 可能未同步。
- 原句“公开面与 index.ts 的再导出面一一对应”予以保留；严格说本 crate 还公开了 TS 没有的 `wire_keys` 模块（供 print/会话落盘的键序还原），按字面理解略有出入，但该句紧接“见 [`index`]”，指的是 barrel 模块。
- 未运行 cargo doc/check（任务禁止）；`agent::AgentOptions::convert_to_llm` 等字段级 intra-doc 链接未实机验证，按 rustdoc 规则应可解析，且目标均为公开项。
- 公开项计数口径：39 = 8 个 pub mod + 10 个 `pub use agent::*` + 1 个 `pub use identity::*` + 20 个 `pub use types::*`；再导出项的文档继承自原定义。仓库中不存在 tools/doc_inventory.py（tools/ 目录缺失），无法与主代理统计脚本对齐口径。
- “run 内部失败不从 prompt 返回值暴露”来自源码路径（handle_run_failure 合成消息后 execute_run 返回 Ok），未用测试交叉验证（任务禁止运行测试）。
- “agent_loop_continue 必须在 tokio 运行时内调用”依据其函数体使用 tokio::spawn；模块文档只对 agent_loop 明确写了这一要求，agent_loop_continue 的该约束属于从代码读出的结论。

### u006-kiana-ai-large

- 预置缺陷（未修改）：`/// createClient(...)` 这条文档注释实际挂在 `merge_headers`（约 1271 行）上，而真正的 `create_client` 原先没有文档；按规则未移动/改写原注释，只在 `create_client` 前新增了正确文档。
- `deferred_tool_placeholder` 的协议动机（为何 native tool changes 必须附带该占位工具）在 spec 中未说明，注释只陈述可观察字段与发送分支，未推断原因。
- 统计口径：public_total=17 采用 docs/COMMENT_PROGRESS.md 的定义（只计 pub fn/struct/type 等，不计字段）；若计入 27 个公开字段，则公开项为 44，且 44/44 均有 `///`。
- 任务禁止运行 cargo build/test/fmt/clippy/doc，因此 Rustdoc 链接有效性与 rustfmt 格式未实机验证；链接仅使用同 crate 可见的公开项，并参照了本 crate 已有写法（如 bedrock_converse_stream.rs 的 `CacheRetention::Long` 链接）。
- 文件头「SDK 的 .asResponse() 只负责发请求并返回原始响应」依据 spec/ai-api-anthropic-openai.md §2.9.8（自定义 SSE 解码）与源码结构，未对照实际安装的 SDK 源码复核。
- `get_beta_features` 的注释说「options.headers 中该头为 null 时返回空列表」，依据 ProviderHeaders=IndexMap<String, Option<String>> 的 None 语义与代码分支，未在测试中逐路径复核。

### u007-kiana-ai-large

- EventStreamDecoder 对 event-stream 头类型 8（timestamp）用 f64::from_bits 解释 8 字节，而 Smithy 规范中 timestamp 是 epoch 毫秒整数；Bedrock 实测帧只用 string 头，故未观察到影响，是否与 TS SDK 的转换逐位一致未核实。
- resolve_profile_credentials/assume_role 的 duration_seconds 用严格 i64::parse，而源码注释写的是 parseInt：TS 对 '7200abc' 会得到 7200，Rust 解析失败会回落到 3600。属潜在行为差异，未修改代码。
- run_stream 的 observed_raw_response 只写不读；TS 源码里 `!observedRawResponse && onResponse` 的兜底回调分支在 Rust 中不存在（当前直接在首帧成功后回调 on_response，可能已等价），是否完全等价未核实。
- js_parse_int 的 0x 分支用 u128 解析，超出 128 位返回 None；JS parseInt 会得到大浮点数或 Infinity。该函数只用于 AWS_MAX_ATTEMPTS，实际影响未核实。
- 本文件之外的调用约定（例如 HttpClient 的 execute 是否在所有实现里都尊重取消令牌）未逐一核对，文档中关于“取消在下一次网络调用/读块时生效”的表述仅基于本文件与 kiana_messages::next_chunk 的实现。

### u008-kiana-ai-large

- parse_openai_body 用 `media_type.ends_with("+json")` 判定 JSON；仓库内没有 TS 源码，无法核对原 TS/SDK 是 ends_with 还是 includes，注释只按现有代码事实描述。
- post_json 的 204 分支保留原注释「`fetch` 也拒绝读」；仓库内无证据支持（fetch 规范通常对空 body 返回空串），本次未改写，仅记录。
- openai_status_error 的测试注释称黄金值来自真实 openai SDK 7.19.0；本次未运行测试复现，注释依据测试断言与代码路径撰写。
- TS timeoutError 的 `name = "TimeoutError"` 依据 spec/ai-api-other.md §2.2，未与 TS 源码逐字核对。

### u009-kiana-ai-large

- 既有注释 `resolve_grammar_constrained_sampling` 的文档里用 [`grammar_variants_of`] 链接到私有函数；公开文档链接私有项可能触发 rustdoc `private_intra_doc_links` 警告，但任务禁止运行 cargo doc，未能验证。按「只在矛盾时改写」的规则未改动。
- `make_json_schema_node_strict` 是递归实现、无深度上限（与 TS 一致）。深嵌套 schema 是否会造成 Rust 栈溢出，取决于上游工具 schema 的来源与深度约束，本文件与测试无法确认；注释只写了「不设上限」这一事实。
- `UNSUPPORTED_STRICT_SCHEMA_KEYS` 在 TS 中是模块私有常量，Rust 侧为 `pub`；这样暴露的原因未知。已确认 crate 内除本文件外无引用。
- `js_truthy` 对 `serde_json::Number` 调用 `as_f64().is_some_and(|n| n != 0.0 && !n.is_nan())`：`Number` 通常无法表示 NaN，`is_nan()` 分支可能不可达；未在不同 feature（如 arbitrary_precision）下验证，注释只描述 JS 真值语义意图。
- `json_string_inner` 依赖 `kiana_json` 的字符串序列化始终返回带首尾 ASCII 引号的字面量；该保证来自已阅读的 kiana-json 实现与文档，未运行测试独立验证。
- fixture `tests/fixtures/api-shared/constrained_sampling.json` 的生成方式（脚本生成还是手工捕获）在仓库中未找到说明；注释只声称它锁定错误文本与序列化结果，未声称生成方式。

### u010-kiana-ai-large

- uses_google_thinking_level 用手写的 id.find 只检查每个模式首次出现的位置，而 TS 的正则 .test() 会继续搜索后续位置；对“同一 id 中先出现不匹配片段、后出现匹配片段”的病态输入，两者可能给出不同结果。未找到覆盖该情形的测试，未改代码。
- handle_google_chunk 用 part.get("functionCall") 判断，若 JSON 里显式为 null 会进入分支并生成 name 为空的 tool call；TS 的 if (part.functionCall) 对 null 为假值会跳过。服务端是否会返回 null 无证据，仅记录。
- request_stream 从响应头取 content-type 用精确小写键 .get("content-type")；默认 reqwest 客户端会小写化头名，但自定义 HttpClient（包括测试里的 CaptureClient）不保证，可能把 JSON 错误体当成非 JSON。已在注释中写明前提，未改代码。
- retry_google_request 完全未读取 StreamOptions.max_retry_delay_ms。由于 GoogleFailure 不携带响应头，不会产生 server-requested delay，当前与 TS 的可观察行为一致，但该字段确实被忽略。
- patch_http_options 对非字符串请求头值提前用 JS String() 字符串化；TS 在 fetch 发送时才转换。若请求级 headers 里出现对象/数组，结果可能不同（对象会变成 "[object Object]"）。
- initial_system_instruction 的原注释称“供 google-generative-ai / google-vertex 复用”，但两个适配器实际在 build_params 内自行调用 get_initial_system_message + get_system_message_text，并未调用它；保留了原句并补充事实说明，未改代码。
- tTool / partToMldev 等转换器声称逐条复刻 @google/genai，但仓库内没有该 SDK 源码可核对；相关行为只能以既有注释、spec/ai-api-other.md 与测试为依据，字段白名单未逐项验证。

### u011-kiana-ai-large

- `as_index` 未按 JS 数组下标上限 2^32-2 截断：contentIndex 为 2^32 及以上的整数会被当成合法下标，`set_block` 会循环填充空洞（如 4e9 会造成明显内存放大/OOM 风险）；TS 里这些数字只是普通属性。已在注释中标注该风险，未改代码。
- `append_rewrite_diagnostic` 对非对象 rewrite（字符串/数组）统一给空 details；JS 的对象展开 `{...rewrite}` 会产生 0、1… 下标键。线上协议声明 rewrite 为 KianaMessagesRewriteImpact 对象，正常输入不可达，但这是与 TS 的一处保真差异（原注释已按此改写并保留说明）。
- `toolcall_end` 在 contentIndex 缺失/不是数字时不复刻 TS 的 `Object.assign(undefined, ...)` TypeError，而是正常产出事件（toolCall 取 event.toolCall 或默认值）；块存在但不是 ToolCall 时也不修改块，TS 会把字段合并进任意块。
- `convert` 对非对象 raw（null/数组/字符串）返回 Ok(None) 静默丢弃；TS 对 null 会在读取 `event.type` 时抛 TypeError 并经外层 catch 变成 error 事件。
- `Utf8StreamDecoder` 不剥离开头的 UTF-8 BOM，而 WHATWG `TextDecoder`（默认 ignoreBOM=false）会剥离；带 BOM 的 SSE 首帧第一行不以 `data:` 开头，可能被忽略。已在解码器注释中写明。
- `debug` 通过 `url.query_pairs_mut().append_pair("debug","1")` 追加参数；TS 用 `searchParams.set`。若 base_url 自身已带 `debug` 参数，Rust 会发送重复的 `debug=old&debug=1`，TS 会覆盖为单个值。
- `reason_phrase` 用固定表重建 statusText，因为 ProviderResponse 只有 status/headers；服务端自定义的 reason phrase 会在错误文本与 diagnosticDetails.statusText 中丢失（undici 会使用真实 statusText）。未知状态码回落空串与 undici 一致。
- `resolve_client` 在未注入 options.fetch 时每次调用新建 ReqwestHttpClient（独立 reqwest::Client/连接池），跨请求不复用连接；真实上层是否总是通过 fetch 注入共享客户端未在本单元核实。
- `stream_simple` 本实现永远返回 Ok，`ProviderStreams::stream_simple` 的 lazy_stream_error 分支对该适配器不可达；api/mod.rs 中“streamSimple 缺鉴权同步抛 No API key for provider: X”的通用描述不适用于本适配器（本文件文案为 `No API key provided for provider "X"`，且在后台任务里异步变成 error 事件）。
- 空洞的占位 Text 块会出现在每个事件的 partial 快照中，并可能留在最终 done 消息的 content 里；TS 的稀疏数组空洞在 JSON 序列化时输出 null。测试未覆盖 contentIndex 跳跃（稀疏）路径。
- `usage_from_json` 对负数/非有限计数按 0 处理，而 TS 直接赋原始 number（负数会保留）；`tool_call_from_json` 缺 id/name 用空串而 TS 是 undefined（序列化时键消失）。均为防御性偏差，协议正常输入不会触发。
- 模块文档中的若干行为依据来自 spec/ai-api-other.md §2.10 与 tests/kiana_messages.rs；未运行 cargo doc/check（任务禁止），因此新增 intra-doc link 未经过 rustdoc 实际验证（已只链接 public 项，并避开私有项链接）。

### u012-kiana-ai-large

- `ToolCallKey::Index(index as i64)` 会把非整数 index 截断，而 TS 直接用原始 number 作为 Map 键；正常 Mistral 流的 index 应为整数，仓库中没有非整数用例，注释未断言该边界行为。
- `MistralFailure::Other` 的 `Invalid URL {base_url}` 与 Node `new URL` 抛出的 message（通常只有 `Invalid URL`）是否逐字一致未实测；若要求逐字保真需核对 Node 版本。
- 服务端返回溢出数字（如 `1e400`）时 `kiana_json::js_parse` 会映射为 `Value::Null`，本文件 usage 因此归 0；TS 会保留 `Infinity`。这是 crate 级既有偏差，未逐用例验证。
- 9 位 alnum 的 tool call id 约束来源（推测为 Mistral 服务端要求）只能由常量名与 TS 实现推断，源码与 spec 均未写原因；注释只描述算法，没有断言原因。
- `map_tool_choice` 额外要求 `type == "function"`，而 TS 的 `mapToolChoice` 不检查 `type`（会无条件返回 `type: "function"`）；该差异已在文档注释中记录，是否有意为之无法从仓库确认。
- `build_mistral_headers` 对 `model.headers` 只支持设置、对 `options.headers` 支持 `None` 删除；这源于 Rust `Model.headers` 的类型 `IndexMap<String, String>`，TS 侧 `model.headers` 是否允许 null 未核对。

### u013-kiana-ai-large

- `is_codex_non_transport_error` 匹配 `ProviderStreamEventCallbackError`，但全仓库 grep 只在此处出现该名字，没有任何构造点；Rust 的回调类型是 `Arc<dyn Fn(JsonValue, &Model)>`（无 Result），TS 里“回调抛错被包成 ProviderStreamEventCallbackError”的路径在 Rust 侧没有等价物，该分支目前不可达。未修改，仅记录。
- `resolve_codex_websocket_url` 在 `url::Url::parse` 失败时返回原字符串（scheme 仍是 https/http），而 TS 的 `new URL()` 会在同一位置抛出；实际错误会推迟到 `into_client_request`/连接阶段，错误文本可能与 TS 不同。未找到覆盖该分支的测试。
- `decode_base64_atob` 会先剥掉尾部所有 `=` 再按 4 的倍数补齐；Node `atob`（WHATWG forgiving-base64）对“过度 padding”（如 `====`）的具体接受/拒绝范围未在本仓库测试中覆盖，无法确认逐字等价。
- 空串 session id 会被 `clamp_openai_prompt_cache_key` 原样返回 `Some("")`，于是请求体写入空字符串 `prompt_cache_key`；而同一路径的 WebSocket 缓存会显式过滤空串。该行为看起来与 TS 的 `??`/序列化语义一致，但没有专门测试，需确认是否有意为之。

### u014-kiana-ai-large

- `utf16_slice` 使用 `from_utf16_lossy`：若截断恰好落在代理对中间，Rust 会得到 U+FFFD，而 JS 的 `slice` 保留孤立代理项。tool call id 通常是 ASCII，真实 provider 是否会返回含非 BMP 字符且超长的 id 未验证；已在注释中标注为已知保真差异。
- `apply_open_router_accounting` 调用共享的 `apply_open_router_reported_cost` 时把 `upstream_inference_prompt_cost`/`upstream_inference_completions_cost` 固定为 0.0（因为已先求和）；这是否与 TS 侧完整 accounting 对象的逐字段行为等价，未逐行核对共享实现。
- `StopReason::Aborted` 在本文件内只有检查（`run_completions_inner` 收尾处），没有赋值来源；它是为外部预置状态保留还是纯 TS 对齐，未确认。
- golden 模块注释称两个 fixture 是「TS 输出快照」；fixture 文件本身没有生成元数据，依据是原有模块注释与测试名 `..._matches_typescript_reference`，未找到生成脚本。

### u015-kiana-ai-large

- supports_developer_role 保留在 get_compat 返回结构中的原因源码未说明；注释只陈述“本文件不消费、字段与 TS Required<OpenAIResponsesCompat> 形状对齐”，未断言动机。
- prompt_cache_options 的 {"mode":"explicit"} 在服务端的具体语义在仓库内未找到说明；注释只描述发送条件，未解释语义。
- supports_additional_tools / supports_tool_search / supports_max_output_tokens 的业务描述取自 providers/compat_schema.rs 的 schema 文案，未见独立测试逐条验证。
- CHATGPT_WORKSPACE_LIMIT_HINT 中“ChatGPT Business 工作区上限”的归类来自 spec §2.11.7；代码本身只是字符串常量。
- gpt-5.5 的 2.5 倍计价规则依据是硬编码常量与 tests/openai_responses_compat.rs 的用例，未找到定价文档。
- thinking_budgets 不透传的依据是 spec §2.11.8 与本文件根本不读取该字段，未验证上游是否期望它被静默忽略。

### u016-kiana-ai-large

- golden 测试模块注释中的 TS 参考实现路径 `/home/ctyun/kiana-work/upstream-reference` 在本机不存在，fixture 与 TS 输出的生成关系只能依据测试名与模块注释判断，未能独立核实。
- `default_http_client` 在 `ReqwestHttpClient::new()` 失败时退回 `ReqwestHttpClient::default()`；reqwest 的 `Client::new()` 在 TLS 后端初始化失败时会 panic，该兜底路径是否真能避免失败未验证（代码未修改）。
- `LineDecoder` 用 `from_utf8_lossy` 解码，注释只称其行为对应 `TextDecoder` 非 fatal 模式；`openai` SDK 源码不在仓库内，未能逐字核对。
- `process_responses_stream` 的 `response.failed` 分支未设置 `saw_terminal_response_event`（spec 伪代码设置了）；因该分支立即返回 Err，可观察行为等价，未改动。
- `response.created` 只在 `response.id` 为字符串时写入 `response_id`，spec 写的是无条件赋值；序列化后的可观察差异未验证。
- `public_total` 按 `docs/COMMENT_PROGRESS.md` 的统计口径只计顶层 `pub` 项（11 个）；另有 16 个公开结构体字段也全部补了 `///`，未计入该数字。

### u017-kiana-ai-large

- 模块文档里原有的 [`HttpClient`] 在本文件没有 use 导入，原样保留会形成未解析的 intra-doc link；为不改动原注释文字，我在 //! 末尾新增了 Markdown 链接引用定义 [`HttpClient`]: crate::http::HttpClient。任务禁止运行 cargo doc，无法确认当前 rustdoc 对该写法的解析/告警情况。
- chat_completions_url 的注释称 Url::join("/chat/completions") 会替换 base 的路径（因此与 TS 的字符串拼接不同），这是依据 WHATWG URL 解析规则的推断，未在运行时用 rustc/url crate 验证。
- 模块文档没有断言 OpenRouter 上游对“纯图像模型请求 text modality”会报错；仅说明 TS 测试 image_only_models_must_not_request_text_output 锁定请求体形状，上游的真实反应在仓库中无资料可证。
- generate_images 文档中“默认 0 次重试”来自 tests/openrouter_images.rs 的 retries_only_when_max_retries_allows_it 与 classifier_shared 的 PostErrorStyle::OpenAi 默认值，未额外运行测试确认。

### u018-kiana-ai-large

- `expires_at` 为 Unix 秒：依据 TS 的 `expiresAt * 1000` 与测试常量 9999999999 推得，spec/ai-auth.md §2.13 未显式写明单位。
- “凭证可能来自磁盘（auth.json），enterpriseUrl 不保证仍是规范化形式”依据 types.rs 中 OAuthCredential/CredentialStore 的持久化说明推断；未见专门测试覆盖“磁盘里存有未规范化 enterpriseUrl”的输入。
- DeviceCode 事件中 open_browser_on_confirm=None / copy_code_on_confirm=None 的具体宿主行为不在本 crate 内；注释沿用既有“验证 URI 会被浏览器打开”的表述，无法在本单元核实宿主实现。
- “CLIENT_ID 是 GitHub OAuth App 的公开标识”由 spec §4.3.4 的请求体（只发 client_id、无 client secret）推得，TS 源码注释未解释其来源。

### u019-kiana-ai-large

- delay_duration_ms 把 ±Infinity 归零；TS 把同一数值交给 setTimeout 时的实际钳制行为未核实，现有注释“Math.max(0, value)”在 Infinity 时与代码行为不完全等价（未改动原注释）。
- status_text 用 StatusCode::canonical_reason 反推；常见状态码短语与 Node HTTP/1.1 reason phrase 是否逐字一致未逐码核对（spec/ai-auth.md §6.2 #17 只称“大多数一致”）。
- OAuthFetchRequest.headers 允许重复同名头；reqwest/undici 传输层最终是合并、覆盖还是都发送，未核实，本层只按调用方顺序保存。
- oauth_callback_host_at_init 的 LazyLock 缓存是进程级一次性的；anthropic/openai-chatgpt 的集成测试是否覆盖自定义回调 host 未逐一核对（anthropic_oauth.rs 中未见该用例，openrouter 用的是每次读取版本）。
- url_with_search 在 fields 为空时会留下末尾 `?` 的结论来自 url 2.5.8 的 set_query 源码与 URL 标准推断，未运行验证；当前调用方都传非空字段。
- 按任务约束未运行 cargo check/doc/fmt，rustdoc 链接可达性与格式仅为静态核对（所有新增链接均指向 pub 项，未链接私有项）。

### u020-kiana-ai-large

- `html_response` 与 spec/ai-auth.md §2.12 存在线格式差异：TS 的 sendHtml 不设 Content-Length（走分块传输），Rust 显式发送 Content-Length 并额外加 Connection: close。spec §6.2 #20 只要求测试断言状态码与 content-type，因此现有测试不覆盖此差异；未运行任何 HTTP 抓包/字节级对比，无法确认是否为有意简化。
- `getaddrinfo_error` 按错误文本是否包含英文 "Temporary failure in name resolution" 来判定 EAI_AGAIN，其余一律 ENOTFOUND。std/tokio 的 lookup_host 通常不暴露原始 EAI 码（测试用 io::Error::other 构造样本），所以这是当前唯一可行做法；但非英文 locale 或不同 libc 文案会把 EAI_AGAIN 误判为 ENOTFOUND。代码未改。
- `login_openai_chatgpt` 中 `url_with_search(...)?` 位于 `ChatGptCallbackServer::start` 之后、收尾（close/closeAllConnections）之前；若该 `?` 真的返回 Err，已启动的 accept 任务不会被关闭（ChatGptCallbackServer 没有 Drop）。AUTHORIZE_URL 是合法绝对常量，当前路径不可达，但属于错误路径上的资源泄漏风险。
- `combine_abort_signals` 返回的 `cleanup` 句柄被丢弃、未调用 cleanup()；注册在 `interaction.signal()` 上的监听器会存活到该 signal 结束，每次登录新增一个。TS 的 `AbortSignal.any` 生命周期是否与此完全等价未逐条核对，故未在注释中断言无副作用。
- `openai_chatgpt_oauth()` 在仓库内未找到调用点（grep 只命中定义；`load_openai_chatgpt_oauth` 是另一个函数）。它可能是面向外部使用者的便捷 API，或尚未接线；无法仅从源码确认，注释只说明内置接线不经过它。
- `start_with_listener` 的 accept 出错分支会 settle 错误后 sleep 10ms 并继续循环；若监听 socket 进入持续性错误状态，日志层面不可见（错误只写进 result 槽，且首个 settle 后后续错误被忽略）。这与 Node `server.on("error")` 的持续运行语义一致，但长期行为未经验证。

### u021-kiana-ai-large

- TS 侧行为依据 spec/ai-auth.md §2.5 的逐字代码与既有注释，未在 Node 运行时实测；本文件所有「TS 语义」描述均来自该规范。
- AbortReason 只承载 name/message，不承载 DOMException 的数值 code（例如 TimeoutError 的 23）；若下游依赖 error.code 需另行核实，本单元未改动也未断言该字段可观察。
- resolve_provider_auth 在竞速返回 Ok 后仍检查 signal.is_aborted()：按源码推断，跨线程下「内部已完成、随后才中止」的调用也会返回 AbortError，比 TS 单线程的「先 settle 者胜」更保守；未与真实 Node 行为逐例比对。
- refresh_stored_oauth_credential 的 Ok(Some) 语义（写后凭证，可能是别人刷新的结果）依赖 CredentialStore::modify 的契约；仓库内默认内存实现符合，自定义持久化实现未在本单元核查。
- 测试模块的分组概括仅基于本文件内的测试名与断言，未覆盖其他 crate 中调用 resolve 的集成测试（如 kiana-config、kiana-durable）。

### u022-kiana-ai-large

- 既有 OAuthCredential 注释称 anthropic/meta/github-copilot 的 TS 凭证以 refresh 开头、其余六条以 access 开头；本单元未逐条核对 TS 流程源码（Rust 侧统一用 OAuthCredential::new，固定 access 在前），保留原文。
- OAuthAuth::refresh 在 store 回调内重入同一 provider 的 modify/delete 会自锁：结论由 resolve.rs 的锁内调用与 InMemoryCredentialStore 的 per-provider AsyncMutex 推得，未写成复现测试。
- AuthError::details 装箱是为了控制 Err 体积（既有注释），未实测 size_of 或 clippy 阈值。
- JsNumber 的 NaN/Infinity → null 不能按 f64 读回：由仅实现 Serialize 的代码与 spec/ai-auth.md §4.1 的“有限 number”要求推得，未运行往返测试（任务禁止 cargo）。
- AuthEvent 的 *_on_confirm 默认 true 来自 spec 文案；本 crate 不消费该默认值（由宿主解释），未见运行期强制。

### u023-kiana-ai-large

- 模块文档原有的 [`ReadlineInterface`]、[`parse_int_js`] 链接指向私有项，而 `cli` 是公开模块，可能触发 rustdoc::private_intra_doc_links 警告；按“保留原注释”原则未改，需主代理跑 cargo doc 确认是否为既有警告。
- parse_int_js 用 Rust `trim_start()`；JS `Number.parseInt` 的 WhiteSpace 集合包含 U+FEFF(BOM)，Rust `char::is_whitespace` 不包含，理论上 "\u{FEFF}1" 在 JS 得 1、Rust 得 None。无测试覆盖，未改代码，仅记录。
- take_line 用 `from_utf8_lossy` 解码；非法 UTF-8 或跨 chunk 多字节序列的替换粒度是否与 Node 的 StringDecoder 完全一致未做对比验证（现有测试只覆盖正常文本）。
- read_chunk 把包括 EINTR 在内的所有 `Err` 当作 EOF；本 CLI 未安装信号处理器时 EINTR 是否可能出现未验证。
- drain_available 在持续写入的 stdin 上可能长时间循环；没有针对该 liveness 的测试或基准，未下结论。
- run 交互选择的三层 Option（Ok(None)/Ok(Some(None))/Ok(Some(Some(id)))）语义是按代码与测试反推并已核对，但 TS 侧同名结构未直接阅读（仅依据 spec/ai-core.md §2.1.13）。

### u024-kiana-ai-large

- 原注释中的 TS 行号（registerBuiltInApiProviders 第 267 行 / compatModels 第 269 行）与仓库内 vendored pi/packages/ai/src/compat.ts（215/217 行）及 spec/ai-core.md §2.1.15b 不符，已按后两者改为 215/217；若原移植参照的是另一修订版 TS，行号仍需复核。
- IndexMap::insert 对已存在 key「只更新值、保留首次插入位置」来自 indexmap 库文档，仓库内没有针对覆盖场景的测试锁定该顺序；现有测试只锁定 10 个内置 API 的初始顺序。
- is_builtin_api_instance 的 (None, None) => true 分支，以及「先 register_api_provider 覆盖内置 api、再调用 register_built_in_api_providers 会把覆盖实现记为内置实例」的组合行为，均无本地测试覆盖；后者与 TS 的引用比较逐字对应，但可能导致内置目录模型绕过快路径之外的覆盖实现，建议由维护者确认是否符合预期。
- set_bedrock_provider_module 只影响之后调用 bedrock_converse_stream_api() 的工厂，不会更新已注册的内置条目；本次未在注释中把它写成保证，仅作为待核实的行为观察。
- 未能运行 cargo check/doc（任务禁止），新增的 intra-doc 链接均按同 crate 可见性人工核对，最终仍需 CI 的 cargo doc 验证。

### u025-kiana-ai-large

- WHATWG Request 构造器的校验顺序（URL 先于 method）来自规范理解，未在本机 Node 上复验；若 Node 实际先校验 method，则 build_request 的“method 先于 URL”顺序差异不成立。
- 未穷举所有调用方是否可能传小写 HTTP method（grep 到的字面量只有 POST/GET/PUT），因此 decodable 对 "HEAD"/"CONNECT" 的大小写敏感比较的实际风险等级未定。
- undici 对 content-encoding 空段/未知段的计数方式未在本次会话对照其源码；本实现“按原始逗号段计数”仅有现有测试佐证。
- Default 回退路径（TLS 后端初始化失败）在本环境是否可达未验证；Client::new() 的 panic 行为已按 reqwest 0.13.5 源码确认。
- 模块文档中既有的“本机 Node 26 实测”结论本次未重新运行 Node 验证，全部原样保留。

### u026-kiana-ai-large

- generator::LoadOptions::nvidia_nim 未被任何代码读取：load_models_dev_data 用的是 inputs.nvidia_nim，构造处（build_all_models）也只传 None；疑似遗留字段，未改动。
- hydrate_model_catalog 的换入顺序是先 remove_dir_all(data_dir) 再 rename(staged, data_dir)，若 rename 失败会丢失旧数据；generator 有 previous-data 回滚而 hydrate 没有，未改动。
- load_models_dev_data 里的 `let _ = &nvidia_nim_ids;` 与 `let _ = options;` 看起来是遗留的抑制 unused 语句（options 实际有使用），是否有意无法确认，未改动。
- ModelCatalogEntry 在仓库内未找到构造或读取点（grep 仅见定义），疑为保留的 TS 类型移植，未核实后续用途。
- apply_temporary_overrides 的“重复调用幂等”是从逐条赋值/合并推得，没有专门测试锁定该性质。

### u027-kiana-ai-large

- publication_locks 的条目在发布结束后不会从 HashMap 移除（TS 的 publicationChains 会在链尾删除）；provider id 大量变化时存在缓慢增长，未改代码，仅记录。
- run_provider_refresh_phase 在没有 tokio runtime 时返回未 spawn 的 operation；该 future 被 await 时会进入 race_with_abort_signal 的 tokio::spawn，无 runtime 会 panic。生产调用路径都在 runtime 内，未找到反例，也未运行测试验证。
- 既有注释中的 [`CancellationToken`]（normalize_request_abort_error 文档）在本文件作用域没有 import，cargo doc 可能已有 broken-intra-doc-link 警告；这是改动前就存在的内容，按要求未改动。
- refresh_generations 的条目同样只增不删（与 TS 的行为一致，非缺陷），仅记录。
- 未执行 cargo check/doc（任务禁止构建命令）；新增 intra-doc 链接已按作用域与可见性人工核对，但仍需主代理统一跑文档检查确认。

### u028-kiana-ai-large

- 修正的旧注释：原文称「Rust 的 AnyModel 只有三种变体」，但 types.rs 中 AnyModel 已有 Chat/Image/Classifier/Remote 四个变体；我改写为「ModelType 只有三个值，Remote 也归一到三者之一，所以 matches! 恒为 true」。结论（恒真、保留过滤点）不变，但请复核是否有意去掉该过滤。
- refresh_models 阶段 1 使用 persist: Unchanged 的动机在 TS/spec 中没有文字说明；注释只描述可观察行为（只更新内存、存储的 etag/lastModified 保持不变），未断言设计动机。
- 未运行 cargo check / cargo doc（任务禁止）；新增的 intra-doc link [`crate::api::lazy::lazy_stream_error`]、[`ModelsPublication`]、[`Models`](super::Models) 需由主代理用 `cargo doc -p kiana-ai --no-deps` 确认无 broken-intra-doc-links 警告。
- 「Models 的刷新协调最多调用 refresh_models 两次」只在 models/mod.rs 的 refresh_models 协调逻辑中验证；其他直接调用方（包装 provider 内部）可能多次调用。
- 既有注释「内置 provider 工厂与 models.json 自定义 provider 共用」沿用未改；本仓库内未找到直接调用 create_provider 的 models.json 组装代码（kiana-config 是包装已有 provider），此说法仅来自原注释，未能独立核实。
- models_are_equal 的调用方描述基于 grep（kiana-config resolver、kiana-interactive model_selector、kiana-extensions bridge），未穷举全部调用点。
- 并发描述（dynamic_models 锁只覆盖 Vec 读写、发布生效由 Models 的代际 + per-provider 锁决定）来自 models/mod.rs 静态阅读，未做运行时并发验证。

### u029-kiana-ai-large

- crates/kiana-ai/tests/faux_provider.rs:547 的期望 prompt 文本使用 `tools:[<json>]`，而本文件 `message_to_text` 实际生成 `tool+:<json>`（spec/ai-core.md §2.3.13 与源码一致）；按任务规约不能运行测试，无法确认该测试当前是失败还是已过期。
- `FauxDeferredEntry` 只插入不删除：`deferred_responses` 与 `cancelled_deferred` 都会在 core 生命周期内持续增长；长时间运行的测试可能累积内存（未实测，仅据源码读写点推断）。
- `schedule_chunk` 对 `tokens_per_second` 只判断 `> 0.0`：若传入极小正数（如 5e-324），`delay_ms` 可能溢出为 inf，`Duration::from_secs_f64` 按标准库约定会 panic；未运行验证。
- `FauxModelDefinition` 派生 `Default` 允许空 id，`create_faux_core` 直接把它写入 `Model::id` 而不校验（TS 在类型层面要求 id）；是否为有意的宽松行为未从文档确认。

### u030-kiana-ai-large

- ModelImageResizeOptions.jpeg_quality 的取值范围（spec/ca-cli.md 只给出 85/70/55/40 等质量步进，未声明 0-100 上限）；注释只写“由图像处理实现解释”。
- ModelImageInputLimits.max_per_message / max_per_request 的强制点不在本 crate；仓库内只确认它们随模型目录往返并被 kiana-session/工具层读取 resize 配置，数量上限具体由谁执行未逐一核实。
- AssistantMessageEvent::Start 的 partial “不含内容块”只在 api/anthropic_messages.rs 与 api/openai_completions.rs 核对，未逐一检查全部 provider 适配器。
- TextSignaturePhase 的 commentary/final_answer 除 spec 中的字面量定义与 OpenAI message.phase 用法外，未找到更细的通道语义说明；变体注释按字面含义解释。
- REMOTE_MODEL_JSON_KEY “绝不出现在最终 JSON/持久化文件”依赖所有写出路径都经过 kiana-agent-core::wire_keys 的解包；本单元只核对了 wire_keys.rs 的调用点，未穷举全部持久化路径。
- DeferredRequest::Window 的 window 取值（15m/1h/24h）在 spec 中只有字面量定义，没有语义说明；注释只写“原样交给 provider 解释”。
- 模块级 //! 因 lib.rs 的 pub mod types 带外层 ///，按 COMMENT_STYLE §4.2 使用全限定链接 [`crate::types::X`]；若 cargo doc 的 redundant_explicit_links 行为与预期不同，需要复核（本环境未运行 cargo doc）。

### u031-kiana-ai-large

- 注释中的 V8 内部行为（Smi 上限、ReadWord/Scan 的字符门槛、legacy 各分支的判定顺序）无法在本仓库内对照 V8 源码核实，沿用文件原有的“逐函数移植”表述与既有注释。
- abortable_sleep 注释称 CancellationToken::cancelled() 在令牌已取消时立即完成；这是 tokio-util 的公开语义，未在本仓库源码中验证。
- 未运行 cargo doc / cargo check：intra-doc 链接只做了人工可见性核对（公开文档中的链接均指向本模块公开项；私有项链接只出现在私有项文档里），未实际编译验证。
- 未运行任何测试；Date.parse 的 318 条 fixture 只读取 JSON 并抽样核对（含 1994-02-30、24:00、小数秒、完整月份名用例），未执行测试。
- get_retry_delay_ms 中 retry_index as i32 在超过 i32::MAX 的重试序号下会截断；实际调用不可能达到，注释标注为“实践中不可达”，代码未改。

### u032-kiana-ai-large

- `ToolCallArgumentsStream` 的隐式 `Drop`（`Arc<Value>`/`JsonObject`）与 `StreamingJsonParser::clone` 的递归析构/克隆在深嵌套下可能栈溢出：这是从代码结构推断，未构造 200k 层 + Clone/Drop 的实测。
- 模块注释引用 `streaming-json.test.ts` 对「预览滞后」的断言，依据是 `spec/ai-core.md` §2.2.26 的转述，未直接阅读 TS 测试文件。
- 「一次快照的真实成本是 open path 上已完成成员的总字节数」与 credit 只按成员数扣费的偏差，由 `copy_container_skipping`/`deep_clone` 代码路径推导，未做基准测量。
- `SNAPSHOT_SEQ`/`snapshot_revision`、`parser()` 在仓库内没有生产调用方（grep 得出）；是否存在仓库外或动态调用（如扩展 JS 侧）未验证。

### u033-kiana-ai-large

- 第 2897 行 GENERAL_CATEGORIES 中出现 "Kiana"，位置在 "Pf"/"Final_Punctuation" 与 "Initial_Punctuation" 之间；Unicode General_Category 的短别名此处应为 "Pi"。这会让 \p{Pi} 被判为非法属性。疑似全局改名（pi→Kiana）误伤，未修改，需确认是否有意。
- 第 1428 行 ZWNJ 分支条件 `prev.is_none_or(|prev| (prev as u32) < 0x0080 && !is_virama(prev))` 实际只要求前一个字符存在且非 ASCII；紧邻注释写“前面必须是非 ASCII 的 Virama”。注释与代码不一致，未改写（无法从仓库确认 TypeBox 原型意图）。
- 第 787 行 next_cluster 的 ZWJ 推进 `end += 1 + usize::from(point(end + 1) > 0xFFFF)`：`end` 索引的是 points（每字符一项），对 ZWJ 后接 BMP 字符的序列只前进 1 个码点，疑似少吞一个码点（如 "👨\u{200D}⚕" 可能被数成 2 个字素）。需对照 TypeBox 的 NextGraphemeClusterIndex 确认，未修改。
- walk_schema_patterns（第 938 行起）只跟随 `#/...` JSON Pointer，不解析 `#anchor`/`#dynamicAnchor`；带 $anchor 的 $ref 目标中的非法 pattern 不会被预检，随后 check_pattern 会静默判为不匹配。TypeBox Compile 是否覆盖 anchor 目标待核实。
- check_string_length（第 3683 行）与 error_string_keywords 把 f64 长度限制转成 usize（负数/NaN 饱和为 0、小数截断）；对非法 schema（如 minLength: 2.5）与“按数值比较”的语义存在偏差，是否有上游测试覆盖待核实。
- 本文件多处“与 TypeBox 1.3.27 一致”的结论依赖既有注释和 tests/fixtures/utils/validation.json；仓库内未找到 typebox 源码（上游 pi 仓库也未包含），无法逐条复核 Compile/Guard 的原始实现。
- normalize_optional_nulls 的 TS 差异说明基于上游 `/home/ctyun/code/kiana_rev/pi/packages/ai/src/utils/validation.ts` 的 getSubSchemaValidator（catch 返回 undefined）；“非法 pattern 会让 Compile 抛错”沿用文件原有注释，未在 typebox 源码复核。

### u034-kiana-ai-group

- event_stream.rs 的 //! 与 lib.rs 的外层 /// 合并后会按父模块（crate 根）作用域解析链接；既有短链接（如 `EVENT_CHANNEL_CAPACITY`、`end_without_result`）是否需要全限定，本次未改动也无法在不运行 cargo doc 的情况下确认是否产生 warning。
- recv() 的 future 在已注册等待者后被取消时，下一次 push 会向已关闭的 oneshot 投递并丢弃该事件；该行为由源码推导，未见测试覆盖，是否为预期设计待确认（已写入 recv 文档的“取消的代价”）。
- Vertex ADC 缓存是进程级且只缓存“非显式 scoped 路径”；spec/ai-core.md §2.1.12 与代码一致，但多调用方共享进程 env 时，进程 env 中 GOOGLE_APPLICATION_CREDENTIALS 变化后缓存不失效的行为没有测试覆盖。
- bun_oauth 原注释写“10 个 OAuth 流”，按 auth/oauth/load.rs 的 OAuthFlowLoaders 九个字段与 spec/ai-auth.md 多处“九个流程”改为 9；若 TS 上游实际还有第 10 个流程，需要重新核对。
- image_models 模块文档中既有的 `ensure_builtins` 私有项链接本次改成行内代码（避免 private_intra_doc_links），但未运行 cargo doc 验证其它既有链接。

### u035-kiana-ai-group

- telemetry.rs 模块头原有「保证 observer 第一次收到事件时 span 已就绪」只对在首次 poll 内同步调用回调的 starter 成立（InMemory 适配器已从 kiana-telemetry/src/memory.rs 核实）；其它适配器未验证，未改写原句，仅在新增小节与 observe_model_request 文档中写明条件。
- session_resources 模块文档中「TS 的 Set 同样不 clear」依据 spec/ai-core.md §2.1.5 的行为描述推断（仓库内无 TS 源码可直接核对）。
- legacy_api_aliases 模块文档中「扩展 JS helper 另行实现、不调用 Rust 函数」已核对 crates/kiana-extensions/js/virtual/kiana-ai-helpers.js；但扩展是否还能通过其它原生桥接路径调到这些 Rust 函数，未全量追踪（grep 只命中 virtual_modules 的导出名单与 JS 实现）。
- 「LazyLock 首次并发调用时其余线程等待」属于 std 文档化行为，仓库内没有对应测试验证。
- session_resources 中「两个线程并发调用 cleanup_session_resources 会让同一回调并发执行」由代码（回调在锁外调用）直接推出，未找到专门测试覆盖。
- telemetry 中 create_instruments 的 unwrap_or_else 回退当前不会触发（已核实 kiana-telemetry 的 TypedMetrics 查询恒返回 Ok）；注释已按此写成防御性回退，若未来改为可失败需同步更新文档。

### u036-kiana-ai-group

- 文件头原有说明“Azure 模型不带 `baseUrl`”与代码中 `model.base_url` 作为最后一级兜底并存；按“保留已有注释”的约束未改写，建议主代理确认这句措辞。
- `on_payload` / `on_response` / `on_provider_stream_event` 是外部回调；若回调 panic，后台任务可能直接 unwind，流端是否只表现为提前关闭而非 `error` 事件，本次只读检查未验证，因此注释未对此作断言。
- `normalize_azure_base_url` 的 `matches!` 含 `"/"`，但 `normalized_path` 已经过 `trim_end_matches('/')`，该分支看起来不可达；与 TS 的 `replace(/\\/+$/, "")` 行为一致，未改代码，建议下轮核实是否保留。
- `parse_ascii_punycode_fallback` 的既有注释称“非 ASCII 的 IDNA 行为与 Node 一致”，现有 fixture 主要覆盖 ASCII punycode 标签，这一点未独立验证；已按要求保留原注释。

### u037-kiana-ai-group

- [`StreamOptions::extra`] 这类结构体字段 intra-doc link 在本仓库已有先例（如 [`Credential::extra`]），但按任务要求未运行 cargo doc，链接是否零警告未实测。
- 两个 Google 适配器的 on_provider_stream_event 回调类型是 Fn 且返回 ()：若回调 panic，后台任务会终止，事件流可能永远收不到 done/error；TS 中回调 reject 会进入 catch 生成 error 事件。这是源码可验证的行为差异，但未在注释中展开，建议复审确认是否需要处理。
- AiBinding.ai_gateway_log_id：TS 接口要求必填（string|null），Rust 是 Option<String> 且 create_ai_binding_fetch 不校验；注释已如实说明，是否要收紧属于设计决策。
- api/cloudflare.rs 的地址常量当前 crate/workspace 内无代码引用（仅 model_catalog::generator 有同值副本）；下游仓库是否使用无法核实。
- Vertex 走 API key 时的 URL 形状（无 projects 前缀、主机固定 aiplatform.googleapis.com）来自 google_shared 的 request_base_with/should_prepend_project_location，未对真实 Vertex 端点做 live 验证。
- GoogleVertexOptions.project/location 通常经 StreamOptions.extra 或 auth provider 注入 provider env；本 crate 内没有直接构造这两个字段的调用方，上游注入路径未逐一核实。
- 全部五个文件均未运行 cargo check/doc/fmt（任务禁止），因此格式、rustdoc 警告与链接有效性均未实测；仓库无提交基线，diff 纯度依靠每次 Edit 只插入注释行来保证。

### u038-kiana-ai-group

- poll_setup_once 用 noop waker 做首次 poll、之后由后台 await 同一 future 的说明依赖「异步原语会在下次 poll 重新注册唤醒源」这一惯例；仓库测试覆盖了 tokio oneshot 场景，但无法对所有自定义 setup future 证明。
- SYSTEM_PROMPT 作为 prompt-injection 提示层防护的定位是从其文案本身推断的，仓库内没有独立设计文档佐证。
- llama-server 的 add_special/parse_special、post_sampling_probs、cache_prompt 字段语义来自字段名与 spec 中的字面值；仓库内没有服务器实现可交叉验证。
- spawn_forward_with_runtime 在已有 ambient runtime 且传入 Some(runtime) 时会忽略并 drop 传入的 runtime；tokio 在异步上下文中 drop Runtime 是否 panic 取决于版本，本次未验证，且当前 lazy_stream 不会产生这种调用组合。
- lazy_stream 在无 ambient runtime 且 Builder::build 失败时会让流永久 pending；该分支无法在测试中构造，注释按代码路径标注为资源耗尽级极端失败，未经运行验证。

### u039-kiana-ai-group

- CONTEXT_SAFETY_TOKENS=4096 的具体用途（协议开销还是估算误差）在源码与 spec 中均未说明；注释只陈述“它是单独扣除的固定值”，未给出动机。
- 504 的“约 5 秒网关超时、约 60 万 token 输入上限”来自既有英文常量注释与 TS 固定错误文案，未在本仓库测试或真实网络请求中验证。
- prompt_cache_key 的 64 码点服务端限制来自 TS 参考与 spec/ai-api-anthropic-openai.md §2.2，未对线上 OpenAI 接口实测；空串保留、None 省略的行为由本文件单测覆盖。
- IndexMap::extend 覆盖同名键时保留首次插入位置，依赖 indexmap 的文档语义；fixture 的 stringify 比较覆盖了所测样例，但未专门为该细节单独写断言。
- parse_classifier_usage 先于答案解析写入 usage（“已计费”语义）依据源码顺序与 TS 注释/测试，未验证服务端在所有失败响应中的计费行为。

### u040-kiana-ai-group

- system_one_shared.rs：payload_with_model、payload_with_input、wire_request 的 expect 依赖“这些类型只含 JSON 值/字符串容器，序列化不会失败”的类型论证，没有直接测试断言；文档按此写成不 panic。
- system_one_shared.rs：SystemOneImageAsset 文档中“响应里的图片不会走这个类型”是从该类型只实现 Serialize、无 Deserialize 且仅用于构造请求推断的用法结论，未在 spec 中逐字确认。
- transform_messages.rs：replace_images_with_placeholder 的原有注释称“Rust 侧公开给其它 api 适配器复用”，但 grep 全 crate 只有本模块两个调用点；已保留原文并补充“当前只有本模块两个调用点”，是否为预留公开 API 待确认。
- transform_messages.rs：split_error_tool_result_images 的动机（Claude 拒绝错误 tool_result 内图片、图片挪到结果之后以保留诊断信息）依据调用方 anthropic_messages/bedrock_converse_stream 的注释与 spec，本文件内无法独立验证。
- system_one_shared.rs：classify_system_one 文档写“失败时 usage 可能已填”依据 run 中先解析 usage 后解析 answers 的顺序及 typesafe_system_one 测试；若未来 parse_classifier_usage 语义变化需同步。
- 两文件均未运行 cargo check/doc（任务禁止），因此 intra-doc link 与 rustdoc 警告仅通过人工核对目标可见性，未实际编译验证。

### u041-kiana-ai-group

- 未运行 cargo check/doc/fmt（任务禁止），新增的 intra-doc 链接（如 [`ProviderClassifier::classify`]）未实机验证；仓库内已有相同的 Trait::method 链接写法，风险低。
- 原有“OpenRouter 会静默丢弃 assets”来自文件原注释与 tests/typesafe_system_one.rs 的测试注释，本次保留，未对线上服务复验。
- 模块文档中的调用方清单依据 grep 到的 providers::typesafe/openrouter/opencode/vercel_ai_gateway 与 kiana-builtin-extensions llama provider；未穷举仓库外或其他动态注册路径。
- detect_compat 的 OpenRouter 检测是 base_url 子串匹配（继承 TS 语义），路径或主机名含 openrouter.ai 即命中；注释已按“子串匹配”描述，未修改代码。

### u042-kiana-ai-group

- Node/TS 对非 UTF-8 环境变量字节的解码是否与 Rust `to_string_lossy`（替换为 U+FFFD）逐字一致，仓库内没有对应测试或 spec 说明；context.rs 已按此标注为待核实。
- tokio `select!` 多分支同时就绪时随机选择、`OnceCell::get_or_init` 的并发等待语义属于外部 crate 文档行为；仓库内没有直接覆盖「门空闲且 signal 已中止」这一竞争窗口的测试。
- `std::env::var_os` 对含 `=`/NUL 的变量名返回 None、`tokio::fs::metadata` 跟随符号链接且目录算存在，均依赖标准库/tokio 行为，仓库测试未逐项固定。
- credential_store 的 gates 条目不回收是源码事实，但「内置 provider id 数量有界」只是静态观察，没有运行期上限或内存测量数据。
- credential_store.rs 原有注释「清理对调用方不可观察」读起来暗示存在清理，而实现从不删除 gates 条目；未改写原句，只在相邻位置补充了「本实现不做回收」。
- auth/mod.rs 原有注释把 crate 根接线写成 `pub mod oauth_compat;`，而 lib.rs 实际是 `pub use auth::oauth_compat;`；保留原句并新增「现状」说明，是否改写原句留给主代理决定。
- `lazy_oauth` 的 load 闭包若 panic（而非返回 Err），`OnceCell` 之后的可用状态未在本仓库验证；文档只声明 Err 会被缓存，未对 panic 作结论。

### u043-kiana-ai-group

- start_oauth_callback_server 的取消注册存在窄竞态：入口预检查在 bind 之前，而 abort 监听在 bind/accept spawn 之后才注册；AbortScope 不向已中止的 scope 补发事件，因此若 signal 在 bind 期间 abort，wait() 不会据此 settle（文档已按事实标注，代码未改；测试未覆盖）。
- handle_connection 先 write_all 再 finish，而 TS 的 response.end 只是入队；对端不读取响应时 Rust 侧会推迟 wait() 的 settle。该差异来自代码结构，未在测试中验证。
- 设备码流程的 poll 闭包签名不接收 signal；各下游流程是否都把同一个 signal 传进各自的 HTTP 请求，本单元未逐个核实（只在文档中写为对闭包的约束）。
- OAuthCallbackServer::wait 声称可多次/并发调用，依据是 Notify::notify_waiters 与循环内先 enable 再检查 result；现有测试只覆盖单 waiter，未专门验证多 waiter 同时唤醒。
- post_json 的 30 秒超时依赖 OAuthFetchRequest::timeout_ms 由传输层执行；注入式 MockFetch 是否实现超时语义未在本单元核实（默认 reqwest 实现另见 http 模块文档）。

### u044-kiana-ai-group

- kimi_coding.rs 的 start_device_authorization 只校验 device_code/user_code 为字符串，未校验非空；spec/ai-auth.md §2.15 要求两者均为非空字符串，空串会被接受并进入设备码轮询。按规则只记录，未修改代码。
- kimi_coding.rs 的 parse_token_response 要求 expires_in 是 JSON 数字（finite_number 走 as_f64，字符串被拒）；spec 只写“有限且 >0”，未逐字说明 TS 是否做 typeof number 检查，字符串型 expires_in 的 TS 行为待核对。
- load.rs 的 get_oauth_auth 不经过 bundled 登记表、radius 每次新建实例；spec 仅在模块树一行提到该函数，无法确认这是有意设计还是遗漏（已按现状写入文档）。
- Kimi 的 device_code 事件使用服务端原文 verification_uri_complete，而 Meta 保存的是 url::Url 规范化后的 URI；spec 未明确要求哪种形式，且 TS URL.href 的规范化细节无法在本仓库验证。
- Kimi refresh 中 30s 超时等传输错误会被当作可重试错误（fetch Err 分支），与 spec 的“fetch 抛错则重试”一致；但未在测试中覆盖超时重试路径，行为依据来自源码而非测试。
- 本单元按任务约束未运行 cargo check/doc/fmt/clippy，新增 intra-doc link 的有效性与 rustdoc 警告未经工具验证（链接目标均为公开项或已改为行内代码）。

### u045-kiana-ai-group

- openai_codex.rs 的 read_token_response：TS 只做 truthy 检查后原样使用 access_token/refresh_token（spec §2.11 写作 { access: json.access_token }），Rust 用 as_str().unwrap_or_default()，真值但非字符串的字段会变成空串；只有畸形响应可观察到，属潜在保真差异，未改代码。
- openrouter.rs 的取消传播：调用方 signal 只有被注入的 OAuthFetch 实现观察 request.signal 时才会立即中止兑换；否则最坏等到 30 秒计时器触发。默认 reqwest 实现会响应取消（oauth/mod.rs 的 execute_default_fetch），但 TS 是 AbortController 直接中止 fetch，自定义传输的语义可能不同。
- Codex 浏览器登录在 KIANA_OAUTH_CALLBACK_HOST 被设为非 localhost/127.0.0.1 地址时的行为：授权 URL、token 兑换与粘贴提示仍使用字面量 http://localhost:1455/auth/callback，而监听地址变成该环境变量值；若浏览器无法把 localhost:1455 路由到监听主机，回调收不到、只能粘贴。结论来自常量和 start_oauth_callback_server 的 host/redirect_host 逻辑，未在真实跨机网络验证。
- openai_codex.rs 的 read_token_response 只要求 expires_in 能按 f64 读取（对应 TS 的 typeof number），不要求有限或非负；服务端若返回负数，expires 会早于当前时间。属边界行为，未改代码。
- openrouter.rs 的 refresh 文档写“标准解析路径不会自动触发刷新”，依据是 auth/resolve.rs 的 needs_refresh 标准实现（expires 与 now+阈值比较）；若调用方传入自定义 needs_refresh 或直接调用 refresh_stored_oauth_credential，仍可能执行到该恒等实现。

### u046-kiana-ai-group

- xai 服务端是否真的只在刷新响应中省略 expires_in（原头部注释的前提）无法从仓库内确认；实现层面 `credentials_from_token_response` 对设备码完成与刷新两条路径都回退 3600s，已在模块注意事项中说明。
- radius 的 OAuthCredential 字段不可省略：token 响应缺少 access_token/refresh_token 时 Rust 写空串，而 TS 会得到 undefined 并在序列化时省略该键；后续读取 auth.json 的一方如何对待空串未在本单元验证（spec/ai-auth.md §A.5 对 anthropic 有同类说明）。
- xai 的 slow_down 用 Value::as_f64 读取 interval，等价于 TS 的 typeof === "number"；JSON 数字在超出 f64 精确范围时的舍入边界未逐值验证。

### u047-kiana-ai-group

- 未运行 cargo doc（任务规则禁止）：新增链接 [`kiana_ai::cli::run`]、[`kiana_ai::cli::CliOutcome::Eof`] 均指向 kiana_ai 的 pub 项（已 grep 确认 pub mod cli、pub enum CliOutcome），但 bin 目标在 cargo doc 默认目标选择下是否会被文档化尚未实测；若不文档化则这些链接不参与解析，也不应产生警告。
- 『未被捕获的 panic 不走 Error 路径』依据是对 kiana-ai/src 全量 grep 未发现 panic::set_hook、根 Cargo.toml 未设置 panic="abort"；注释只写『按 Rust 默认 panic 行为终止』，未断言具体退出码 101，以免依赖构建环境。
- 『bin 作为独立 crate 根无法访问 cli.rs 私有项』属于 Rust 编译模型常识，未用编译错误复现验证。
- 执行流程中『参数在 cli::main 内部用 std::env::args().skip(1) 获取、运行时在其内部显式构建并 block_on』已对照 cli.rs:1057-1078 确认；未运行二进制验证其实际退出码。

### u048-kiana-ai-group

- OAuthCallbackFuture 要求 Send + 'static 的动机未能从源码确认：kiana-extensions 的 JS 桥实际用 tokio::task::spawn_local（本地任务）等待它，并不需要 Send；可能只是为宿主通用性保留的保守约束，报告中未把该理由写进注释。
- OAuthDeviceCodeInfo 的 open_browser_on_confirm / copy_code_on_confirm 默认 true 只写在 TS 注释里；Rust 内置流程与 kiana-config 适配层都只透传 Option，未在 kiana-ai 内找到统一施加默认值的位置，注释按“由宿主客户端决定”描述，是否真有客户端施加默认值未核实。
- 模块文档中“onDeviceCode 通常先于设备码轮询”只在 kiana-ai/src/auth/oauth/github_copilot.rs 的流程说明与 device_code 轮询结构中核对过，其他内置 provider 未逐一确认。
- OAuthSelectHandler 的 None→undefined 在加载器侧已核实（HostValue::Undefined），但“undefined 表示取消”之后的宿主行为未统一：kiana-config 的 ExtensionOAuthCallbacks::on_select 返回 Result<String> 无法表达 None，runner 也始终包成 Some；注释只按 TS 语义描述，未声称宿主如何处理取消。

### u049-kiana-ai-group

- crates/kiana-ai/src/http/mod.rs:4 的既有短链接 [`reqwest_client`] 位于同时具有 lib.rs 外层 /// 的模块文档中；按 COMMENT_STYLE §4.2，rustdoc 可能按父模块作用域解析而报 broken-intra-doc-links。这是原有内容，本次未改动，需 cargo doc 验证。
- 模块文档中「timeout_ms/signal 应覆盖整条请求（等头 + 读 body）」是依据 reqwest_client.rs 模块文档与 TS fetch 语义写的契约要求；未逐一核实所有自定义实现（如 api/cloudflare.rs 的 AiBindingHttpClient）是否在读 body 阶段也观察取消。
- HttpRequest::body 的 None 与 Some(vec![]) 区别：已确认默认实现只在 Some 时调用 reqwest 的 body()，但未通过线上抓包验证两种形态在 Node fetch 下 content-length/请求体上的具体差异。
- HttpError 无结构化错误种类是基于字段定义的客观事实；「改文案会改变行为」依据 utils::retry 与 auth/oauth、各 provider 适配器的文本比较，未穷举全部消费点。

### u050-kiana-ai-group

- store.rs 的 Serialize 比较存在一个未覆盖边界：若 raw_models 全部是未知类型模型（已知投影为空）且调用方把 models 清空，known_raw == current（都是空），原始数组仍会被写回，无法用空 models 表达“清空”。已在 raw_models 文档中如实描述，但无法确认这是刻意保留还是疏漏。
- any_model_to_value 对 AnyModel::Remote 直接返回 json，因此修改 Remote 内部的 Model 投影不会被持久化。文档已注明；仓库内未找到依赖“改投影后写回”的调用方，但外部使用方无法确认。
- take_optional_i64 拒绝 1.0 这类整值浮点（serde_json Number::as_i64 对 Float 返回 None），比 TS 的 number 语义严格；kiana-config 的 FileModelsStore 会在反序列化前做清洗掩盖该差异，但直接使用 ModelsStoreEntry serde 的调用方会收到错误。是否为预期未在 spec 明确。
- ModelsStoreEntry::extra 是公开字段；若手工放入 models/lastModified/checkedAt/etag 之一，Serialize 会写出重复键（serde_json::to_string 文本层面重复；to_value 会覆盖）。代码未做防护，已在字段文档提醒，是否为可接受风险待确认。
- 仓库内 grep 未发现对 models::generated 的引用，模块文档据此写“crates/* 目前没有代码引用”；若存在生成代码或仓库外使用则不成立（crate 为 publish=false，风险较低）。
- cost.rs 的 u64 三项求和溢出（debug panic/release 回绕）与转 f64 的 2^53 精度损失均未处理；文档按事实描述为无校验，但未在 spec 中找到“可接受”的明文依据。

### u051-kiana-ai-group

- cloudflare_auth.rs 的两个 login 在 prompt 前后没有调用 throw_if_aborted（同 crate 的 env_api_key_auth 有两次检查）；仓库内没有 TS 源可对照，注释中未断言这是有意差异。
- cloudflare_auth.rs 的逐字段合并采用“凭证中空串也算存在”的语义，会屏蔽同名环境变量；依据是 spec/ai-core.md §2.3.8 的 value !== undefined 规则，仓库内未找到直接覆盖空串边界的测试。
- catalog.rs 关于 LazyLock 初始化 panic 后静态量 poisoned、后续访问继续 panic 的说明依据标准库 Once 语义，本仓库测试未覆盖该路径。
- azure.rs 的 on_payload 在 Rust 是同步闭包（TS 的 onPayload 可返回 Promise），注释只描述同步可达的语义；两种语言回调时序是否等价未在仓库内验证。
- cloudflare_workers_ai.rs 关于“TS 工厂把 chat 与 classifier 两个分片都展开进 models”的说法来自 spec/ai-core.md §2.3.4 的表格备注，仓库内没有 TS 源可逐行核对。
- all.rs 关于 builtin_providers() 每次重新解析目录、非零成本的论断由代码路径推断（catalog_chat_models 每次都会反序列化），未做基准测量；注释刻意没有给出性能结论。
- azure.rs 的“非对象 payload 会被替换成空对象”分支在正常适配器路径不可达，且没有测试覆盖；注释已标明它是防御性行为。

### u052-kiana-ai-group

- 按任务规则未运行 cargo check / cargo doc / cargo fmt：文中 intra-doc 链接（[`Value`]、[`serde_json`]、[`crate::providers::all::builtin_providers`] 等）仅人工核对可见性，需在允许执行文档检查时确认无新增 broken/private link 警告。
- model_schema 的键序语义依赖工作区 Cargo.toml 中 serde_json 的 preserve_order feature；已在 Cargo.toml 核实启用，若未来移除该 feature，文档与测试前提都会失效。
- apply_defaults 在当前仓库内只被 tests/compat_schema.rs 调用（全仓 grep 核实）；文档已按“当前调用方是测试”描述，未声称 kiana-config 会在加载配置时调用它。
- providers/mod.rs 的 parse_catalog_model 对未知 type 字符串静默按 chat 处理（例如未来新增类型不会被报错，只会被当成 chat）；属现有行为，未修改，仅记录。
- github-copilot 工厂注册了 openai-responses 实现，但 vendored 目录 data/github-copilot.json 当前只含 anthropic-messages 与 openai-completions 两组 api；是否为动态/未来条目保留无法从源码确认。
- 各 provider 工厂的 `.expect("... declares an api implementation")` 文案在 typesafe（只有 classifiers）等文件里并不精确，但这些文件不在本单元范围；本单元 17 个工厂都确实提供了 api，因此未改动。

### u053-kiana-ai-group

- Qwen Token Plan 个人版与国际站共用同一端点与 QWEN_TOKEN_PLAN_API_KEY，但 id/目录不同；仓库内没有文档解释两者并存的产品原因，注释只陈述配置事实。
- opencode 与 opencode-go 共用 OPENCODE_API_KEY；未找到说明同一个 key 是否设计为可同时服务两个 provider。
- personalize_open_router_catalog 使用 default_oauth_fetch()（只能由进程级测试替身替换），不走 StreamOptions.fetch/HttpClient 注入点；这是否有意为之未见文档（kiana-config 的刷新路径也没有客户端注入参数）。
- Radius 的 refresh_models 完全不检查 context.force，只要 allow_network 就联网拉取；spec 只描述了 TS 的分支，没有说明 force 对 Radius 的预期语义。
- 自定义 gateway 的出厂基线为空这一行为由测试锁定，但仓库内没有设计文档解释其动机；注释只陈述“刷新前不展示模型”的可观察结果。

### u054-kiana-ai-group

- `together` 位于 provider 顺序第 33 位：按 spec/ai-core.md §2.3.1、providers/all.rs 与 build.rs 的 PROVIDER_ORDER 手工计数，三处一致，但没有测试直接断言该序号。
- “CLI 的 provider 列表与 help 输出依赖 provider 顺序”来自 spec/ai-core.md §2.3.1 原文，未实际运行 CLI 验证。
- `truncate_http_body` 的“切点落在代理对中间时可能多保留一个 UTF-16 码元”依据 utils/text.rs 的 owned_slice 实现推断；现有测试只覆盖 ASCII，未构造真实代理对用例。
- “ProviderApiImpl::Single 不按 model.api 分派”依据 models/provider.rs 的 From 实现与 api_for；未新增测试验证，仅按源码核对。
- zai / zai-coding-cn 的端点语义仅依据 base URL 路径与 id 命名描述；注释中未断言具体地区归属，避免超出证据。
- “目录解析失败会 panic 且 build.rs 先校验”依据 build.rs 的 validateModelValue 与 providers/catalog.rs 文档，未运行构建验证。

### u055-kiana-ai-group

- openrouter_images_api() 每次调用都新建零大小 Arc 适配器，源码与文档未说明这是有意设计还是沿用 TS 包装结构；注释只陈述事实，未推断动机。
- 文档中新增的 crate::api::openrouter_images::generate_images 与 crate::image_models::generate_images 链接按源码 pub 可见性判断应可解析，但任务禁止运行 cargo，未实际用 cargo doc 验证。
- 模块文档原有的 Markdown 相对链接 [generateImagesOpenRouter](register-builtins.ts#...) 未改动，未用 cargo doc 确认它是否产生警告。

### u056-kiana-ai-group

- 现有注释引用的 ARCHITECTURE.md §4.2/§4.6 与当前 ARCHITECTURE.md 的章节编号不一致（当前 §4.2 是「工具」，§4.6 不存在；JS 语义在 §6.1）。按规则未改原注释。
- abort_signals.rs 的 Arc 引用环与监听累积是按代码结构分析的结论，仓库内没有对应测试或泄漏测试；models 的既有注释只说明丢弃 cleanup 不会停止传播，未提及资源滞留。
- estimate_context_tokens_for_context 在仓库内没有调用方；其「只应在已归一化上下文使用」的约束是根据函数实现与 Context/TranscriptContext 不变量推断的。
- AbortScope::abort_with 在并发调用下 reason 可能被后写者覆盖（先到者胜只在串行调用下成立），这是代码级分析，未写测试验证。
- serde_json::to_value(tools) 对正常 Tool/ToolReference 是否可能失败未验证；注释按 unwrap_or_default() 得到 Value::Null 从而低估的实际情况描述。
- race_with_abort_signal 依赖的 tokio::select!「多分支同时就绪时随机选择」与 tokio::spawn 在无运行时 panic 属于 tokio 文档化行为，仓库内无测试覆盖。

### u057-kiana-ai-group

- short_hash 的输出长度「13–14 字符」来自规范所述 20 万随机样本实测，不是数学保证；注释已标为经验值，未做更大规模验证。
- node_arch 只映射常见架构；未列出的 target（尤其 powerpc64 的大小端变体）是否与 Node process.arch 命名一致未核对。
- merge_client_identity 对「Some 源但全部字段缺省」返回 Some(默认结构)；spec/ai-core.md §2.2.22 只描述字段级覆盖，未规定该 Option 形状是否与 TS 完全一致。
- headers_to_record 文档中「响应头到达前已由 HTTP 客户端合并」依据 http/reqwest_client.rs 的 response_headers 测试；其它 HttpClient 注入实现是否同样处理未逐一核对。
- merge_options_identity 总是返回 Some，会把诊断快照里的 identity 从 null 变成 {}（见 api/simple_options.rs 的 base_options_json）；是否有测试依赖该形状未核对。
- probe_runtime_version 没有超时控制（已在函数文档中写明）：若 bun/node 可执行文件挂起，首次 format_user_agent 会阻塞；按约束未修改代码。
- utils/mod.rs 原有注释「identity 归 models-runtime 单元」与本单元分工不一致；按保留既有注释的约束未改动。

### u058-kiana-ai-group

- crate::error::ModelsError::with_cause_detail（crates/kiana-ai/src/error.rs:255）使用 Rust 的 str::trim()，而本单元与 TS 使用 JS trim：两者在 U+FEFF（JS 视为空白）和 U+0085（Rust 视为空白）上不一致。该文件不在本单元范围，未修改，建议主代理核实是否统一。
- node_http_proxy 在仓库内没有生产调用方（仅 mod.rs 声明与 tests/node_http_proxy.rs）；TS 的 Bedrock/Codex 适配器使用该工具，Rust 端口未接入，是否为待办事项无法从源码确认。
- model_operations::image_result 无调用方且 TS 中没有同名函数（TS 由 provider 直接构造成功对象）；该公开扩展是否预期长期保留未知。
- OpenRouterUsageAccounting 无法表达 TS 运行期的 undefined；仓库内适配器会在构造前过滤，但外部直接构造者需自行遵守，已在文档中写明。
- overflow 中 usage.input + cache_read 的 u64 相加与 output 的 u64→i64 转换在极端值下会回绕（debug 下相加会 panic），无测试覆盖该边界；注释只陈述代码行为，未做修复。
- 按任务约束未运行 cargo build/test/fmt/clippy/doc；rustdoc 链接与私有/公开可见性为人工核对，未经过编译器验证。

### u059-kiana-ai-group

- retry_delay_ms_js 的 NaN 边界已写入文档：Rust 的 f64::min 在 max_agent_delay_ms = NaN 时返回非 NaN 一侧，而 JS Math.min 返回 NaN。设置层/扩展 JS 互操作是否可能把 NaN 传入未验证；从 JSON 读出的配置不会产生 NaN。
- sleep 与 retry 内的 sleep_with_abort 都用 tokio::select!，默认在多个分支同时就绪时随机选择。当 ms = 0 且取消与定时器同时就绪时，理论上可能返回 Ok（retry 中可能多执行一次 produce）。未运行测试验证该窗口是否可观察，也未确认与 TS 监听器先注册的语义是否有差异。
- owned_slice 只调整 end 边界；start 落在代理对中间时，切片以孤立低代理项开头，String::from_utf16_lossy 会把它替换成 U+FFFD。当前生产调用方都传 start = 0（error_body、kiana_messages、radius_config），因此该边界目前不可达；已在函数文档中如实记录。
- spec/ai-core.md §2.2.4 称 getSystemMessageText 的 sections 顺序为「对象插入顺序」，但实现与单元测试 section_rendering_follows_js_object_key_order 按 JS 对象枚举序（数组索引键按数值升序优先）。实现与测试一致，推断 spec 表述是简化；未修改 spec。
- TS 的 Bun 回退还要求 process.versions?.bun 为真；Rust 实现只检查「进程环境是否一个变量都没有」，去掉了 Bun 判定（Rust 进程不运行在 Bun 下）。非 Bun 但进程 env 为空的场景下行为是否与 TS 等价未验证。
- sanitize_surrogate_units 在生产代码中没有调用方（全仓库 grep 只命中定义与本文件测试），已按事实写入模块文档；它是否计划供上层使用、是否应保持公开需主代理确认。

### u060-kiana-ai-group

- spec/ai-core.md §2.2.6 写“首次调用用 bytes[1..5] 组成 41 位 sequence”，Rust 实现取 bytes[1..=5] 共 5 字节（40 位）；注释按实现写，TS 原始字节范围未核对。
- Rust 的 string_enum 在仓库内没有生产调用方（仅经 index 再导出，JS 虚拟模块使用自己的 kiana-ai-helpers.js 实现），它是否属于下游公开契约未核实。
- uuidv7 在操作系统随机源失败时直接 panic、UuidV7Error 不提供随机源错误变体，是否为有意设计在 spec 中未找到明确说明；注释只陈述事实，未推断动机。
- version.rs 原注释中的 `kiana --version` 实际由 kiana_core::config::VERSION 提供（kiana-interactive 使用），并非本常量；新增小节已区分二者，原句按“保留已有注释”要求未改写。

### u061-kiana-ai-root

- index.rs 第 116 行注释称 crate::error 的 AiError/AiErrorKind 由 models::* 再导出，但 models/mod.rs 只有 `pub use crate::error::{ModelsError, ModelsErrorCode}`，AiError/AiErrorKind 仅为私有 use；`kiana_ai::index::AiError` 是否真实可达未用编译器验证。仅记录，未修改 index.rs（不属于本单元）。
- 任务描述称本单元约 48 行；磁盘上的 lib.rs 基线实际为 132 行，且已包含完整 crate 级 //!，因此本次为增量补充而非从零撰写。
- 按任务禁令未运行 cargo check/doc/fmt，新增 intra-doc 链接（index、cli、cli::main、bedrock_provider、bun_oauth、legacy_api_aliases、compat 等）仅人工确认目标为 pub 项，rustdoc 实际解析与 -D warnings 结果未经机器验证。
- 本文件既有的外层 /// 与对应模块文件内 //! 并存（error/event_stream/http/types/bun_oauth/legacy_api_aliases/session_resources/auth::oauth_compat），是否产生 broken-intra-doc-links 未验证；这是 COMMENT_STYLE §4.2 已描述的历史状况，本次未改动这些行。
- 新增第 8 条对 compat 模块“迁移完成后删除”的表述来自 compat.rs 的文件头说明；其余模块定位均取自各自文件开头 //!，未逐行核对实现。
- 既有“下游共 18 个 crate”已用 Cargo.toml 依赖键 grep 复核（18 个），但“42 个 builtin provider”仅依据 providers/mod.rs 文档与文件清单，未逐个枚举验证。

### u062-kiana-builtin-extensions-large

- 原有 UNIX_ERRNO 注释称“其它平台先按 ErrorKind 兜底”，但该表由 #[cfg(unix)] 编译且固定使用 Linux errno 数值；macOS 等其它 Unix 上部分数值不同，本次未在非 Linux 平台验证，也未改动原注释。
- ModelCallLimiter 存在隐含前提：等待中的 acquire future 若被丢弃，sender 留在队列，release 时 send 失败且 active 不递减，槽位会永久丢失；本次未实测该取消路径，只在文档中记为前提，未改代码。
- 原有常量 DEFAULT_MAX_CODEMODE_WORKERS 注释“允许一个替换正在退出的 worker”的准确含义未能从源码确认，保留原样。
- 原有 execute_codemode 首句“写入被丢弃”仅在 options.append_entry 为 None 时成立；新增文档按实际条件表述（取决于 append_entry 是否存在），原句未改。
- SandboxClosed 在执行路径中会映射为 Err，但本函数新建沙箱后立即 execute，该分支在此调用路径上基本不可达；文档按代码映射列出，未进一步断言平台行为。
- 按任务约束未运行 cargo check/doc/fmt/clippy 与 tools/check_comment_only_diff.py；改动纯度靠逐处 Edit 构造（新增内容全部为 //、///、//! 与空行）与人工回读保证，编译与 rustdoc 链接未实际验证。

### u063-kiana-builtin-extensions-large

- MCP 扩展用 reaches_tools 决定执行前等待哪些 server：依据 mcp/index.rs 的 on_tool_call 源码阅读，未运行测试。
- TUI 渲染器对无效 details 退回复 Default：依据 renderer.rs 的 details_of，未运行测试。
- golden 快照锁定描述文本：依据 tests/tool_search.rs 的 include_str 比对，未运行 cargo test。
- create_codemode_tool 在 models=true 时描述声明 models、但执行时 ctx 被还原为 None 导致 models 不注入；这与 TS 是否一致未核实。

### u064-kiana-builtin-extensions-large

- default_http_client 的回退路径「Client::default() 构建失败会 panic、且不关闭 reqwest 内建解压」依据的是 kiana-ai/src/http/reqwest_client.rs 的文档（第 143-151 行），未实机触发 TLS 失败路径验证。
- load_and_wait/download_and_wait 的 future 不满足 Send 的结论由 Rc<RefCell<...>> 跨越 .await 以及 llama/ui.rs 的 LocalBoxFuture（kiana-ext-api/src/lib.rs:131 无 Send 约束）推导，未用编译器验证。
- 未运行 cargo check/doc/fmt（任务禁止）；所有 intra-doc 链接仅指向 pub 项（私有函数/方法/字段一律用行内代码），但未经过 rustdoc 实际校验。
- LlamaModelsResponse、LlamaModelStatus 以及 LlamaModelMeta 的 size/ftype 在本 crate 内没有读取点（仅定义/测试赋值），保留它们是否属于对外 API 形状需求无法从源码确认。

### u065-kiana-builtin-extensions-large

- try_join_all 在首个错误后 drop 其余 /props future（可能取消进行中的 HTTP 请求），与 TS Promise.all 允许其余 promise 继续执行不同；既有 TS 注释只承诺“首个拒绝立即传播”，本文件测试也只覆盖该延迟语义，取消副作用未验证。
- resolve_server_url 对凭据 URL 做了两次 normalize（credential_server_url 内一次、调用处再一次）；与 spec §2.17.2 的 TS 代码一致，但‘对已规范化值幂等’只是从函数实现推断，仓库内没有专门测试。
- Provider::base_url 固定返回 DEFAULT_INFERENCE_URL 可从代码确认，但仓库内是否有展示/目录消费者读取该值（而不是 auth.base_url）未逐一追踪；注释只陈述方法返回值与 refresh_models 不读 context.base_url 这两点。

### u066-kiana-builtin-extensions-large

- oneshot 发送端被 drop 后接收 future 立即完成并返回 Err 属于 futures crate 的契约；本文件及测试没有直接断言“页面被替换/视图释放”路径（现有测试未覆盖），相关注释按调用点 .ok()/unwrap_or 的用法把它描述为取消。
- js_round(-0.5) 返回 -0.0 依赖 Rust Range<f64>::contains 包含起点；测试里的 assert_eq! 无法区分 +0.0/-0.0，符号位没有被单独锁定。
- render_requester_for_tui 的 SAFETY 论证依赖“request_render(false) 只置脏标记、不重入组件”：已在 kiana-tui/src/tui.rs 的 TuiRenderer::request_render 中确认，但 Tui 是 trait，其他实现未逐一核对。
- 未运行 cargo doc/check（任务禁止），新增 intra-doc 链接的有效性与 private_intra_doc_links 只能做静态检查，建议主代理运行 RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-builtin-extensions --no-deps 复核。

### u067-kiana-builtin-extensions-large

- js_round 在负数 .5 边界与 JS Math.round 不同（例如 -1.5 得 -2，JS 为 -1）；当前唯一调用点传入正的 timeout_ms，路径不可达，未修改代码，仅在注释中标注不要复用为通用 Math.round。
- 未实际运行程序验证 TTY/颜色分支：FORCE_COLOR、NO_COLOR 与 stdout.is_terminal 的优先级仅按源码顺序描述；测试只是剥离 ANSI 码。
- list 通过 join_all 同时推进所有服务器的连接，源码没有并发上限；未评估大量服务器时的资源占用。
- server 报告里的 `transport` 保留原始 JSON 值（可为 null/数字）依赖 config_to_value 的键存在性语义；对 stdio 携带 url 的条目仅在测试 lists_the_raw_url_of_stdio_configs_that_carry_a_url_key 中验证，未覆盖更多类型。

### u068-kiana-builtin-extensions-large

- 原 //! 称扩展注册的服务器“立刻连接”：session_start 实际把所有启用服务器（含注册项）一起在后台启动，只有会话中途注册/改配置才由 mcp_servers_change 立即连接（spec §2.9.23）。已按源码改写该句，建议评审确认措辞。
- 原 session_shutdown 内注释称“track 的 promise 在 settle 后从集合里移除”，但 track 实现只 push、从不移除，仅 session_shutdown 里 clear()。已改写为事实描述，并在 track 的文档中记录该与 TS 的差异（长会话中列表会累积到 shutdown）。
- format_status 中 needs-auth 分支位于 disabled 判断之前：一台“被禁用且连接状态为 needs-auth”的服务器会显示 needs-auth 行；现有测试样例未覆盖该组合。注释按代码实际顺序描述，未改行为。
- ensure_discovery_active 的退出条件是“任一存在的触达工具已在 active 中”：若 codemode 与 tool_search 都存在但只激活其中一个，另一个不可达时也不会发警告。spec §2.9.15 的描述与代码一致，已按行为记录，是否为预期需评审确认。
- tui_login 在 lookup_local 失败（扩展实例已销毁）时 slot 保持 None，login_command 随后会通知“Signed in ... (0 tools)”，把未执行当成成功。已在注释中如实描述该边界，未改代码。
- register_tool 对同名工具的覆盖语义来自本文件的重注册设计与 spec §2.9.13；未阅读 kiana-extensions 注册表实现，故注释没有断言其内部替换细节。
- render_servers_section / McpServerListing 在本仓库除 lib.rs 再导出与 tests/mcp_extension.rs 外没有调用方；文件原有“由下游 kiana-session 注入”的契约缺口说明沿用未改，我未能在 kiana-session 找到对应实现。
- 未运行 cargo check / cargo doc / cargo fmt --check（任务明确禁止），因此没有编译与 rustdoc warning 的实测结果；新增 intra-doc link 只指向 pub 项（MAX_SERVERS_SECTION_CHARS、McpExtensionOptions、create_mcp_extension、render_servers_section、McpServerConnection、McpManagerView）。
- McpServer 的 ready/closing 生命周期描述基于本文件内 new_ready/wait_ready/start_connection/reconnect/set_enabled 的用法；未在其他模块找到对这些字段的额外写入点（本文件内 grep 未发现遗漏）。

### u069-kiana-builtin-extensions-large

- 未能运行 cargo doc / rustdoc（任务禁止 cargo 命令），intra-doc link 是否全部解析只能人工核对：新增链接均指向本模块公开项或外部公开类型，私有目标一律写成行内代码；`Self::message`/`Self::name` 这类固有方法链接未实测。
- 原注释 `default_fetch` 写作“（共享的 reqwest 客户端）”，但每次调用都 `ReqwestFetch::new()` 新建 `reqwest::Client`；该注释是既有内容，未改动，措辞是否准确待核实。
- callback_settings 文档中的“默认端口视为未写”依赖 url crate 对 WHATWG 默认端口的归一化（`Url::port()` 对显式 80 端口返回 None）；未实际运行验证，属标准语义推断。
- remove 在现名键与 legacy 键同时存在时只删现名键、legacy 条目保留：由代码分支直接读出，但 tests/mcp_oauth_store.rs 没有覆盖“两键同时存在”的用例。
- McpAuthProvider::refresh 在没有 Tokio 运行时（Handle::try_current 失败）时由调用方 `flight.await` 驱动刷新：来自代码路径推断，测试均在 tokio 运行时下进行，未实测无运行时的场景。

### u070-kiana-builtin-extensions-large

- 「MCP App 正则要求分号，`text/html profile=mcp-app` 不命中」由正则字面量直接读出，未实际运行该用例验证（现有测试未覆盖无分号情形）。
- 注释说服务器名排序「与 TS 一致」依据 kiana-tui `locale_compare` 的文档（ICU collation，回退字节序）；跨环境与 Node 默认 locale 的完全一致性未实测。
- public_total=13 只统计本文件定义的公开项（trait 1 + 方法 7 + 类型别名 1 + 结构体 1 + 字段 2 + 工厂函数 1），不含 3 个 `pub use` 的常量重导出（其文档在 kiana-core）。
- 模块 //! 中「传输与重连由 runtime 层负责」依据 runtime.rs 的 McpServerConnection 文档与 with_client 实现，未逐条追踪重连内部逻辑。

### u071-kiana-builtin-extensions-large

- `error_message` 的原注释“被丢弃的 `Error` 消息”语义不明（函数实际用于 connect_failed 拼接错误文案），未改动，待核实原意。
- `oauth_url_str` 对 HTTP 条目不检查 truthiness，而 `oauth_url_value` 与 stdio 分支会按 JS truthiness/非空字符串过滤；异常配置下两者可能给出不一致结果，仅记录，未改代码。
- “会话过期（HTTP 404）表示请求未执行、工具调用可安全重跑”来自仓库内既有 TS 移植注释与 `McpTransportError::session_expired` 的构造语义，未在本任务中独立实测服务器行为。
- `close()` 文档写“可重复调用”，依据是 `CancellationToken::cancel` 与 `Shared` future 的幂等路径推断；仓库测试只覆盖单次 close，未专门验证并发重复调用。
- Node 等价断言（`file_url`/`expand_home`/`basename`）沿用既有测试注释中的来源说明，本任务未重新运行 node 验证。

### u072-kiana-builtin-extensions-large

- UNIX_ERRNO 表使用 Linux errno 数值，但编译条件是 #[cfg(unix)]；macOS/BSD 上 ENAMETOOLONG/ELOOP/EDQUOT 的数值不同。仓库内没有非 Linux 平台的证据，注释只沿用原有“Linux 数值”的表述，未断言跨平台行为。
- node_errno 的 11 条 errno 文案里只有 ENOENT/EACCES/ENAMETOOLONG 被单元测试锁定；EINVAL 来自 ErrorKind 兜底，其余条目（EEXIST/ENOTDIR/EISDIR/EMFILE/ENOSPC/EROFS/ELOOP/EDQUOT）的 Node 逐字文案未在仓库内找到测试证据。
- buffer_from_base64 的数组分支用 `number.trunc() as i64 & 0xff`：当 |数值| 超出 i64 范围时 Rust 会饱和转换，与 JS `& 255`（ToInt32 取低位）可能不同；仓库没有该边界的测试，注释只描述了代码行为，未断言与 Node 完全等价。
- kiana-ext-api 的 ToolDefinition::output_schema 字段文档写的是“成功结果中 structuredContent 的 JSON Schema”，而本单元给 MCP 工具放的是整个 CallToolResult 的 schema（tests/mcp_tools.rs:127 断言 codemode 按 `{ content; ...; structuredContent?: unknown; }` 渲染）；该文件不属本单元，仅记录未修改。
- McpToolDetails 在仓库内只有定义，没有被构造或引用（grep 只在本文件命中）；无法从仓库确认它是否为 SDK 预留的公开契约类型。
- 模块文档第 6 条差异声称默认保存器失败文案与 Node 等价，但占位路径的随机段来自 RandomState+纳秒+pid 的哈希，不是 Node 的 crypto.randomBytes；随机部分不可观测，实际只能保证形状一致（已在注释中按此描述）。

### u073-kiana-builtin-extensions-group

- index.rs 原注释中“清单顺序决定 /tools 的展示顺序”的断言，未在本单元找到 /tools 的展示/排序实现加以验证；该句被保留，新增文档只陈述了可验证的 builtin 枚举顺序。
- theme_snapshot 中 bold 在含换行/ESC 文本上与 Theme::bold 的差异来自静态阅读 kiana-theme/src/theme.rs 的 chalk_wrap 与 AnsiWrap::wrap，未运行 cargo test 实测（任务禁止）。
- 未运行 cargo check/doc/fmt（任务禁止），Rustdoc 链接仅人工核对：index.rs //! 中的 [`builtin_extensions`] 指向同模块公开函数，theme_snapshot 的 //! 未使用 Markdown 链接。

### u074-kiana-builtin-extensions-group

- index.rs 模块文档中『TS 的 prepare_loadout 也在会话的同一线程上运行』以及『从其它线程调用』的前提来自原有注释；本次只验证了查不到 API 时的回退值（options 覆盖值 / SETTINGS_DEFAULTS / None），未找到 prepare_loadout 跨线程调用的直接证据。
- worker.rs 的『TS 的 import 即启动 worker』来自 `kiana-codemode/src/runtime/worker.rs` 的模块文档，本仓库没有 TS 源码，未直接核对 TS 的 worker.ts。
- `ansi_parts` 的探测失败分支（返回两个空串）在当前 `Theme::fg` 实现下不可达（fg 总把文本嵌在前后缀之间），没有测试覆盖，文档按防御性分支描述。
- `format_duration` 对 NaN/负值没有专门分支，测试也未锁定其具体输出；文档只写『不校验』，没有断言结果。
- renderer 与 TS 的逐字一致性由 `tests/codemode_renderer.rs` 的 golden 值和本文件测试间接覆盖，TS 源码不在本仓库，未做逐字对照。
- 公开项与已文档化数量为人工统计，口径包含 pub mod 声明与 public struct 字段；若采用只统计类型/函数的口径，数字会不同。

### u075-kiana-builtin-extensions-group

- `model_is_loaded` 的原注释“sleeping 视为已加载，选中会唤醒”中的“唤醒”在 index.rs/ui.rs 没有依据：run_manager_loop 对 sleeping 模型走的是卸载分支（unload_model）。按规则保留了原注释未改，建议主代理核实“唤醒”是否另有所指。
- `HuggingFaceClient::request` 把传输与 HTTP 状态错误统一构造成 `AiErrorKind::Transport`，且 `name` 保持 `AiError::new` 缺省的 "Error"；TS 的 `fetch` 会抛出带 TypeError/TimeoutError 名字的错误。当前 index.rs 只按 message 展示、按 name+message 子串做连接判定，未见可见差异；若将来按 name 分支则可能与 TS 不一致。
- `parse_details` 的排序比较器在 left 与 right 同为 Q4_K_M 时返回 Less（非严格弱序）；由于量化名来自 IndexMap 键、天然唯一，实际不会触发，但复用该比较器时需注意。
- `sync_catalog` 里 registry 刷新会经 `provider.refresh_models` 的 publish `update` 再次写 provider 状态（含 stored 缓存恢复分支）。我读了 provider.rs 的实现，但未逐一验证 registry 的所有 publish 路径，因此文档只写“可能被覆盖”。

### u076-kiana-builtin-extensions-group

- config.rs 模块文档中‘fs::write 直接覆盖、没有临时文件+rename 的原子替换，因此写失败后不能假定磁盘仍是旧内容’来自 std::fs::write 的覆盖语义与源码调用，仓库内没有对应测试，也未实测写一半的现场。
- read_config_file 与 remove_mcp_server_config 先用 Path::exists() 判断：如果 stat 因权限等原因失败会走‘文件不存在’分支而静默跳过，与 Node readFileSync 抛 EACCES 的可见行为可能不同；仓库没有该场景测试。
- node_error_code 把 raw_os_error 数值硬编码为 Linux errno（如 21=EISDIR、20=ENOTDIR）；其它平台的 errno 编号不同，未在非 Linux 平台验证。
- log.rs 中 .1 已存在且是普通文件时的覆盖行为取决于 fs::rename 的平台语义；测试只覆盖 .1 是非空目录导致 rename 失败的情形。
- 跨进程并发追加/轮转的具体交错结果未实测；注释只描述代码中的检查顺序、锁范围与 append 打开方式。
- log.rs 测试头注释称 tests/mcp_tools.rs 使用 TS 生成的 golden，依据是测试函数名 matches_the_ts_golden_fixture 与 fixtures/mcp_core_golden.json，未追溯 golden 的生成脚本。
- public_total/public_documented_after 的口径：按 rustdoc 可见的顶层公开项统计（config.rs 含 6 个 kiana-core 重导出名与 McpConfigError::new；log.rs 计 struct+2 方法+1 函数；mod.rs 计 9 个 pub mod，其文档来自子文件 //!），未计入公开字段与枚举变体。

### u077-kiana-builtin-extensions-group

- 宿主是否真的会重复调用同一个 CustomFactory 未在仓库中找到调用路径；注释只按 `slot.take()` 的机械行为描述第二次调用会立即 done 并返回初始视图。
- `done()` 与宿主卸载组件的确切先后顺序未实测；ext-api 文档只说明 done 解析 custom 的 responder。
- “`request_render(false)` 只更新重绘标志、不同步渲染”核对的是 TuiBase / TuiMainScreen / TuiAltScreen 三个现有实现；若将来新增其它 Tui 实现需复核该 SAFETY 说明。
- `key_hint` 读取全局键位表和全局主题的结论来自 kiana-ui-kit 源码与 tests/auth_url_copy.rs 的全局键位安装；宿主传入的 KeybindingsManager 与全局表在真实会话中是否始终一致未实测。
- `SelectList::on_selection_change` 的具体触发条件（方向键/滚轮/鼠标）取自 kiana-tui 源码文档，未运行测试逐路径验证；文档中统一表述为“选中变化”。
- 原有注释“缺省实现仍是 AuthUrlComponent::copy”未改写：实际默认路径是直接调用 kiana_core::clipboard::copy_to_clipboard，再用 copy_with 复刻同一提示逻辑，行为等价但入口不同；保留原文供主代理判断是否统一措辞。

### u078-kiana-builtin-extensions-group

- index.rs 中“扩展 entry 失效 → ensure_active panic → 转发任务结束 → 后续 request 返回 None”的完整链路依据 kiana-extensions 的 loader 源码与 tokio 的任务 panic 语义推导，仓库测试没有覆盖该路径。
- spawn_local 在缺少 LocalSet 上下文时的具体失败形态（panic）未在仓库测试中验证；文档只声明“要求当前线程存在 LocalSet 上下文”。
- ToolSearchResultTool 的 Serialize/Deserialize 在本 crate 内没有实际消费点（只有 search_and_load 构造它），其序列化用途未能确认。
- create_tool_search_extension 文档提到的 SDK 重导出只在 kiana-coding-agent/src/sdk_exports.rs 看到符号转发，未验证其运行时行为。

### u079-kiana-builtin-extensions-root

- 既有注释称 TS 源为“24 个源文件”，但 spec/ca-builtin-extensions.md §0.1 列出 25 个源文件（含 3 个 .lazy.ts），ARCHITECTURE.md 第 58 行也记 25 / 7,585；未修改原注释，需核实计数口径。
- 既有注释引用“ARCHITECTURE.md §5.2”说明 .lazy.ts 合并，但根 ARCHITECTURE.md §5 是“每个 crate 的模块树”且没有 §5.2 小节，docs/ARCHITECTURE_OVERVIEW.md §5 是“五个可执行目标与四种模式”；未修改，引用章节号待核实。
- “资源 loader 按 builtin:<name> 实例化工厂、按清单顺序写入内置扩展表”来自本 crate src/index.rs 的模块文档与 tests/builtin_registry.rs，未直接阅读 kiana-resources 的 DefaultResourceLoader 源码。
- run_mcp_command 的退出码仅读了 mcp/cli.rs 的文档与分派开头（0 成功/帮助、1 失败），未运行测试覆盖所有子命令分支。
- “MCP 扩展在工具只能经脚本触达时会主动激活 codemode”依据 codemode/index.rs 的注释，未逐行追踪 mcp/index.rs 中的激活调用点。

### u080-kiana-chord-large

- `AbortSignal::token()` 返回底层 `CancellationToken`，外部可直接 cancel 从而绕过 reason 与 on_abort 监听器；本文件已把该行为写入文档，但它是否为预期 API 边界，源码与测试未说明。
- `AbortError::Reason` 的 `name()` 恒为 "Error"，因此 `is_abort_error()` 对 Error 原因永远为 false；已按现有代码澄清文档，但这是否有意（TS 里可自定义 error.name 的场景在 Rust 端口无法表达）未见设计说明。
- `await_handle_with_context` 的“drop JoinHandle 只 detach、任务继续运行”依据 tokio 语义；仓库测试 `stops_waiting_when_the_invocation_is_cancelled` 只 sleep 10ms 后发送消息，未直接断言任务收到消息。
- `kiana-durable` 的 `SnapshotContext` 是本 crate 之外的外部 `Context` 实现；本次未阅读其实现，无法确认它满足新增文档里的实现契约。
- `AbortSignal::any` 在遍历中发现某个来源已中止时提前返回、不再订阅其余来源；已按代码描述该行为，但是否有意（而非逐个订阅）未见注释或测试说明。

### u081-kiana-chord-large

- resolve_host 与 TS 可能存在数组输入差异：Rust 只匹配 Value::Object，而 TS 的 typeof input === "object" 包含数组（可按 key 索引）。已在文档中如实标注差异，未改代码；是否属可接受偏差需维护者确认。
- remote_from_json 对 JSON 浮点 version（如 1.0）经 serde_json::Value::as_i64 得到 None 并回退 0，最终报 positive-integer 错误；此结论依据 serde_json 的 as_i64 语义推断，未运行代码验证。
- MEMBER 常量在当前 crate 内没有引用点；原注释“保留给按品牌字符串判别的调试/序列化路径”是意图描述，源码中未找到对应路径（按规则保留原文未改）。
- Factory 文档称“实现者由 host/object 单元使用”，但本 crate 的 host.rs/object 目前是骨架占位，未找到实际调用点（保留原文未改，列入待核实）。
- contract_from_description_json 的“fromWireError 风格”是原有注释，未在本文件或测试中找到明确对应关系，保留原文未改。

### u082-kiana-chord-large

- 未运行 cargo check/doc（任务禁止），[`Op`]、[`Op::Replace`]、[`diff_revisions`] 等 intra-doc 链接仅经人工核对作用域，未由 rustdoc 实测；该文件父模块 delta/mod.rs 的 pub mod diff 无外层 ///，链接不会落到父作用域。
- same_value 与 TS sameValue 的等价性（对无环 Value，=== 成立时 equalJson 必成立）来自 kiana-json/js_object.rs 与 TS 源码的代码推导，没有单独测试；42 2 条金标准用例与模块内测试间接覆盖主路径。

### u083-kiana-chord-large

- delta/mod.rs 第 17 行注释称 Truncate 的 count 是“保留的 UTF-16 单元数”，与本文件实现 `utf16_slice(current, *count, None)` 及 tests/delta_ops.rs 的 truncation_counts_utf16_units（count 是丢弃的前缀码元数）矛盾。未修改 mod.rs，仅在本文件 Truncate 分支按实际语义注明；需主代理修正 mod.rs。
- apply_inner 中原注释“采用而非复制：消费者拥有它拿到的批次”与 `root = value.clone()` 矛盾（TS 的 apply 才直接采用 payload）。已改写为 clone 语义并说明与 TS 的可观察差异，需确认接受这处注释替换。
- RESERVED_SEGMENTS 文档中“与 TS 接受/拒绝同一组路径”的动机来自 TS 源注释（pi/packages/chord/src/delta/index.ts 第 134-145 行）与文件头的移植约定；Rust 侧没有独立的动机说明，严格来说属于移植意图推断。
- resolve_mut 是递归实现，递归深度等于路径段数且没有上限；超长路径的实际栈溢出阈值未测试，注释只陈述“深度等于段数、不限制深度”，未断言崩溃阈值。
- op_to_json 与 parse_op 的往返恒等性只对 wire 解析得到的 op 成立；手工构造超出 f64 精确范围的 Seg::Index 会按 JS 数字语义舍入，这一点已写入注释但测试未覆盖。
- 环境禁止运行 cargo doc/fmt/check，rustdoc 链接仅经人工核对（均指向本模块公开项，私有目标用行内代码），未做编译期验证。

### u084-kiana-chord-large

- SystemClock::set_timer 直接调用 tokio::spawn，因此无 Tokio 运行时时会 panic：依据是 tokio::spawn 的语义与本文件既有 spawn_detached 注释（“tokio::spawn 会 panic”），仓库里没有直接测试这条路径；同步场景的测试注入的是 TestClock，且两次提交都命中 wait<=0 的立即 flush。
- 公开文档中 [`OutputError::TAIL_RETENTION`] 的关联常量 rustdoc 链接未实机验证（按规则未运行 cargo doc）；若 rustdoc 不解析该形式，应改为行内代码。
- “本仓库其它源码模块目前没有引用 output” 是本次对 kiana 仓库 grep 的结果（仅 tests/output.rs、tests/output_skip.rs 与本文件测试引用），后续其他单元合并后可能失效。
- “stop 之后再登记的 waiter 由后续 stop 返回，或随 Progress 丢弃得到 ProgressAbandoned” 由源码推导，现有测试只覆盖“在飞期间登记、由 stop 返回”的场景。
- `OutputBuffer::snapshot` 的压缩透明性有 tests/output.rs（head 与 tail 各两组限制）和 tests/output_skip.rs 的随机快照对比支持，但“重复调用结果不变”本身没有逐次断言的专门测试，因此注释只写了测试实际验证的对比方式。

### u085-kiana-chord-large

- first_invalid_regex 只沿对象节点递归：数组形式的子 schema（items: [...]、anyOf/allOf/oneOf/prefixItems）会被跳过；同时只跳过 const/enum，default/examples 等数据位置的对象也会被当作 schema 递归，可能误报。两个行为均已按源码写入注释，但未实测其与 TS 的用户可见差异，也未修代码，需维护者确认。
- resolve_ref_value/find_dynamic_anchor 返回遍历中第一个命中的 $id/$anchor；serde_json（preserve_order）的 Map 迭代按插入序，而 TS 对象枚举会把整数样式键排到前面。含整数样式键的 schema 上解析顺序是否与 TS 一致，本次未验证。
- 既有注释「TS 的 BuildPattern/BuildPatternProperties 在构造校验函数时执行 new RegExp」未核对 TS 源码；本次沿用该说法，未独立验证非法 pattern 的抛错时机。
- hash64/value_hash 与 TS hash.mjs 的逐字一致性由 tests/schema.rs 的 golden 覆盖，但按任务约束未运行任何 cargo 测试，无法在本次确认。

### u086-kiana-chord-large

- 超大整数在两条校验路径上的差异未实测：内嵌 schema 的 `integer` 没有上限，而 `js_integer` 要求落在 i64 范围内；TS/TypeBox 对 1e30 这类 f64 整数的判定没有在仓库内测试，因此未把该差异写成事实，只记录了代码侧行为。
- `DescriptionToken` 不保留 `DescriptionSchema` 允许的未知字段，而 `contract::Description`/`MemberDescription` 用 `#[serde(flatten)]` 保留额外字段并计入契约指纹（见 contract/describe.rs）；未来 token→Description 的转换若经过本类型会丢字段，但该转换代码尚未实现，影响无法确认。
- `contract::Description.version` 的文档写“正整数”，线上 `DescriptionToken.version` 的 schema 只有 `type: integer`、没有 minimum（0 与负数合法）；以哪边为准需维护者确认（wire 侧按 schema 如实描述，未改 contract）。
- 任务禁止运行 cargo build/test/fmt/clippy/doc，因此 rustdoc 只做了静态核对：公开文档中的链接均指向公开项（crate::index、Frame、Frame::validate、Frame::Call、Frame::Hello、Frame::to_json、DescriptionToken::host、is_frame、to_wire_error、from_wire_error、PROTOCOL_VERSION、ChordErrorCode、crate::errors::ChordError::full_message），私有项一律用行内代码，但未经 cargo doc 实机验证。

### u087-kiana-chord-group

- 未运行 cargo fmt/check/doc（任务禁止），intra-doc 链接目标全部按源码人工核对存在（含 errors 的 NAME、index 的模块链接），但未做 rustdoc 机器验证；建议主代理跑 `RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-chord --no-deps`。
- registry.rs 原有“具体实现由 host::tables 提供”与 types.rs 原有“实现在 object::refs/remote”是计划性描述；这两个 Rust 模块目前都是骨架，无法在源码中验证将来的实现，注释按原文保留。
- index.rs 的再导出面与 TS index.ts 并非逐项一致：Rust 多导出 ChordErrorReason，且未导出 TS 有的 ValueOf/EventOf（以及其它尚未移植模块）。未改 pub use（禁止改代码），留待 lead 合并时确认。
- StaticRegistryView 在本 crate 内只被其自身单元测试使用，是否会被 host 之外的代码复用未知；注释未声称宿主实现会复用它。
- kiana-durable 对 copy_json 的两处调用仅通过阅读 crates/kiana-durable/src/harness/scheduler.rs 确认，未运行其测试。
- publish.rs 关于多线程 hold 的结论来自对 drain 入口检查与循环的源码阅读（HELD 为全局 AtomicUsize、检查发生在拿队列锁之前），未用并发测试复现（任务禁止运行 cargo test）。

### u088-kiana-chord-group

- ARCHITECTURE.md 把 connection/feed.rs、connection/relay.rs 映射到 connection/feed.ts、connection/relay.ts，但 spec/chord-core.md §0.1/§2.4 与当前检出的 TS 树（/home/ctyun/code/kiana_rev/pi/packages/chord/src）都只有单文件 connection.ts，没有 connection/ 子目录；两个 Rust 文件的实际拆分边界需架构者确认。文档中已按事实并列陈述，并提示以 §2.4 条目为准。
- 文档中 Feed/RelayRef/RelayCall 的语义全部转述自 spec §2.4，并明确标注“尚未实现”；Rust 侧是否按这些名字拆类型、所有权/生命周期如何设计，目前无源码可证，文档未作断言。
- mod.rs 的 public_total=2 按两个 pub mod 声明计；它们的 rustdoc 文档来自各自文件内的 //!。若统计口径不含 mod 声明，则应为 0/0。
- mod.rs 中“两份实现会导致两套 id 与窗口记录”是对文件头 TODO 的工程解释，依据是 spec §2.4.5 列出的 #nextId/#unacked/#pending 等字段，不是对现网行为的实测。
- “本工作区检出的 TS 参考树”指与 kiana 并列的 pi 检出；若该参考树日后拆出 connection/ 目录，文档中的现状描述会过时，但以 §2.4 为准的移植指引仍然成立。

### u089-kiana-chord-group

- 新增的 intra-doc 链接（Context、ContextRef、ContextKey、ContextValue、KeyIdentity、AbortSignal、AbortReason、AbortError、BACKGROUND_CONTEXT、TODO_CONTEXT、with_context_value、with_abort_signal、without_abort_signal、with_telemetry、trace_in、trace_in_try、create_context_key）都指向本文件 pub use 清单里的公开项；按任务规则未运行 cargo doc，rustdoc 解析结果未实测。
- “await_handle_with_context 取消后任务继续运行”依据 index.rs:761-762 的文档与 tests/context.rs 的测试注释；该测试只发送消息并 sleep，未断言任务最终完成，也未在本仓库实测 tokio JoinHandle drop 后 detach 的行为。
- “kiana-durable 等下游 crate 通过 kiana_chord::context::… 使用本模块”依据 grep 到的 import 行（kiana-durable、kiana-env-client），未逐个核对其具体用途。
- 原有注释“按 ARCHITECTURE §4.2”对应仓库根 ARCHITECTURE.md 的 4.2“异步运行时”（CancellationToken + 可选原因）；docs/ARCHITECTURE_OVERVIEW.md 的 4.2 是“工具”。同名文档不同，本次未改动原注释，仅在此记录出处依据。
- “context feature 已在 Cargo.toml 声明但源码没有任何 #[cfg(feature = "context")]”来自对 src/tests 的 grep，仅在当前源码状态下成立；将来接入门控后该句会过时。

### u090-kiana-chord-group

- describe.rs 模块头原有句子称 isFrame/fromFrame 的 hold.token 校验依赖本文件的 JSON Schema；但 wire.rs 实际使用独立的 DescriptionToken 加手写校验（is_schema_object/is_placement/is_member_description），并未调用 description_schema()。该句可能指 TS 侧或合并后的计划，未改动，需核实。
- coverage.rs 中 Accepts 的原有注释称 JS 数字键按数值升序迭代；严格说 JS 只对 0..2^32-2 的整数索引键提前并按数值排序，超过该范围的版本号会退化为字符串键插入序。已追加限定说明而未改写原句，请确认表述。
- coverage/bind 的 reason 与错误消息只对照了仓库内 spec/chord-core.md §2.2.9-2.2.11 与 spec/chord-rest.md §3.3 的文本，本环境没有 TS 参考源码，未做逐字比对。
- Description 带 extra（#[serde(flatten)]）时额外键在序列化输出中的确切插入位置没有测试覆盖；本文件只对无 extra 的固定描述断言了逐字节输出。

### u091-kiana-chord-group

- delta/mod.rs 中“超过 i64 的路径段保留为 Seg::Key、数组父容器随后报 UnsafePathError”依据 index.rs 的 number_to_seg/check_parent_key 静态阅读，未运行测试验证。
- apply_immutable_trusted 的“TS 侧唯一调用方是 tracker.ts”、revision_validator 的“TS 侧只有基准测试直接引用”来自对 pi 树的 grep（源码+测试，不含编译产物）；若存在重导出后的动态/间接调用，措辞需复核。
- revision_validator 的“安全构造的 Value 上 validate 不会返回 Err”“finished/ancestors 永不命中”依赖 serde_json::Value 为树且 Number 不能表示 NaN/±Infinity；未穷举所有反序列化入口实测，若未来引入共享/可循环表示需重评。
- pending 的“折叠不改变回放结果”只由 tests/delta_pending.rs 的若干用例锁定，不是形式化证明；注释已按“测试锁定”措辞。
- tracker/draft 的职责与行为描述全部来自 TS 源码与 spec/chord-rest.md §2.30，Rust 侧未实现，无法用 Rust 代码或测试交叉验证。
- 既有测试 revision_validator::tests::deduplicates_within_one_revision 的注释声称验证 DAG 去重，但 json! 构造出的是两棵独立副本，finished 实际不会命中；该测试只能验证“接受”，未修改测试，仅记录。
- revision_validator 中 PLAIN_CONTAINER/DENSE_ARRAY/ARRAY_ENTRY/SYMBOL/OBJECT_ENTRY 五个公开错误文案在当前实现没有任何构造点；是否保留或调整属于 API 决策，未改动。

### u092-kiana-chord-group

- ARCHITECTURE.md 第 923-927 行与四个文件首行都写 host/facets.ts、host/roots.ts、host/routes.ts、host/tables.ts，但当前检出的 TS 源 /home/ctyun/code/kiana_rev/pi/packages/chord/src 只有单文件 host.ts，没有 host/ 目录；无法确认这些 TS 文件是否存在于其它快照/分支。已在 host/mod.rs 的 //! 中以“待核实”标注。
- spec/chord-core.md §8.1 的标题明确是“建议布局”，四个子模块的最终职责划分可能调整；本次文档一律使用“计划职责/建议布局”措辞，未当成已定结论。
- 本单元五个文件均为空占位，没有代码、调用方或测试；facet/root/route/registry 的语义描述来自 spec/chord-core.md 与 TS host.ts（以及 registry.rs 的现有文档），不是已实现的 Rust 行为，无法在本仓库内实测验证。
- index.rs 第 15 行称 host 再导出“3 个值”，而当前 TS index.ts 从 host.ts 再导出的值只有 createHost 与 defineFacet 两个；第三个值无法确认（可能是其它快照或表述误差）。本单元未改动 index.rs。
- routes.rs 中“最长前缀匹配”与 route.principal 缺失抛 TypeError 来自 TS host.ts 的 #routeFor/route 实现，Rust 侧尚无对应代码或测试可验证。
- tables.rs 引用的 RegistryView/RegistryTable/Registry 公开契约与 crate::registry 现有文档一致，但 host::tables 尚未实现，无法验证未来实现是否严格保持 superseded 与同步首回调语义。

### u093-kiana-chord-group

- TS 布局不一致：ARCHITECTURE.md 的映射表与六个文件原有首行注释提到 object/declarations.ts、object/invoke.ts、object/refs.ts、object/resource.ts，但当前检出树只有 pi/packages/chord/src/object.ts（1932 行，spec/chord-core.md §2.3 也按单文件分析），未找到拆分后的 TS 源。
- declarations.rs 的 registries 职责：spec §8.1 把 registries 列入该文件，但 TS 中注册表校验/重建由 HostCore.checkRegistrations / registriesChanged 在 ObjectImpl 内调用，Rust 落点未定。
- unwind 在 spec §8.1 中同时列在 object_impl.rs 与 invoke.rs；两份模块文档分别从状态机与释放入口描述，实际实现归属需确认。
- spec §8.1 把 install 列为 object_impl.rs 的方法，但 TS 的 ObjectImpl 没有名为 install 的方法（安装分散在 #activate 与 reload.ts 的 commit 中）；Rust 侧的 install 边界需确认。
- RefCore 的 Rust 形态未定：spec 说 refs.rs 提供 RefCore trait，而 crate::types 已有 RefBase/Ref；是否合并或如何分层没有源码依据。
- ObjectImpl 的 Rust 并发/锁模型未在源码或 spec 中给出；文档只描述 TS 行为，未断言 Send/Sync、锁类型或持锁范围。

### u094-kiana-chord-group

- ARCHITECTURE.md（约 944-947 行）把 reload/{prepare,gate,commit}.rs 映射到同名 TS 文件 reload/{prepare,gate,commit}.ts，但当前检出的 TS 源 pi/packages/chord/src/ 只有单文件 reload.ts；本单元按 spec/chord-core.md §8.1 的拆分来描述，Rust 最终是否采用该布局未验证。
- 四个文件全部是空占位：文档中的流程、默认值（5000 ms、ABORTED_ROOTS_GRACE=1000 ms）和错误码（aborted/gate_deadline、contract_mismatch、upgrade_required、unavailable/reload_running、superseded）均来自 TS reload.ts 与 spec/chord-rest.md §2.20，Rust 侧尚无实现，也没有 reload 测试可交叉验证。
- spec §8.1 未说明 ReloadOptions/ReloadReport 应放在哪个子文件；只能确认 index.rs 的再导出表要求从 crate::reload 提供这两个类型，文档中已标为“放在哪个子文件待定”。
- spec §8.1 把 summary 列在 reload/mod.rs，而 TS 中它是 Reload 类的公开方法；Rust 侧最终是 impl 方法还是独立函数未确定，文档只按 §8.1 的布局陈述。
- public_documented_after 的计数口径：mod.rs 的 3 个 pub mod 没有也无法添加外层 ///（任务与 COMMENT_STYLE 均禁止），其 rustdoc 文档由子文件 //! 提供，故按“有文档的公开模块”计为 3；若统计工具只数 ///，此处应为 0。

### u095-kiana-chord-group

- read_row 对 TEXT 使用 String::from_utf8_lossy（非法 UTF-8 变替换字符）：这是本实现的行为；未验证 node:sqlite/TS 在同样输入下的表现。
- node_errno 只映射源码列出的 errno；未核对目标 libuv 版本是否还有其它错误码文案。
- 事务回调 panic 时由 TurnGuard::drop 回滚这一路径来自 Rust unwind 语义；仓库测试只覆盖 future 被丢弃，没有 panic 用例。
- cloudflare::UNSUPPORTED_MESSAGE 在本 crate 内没有引用、也没有测试断言其与 Display 相等；是否有外部消费者依赖该常量未核实。
- first_statement_prefix 用指针差切 &sql[..offset]，假定 offset 落在 UTF-8 字符边界；未用非 ASCII 多字节 SQL 实测该分支。
- open_inner 中“foo.db 的 parent 是空路径”依据 std::path 语义；本仓库测试没有直接覆盖该分支。
- SqliteRow 重名列“后值覆盖、位置保留”依据 IndexMap::insert 语义；本仓库没有重名列测试。
- migrations 模块文档中“失败时数据库保持调用前状态”以适配器遵守回滚契约为前提；本 crate 内只有 node 实现，未验证其它适配器。
- NodeSqliteDatabase 可跨任务/线程同时调用的说法来自 Send + Sync 约束与内部互斥锁；没有多线程压力测试数据。

### u096-kiana-chord-group

- carrier 键不一致：spec/chord-rest.md §2.34 写作 `kiana-sqlite-span`/`kiana-sqlite-trace`，而参考 TS 源 packages/chord/src/telemetry-sqlite/index.ts 与 test/telemetry-sqlite.test.ts 均为 `pi-sqlite-span`/`pi-sqlite-trace`；已在 mod.rs 注释中并列标注，最终采用哪组需移植负责人确认。
- feature 与模块声明不一致：Cargo.toml 定义了 `telemetry-sqlite = ["sqlite"]`，但 lib.rs 无条件 `pub mod telemetry_sqlite;`，且当前模块未使用 rusqlite。这是否是骨架阶段的临时状态、未来是否应加 #[cfg(feature = ...)]，需要架构负责人确认；本单元未改代码。
- store.rs 的 Rust 侧公开签名尚未定义（如无行时用 Option 还是空 trace、函数命名、异步返回类型），注释只陈述 TS 行为，未写任何 Rust 类型或签名断言。
- TS 侧只看到两个测试文件调用 createSqliteTelemetry，未发现生产代码调用方；若仓库外或后续分支存在嵌入方，模块文档中的调用关系描述可能不完整。

### u097-kiana-chord-root

- public_total=29 按 `pub mod` 模块声明计数（本文件没有其它公开项）；这些模块的文档都写在各文件自身的 //!（符合 COMMENT_STYLE §4.2，未加外层 ///）。仓库里没有 tools/doc_inventory.py，无法确认该工具的公开项统计口径是否计入 pub mod。
- 禁止运行 cargo check/doc，所有 intra-doc 链接（index、errors::ChordError、contract、declare、types、delta::Op、schema、wire::Frame、wire::Frame::validate、context::Context、context::ContextRef、types::Ctx、registry、publish、output、json、sqlite、delta::pending::PendingOps）只做了源码可见性人工核对（目标均为 pub 项），未经 rustdoc 编译验证。
- “现状与边界”一节的模块完成度是当前快照的时点信息：依据各文件是否存在 pub 项以及其文件级说明（如 connection/host/object/reload/telemetry_sqlite 的 //!），未通过构建或测试验证；后续单元落地后该节会过期。
- “kiana-durable 使用 json”由 harness/scheduler.rs 的 copy_json 导入与 src 中的 use 推导；测试目录里的使用未计入下游用途。

### u098-kiana-codemode-large

- 既有文档中的实测数字（MAX_TYPE_DEPTH 的 595 层 abort、每层约 4 KiB 栈、Node 26 的 3295/11385/6956 层、黄金样例最深 11 层）本次未复测（任务禁止 cargo test），保留原文。
- “render_declarations 在本仓库 Rust 生产代码里没有调用点”来自本次全仓 grep；仓库外的调用方不可见。
- render_global 要求 signature 不含结尾分号这一点由代码 format!("{head}{signature};") 推出，未在 TS 侧核对调用约定。
- “MAX_TAG_CHARS 先测量、后做行终止符转义”的 TS 顺序未直接核对 TS 源码；黄金样例只覆盖了远低于上限的含转义 tag。

### u099-kiana-codemode-large

- js_u64 的边界：代码用 `number > u64::MAX as f64`（严格大于），而 `u64::MAX as f64` 恰为 2^64；按 Rust 浮点转整数的饱和语义，恰好 2^64 会得到 `u64::MAX` 而不是 None。我据此改写了该函数文档，但未运行验证，且仓库内没有针对该边界的测试。
- `result_future` 中 `receiver.changed()` 返回 Err 的兜底分支在 `_keepalive`（future 自持 `Arc<Execution>`）存在时似乎不可达，原注释保留未改；这只是阅读推断，未实测。
- HostEventLoop 的「不会错过一次 set_idle」结论来自 watch 的订阅/版本语义（先 subscribe 再读 generation）；`tests/execution.rs` 没有直接覆盖闸门时序，未实测。
- TS 源文件 packages/codemode/src/runtime/execution.ts 不在当前工作区；所有 TS 行为对照均转引自本文件既有注释与 crates/kiana-codemode/tests/execution.rs，未直接核对 TS 源码。
- `Executors` 既有注释称「构造后新注册的全局函数能被在途执行看到」（TS 的活表语义），但 Rust 侧 CodemodeSandboxBase 没有注册全局函数的公开入口，该场景实际不可触发；未改动该注释。

### u100-kiana-codemode-large

- `qjs_get_quickjs_version()` 返回的指针是否需要 `qjs_free_cstring`：`Exports::read_c_string` 只读不释放，无法从本仓库确认该导出返回的是静态串还是分配串，因此未在注释中断言所有权。
- `__stack_pointer` 仅用于快照/恢复的说法来自原文件既有注释，未能在仓库内找到快照实现佐证；我保留原注释，只在 Exports::load 的新增说明里复述了“只确认存在”。
- `Session::create` 未包 `catch_unwind`：若其中 panic，VM 线程会带着未发送的 ready 通道退出，`start_vm` 得到 `Failed to start QuickJS`——该结论由 std::sync::mpsc 的 drop/recv 语义推得，未实测。
- `fd_fdstat_get` 的偏移 0/2/8/16 按 wasi_snapshot_preview1 的 `__wasi_fdstat_t` 布局解释；仓库内没有 ABI 头文件可对照，属于外部规范知识。
- 既有模块文档把 `WASM_STACK_SIZE`、`INLINE_WASM_STACK_SIZE`（pub(crate)）和 `VM_THREAD_STACK_SIZE`（私有）写成 intra-doc link；若以 `RUSTDOCFLAGS=-D warnings` 构建可能触发 `private_intra_doc_links`。按要求未改动既有注释，新增文档一律用行内代码而非链接。
- `Session` 持有的 `api`/`settle`/`stalled`/`bridge`（以及 preamble 局部的 `run`）从不 `qjs_free_value`，只能随 VM 上下文销毁；注释只写了“从不单独释放”，未断言销毁时机。

### u101-kiana-codemode-large

- “CodemodeResult/CodemodeOutcome 的反序列化要求自描述格式”依据 serde_json::Value::deserialize 使用 deserialize_any 的机制推断，仓库内没有用非 JSON 格式的测试。
- deserialize_optional_value 文档中“带 #[serde(default)] 时键缺失不会调用 deserialize_with”依据 serde_derive 1.0.229 的 expr_is_missing 与 serde 的 missing_field 实现（cargo registry 源码），仓库内没有直接覆盖该语义的测试。
- worker release 回调的 panic 被 catch_unwind 吞掉，前提是 panic=unwind；workspace 的 [profile.release]/[profile.test] 未设置 panic=abort，但本单元未运行验证。
- “Sandbox host failed: control.onCall is not a function”来自 runtime/execution.rs 的 forward_call 与 HostFailure 格式化路径，未运行测试逐字比对。
- 模块文档中“journal 与 worker/remote 协议都按这个形状读写”沿用文件原有说明与 spec，未在 kiana-durable 与 remote.rs 侧逐字段复核。

### u102-kiana-codemode-large

- describe_read_error 对除 EISDIR 之外的所有 errno 都把 syscall 记为 open。Rust 的 std::io::Error 不携带 syscall，仓库测试只覆盖 ENOTDIR/EISDIR/ENAMETOOLONG/ELOOP/ENOENT/EACCES；其他 errno（如 EIO）是否可能来自 read、从而在 Node 文本里应显示 read，本仓库无法验证。原有注释未改动，仅记录。
- NUL 路径“先转义再截断”的结论来自用本地脚本按 node_inspect_string/node_str_escape 的逻辑重放 tests/fixtures/nul_path_cases.json（19/19 匹配；boundary_63、boundary_64、long_ascii 可区分两种顺序）。本次未运行 Node 26，黄金值来源依据的是测试注释。
- “debug 构建一次编译十几秒”沿用文件中已有注释，本次未运行 cargo，也没有实测编译耗时。
- shared_engine/inline_engine 的“Engine::new 拒绝 Config 时静默回退”来自代码里的 unwrap_or_else 与现有注释；未构造失败用例验证回退后 max_wasm_stack 的实际取值，也未验证 Engine::new 的全部失败原因。
- 公开项计数按生产代码中的 11 条 pub 声明统计（9 个唯一名称：CodemodeWasmModule、QUICKJS_WASM_BYTES 因 cfg 各出现两次）；cfg(test) 中 test_hooks 的 3 个 pub 测试钩子未计入。
- EMBEDDED_WASM_SHA256 文档里的“长度固定 64 个 ASCII 字符”是对当前字面量的描述，代码与测试都不校验字符串长度。
- “成功槽位永不删除”是就本文件内部调用路径而言：evict 只在 load_quickjs_wasm 的失败分支被调用，且缓存表是私有静态、其他文件无法直接访问；未检查 crate 外是否存在间接清理机制（逻辑上不可达）。

### u103-kiana-codemode-group

- cloudflare.rs 中“TS 两个入口都是同步的（cloudflareQuickJSWasm 是常量、createInlineCodemodeSandbox 返回沙箱）”依据仓库外的 TS 参考树 /home/ctyun/code/kiana_rev/pi/packages/codemode/src/cloudflare.ts；本 Rust 仓库内没有该 TS 文件，若参考版本变化需复核。
- index.rs / portable.rs 关于“rustdoc 对 pub use 再导出继承源项文档”的说明是 rustdoc 的通用行为，本轮按任务约束未运行 cargo doc 验证。
- source.rs 对 Lark 语法三段（OPTIONS_LINE 允许行首空格/TAB、NEWLINE 允许 CRLF、SOURCE 至少一个字符）的解读来自常量字面文本，未用 Lark 实际解析验证。
- prelude.rs“PRELUDE_HASH 进入 VM 指纹”依据 runtime/vm.rs 中的格式化字符串（约 2269 行）包含 `prelude {PRELUDE_HASH}`；完整指纹拼装规则由 vm 单元维护，未逐行复核。
- cloudflare.rs“任何目标上结果相同”依据两个函数体只返回 Err、无 cfg/平台分支；未做多目标编译验证。
- 既有措辞差异（未修改）：index.rs 模块文档称 PRELUDE_SOURCE“任何公开入口都不导出”，但 Rust 侧 `pub mod prelude` 与 `pub mod runtime::prelude_source` 都可公开访问到它；这里的“公开入口”指 TS 的 package 入口（index.ts/portable.ts），我在 prelude.rs 新文档中按 Rust 可见性写明了实际路径。
- 既有笔误（未修改，不构成与代码矛盾）：source.rs 的 parse_codemode_source 文档写“可选的首页”，应为“首行”；prelude.rs 既有注释里的 “plannner” 拼写。
- module_doc_before 的 schema 只允许 none/thin/adequate；cloudflare.rs 与 source.rs 原有模块文档实际已较丰富（接近 rich），此处按枚举上限记为 adequate。

### u104-kiana-codemode-group

- check_worker_url 的绝对路径判定用 `url.starts_with('/')`，只在 Unix 语义下等价于 Node 的 path.isAbsolute；Windows 盘符路径（如 C:/x.js）会被拒绝。测试只锁定了这组输入在测试平台上的结果，未验证 Windows 目标的行为。
- `VmSession::dispose`「发 Stop 并 join VM 线程」沿用 inline.rs 原有模块文档的说法，未逐行核对 vm.rs 的线程实现，也未验证 dispose 是否可能长时间等待。
- `InlineChannel::stop` 在另一任务正持锁执行 `post`→`deliver` 时会阻塞到该段脚本让出（同一把 std::sync::Mutex；已按代码写入注释）；它对 Tokio 运行时工作线程的实际影响未做测量。
- prelude 的 4 个 MAX_* 常量在本 crate 内只被再导出、没有运行时代码读取；这一结论基于对整个 src 目录的 grep，不排除下游 crate 另行引用。

### u105-kiana-codemode-group

- js_math_min 对 ±0 的处理与 JS Math.min 不同（实现按 a<b 返回 b，a=-0、b=+0 时得到 +0）；未运行 Node 实测 JS 侧，且未修改代码，仅在注释中陈述实现的选择规则。
- WorkerDone.ok / WorkerDoneError.ok 的 deserialize_true/false 只在反序列化时校验；序列化会原样写出字段值，手工构造错误值仍会写出矛盾消息。已在字段文档标注安全构造入口，未改代码。
- “type 在首位”的断言来自 serde internally-tagged 序列化行为与 src/runtime/protocol.rs 单元测试、tests/protocol.rs 的文本断言；按任务约束未运行 cargo test，未现场复验。
- serve_codemode_remote 的返回依赖 end() 被触发；若客户端提供的 exchange future 永不结算，end 的停止任务会一直等 delivered，serve 也就不返回（与 TS 的 Promise 语义一致，但未实测该病态输入）。
- memoryLimitBytes 若为 -0（JSON "-0"）时 js_math_min 的符号处理及 VM 对负零上限的反应未验证；仅在注释中记录实现选择规则。
- VmStartData.store 的既有注释写 `serialize_js_map`，代码实际使用 `serialize_js_keyed_map`（两者同源）；属原有注释措辞，未改写。
- WorkerData 在本 crate 内没有任何构造点/使用点（全仓 grep）；“保留它是为了与 TS 公开面一致”依据 index.rs 的契约表，而非注释或测试的直接陈述。

### u106-kiana-codemode-group

- default_start_vm 只把 runtime/vm.rs 的 StartVmError::message() 搬进本文件的 StartVmError，name 被重置为 "Error"：代码事实明确，但这是有意偏差还是疏漏无法从注释/测试确认。生产路径 spawn_worker_thread 固定传 max_stack_size: None，普通执行不会走到 vm.rs 的 RangeError 校验；只有直接构造 WorkerStart 调 run_worker 时可见。
- worker 线程/VM 专用线程的双层模型、deliver 的同步阻塞语义与 Handle::drop“只发 Stop 不 join”均依据 crates/kiana-codemode/src/runtime/vm.rs 的模块文档和 Handle 实现；本次按要求未运行任何 cargo 命令，未做运行时实测。
- “WorkerToHostMessage 的 serde_json::to_value 不会失败”是依据当前协议字段只有字符串/整数/布尔/枚举推断的；若将来加入非字符串键 map 等形状需复核该 expect 注释。
- “TS 不做第二步反序列化”依据 spec/agent-codemode.md §2.2.13 对 worker.ts 的描述（仓库内没有 TS 源码），未逐字对照 TS 实现。
- 退出码 1 的分支只有源码依据；tests/worker_lifetime.rs 只覆盖了 panic 后的 exit(0)（a_panicking_start_vm_reports_an_error_and_exits），没有测试断言 terminate 后 exit(1)。
- “默认后端下 worker 线程不执行 JavaScript”来自 vm.rs 的专用 VM 线程设计（start_vm 在 ready_rx.recv 处阻塞），未在本会话运行验证。

### u107-kiana-codemode-root

- crate 根 //! 中的短链接（如 [`CodemodeTool`]、[`parse_codemode_source`]）依赖 `pub use index::*` 的 glob 再导出解析；按 rustdoc 的 use 绑定解析应有 效，但任务禁止运行 cargo doc，未做工具验证。若报 broken-intra-doc-links，可改为 [`types::CodemodeTool`] 等全限定路径。
- public_total=12 的口径是：本文件内 11 个 `pub mod` + 1 个 `pub use index::*`。按注释规范不得给父模块的 mod 声明新增外层 ///，因此模块的文档来自各自文件的 //!，再导出项的文档来自 index.rs 及其源项。
- 『宿主据此在重启后重放』依据 kiana-durable 模块文档与 execution.rs 的 start() 校验/确定性参数，未逐行追踪 durable 的重放实现（本次只读了其模块头与 start 调用点）。

### u108-kiana-coding-agent-large

- app.rs:218 既有的 SAFETY 注释假定「单线程启动阶段」，这只对两个二进制入口成立；main 是公开的 SDK 入口，嵌入方若在已有其它线程时调用，std::env::set_var 的安全性由调用方负责。已在模块 //! 记录该调用方义务，但未改写既有 SAFETY 注释。
- stdout 接管在 --help/--list-models 的提前返回路径（仅当显式 --mode/--print 时才已接管）以及 RPC 分支都不调用 restore_stdout；main 注释声称可在同一进程内多次调用（app.rs 第 201 行附近），因此接管标志理论上会保留到下一次调用。已在 main 文档记录该行为，未修改代码。
- time("resolveModelScope") 位于 37/38 步，但 model scope 的实际解析发生在 createRuntime 工厂内（由 create_agent_session_runtime 调用期间完成）；该计时标签是否刻意对齐 TS 无法从本文件确认，未加动机说明。
- run_main 的编号注释缺少第 25 步（time("createSessionManager") 实际在 490 行），编号 5/6 与 37/38 合并；只影响注释编号，不影响行为，未修改。
- 启动早期的 configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS) 丢弃了返回值（let _ =），未在源码/文档中找到解释该选择的依据，因此没有为它编写动机性注释。
- auto_trust_on_reload_cwd 的深层语义跨越本文件与 kiana-interactive（maybe_save_implicit_project_trust_after_reload）；本文件只按字段文档与调用点描述为「候选 cwd」，未断言完整触发条件。
- Node 在 stdin EOF 时「静默退出 0」以及 readline 丢弃同块剩余字节的行为来自既有注释与 spec/ca-cli.md §2.4，本次未在测试中重新实测；相关既有注释原样保留。
- resolve_session_path 本地前缀匹配先于全局精确 id，因而可能优先选中本地前缀会话；这是 spec §2.4.4 规定的 TS 顺序，已按顺序记录，未视为缺陷。

### u109-kiana-coding-agent-large

- 模块 //! 中原有链接 [`drive_until_settled`]、[`startup_process_terminal`]（pub(crate)）与 [`PENDING_COLOR_APPLY`]（私有线程局部）在公开模块文档里可能触发 rustdoc::private_intra_doc_links；任务禁止运行 cargo doc，无法验证。本次新增的链接均指向公开项。
- `load_startup_themes` 的注释称“启动横幅出现在项目信任判定之前”，依据是 app.rs 第 19 步（first-time setup）在 create_runtime 之前；若启动顺序调整需同步注释。
- `set_enable_analytics(true)` 在缺少 tracking id 时生成一个：依据 kiana-config/src/settings/mod.rs 的实现与 tests/first_time_setup.rs；属于跨 crate 观察，若实现变化需更新。
- `placeholder` 被 ExtensionInputComponent 忽略，仅依据 kiana-interactive 的构造参数名 `_placeholder` 与实现；未找到 TS 侧同样忽略的直接证据。
- `project_trusted: Some(false)` 与“没有项目设置可加载”的说法基于 InMemorySettingsStorage 的 project 槽初始为 None 及 from_storage_with_options 的加载逻辑；未逐行追踪 project_trusted=false 的所有分支。
- 按任务约束未运行 cargo check/doc/fmt：仅做了只读的花括号配平、注释外中文扫描与结构比对，无法排除文档链接或格式化层面的告警。

### u110-kiana-coding-agent-large

- ManagedLockGuard::release 直接 remove_dir、不校验锁是否仍属于自己；若锁被外部判 stale 并重建，可能删除新锁。该竞态是按源码推导的，仓库测试未覆盖。
- managed_npm_ci_args 中各 npm 开关（--ignore-scripts/--omit=dev/--include=optional 等）的语义属于 npm CLI 行为，仓库只逐字锁定参数列表，没有断言其含义的测试。
- PackageCommandRuntimeOptions::from_process 的原注释写“应用入口使用”，但 app.rs 直接构造结构体且仓库内无该函数调用方；已按源码改写此句（本文件唯一因原注释与源码矛盾而改写的既有注释）。
- 未运行 cargo check/test/doc（任务禁止）；所有 rustdoc 链接仅做静态核对（目标均为公开项或 std），未用 cargo doc 实测 private_intra_doc_links。
- Windows 专属行为（filetime 刷新目录 mtime、rename 覆盖 current-version）沿用模块原有说明，本次未在 Windows 实测。
- PackageCommandRuntimeOptions 结构“为了可注入/并行测试”的设计动机是从结构与 tests/package_command_paths.rs 的用法推断的，源码中没有直接说明。

### u111-kiana-coding-agent-group

- JsPlainObject::set 的 Value::Null/Number/Bool 分支在当前唯一调用点不可达（migrate_auth_to_auth_json 只传 Value::Object）；并且 prototype=None 同时表示“默认 Object.prototype”与“__proto__ 被赋为 null”。若将来复用该类，get_truthy 在后者仍会把 toString 等 Object.prototype 名字判为 truthy，而 JS 在原型链被置 null 时这些名字应为 falsy。未修改代码，仅记录。
- js_string_entries 对非 BMP 字符（UTF-16 代理对）的两个码元都产出 U+FFFD，而 TS 得到两个独立代理项字符串；已在注释中按代码行为标注为偏差，但仓库现有测试只覆盖 ASCII，没有该场景的锁定。
- migrate_sessions_from_agent_root 只用 trim_js 判断首行是否“纯空白”（U+FEFF 计入 JS 空白），实际 JSON 解析使用的是未去 BOM 的原始首行，因此带 UTF-8 BOM 的 header 会解析失败并被跳过。TS 源在该函数是否 stripBom 未能从本仓库源码确认（spec/ca-cli.md §2.7 未提），故未把 BOM 行为写成断言。
- write_file_syscall 把 StorageFull/WriteZero 归为 write、其余归为 open，沿用原有注释的近似；未与 Node/libuv 在各类错误下的实际 syscall 文案逐项验证。

### u112-kiana-coding-agent-group

- `/proc/self/comm` 的 15 字节截断，以及“写失败只影响 ps 显示、不影响启动”的依据是 cli/setup.rs 既有注释与 Linux 常识；本环境未实测（启动不受影响这一点由 `let _ =` 直接可见）。
- “ExtensionHost::new() 失败 → 扩展加载逐条报 Extension runtime engine is not installed → app.rs 诊断含 Failed to load extension 时打印提示并返回 1”是把 kiana-extensions/src/loader/mod.rs、app.rs、setup.rs 三处串读得出的链条，未跑端到端测试验证。
- “用户在 argv 里再传 --mode 会覆盖前置的 rpc”由 cli/args.rs 的赋值顺序与 app.rs 的 resolve_app_mode 推得；仓库内未找到直接测试该覆盖场景的用例。
- rquickjs 与 AgentSession 的 `!Send`/线程亲和性来自 src/main.rs、lib.rs 的既有文件级文档，未深入阅读 rquickjs 封装实现。
- configure_http_dispatcher 失败后 shared_http_client 的惰性构造与 fallback_client 兜底路径是读 kiana-core/src/http_dispatcher.rs 得出的，未实测。

### u113-kiana-coding-agent-group

- 原注释「供历史调用点保持形状」已改写为「本仓库内没有任何调用方」：依据是全仓 .rs 中 grep restore_sandbox_env 仅命中本文件定义与文档链接，lib.rs 仅有 pub mod bun; 导出、无调用；未见任何测试或二进制入口调用。若存在通过反射/字符串调用的外部使用，需复核。
- 模块开头「Rust 版是原生二进制：进程启动时环境变量天然完整」为原有保真说明，本次保留未改；仓库内 kiana-ai/src/utils/provider_env.rs 与 kiana-resources/src/package/exec.rs 仍保留同一 /proc/self/environ 回退（注释称其为 Bun 单文件二进制在 Linux 沙箱的缺陷），因此该前提在「二进制被以空 envp 启动」时是否成立无法从源码证实。
- 第 5-6 行「在评估读取环境变量的模块之前恢复沙箱环境」照录 TS 源注释（spec/ca-cli.md §2.20 同样如此记载），但按 ESM 语义 bun/cli.ts 的 import 会在 restoreSandboxEnv() 语句之前求值，该时序说法可能有误；本仓库无 TS 源码，无法核实，故未改动。
- TS 构建脚本 scripts/build-binaries.sh、test/restore-sandbox-env.test.ts 与 bun/*.ts 均不在本仓库内，相关描述只能以 spec/ca-cli.md §2.20 与 ARCHITECTURE.md 为依据，未做实测。

### u114-kiana-coding-agent-group

- create_auth_check_model_runtime 的 `$KIANA_CODING_AGENT_DIR/models.json` 说法沿用原注释；实际路径是 getAgentDir()/models.json，本次未追踪 getAgentDir 的全部回退分支（tests/auth_check.rs 只覆盖设置该环境变量的情况）。
- config_selector 的 stdin EOF 路径是否与 Node 退出时的终端恢复字节完全一致，未实测；结论仅依据 startup_ui.rs 的 DriveOutcome 注释与代码路径。
- args.rs 帮助文本 golden 由 TS 参考实现生成的说法来自 tests/args.rs 的注释，未比对 TS 源码。
- auth_check 中 ModelRuntime::get_error() 覆盖 config/composition/availability 三类错误，来自 kiana-config/src/models/runtime.rs 的聚合实现，未逐错误来源验证各分支。
- 受任务约束未运行 cargo doc/fmt/check；所有 rustdoc 链接仅人工核对目标可见性（未链接任何私有或 pub(crate) 项）。

### u115-kiana-coding-agent-group

- session_picker: 通过阅读 kiana-interactive/src/components/session_selector.rs 确认 SessionList 只保存 on_exit、文件内没有任何调用点，默认键位下 Ctrl+C 命中 tui.select.cancel 并走 on_cancel。因此 SelectionOutcome::Exit 在当前选择器路径中通常不可达；这是否是 TS 对齐的有意契约需维护者确认。我只在注释中按事实说明，未改代码。
- project_trust: 模块文档第 17 行既有的 [`LocalBoxFuture`] 链接没有导入到本模块作用域，可能在 cargo doc -D warnings 下产生 broken-intra-doc-link；这是原有注释，未改动也未验证。
- file_processor: std::fs::metadata 失败时 continue 是否与 TS stat 抛错的行为一致，无法从仓库内 spec 摘要确认；注释只描述 Rust 侧可见行为。
- session_picker: 未实际运行手动 TTY 测试；Ctrl+C→cancel 的结论来自 tui.select.cancel 默认键位表与 SessionList::handle_input 源码，未经真机验证。
- credential_print: 未实测 OAuth 刷新/取消返回的具体错误子类；注释只断言所有 AuthError 经 map_err 折叠为固定文案（源码直接可见）。
- list_models: 未运行 CLI 实测表格；输出格式结论来自源码与 tests 中的 format_token_count 断言，排序/宽度行为依据 kiana-tui 的实现文档。

### u116-kiana-coding-agent-group

- set_process_title 的“15 字节上限、失败静默”是原有注释，本次未在仓库测试或内核源码中核实；非 Linux 分支“与 TS 的可观察行为无关”的说法无法从仓库验证（TS 的 process.title 在其它平台可能仍能被 ps 观察到），本次未改写。
- chalk_color_enabled 的既有注释称与 kiana-config 的 resolver “已验证的实现相同”、vendored supports-color 不读 NO_COLOR：我核对了 /home/ctyun/code/kiana_rev/kiana/crates/kiana-config/src/models/resolver.rs 中同名函数代码一致，但未运行其测试，也未查看 node_modules 的 vendored 源码。
- UNIX_ERRNO_PARTS 的“Node 26 实测”文案来自原有注释；仓库内没有可复现该测量的测试（kiana-evals 有一份更全的同类表，同样标注实测）。
- Output::write_stdout 在直写部分字节后才失败时会整段回退重排，可能造成重复前缀；该失败路径由源码推得，未见测试覆盖，本次仅在注释中提示“不补偿已写字节”。
- node_error_parts 在非 Linux 平台的 ErrorKind 回退只按 cfg 与代码判断，未实测。
- 仓库内存在多份 Node fs 错误文案实现（kiana-evals/src/harness.rs、kiana-resources/src/loader/source_info_map.rs、kiana-session/src/agent_session.rs 各有一份本地副本），未来可能漂移；本次未核对全部副本的等价性，也未改动它们。

### u117-kiana-coding-agent-root

- 未运行 `cargo doc -p kiana-coding-agent --no-deps`，新增 //! 中的 intra-doc 链接（main、CliExit、Output、Style、setup_cli、sdk_exports）是否零警告未实测。
- “main 可在同一进程内多次调用”依据的是 app.rs 的注释与每次启动重置 `PROMPT_CONSUMED_STDIN`；未审计其它全局状态（OnceLock/静态缓存）是否全部可重入。
- `AgentSession`/扩展运行时 `!Send`、TUI 类型基于 `Rc`、扩展引擎的 `rquickjs` 线程亲和来自 main.rs 与 cli/startup_ui.rs 的模块注释，本次未做编译验证。
- `ExtensionHost` 缺失时扩展加载报 `Extension runtime engine is not installed` 来自 main.rs 的注释，未在测试中复现。

### u118-kiana-coding-agent-root

- LocalSet::block_on “在调用线程轮询根 future、spawn_local 任务不迁移”来自 tokio 公开 API 语义；本环境无法读取 cargo registry 源码，未在仓库内取证。
- to_string_lossy 把非法字节替换为 U+FFFD 是 std 语义；仓库内只有 cli/setup.rs 对 Node “UTF-8 有损解码”的说明，没有 Rust 侧的实测。
- “默认 panic 以退出码 101 结束”引自 cli/setup.rs 中 FatalStartupError 的文档说明，本次未实际运行二进制验证。
- 未运行 cargo doc，[kiana_coding_agent::main]、[kiana_coding_agent::setup_cli]、[Output::flush] 的可解析性只按 lib.rs 的 pub use 与 main.rs 顶部的 use 静态推断。

### u119-kiana-compaction-large

- 「摘要长度由模型输出上限决定」转述自既有 GenerateSummaryOptions 文档里的 TS #9512 注释，未在本仓库独立验证 provider 行为。
- find_projected_cut_point 的 recovery-omission-suffix 规则（四个条件与前移一位）解释依据本文件代码及其 golden 测试 projected_cut_point_scenarios_match_ts_reference_golden，未逐行对照 TS 源码。
- complete_summarization 文档把 cache_retention = None 解释为「不写 prompt cache」，依据既有注释与字段语义，未验证 provider 侧实际缓存行为。
- 「kiana-session 会把它拼进 Compaction failed: {error}」依据 agent_session.rs 的 AgentSessionError::CompactionFailed Display 与手动压缩 error_message 拼接；未逐一核对自动压缩的所有错误事件分支是否都走该格式。

### u120-kiana-compaction-group

- `FileValue` 文档中关于 serde_json 解析 `"-0"` 为负零的说法来自 `crates/kiana-json/src/lib.rs` 的文档描述（称 `from_str("-0")` 与 `from_str("0")` 在 Value 下不相等），本次未实际运行代码验证；`PartialEq`/`Hash` 已按 SameValueZero 把 -0 与 0 视为同一元素，即使该前提不成立也只是文档表述问题。
- `collect_entries_for_branch_summary` 文档中“target_id 不会出现在返回 entries 里”是读代码得出的推论（共同祖先在 push 之前 break），仓库内没有直接针对该断言的测试。
- `prepare_branch_entries` 文档中“toolResult 的 nestedCalls 文件操作不会进入分支摘要 file_ops”依据的是 `get_message_from_entry` 跳过 toolResult 的控制流，以及 TS spec §2.21.3/§2.21.4 的描述；仓库测试未覆盖该路径。
- 按要求未运行 cargo check/doc/fmt，新增的 intra-doc 链接（crate::compaction::compact、crate::branch_summarization::generate_branch_summary、crate::utils、crate::compaction）仅做了目标可见性的静态核对。

### u121-kiana-compaction-root

- 未运行 cargo check/doc/fmt/test（任务禁止），Rustdoc 链接仅按源码静态核对；`cargo doc -D warnings` 的实际结果待主代理验证。
- `tests/differential_ts_reference.rs` 文件头说明其期望值由仓库外路径（/home/ctyun/kiana-work/scratch/gate-compaction/probe）的 TS probe 生成，本次无法复跑核对。
- 关于 append_compaction 的持久化：仅看到 `persist_last_entry` 受 `persist` 标志与 `session_file` 是否存在控制（session_manager.rs:5049），因此正文写成“写入会话（是否持久化到文件由该管理器的配置决定）”；未继续追踪 `persist` 字段的全部设置路径。
- 原注释“ARCHITECTURE.md §5 的 kiana-compaction 小节”未改动：它对应仓库根 ARCHITECTURE.md §5（每个 crate 的模块树，kiana-compaction 在 1195 行附近），而 docs/ARCHITECTURE_OVERVIEW.md 的 §5 是另一内容；两文件同名可能让读者混淆，仅记录。

### u122-kiana-config-large

- Windows 分支（探针文件刷新 mtime、Win32 错误码表）未在本机运行验证，注释只描述源码路径；按 ARCHITECTURE §8 第 26 条该分支仅保证编译。
- “mtime 精度不足且两次设置落在同一刻度时可能漏检接管”是从“心跳只比较 mtime”的代码推断，未找到对应测试，已在模块文档中使用“可能”措辞。
- lowercase_first 对非 ASCII 首字母（如 'İ'）会按 Unicode 小写扩展 UTF-8 字节长度，此边界未实测。
- AsyncLockGuard::release_blocking 的原有文档提到 notify_waiters/blocking 调用点，但全仓库 grep 未找到调用点；按规则未修改原注释。
- LockError::Io 的 syscall 文档列出 open，本文件从不产生该值（字段公开，可能由调用方构造），未修改该列表。

### u123-kiana-config-large

- `parent_dir("/")` 实际返回 `.`（Rust `Path::parent` 对根返回 `None`），与原文注释“根 → 根”及 JS `path.dirname("/")` 不同；已按代码改正该注释，未改代码，仓库内也没有根路径输入的测试。
- `StorageDocument::parse` 不重排对象键序：`entries()`/`list_credentials` 对整数样式的顶层 provider id 可能按 JSON 文本顺序而非 JS 升序枚举；写盘时 `kiana_json` 会重新排序，字节仍匹配。该输入无测试覆盖。
- `read_syscall` 只在 Unix 上把 EISDIR 归为 `read`；其它读取阶段错误会按 `open 'path'` 报告，可能与 Node 的 syscall 字段不同（未实测）。
- `write_current` 没有 fsync、临时文件或 rename；`with_lock_async` 的最后一次锁破坏检查在写盘之后，若此时才发现破坏会返回 ECOMPROMISED 但文件已写入。该窗口无测试覆盖。
- Windows 分支（WINDOWS_ERROR_PARTS、PermissionDenied→EPERM、read_syscall 的非 Unix 路径）在当前 Linux 环境无法执行验证。
- 原文模块文档中指向私有 `credential_from_value` 的 rustdoc 链接已改为行内代码（文字未变），以便满足 private_intra_doc_links；这属于对既有注释的修改，特此说明。
- `StorageDocument::set_property` 对字符串 `length` 会走“Cannot create property…”分支；V8 对只读的 `length` 可能给出“Cannot assign to read only property…”（未在仓库内验证，代码未改，文档只描述代码行为）。
- 公开项统计口径：45 个 `pub` 声明（含 2 个公开字段、2 个 `#[doc(hidden)]` 测试钩子；不含枚举变体与 trait impl 方法）。

### u124-kiana-config-large

- CacheWarmingDecideFn 文档中「decide panic 会让 run 停在 refreshing」是读码推导：refresh_run 在调用 decide 前已把 run.timer 置 None，而默认 tokio 定时器把回调放在 tokio::spawn 任务里，panic 会终止该任务；没有测试覆盖，未实测。
- CacheWarmerTimerHandle::clear 文档中「重复调用应当安全」只在默认实现（CancellationToken::cancel 幂等）与测试用的 ManualTimer 上得到印证，trait 的其他实现未验证。
- TokioCacheWarmerTimer::now() 在系统时钟早于 UNIX_EPOCH 时返回 0、进而使所有基于它的截止判断一起偏移——这是读 map_or(0, …) 的推断，未构造异常时钟实测。
- CacheWarmerModels::stream_simple 文档中「同步阻塞到响应结束会占住轮询任务」是一般 async 事实，本仓库未做实测或基准。
- schedule 的句柄安装存在一个理论窗口：set_timeout 返回后、第二次加锁写回 run.timer 之前，若定时器回调已经开始（延迟为 0 时可能），refresh_run 会先把 timer 置 None，随后 schedule 又写回 Some(handle)，使 status() 短暂报 scheduled；该 handle 的回调已消费，不会重复刷新。未在测试中复现（ManualTimer 只在 advance 时执行回调），仅记录，未改动代码。
- reason 字符串中 request disabled prompt caching、one-hour safety limit reached、30-minute idle safety limit reached、stopped by extension、expected savings below threshold、inactive 未被本 crate 测试直接断言（spec §2.28.8 有逐字表）；文档中关于文案冻结的表述据此写成「多数已被断言」。

### u125-kiana-config-large

- 预存在（非本次新增）的模块文档链接 [`OBJECT_PROTOTYPE_KEY_TARGETS`]（mod.rs 第 32 行）指向私有常量（第 613 行）。公开模块的 //! 链接私有项可能在 RUSTDOCFLAGS='-D warnings' cargo doc 时触发 rustdoc::private_intra_doc_links；按「保留已有注释」规则未改写，交由文档检查阶段判定。
- 本次未运行 cargo check/doc/fmt（任务禁止）；所有 Rustdoc 链接仅通过阅读定义确认目标为公开项（KeybindingsManager、keybindings、KeybindingsConfig、migrate_keybindings_config 等），未实际验证 rustdoc 解析结果。
- platform_for_os 的 `_ => "linux"` 回退与既有注释「保留完整映射以免非 Linux 的 Unix 误用 Windows 键位」只在已列出的 OS 上成立；未改原注释，仅补一行说明未列出 OS 会被判成 linux、进而可能命中 WSL 分支。
- `load_raw_config` 注释中「serde_json 启用 preserve_order」依据仓库根 Cargo.toml 第 82 行的 workspace 依赖特性（preserve_order/raw_value）与 kiana-config 使用 workspace serde_json；`kiana_json::js_parse` 会做与 JS 相同的键重排这一点依据 tests/keybindings_migration.rs 中 parse_config 的注释与用例，未单独执行验证。
- 保留的原注释中关于 Node/TS 具体行为（process.platform 的编译期宏来源、String(fn) 生成 [native code]、V8 Object.getOwnPropertyNames 顺序）无法在本仓库内独立复现，按原样保留并在其基础上补充说明。

### u126-kiana-config-large

- V8 Date.parse 的逐条行为（尤其 legacy 分支、BST 等未知时区缩写）未在本环境用 node 实测；注释依据是文件内既有测试注释、测试期望值以及 spec/ca-config-models.md §2.24.2/§A.1.5。
- chrono 的 LocalResult::None（DST 空档）与 Ambiguous（取较早偏移）语义来自 chrono 文档，仓库内没有 DST 边界测试验证这两条注释。
- Url::set_query 不会把已编码的 %2C 二次转义，仅由单元测试 builds_the_catalog_url 的期望字符串间接支持（本任务未运行测试）。
- “对象值与原始条目按插入顺序遍历”依赖 workspace Cargo.toml 为 serde_json 启用的 preserve_order feature；若该 feature 变更，parse_catalog 文档中的顺序断言需同步修改。
- “fetch_with_retry 在两次尝试之间不等待”按当前 management_http.rs 的重试循环（无 sleep）判断；若上游加入退避，CATALOG_ATTEMPT_TIMEOUT_MS 与 request_catalog 中“3 次各 4 000 ms”的耗时描述需更新。

### u127-kiana-config-large

- apply_models_json 的 existing_index 对 AnyModel::Remote 只按 id 匹配、不检查 model_type（代码如此），而 spec/ca-config-models.md §2.19.6 描述 TS 只替换 chat 模型；若远端目录存在同 id 的 image/classifier，会被替换成 chat。已在注释中按代码事实描述，未修改代码，是否有意待确认。
- refresh_models 的取消检查（context.signal.is_aborted()）位于 base 刷新与扩展 refreshModels 之后，取消时 base 可能已完成自己的刷新与发布；是否为有意的取消语义待核实（本文件只按代码顺序记录）。
- configured_request_auth_status 与 resolve_compatibility_request_config 解析环境变量时使用 None（只读进程环境），签名没有注入 env 的入口；这是按代码行为记录，是否符合调用方预期待核实。
- compose_api_key_auth 在配置的 apiKey 为空串时仍把 provider 视为“已配置”（check 返回 configured API key；configured_request_auth_status 走 ModelsJsonKey/Fallback 分支）；空串是否应视为未配置，文档未下结论，待产品/TS 语义确认。
- ExtensionOAuthConfig::login 适配层不转发 LoginOptions（设备 ID、agent 名，参数为 _options）；若扩展 OAuth 流程依赖这些信息，可能是兼容性缺口，待核实。
- has_generate_images/has_classify 以 images/classifiers map 非空判断能力，images_defined=true 但显式空表不贡献能力；与重注册合并语义的关系未在测试中直接覆盖，待核实。
- 未运行 cargo doc（任务禁止写文件/构建），所有 intra-doc 链接按作用域人工核对为 public 目标或已导入的 public 外部类型，但未通过 rustdoc::broken_intra_doc_links 实测。

### u128-kiana-config-large

- input_limits_from_value 中 jpegQuality 的 `as u8` 截断（u64→u8 取模 256）是 Rust 语言语义确定的行为；但「catalog_http 宽松路径会实际传入 >255 的值」是从 parse_catalog_entry 的 serde 失败回退路径推断的可达性，未构造实际用例验证。
- ModelsJsonSchemas 与 TS generate-references.ts 生成的 schema 是否逐字节一致，仓库内没有对应的黄金测试（tests/config_schemas.rs 未覆盖 models schema）；注释只写了形状与来源，没有断言字节级相等。
- node_read_error_message 的 errno 表只按源码字面记录；非 Linux 平台上 raw_os_error 的取值是否命中这些 libc 常量未验证，文档沿用原有的「Linux 上」措辞，未写平台结论。
- composer 的 modelOverrides 最后阶段对任意 AnyModel::Remote 都应用覆盖（未按 model_type 过滤），与其自身注释「仅 chat」存在张力；本文件的 ModelsJsonModelOverride 文档回避了「仅 chat」断言，未改代码，仅记录此差异供审阅。
- 「models.json 顶层未知键被忽略」由 serde 未使用 deny_unknown_fields 与对象校验器不拒绝额外键推断；tests/model_config.rs 只显式覆盖了 provider/model 级未知字段，没有顶层未知键用例。
- JsonHeaders 的解析时机写作「解析认证或请求配置时」，依据 compose_api_key_auth 的 check/resolve 与 resolve_compatibility_request_config 等可见调用点；未穷举容器内全部调用路径。

### u129-kiana-config-large

- 新增的 chalk_color_enabled 判定顺序是按本函数代码归纳的，未与 node_modules 中 chalk/supports-color 的对应版本逐行对照（仓库内没有该依赖源码）。
- expand_sequence 中“码点为 92（反斜杠）的条目被清空”的来源未在源码或 spec 中说明，注释中没有推断其动机；若需解释需对照 brace-expansion 源码。
- parse_segment 注释中“plain 必须在大小写折叠之前计算”的依据是同函数内的实际执行顺序；spec §2.21 未展开 minimatch 内部，未与 TS 参考实现逐行核对。
- resolve_model_scope_from_models 中“glob 跨 provider 命中不产生 ambiguous 诊断”“同一模型先到者不更新 thinking 等级”来自分支结构阅读，未逐条对照 scope_cases.json 全量夹具。
- 既有公开文档链接 [`minimatch`]（模块 //!）与 [`chalk_color_enabled`]（doc(hidden) 函数）指向私有项，可能触发 rustdoc::private_intra_doc_links；按规则未改动也未验证。
- trait 文档中“has_configured_auth 不发起网络请求”只对 ModelRuntime 的实现成立（读取内存快照），其他实现者不保证；文中已用“以 ModelRuntime 的实现为例”限定。
- 按任务约束未运行 cargo check/doc/fmt，因此新增 /// 是否触发 rustfmt 重排、rustdoc 是否新增警告均未实际验证；新增链接只指向公开项（ModelReferenceTarget、DEFAULT_MODEL_PER_PROVIDER、resolve_cli_model、ModelRuntime、resolve_model_scope_writing_to）。

### u130-kiana-config-large

- 未运行 cargo check/doc/fmt（任务禁止），Rustdoc 链接仅按作用域人工核对，未实测是否新增警告。
- get_auth_for_model 返回的 AuthResult.env 是底层解析结果、不含 overrides.env（overrides.env 只参与配置头解析）；这是源码事实，但是否符合 TS getAuth 语义没有测试断言，待核实。
- 全量可用性刷新 run_availability_refresh 直接采用底层 get_available(None) 的顺序，本文件未显式按 all 重排；注释只对局部刷新断言 available 会按 all 排序。
- register_native_provider 移除同 id 扩展注册配置的动机未见 TS/spec 说明，注释只描述可观察行为。
- 模块原有断言“三个序号都是 AtomicU64”与代码不符：provider_availability_seq 实际是 Mutex<IndexMap<String,u64>>；已依据字段声明及其在 queue/refresh 中的使用改写该句。

### u131-kiana-config-large

- 超时后 `kill` 只调用一次；若子进程不响应 kill 且 stdout 始终不关闭，`spawn_sync_command` 的主循环没有额外退出条件，会持续轮询。此结论来自控制流阅读，未做进程级实测。
- Rust `Child::kill()` 在 Unix 上发送 SIGKILL，而既有注释按 Node 描述“超时后先发 killSignal（默认 SIGTERM）”；对捕获 SIGTERM 做清理的命令，行为可能与 TS 不同，未实测。
- 既有注释称“ENOBUFS/超时与 Node 一致：status 为 null”，代码只在 overflow 时强制 `status: None`；超时分支保留 `ExitStatus::code()` 的结果（Unix 上被信号杀死时通常为 None），Windows 行为未验证。
- `std::env::var` 遇到非 Unicode 环境变量会返回 Err，本模块将其视为缺失；该 std 行为未在本仓库测试中覆盖。
- `try_wait` 报错提前跳出循环时仍会 `writer.join()` 等待 stdin 写线程；若此时子进程仍存活且不读 stdin，理论上可能阻塞，该路径未测试。

### u132-kiana-config-large

- 未运行 cargo doc/check/fmt/clippy（任务禁止），新增 intra-doc 链接均人工核对为本模块公开项，但未经过 rustdoc 实测。
- 『kiana-session / kiana-interactive / kiana-coding-agent 等 crate 持有 SettingsManager』来自 grep 到的字段/参数用法（agent_session.rs、interactive_mode.rs、package_manager_cli.rs 等），未验证运行期所有权与生命周期。
- 『settings.json 键序与无结尾换行由逐字节黄金用例锁定』依据 tests/settings_manager_bug.rs 的测试头与 persist_golden_fixture_matches_typescript_bytes，未实际运行该测试。
- 『嵌套 setter 内存与磁盘均不变』在单线程调用语义下由 mutate_global_nested_and_save 在 apply 前返回保证；并发场景下其他线程可能同时写盘，未做额外断言。
- 原注释『每个 settings_object! 结构体实现一次』与代码不符（schema.rs 的 17 个结构中本文件只有 12 个 impl_settings_merge!），已按实际实现改写，依据为两处的声明与调用点。

### u133-kiana-config-large

- record_pattern_matches 的 TypeBox 1.3.27 `patternProperties: {"^.*$"}` 与 ECMA-262 行终止符行为沿用原有注释和 tests/config_schemas.rs 的断言；本次未运行 TS 侧 Compile(...).Check 实测。
- 任务禁止构建，未运行 cargo doc/cargo check；新增 intra-doc 链接（Settings::raw、CompactionSettings::raw_token_setting、crate::settings::deep_merge_settings 等）按源码逐条核对为可见项，但无法实测 rustdoc 警告。
- `Settings::raw` 对 f64 NaN/±Infinity 的 `serde_json::to_value` 行为（serde_json 1.0.154 源码中会变成 Value::Null 而非 Err）来自依赖源码阅读，仓库测试未覆盖；注释未展开该细节，compaction getter 先检查类型化字段所以未观察到影响。
- kiana-resources 缺失 source 时的错误文案取自 crates/kiana-resources/src/package/parse.rs 的 get_package_source_string 及其文档注释，未运行该 crate 的测试。

### u134-kiana-config-group

- index.rs 原有注释称 glob 引入的同名函数会与模块名在 value 命名空间冲突，未能验证：按 Rust 命名空间规则模块属 type 命名空间、函数属 value 命名空间，二者看起来可以共存；未编译验证，也未改动原注释，只在下方补了一句“模块路径仍由 lib.rs 提供”。
- defaults.rs 的 RADIUS_PROVIDER_ID 与 kiana-ai/src/providers/radius.rs 中 radius_provider 的缺省字面量 "radius" 相同，但 kiana-ai 并不引用本常量（跨 crate 复制字面量），是否为刻意设计未在文档中找到依据。
- client_identity.rs 模块文档中“三层身份合并”的细节引自 kiana-ai/src/utils/identity.rs 的模块文档与 merge_client_identity 注释，未逐条复测实际请求路径。
- auth_guidance.rs 中“非交互模式缺少模型时写到 stderr 并返回退出码 1”来自 kiana-coding-agent/src/app.rs:611-612 的源码阅读，未运行程序验证退出码。
- 多处提到 std::env::var 在值非 UTF-8 时返回 Err 并按未设置处理：这是标准库语义，仓库内没有对应测试用例覆盖该情形。
- 未运行 cargo check / cargo doc（任务禁止），新增的 intra-doc 链接只做了人工解析检查，未实测 rustdoc 警告。

### u135-kiana-config-group

- read_latest 与 ensure_reload 之间存在读者计数竞态窗口：ensure_reload 返回后、readers.fetch_add 之前，最后一名旧读者可能清空槽位并 abort 该重载；此时新读者加入同一重载会收到中止错误，即使它自己的 signal 未中止。仅记录，未改代码，测试未覆盖此窗口。
- RuntimeCredentials::list 合并覆盖项时不过滤空串，而 read 过滤空串：set_runtime_api_key(id, "") 会导致 list 与 has_runtime_api_key 报告已覆盖，但 read 回落持久层。仓库内没有 TS 源可对照，无法确认这是否与原实现一致。
- RuntimeCredentials 的 Debug 会打印覆盖层内容（明文 API key）。已在 Debug 实现上方写成警告注释，但未修改代码。
- entry_to_value 只清理 lastModified/checkedAt/etag 三个 extra 同名字段，不清理 extra 中的 "models"：手工构造的 ModelsStoreEntry 若把 models 放进 extra，序列化会写出重复键（kiana-ai 的 ModelsStoreEntry::extra 文档已警告；本文件的读取路径不会产生这种条目）。
- 未运行 cargo doc / cargo fmt --check / cargo check（任务禁止）：新增的 intra-doc 链接都指向公开项，但 rustdoc 与 rustfmt 结果未实测。
- lock.rs 自述 Windows 分支“只保证编译”；本单元注释未展开任何平台差异，Windows 上 auth/models 的加锁与路径行为未在本次核实。
- models_store 模块文档中“读取一个不存在的 store 会留下空文件”来自 FileAuthStorageBackend::with_lock_async 的 ensure_parent_dir + ensure_file_exists 源码，本文件的测试未直接覆盖该场景。

### u136-kiana-config-group

- 未运行 cargo doc（任务禁止），[`warmer`] 这条 intra-doc link 的解析未实测；它是本文件的公开子模块，按规则应可解析。
- public_total 记为 1（pub mod warmer），public_documented_after 记为 0：按规范禁止在父模块的 pub mod 声明上方新增外层 ///，child 模块文档由 warmer.rs 内的 //! 提供；doc_inventory.py 是否把模块声明计入公开项未能确认。
- “cache-stats 属于会话数据层而拆到 kiana-session-core”依据 ARCHITECTURE.md §3.7 表格与 cache_stats.rs 自身的 SessionEntry 导入，未逐条复核该层所有使用方。

### u137-kiana-config-group

- `description` 缺失分支：Rust 侧 `unwrap_or_default()` 会写出 `"description": ""`；TS/TypeBox 在 `description: undefined` 时是否在 JSON 中省略该键，未在仓库内找到 fixture 或测试证据（当前 90 个定义均为 `Some`，分支不可达）。
- `tests/fixtures/keybindings_schema.json` 的来源仅依据测试文件头注释（TS 参考实现 `JSON.stringify(KeybindingsSchema, null, 2)`、未经 `addDefinitions`），仓库内没有生成脚本副本可核对。
- “schema 模块当前只被测试调用”与“keybindings_schema_static 无调用点”来自全仓 `*.rs` 的 grep；宏展开、构建脚本或非 Rust 工具中的间接引用无法由此排除（可能性低）。
- “输出与平台无关”基于当前 `build_keybindings`/`app_keybindings_for` 只在平台分支修改 `default_keys`；若未来默认表按平台增删 id 或描述，该结论失效。

### u138-kiana-config-group

- public_total 的计数口径：本次按“rustdoc 可见符号”统计（含 enum 变体与结构体字段、含再导出入口；virtual_models 的 20 是 re-export 名单）。若仓库 tools/doc_inventory.py 的口径不同，数字需要重算。
- REMOTE_CATALOG_MODEL_TYPES 与 catalog_http::CATALOG_MODEL_TYPES 必须同步，这是从 request_catalog 的源码读出的维护约束；没有测试直接断言两个常量相等（测试各自断言值），因此改动其中一处不会被现有测试发现。
- get_provider_display_name 文档中“自定义 provider 未给 name 时 name() 也等于 id”依据的是 CreateModelRuntimeOptions/CreateProviderOptions 的文档（provider.rs: "展示名；缺省等于 id"），不是本文件的代码。
- remote_catalog 中“304 且 context.stored 为 None 会落入非 2xx 错误分支”是对代码分支的推断（304/404/501 分支都要求 stored 为 Some），该组合在正常协议下不应出现，未用测试验证。
- 未运行 cargo build/test/fmt/clippy/doc（任务明确禁止），Rustdoc 链接仅人工核对：均为同 crate 公开项或同模块内路径，未链接 pub(crate)/私有项。
- registry.rs 的 get_api_key_and_headers 文档称“每次调用都会重新解析 auth”，依据是 tests/model_registry.rs 的两个测试（every_request 与 exactly_once）；只覆盖了配置命令场景，其它凭证来源的缓存行为未逐一验证。

### u139-kiana-config-group

- write_utf8 的「File::create 先截断、write_all 失败可能留下空文件或半截内容」是按代码结构推断，未做故障注入实测；storage.rs 的 /dev/full 测试只验证 ENOSPC 错误文案。
- FileSettingsStorage 中 exists() 与后续加锁/读取之间的竞态窗口描述来自代码结构（检查时刻决定分支），未见并发测试覆盖。
- migrate_retry_max_delay 在 provider 为标量或 null 且需要迁移时会整体替换 provider；该分支可由代码读出，但 tests/settings_migrations.rs 未覆盖。
- resolve_from_cwd 在 current_dir() 失败时退回 "." 的行为本仓库无测试；Node 侧会抛错的对照依据是 auth/lock.rs 的模块文档，而非直接实测。
- public_total 为人工按 pub 项（含 pub 字段）计数；仓库内没有 tools/doc_inventory.py，无法用工具核对统计口径。

### u140-kiana-config-group

- manager.rs 既有注释称 dirname "空 → ."，但按 Rust std 语义 Path::new("").parent() 返回 None，函数会落到 None 分支返回空串；未运行验证，且现有调用方不会传空路径，因此只记录、未改写原注释。
- normalize_cwd 文档中“realpath 失败时回退为 resolvePath 词法结果”来自对 kiana-core/src/paths.rs（canonicalize_path，约 794–799 行）的源码阅读，未运行验证。
- 按任务要求未运行 cargo check/doc/fmt，因此 rustdoc 链接解析与 rustfmt 稳定性仅做人工检查（新增内容全部是 //、///、//! 与空行，未触碰属性与代码）。
- “Lock file is already being held”文案取自 auth/lock.rs 的 LockError Display 实现，未在 trust store 竞争场景下实测。
- 未覆盖 Windows 下 Path::parent 对盘符根、HOME/dirs::home_dir 差异的行为；注释只描述当前源码可见逻辑，未做平台结论。

### u141-kiana-config-root

- 关于 settings 写盘字节格式的证据来自静态阅读 tests/settings_manager.rs:1741 的原始文本断言，未运行 cargo test 验证。
- auth::lock 要求与 Node 版工具互操作，依据的是该模块 //! 的说明，本次未做 Node 侧联调。
- 顶层扁平符号（如 kiana_config::DEFAULT_THINKING_LEVEL）依据 index.rs 当前的 glob 再导出推断，未编译验证。
- CacheWarmer 由 kiana-session::sdk 在请求路径调用 start，依据 sdk.rs 的装配代码与 cache/mod.rs 注释，未运行确认触发条件。
- 编辑期间 index.rs 被其它单元并发更新（mtime 变为 19:11）；文中『只有四个叶子模块提供扁平符号』以当前版本为准，若再导出调整需同步。
- bootstrap SettingsManager 的早期用途在 app.rs 中只确认了 HTTP 代理；docs/EXECUTION_FLOW.md 还提到代理/主题/会话目录，文中用『等』概括，未逐处核对。

### u142-kiana-core-large

- `posix_normalize` 对 Node `path.posix.normalize` 的细节（相对路径保留多余的 `..`、保留尾随 `/`）仅由链接改写测试间接覆盖（如 `[x](../../../README.md)` 保持原样），未直接与 Node 对拍。
- `is_directory_target` 对无扩展名文件（如 `LICENSE`、`Makefile`）会生成 `/tree/` 链接的误判，是从代码推导的结论，仓库内没有对应测试。
- 未运行 `cargo check` / `cargo doc` / `cargo fmt --check`（任务禁止），因此新增的 intra-doc 链接（含 `crate::management_http::fetch_with_retry`）未经 rustdoc 编译验证，也无法确认补注释后 rustfmt 的布局。
- `warn_deprecation` 的并发结论（同一消息仅一个线程打印、不同消息顺序不保证）由锁作用域推导，现有测试只覆盖单线程去重，没有并发用例。
- 模块文档中 “kiana-interactive 在启动时以 fire-and-forget 方式调用版本检查” 依据 `interactive_mode.rs` 的 `spawn_local` 调用点，未进一步验证其调度与取消语义。
- `public_total` 按本文件的类型、公开字段、枚举变体、公开函数与 `pub use` 再导出人工计数为 34；仓库中不存在 `tools/doc_inventory.py`，无法用工具复核口径。

### u143-kiana-core-large

- Windows 分支（cross-spawn 复刻、PATHEXT/PATH/ComSpec 大小写回退、shebang 探测、cmd.exe 双转义）未在 Windows 目标上编译或运行，注释依据仅为该文件源码与既有交叉引用注释，未实测。
- 「同一轮 try_wait 观察到的退出即使实际发生在 deadline 之后，也按超时前退出保留退出码」由 spawn_process_sync_bytes 的语句顺序推得；现有测试只覆盖已退出/晚退出两种粗粒度情形，未覆盖该精确竞争窗口。
- PipePump::new 中「fcntl 失败时管道保持阻塞、drain 的 read 可能阻塞调用线程」由 F_GETFL/F_SETFL 分支推得，未构造 fcntl 失败场景验证。
- node_spawn_error 的新错误不保留 raw_os_error，依据的是 std::io::Error::new 的语义，未运行验证；下游 js_host 的 errno_info 会退回到 ErrorKind 映射。
- 「Unix 上 PATH 查找由 exec 完成」「管道未排空会让子进程阻塞」属于通用操作系统/管道语义，仓库内没有直接断言它们的测试。
- kiana-tools/src/bash.rs 的「同状态机、额外返回信号编号」依据该文件模块注释与 ChildExit 返回类型判断，未逐行比对两份状态机实现。

### u144-kiana-core-large

- 既有文档把 `_with` 变体标为「测试用」，但 kiana-interactive/src/interactive_mode/input.rs 的生产路径也使用 ClipboardTestIo::from_process() 与 read_clipboard_text_with/read_clipboard_file_paths_with；按要求未改写原措辞，只在模块 //! 注意事项中按源码事实说明这些注入项同时服务于该调用方。
- Windows 专属分支（windows_tmpdir、CREATE_NO_WINDOW、WSL 的 wslpath/PowerShell 流程）无法在本机（Linux）实测，注释依据是源码与 spec/ca-cli.md §2.21.28/§2.21.29；未在 Windows 上验证。
- write_private_file 的 mode(0o600) 仅在创建新文件时生效属于 std 语义，仓库内无测试验证，因此注释只写「已存在会被 truncate 整体替换」与「调用方传带 UUID 的新路径」，未断言权限位行为。
- launch_detached 注释中的「父进程退出不会波及」依据 process_group(0) 的既有注释与 TS detached 语义，未做进程实验；Ctrl+Z 的具体交互只保留在 run_clipboard_command 的既有 #11179 说明中，没有扩展到 open_browser。
- 未运行 cargo doc/fmt（任务禁止运行 cargo）；新增 intra-doc 链接只指向该文件内的 pub 项（read_clipboard_text、copy_to_clipboard、open_browser、ClipboardUnavailableError、read_clipboard_file_paths），私有/pub(crate) 目标一律写成行内代码。

### u145-kiana-core-large

- 原有模块文档与 get_quickjs_wasm_path 文档称“由 kiana-codemode 调 set_embedded_quickjs_wasm_path”，但全仓库 grep 只找到 crates/kiana-core/tests/config.rs 的调用；已按现状改写该句，但若存在本仓库之外的 Bun 入口代码需复核。
- Windows 分支（path_is_writable、路径比较转小写）未在本机实测，结论来自源码与既有模块文档；Windows 用例在现有测试中被 cfg 排除。
- get_codemode_worker_specifier 的 BundledNode data: URL 分支因 IS_BUNDLED_NODE 恒 false 而不可达，未实测。
- get_api_base_url 对 "/" 返回 Some("")、get_share_viewer_url 对 KIANA_SHARE_VIEWER_URL 不做 trim，均由源码直接推出，但现有测试未覆盖这些边界。
- 模块文档既有的“CI 校验 VERSION 与 package.json”说法未在本单元核实（未读取 CI 配置）。

### u146-kiana-core-large

- 模块文档沿用“281 条黄金语料锁定错误消息”的说法，但 golden_tests::matches_yaml_package_on_corpus 对错误用例只比较 message().lines().next()（首行）；完整消息由 tests/frontmatter.rs 中的若干 assert_eq 覆盖，本次未逐条核对 281 条的全部消息。
- 调用方行为描述依据当前源码：kiana-resources 的 skills/discover.rs 与 prompts/mod.rs、kiana-session 的 agent_session.rs、kiana-extensions 的 virtual_modules.rs、kiana-coding-agent 的 sdk_exports.rs；若这些调用点后续变化，模块文档“在系统中的位置”需同步。
- “错误消息逐字复刻 yaml 的 prettifyError”为仓库既有结论，本次未在真实 yaml 2.9.0 运行时重新验证，只能以 tests/frontmatter.rs 的精确断言与 tests/fixtures/yaml_corpus.json 为据。
- Parser 的 chars 下标与 line/column（UTF-16）的对应关系只从 new/advance/restore 与单测读到；未穷举验证所有分支的列号正确性。

### u147-kiana-core-large

- WebP `find_webp_tiff_offset` 中 `chunk_size` 来自 int32 语义，负值会让 offset 先向负方向移动（随后 `byte_at` 补 0 再回升）；公式与 TS 完全一致，但损坏 WebP 的实际路径没有测试覆盖，未实测。
- JPEG marker 扫描没有为 SOS/EOI 单独分支，完全按段长推进；仓库测试只覆盖「XMP APP1 在 Exif APP1 之前」与缺失/非法 orientation，畸形 JPEG 的分支未逐例验证。
- `install_quiet_panic_hook` 只在首次调用时包装当时的 panic hook；若其它组件之后调用 `std::panic::set_hook` 会替换这层包装（std 语义）。仓库内未见此类调用，未实测。
- `get_exif_orientation` 不读取 PNG 的 `eXIf` chunk（代码只处理 JPEG/WebP）；上游是否依赖 PNG 方向未核实，注释未声称支持。

### u148-kiana-core-large

- “校验顺序与错误文案同 TS 逐字一致”沿用了文件原有断言；tests/fixtures/mcp_validation.json 的用例每个只断言单个错误，未覆盖多字段同时非法时的报错优先级。
- is_loopback_host 文档中“`[0:0:0:0:0:0:0:1]` 经 url 解析归一为 `::1`”由测试 is_loopback_redirect_uri_matches_the_whatwg_rules 的真值间接支持，未单独验证 url crate 的解析实现细节。
- 将 serialize_js_number_opt 原注释示例 `1e+20` 改为 `1e+21`：依据是模块文档“1e20 写成 100000000000000000000”与测试 timeout_serializes_with_js_number_formatting（1e21 -> "1e+21"）；按任务约束未运行测试复核。
- McpClientRegistration::Cimd 的 `KIANA_API_BASE_URL/oauth` 说明是原有注释，本单元未在仓库中找到对应实现证据。
- 模块文档原有第 3 条（JS 原型链差异）与第 3 行“telemetry-mcp 单元”归属未在本单元复核，仅原样保留。
- 注册表回调“并发时看到的状态可能包含其它线程更新、回调顺序不能当变更日志”由代码结构（锁外同步回调）推演，仓库中没有并发测试覆盖。
- RegisteredMcpServer::Clone 为深拷贝的结论来自 Rust 类型语义（String/IndexMap/Value 均拥有数据），注册表 list/get 的深拷贝有测试，但针对 RegisteredMcpServer 本身没有专门断言。

### u149-kiana-core-large

- `contains_encoded_backslash` 调 `contains_encoded_byte(url, b'c')`，而后者把第一个十六进制位硬编码为 `b'2'`：实际只能匹配 `%2c`（逗号），永远匹配不到反斜杠的 `%5C`。这是 Windows 分支 `file_url_to_path_windows` 的疑似缺陷；Windows 路径不在现有测试矩阵内，我未修改代码，只按代码实际形状写了 `contains_encoded_byte` 的注释并在本条记录。
- `MAX_REALPATH_LINK_EXPANSIONS = 40`，但 `node_realpath` 的 `for _ in 0..=MAX` 允许 MAX+1=41 轮展开；常量名/注释说的是“上限 40”，是否与 Node 的 realpathSync 计数一致无法在本仓库验证，已在 `node_realpath` 文档中如实写成“`0..=MAX` 允许 MAX+1 轮”。
- 模块文档原有断言“实现用 2000+ 条 Node 26 生成的输入对拍过”以及多处“Node 会抛 ELOOP/ERR_INVALID_FILE_URL_*”属于已有保真说明，仓库内没有 Node 源码可核对，我保留未改。
- `file_url_to_path_windows` 依赖 `url::Url` 与 Windows 的 `to_file_path`，现有测试全部跑在 Linux 的 POSIX 分支上；其实际 Windows 行为（含上面 `%5C` 问题）无法在本机验证。
- 模块文档新增的“`win32_normalize` 把 `\\?\` / `\\.\` 设备前缀按 UNC 形态解析”来自代码推导，测试只锁定了 `\\?\C:\x` 原样往返；`\\.\C:\x` 之类未实测。

### u150-kiana-core-large

- 第 72 行（原文件已有）的模块文档链接 [`ClassBodyDef`] 指向私有 struct；public 模块文档链接私有项可能触发 rustdoc::private_intra_doc_links。按要求未改动，建议主代理运行 cargo doc -D warnings 确认，必要时把它改成行内代码。
- 第 22-24 行（原文件已有）称“扫描器的每条规则都以 \A(?:...) 锚定……整体保持线性”。但 parse_function_decl、try_perl_regexp、js_object_attr_matches 等手写解析在失败路径上会向前/向后多扫一段，病态输入（如长篇无括号的标识符序列）可能不是严格线性。未实测，未改动。
- 第 6291 行 KW_LUA 的 built_in 词表里出现 `ldexp kiana cos`：按 Lua 数学库与 highlight.js 10.7.3 的对应序列推断应为 `fmod`，疑似项目改名脚本误替换；这会让 lua 的 `fmod` 失去 built_in 着色、`kiana` 被误着色。未修改，建议与 TS 参考实现核对。
- 模块文档中“其余 167 种未实现语言”的计数未独立复核：ALL_LANGUAGE_NAMES 确认为 364 项（awk 计数），但未逐项核对已实现 grammar/别名的划分。
- KW_DART 里的 `inferface` 拼写看起来像 typo，但记得 highlight.js 10.7.3 上游本身就有该拼写，未改动也无法在仓库内用 TS 源码复核。

### u151-kiana-core-large

- 既有注释「丢弃 body 以复用连接」（get_latest_version_with_base）未实测：reqwest/hyper 对未读 body 的 Response 做 drop 是否复用连接取决于连接池实现，按规则保留原文未改。
- std::fs::rename 覆盖已存在目标的行为依赖平台/标准库语义；本 crate 的集成测试只覆盖首次安装，未覆盖「重复下载覆盖已有二进制」场景。
- Windows 专属分支（%SystemRoot%\System32\tar.exe、powershell.exe Expand-Archive）未在 Windows 上实际运行，注释依据源码与 spec/ca-tools-compaction-export.md §2.28.9。
- 部分设计动机（GitHub release 页面无 API 配额、Git Bash 的 GNU tar 不支持 zip）来自 TS 源码注释与既有 Rust 注释，而非本仓库实测。
- host_platform 依赖 std::env::consts::OS 在 Android 上等于 "android"；本 crate 测试是直接注入 "android" 字符串，未覆盖真实 Android 构建。

### u152-kiana-core-group

- ansi.rs：utf16_byte_index 返回 None 的 fallback 看起来不可达——find_unfinished_ansi_at_end 的匹配起点必然是 ASCII 的 ESC(0x1B) 或 U+009B，其 code-unit 下标在完整字符串里总是边界。若分支确实不可达，其存在只是防御；请作者确认是否应该删除或补测试。
- ansi.rs：原模块文档与 fallback 行内注释称代理对会留在 pending，但代码 split_at += 1 后 complete = value[..byte_index]，实际把整个代理对放进 complete。已按代码改写这两处注释；若原始设计意图是留 pending，则实现应改为 -= 1，请对照 TS 参考实现确认。
- exec.rs：模块文档既有的 [`PipePump::destroy`] 链接指向 pub(crate) 类型/方法，可能触发 rustdoc::private_intra_doc_links；这是本次改动前就存在的内容，按约束未修改，也未运行 cargo doc 验证。
- exec.rs：模块文档中「TS killProcess 的 5s SIGKILL 定时器永不执行」是既有保真结论，只做了保留与引用；本次未阅读 TS 源码独立验证。
- abort.rs：settle 的文档沿用「非 panic JoinError 只来自取消、本实现从不 abort」的内部不变量；运行时关闭等外部取消路径未验证，已用「不应发生」措辞而非断言不可能。
- event_bus.rs：模块文档中「Node 的 EventEmitter 先复制监听器数组」为既有说法，本次未在仓库内找到验证依据。
- fs_watch.rs：回调具体在哪个线程执行未写死（仅有 Send + 'static 约束作为证据）；NonRecursive 不上报子目录变化取自 notify 语义，仓库测试只覆盖直接子项。

### u153-kiana-core-group

- git.rs 中 GitSource.repo 的原有文档称“总是可 git clone，不含 ref 后缀”，但黄金语料 tests/fixtures/git_url_corpus.json 显示 `#v1.0.0` 形式的 committish 会保留在 `repo` 中，且 kiana-resources 的 install_git 直接把 `source.repo` 传给 `git clone`。带 `#fragment` 的 URL 在 git 中是被忽略、被当作路径一部分还是导致 clone 失败，未实测；按规则保留原注释并只追加了 `#committish` 保留说明。
- 模块文档沿用“url crate 与 Node 的 WHATWG URL 同一规范”的既有说法，并据此描述 correctProtocol/correctUrl 的等价性；本次只用 123 条 TS 黄金语料与本地 hosted-git-info 9.0.3 源码做了对照，未穷举非法 percent、非常规 scheme 等 WHATWG 边界。
- html.rs 原有模块注释写“`html.indexOf(";", index + 1)` 上限 16 个 code unit”，实现实际是先找第一个分号再检查 `semicolon - index > 16`。两者可观察行为一致（超过 16 的第一个分号同样被拒绝），但与 TS 的逐字实现略有表述差异，未改写原注释。
- extract_gitlab 文档把 `/-/` 与 `/archive.tar.gz` 描述为“不是仓库根地址”，这是从 URL 形态与测试语料推断的用途说明；源码只体现为返回 None，未在仓库文档中找到直接依据。

### u154-kiana-core-group

- apply_http_proxy_settings 的既有 SAFETY 注释称本函数“只在启动期调用一次”，但仓库里 crates/kiana-coding-agent/src/app.rs 有 :177 与 :503 两处启动期调用；create_agent_session_runtime（在第二次调用之前执行）是否可能已启动读取环境变量的线程未核实。按规则未改动该 SAFETY 注释。
- management_http / http_dispatcher 模块文档中既有的 Node/undici 逐字文案与内部行为（DOMException 文案、#parseNoProxy/#shouldProxy、kHttpsProxyAgent 回退等）本次未用真实 Node/undici 复核；测试 crates/kiana-core/tests/http_dispatcher.rs 与 tests/management_http.rs 覆盖了主要可观察行为。
- http_dispatcher 模块文档称 reqwest 0.13.5 不暴露 happy_eyeballs_timeout；本次只核对了根 Cargo.toml 的版本 pin 与 Cargo.lock（均为 0.13.5），未读 reqwest 源码确认该 API 不存在。
- 公开项计数为人工统计，口径为 rustdoc 可见条目（含公开结构体字段、枚举变体、变体字段与方法）；仓库内未找到 tools/doc_inventory.py，无法用工具复核。

### u155-kiana-core-group

- shell.rs 中关于 POSIX 信号语义的说明（负 pid 表示进程组、pid=0 表示调用者所在进程组、ESRCH 错误）来自 libc/POSIX 契约，仓库测试只覆盖注入的 kill/spawn 调用序列，未实测 pid=0 场景。
- output_files.rs 中“create_new 在路径是符号链接时失败”“Path::exists 把无权限折叠为 false”属于 std/OS 契约，现有测试只验证 0600 权限与文件存在性，未构造符号链接/无权限场景。
- shell.rs 中“std::env::vars_os() 不承诺顺序”依据标准库文档常识；本机未安装 std 源码，无法在仓库内给出证据文件。
- Windows 相关断言（where 输出 CRLF、taskkill 的 creation_flags 避免继承控制台、Windows 上 mode 被忽略）无法在 Linux 上实测；测试只注入 spawn 并比对 argv。
- output_guard.rs 中“写循环持有 stdout 锁，进程内其它 io::stdout() 用户不会穿插”属于 std 同步契约，测试只验证同一 writer 线程内的顺序与背压，未直接验证跨线程抢锁。

### u156-kiana-core-group

- sleep：`tokio::select!` 未加 `biased` 时同时就绪分支的选择是随机的——此语义来自 Tokio 文档，仓库内没有实测用例。
- sleep：`tokio::time::sleep` 在未启用 time 驱动的 Tokio 运行时外调用会 panic——Tokio 文档行为，本仓库测试只在 `#[tokio::test]` 下运行。
- text：`serde_json` 拒绝前导 BOM 的行为未在本仓库实测；JS `JSON.parse` 不接受 BOM 有 spec/ca-builtin-extensions.md:6193 的明确证据。
- zip：`std::fs::write` 的“创建/截断、写失败时可能已截断”来自 std 文档；仓库测试只覆盖父目录缺失返回 NotFound 与空归档、UTF-8 文件名、往返解压。
- zip：模块文档“仓库内唯一调用方是 bug_report”依据本轮全仓 grep（crates/kiana-resources/src/bug_report.rs:1303 与 tests/zip.rs），未覆盖后续新增代码。
- slash_commands：`kiana-rpc-print` 把 `SlashCommandInfo` 再导出为 `RpcSlashCommand`、camelCase 字段名是跨进程契约——依据 crates/kiana-rpc-print/src/rpc_types.rs:32-34 的注释与再导出，未完整追踪 RPC 序列化链路。
- timings：`LazyLock` 环境变量只求值一次、panic 毒化后 `into_inner` 恢复属 std 语义，本轮未运行测试验证。

### u157-kiana-core-root

- “47 个 TS 文件、约 5,360 行”直接取自 ARCHITECTURE.md §1 的映射表，未对照 TypeScript 源码重新统计，文档数字可能滞后。
- “15 个 crate 直接依赖”按 crates/*/Cargo.toml 中 `kiana-core =` 行统计（kiana-config、kiana-builtin-extensions、kiana-theme、kiana-resources、kiana-extensions、kiana-compaction、kiana-ext-api、kiana-rpc-print、kiana-session、kiana-ui-kit、kiana-export、kiana-tools、kiana-coding-agent、kiana-session-core、kiana-interactive），后续依赖增删会使该数字失效。
- 启动期调用顺序依据 crates/kiana-coding-agent/src/app.rs 的 run_main 当前实现（reset_timings 为第一步；take_over_stdout 仅在非交互且非纯元数据命令时调用；restore_stdout 在分发结束前调用），调用方重构后需同步更新。

### u158-kiana-durable-large

- 全局函数在“执行完成但结果尚未提交进 journal”的窗口内崩溃时，重放会再执行一次；这是由 launch() 先执行、pump() 后提交的顺序推导，未见测试直接覆盖该崩溃窗口。
- ‘VM worker 线程有独立单线程运行时、脚本结束后消失’来自 kiana-codemode 的 host.rs 与本文件既有 runtime 字段注释，未在本 crate 实测。
- Gate/Ready 的 notify_waiters 不丢唤醒依据 tokio 1.54.1 的 Notify 文档（已读 vendored 源码注释：对已创建但未 poll 的 Notified 也保证送达），非本仓库测试覆盖。
- resolve_output_dir 的“创建目录与记录非原子、可能遗留未引用目录”由代码顺序推导，没有测试覆盖该崩溃点。
- js_number 关于“超出 JS 安全整数时 TS 写成指数形式”的说明沿用原注释，仓库内无 TS 源码可核对；CODEMODE_MODULE_VERSION=2 的版本约定同样沿用原注释。
- outcome_of 的错误消息优先级与 tests/codemode.rs 的两个断言一致；结构化工具无 structuredOutput 时 properties 缺失的行为由代码推导，测试覆盖了 schemaless 与 structured-with-value 两种。
- queue_item_clone 与已 derive(Clone) 的 QueueItem 重复，属代码冗余（不影响正确性），按规则未改动，供主代理决策。

### u159-kiana-durable-large

- createTempDir 的 FileError.path 与 spec 可能不一致：create_temp_dir_in 会把加随机字符后的候选路径填进 FileError.path，而 spec/durable-rest.md §2.23 记载 TS 的 createTempDir 使用 toFileError(error)（不带 fallback path，描述为无 path）。已在函数注释中标注为可能的对齐差异，需维护者确认是否有意为之。
- spawn_error_message 在非 Unix 平台存在不可达分支：cfg(not(unix)) 的 node_errno 恒返回 None，函数在到达 async_error 判定前就 return error.to_string()，因此 Windows 上 spawn 错误不会出现 Node 的 spawn <file> <CODE> / spawn <CODE> 文案；是否与 Node 的 Windows 行为一致未实测，仅记录未修改。
- cleanup 杀死命令后 exec 返回 Ok（信号退出码）只有 Unix 测试 tests/env_node.rs::cleanup_terminates_active_shell_processes 覆盖；Windows 的 taskkill 分支未实测，相关注释只按代码路径描述。
- start_spill 注释中“命令继续跑只会产生无法保存的输出”属于动机说明，依据是函数行为（创建/打开失败即 kill）与 TS 测试名 fails_rather_than_silently_losing_a_requested_spill，源码没有逐字文字依据。
- 按任务约束未运行 cargo build/test/fmt/clippy/doc；所有 intra-doc 链接（NodeExecutionEnv、NodeEnvOptions、NodeEnvOptions::new、NodeExecutionEnv::cleanup）均人工核对为 pub 项，但未能实际验证 rustdoc 无警告。

### u160-kiana-durable-large

- Node `setInterval` 对非有限/非正/超过 2^31-1 的延迟按 1 ms 处理，以及 `Duration::from_secs_f64` 对 NaN panic：沿用文件内既有注释与 TS 语义，未在本仓库运行验证。
- `is_denied` 把 raw_os_error 1/13 也当作拒绝访问；这两个数值在 Unix 上是 EPERM/EACCES，但在 Windows 上并非权限错误，强制 Native 模式下可能误分类（仅记录，未修改）。
- 非 UTF-8 文件名经 `to_string_lossy` 后可能与真实路径不一致，随后的 `lstat` 失败时该条目被跳过；这是从代码推导的行为，未实际构造非 UTF-8 文件名验证。
- notify 8.2.1 的 Access/Rescan 事件映射、`error.paths` 内容来自既有注释、TS 语义与单元测试中构造的事件，未在真实 inotify/FSEvents 后端逐项验证。
- 非 Unix 平台 `dev`/`ino` 恒为 0（代码事实），因此 `Installed` 的身份比较无法发现同路径替换是直接推论；未在该平台运行验证。
- `FlushOutcome` 的返回值在 `run_driver` 中被忽略（代码事实，已写入注释），这是有意保留还是遗漏未确认。
- `in_scope` 中的 `path == target.path` 分支不可达：前面的 `is_within(&target.path, path)` 在相等时已返回 true。仅记录，未修改。

### u161-kiana-durable-large

- 未运行 cargo check/test/doc（任务禁止构建）；新增 intra-doc 链接只指向公开项（resolve、resolve_string、resolve_string_with_cwd、normalize、relative、PathBuf、super::posix），按规则预计无新警告，但未实测。
- 空 cwd 会在 resolve_string_with_cwd 的 path.as_bytes()[0] 处 panic：由源码路径推导（parts=["foo"], cwd=""），未运行验证；env/node_watch.rs 的 win32 分支以 "" 调用 relative_with_cwd(from, to, "", true)，若 watch 目标是相对路径可能触发，已只在报告记录、未改代码。
- is_windows_reserved_name 的负 colon_index 语义（CONx 截成 CON 而命中）用 Node 26.12.1 实测 path.win32.normalize('CONx') === '.\\CONx'，但仓库 path_corpus.json 未覆盖该输入；注释按代码行为描述并提示对照 Node。
- basename 在 ext 非空且路径只有分隔符时（C:\\ 加 "x"）返回分隔符本身，Node 26.12.1 行为相同；corpus 未覆盖，文档因此没有断言这类输入一定返回空串。
- resolve 文档示例 C:a + cwd /cwd 得到 C:\\cwd\\a 来自同一代码路径推导；corpus 只有带第二个参数的同类用例（C:a, b/c → C:\\cwd\\a\\b\\c）与 C: 用例，单参数组合未被直接覆盖。

### u162-kiana-durable-large

- resolve_settings 原有文档中的 [`RetrySettings`] / [`ProgressSettings`] 短链接在本模块没有对应 use，本次按『保留已有注释』原则未改动；若 cargo doc 报 broken-intra-doc-link，属于本次之前就存在的问题。
- add_tools 文档沿用既有 TS 注释『等值写入不产生文档变化』；本次未逐行验证 Rust 事务层对相等文档值的 diff 行为。
- AgentHook::handlers 的『hook 名 → 处理器』映射形状来自 scheduler.rs 中 SchedulerHooks::handlers_with_extension 的 downcast 逻辑；其它任务可以使用不同的处理器表形状（scheduler 会把无法识别为映射的值原样交给任务代码），已在注释中限定为『约定形状』。
- DEFAULT_PROGRESS_POLICY.persist_within_ms 实际不参与 resolve_settings 的 persist_within_ms 解析（后者走 session::persist_within，默认常量同为 1000.0）；若两处常量将来分叉，解析结果以 session 常量为准，这一点只写在报告里，未写成公共 API 的保证。
- section_extension 对 key=instructions 的包装器归属是静态声明层面的结论（resolve_agent 在包装阶段之后才追加 instructions 分段），未在 generation 的实际错误上报路径中单独跑测试验证。

### u163-kiana-durable-large

- estimate_context 用 AssistantMessage 的结构相等在 view.messages 中定位基线消息（TS 的 lastIndexOf 是引用相等）；若存在两条完全相同的 assistant 消息，Rust 可能取到较晚的一条，导致 from 偏大。已在函数文档标注该差异，但未构造用例验证触发条件。
- truncate_chars 在 UTF-16 代理对中间截断时用 String::from_utf16_lossy，落单代理项会变成 U+FFFD；JS 的 slice 会保留孤立代理项。未找到针对代理对边界的测试。
- slice_end 对 end == i64::MIN 会计算 -end：开启溢出检查的 dev/test 构建会 panic；本 crate 内只传 -1。已在函数文档中作为边界注明，未新增测试。
- simple_stream_options 把 i64 的 max_retries 用 `as u32` 转换：负数钳成 0，但超过 u32::MAX 的值会截断；未确认是否需要饱和处理，也未找到传大值的调用方。
- place_summary 的 requestId 幂等去重依赖 admit_submission 先查已有提交；并发或任务重放下的完整语义未在本文件测试中覆盖。
- create_compaction 文档中『background 不阻塞普通 idle 等待、会话 abort 与级联』来自 TaskOptions.background 的字段文档，未在调度器中逐路径实测。

### u164-kiana-durable-large

- capture_context_bounds 原文档称「只需两次 O(1) 读取」，但 at 为 Some 时还会先执行一次 visible_entry 可见性校验（实际三次存储读取）。原句保留未改写，仅在新补充段落里说明实际步骤；这是否为 TS 原文档口径待维护者确认。
- 「第一个 kiana.system 之后的同类条目通常只带增量补丁」依据 harness/prompt.rs 的 plan_system_entries 与 tests/harness_context.rs 的行为归纳，未做穷尽证明（用「通常」限定）。
- scan_range 依赖所有 Storage 后端在 EntryQuery.order=None 时默认降序；本次核对了 types.rs 字段文档、storage/scan.rs 与 memory.rs 实现，未逐一阅读 sqlite/jsonl 后端。
- 注释中的「会话变更线」仅依据 session.rs 的 read_on_line/enqueue 说明本会话任务串行；未声称跨会话快照或全局原子性。
- ContextBounds 在类型层没有强制 head.id <= tail 或 head.head <= head.id，注释刻意避开这两个断言；若维护者确认不变量可再补充。
- 既有注释 `/// 基线的 system 消息；见 [`find_baseline`]` 链接到私有函数，已改为行内代码以符合 rustdoc::private_intra_doc_links 规则；若日后 find_baseline 变为公开可恢复链接。
- Origin::Unknown 的旧注释称「复用的上一段范围里的 settled/open：来源已丢失」，与当前实现不符：extend_range 会把 previous.settled/open 与 previous.origins 重新配对（grep 可见 Unknown 仅在 order_tool_results 与 cfg(test) 辅助函数中构造），已按此改写。

### u165-kiana-durable-large

- 既有注释引用的 `spec §9.4` 在本仓库中未找到同名章节；仓库内与 events.ts 对应的是 `spec/durable-harness.md` §2.13（事件顺序权威实现）。未改动原引用，只在 translate 文档里补了该节号。
- `parts()` 把内建文档“解析失败”与“文档缺席”都归为 `None`；若 before 为 Some 而 after 解析失败，会按删除式语义发出 InboxUpdate/AgentChanged/UsageChanged（值为空列表/初始值）。这是源码可读出的行为，但没有测试直接锁定“非法文档”场景，未写成设计意图。
- `tool_update` 用当前列表下标（`tools`/`nestedTools` 的 index）构造 output/details/diagnostics 的 op 路径，而 previous 槽按 call id / task id 匹配；若同一提交内槽列表发生重排，op 路径与 previous 可能对不上。仅记录未修改。
- 文件末尾 `const _: fn()` 只用 PhantomData 标记 `TaskGraphState` 为已使用；既有注释把它与 `TableCommitChange` 并称为“文档可见性的一部分”，但 `TableCommitChange` 实际在 translate 的 match 中使用，该注释可能已过时。按规则未改动。
- 未运行 cargo check/doc/fmt（任务禁止）；rustdoc 链接只做了静态可见性核对：公开文档中的链接目标均为公开项，私有项一律用行内代码。

### u166-kiana-durable-large

- 公开项计数口径未与 tools/doc_inventory.py 核对：本报告按 rustdoc 口径把结构体字段与枚举变体计入（52）；若只计顶层项则为 23/23。
- prepare 的两处 outcomes 检查中，None 分支的容忍行为按代码与 spec §2.10.3 注释；当前调度器实现（scheduler.rs:5763-5790）对不存在或非终态任务返回 Err，因此该 None 分支在现实现下可能不可达——注释只描述该分支行为，未断言其可触发。
- abort 处理器在本轮工具任务终态之后运行，依据 spec §8.5 与已有代码注释，未逐行追踪调度器的 abort 分发路径。
- start_run『调用方只在没有活动运行时调用』来自对 submissions.rs 与该文件全部调用点的阅读（busy 分支不启动、其余路径都在 end_run 之后），函数本身不做检查。
- 工具任务随 generation 级联的说法已从注释中删除，只保留 TaskOwnership::Task 这一源码事实，因为未逐行核对调度器级联实现。
- submit/usage 提交的原子性（append_assistant 注释中的『一起提交或一起回滚』）依据 Tx 事务语义与既有注释，未通过测试实测。

### u167-kiana-durable-large

- register_views 的 key（Weak::as_ptr 转 usize）与 conversation_views 从 &dyn Harness 取得的数据指针是同一对象地址：这是按 Rust 胖指针语义推断，未运行验证。
- safe_now 既有说明“时钟在每次任务变化时都会被读”未逐一核对调度器的全部读取点，按原注释保留。
- usage 既有说明“总量只会增长”沿用原注释，未验证 UsageState 各计数字段在存储层的单调性来源。
- Harness::open 的第 2 步（build_harness）失败时只返回错误、不显式关闭已构造的 Session/Storage；文档只说“直接返回”，未断言该路径的可达性，也未修改代码。
- inspect/usage 的一致读说明只相对本 Session 的变更线成立；外部进程直接并发写同一 Storage 的可见性不在保证范围。
- compact 注释中“压缩结果由该任务的终态给出”依据 create_compaction 与 TaskId<CompactionResult> 的类型和文档，未追到调度器回填任务结果的实现。
- watch_view 随“内存里采纳的延迟提交”推进依据 view.rs 的模块头说明，未运行对应测试验证。

### u168-kiana-durable-large

- nestedTools 二分查找依赖任务 ID 随创建顺序单调递增；我只在 tool.rs 的 start_nested_call 看到 create_task 后立即追加槽位，未读 create_task 的 ID 分配实现，无法在本单元内证明跨提交的全局单调性。
- LiveRetry.at、LiveDeferred.poll_at、NestedToolSummary.duration_ms 是 f64，走 serde 默认序列化；events.rs 对快照另有 JsNumber 自定义格式，live 文档本身是否与 TS JSON.stringify 逐字一致未验证（现有单测锁定 10.0/5.0 这类输出）。
- PROGRESS_COMMIT 的延迟落盘时机（下一次读写 kiana.live 的提交一并写入或 settings.progress.persistWithinMs 定时器）我只读了 session.rs 的 deferral/run_commit 入口，未逐行跟踪 hold/定时器的落盘路径。
- DocumentFork::Initial 在 fork 时使用定义初始值（本定义为空对象）这一说法依据枚举文档与 DocDefinition::new 的默认 initial，未读 fork 的具体实现。
- compactions 按任务 ID 升序依赖 create_compaction 在同一提交里 create_task 后立即 push；grep 目前只看到这一个 add_compaction_status 调用点，但未排查未来/其它提交路径是否可能乱序追加。

### u169-kiana-durable-large

- 模块 //! 与 TaskSchedulerOptions 文档中“State 守卫不跨 await、宿主回调一律在锁外调用、因此可重入”的结论来自逐处通读（含既有注释佐证），未用静态分析工具穷举验证。
- erased_migrate 除 ErasedTaskMigrate 外还接受裸 TaskMigrate<JsonValue, JsonValue> 分支；两种存放形态的来源与用途在本文件、调用方和 spec 中均无直接说明，故未写入注释，仅记录为待核实。
- wake() 文档把 active 描述为“存在未被 completing/等待活任务/未到期睡眠/停泊/定义检查排除的活记录”，这是对代码分支的归纳；宿主对 Wake 的全部消费方式只核对了 types.rs 的字段文档，未逐一追踪 harness/tui 调用方。
- TaskSchedulerOptions 的“回调重入不自死锁”是对本文件所有回调调用点的归纳（settings/registry/timers 等既有注释提供依据），未编写并发测试验证每个回调路径。

### u170-kiana-durable-large

- public_total/public_documented_after 按“顶层 pub 项 + pub 方法”计数为 22；若统计把枚举变体/结构体字段也算独立公开项，数字会更大，但它们在改动前后都有文档。
- 未运行 cargo check/doc（任务禁止），intra-doc 链接仅人工核对：模块文档链接指向 TOOL_TASK、ToolTaskInput、ToolTaskResult、NESTED_CALL_DOC、ToolRegistration、ToolTaskCheckpoint::Execute、ReplayPolicy::Safe，公开项文档链接指向 AssistantEntry、NestedToolExecutionResult、DiagnosticSeverity::Error、AbortPhase 等公开项；未实际渲染验证。
- 仓库目录未被 git 跟踪、无基线快照，无法运行 comment-only diff 脚本；只能保证每次编辑仅插入注释行，并复核了公开签名与 #[serde]/#[derive] 属性文本未变。
- “Failed 会取消调用拥有的会话”沿用了 execute() 内原有注释，未在 scheduler 侧独立验证。
- “自动 key 与显式正整数共用同一命名空间”是从 begin_admission 的 sequence 自增与 check_key 的规则关联得出，源码文档没有明说。
- NESTED_CALL_DOC 的“随调用者任务退休”依据是 tests/codemode_checkpoints.rs 的模块说明与 DocDefinition scope: Task，未追踪存储层删除实现。

### u171-kiana-durable-large

- AnyTaskDefinition.hooks 的读取方：仓库内只找到写入点（harness/scheduler.rs 的 erase_task 与 harness/live.rs 的 erase_task_definition），未找到读取点；注释只描述存储约定（Arc<H>），未断言调度器如何使用。
- Harness::wait_for_task / TaskRuntime::wait_for_task 在任务不在本进程 live 集合时可能经 SettledTask::from_record_unchecked 返回非终态记录：依据 harness/scheduler.rs 的实现与既有注释，未找到专门锁定该行为的测试。
- TaskInspectionState 的判定优先级（Running → Completing → Waiting → Sleeping → Blocked → Ready）来自阅读 scheduler.rs 的 inspect_task，未找到对应测试。
- PromptSection::render 失败时保留上一版该 key 文本：依据 harness/prompt.rs 的 render_sections 代码，未找到测试名。
- Conversation::entries 的 fork-aware 拼接细节在 Storage 的 scan_entries 实现中，本次只验证了 harness.rs 的透传（补会话 ID、limit/cursor 原样传入），未深入存储层。
- HarnessOptions.identity 与 DEFAULT_HARNESS_IDENTITY 的逐字段合并顺序来自 harness.rs 的 current_models 注释与 merge_client_identity 调用，未逐字段验证该函数本身。

### u172-kiana-durable-large

- 未运行 cargo check/doc/fmt（任务明令禁止），新增和既有 intra-doc 链接的可解析性未经工具验证；文件原有 //! 中 [`JsonWatch`] / [`NativeWatch`] 指向 pub(crate) 项，按“不改写既有注释”的要求保留，可能触发 rustdoc::private_intra_doc_links。
- WatchCore::schedule_arc 在 Tokio 运行时之外调用时 Handle::try_current() 返回 Err，不 spawn 也不复位 scheduled，该 watch 之后不会再排程（advance 会因 scheduled 为真而跳过）；此结论由代码分支直接读出，未写用例验证。
- ConversationViews 的 _commit_unsubscribe / _close_unsubscribe 类型是 Box<dyn FnOnce()>，丢弃不会执行注销；若 Harness 未 close 就被 drop，提交监听器闭包持有 SessionImpl 克隆，可能与 SessionInner 形成引用环。是否真有泄漏、是否还有其他清理路径未验证。
- ConversationViews::attach 在 create 失败、上下文取消或关闭检查失败时会丢弃已经构造的观察者（含 JsonWatch），其 detach 不会被调用；因为观察者尚未登记，我判断不是泄漏，但未写测试验证。
- 私有 advance 对 head 标记的增量截断与重新构建挂载在“head 目标早于当前活动范围”时结果不同（已由 tests/harness_view.rs 的 keeps_only_mounted_entries_for_a_raw_head_write_before_the_active_range 锁定）；注释按测试名引用，未逐行推导 harness/context.rs 的 active_entries 内部实现。
- entry_value 序列化失败退化为 Null、随后整帧 from_value::<ConversationView> 失败并让 Session 失败，这条链路是从代码推导的，未构造真实失败输入实测。
- attach 中 `let report = Arc::clone(&self.report);` 以及 create 失败分支的 `let _ = report;` 看起来是未真正使用的克隆；按只记录不修改的规则保留。
- 模块 //! 原有“语义与 session/session.rs 的文档 watch 一致”一句保留未改；实际实现中 value() 的 retained 列表挂在共享 WatchCore 上（session 的 Watch 是每句柄一份、克隆为空），投递语义一致但保留策略不同。
- WatchCore::current 的 retained 列表对长期存活且反复 value() 的 watch 会持续增长（只在 core 释放时清空）；代码可确认，但是否有调用方这样使用未验证。

### u173-kiana-durable-large

- 无 Tokio 运行时时 SessionInner::fail() 只执行到 begin_close() 而不 spawn finish_close；之后 close_result() 会以 Joiner 身份等待一个无人完成的 SharedClose（源码观察；是否有意、TS 是否等价未用测试确认）。
- Watch::schedule() 在无 Tokio 运行时时已把 scheduled 置为 true 却不 spawn 投递任务；之后即使进入运行时，advance()/start() 也可能不再排程（源码观察，未验证）。
- GuardedStorage::call() 的 closing 检查与 underway 计数递增不是原子步骤；多线程下 close_storage() 可能在这两步之间读到旧计数并开始关闭后端（已在私有文档中标注；sqlite 模块的 admit_read 存在同类既有注释，但本处未实测）。
- idle_documents 的“最旧在前”清扫依赖宿主时钟不倒退；时钟倒退时 sweep_documents() 会在第一个未过期条目处提前停止（由代码推断，无测试覆盖）。
- SessionHost::now() 在 Weak<SessionInner> 升级失败时返回墙钟而非错误；正常提交路径中事务由 run_commit 持有的 Session 保活，该分支是否可达未验证。
- commit_with_scope() 的原有注释称“Harness 用”，但当前仓库内没有调用方（grep 仅本文件定义）；实际使用方未知。
- SessionInner 是 pub，但仓库内没有外部引用；其公开可见性是否属于稳定契约未知。

### u174-kiana-durable-large

- `assert_task_documents_open` 只检查本事务中该任务的候选写是否终态；已持久化的终态任务如果其 task 作用域文档已存在，`doc()` 会走 `acquire_document` 的加载分支而不报错。这是否与 TS 一致无法确认（仓库不含 packages/durable 源码）。
- `retire_doc` 的 `WaitAcquire` 分支在并发 `doc()` 获取失败（条目 target 仍为 None）后会静默跳过退休（`plan_document` 返回 None）；是否为预期行为未核实。
- `holdable()` 里 `base: content.kind == Base` 在当前源码下似乎不可能为 true：它同时要求 `stored_version == change.version`，而 `plan_document` 只在 `stored_version < version` 时产生 base 内容。疑似为 TS 形状保留的字段，未核实是否有其他写入路径。
- 原有模块注释称“adopt 回调只短暂加锁”，但 `Transaction::adopt` 在整个计划循环（包括同步调用 `host.install`/`evict`）期间持有 `Mutex<TxInner>`；它不跨 await，因此不影响异步安全，但“短暂”与实现不完全一致。按保留原注释的要求未改写，仅在此记录。
- 同一个逻辑地址在一个事务内用不同 `definition.version` 再次 `doc()` 时，代码沿用条目上首次登记的定义且不做版本校验（已在 `doc()` 文档中如实描述）；与 TS 行为是否一致未核实。
- `committed_task` 的 OnceCell 缓存保证“成功时每个 TaskId 每事务只读一次后端”；读取失败时 tokio OnceCell 是否保留未初始化状态（因而下次会重试）未在本仓库内验证，注释已按“成功时”限定。

### u175-kiana-durable-large

- 并发：本文件只在 StorageState 上使用 Mutex，commit 的 prepare、文件 I/O 与 apply 都不持该锁，因此本类型没有实现跨 I/O 的提交串行化；上游（Session/Harness）是否保证同一实例的 commit 串行调用，本次未在源码或测试中找到依据。
- commit 在 main.jsonl 的 marker 已追加成功之后，若 prepared.apply() 失败会直接返回 Err 而不 poison；memory.rs 的文档称 apply 只可能在内部不变量破坏时失败，本次没有构造出触发路径，测试也未覆盖。
- recover 用 sidecar_numeric_part + parse::<i64>() 还原 ID，超长数字文件名解析失败时回落到 i64::MIN 哨兵；TS 侧是 Number 解析（浮点近似，同样不会命中真实 ID），两侧可观察结果是否等价未做对照测试。
- fsync 注释描述的是调用顺序（marker 前 flush sidecar、回收前 flush main），并未验证 NodeExecutionEnv 等 FileSystem 实现中 flush_file 的实际落盘保证；本次未阅读其实现。
- 模块文档中“外部进程对目录的修改只有在重新 open 时才会被看到”由代码结构直接推出（读取全部走内存镜像），没有对应测试断言。

### u176-kiana-durable-large

- 并发 prepare→apply 可能拿到相同默认序号，或让 next_seq 因乱序 apply 回退：这是从 Rust 代码与 TS 源（commit 中 prepareCommit 与 apply 之间无 await）推演的行为，仓库测试中没有并发提交用例，无法确认上层 session/harness 是否已串行化。
- PreparedMemoryCommit::apply 在唯一错误分支（未解析的 document.copy）下可能已把排在失败写入之前的表记录落地；该路径经公开 API 不可达，未找到测试覆盖。
- close 只置位 closed、已 prepare 的提交仍可 apply：代码与 TS 一致（TS 的 apply 也不检查 closed），但仓库中未找到专门锁定该行为的测试。
- 对 current-only 文档的历史读取只在 materialize_document 拒绝；find_document 对同一地址的历史时间点会正常返回记录。这与 TS 源码一致（findDocument 不调用 materializeDocument），但调用方是否依赖这一差异未核实。

### u177-kiana-durable-large

- admit_read 的 `assert_open` 与计数递增相对 `close_result` 的交错（关闭方在递增前看到计数 0 就会关库）是由“两条无锁语句”的代码结构推导出的失败路径，未用并发测试复现；若审阅认为不应作为断言，可删掉该段。
- “单查询读与 commit 与 close 的相对顺序由数据库适配器契约决定”依据的是 TS `admitRead` 注释与 kiana-chord `SqliteDatabase` 的事务排序契约；chord 的 `close` 文档只写“关闭后其它操作的行为由适配器决定”，没有逐字承诺在途单查询先完成。
- mint_id 注释中“提交前崩溃/关闭会丢掉这次递增”是由 initialize 只读 durable_metadata、只有 commit 写回推导；仓库测试只覆盖正常重开路径，未实测崩溃。
- 既有模块文档里的 [`ErrorSlot`]（私有类型别名）与 [`super::node::open_node_sqlite_storage`]（cfg(feature = "sqlite")）链接未改动；按 COMMENT_STYLE 可能触发 private_intra_doc_links 或 feature 关闭时的 broken link，请主代理跑 cargo doc 验证。
- public_total=9 只统计本文件声明为 pub 的项（2 个 struct、1 个 pub 字段、1 个 pub 错误字段、new/open/next_id/close_result 及 SqliteStorage 本身）；impl Storage 的 17 个方法继承 trait 文档，其中 6 个另加了本地 ///，未计入该数字。
- complex_blocks_explained=18 按“获得分阶段/机制级说明的函数或算法”计数（commit、commit_transaction、prepare_document_actions、check_global_ids、check_document_actions、apply_table_write、apply_document_actions、materialize_document、admit_read/守卫、read_visible_entry、read_latest_head_marker、read_entries、read_entries_ascending、scan_documents、mint_id、candidate_next_id、close 流程、地址编码）；另有若干 1-3 行澄清未计入。
- 未运行任何 cargo build/test/fmt/doc（任务硬性约束），因此未验证编译与 rustdoc；改动纯净性依赖人工核对，所有新增行均为 //、/// 或 //!。

### u178-kiana-durable-large

- read_all 在读取或断言 panic 时不会 close 读取器（TS 版用 try/finally 保证关闭）；Rust 版仅正常结束与 1000 页耗尽两条路径显式 close。panic 路径是否依赖 Arc<dyn DirReader> 的 Drop 或 provider 清理，本次未验证。
- watching 文档中说“实现可能从其他线程调用 watch 回调”仅来自 WatchCallback 的 Send + Sync 约束推断，未逐实现确认实际线程模型。
- list_files_or_panic 依赖错误码加固定消息文本探测“未实现”；若某实现自定义 not_supported 消息，会被当作普通错误 panic。这是 TS 成员缺失语义在 Rust trait 下的移植折衷，未验证是否所有实现都使用默认消息。
- 模块文档称 provider 必须恰好调用一次回调、cwd 为全新空可写目录，依据是 testing/types.rs 中 EnvConformanceOptions::with_env 的文档；本文件本身不校验该契约。
- 未运行 cargo check/doc/fmt（任务禁止），因此 rustdoc 链接与格式仅经人工核对：公开文档中的链接仅指向 ExecutionEnv、create_env_conformance、crate::testing::runner::register_env_conformance 等公开项。

### u179-kiana-durable-large

- “provider 每次 use 必须提供干净 storage”是从用例断言反推的：第一个用例断言未写入时 mint_id 先返回 2，且 26 个用例都先调用 create_root；TS 源码只写明 withStorage 必须每个用例恰好调用并等待一次回调，没有显式要求 storage 全新。若上游文档有不同约定，需回调模块文档这一段。
- AdoptionChecked 的 settled 标记只能发现“包装器观察到 inner.commit 返回之后才调用 adopt”；若后端从另一线程在持久化完成后、commit 返回前并发调用 adopt，包装器无法区分，因此它不证明后端内部的持久化顺序（文档已按此限制描述）。
- 既有用例 24 的注释引用 spec/env-evals-durable-testing §2.3.6 与 spec/durable-rest §8.9；本次按规则原样保留，未核对这两个 spec 文件的内容。
- assert_rejects 只做子串匹配，具体错误文本由后端决定；新增后端必须保留本套件用到的子串（如 already belongs to、Unknown conversation、does not retain historical content、version transition requires a base、ID space is exhausted、closed）。
- tests/memory_storage.rs 没有调用 create_storage_conformance（它把 26 个用例内联成独立测试），所以模块文档只把 jsonl/sqlite 测试列为直接调用方；这是基于 grep 的事实，不是遗漏。
- complex_blocks_explained 计的是函数级机制注释（AdoptionChecked::commit、conformance_case、create_storage_conformance、scan_id_page、scan_all_ids、scan_with_cursor_future、submission_ids、create_root 及 JSON 辅助组）与 7 处复杂场景注释（混合表回滚、owner 交集、fork 树、缺省方向游标、文档时间线、长 delta 流、文档复制）的合计近似值。

### u180-kiana-durable-large

- detect_line_ending 的 match 分支 `(Some(_), None)` 在逻辑上不可达：存在 `\r\n` 就必然存在 `\n`。只记录，未改动代码。
- 按任务要求未运行 cargo check / cargo doc / cargo fmt / clippy：语法、intra-doc link 与 rustfmt 影响仅经人工核对；所有新增链接均指向本模块公开项或公开的 `crate::tools::edit`。
- diff_line_changes 末尾的 `Vec::new()` 兜底分支没有测试覆盖，无法确认是否可达；注释按“兜底、structured_patch 视为无差异”描述。
- structured_patch 用 0 表示“没有打开的 hunk”的安全性来自推演（行号从 1 起、回填的上下文不超过已消费行数），没有专门测试。
- 纯空白 oldText 归一化为空、且内容本身也只剩空白时会走到零长度匹配，随后整行保留路径可能因两侧行数不一致报错；TS 源码逻辑相同但未实际运行 TS 验证，此极端路径无测试，未在文档中写成确定行为。
- “错误文案是模型可见输出”的依据是 edit.rs 把 BoxError 从工具执行返回、tests/tools.rs 断言 Display；harness 内部如何呈现该错误未逐层追踪。
- UTF-8 字节偏移与 TS 的 UTF-16 码元差异只确认了本模块内部自洽（匹配与应用使用同一坐标系），未逐例用含 emoji 的输入对照 TS 参考实现。

### u181-kiana-durable-large

- TaskRecord 没有 key_order 提示，键序固定为字段声明序；TS 的 setTask/#stampTimes/调度器是否在所有路径上都以“创建序 + 追加序”落键，本次只读了 types.rs 与少量调用点，未核对全部写入点，若存在其它赋值顺序会与 TS 字节序不同。
- DocDraft::value()/value_mut() 通过 unsafe 返回脱离 MutexGuard 的引用，依赖“事务草稿只在同一任务使用”的约定；但 DocDraft 自动实现 Send + Sync 且 derive Clone，安全代码可跨线程克隆后并发调用 value()/value_mut()，与该 SAFETY 前提冲突，是否可接受需维护者确认。
- Transaction 的 try_settle_submission / try_place_submission 是否构成对外可用的错误通道未确认：Tx trait 只暴露无 Result 版本，SessionChange 拿到的也是 &dyn Tx；文档只描述了实现中存在该变体。
- EntryRecord/EntryDraft/SubmissionRecord 的反序列化会丢弃未知键（IgnoredAny + 规范键过滤），旧/新版本数据混用时的前向兼容影响在仓库文档中未见说明。

### u182-kiana-durable-group

- native_timer 的取消/到期竞态与 abort 行为按源码顺序和 tokio 文档描述，仓库内没有直接测试（tests 使用替身计时器），未实测。
- `output <= total` 不变量只在 TruncationTotals 的前缀契约成立时成立；调用方给错 totals 时 total_* 与 content 的统计会互相矛盾，注释已按条件表述。
- index.rs 的 public_total 按 27 条 pub use 语句计；若按再导出名字逐个统计会更大，且其文档全部继承自目标项。
- search 与 kiana-builtin-extensions 两处 BM25/分词实现是否必须同步属于维护约定，代码中无强制机制；注释只说明修改时需分别确认。
- TS 侧空前缀时 lines[0] 会抛错的细节只在 spec/durable-rest.md 读到，未运行 TS 验证；Rust 注释只描述 lines.first() 的 0 字节处理。

### u183-kiana-durable-group

- 英文提示文本（DESCRIPTION_INTRO、globals_description、MODELS_DESCRIPTION、错误消息与输出模板）是否与 TS 逐字一致：只对照 pi/packages/durable/src/codemode/*.ts 做了人工目视核对，未做自动化字节级对比；仓库测试只断言片段。
- describe.rs 文档中“render_tool_sample 在 schema 的 $ref 含畸形百分号转义时会 panic”来自 kiana-codemode 的源码文档，未在本单元的测试中实际触发验证。
- truncate 的预算只统计文本项，图片块（base64）不参与 max_bytes 计算；现有注释假设“结果的其余部分只有几行”。超大 image() 输出是否会被 Harness 的 output_limits 兜底裁剪未在源码或测试中确认（keeps_a_result_inside_the_tool_output_limits... 只覆盖文本）。
- namespace_of 用 prefix.len()（UTF-8 字节数）比较“最长”。若 registry 允许非 ASCII 工具名/前缀，字节长度与字符长度的“最长”语义可能不同；未验证 registry 对名字字符集的约束。
- codemode_section 仅按命名空间 name 合并组：同名但 prefix 不同的配置会合并，description/instructions 取首次出现者。TS 侧同样以 ns:name 为键，但本仓库是否会配置这种重复名字未核实。
- select_catalog 以工具名作为展示去重键；registry/define_tool 是否允许同名工具未核实。若允许，同名工具会共享“是否展示”的判断（模块文档已按条件句描述，未写成事实）。

### u184-kiana-durable-group

- decode_whole 与 Node `new TextDecoder().decode(bytes)` 的逐字节等价（尤其第二个连续 BOM 的处理）是从 U+FFFD 编码规则与既有 TS 注释推导的；本仓库测试只断言 StreamDecoder 与 decode_whole 相等，没有对 Node 的交叉验证。
- 文件级注释原样保留的“Node 流式解码器在开启 BOM 处理时可能丢弃块边界之后的 U+FEFF”来自 TS 源码注释，本次未在 Node 上复验。
- FileWatcher 的“close 返回后不再有回调”依据 trait 方法注释与 node_watch 的 closed 检查；若回调在 close 期间已在另一个任务中执行，未分析其并发时序。
- TextLineReader“最后一行无换行时 terminated=false”只在 node.rs 实现中核对，kiana-env-client 的实现未逐一审阅。
- “kiana-env-client 的远程实现复用 RangeDecoder/StreamDecoder”依据 remote_env.rs 的 import 与模块文档，未审阅其全部调用路径。
- line_scan 的“内存占用与文件大小无关”是结构观察（不保留文件内容、head 至多 3 字节），没有实测数据。

### u185-kiana-durable-group

- mod.rs 原有第 3 条落点理由称 kiana-env-client 通过 `pub use kiana_durable::env::path::posix as node_path;` 复用，但全仓 grep 未找到该再导出，实际只有 remote_env.rs 的 `use kiana_durable::env::path::{PathFlavor, posix, win32};`。已按源码改写注释（保留 ARCHITECTURE.md §8 第 23 条为设计意图的说明），是否同步更新 ARCHITECTURE.md 由主代理决定。
- posix_cwd() 直接返回 std::env::current_dir() 的 lossy 字符串；在 Windows 宿主上它是 `C:\...` 形式，却会被 resolve_string 当作 posix cwd（相对串）。本次未验证 kiana-env-client 在“Windows 客户端 + posix 远端”组合下的实际行为，未写入文档事实。
- resolve_string_with_cwd 的快路径在“无参数/仅空串或 .”且 cwd 以 `/` 开头时直接返回 cwd、不重新归一化；若调用方传入带尾随斜杠或含 `.`/`..` 的 cwd，结果会与完整归一化路径不同。现有测试与调用方均传规范化的 `/cwd`，未发现受影响路径。
- relative_with_cwd 假定两次 resolve 结果都是绝对路径（from_start/to_start 固定为 1）；env::node_watch 传 cwd="" 依赖快照键为绝对路径。该保证来自 node_watch 模块文档（快照键保持 resolve_path 输出形式）与 node.rs 的 platform_resolve 调用，未逐条追踪 in_scope 的所有调用来源。
- normalize_string 与 Node 在 UTF-16 码元上扫描的等价性结论，依据是现有 7972 例 corpus 与代码推理；本次按任务要求未运行 cargo test，也未用 Node 实跑新构造的边界用例。
- posix.rs 原文件级文档写“join（空串参数忽略、`..` 与绝对段截断）”与代码/单测矛盾：posix.rs 单测断言 join(&["a", "/b"]) == "a/b"（绝对参数不截断）。已改为“绝对参数按普通段拼接”，此处属于改写既有注释，需要复核。
- posix::SEP 在当前仓库没有调用方（grep 仅命中定义与文档）；注释按 Node 契约描述，未声称有实际使用者。

### u186-kiana-durable-group

- ‘prepare_boundary 必须在首次表写前调用、否则违反 Session 读后写约束’来自 spec/durable-harness.md §2.7.3 与 submissions.rs 注释，未在 Rust 事务实现中逐行验证是否强制。
- BoundaryResult.users ‘正常入队时位置序与提交 ID 升序一致’依赖提交 ID 单调分配；未深入 storage 的 mint_id 实现验证。
- 任务禁止 cargo check/doc/fmt，无法实测文档构建；新增 intra-doc link 仅按名字可见性人工核对（全部指向公开项或本模块条目）。
- 仓库无提交与基线快照，未运行 tools/check_comment_only_diff.py；自检方式是 Edit 保留原代码行、过滤注释后的代码行哈希在最终编辑前后不变、括号平衡检查通过。
- json.rs 原注释‘PlanFields 按插入序的键值表’与代码矛盾（std::collections::HashMap 不保序），已改写为查找表并说明调用点不依赖顺序。
- inbox.rs BoundaryResult 原注释‘按 ID 顺序’与实现 users.sort_unstable()（按下标）不完全一致，已改为‘按 inbox 位置升序’并说明正常入队时与 ID 序一致；测试 a_queued_steer_behind_waiting_items_places_with_the_first_follow_up_in_id_order 佐证。
- INBOX_DOC 的 checkpoint ‘空队列存 base、非空保持 delta 链’引用 spec §2.7.1，未运行单测验证。
- apply_json 的 Append 分支只校验 UTF-16 长度、不校验 text 确是 value 后缀；已写成调用方前提，未找到针对同长度不同内容的测试。

### u187-kiana-durable-group

- plan_tools 在 offered 出现重名工具时会让回放列表长于 desired，随后 desired[index] 逐位比较越界 panic；当前调用链（resolve_agent 按名字合并进 IndexMap、get_current_tools 返回 IndexMap 值集合）保证名字唯一，未构造重名输入实测。
- ToolCallState.committed 在事务最终失败后不复位：依据是 generation::commit_partial 在 TaskCommit 闭包内调用 PartialChanges::commit、而 PartialChanges 独立于存储事务；未用真实失败的 runtime.commit 实测该时序。
- “文本增量不 restore，下一次 assign_json 也会整体写入”来自对 harness/json.rs 中 assign_json 字符串长度吻合条件的推导；测试只覆盖了工具参数经 restore 的路径，未单独跑该文本序列。
- apply_change 中“数组父容器 + Key / 对象父容器 + Index 是防御分支、正常不会出现”依据 kiana-ai streaming_json 的 reported_path 按 Slot::Member/Element 生成 Key/Index；未构造该输入验证实际行为。
- TS 黄金值与顺序结论引用自 spec/durable-harness.md 与测试内既有注释，按任务要求未运行 cargo test 重新验证。

### u188-kiana-durable-group

- JS 正则 `$` 可在末尾换行前匹配，而 `is_valid_section_key` 的逐字节扫描会拒绝 `"abc\n"`；仓库内没有对应测试，也没有 TS 源码可对照，无法确认这是有意的严格化还是移植差异。
- provider session id 用于 prompt cache 归组的说法来自 `tests/provider_session_cache_e2e.rs` 的模块说明（该 live 用例 `#[ignore]` 且当前以 panic 占位），未实际运行验证；模块文档已按“用例目标”措辞，未当作已实现能力。
- 展示型写入与空闲提交路径通过 `inbox_items` 做类型化解析，inbox 中出现未知 mode 或缺字段的项会让提交失败；`harness/inbox.rs` 的边界路径则保留原始 JSON。仓库内没有 TS 源码，无法确认两边的容错行为是否一致。
- `SECTION_KEY_PATTERN` 在仓库内只有定义、没有任何引用（`is_valid_section_key` 不读它）；是否存在仓库外消费方未知。
- `RegistryImpl` 的并发发布语义（每次发布按自己复制的监听器列表各调用一遍、监听器看到调用时的最新快照）是从锁作用域直接读出的，但仓库内没有并发测试覆盖。

### u189-kiana-durable-group

- telemetry.rs 中 phase span 的 status.error_when 文本引用 task_failed / task_orphaned，但 REPORT_TYPES 的九个取值里没有这两个（TS 原文件同样如此）。未改任何字符串，仅记录，需与 TS 维护者确认是否为文案漂移。
- TaskGraphView 的三个 _commit_unsubscribe/_blocked_unsubscribe/_close_unsubscribe 字段在本模块内从未被调用；Unsubscribe 是 Box<dyn FnOnce()>，字段被丢弃时也不会执行注销。实际清理依赖 Session 关闭时清空 internal/close 监听器与调度器释放；这算不算有意设计未能确认。
- add_usage 的 typed 路径对 u64 计数器用 +=（debug 下溢出 panic、release 下回绕），而 add_usage_json 的整数路径用 saturating_add；大计数/异常数据下的差异未实测，也未改代码。
- TaskGraph.tasks 的增量节点经 apply_immutable 追加到 IndexMap 末尾；我只按代码与 kiana-chord 的 apply 文档描述，未在测试中专门验证长期运行后的迭代序。
- phase_of 在 checkpoint 缺失 phase 或 phase 非字符串时返回空串，而 TS 的 phaseOf 会得到 undefined（checkpoint 为 null 时还会抛错）；该 Rust 兜底没有对应测试覆盖。

### u190-kiana-durable-group

- 原 `Waiters::add` 文档称接收端在发送端被丢弃时“解析为 HarnessClosed”，与代码矛盾：tokio oneshot 发送端消失时外层 `.await` 返回 `oneshot::error::RecvError`，调用方分别把它映射为 `io_error("Harness is closed")`（/home/ctyun/code/kiana_rev/kiana/crates/kiana-durable/src/harness/scheduler.rs:1663-1665）与 `closed_error`（/home/ctyun/code/kiana_rev/kiana/crates/kiana-durable/src/harness/submissions.rs:405-417）。已改写该句，请审阅确认。
- `Waiters::keys` 的严格插入序（包括 shift_remove 后重新插入的 key 排到末尾）来自 indexmap 的公开契约；仓库内测试只覆盖单 key 与空表，未实测多 key 跨删除顺序。
- `optional_extension` / `OptionalExtension` 在 crate 内无调用方的结论基于全仓库 grep；其“对应 TS util.ts 公开导出”的归因来自 /home/ctyun/code/kiana_rev/kiana/spec/durable-harness.md §2.22。
- “同一等待项至多结算一次”由 resolve/reject_all/取消回调三条路径互斥地 `Option::take()` 推出；除文件内的 abort 竞态测试外，没有并发压力测试直接验证这一点。
- `reject_all` 分发的错误无法 downcast 回 `HarnessClosed` 等原始类型，是基于 `SharedError` 包装的结构性结论，未找到直接断言 downcast 失败的测试。

### u191-kiana-durable-group

- Cargo.toml 的 cloudflare feature 说明称开启后 images/cloudflare.rs 的占位实现返回 not supported 错误，但该文件没有任何 cfg(feature = "cloudflare")，入口在所有构建下都存在并恒返回同一个 Err；以源码为准写文档，未改 Cargo.toml。
- NodePhotonImagesOptions::idle_ms 的单位（从名称与默认 30_000 推测为毫秒）在可见源码与测试中既未使用也未断言，文档没有写单位。
- “photon-rs 的缩放/编码路径是否真的会 panic”未验证；prepare_with_photon 的文档只说明 catch_unwind 是兜底、并写出 AssertUnwindSafe 的前提，未断言 panic 的具体来源。
- 文件级文档中“image 0.25 的 PNG 编码器与 image 0.24 输出逐字节相同”为原有断言，本次未复核；workspace Cargo.toml 只确认 image=0.25.10、image024=0.24.9、photon-rs=0.3.3、jpeg-decoder=0.3.1。
- 按任务要求未运行 cargo check/doc/fmt，新增 intra-doc 链接仅按可见性静态核对（全部指向 pub 项）。node.rs 原有指向私有且受 images feature 门控的 [`decode_image`] 两处已改为行内代码，[`exif_orientation`] 改为全限定 `crate::images::exif::exif_orientation`，以避免 private_intra_doc_links 或关闭 feature 时断链。

### u192-kiana-durable-group

- forks.rs 在 source.history 为 None 时会把副本 history 写成 Rewindable。MemoryStorage 的 document.copy 展开要求 record.history 与源记录逐字相等（crates/kiana-durable/src/storage/memory.rs 的 resolve 步骤），若正常 define_doc 路径真能产生 conversation 记录 history=None，该复制会在提交时被拒；本次未在定义校验中找到 history 必须为 Some 的强制点。
- mint_id 在判重与错误返回之前调用；MemoryStorage 的 mint_id 直接自增全局计数器，因此一次因不可见条目或地址重复而失败的 fork 仍会消耗已铸出的 ID。其他 Storage 后端是否同样表现未核实。
- 关于 DocumentCopy 在提交时按 source.at 读取源值的描述，依据是 crates/kiana-durable/src/storage/memory.rs 的实现；jsonl/sqlite 后端的同类路径未阅读，文档其余部分按 Storage trait 的公开契约表述。
- SCAN_PAGE_SIZE 的“每页最多请求多少条”依据 scan_documents 的 limit 形参；MemoryStorage 的循环会在 limit 处多收集一条再由 page() 切分，各后端的分页细节未逐一验证，注释未对页大小做更强保证。

### u193-kiana-durable-group

- Rust 的 IndexedDbStorageOptions（database_name/database_version）与 TS 类型不一致：TS 的 IndexedDbStorageOptions 只有 indexedDB?: IDBFactory，数据库名是 open(name, options) 的独立参数（pi/packages/durable/src/storage/indexeddb.ts）；未改代码，只在类型文档中标注，请确认是否是有意偏差。
- “本仓库没有调用点”的判断基于当前对全部 *.rs 的 grep（scan_start_json、next_cursor_value/NextCursor、IndexedDbStorage::options/Default）；后续新增调用会让该说法过期。
- storage/sqlite/cloudflare.rs 的模块文档仍称 feature cloudflare 关闭时只提供类型，但 storage/sqlite/mod.rs 无条件 pub mod cloudflare 且该文件无 cfg；不在本单元允许修改的文件内，未修改。
- wasm feature 在 kiana-durable 源码中没有被任何 cfg 使用；cloudflare feature 仅被 timers.rs 的 cfg! 读取（grep 结果）。feature 声明是否有意保留请主代理确认。
- 按任务要求未运行 cargo doc/fmt/check；intra-doc 链接有效性仅人工核对，链接目标均为 pub 项（crate::types::Storage、scan::scan_start、MemoryStorage 等），建议合批时用 RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-durable --no-deps 复验。

### u194-kiana-durable-group

- 未运行 cargo doc/check（任务禁止构建），node.rs 中 [`JsonlStorage`]、[`JsonlStorage::open`]、[`JsonlStorageOptions`] 等 intra-doc 链接仅按作用域推断可解析，未经 rustdoc 实测。
- 「同一目录被多个 JsonlStorage 同时打开」的语义只来自 storage.rs 模块文档的『没有实现跨 I/O 的提交串行化』；没有检查是否存在 OS 级文件锁或 FileSystem 实现内部的互斥。
- process.cwd() 读取失败时 cwd 退化为空串的路径无法在测试中注入，文档只描述了 resolve_path 对绝对/相对路径的两条分支（env/node.rs:378-382），未实测 current_dir() 失败时的最终结果。
- 『读取走内存镜像、运行期读不访问磁盘』取自 storage.rs 模块文档与 store() 的集中入口，未逐个审计 Storage trait 的全部读方法。

### u195-kiana-durable-group

- SQLite 将 CREATE 语句原文存入 `sqlite_schema.sql` 的“逐字”程度沿用仓库既有注释与 `tests/sqlite_migrations.rs` 的 golden 说明，本次未独立用 sqlite3 验证保存粒度。
- `DROP TABLE` 会连带删除表上索引属于 SQLite 语义；仓库测试只断言迁移后五个索引存在，未直接断言中间状态，注释按标准行为描述。

### u196-kiana-durable-group

- mod.rs 原 //! 中的 [`PanicAssertions`]/[`PanicExpect`] 链接位于 crate::testing 作用域，而这两个项只在 assertions 子模块公开、未在 mod.rs 重导出；据此改为 assertions::PanicAssertions/assertions::PanicExpect，但按任务约束未运行 cargo doc 验证。
- assertions.rs 既有注释称 `ok`/`greaterThan` 的 actual 不参与 toBeGreaterThan 比较；若用 create_expect_assertions(Arc::new(PanicExpect))，PanicExpectResult::to_be_greater_than 实际会读取 actual，该注释措辞与这条路径不完全一致，未改动。
- storage_benchmark.rs 的 document_count 把缺失或非数字 count 都映射为 NaN；TS 的 Number(value.count) 对 null 返回 0、对数字字符串返回数值。种子数据只产生数字或缺失，基准行为不受影响，但一般语义存在差异，未改动代码。
- “用例可以交给多线程 runner”只是由 Send + Sync 约束允许；仓库内可见的 runner（tests/storage_conformance.rs 的 CollectingRunner）是同步收集器，未见到实际并发执行实现，注释未声称并发保证。
- seed_storage_benchmark_with_scale 在非空/已用过的 storage 上的行为（ID 冲突、根会话重复）没有测试或文档依据；注释只说明了写入顺序与错误上抛，没有承诺空 storage 前置条件。

### u197-kiana-durable-group

- 未运行 cargo doc（任务禁止），//! 中的 intra-doc link 只按全限定路径静态核对；因 mod.rs 对子模块已有外层 ///，为避免 rustdoc 父作用域解析陷阱，//! 链接统一写成 crate::... 全限定形式，实际 rustdoc 结果未实测。
- details.loaded 在本仓库内没有读取方；文档按 kiana-durable/src/harness/types.rs 中 ToolExecutionResult.details 的字段说明概括为“给 UI 的结构化结果”，外部 UI 的实际消费方式未核实。
- 空查询与非法 limit 两条错误路径在 crates/kiana-durable/tests/tool_search.rs 中没有直接测试；注释依据是 execute_search 代码本身与 harness/tool.rs 的 Err -> is_error 折算约定。
- kiana-builtin-extensions 中的另一份 tool_search 只核对了 crate 归属（kiana-ext-api/kiana-agent-core，不依赖 kiana-durable）与文件头；其搜索文本、输出格式与本实现是否逐字一致未逐行比对。
- limit 的 schema 声明为 number、执行期要求正整数（TS 对齐），但未找到覆盖小数被拒绝的测试；超大数值经 as u64/as usize 的转换边界也未实测，注释未对此下结论。

### u198-kiana-durable-group

- path_utils 在本 crate 内没有直接单元测试或 fixture（read 的测试也未覆盖 @ 前缀与 Unicode 空白）；其行为依据 spec/durable-rest.md §2.25.3 与源码，未实测。
- file_mutation_queue 的 FIFO 顺序依赖 tokio::sync::Mutex 的公平性（tokio 文档保证，本仓库测试只验证串行化与取消）；future 在持锁/等待期间被丢弃时会在全局表中残留一个失效 Weak 表项，直到同 key 再次被调用——该结论由源码结构推断，未用测试复现。
- bash 在 timeout=None 且环境返回 Timeout 时会生成 `Command timed out after undefined seconds`，该分支没有测试覆盖（代码中按 TS 的 `${options?.timeout}` 实现）。
- image 的异步 detect_supported_image_mime_type_of 与 APNG 分块读取没有直接单元测试，仅通过 read 集成路径及同步参考用例间接覆盖；`32 字节足够覆盖 BMP 读取` 依据 is_bmp 的最大读取偏移 29。
- ImageLimits::max_bytes 计的是 base64 文本长度（不是原始字节数），依据 images/node.rs 的字段文档与 read.rs 中 base64_length 的比较，未找到独立测试断言。
- ShellStructuredOutput 与 EditToolDetails 只是手工构造 JSON 的 Rust 视图，字段与 JSON 键的对应关系没有编译期校验，只能靠人工同步。

### u199-kiana-durable-group

- image_model 把模型目录的 max_width/max_height（u64）用 `as u32` 转成 ImageLimits 字段；若目录声明超过 u32::MAX 会被静默截断。未修改代码，仅记录。
- read_image 以 `info.size as usize` 一次性读取整图；32 位目标上超大文件存在 usize 截断风险，本次只在 64 位环境做源码审阅，未验证。
- 除 NodeExecutionEnv 外，其他 ExecutionEnv 实现的 write_file 是否也递归创建父目录未核对；write 工具自身不做该处理（模块文档已把该行为限定为 Node 实现）。
- 未运行 cargo check / cargo doc / cargo fmt --check（任务禁止）；rustdoc intra-doc 链接是按源码可见性人工核对的，需主代理用 `RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-durable --no-deps` 复核。

### u200-kiana-durable-root

- ARCHITECTURE.md §4.2 称 kiana-durable 必须跑在 tokio::task::LocalSet 上且公开 API 的 future 不要求 Send；但本 crate 的 trait 使用 #[async_trait] 默认生成 Send future（全 crate 未见 ?Send）。我没有把这条架构约束写进 lib.rs，建议维护者确认其现状与表述。
- Cargo.toml 把 node feature 注释为“原生实现：kiana-tui 同款后端选择”，但全 src 没有任何 cfg(feature = "node")；lib.rs 按实测写成“未被 cfg 读取”，是否为待实现项需确认。
- wasm feature 同样没有 cfg 读取（storage/indexeddb.rs 的模块文档已说明），其未来用途未核实。
- 未逐条核对 ARCHITECTURE.md 的 84 文件 .rs↔.ts 对照表；仅确认 src 下 .rs 数量为 84 且抽查的模块头一致。
- 任务禁止运行 cargo 命令，未执行 cargo doc/check；crate 根 //! 中的 intra-doc 链接是按“公开且可达路径”人工核对的，建议主代理统一跑 RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-durable 复验。

### u201-kiana-env-large

- watch 只检查 Control::aborted；若客户端对 watch 发送 { mode: "kill" }，main.rs 的 Control::cancel(true) 只设置 killed，本模块不读取该字段，主循环不会因此退出。协议文档只笼统写 watch 取消后以 {} 结束，未单列 kill 语义，是否有意待核实。
- on_event 的错误分支直接用 notify 返回的路径执行 installed.remove，未经过 watched_spelling；若平台在错误事件里返回规范路径（如 macOS /private/var），键可能匹配不上而留下旧 installed 条目。notify 错误路径的实际写法未在本仓库/环境中验证。
- 首次 sync 中若 native watcher 安装失败，switch_to_polling 会在 ready 之前发出 overflow 事件。该顺序直接来自代码，但客户端是否按此顺序处理，仓库内无客户端实现可核对。
- HASH_RECENT = 5s 的取值依据在源码与 spec/durable-rest.md §2.24 中没有展开，只能确认它用于对“最近修改且较小”的文件做内容哈希以弥补 mtime 精度；为何是 5 秒待核实。
- maxDirectories 与 pollIntervalMs 未做 0/上限校验：maxDirectories=0 会让第一个被计数的目录立刻超预算，pollIntervalMs=0 会形成无间隔轮询。协议文档未约定这些边界值的语义，调用方预期待核实。

### u202-kiana-env-group

- decode.rs：decode_into 的容量计算与“一次调用消费全部输入”的说明依据 encoding_rs API 语义和 debug_assert_eq!(read, bytes.len())，仓库内没有针对极端输入的独立测试。
- errors.rs：io_between 注释中“path 字段只记录源路径（与 Node 一致）”来自移植来源与测试断言（tests 中 path == '/a'），未对照 Node 实机输出逐版本核实。
- exec.rs：timeoutMs 负数、NaN 或过大值会使 Duration::from_secs_f64 panic 的说明来自 Rust 标准库文档；仓库内无对应测试，协议把超时校验放在客户端。
- exec.rs：无窗口时“4 MiB 反压是全局的”基于 Output::wait_for_room 对 bulk_bytes 的统计（所有 bulk 帧共享该计数），未做并发压力测试验证。
- exec.rs：spill 失败后立即杀命令的注释动机（“避免继续产生无法留存的输出”）是对代码行为的解释，docs/semantics.md 未明确写出该动机。
- frame.rs：把 0..=3 字节后 EOF 解释为“客户端正常断开”依据 main.rs 的调用方式与单元测试；docs/protocol.md 未逐字规定该情形。

### u203-kiana-env-group

- mkdir_recursive 文档中“std::fs::create_dir_all 在父级是文件时会报 EEXIST”的对比，依据标准库语义与现有测试反推，本单元未直接运行 create_dir_all 做对照；若要作为规范结论，建议在目标平台复核。
- Windows 上隐藏属性过滤与 OsStr 排序细则由 ignore crate 提供，仅依据其 API、既有英文注释和 docs/protocol.md 描述，未在 Windows 环境实测。
- dir_entry 的 raw 字段供客户端按原始字节排序这一用途引用自 docs/protocol.md；本单元未检查客户端侧实现。
- output.rs 中“多个发送方各自入队后才等待，未写出总量可能短暂超过 BULK_LIMIT”是基于入队与等待顺序的推断，未做压力测试。
- 按任务约束未运行 cargo doc，新增 intra-doc link 未经编译器校验；链接目标均为同文件/同模块的 pub 项，已人工核对。

### u204-kiana-env-group

- scan.rs：begin 注释中“行首可作为独立 WHATWG 解码重启点”（0x0A 不属于任何多字节序列）是根据 UTF-8/WHATWG 规则推导的；仓库内没有“非法序列紧邻换行”的专门测试，建议后续用对拍确认。
- window.rs：“一次 take 可能交付接近 4 倍窗口”由 push 的 trim 阈值和 exec 的 take 时机推导，测试只锁定跳过后保留尾部仍是原文后缀，未断言最大交付量。
- window.rs：“调用方只关心最近 maxBytes 字节/maxLines 行的尾部”来自模块原有说明与 Durable 的 window 语义；daemon 内没有客户端代码可进一步核实其保留策略。
- pty.rs：读取线程对 Output::wait_for_room 的反压效果（“最多再多一块数据”的上界）是基于 Output 实现与调用顺序的推断，没有专门测试锁定严格上界。
- pty.rs：Terminal.input 使用无界 mpsc::channel，注释据此说明 ptyWrite 不施加反压；仓库内没有文档解释选择无界队列的设计原因，因此未写动机。
- pty.rs：EXIT_GRACE 取 100 ms 的依据只见于协议/semantics 文档的“100 ms 无输出后结束收集”，源码中没有更细的取值说明。
- scan.rs：firstLineBytes/lastLineStart 在客户端的具体用途未在 daemon 源码中出现；注释只描述 daemon 侧数值语义，客户端如何使用待核实。
- pty.rs：process_name 对 Apple/Linux/Android 之外的 Unix 返回 None，仅由 cfg 分支保证可编译；这些平台的 ptyForeground.name 行为没有运行测试覆盖。

### u205-kiana-env-group

- unix.rs 原模块注释把 pty 与 /proc 读取列为该文件覆盖范围，但当前仓库中这些代码位于 pty.rs；已在模块头加注说明，未删除原句。TS 参照源码不在仓库内，无法核实原 sys/unix.rs 是否确实包含这些职责。
- unix.rs 测试注释原写“The libuv table ... 26 codes”，但常量数组长度、match 分支及 spec §2.2.9 列举均为 25 项；已把该注释改为 25 并说明依据。
- windows.rs 的 resolve_program 英文概述称“带目录的名字按原样尝试”，而实现会在有扩展名时尝试原路径并始终追加 .com/.exe（无扩展名时只尝试这两个后缀）；中文按代码写明，未改英文原句。
- spec §2.2.9 举例称 macOS 上 ENOTEMPTY=35，该具体编号无法在仓库内核实，注释未采用具体数字，只说明编号因平台而异。
- Win32 行为（Job Object 退出清理、taskkill 不等待、CancelSynchronousIo 打断阻塞读）依据现有注释与 spec §2.2.11 描述，未在 Windows 实机运行验证。
- 未运行 cargo fmt/check/doc/test（任务约束），因此 rustdoc 警告与测试结果未实测；已人工检查新增注释无反引号不配对、无代码围栏、无新增 intra-doc 链接。

### u206-kiana-env-root

- 静默判断使用 SystemTime 墙钟（非单调时钟）：时间前跳可能提前触发退出，回拨由 saturating_sub 抑制；此结论来自标准库语义与代码结构，未用测试验证。
- 同一请求 id 重复登记会覆盖 controls 中的旧控制块，respond 随后按 id 注销可能删到另一请求的登记；该结论由 HashMap 的 insert/remove 代码推得，未找到专门测试。
- WORKERS=16、16 MiB 线程栈、CONTROL_PAYLOAD=64 KiB 的设计动机没有设计文档，注释只描述机制与数值，未推断原因。
- main 中 process::exit(0) 的确切动机源码仅以英文注释提到“请求线程可能仍阻塞在 FIFO open”，未在文档说明；注释只陈述其终止进程并跳过析构的行为。
- 按任务约束未运行 cargo build/test/fmt/clippy 或 cargo doc，编译与 Rustdoc 警告未实测；仅用基线快照做了差异纯度检查。

### u207-kiana-env-client-large

- 窄竞态（未改代码）：read_stdout 解析出 RESULT 后、dispatch 取 pending 锁之前，若另一线程的 teardown 先把 live 置 false，dispatch 仍可能先 remove 并发送 Ok；teardown 随后的 drain 看不到该请求，于是 close()/连接丢失之后请求仍以成功结算。现有测试 close_between_the_liveness_check_and_the_pending_install_settles_the_request 只覆盖登记窗口，未覆盖此窗口。
- 健壮性（未改代码）：start 由 tokio::spawn 驱动，若任务 panic（例如 random_token 里 getrandom 失败触发 expect），finish_start 不会执行，State::starting 永远保持 Some，之后所有 connect 都会排队等待同一个不会再来的结果且没有超时。
- frame() 把 length 与 body.len() 直接 as u32 写入帧头，既没有 MAX_FRAME 检查也没有 u32 溢出检查；当前调用方（terminal 1 MiB、remote_env 512 KiB）都会分块，但函数本身不设防。已在注释中说明发送侧不校验。
- next_id 使用 wrapping_add：u32 回绕后可能与仍在途的旧请求 id 重复（需约 2^32 个请求）；协议帧 id 本身就是 u32，代码与注释都未处理冲突。
- 既有 MAX_FRAME 注释写“daemon 与客户端都拒绝更大的帧”，实际客户端只在 read_stdout 校验入站帧，发送侧 frame() 不检查；我保留了原句并追加接收/发送的区别说明，未改写原注释。
- 既有 //! 与常量注释引用 docs/protocol.md，仓库中的实际路径是 crates/kiana-env/docs/protocol.md；按要求只记录，未修改原文字。

### u208-kiana-env-client-large

- 仓库内没有 packages/env 的 TS 源，凡涉及 TS 对齐的表述均只依据代码结构与文件里已有的 TS 注释，未逐条核对。
- create_temp_file 在 remote_path 失败、file_path 仍为空串时，最终 FileError.path 会是 Some("")（空串）而不是 None；这是从代码推出的边界，未见测试覆盖。
- RemoteTextLineReader 的锁在 pread().await 前释放，因此并发调用 read_line 可能重复读取同一段字节；这是从锁作用域分析得出，trait 未声明并发语义，也无测试。
- read_file 的已知大小解码分支会一次排最多 READ_DEPTH 个 pread，可能读过 size；多余请求的回复随 future 被丢弃，这一点没有测试直接锁定。
- append 的写入过程中不检查 abort（signal 为 None），中止在 file_op 返回后按 AbortCheck::Both 上报；是否为 TS 的原始行为未核实。
- list_dir_impl 对条目错误（含 ENOENT）一律失败，而 RemoteDirReader::next 会跳过 ENOENT；两者差异是否有意对齐 TS 未核实。
- 注释中的 daemon 行为（mkdtemp 追加六个随机字符、writeChunk 失败后粘性 EBADF、pread 无 offset 推进文件位置、open mode=read 返回 statError、readdir 的 raw 字节）依据 crates/kiana-env/src/main.rs 与 fs.rs，属于本文件之外且可能随 daemon 变化。
- 未运行 cargo build/test/fmt/clippy 或 rustdoc，因此新增文档的 intra-doc 链接（尤其是 [Shell::exec]）与 rustfmt 排版未实测。

### u209-kiana-env-client-large

- 仓库没有基线提交或快照（工作区全部未跟踪，也没有 tools/check_comment_only_diff.py），无法用脚本机器比对 diff 纯度；本次所有 Edit 均逐字保留 old_string 中的代码行，只插入/扩写注释行与空行。
- 任务禁止运行 cargo build/test/fmt/clippy，因此未机器验证 rustdoc 与 rustfmt；已人工核对公开文档不链接私有项（deploy_daemon 文档中的 check_daemon 已改成行内代码）。
- BatchMode / SendEnv=-* / ControlMaster 等 ssh 选项的语义取自选项名与 TS 源注释（/home/ctyun/code/kiana_rev/pi/packages/env/src/ssh.ts），本仓库测试未逐项验证这些外部 ssh 行为。
- 既有注释 node_fs_error 称消息「原样放进 RemoteError.message」；实际路径是 SshFailure → kiana_durable::env::BoxError → RemoteError::spawn_error，措辞略宽松，按不改写规则保留并在报告中记录。
- check_daemon 的 POSIX 分支在远端完全无输出时返回空串而不是 "missing"（与 TS 的 .at(-1) ?? 行为一致），注释已按实际行为描述；这是否是 TS 的疏漏无法确认。
- write_temp 在 async 上下文中使用同步 std::fs I/O 写小文件（known-hosts 临时文件），只记录未修改；是否有意为之无法从源码确认。
- Windows/PowerShell 分支（-EncodedCommand、Get-FileHash、Move-Item 20×250ms 重试、base64 折行）未在 Windows 上实测，注释只描述代码中可见的字符串与流程。

### u210-kiana-env-client-group

- watch.rs：`RemoteWatcher::close` 的既有文档（以及 `FileWatcher` trait 契约）说“返回后不再有 on_change 调用”。实现里 `deliver` 先读 `closed` 再执行回调，而 `close` 从另一线程置位并等待请求结算；存在“已通过检查、尚未执行”的回调在 close 返回后才开始的窗口。我保留了原文未改写，需确认是否接受这种与 TS 单线程语义近似的保证。
- list_files.rs：`max_entries`/`max_bytes` 的上界比较写作 `value > MAX_SAFE_INTEGER as usize`。若目标为 32 位，f64→usize 饱和会把上界变成 usize::MAX，比较可能恒假（TS 会拒绝超出 2^53−1 的值）。本次未在文档中断言这一点，也未修改代码，待核实构建目标。
- index.rs 的 public_total=34 是按 34 条 `pub use` 再导出符号人工核对 spec §2.1.1/§5.2 得到的；若统计工具只计本文件内 `pub` 定义而不计再导出，该文件的计数口径会不同。
- list_files.rs：模块与 LocalExecutionEnv 文档把“connection 必须连到本机 daemon”写成不变量，依据是字段注释（`Connection::new(vec![local_daemon()])`）与 list_files_over 的直连语义；代码没有运行时校验，连到远端时只会表现为列出的路径与本地操作不一致。

### u211-kiana-env-client-root

- `serde_json` 会把 f64 的 `300.0` 序列化成 `300.0`：沿用原注释的说法，未运行代码验证（任务禁止 build/test）。
- 非 Unix 分支（uv_err_name/uv_error_text/spawn_error_is_asynchronous 的 cfg(not(unix))）在仓库内没有测试（相关测试均 #[cfg(unix)]），文档已注明“只保证编译”。
- `child.id()` 在子进程被 wait 回收后返回 None 属于 tokio API 契约，未在本仓库测试中验证。
- `js_number` 对绝对值 ≥ 2^53 的整数值会走 f64 的 serde_json 序列化，理论上可能输出指数形式（与 JSON.stringify 的定点输出不同）；未实测，当前调用点（毫秒/速率）通常在该范围以下。
- `js_string_of` 对数组/对象返回 serde_json 文本，与 JS 的 String() 不同；已在注释中标明，仓库内无测试覆盖这一分支。
- 原 `format_js_number` 注释称调用点只有 `timeout:{seconds}` 与事件里的 `String(message)`，实际还有 `remote_env::timeout_text` 与 `ssh::timeout_seconds_text`（都是超时秒数的同语义用法）。原句未改，仅记录。

### u212-kiana-evals-large

- cli.rs 的 write_file_node 在 write_all 后没有 flush，而 report.rs 的 persist_session 显式 flush 并注释说明 tokio fs 的写操作委托给 blocking 线程、flush 才等待完成；main.rs 又用 std::process::exit 退出（不等待运行时析构）。最后一次写是否可能在进程退出前尚未落盘，需要实测确认（我未运行任何 cargo 命令）。
- Cargo.toml 声明了 glob 依赖，但在 crates/kiana-evals 的 src 与 tests 中未找到 glob:: 调用；本文件的默认发现是手写递归。该依赖是否为遗留项未核实。
- 原注释『kiana-json 不在本 crate 的依赖白名单里』与 Cargo.toml（kiana-json 依赖及其说明）以及本文件对 kiana_json::parse 的调用矛盾；已改写为『本文件自带实现，未直接调用 kiana_json::parse_js_number』。为何存在本地重复实现（kiana-json 根部实际 pub use 了 parse_js_number）未确认。
- 未运行 cargo check / cargo doc / cargo fmt（任务禁止），intra-doc 链接只做了人工核对：均为公开项（run、run_with、CliError、crate::docker/plan/report）或在私有文档内。
- Node fs.globSync 的具体行为条目（follow:false、dot:false、不可读目录静默跳过）沿用文件原有注释与 spec/env-evals-durable-testing.md；我未在 Node 实机上复核。
- complex_blocks_explained 按新增的 // 说明块计数（parse_eval_cli 7、run_with 8、测试模块头 1）；若统计口径不同需调整。

### u213-kiana-evals-large

- typescript_repository_root 的英文原注释称默认是 "sibling `kiana` checkout"，但代码实际计算为 `package_root()/../../kiana`（本仓库中即 `<Rust 工作区根>/kiana`，该目录当前不存在）。我保留了英文原句并在下方补充精确计算，未核实预期的 TS 仓库部署位置。
- ENOEXEC 回退中若 `/bin/sh` 自身启动失败，现有实现按 `spawnSync /bin/sh <ERRNO>` 报错；Node/libuv 在该场景是报原命令还是 `/bin/sh` 未在 TS 源码或测试中核实，因此没有写成注释断言。
- create_dir_all_mode 的 0o700 是否会被宿主 umask 削减只依据 `create_dir_once` 的既有注释与 std 行为；测试没有控制 umask，未实测。
- `repository_root()` 在 crate 内没有调用点（grep 验证），但作为 pub 函数是否被仓库外调用无法确认（Cargo.toml 中 publish = false）。

### u214-kiana-evals-large

- TS 源 packages/evals/src/harness.ts 不在本仓库，所有「TS 对应/逐字」结论只能沿用文件原有注释，无法在本次工作中核对。
- create_failed_harness_run 的 errors 用 serialize_error 固定写 {type,message}，而中途失败路径用 normalize_simple_error；因此早期文件系统错误（如 mkdtemp 的 ENOENT）合成的 run 会丢掉 errno/code/syscall/path。是否与 TS 的 createFailedHarnessRun 一致无法核实，未改代码。
- 沙箱分支会删除在 apply_isolated_environment 之前捕获的 host_agent_dir/auth.json，且 cleanup 只恢复环境变量、不恢复该文件；在宿主机上设置 KIANA_EVAL_SANDBOX_UID/GID 会删掉真实凭证文件（代码行为，已在注释中标注，未修改）。
- apply_isolated_environment 的 recorded 集合记录的是 KIANA_EVAL_ 前缀的键，而三个覆盖键（HOME/USERPROFILE/KIANA_CODING_AGENT_DIR）永不带该前缀，因此 recorded.contains 分支实际恒为 false；疑似移植残留，未改代码。
- 模块原有 //! 表格与正文用 [`complete_harness_run`] 链接私有函数，存在 rustdoc::private_intra_doc_links 告警风险；本次按 COMMENT_STYLE §2.6 改为行内代码（纯注释改动，已在 notes 说明）。
- 原有 ensure_run_trace 文档写「追加一条 trace」，但代码是 run.traces = Some(vec![...]) 的整体替换（会丢弃只有空 span 的旧 trace）；本次按 §2.4 改写为「写入/整体替换」，特此说明依据。
- 未运行 cargo fmt/build/test/clippy（任务禁止），因此 rustfmt 对新增注释的排版影响与 rustdoc 链接告警无法实测；已人工确认没有新增 doctest 代码块、没有把 /// 写进函数体。

### u215-kiana-evals-large

- difference 对 ±0 的归一：注释依据 ECMA-262（toPrecision 对零输出 "0.000…"，Number 还原为 +0）写成；仓库测试用 assert_eq! 无法区分 +0/-0，未在 Node 26 实测。
- comparison_flags 的 Flaky 聚合键只有 (case_id, variant)，不含 eval_set/model；仓库内没有 TS 源码，无法确认这是否与原实现逐字一致（仅依据函数标注为逐行移植）。
- reader::relative_path / display_report_source 在 Windows 不同盘符下与 Node path.relative 的差异未验证（当前为 linux，测试未覆盖）。
- optional_metadata_metric 没有显式 is_finite 检查，依赖 serde_json 的 Number 不能表示 NaN/±Infinity；未找到直接测试超大整数 as_f64 结果的用例。
- process_env 对非 UTF-8 环境变量做 lossy 转换，是否与 Node 的 process.env 完全一致未验证。
- JsNumber 在非 serde_json Serializer 或 serde Content 下的确切行为（原始文本丢失还是报错）未实测，公开文档采用了保守措辞“丢失或序列化失败”。
- 多个 Option 字段未使用 serde_option（如 VitestJsonAssertion.duration、HarnessRun.output、EvalScore.score、EvalMeta.avg_score/output），serde 会把显式 null 当作缺失；若 TS 侧是 .optional()（拒绝 null）而非 nullable，则存在保真差异。现有测试只覆盖 1e999 溢出，不覆盖显式 null。
- persist_session 的文件权限 mode(0o600) 只在 #[cfg(unix)] 下设置；非 unix 平台与 Node 写文件行为是否一致未验证。
- read_task_observation 用单空格拼接 eval_set/case_id 这一约束来自本文件代码与 tests/report.rs 的 fixture，未在 TS 源码中核对（仓库无 TS 源）。

### u216-kiana-evals-group

- create_task_plan 的既有注释称变体交替是为了「抵消缓存/顺序偏差」，但 TS 的 plan.ts 及其测试都没有给出这一动机；本次按要求保留原注释，动机本身未在仓库源码/测试中找到依据。
- `Vec::with_capacity(cases.len() * runs as usize * 2)` 的乘法在极端输入下可能溢出 usize（cli 允许 runs_per_variant 取到 2^53-1，此时 cases.len() >= 1024 左右即可溢出），可能 panic 于容量计算而非给出可读错误；按规则未改代码，也未实测。
- public_total=21 的计数不含 Display impl 与 PlanError 的匿名元组字段；若统计口径把它们算作公开项，数字会略高。
- 任务禁止 cargo build/test/fmt/clippy 与 cargo doc，因此本次未实际编译验证 rustdoc 链接和 rustfmt 排版；已人工确认所有 intra-doc 链接只指向本模块公开项。

### u217-kiana-evals-root

- docker build context 的默认路径与 KIANA_EVAL_TYPESCRIPT_REPOSITORY 覆盖逻辑来自 docker.rs 的既有模块文档（本次只读了模块头、run_task 及部分路径/错误辅助），未逐行核对 build_images 的路径解析实现。
- harness 改造/恢复进程环境变量（HOME/USERPROFILE/KIANA_CODING_AGENT_DIR）与“调用方需自行串行化”的说明来自 harness.rs 的既有 //!（本次只读了前 60 行与公开签名），未逐行核对 apply_isolated_environment 与收尾实现。
- blocked_pairs 序列化为 JSON 字段 blockedPairs 依据 report.rs 的 #[serde(rename_all = "camelCase")] 与 cli.rs 的 report.blocked_pairs.is_empty()；未运行程序验证实际产物 JSON。
- 阅读顺序按实际依赖编排（report 在 harness 之前，因为 harness.rs 文档称其复用 report 的 wire 类型），与 docs/UNDERSTANDING_GUIDE.md 列出的 main→cli→plan→docker→harness→report 顺序不同；这是编排选择而非源码断言。
- public_documented_after 的统计口径：lib.rs 只有 5 个 pub mod，规范禁止在 pub mod 上方新增外层 ///，此处按“各模块自身 //! 已文档”计为 5；若按外层 /// 严格计数则为 0。

### u218-kiana-evals-root

- Rustdoc 链接 [`kiana_evals::cli`] 与 [`kiana_evals::cli::CliError`] 未实际运行 cargo doc 校验（任务禁止构建）；二者与文件中原有的 [`kiana_evals::cli::run`] 写法一致，且都是公开项，但链接能否在 bin target 下解析未经实测。
- 「Builder::build() 失败源于运行时所需的 OS 资源无法初始化」取自本地 registry 中 tokio 1.54.1 的 API 文档源码，不是仓库内文件；仓库内只能确认返回的错误被 match 处理并以 1 退出。
- 「worker 线程数未显式设置（取 tokio 默认值）」只断言代码未调用 worker_threads；具体默认数量依赖 tokio 版本/环境（available_parallelism、TOKIO_WORKER_THREADS），未在仓库验证，因此文档刻意未写数字。
- 没有实际执行 kiana-evals 二进制：退出码、stderr 逐字内容与 stdout 为空的行为来自 tests/docker.rs 的 binary_reports_errors_on_stderr_with_exit_code_one 以及 cli.rs 源码阅读，而非进程级实测。
- 「cli::run 路径上的异步只有 tokio::fs 的文件 I/O」基于 src/ 的 grep 与 cli.rs/report.rs 的 .await 清单；harness.rs（不在主路径）也有异步文件 I/O，这句话按「cli::run 路径」限定，不覆盖 harness。

### u219-kiana-export-large

- 模块文档中既有的 `$` 替换字节数（template.js 1 处 `$$`、highlight.min.js 1 处 `$$` + 1 处 `$&`，多 12 字节）与 export_leaf_id 关于 `_buildIndex` 的描述属于原有注释，本次未重新实测或对照 TS 源码。
- 任务禁止运行 cargo 构建类命令，因此未能实际执行 cargo doc 验证 `private_intra_doc_links`；已把 3 个指向私有项的链接改为行内代码（js_replace_first、session_data_json_string、node_session_read_error），其余链接均指向本模块公开项、std 公开项或直接依赖的公开项。
- 既有跨 crate 链接 [`SessionManager::get_header_json`]、[`SessionManager::get_entries_json`]、[`kiana_json::js_object::stringify`] 指向直接依赖的公开项；按 COMMENT_STYLE §2.6 更宜写成行内代码，本次为不改动既有文案而保留，建议主代理复核。
- export_from_file 文档中“SessionManager::open 可能迁移并重写输入文件”来自 kiana-session-core 对该方法的文档说明，未在导出调用路径上单独写测试验证。
- Path::new(...).exists() 在父目录不可读等错误下的具体返回值（std 的 exists 对任何元数据错误返回 false）未在仓库测试中覆盖，因此注释未对此作断言。

### u220-kiana-export-group

- assets.rs 原有注释引用“ARCHITECTURE.md §5”，但当前 ARCHITECTURE.md 中 grep 不到 include_str! 相关内容（该既有注释未改动，引用可能已过时）。
- DefaultToolHtmlRenderer 在 render_call/render_result 中先 remove 旧组件再调用渲染器：渲染器 panic 时旧组件不会回到映射，而 TS 的 Map 会保留旧值，导致同一 toolCallId 下一次的 lastComponent 在 Rust 侧为 None。此差异已在注释中写明，需维护者确认是否有意为之。
- js_number_to_string/integral_to_hex 只在当前调用点（非负整数值）下与 JS 语义等价；若未来新增小数或负数调用点，指数写法与 toString(16) 的小数处理会偏离 JS（注释已注明假设）。
- 未运行 cargo doc/check/fmt（任务禁止），Rustdoc 链接仅按可见性人工核对；cargo doc 的实际警告状态未验证。
- “公开项”按顶层 public 项计数（ansi_to_html 2、assets 6、tool_renderer 6）；若把公开字段与 trait 方法也算入，tool_renderer 为 14 项，且全部已有文档。

### u221-kiana-export-root

- 未运行 cargo doc（任务禁止运行构建工具），因此新增 intra-doc 链接未经 rustdoc 实测；其中 [`ansi_to_html`] 同时存在同名模块与根重导出函数，沿用文件原有写法，若 rustdoc 报歧义需复核。
- crate 文档把 tests/ 下的 XSS、CJK、包覆盖等测试按文件名归类，仅逐字读过 assets.rs 与 export_html_golden.rs 的模块头，其余测试未逐个阅读断言。
- 「模板里没有 <link>/<script src> 一类外部引用」是对 template.html/css/js 做文本 grep（<link、script src=、url(、http(s)://）的结果；未审计 vendor bundle 内部是否存在运行期网络调用，文档也未作该断言。
- public_total 按本文件声明计：4 个 pub mod + 17 个 pub use 重导出 = 21；重导出项本身没有独立 ///，其文档来自定义处与根 //! 的公开面/核心概念说明，若统计工具只计算带 /// 的条目则分子口径可能不同。

### u222-kiana-ext-api-large

- `SessionBeforeCompactEvent`/`SessionBeforeTreeEvent` 的 `signal` 在 JS handler 侧是否通过 `ctx.signal` 收到同一 token：本次在 dispatch 中只确认事件 JSON 不含 signal，未找到针对该事件的 signal 注入证据，文档只陈述了“不参与 serde、反序列化得到未取消 token”。
- 仓库内 `OptionalPatch` 的唯一生产消费者是 `kiana-session` 的 `after_tool_call`；未来若新增消费者，`Null` 是否仍应按 `??` 视为“未提供”无法从当前源码确认。
- 公开项计数 179 = 138 个手写声明 + 41 个由 event_type_enum! 生成的 *EventType 单变体枚举；不含 `pub use kiana_config::...` 再导出（其文档来自目标类型）。
- 模块文档提到 crate 根 barrel 只再导出 TS types.ts 的 186 个名字，该数字取自 index.rs 的模块注释，未逐项核对 TS 源。

### u223-kiana-ext-api-large

- ExtensionContextApi::signal 的“非流式时为 None”未在实现中核实（原注释保留）。
- ExtensionVirtualModel 的默认值（thinking_levels 缺省 ["off"]、input 缺省 text+image、context_window/max_tokens 未设为 0）未在 kiana-session/模型运行时中核实（原注释保留）。
- ExtensionApi::set_model 的“provider 未配置认证时返回 false”与 set_active_tools 的“未知与 hidden 忽略”未在实现中核实（原注释保留）。
- ProviderConfig::api_key（$ENV 插值 / 前导 !command）与 auth_header 的请求头注入行为未核实（原注释保留）。
- ProviderChatModelConfig 等结构“字段顺序即 TS 声明顺序”依赖仓库外的 TS 源，未核实。
- ProviderModelConfig 当 type 键存在但值不是字符串时会落入 chat 分派，随后由 chat 结构体自身的反序列化报错；该边界无测试覆盖，未写入注释。
- MarkdownTransformContext.timestamp 的 ISO-8601 形态（3 位小数 + Z）为原注释，未核实。

### u224-kiana-ext-api-group

- 路径 /home/ctyun/kiana-work/upstream-reference 在本机不存在，无法直接核对 TS 的 core/exec.ts。关于 ExecOptions.cwd：仓库内 kiana-core/src/exec.rs 的模块文档称 TS 的 execCommand 从不读取 options.cwd；但 kiana-extensions/src/loader/mod.rs 的 ExtensionApi::exec 实现会读取该字段作为位置参数 cwd（缺省回落到扩展 cwd）。我据此后写了“Rust 扩展桥会读取、与 TS execCommand 忽略不同”，建议主代理对照上游确认这是否是有意偏离。
- ToolLoadout::get_prompt_guidelines 的旧注释“hidden declaration 的工具不贡献指引”只在最终系统提示构建层验证（build_system_prompt_sections 用 hidden_tools 过滤 selected_tools）；会话的 get_prompt_guidelines 本身只查快照、不做 hidden 过滤。我保留原句并补充了层级说明，若 TS 语义不同需再核对。
- ToolDefinition 的“20 个字段”、Skill 的命名/长度约束、BashOperations 的“信号终止按 128 + signal 报告”等均为保留的既有断言；其中 128+signal 已在 kiana-tools 的 LocalShellOperations 见到实现，另两项受限于上游 TS 源码缺失未能逐字复核。
- RenderCallFn 注释中“调用方可能缓存旧组件并在下一帧通过 last_component 交回”依据 kiana-interactive 的 CachedComponent/reclaim_cached_children 实现；其他渲染消费方（如 kiana-export）是否同样缓存未逐一核实，注释用的是“可能”。
- public_total 按文件内 pub 行统计（含字段；index.rs 按 pub use 语句计 9 条、对应 186 个名字）。若 doc_inventory 的统计口径不同，数字需要重算。

### u225-kiana-ext-api-group

- 既有的 WorkingIndicatorOptions 注释（`frames: []` 隐藏指示器、`interval_ms` 仅动画指示器使用）本次未在实现或测试中核对，保留原样。
- 既有的 ThemeInfo::path“内置主题没有路径”本次未逐项核对主题注册表（get_available_themes_with_paths），保留原样。
- 既有的 get_available_provider_count“有可用模型的 provider 数量”定义本次未在下游 footer 实现中核对。
- 既有的 footer-data-provider.ts:126/245/247/260/263 行号引用未对照 TS 源文件复核；detached HEAD 小写字面量已由 kiana-resources/src/footer_data_provider.rs 的 resolve_git_branch_* 佐证。
- 交互式 timeout 的“到期即返回默认值”由 ExtensionSelectorComponent / ExtensionInputComponent 的倒计时路径与 confirm 复用 selector 推断，未逐行验证计时器回调在 signal/response 同时到达时的优先级。
- TerminalInputResult 既有字段注释“不再传给默认处理”与 kiana-tui 的 consume 语义一致，但未逐一核对所有 TuiInputListener 消费方。

### u226-kiana-ext-api-root

- “186 个公开名字”只在 index.rs 注释与 tests/public_surface.rs 的模块头之间交叉核对，未从上游 TS 源码（仓库内无 packages/）重新清点导出表。
- “41 个事件名”以 events.rs 的 ExtensionEventKind::ALL（[_; 41]）与测试头注释为依据；“32 个顶层变体”由脚本统计 enum 得出，若事件增删需同步 Update 文档。
- 下游行为只读了代表性文件：kiana-extensions/src/runner/dispatch.rs（ts_event_json 重排的 5 类事件）与 kiana-session/src/agent_session.rs（emit 调用点），未穷尽所有 emit/构造点；描述按这些文件成立。
- 直接依赖列表来自 crates/*/Cargo.toml 的 kiana-ext-api 字符串匹配，未逐个检查这些 crate 是否实际使用；列表是“Cargo 依赖”而非“实际使用”的精确清单。

### u227-kiana-extensions-large

- 模块头既有的 [`native_cjs_interop`] 链接指向私有 fn；按 rustdoc::private_intra_doc_links 可能产生既存告警。任务禁止运行 cargo doc，未能验证。
- 既有注释 “Resolve a relative re-export target (Node probe order …)” 与实际探测顺序不等同：实际按固定扩展名表（含 TS/JSON）追加、再试 index.<ext>，且不读目录 package.json 的 main。按规则保留原文，只在下方补充实际顺序。
- 既有所有 “jiti 2.7 实测” 结论（两条 default interop 规则、CJS 分派、moduleCache:false）来自仓库原注释与 tests/cjs_loader.rs，本次未重新运行 jiti 验证。
- heuristic_has_esm 中 `&rest[..rest.len().min(512)]` 在解析失败且第 512 字节落在多字节字符中间时会 panic；按要求只记录、未改代码。
- is_cjs_module 的 has_esm 包含文本 `source.contains("import.meta")`，注释/字符串中的提及也会把 .cjs 判成 ESM；已在文档写明，是否为预期行为需确认。
- package_type_for 对“不可读的 package.json”与“不存在”不加区分、坏 JSON 静默返回 None；集成测试只覆盖存在/缺失/type:module 三种，坏 manifest 的对齐意图未被测试锁定。
- 具名导出顺序依赖 oxc_semantic 0.155 的 AstNodes::iter（已核对该版本为 NodeId 插入序、按访问序分配），但未运行测试验证未来版本是否保持。

### u228-kiana-extensions-large

- prelude_node_source()/intl_source() 的文档写“体积断言用”，但全仓库 grep 未找到调用方，实际用途未确认。
- pathToFileURLObject 宿主原语在 js/ 与 tests/ 中均无调用点，别名用途未确认。
- child::signal_name / signal_number 使用硬编码信号编号（未取 libc 常量）；非 Linux 的 Unix 平台编号是否一致未经测试验证，仓库内无相关断言。
- handle_listen_kind 中 listener.local_addr() 失败时只 reject、不移除 servers 表项（疑似登记泄漏）；仅记录，未改代码。
- handle_watch_kind 失败路径：start_watcher 会在返回 None 前同步调用 on_error（写入无人接收的通道），函数随后又写一条并 reject；所有失败原因都被固定成 ENOENT ... watch。行为已按源码注释，是否为预期设计未知。
- stdout 读错误（非 BrokenPipe）发出的事件类型是 stderrError，且 js/node/child_process.js 明确忽略它；命名/用途待核实。
- path_to_file_url 的兜底分支对整串路径用 NON_ALPHANUMERIC 编码（会把 / 编码），该分支是否可达/是否符合预期未确认。
- number_format_big_json 不应用 minimumIntegerDigits，而 f64 路径会应用；是否刻意保留该差异未确认。
- paths::set_module_resolver 原文档称 loader 会在装好虚拟模块后调用，但仓库内无调用方（grep 仅命中定义与 re-export）；是否由外部消费方或未来代码使用未知。
- 本地无法运行 cargo doc（任务禁止），rustdoc 警告未机器验证；已人工核对仅 4 处 intra-doc link，均指向本文件同模块可见项。

### u229-kiana-extensions-large

- `transpile::module_path` 无条件在第一个 `?` 处截断；`install_import_meta` 对非 data: 模块直接调用它，因此 Unix 文件名本身含 `?` 时可能被截断。未实测 Resolver 是否会返回这种名字，仅记录、未改代码。
- 本次未运行 cargo check/doc（任务禁止构建命令）。5 处 public 文档→私有项链接的清理与所有 rustdoc 链接均按源码可见性静态判断，建议主代理用 RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-extensions --no-deps 复核。
- `absolute_path` 原注释称 `std::path::absolute`『解析到 cwd 并归一化』；本机 Rust 1.99.1 std 文档说明 POSIX 下会保留 `..`（Node path.resolve 会归一化），已据此改写该注释并在报告中记录依据。
- 模块文档中关于 jiti 2.7 的行为描述全部沿用原有注释（review finding 引用），本次未重新对照 TS 源码验证。

### u230-kiana-extensions-large

- 既有注释段落「`loadExtensionModule` 的 Rust 形态……缓存命中时……返回 None 且不写缓存」实际位于 `struct LoadedFactory` 上方，描述的是 `load_extension_module`；遵守“不移动既有注释/条目”的约束未搬移，只给函数补了一份独立 ///，建议主代理裁定是否移动。
- 原 install 文档写“调用方在进程生命周期内持有同一个 Rc”，与 crates/kiana-extensions/src/lib.rs 中 GenerationInner 持有引擎 Rc、ExtensionHost::reload 先 drop 旧代的行为矛盾；据此改写为“本代存活期间持有同一个 Rc”，依据为 src/lib.rs 的 Generation::with_config / reload；未运行测试验证。
- 所有 TS 对应关系（loader.ts 行号、extensions-runner.test.ts 等）沿用仓库既有注释；本仓库未见 packages/ 目录，未独立核对 TS 源码。
- 任务禁止运行 cargo check / cargo doc，新增 intra-doc link（install、RuntimeShared、load_extensions*、live_extension、create_extension_runtime 等）仅人工确认均指向本模块 pub 项，未实际生成文档验证。
- `ensure_extension_local` 返回 None 时的后续后果未追踪，注释只声明“返回值在此不使用”。
- “handler 顺序即结果顺序”等既有断言保持原样，未新增验证。

### u231-kiana-extensions-large

- `is_relative` 只认 `./`、`../`、`.`，不含 `..`，而 `is_path_like` 含 `..`：从虚拟模块名导入裸 `".."` 时不会走虚拟命名空间的 fail-fast，而会按宿主机目录解析。是否有意为之没有测试或文档证据，未改动，仅记录。
- `require_stack_importer` 只按 `Path::is_absolute()` 判断导入方，`file:` URL 形式的 base 会被当成非文件路径，Require stack 显示加载器源码路径；而同文件的 `importer_dir` 会把 `file:` base 转成真实路径。两者不一致是否有意未确认。
- `legacy_main` 把 package.json 读取失败或 JSON 解析失败都静默折算为「无 main」；Node 的 require 遇到坏 package.json 会抛错。逐字兼容性未实测（仓库内无对应测试）。
- `build_oxc_resolver` 中 `TsconfigReferences::Disabled` 的具体语义（是否完全不跟随 project references）来自 `oxc_resolver` API 名称与构造调用，未查阅其文档或测试确认。
- 虚拟文件 `name` 重复会导致模块缓存出现两个条目、进而产生两份 typebox `Type` 对象的说法，依据是 virtual_modules 模块文档中「typebox 求值一次、共享同一模块缓存」的说明；本文件中没有直接测试证明该后果。
- 设计文档 §3.8 的目录解析顺序（TS 优先、相对目录使用 exports/module/main）与本实现（实测 jiti 2.7：`.js` 先于 `.ts`、index 先于 main、忽略 module、相对目录不用 exports）不一致；既有模块注释已声明以实测 jiti 为准，我未改动，仅在此记录。

### u232-kiana-extensions-large

- CallbackBridge::start 的原注释称 `__kiana_ctx` 未安装时返回 None，但代码没有 Ok(None) 分支（globals().get::<Object> 失败会返回 Err）；已按源码改写为“错误原样返回”，若 rquickjs 对缺失全局的行为与预期不同需复核。
- classify/generateImages 原注释写“永不 reject”；注册表实现确实不 reject（ModelRegistry::classify 返回 ClassifierResult），但桥层 parse_arg/resolve_json 失败仍会 reject，已改为限定表述，需确认是否与 TS 文档意图冲突。
- 流关闭行为（只调用 result() 而不迭代时不会触发 streamClose、表项保留到进程结束）来自 js/context.js 的静态阅读，未做运行时/内存实测。
- UI 句柄表与 signal 句柄表没有移除路径、会持续增长，是代码结构上的静态结论，未做泄漏实测。
- 未运行 cargo doc/check（任务禁止），新增的 intra-doc link（NewSessionOptions/ForkOptions/NavigateTreeOptions/SwitchSessionOptions/SendUserMessageOptions）按可见性判断可解析，最终以 cargo doc 为准。
- 仓库内未找到 tools/check_comment_only_diff.py，纯度检查未执行；所有编辑均为注释行/空行追加，代码行按 Edit 的 old/new 对照未变。

### u233-kiana-extensions-large

- 模块文档沿用了原有“Persistent 句柄物化未实现（设计文档 §4.6）”的结论；我确认了 loader 适配器确实按 JSON 物化事件，但没有查阅 spec/design-extension-runtime.md §4.6 原文。
- ts_event_json 原有注释称除 ToolCall/ToolResult/TurnEnd/AgentBeforeSettle/BeforeAgentStart/ProviderStreamEvent 外其它事件字段顺序已与 TS 一致；我没有逐个事件对照 TS 源，仅保留原注释。
- emit_before_agent_start 的覆盖栈 push/pop 不是 RAII、future 被丢弃时不会 pop：这是从代码结构直接推出的结论，测试未覆盖取消路径。
- install 失败后重复调用会因 has_host_kind 命中而返回 Ok(()) 且不补装 dispatch.js：依据代码中 host kind 先于 eval_script 注册的顺序，未见对应测试。
- DispatchError::message 的“进入 UI”沿用原注释；我确认了 project_trust 的 ExtensionError.error 与 kiana-session 的 before_tool_call 两类消费，未穷举所有 UI 路径。

### u234-kiana-extensions-large

- `RunnerUiPromptHost` 经 `spawn_dispatch_event` 使用 `tokio::task::spawn_local`，而默认 `DeferredScheduler` 的排空任务由 `Handle::spawn` 启动、不一定处于 `LocalSet` 上下文；若 UI prompt 事件从该任务发出，tokio 可能 panic。未实测（测试使用注入的 host/调度器），已在 `spawn_dispatch_event` 文档中只陈述“不检查 LocalSet”的事实。
- `RunnerCore.ui_prompt` 字段在全 crate 内只写不读（grep 确认），已在 `set_ui_context` / `get_ui_context` 文档中如实标注“只写不读”；它是否为与 TS 字段对齐的残留、是否有计划中的读取方，未核实。
- `get_active_tools` 在 stale 后 panic 由源码直接可见（`assert_active().unwrap_or_else(panic!)`），但未找到覆盖该路径的测试（extensions_runner.rs 只测了 assert_active 本身）。
- `ExtensionRunner` / `RunnerCore` 的 `Send + Sync` 由字段类型与 kiana-session 的 `Arc<Mutex<Option<Arc<ExtensionRunner>>>>` 用法推断；本单元未加编译期断言，也未运行 cargo check 验证。
- `emit_boundary` 的 valid/entries/continue 语义依据 `runner/boundary.rs` 模块文档与 `dispatch::run_boundary_dispatch` 的实现，未逐行核对 `js/dispatch.js` 的 `emitBoundary`。
- 未运行 `cargo doc` / `cargo fmt --check`（任务禁止）；新增 intra-doc 链接理论上都指向本模块 pub 项、pub 子模块或外部 pub 类型（私有目标已改用行内代码），但未经 rustdoc 实际验证。
- `normalize_build_system_prompt_options` 的缺省值由测试 `normalizes_system_prompt_options_with_ts_defaults` 的断言锁定（测试已读）；与 TS `core/system-prompt.ts` 的逐字对应关系仅据文件内既有注释，未核对 TS 源。
- `spawn_dispatch_event` 对 `LocalSet` 上下文的要求来自 tokio 的公开约束，仓库内没有额外文档佐证；MCP 变更监听等调用点是否总在 LocalSet 内未逐一实测。

### u235-kiana-extensions-large

- get_expanded_text：方法存在但返回 undefined/null 时不会回退到 get_text（call_method 返回 Some(undefined)，js_string 得到字符串 "undefined"），与注释引用的 TS 表达式 `getExpandedText?.() ?? getText()` 语义不同；tests/js_component.rs 只覆盖了方法缺失的分支。已在注释中记录该偏差，未改代码。
- Component::dispose 未被三个适配器覆写：宿主对 Box<dyn Component> 调用 trait 的 dispose() 不会到达 JS；kiana-interactive 的 OwnedComponent（extension_ui/widgets.rs:236-239）会转发该 trait 方法，因此对 OwnedJsComponent 是空操作。当前常见路径由随后的 Drop 兜底，但若某处长期克隆持有 SharedComponent，JS dispose 会被推迟；未找到实际泄漏用例，未修改代码。
- js_truthy 对 BigInt 0n 返回 true（实现只检查 bool/number/string，其余落到 true），与 ECMAScript TO-BOOL 不同；依据是 adapter.rs 的分支与 rquickjs 的 as_bool/as_number/as_string 只覆盖原生类型，未用测试确认扩展会传 BigInt。
- with_engine_js 只取线程上 ACTIVE_CTX 的最后一个 Ctx，不校验它属于传入的 engine；同一线程存在多个引擎且发生跨引擎嵌套事务时会读到错误的引用表。tests/public_surface.rs 的多宿主测试是顺序执行的，未覆盖嵌套场景。
- load_ref 用 Option<Value> 判断属性缺失，而 rquickjs 0.14 的 FromJs<Option> 把 null 与 undefined 都视为空，因此存进 __kiana_refs 的 JS null 读出来是 undefined。该结论来自本地 cargo registry 的 rquickjs-core 源码，仓库内没有对应测试。
- set_on_submit/set_on_change 的 JS 闭包在调用宿主回调期间持有 RefCell::borrow_mut()；同一闭包在此期间被再次调用会 panic。仓库内未找到触发这种重入的调用路径，因此注释只陈述机制，不声称一定发生。
- JsEngineSlot::attach 返回位置序号（position+1），注销排在前面的引擎后序号会前移；当前仓库内所有调用方都忽略返回值（ui::install_attached），未来若有人持久化该 id 会失效。
- JsComponent::with_object 在错误没有对应挂起 JS 异常时（例如 __kiana_refs 未安装），上报文本会退化为字符串 "undefined"（依据 engine/promise.rs 的 fallback_error_from_value）；未构造测试验证该输出。
- 既有注释中的 TS 行号（interactive-mode.ts:2402-2458、tui.ts:672-680、box.ts:43-46、tui.ts:770-790 等）沿用原注释；仓库内没有 TS 源码，未逐条核对。
- set_autocomplete_provider 创建的 provider 会在调用后立即从 __kiana_refs 删除；PROVIDER_HANDLES 中原生 provider 的释放依赖 JsAutocompleteProvider 的 Drop，本次未展开核对那条路径（注释只描述引用表项的生命周期）。

### u236-kiana-extensions-large

- JsAutocompleteProvider::apply_completion / trigger_characters / should_trigger_file_completion 在同步 JS 方法里对 tokio::sync::Mutex 调用 blocking_lock()；Tokio 文档称在异步运行时上下文中调用会 panic，但本仓库调用是否总发生在 LocalSet 任务轮询期间、因而可实际触发，未实测。
- JsBorderedLoader::set_on_abort 在 cancellable=false 时仍执行 store_function_ref，但丢弃返回 id 且不 release，引用保留到引擎销毁；这是有意对齐 TS 还是遗漏无法从注释/测试确认（已在新增注释中按源码事实描述）。
- JsScrollView::set_request_render 每次调用都会通过 request_render_from_js 新建引用表项，但只更新 request_render/回调，不记录引用 id，重复调用会累积到引擎销毁；是否有调用方重复调用未验证。
- JsTui::add_child 在空代理（new TUIProxy()）路径只记录引用；其 Drop 注释承认没有释放路径，引用保留到引擎销毁，生产调用方是否使用空代理未验证。
- submenu_factory_from_ref 对返回 Promise 的异步子菜单退化为空 Spacer；TS 允许异步子菜单这一差异的实际影响未测。
- JsTerminal::start/drain_input 为 no-op（TS 会等待 drain 完成）；退出流程是否依赖真实 drain 行为未在测试中确认。

### u237-kiana-extensions-large

- start_provider_stream 的 `source == "provider"` / `provider` 分支：仓库内唯一发送 `kiana-ai.provider-stream` 的 JS（js/virtual/kiana-ai-helpers.js 的 builtinProviderStream）只发送 api/mode/model/context/options/onEvent，未找到会发送 source/provider 的调用方，该分支的实际用途未能确认（文档只按条件分支描述，未声称谁会使用）。
- build_namespace_seed 中 helper.get 失败时改写为 loading 错误（`missing helper export`）的路径是否会在「helper 缺少导出」时真正触发，取决于 QuickJS 模块 namespace 对不存在属性的 get 语义，未实测；文档按代码里 `map_err` 的作用描述，未断言必然触发。
- typebox 三个公开键必须使用再导出 shim 的精确失败机制未独立验证：既有注释称公开键不能直接复用桶文件源，我保留了原注释，只补充了 shim 不含相对说明符这一可从源码确认的事实。
- install 第 6 步（原生桥）失败后 `__kiana_virtual` 已存在、重试会被 is_installed 视为已安装而直接返回，这是读 install_inner 的代码路径得出的结论，未构造失败注入测试验证。

### u238-kiana-extensions-group

- project_trust.rs 既有 //! 里的 [`ProjectTrustHandler`] 未在本文件 import（本文件只 import 了 ProjectTrustContext/Event/EventResult）。按 rustdoc 作用域规则这可能是既有的 broken intra-doc link；任务禁止运行 cargo doc，故未验证也未改动原行。
- DispatchError::Js 的 name 字段在 dispatch_error_to_extension_error 中被丢弃，只保留 message/stack；这是代码现状，仅记录未修改。
- 包装后的 AgentTool 能否在 runner 创建线程之外执行未在源码/测试中确认；wrapper.rs 的文档只写了可验证的取消令牌共享与字段投影，没有写线程结论。
- index.rs 的 193/177/186/23 等计数沿用现有注释与 tests/public_surface.rs 的常量；未独立重算 TS 导出集合，仅与 spec/ca-extensions.md §2.4 做了对照。
- index.rs 原注释“runner.ts 的 6 个 handler 类型 / 见 types.rs#ExtensionContextActions”与代码矛盾：本地只有 5 个别名，且 new_session/fork/navigate_tree/switch_session 在 ExtensionCommandContextActions、shutdown 在 ExtensionContextActions。已按 spec §2.4 与 kiana-ext-api/src/types.rs 改写该段注释。

### u239-kiana-extensions-group

- interrupt() 的生产调用方未找到证据：全仓库 src 内没有 JsEngine::interrupt() 的调用点，只有 tests/engine.rs 直接调用；limits.rs 原有“/reload、UI 的 Esc”用途说法无法从本仓库源码证实（shutdown.rs 只在 Drop 里把标志清为 false）。
- DEFAULT_MEMORY_LIMIT 文档中的“0 表示不限制”依据是 rquickjs 0.14 的 API 文档（cargo registry 源码 runtime/base.rs），不在本仓库内；本仓库测试未覆盖 memory_limit = 0 的情形。
- install_runtime_limits 的“必须在创建 Context 之前调用”沿用原注释；我在 rquickjs 0.14 源码里只确认这些 setter 会锁 runtime 内部互斥量，未找到明确的前置条件说明，无法独立证实该“必须”。
- pump_completions 的返回值在 prelude 未安装（__kiana 缺失）时仍等于批大小（apply_completions 返回 batch.len()），此时 Settle 事件并未真正应用；文档沿用原有“应用的事件数”措辞，未把这个边角差异写进注释。
- Module::declare 从 loader 的 sources 表取源：已确认 loader/mod.rs 在安装时快照 engine.module_sources()、loader_impl.rs 有 sources 表与取值接口，但未逐行核对 declare 路径的读取位置，因此注释写成“快照后取源”而不断言具体调用栈。
- FACTORY_DEADLINE 只覆盖工厂调用的同步段：依据 loader/mod.rs 的工厂创建路径（with_js_factory_deadline 内调用工厂，返回的 Promise 在其外等待），未逐一检查所有工厂调用点。
- store_handle 的“本仓库只有测试使用”来自全仓库 grep（store_handle / JsHandleId 在 src 中仅 engine/mod.rs 定义、调用点在 tests/engine.rs 与 tests/engine_shutdown.rs）；若未来新增生产调用方，这条说明会过时。

### u240-kiana-extensions-group

- `JsEngine::invalidate`/`stale_message` 在当前 `src/` 中没有非测试调用者；既有注释称“api.js 的 assertActive() 用它”，但 `js/api.js` 里找不到 assertActive/stale 实现（stale 判定实际在 `runner/context.rs` 的原生类上）。未改动这条既有描述，待核实是否为将来接线或文档滞后。
- `fallback_error_from_value` 的 `stack` 读取与 `ctx.json_stringify(...)` 都只用 `.ok()` 而没有 `ctx.catch()`；若被物化值带有会抛异常的 getter / toJSON，可能残留挂起异常（`name`/`message` 分支有 catch）。未修改代码，仅在此记录。
- `HostValue` 的 i64/u64 精度说明依据 Rust `as f64` 语义与 JS Number 的 2^53 精确范围；仓库测试未覆盖超过 2^53 的转换。
- `fire_due_timers` 在 `__kiana`/`timerFire` 缺失时“interval 停表”的结论由 `finish(keep=false)` 分支推导，仓库内没有对应测试覆盖该前置状态。
- `live_persistent_count` 的“四个容器条目数之和”口径来自实现；其中 `pending` 条目不含 JS 值、`errors` 每条最多两个 Persistent，测试只用 Drop 不 abort 做断言，未逐一验证精确数值语义。

### u241-kiana-extensions-group

- transpile.rs 模块文档第 44 行原有链接 [`unresolved_bindings`] 指向私有函数；按“保留已有注释”未改写，若以 RUSTDOCFLAGS='-D warnings' 生成文档，可能触发 rustdoc::private_intra_doc_links（本任务禁止运行 cargo doc，无法实测）。
- inline_oxc_runtime_helpers 把内联 helper 的 preamble 无条件放在输出最前；若同一 TS 文件既以 shebang 开头、又触发装饰器 helper import，shebang 会被推到第二行（不再是合法的首行 #!）。现有测试分别覆盖 shebang 与 decorators，无组合用例；按要求只记录、未改代码。
- module_path 无条件在第一个 '?' 处截断；loader::resolver 的 resolve_specifier（resolver.rs:628）直接对 base 调用它，而 data: URL 的 payload 可含 '?'（loader_impl 对 data: URL 的 import.meta 有显式绕开，但 resolver 没有）。“data: 模块再相对导入”这一组合是否受影响未见测试，仅记录待核实。

### u242-kiana-extensions-group

- call_js_import 从 wasm 宿主闭包里再次调用 JsEngine::with_js；engine/mod.rs 的 with_js 文档声明“不得嵌套”，loader/mod.rs 的 call_event_handler 也专门复用 Ctx 以规避重入。按 rquickjs 0.14 非 parallel 构建（Context::with 用 RefCell::borrow_mut）推断，JS 调导出函数 → wasm 再调导入 JS 会走到嵌套 with_js，可能 panic。本单元禁止运行 cargo，无法验证；未写成事实。
- module_from_value 的 TypeError 文案固定以 'WebAssembly.Instance: Argument 0 must be a WebAssembly.Module' 开头，但 Module.exports / Module.imports 也复用它，逐入口文案与 Node 不一致（未实测 Node）。按要求只记录、未修改。
- WebAssembly.validate 对非 BufferSource 返回 false 而不是抛 TypeError；compile/instantiate 的编译、链接错误是同步抛出而非 rejected promise。这些是代码可见行为（已在注释中如实描述），但是否符合“对齐 Node”的目标需产品确认。
- js_to_val 用 Rust 的饱和 `as` 转换（NaN→0、越界夹到边界）；与 WebAssembly JS API 对 i32 参数通常采用的 ToInt32/ToUint32 取模转换可能不同。仓库内没有对照测试，未作为结论写死（注释只描述 Rust 语义）。
- i64 导入参数同时接受 BigInt 与普通 Number（后者经 Number→f64→i64）；JS 侧是否要求 i64 必须用 BigInt 未在本仓库验证。另外原有注释“数字按 JS ToNumber/ToBigInt 语义”对 Number→i64 这条路径并不严格（ToBigInt 遇 Number 会抛错），已在其下补充说明，未改写原句。
- 导出表是占位对象：get 恒 undefined、set 空操作、grow 返回创建时的 length；导入表统一用 Ref::Func(None) 初始化。非 funcref（如 externref）表是否被 wasmtime 的 Table::new 接受未实测，失败会映射为 LinkError。
- import_value 用 `.ok()?` 吞掉属性读取错误：若 JS import 对象的 getter 抛异常，这里会被当作“导入缺失”；挂起异常是否残留在 Ctx 上未实测。
- WebAssembly.instantiate 的 imports 参数不是对象时被 `as_object()` 静默当作“无导入”，而不是 TypeError；未与 Node 行为对照。
- 每个函数导入都通过 JsCallback::new → engine.put_persistent 登记，wasm.rs 内没有注销路径（未调用 take_persistent，InstanceData 也没有 Drop），反复实例化带函数导入的模块会累积引擎持久槽条目直到引擎 Drop。未找到相关测试。
- tests/ 目录没有任何 WebAssembly/webassembly 用例（只在 corpus.rs 注释里提到 doom-overlay 需要该特性），本文件的借用冲突文案、Promise 形状、错误对象 name 均无测试覆盖。
- 文档中“导入的 memory/table/global 只检查 JS 侧是否提供”已由源码确认；但 value 为 null 时 memory/table/global 视为已提供、function 则报 requires a callable，这一不对称行为未写入注释，也未找到测试。

### u243-kiana-extensions-group

- keybindings.rs 原有注释称“键位 id 全为 ASCII，str::to_lowercase 等价于 JS toLowerCase()”；本次未逐一核对全部 KeyId 的字符集，保留原断言。
- mcp_registration.rs 原有注释称错误 Display“与 TS Error.message 逐字一致”；本会话未打开 TS 源核对文案，属沿用的既有保真说明。
- noop_ui.rs 原有注释关于 TS theme Proxy 抛错行为与 ui-adapter JS 桥身份保证的说明未在本会话验证（未读 TS/ui-adapter 实现）。
- renderers.rs 原有注释称“TS 的闭包同样捕获 resolvers/base”，本次只核对了 Rust 实现与 kiana-ext-api/loader 的续延桥接，未查阅 TS 源。
- module_doc_before 一律填 adequate 是 schema 只允许 none/thin/adequate 所致；实际这 7 个文件的 //! 在本轮开始时就已接近 rich。

### u244-kiana-extensions-group

- 现有 //! 首段“且与交互式共用**同一个**实现”措辞有歧义：交互式模式本身使用 kiana-interactive 的独立实现（kiana-ext-api/src/ui.rs:36-40 明确“交互式实现”是另一个实现）；若该句指“任意模式在 set_ui_context(None) 时的无 UI 回落共用同一单例”（runner/mod.rs:1443-1460），则成立。按“不改写已有注释”的规则保留原文，请审阅者确认原意。
- “print/json 的装配路径是 set_ui_context(None, mode)”依据 spec/ca-extensions.md §2.8.3（print/json 不提供 uiContext）与 kiana-session 的 extension_ui_context 初值 None（agent_session.rs:1636-1637、6479-6481）；若存在仓库外的装配方显式传入 Some(print_json_ui_context())，该结论不适用，无法从本仓库确认。
- print_json_ui_context 在仓库内的直接调用方只有 tests/print_json_ui.rs（全 crates 目录 grep 结果）；是否存在仓库外消费者无法确认。
- 本次未运行 cargo check/doc（任务禁止），rustdoc 链接有效性为逐条人工核对：crate::runner::noop_ui、crate::runner::noop_ui::is_no_op_ui_context、crate::runner::ui_prompt::UiPromptContext 与 print_json_ui_context 均为公开项；仍需主代理用 RUSTDOCFLAGS='-D warnings' 复核。
- 公开项计数 1 采用 docs/COMMENT_PROGRESS.md 的统计口径（仅匹配 pub fn/struct/enum/trait/type/const/static/mod）；pub use 再导出的 UI_NOT_AVAILABLE/is_no_op_ui_context/no_op_ui_context 不计入，其文档由 runner::noop_ui 的定义处提供。

### u245-kiana-extensions-root

- 未运行 cargo doc / cargo check（任务明确禁止），因此新增的 /// intra-doc 链接（index、loader::install、ui::detach、Generation、ExtensionHost）未机器验证；链接目标均为本 crate 的 pub 项。
- into_engine 之后“UI 线程槽继续持有引擎 Rc、引擎留到线程退出”是根据 ui::install_attached 调用 JsEngineSlot::attach 且 JsEngineSlot 的 thread_local 存放 Vec<Rc<JsEngine>> 推导，仓库内无针对性测试；未修改代码。
- 既有 crate 文档中“kiana-coding-agent 入口丢弃 ExtensionHost::new() 错误并继续启动”等调用方断言本次未复核（未读 main.rs），保持原样。

### u246-kiana-interactive-large

- 模块文档中 Windows 路径分支（UNC、盘符根、跨盘 relative）的近似描述来自源码与既有说明，本次未在 Windows 上实测。
- “设置写入由 SettingsManager 后台队列完成、调用方负责 flush”依据 kiana-config/src/settings/mod.rs 的 setter 实现与 package_manager_cli 在 select_config 后调用 flush() 的代码；未实测异步时序。

### u247-kiana-interactive-large

- 第 483/491/539 行附近三处既有 fdlibm 注释把 π/2 写作 `kiana/2`（同仓库测试头也出现「±kiana」、spec 出现 `Math.KIANA`），疑为全局改名产物；按「保留已有注释」未改，建议统一修正为 π/2。
- `fallback_colors` 在 `THEME.appearance()` 返回 Err 时按暗色（黑底）兜底；spec §2.60.7 的 TS 片段是 `theme.appearance === "dark"`，appearance 缺失时两者可能不一致，未实测（主题初始化后通常不会走到该分支）。
- 未运行 cargo doc，intra-doc 链接仅人工检查：已确认公开文档不链接私有项，但 `ARMIN_WIDTH`/`ARMIN_HEIGHT` 等通过本模块私有 use 引入的短链接未做编译期验证。
- `public_total` 采用「pub 声明（含关联函数）」口径 64；若统计口径把公开字段/枚举变体也计入，则为 103（64+32+4+3），这些项本次也均有 ///。

### u248-kiana-interactive-large

- 任务禁止运行 cargo doc，因此新增的两个 intra-doc 链接（`create_mermaid_markdown_transformer`、`DiagramKind`）未实测是否零告警；两者都是本模块公开项，且本文件没有外层 /// 文档，按 COMMENT_STYLE §4.2 的陷阱说明应可正常解析。
- “golden 数据对应 grok-mermaid/TS 输出”的依据是测试文件注释（labels 用例注明 goldens 由 grok-mermaid@0.2.3 生成）与测试模块头的“移植自 mermaid.test.ts”，未逐条核对 fixture 生成脚本。

### u249-kiana-interactive-large

- 原模块文档用 `spec/ca-interactive-components.md` §8.1 支撑“单线程 LocalSet”结论，但该节实际是“建议模块树”；单线程依据实际来自 `ARCHITECTURE.md` §4.2 与 `crates/kiana-coding-agent/src/main.rs` 的 `new_current_thread` + `LocalSet`。按要求保留原注释未改。
- `models_equal` 原有“get_available_snapshot 只返回 chat 模型”的前提，依据是 `crates/kiana-ai/src/models/mod.rs` 中 `get_available` 的文档/实现以及 `ModelRuntimeSnapshot.available` 的构造路径；未在真实模型目录上运行验证每个模型的 `model_type` 实际取值。
- TS 侧行为（如 onSubmit 只选中当前项、初始 scope 规则、15 秒超时文案）来自 `spec/ca-interactive-components.md` §2.53.1 与 `spec/ca-interactive-mode.md` 的摘录，未直接阅读 TS 源文件 `model-selector.ts`。
- `tokio::select!` 在多个分支同时就绪时随机选择，依据 tokio 的公开语义；本仓库测试未覆盖“刷新与 15 秒 sleep 同时就绪”的竞态。
- `spawn_local` 要求 `LocalSet` 上下文是 tokio API 约束，已由 `crates/kiana-coding-agent/src/main.rs` 与测试中的 `LocalSet::run_until` 印证，但未单独运行验证。

### u250-kiana-interactive-large

- run_callback 的注释称 Pending 时调用的 tokio::task::spawn_local 要求当前线程存在 LocalSet：这是 tokio API 的约束，仓库内只能确认交互模式与调用方处于 LocalSet/spawn_local 环境，未在无 LocalSet 场景实测。
- “重新构造组件会丢失搜索框内容、选中位置与 is_dirty”是由构造函数默认值推得，测试未直接断言。
- 原模块文档中的“47 个 TUI + 43 个 app.*”未逐一核对计数（该行本次未改动）。
- “组件被丢弃不会取消已 spawn 的 future”依据是组件不保存任何任务句柄且代码直接 spawn_local；实际取消时机取决于执行它的 LocalSet 生命周期，未实测。

### u251-kiana-interactive-large

- LoadSlot.generation 在创建后不再被写入，因此回调里的 slot.generation != generation 恒为假；旧加载结果不可见的实际机制是 cancel_loads 丢弃 LoadHandle 后 sync_slot 不再读取旧槽位。注释已按此说明，未改代码。
- Path::exists 跟随符号链接与 Node existsSync 的等价性只在常规文件与测试覆盖的缺失文件上核对；broken symlink 等边界未实测，注释只声明常规文件一致。
- “request_render 不得同步重入组件，否则 RefCell 会 panic”依据借用范围与仓库内两个宿主的通道实现推断，没有测试直接验证该 panic。
- 公开项计数 59 = 29 个类型/函数声明 + 24 个公开字段 + 6 个枚举变体；若统计口径只含类型/函数则为 29。
- 未运行 cargo check/test/doc（任务禁止）；所有 Rustdoc 链接按源码可见性人工核对，未验证编译与测试结果。

### u252-kiana-interactive-large

- TS 源码不在工作区（packages/.../session-selector-search.ts 不可达）：模块文档中继承自移植期的 TS 行为描述（WeakMap 缓存、返回数组、TS 的 comparator/回退写法）无法直接对照 TS 源验证；本次只核对了 Rust 实现、crate 内测试与调用方。
- `translate_js_regex` 对具名组只扫描到第一个 `>`、不校验名字，因此 `(?<>)` 这类空/非法组名会被当作非捕获组接受；JS 是否对其报 SyntaxError 未在仓库测试中出现，待核实（注释只陈述“不校验”，未断言 JS 行为）。
- 修饰符组“JS 中只作用于组内”依赖 ECMAScript 规范而非仓库内证据：`flag_stack` 弹出值未写回是源码可确认的事实（`session_selector_search.rs` 原 829-836 行、现约 844-853 行），泄漏行为确定，但“JS 组内作用域”的对照依据只有移植注释与测试命名，未找到直接规范引用。
- 分数分层注释里“单次 fuzzy_match 分数小于 TIER_WEIGHT（1e6）”只是比较成立的前提说明；`SessionInfo.name` 没有长度上限，超长名字理论上可能打破该前提，未做实测，故未写成保证。
- `parse_class` 的区间端点用 `char as u32`（码点）比较，与 JS 非 u 模式的 UTF-16 code unit 顺序只在端点全为 BMP 时等价；星平面端点导致的“接受/报错”差异没有测试覆盖，注释已写成“可能不同”。
- 正则翻译与匹配行为的验证依赖 `crates/kiana-interactive/tests/session_selector_search.rs` 的断言（\s 含 BOM 不含 NEL、`$` 不匹配末尾换行前、字符类 i 闭包、legacy octal、修饰符组等）；未运行任何 cargo 命令（任务禁止），因此 fmt/doc/clippy 结果待主代理验证。

### u253-kiana-interactive-large

- AutomaticMenu::handle_mouse 的越界判断用 offset + height（按实际渲染行数累加）而不是 event.height；原文注释描述的是 TS Container 用 event.height 的语义，两者只在 event.height 恰等于该渲染总行数时等价，未运行验证。
- 「当前/默认模型不在列表时第一步预选回落到下标 0」依据是 settings_submenu.rs 的 build_select_list 在找不到 value 时不调用 set_selected_index，而 SelectList::new 的 selected_index 初始为 0；未运行测试验证。
- SettingsList 的模糊搜索按匹配分数升序稳定排序（kiana-tui/src/fuzzy.rs:327-329），注释已按该实现修正；该排序方向是否符合 TS 预期不在本单元文件范围内，未核对。

### u254-kiana-interactive-large

- update_display 末尾的 hide_component 判定当前不可达：if/else 两个分支都把 has_content 置 true，条件 `!has_content` 恒假。是否为移植遗漏（TS 对应逻辑可能依赖渲染器返回 undefined）无法确认；仅加注释说明现状，未改代码。证据：本文件 update_display，测试 self_rendered_empty_tool_rows_take_no_layout_space 显示空行效果实际由 render_self_shell 返回空 vec 实现。
- update_result / set_expanded / set_output_pad / set_show_images / set_image_width_cells 自身不调用 request_render；注释按「调用方补重绘」表述。已确认 events.rs 在 update_result 后、settings.rs 在 set_output_pad 后调用 request_render，但没有逐一审计所有调用路径（如 set_tools_expanded 经 show_status 间接重绘）。
- ensure_png_transcoder 的实现只在 Kitty 协议下注册转码器（kiana-core/src/image_process.rs）；非 Kitty 图片协议下非 PNG 图片是否由 Image::render 自行处理，未读完 Image 的所有协议分支，未在注释中断言。
- render_self_shell 注释称「内容与图片都为空时返回空 vec」；测试只覆盖「内容为空且无图片」的组合，图片存在而内容为空的行为来自代码阅读而非测试断言。
- ToolExecutionOptions 文档称三个值由调用方从设置取一次，依据是 interactive_mode/render.rs::create_tool_component；这是本仓库当前唯一构造点，公开 API 的其他宿主用法未验证。
- duration_ms 的换算规则（partial 为 None；最终结果中有限且 >0 才 floor，其余给 0）直接来自本文件代码，未对照 TS 的取整实现。

### u255-kiana-interactive-large

- 文件头“测试里是 thread-local 单例，需先 set_keybindings”表述略强：kiana-tui 的 get_keybindings() 未安装时会懒初始化默认表而不会失败，测试的 setup 只是为了安装确定的绑定表。该句未改动，仅在此记录。
- parse_js_date 的注释称“V8 还接受若干实现自定义格式”，该平台行为未在本仓库源码或测试中验证，属于既有注释的既有断言。
- 空树自动取消路径在本文件可达，但直接调用方 interactive_mode::commands::session::show_tree_selector 在 tree 为空时会提前 show_status 返回，因此该路径在仓库内是否被实际触发未确认。
- 交互模式下的调用方是否始终设置 kiana_theme::THEME 未在本次阅读范围内逐处确认；注释只陈述未初始化时 panic 这一代码事实。

### u256-kiana-interactive-large

- init() 的既有注释称 spec §2.50.3 共 16 步，而函数内步骤注释编号为 1–15；仓库内没有 spec 目录，无法核对，未改动该旧注释。
- native_is_absolute 在 Windows 下把任何第二位为 ':' 的路径（含 '1:foo' / 'ab:cd'）视为绝对路径，与同文件 path_basename 注释中“非盘符”的说明可能不一致；未实测，未改动代码。
- path_basename 的 Win32 语义仅由 #[cfg(windows)] 测试覆盖，本机为 Linux，未执行这些测试。
- format_display_path 使用字符串前缀判断 home（'/home/a' 会命中 '/home/ab'）；已按代码事实写入注释，但无法确认这是否是对齐 TS 的有意行为。
- 仓库 docs/ 下没有 spec/*.md，既有注释引用的 spec §2.50.1、§2.50.3、§7.2 无法在本地核对，相关断言只做了保留。

### u257-kiana-interactive-large

- get_auth_credential 使用 Rust regex 的 `\s` 与 `.`：与 ECMAScript 的 WhiteSpace/LineTerminator 集合存在已知差异（Rust 的 `\s` 含 U+0085、不含 U+FEFF，JS 相反），现有测试 auth_credential_matches_ts_bearer_header_semantics 只覆盖 ASCII 空白。仅记录，未改代码。
- 文件级 //! 中既有的 §2.50.12 与 TS 章节/测试名引用未在本次逐条对照 spec 原文核对，按“保留已有说明”处理。
- “文案是契约”的结论抽查了 tests/interactive_mode_model_auth.rs 的部分断言（/logout 无凭据文案、Anthropic 警告），未穷举全部 show_status/show_error 字符串。
- get_logout_provider_options 与 logout_provider 的 15 秒定时任务在操作提前完成后不会被取消；成功路径下 15 秒后会对已无人等待的 AbortScope 调 abort_with。未确认这是否影响已注册监听器，故未写入注释。

### u258-kiana-interactive-large

- handle_escape 的 500 ms 双击窗口与 Fork/Tree 分支按代码和 settings schema 描述；未找到覆盖窗口边界（恰好 500ms）的集成测试。
- show_extension_editor 返回 None 的“无 UI / 请求失败 / AbortSignal”路径来自 ExtensionUiContext::editor 的 rx.await.unwrap_or(None) 与 begin_extension_editor 的 on_cancel，未逐场景实测。
- 既有注释两处把延后 clear 的原因写成“drop 正在分发的代理对象（Rust 里是悬垂引用）”；按代码更可能是 RefCell 重入借用 panic，本次未改写这些历史注释，仅记录。
- public_total=35 的口径为 32 个 pub/pub(crate) 声明（函数/类型/字段）加 3 个 SelectorFocus 变体，不含 8 个 pub mod 声明（其模块文档应在各子文件 //!）；若按严格 crate 外可见口径，只有 8 个 #[doc(hidden)] pub test 接缝。

### u259-kiana-interactive-large

- SendOverlayOptions 的 unsafe impl Send 注释声称“值只在 UI 线程上使用（创建线程 = 扩展线程；本移植中与 UI 线程相同）”；本次只确认类型定义与转换路径，未逐一追踪所有 custom() 调用方所在线程，故保留原注释并列为待核实。
- set_custom_editor_component 文档中“TS 的 actionHandlers in newEditor 鸭子判定”等 TS 动机性表述来自既有注释，仓库内无 TS 源码可供对照，本次未改动。
- editor_slot_address 取容器 last()，而测试接缝 test_editor_slot_is_focused 取 first()；正常槽位只有一个子项，本次未深究两者是否在所有异常路径下等价，仅记录。
- process_request 的 CustomStart 分支没有为工厂 future panic 或 custom_ready 堆积提供失败路径注释，因为源码中未见相应处理（future panic 会由运行时传播），未作推断。

### u260-kiana-interactive-large

- `ExtensionTerminalInputSubscription.unsubscribe` 在 add_extension_terminal_input_listener 中始终是空闭包，且全 crate 未发现调用订阅项的 unsubscribe（只调用了 InteractiveModeInner 的同名字段）。这可能是遗留字段或潜在缺陷，未改动，建议核实设计意图。
- header_index 的 Empty 回退固定取下标 1，隐含“自定义 header 位于下标 0、内置空 Text 被挤到下标 1”的布局假设；若 header 容器被其它路径加入额外子项，回退可能指错槽位。
- set_working_visible(true) 在会话流式且当前指示器是 Compaction/Retry 等非 Working 时会用 Working 覆盖它（show_status_indicator 只保留一个活动指示器）。这是否与 TS 行为完全一致需对照 TS 源码确认。
- update_available_provider_count 只写 footer 数据、不调用 request_render；是否正确依赖调用方随后的渲染路径，本文件内无证据。

### u261-kiana-interactive-large

- restore_compaction_queue 在部分消息已成功提交、后续失败时会把这些成功条目也放回压缩队列（代码直接可见：restore_queue 用的是提交前的原始 queued 切片）。注释里如实描述了这一行为，但没有运行时证据证明 TS 侧行为一致（未读 TS 源），只写了"调用方持有的是提交前的原始切片"这一代码事实，未断言与 TS 等价。
- flush_compaction_queue 的 will_retry==true 路径对扩展命令也走 prompt（代码 1604-1626 行可见），注释只描述了这一事实；扩展命令经 prompt 是否总是同步执行完毕（try_execute_extension_command 分支）在 agent_session.rs:5240-5246 有源码依据，但未追踪扩展命令内部是否会启动 run，注释未对此做断言。
- render.rs 1847→约 2150 行全文件已读，覆盖：文件级 //!、全部 46 个 pub(crate)/pub 函数（其中 11 个 doc(hidden) 测试接缝保持原有一句话文档未扩充）、自由函数。私有小函数（fg/iso_now/assistant_of/text_block 等）原本已有文档，未改动。
- show_status 去重条件中"last_status_text/last_status_spacer 还持有句柄"的描述来自代码的直接结构（Option 句柄 + 指针比较），未验证 managed_tool_status 清空后 show_status 紧随的实际运行时表现；注释只写了代码保证（身份对不上就追加），未断言更多。

### u262-kiana-interactive-large

- is_package_source_info 在本 crate 内用 grep 只命中定义、没有调用点；其“保留以对应 TS 调用点”的理由仅来自既有注释，未对照 TS 源码验证，疑似未使用代码。
- format_diagnostics 中 fg(Dim, ... 内含 fg(Success/Warning) ...) 的嵌套 ANSI 复位效果未实测（未运行程序或测试），因此注释未断言嵌套着色后的实际显示，只描述代码结构。
- 重复路径的紧凑标签会回退为全段拼接路径（如 a/b），这一结论由 get_compact_extension_labels 与 get_compact_non_package_extension_label 的源码推导，tests/interactive_mode_resources.rs 未覆盖重复输入。
- 隐藏扩展被排除在 extensions 之外、因此不进 source_infos，引用其路径的诊断行没有来源标签；这是源码推导，未找到对应测试。
- format_scope_groups 对空组仍会打印组标题，当前所有调用方都先经 build_scope_groups 过滤；若未来新增不过滤的调用方会出现孤立标题（源码推导，未有测试覆盖）。
- Context 小节在 context_files 非空但 format_context_path 结果全为空串时，仍会创建一个正文为空的小节；该边界未在测试中覆盖。

### u263-kiana-interactive-large

- tempfile::Builder::tempdir() 的 TempDir 在 Drop 时递归删除目录属于外部 crate 行为，未在本仓库源码或测试中直接验证，仅按类型语义描述。
- `to_string_lossy` / `Path::display` 把非 UTF-8 字节变成 U+FFFD 是标准库行为，本文件测试未覆盖非 UTF-8 路径。
- node_write_file_error 仅按 errno code 把 ENOSPC/EDQUOT/EFBIG 判为 write 阶段；若打开阶段本身返回这些 errno，文案会与实际系统调用不符（代码未修改，仅记录）。
- share_via_gist 把 `stdout.trim()` 整体当作 URL 再取最后一段；若 gh 在 URL 之外输出额外行，解析结果会偏差（代码未修改，仅记录）。

### u264-kiana-interactive-large

- kiana-resources/src/package/paths.rs:1067-1071 存在 5 处 E0753（//! 内层文档注释被其他单元放到了 path_exists 函数之后、#[cfg(test)] 之前），导致 cargo check/doc -p kiana-interactive 全链路失败。这不是本单元文件，本单元无权修改；需主代理把该块注释移到文件顶部或改为普通 //。
- cargo fmt --check 在 components/armin.rs:272/289 报两处 diff（枚举变体被 rustfmt 折叠），是其他单元为变体新增 /// 触发的既有问题，与本单元无关，未处理。
- 既有模块文档声称 'InteractiveMode::run 启动的轮询任务每 10 ms 调一次 pump_events'——已确认 interactive_mode.rs:3204 的 Duration::from_millis(10)，但睡眠 + 执行耗时的实际节拍必然略高于 10ms；现有注释表述可接受，未改动。
- create_interactive_tui 文档称样式闭包'调用时读取全局主题、主题切换后不需要重建渲染器'：闭包体内调用 THEME.fg 等（每次调用走 current_theme()），该表述与实现一致，已保留；但'主题热切换'这一场景是否有真实调用方（interactive_mode 内是否运行中切主题）本次未逐条追溯，属既有注释，维持原状。

### u265-kiana-interactive-group

- radius_host_from_gateway 测试注释称期望值来自「Node 26 实测」——测试断言存在且与 url crate 语义一致（端口规则、IDN），但 Node 26 的实测数据本身无法在本环境复核，以测试锁定的值为准。
- external_editor.rs 模块文档称 Windows 上用 cmd /d /s /c 复刻 Node 的 shell:true——代码路径属实（cfg!(windows) 分支），但本环境为 Linux，无法实测 Windows 行为是否与 cmd.exe 参数完全一致。
- chat_viewport.rs 称 ScrollView::from_shared 的 UnsupportedAxis 分支不可达（本模块不设 axis）——依据是本文件构造 ScrollViewOptions 时不写 axis 字段；未逐字核实 ScrollViewOptions 默认值中 axis 是否确为 None（kiana_tui 侧源码未读），若默认非 None 则该 expect 会触发。

### u266-kiana-interactive-group

- reset() 不清空 blocked 列表与 last_report 是代码与 TS/spec 一致的行为（已在注释中如实说明），但“切换会话时是否可能残留过期 blocked”取决于所有对话框关闭路径，本单元未逐一追踪所有调用方。
- report() 只调用 Terminal::set_program_status；该调用是否真正产生 OSC 7501、以及终端支持性探测（如 KIANA_PROGRAM_STATUS）由 kiana-tui 决定，未在本单元验证，注释中未作此类断言。
- get_terminal 每次 report 重新调用、终端可原地替换，依据是构造函数注释与 events.rs 的调用方式；未追踪 renderer.terminal() 在多长时间内可能变化。
- 已按任务要求未运行 cargo check/test/fmt/clippy，注释改动未做编译与 rustdoc 校验。

### u267-kiana-interactive-group

- 模块文档中既有的「640x537」像素尺寸无法从本文件的 PNG 字节直接确认，未校验，保留原样。
- 「OnceLock 保证初始化闭包最多执行一次、并发调用者等待首个初始化完成」依据 std 文档语义而非仓库内测试，仓库测试未覆盖并发场景。
- 未核实 as_chunks::<3> 所要求的最低 Rust 版本（Cargo.toml 使用 workspace rust-version，本单元未读取 workspace 根配置）。
- 「include_bytes! 路径缺失会导致编译失败」是宏的编译期行为，仓库内没有针对该失败路径的测试。

### u268-kiana-interactive-group

- assistant_message.rs 注释称「重建后每个 run 会拿到新闭包」「覆盖值总是与本次渲染的 run 状态成对」：这是从闭包捕获语义直接读出的，未找到测试显式锁定跨重建的覆盖行为；已把最初写的「run 顺序跨重建稳定」改为只描述代码可见事实。
- assistant_message.rs 模块文档称「/thinking 等命令经注册表广播」：调用链已核实（widgets.rs:1342 set_hide_thinking_block、widgets.rs:867 set_hidden_thinking_label），但这两个入口对应的用户命令名未逐一追踪（只看到 extension_ui/mod.rs:2240 调 set_hidden_thinking_label），「/thinking」这个具体命令名是合理推断，未验证命令注册处。
- 工具 check_comment_only_diff.py 与 doc_inventory.py 在仓库中不存在（docs/COMMENT_STYLE.md 引用了它们）；本次以结构审计替代差异纯度检查。

### u269-kiana-interactive-group

- cargo check -p kiana-interactive --all-targets 当前无法完成：依赖 crate kiana-resources 的 src/package/paths.rs 在文件末尾（#[cfg(test)] 之前）有一段错位的 //! 块（E0753: expected outer doc comment，5 个错误），由本单元之外的并发注释工作引入。我无权修改该文件，已通过独立 rustc --edition 2024 解析确认我负责的四个文件本身无语法/文档注释结构错误，但完整的 crate 级编译与 RUSTDOCFLAGS='-D warnings' cargo doc 验证待 kiana-resources 修复后由主代理补跑。
- custom_editor.rs render_top_border_with_status 阶段 1 注释里「5 列 = 前缀 3 + 间隔 1 + 至少 1 列填充」是对常量 5 的最小合理解释（与阶段 5 的 width >= status_width + 5 一致），但代码中无直接注释或测试断言这个对应关系，属于我从两处代码推断的等式；TS 侧 custom-editor.ts 的 renderTopBorder 原始逻辑我未逐行比对到这一常量。
- branch_summary_message.rs 原 ///「（无变化守卫）」的意图存在另一种解读：可能指「调用方无需自己做守卫」，与代码不矛盾；我按更自然的读法（实现内无变化检查）判断其误导并与模块 //! 的「无条件重建」矛盾后改写。若原意是前者，新文案（明确写「TS 也不做变化守卫」）仍然准确，风险很低。

### u270-kiana-interactive-group

- extension_input.rs 模块文档写「宿主侧 finish_dialog_token 保证只 resolve 一次」——依据是 extension_ui/mod.rs 的注释（token 取出发生在同步调用里），但我未读 finish_dialog_token 完整实现，无法完全独立验证。
- extension_editor.rs 中「交互模式宿主从 settings 读命令传入（覆盖环境变量）」——看到 extension_ui/mod.rs begin_extension_editor 传 get_external_editor_command()，settings 值为空串时行为（会回落到 $VISUAL/$EDITOR）未实测。
- production 路径中 ExtensionInputComponent/ExtensionSelectorComponent 的 set_timer 从未被交互模式宿主调用（全仓库搜索只有测试调用与 widgets.rs 的 StatusIndicator 转发）——意味着生产环境里启用 timeout 的扩展对话框倒计时可能不走针（只有构造时的立即 tick）。这与 TS 的 setInterval 行为有实质差异，但本次只记录未在组件文档中断言宿主一定会装定时器；建议主代理核实是否有我漏看的安装路径。

### u271-kiana-interactive-group

- first_time_setup.rs //! 中的 TS 对应断言（如 invalidate() { this.update(); super.invalidate(); } 的顺序、TS 在同条件下抛异常）无法核实：本仓库未包含 TS 源码 packages/coding-agent/**，只能确认 Rust 侧行为与测试/golden 一致；这些为既有注释，按规则保留未改。
- footer.rs 头部 'TS 源：packages/coding-agent/src/modes/interactive/components/footer.ts' 同上，无法在仓库内核实 TS 文件存在或行号。
- index.rs 头部 'components/index.ts（37 行）' 的行数与 lib.rs 中 '约 19,342 行 / 245.7 KB' 等数字无法核实（无 TS 源），为既有注释。
- first_time_setup.rs 的 golden 对拍声明（"改字会破坏 golden 对拍"）依赖 TS 生成的 fixture（tests/fixtures/component-renders-dark.json、render_goldens_dark.json），fixture 本身由真实 TS 组件生成的说法未验证。
- footer.rs 中 'Kimi Coding 虽是 API-key 认证，但按订阅计费' 为既有注释，其 TS 依据无法在仓库内核实（仅确认代码行为：provider == "kimi-coding" 直接算订阅）。

### u272-kiana-interactive-group

- markdown_transform.rs 既有注释称「扩展桥把 JS 异常转成返回值」：已在 crates/kiana-extensions/src/loader/mod.rs:5731 (js_markdown_transformer) 核实，非失败路径返回 markdown.to_string()——准确，但该断言的证据在另一个 crate，本报告仅作确认，未写入注释。
- login_dialog.rs 的 fg/bold expect 消息称「TS throws here」：TS 源码（packages/coding-agent）不在本仓库，无法直接核对 TS 行为；该注释为既有内容且与 theme.rs 的 Result 语义（未初始化返回 Err）一致，未改动。
- oauth_selector.rs 模块文档称过滤文本含 method.name、视窗 8 行等：与源码逐一核对无误；但「TS 一致：up/down clamp」无法对照 TS 源验证（TS 不在仓库）。
- cargo check 无法完整运行：kiana-resources/src/package/paths.rs:1067-1071 存在既有编译错误（//! 出现在文件中部），疑似其他并行单元引入，阻塞了 kiana-interactive 的编译验证；本单元 diff 纯度靠逐行核对与 fmt --check 保障，未能得到 cargo check 的最终确认。

### u273-kiana-interactive-group

- radius_login_selector：本仓库内只找到集成测试 tests/components_small_selectors.rs::radius_login_selector_shimmers_the_selected_option 调用 set_timer；crate 之外是否有宿主调用无法从本仓库确认，因此模块文档按“当前仓库里唯一的生产调用方 auth 未调用”表述。
- skill_invocation_message：仅核实了 alt_screen.rs 的 apply_mouse_dispatch_result 对 Click 的 default_render 行为；主屏（main_screen.rs）鼠标分发是否也默认重绘未逐行核对，文档中只点名了 alt_screen。
- show_images_selector：ListProxy::as_any 是否真的被下转，在仓库内未找到调用点；注释只描述“暴露自身以便按类型下转”，未断言实际使用者。
- settings_submenu：搜索框 Input.on_submit 在自定义 keybinding 下的确切路径只按代码顺序与 TS 注释推断，未运行验证。
- radius_login_selector.rs 第 136 行注释里的 `[0, cycle)` 会让粗糙的括号配对脚本报 -1 偏差，这是纯注释内容，不是代码改动。

### u274-kiana-interactive-group

- theme_selector 的生产调用方在仓库内未找到（仅 tests/components_small_selectors.rs 与 sdk_surface.rs、barrel 再导出引用）；on_preview 由宿主调 initTheme 做实时预览的描述来自 spec/ca-interactive-components.md §2.11，而非仓库内的 Rust 调用方源码。
- thinking_selector 模块文档中“过滤后保留原选中项，否则回到过滤结果第一项”的“回到第一项”：SelectList::new 默认 selected_index=0（select_list.rs:240），重建后自然落在 0；但 TS 原文的精确行为（index vs 保持绝对位置）只依据 spec §2.50 一句描述，未逐行核对 TS 源。
- status_indicator.rs 模块文档声称的“更换编辑器时通过共享单元格把同一实例重新挂到新编辑器”取自既有 //! 注释，未逐行核对 extension_ui/widgets.rs 的挂载实现（不在本单元覆盖范围内）。
- rustfmt 对 armin.rs（非本单元文件）的两处枚举折叠 diff 是预先存在的，来源未知，已按规范只记录不修改。

### u275-kiana-interactive-group

- UserMessageSelectorComponent 未覆写 handle_mouse（Component 默认返回 None），而 UserMessageComponent 覆写并转发给容器。TS 的 Container 基类有 handleMouse，但仓库中无 TS 源码可查，无法确认选择器侧不转发鼠标是有意复刻还是潜在遗漏——未写注释，仅在此记录。
- 按任务约束未运行 cargo check / cargo doc，user_message.rs 新增 4 行 //! 中 [UserMessageComponent::set_output_pad] 内联链接的有效性未经工具验证（目标是同模块 pub 方法，且 mod.rs 中 pub mod user_message; 无外层 ///，理论上短链接可解析）。
- user_message.rs 注释称 OSC 133 标记『常量与 TS 逐字节一致』：仓库内无 TS 源码，仅凭常量形态（\x1b]133;A\x07 等）与既有注释自洽，未独立核实。

### u276-kiana-interactive-group

- cargo check / cargo doc 未能对 kiana-interactive 跑通：依赖 crate kiana-resources 的 src/package/paths.rs:1067-1071 有 5 个既有 E0753 错误（//! 出现在 mod tests 声明之后的位置），该文件不属于本单元。RUSTDOCFLAGS='-D warnings' cargo doc 的链接检查因此未完成；我改为静态核查了全部 intra-doc 链接目标（仅剩 7 个链接，均指向公开项）
- input.rs 模块文档称“Ctrl+D 直接退出”未提编辑器非空分支——spec/ca-interactive-mode.md §2.50.7 写的是 handleCtrlD 编辑器为空才 shutdown，但 Rust 代码（input.rs:254-262）无条件 shutdown。代码与 spec 可能存在移植出入；按任务规则我只描述了代码行为，未修改代码，此差异应提请主代理核实
- events.rs 的 AgentStart 分支注释举了“压缩失败→AutoRetryStart→重试触发新一轮 agent 运行”的嵌套场景，这是依据 spec §2.50.5 的“不碰 autoCompactionEscapeHandler”与 retry/auto_compaction 双槽位字段结构推断的组合场景，spec 有对应语义但未逐字展示该组合路径；注释措辞已限定为机制说明而非实测断言

### u277-kiana-interactive-group

- lifecycle.rs 的 //! 原文声称 `app.suspend` 键位经 `run_after_dispatch` 调用 `handle_ctrl_z()`，我已在 commands/mod.rs:445 看到 handle_ctrl_z 调用，但未核对该调用是否确实挂在 app.suspend 键位下（属于原有注释，未擅自改写）。
- drain_terminal_input 的注释写“关停前清空终端里尚未被消费的输入”，参数 1000/50 的语义依据是 kiana-tui/src/terminal.rs:1503 的 drain_input(max_ms, idle_ms) 与实现（总时长/空闲窗口），但未实测 drain 期间被丢弃字节的实际去向。
- stdout_supports_color 的注释只陈述代码可见事实（NO_COLOR 存在即 false、FORCE_COLOR=="0" 即 false、否则 is_terminal），未声称与 Node 的 supportsColor 库完全等价。
- reload.rs 的 //! 说“会话本身的重建在 AgentSession::reload”，仅从 kiana-session/src/agent_session.rs:7747 的签名与实现确认它是会话重载入口，未逐段核对其内部全部副作用。
- uncaught_crash_code 注释称死终端错误返回退出码 129，依据是 emergency_terminal_exit_code 的字面返回 129；未核对该值在 TS 侧是否也是 129（退出码映射不在本单元可验证范围内）。
- handle_ctrl_z_with_platform 注释称恢复回调由轮询任务触发，依据是 ProcessCtrlZPlatform::on_sigcont 里 25ms 轮询 RESUMED 的实现；未验证 Windows 分支以外的平台差异。

### u278-kiana-interactive-group

- `maybe_save_implicit_project_trust_after_reload` 中 app.rs 的注释称该候选只在「当前项目没有需要信任的资源」时给出；函数体又要求 `has_trust_requiring_project_resources(&cwd)` 为真。两处描述的时点差异（启动 vs reload 时重查）是我按源码顺序推断的解释，未找到明确设计文档佐证。
- `handle_resume_session` 的信任工厂在模式已释放时 select 返回 None → `resolve_project_trusted` 以 Ok(false) 收场：该结论来自 kiana-config/src/trust/project_trust.rs:295-305 的源码阅读，未跑测试验证。
- `create_extension_ui_context` 每次 rebind 新建通道/drain 任务并覆盖线程局部 EVENT_SENDER：依据 extension_ui/mod.rs 的模块文档与 1949-1962 行，但“旧上下文是否会泄漏”未核实，故未写入。
- `install_current_mode` 注释里“会话替换前后是同一个 InteractiveModeInner”依据是 interactive_mode.rs 中仅 1706 行一处构造；若未来新增重建路径该说法会失效。
- `record_crash` 的“裁剪到上限”依据 kiana-resources/src/crash_log.rs:548-552 的 MAX_CRASH_RECORDS 逻辑，具体上限数值未写入注释。

### u279-kiana-interactive-group

- bash_compact.rs 中“本地 bash 的输出在本任务被 poll 时同步到达”依据 kiana-tools/src/bash.rs 模块文档与 execute_bash_with_operations 的实现；若将来 BashOperations 改为在别的线程回调 on_data，线程局部表方案会静默丢输出——注释按现状描述，未把跨线程行为写成保证。
- handle_bash_event_result 的文档说 truncated 时 TruncationResult 的统计字段取 Default，这与 TS 只传 {truncated:true, content} 一致；但 BashExecutionComponent 目前只读取 truncated 与 full_output_path（bash_execution.rs:376-390），无法从源码确认没有别的消费方依赖这些统计值，需要组件维护者确认。
- commands/misc.rs 的 /debug 文档写 panic hook 会“按未捕获异常记录并退出”；lifecycle.rs 的 hook 在模式状态已被可变借用或当前模式缺失时会退回默认 hook，因此极端情况下并不走崩溃路径。注释只描述常规路径，这一例外已在 lifecycle.rs 注释里，未在本文件重复。
- get_path_command_argument 的文档只断言“命令后必须恰好一个空格”；未验证调用方是否可能传 '/import\t...'（当前 dispatch_submit 的匹配用 'text == "/import" || text.starts_with("/import ")'，制表符分隔的命令根本进不到这里），因此制表符行为虽可从实现推出，但未写成调用方契约。
- export_import_share.rs 的 /import 文档把 cancelled=true 归因于扩展的 before-switch 处理器；agent_session_runtime.rs 的两个 cancelled 分支均在 emit_before_switch 返回 true 时产生，未见其它来源，但未穷举全文件所有返回 Ok(SessionReplacementResult{cancelled:true}) 的位置之外的深层调用。

### u280-kiana-interactive-group

- model.rs 的 `/scoped-models` 刷新续体是否在 `update_session_models` 之后还要等待其他任务：我只按源码与 interactive_mode_model_auth.rs 描述，未覆盖所有调用方测试。
- session.rs 中 `uses_default_session_dir` 的取值语义（自定义目录时 list_all_in 只列该目录）来自 session_manager.rs 的方法名与 list_all_in 调用形态，未在测试中直接验证。
- `show_session_selector` 的重命名失败静默忽略（`if let Ok(...)`）是源码事实，但为什么允许静默失败（是否有 TS 对应注释）我未在 TS/spec 中找到直接证据，因此只描述行为不述动机。
- `ModelPriceSource` 适配返回 `None` 时缓存统计如何处理该轮次，我只在 cache_stats.rs 看到 trait 与 scan 的接口，未逐行确认 scan 对 None 的分支，故注释避免断言具体计价跳过方式。
- `ThemedText` 主题失效重建时会重新调用 build，从而让 now_epoch_millis 取新值；这一点基于 kiana-ui-kit/src/themed_text.rs 的 invalidate/render 实现推断，未跑测试验证交互模式下 /session 文本主题切换后的实际刷新。

### u281-kiana-interactive-group

- settings.rs：`selector_slot` 与回调闭包共享 `Rc`，已在文档里记为“形成 Rc 引用环”，但未实际运行验证是否真的泄漏，也未确认在什么时机会释放。
- settings.rs：`for_each_message_child` 用 `try_borrow_mut` 遍历消息子组件，遇到借用冲突会静默跳过；被跳过时具体哪些工具消息的图片/宽度不会更新，只能从代码推断，未运行验证。
- settings.rs：`set_output_pad` 把 i64 收窄成 u8 的安全性仅依赖 `kiana-config` 的 `get_output_pad` 只返回 0 或默认值 1，未做范围校验的部分按代码推断记录，未实际构造越界值测试。
- thinking.rs：ThemeSelector 取消子菜单时用原始设置再调 `on_theme_preview` 恢复画面，此行为读自 `components/settings_selector.rs` 注释与代码，未运行 UI 验证实际重绘效果。
- 两个文件均未执行 cargo build/test/fmt/clippy/doc，注释正确性只做了静态核对（代码行数、括号/引号平衡、属性数量、intra-doc 链接目标可见性）；涉及 rustdoc 链接有效性的结论未由 `cargo doc` 验证。

### u282-kiana-interactive-root

- 「唯一调用方是 kiana-coding-agent」依据 workspace 内 grep 与各 Cargo.toml；若 workspace 之外有外部消费方则不成立（本仓库范围内未发现）
- 「行数约 19,342 / interactive-mode.ts 245.7 KB」引自 ARCHITECTURE.md §1 与 §7 的统计，未逐行实测
- 48 个组件的数量来自 Cargo.toml description 与 ARCHITECTURE.md §3.7 表述，未逐一清点 components/ 目录

### u283-kiana-json-large

- `parse` 的文档写「错误消息是 V8 的 16 类模板之一」，而 `ParseError::at_position` 的注释写「V8 的 15 类位置模板」；两者是否指同一分类（15 个带位置模板 + 1 个不带位置模板）在仓库内无依据，保留原文未改。
- `parse_number` 的指数饱和值选用 `i64::MAX / 4`：本次注释只依据源码解释了「饱和不影响结果」，除以 4 这个具体选择没有仓库内说明，未写成设计动机。
- `repair` 的签名返回 `Option<String>` 但当前实现恒为 `Some`；为何保留 Option（历史接口或为将来失败分支预留）在仓库内无记录，只写成事实、未推断原因。
- `Parser::new` 注释中「先展开成 Vec<u16> 是因为多处按下标回看」是依据 `char_at`/`slice_ascii`/`excerpt` 的实际用法归纳，源码未明文说明该动机。
- `SPECIAL_STRINGS` 的既有文档写作「走 `"%\" is not valid JSON` 模板」，引号疑似不配对（可能本意是 `"<input>" is not valid JSON`）；未改动，待核实。

### u284-kiana-json-group

- 未运行 cargo doc（任务禁止构建），新增的 intra-doc link（如 [`f64::from_bits`]、[`crate::js_parse::parse`]、[`JsType::as_str`]）仅按同 crate 公开项/标准库项人工核对，渲染与警告未实测。
- 统计口径：public_total 只计公开类型、方法与函数，不计 6 个 JsType 变体；若把变体也计入，js_string.rs 为 14 项而非 8 项。
- parse_js_number 文档中「JS 的 Number() 字符串不接受数字分隔符」属于语言规则，仓库内没有单独夹具验证这一点（代码对 `_` 返回 NaN 可从 valid 函数直接确认）。
- js_num 模块文档「ryu 与 Number::toString 的最短位数一致」依据的是 tests/js_number.rs 的黄金表（1200 随机 + curated）与属性测试，未重新查阅 ryu 上游规范。
- V8 行为描述（DoubleToRadixStringView 细节、过深数组抛 RangeError、JSON.stringify 对 U+2028 不转义）沿用原有注释与测试结论，本单元未重新对照 V8 源码。

### u285-kiana-json-group

- 文件头原有注释称 Rust 固定栈在约 1.4 万层会 abort；该数字沿用原注释，本任务禁止 cargo 构建，未重新实测。
- parse_num 的两处 TS 差异（找不到 e 时 Rust 跳过 -1 重试；最终错误取第一次 token 而非 TS 重试那次）来自逐行对照 partial-json dist 源码；「不会从公开入口逃出」是由 parse_container/frame_error/deliver 的调用结构推导，现有测试未直接覆盖该错误文本。
- 模块头第 8-10 行原有表述把 parse_partial 与 parseStreamingJson 的调用方式关联；实际 js_parse.rs 调用的是 parse_partial_tagged。函数级注释已按源码更正，模块头保留原句（其重点在 ALL 掩码），如需完全统一可后续调整。

### u286-kiana-json-root

- 现有首句称“三个 crate 边界上的约定”，但随后列出 4 个模块；按“保留原注释”规则未改写，无法确认是否为笔误，待主代理核实（my section 也未改变该句）。
- 现有注释引用的 `ARCHITECTURE.md` §4.3/§8 与 TS 源 `packages/ai/src/utils/json-parse.ts` 在本仓库 docs/ 中不存在（docs/ 目前只有 COMMENT_STYLE.md），这些章节号与 TS 路径无法在本单元独立核验，仅保留原引用。
- public_total=24 的口径是 4 个 `pub mod` + 20 个根部重导出名；这 20 个名字的 `///` 位于各自定义模块，本次未逐项重审其内容（例如 `ParseError.message` 字段在 js_parse.rs 中缺少 `///`），只确认了重导出目标存在且可见。
- “工作区中的大多数 crate 直接依赖它”基于各 crate Cargo.toml 的静态检索（23/29 个 crate 直接依赖 kiana-json），未运行 `cargo metadata` 验证实际依赖图。
- 本单元未运行 cargo 检查（任务禁止 build/test/fmt/clippy），Rustdoc 链接只按可见性人工核对（均为根部重导出或 pub 模块），未经 rustdoc 实际解析。
- parse_streaming 第二个 bool 的含义来自 js_parse.rs 文档注释及测试中的命名（from_strict/_complete），未通过运行测试验证。

### u287-kiana-mcp-large

- 未能运行 cargo check/doc/fmt（任务禁止），Rustdoc 链接只在源码层面核对：所有 [`...`] 目标均为本模块 pub 项或已 use 的公开类型（McpError）；本模块在 lib.rs 中无外层 ///（已确认），因此 //! 里的短链接按模块作用域解析。
- public_total=104 是我的计数口径：80 个函数（含 4 个 trait 方法、不含 trait impl 方法与 From impl 的 from）+ 10 个类型项 + 11 个公开字段 + 3 个枚举变体。仓库中没有 tools/doc_inventory.py（tools/ 不存在），无法与项目统计口径对齐。
- 原注释『to_header_map 尽力而为：无法表达的条目被跳过』与代码矛盾：实现是 try_to_header_map().unwrap_or_default()，任一条目失败会让整个 HeaderMap 变空。我改写了该行并在其下写明机制；依据是本文件 661 行与 tests/fetch.rs 的 try_to_header_map 用例。
- 原注释『take_stream 取走后其余读取方法得到空体』不准确：bytes() 在 body.is_none() 时返回 body_unusable（TypeError）。我改为『报 TypeError（体已不可用）』；依据是 1108-1117 行。
- UnauthorizedContext::body_text 的原注释称等价于 await response.text()，但它不剥离开头 U+FEFF，而 FetchResponse::text 会剥。我只补充说明、未改写原句；这是否为有意偏差需主代理确认。
- 模块文档中『超时是一个总预算』的严格时间线：deadline 在 send 前算出，而 tokio::time::timeout(timeout, send) 的计时从 await 时开始（与 deadline 相差极小），因此响应头阶段最多可耗到 deadline，体读取只拿剩余预算；极端情况下体阶段可能只剩接近 0 的预算。
- 跨 crate 断言（kiana-builtin-extensions 实现 AuthProvider、包装 McpFetch 加超时、needs_sign_in 依赖 is_oauth_authorization_required）来自读源码（src/mcp/oauth.rs 的 McpAuthProvider/TimedFetch、src/mcp/runtime.rs 的 needs_sign_in），但没有编译验证。
- 未运行 tools/check_comment_only_diff.py（仓库无 tools/），差异纯度靠手工核对每次 Edit 的 old/new 串：所有改动行均为 //、///、//! 或空行，代码 token 未变。

### u288-kiana-mcp-large

- 未运行 cargo check/doc/fmt（任务禁止构建与写操作），Rustdoc 链接解析与行宽仅人工核对；`McpClientOptions::request_timeout_ms` 字段链接沿用仓库既有 `Self::roots` 写法。
- public_total=68 是源码中 `pub` 声明计数（含 7 个 McpClientOptions 字段与 4 个 McpRequestOptions 字段）；若只计类型/函数/常量则为 57，二者都已有 ///。
- 「锁序固定为客户端锁→子锁、无反向获取」「监听器/handler 在释放客户端锁后调用」是通读本文件全部加锁点与调用点的结论，未做运行时验证。
- 模块文档中「生产代码里构造并使用 McpClient 的地方是 kiana-builtin-extensions/src/mcp/runtime.rs」依据全仓 grep `McpClient::new`（生产代码仅此一处），未排除仓库外的调用方。
- 模块文档中「13 个 it 逐字移植」取自 crates/kiana-mcp/tests/client.rs 自述，未与 TS 原文件逐条比对。

### u289-kiana-mcp-large

- 未运行 cargo fmt --check / cargo doc（任务明令禁止构建命令）：rustfmt 对新增 /// 的排版影响与 rustdoc intra-doc 链接仅人工核对，未实测。所有新增链接目标均为公开项；另把原有的两处公有文档链接（指向私有 deserialize_params、parse_response）改为行内代码，以避免 private_intra_doc_links。
- JsonRpcId 的派生 untagged 反序列化对超大数字/指数（如 1e400）的具体行为未在仓库测试中覆盖，文档只描述了“字符串进 String、数字进 Number、其余类型拒绝”的类型层面行为。
- JsonRpcErrorCodes 的 PARSE_ERROR / INVALID_PARAMS 语义按 TS 字段名直译（tests/api_surface.rs 只断言数值，未覆盖这两个码的使用点）；METHOD_NOT_FOUND / INTERNAL_ERROR 的说明有 client.rs 的调用点佐证。
- “serde_json::to_value(f64) 会得到 1.0”是 serde_json 的通用行为，本仓库没有直接测试该断言，仅用于说明 value_to_json_rpc_id 的反向转换应走 JsonRpcId::Serialize。
- 模块文档称 stdio 的非法帧“交给 error listener 后继续读下一行”，依据是 stdio.rs 的解析循环与 dispatch_stdout 会丢弃 listener 错误；未运行 stdio 相关测试验证。

### u290-kiana-mcp-large

- cargo check / cargo doc / cargo fmt --check 均未执行（任务明令禁止）；新增 /// 是否会触发 rustfmt 对枚举结构体变体的重排、以及 rustdoc 链接是否零警告，需主代理统一验证。
- 模块文档与 close 文档中「close 之后仍可调用 wait_for_callback、只能等超时」由代码路径直接可见，但 tests/oauth.rs 与 tests 目录没有覆盖该场景。
- read_request_head 中 head_bytes 用 latin1 解码后 String 的 UTF-8 字节数（0x80–0xFF 占两字节）估算头块大小，偏大；该细节无测试覆盖，仅为源码推导。
- Node 26 实测类断言（NODE_METHODS、NODE_HTTP_VERSIONS、400/431 语义、llhttp 请求行形状）沿用仓库既有注释与测试，本环境未复测。
- 生产调用方只精读了 kiana-builtin-extensions/src/mcp/oauth.rs 的 sign_in_mcp_server/run_sign_in_flow 相关段（约 1050–1310 行），未通读其全文；kiana-ai/src/auth/oauth/callback_server.rs 是同名但独立的实现，不是本类型。
- drain_chunked_body 的 u64 加法溢出边界是从算术与分支推断，未编写复现用例。

### u291-kiana-mcp-large

- 既有模块文档『4xx 与 502 是 discovery miss（继续试下一个候选）』只精确描述授权服务器发现；对 discover_protected_resource_metadata，4xx/502 仅在『自动推导 + 非根路径』时触发回退，显式 URL 与根路径时是终止错误。未改写原句，只在新小节与函数注释中限定适用范围，请审阅是否统一措辞。
- js_origin 的既有文档写『内层 scheme 为 http/https/file 时取内层 origin』，代码只显式匹配 http/https、其余返回 "null"；对 file: 两种写法结果相同（file: 的 origin 序列化就是 "null"），故未改动，如需严格逐分支表述可再调整。
- discover_authorization_server_metadata 中 metadata.issuer().unwrap_or_default() 的默认分支对 parse_authorization_server_metadata 的产物不可达（parse 要求 issuer 必填）；未改动，仅记录。
- discover_oauth_server_info 的 origin 回退在 js_url::parse_relative(&server, "/") 之后又 set_query(None)/set_fragment(None)，对以 "/" 作相对引用而言这两步看似冗余；未改动，仅记录。
- discard 在 TS 里接收必填 Response，Rust 侧是 Option<FetchResponse>；该签名差异的动机源码与注释均未说明，本次只按实际行为描述（None 直接返回），未推断原因。

### u292-kiana-mcp-large

- 未运行 cargo build/test/doc：新增 rustdoc 链接 [`OAuthFailure::Transport`]、[`parse_oauth_tokens`]、[`OAuthIssuerMismatchError`] 均指向 pub 项，按源码可见性判断可解析，但未实测。
- OAuthClientProvider 文档中「tokens()/save_tokens() 可能被并发调用」由 trait 的 Send+Sync、&self 方法与并发 401 测试推断；没有测试覆盖两个 save_tokens 同时执行，若实现假定独占需复核。
- public_total=31 的统计口径是顶层 pub 类型/trait/自由函数，不含 pub 字段、枚举变体与 trait 方法（这些成员也都已有 ///）。
- 为避免 rustdoc::private_intra_doc_links，把 //! 中指向私有 js_nullish/js_includes/js_length/is_js_whitespace 的 4 处链接改成行内代码；未运行 cargo doc 验证原先是否真的告警。

### u293-kiana-mcp-large

- 未运行 cargo doc（任务禁止）：`invalidate_credentials` 新增的 [`CredentialKind`] 与 `state()` 新增的 [`OAuthClientProvider`] 依赖模块内私有 use 解析到 flow 的 pub 项，按 rustdoc 规则目标项公开即不应触发 private_intra_doc_links，但需主代理用 cargo doc 复核。
- 「TS 的 load 只 await this.writes、不把 store.load 的结果 promise 写回链」来自 spec/telemetry-mcp.md §30.17 的文字与 Rust 实现对照，仓库内没有 TS 源码可直接核对。
- MemoryOAuthStateStore 在锁中毒后取回内部值继续使用，这一设计意图（panic 后仍可读写、对应 JS 无中毒概念）是依据实现与字段结构给出的解释，没有测试覆盖。
- own() 文档中「单槽 store 会被本服务器的状态覆盖、按 URL 分键的 store 不会」由 update 的 read-modify-write 代码推断；集成测试 binds_persisted_credentials_to_the_exact_mcp_server_url 只覆盖读路径。
- spec §30.17 明确说 TS 侧该毒化行为「测试不覆盖」；本文件单元测试 a_failed_write_poisons_the_chain 覆盖的是 Rust 实现，不能作为 TS 行为的证据。

### u294-kiana-mcp-large

- `#[serde(transparent)]` 让这五个 Map 支撑类型在反序列化时要求输入是 JSON 对象（数组/标量失败）：依据 serde 把该属性委托给 `Map<String, Value>` 的通用行为，仓库内没有针对这五个类型反序列化非对象输入的测试。
- `js_number_value` 的 `debug_assert!` 只在不变量被违反时触发；release 下 `Number::from_f64` 返回 `None` 时落成 `0`。本模块的调用点不可达该分支（serde_json 的 `Number` 表示不了 NaN/Infinity），未运行验证。
- `js_ordered` 是输入的深拷贝、代价与输入规模成正比：依据 `kiana_json::js_object::sort_js_keys` 的模块文档（结果树与输入同规模），未做基准测试。
- `OAuthChallenge::to_value` 用 `Url::as_str()` 输出字符串；它只被黄金向量 `oauth_www_authenticate_golden.json` 覆盖的那些 URL 形态验证过，fixture 未覆盖的 URL 规范化形态与 TS `URL.href` 是否逐字一致未经实测。
- provider 的 `save_tokens` 对 `expires_in` 为 `null`/`""` 时按 0 处理：依据 provider.rs 的模块文档与 `js_number` 实现（`Value::Null -> 0.0`、`parse_js_number("") -> 0.0`）读到，未运行其测试验证。

### u295-kiana-mcp-large

- ProgressNotification.total 的非数字原值会丢失：源码路径是 number_field 返回 None、total 在 keys::PROGRESS 已知键中、from_value 只对 message 调 restore_unconsumed；仓库没有测试覆盖该组合，也未找到说明它是否有意，故只在字段/模块文档中记为现状，未改代码。
- ContentAnnotations 直接使用派生 Deserialize 时 audience 的未知成员会报错，这一条依据 serde 派生语义推断；仓库测试只覆盖 Resource 经 from_object 的宽松路径（tests/content.rs / 单元测试断言 "robot" 被丢弃）。
- serialize_js_number 注释里「直接用 serialize_f64 与 JS 文本不同（-0、极大指数）」依据 kiana-json 的 js_num/js_object 文档描述，本单元没有实测 serde_json 的 f64 输出。
- index.rs「按 TS 的导出面再导出」依据 index.rs 的模块注释与其再导出清单，未逐一比对 67 个名字。
- ProgressNotification::from_value 在 crate 内没有调用点（grep 仅命中文档）；「client.rs 的 handle_progress 忽略非法 progressToken」是 handle_progress 自己的独立校验路径，不是对该函数的调用。

### u296-kiana-mcp-large

- 模块文档里既有的 TS 实测结论（probe A/B2/D/P1 的 microtask 顺序、structuredClone/queueMicrotask 语义）来自参考实现的历史实测；仓库内没有 TS 源码，本次无法复核。新增注释刻意不引入新的 TS 行为断言，只引用这些既有结论。
- 新增的「并发 close 落在 send 的某次检查之后时消息仍可能被投递」是从代码推导（多次独立原子 load、drain 每条消息前重查 closed），没有专门测试该交错。
- 「close 监听器里重入 close() 会立即返回 Ok」由代码路径推导；现有测试只覆盖 message 监听器里调用 close()（a_message_listener_can_close_the_transport）。
- DrainSession::drop 在 drain 意外 panic 时的排空者交接（drainer 编号 +1、旧守卫不误清）没有对应测试，注释描述的是代码行为而非实测。
- 若排空 task 在首次 poll 前被运行时丢弃，且之后再也没有 enqueue，队列中消息的 oneshot 等待方不会被唤醒；这是从代码推导的活性边界，未见测试覆盖。
- public_total=15 只统计结构体、结构体公开字段、固有方法与自由函数，不含 trait impl 方法以及 Default/Clone/Drop/Debug 四个 trait 实现（其中 Default/Debug 本次补了说明）。

### u297-kiana-mcp-large

- atexit 钩子在另一线程持有注册表锁时的行为按 std::sync::Mutex 语义从源码推断（lock() 正常持锁会阻塞，只有 poison 才让 .lock().ok() 返回 None），未实测；原注释「会跳过清理」已按代码改写，建议复核。
- close_impl 中 *child_closed.borrow() 为 true 的分支只在「读到 pid 后、订阅前 close 处理器完成」的竞态下可达，这是按加锁顺序做的结构推断，没有专门测试覆盖该分支。
- max_message_bytes = 0 时仅含空白的行（例如只有 \r 的行）也会超限，因为长度检查先于剥 CR/trim；这是代码顺序的直接结论，但现有 stdio_framing 测试只覆盖了空行 \n 与正常 JSON 行，建议补边界用例确认 TS 行为。
- 公开项计数 21 含 10 个字段与 2 个枚举变体，不含 6 个 McpTransport trait 实现方法；若统计口径把 trait impl 方法单列，则 27 个公开入口全部可读到文档（3 个本地 docs + 3 个继承 trait 文档）。
- 模块文档原有叙述把「剥尾部 CR、trim 后跳过空行」写在「超限检查」之前，而 handle_stdout 实际先做长度检查；未改原文，只在字段文档与行内注释中补充了真实顺序。
- kill_on_drop(false) 的理由（运行期由关闭阶梯显式发信号、进程正常退出由 atexit 兜底）来自代码结构与 tokio 默认语义，没有单独的行为测试。

### u298-kiana-mcp-large

- 既有注释 `StreamableHttpReconnectOptions::initial_delay_ms` 写“首次重连前的延迟”，但 `reconnect_delay` 在服务器给过 `retry:` 时对该条流的**每次**重连都直接返回服务器值；我只在结构体文档中写明，未改字段原句，是否改写原字段注释待定。
- 既有注释 `StreamableHttpTransportOptions::auth_provider` 只写“401 后刷新”，而代码对 `insufficient_scope` 403 也走同一授权恢复路径（见 `authorized_fetch` 与测试 `hands_401_and_insufficient_scope_403_responses...`）；未改原句。
- 为消除 rustdoc::private_intra_doc_links，我把公开文档中 4 处指向私有项的链接改为行内代码（模块文档里的 `Utf8Decoder`×2、`node_timeout_duration`×1，以及公开别名 `SseRetryHandler` 文档里的 `node_timeout_duration`）；另把 `Utf8Decoder` 文档中的 [`Self::chunk`] 改为 `pending`，因为该结构体的字段实际叫 `pending`（`Self::chunk` 不存在）。任务禁止运行 cargo doc，链接是否零警告仅按源码可见性判断。
- 模块文档原写“四个常量”，我改为“五个常量”：依据是文件内 5 个 `pub const`（MAX_ERROR_BODY_BYTES、ERROR_MESSAGE_BODY_CHARS、三个 DEFAULT_RECONNECT_*）与 `tests/api_surface.rs` 的全部导入；若 §30.1 的计数口径不是这五个，请复核。
- 既有 `close()` 注释“错误全部忽略”改为“DELETE 1 秒超时且失败被忽略”，因为 `emit_close()` 的监听器错误会通过返回值向上传播；`controller` 字段注释原写“中止所有出站 fetch”，改为限定 `authorized_fetch`，因为 `close()` 自己的 DELETE 只带超时、没有 `with_signal`。
- 本环境未运行 cargo check/fmt/doc（任务明确禁止 build/test/fmt/clippy 与写文件命令），因此“仅注释行改动”是按每次 Edit 的 old/new 文本逐对核对得出的，未用工具快照验证。
- `Utf8Decoder` 文档中“8,000 组随机对拍”“Node 26 实测”等既有结论保留原文，本次未独立复验。
- `SseParser`、`TransportInner` 新增的字段级注释来自字段的读写点（新增/清空位置），未通过运行测试验证。

### u299-kiana-mcp-group

- oauth.rs 顶部仍把 types/errors/discovery 标为“本单元”，与本次注释单元的职责划分不一致；这是历史单元说明、不与代码矛盾，未改动。
- transports.rs 原 ListenerStore 注释“emit 时先取 id 快照”与实现及测试（listeners_added_during_an_emit_run_in_the_same_emit、a_listener_that_removes_itself_does_not_skip_its_successor）矛盾，已按 COMMENT_STYLE 允许的例外改写并在总结中说明依据。
- transports.rs 原 TransportEvents 注释“emit_* 只在 crate 内部（但对兄弟模块 pub）使用”只描述使用现状；已替换为 TS protected 与 Rust pub 可见性的对照。仓库内 grep 未发现 crate 外调用 emit_*，但这些方法是 pub，不能断言外部不会调用。
- DEFAULT_MAX_MESSAGE_BYTES 的用途描述依据当前 stdio.rs（max_message_bytes 限制 stdout 行/缓冲）与 streamable_http.rs（max_message_bytes 作为 SSE 事件上限）的接线；若默认值/接线变化需同步该注释。
- 任务禁止运行 cargo check/doc，新增的 intra-doc 链接（crate::index、McpOAuthProvider、authorize_mcp、McpOAuthStateStore、crate::client::McpClient、crate::transports::*::*）均指向公开项，但未经过编译器验证。

### u300-kiana-mcp-group

- 既有注释（本次未改）把 needsSignIn 归给 coding-agent；仓库里实际按名字分支的是 kiana-builtin-extensions/src/mcp/runtime.rs:1069-1070 的 needs_sign_in，kiana-coding-agent 内未搜到该符号。可能是沿用了 TS 包名，未改原文。
- 任务禁止运行 cargo check/doc，新增与既有 intra-doc link 是否零警告未实测；新增链接目标均已确认是 pub 项（McpTransportError、McpFetch、OAuthFailure 及其变体/方法），format_issuer 等私有项只用了行内代码。
- js_quote 与 JSON.stringify 的完全逐字符等价未验证（只核对了 js_string.rs:289-309 的转义分支及其测试），U+2028/U+2029 等边界未覆盖。
- OAuthFailure::as_authorization_required 与 as_transport 在仓库内没有调用方，文档只写语义，未描述消费场景。
- OAuthFailure::Error 归属 oauth::types 的 `Invalid …` 依据 types.rs 模块文档与 8 处 OAuthFailure::error 调用，未逐条核对每个消息文本。

### u301-kiana-mcp-root

- 未运行 cargo doc（任务禁止构建类命令）：[`McpClient`]、[`McpClientOptions`]、[`McpRequestOptions`]、[`McpError`] 的链接依赖 rustdoc 对 `pub use index::*` glob 再导出的解析。静态检查上这些名字都在 src/index.rs 中逐名 pub use，但需要一次 `RUSTDOCFLAGS='-D warnings' cargo doc -p kiana-mcp --no-deps` 才能最终确认无 broken-intra-doc-links。
- “crate 根是 64 个 Rust 名字”来自 crates/kiana-mcp/tests/api_surface.rs 的模块文档（67 个 TS 名中 4 个错误类合并成 McpError），未实际编译计数。
- “唯一的 workspace 内 crate 依赖是 kiana-json”依据 crates/kiana-mcp/Cargo.toml 当前依赖列表；后续新增依赖需同步该句。

### u302-kiana-resources-large

- Windows 专用代码（RtlGetVersion / RegGetValueW / szCSDVersion）无法在 Linux 上编译或实测；新增文档只按源码结构描述，未验证 API 失败时结构体是否被部分写入。
- `collect_environment` 的 runtime 探测（spawn bun/node）与 /proc 读取的失败分支只按源码描述，未在本机逐一触发验证。
- 模块头沿用的 TS 行数（bug-report.ts 375 行、bug-report-upload.ts 37 行）与 spec 溯源未逐步核对，保留原注释。

### u303-kiana-resources-large

- ‘写锁只覆盖字段写入’改为‘通常只覆盖，例外是第 20 步’：依据是本文件 1067-1073 行在写锁内调用 apply_extension_source_info，后者经 source_info_map.rs 的 get_default_source_info_for_path 执行 std::fs::metadata；未运行任何编译/测试验证。
- get_system_prompt_source 原文‘存在且可读入’与代码不符：实现只判断 Path::exists()，读取失败或 override 返回 None 时仍可能保留 source path（本文件第 25 步）；已按代码改写。
- get_append_system_prompt 原文‘不存在的项已被丢弃’与代码不符：resolve_prompt_input 对不存在的路径返回原文，filter_map 只丢弃空串（本文件 2012-2031 行；测试 does_not_expose_literal_append_system_prompt_text_as_a_source 也保留字面文本）；已改写。
- 模块文档与既有注释中‘load_extensions_cached / load_extension_from_factory 返回非 Send future（rquickjs Persistent 为 !Send）’为原有断言，我阅读了 kiana-extensions 的函数签名与文档但未独立验证 !Send 属性，未改动。
- 步骤 12 为 CLI 扩展/技能写入的是固定的 {source:"cli", scope:"temporary"} metadata，未带 package_root；这会使步骤 15 的 collect_extension_package_warnings 看不到 CLI 包的 packageRoot。是否为 TS 的原始行为未核实，仅记录，未修改代码。
- reload 第 3 步的 resolveProjectTrust 回调与 extend_resources 的重入组合（回调里再次 extend_resources）没有测试覆盖；我只在文档中保留原有‘可重入’说法，未新增更强结论。

### u304-kiana-resources-large

- 原 list_configured_packages 注释声称存在 `try_list_configured_packages`，但全仓库 grep 未找到该符号；已按源码改为“读取失败当空列表、解析失败跳过”，上游 TS 是否有对应 API 未能核对。
- GitUpdateTarget.source 标注了 `#[allow(dead_code)]`，但 update_configured_sources 的 git 任务实际读取该字段；属性疑似过期，按规则未修改，是否可移除未经编译验证。
- update() 中 local 源的输入侧 identity 以 cwd 为基准，而 settings 条目侧按各自作用域基准解析；该差异是否与 TS 完全一致未对照 spec/*.md 或 tests 逐条核实。
- 按任务要求未运行 cargo check/doc/fmt/clippy；新增的 /// 与 //! 链接只做了静态核对（公开文档未链接私有项），Rustdoc 实际解析结果与 rustfmt 无 diff 尚未由工具确认。

### u305-kiana-resources-large

- 未运行 cargo check/doc/fmt（任务禁止），因此 rustdoc 链接与 rustfmt 结果未实测；新增内容没有使用方括号 intra-doc 链接，避免链接到 pub(crate) 项。
- 未逐一复核 tests/fixtures/package/semver_golden.json 的全部条目，只确认 fixture 存在且测试确实加载它；node-semver 极端输入的行为以现有测试与文档描述为准。
- parse_npm_spec 用显式字符类仿 JS 的 `.`，但 JS 的 `$` 与 Rust regex 的 `$` 在“字符串以换行结尾”等边界上的等价性未实测；parse_source 调用前会先 js_trim，常规路径不受影响。
- PartialVersion::parse 中 `parts.is_empty()` 分支实际不可达（split 至少产生一个元素），仅记录，未修改代码。

### u306-kiana-resources-large

- 原 //! 中“getdents 原始顺序”是对 std::fs::read_dir 底层行为的表述：本仓库内可证的是 read_dir_sorted 存在的必要性（测试锁顺序）与 sort_by_key(file_name) 实现；getdents 一词是底层推断，未改写（不与源码矛盾）。
- to_posix_path 文档说“只在 Windows 上替换反斜杠”——由 cfg!(windows) 分支直接可证，未列为待核实；但 Windows 实际行为未在本机验证过。

### u307-kiana-resources-large

- 未运行 cargo doc（任务禁止），无法机器验证新增 intra-doc 链接；新增链接均指向同文件 pub(crate) 或私有项，位于非公开文档中。
- expand_braces_rec 的 is_top 参数被 `let _ = is_top;` 消费，注释只陈述其当前未参与逻辑，是否为后续对齐预留无法从源码确认。
- path_exists / path_is_dir 把 std::fs::metadata 的任何 Err（含 EACCES）都视为不存在，这是 std 的 Result 语义，未做各平台实测。
- expand_package_glob 不返回 Err 的结论继承自模块原注释与代码（所有 IO 失败被静默吞掉），本次未新增实测。

### u308-kiana-resources-large

- 未运行 cargo check/doc/fmt（任务禁止），新增内容未经 rustdoc 工具验证；为避免 private_intra_doc_links，新增注释只使用行内代码而未使用方括号链接。
- expand_package_glob 的 Err 分支在当前源码中未见实际产生路径，注释只陈述源码里 `if let Ok(...)` 会忽略 Err 这一可见行为。

### u309-kiana-resources-large

- parent_of 的绝对路径/含 `//` 分支会丢掉前导 `/`（`/a/b` -> `a/`）。本文件内递归调用只依赖相对路径，未见问题，但未能用 npm 源码核对这是否与上游 parentOf 完全一致，建议审阅者确认。
- test(path, check_unignored) 的 check_unignored=true 分支在本文件与已读调用方中都未被使用（ignores() 固定传 false），其语义仅按代码推导，未有测试覆盖。
- compile_pattern 文档中“skills.ts 的 try/catch 会中止整个目录扫描”为原有注释，本次未阅读 TS 源码核实，仅原样保留。
- posix_class_ranges 的区间标注为 git wildmatch.c 的 ASCII 类别，本次未与上游 C 源码逐项比对。
- 模块文档称 package/discover 会先给模式加 root 前缀；skills 侧已核对 prefix_ignore_pattern，package 侧只确认存在 add_ignore_rules 调用与相等注释，未逐行读完该函数。
- compile_pattern 在正则编译失败时回退为 NEVER_MATCH；这一行为对 check_pattern 已通过但语法畸形的模式成立，但本次没有构造这样的输入做实测（受任务限制不能运行 cargo）。

### u310-kiana-resources-large

- win32 路径实现（win32_*）在 Linux 上只参与编译、无法运行，其与 Node 语义一致的说法来自逐行阅读实现与测试注释，未实测。
- IGNORE_FILE_NAMES 的读取顺序会影响规则覆盖，依据是 ignore_match.rs 中 add 按序 push、test_rules 按规则顺序求值（约 803-843 行），未逐条对拍 npm ignore 7.0.8。
- `create_skill_source_info` 中 scope 传 None 被补成 Temporary 的结论来自 kiana-core/src/source_info.rs:180-188；若上游后续改默认值，本条注释需要同步。

### u311-kiana-resources-group

- crash_log.rs 中 write_crash_log 的“写入中途失败留下不完整 JSON”结论来自 std::fs::write 的标准库语义（truncate 后 write_all、无临时文件 rename），未在本仓库实测。
- diagnostics.rs 的 4 个公开项是 pub use 再导出，计为“已文档化”依据是 rustdoc 继承 kiana_core::diagnostics 的定义文档；本文件未给 use 语句新增 ///。
- CrashInput 原有 /// 写的是 record_crash 的语义（“记录一次崩溃；失败时返回 None”），与 pub struct 不符，已改写为结构体说明，并把原句保留到 record_crash 文档；这是本单元唯一改写的既有注释。

### u312-kiana-resources-group

- footer 的轮询任务只在“前后两次都成功读到元数据且快照不同”时刷新；文件消失或从缺失恢复不会触发刷新。这是按源码条件注释的观察，未对照 TS 规范确认是否为有意行为。
- FooterDataProvider::available_provider_count 用原子访问；注释称 footer 渲染与扩展查询都可能读，但实际调用点只见 InteractiveMode 的 UI 线程与测试，是否存在跨线程读取未逐一核实。
- manifest.rs 的模块文档使用了 [`read_kiana_manifest`] 短链接指向本模块再导出项；按 rustdoc 语义应可解析，但按任务约束未运行 cargo doc 验证。
- footer 模块文档把 reftable 的目录监听与 tables.list 轮询并存的理由写成纯行为描述，未断言平台差异或性能动机。
- 本次只做了注释编辑，未运行 cargo check/doc/fmt；仅通过阅读确认代码行未改动。

### u313-kiana-resources-group

- themes.rs:150-152 “Node 的 readdirSync（libuv uv_fs_scandir）按文件名（strcmp）排序”是对 libuv 内部机制的陈述：仓库内测试（themes_load_in_node_readdir_name_order）只锁定输出顺序，机制本身无法在仓库内验证（TS 源码与 libuv 不在本仓库）。
- context_files.rs / warnings.rs 中多处对 TS 侧行为的断言（如 “${error} 是 Error.toString()”、“JSON.parse 抛出未被捕获”）依据 spec/ca-resources.md 与注释中引用的 TS 测试名（resource-loader.test.ts），TS 源码本身不在本仓库，无法直接核对 TS 实现。
- warnings.rs:163 “名单里的名字都是 ASCII”是从 HOST_PROVIDED_EXTENSION_PACKAGES 的 10 个字面量读出的，对当前常量成立；若未来加入非 ASCII 名字，sort_unstable 与 JS UTF-16 排序的差异会重新出现——注释已写明前提，但这是对常量内容的观察而非可执行的保证。

### u314-kiana-resources-group

- paths.rs（不在本单元文件清单）在 1067-1071 行有历史遗留的块级错误：/// 结束后出现 5 行 //!（E0753: expected outer doc comment），导致整个 kiana-resources 的 cargo check / cargo doc / cargo fmt --check 全部失败。这是并行单元的产物或原有状态，我只在 /tmp 的验证副本中临时将其转为 // 以完成本单元校验，原文件未动，需负责 paths.rs 的单元修复。
- exec.rs decode_pipe 的原有注释断言“libuv 的读缓冲区是 64 KiB”——我未找到 Node/libuv 源码证据，属既有内容，未改动、未背书。
- exec.rs get_env 注释中“Bun 编译二进制的兼容路径”为既有表述，仓库内未找到直接佐证（TS 源 package-manager.ts 对应处也未核对到该行），未改动。
- skills/discover.rs:7 有一个指向私有 crate::skills::ignore_match 的 intra-doc 链接警告，同样是既有问题且不在本单元清单，未改。
- collect_auto_skill_entries 与 collect_skill_entries 的 TS 调用点对应关系（“resolve.rs 分别调用两者”）基于 resolve.rs 与 TS 源的 grep；TS 中 collectAutoSkillEntries 确为单行转调（已实读 package-manager.ts:466），但“保留独立名字是为了对应 TS 调用点”属于意图推断，已按推测措辞书写。

### u315-kiana-resources-group

- npm.rs 模块文档中写"错误文案是字节级契约的一部分，调用方（如交互式 UI）依赖它们与 TS 行为一致"——错误文案与 TS 逐字对应在测试（truthy_non_iterable_npm_command_matches_v8_error 等）中有直接证据，但"交互式 UI 依赖这些文案"是推断，未逐个追查 UI 调用点；若需严格，可弱化为"与 TS 逐字对应，测试锁定"。
- git.rs 模块文档原有断言（如 prune_empty_git_parents 的 starts_with 字符串前缀语义、temporary 作用域布局）本次未重新逐一实测，沿用原文；这些断言与 paths.rs 的 get_git_install_root/get_temporary_dir 实现一致，但未跑测试验证。

### u316-kiana-resources-group

- 编译无法整体验证：crates/kiana-resources/src/package/paths.rs 第 1067-1071 行有一段写在文件中部（#[cfg(test)] mod tests 之前）的 //! 块，触发 5 个 E0753（expected outer doc comment）。该错误在我介入前已存在于磁盘状态，且 paths.rs 不属于本单元（绝对规则禁止我改动非负责文件，即使这段 //! 明显应移入 tests 模块文档位置或改为 //）。同错误也出现在 cargo doc 中，导致无法对 kiana-resources 运行 RUSTDOCFLAGS=-D warnings 验证新增链接——但新增链接 [PathMetadata]/[SourceOrigin]/[SourceScope]/[InstallScope] 全部指向本文件 pub use/pub enum，均为公开项，不触发 private_intra_doc_links。
- check_comment_only_diff.py 最后一次全仓运行报告 1 条违规：crates/kiana-session/src/agent_session.rs 代码行序列变化（thread_local! RESOURCE_LOADERS 被删除）。该文件不属于本单元，应是并行单元的工作；本单元两个文件相对基线的代码行序列零差异（用过滤注释后的 diff 逐一验证）。

### u317-kiana-resources-group

- mod.rs 中「目录 read_dir 失败被静默跳过」既有注释与代码一致（load_templates_from_dir 第 226 行），但 `Path::exists()` 为 false 与 `read_dir` 为 Err 的区分未在 TS 源中逐行核对（TS 用 existsSync + readdirSync try/catch，语义等价但错误细节未比对）——未新增相关断言，仅保留原有注释。
- mod.rs 新增注释称 metadata 失败「只对应两次调用之间状态变化的竞态」：TS 源用 try/catch 包住 statSync 且注释说「如权限不足也会记 warning」（mod.rs 既有 /// 中的表述），但 Rust 侧 exists() 通过后 metadata 立即失败是否真的只剩竞态（Windows 上的权限语义等）未实测；该断言措辞为「与 TS 的 try/catch 一样记 warning」，保守且与代码行为一致，但竞态归因本身未验证。
- substitute.rs 中「L 溢出 usize::MAX 经 saturating_add 后再 min(args.len()) 截到末尾」的等价性由代码结构直接推出（第 130-145 行），未跑测试验证 `${@:1:99999999999999999999999}` 这类极端输入的实际输出；测试覆盖了 `${@:2:99}` 但未覆盖溢出 L。

### u318-kiana-resources-group

- discover.rs:233-234 与 tests/skills.rs:554 的断言「Node readdirSync 由 libuv uv_fs_scandir 按文件名（strcmp）排序后返回」与 spec/ca-resources.md:477-478、:6098（「未排序……不得排序」）表面冲突。实测本机 Node v26.12.1（3000 个乱序创建的目录）确认 readdirSync 返回字典序；同款论断也出现在 prompts/mod.rs:232、loader/themes.rs:150、package/paths.rs:372、kiana-tools/ls.rs:174。已保留原注释不改，spec 那两处疑为描述旧版 Node 或另一上下文，建议主代理核对。
- discover.rs:77「内容按 from_utf8_lossy 解码，与 Node 读文本时的替换字符行为一致」：这是 Node readFileSync utf-8 的标准行为，但仓库内未找到针对非法 UTF-8 ignore 文件的测试锁定，未获测试级证据。
- validate.rs:57-58「对应 JS 正则没有 m 标志、$ 只匹配输入末尾」：与 TS 源 /^[a-z0-9-]+$/ 一致，但只是对正则语义的静态推断，仓库内靠 name_regex_requires_the_end_of_input_like_js 测试锁定行为（测试通过即等价），未在真实 Node 下跑过带换行用例对拍。

### u319-kiana-resources-root

- 新增文档中“kiana-interactive 的交互模式/`/bug` 流程直接使用本 crate”依据是 kiana-interactive/Cargo.toml 的依赖声明与其 src/bug_report.rs、tests/bug_report.rs 的导入；我没有逐条阅读 /bug 命令的完整调用链。
- “包安装失败记录为安装失败项”依据是 package/mod.rs 的模块文档第 38-39 行，未逐行核对 resolve_inner 实现。
- 原文件中“package-manager-cli.ts 归属 kiana-coding-agent”“mcp-servers.ts 在 kiana_core::mcp_servers”等历史断言未改动，本次也未独立验证其正确性。
- public_documented_after=0 是刻意结果：crate 根仅含 pub mod 声明，按规范不得在父模块 mod 声明上方新增外层 ///；实际模块说明位于各自的 //!。

### u320-kiana-rpc-print-large

- SharedState::unsubscribe_backpressure 原注释『Rust 侧无法建立（见模块文档）』疑为历史遗留（模块文档第 2 条说的是『错误通道』，不是背压订阅）。我依据 rebind_session 中实际建立订阅并 set_unsubscribe_backpressure 的代码改写了该字段注释；若该注释另有所指（例如指某个早期桩阶段），请复核。
- 模块文档称『同一进程同一时刻只支持一个 print 模式运行』——依据是 signal.rs 头部说明与 thread_local 槽位的 expect 消息，未在测试中实测并发行为。
- print_signal_exit_code 的 143/129 值沿用 TS 与既有代码及测试断言（signal_exit_codes_match_ts），未验证 143=128+15 的语义惯例在目标平台是否一致。

### u321-kiana-rpc-print-large

- signal_child 的 pid 为 None 的成因（spawn 后 id 未就绪 / 已 reap）我未在源码中找到推导路径，注释只写了“不发送”这一可观测事实，未断言原因。
- stop() 里 stdout_task.abort() 与 TS attachJsonlLineReader 返回的 detach 闭包在“未读 stdout 字节是否继续被缓冲/消费”上的差异，我只按源码描述为“行留在管道、未分发行随任务丢弃”，未验证 Node 侧流的行为。
- supervise 的控制通道关闭分支（control_open = false）之后 supervisor 仍会等到 child.wait()；这在 stop() 先把 Inner::process 置 None、句柄被丢弃时触发，但具体何时观察到我未实测。

### u322-kiana-rpc-print-large

- run_rpc_mode_with_host 的『嵌入式调用方』仅保留原有注释的说法；仓库内实际验证到的使用者只有测试（rpc_dispatch_parity.rs 等），未见嵌入式生产调用方。
- Windows 分支的『没有 tokio SIGTERM 源』沿用原注释；未在 Windows 上实测。
- lock() poison 恢复的注释描述了选择恢复而非继承 panic 的行为事实，但『为什么这样设计』（是否对齐 TS）无直接证据，措辞只陈述机制不陈述动机。
- 未运行 cargo check / cargo doc（任务禁止写命令之外的 cargo 校验仅允许只读；fmt --check 已做），rustdoc 链接有效性靠人工核对链接目标列表而非工具输出。

### u323-kiana-rpc-print-large

- RpcCommandType 的这些变体在 TS 侧是 RpcCommand["type"] 的提取类型；Rust 无法自动提取内部标签枚举的 tag，因此手写 as_str 的说法是对 Rust 语言能力的陈述，未在仓库文档中找到明文依据（属于可验证的语言事实，但如需严格出处请复核）。
- 文档中说服务端 dispatch 路径不经过 RpcCommand 的 Deserialize：依据是 rpc_mode.rs handle_input_line/handle_command 全程用 kiana_json::js_parse 与 Value 取字段；但如果仓库中还有外部调用方直接反序列化 RpcCommand（本单元未发现），该表述需收紧。
- RpcExtensionUIResponse 的“写出方”未能在仓库内定位到实际构造/序列化点（rpc_client.rs 中无 extension_ui_response 的发送路径），因此文档只描述其类型语义与存在性判定，没有声称具体调用方。

### u324-kiana-rpc-print-group

- signal.rs 模块文档中“管道写满时 write 被忽略而不影响语义”的依据是代码（write 返回值未检查）与 read_wake_byte 的语义（任意字节/EOF/错误都返回），除此之外没有针对 EAGAIN 的测试或实测；若需更强断言请以 tests/rpc_signal_parity.rs 与 tests/signal_exit_codes.rs 的现有覆盖为准。
- json_event.rs 文档称“错误监听器不写 stderr、后续事件跳过直到 agent_settled”来自 print_mode.rs:948-970 与 rpc_mode.rs:720-765 的代码；spec §2.2.6 称这是近似而非完整复刻，但 Rust 侧没有与 TS 失败 run 合成事件的端到端测试，该差异范围未在测试中量化。
- jsonl.rs 文档称 attach_jsonl_line_reader 当前只有测试与导出面审计引用（src 内无生产调用）。这是基于全仓库 grep 的结果；若工作区外还有依赖方（例如另一个未纳入本次 grep 的 crate），该断言需要更新。
- json_event.rs 的 message_usage 文档中“自定义消息 usage 为 null 时写出 \"usage\": null”是按代码路径（object.get(\"usage\\).cloned()）推导，未见对应测试 fixture 覆盖该分支。
- 本单元未运行 cargo check/doc（任务禁止）；signal.rs 的 module 文档未加任何 intra-doc 链接指向私有项，但 lib.rs 在 `mod signal;` 上方有历史外层 ///，按 COMMENT_STYLE §4.2 的 rustdoc 作用域规则，`//!` 中的 [`Registration`] / [`restore`] 可能因合并到父模块作用域而告警——这两个链接是基线原有内容，本次未改动，需在文档检查阶段确认是否已存在该告警。

### u325-kiana-rpc-print-root

- “RPC 的 SIGHUP 不跳过 flush”这一表述的 TS 侧对齐依据只有 rpc_mode.rs 里 RunSignal::skips_flush 的注释（“TS 只在 SIGTERM 上跳过”）；未在 TS 源码中直接核实，但 Rust 侧代码路径确凿。
- crate 根 //! 中“正常退出路径在退出前 await flush_raw_stdout”对 print 侧依据是 print_mode.rs:688 的 io.flush()（信号路径跳过），对 RPC 侧是 perform_shutdown 中非 skip_flush 分支的 io.flush()；两处均亲自读到，但“正常退出路径”的完整枚举（如扩展触发的关停）未逐一追踪。
- 未运行 RUSTDOCFLAGS='-D warnings' cargo doc（任务只允许只读命令）；链接有效性靠人工核对全部 [`...`] 目标均为 pub（本 crate 内 pub 项、glob 再导出的 serialize_* 与 WireError、跨 crate 的 kiana_core::output_guard）。

### u326-kiana-session-large

- check_compaction 中 'assistant_is_projected' 的语义（usage 匹配投影）是从变量名与判断结构推断的，源码无直接注释证据；已把注释写成结构描述（'usage 匹配投影的隐式溢出'）而非动机断言。
- run_local_blocking 里 thread::park_timeout(100us) 的忙等细节：注释只复述了已有行内注释（'只用 noop waker 同步轮询'），未新增关于唤醒及时性的断言（未实测）。
- 'AgentSession.compact 先 abort() 等 idle' 这一行为在 Core::compact 首行可见（self.abort().await），但用户可见的等待语义（会不会卡在已 abort 的 run 上）未实测，注释只写了代码事实。
- task 提到的 docs/COMMENT_PROGRESS.md 与 tools 脚本在本仓库不存在（find 无结果），覆盖率统计无法执行。
- 磁盘曾出现 ENOSPC（/dev/vda3 100%），期间一次 Edit 报错；重试后成功。建议主代理关注磁盘水位——本次编辑若需重跑 cargo check 可能因空间不足失败。

### u327-kiana-session-large

- setup 换出/换回时第二段代码再次新建了 placeholder 用于从共享 Mutex 换回，注释说"与 in-memory fork 一致"，但未逐一核对两个 placeholder 的必要性（是否必须各建一个）——只描述了行为，未断言设计必要性。
- emit_before_fork 的注释断言"取消时 fork 不会读取条目，因此无效 entry id 不会报 InvalidEntryIdForForking"——依据是 fork 源码中 emit_before_fork 先于 get_entry 执行，但仓库内没有直接锁定该顺序的测试（只有合法 id 的取消测试）。
- teardown_current 注释中"被中断的工具调用留下持久化的 toolResult"引用了测试断言的 roles 序列（system/user/assistant/toolResult/assistant），该行为由测试锁定，但"toolResult 条目由 abort 结算产生"的机制细节（core.abort 内部如何写入）未在本次阅读范围内验证，只引用了测试名。
- copy_file_exclusive 注释保留了原有"libuv 的 uv_fs_copyfile 在 fchmod 里写入源 st_mode"断言（TS 对应关系），其上游依据（libuv 源码）不在本仓库，按规则未改动原注释也未新增断言；权限保留行为由测试 import_preserves_the_source_file_permissions 锁定。

### u328-kiana-session-large

- 模块 //! 中“kiana-interactive 与 kiana-evals 的测试/挂具也直接调用本工厂”仅核对到 grep 命中文件名（crates/kiana-interactive/tests/common/mod.rs、crates/kiana-evals/src/harness.rs），未逐文件确认调用形态；若需绝对精确可改为“测试/挂具中引用”。
- “事件类钩子静默丢弃”表述基于 run_on_session_thread_blocking 返回 Option 而调用处用 let _ = 忽略的源码事实；若派发表缺失时同步轮询失败的具体行为（agent_session.rs 的 run_local_blocking）有更细的差异，未逐行追入。
- H32 注释断言“正常流程中 stream_fn 只会在会话开始 run 之后被调用，expect 不会触发”是推断：agent_ref 在构造后立即 set，而 stream_fn 需要 agent 状态；未找到显式测试锁定“stream_fn 不会在 set 之前被调用”，谨慎起见保留了原代码 expect 语义的描述。

### u329-kiana-session-group

- 「单条注册失败循环继续（TS 同为逐条 try/catch）」中的 TS 行为引自 spec/ca-session.md §2.5.3 的伪代码注释，未读 TS 源文件本身核实
- 「诊断文案是逐字锁定的用户可见表面」中 report_diagnostics 的消费方式只在 kiana-coding-agent/src/app.rs:911 见到函数名与部分调用，未逐行核对其输出格式
- create_agent_session_from_services 文档说「sdk 侧见到 Some 即跳过重建」——已核实的证据是 sdk.rs 对 resource_loader/model_runtime/settings_manager 的 match-Option 分支各自跳过创建/加载；对 cwd/agent_dir 两个字段只确认被透传为 Some，未逐一核对 sdk 内这两个字段的全部消费点是否完全等效于直连 create_agent_session
- apply_extension_flag_values 文档说「与 TS 的行为一致（返回诊断而非 Result）」——依据是 spec §2.5.2 与当前 Rust 实现，未读 TS 源码确认 TS 该函数是否可能抛异常

### u330-kiana-session-root

- 「持久化/压缩失败记入内部错误并在 run 结束点返回」：来自 agent_session.rs//! 对 Core::persistence_error 的说明，属于模块文档自述，本次未逐行核对 start_run 的实际实现（agent_session.rs 9191 行超出本单元范围）。
- 「下游为 kiana-interactive / kiana-rpc-print / kiana-coding-agent」基于这三个 crate 的 Cargo.toml 直接依赖；是否还有间接消费者未全面排查。

### u331-kiana-session-core-large

- build_session_info_inner 使用 serde_json::from_str 解析行，而文件加载路径使用 kiana_json::js_parse；V8 能接受但 serde_json 拒绝的行（孤立代理项转义、超出 f64 范围的数字）在会话列表统计里会被跳过（同文件 parse_session_entry_value 的注释已说明两者差异）。我按现状加了注释，是否为有意的性能/依赖取舍待核实。
- append_entry 对 LeafPointer::Other（非字符串 leaf id）与 Undefined 一样省略 parentId，而 TS 的 `parentId: this.leafId` 会把该非字符串原始值写进 JSON。注释已标为能力边界；是否存在真实可达场景（append 前 leaf 为非字符串 id）待核实。
- get_tree 对重复 id 的处理可能与 TS 不同：Rust 为每个条目各建节点、仅父查找按 id 后写覆盖；TS 第二遍遍历复用 nodeMap 中后写节点，可能让同 id 的前一个节点从树中不可达。我按 Rust 实际行为注释，未修改逻辑，待审阅者确认是否需要对齐。
- project_context_entry 把“缺少 replacement 键”与 `replacement: null` 都按移除处理；TS 的 projectContextEntry 只对 `=== null` 返回 []，缺少键会访问 replacement.content。仅畸形文件可达，行为差异待核实。
- 原文件中的 `/// TS: session-manager.ts#SessionManagerError`、`#SessionListError`、`#isHeaderScanLimit` 在 TS 源 pi/packages/coding-agent/src/core/session-manager.ts 中没有同名符号（只有 SessionHeaderScanLimitError 类，且用 instanceof 判断）。按“不修改无关注释”的规则保留，锚点规范待维护者确认。

### u332-kiana-session-core-group

- cache_stats.rs 的 CacheMiss::missed_cost 原注释“定价未知时为 0”与代码及 tests/cache_stats.rs 的 NoModels 用例（input=2000、cost.input=0.002、无目录价 → missed_cost=0.002）不符，已改写为“本轮 usage 没有成本数据时为 0”并补公式；若以 TS 源注释为准需人工复核。
- //! 中的链接按 COMMENT_STYLE §4.2 的说明写成 crate::模块::条目 全限定形式，以规避 lib.rs 外层 /// 与文件内 //! 合并时的 rustdoc 作用域问题；任务禁止构建，未运行 cargo doc 验证。
- “结构体字段声明顺序 = JSON 键序”的结论依据 workspace Cargo.toml 的 serde_json preserve_order 特性与 serde 的字段序列化顺序，未用测试做逐字节验证。
- bash_execution_to_text 与 bash_execution_to_text_from_object 在 full_output_path 为 Some("") 时行为不同（前者附加空路径，后者按 JS 真值跳过）；这是阅读代码得到的行为差异，已在私有文档中标注，未改代码。
- 交互式调用方的时序可能不满足 detect_cache_miss 的前置条件：agent_session.rs 先 emit(MessageEnd) 再 append_message，而 kiana-interactive 经 mpsc/tokio::spawn_local 异步转发后才调用 detect_cache_miss（可能拿到已包含该消息的 entries）。未实测，也未修改调用方；文档只按 TS 约定写“应在消息写入会话之前调用”。
- is_successful_response 与 tool_result_content_json 在仓库内 grep 不到调用方（仅定义），文档未声称具体调用方用途。

### u333-kiana-session-core-group

- 疑似与 TS 口径不一致（未运行测试验证）：SessionEntry::Compaction 的 usage 字段用 deserialize_lenient_option 反序列化，残缺 usage 对象会被吞成 None 且条目仍是 Compaction；get_usage_cost_breakdown 的 Compaction 分支只读 typed usage，因此不会像 TS 那样从原始 entry.usage 统计。branch_summary 因同一位置没有 lenient 属性，解析失败会退化为 Unknown 并走原始字段路径，故其残缺 usage 测试能通过。证据：session_manager.rs 的 parse_entry_object/CompactionEntry.usage 属性与 usage_totals.rs 的 Compaction 分支。
- NestedCallRecorder 同时派生 Default 与提供 new()：new() 的 complete=true，而派生的 Default 的 complete=false（空记录时 snapshot 会返回 Some{calls:[], complete:false}）。仓库内未发现 Default 的使用，但它是公开 API。已在文档中提示“构造请用 new()”，未改代码。
- UsageNumbers::from_json 用 as_f64 读取字段，非数字（如字符串 "5"）按 NaN 处理；TS 的 number 运算在遇到字符串时会做字符串拼接而非 NaN。该差异只在畸形非数字字段时可见，仓库内无对应测试，现有注释已把语义概括为 undefined→NaN。
- session_cwd 的检测只调用 Path::exists；std 文档称元数据读取失败（如权限不足）时返回 false，会把不可访问路径报告为缺失。该行为无仓库内测试覆盖，因此文档只写“只判断路径存在、不检查是否为目录”，未把权限语义写成事实。
- 文档称 breakdown 为稳定排序；依据是仓内测试与 spec/ca-config-models.md §2.30.2（要求 Rust 使用稳定排序），未在本环境运行测试复核。

### u334-kiana-session-core-group

- public_total 采用 docs/COMMENT_PROGRESS.md 的口径（pub fn/struct/enum/type/const/mod 行），本文件实测 21 项且全部有文档；另有 29 个公开结构体字段也已有 ///，未计入该数字。
- 任务禁止运行 cargo doc，因此未验证 rustdoc：lib.rs 对本模块存在外层 ///，按规范 //! 内链接写成 crate::virtual_models::… 全限定形式，仍需最终 RUSTDOCFLAGS=-D warnings 复核无 broken-intra-doc-link。
- with_virtual_models 的 filter_all_models 总是覆盖实现，而 TS 仅在存在 filterAllModels 时才安装；内层默认实现语义相近且 has_filter_all_models 转发内层值，该结构差异未见本 crate 测试覆盖。
- ModelRouteReason 文档中 Direct 的使用位置（Models::stream_simple 与压缩摘要）依据调用方源码 crates/kiana-config/src/models/runtime.rs 与 crates/kiana-session/src/agent_session.rs，不在本 crate 测试范围内。
- VirtualModelDefinition::route 的并发说明（ModelRuntime 克隆 Arc 后在锁外 await 回调）依据 runtime.rs 的取值顺序与 Fn + Send + Sync 类型，仓库中未见并发测试验证。

### u335-kiana-session-core-root

- lib.rs 中 9 个 pub mod 上方本来就存在外层 ///（TS: ...）；按任务说明的 rustdoc 作用域陷阱，这些子模块 //! 内的短链接可能在父作用域解析失败。任务禁止运行 cargo doc，我无法确认当前是否已有 broken-intra-doc-links 警告；该问题不在本单元可改文件内，未改动。
- “SessionManagerError 的 Display 与 TS error.message 逐字一致”来自 session_manager.rs 中枚举自身的文档注释，我未在运行期对照 TS 输出验证；个别经 ? 直接转换的 std::io::Error 走 Io 变体，其文本是否为 Node 格式未实测。
- “首次落盘要求已有 user/assistant 消息（只有 setup 条目的会话不建文件）”来自 session_manager.rs 模块文档的写路径说明，未运行测试验证。
- kiana-coding-agent 的“会话列表查询”依据 app.rs 中 SessionManager::list/list_all/list_all_in 的调用与 Cargo.toml 依赖，未通读其 UI 层。

### u336-kiana-telemetry-large

- 仓库没有基线提交（git log 为空、文件为 untracked），且任务提到的 tools/check_comment_only_diff.py 在本仓库不存在，因此无法用工具机械校验“仅注释差异”；本次全部 Edit 的 old/new 串中只有注释与空行（两处原有注释修正见下），代码行按构造未变。
- 按任务硬性约束未运行 cargo doc / fmt / check / test；新增 intra-doc link 未经 rustdoc 编译验证，已人工确认公开文档中的链接只指向公开项（私有链接仅出现在私有项文档中）。
- 关于 TS 侧行为的论断（extract 未识别时 return this、惰性 span 不是 NOOP 单例、spec §22.5/§28.2 的错误文本等）沿用仓库现有注释、测试 fixture 与 spec 引用；本单元未直接阅读 packages/telemetry 的 TS 源码（该目录不在本 Rust 仓库内），如需逐字核对应回查原 TS 仓库。
- 修正了两类原有注释与代码不符之处：① 原注释把实现四契约的实际类型 RecorderIdentity 写成 Recorder（Inner.recorder 字段文档与 RecorderIdentity 结构文档各一处），依据是字段类型 OnceLock<Arc<RecorderIdentity>> 与 trait 实现位置；② SystemClock 原写“进程启动以来的 Instant”，但 MONOTONIC_ORIGIN 是 LazyLock，实际原点是首次读取时刻，已改写并说明只用于求差。
- public_total=51 采用本单元口径：7 个公开类型/trait + 24 个公开字段 + 15 个 inherent 方法 + 3 个自由函数 + 2 个 Clock trait 方法；未重复统计 trait impl 方法（它们继承对应 trait 的文档）。若上级使用不同盘点工具，数字可能不同。
- complex_blocks_explained=18 的口径：带多阶段、并发、失败模式或 JS 语义边界说明的块（span 创建/结算/取消、锁与注册表、RecorderIdentity 降级、指标记录、store 投影排序、cursor 链、属性严格相等等）。
- 未运行 cargo test --doc；本次没有新增任何 ``` 代码块或可执行示例，不存在新增 doctest 失败风险。

### u337-kiana-telemetry-large

- TracePage.traces[*].spans 按 startedMs（同刻按 span id）升序是否属于所有 store 适配器的正式契约：本套件的 first_span_names 依赖该顺序，但依据只在 memory 实现与 spec §8 的断言映射中找到，未见契约明文。
- set_now 为 no-op 时第 6 条指标 case 也会失效：这是从该 case 固定的 20_000/30_000 桶查询与 set_now(20_001) 推出；原字段文档只点名第 1/2/3 条。
- 工厂每次调用必须返回相互隔离的 fixture：由 remote_ids_case 同时持有 sender/receiver 并断言 receiver 中恰有一个继续 span 推出，testing 模块文档也写「隔离」，但本模块没有契约明文。

### u338-kiana-telemetry-large

- 未运行 cargo check/doc/test（任务明确禁止），因此 intra-doc link 与 rustdoc 警告未实测；仅人工核对所有 [`...`] 均指向本模块可见的公开项（TelemetryContext、create_telemetry_adapter_conformance、same_allocation、json_roundtrip_carrier、TelemetryAdapterFixture 的字段与方法）。
- `sync-error` 与 `async-error` 在 Rust 中形态相同，注释称其对应 TS 的同步 throw / 异步 reject，依据是 spec/telemetry-mcp.md §7.3 第 10 条与代码结构，未读取上游 TS 源文件核对。
- `metrics_copied_case` 与 `links_initial_and_late_case` 中“事后修改本地数组”的步骤在 Rust 所有权下不可能被实现观察到；注释已如实标注，但无法从仓库确认这是有意保留的移植结构还是遗留。
- `IndexMap` 的 `PartialEq` 忽略顺序沿用了文件原有注释，未在本仓库内重新验证（属 indexmap 上游行为）；recording case 的键序断言依赖该结论。
- `parentage_case` 的结算顺序（second < first < parent）依赖 tokio 调度与信号量 gate，仅按源码推导，未运行测试验证。
- `json_roundtrip_carrier` 的“往返必然成功”与 `Waker::noop` 语义分别依赖 kiana_json 实现与 std 文档，未在本任务中运行验证。

### u339-kiana-telemetry-group

- 既有注释称 SpanContextId“供 SpanLink 与 store 查询使用”，但 TraceQuery/MetricQuery 等公开 store 查询签名中没有直接使用该类型；实际用途是 SpanLink.context 与 TelemetryContext::span_context_id。原文保留未改，需确认原意。
- StoredMetric.kind 是 Option<MetricKind> 而 TS 的 kind 必填，未找到该 Rust-only 形状的设计依据；已只按可验证行为记录（内存实现总为 Some，json::stored_metrics_to_value 把 None 渲染为空字符串），理由待核实。
- jsjson 对对象 ToPrimitive 的极端差异（JSON 对象含自有、不可调用的 toString，如 {"toString":1}，V8 抛 TypeError 而本实现返回 "[object Object]"）已用 Node 26.12.1 实测，但仓库测试未覆盖；是否补测试或接受为已知偏差需主代理决定。
- 既有注释中的 V8 实测数值（JSON.parse 约 100 万层、Array#join 约 4650 层抛 RangeError）本次未重新测量，保留原文未改。

### u340-kiana-telemetry-group

- TS 源码不在本仓库内；TS 侧字段顺序/行为依据现有注释、spec/telemetry-mcp.md 与黄金文件，未逐行对照 packages/telemetry 源码。
- is_js_whitespace 的字符集合按当前实现描述为 ECMAScript WhiteSpace+LineTerminator；未按目标 ES 版本逐项核对历史上 U+180E 等差异。
- 本仓库只有内存适配器与 no-op 实现；第三方/真实后端对 InstrumentOptions、HistogramOptions.boundaries 的分桶与校验行为未核实，文档只陈述契约与内存实现的可观察行为。
- RecordedTelemetrySpan.parentId 在外部父级或同 scope 非数字 id 时都落为 JSON null，依据 memory.rs 与现有 fixture/测试；本单元禁止运行 cargo test，未重新执行验证。

### u341-kiana-telemetry-group

- 模块文档原有的“20,000 组随机向量”与 memory.rs 注释中的“4000 组随机用例”数字不一致；仓库内可见的 tests/fixtures/sort_vectors.json 实为 104 组，tests/js_sort.rs 只断言 >= 100。三种数字无法互相印证，按原文保留，待主代理核实。
- `sort_by_js` 中的 `expect("排序排列覆盖每个下标一次")`：在所有不一致比较器输入下 `order` 是否恒为 `0..len` 的排列未做形式化验证；若 `read`/`write` 的越界降级真的触发（越过 V8 也会越界的边界），可能破坏该不变量并触发 panic。模块原有“对所有比较器都不 panic”的承诺依赖这一前提。
- 仓库内没有 V8 third_party/v8/builtins/array-sort.tq 原文，GallopLeft/GallopRight 的下界/上界语义、`Smi` 中间量等说明依据本文件已有注释与代码推导，未能逐行对照 V8 原始 Torque 源码。
- 原模块文档中的 [`copy_run`] 指向本文件不存在的函数（当前只有 `read`/`write` 与 `copy_*` 系列）；已按“原注释与源码矛盾”改为行内 `copy_*` 并保留其余原句，依据是当前源码无 copy_run 定义。
- `items.reserve(len)` 在 `drain(..)` 之后是否必要取决于 `Vec::drain` 保留容量的实现细节；未对该行添加解释，也未改动。
- `sort_by_js` 没有直接的单测入口：tests/js_sort.rs 通过 render_trace 间接覆盖排序（104 组黄金向量 + NaN 不 panic），stored_spans_impl / find_traces_impl 的排序仅由 reviewer_fixes.rs 的 NaN 场景间接覆盖。

### u342-kiana-telemetry-root

- 原有注释“进程级身份注册表（memory.rs）只做身份查询，不参与传播语义”与 memory.rs 中 inject/link/exemplar 通过注册表解析 carrier 的实现看起来存在张力；按“不改写原有注释”规则保留，未列入本次修改，建议复核原意。
- 原注释“共享约定见 ARCHITECTURE.md §4”：根 ARCHITECTURE.md §4 确实是“共享约定”，但 docs/ARCHITECTURE_OVERVIEW.md §4 是“核心数据模型”，两处同名文档易混淆；原句未改。
- 本次新增的“调用方未配置遥测时常用 no-op 作为回退后端”依据 kiana-chord/src/context/index.rs 的 unwrap_or_else(noop_telemetry_context) 与 kiana-durable harness 的 noop 使用点，未逐一审计仓库内全部调用方。
- 未运行 cargo doc/check/test/fmt（任务明确禁止），新增 intra-doc 链接（如 TelemetryContextExt::start_span_typed、render::render_trace）只做了静态核对，零警告需主代理执行 cargo doc 确认。
- public_total=8 的计数口径：只统计本文件声明的 8 个 pub mod；4 组 pub use 是对其他文件中已文档化项的再导出，不重复计数，其文档由被导出项继承。
- “根只再导出 memory::start_span_try，而 memory::start_span_typed / memory::start_child_span 留在模块内”是对当前 lib.rs 的事实描述；这种不对称是否是有意设计，仓库中未找到说明。
- 原注释引用“README 的原文承诺”，本 Rust crate 没有自己的 README；该承诺实际来自上游 pi/packages/telemetry/README.md（“Telemetry is diagnostics, not durable application state”），既有注释保持原样。

### u343-kiana-theme-large

- 未阅读 spec/*.md 与 ARCHITECTURE.md 中 system-theme 的对应章节；文中 TS 语义均来自本文件既有注释、tests/system_theme.rs 与 theme.rs 调用方，若 spec 措辞不同以 spec 为准。
- “t = 2 时求解必然完成”是按 target() 公式推导：乘子为 0，目标等于 surface 亮度；前提是每条规则至少有一个 surface（当前 RULES 满足）。若未来出现 on 为空的规则，fold 会得到 ±Infinity，该结论不再成立。
- kiana-tui 对 NaN 饱和度“按灰阶处理、不 panic”的说法来自 crates/kiana-tui/src/colors.rs 文档与 oklab.rs 的 okhsl_to_rgb 实现阅读，未运行验证；本文件在调用前已显式 panic，不影响该路径。
- 未执行 cargo check/doc/fmt/clippy（任务禁止）；Rustdoc 链接仅人工核对，均指向本模块公开项，但未经过 cargo doc 验证。

### u344-kiana-theme-large

- theme_tokens.rs 的模块注释称 token 声明顺序决定 Theme::colors 的迭代顺序；theme.rs 的实际实现是“主题文档 colors 键序，前景段在前、背景段在后，fallback/默认 token 追加末尾”（Theme::new 的 merge/split/add_token 与 colors()）。内置 dark.json 的 colors 按声明顺序交错书写，因此两者并不一致；未改 theme_tokens.rs，请在主代理汇总时核对。
- Theme::colors 的缓存键只有 Arc<TerminalColors> 身份，但其中“猜测的终端默认色”还依赖 appearance() -> get_terminal_theme() -> terminal_color_scheme()。当主题未声明外观且具体颜色全是 ANSI 0-15 时，单独调用 set_terminal_color_scheme 不会使缓存失效，可能返回旧猜测色；该场景未被测试覆盖，代码未修改。
- get_custom_theme_infos 用 Path::exists() 判断目录存在；std 在 stat 失败（例如父目录不可读）时也返回 false，会把这类错误当成“目录不存在”静默返回空表，而不是像 Node 那样抛 EACCES。源码中无法确认这是否为有意取舍，未实测。
- node_error_code_and_description 的 raw_os_error 数字表注释标明为 Linux errno；非 Linux 平台（如 macOS 的 ELOOP/ENAMETOOLONG 编号不同）未验证，文档未对平台差异作承诺。
- Theme::style 对 dim token 会无条件把 attributes.dim 置为 Some(true)，覆盖调用方传入的 Some(false)；已在文档中如实描述，但未确认 TS 是否同样的优先级。

### u345-kiana-theme-large

- js_object_entries() 的字符串分支按 Rust char（Unicode 标量）枚举，而 JS Object.entries 按 UTF-16 code unit 枚举；该分支只在 colors 为字符串这类未校验异常输入上可达，当前都会先撞上未知 token 的 slot TypeError，但没有测试覆盖非 BMP 字符，逐字等价未实测。
- js_string_of() 的数组分支对 null 元素会输出 "null"，而 JS Array#toString/join 对 null/undefined 元素输出空串；其唯一调用点是 json_var_lookup() 的 in 运算符 TypeError 文案，而数组在此之前已被单独匹配，因此该差异当前不可达，也未写测试。
- color_from_resolved() 用 kiana_tui::colors::format_number 拼数字，该函数对非有限值写 "null"（JSON.stringify 语义）；若 TS indexedColor() 的模板字符串用的是 Number#toString 的 NaN/Infinity，非有限输入下文案会不同。但 JSON 文档无法携带非有限数（kiana_json 把 1e999 映射为 null，见 theme.rs 测试），该分支未实测。
- validate_theme_json() 中“name 含 / 的预检先于 schema 校验”的依据是代码顺序与测试名 rejects_names_with_slashes_before_schema_validation；TS 源文件不在本仓库，无法直接对照原始实现。

### u346-kiana-theme-group

- assets.rs 与 theme_tokens 文档中“与 TS 的 dark.json/light.json 逐字节一致”沿用原注释，本仓库没有 TS 源码可逐字节比对，未实测。
- theme_tokens 文档称“对外发布的 JSON Schema 的属性顺序”取自原注释；仓库内没有生成的 theme.schema.json 文件可核对（theme_schema.rs 只做手写校验）。
- theme_controller 模块文档称监听器在 kiana-tui 的 handle_terminal_input 内同步调用：依据 tui.rs:3288/3298 与 startup_ui.rs 的注释确认；但“事件循环每次输入后调用 wait_for_terminal_colors/pump”的契约在生产调用方未完全落实——kiana-interactive 只在启动路径调用 wait_for_terminal_colors（interactive_mode.rs:2909-2910），全仓未搜到 controller.pump( 的调用点。该契约保留为“应当”，未改代码。
- start_query 的 HostRef::None 分支（theme_controller.rs:820 附近）在当前调用图中不可达：两处调用（apply_from_settings、begin_deferred）都传 HostRef::Host。只作为可能的防御性残留记录，未改动。
- set_theme_name 只用 theme_name == SYSTEM_THEME_NAME 判断 auto-sync（theme_controller.rs:704），传入 auto 形式 "light/dark" 会被当作具体主题名并关闭通知；是否符合 TS 语义未核实，未写入文档。
- description 字段“运行期无读取点”的结论来自全仓 grep（kiana-theme/src 与 tests 中无 descriptor.description 读取）；Rust 无反射，但若未来宏/生成代码读取则结论会变，仅作现状记录。
- set_theme(..., true) 每次应用主题都会调用 start_theme_watcher（theme.rs:1692-1701）；start_theme_watcher 对重复调用的幂等/开销未深入核实，因此文档只写“启动 watcher”这一调用事实。

### u347-kiana-theme-root

- 公开项计数口径：lib.rs 的 8 个公开项按 7 个 `pub mod` + 1 个 `pub use crate::index::*` 统计。按规范未给 `pub mod` 增加外层 `///`，模块文档来自各文件自身的 `//!`；根 `//!` 的「阅读顺序」逐一说明其职责，因此计为已覆盖。
- 未运行 `cargo doc`（任务禁止构建类命令），`//!` 中的 intra-doc 链接只做了静态核对：Theme、THEME、theme()、ThemeProxy、ThemeResult、ThemeError、ThemeJson、ThemeToken/ThemeColor/ThemeBg、validate_theme_json、set_theme_json_validator、current_theme、init_theme、set_theme、set_theme_instance、stop_theme_watcher、get_theme_by_name、load_theme_from_path、generate_system_theme_colors、InteractiveThemeController、ThemeHost 均确认在 crates/kiana-theme/src/index.rs 的再导出列表中；`[`theme()`]` 与同名模块 `theme` 的 rustdoc 解析未实测。
- crates/kiana-theme/Cargo.toml 声明了 `kiana-config` 依赖，但在 crates/kiana-theme/src 下搜索不到任何 `kiana_config` 使用；文档中没有断言其用途，建议主代理核实该依赖是否为预留（未修改 Cargo.toml）。
- 下游角色描述只覆盖本次实际 grep 到调用点的 crate（kiana-coding-agent、kiana-ui-kit、kiana-tools、kiana-extensions、kiana-export、kiana-resources、kiana-session、kiana-interactive）；kiana-ext-api、kiana-rpc-print、kiana-builtin-extensions 等仅在 Cargo.toml 中声明依赖，未逐项核实用途，故文档未提及。
- 「明暗变化触发的重新查询在下一次 wait_for_terminal_colors() 发起」来自 theme_controller.rs 的模块文档，本次未执行其异步时序；如实现变更需重新核对。

### u348-kiana-tools-large

- cwd 存在性检查用 tokio::fs::metadata（bash.rs 内），TS 用 fs.access(cwd, F_OK)；仓库未见过测试逐平台验证二者的权限/符号链接差异，注释未断言完全等价。
- parse_tool_input 对非字符串 command 回落空串、input_timeout 对非数字返回 NaN，这两条路径只会在绕过 agent loop schema 校验、直接调用 ToolDefinitionExecute::execute 时可达；未找到覆盖该路径的测试。
- kill_process_tree 的失败被 kiana-core 静默吞掉；本地 exec 在 kill 失败后是否可能永久等待未实测，注释只描述代码路径，未做性能/挂起结论。
- wait_for_child_streaming 末尾的 'child process did not exit' 分支按当前控制流看似不可达（deadline 只在 exited 后设置），仅记录，未改代码。

### u349-kiana-tools-large

- 未运行 cargo check/doc/fmt（任务禁止），新增的 intra-doc 链接（EditError::message、EditDiffError::error、LineEnding::Lf、AppliedEditsResult 等）未经 rustdoc 实测；原 //! 中指向私有 diff_lines 的链接已改为行内代码，以规避 private_intra_doc_links。
- decode_utf8 中“非法 UTF-8 按 WHATWG 替换算法变成 U+FFFD”来自 encoding_rs 的公开语义，仓库内没有直接断言该行为的测试。
- format_unified_patch 的哨兵与收尾流程说明来自静态阅读；tests/edit_diff.rs 的黄金测试覆盖输出，但本次未运行验证。
- compute_edits_diff 中 access(2) 与 read_file_bytes 之间文件状态变化窗口的描述只是两次调用的顺序事实，未做并发/竞态实测。
- restore_line_endings 对已含 CRLF 的输入会产生 \r\r\n 是逐字代码事实，但仓库未见测试覆盖这一误用场景。
- public_total 的统计口径包含 pub use ... as Edit 这一重导出（原先无文档，本次补上）；若统计工具不计 re-export，则公开项为 39。
- “compute_edits_diff 用于 TUI 预览”依据是当前唯一调用方 renderers/edit.rs；若后续新增调用方需复核该表述。

### u350-kiana-tools-large

- `strip_suffix('\r')` 紧接着 `js_trim` 看似冗余：kiana-json 的 js_trim 文档明确其字符集包含 U+000D，因此 trim 本身也会去掉 CR；源码中没有说明为何保留显式 strip（可能为 TS 对齐），未改动。
- 既有行内注释“这里的返回值永远不会被读取”（LocalFindOperations::glob）严格说只对框架默认用法成立：调用方若显式把 LocalFindOperations 放进 FindToolOptions.operations，glob 会被调用并返回空列表。类型文档按后者表述，原行内注释保留未改。
- 真实 fd 的 `--hidden` / `--no-require-git` / `--full-path` / `--max-results 2.5` 行为只有假 fd 的 argv 断言覆盖（tests/find.rs）；任务禁止运行 cargo test，未用真实 fd 实测。
- Windows 下把模式中的 `/` 替换为字符类 `[/\\]` 以同时匹配两种分隔符，这是对 glob 语法的直读推断，未在 Windows 构建上验证。
- 默认分支中 fd 遇到不存在搜索根时的具体退出码与 stderr 文本未在本仓库测试中断言，注释只写“由 fd 的退出码/stderr 体现”。
- public_total=31 的口径为：19 个顶层非字段公开条目 + 2 个 re-export 名称（FindToolDetails/FindToolInput）+ 5 个 trait 方法 + 5 个公开字段；不含 pub(crate) 的 max_safe_integer_usize/LossyLines/abortable。
- complex_blocks_explained=9 为手工计数：LossyLines/line_terminator、abortable、split_path、relative_segments、relativize_find_result_path、build_result、execute、run、run_custom；join_path 等简单函数未计入。
- 未运行 cargo doc/fmt/check（任务禁止构建类命令），intra-doc link 已人工审计：新增公开文档链接均指向 public 项，private 目标使用行内代码；仅保留基线中已有的 `tokio::io::AsyncBufReadExt::lines` 外部链接。

### u351-kiana-tools-large

- 模块文档既有句“每个 await 之后检查中止”对 read.rs 并不精确：execute 只用一个 tokio::select! 包住整个 run；该行是原有注释，未改动，建议审阅者决定是否重写。
- create_read_tool 的“占位上下文、cwd 固定、model() 恒为 None”行为依据 crate::definition::wrap_tool_definition(None) 与 MissingToolContext 源码推断（cwd 默认为进程目录、model() 返回 None），本单元未运行测试实测。
- 按任务约束未运行 cargo check/doc/fmt；rustdoc 链接仅按仓库内可见性人工核对（模块文档中原有的私有链接 [`js_slice`] 已改为行内代码，属注释级修正）。
- 非 unix 分支 access_path 的注释只描述 cfg(not(unix)) 代码路径（目录不检查 mode），本环境无法验证 Windows 上的实际行为。

### u352-kiana-tools-group

- bash_executor.rs 模块文档中原有的 [`Utf8StreamDecoder`] 链接指向 pub(crate) 项（output_accumulator.rs），按 rustdoc::private_intra_doc_links 很可能产生既有警告；按要求保留原注释未改动，请在带基线时确认不是本次新增。
- file_mutation_queue.rs 模块文档中原有的 [`Gate`] / [`ReleaseGuard`] 链接指向私有类型，同样可能是既有 private_intra_doc_links 警告；未改动。
- bash_executor：滚动缓冲丢弃旧块后返回值 output 会缺少前缀、而 full_output_path 有完整内容——该场景没有专门测试，结论来自代码路径推导（UTF-16 单元数不超过来源原始字节数，故丢弃发生时临时文件必然已创建）。
- bash_executor：full_output_path 在 truncated == false 时仍可能为 Some（例如大量 ANSI 序列被清理后文本很小），现有 golden 与测试未覆盖该组合。
- bash_executor：finish_temp_file 里 `let _ = writer.flush()` 忽略 flush 错误，因此返回 Ok 不保证临时文件内容完整；仓库内没有对应 TS 行为或测试可对照。
- edit："混合行尾文件写回后被统一"来自 normalize_to_lf + restore_line_endings 的代码路径；现有测试只验证了混合行尾的重复检测，没有写回断言。
- file_mutation_queue：非 UTF-8 文件名经 to_string_lossy 折叠成同一 key（只会多串行）的结论来自代码，仓库测试未覆盖非 UTF-8 路径。
- definition：占位上下文 mode() 固定 Tui、has_ui() 为 false、session_manager().get_cwd() 为空串，这些组合的消费方行为未在仓库中找到断言；只记录了代码事实。
- 跨文件不一致（未修改）：kiana-ext-api/src/tool_result.rs 中 BashResult.full_output_path 的文档写“输出超过截断阈值时才有”，而 bash_executor 的实际条件是“累计原始输入超过 DEFAULT_MAX_BYTES 即创建”，两者不等价。

### u353-kiana-tools-group

- ls.rs 的 const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991 在 32 位目标上无法用 usize 表示（会编译失败）；本 crate 是否仅支持 64 位未见仓库明确声明。这是既有代码，未修改，仅报告。
- ls.rs 的 js_number_text 复用 format_js_number，非有限值得到 "null"，与 JS String() 的 "NaN"/"Infinity"/"-Infinity" 不同；我已把该差异写进注释，但没有实测 serde_json 能否从 JSON 文本（如 1e999）构造出非有限 f64，因此注释只断言 NaN/+Infinity 不会触发上限分支，未断言 -Infinity 的可达性。
- grep.rs 模块首段既有注释写「通过 kiana_core::tools_manager::ensure_tool 解析」，实际调用是 ensure_tool_without_status（同一族函数，后者是省略状态回调、保持 future 为 Send 的入口）。按保留既有注释的规则未改写，仅在此记录。
- index.rs 模块既有注释中的 [`create_all_*`] 是一个带通配符的 intra-doc 链接，目标不是实际条目，可能触发 rustdoc broken-intra-doc-links；它在本轮改动前就已存在，未修改。
- ls.rs 既有注释关于 Node 26/libuv uv_fs_scandir 按原始字节序返回的说明来自上一轮作者与黄金用例，本次未独立复核 Node 侧行为，只做了保留。
- 按任务约束未运行 cargo check/doc/fmt/clippy，因此新增的 rustdoc 链接解析（如 ToolName::ALL、LsOperations::exists、path_exists）与 rustfmt 排版未实测；链接目标均已人工核对为 public。
- grep.rs 的 js_relative 与 Node path.relative 在 Windows 盘符大小写/分隔符上的差异未验证；本次注释未对平台差异作任何断言。
- ls.rs 中 limit 为 NaN 时（比较永远为假）会列出全部条目且无通知，这一行为仅由代码语义推出，未见测试覆盖；注释只覆盖已测试的 0/负数/小数情况。

### u354-kiana-tools-group

- output_accumulator.rs 原有 //! 链接 [`Utf8StreamDecoder`] 指向 pub(crate) 类型，按 rustdoc::private_intra_doc_links 会告警；已将这一处改为行内代码（内容不变），其余 pub(crate) 类型的文档链接（Self::pending 等）保留。
- ensure_temp_file 的既有注释断言 tokio File 的 try_into_std「在本进程内总是成功」；按任务约束未运行 cargo build/test，无法实测，仅保留原文。
- close_temp_file 在 flush 失败前已用 take() 取走句柄，之后无法重试 flush；文档如实描述了这一行为，但 TS 的错误处理（reject 后 stream 状态）未能在本仓库源码中核对，可能有语义差异。
- path_utils.rs 原 //! 的「只有 read 使用 resolve_read_path」与 kiana-coding-agent/src/cli/file_processor.rs（第 14、74 行调用同步版）矛盾，已改为「工具层只有 read…，crate 外 CLI 也调用同步版」，请主代理复核措辞。
- powershell 的 Windows 成功路径（POWERSHELL_ARGS、pwsh 回退）依据 kiana-core/src/shell.rs 与 tests/powershell.rs 的注释/断言，本次环境为 Linux，未实际运行 Windows 分支。
- public_total 计数口径：仅统计本文件内定义或再导出的具名公开项；output_accumulator 含 2 个错误变体与 8 个公开字段，powershell 未计入 3 个 pub use 别名（PowerShellOperations、BashToolDetails、BashToolInput）。

### u355-kiana-tools-group

- truncate_head 的 output_bytes 与循环累加值关系：按本实现（仅非首行 +1 换行）两者恒等，已用 20 万随机用例离线验证无差异；但 spec/ca-tools-compaction-export.md §8.2 #16 写“outputBytes 可能比累加值小 1”，且与同文件 §2.1.5 步骤 4 自相矛盾。已按代码改写原注释，建议主代理确认。
- shorten_path_str 对空 HOME 的边界：kiana_tui::terminal_image::home_dir 在 POSIX 上对已设置的 $HOME 原样返回（可能是空串），此时 path.starts_with("") 恒真，任何路径都会被写成 "~" + 原路径。TS render-utils 是否有空 home 守卫无法从本仓库确认（相邻的 shorten_image_path 明确有守卫）。代码未改，仅记录。
- ToolRenderResultLike 在本仓库只有定义、没有构造或消费点（grep 验证），文档按此写明；若它是供外部扩展使用的公开类型，措辞需要复核。
- render_utils 模块文档中既有的 [`str`] 链接可能解析到 Rust 原始类型 str 而不是本模块的 str 函数（历史注释，未改动）；rustdoc 不会因此报错，但读者可能跳到错误页面。

### u356-kiana-tools-group

- edit.rs 中 get_renderable_preview_input 先取 path、后取 file_path 的顺序与显示用 pick_path_value 相反；仓库内没有 TS 源码，无法确认与 TS 原实现逐字一致，文档仅按 Rust 源码描述，并注明该优先级无测试覆盖。
- mod.rs 中「同一套工厂同时服务实时执行与会话回放」的表述依据 edit.rs 既有注释、edit 回放测试（result 无 call component）与 tool_execution 的组件缓存复用；未完整追踪会话恢复/导出的全部调用路径。
- details.rs 中「非数字 maxBytes 最终文案为 NaNMB」由 format_size_number 与 kiana_ai::model_catalog::js_to_fixed 的源码共同推导（NaN 分支返回 "NaN"），未运行代码验证。
- bash.rs 的脚注去重描述为：最终结果、truncated、fullOutputPath 真值、文本以 ] 结尾、最后一个 "\n\n[" 之后包含该路径；测试只覆盖单脚注场景，多段脚注的边界行为未实测。
- 任务禁止运行 cargo check/doc/fmt/clippy，因此本次未做编译与 rustdoc 验证；所有新增 intra-doc 链接均人工核对为同模块 pub 项或公开类型（private 目标仅出现在私有项文档中）。

### u357-kiana-tools-group

- 高亮缓存“行数组一一对应”的不变量依赖 highlight_code 对输入/输出行数保持一致；该性质仅有 kiana-theme 的实现说明（空输入返回长度 1 的数组）与现有测试间接支持，本单元未运行测试验证。
- “宿主每次重新渲染都会把上一帧组件交回”依据 kiana-interactive 的 ToolExecutionComponent 源码（get_render_context/reclaim_cached_children）；扩展或其它宿主的渲染循环未检查。
- write.rs 原字段注释 `raw_path` 写的是“None 表示没有语言”，但本模块实际只在能确定语言时创建缓存（由 Default/外部构造才可能出现 None）。已在结构体级文档补充说明，未改动原字段注释，字段注释是否应重写待维护者确认。
- read 紧凑分类中“包根取决于 KIANA_PACKAGE_DIR 或可执行文件位置”来自 kiana-core config 的源码说明，未在真实安装布局下运行验证。

### u358-kiana-tools-root

- rustdoc 链接 [`ToolName`]、[`ToolsOptions`]、[`ToolNameError`] 依赖 lib.rs 的 `pub use index::*` glob 再导出解析；本单元禁止运行 cargo doc，需在文档检查阶段确认没有 broken-intra-doc-links。
- “details 形状和 JSON 键序都是对外契约”依据各模块文档与 tests/fixtures 中的黄金测试；`tool_definitions_golden.rs` 只直接覆盖工具定义，未逐个验证所有 details 形状，请审阅者确认该措辞。
- `kiana-rpc-print` 仅从 Cargo.toml 确认依赖本 crate，未核对其源码中的实际调用点。

### u359-kiana-tui-large

- set_layout_root_component 的“相同根直接返回”比较的是每次 wrap_child 新建的 SharedHandle 值地址；由于每次调用都会新建句柄且旧句柄仍存活，常规路径下该早退似乎不会命中（可能是无效早退）。未改代码，是否与 TS 的引用相等语义一致需核实。
- handle_right_click_paste 用 event.button != 2 排除修饰键，即依赖终端把修饰位（4/8/16）合入 SGR 按键码；不同终端/复用器的实际编码未在本仓库验证，注释只按代码现状写“未修饰右键”。
- prepare_kitty_screen 的 placement.replacement_line 与 transmission_generation 语义在仓库内没有文档；我只按字段名与用法描述为“同代次替换整行”，没有写成“图片数据已上传、无需重传”的结论。
- after_terminal_stop_alt 中 iTerm2 图片能力被临时关闭（before_terminal_start_alt）再恢复的原因，源码与仓库文档都没有说明；注释只描述行为，未写动机。
- TuiAltScreen::render 在无布局根时直接渲染隐式子文档，不经过主滚动视图（不裁剪/不滚动），与 TS `super.render(width)` 是否逐字一致未实测；只按代码现状描述。
- 未运行 cargo check / cargo doc（任务禁止 build/test/fmt/clippy），rustdoc 链接仅人工核对可见性；已避免链接私有项，trait 方法链接统一改用 `Tui::…` 形式。
- parse_sgr_mouse_event 中 `let mut chars` 与 `let _ = &mut chars;` 属可疑代码（未使用变量），按规则只记录、未修改。
- render_now 帧内 pending_render 合并语义（帧末只补一次 request_render）按源码阅读推断，未做运行时验证。

### u360-kiana-tui-large

- rustdoc 私有链接：本文件原有的 //! 中 [`LANGUAGE_ALIASES`]、[`REGION_ALIASES`] 以及 pub fn resolve_process_locale 的 /// 中 [`normalize_locale`] 指向私有项，按 rustdoc::private_intra_doc_links 可能已产生警告；按要求未改动这些既有注释，需 cargo doc -p kiana-tui --no-deps 验证。
- 公开项计数口径：52 = 9 个公开类型/trait/类型别名 + 14 个 pub 字段 + 2 个枚举变体 + 27 个 pub 函数/方法。若统计工具不计字段或枚举变体，则分别是 36 或 50；仓库内未找到 tools/doc_inventory.py（该路径不存在），无法用工具核对。
- 未运行 cargo build/test/fmt/clippy/cargo doc（任务明确禁止），因此编译、格式与 rustdoc 链接有效性均为按源码静态阅读自检，未实测。
- “get_file_suggestions 会阻塞调用它的任务线程”是基于 std::fs::read_dir 同步且 get_suggestions_impl 未使用 spawn_blocking 的代码事实；实际调度/性能影响未测量，仅为机制说明。
- 模块文档中既有的大量 Node/ICU 26.12.1 实测规则与 fd 行为说明是原注释保留内容，本次未重新实测；新增注释只在其上补充代码结构与调用关系。
- “skill: 两轮匹配”注释中的示例（输入 skill:idea 可命中 skill:research-idea）由 fuzzy_filter 的子序列匹配逻辑推导，未在 tests/autocomplete_skill_slash.rs 中找到该确切用例（该文件只测了 idea 与 skill:side 等）。

### u361-kiana-tui-large

- 未运行 cargo check/doc（任务禁止），无法机器确认新增 intra-doc 链接零警告；本次新增链接只指向本模块 pub 项（Editor、TextChunk、TextSegment、word_wrap_line、BorderRenderFn 等），但文件里原本就存在指向私有项的链接（如 //! 中的 [`Editor::reclaim_provider`]、[`QueuedRequest`]），这些链接的警告状态需主代理用 cargo doc 核实。
- 仓库没有基线提交，无法执行 tools/check_comment_only_diff.py 的机器校验；本次全部通过 Edit 在原代码块中插入 //、///、//! 行完成，未删除或改写任何代码行，建议主代理再用快照工具复核。
- handle_forward_delete 只按字素段删除文本、不维护 paste registry；按行/按词删除同样没有 registry 维护逻辑。已对照 /home/ctyun/code/kiana_rev/pi/packages/tui/src/components/editor.ts 的 handleForwardDelete 确认 TS 行为相同。该行为可能让 registry 与文本失配，按规则只记录未修改。
- handle_backspace 删除 marker 时直接执行 paste_counter -= 1 并依赖 id 从 1 连续这一不变量；若先经过 ForwardDelete 或按行/按词删除导致 registry 出现空洞，后续重编号/展开行为未测试，仅按源码现状描述。
- paste marker 金表（TRIGGER_PATTERN_GOLDEN）与 tests/fixtures/editor_trigger_patterns.json 的期望值均来自 TS 引擎（测试注释与 fixture 说明），本次未重新执行测试；只补充了“不要手改期望值”的说明，未改动任何数据。
- 模块文档中“返回行由左 padding + 内容 + 填充 + 右 padding 组成”等渲染描述基于 render_editor 源码；未对 width==1、自定义边框渲染器返回多行文本等极端组合实测。
- 未逐一注释约 600 行金表数据（遵循 COMMENT_STYLE §6 的生成/数据表策略），大型测试数据集无法在本次以行级注释覆盖。

### u362-kiana-tui-large

- Lock 位 64/128 对应 Caps Lock/Num Lock、以及“Kitty 与 xterm 都用这两位”的说法来自既有常量名/注释，未在本次会话中查阅外部协议文档核实。
- matches_kitty_sequence 的 base layout 回退把数字也当作可回退码点，而 format_parsed_key 把数字视为权威码点；已在注释中记录这一差异，但是否为有意设计无法从源码确认。
- matches_modify_other_keys 对特殊键做精确比较、未屏蔽 LOCK_MASK；真实终端是否会在 CSI 27 序列里带上锁定位未核实（可打印字符路径会屏蔽）。
- set_kitty_protocol_active 与 matches_key/parse_key 是否始终在同一线程调用，未逐一核对全部调用路径；文档按 thread_local 语义写成“应在同一线程”的约束。
- “目前仓库内没有使用 KEY 的调用点”基于本次 grep 快照，后续代码变化可能失效。
- is_key_release/is_key_repeat 是子串嗅探，理论上非粘贴输入只要包含 :3u/:2u 等片段就会命中；这一行为由代码直接可见，但没有对应的失败用例测试。

### u363-kiana-tui-large

- SYMBOLS 表里的 ("Kiana", "Π") 与 TS 源 /home/ctyun/code/kiana_rev/pi/packages/tui/src/latex.ts:39 的 `Pi: "Π"` 不一致；逐键比对 224 条确认这是两表唯一的键差异。疑为仓库改名残留，导致 `\Pi` 不可用、`\Kiana` 被渲染成 Π。按规则未改代码，请主代理决定是否修复。
- 模块文档中既有的私有项 intra-doc 链接（SURROGATE_HIGH_BASE、latex_visible_width、decode_output、CommandValue::prototype 等）在本次改动前就存在，可能触发 rustdoc 的 private_intra_doc_links 警告；本次新增的 //! 链接只指向公开项 render_latex / RenderLatexOptions / SYMBOLS。未运行 cargo doc 验证。

### u364-kiana-tui-large

- LayoutFrame::primary_scroll_view 的语义：原 /// 写“第一个 primary”，但代码 `if scroll_view.primary() || context.primary_scroll_view.is_none()` 对每个 primary 都会覆盖，实际记录的是遍历到的最后一个 primary（TS 规格 §2.11.3 同为该条件）。已按代码改写注释，需确认是否应反过来改代码。
- normalize_line_output 的原注释暗示 TuiBase::applyLineResets 使用它；实际 tui.rs::apply_line_resets 直接调用 crate::text::normalize_terminal_output，且全仓库 grep 不到 normalize_line_output 的任何调用方。已改为事实描述，是否保留该 pub 导出需确认。
- 原模块文档中的 [`resolve_size_value`] 指向仓库里不存在的符号（workspace 级 grep 无结果），会形成 broken intra-doc link；已替换为 [`crate::tui::SizeValue::resolve`]。任务禁止运行 cargo doc，未能实测“零新增警告”。
- replace_scrollbar_cell 在 column < 0 时会把 column_u 置为 usize::MAX，随后 `column_u + 1` 溢出（debug 下 panic、release 下回绕）。当前唯一调用方 paint_scrollbar 传入的 column 由 rect/clip 计算且布局根 clip.x=0，实际不可达；仅记录，未修改代码。
- component_key 以 Rc 数据地址作同帧缓存键，依赖“被 render 的组件在帧内始终有 Rc 持有（盒子或缓存）”；从本文件的构造路径看成立，但没有专门测试锁定地址复用。
- 未运行 cargo check/test/doc/fmt（任务明确禁止）；该文件在 git 中未跟踪且仓库没有基线快照，tools/check_comment_only_diff.py 也不存在，因此差异纯度只能靠逐次 Edit 的 old/new 仅含注释行来保证。
- StackLayoutNode.gap 是 f64，layout_component 用 `gap as isize` 向零截断；内置 Stack 在 components/stack.rs::normalize_size 里已把 gap floor 成整数，但自定义布局节点若传小数 gap，坐标会与 TS 的浮点 rect 有亚行差异（已在注释中说明，未实测）。

### u365-kiana-tui-large

- 疑似缺陷（只记录未修改）：main_screen.rs 的 TuiMainScreen::clear_children 用 `while let Some(child) = self.base.remove_child_at(0)` 循环；而 tui.rs 的 TuiBase::remove_child_at（约 2531-2539 行）在被删子树包含当前焦点时会把子树移入 DetachedFocus 并返回 None（reconcile_focus_after_removal，约 2475-2502 行）。因此当持有焦点的子项不是最后一个时，循环提前结束，剩余子项仍留在 children，与 TS 的 `children = []` 不等价。已在方法文档中标注该实际行为，建议主代理复核是否需要修复。
- 疑似偏差（只记录未修改）：TuiMainScreen::remove_child 命中持有焦点的子树时，TuiBase::remove_child_at 返回 None，组件由 base 的 DetachedFocus 接管而不会进入 detached_children；随后同一帧内 handle_mouse 只查 children 与 detached_children，因此该组件的鼠标事件会落空。TS 的 mouseLayout 持有组件引用，仍会命中。模块文档「已知偏差」只列了两处无法复现的角落，这是第三处，已在 remove_child/handle_mouse 的注释中说明。
- 既有模块文档（//! 表格）中的 [`BoundedTerminalWriter`]、[`parse_kitty_image_header`]、[`MainScreenHooks`] 指向模块私有项；若 crate 开启 rustdoc::private_intra_doc_links 会产生告警。按要求未改动既有注释，且未运行 cargo doc 验证。
- beforeTerminalStop 在移动光标前先向终端写一个空格的动机在 spec/tui-core.md §2.10.3 与源码注释中均未说明；本次只按代码顺序描述（先写空格、再按 hardware_cursor_row 算行差、最后 CRLF），未断言原因。
- Termux 高度变化路径（is_termux_session 为真时跳过高度全量重绘）与 Windows 版 os_tmpdir 分支未在本次阅读的测试中看到端到端覆盖，相关注释只依据源码与 spec，未实测。

### u366-kiana-tui-large

- blockquote 的两个续行分支用 String::len()/truncate 按字节截断 raw/text（TS 的 substring 按 UTF-16 码元）。若旧 token 的 raw/text 不是累积文本的字节后缀，keep 可能落在非字符边界导致 panic，或与 TS 的截取位置不同；本次未运行测试，无法确认是否存在触发输入（block.rs 的 blockquote 续行分支）。
- 任务项 checked 判定为 task_raw != "[ ]"，即 [x] 与 [X] 都算勾选；仓库内没有 marked TS 源，无法核对 TS 原实现是否对大小写有不同处理。
- 任务项行内队列采用「从队尾找第一个仍带任务标记的 src」，第一遍已登记全部 item，因此与当前 item 并不严格一一对应；多个任务 item/嵌套结构下与 TS 的等价性未逐例验证，现有证据只有 tests/markdown.rs 的 loose_task_item_checkbox_survives_inline_queue_repro 与 loose_task_item_keeps_checkbox_token_in_paragraph。
- indent_code_compensation 只对反引号围栏生效（lexer.rs 规则为 ^(\s+)(?:```)），这是从代码读出的行为；是否为 marked 原行为未与 TS 源核对。
- definition 的 tag 使用 Rust str::to_lowercase（全 Unicode 小写），与 JS toLowerCase 在极少数码点上可能有差异，仓库内无对应测试可验证。

### u367-kiana-tui-large

- `modern_layout` 恒为 true 的结论由 lexer.rs 中 em_strong_l_delim 的 4 个显式捕获组 + fancy-regex 0.16.2 的 Captures::len()（含组 0，saves.len()/2）推得；未运行编译或测试验证。
- `is_gfm` 恒为 true 由 lexer.rs 的 init_rules 用 PUNCTUATION_GFM 构造 em_strong_r_delim_ast（模式含 `(?!~)`）推得；未运行验证。
- 文件头原有注释称掩码供 emStrong/del 使用，但本文件中 del 不接收 masked_src，只有 em_strong 使用；按规则未改原注释，建议核实是否为历史遗留表述。
- output_link 成功路径只恢复 link_emitted 与 in_link，不恢复递归中被改动的 in_raw_block（只有嵌套链接拒绝路径恢复）；已如实写入注释，是否与 TS 原实现一致无法核实（仓库内无 marked 的 TS 源码）。
- output_link 的 link_emitted 保存/恢复机制与 TS marked 的对应关系无法核实（无 TS 源码可对照）。
- mask_blocks 假设 block_skip 的匹配至少 2 个 UTF-16 码元（否则 `[`+`]` 会使掩码变长）；从规则构造看应成立，但未逐一验证所有边界输入。
- is_prev_char_cjk 中 punctuation_cjk 分支把空白/标点/符号也判为 true，函数名与实际语义不完全一致；仅记录，未修改代码。
- link() 中 find_closing_bracket 截断后 raw/href 具体包含哪个 `)` 只按代码与 marked 公式描述，未与 TS 逐例对照。
- 按要求未执行 cargo check/doc/fmt/clippy，因此未验证编译通过与否及 rustdoc 是否新增警告。

### u368-kiana-tui-large

- markdown/mod.rs 中 Token::inline_id 的文档引用了不存在的 Lexer::run_inline_queue；该文件不在本单元范围，未修改，属既有文档缺陷。
- 文件原有 //! 中的 [`edit_first`]/[`edit_all`]/[`translate_pattern`] 指向私有/pub(crate) 项，cargo doc 可能产生 private_intra_doc_links 警告；按‘保留已有注释’要求未改动。
- ‘标准 regex crate 不支持反向引用与 lookbehind’是通用 Rust 事实，仓库内没有文字依据（仅 Cargo.toml 同时依赖 regex 与 fancy-regex）。
- build_reflink_search 注释里‘nolink 在前会把 [label][ref] 只吃掉前半段’由正则交替语义推得，仓库中没有对应测试锁定。
- JS \s 与 Rust regex \s 在 U+FEFF 上的差异依据 Unicode White_Space 属性，未在本仓库实测；JS_DOT 的四个行终止符同理。
- block_tokens 注释沿用原文件关于 marked 抛 ‘Infinite loop on byte’ 的说法，未对照 marked 18 源码逐行核实其抛出位置。
- Rules 的 [Re;4] 下标与 indent 的映射依据 block.rs 的 cached_indent_index = clamp(indent-1,0,3) 推得；若调用方映射变化需同步改注释。
- 按任务要求未运行 cargo check/doc/fmt，新增的 intra-doc 链接未经 rustdoc 验证（已确保公开文档只链接 pub 项，私有项均用行内代码）。

### u369-kiana-tui-large

- 未运行 cargo check/doc/fmt（任务明确禁止），因此无法实测 rustdoc 警告与编译结果；仅对代码行做了括号配平静态检查（{ }、( )、[ ] 均平衡）。
- `parse_display(None)` 读取 `$DISPLAY`、connect_instruction 的 Unix socket/TCP 地址细节来自 x11rb API 与 tests/native_clipboard_linux.rs 的注释/用例，未对照 x11rb 源码核实。
- X11 协议细节（GetProperty 的 length 以 4 字节为单位、TARGETS 为 ATOM[]、ICCCM 的 STRING 是 Latin-1）依据 spec/tui-core.md §2.16.4 与 tests/native_clipboard_linux.rs，未对照 X11/ICCCM 规范原文。
- 模块文档中“2000 ms deadline 先以 Err 上报”依据 tests/native_clipboard_linux.rs 的 Stall/Partial/Invalid 用例（expect_err 且 <3.5s）；连接建立阶段超时则走 open 失败 → Unavailable，二者边界是从代码路径推断的。

### u370-kiana-tui-large

- refresh_terminal_dimensions 只向自身投递 SIGWINCH，不读取尺寸；全仓 grep 未发现 SIGWINCH 处理器安装（测试只断言不 panic、kill 被调用），因此它在真实进程中的实际刷新效果未验证，已只按事实写为“只投递信号”，真正读尺寸的是 poll_resize/stdout_size。
- DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS 在仓内没有任何引用（查询串硬编码 \x1b[>7u），数值与查询串可能脱节；仅记录，未修改。
- Windows 分支（set_raw_mode 的 SetConsoleMode、win32_console、stdout_size）无法在当前 Linux 环境编译/运行验证，其注释依据源码与文件内既有 TS 对照说明写成。
- drain_input 在等待期间把读到的字节直接丢弃、只用到达时刻延长空闲窗口；SV 的 onData 是否同样不解析内容无法从本仓确认，注释只陈述了 Rust 侧行为。
- set_raw_mode 在 fd 0 是 tty 但 tcgetattr/tcsetattr 失败时仍会把 stdin_raw_mode 置为请求值；已按 Node isRaw 语义在注释中说明，该失败路径未实测。
- local_time_parts 的 localtime_r 失败回退 UTC 分支、TimerSleep 线程创建失败退化为阻塞的分支均未测试覆盖，相关注释来自代码阅读。

### u371-kiana-tui-large

- 模块文档「新线程首次使用看到默认值（无缓存、单元格 9x18）」由 thread_local 初始化器直接推得，未写跨线程实测。
- `KittyImagePlacement.estimated_decoded_bytes` 的「每像素 4 字节」仅依据本文件 width*height*4 公式与 alt_screen 的缓存配额用法，未核对 TS 原始实现。
- `ImageRenderOptions.image_id` 的「复用/替换该 ID 的图片」沿用原注释，仓库内没有 Kitty 协议规范可佐证终端侧语义。

### u372-kiana-tui-large

- 未运行 cargo doc/check/fmt（任务明令禁止），新增的 intra-doc 链接只指向本模块公开项，但 rustdoc 是否有警告未能实测。
- 模块文档中原有的“与 ICU 78.3 逐例一致”“孤立代理的分段行为与 U+FFFD 完全相同”等结论来自既有注释与测试，本次未重新验证，按要求原样保留。
- default_word_segments 中 UnhandledEngine 的 Script 记忆、CjkBreakEngine 进程级缓存等细节来自既有注释与 word_dict 的移植说明，未逐行对照 ICU4C 源码。
- elem_matches 目前仅通过 `let _ = elem_matches;` 引用，是否为待删除死代码需维护者确认；本次未改动。

### u373-kiana-tui-large

- 仓库内未找到 text_tables.rs 的生成脚本（检索 tools/ 与 *.py/*.js/*.mjs/*.ts/*.sh/*.json 中的 text_tables、get-east-asian-width、RGI_EMOJI 均无结果），无法核实再生成流程；模块文档因此只写“必须使用相同版本数据源”，未写具体生成命令。
- 各表的实际升序/互不重叠性质未用脚本逐表验证；文档把该性质写成调用方（range_contains / word_break_class / is_rgi_emoji 的二分查找）所要求的硬性不变量，而非对当前数据的实测结论。
- 文件头声明的 Node 26.12.1 / ICU 78.3 / Unicode 17.0 版本沿用原有注释与 text.rs 模块文档，未独立核对上游发布版本。
- COMMON_SCRIPT_RANGES 关于 ICU UnhandledEngine 按 Script 记忆并吞掉相邻 ー 的行为，引用自该静态项已有注释与 text.rs 的 default_word_segments 文档；本次未运行测试复验。
- 未运行 cargo check/doc/fmt（任务明确禁止运行构建命令），因此未实际确认 rustdoc 无新增警告；文档中的反引号引用均使用行内代码、未引入 intra-doc 链接，且模块为私有，风险较低。

### u374-kiana-tui-large

- TuiBase.redraw_requested 字段在全仓库只被初始化和 take_redraw_request 自增，没有读取点（grep crates/ 确认）；其用途无法从源码确认，未写断言性注释。
- MountState 的公开字段使用私有类型别名 ComponentKey（pub addresses: Vec<ComponentKey>、pub focus_addresses: Vec<(FocusTarget, ComponentKey)>），可能触发 private_interfaces 诊断；未修改代码，仅记录。
- OverlayEntry.options 允许 None 且注释称其等价 TS 的 undefined，但公开入口 TuiBase::show_overlay 总是传 Some；None 分支当前是否可达未验证。
- 未能运行 cargo doc（任务禁止写文件），intra-doc 链接仅做静态核对；TuiBase::terminal、TuiBase::images_capability、OverlayOptions::row 等字段链接与 OverlayAnchor::Center 变体链接依赖 rustdoc 的字段/变体解析，若工具链报错需改行内代码。
- TuiCore::set_focus_internal 中“聚焦存在但不可见的 overlay 会跳过 Blocked 判定”是从 Rust 分支直接读出的事实，未对照 TS 源码确认是否为有意偏差。
- TuiBase::stop 文档中的“摘 exit 处理器”实际只是把 exit_restore_registered 置 false；start 文档已补充说明，stop 原文未改。

### u375-kiana-tui-large

- “与 ICU 相同/一致”的逐函数对应（如 BytesTrie::next 负数加 0x100、fMarkSet 语义、各 divide 阶段）依据本文件模块头既有的“逐函数移植”声明与代码形状，未对照 ICU4C 源码逐条核实。
- “ICU 78.3 标准数据里没有韩语词典”转引自直接调用方 text.rs 的 default_word_segments 注释，未查 ICU 数据包。
- “SA 引擎 fSet 全在 BMP”由 text_tables.rs 的四个脚本区间与 COMPLEX_CONTEXT_RANGES 的交集推出；注意 MYANMAR_SCRIPT_RANGES 含补充平面 U+116D0..U+116E3，但它不在 COMPLEX_CONTEXT_RANGES，当前调用方不会把它放进缅文段。若上游表变化，该注释与 PossibleWord 的“码点长度==码元长度”前提需复核。
- “返回下标严格递增、映射值单调不减、末尾可达性”等由代码推理得出，未运行测试实测。
- 泰文 <=4 / 其他 SA <4 的短段门槛差异有 tests/regression_word_segmentation.rs 的测试与代码佐证，但未对照 ICU 源码确认其来源。

### u376-kiana-tui-group

- colors.rs 的 format_number 原文档声称 JS Number.prototype.toString()，但实际委托的 kiana_json::format_js_number（crates/kiana-json/src/js_num.rs:83-90）对 NaN/±Infinity 输出 "null"、对 -0 输出 "0"，按“原注释与代码矛盾可改写”规则已改写为 JSON.stringify 语义；这是否是有意与 TS 模板字符串插值（会得 NaN/Infinity）不同，未验证。
- Color::Rgb 通道越界或为负时：color_to_rgb 使用 js_round(...) as u8（Rust 浮点转整数为饱和转换），color_to_hex 对负值用 i64 的十六进制表示；与 TS 的行为是否一致没有测试覆盖，注释只写了“不校验、不裁剪”，未声明越界语义。
- colors.rs 的 color_to_rgb 现有文档链接 [`color_to_rgb_float`] 指向 pub(crate) 项，cargo doc 可能报 private_intra_doc_links；按“已有注释非矛盾不改”保留原样，未机器验证。
- rgb_to_ansi256 的灰阶分支（通道极差 < 10 且灰阶距离更近）在 crates/kiana-tui/tests/colors.rs 与 tests/regression_review_fixes.rs 中都没有直接断言，注释只描述本仓库代码，未验证与 TS 的逐字一致性。
- AltScreenSearchComponent 的导航按钮命中框按未着色文本宽度计算；若调用方提供的 NavigationButtonStyle 改变了可见宽度，命中区间会与实际绘制错位。该约束由 render 代码推导并已写入公开文档，但仓库测试只用了仅追加 ANSI 的样式，未覆盖破坏性样式。
- flash.rs 的 composite_flashes 底对齐行为来自 alt_screen.rs:4334-4338 的调用方代码，未在 flash 模块测试中覆盖（测试只覆盖到期消失与单帧）。

### u377-kiana-tui-group

- KeyIdList::Many 的既有文档注释写着“顺序即优先级”，但本 crate 中 matches 用 any() 逐一尝试、get_keys/get_resolved_bindings 只保序返回，未找到任何把首键当优先级的代码；该断言无法从仓库证实，按要求未改动，建议审阅者确认 TS 语义或删去“优先级”。
- crate::image 在仓库内没有找到任何调用方（grep kiana_tui::image 无结果）；kiana-core 直接使用 kiana_tui::native，index.rs 也只从 crate::native 再导出两个符号。该模块是否仍需保留为 TS 对齐面，需要维护者确认。
- index.rs 既有注释称“index.ts 的每个具名符号在这里都有对应再导出”，本仓库未包含 TS 源码，无法逐项核对；该断言沿用原注释未改。
- kiana-extensions::ui::fuzzy_filter_native 手写复刻了 fuzzy_filter，但分词用 Rust 的 String::trim / char::is_whitespace，而 kiana_tui::fuzzy::fuzzy_filter 用 JS 空白集合（trim_js / is_js_whitespace）。对 U+FEFF（JS 空白、Rust 非空白）与 U+0085（Rust 空白、JS 非空白）等字符，两条路径的分词可能不同；仅记录，未修改调用方。
- image.rs 文档中“get_native_module_candidates 保留只为对齐 native-module-path.test.ts 的可观察契约”来自 crate::native 模块内的既有注释，未独立对照 TS 源验证。
- get_conflicts 的顺序断言（冲突键在用户配置中首次出现的顺序）来自 rebuild 代码推导；现有测试只覆盖单个冲突项，没有多冲突排序的断言。
- score_positions 的“positions 必须小于 text_lower.len()”是从代码索引方式推导的内部前提，源码没有 debug_assert 或文档化校验；目前所有调用点都由扫描/子串搜索产生合法位置。

### u378-kiana-tui-group

- `StackAlign` 在本仓库只被 HStack 路径读取（layout.rs 的 HStack 分支、components/h_stack.rs）；VStack 分支和 Stack::render_vertical 都不读该字段。但 spec/tui-core.md §2.19.3 的说法暗示 vstack 也用 align，未修改代码，仅记录待核实。
- `LayoutComponent` trait 在仓库内没有调用方；它与 `Component::layout_node` 同名但返回类型不同（LayoutNode vs Option<LayoutNode>），具体类型同时实现两者时调用点可能歧义。文档只断言“布局器读取 Component::layout_node”，未推断该 trait 的设计用途。
- `distribute` 的候选判定与 capacity 使用条目里未经 floor 的 min_size/max_size；直接构造 StackLayoutEntry（绕过 components/stack.rs 的 normalize_size）且字段带小数时，allocate_stack_sizes 的“整数”承诺可能不成立。已写成注意事项，但未运行时验证。
- program_status 的 4096 字节序列上限来自 tests/program_status.rs 的断言；2732 是由 2048 字节按 base64 4/3 换算得到的值，未实际发送给终端验证。
- `js_cbrt` 的注释把高 32 位整数除以 3 描述为“指数估计”，这是对 FreeBSD/V8 位技巧的简化表述；未逐位推导其尾数近似误差。
- `okhsl_to_rgb` 对 saturation > 1 的“继续走第二段公式外推”是照代码分支的事实描述；极端输入下分母接近 0 的具体数值行为未验证。

### u379-kiana-tui-group

- `StdinBuffer::pending_timeout_deadline` 在整个仓库里没有调用方（grep 只命中定义）；原注释“宿主据此排程”目前没有使用证据，我只补了“纯读取、无 deadline 返回 None”，未改写原句。
- `is_complete_csi_sequence` 的 SGR 正则分支与随后的“按 ; 切三段全数字”检查语法等价，后者可能不可达（只影响可读性，不是 bug）；按规则未改代码，仅记录。
- `process_inner` 在括号粘贴起点之前的 `extract_complete_sequences` 第二个返回值（未完成的半截序列）被忽略：紧邻粘贴起点的半截转义序列会丢失。这是代码事实，已在注释中说明；是否为 TS 原行为未核实（未拿到 TS 源码）。
- `OscColorTarget` 的 `#[serde(untagged)]` 下两个 unit 变体的 JSON 形状未在本仓库测试或使用，文档只写了“没有判别字段、Palette 直接是数字”，没有断言 unit 变体的具体表示。
- `IndexedColor` / `RgbColorValue` / `OklchColorValue` 在仓库内只被 index.rs 与 colors.rs 再导出，未找到实际消费方；因此只按结构/编码意图描述，没有写用途。
- TS 行为对照（空输入发 Data("")、drainInput 默认 1000/50）依据是 tests/stdin_buffer.rs 的移植说明与 spec/tui-core.md（第 1536 行），未直接阅读 TS 源码。
- 未运行 cargo build/test/fmt/clippy/doc（任务禁止）；只用“代码行 md5 前后一致”验证了 diff 纯度，rustdoc 链接为人工核对。
- `StdinBuffer::process` 每次调用独立做 UTF-8 lossy 解码，跨调用被拆开的多字节字符会各自变成 U+FFFD；生产路径由 ProcessTerminal 的 Utf8Decoder 规避，该边界来自代码阅读，未实测。

### u380-kiana-tui-group

- 仓库内没有 TS 源码：关于 TS 侧行为的断言（Stack 不覆写 handleMouse、Stack 没有 handleInput、Box 的 truthiness、Image 语义）依据的是仓库既有注释与测试（v_stack.rs、tui.rs、loader.rs 等），未直接比对 packages/tui。
- 未运行 cargo check/doc/fmt（任务明令禁止任何 cargo 命令），因此 rustdoc 链接（含字段链接 [`ImageOptions::image_id`]、[`CancellableLoader::on_abort`]）与 cargo fmt --check 仅做人工核对；若 rustdoc 不支持字段链接需改为行内代码。
- HStack::handle_mouse 只按 y 区间路由、x/width 原样下发；主屏普通组件树中嵌套 HStack 的按列命中可能缺失，是否与 TS 完全一致未实测（备用屏布局派发确实会跳过带 layout_node 的组件）。
- BoxComponent 以组件地址作为身份：零尺寸（ZST）组件的多个实例地址可能相同，且被释放子项的地址理论上可被新子项复用；仓库内未见 ZST 组件，本次未实测。
- CancellableLoader 在令牌已取消后再次按取消键仍会重复调用 on_abort（代码事实）；TS 是否同样重复回调未验证。
- Image::render 中 render_image 返回 None 的分支在单线程、能力缓存不被中途重置的前提下几乎不可达，注释只描述为与“不支持图片”相同的文本兜底路径，未断言可达性。
- Image 的能力探测（get_capabilities 的环境变量/缓存规则）未展开，只在必要时引用 terminal_image 的公开函数。
- BorderedLoader 把 signal 交给异步任务的行为在 kiana-interactive/src/bug_report.rs 等处可见，但未穷举全部调用方。

### u381-kiana-tui-group

- loader.rs 原有的“未安装定时器时（默认 [`crate::tui::NoopTimer`]）”与代码不符：Loader::new 里是 timer: None，从未构造 NoopTimer。已改为“timer 为 None，行为等价于 NoopTimer”，依据是构造器字段初始化。若主代理认为该句是刻意简化，可回退。
- loader.rs 中 Loader::set_timer 原注释称“生产用 tokio 定时器”，但仓库内未找到 tokio 版 Timer 实现（可见的是 kiana-extensions 的 JsIntervalTimer 与各测试 FakeTimer）。按“不改既有注释”原则保留，待核实。
- loader.rs 模块文档中既有的 [`Loader::sync_display`] 与 input.rs 模块文档中既有的 [`utf16_slice`] 都指向私有项，公开文档里可能触发 rustdoc::private_intra_doc_links；它们属于改造前内容，未改动。本次新增的 intra-doc 链接均已逐一核对为 pub 项。
- mouse_region.rs 文档称“父级分发者投递时已把 x/y 换算成相对本区域坐标”，依据是 TuiMouseEvent 的字段说明与 BoxComponent::handle_mouse 的 child_event 重定位实现；未逐一检查所有分发路径（layout/alt_screen 等）。
- Input::render 的 placeholder 分支不会重置 rendered_start_column（与 TS 相同），已在注释中记录；没有编写用例验证该旧值对后续点击的影响，只是从“value 为空时命中循环不执行、cursor 恒为 0”推断其无影响。
- Loader::tick 文档中“渲染过一次后同一帧内再调用返回 false”只在组件自身测试 tick_reports_when_the_frame_has_advanced 的调用方式下成立；若宿主传入与最近一次渲染不同相的 now，结果可能不同，未进一步实测。
- input.rs 的 yank_pop 中 insert_start 与 insert_end 在数学上恒相等（js_slice_end/js_slice_start 对正负下标的归一化结果一致），代码比 TS 多了两次切片计算但语义等价；仅记录，未改动代码。
- public_total 的统计口径为 pub 结构体/公开字段/类型别名/常量/固有 pub 方法，不含 trait impl 方法与 pub mod；mod.rs 的 17 个 pub mod 按规范无 ///，故 documented 为 0。

### u382-kiana-tui-group

- 既有注释声称 Node 对不在 [1, 2^31-1] 的 setTimeout delay 一律按 1 ms 排程；仓库内只有 tests/layout.rs::extreme_scrollbar_hide_delays_do_not_panic 的 no-panic 回归间接支持，未实测 Node 行为。
- spec/tui-components.md §2.12 写 setSelectedIndex 空列表结果为 -1，但其自身给出的公式与 Rust 实现（先 min(index, len-1) 再 max(0, …)）都得到 0；本单元按代码事实注释为 0，未改代码。
- 未运行 cargo doc/check（任务禁止），intra-doc link 仅按可见性与导入手工核对；scroll_view.rs 模块文档里既有的 [`crate::components::stack::SharedHandle`] 指向 pub(crate) 项，可能触发 private_intra_doc_links，属原有内容未改。
- 公开项计数口径为 pub 函数 + pub 类型（struct/enum/const/type/use）+ pub 字段，不含 trait impl 方法与枚举变体；仓库缺少 tools/ 目录与 doc_inventory.py，无法与工具口径对齐。

### u383-kiana-tui-group

- Spacer::line_count 对超大有限值“浮点转整数饱和到接近 usize::MAX”的说明基于 Rust 语言语义（源码里的 `as usize`），仓库内没有覆盖该输入的测试。
- handle_container_mouse 关于 gap 行导致命中区域相对画面上移、以及 HStack 忽略 x 的结论，是对照外部 TS 副本 /home/ctyun/code/kiana_rev/pi/packages/tui/src/tui.ts 的 Container.handleMouse 与 v-stack.ts 的渲染顺序得出的；TS 源不在本仓库，若该副本非权威版本需复核。
- kiana-extensions 的 JS 宿主如何驱动 SettingsList/Text/Spacer，仅按 grep 到的 class 定义与调用点（src/ui/mod.rs 的 JsSettingsList/JsText/JsSpacer 与少量 with_padding 调用）确认，未逐行阅读全部桥接方法体。

### u384-kiana-tui-group

- TruncatedText::render 中 `self.padding_x * 2` 是普通 usize 乘法，而 JS 绑定 kiana-extensions 的 non_negative_floor 用 `value.max(0.0).floor() as usize` 会把超大有限 f64 饱和为 usize::MAX；若脚本传极大 paddingX，开启溢出检查的构建可能在乘法处 panic。未实测，也未核对工作区 profile 的 overflow-checks 设置；未改代码。
- TruncatedText 在 width <= padding_x * 2 时内容行可能宽于 width（available_width 被夹到 1 列，左右 padding 照常输出，右侧 saturating_sub 归零）。该结论由源码推导；spec/tui-components.md §2.5 描述的 TS 算法相同，但仓库中无 TS 源码可核对，测试也未覆盖该边界。
- VStack 鼠标命中“gap 行不空出、从下一个子项第一行起参与命中”的结论，来自 stack.rs 的 handle_container_mouse 实现与 spec/tui-core.md §2.9.4 对 Container.handleMouse 的描述；未找到直接锁定 VStack + gap 鼠标行为的测试。
- 并行工作单元在本任务期间也在修改 crates/kiana-tui/src/components/stack.rs（render_vertical 等行号发生移动）。我按当前内容复核了行为未变，但未运行任何构建/测试；注释中的 Stack::render_vertical / handle_container_mouse 名称以当前源码为准。
- src 中未写入扩展运行时信息：crates/kiana-extensions/js/ui/tui.js 另有一份 JS 版 VStack，扩展脚本不经过本 Rust 类型（已读源码确认，但属于非直接调用方，仅记录于此）。

### u385-kiana-tui-group

- 未运行 cargo check/doc/fmt（任务明令禁止），新增 intra-doc 链接未经编译器验证：字段链接 [`MarkdownOptions::transform`] 以及 [`Lexer`]、[`wrap_text_with_ansi`]、[`is_image_line`]、[`get_capabilities`] 这类导入名短链接按 rustdoc 的作用域解析推断为可用。
- 公开项统计口径：按 3 个类型别名 + 4 个结构体 + 26 个公开字段 + 6 个固有公开方法 + 3 个 Component 实现方法 = 42 项计；仓库中未找到 docs/COMMENT_STYLE.md 提到的 tools/doc_inventory.py，无法按官方口径复核。
- 模块文档列出的调用方来自 grep 到的构造点（kiana-interactive 的 user_message.rs、commands/misc.rs、render.rs，kiana-extensions 的 ui/mod.rs、kiana-theme 的 get_markdown_theme），未逐个运行验证运行时行为。
- with_options / with_default_text_style 不清理渲染缓存（源码事实，已在文档中标为使用注意）；这是否为有意设计需对照 TS 源确认，本仓库不含 packages/。
- 既有注释“解析（token 缓存只依赖源文本）”保留未删；实际键是 transform 且制表符归一化后的文本，已在紧随其后的新增注释中澄清，两者合读无歧义。
- 测试文件 tests/markdown.rs 头部写 92 个 TS 用例，但当前有 104 个 #[test]；模块文档只写“逐用例对照”，未写数量以免不准确。

### u386-kiana-tui-group

- 未运行 cargo doc（任务禁止写文件）：新增的 enum variant 链接 [`crate::markdown::Token::List`]、[`crate::markdown::Token::Table`] 与重导出链接 [`Lexer::new`]/[`Lexer::lex`] 能否在 -D warnings 下零警告未实测。
- mod.rs 原有 //! 中的 [`lexer::utf16_len`]、[`lexer::translate_pattern`] 指向 pub(crate) 项，可能触发 private_intra_doc_links；按“保留已有注释”要求未改动，仅记录。
- list.rs 中“嵌套列表最终每行不超过 width”是按递归宽度计算推导的，未找到直接断言该点的单测。
- trim_partial_closing_fences 中“长度达到开围栏的末行必是闭围栏、已不在 text 内”的说明依据 fences 正则行为推导，未找到专门覆盖该分支的测试。
- TableCell::header/align、TableToken::align 不被渲染器读取的结论来自本 crate 的 grep；若下游 crate 直接读取这些字段则不成立。

### u387-kiana-tui-group

- darwin.rs 与 win32.rs 的说明主要来自源码与 spec/tui-core.md §2.16.5/§2.16.6；当前环境是 Linux，两个后端无法编译或真机运行，AppKit 重编码、Win32 剪贴板打开/所有权等平台行为未实测。
- ‘work 在写回共享状态前 panic 会使 busy 永久为 true’ 是由 ClipboardWorker::run 与 ClipboardJob::drop 的控制流推得，仓库中没有对应测试。
- Darwin 文本/文件路径使用 from_utf8_lossy、Win32 文本使用 from_utf16_lossy 的 U+FFFD 替换路径是按代码行为描述；正常剪贴板内容通常不会触发，未构造数据实测。
- ‘module_path 在仓库内没有真实调用方（只有测试与再导出）’ 基于 crates/**/*.rs 的 grep，不覆盖仓库外下游使用者。
- dib_pixel_offset 中 BITMAPCOREHEADER、BI_BITFIELDS/BI_ALPHABITFIELDS 的偏移规则与 spec 的逐字伪代码及代码一致，但未用真实 DIB 数据验证。

### u388-kiana-tui-root

- ARCHITECTURE.md §5 的 `native/linux_x11.rs` 行称“无 DISPLAY 返回 Unavailable”，但源码 native::linux_clipboard() 在 DISPLAY 缺失或为空时返回 None（crates/kiana-tui/src/native/mod.rs:212-216），Cargo.toml 的注释也写“无 DISPLAY 时仍返回 None”。注释按代码写 None；文档不一致未修改，留给主代理核查。
- “14 个下游 crate”取自 ARCHITECTURE.md §2.2 的依赖边，并用各 crate 的 Cargo.toml 逐条核对（kiana-core/config/theme/ext-api/ui-kit/export/tools/extensions/builtin-extensions/resources/session/interactive/rpc-print/coding-agent）；新增或移除依赖后该数字会过时。
- “ProcessTerminal 忽略输出错误（write_raw 丢弃 io::Result）”与“主屏超宽行/文件系统错误走 panic!”转述自 terminal.rs、main_screen.rs 的模块/类型文档，本次未运行程序实测。
- “Tui::start 进入 raw mode、开启括号粘贴、发起键盘协议协商”依据 TuiBase::start 的代码与 terminal.rs 模块文档/ProcessTerminal 实现描述，未在真实 TTY 上验证。
- 复杂代码块只有 `pub use crate::index::*;` 这一处；其原有 4 行解释保留未改，新增的 //! 从“阅读顺序”和“一次典型调用”两处交叉说明其作用，因此 complex_blocks_explained 记为 1。

### u389-kiana-ui-kit-large

- jsdiff diff@8.0.4 的 word.js / base.js 源码不在仓库（无 node_modules），注释中「逐条对应 jsdiff 分支」依据文件头既有移植说明与 spec/ca-interactive-components.md §2.57/§6.3，未对照 npm 原文。
- 未运行 cargo check/test/doc（任务禁止）。括号/花括号已只读配平检查，但 intra-doc link（[`ThemeColor`]、[`render_diff`]、[`RenderDiffOptions`]）未经 rustdoc 实测；三者均为公开项。
- 「输出行数等于 split 元素数」「公共+added 可精确还原新文本」「removed 侧不保证逐字节还原旧文本」来自代码推理与对 TS 黄金样张的只读比对，未在 Rust 侧执行单测验证。
- Rust char::is_whitespace 与 JS \s 的 U+0085/U+FEFF 差异依据 Unicode White_Space 属性及 spec §6.3 记载，未在本机写程序实测。
- 「只剥第一个 changed 片段的前导空白是为了避免缩进被反显」等设计原因取自 TS diff.ts 的源注释，不是 Rust 代码注释；「arena 避免 Rc/RefCell」「剪枝目的」等属于对当前实现的解释，未能从上游原文逐字核对。

### u390-kiana-ui-kit-group

- 未运行 cargo check/doc/fmt（任务明确禁止）；新增/沿用的跨 crate 短链接（Text、Container、Theme、Timer、StyleFn、Component）是否触发 rustdoc 警告未实测，写法与本 crate 既有文档一致。
- public_total 只统计 pub 类型、pub 字段与固有 pub 方法（index.rs 为纯 pub use，记为 0），不含 Component trait impl 方法与枚举变体，可能与 doc_inventory 的统计口径不同。
- truncate_to_visual_lines 对『纯空白文本返回 0 行』与『每行补齐到 width』的结论来自 kiana-tui Text 的源码/模块文档；本 crate 测试只直接覆盖空串与 padding_x 的宽度断言。
- kiana_logo 的调用方描述依据 kiana-interactive/src/interactive_mode.rs 的 header_with_logo/show_logo（约 1940-1948、2915 行）；未逐一追踪仓库外或测试内的其它调用点。
- BorderedLoader 的调用点按 grep 到的 kiana-interactive 三处（tui_renderer、bug_report、session_share）描述，未来新增调用点不在覆盖范围。
- auth_url 中『两个真实调用方都在后台任务里驱动 copy_with』依据 login_dialog 的 default_spawn（spawn_local）与 mcp/ui.rs 的 spawn_local；仓库内没有直接调用 copy() 的地方。

### u391-kiana-ui-kit-root

- “宿主在主题切换/全量重绘时调用 Component::invalidate()”取自 kiana-tui/src/tui.rs 的 trait 文档，未追踪 kiana-interactive 宿主的具体调用点。
- BorderedLoader 的 Snapshot 分支走 Theme::fg（返回 String，非 Result），其缺 token 时的行为未在本单元核实；crate 文档只表述为“见其模块文档”。
- CountdownTimer“由宿主组件持有并驱动”只核实了 kiana-interactive/src/components/status_indicator.rs 的字段用法，未穷举所有调用点。
- 阅读顺序中“dynamic_border.rs 是最小的一类组件：无缓存、无状态”是编辑性排序理由（依据其 75 行实现与每次 render 现算），不是源码断言。

## 3. 审阅发现的代码改动（应为空；若不为空需人工恢复基线）

以下 3 条均经主代理人工复核排除，不是真实代码改动；确定性纯净度检查
（tools/check_comment_only_diff.py，基线 diff + 代码行序列比对）当前通过：

- u218-kiana-evals-root: 0 (only comment lines added; no code lines changed) —— verifier 原文即“无代码改动”
- u247-kiana-interactive-large: 0（easter_egg_3d.rs 的全部新增/删除行均为注释行或空行，逐行核对 diff 无代码改动）—— 同上
- u347-kiana-theme-root: crates/kiana-theme/src/lib.rs —— 人工 diff 复核：新增 99 行全部为 //! 注释，
  代码行序列与基线一致（该报告字段是疑似清单，不是判定）

## 4. 统计

- 有未修复项的单元：16
- 有待核实项的单元：392
- 被审阅发现过问题的单元：364
- 审阅报告的代码改动条目：3
