//! components/input.ts。
//!
//! 单行文本输入：水平滚动、伪光标 + 硬件光标标记、括号粘贴、Emacs 风格 kill/yank、撤销栈、
//! 鼠标点击定位。
//!
//! ## 表示差异
//!
//! TS 的 `value`/`cursor` 全部按 UTF-16 码元索引（`slice`/`length`）。Rust 侧用
//! `utf16_slice` 系列辅助函数复刻同一语义；落在代理对中间的切片用 `U+FFFD` 代替孤立
//! 代理项（与 `crate::text` 的 `units_to_string` 约定一致）。
//!
//! # 职责与边界
//!
//! `Input` 只维护随输入变化的状态：文本 `value`、光标 `cursor`（UTF-16 码元偏移）和上一次
//! 动作 `last_action`（驱动 kill 累积与 yank-pop 链）。渲染本身不依赖历史：在 prompt /
//! placeholder / 焦点配置不变时，相同的 `value`/`cursor`/`width` 得到同一行输出；渲染期间
//! 唯一被写入的 `Input` 字段是 `rendered_start_column`，供鼠标定位把点击列换算回文本列。
//!
//! 组件不持有终端或 TUI 句柄，也不请求重绘：提交与取消通过 `on_submit` / `on_escape`
//! 回调交给宿主，是否重绘由 TUI 的输入分发逻辑决定。宿主包括 `SettingsList` 的搜索框、
//! `alt_screen_search` 的查询框，以及 `kiana-extensions` 暴露给 JS 的 `Input` 包装。
//!
//! # 键盘分发的顺序
//!
//! [`Component::handle_input`] 是一条"首个匹配即返回"的分支链，顺序固定：
//!
//! 1. 括号粘贴（`\x1b[200~` … `\x1b[201~`）跨多次调用缓冲，完整后才插入；
//! 2. `tui.select.cancel`（默认 Escape / Ctrl+C）→ 调用 `on_escape`；
//! 3. `tui.editor.undo` → 弹出一份撤销快照；
//! 4. `tui.input.submit` 或裸 `\n` → 调用 `on_submit`；
//! 5. 删除类键位（字符 / 词 / 行首 / 行尾）；
//! 6. yank / yank-pop；
//! 7. 光标移动（字符 / 词 / 行首 / 行尾）；
//! 8. Kitty CSI-u 可打印字符；
//! 9. 其余输入：只要不含控制字符（C0、DEL、C1）就整体插入。
//!
//! 调整顺序会改变行为：只有两个动作被绑定到同一个键（键位重叠）时，先后顺序才决定哪个
//! 分支先命中。默认键位表里提交（`Enter`）与删除类动作互不重叠，所以调换它们的顺序不会让
//! `Enter` 触发删除；改动前先对照 `tests/input.rs` 的用例。
//!
//! # 不变量
//!
//! - `cursor` 是 UTF-16 码元偏移而非字节偏移；按码元切片的操作都经 `utf16_slice` 完成，
//!   光标移动以**字素簇**为单位，emoji 与组合字符不会被从中间拆开。
//! - 每个撤销单元在第一次修改前 `push_undo()` 存一份"修改前"快照：连续的词内字符共用一个
//!   单元（后续字符不再入栈），空白字符、删除、粘贴、yank 各自新开单元。`undo()` 弹出后把
//!   `last_action` 清空。`set_value()` 是例外：它不入栈、也不重置 `last_action`（与 TS 相同，
//!   `tests/input.rs` 里 yank-pop 负下标的回归用例依赖这一点）。
//! - `last_action == Kill` 时，连续的删除会累积进同一条 kill ring 条目；光标移动、打字、
//!   粘贴、撤销与鼠标点击都会打断累积。`Yank` 状态是 yank-pop 的前置条件，否则 `Alt+Y`
//!   是 no-op（kill ring 不足两条时同样是 no-op）。
//!
//! # 渲染与边界
//!
//! [`Component::render`] 恒返回一行：可用宽度 = `width - visible_width(prompt)`；可用宽度 ≤ 0
//! 时只返回截断到 `width` 的 prompt。值放不下时按"光标尽量居中"的启发式水平滚动，并用
//! `slice_by_column(..., strict = true)` 取窗口，保证宽字素不会跨出右边界（回归测试
//! `tests/input.rs` 的 CJK/全角用例断言输出不超过 `width`）。值为空且配置了 placeholder
//! 时，占位文本的首个字素被伪光标包住，其余文本经 `placeholder_style` 着色。
//!
//! 鼠标定位与渲染宽度用的不是同一个 prompt 宽度：点击换算固定按 2 列减去 prompt（TS 的
//! 既有假设，见 `handle_mouse` 的注释），而渲染用 `visible_width(prompt)`。自定义 prompt
//! 不是 2 列时，点击位置会整体偏移——这是保留的 TS 行为，不是渲染错误。
//!
//! 括号粘贴若一直收不到结束标记，`paste_buffer` 会持续增长且不插入任何内容——TS 同样没有
//! 长度上限；这里只记录该失败模式，不做截断。
//!
//! # 阅读顺序
//!
//! `InputState` / `InputOptions` → `Input` 字段 → `handle_input`（分发链）→ 各 `handle_*` /
//! `delete_*` / `yank*` 私有方法 → `render` → `handle_mouse` → 文件末尾的 UTF-16 辅助函数。

