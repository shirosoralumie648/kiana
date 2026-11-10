//! `packages/tui/test/bug-regression-isimageline-startswith-bug.test.ts`（11 个 it）的 Rust 移植。
//!
//! Bug：`isImageLine()` 曾用 `startsWith()`，在终端不支持图片时对含图片转义序列的行返回
//! `false`，TUI 随后对 300 KB+ 的 base64 行做宽度检查并崩溃
//! （`Rendered line exceeds terminal width (304401 > 115)`）。
//!
//! 修复：改成 `includes()`，图片转义序列出现在行内任意位置都能探测；四组标题保留以便对照。

use kiana_tui::terminal_image::is_image_line;

// ===========================================================================
// describe("Bug regression: isImageLine() crash with image escape sequences")
// ===========================================================================

// ---------------------------------------------------------------------------
// describe("Bug scenario: Terminal without image support")
// ---------------------------------------------------------------------------

/// 旧实现（有 bug）的等价物，仅用于演示 bug 本身。
fn old_is_image_line(line: &str, image_escape_prefix: Option<&str>) -> bool {
    match image_escape_prefix {
        Some(prefix) => line.starts_with(prefix),
        None => false,
    }
}

#[test]
fn old_implementation_would_return_false_causing_crash() {
    // 终端不支持图片时 prefix 为 null。
    let terminal_without_image_support: Option<&str> = None;

    let line_with_image_sequence =
        "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64data...\x07";

    let old_result = old_is_image_line(line_with_image_sequence, terminal_without_image_support);
    assert!(
        !old_result,
        "Bug: old implementation returns false for line containing image sequence when terminal has no image support"
    );
}

#[test]
fn new_implementation_returns_true_correctly() {
    let line_with_image_sequence =
        "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64data...\x07";
    assert!(
        is_image_line(line_with_image_sequence),
        "Fix: new implementation returns true for line containing image sequence"
    );
}

#[test]
fn new_implementation_detects_kitty_sequences_in_any_position() {
    let long = format!(
        "Text before \x1b_Ga=T,f=100{} text after",
        "A".repeat(300_000)
    );
    let scenarios = [
        "At start: \x1b_Ga=T,f=100,data...\x1b\\".to_string(),
        "Prefix \x1b_Ga=T,data...\x1b\\".to_string(),
        "Suffix text \x1b_Ga=T,data...\x1b\\ suffix".to_string(),
        "Middle \x1b_Ga=T,data...\x1b\\ more text".to_string(),
        long,
    ];
    for line in &scenarios {
        assert!(
            is_image_line(line),
            "Should detect Kitty sequence in: {}...",
            &line[..line.len().min(50)]
        );
    }
}

#[test]
fn new_implementation_detects_iterm2_sequences_in_any_position() {
    let long = format!(
        "Text before \x1b]1337;File=size=800,600;inline=1:{} text after",
        "B".repeat(300_000)
    );
    let scenarios = [
        "At start: \x1b]1337;File=size=100,100:base64...\x07".to_string(),
        "Prefix \x1b]1337;File=inline=1:data==\x07".to_string(),
        "Suffix text \x1b]1337;File=inline=1:data==\x07 suffix".to_string(),
        "Middle \x1b]1337;File=inline=1:data==\x07 more text".to_string(),
        long,
    ];
    for line in &scenarios {
        assert!(
            is_image_line(line),
            "Should detect iTerm2 sequence in: {}...",
            &line[..line.len().min(50)]
        );
    }
}

// ---------------------------------------------------------------------------
// describe("Integration: Tool execution scenario")
// ---------------------------------------------------------------------------

#[test]
fn detects_image_sequences_in_read_tool_output() {
    let tool_output_line =
        "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64image...\x07";
    assert!(
        is_image_line(tool_output_line),
        "Should detect image sequence in tool output line"
    );
}

#[test]
fn detects_kitty_sequences_from_image_component() {
    let kitty_line = "\x1b_Ga=T,f=100,t=f,d=base64data...\x1b\\\x1b_Gm=i=1;\x1b\\";
    assert!(
        is_image_line(kitty_line),
        "Should detect Kitty image component output"
    );
}

#[test]
fn handles_ansi_codes_before_image_sequences() {
    let lines = [
        "\x1b[31mError\x1b[0m: \x1b]1337;File=inline=1:base64==\x07",
        "\x1b[33mWarning\x1b[0m: \x1b_Ga=T,data...\x1b\\",
        "\x1b[1mBold\x1b[0m \x1b]1337;File=:base64==\x07\x1b[0m",
    ];
    for line in lines {
        assert!(
            is_image_line(line),
            "Should detect image sequence after ANSI codes: {}...",
            &line[..line.len().min(30)]
        );
    }
}

// ---------------------------------------------------------------------------
// describe("Crash scenario simulation")
// ---------------------------------------------------------------------------

#[test]
fn does_not_crash_on_very_long_lines_with_image_sequences() {
    let base64_char = "A".repeat(100);
    let iterm2_sequence = "\x1b]1337;File=size=800,600;inline=1:";
    let crash_line = format!(
        "Output: {}{} end of output",
        iterm2_sequence,
        base64_char.repeat(3040)
    );

    assert!(crash_line.len() > 300_000, "Test line should be > 300KB");
    assert!(
        is_image_line(&crash_line),
        "Should detect image sequence in very long line, preventing TUI crash"
    );
}

#[test]
fn handles_lines_exactly_matching_crash_log_dimensions() {
    // 崩溃日志：行宽 58649 字符、终端宽 115。
    let target_width = 58_649;
    let prefix = "Text";
    let sequence = "\x1b_Ga=T,f=100";
    let suffix = "End";
    let padding = "A".repeat(target_width - prefix.len() - sequence.len() - suffix.len());
    let line = format!("{prefix}{sequence}{padding}{suffix}");

    assert_eq!(line.len(), 58_649);
    assert!(
        is_image_line(&line),
        "Should detect image sequence in 58649-char line"
    );
}

// ---------------------------------------------------------------------------
// describe("Negative cases: Don't false positive")
// ---------------------------------------------------------------------------

#[test]
fn does_not_detect_images_in_regular_long_text() {
    let long_text = "A".repeat(100_000);
    assert!(
        !is_image_line(&long_text),
        "Should not detect images in plain long text"
    );
}

#[test]
fn does_not_detect_images_in_lines_with_file_paths() {
    let file_paths = [
        "/path/to/1337/image.jpg",
        "/usr/local/bin/File_converter",
        "~/Documents/1337File_backup.png",
        "./_G_test_file.txt",
    ];
    for path in file_paths {
        assert!(
            !is_image_line(path),
            "Should not falsely detect image sequence in path: {path}"
        );
    }
}
