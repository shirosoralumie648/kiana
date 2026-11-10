//! 公开面编译期测试（`ARCHITECTURE.md` §5.1/§5.5、spec ca-interactive-mode §5.1、
//! spec ca-interactive-components §5.2）。
//!
//! 本文件只做两件事：
//! 1. 让每个应导出的名字以**类型位置**出现一次（`Option<T>` 不需要构造，也没有副作用），
//!    从而在编译期证明它确实从 crate 根可见；
//! 2. 用 `type_name` 比较路径，证明 crate 根 / barrel 的再导出与子模块里的定义是**同一项**
//!    （不是副本）。
//!
//! 不执行任何渲染、I/O 或主题初始化；所有断言与运行环境无关。

#![allow(clippy::let_underscore_untyped)]

use std::any::type_name;
use std::any::type_name_of_val;

// ---------------------------------------------------------------------------
// components/index.ts barrel 中的类型
// ---------------------------------------------------------------------------

#[test]
fn component_barrel_types_resolve_from_crate_root() {
    // 消息组件。
    let _: Option<kiana_interactive::ArminComponent> = None;
    let _: Option<kiana_interactive::AssistantMessageComponent> = None;
    let _: Option<kiana_interactive::BashExecutionComponent> = None;
    let _: Option<kiana_interactive::BranchSummaryMessageComponent> = None;
    let _: Option<kiana_interactive::CompactionSummaryMessageComponent> = None;
    let _: Option<kiana_interactive::CustomMessageComponent> = None;
    let _: Option<kiana_interactive::SkillInvocationMessageComponent> = None;
    let _: Option<kiana_interactive::UserMessageComponent> = None;

    // 选择器 / 面板。
    let _: Option<kiana_interactive::CustomEditor> = None;
    let _: Option<kiana_interactive::CustomEditorOptions> = None;
    let _: Option<kiana_interactive::ExtensionEditorComponent> = None;
    let _: Option<kiana_interactive::ExtensionInputComponent> = None;
    let _: Option<kiana_interactive::ExtensionSelectorComponent> = None;
    let _: Option<kiana_interactive::FirstTimeSetupComponent> = None;
    let _: Option<kiana_interactive::FirstTimeSetupOptions> = None;
    let _: Option<kiana_interactive::FirstTimeSetupResult> = None;
    let _: Option<kiana_interactive::LoginDialogComponent> = None;
    let _: Option<kiana_interactive::ModelSelectorComponent> = None;
    let _: Option<kiana_interactive::OAuthSelectorComponent> = None;
    let _: Option<kiana_interactive::ScopedModelsSelectorComponent> = None;
    let _: Option<kiana_interactive::ModelsCallbacks> = None;
    let _: Option<kiana_interactive::ModelsConfig> = None;
    let _: Option<kiana_interactive::SessionSelectorComponent> = None;
    let _: Option<kiana_interactive::SettingsCallbacks> = None;
    let _: Option<kiana_interactive::SettingsConfig> = None;
    let _: Option<kiana_interactive::SettingsSelectorComponent> = None;
    let _: Option<kiana_interactive::ShowImagesSelectorComponent> = None;
    let _: Option<kiana_interactive::ThemeSelectorComponent> = None;
    let _: Option<kiana_interactive::ThinkingSelectorComponent> = None;
    let _: Option<kiana_interactive::ToolExecutionComponent> = None;
    let _: Option<kiana_interactive::ToolExecutionOptions> = None;
    let _: Option<kiana_interactive::TreeSelectorComponent> = None;
    let _: Option<kiana_interactive::TrustSelectorComponent> = None;
    let _: Option<kiana_interactive::UserMessageSelectorComponent> = None;

    // kiana-ui-kit 的共用渲染小件（TS 文件在 components/ 下）。
    let _: Option<kiana_interactive::BorderedLoader> = None;
    let _: Option<kiana_interactive::DynamicBorder> = None;
    let _: Option<kiana_interactive::RenderDiffOptions> = None;
    let _: Option<kiana_interactive::VisualTruncateResult> = None;
    let _: Option<kiana_interactive::CountdownTimer> = None;
    let _: Option<kiana_interactive::ThemedText> = None;
    let _: Option<kiana_interactive::VisualLinePreview> = None;

    // 基础设施 / 非 barrel 的公开件（§2.1 要求可访问，Rust 侧以 pub 暴露）。
    let _: Option<kiana_interactive::FooterComponent> = None;
    let _: Option<kiana_interactive::CustomEntryComponent> = None;
    let _: Option<kiana_interactive::EarendilAnnouncementComponent> = None;
    let _: Option<kiana_interactive::SelectSubmenu> = None;
    let _: Option<kiana_interactive::SteppedSubmenu> = None;
    let _: Option<kiana_interactive::EasterEgg3dAnimation> = None;
    let _: Option<kiana_interactive::EasterEgg3d> = None;
    let _: Option<kiana_interactive::AuthSelectorProvider> = None;
    let _: Option<kiana_interactive::RadiusOption> = None;

    // StatusIndicator 层级。
    let _: Option<kiana_interactive::BaseStatusIndicator> = None;
    let _: Option<kiana_interactive::WorkingStatusIndicator> = None;
    let _: Option<kiana_interactive::RetryStatusIndicator> = None;
    let _: Option<kiana_interactive::CompactionStatusIndicator> = None;
    let _: Option<kiana_interactive::BranchSummaryStatusIndicator> = None;
    let _: Option<kiana_interactive::IdleStatus> = None;
    // trait 不能放进 `Option<T>`（`?Sized`），用 trait object 引用即可。
    let _: Option<&dyn kiana_interactive::StatusIndicator> = None;

    // 会话搜索。
    let _: Option<kiana_interactive::SortMode> = None;
    let _: Option<kiana_interactive::NameFilter> = None;
    let _: Option<kiana_interactive::ParsedSearchQuery> = None;
    let _: Option<kiana_interactive::MatchResult> = None;

    // 常量。
    let _: usize = kiana_interactive::ARMIN_WIDTH;
    let _: usize = kiana_interactive::ARMIN_HEIGHT;
    assert_eq!(kiana_interactive::ARMIN_WIDTH, 31);
    assert_eq!(kiana_interactive::ARMIN_HEIGHT, 36);
}

