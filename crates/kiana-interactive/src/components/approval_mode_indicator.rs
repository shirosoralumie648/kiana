//! 权限模式指示器（approval mode indicator）。
//!
//! 对应 qwen-code `packages/cli/src/ui/components/AutoAcceptIndicator.tsx` +
//! `approvalModeVisuals.ts` 与 gemini-cli `ApprovalModeIndicator.tsx` 的语义：把当前
//! [`PermissionMode`] 渲染成一段带颜色的短文本，挂在状态行/页脚右侧。
//!
//! # 为什么需要它
//!
//! 权限模式决定每一次工具调用是放行、询问还是拒绝。模式在运行期可以切换（`/permissions`
//! 面板的 `m` 键、CLI `--permission-mode`），但切换入口本身不在主界面上；没有指示器时，
//! 用户只能凭记忆判断“现在按 Enter 是批准还是直接执行”。本组件把模式变成一个持续可见
//! 的状态，降低误操作风险。
//!
//! # 与 TS 参考实现的差异（按 Kiana 设计文档 §4）
//!
//! - 文本直接用 Kiana 的稳定模式名（[`PermissionMode::as_str`]：
//!   `read-only` / `default` / `accept-edits` / `bypass`），不引入 qwen 的
//!   `plan mode` / `YOLO mode` 等五套名字；
//! - **不**附带 `(shift + tab to cycle)`：Kiana 没有这个键位，模式切换在 `/permissions`
//!   面板里（`m` 键）。[`ApprovalModeIndicator::hint`] 提供 `m to change` 供独立展示
//!   （如权限面板标题）使用，页脚段（[`ApprovalModeIndicator::segment`]）不含提示，
//!   以保证 `mode = None` 时页脚输出逐字节不变；
//! - 渲染不新增页脚行：Kiana 的 footer 是冻结契约（golden 测试逐字节锁定），[`segment`]
//!   只返回 `" • <mode>"` 供 footer 追加在第二行右侧。
//!
//! # 颜色映射
//!
//! | 模式 | 文本 | 颜色 | 粗体 |
//! |---|---|---|---|
//! | [`PermissionMode::ReadOnly`] | `read-only` | `Success` | 否 |
//! | [`PermissionMode::Default`] | `default` | `Dim` | 否 |
//! | [`PermissionMode::AcceptEdits`] | `accept-edits` | `Warning` | 否 |
//! | [`PermissionMode::BypassPermissions`] | `bypass` | `Error` | 是 |
//!
//! `bypass` 用最重的视觉（红色 + 粗体）：它意味着灾难性命令检查也会放行。
//!
//! # 边界与失败模式
//!
//! - `mode = None`（权限层被 settings/env 关闭）时所有渲染方法返回 `None`；调用方必须
//!   保持现状（footer 不追加任何字符），不能把空态当成 `default` 显示。
//! - 渲染依赖全局主题已初始化。与 footer 不同，这里在主题未初始化时退化为无颜色文本而
//!   不是 panic：指示器只是辅助信息，不应让整个 TUI 崩溃。测试仍会先 `init_theme`，
//!   以锁定颜色转义序列。
//!
//! TS 源：`packages/cli/src/ui/components/AutoAcceptIndicator.tsx`（qwen-code）与
//! `packages/cli/src/ui/components/ApprovalModeIndicator.tsx`（gemini-cli）。

use kiana_permissions::PermissionMode;
use kiana_theme::{THEME, ThemeColor};

/// 一个模式的展示属性（文本、颜色、是否加粗）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionModeVisual {
    /// 稳定模式名（[`PermissionMode::as_str`]）。
    pub text: &'static str,
    /// 主题前景 token。
    pub color: ThemeColor,
    /// 是否加粗（仅 `bypass`）。
    pub bold: bool,
}

/// 模式 → 展示属性。
///
/// 这张表是 `approvalModeVisuals.ts` 的 Kiana 等价物；颜色语义与设计文档 §4.2 一致。
pub fn permission_mode_visual(mode: PermissionMode) -> PermissionModeVisual {
    match mode {
        PermissionMode::ReadOnly => PermissionModeVisual {
            text: mode.as_str(),
            color: ThemeColor::Success,
            bold: false,
        },
        PermissionMode::Default => PermissionModeVisual {
            text: mode.as_str(),
            color: ThemeColor::Dim,
            bold: false,
        },
        PermissionMode::AcceptEdits => PermissionModeVisual {
            text: mode.as_str(),
            color: ThemeColor::Warning,
            bold: false,
        },
        PermissionMode::BypassPermissions => PermissionModeVisual {
            text: mode.as_str(),
            color: ThemeColor::Error,
            bold: true,
        },
    }
}

