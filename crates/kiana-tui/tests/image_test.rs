//! `packages/tui/test/image-test.ts` 的手工脚本移植（`#[ignore]`）。
//!
//! TS 脚本读一张 PNG（默认 `/tmp/test-image.png`，可用 argv[2] 覆盖），打印终端能力与图片
//! 尺寸，然后在主屏 TUI 里渲染 `Text` / `Spacer` / `Image` / `Text`。
//!
//! Rust 版保留同样的步骤与打印文本；作为 `#[ignore]` 测试它只渲染一帧就退出（交互需要真实
//! TTY）。图片路径取 `KIANA_TEST_IMAGE` 环境变量，默认同为 `/tmp/test-image.png`；文件缺失时
//! 打印与 TS 相同的两行错误并返回（测试里不能 `process.exit(1)`）。

use kiana_tui::components::image::{Image, ImageOptions, ImageTheme};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::terminal_image::{get_capabilities, get_image_dimensions};
use kiana_tui::tui::{Component, FocusTarget, Tui, TuiStopOptions};

/// 只用 `handleInput` 的编辑器桩（收到 Ctrl+C 就退出），用于在非交互测试里驱动主屏渲染。
struct CtrlCExit {
    exit_requested: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Component for CtrlCExit {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn handle_input(&mut self, data: &str) {
        if data.chars().next().is_some_and(|ch| ch as u32 == 3) {
            self.exit_requested.set(true);
        }
    }
}

#[test]
#[ignore = "manual demo: needs a real TTY and a PNG file (KIANA_TEST_IMAGE)"]
fn image_rendering_test_renders_one_frame() {
    let test_image_path =
        std::env::var("KIANA_TEST_IMAGE").unwrap_or_else(|_| "/tmp/test-image.png".to_string());

    println!("Terminal capabilities: {:?}", get_capabilities());
    println!("Loading image from: {test_image_path}");

    let image_buffer = match std::fs::read(&test_image_path) {
        Ok(buffer) => buffer,
        Err(_) => {
            eprintln!("Failed to load image: {test_image_path}");
            eprintln!("Usage: node test/image-test.ts [path-to-image.png]");
            return;
        }
    };

    let base64_data = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&image_buffer)
    };
    let dimensions = get_image_dimensions(&base64_data, "image/png");

    println!("Image dimensions: {dimensions:?}");
    println!();

    let terminal = ProcessTerminal::new();
    let mut tui = TuiMainScreen::new(Box::new(terminal));

    tui.add_child(Box::new(Text::with_padding("Image Rendering Test", 1, 1)));
    tui.add_child(Box::new(Spacer::new()));

    match dimensions {
        Some(dimensions) => {
            tui.add_child(Box::new(Image::new(
                base64_data,
                "image/png",
                ImageTheme::plain(),
                ImageOptions {
                    max_width_cells: Some(60.0),
                    ..ImageOptions::default()
                },
                Some(dimensions),
            )));
        }
        None => {
            tui.add_child(Box::new(Text::with_padding(
                "Could not parse image dimensions",
                1,
                0,
            )));
        }
    }

    tui.add_child(Box::new(Spacer::new()));
    tui.add_child(Box::new(Text::with_padding("Press Ctrl+C to exit", 1, 0)));

    let exit_requested = std::rc::Rc::new(std::cell::Cell::new(false));
    tui.add_child(Box::new(CtrlCExit {
        exit_requested: std::rc::Rc::clone(&exit_requested),
    }));
    tui.set_focus(Some(FocusTarget::Component { index: 4 }));

    tui.start();
    tui.render_now(false);
    tui.stop(TuiStopOptions::default());
}