// ---------------------------------------------------------------------------
// modes/index.ts 交互侧 + interactive-mode.ts / tui-renderer.ts 入口
// ---------------------------------------------------------------------------

#[test]
fn mode_entry_points_resolve_from_crate_root() {
    let _: Option<kiana_interactive::InteractiveMode> = None;
    let _: Option<kiana_interactive::InteractiveModeOptions> = None;
    let _: Option<kiana_interactive::InteractiveTuiOptions> = None;
    let _: Option<kiana_interactive::InteractiveRenderer> = None;
    let _: Option<kiana_interactive::InteractiveTuiReference> = None;
}

// ---------------------------------------------------------------------------
// 函数再导出（逐个按值引用，不做任何求值）
// ---------------------------------------------------------------------------

#[test]
fn exported_functions_resolve_from_crate_root() {
    // components/index.ts barrel 的函数。
    let _ = kiana_interactive::render_diff;
    let _ = kiana_interactive::key_hint;
    let _ = kiana_interactive::key_text;
    let _ = kiana_interactive::raw_key_hint;
    let _ = kiana_interactive::truncate_to_visual_lines;

    // kiana-logo / mermaid / markdown-transform。
    let _ = kiana_interactive::kiana_logo_lines;
    let _ = kiana_interactive::kiana_wordmark;
    let _ = kiana_interactive::supports_kiana_logo;
    let _ = kiana_interactive::create_mermaid_markdown_transformer;
    let _ = kiana_interactive::create_markdown_transform;

    // 3D 彩蛋。
    let _ = kiana_interactive::play_kiana_logo_3d;
    let _ = kiana_interactive::play_armin_3d;
    let _ = kiana_interactive::play_easter_egg_3d;
    let _ = kiana_interactive::is_armin_pixel;

    // 选择器辅助函数。
    let _ = kiana_interactive::format_auth_selector_provider_type;
    let _ = kiana_interactive::format_auth_selector_provider_status;
    let _ = kiana_interactive::format_tokens;
    let _ = kiana_interactive::format_cwd_for_footer;

    // 会话搜索。
    let _ = kiana_interactive::parse_search_query;
    let _ = kiana_interactive::match_session;
    let _ = kiana_interactive::filter_and_sort_sessions;
    let _ = kiana_interactive::has_session_name;

    // interactive-mode.ts / tui-renderer.ts。
    let _ = kiana_interactive::format_crash_extension_hint;
    let _ = kiana_interactive::format_resume_command;
    let _ = kiana_interactive::create_interactive_tui;
    let _ = kiana_interactive::create_interactive_tui_reference;
}

