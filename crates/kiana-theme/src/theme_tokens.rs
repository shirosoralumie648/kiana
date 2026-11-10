//! theme-tokens.ts（56 个 token 元数据，顺序即 schema 顺序）。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/theme-tokens.ts`（121 行）。
//!
//! 声明顺序是**冻结的**：它是 [`ThemeToken::ALL`] 与 [`THEME_TOKENS`] 的下标基准，也是
//! 对外发布的 JSON Schema 的属性顺序。内置主题文件大体按同一顺序
//! 书写，但并不严格（dark.json/light.json 把 `selectedBg` 放在 `scrollbarTrack`/
//! `scrollbarThumb` 之前），主题校验也不要求键序；`Theme::colors` 的迭代顺序来自主题文件
//! 的键序（`Theme::new()` 先插入全部前景 token、再插入背景 token），HTML 导出的 CSS 变量
//! （`get_resolved_theme_colors()`）也沿用这个顺序，因此不能把它当作本表声明顺序的保证。
//!
//! TS 的 `defineThemeTokens()` 在模块加载时校验 fallback：目标 token 必须存在且位于同一
//! slot，否则抛 `Theme token {name} has unknown fallback {fallback}` /
//! `Theme token {name} has fallback {fallback} in a different color slot`。Rust 侧的表是
//! 编译期常量，这层校验由本模块的测试 `tests::fallbacks_are_known_and_in_the_same_slot`
//! 保证（同样的两条不变量；该测试模块只在 `cfg(test)` 下编译，故此处用行内代码而非链接）。
//!
//! TS 的类型层把 56 个 token 拆成 `ThemeColor`（foreground）与 `ThemeBg`（background）：
//! 这里用两个独立枚举表达，`ThemeColor::from_token` / `ThemeBg::from_token` 是运行时
//! 的 slot 检查入口。
//!
//! # 单一事实来源与生成方式
//!
//! 三个 `macro_rules!` 把 token 清单集中到唯一的 `define_theme_tokens!` 调用：枚举变体、
//! [`ThemeToken::ALL`]、[`ThemeToken::NAMES`]、`as_str`、`slot` 都由同一次展开生成，
//! 不会出现“数组比枚举少一项”的漂移。新增/调整 token 时要同时改三处并保持下标对齐：
//!
//! 1. `define_theme_tokens!` 的调用表（决定枚举、NAMES 与下标基准）；
//! 2. [`THEME_TOKENS`] 数组中同一位置的描述符（slot/description/fallback）；
//! 3. 按 slot 加进 `define_theme_color!`（前景）或 `define_theme_bg!`（背景）。
//!
//! 测试 `descriptors_follow_the_token_declaration_order` 逐项核对 [`THEME_TOKENS`] 与
//! [`ThemeToken::ALL`] 的 slot/name 一致，`theme_color_and_bg_partition_every_token`
//! 核对两个子枚举恰好覆盖全部 token。
//!
//! # fallback 的解析方式与约束
//!
//! `fallback` 只在主题文件省略该 token 时使用：`theme.rs` 的
//! `with_theme_token_fallbacks()` 按 [`THEME_TOKENS`] 声明顺序**单次遍历**，缺失项取
//! fallback token 已解析的值；fallback 也缺失时报
//! `Theme token <name> has unresolved fallback <fallback>`（`theme_schema` 在更早的校验
//! 阶段也会检查同样的条件）。
//!
//! 现有 5 个 fallback 的目标（`muted`、`text`、`selectedBg`、`thinkingXhigh`，其中 `text`
//! 被两个 token 使用）本身都是必填 token（`fallback: None`），所以单次遍历足够。若将来让
//! 某个必填 token 指向另一个**可选** token，必须保证被指向者先在表中出现并被补全，否则
//! 会得到 unresolved fallback 错误。
//!
//! # 运行时消费者
//!
//! - `theme.rs`：补全 fallback、按 slot 拆分颜色、构造 `Theme`；
//! - `theme_schema.rs`：用 `fallback.is_none()` 生成必填属性列表，用 [`ThemeToken::ALL`]
//!   生成 `additionalProperties: false` 的已知键白名单，并校验 fallback 可解析；
//! - `description` 字段是 JSON Schema 文案的逐字镜像，本仓库当前源码中没有读取它的运行期
//!   逻辑，不要以为改渲染行为需要动它（改它属于改对外规格）。
//!
//! [`ThemeToken::ALL`]: crate::theme_tokens::ThemeToken::ALL
//! [`THEME_TOKENS`]: crate::theme_tokens::THEME_TOKENS
//! [`ThemeToken::NAMES`]: crate::theme_tokens::ThemeToken::NAMES

