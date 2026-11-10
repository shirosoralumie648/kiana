//! 第 12 周前半的「下一代理渲染核心」（实验开关控制，不替换现有 48 个组件）。
//!
//! # 这个模块解决什么问题
//!
//! 现有 `interactive_mode` / `tui_renderer` 是 TS 交互模式的逐行移植：渲染树里的组件
//! 每帧整体重绘，历史（scrollback）由主屏渲染器按「首/末变化行」推断。第 12 周要验证
//! 的是另一套更严格的契约——**应用层决定 finality，终端层只负责写**：
//!
//! * `frame`：`FrameProvider` 每帧返回可选 `HistoryBatch` + 完整可变 viewport；写端对每个
//!   接受的 batch 只写一次并 ack，viewport 做行级 diff，overlay 单独合成，光标落在
//!   synchronized-output 帧内。
//! * `history`：`active` / `settled` / `committed` 三态账本；只有容量压力才退休最短
//!   `settled` 前缀（active 永不退休）。
//! * `transcript`：不可变事件日志 → 渲染模型；Action 被 Observation 原地替换、连续工具
//!   折叠成组、thought 提升、artifact/error/finish 作为组边界、流式文本进 UI-only
//!   provisional slot（anchor_seq 定位、durable 到达退役、attempt 高者覆盖）。
//! * `pending_user`：乐观用户消息队列（trimmed 内容 + 服务端时间戳围栏确认、确认 id
//!   去重、150s watchdog 转可重试错误）。
//! * `approval`：per-tool 审批卡（Bash/FileEdit/FileWrite/WebFetch/MCP/Skill/未知），
//!   suggestions、decision reason、可选 advisory 风险解释、危险动作默认聚焦 Reject。
//! * `modal`：单一 modal 仲裁 + 审批队列（同一时刻最多一个，显示 index/total；
//!   ask-user 单选/多选/All/Other/文本/yes-no；键盘/鼠标/滚动/备用屏隔离）。
//! * `i18n`：`t(key, params)` + 多行数组；语言包只读数据文件（JSON/TOML 子集），
//!   不执行用户 JS；用户目录 > 内置 > fallback；strict locale 的 key parity 与
//!   `MUST_TRANSLATE_KEYS` 校验。
//!
//! # 实验开关
//!
//! [`experimental_enabled`] 读取 `KIANA_NEXT_RENDER_CORE`。默认关闭时现有渲染路径完全
//! 不变；打开后由接线层（见 `docs/history/porting/handoffs/phase-12a.md`）选择这些模型与写端。
//! 本模块自身没有副作用：不读终端、不启动任务、不触碰全局主题，因此可以独立测试。
//!
//! # 设计来源
//!
//! 参考项目与借鉴点逐条列在 `docs/history/porting/phase-12a-tui-frame-contract.md` 的表格里；实现全部按 Kiana 的
//! 类型与运行时重写，没有复制参考代码。

pub mod approval;
pub mod frame;
pub mod history;
pub mod i18n;
pub mod modal;
pub mod pending_user;
pub mod transcript;

/// 是否启用下一代理渲染核心（`KIANA_NEXT_RENDER_CORE=1`）。
///
/// 只认 `1` / `true` / `yes`（大小写不敏感），其余值都按关闭处理；未设置时关闭。
/// 接线层应在构造渲染器前读一次并保存，避免渲染过程中环境变量变化造成半新半旧。
pub fn experimental_enabled() -> bool {
    match std::env::var("KIANA_NEXT_RENDER_CORE") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// 乐观用户消息队列是否启用（生产默认开启）。
///
/// 只有显式的 `KIANA_NEXT_RENDER_CORE=0/false/no/off` 关闭；未设置时开启。
/// 帧写端（`FrameWriter`/`TranscriptLedger`）仍是独立实现，由
/// [`experimental_enabled`] 决定是否启用。
pub fn pending_user_enabled() -> bool {
    match std::env::var("KIANA_NEXT_RENDER_CORE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}