/// `create_login_menu_selector` 带 `impl FnMut` 参数，不能直接按 fn item 引用；
/// 用一个全具体类型的包装函数做同样的类型检查（包装函数不被调用）。
#[test]
fn create_login_menu_selector_signature_is_public() {
    fn wrapper<F, G>(
        request_render: kiana_tui::components::loader::RequestRenderCallback,
        title: &str,
        options: Vec<String>,
        radius_option: kiana_interactive::RadiusOption,
        on_select: F,
        on_cancel: G,
    ) -> kiana_interactive::components::radius_login_selector::RadiusLoginMenuComponent
    where
        F: FnMut(&str) + 'static,
        G: FnMut() + 'static,
    {
        kiana_interactive::create_login_menu_selector(
            request_render,
            title,
            options,
            radius_option,
            on_select,
            on_cancel,
        )
    }

    // 只实例化（不调用）：证明参数/返回类型与公开签名一致。
    let _ = wrapper::<fn(&str), fn()>;
}

// ---------------------------------------------------------------------------
// 同一性：barrel / crate 根 / 子模块指向同一个定义
// ---------------------------------------------------------------------------

#[test]
fn crate_root_reexports_are_the_same_items_as_the_submodules() {
    // components/index.ts 的成员。
    assert_eq!(
        type_name::<kiana_interactive::ArminComponent>(),
        type_name::<kiana_interactive::components::armin::ArminComponent>()
    );
    assert_eq!(
        type_name::<kiana_interactive::components::index::AssistantMessageComponent>(),
        type_name::<kiana_interactive::components::assistant_message::AssistantMessageComponent>()
    );
    assert_eq!(
        type_name::<kiana_interactive::components::index::CustomEditorOptions>(),
        type_name::<kiana_interactive::components::custom_editor::CustomEditorOptions>()
    );
    assert_eq!(
        type_name::<kiana_interactive::FirstTimeSetupResult>(),
        type_name::<kiana_interactive::components::first_time_setup::FirstTimeSetupResult>()
    );
    assert_eq!(
        type_name::<kiana_interactive::ToolExecutionOptions>(),
        type_name::<kiana_interactive::components::tool_execution::ToolExecutionOptions>()
    );

    // kiana-ui-kit 的 9 个共用小件：再导出即 kiana-ui-kit 里的定义。
    assert_eq!(
        type_name::<kiana_interactive::BorderedLoader>(),
        type_name::<kiana_ui_kit::bordered_loader::BorderedLoader>()
    );
    assert_eq!(
        type_name::<kiana_interactive::DynamicBorder>(),
        type_name::<kiana_ui_kit::dynamic_border::DynamicBorder>()
    );
    assert_eq!(
        type_name::<kiana_interactive::RenderDiffOptions>(),
        type_name::<kiana_ui_kit::diff::RenderDiffOptions>()
    );
    assert_eq!(
        type_name::<kiana_interactive::VisualTruncateResult>(),
        type_name::<kiana_ui_kit::visual_truncate::VisualTruncateResult>()
    );
    assert_eq!(
        type_name::<kiana_interactive::CountdownTimer>(),
        type_name::<kiana_ui_kit::countdown_timer::CountdownTimer>()
    );
    assert_eq!(
        type_name::<kiana_interactive::ThemedText>(),
        type_name::<kiana_ui_kit::themed_text::ThemedText>()
    );
    assert_eq!(
        type_name::<kiana_interactive::VisualLinePreview>(),
        type_name::<kiana_ui_kit::visual_truncate::VisualLinePreview>()
    );

    // 函数：根、barrel、源模块是同一个 fn item。
    assert_eq!(
        type_name_of_val(&kiana_interactive::render_diff),
        type_name_of_val(&kiana_ui_kit::diff::render_diff)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::components::index::key_hint),
        type_name_of_val(&kiana_ui_kit::keybinding_hints::key_hint)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::key_text),
        type_name_of_val(&kiana_ui_kit::keybinding_hints::key_text)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::raw_key_hint),
        type_name_of_val(&kiana_ui_kit::keybinding_hints::raw_key_hint)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::truncate_to_visual_lines),
        type_name_of_val(&kiana_ui_kit::visual_truncate::truncate_to_visual_lines)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::format_tokens),
        type_name_of_val(&kiana_interactive::components::footer::format_tokens)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::parse_search_query),
        type_name_of_val(
            &kiana_interactive::components::session_selector_search::parse_search_query
        )
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::play_armin_3d),
        type_name_of_val(&kiana_interactive::components::easter_egg_3d::play_armin_3d)
    );

    // modes/index.ts 的交互侧与 interactive-mode.ts 是同一项。
    assert_eq!(
        type_name::<kiana_interactive::InteractiveMode>(),
        type_name::<kiana_interactive::interactive_mode::InteractiveMode>()
    );
    assert_eq!(
        type_name::<kiana_interactive::index::InteractiveMode>(),
        type_name::<kiana_interactive::interactive_mode::InteractiveMode>()
    );
    assert_eq!(
        type_name::<kiana_interactive::InteractiveModeOptions>(),
        type_name::<kiana_interactive::interactive_mode::InteractiveModeOptions>()
    );
    assert_eq!(
        type_name::<kiana_interactive::index::InteractiveModeOptions>(),
        type_name::<kiana_interactive::interactive_mode::InteractiveModeOptions>()
    );

    // tui-renderer 组合根。
    assert_eq!(
        type_name_of_val(&kiana_interactive::create_interactive_tui),
        type_name_of_val(&kiana_interactive::tui_renderer::create_interactive_tui)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::create_interactive_tui_reference),
        type_name_of_val(&kiana_interactive::tui_renderer::create_interactive_tui_reference)
    );
    assert_eq!(
        type_name::<kiana_interactive::InteractiveRenderer>(),
        type_name::<kiana_interactive::tui_renderer::InteractiveRenderer>()
    );
    // interactive-mode.ts:206 也再导出了这两个函数（§5.1）。
    assert_eq!(
        type_name_of_val(&kiana_interactive::interactive_mode::create_interactive_tui),
        type_name_of_val(&kiana_interactive::tui_renderer::create_interactive_tui)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::interactive_mode::create_interactive_tui_reference),
        type_name_of_val(&kiana_interactive::tui_renderer::create_interactive_tui_reference)
    );

    // interactive-mode.ts 的函数只在 crate 根 / interactive_mode 出现。
    assert_eq!(
        type_name_of_val(&kiana_interactive::format_crash_extension_hint),
        type_name_of_val(&kiana_interactive::interactive_mode::format_crash_extension_hint)
    );
    assert_eq!(
        type_name_of_val(&kiana_interactive::format_resume_command),
        type_name_of_val(&kiana_interactive::interactive_mode::format_resume_command)
    );
}