/// TS: `ThemeTokenDescriptor.slot`（`"foreground" | "background"`）。
///
/// slot 是 token 的固有属性：`Theme::new()` 按它把颜色拆成前景/背景两张表，
/// [`ThemeColor`] / [`ThemeBg`] 只接受对应 slot 的 token（测试
/// `theme_color_and_bg_partition_every_token` 锁定这一划分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// `"foreground"`。
    Foreground,
    /// `"background"`。
    Background,
}

/// TS: `ThemeTokenDescriptor` —— 每个 token 的静态元数据。
///
/// [`THEME_TOKENS`] 与 [`ThemeToken::ALL`] 按下标一一对应；本结构只保存与具体主题无关的
/// 规格信息（slot、schema 描述、fallback），实际颜色在 `Theme` 实例里。
#[derive(Debug, Clone, Copy)]
pub struct ThemeTokenDescriptor {
    /// Token 所属的颜色槽。
    pub slot: Slot,
    /// 发布到 JSON Schema 的描述（逐字，见 spec §2.1）。
    ///
    /// 本仓库没有读取它来渲染的代码；它的用途是保持对外发布的 schema 文案与 TS 规格一致。
    pub description: &'static str,
    /// 主题文件省略该 token 时使用的 token（`Type.Optional` 的来源）。
    ///
    /// `None` 表示必填：`theme_schema` 用它生成 `required` 列表。
    pub fallback: Option<ThemeToken>,
}