use std::sync::Arc;

use crate::components::text_component::StyleFn;
use crate::keybindings::get_keybindings;
use crate::keys::decode_kitty_printable;
use crate::text::{
    KillRing, PushOptions, UndoStack, get_grapheme_segmenter, is_whitespace_char, slice_by_column,
    truncate_to_width, visible_width,
};
use crate::text::{find_word_backward, find_word_forward};
use crate::tui::{
    CURSOR_MARKER, Component, Focusable, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult,
    TuiMouseEventType, render_fake_cursor,
};

/// 撤销快照（TS `InputState`）。
///
/// 每个撤销单元在第一次修改前，把当时的完整状态克隆进撤销栈；撤销就是整体恢复这份快照，
/// 因此不需要记录"反向操作"。只有可撤销的编辑（插入字符、删除、粘贴、yank/yank-pop）会
/// 入栈：`Input::set_value` 与纯光标移动直接改字段，不产生快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputState {
    /// 快照时刻的完整输入值。
    pub value: String,
    /// 快照时刻的光标位置，UTF-16 码元偏移。
    pub cursor: usize,
}

/// 提交回调（TS `onSubmit?: (value: string) => void`）。
pub type InputSubmitCallback = Box<dyn FnMut(&str)>;
/// Esc 回调（TS `onEscape?: () => void`）。
pub type InputEscapeCallback = Box<dyn FnMut()>;

/// `Input` 的可选参数（TS `InputOptions`）。
///
/// 三个字段都缺省：`prompt` 缺省为 `"> "`，其余缺省为空/恒等函数。字段在构造时被复制进
/// [`Input`]，之后修改 `InputOptions` 不会影响已创建的组件。
#[derive(Clone, Default)]
pub struct InputOptions {
    /// 提示符前缀；参与可用宽度计算，因此修改它会影响水平滚动的窗口大小。
    pub prompt: Option<String>,
    /// 值为空时显示的占位文本；只影响渲染，永远不会进入 `value` 或被提交。
    pub placeholder: Option<String>,
    /// 占位文本的着色函数；分别作用于光标下的首字素和其后的剩余文本。
    pub placeholder_style: Option<StyleFn>,
}

