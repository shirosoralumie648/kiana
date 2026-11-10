//! `editor-component.ts`：`EditorComponent` 接口（扩展可替换的编辑器契约）。
//!
//! 它扩展 [`crate::tui::Component`]，额外要求 `getText`/`setText`/`handleInput` 与
//! 一组**可选**回调；`kiana-ext-api` 的 `ExtensionUIContext` 与 `kiana-interactive` 的自定义编辑器
//! 都以这里为准。
//!
//! ## 与 TS 的表示差异
//!
//! 1. TS 的 `onSubmit?` / `onChange?` / `borderColor?` 是**可写属性**（外部直接赋值）；
//!    Rust 用 `set_*` 安装回调 + 渲染期读取方法描述同一契约，因为 trait 对象无法表达
//!    "公开可写字段"。`set_on_submit` / `set_on_change` / `set_border_color` 就是 TS 的
//!    赋值动作，`border_color` 是渲染期读取（默认恒等，即 TS 的 `undefined` 分支）。
//! 2. TS 的可选方法在 Rust 里同样有默认实现（`None`/空），因此实现者只需覆写需要的部分。
//! 3. `AutocompleteProvider` 由 `kiana-tui:autocomplete` 单元定义；这里用类型擦除的
//!    [`AutocompleteProviderObject`] 留出接口面，避免本文件依赖该单元的具体类型
//!    （届时可直接换成 `crate::autocomplete::AutocompleteProvider`）。
//!
//! ## 谁实现、谁使用（仓库内可核对）
//!
//! - `kiana-extensions` 的 `JsEditorComponent`（`crates/kiana-extensions/src/ui/adapter.rs`）
//!   实现本 trait，把每个方法转发给 JS 编辑器对象；可选方法在 JS 侧缺失时静默跳过
//!   （`call_optional` 的语义）。
//! - `kiana-interactive` 的 `CustomEditor`（`crates/kiana-interactive/src/components/custom_editor.rs`）
//!   实现本 trait，并通过 `as_editor_component()` 以 `&mut dyn EditorComponent` 暴露自身。
//! - 宿主以 `Box<dyn EditorComponent>` 存放替换后的编辑器
//!   （见 `crates/kiana-interactive/src/interactive_mode/reload.rs` 与 `EditorFactory`）。
//!
//! ## 实现约定
//!
//! - 所有方法都没有泛型参数，trait 可对象安全地以 `dyn EditorComponent` 使用。
//! - 可选方法默认是空操作；`get_expanded_text` 默认回退到 [`EditorComponent::get_text`]，
//!   `border_color` 默认是恒等函数。
//! - `set_on_submit`/`set_on_change`/`set_border_color` 接收 `Option`：传 `None` 表示清除
//!   （等价 TS 赋 `undefined`）；`set_autocomplete_provider` 按值接管提供者。
//! - `set_border_color` 与 `border_color` 是一对：只覆写前者而不覆写后者时，回调不会生效
//!   （默认 `border_color` 直接返回原文），实现者需自行存储并在渲染期调用。

use std::any::Any;

use crate::tui::Component;

/// 类型擦除的 `AutocompleteProvider`（TS `setAutocompleteProvider?(provider: AutocompleteProvider)`）。
///
/// `kiana-tui:autocomplete` 单元定义具体类型；本文件只承诺"把提供者交给编辑器"这一动作，
/// 因此用 `Box<dyn Any>` 传输，实现者用 `downcast` 取回。
pub type AutocompleteProviderObject = Box<dyn Any>;

/// `onSubmit` / `onChange` 回调类型（TS `(text: string) => void`）。
pub type EditorTextCallback = Box<dyn FnMut(&str)>;

/// `borderColor` 回调类型（TS `(str: string) => string`）。
pub type EditorBorderColor = Box<dyn Fn(&str) -> String>;