// 一次展开生成四张必须保持一致的表面（枚举、ALL、NAMES、as_str/slot）。手写 56 个 token
// 的这五份数据极易漏改，因此集中到唯一的调用清单；`THEME_TOKENS` 描述符表仍需手写，由
// 测试按下标核对。宏内文档里的 `THEME_TOKENS` 指向本模块中的常量。
macro_rules! define_theme_tokens {
    ( $( $variant:ident => $name:literal : $slot:ident, )* ) => {
        /// TS: `type ThemeToken = keyof typeof THEME_TOKENS`。
        ///
        /// 变体声明顺序 == [`THEME_TOKENS`] 数组顺序 == schema 的属性顺序；主题文件只需
        /// 覆盖同一个 token 集合，键序不受约束。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ThemeToken {
            $(
                #[doc = concat!("TS: `THEME_TOKENS[\"", $name, "\"]`。")]
                $variant,
            )*
        }

        impl ThemeToken {
            /// 全部 56 个 token，声明顺序。
            ///
            /// 与 [`THEME_TOKENS`] 的下标一一对应（测试保证）。
            pub const ALL: [ThemeToken; 56] = [ $( ThemeToken::$variant, )* ];

            /// 全部 56 个 token 的名字，声明顺序（TS: `Object.keys(THEME_TOKENS)`）。
            pub const NAMES: [&'static str; 56] = [ $( $name, )* ];

            /// TS: `Object.keys(THEME_TOKENS)` 里的键名。
            pub const fn as_str(self) -> &'static str {
                match self { $( ThemeToken::$variant => $name, )* }
            }

            /// TS: `THEME_TOKENS[name].slot`。
            pub const fn slot(self) -> Slot {
                match self { $( ThemeToken::$variant => Slot::$slot, )* }
            }

            /// 该 token 是否是 background token（TS: `slot === "background"`）。
            pub const fn is_background(self) -> bool {
                matches!(self.slot(), Slot::Background)
            }

            /// TS: `THEME_TOKENS[name] !== undefined` 的名字查找。
            ///
            /// 名字是 port 规格的一部分（对应 TS 的 `name in THEME_TOKENS`），因此不用
            /// `std::str::FromStr`。任意 slot 的名字都返回 `Some`；需要按槽位过滤时用
            /// [`ThemeColor::from_token`] / [`ThemeBg::from_token`]。
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(name: &str) -> Option<ThemeToken> {
                match name { $( $name => Some(ThemeToken::$variant), )* _ => None }
            }
        }

        impl std::fmt::Display for ThemeToken {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

define_theme_tokens! {
    Accent => "accent": Foreground,
    Border => "border": Foreground,
    BorderAccent => "borderAccent": Foreground,
    BorderMuted => "borderMuted": Foreground,
    Success => "success": Foreground,
    Error => "error": Foreground,
    Warning => "warning": Foreground,
    Muted => "muted": Foreground,
    Dim => "dim": Foreground,
    Text => "text": Foreground,
    ThinkingText => "thinkingText": Foreground,
    ScrollbarTrack => "scrollbarTrack": Foreground,
    ScrollbarThumb => "scrollbarThumb": Foreground,
    SelectedBg => "selectedBg": Background,
    SearchMatchBg => "searchMatchBg": Background,
    SearchMatchText => "searchMatchText": Foreground,
    UserMessageBg => "userMessageBg": Background,
    UserMessageText => "userMessageText": Foreground,
    CustomMessageBg => "customMessageBg": Background,
    CustomMessageText => "customMessageText": Foreground,
    CustomMessageLabel => "customMessageLabel": Foreground,
    ToolPendingBg => "toolPendingBg": Background,
    ToolSuccessBg => "toolSuccessBg": Background,
    ToolErrorBg => "toolErrorBg": Background,
    ToolTitle => "toolTitle": Foreground,
    ToolOutput => "toolOutput": Foreground,
    MdHeading => "mdHeading": Foreground,
    MdLink => "mdLink": Foreground,
    MdLinkUrl => "mdLinkUrl": Foreground,
    MdCode => "mdCode": Foreground,
    MdCodeBlock => "mdCodeBlock": Foreground,
    MdCodeBlockBorder => "mdCodeBlockBorder": Foreground,
    MdQuote => "mdQuote": Foreground,
    MdQuoteBorder => "mdQuoteBorder": Foreground,
    MdHr => "mdHr": Foreground,
    MdListBullet => "mdListBullet": Foreground,
    ToolDiffAdded => "toolDiffAdded": Foreground,
    ToolDiffRemoved => "toolDiffRemoved": Foreground,
    ToolDiffContext => "toolDiffContext": Foreground,
    SyntaxComment => "syntaxComment": Foreground,
    SyntaxKeyword => "syntaxKeyword": Foreground,
    SyntaxFunction => "syntaxFunction": Foreground,
    SyntaxVariable => "syntaxVariable": Foreground,
    SyntaxString => "syntaxString": Foreground,
    SyntaxNumber => "syntaxNumber": Foreground,
    SyntaxType => "syntaxType": Foreground,
    SyntaxOperator => "syntaxOperator": Foreground,
    SyntaxPunctuation => "syntaxPunctuation": Foreground,
    ThinkingOff => "thinkingOff": Foreground,
    ThinkingMinimal => "thinkingMinimal": Foreground,
    ThinkingLow => "thinkingLow": Foreground,
    ThinkingMedium => "thinkingMedium": Foreground,
    ThinkingHigh => "thinkingHigh": Foreground,
    ThinkingXhigh => "thinkingXhigh": Foreground,
    ThinkingMax => "thinkingMax": Foreground,
    BashMode => "bashMode": Foreground,
}

/// TS: `THEME_TOKENS`（56 项，声明顺序）。
///
/// 每项与 [`ThemeToken::ALL`] 的下标一一对应（测试保证）。
///
/// `fallback: None` 的 token 是主题文件的必填项：`theme_schema` 的校验用它生成
/// `required` 列表；`Some(...)` 的 token 省略时由 fallback 补值。这里的 `description`
/// 文本只服务于对外发布的 schema 规格，不影响渲染。
pub const THEME_TOKENS: [ThemeTokenDescriptor; 56] = [
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Primary accent color (logo, selected items, cursor)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Normal borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Highlighted borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Subtle borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Success states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Error states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Warning states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Secondary/dimmed text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Very dimmed text (more subtle than muted)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Default text color (usually empty string)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking block text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Fullscreen scrollbar track foreground (falls back to muted when omitted)",
        fallback: Some(ThemeToken::Muted),
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Fullscreen scrollbar thumb foreground (falls back to text when omitted)",
        fallback: Some(ThemeToken::Text),
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Selected item background",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Transcript search match background and current-match text (falls back to selectedBg when omitted)",
        fallback: Some(ThemeToken::SelectedBg),
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Transcript search match text and current-match background (falls back to text when omitted)",
        fallback: Some(ThemeToken::Text),
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "User message background",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "User message text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Custom message background (hook-injected messages)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Custom message text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Custom message type label color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Tool execution box (pending state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Tool execution box (success state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Background,
        description: "Tool execution box (error state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Tool execution box title color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Tool execution box output text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown heading text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown link text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown link URL",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown inline code",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown code block content",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown code block fences",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown blockquote text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown blockquote border",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown horizontal rule",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Markdown list bullets/numbers",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Added lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Removed lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Context lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: comments",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: keywords",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: function names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: variable names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: string literals",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: number literals",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: type names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: operators",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Syntax highlighting: punctuation",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: off",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: minimal",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: low",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: medium",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: high",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: xhigh",
        fallback: None,
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Thinking level border: max (falls back to thinkingXhigh when omitted)",
        fallback: Some(ThemeToken::ThinkingXhigh),
    },
    ThemeTokenDescriptor {
        slot: Slot::Foreground,
        description: "Editor border color in bash mode",
        fallback: None,
    },
];

/// TS: `Object.keys(THEME_TOKENS)`（56 个名字，声明顺序）。
///
/// 与 [`ThemeToken::NAMES`] 内容相同，保留这个名字是为了对应 TS 的导出名。
pub const THEME_COLOR_NAMES: [&str; 56] = ThemeToken::NAMES;

macro_rules! define_theme_color {
    ( $( $variant:ident => $token:ident : $name:literal, )* ) => {
        /// TS: `ThemeColor`（`slot === "foreground"` 的 token）。
        ///
        /// 49 个变体按 [`ThemeToken::ALL`] 中前景 token 的相对顺序排列；背景 token 用
        /// [`ThemeBg`] 表示。两个枚举的 `ALL` 合起来覆盖全部 56 个 token（测试保证）。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ThemeColor {
            $(
                #[doc = concat!("TS: foreground token `", $name, "`（`THEME_TOKENS[\"", $name, "\"]`）。")]
                $variant,
            )*
        }

        impl ThemeColor {
            /// 全部 foreground token，声明顺序。
            pub const ALL: [ThemeColor; 49] = [ $( ThemeColor::$variant, )* ];

            /// 对应的 [`ThemeToken`]。
            pub const fn token(self) -> ThemeToken {
                match self { $( Self::$variant => ThemeToken::$token, )* }
            }

            /// token 名（`theme.fg("accent", …)` 里的 `"accent"`）。
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $name, )* }
            }

            /// 名字 → foreground token；名字不存在或属于 background slot → `None`。
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(name: &str) -> Option<Self> {
                match name { $( $name => Some(Self::$variant), )* _ => None }
            }

            /// token → foreground token；background token → `None`。
            pub const fn from_token(token: ThemeToken) -> Option<Self> {
                match token { $( ThemeToken::$token => Some(Self::$variant), )* _ => None }
            }
        }

        impl From<ThemeColor> for ThemeToken {
            fn from(value: ThemeColor) -> Self {
                value.token()
            }
        }

        impl std::fmt::Display for ThemeColor {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

define_theme_color! {
    Accent => Accent: "accent",
    Border => Border: "border",
    BorderAccent => BorderAccent: "borderAccent",
    BorderMuted => BorderMuted: "borderMuted",
    Success => Success: "success",
    Error => Error: "error",
    Warning => Warning: "warning",
    Muted => Muted: "muted",
    Dim => Dim: "dim",
    Text => Text: "text",
    ThinkingText => ThinkingText: "thinkingText",
    ScrollbarTrack => ScrollbarTrack: "scrollbarTrack",
    ScrollbarThumb => ScrollbarThumb: "scrollbarThumb",
    SearchMatchText => SearchMatchText: "searchMatchText",
    UserMessageText => UserMessageText: "userMessageText",
    CustomMessageText => CustomMessageText: "customMessageText",
    CustomMessageLabel => CustomMessageLabel: "customMessageLabel",
    ToolTitle => ToolTitle: "toolTitle",
    ToolOutput => ToolOutput: "toolOutput",
    MdHeading => MdHeading: "mdHeading",
    MdLink => MdLink: "mdLink",
    MdLinkUrl => MdLinkUrl: "mdLinkUrl",
    MdCode => MdCode: "mdCode",
    MdCodeBlock => MdCodeBlock: "mdCodeBlock",
    MdCodeBlockBorder => MdCodeBlockBorder: "mdCodeBlockBorder",
    MdQuote => MdQuote: "mdQuote",
    MdQuoteBorder => MdQuoteBorder: "mdQuoteBorder",
    MdHr => MdHr: "mdHr",
    MdListBullet => MdListBullet: "mdListBullet",
    ToolDiffAdded => ToolDiffAdded: "toolDiffAdded",
    ToolDiffRemoved => ToolDiffRemoved: "toolDiffRemoved",
    ToolDiffContext => ToolDiffContext: "toolDiffContext",
    SyntaxComment => SyntaxComment: "syntaxComment",
    SyntaxKeyword => SyntaxKeyword: "syntaxKeyword",
    SyntaxFunction => SyntaxFunction: "syntaxFunction",
    SyntaxVariable => SyntaxVariable: "syntaxVariable",
    SyntaxString => SyntaxString: "syntaxString",
    SyntaxNumber => SyntaxNumber: "syntaxNumber",
    SyntaxType => SyntaxType: "syntaxType",
    SyntaxOperator => SyntaxOperator: "syntaxOperator",
    SyntaxPunctuation => SyntaxPunctuation: "syntaxPunctuation",
    ThinkingOff => ThinkingOff: "thinkingOff",
    ThinkingMinimal => ThinkingMinimal: "thinkingMinimal",
    ThinkingLow => ThinkingLow: "thinkingLow",
    ThinkingMedium => ThinkingMedium: "thinkingMedium",
    ThinkingHigh => ThinkingHigh: "thinkingHigh",
    ThinkingXhigh => ThinkingXhigh: "thinkingXhigh",
    ThinkingMax => ThinkingMax: "thinkingMax",
    BashMode => BashMode: "bashMode",
}

macro_rules! define_theme_bg {
    ( $( $variant:ident => $token:ident : $name:literal, )* ) => {
        /// TS: `ThemeBg`（`Exclude<ThemeToken, ThemeColor>`）。
        ///
        /// 7 个变体按 [`ThemeToken::ALL`] 中背景 token 的相对顺序排列；前景 token 用
        /// [`ThemeColor`] 表示。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ThemeBg {
            $(
                #[doc = concat!("TS: background token `", $name, "`（`THEME_TOKENS[\"", $name, "\"]`）。")]
                $variant,
            )*
        }

        impl ThemeBg {
            /// 全部 background token，声明顺序。
            pub const ALL: [ThemeBg; 7] = [ $( ThemeBg::$variant, )* ];

            /// 对应的 [`ThemeToken`]。
            pub const fn token(self) -> ThemeToken {
                match self { $( Self::$variant => ThemeToken::$token, )* }
            }

            /// token 名（`theme.bg("selectedBg", …)` 里的 `"selectedBg"`）。
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $name, )* }
            }

            /// 名字 → background token；名字不存在或属于 foreground slot → `None`。
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(name: &str) -> Option<Self> {
                match name { $( $name => Some(Self::$variant), )* _ => None }
            }

            /// token → background token；foreground token → `None`。
            pub const fn from_token(token: ThemeToken) -> Option<Self> {
                match token { $( ThemeToken::$token => Some(Self::$variant), )* _ => None }
            }
        }

        impl From<ThemeBg> for ThemeToken {
            fn from(value: ThemeBg) -> Self {
                value.token()
            }
        }

        impl std::fmt::Display for ThemeBg {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

define_theme_bg! {
    SelectedBg => SelectedBg: "selectedBg",
    SearchMatchBg => SearchMatchBg: "searchMatchBg",
    UserMessageBg => UserMessageBg: "userMessageBg",
    CustomMessageBg => CustomMessageBg: "customMessageBg",
    ToolPendingBg => ToolPendingBg: "toolPendingBg",
    ToolSuccessBg => ToolSuccessBg: "toolSuccessBg",
    ToolErrorBg => ToolErrorBg: "toolErrorBg",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_follow_the_token_declaration_order() {
        assert_eq!(THEME_TOKENS.len(), ThemeToken::ALL.len());
        assert_eq!(THEME_COLOR_NAMES.len(), 56);
        for (index, token) in ThemeToken::ALL.iter().enumerate() {
            let descriptor = THEME_TOKENS[index];
            assert_eq!(descriptor.slot, token.slot(), "slot of {token}");
            assert_eq!(THEME_COLOR_NAMES[index], token.as_str());
            assert_eq!(ThemeToken::from_str(token.as_str()), Some(*token));
        }
        // Spot checks from the TS table.
        assert_eq!(THEME_COLOR_NAMES[0], "accent");
        assert_eq!(THEME_COLOR_NAMES[11], "scrollbarTrack");
        assert_eq!(THEME_COLOR_NAMES[13], "selectedBg");
        assert_eq!(THEME_COLOR_NAMES[55], "bashMode");
    }

    #[test]
    fn fallbacks_are_known_and_in_the_same_slot() {
        // TS: `defineThemeTokens()` —— 同样的两条不变量，同样的错误文案。
        for (token, descriptor) in ThemeToken::ALL.iter().zip(THEME_TOKENS.iter()) {
            let Some(fallback) = descriptor.fallback else {
                continue;
            };
            assert!(
                ThemeToken::ALL.contains(&fallback),
                "Theme token {token} has unknown fallback {fallback}"
            );
            assert_eq!(
                fallback.slot(),
                descriptor.slot,
                "Theme token {token} has fallback {fallback} in a different color slot"
            );
        }
        let optional: Vec<&str> = ThemeToken::ALL
            .iter()
            .zip(THEME_TOKENS.iter())
            .filter(|(_, descriptor)| descriptor.fallback.is_some())
            .map(|(token, _)| token.as_str())
            .collect();
        assert_eq!(
            optional,
            vec![
                "scrollbarTrack",
                "scrollbarThumb",
                "searchMatchBg",
                "searchMatchText",
                "thinkingMax"
            ]
        );
    }

    #[test]
    fn theme_color_and_bg_partition_every_token() {
        assert_eq!(ThemeColor::ALL.len() + ThemeBg::ALL.len(), 56);
        for token in ThemeToken::ALL {
            match token.slot() {
                Slot::Foreground => {
                    let color = ThemeColor::from_token(token).expect("foreground token");
                    assert_eq!(color.token(), token);
                    assert_eq!(color.as_str(), token.as_str());
                    assert_eq!(ThemeBg::from_token(token), None);
                }
                Slot::Background => {
                    let bg = ThemeBg::from_token(token).expect("background token");
                    assert_eq!(bg.token(), token);
                    assert_eq!(bg.as_str(), token.as_str());
                    assert_eq!(ThemeColor::from_token(token), None);
                }
            }
        }
        // Names in the wrong slot are rejected.
        assert_eq!(ThemeColor::from_str("userMessageBg"), None);
        assert_eq!(ThemeBg::from_str("accent"), None);
        assert_eq!(ThemeColor::from_str("notAToken"), None);
    }
}