/// 按主题 token 给文本着色；主题未初始化时原样返回。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// 加粗；主题未初始化时原样返回。
fn bold(text: &str) -> String {
    THEME.bold(text).unwrap_or_else(|_| text.to_string())
}

/// 权限模式指示器（空态 + 四种模式）。
///
/// 这是一个**无状态渲染件**：不持有会话、不读 settings，只保存调用方传入的模式值。
/// 页脚/状态行的持有者负责在模式变化后调用 [`ApprovalModeIndicator::set_mode`]。
///
/// 构造后默认 `None`（引擎未启用）。`None` 表示“不显示”，不是 `default` 模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApprovalModeIndicator {
    mode: Option<PermissionMode>,
}

impl ApprovalModeIndicator {
    /// 用初始模式构造；`None` 表示权限层关闭（不显示）。
    pub const fn new(mode: Option<PermissionMode>) -> Self {
        Self { mode }
    }

    /// 更新模式。`None` 会隐藏指示器（权限层被关闭）。
    pub fn set_mode(&mut self, mode: Option<PermissionMode>) {
        self.mode = mode;
    }

    /// 当前模式（测试接缝）。
    pub fn mode(&self) -> Option<PermissionMode> {
        self.mode
    }

    /// 与设计文档 `FooterComponent::set_permission_mode` 同名的别名。
    ///
    /// footer 的持有者（Integrate/`footer.rs`）可以直接按设计文档的命名调用；语义与
    /// [`Self::set_mode`] 完全一致。
    pub fn set_permission_mode(&mut self, mode: Option<PermissionMode>) {
        self.set_mode(mode);
    }

    /// 与设计文档 `FooterComponent::permission_mode` 同名的测试接缝。
    pub fn permission_mode(&self) -> Option<PermissionMode> {
        self.mode()
    }

    /// 是否应渲染任何内容。
    pub fn is_visible(&self) -> bool {
        self.mode.is_some()
    }

    /// 纯模式文本（`read-only` 等）；`None` 时不显示。
    pub fn text(&self) -> Option<&'static str> {
        self.mode.map(PermissionMode::as_str)
    }

    /// 切换提示。页脚段不含它（见模块文档），权限面板标题可用。
    pub fn hint(&self) -> &'static str {
        "m to change"
    }

    /// 带颜色的模式文本；`None` 时返回 `None`。
    pub fn styled_text(&self) -> Option<String> {
        let mode = self.mode?;
        let visual = permission_mode_visual(mode);
        let styled = fg(visual.color, visual.text);
        if visual.bold {
            Some(bold(&styled))
        } else {
            Some(styled)
        }
    }

    /// 页脚追加段：`" • <mode>"`，只对模式文本着色。
    ///
    /// 调用方在 footer 第二行右侧**原样追加**；`None` 时不得追加任何字符（保护 footer
    /// golden 的逐字节输出）。
    pub fn segment(&self) -> Option<String> {
        Some(format!(" • {}", self.styled_text()?))
    }

    /// 与 [`Self::segment`] 等价的纯文本形式（无 ANSI），供测试与 golden 使用。
    pub fn plain_segment(&self) -> Option<String> {
        Some(format!(" • {}", self.text()?))
    }

    /// 独立展示的一行（含切换提示）：`Mode: <mode> (m to change)`。
    ///
    /// 页脚不要用这个；它供权限面板、状态行等需要提示的场景使用。
    pub fn render(&self) -> Option<String> {
        self.mode?;
        Some(format!("Mode: {} ({})", self.styled_text()?, self.hint()))
    }

    /// 与 [`Self::render`] 等价的纯文本形式。
    pub fn plain_text(&self) -> Option<String> {
        Some(format!("Mode: {} ({})", self.text()?, self.hint()))
    }

    /// 始终有输出的一行：`Mode: off (m to change)` 或 `Mode: <mode> (m to change)`。
    ///
    /// 权限面板用它显示关闭状态；页脚不使用。
    pub fn mode_line(&self) -> String {
        match self.mode {
            Some(_) => self.render().expect("mode is Some"),
            None => format!("Mode: {} ({})", fg(ThemeColor::Dim, "off"), self.hint()),
        }
    }
}
