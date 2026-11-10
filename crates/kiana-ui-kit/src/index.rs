//! 再导出面。
//!
//! TS 侧的 barrel 是 `components/index.ts`（由 `kiana-interactive` 拥有）；本 crate 只汇总自己的
//! 九个模块。TS barrel 至少导出 `BorderedLoader`、`DynamicBorder`、`renderDiff`/`RenderDiffOptions`、
//! `keyHint`/`keyText`/`rawKeyHint`、`truncateToVisualLines`/`VisualTruncateResult`；其余组件在
//! TS 里由宿主按路径直接 import，这里一并再导出（**增量公开面**，方便下游 crate 使用）。
//!
//! 本文件没有 TS 对应实现，只是 `pub use`。
//!
//! # 维护约束
//!
//! crate 根通过 `pub use crate::index::*;` 把这里的内容提升到 `kiana_ui_kit::*`。因此新增
//! 公开组件时必须同时在本文件补一条 `pub use`，否则该条目只能通过
//! `kiana_ui_kit::模块::条目` 访问。这里只做再导出、不定义类型；改名或移除条目会直接
//! 改变 crate 根的公开面（`tests/index.rs` 对每个条目做了可用性检查）。

pub use crate::auth_url::AuthUrlComponent;
pub use crate::bordered_loader::{BorderedLoader, BorderedLoaderOptions};
pub use crate::countdown_timer::CountdownTimer;
pub use crate::diff::{RenderDiffOptions, render_diff};
pub use crate::dynamic_border::DynamicBorder;
pub use crate::keybinding_hints::{
    KeyTextFormatOptions, format_key_text, key_display_text, key_hint, key_text, raw_key_hint,
};
pub use crate::kiana_logo::{kiana_logo_lines, kiana_wordmark, supports_kiana_logo};
pub use crate::themed_text::ThemedText;
pub use crate::visual_truncate::{
    Keep, VisualLinePreview, VisualLinePreviewOptions, VisualTruncateResult,
    truncate_to_visual_lines,
};