// 手写 `Debug`：`placeholder_style` 是 `Arc<dyn Fn>`，无法 derive，用 `<fn>` 占位。
impl std::fmt::Debug for InputOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputOptions")
            .field("prompt", &self.prompt)
            .field("placeholder", &self.placeholder)
            .field(
                "placeholder_style",
                &self.placeholder_style.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

/// 单行文本输入组件（TS `class Input implements Component, Focusable`）。
///
/// 构造后 `value` 为空且未聚焦；焦点由 TUI 通过 `set_focused` 写入。可用宽度
/// （`width - visible_width(prompt)`）大于 0 时渲染**总是**输出伪光标（`render_fake_cursor`
/// 标记）；可用宽度 ≤ 0 时只返回截断到 `width` 的 prompt，没有伪光标。硬件光标标记
/// `CURSOR_MARKER` 只在聚焦时输出——未聚焦的 Input 仍会画出伪光标（见 `tests/tui_render.rs`
/// 的 `keeps_the_fake_cursor_of_an_unfocused_input_when_the_hardware_cursor_is_shown`）。
///
/// `on_submit` / `on_escape` 是 `Box<dyn FnMut>`，在 `handle_input` 内部同步调用；回调里
/// 不能再次可变借用同一个 `Input`（借用检查会拒绝），需要联动其他状态时请通过
/// `Rc<RefCell<_>>` 之类的外部共享句柄。
pub struct Input {
    value: String,
    /// 光标位置（UTF-16 码元）。
    cursor: usize,
    prompt: String,
    placeholder: String,
    placeholder_style: StyleFn,
    rendered_start_column: usize,
    /// 提交回调（TS `onSubmit`）。
    pub on_submit: Option<InputSubmitCallback>,
    /// Esc 回调（TS `onEscape`）。
    pub on_escape: Option<InputEscapeCallback>,
    focused: bool,

    // 括号粘贴缓冲。
    paste_buffer: String,
    is_in_paste: bool,

    // Emacs 风格 kill/yank。
    kill_ring: KillRing,
    last_action: Option<LastAction>,

    // 撤销。
    undo_stack: UndoStack<InputState>,
}

/// 上一次动作，驱动两套跨调用的状态：`Kill` 决定下一次删除是否并入同一条 kill ring 条目，
/// `Yank` 决定 `Alt+Y` 能否轮换；`TypeWord` 让连续的词内字符共用一个撤销单元。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

impl Default for Input {
    fn default() -> Self {
        Input::new(InputOptions::default())
    }
}

impl Input {
    /// `constructor(options = {})`。
    ///
    /// 只做字段初始化，不触发回调、不请求重绘；光标位于 0，撤销栈与 kill ring 为空。
    pub fn new(options: InputOptions) -> Self {
        Input {
            value: String::new(),
            cursor: 0,
            prompt: options.prompt.unwrap_or_else(|| "> ".to_string()),
            placeholder: options.placeholder.unwrap_or_default(),
            placeholder_style: options
                .placeholder_style
                .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
            rendered_start_column: 0,
            on_submit: None,
            on_escape: None,
            focused: false,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: KillRing::new(),
            last_action: None,
            undo_stack: UndoStack::new(),
        }
    }

    /// `getValue()`。
    pub fn get_value(&self) -> &str {
        &self.value
    }

    /// `setValue(value)`。
    ///
    /// 只替换值并把光标夹到新值的长度；撤销栈与 `last_action` 保持原样（TS 同样），因此
    /// "先 yank 再 setValue 缩短值，然后 Alt+Y"这类序列仍会进入 yank-pop 分支，
    /// `tests/input.rs` 对此有专门回归。
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.cursor.min(utf16_len(&self.value));
    }

    /// 当前光标（UTF-16 码元偏移）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 当前 prompt。
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// 当前可见窗口的起始列（TS `renderedStartColumn`）。
    ///
    /// 按 `render` 走的分支不同：
    /// - 常规路径（有可用宽度且不是在渲染 placeholder）先把字段清零；值放得下
    ///   （`total_width < available_width`）时保持 0；
    /// - 值放不下时进入水平滚动分支，仍有可视列时写入窗口左端在文本中的显示列。值与可用
    ///   宽度恰好相等且光标在末尾时，为给伪光标预留一列会滚动 1 列，字段为 1 而不是 0；
    /// - placeholder 分支与可用宽度 ≤ 0 的分支都在写字段之前提前返回，字段保留上一次的值，
    ///   这些分支也不用它做渲染决策。
    ///
    /// 它只用于把鼠标 x 坐标换算回文本列。
    pub fn rendered_start_column(&self) -> usize {
        self.rendered_start_column
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        });
    }

    fn undo(&mut self) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.value = snapshot.value;
        self.cursor = snapshot.cursor;
        self.last_action = None;
    }

    fn insert_character(&mut self, character: &str) {
        // 连续的词内字符合并成一个撤销单元。
        // 空白字符例外：它每次都会强制新开一个撤销单元，随后紧跟的词内字符继续并入这个
        // 单元（例如对 "hello world" 一次 undo 撤销的是 " world"，空格与单词一起撤销）；
        // 只有连续的空白才各自占一个单元（见 `tests/input.rs` 的
        // `coalesces_consecutive_word_characters_into_one_undo_unit` 与
        // `undoes_spaces_one_at_a_time`）。
        if is_whitespace_char(character) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);

        self.value = format!(
            "{}{}{}",
            utf16_slice(&self.value, 0, self.cursor),
            character,
            utf16_slice(&self.value, self.cursor, usize::MAX)
        );
        self.cursor += utf16_len(character);
    }

    fn handle_backspace(&mut self) {
        // 先打断 last_action：即使光标已在开头、下面直接返回，也会终止 kill 累积与 yank 链。
        self.last_action = None;
        if self.cursor > 0 {
            self.push_undo();
            // 按字素簇回退：emoji、组合字符整体删掉，不会被拆成半个。
            let before_cursor = utf16_slice(&self.value, 0, self.cursor);
            let grapheme_length = get_grapheme_segmenter()
                .segment(&before_cursor)
                .last()
                .map_or(1, |grapheme| utf16_len(grapheme.segment));
            self.value = format!(
                "{}{}",
                utf16_slice(&self.value, 0, self.cursor - grapheme_length),
                utf16_slice(&self.value, self.cursor, usize::MAX)
            );
            self.cursor -= grapheme_length;
        }
    }

    fn handle_forward_delete(&mut self) {
        // 与后退删除相同：先打断 last_action，再决定是否真的删除。
        self.last_action = None;
        if self.cursor < utf16_len(&self.value) {
            self.push_undo();
            let after_cursor = utf16_slice(&self.value, self.cursor, usize::MAX);
            let grapheme_length = get_grapheme_segmenter()
                .segment(&after_cursor)
                .next()
                .map_or(1, |grapheme| utf16_len(grapheme.segment));
            self.value = format!(
                "{}{}",
                utf16_slice(&self.value, 0, self.cursor),
                utf16_slice(&self.value, self.cursor + grapheme_length, usize::MAX)
            );
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let deleted_text = utf16_slice(&self.value, 0, self.cursor);
        self.kill_ring.push(
            &deleted_text,
            PushOptions {
                prepend: true,
                accumulate: self.last_action == Some(LastAction::Kill),
            },
        );
        self.last_action = Some(LastAction::Kill);
        self.value = utf16_slice(&self.value, self.cursor, usize::MAX);
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        if self.cursor >= utf16_len(&self.value) {
            return;
        }
        self.push_undo();
        let deleted_text = utf16_slice(&self.value, self.cursor, usize::MAX);
        self.kill_ring.push(
            &deleted_text,
            PushOptions {
                prepend: false,
                accumulate: self.last_action == Some(LastAction::Kill),
            },
        );
        self.last_action = Some(LastAction::Kill);
        self.value = utf16_slice(&self.value, 0, self.cursor);
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }

        // 先保存 lastAction（moveWordBackwards 会重置它）。
        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;

        let deleted_text = utf16_slice(&self.value, delete_from, self.cursor);
        self.kill_ring.push(
            &deleted_text,
            PushOptions {
                prepend: true,
                accumulate: was_kill,
            },
        );
        self.last_action = Some(LastAction::Kill);

        self.value = format!(
            "{}{}",
            utf16_slice(&self.value, 0, delete_from),
            utf16_slice(&self.value, self.cursor, usize::MAX)
        );
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        if self.cursor >= utf16_len(&self.value) {
            return;
        }

        let was_kill = self.last_action == Some(LastAction::Kill);

        self.push_undo();

        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;

        let deleted_text = utf16_slice(&self.value, self.cursor, delete_to);
        self.kill_ring.push(
            &deleted_text,
            PushOptions {
                prepend: false,
                accumulate: was_kill,
            },
        );
        self.last_action = Some(LastAction::Kill);

        self.value = format!(
            "{}{}",
            utf16_slice(&self.value, 0, self.cursor),
            utf16_slice(&self.value, delete_to, usize::MAX)
        );
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };

        self.push_undo();

        self.value = format!(
            "{}{}{}",
            utf16_slice(&self.value, 0, self.cursor),
            text,
            utf16_slice(&self.value, self.cursor, usize::MAX)
        );
        self.cursor += utf16_len(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }

        self.push_undo();

        // 删除上一次 yank 的文本（轮换前它在环顶）。
        //
        // TS 直接写 `this.value.slice(0, this.cursor - prevText.length)` 与
        // `this.value.slice(this.cursor)`；`cursor - prevText.length` 在 `setValue()` 缩短过
        // 值之后可以是负数，JS 的 `slice` 会把负下标解释为"从末尾倒数"。这里按同样的规则
        // 归一化两个下标，而不是把下标夹到 0。
        let prev_text = self.kill_ring.peek().unwrap_or_default().to_string();
        let prev_length = utf16_len(&prev_text) as isize;
        let cursor = self.cursor as isize;
        let total = utf16_len(&self.value);
        let delete_end = js_slice_end(total, cursor - prev_length);
        let after_start = js_slice_start(total, cursor);
        self.value = format!(
            "{}{}",
            utf16_slice(&self.value, 0, delete_end),
            utf16_slice(&self.value, after_start, usize::MAX)
        );
        // TS 的 `this.cursor -= prevText.length` 允许为负；Rust 的 `cursor` 是 `usize`
        // （公开的 `cursor()`/`InputState` 都按码元偏移暴露），负值无法表示。下面的插入
        // 位置仍按负下标语义计算；仅当**最终**光标仍为负时夹到 0（TS 只有在 `setValue()`
        // 缩得比被 yank 的文本还短时才可能落到这种状态，此时光标视觉位置与 0 等价）。
        let cursor_after_delete = cursor - prev_length;

        // 轮换并插入新条目。
        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or_default().to_string();
        let total = utf16_len(&self.value);
        let insert_start = js_slice_end(total, cursor_after_delete);
        let insert_end = js_slice_start(total, cursor_after_delete);
        self.value = format!(
            "{}{}{}",
            utf16_slice(&self.value, 0, insert_start),
            text,
            utf16_slice(&self.value, insert_end, usize::MAX)
        );
        self.cursor = (cursor_after_delete + utf16_len(&text) as isize).max(0) as usize;
        self.last_action = Some(LastAction::Yank);
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_backward(&self.value, self.cursor, None);
    }

    fn move_word_forwards(&mut self) {
        if self.cursor >= utf16_len(&self.value) {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_forward(&self.value, self.cursor, None);
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();

        // 清理粘贴文本：去掉换行/回车，tab → 4 个空格。
        // 顺序关键（先 `\r\n` 再去单个 `\r`/`\n`），因此不能合并成一次 replace。
        #[allow(clippy::collapsible_str_replace)]
        let clean_text = pasted_text
            .replace("\r\n", "")
            .replace('\r', "")
            .replace('\n', "")
            .replace('\t', "    ");

        self.value = format!(
            "{}{}{}",
            utf16_slice(&self.value, 0, self.cursor),
            clean_text,
            utf16_slice(&self.value, self.cursor, usize::MAX)
        );
        self.cursor += utf16_len(&clean_text);
    }
}

