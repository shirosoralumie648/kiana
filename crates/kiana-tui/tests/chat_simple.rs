//! `packages/tui/test/chat-simple.ts` 的手工脚本移植（`#[ignore]`）。
//!
//! TS 脚本：`ProcessTerminal` + `TuiMainScreen`，欢迎文本 + `Editor`（带自动补全）构成一个
//! 简易聊天界面；`/delete` 删除最后一条消息，`/clear` 清空消息，其余输入插入一条 Markdown
//! 用户消息并在 1 秒后追加一条模拟回复。
//!
//! **逐字保留**：欢迎文本、两条斜杠命令的语义（`children.length > 3` 时删除倒数第二个子项；
//! `/clear` 从下标 2 开始删除 `len - 3` 个子项）、消息插入位置（每次 `children.length - 1`）、
//! `isResponding` 期间禁止提交、回复期间的 `Loader`（`Thinking...`）。
//!
//! **偏差（已记录）**：
//! - `Editor` 属于 `tui-components` 的 editor 单元（本仓库仍是骨架桩），这里用 `Input` 作为
//!   输入面；自动补全提供器（`CombinedAutocompleteProvider`）只挂在 Editor 上，因此本移植把
//!   斜杠命令直接放进提交处理器（行为一致，少了补全下拉）。`Input` 不渲染 Editor 的占位提示
//!   行，但**不**为此额外插入子项（子项下标与插入位置是当前稳定契约，改动需同步测试）。
//! - TS 用 `setTimeout(…, 1000)` + `Math.random()` 选回复；手工演示测试里同步执行并固定选
//!   第一条回复（演示的是交互结构，不是随机性）。
//! - 作为 `#[ignore]` 测试只跑几次提交并渲染，然后退出（交互需要真实 TTY）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_tui::components::loader::Loader;
use kiana_tui::components::markdown::Markdown;
use kiana_tui::components::markdown::MarkdownTheme;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::components::text_component::Text;
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::tui::{FocusTarget, Tui, TuiStopOptions};

/// `test-themes.ts` 的 `defaultMarkdownTheme` 的等价物（恒等样式：演示不依赖颜色）。
fn plain_markdown_theme() -> MarkdownTheme {
    let plain: StyleFn = Arc::new(|text: &str| text.to_string());
    MarkdownTheme {
        heading: Arc::clone(&plain),
        link: Arc::clone(&plain),
        link_url: Arc::clone(&plain),
        code: Arc::clone(&plain),
        code_block: Arc::clone(&plain),
        code_block_border: Arc::clone(&plain),
        quote: Arc::clone(&plain),
        quote_border: Arc::clone(&plain),
        hr: Arc::clone(&plain),
        list_bullet: Arc::clone(&plain),
        bold: Arc::clone(&plain),
        italic: Arc::clone(&plain),
        strikethrough: Arc::clone(&plain),
        underline: Arc::clone(&plain),
        highlight_code: None,
        code_block_indent: None,
    }
}

/// TS `responses` 数组（固定取第一条，见模块文档）。
const RESPONSES: [&str; 8] = [
    "That's interesting! Tell me more.",
    "I see what you mean.",
    "Fascinating perspective!",
    "Could you elaborate on that?",
    "That makes sense to me.",
    "I hadn't thought of it that way.",
    "Great point!",
    "Thanks for sharing that.",
];

/// 提交处理器（TS `editor.onSubmit`）持有的共享演示状态。
struct ChatState {
    responding: bool,
}