/// 自定义编辑器组件（TS `EditorComponent`）。
///
/// 扩展允许扩展提供自己的编辑器实现（vim 模式、emacs 模式、自定义键位），同时保持与核心
/// 应用的兼容性。
pub trait EditorComponent: Component {
    // =========================================================================
    // 核心文本访问（必需）
    // =========================================================================

    /// 当前文本内容（`getText()`）。
    fn get_text(&self) -> String;

    /// 设置文本内容（`setText(text)`）。
    fn set_text(&mut self, text: &str);

    /// 处理原始终端输入（按键、粘贴序列等；`handleInput(data)`）。
    ///
    /// 与 [`Component::handle_input`] 是同一入口；这里再声明一次以便按
    /// [`EditorComponent`] 使用时不丢失语义（默认转发）。默认实现会调用实现者覆写的
    /// `Component::handle_input`，因此覆写父 trait 的方法就足够，不需要两个都写。
    fn handle_input_data(&mut self, data: &str) {
        self.handle_input(data);
    }

    // =========================================================================
    // 回调（TS 里是必需属性 `onSubmit?` / `onChange?`，Rust 用 set_* 安装）
    // =========================================================================

    /// 安装提交回调（TS `editor.onSubmit = fn`）。
    ///
    /// 编辑器在用户提交（如回车）时调用它，参数是提交时的文本。传 `None` 清除回调
    /// （`kiana-extensions` 的适配器把它映射为 JS 属性 `undefined`）；默认实现忽略安装。
    fn set_on_submit(&mut self, _callback: Option<EditorTextCallback>) {}

    /// 安装文本变化回调（TS `editor.onChange = fn`）。
    ///
    /// 参数是变化后的完整文本。传 `None` 清除回调；默认实现忽略安装。
    fn set_on_change(&mut self, _callback: Option<EditorTextCallback>) {}

    // =========================================================================
    // 历史（可选）
    // =========================================================================

    /// 把文本加入历史（上下键浏览；`addToHistory?`）。
    fn add_to_history(&mut self, _text: &str) {}

    // =========================================================================
    // 高级文本操作（可选）
    // =========================================================================

    /// 在光标处插入文本（`insertTextAtCursor?`）。
    fn insert_text_at_cursor(&mut self, _text: &str) {}

    /// 展开标记后的文本（如粘贴标记）；未实现时回退到 [`EditorComponent::get_text`]。
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    // =========================================================================
    // 外观（可选）
    // =========================================================================

    /// 设置水平内边距（`setPaddingX?`）。
    fn set_padding_x(&mut self, _padding: usize) {}

    /// 设置自动补全下拉的最大可见项数（`setAutocompleteMaxVisible?`）。
    fn set_autocomplete_max_visible(&mut self, _max_visible: usize) {}

    /// 设置自动补全提供者（`setAutocompleteProvider?`）。
    ///
    /// `provider` 的类型由 `kiana-tui:autocomplete` 单元定义；这里用
    /// [`AutocompleteProviderObject`] 留出接口面，避免本模块依赖具体类型。
    /// 实现者用 `downcast` 取回具体类型。本 trait 不规定类型不认识时的行为：默认实现
    /// 直接丢弃，参数按值传入、所有权交给编辑器。
    fn set_autocomplete_provider(&mut self, _provider: AutocompleteProviderObject) {}

    /// 安装边框着色函数（TS `editor.borderColor = fn`）。
    ///
    /// 传 `None` 表示未设置（渲染时用
    /// [`EditorComponent::border_color`] 的恒等默认值）。
    fn set_border_color(&mut self, _color: Option<EditorBorderColor>) {}

    /// 渲染期读取边框着色（TS `this.borderColor?.(str) ?? str`）。
    ///
    /// 默认恒等；需要自定义的编辑器可覆写（或通过 `set_border_color` 安装回调后在此调用）。
    /// 注意：默认的 `set_border_color` 是空操作，**只覆写它不会改变本方法的输出**。
    fn border_color(&self, text: &str) -> String {
        text.to_string()
    }
}