impl Focusable for Input {
    /// 焦点由 TUI 设置。
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

impl Component for Input {
    fn handle_input(&mut self, data: &str) {
        // 括号粘贴开始：\x1b[200~；结束：\x1b[201~
        let mut data = data.to_string();
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replacen("\x1b[200~", "", 1);
        }

        if self.is_in_paste {
            // 粘贴内容可能分多次 handle_input 到达：进入粘贴态后先全部追加到缓冲，直到看见
            // 结束标记才一次性插入（因此整个粘贴只占一个撤销单元）。缓冲没有上限；结束标记
            // 永远不到时不会插入任何内容，`is_in_paste` 也不会自行复位。
            self.paste_buffer.push_str(&data);

            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                // 完整的粘贴内容。
                let paste_content = self.paste_buffer[..end_index].to_string();
                self.handle_paste(&paste_content);

                // 重置粘贴状态。
                self.is_in_paste = false;

                // 处理粘贴结束后剩余输入（6 = `\x1b[201~` 的长度）。
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        let kb = get_keybindings();

        // 下面每个分支处理完都 `return`：首个匹配的键位说了算。顺序见模块文档
        // "键盘分发的顺序"，改动顺序会改变可见行为。

        // Escape/Cancel
        if kb.matches(&data, "tui.select.cancel") {
            if let Some(on_escape) = &mut self.on_escape {
                on_escape();
            }
            return;
        }

        // Undo
        if kb.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // Submit
        if kb.matches(&data, "tui.input.submit") || data == "\n" {
            // 先克隆值：复刻 TS 的按值传参语义，回调拿到的是提交瞬间的独立快照，
            // 之后输入框再怎么变化都不影响这次调用看到的字符串。
            // 提交本身不修改输入框，也不产生撤销快照——是否清空由宿主在回调里决定。
            let value = self.value.clone();
            if let Some(on_submit) = &mut self.on_submit {
                on_submit(&value);
            }
            return;
        }

        // Deletion
        if kb.matches(&data, "tui.editor.deleteCharBackward") {
            self.handle_backspace();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteCharForward") {
            self.handle_forward_delete();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineStart") {
            self.delete_to_line_start();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineEnd") {
            self.delete_to_line_end();
            return;
        }

        // Kill ring actions
        if kb.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        // Cursor movement
        if kb.matches(&data, "tui.editor.cursorLeft") {
            self.last_action = None;
            if self.cursor > 0 {
                let before_cursor = utf16_slice(&self.value, 0, self.cursor);
                let last_length = get_grapheme_segmenter()
                    .segment(&before_cursor)
                    .last()
                    .map_or(1, |grapheme| utf16_len(grapheme.segment));
                self.cursor -= last_length;
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorRight") {
            self.last_action = None;
            if self.cursor < utf16_len(&self.value) {
                let after_cursor = utf16_slice(&self.value, self.cursor, usize::MAX);
                let first_length = get_grapheme_segmenter()
                    .segment(&after_cursor)
                    .next()
                    .map_or(1, |grapheme| utf16_len(grapheme.segment));
                self.cursor += first_length;
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.cursor = 0;
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = utf16_len(&self.value);
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        // Kitty CSI-u 可打印字符（例如 `\x1b[97u` 表示 'a'）。
        if let Some(kitty_printable) = decode_kitty_printable(&data) {
            self.insert_character(&kitty_printable);
            return;
        }

        // 普通字符输入：接受可打印字符（含 Unicode），拒绝控制字符
        // （C0: 0x00-0x1F、DEL: 0x7F、C1: 0x80-0x9F）。
        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(&data);
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 只消费"在本行按下左键"：其余事件返回 None，让父级继续分发（或交给渲染器兜底）。
        if event.event_type != TuiMouseEventType::Press
            || event.button != TuiMouseButton::Left
            || event.y != 0
        {
            return None;
        }
        // 注意：`- 2` 是 TS 里假设 prompt 占 2 列的既有行为（自定义 prompt 下同样如此）。
        // 事件坐标已经是相对本组件的局部坐标（见 `TuiMouseEvent`），这里只需再叠加渲染时
        // 记下的滚动偏移，就得到目标在文本显示列空间中的位置。
        let visible_column = (event.x - 2).max(0);
        let target_column = self.rendered_start_column as isize + visible_column;
        let mut current_column: isize = 0;
        // 默认落在行尾：点在文本右侧的空白区域时不会把光标移到更左的位置。
        self.cursor = utf16_len(&self.value);
        let segments: Vec<(usize, isize)> = get_grapheme_segmenter()
            .segment(&self.value)
            .map(|grapheme| (grapheme.index, visible_width(grapheme.segment) as isize))
            .collect();
        // `grapheme.index` 是 UTF-16 偏移，与 `cursor` 同单位；命中条件是"起始列 ≤ 目标列 <
        // 结束列"，所以宽字符（占两列）的右半列也能把光标放到该字素上。
        for (index, width) in segments {
            let next_column = current_column + width;
            if target_column < next_column {
                self.cursor = index;
                break;
            }
            current_column = next_column;
        }
        // 点击同样会打断 kill 累积与 yank 链。
        self.last_action = None;
        Some(TuiMouseEventResult {
            handled: Some(true),
            focus: Some(true),
            ..Default::default()
        })
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        // 计算可见窗口。
        let available_width = width as isize - visible_width(&self.prompt) as isize;

        if available_width <= 0 {
            return vec![truncate_to_width(&self.prompt, width, "", false)];
        }
        let available_width_usize = available_width as usize;

        if self.value.is_empty() && !self.placeholder.is_empty() {
            // 占位分支提前返回，不会把 `rendered_start_column` 重置为 0（与 TS 相同）；
            // 此时值为空，鼠标定位的循环没有字素可命中，读到的旧值不影响结果。
            let placeholder =
                truncate_to_width(&self.placeholder, available_width_usize, "", false);
            // 伪光标包住占位文本的第一个字素（为空时用空格），保证光标有一个可反显的字符。
            let first = get_grapheme_segmenter()
                .segment(&placeholder)
                .next()
                .map(|grapheme| grapheme.segment.to_string());
            let at_cursor = first.unwrap_or_else(|| " ".to_string());
            let after_cursor = utf16_slice(&placeholder, utf16_len(&at_cursor), usize::MAX);
            let marker = if self.focused { CURSOR_MARKER } else { "" };
            let cursor_char = render_fake_cursor(&(self.placeholder_style)(&at_cursor));
            let text_with_cursor = format!(
                "{marker}{cursor_char}{}",
                (self.placeholder_style)(&after_cursor)
            );
            let padding = " ".repeat(
                (available_width - visible_width(&text_with_cursor) as isize).max(0) as usize,
            );
            return vec![format!("{}{text_with_cursor}{padding}", self.prompt)];
        }

        let mut visible_text = String::new();
        let mut cursor_display = self.cursor;
        self.rendered_start_column = 0;
        let total_width = visible_width(&self.value);

        if (total_width as isize) < available_width {
            // 全部放得下（末尾给光标留位置）。
            visible_text = self.value.clone();
        } else {
            // 需要水平滚动；光标在末尾时预留一列。
            // 预留这一列是因为行尾没有真实字符，伪光标会渲染成一个空格（见下方 `at_cursor`），
            // 不预留就会把它挤出窗口。
            let scroll_width = if self.cursor == utf16_len(&self.value) {
                available_width - 1
            } else {
                available_width
            };
            let cursor_col = visible_width(&utf16_slice(&self.value, 0, self.cursor)) as isize;

            if scroll_width > 0 {
                // 半窗启发式：光标靠近左端/右端时窗口贴边，否则把光标放在窗口中部附近。
                // 三个分支的边界使用 `half_width`（整数除法），与 TS 的 `Math.floor` 一致。
                let half_width = scroll_width / 2;
                let start_col = if cursor_col < half_width {
                    // 光标靠近起点。
                    0
                } else if cursor_col > total_width as isize - half_width {
                    // 光标靠近末尾。
                    (total_width as isize - scroll_width).max(0)
                } else {
                    // 光标在中间。
                    (cursor_col - half_width).max(0)
                };

                self.rendered_start_column = start_col as usize;
                // `strict = true`：整个字素必须落在右边界内，宽字符不会跨出窗口。
                visible_text =
                    slice_by_column(&self.value, start_col as usize, scroll_width as usize, true);
                let before_cursor = slice_by_column(
                    &self.value,
                    start_col as usize,
                    (cursor_col - start_col).max(0) as usize,
                    true,
                );
                // 用切片文本自身重新数 UTF-16 长度，得到窗口内光标前的码元数。不能拿
                // `cursor - start_col` 代替：`cursor` 是码元偏移、`start_col` 是显示列，
                // 一个宽字符占 2 列却只占 1 个码元，两者在非 ASCII 文本上并不等价。
                cursor_display = utf16_len(&before_cursor);
            } else {
                // `available_width == 1` 且光标在行尾：减去预留列后没有内容可显示。
                visible_text.clear();
                cursor_display = 0;
            }
        }

        // 构造带伪光标的一行：在光标位置插入光标字符。
        let after_cursor_source = utf16_slice(&visible_text, cursor_display, usize::MAX);
        let first_grapheme = get_grapheme_segmenter()
            .segment(&after_cursor_source)
            .next()
            .map(|grapheme| grapheme.segment.to_string());

        let before_cursor = utf16_slice(&visible_text, 0, cursor_display);
        let at_cursor = first_grapheme.unwrap_or_else(|| " ".to_string()); // 光标在末尾时用空格
        let after_cursor = utf16_slice(
            &visible_text,
            cursor_display + utf16_len(&at_cursor),
            usize::MAX,
        );

        // 硬件光标标记（零宽，置于伪光标前，供输入法定位）。
        let marker = if self.focused { CURSOR_MARKER } else { "" };

        // TUI 会用反显绘制伪光标；显示硬件光标时省略。
        let cursor_char = render_fake_cursor(&at_cursor);
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        // 计算可视宽度。padding 把内容区补到 `available_width` 列，所以整行可见宽度为
        // `width`（`width - prompt 宽度 <= 0` 的退化分支除外，那里只有截断后的 prompt）。
        let visual_length = visible_width(&text_with_cursor) as isize;
        let padding = " ".repeat((available_width - visual_length).max(0) as usize);
        let line = format!("{}{text_with_cursor}{padding}", self.prompt);

        vec![line]
    }

    fn invalidate(&mut self) {
        // TS：无可失效的缓存状态。
    }
}

// ============================================================================
// UTF-16 辅助（与 crate::text 的 JS 语义约定一致）
// ============================================================================

/// JS `string.length`：UTF-16 码元数。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `string.slice(start, end)`（`end == usize::MAX` 表示到结尾）。
///
/// 恰好切在代理对中间时，孤立代理项用 `U+FFFD` 代替（Rust 字符串不能保存孤立代理项）。
fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    if start == 0 && end == usize::MAX {
        return text.to_string();
    }
    let total = utf16_len(text);
    let start = start.min(total);
    let end = end.min(total).max(start);
    if start == 0 && end == total {
        return text.to_string();
    }
    let mut units: Vec<u16> = Vec::with_capacity(end - start);
    let mut position = 0usize;
    for ch in text.chars() {
        let mut buffer = [0u16; 2];
        for unit in ch.encode_utf16(&mut buffer) {
            if position >= start && position < end {
                units.push(*unit);
            }
            position += 1;
        }
        if position >= end {
            break;
        }
    }
    String::from_utf16_lossy(&units)
}

/// JS `string.slice(0, end)` 的 `end` 归一化。
///
/// 负数表示从末尾倒数（`max(len + end, 0)`），正数夹到 `[0, len]`。用于 `yankPop` 里
/// 可能为负的 `cursor - prevText.length`。
fn js_slice_end(total: usize, end: isize) -> usize {
    if end < 0 {
        (total as isize).saturating_add(end).max(0) as usize
    } else {
        (end as usize).min(total)
    }
}

/// JS `string.slice(start)` 的 `start` 归一化（负数同样从末尾倒数）。
fn js_slice_start(total: usize, start: isize) -> usize {
    if start < 0 {
        (total as isize).saturating_add(start).max(0) as usize
    } else {
        (start as usize).min(total)
    }
}