/// 处理一次提交，返回是否请求重绘。
///
/// 插入位置固定为 `children.length - 1`（输入框之前），而不是固定的槽位
/// 下标——用户消息 → loader → （移除 loader）→ bot 回复，因此回复永远在用户消息**下方**。
fn handle_submit(tui: &mut TuiMainScreen, state: &Rc<RefCell<ChatState>>, value: &str) -> bool {
    // 正在回复时不允许提交。
    if state.borrow().responding {
        return false;
    }

    let trimmed = value.trim().to_string();

    if trimmed == "/delete" {
        // children[0] = 欢迎文本；children[1..n-1] = 消息；children[n] = 输入。
        // TS：`if (children.length > 3) children.splice(children.length - 2, 1)`。
        let children = tui.children();
        if children.len() > 3 {
            children.remove(children.len() - 2);
        }
        return true;
    }

    if trimmed == "/clear" {
        // TS：`children.splice(2, children.length - 3)`。
        let children = tui.children();
        let count = children.len().saturating_sub(3);
        for _ in 0..count {
            children.remove(2);
        }
        return true;
    }

    if trimmed.is_empty() {
        return false;
    }

    state.borrow_mut().responding = true;

    // `children.splice(children.length - 1, 0, userMessage)`。
    let user_message = Markdown::new(value.to_string(), 1, 1, plain_markdown_theme());
    let index = tui.children().len() - 1;
    tui.children().insert(index, Box::new(user_message));

    // `children.splice(children.length - 1, 0, loader)`：loader 在用户消息**之后**。
    let loader = Loader::new(
        None,
        Arc::new(|text: &str| format!("\x1b[36m{text}\x1b[0m")),
        Arc::new(|text: &str| format!("\x1b[2m{text}\x1b[0m")),
        "Thinking...",
        None,
    );
    let index = tui.children().len() - 1;
    tui.children().insert(index, Box::new(loader));

    // TS 用 `setTimeout(…, 1000)`；演示里同步完成：`tui.removeChild(loader)`。
    let loader_index = tui.children().len() - 2;
    tui.children().remove(loader_index);
    // `children.splice(children.length - 1, 0, botMessage)`。
    let bot_message = Markdown::new(RESPONSES[0], 1, 1, plain_markdown_theme());
    let index = tui.children().len() - 1;
    tui.children().insert(index, Box::new(bot_message));

    state.borrow_mut().responding = false;
    true
}

#[test]
#[ignore = "manual demo: needs a real TTY (ProcessTerminal + raw mode)"]
fn chat_simple_renders_one_frame() {
    let terminal = ProcessTerminal::new();
    let mut tui = TuiMainScreen::new(Box::new(terminal));

    tui.add_child(Box::new(Text::new(
        "Welcome to Simple Chat!\n\nType your messages below. Type '/' for commands. Press Ctrl+C to exit.",
    )));

    // TS 的子项只有 [欢迎文本, Editor]；编辑器的占位提示行由 Editor 自己渲染。输入面的替代
    // （`Input`）不渲染提示行，但**不能**因此多插一个子项——插入位置与 `/delete` 的语义都
    // 依赖 TS 的子项下标。
    let mut input = kiana_tui::components::input::Input::new(
        kiana_tui::components::input::InputOptions::default(),
    );
    input.on_submit = Some(Box::new(|_value: &str| {}));
    tui.add_child(Box::new(input));

    let input_index = tui.children().len() - 1;
    tui.set_focus(Some(FocusTarget::Component { index: input_index }));

    tui.start();
    tui.render_now(false);

    // 走一遍提交流程（模拟用户输入一条消息）。
    let state = Rc::new(RefCell::new(ChatState { responding: false }));
    let submitted = handle_submit(&mut tui, &state, "Hello there!");
    assert!(submitted, "submission should request a render");
    // TS 的顺序是 [welcome, user, bot, editor]：用户消息在回复**上方**。
    assert_eq!(tui.children().len(), 4, "welcome + user + bot + input");
    let rendered = tui.render_children(80).join("\n");
    let user_position = rendered
        .find("Hello there!")
        .expect("the user message should render");
    let bot_position = rendered
        .find(RESPONSES[0])
        .expect("the bot reply should render");
    assert!(
        user_position < bot_position,
        "the user message must render above the bot reply:\n{rendered}"
    );

    // `/delete` 删除倒数第二个子项（最后一条 bot 回复），用户消息保留。
    handle_submit(&mut tui, &state, "/delete");
    assert_eq!(tui.children().len(), 3, "welcome + user + input");
    let rendered = tui.render_children(80).join("\n");
    assert!(rendered.contains("Hello there!"));
    assert!(!rendered.contains(RESPONSES[0]));
    tui.render_now(false);

    // `/clear`：TS `children.splice(2, children.length - 3)`。
    handle_submit(&mut tui, &state, "Second message");
    assert_eq!(
        tui.children().len(),
        5,
        "welcome + user + user + bot + input"
    );
    let submitted = handle_submit(&mut tui, &state, "/clear");
    assert!(submitted);
    // 5 - 3 = 2 项被移除（第二条用户消息与它的回复）。
    assert_eq!(tui.children().len(), 3, "welcome + first user + input");
    let rendered = tui.render_children(80).join("\n");
    assert!(!rendered.contains("Second message"));
    assert!(rendered.contains("Hello there!"));

    tui.stop(TuiStopOptions::default());
}
