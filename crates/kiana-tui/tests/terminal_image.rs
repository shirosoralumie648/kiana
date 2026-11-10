//! `packages/tui/test/terminal-image.test.ts`（80 个 it）的 Rust 移植。
//!
//! 结构按 `describe` 分组，标题保留。需要 `Image` 组件（tui-components 单元）的
//! 用例无法在此断言，全部对应到 `tests/terminal_image.rs` 之外，见文件末尾的 `#[ignore]` 清单。
//!
//! ## 环境变量与全局单例
//!
//! TS 的 `withEnv()` 先清空 `ENV_KEYS` 再注入，退出时还原。Rust 侧用同样的做法
//! （[`kiana_tui::terminal_image::clear_capability_env`]），并由 [`EnvGuard`] 持有进程级互斥锁，
//! 保证同一进程内的环境变量修改互不干扰。`cargo test` 默认多线程，因此**所有**读写环境的
//! 用例都必须先取 [`env_lock`]。

use std::sync::{Mutex, MutexGuard, OnceLock};

use kiana_tui::terminal_image::{
    CapabilityOverrides, CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions,
    KittyImageMetadata, TerminalCapabilities, calculate_image_cell_size,
    calculate_image_cell_size_default, calculate_image_rows, calculate_image_rows_default,
    clear_capability_env, crop_kitty_image_line, delete_all_kitty_images,
    delete_all_kitty_placements, delete_kitty_image, encode_iterm2, encode_kitty, get_capabilities,
    get_cell_dimensions, get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions,
    get_kitty_image_metadata, get_kitty_image_placement, get_kitty_image_placement_rows,
    get_max_kitty_image_rows, get_png_dimensions, get_webp_dimensions, hyperlink, image_fallback,
    is_image_line, register_kitty_image_metadata, render_image, reset_capabilities_cache,
    restore_capability_env, set_capabilities, set_capability_env, set_capability_overrides,
    set_cell_dimensions,
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 设置/删除任意环境变量（含不在 `CAPABILITY_ENV_KEYS` 里的，如 `HOME`）。
///
/// # Safety
///
/// 与 `kiana_tui::terminal_image::set_capability_env` 相同：调用方必须持有 [`env_lock`]。
fn set_os_env(key: &str, value: Option<std::ffi::OsString>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

/// 清空能力相关环境变量，并在 drop 时还原。
struct EnvGuard {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(overrides: &[(&str, Option<&str>)]) -> Self {
        let guard = env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let saved = clear_capability_env();
        for (key, value) in overrides {
            set_capability_env(key, *value);
        }
        Self {
            _guard: guard,
            saved,
        }
    }

    fn without_env() -> Self {
        Self::new(&[])
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_capability_env(std::mem::take(&mut self.saved));
        set_capability_overrides(CapabilityOverrides::default());
        reset_capabilities_cache();
        set_cell_dimensions(CellDimensions::default());
    }
}

// ===========================================================================
// describe("isImageLine")
// ===========================================================================

mod is_image_line {
    use super::*;

    // --- iTerm2 image protocol（5） ---

    #[test]
    fn should_detect_iterm2_image_escape_sequence_at_start_of_line() {
        let line = "\x1b]1337;File=size=100,100;inline=1:base64encodeddata==\x07";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_iterm2_image_escape_sequence_with_text_before_it() {
        let line = "Some text \x1b]1337;File=size=100,100;inline=1:base64data==\x07 more text";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_iterm2_image_escape_sequence_in_middle_of_long_line() {
        let line = format!(
            "Text before image...{}{}",
            "\x1b]1337;File=inline=1:verylongbase64data==", "...text after"
        );
        assert!(is_image_line(&line));
    }

    #[test]
    fn should_detect_iterm2_image_escape_sequence_at_end_of_line() {
        let line = "Regular text ending with \x1b]1337;File=inline=1:base64data==\x07";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_minimal_iterm2_image_escape_sequence() {
        assert!(is_image_line("\x1b]1337;File=:\x07"));
    }

    // --- Kitty image protocol（3） ---

    #[test]
    fn should_detect_kitty_image_escape_sequence_at_start_of_line() {
        let line = "\x1b_Ga=T,f=100,t=f,d=base64data...\x1b\\\x1b_Gm=i=1;\x1b\\";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_kitty_image_escape_sequence_with_text_before_it() {
        let line = "Output: \x1b_Ga=T,f=100;data...\x1b\\\x1b_Gm=i=1;\x1b\\";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_kitty_image_escape_sequence_with_padding() {
        let line = "  \x1b_Ga=T,f=100...\x1b\\\x1b_Gm=i=1;\x1b\\  ";
        assert!(is_image_line(line));
    }

    // --- Bug regression tests（4） ---

    #[test]
    fn should_detect_image_sequences_in_very_long_lines_304k_chars() {
        let base64_char = "A".repeat(100);
        let image_sequence = "\x1b]1337;File=size=800,600;inline=1:";
        let long_line = format!(
            "Text prefix {}{} suffix",
            image_sequence,
            base64_char.repeat(3000)
        );
        assert!(long_line.chars().count() > 300_000);
        assert!(is_image_line(&long_line));
    }

    #[test]
    fn should_detect_image_sequences_when_terminal_doesnt_support_images() {
        let line = "Read image file [image/jpeg]\x1b]1337;File=inline=1:base64data==\x07";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_image_sequences_with_ansi_codes_before_them() {
        let line = "\x1b[31mError output \x1b]1337;File=inline=1:image==\x07";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_image_sequences_with_ansi_codes_after_them() {
        let line = "\x1b_Ga=T,f=100:data...\x1b\\\x1b_Gm=i=1;\x1b\\\x1b[0m reset";
        assert!(is_image_line(line));
    }

    // --- Negative cases - lines without images（7） ---

    #[test]
    fn should_not_detect_images_in_plain_text_lines() {
        assert!(!is_image_line(
            "This is just a regular text line without any escape sequences"
        ));
    }

    #[test]
    fn should_not_detect_images_in_lines_with_only_ansi_codes() {
        assert!(!is_image_line(
            "\x1b[31mRed text\x1b[0m and \x1b[32mgreen text\x1b[0m"
        ));
    }

    #[test]
    fn should_not_detect_images_in_lines_with_cursor_movement_codes() {
        assert!(!is_image_line("\x1b[1A\x1b[2KLine cleared and moved up"));
    }

    #[test]
    fn should_not_detect_images_in_lines_with_partial_iterm2_sequences() {
        assert!(!is_image_line(
            "Some text with ]1337;File but missing ESC at start"
        ));
    }

    #[test]
    fn should_not_detect_images_in_lines_with_partial_kitty_sequences() {
        assert!(!is_image_line("Some text with _G but missing ESC at start"));
    }

    #[test]
    fn should_not_detect_images_in_empty_lines() {
        assert!(!is_image_line(""));
    }

    #[test]
    fn should_not_detect_images_in_lines_with_newlines_only() {
        assert!(!is_image_line("\n"));
        assert!(!is_image_line("\n\n"));
    }

    // --- Mixed content scenarios（3） ---

    #[test]
    fn should_detect_images_when_line_has_both_kitty_and_iterm2_sequences() {
        let line = "Kitty: \x1b_Ga=T...\x1b\\\x1b_Gm=i=1;\x1b\\ iTerm2: \x1b]1337;File=inline=1:data==\x07";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_detect_image_in_line_with_multiple_text_and_image_segments() {
        let line = "Start \x1b]1337;File=img1==\x07 middle \x1b]1337;File=img2==\x07 end";
        assert!(is_image_line(line));
    }

    #[test]
    fn should_not_falsely_detect_image_in_line_with_file_path_containing_keywords() {
        assert!(!is_image_line("/path/to/File_1337_backup/image.jpg"));
    }
}

// ===========================================================================
// describe("detectCapabilities")（27）
// ===========================================================================

mod detect_capabilities {
    use super::*;
    use kiana_tui::terminal_image::detect_capabilities;

    #[test]
    fn defaults_to_hyperlinks_false_for_unknown_terminals() {
        let _guard = EnvGuard::without_env();
        let caps = detect_capabilities(&mut || panic!("tmux probe must not run"));
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn applies_environment_overrides() {
        {
            let _guard = EnvGuard::new(&[
                ("KIANA_HYPERLINKS", Some("1")),
                ("KIANA_IMAGE_PROTOCOL", Some("kitty")),
                ("KIANA_TRUE_COLOR", Some("1")),
            ]);
            let caps = detect_capabilities(&mut || false);
            assert_eq!(
                caps,
                TerminalCapabilities {
                    images: Some(ImageProtocol::Kitty),
                    true_color: true,
                    hyperlinks: true
                }
            );
        }
        {
            let _guard = EnvGuard::new(&[
                ("TERM_PROGRAM", Some("iterm.app")),
                ("KIANA_HYPERLINKS", Some("0")),
                ("KIANA_IMAGE_PROTOCOL", Some("none")),
                ("KIANA_TRUE_COLOR", Some("0")),
            ]);
            let caps = detect_capabilities(&mut || false);
            assert_eq!(
                caps,
                TerminalCapabilities {
                    images: None,
                    true_color: false,
                    hyperlinks: false
                }
            );
        }
    }

    #[test]
    fn preserves_auto_detection_for_auto_environment_overrides() {
        let _guard = EnvGuard::new(&[
            ("TERM_PROGRAM", Some("ghostty")),
            ("KIANA_HYPERLINKS", Some("auto")),
            ("KIANA_IMAGE_PROTOCOL", Some("auto")),
            ("KIANA_TRUE_COLOR", Some("auto")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert_eq!(
            caps,
            TerminalCapabilities {
                images: Some(ImageProtocol::Kitty),
                true_color: true,
                hyperlinks: true
            }
        );
    }

    #[test]
    fn applies_and_clears_programmatic_overrides() {
        let _guard = EnvGuard::new(&[
            ("KIANA_HYPERLINKS", Some("1")),
            ("KIANA_IMAGE_PROTOCOL", Some("kitty")),
            ("KIANA_TRUE_COLOR", Some("1")),
        ]);
        set_capability_overrides(CapabilityOverrides {
            images: Some(None),
            true_color: Some(false),
            hyperlinks: Some(false),
        });
        assert_eq!(
            get_capabilities(),
            TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false
            }
        );
        set_capability_overrides(CapabilityOverrides::default());
        assert_eq!(
            get_capabilities(),
            TerminalCapabilities {
                images: Some(ImageProtocol::Kitty),
                true_color: true,
                hyperlinks: true
            }
        );
    }

    #[test]
    fn bypasses_the_tmux_probe_when_hyperlinks_are_overridden() {
        let _guard = EnvGuard::new(&[
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("KIANA_HYPERLINKS", Some("1")),
            ("KIANA_IMAGE_PROTOCOL", Some("kitty")),
        ]);
        let mut probed = false;
        let caps = detect_capabilities(&mut || {
            probed = true;
            false
        });
        assert!(!probed);
        assert!(caps.hyperlinks);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn enables_hyperlinks_under_tmux_when_the_client_forwards_them() {
        let _guard = EnvGuard::new(&[
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM_PROGRAM", Some("ghostty")),
        ]);
        let caps = detect_capabilities(&mut || true);
        assert!(caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn disables_hyperlinks_under_tmux_when_the_client_does_not_forward_them() {
        let _guard = EnvGuard::new(&[
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM_PROGRAM", Some("ghostty")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn checks_tmux_capability_when_term_starts_with_tmux() {
        let _guard = EnvGuard::new(&[
            ("TERM", Some("tmux-256color")),
            ("TERM_PROGRAM", Some("iterm.app")),
        ]);
        assert!(detect_capabilities(&mut || true).hyperlinks);
        let caps2 = detect_capabilities(&mut || false);
        assert!(!caps2.hyperlinks);
        assert_eq!(caps2.images, None);
    }

    #[test]
    fn forces_hyperlinks_false_when_term_starts_with_screen() {
        let _guard = EnvGuard::new(&[("TERM", Some("screen-256color"))]);
        let caps = detect_capabilities(&mut || true);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    // #10573
    #[test]
    fn enables_hyperlinks_and_kitty_images_for_herdr() {
        {
            let _guard = EnvGuard::new(&[
                ("TERM_PROGRAM", Some("herdr")),
                ("TERM", Some("xterm-256color")),
                ("COLORTERM", Some("truecolor")),
                ("KITTY_WINDOW_ID", Some("1")),
            ]);
            let caps = detect_capabilities(&mut || false);
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
        }
        {
            let _guard = EnvGuard::new(&[
                ("TERM_PROGRAM", Some("herdr")),
                ("KIANA_HYPERLINKS", Some("0")),
            ]);
            assert!(!detect_capabilities(&mut || true).hyperlinks);
        }
    }

    // #11131
    #[test]
    fn enables_hyperlinks_without_images_for_rex() {
        {
            let _guard = EnvGuard::new(&[
                ("TERM_PROGRAM", Some("rex")),
                ("TERM", Some("xterm-256color")),
                ("KITTY_WINDOW_ID", Some("1")),
            ]);
            let caps = detect_capabilities(&mut || false);
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
        }
        {
            let _guard = EnvGuard::new(&[
                ("TERM_PROGRAM", Some("rex")),
                ("KIANA_HYPERLINKS", Some("0")),
            ]);
            assert!(!detect_capabilities(&mut || true).hyperlinks);
        }
    }

    #[test]
    fn enables_hyperlinks_for_ghostty() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("ghostty"))]);
        assert!(detect_capabilities(&mut || false).hyperlinks);
    }

    #[test]
    fn does_not_disable_ghostty_images_solely_because_cmux_is_present() {
        let _guard = EnvGuard::new(&[
            ("TERM_PROGRAM", Some("ghostty")),
            ("CMUX_WORKSPACE_ID", Some("workspace")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.hyperlinks);
    }

    #[test]
    fn enables_hyperlinks_for_kitty() {
        let _guard = EnvGuard::new(&[("KITTY_WINDOW_ID", Some("1"))]);
        assert!(detect_capabilities(&mut || false).hyperlinks);
    }

    #[test]
    fn enables_hyperlinks_for_wezterm() {
        let _guard = EnvGuard::new(&[("WEZTERM_PANE", Some("0"))]);
        assert!(detect_capabilities(&mut || false).hyperlinks);
    }

    #[test]
    fn enables_images_and_hyperlinks_for_warp_via_term_program() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("WarpTerminal"))]);
        let caps = detect_capabilities(&mut || false);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
    }

    #[test]
    fn enables_images_and_hyperlinks_for_warp_via_warp_session_id() {
        let _guard = EnvGuard::new(&[("WARP_SESSION_ID", Some("some-session-id"))]);
        let caps = detect_capabilities(&mut || false);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
    }

    #[test]
    fn enables_images_and_hyperlinks_for_warp_via_warp_terminal_session_uuid() {
        let _guard = EnvGuard::new(&[(
            "WARP_TERMINAL_SESSION_UUID",
            Some("d0e1a2e5-7ca7-44cd-9037-ac7222011161"),
        )]);
        let caps = detect_capabilities(&mut || false);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
    }

    #[test]
    fn disables_images_for_warp_inside_tmux() {
        let _guard = EnvGuard::new(&[
            ("TERM_PROGRAM", Some("WarpTerminal")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ]);
        let caps = detect_capabilities(&mut || true);
        assert_eq!(caps.images, None);
        assert!(caps.hyperlinks);
    }

    #[test]
    fn enables_hyperlinks_for_iterm2() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("iterm.app"))]);
        assert!(detect_capabilities(&mut || false).hyperlinks);
    }

    #[test]
    fn enables_hyperlinks_for_vscode() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("vscode"))]);
        assert!(detect_capabilities(&mut || false).hyperlinks);
    }

    #[test]
    fn enables_alacritty_capabilities_for_zed() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("zed"))]);
        assert_eq!(
            detect_capabilities(&mut || false),
            TerminalCapabilities {
                images: None,
                true_color: true,
                hyperlinks: true
            }
        );
    }

    #[test]
    fn enables_truecolor_and_hyperlinks_for_windows_terminal_outside_multiplexers() {
        let _guard = EnvGuard::new(&[
            ("WT_SESSION", Some("session")),
            ("TERM", Some("xterm-256color")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn enables_truecolor_without_hyperlinks_for_jetbrains_terminal() {
        let _guard = EnvGuard::new(&[
            ("TERMINAL_EMULATOR", Some("JetBrains-JediTerm")),
            ("TERM", Some("xterm-256color")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert!(caps.true_color);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn does_not_inherit_windows_terminal_truecolor_through_tmux() {
        let _guard = EnvGuard::new(&[
            ("WT_SESSION", Some("session")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert!(!caps.true_color);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn trusts_explicit_truecolor_hints_through_tmux() {
        let _guard = EnvGuard::new(&[
            ("COLORTERM", Some("truecolor")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ]);
        let caps = detect_capabilities(&mut || false);
        assert!(caps.true_color);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    }

    #[test]
    fn detects_truecolor_from_direct_color_term_values() {
        let _guard = EnvGuard::new(&[("TERM", Some("xterm-direct"))]);
        assert!(detect_capabilities(&mut || false).true_color);
    }

    #[test]
    fn get_capabilities_caches_and_resets() {
        let _guard = EnvGuard::new(&[("TERM_PROGRAM", Some("ghostty"))]);
        let first = get_capabilities();
        assert_eq!(first.images, Some(ImageProtocol::Kitty));
        // 改环境但不清缓存：仍是旧值。
        set_capability_env("TERM_PROGRAM", Some("vscode"));
        assert_eq!(get_capabilities(), first);
        reset_capabilities_cache();
        assert_eq!(get_capabilities().images, None);
    }

    #[test]
    fn get_capabilities_honors_hyperlinks_override_without_probing() {
        let _guard = EnvGuard::new(&[("TMUX", Some("/tmp/tmux-1000/default,1234,0"))]);
        set_capability_overrides(CapabilityOverrides {
            images: None,
            true_color: None,
            hyperlinks: Some(true),
        });
        assert!(get_capabilities().hyperlinks);
    }
}

// ===========================================================================
// describe("iTerm2 image encoding")（1）
// ===========================================================================

#[test]
fn includes_the_decoded_payload_size_in_osc_1337_metadata() {
    let sequence = encode_iterm2(
        "AAAA",
        kiana_tui::terminal_image::Iterm2EncodeOptions {
            width: Some(2.0.into()),
            height: Some("auto".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        sequence,
        "\x1b]1337;File=inline=1;size=3;width=2;height=auto:AAAA\x07"
    );
}

// ===========================================================================
// describe("Kitty image cursor movement")（11）
// ===========================================================================

mod kitty_image_cursor_movement {
    use super::*;

    fn kitty_caps() {
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
    }

    #[test]
    fn can_request_no_terminal_side_cursor_movement() {
        let sequence = encode_kitty(
            "AAAA",
            kiana_tui::terminal_image::KittyEncodeOptions {
                columns: Some(2.0),
                rows: Some(2.0),
                move_cursor: Some(false),
                ..Default::default()
            },
        );
        assert!(sequence.starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=2,r=2;"));
    }

    #[test]
    fn reads_explicit_placement_rows_without_registered_metadata() {
        let sequence = encode_kitty(
            "AAAA",
            kiana_tui::terminal_image::KittyEncodeOptions {
                columns: Some(2.0),
                rows: Some(3.0),
                move_cursor: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(get_kitty_image_placement_rows(&sequence), Some(3));
    }

    #[test]
    fn suppresses_kitty_replies_for_delete_commands() {
        assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
        assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A,q=2\x1b\\");
        assert_eq!(delete_all_kitty_placements(), "\x1b_Ga=d,d=a,q=2\x1b\\");
    }

    #[test]
    fn preserves_render_images_default_terminal_side_cursor_movement() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 10.0,
            height_px: 10.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 20.0,
                height_px: 20.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(2.0),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert!(!result.sequence.contains(",C=1,"));
        assert_eq!(result.rows, 2);
    }

    #[test]
    fn can_opt_render_image_into_no_terminal_side_cursor_movement() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 10.0,
            height_px: 10.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 20.0,
                height_px: 20.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(2.0),
                move_cursor: Some(false),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert!(result.sequence.contains(",C=1,"));
        assert_eq!(result.rows, 2);
    }

    #[test]
    fn registers_metadata_and_crops_a_partially_visible_placement() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 10.0,
            height_px: 10.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 100.0,
                height_px: 100.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(3.0),
                image_id: Some(42),
                move_cursor: Some(false),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert_eq!(
            get_kitty_image_metadata(&result.sequence),
            Some(KittyImageMetadata {
                image_id: 42,
                columns: 3,
                rows: 3,
                width_px: 100.0,
                height_px: 100.0
            })
        );
        assert!(crop_kitty_image_line(&result.sequence, 2, 1).contains("y=66,h=34,r=1"));
    }

    #[test]
    fn creates_placement_only_commands_for_uploaded_and_cropped_images() {
        let _guard = EnvGuard::without_env();
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 42,
            columns: 3,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        });
        let transmission = encode_kitty(
            &"A".repeat(8192),
            kiana_tui::terminal_image::KittyEncodeOptions {
                columns: Some(3.0),
                rows: Some(3.0),
                image_id: Some(42.0),
                move_cursor: Some(false),
            },
        );
        let line = format!("left {} right", crop_kitty_image_line(&transmission, 2, 1));
        let placement = get_kitty_image_placement(&line).expect("placement");
        assert_eq!(get_kitty_image_placement_rows(&line), Some(1));
        assert_eq!(
            placement.transmission_bytes as isize,
            line.len() as isize - "left ".len() as isize - " right".len() as isize
        );
        assert_eq!(placement.estimated_decoded_bytes, 100.0 * 100.0 * 4.0);
        assert_eq!(placement.rows, 1);
        assert_eq!(
            placement.sequence,
            "\x1b_Ga=p,q=2,C=1,c=3,i=42,y=66,h=34,r=1\x1b\\"
        );
        assert_eq!(
            placement.replacement_line,
            format!("left {} right", placement.sequence)
        );
        assert!(!placement.replacement_line.contains("AAAA"));
    }

    /// 逐条对照 TS 26.12.1 的实测输出（`getKittyImageMetadata` / `getKittyImagePlacementRows` /
    /// `getKittyImagePlacement` / `cropKittyImageLine` 的边界输入）。
    #[test]
    fn kitty_control_parsing_edge_cases_match_ts() {
        let _guard = EnvGuard::without_env();
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 5,
            columns: 2,
            rows: 4,
            width_px: 100.0,
            height_px: 50.0,
        });

        // 负载里的 `;` 不参与控件解析（只有第一个 `;` 之前是控件串）。
        let line = "x\x1b_Gi=5;payload;withsemi\x1b\\y";
        let placement = get_kitty_image_placement(line).expect("placement");
        assert_eq!(placement.transmission_bytes, 25);
        assert_eq!(placement.sequence, "\x1b_Ga=p,q=2,i=5\x1b\\");
        assert_eq!(placement.replacement_line, "x\x1b_Ga=p,q=2,i=5\x1b\\y");
        assert_eq!(
            crop_kitty_image_line(line, 0, 1),
            "x\x1b_Gi=5,y=0,h=13,r=1;payload;withsemi\x1b\\y"
        );

        // `i=` 前必须有串首或 `,`：`\x1b_G i=5` 不匹配。
        assert_eq!(get_kitty_image_metadata("\x1b_G i=5;AA\x1b\\"), None);
        assert_eq!(get_kitty_image_placement_rows("\x1b_G i=5;AA\x1b\\"), None);
        // 前导零按十进制解析（`i=05` 命中 id 5），但序列里保留原文。
        assert_eq!(
            get_kitty_image_placement("\x1b_Gi=05;AA\x1b\\")
                .expect("placement")
                .sequence,
            "\x1b_Ga=p,q=2,i=05\x1b\\"
        );
        // `r=0` 视作未给出 → 回退注册表的 rows；`r=09` → 9。
        assert_eq!(
            get_kitty_image_placement_rows("\x1b_Gi=5,r=0;AA\x1b\\"),
            Some(4)
        );
        assert_eq!(
            get_kitty_image_placement_rows("\x1b_Gi=5,r=09;AA\x1b\\"),
            Some(9)
        );
        // 未知键（`junk`）与空项被丢弃。
        assert_eq!(
            get_kitty_image_placement("\x1b_Gi=5,junk;AA\x1b\\")
                .expect("placement")
                .sequence,
            "\x1b_Ga=p,q=2,i=5\x1b\\"
        );
        // 分块续传：`m=1` 时继续找下一个 `\x1b\`，transmissionBytes 覆盖全部块。
        assert_eq!(
            get_kitty_image_placement("\x1b_Gi=5,m=1;AA\x1b\\\x1b_Gm=0;BB\x1b\\")
                .expect("placement")
                .transmission_bytes,
            26
        );
        // `m=1` 之后不是 `\x1b_G` 开头 → 放弃。
        assert_eq!(
            get_kitty_image_placement("\x1b_Gi=5,m=1;AA\x1b\\GARBAGE"),
            None
        );
    }

    #[test]
    fn honors_max_height_cells_by_reducing_rendered_width() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 10.0,
            height_px: 10.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 10.0,
                height_px: 100.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(10.0),
                max_height_cells: Some(5.0),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert_eq!(result.rows, 5);
        assert!(result.sequence.contains(",c=1,r=5"));
    }

    // 以下 3 个用例（`caps Image component height to a square pixel box by default` /
    // `places image sequence on first line with empty padding rows` /
    // `truncates long image fallback lines to render width`）在
    // `src/components/image.rs` 的 `#[cfg(test)]` 模块里逐字移植（同名）；Image 组件已合并，
    // 因此这里不再保留 `#[ignore]` 占位。
}

// ===========================================================================
// describe("image cell sizing")（7 = Kitty 5 + iTerm2 2）
// ===========================================================================

mod image_cell_sizing {
    use super::*;

    fn kitty_caps() {
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
    }

    fn iterm2_caps() {
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        });
    }

    #[test]
    fn reserves_at_least_one_kitty_row_for_thin_images() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 1200.0,
                height_px: 12.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(60.0),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert_eq!(result.rows, 1);
        assert!(result.sequence.contains(",c=60,r=1;"));
    }

    #[test]
    fn keeps_kitty_placement_reserved_lines_and_cropping_metadata_consistent_across_width_changes()
    {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        });
        // Image 组件的行为：maxWidth = max(1, min(width - 2, maxWidthCells))，
        // defaultMaxHeight = max(1, ceil(maxWidth * cellW / cellH))，Kitty 走 moveCursor: false。
        let render = |width: f64| {
            let max_width = 1.0f64.max((width - 2.0).min(60.0));
            let cell = get_cell_dimensions();
            let default_max_height =
                1.0f64.max((max_width * cell.width_px / cell.height_px).ceil());
            let dimensions = ImageDimensions {
                width_px: 615.0,
                height_px: 86.0,
            };
            let image_id = 8938u64;
            // Image 在 renderImage 前注册的 ID 就是 options.imageId，这里等价地交给 renderImage。
            render_image(
                "AAAA",
                dimensions,
                ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(default_max_height),
                    image_id: Some(image_id),
                    move_cursor: Some(false),
                    ..Default::default()
                },
            )
            .expect("kitty render")
        };

        let result = render(62.0);
        assert_eq!(result.rows, 4);
        assert!(result.sequence.contains(",c=60,r=4,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&result.sequence),
            Some(KittyImageMetadata {
                image_id: 8938,
                columns: 60,
                rows: 4,
                width_px: 615.0,
                height_px: 86.0
            })
        );
        let cropped = crop_kitty_image_line(&result.sequence, 1, 2);
        assert_eq!(
            get_kitty_image_placement(&cropped).map(|p| p.sequence),
            Some("\x1b_Ga=p,q=2,C=1,c=60,i=8938,y=21,h=44,r=2\x1b\\".to_string())
        );

        let narrower = render(32.0);
        assert_eq!(narrower.rows, 2);
        assert!(narrower.sequence.contains(",c=30,r=2,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&narrower.sequence).map(|m| m.rows),
            Some(2)
        );
    }

    #[test]
    fn keeps_the_ceiling_placement_when_rounding_down_would_increase_distortion() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 15.0,
            height_px: 28.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 615.0,
                height_px: 86.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(60.0),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert_eq!(result.rows, 5);
        assert!(result.sequence.contains(",c=60,r=5;"));
    }

    #[test]
    fn keeps_height_limited_kitty_columns_reservations_and_crop_metadata_consistent() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 14.0,
            height_px: 28.0,
        });
        let render = |width: f64| {
            let max_width = 1.0f64.max((width - 2.0).min(30.0));
            let cell = get_cell_dimensions();
            let default_max_height =
                1.0f64.max((max_width * cell.width_px / cell.height_px).ceil());
            render_image(
                "AAAA",
                ImageDimensions {
                    width_px: 400.0,
                    height_px: 900.0,
                },
                ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(default_max_height),
                    image_id: Some(8938),
                    move_cursor: Some(false),
                    ..Default::default()
                },
            )
            .expect("kitty render")
        };

        let result = render(32.0);
        assert_eq!(result.rows, 15);
        assert!(result.sequence.contains(",c=13,r=15,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&result.sequence),
            Some(KittyImageMetadata {
                image_id: 8938,
                columns: 13,
                rows: 15,
                width_px: 400.0,
                height_px: 900.0
            })
        );
        assert_eq!(
            get_kitty_image_placement(&crop_kitty_image_line(&result.sequence, 1, 2))
                .map(|p| p.sequence),
            Some("\x1b_Ga=p,q=2,C=1,c=13,i=8938,y=60,h=120,r=2\x1b\\".to_string())
        );

        let narrower = render(22.0);
        assert_eq!(narrower.rows, 10);
        assert!(narrower.sequence.contains(",c=9,r=10,i=8938;"));
    }

    #[test]
    fn chooses_thin_kitty_widths_by_proportions_while_keeping_at_least_one_column() {
        let _guard = EnvGuard::without_env();
        kitty_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 1.0,
            height_px: 1.0,
        });
        for (width_px, columns) in [(1.0, 1usize), (140.0, 1), (149.0, 2)] {
            let result = render_image(
                "AAAA",
                ImageDimensions {
                    width_px,
                    height_px: 1000.0,
                },
                ImageRenderOptions {
                    max_width_cells: Some(30.0),
                    max_height_cells: Some(10.0),
                    ..Default::default()
                },
            )
            .expect("kitty render");
            assert_eq!(result.columns, columns);
            assert_eq!(result.rows, 10);
            assert!(result.sequence.contains(&format!(",c={columns},r=10;")));
        }
    }

    #[test]
    fn keeps_iterm2s_ceiling_width_when_height_limited() {
        let _guard = EnvGuard::without_env();
        iterm2_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 14.0,
            height_px: 28.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 400.0,
                height_px: 900.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(30.0),
                max_height_cells: Some(15.0),
                ..Default::default()
            },
        )
        .expect("iterm2 render");
        assert_eq!(result.columns, 14);
        assert_eq!(result.rows, 15);
        assert_eq!(
            result.sequence,
            "\x1b]1337;File=inline=1;size=3;width=14;height=auto:AAAA\x07"
        );
    }

    #[test]
    fn keeps_iterm2s_ceiling_based_reserved_lines_and_cursor_offset() {
        let _guard = EnvGuard::without_env();
        iterm2_caps();
        set_cell_dimensions(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        });
        // Image 组件（iTerm2 路径）：rows-1 个空行 + `\x1b[${rows-1}A` 前缀的最后一行。
        let width = 62.0f64;
        let max_width = 1.0f64.max((width - 2.0).min(60.0));
        let cell = get_cell_dimensions();
        let default_max_height = 1.0f64.max((max_width * cell.width_px / cell.height_px).ceil());
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 615.0,
                height_px: 86.0,
            },
            ImageRenderOptions {
                max_width_cells: Some(max_width),
                max_height_cells: Some(default_max_height),
                ..Default::default()
            },
        )
        .expect("iterm2 render");
        let mut lines: Vec<String> = vec![String::new(); result.rows - 1];
        let row_offset = result.rows - 1;
        let move_up = if row_offset > 0 {
            format!("\x1b[{row_offset}A")
        } else {
            String::new()
        };
        lines.push(format!("{move_up}{}", result.sequence));
        assert_eq!(
            lines,
            vec![
                "",
                "",
                "",
                "",
                "\x1b[4A\x1b]1337;File=inline=1;size=3;width=60;height=auto:AAAA\x07"
            ]
        );
    }
}

// ===========================================================================
// describe("imageFallback")（4）
// ===========================================================================

mod image_fallback {
    use super::*;
    use kiana_tui::terminal_image::home_dir;

    fn with_hyperlinks(hyperlinks: bool) {
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks,
        });
    }

    /// TS 的 `homedir()`——测试拿它拼期望值，避免硬编码 `/home/tester`。
    fn home() -> String {
        home_dir()
            .expect("home directory")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn shortens_home_prefixed_absolute_paths_without_hyperlinks() {
        let _guard = EnvGuard::without_env();
        with_hyperlinks(false);
        let abs = format!("{}/.kiana/agent/shot.png", home());
        let result = image_fallback(
            "image/png",
            Some(ImageDimensions {
                width_px: 1280.0,
                height_px: 720.0,
            }),
            Some(&abs),
        );
        assert_eq!(
            result,
            "[Image: ~/.kiana/agent/shot.png [image/png] 1280x720]"
        );
    }

    #[test]
    fn wraps_shortened_absolute_paths_in_osc_8_file_links_when_hyperlinks_are_enabled() {
        let _guard = EnvGuard::without_env();
        with_hyperlinks(true);
        let abs = format!("{}/.kiana/agent/shot.png", home());
        let result = image_fallback(
            "image/png",
            Some(ImageDimensions {
                width_px: 10.0,
                height_px: 10.0,
            }),
            Some(&abs),
        );
        assert!(
            result.contains("\x1b]8;;file://"),
            "expected OSC 8 file link"
        );
        assert!(
            result.contains(&abs.replace('\\', "/")) || result.contains(&abs),
            "file URL should target absolute path"
        );
        // 可见文本必须是 ~/... 而不是展开后的 home 路径。
        let mut visible = String::new();
        let mut rest = result.as_str();
        while let Some(start) = rest.find("\x1b]8;;") {
            visible.push_str(&rest[..start]);
            match rest[start..].find("\x1b\\") {
                Some(end) => rest = &rest[start + end + 2..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        visible.push_str(rest);
        assert_eq!(
            visible,
            "[Image: ~/.kiana/agent/shot.png [image/png] 10x10]"
        );
    }

    /// `$HOME=""` 时 `os.homedir()` 返回空串，但 `shortenImagePath` 的 `if (home && ...)` 要求
    /// home 非空，否则任意绝对路径都会被缩成 `~...`。
    #[test]
    fn empty_home_does_not_shorten_paths() {
        // `HOME` 不在 `CAPABILITY_ENV_KEYS` 里，`EnvGuard` 不会替我们还原——自行保存/恢复。
        struct HomeGuard {
            _guard: MutexGuard<'static, ()>,
            saved: Option<std::ffi::OsString>,
        }
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match self.saved.take() {
                    Some(value) => set_os_env("HOME", Some(value)),
                    None => set_os_env("HOME", None),
                }
            }
        }
        let guard = HomeGuard {
            _guard: env_lock()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            saved: std::env::var_os("HOME"),
        };
        with_hyperlinks(false);
        set_os_env("HOME", Some(std::ffi::OsString::from("")));
        let result = image_fallback(
            "image/png",
            Some(ImageDimensions {
                width_px: 2.0,
                height_px: 3.0,
            }),
            Some("/tmp/shot.png"),
        );
        assert_eq!(result, "[Image: /tmp/shot.png [image/png] 2x3]");
        drop(guard);
    }

    #[test]
    fn leaves_bare_basenames_unchanged_and_does_not_hyperlink_them() {
        let _guard = EnvGuard::without_env();
        with_hyperlinks(true);
        let result = image_fallback(
            "image/png",
            Some(ImageDimensions {
                width_px: 1.0,
                height_px: 1.0,
            }),
            Some("clankolas.png"),
        );
        assert_eq!(result, "[Image: clankolas.png [image/png] 1x1]");
        assert!(
            !result.contains("\x1b]8;"),
            "basename must not be hyperlinked"
        );
    }

    #[test]
    fn omits_filename_segment_when_not_provided() {
        let _guard = EnvGuard::without_env();
        with_hyperlinks(false);
        assert_eq!(
            image_fallback(
                "image/png",
                Some(ImageDimensions {
                    width_px: 8.0,
                    height_px: 6.0,
                }),
                None
            ),
            "[Image: [image/png] 8x6]"
        );
    }
}

// ===========================================================================
// describe("Image transcoding")（4）
// ===========================================================================
//
// `sends converted PNG data sized from the PNG` /
// `renders a text fallback until a working transcoder is registered` /
// `converts each image once` / `does not convert PNG data or iTerm2 output`
// 在 `src/components/image.rs` 的 `#[cfg(test)]` 模块里逐字移植（同名）；Image 组件与
// `setImageTranscoder` 现在都在本 crate 内实现，因此不在这里重复占位。

// ===========================================================================
// describe("hyperlink")（4）
// ===========================================================================

#[test]
fn wraps_text_in_osc_8_open_and_close_sequences() {
    assert_eq!(
        hyperlink("click me", "https://example.com"),
        "\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\"
    );
}

#[test]
fn preserves_ansi_styling_inside_the_hyperlink() {
    let styled = "\x1b[4m\x1b[34mclick me\x1b[0m";
    let result = hyperlink(styled, "https://example.com");
    assert!(result.starts_with("\x1b]8;;https://example.com\x1b\\"));
    assert!(result.contains(styled));
    assert!(result.ends_with("\x1b]8;;\x1b\\"));
}

#[test]
fn works_with_empty_text() {
    assert_eq!(
        hyperlink("", "https://example.com"),
        "\x1b]8;;https://example.com\x1b\\\x1b]8;;\x1b\\"
    );
}

#[test]
fn works_with_file_uris() {
    let result = hyperlink("README.md", "file:///home/user/README.md");
    assert!(result.contains("file:///home/user/README.md"));
    assert!(result.contains("README.md"));
}

// ===========================================================================
// 黄金用例：由 Node 26.12.1 上的 TS 参考实现生成（tests/fixtures/terminal_image/）
// ===========================================================================

mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    fn load(name: &str) -> Value {
        let text = match name {
            "encode.json" => include_str!("fixtures/terminal_image/encode.json"),
            "cells.json" => include_str!("fixtures/terminal_image/cells.json"),
            "placement.json" => include_str!("fixtures/terminal_image/placement.json"),
            "misc.json" => include_str!("fixtures/terminal_image/misc.json"),
            "base64.json" => include_str!("fixtures/terminal_image/base64.json"),
            "windows_urls.json" => include_str!("fixtures/terminal_image/windows_urls.json"),
            "kitty_chunks.json" => include_str!("fixtures/terminal_image/kitty_chunks.json"),
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(text).expect("valid fixture json")
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Dims {
        width_px: f64,
        height_px: f64,
    }

    fn dims(value: &Value) -> Option<ImageDimensions> {
        if value.is_null() {
            return None;
        }
        let parsed: Dims = serde_json::from_value(value.clone()).expect("dims");
        Some(ImageDimensions {
            width_px: parsed.width_px,
            height_px: parsed.height_px,
        })
    }

    fn cell_size(value: &Value) -> (usize, usize) {
        (
            value["columns"].as_u64().expect("columns") as usize,
            value["rows"].as_u64().expect("rows") as usize,
        )
    }

    #[test]
    fn encode_kitty_matches_ts() {
        let fixture = load("encode.json");
        let mut checked = 0usize;
        for case in fixture["encode_kitty"].as_array().expect("array") {
            let len = case["data_len"].as_u64().expect("data_len") as usize;
            let head = case["data_head"].as_str().expect("data_head");
            let mut data = head.to_string();
            data.push_str(&"A".repeat(len.saturating_sub(head.len())));
            let options = &case["options"];
            let expected = case["expected"].as_str().expect("expected");
            let opts = kiana_tui::terminal_image::KittyEncodeOptions {
                columns: options["columns"].as_f64(),
                rows: options["rows"].as_f64(),
                image_id: options["imageId"].as_f64(),
                move_cursor: options["moveCursor"].as_bool(),
            };
            assert_eq!(encode_kitty(&data, opts), expected, "case {case}");
            checked += 1;
        }
        assert_eq!(checked, 13);
    }

    #[test]
    fn encode_iterm2_matches_ts() {
        let fixture = load("encode.json");
        for case in fixture["encode_iterm2"].as_array().expect("array") {
            let data = case["data"].as_str().expect("data");
            let expected = case["expected"].as_str().expect("expected");
            let options = &case["options"];
            let to_size = |v: &Value| -> Option<kiana_tui::terminal_image::Iterm2Size> {
                if let Some(number) = v.as_f64() {
                    Some(kiana_tui::terminal_image::Iterm2Size::Number(number))
                } else {
                    v.as_str().map(kiana_tui::terminal_image::Iterm2Size::from)
                }
            };
            let opts = kiana_tui::terminal_image::Iterm2EncodeOptions {
                width: to_size(&options["width"]),
                height: to_size(&options["height"]),
                name: options["name"].as_str().map(str::to_string),
                preserve_aspect_ratio: options["preserveAspectRatio"].as_bool(),
                inline: options["inline"].as_bool(),
            };
            assert_eq!(encode_iterm2(data, opts), expected, "case {case}");
        }
    }

    #[test]
    fn dimension_headers_match_ts() {
        let fixture = load("encode.json");
        for case in fixture["dimensions"].as_array().expect("array") {
            let data = case["data"].as_str().expect("data");
            let expected = dims(&case["expected"]);
            let actual = match case["fn"].as_str().expect("fn") {
                "png" => get_png_dimensions(data),
                "jpeg" => get_jpeg_dimensions(data),
                "gif" => get_gif_dimensions(data),
                "webp" => get_webp_dimensions(data),
                other => panic!("unknown fn {other}"),
            };
            assert_eq!(actual, expected, "case {case}");
        }
    }

    #[test]
    fn image_dimension_dispatch_matches_ts() {
        let fixture = load("encode.json");
        for case in fixture["image_dimensions_dispatch"]
            .as_array()
            .expect("array")
        {
            let data = case["data"].as_str().expect("data");
            let mime = case["mime"].as_str().expect("mime");
            let expected = dims(&case["expected"]);
            assert_eq!(get_image_dimensions(data, mime), expected, "case {case}");
        }
    }

    #[test]
    fn calculate_image_cell_size_matches_ts() {
        let fixture = load("cells.json");
        let mut checked = 0usize;
        for case in fixture["cell_sizes"].as_array().expect("array") {
            let image = ImageDimensions {
                width_px: case["image"]["widthPx"].as_f64().expect("w"),
                height_px: case["image"]["heightPx"].as_f64().expect("h"),
            };
            let cell = CellDimensions {
                width_px: case["cell"]["widthPx"].as_f64().expect("cw"),
                height_px: case["cell"]["heightPx"].as_f64().expect("ch"),
            };
            let (columns, rows) = cell_size(&case["expected"]);
            let actual = calculate_image_cell_size(
                image,
                case["maxW"].as_f64().expect("maxW"),
                case["maxH"].as_f64(),
                cell,
                case["opt"].as_bool().expect("opt"),
            );
            assert_eq!(
                (actual.columns, actual.rows),
                (columns, rows),
                "case {case}"
            );
            checked += 1;
        }
        assert!(checked >= 1000, "expected a dense matrix, got {checked}");
    }

    #[test]
    fn calculate_image_rows_matches_ts() {
        let fixture = load("cells.json");
        for case in fixture["cell_rows"].as_array().expect("array") {
            let image = ImageDimensions {
                width_px: case["image"]["widthPx"].as_f64().expect("w"),
                height_px: case["image"]["heightPx"].as_f64().expect("h"),
            };
            let cell = CellDimensions {
                width_px: case["cell"]["widthPx"].as_f64().expect("cw"),
                height_px: case["cell"]["heightPx"].as_f64().expect("ch"),
            };
            let expected = case["expected"].as_u64().expect("expected") as usize;
            assert_eq!(
                calculate_image_rows(image, case["maxW"].as_f64().expect("maxW"), cell),
                expected,
                "case {case}"
            );
        }
    }

    /// 放置/裁剪用例按文件顺序重放：注册表是有状态的，顺序必须与生成时一致。
    #[test]
    fn kitty_placement_and_crop_match_ts() {
        let _guard = EnvGuard::without_env();
        let fixture = load("placement.json");
        // 注册表是有状态的：每个用例重放它生成时的**完整**注册序列（同一 id 会被重复注册，
        // 每次都会换一个 transmission generation）。generation 的绝对值依赖本线程此前是否
        // 注册过其它图片，所以这里与 TS 对齐的是"同一 id 的 generation 随注册单调递增且
        // 与最后一条注册一致"，不比较绝对值本身。
        let mut registry: Vec<KittyImageMetadata> = Vec::new();
        for case in fixture["placement"].as_array().expect("array") {
            let line = case["line"].as_str().expect("line");
            let name = case["name"].as_str().expect("name");

            let expected_registry: Vec<KittyImageMetadata> = case["registry"]
                .as_array()
                .expect("registry")
                .iter()
                .map(|reg| KittyImageMetadata {
                    image_id: reg["imageId"].as_u64().expect("imageId"),
                    columns: reg["columns"].as_u64().expect("columns") as usize,
                    rows: reg["rows"].as_u64().expect("rows") as usize,
                    width_px: reg["widthPx"].as_f64().expect("widthPx"),
                    height_px: reg["heightPx"].as_f64().expect("heightPx"),
                })
                .collect();
            while registry.len() < expected_registry.len() {
                let entry = expected_registry[registry.len()];
                register_kitty_image_metadata(entry);
                registry.push(entry);
            }

            let expected_metadata = &case["metadata"];
            let actual_metadata = get_kitty_image_metadata(line);
            if expected_metadata.is_null() {
                assert_eq!(actual_metadata, None, "{name}: metadata");
            } else {
                let actual = actual_metadata.unwrap_or_else(|| panic!("{name}: expected metadata"));
                assert_eq!(
                    actual.image_id,
                    expected_metadata["imageId"].as_u64().expect("imageId"),
                    "{name}: imageId"
                );
                assert_eq!(
                    actual.columns,
                    expected_metadata["columns"].as_u64().expect("columns") as usize,
                    "{name}: columns"
                );
                assert_eq!(
                    actual.rows,
                    expected_metadata["rows"].as_u64().expect("rows") as usize,
                    "{name}: rows"
                );
                assert_eq!(
                    actual.width_px,
                    expected_metadata["widthPx"].as_f64().expect("widthPx"),
                    "{name}: widthPx"
                );
                assert_eq!(
                    actual.height_px,
                    expected_metadata["heightPx"].as_f64().expect("heightPx"),
                    "{name}: heightPx"
                );
            }

            assert_eq!(
                get_kitty_image_placement_rows(line),
                case["placementRows"].as_u64().map(|v| v as usize),
                "{name}: placementRows"
            );

            let actual_placement = get_kitty_image_placement(line);
            let expected_placement = &case["placement"];
            if expected_placement.is_null() {
                assert_eq!(actual_placement, None, "{name}: placement");
            } else {
                let actual =
                    actual_placement.unwrap_or_else(|| panic!("{name}: expected placement"));
                assert_eq!(
                    actual.image_id,
                    expected_placement["imageId"].as_u64().expect("imageId"),
                    "{name}: placement imageId"
                );
                // 同一 id 的最后一次注册序号（从 1 开始）。
                let expected_generation = registry
                    .iter()
                    .rposition(|entry| entry.image_id == actual.image_id)
                    .map(|index| index as u64 + 1)
                    .expect("registered id");
                assert_eq!(
                    actual.transmission_generation, expected_generation,
                    "{name}: transmissionGeneration"
                );
                assert_eq!(
                    actual.transmission_bytes,
                    expected_placement["transmissionBytes"]
                        .as_u64()
                        .expect("bytes") as usize,
                    "{name}: transmissionBytes"
                );
                assert_eq!(
                    actual.estimated_decoded_bytes,
                    expected_placement["estimatedDecodedBytes"]
                        .as_f64()
                        .expect("decoded"),
                    "{name}: estimatedDecodedBytes"
                );
                assert_eq!(
                    actual.rows,
                    expected_placement["rows"].as_u64().expect("rows") as usize,
                    "{name}: placement rows"
                );
                assert_eq!(
                    actual.sequence,
                    expected_placement["sequence"].as_str().expect("sequence"),
                    "{name}: sequence"
                );
            }

            for crop in case["crops"].as_array().into_iter().flatten() {
                let hidden = crop["hidden"].as_i64().expect("hidden") as isize;
                let visible = crop["visible"].as_i64().expect("visible") as isize;
                let expected = crop["expected"].as_str().expect("expected");
                assert_eq!(
                    crop_kitty_image_line(line, hidden, visible),
                    expected,
                    "{name}: crop({hidden},{visible})"
                );
            }
        }
    }

    #[test]
    fn unregistered_kitty_lines_match_ts() {
        let fixture = load("placement.json");
        for case in fixture["unregistered"].as_array().expect("array") {
            let line = case["line"].as_str().expect("line");
            assert_eq!(
                get_kitty_image_placement_rows(line),
                case["placementRows"].as_u64().map(|v| v as usize),
                "{}: placementRows",
                case["line"]
            );
            assert_eq!(
                get_kitty_image_placement(line).is_some(),
                !case["placement"].is_null(),
                "{}: placement presence",
                case["line"]
            );
            assert_eq!(
                get_kitty_image_metadata(line).is_some(),
                !case["metadata"].is_null(),
                "{}: metadata presence",
                case["line"]
            );
            assert_eq!(
                crop_kitty_image_line(line, 0, 1),
                case["crop"].as_str().expect("crop"),
                "{}: crop",
                case["line"]
            );
        }
    }

    #[test]
    fn hyperlink_and_is_image_line_match_ts() {
        let fixture = load("misc.json");
        let _guard = EnvGuard::without_env();
        for case in fixture["hyperlink"].as_array().expect("array") {
            assert_eq!(
                hyperlink(
                    case["text"].as_str().expect("text"),
                    case["url"].as_str().expect("url")
                ),
                case["expected"].as_str().expect("expected")
            );
        }
        for case in fixture["is_image_line"].as_array().expect("array") {
            assert_eq!(
                is_image_line(case["line"].as_str().expect("line")),
                case["expected"].as_bool().expect("expected"),
                "line {}",
                case["line"]
            );
        }
    }

    /// `imageFallback` 的黄金用例在 `HOME=/home/tester` 下生成；本机 home 不同，
    /// 这里只对**不依赖 home 前缀**的用例逐字比较，home 相关用例由 `image_fallback` 模块
    /// 用 `home_dir()` 拼期望值。
    #[test]
    fn image_fallback_home_independent_cases_match_ts() {
        let fixture = load("misc.json");
        let _guard = EnvGuard::without_env();
        for case in fixture["fallback"].as_array().expect("array") {
            let file = case["file"].as_str();
            if file.is_some_and(|f| f.contains("/home/tester")) {
                continue;
            }
            set_capabilities(TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: case["hyperlinks"].as_bool().expect("hyperlinks"),
            });
            assert_eq!(
                image_fallback(
                    case["mime"].as_str().expect("mime"),
                    dims(&case["dims"]),
                    file
                ),
                case["expected"].as_str().expect("expected"),
                "case {case}"
            );
        }
        reset_capabilities_cache();
    }

    #[test]
    fn max_kitty_image_rows_is_monotonic() {
        let _guard = EnvGuard::without_env();
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 1,
            columns: 1,
            rows: 7,
            width_px: 1.0,
            height_px: 1.0,
        });
        assert!(get_max_kitty_image_rows() >= 7);
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 2,
            columns: 1,
            rows: 2,
            width_px: 1.0,
            height_px: 1.0,
        });
        assert!(get_max_kitty_image_rows() >= 7);
        reset_capabilities_cache();
        assert!(
            get_max_kitty_image_rows() >= 7,
            "reset must not clear max rows"
        );
    }

    fn from_hex(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("ascii hex");
                u8::from_str_radix(text, 16).expect("hex digits")
            })
            .collect()
    }

    /// 回归（review finding 1）：`Buffer.from(s, "base64")` 是**宽松**解码器。
    ///
    /// 旧的 `base64_decode` 会把输入补齐成 4 的倍数再交给严格引擎，尾部低位非零的
    /// 2/3 字符尾组（例如 `"QUJ"`、被截断的 PNG 头）会让整个 `decode` 返回空 Vec。
    /// 这里对 493 条 Node 26.12.1 实测样本逐字节比较（含 URL-safe、空白、非法字符、
    /// `=` 截断、非 ASCII 与全部单字符尾组）。
    #[test]
    fn base64_lenient_decode_matches_node() {
        use kiana_tui::terminal_image::base64_decode;
        let fixture = load("base64.json");
        let cases = fixture.as_array().expect("array");
        assert!(cases.len() >= 400);
        for case in cases {
            let input = case["input"].as_str().expect("input");
            let expected = from_hex(case["hex"].as_str().expect("hex"));
            let actual = base64_decode(input);
            assert_eq!(actual, expected, "input {input:?}");
        }
    }

    /// 回归（review finding 4）：Windows `pathToFileURL` 的 UNC 分支必须做 WHATWG 点段规范化，
    /// 前向斜杠的 UNC 输入（`//server/share/x`）也必须识别为 UNC。
    ///
    /// 800 条期望值全部来自 Node 26.12.1 的 `pathToFileURL(p, { windows: true })`
    /// （生成时把 `path.sep` 固定为 `\`，与真 Windows 一致）；Node 会抛错的输入不在样本里。
    #[test]
    fn path_to_file_url_windows_matches_node_golden() {
        use kiana_tui::terminal_image::path_to_file_url_windows;
        let fixture = load("windows_urls.json");
        let cases = fixture.as_array().expect("array");
        assert!(cases.len() >= 500);
        for case in cases {
            let path = case["path"].as_str().expect("path");
            let expected = case["expected"].as_str().expect("expected");
            assert_eq!(path_to_file_url_windows(path), expected, "path {path:?}");
        }
    }

    /// 回归（review finding 2/7）：Kitty 分块必须按 4096 个 UTF-16 码元切片，且逐字节与期望 fixture 一致。
    ///
    /// 覆盖 4095/4096/4097/8192/12288 的块边界、参数只出现在首块、以及**切点落在代理对中间**
    /// 的情形（TS 产出两个孤立代理；按 UTF-8 写出时各是 `EF BF BD`，即 Rust 的 U+FFFD）。
    #[test]
    fn encode_kitty_chunk_boundaries_match_ts_bytes() {
        use kiana_tui::terminal_image::KittyEncodeOptions;
        let fixture = load("kitty_chunks.json");
        let cases = fixture.as_array().expect("array");
        assert!(cases.len() >= 8);
        for case in cases {
            let data = case["data"].as_str().expect("data");
            let options = &case["options"];
            let expected = from_hex(case["expectedHex"].as_str().expect("expectedHex"));
            let opts = KittyEncodeOptions {
                columns: options["columns"].as_f64(),
                rows: options["rows"].as_f64(),
                image_id: options["imageId"].as_f64(),
                move_cursor: options["moveCursor"].as_bool(),
            };
            let actual = encode_kitty(data, opts);
            assert_eq!(
                actual.as_bytes(),
                expected.as_slice(),
                "kitty chunk case len {} options {options}",
                data.chars().count()
            );
        }
    }
}

// ===========================================================================
// 模块面覆盖（无 TS 对应；保证公开 API 至少被一个测试触达）
// ===========================================================================

mod module_surface {
    use super::*;
    use kiana_tui::terminal_colors::TerminalColorMode;
    use kiana_tui::terminal_image::{
        allocate_image_id, base64_decode, get_terminal_color_mode, get_terminal_color_mode_default,
        path_to_file_url, probe_tmux_hyperlinks,
    };

    #[test]
    fn base64_decode_matches_node() {
        // 每行都是 `Buffer.from(input, "base64").toString("hex")` 在 Node 26.12.1 上的实测值。
        let cases = [
            ("AAAA", "000000"),
            ("AQ==", "01"),
            ("AA==", "00"),
            ("AAA=", "0000"),
            ("A", ""),
            ("AA", "00"),
            ("AAA", "0000"),
            ("AAAAA", "000000"),
            ("AA!A", "0000"),
            ("A A A A", "000000"),
            ("==== ", ""),
            ("AA==AA", "00"),
            ("AAA=AAA", "0000"),
            ("aGVsbG8=", "68656c6c6f"),
            ("aGVsbG8", "68656c6c6f"),
            ("aGVsbG8===", "68656c6c6f"),
            ("!!!!", ""),
            ("", ""),
            ("AA-A", "000f80"),
            ("AA_A", "000fc0"),
            ("AA.AA", "000000"),
            // 非 ASCII 按 UTF-16 code unit 的低 8 位参与判定（V8 的 `charCodeAt(i)` 语义）。
            ("AAéA", "0000"),
            ("AA😀A", "00"),
            ("éAA", "00"),
            ("AAŁA", "000000"),
            ("AAīA", "000f80"),
            ("AAĽA", "00"),
        ];
        for (input, expected_hex) in cases {
            let actual: String = base64_decode(input)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(actual, expected_hex, "input {input:?}");
        }
    }

    #[test]
    fn path_to_file_url_matches_node() {
        // `pathToFileURL(p).href` on Node 26.12.1（POSIX）。
        let cases = [
            ("/home/user/README.md", "file:///home/user/README.md"),
            ("/tmp/a b.png", "file:///tmp/a%20b.png"),
            ("/tmp/a#b.png", "file:///tmp/a%23b.png"),
            ("/tmp/a?b.png", "file:///tmp/a%3Fb.png"),
            ("/tmp/100%.png", "file:///tmp/100%25.png"),
            (
                "/tmp/héllo 世界.png",
                "file:///tmp/h%C3%A9llo%20%E4%B8%96%E7%95%8C.png",
            ),
            ("/tmp/a[b].png", "file:///tmp/a%5Bb%5D.png"),
            ("/tmp/a'b.png", "file:///tmp/a'b.png"),
            ("/tmp/a\"b.png", "file:///tmp/a%22b.png"),
            ("/tmp/a<b>.png", "file:///tmp/a%3Cb%3E.png"),
            ("/tmp/a{b}.png", "file:///tmp/a%7Bb%7D.png"),
            ("/tmp/a`b.png", "file:///tmp/a%60b.png"),
            ("/tmp/a|b.png", "file:///tmp/a%7Cb.png"),
            ("/tmp/a^b.png", "file:///tmp/a%5Eb.png"),
            ("/tmp/a~b.png", "file:///tmp/a%7Eb.png"),
            ("/tmp/a+b.png", "file:///tmp/a+b.png"),
            ("/tmp/a&b.png", "file:///tmp/a&b.png"),
            ("/tmp/a=b.png", "file:///tmp/a=b.png"),
            ("/tmp/a;b.png", "file:///tmp/a;b.png"),
            ("/tmp/a,b.png", "file:///tmp/a,b.png"),
            ("/tmp/a@b.png", "file:///tmp/a@b.png"),
            ("/tmp/a:b.png", "file:///tmp/a:b.png"),
            ("/tmp/a$b.png", "file:///tmp/a$b.png"),
            ("/tmp/a!b.png", "file:///tmp/a!b.png"),
            ("/tmp/a*b.png", "file:///tmp/a*b.png"),
            ("/tmp/a(b).png", "file:///tmp/a(b).png"),
            ("/tmp/back\\slash.png", "file:///tmp/back%5Cslash.png"),
            ("/tmp/ümlaut.png", "file:///tmp/%C3%BCmlaut.png"),
            ("/tmp/😀.png", "file:///tmp/%F0%9F%98%80.png"),
            // `path.resolve` 规范化
            ("/tmp//a.png", "file:///tmp/a.png"),
            ("/tmp/a/../b.png", "file:///tmp/b.png"),
            ("/tmp/a/./b.png", "file:///tmp/a/b.png"),
            ("/tmp/a/", "file:///tmp/a/"),
            ("/", "file:///"),
            ("//tmp/a.png", "file:///tmp/a.png"),
            ("/tmp/../", "file:///"),
            ("/tmp/a/..", "file:///tmp"),
            ("/tmp/a/../", "file:///tmp/"),
            ("/tmp/a/.", "file:///tmp/a"),
            ("/tmp/a/./", "file:///tmp/a/"),
            ("/tmp//", "file:///tmp/"),
            ("/tmp/a//b//", "file:///tmp/a/b/"),
            ("////", "file:///"),
            ("/..", "file:///"),
            ("/tmp/a\u{0}b.png", "file:///tmp/a%00b.png"),
            ("/tmp/a%20b.png", "file:///tmp/a%2520b.png"),
            ("/tmp/a\nb.png", "file:///tmp/a%0Ab.png"),
        ];
        for (input, expected) in cases {
            assert_eq!(path_to_file_url(input), expected, "path {input:?}");
        }
    }

    /// 尺寸头解析与 iTerm2 编码的边界值，全部为 TS 26.12.1 的实测输出。
    #[test]
    fn dimension_header_and_iterm2_edge_cases_match_ts() {
        use base64::Engine as _;
        let b64 = |bytes: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(bytes);
        let zeros = |len: usize| b64(vec![0u8; len]);
        let be16 = |n: u16| vec![(n >> 8) as u8, n as u8];

        // 长度门槛：PNG < 24、GIF < 10、WEBP < 30，且 0 长度的签名检查不 panic。
        for len in [0usize, 1, 2, 9, 10, 23, 24, 25, 29, 30] {
            let data = zeros(len);
            assert_eq!(get_png_dimensions(&data), None, "png len {len}");
            assert_eq!(get_gif_dimensions(&data), None, "gif len {len}");
            assert_eq!(get_webp_dimensions(&data), None, "webp len {len}");
        }

        // PNG：签名齐但只有 24 字节 → 读 16/20 处的零。
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend([0, 0, 0, 13]);
        png.extend([0u8; 8]);
        png.extend([0, 0, 0x02, 0x80, 0, 0, 0x01, 0xe0]);
        assert_eq!(
            get_png_dimensions(&b64(png)),
            Some(ImageDimensions {
                width_px: 0.0,
                height_px: 640.0
            })
        );

        // JPEG：`while (offset < len - 9)` 意味着尾部 9 字节内的 marker 永不解析。
        let mut sof = vec![0xff, 0xd8];
        sof.extend([0xff, 0xc0]);
        sof.extend(be16(17));
        sof.push(8);
        sof.extend(be16(480));
        sof.extend(be16(640));
        sof.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        assert_eq!(
            get_jpeg_dimensions(&b64(sof)),
            Some(ImageDimensions {
                width_px: 640.0,
                height_px: 480.0
            })
        );
        for extra in 0..=7usize {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0];
            bytes.extend(vec![0u8; extra]);
            assert_eq!(get_jpeg_dimensions(&b64(bytes)), None, "jpeg tail {extra}");
        }
        for extra in 8..=10usize {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0];
            bytes.extend(vec![0u8; extra]);
            assert_eq!(
                get_jpeg_dimensions(&b64(bytes)),
                Some(ImageDimensions {
                    width_px: 0.0,
                    height_px: 0.0
                }),
                "jpeg tail {extra}"
            );
        }
        // 只有 SOF0/1/2 被识别。
        for marker in [0xc0u8, 0xc1, 0xc2, 0xc3] {
            let mut bytes = vec![0xff, 0xd8, 0xff, marker];
            bytes.extend(be16(17));
            bytes.push(8);
            bytes.extend(be16(100));
            bytes.extend(be16(200));
            bytes.extend([1, 1, 0x11, 0]);
            bytes.extend(vec![0u8; 20]);
            let expected = if marker == 0xc3 {
                None
            } else {
                Some(ImageDimensions {
                    width_px: 200.0,
                    height_px: 100.0,
                })
            };
            assert_eq!(
                get_jpeg_dimensions(&b64(bytes)),
                expected,
                "marker {marker:#x}"
            );
        }
        // 段长 < 2 → null。
        for length in [0u16, 1] {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
            bytes.extend(be16(length));
            bytes.extend(vec![0u8; 20]);
            assert_eq!(get_jpeg_dimensions(&b64(bytes)), None, "len {length}");
        }

        // GIF：大小端与签名。
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x40, 0x01, 0xf0, 0x00]);
        assert_eq!(
            get_gif_dimensions(&b64(gif.clone())),
            Some(ImageDimensions {
                width_px: 320.0,
                height_px: 240.0
            })
        );
        gif[4] = b'7'; // GIF89a → GIF87a
        assert_eq!(
            get_gif_dimensions(&b64(gif)),
            Some(ImageDimensions {
                width_px: 320.0,
                height_px: 240.0
            })
        );
        let mut bad_sig = b"GIF88a".to_vec();
        bad_sig.extend([0x40, 0x01, 0xf0, 0x00]);
        assert_eq!(get_gif_dimensions(&b64(bad_sig)), None);

        // WEBP：三个分支。
        let riff = |chunk: &[u8], body: Vec<u8>| {
            let mut bytes = b"RIFF".to_vec();
            bytes.extend([0, 0, 0, 0]);
            bytes.extend(b"WEBP");
            bytes.extend(chunk);
            bytes.extend(body);
            b64(bytes)
        };
        let mut vp8 = vec![0u8; 10];
        vp8.extend([0x40, 0x01, 0x20, 0x03]);
        vp8.extend([0u8; 4]);
        assert_eq!(
            get_webp_dimensions(&riff(b"VP8 ", vp8)),
            Some(ImageDimensions {
                width_px: 320.0,
                height_px: 800.0
            })
        );
        let mut vp8x = vec![0u8; 8];
        vp8x.extend([0x3f, 0x02, 0x00, 0x1f, 0x01, 0x00, 0, 0]);
        assert_eq!(
            get_webp_dimensions(&riff(b"VP8X", vp8x)),
            Some(ImageDimensions {
                width_px: 576.0,
                height_px: 288.0
            })
        );
        let mut vp8l = vec![0u8; 5];
        vp8l.extend([0xff, 0xff, 0xff, 0xff]);
        vp8l.extend([0u8; 10]);
        assert_eq!(
            get_webp_dimensions(&riff(b"VP8L", vp8l)),
            Some(ImageDimensions {
                width_px: 16384.0,
                height_px: 16384.0
            })
        );
        let mut bad_riff = b"RIFX".to_vec();
        bad_riff.extend([0, 0, 0, 0]);
        bad_riff.extend(b"WEBP");
        bad_riff.extend(b"VP8 ");
        bad_riff.extend(vec![0u8; 10]);
        bad_riff.extend([0x40, 0x01, 0x20, 0x03]);
        assert_eq!(get_webp_dimensions(&b64(bad_riff)), None);

        // 未知 mime → null。
        assert_eq!(get_image_dimensions(&zeros(30), "image/avif"), None);

        // iTerm2：name / 空 name / 数字 0 / 字符串尺寸 / 非 ASCII 负载的 size。
        assert_eq!(
            encode_iterm2(
                "AAAA",
                kiana_tui::terminal_image::Iterm2EncodeOptions {
                    name: Some("héllo".into()),
                    ..Default::default()
                }
            ),
            "\x1b]1337;File=inline=1;size=3;name=aMOpbGxv:AAAA\x07"
        );
        assert_eq!(
            encode_iterm2(
                "AAAA",
                kiana_tui::terminal_image::Iterm2EncodeOptions {
                    name: Some(String::new()),
                    ..Default::default()
                }
            ),
            "\x1b]1337;File=inline=1;size=3:AAAA\x07"
        );
        assert_eq!(
            encode_iterm2(
                "AAAA",
                kiana_tui::terminal_image::Iterm2EncodeOptions {
                    width: Some(0.0.into()),
                    height: Some(0.0.into()),
                    preserve_aspect_ratio: Some(true),
                    inline: Some(true),
                    name: None,
                }
            ),
            "\x1b]1337;File=inline=1;size=3;width=0;height=0:AAAA\x07"
        );
        assert_eq!(
            encode_iterm2(
                "AAAA",
                kiana_tui::terminal_image::Iterm2EncodeOptions {
                    width: Some("50%".into()),
                    height: Some("auto".into()),
                    ..Default::default()
                }
            ),
            "\x1b]1337;File=inline=1;size=3;width=50%;height=auto:AAAA\x07"
        );
        assert_eq!(
            encode_iterm2("AAéA", Default::default()),
            "\x1b]1337;File=inline=1;size=3:AAéA\x07"
        );
    }

    /// `pathToFileURL(p)` 的 Windows 分支逐条对拍 Node 26。
    ///
    /// 期望值来自 `pathToFileURL(p, { windows: true })` 在 **Linux 宿主**上的实测；唯一的宿主差异是
    /// Node 的尾斜杠补全用 `path.sep` 判定（Linux 上是 `/`，真 Windows 上是 `\`），
    /// 因此 `C:\`、`C:/`、`C:\a\..\` 这类「解析结果以 `\` 结尾」的用例按**真 Windows**
    /// 语义取 `file:///C:/`（源码注释里记了这条），其余用例两边一致。
    #[test]
    fn path_to_file_url_windows_matches_node() {
        use kiana_tui::terminal_image::path_to_file_url_windows;
        let cases = [
            ("C:\\Users\\me\\shot.png", "file:///C:/Users/me/shot.png"),
            ("C:/Users/me/shot.png", "file:///C:/Users/me/shot.png"),
            ("C:\\a b\\c#d.png", "file:///C:/a%20b/c%23d.png"),
            ("C:\\a/../b.png", "file:///C:/b.png"),
            // 真 Windows 语义（Linux 宿主上 Node 会因 `path.sep === "/"` 多补一个斜杠）。
            ("C:\\", "file:///C:/"),
            ("C:/", "file:///C:/"),
            ("D:\\", "file:///D:/"),
            ("C:\\a\\..", "file:///C:/"),
            ("C:\\a\\..\\", "file:///C:/"),
            ("C:\\a\\", "file:///C:/a/"),
            ("C:\\a", "file:///C:/a"),
            ("C:\\A%B", "file:///C:/A%25B"),
            ("C:\\\u{e9}.png", "file:///C:/%C3%A9.png"),
            ("C:\\a\tb", "file:///C:/a%09b"),
            ("\\\\server\\share\\f.png", "file://server/share/f.png"),
            (
                "\\\\Server\\Share\\File.PNG",
                "file://server/Share/File.PNG",
            ),
            (
                "\\\\server\\Share Dir\\a b.png",
                "file://server/Share%20Dir/a%20b.png",
            ),
            ("\\\\server\\share\\", "file://server/share/"),
            ("\\\\server\\share", "file://server/share"),
            ("\\\\?\\UNC\\srv\\share\\x.png", "file://srv/share/x.png"),
            ("\\\\?\\UNC\\SRV\\S\\x.png", "file://srv/S/x.png"),
            ("\\\\?\\UNC\\srv\\share", "file://srv/share"),
            ("\\\\?\\C:\\x", "file:///C:/x"),
            ("\\\\?\\C:\\a b\\x.png", "file:///C:/a%20b/x.png"),
            ("\\\\?\\D:\\x y\\z.png", "file:///D:/x%20y/z.png"),
            ("\\\\?\\C:\\a\\", "file:///C:/a/"),
            ("\\\\.\\C:\\x.png", "file://./C:/x.png"),
            ("/x/y.png", "file:///x/y.png"),
            ("\\x", "file:///x"),
        ];
        for (input, expected) in cases {
            assert_eq!(path_to_file_url_windows(input), expected, "path {input:?}");
        }
    }

    #[test]
    fn allocate_image_id_stays_in_js_range() {
        for _ in 0..1000 {
            let id = allocate_image_id();
            assert!((1..=0xffff_fffe).contains(&id), "id out of range: {id}");
        }
    }

    #[test]
    fn terminal_color_mode_follows_true_color() {
        assert_eq!(
            get_terminal_color_mode(TerminalCapabilities {
                images: None,
                true_color: true,
                hyperlinks: false
            }),
            TerminalColorMode::Truecolor
        );
        assert_eq!(
            get_terminal_color_mode(TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: true
            }),
            TerminalColorMode::Palette256
        );
    }

    /// TS `getTerminalColorMode()` 的默认参数 `capabilities = getCapabilities()`：
    /// 生产调用方（theme.ts 的 `createTheme`/`createSystemTheme`）用的零参数形态。
    #[test]
    fn terminal_color_mode_default_derives_from_cached_capabilities() {
        {
            let _guard = EnvGuard::new(&[("KIANA_TRUE_COLOR", Some("1"))]);
            reset_capabilities_cache();
            assert_eq!(
                get_terminal_color_mode_default(),
                TerminalColorMode::Truecolor
            );
        }
        {
            let _guard = EnvGuard::new(&[("KIANA_TRUE_COLOR", Some("0"))]);
            reset_capabilities_cache();
            assert_eq!(
                get_terminal_color_mode_default(),
                TerminalColorMode::Palette256
            );
        }
        // `setCapabilities(caps)` 写入的缓存同样被零参数形态读取。
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        });
        assert_eq!(
            get_terminal_color_mode_default(),
            TerminalColorMode::Truecolor
        );
        reset_capabilities_cache();
    }

    /// TS 默认参数 `cellDimensions = {widthPx:9,heightPx:18}`、`optimizeAspectRatio = false`。
    ///
    /// 期望值来自 `calculateImageCellSize(dims, w, h)` / `calculateImageRows(dims, w)` 在
    /// Node 26.12.1 上的实测（省略参数即默认）。
    #[test]
    fn image_sizing_default_argument_forms_match_ts() {
        // (widthPx, heightPx, maxW, maxH, columns, rows)
        let cells = [
            (100.0, 50.0, 80.0, None, 80usize, 20usize),
            (100.0, 50.0, 80.0, Some(10.0), 40, 10),
            (1280.0, 720.0, 40.0, None, 40, 12),
            (1280.0, 720.0, 40.0, Some(12.0), 40, 12),
            (1.0, 1.0, 1.0, None, 1, 1),
            (300.0, 500.0, 33.0, Some(7.0), 9, 7),
        ];
        for (width_px, height_px, max_w, max_h, columns, rows) in cells {
            let dimensions = ImageDimensions {
                width_px,
                height_px,
            };
            let size = calculate_image_cell_size_default(dimensions, max_w, max_h);
            assert_eq!(
                (size.columns, size.rows),
                (columns, rows),
                "dims {width_px}x{height_px}, maxW {max_w}, maxH {max_h:?}"
            );
            // 显式传入 TS 默认值必须得到同一结果（默认是静态 9x18，而不是 getCellDimensions()）。
            assert_eq!(
                size,
                calculate_image_cell_size(
                    dimensions,
                    max_w,
                    max_h,
                    CellDimensions::default(),
                    false
                )
            );
            let rows_default = calculate_image_rows_default(dimensions, max_w);
            assert_eq!(
                rows_default,
                calculate_image_rows(dimensions, max_w, CellDimensions::default())
            );
            // TS：`calculateImageRows(dims, w)` ≡ `calculateImageCellSize(dims, w).rows`。
            assert_eq!(
                rows_default,
                calculate_image_cell_size_default(dimensions, max_w, None).rows
            );
        }

        // 默认单元格尺寸是静态 9x18，**不**读取 getCellDimensions()（TS 默认参数就是字面量）。
        // 若误用 getCellDimensions()，10x10 的实测值会让 100x50 的 rows 变成 40。
        set_cell_dimensions(CellDimensions {
            width_px: 10.0,
            height_px: 10.0,
        });
        let dimensions = ImageDimensions {
            width_px: 100.0,
            height_px: 50.0,
        };
        let size = calculate_image_cell_size_default(dimensions, 80.0, None);
        assert_eq!((size.columns, size.rows), (80, 20));
        assert_eq!(calculate_image_rows_default(dimensions, 80.0), 20);
        set_cell_dimensions(CellDimensions::default());
    }

    /// Node 的 `pathToFileURL` 对格式错误的绝对 UNC 路径抛 `ERR_INVALID_ARG_VALUE` /
    /// `ERR_INVALID_URL`；Rust 的 [`image_fallback`] 冻结签名没有错误通道，这些输入退化为
    /// 可读文本（见 `terminal_image.rs` 模块文档「已知偏差」）。本测试固定该退化输出。
    #[test]
    fn malformed_windows_unc_paths_degrade_to_readable_fallback() {
        use kiana_tui::terminal_image::path_to_file_url_windows;
        // 左列都是 path.win32.isAbsolute 为真、但 Node 会抛错的输入。
        let cases = [
            ("\\\\srv", "file:///srv"),
            ("\\\\?\\UNC\\server", "file:///%3F/UNC/server"),
            ("\\\\\\\\share\\x", "file:///share/x"),
            ("\\\\a b\\share\\x", "file:///a%20b/share/x"),
            ("\\\\s rv\\share\\x", "file:///s%20rv/share/x"),
            ("\\\\se%zzrver\\share\\x", "file:///se%25zzrver/share/x"),
        ];
        for (input, expected) in cases {
            assert_eq!(path_to_file_url_windows(input), expected, "path {input:?}");
        }
    }

    #[test]
    fn image_protocol_literals() {
        assert_eq!(ImageProtocol::Kitty.as_str(), "kitty");
        assert_eq!(ImageProtocol::Iterm2.as_str(), "iterm2");
    }

    #[test]
    fn cell_dimensions_default_to_9x18() {
        let _guard = EnvGuard::without_env();
        assert_eq!(get_cell_dimensions(), CellDimensions::default());
        assert_eq!(CellDimensions::default().width_px, 9.0);
        assert_eq!(CellDimensions::default().height_px, 18.0);
        set_cell_dimensions(CellDimensions {
            width_px: 12.0,
            height_px: 24.0,
        });
        assert_eq!(
            get_cell_dimensions(),
            CellDimensions {
                width_px: 12.0,
                height_px: 24.0
            }
        );
    }

    /// 没有 tmux（或 tmux 不在 PATH）时必须安全回退 `false`，绝不 panic。
    #[test]
    fn tmux_probe_never_panics() {
        let _guard = EnvGuard::without_env();
        let _ = probe_tmux_hyperlinks();
    }

    /// 回归（review finding 3）：`execSync(..., { timeout: 250 })` 到点必须立即返回。
    ///
    /// 桩脚本先起一个继承 stdout 的后台孙进程再自己 `sleep 5`；旧实现超时后仍 `join()` 读线程，
    /// 于是被孙进程握着的管道拖到 5 s（实测 30 s 的场景同理）。Node 在 250 ms 处 kill 子进程并抛
    /// ETIMEDOUT，这里要求 `probe_tmux_hyperlinks()` 在 250 ms 量级返回 `false`。
    #[cfg(unix)]
    #[test]
    fn tmux_probe_honours_the_deadline_with_a_lingering_grandchild() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let _guard = EnvGuard::without_env();
        let dir = std::env::temp_dir().join(format!(
            "kiana-tui-tmux-probe-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create stub dir");
        let script = dir.join("tmux");
        std::fs::write(&script, "#!/bin/sh\n(sleep 5) &\nsleep 5\n").expect("write tmux stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

        let saved_path = std::env::var_os("PATH");
        let mut path = dir.clone().into_os_string();
        if let Some(saved) = &saved_path {
            path.push(":");
            path.push(saved);
        }
        // Safety: 本测试持有进程级 env_lock（[`EnvGuard`]），不会与其它环境用例并发。
        unsafe { std::env::set_var("PATH", &path) };
        let started = std::time::Instant::now();
        let result = probe_tmux_hyperlinks();
        let elapsed = started.elapsed();
        // Safety: 同上，仍持有 env_lock。
        unsafe {
            match &saved_path {
                Some(saved) => std::env::set_var("PATH", saved),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!result);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "probe took {elapsed:?}; it must give up at the 250 ms deadline, not wait for the pipe"
        );
    }

    /// 回归（review finding 2）：Kitty 分块循环必须是线性的。
    ///
    /// 旧实现每块都从串首重扫 UTF-16 下标（O(n²)）：debug 下 1 MB 约 4 s，4 MB 会到分钟级。
    /// 这里用 4 MiB + 10 s 的宽松上限：现行实现约 0.1 s，而平方级实现必然超时。同时校验
    /// 各块负载拼回去与原串逐字节相同、块数等于 `ceil(len / 4096)`。
    #[test]
    fn encode_kitty_large_payload_is_linear_and_byte_identical() {
        use kiana_tui::terminal_image::KittyEncodeOptions;

        let data = "A".repeat(4 * 1024 * 1024);
        let started = std::time::Instant::now();
        let encoded = encode_kitty(&data, KittyEncodeOptions::default());
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "encoding 4 MiB took {elapsed:?}; the chunk loop is quadratic again"
        );
        let mut payload = String::with_capacity(data.len());
        let mut chunks = 0usize;
        for part in encoded.split("\x1b\\") {
            if part.is_empty() {
                continue;
            }
            chunks += 1;
            let body = part.strip_prefix("\x1b_G").expect("kitty prefix");
            let (_, chunk) = body.split_once(';').expect("header/body separator");
            payload.push_str(chunk);
        }
        assert_eq!(chunks, data.len().div_ceil(4096));
        assert_eq!(payload, data);
    }

    /// 回归（review finding 6）：`r=` 按 `Number.parseInt(value, 10)`（JS 双精度）解析。
    ///
    /// 超过 2^53 的值按双精度舍入；超过 u64::MAX 的值仍使用显式 `r`（饱和到 `usize::MAX`），
    /// 而不是解析失败后错误地回退到注册表里的 `rows`。
    #[test]
    fn kitty_placement_rows_use_js_parse_int_semantics() {
        use kiana_tui::terminal_image::get_kitty_image_placement_rows as rows;

        assert_eq!(
            rows("\x1b_Gi=5,r=9007199254740993;AA\x1b\\"),
            Some(9_007_199_254_740_992)
        );
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 987_654_321,
            columns: 1,
            rows: 4,
            width_px: 1.0,
            height_px: 1.0,
        });
        assert_eq!(
            rows("\x1b_Gi=987654321,r=18446744073709551616;AA\x1b\\"),
            Some(usize::MAX)
        );
        // TS 的这个值是 2^64（18446744073709552000），超出 usize 的表示范围。
        assert_eq!(
            rows("\x1b_Gi=987654321,r=0;AA\x1b\\"),
            Some(4),
            "r=0 在 TS 里是 falsy → 回退到注册表 rows"
        );
    }

    #[test]
    fn set_capability_overrides_is_idempotent() {
        let _guard = EnvGuard::without_env();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        // 与当前 overrides 相同（都是空）→ 不清缓存。
        set_capability_overrides(CapabilityOverrides::default());
        assert_eq!(get_capabilities().images, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn empty_filename_is_treated_as_absent() {
        let _guard = EnvGuard::without_env();
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        assert_eq!(
            image_fallback(
                "image/png",
                Some(ImageDimensions {
                    width_px: 2.0,
                    height_px: 3.0
                }),
                Some("")
            ),
            "[Image: [image/png] 2x3]"
        );
    }

    /// `renderImage` 的默认值：`maxWidthCells` 缺省 80、iTerm2 的 `preserveAspectRatio` 缺省
    /// `true`、Kitty 的 `imageId: 0` 会注册但不会写进序列（JS 真值判定）。
    #[test]
    fn render_image_defaults_match_ts() {
        let _guard = EnvGuard::without_env();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        });
        set_cell_dimensions(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 9000.0,
                height_px: 100.0,
            },
            ImageRenderOptions::default(),
        )
        .expect("iterm2 render");
        assert_eq!(result.columns, 80);
        assert_eq!(result.rows, 1);
        assert_eq!(
            result.sequence,
            "\x1b]1337;File=inline=1;size=3;width=80;height=auto:AAAA\x07"
        );

        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 90.0,
                height_px: 100.0,
            },
            ImageRenderOptions {
                preserve_aspect_ratio: Some(false),
                ..Default::default()
            },
        )
        .expect("iterm2 render");
        assert_eq!(result.columns, 80);
        assert_eq!(result.rows, 45);
        assert_eq!(
            result.sequence,
            "\x1b]1337;File=inline=1;size=3;width=80;height=auto;preserveAspectRatio=0:AAAA\x07"
        );

        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        let result = render_image(
            "AAAA",
            ImageDimensions {
                width_px: 90.0,
                height_px: 100.0,
            },
            ImageRenderOptions {
                image_id: Some(0),
                ..Default::default()
            },
        )
        .expect("kitty render");
        assert_eq!(result.columns, 80);
        assert_eq!(result.rows, 44);
        assert_eq!(result.image_id, Some(0));
        // `if (options.imageId)` 对 0 为假 → 序列里没有 `i=`，也就取不到元数据。
        assert_eq!(result.sequence, "\x1b_Ga=T,f=100,q=2,c=80,r=44;AAAA\x1b\\");
        assert_eq!(get_kitty_image_metadata(&result.sequence), None);
        reset_capabilities_cache();
    }

    #[test]
    fn render_image_returns_none_without_image_support() {
        let _guard = EnvGuard::without_env();
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        });
        assert_eq!(
            render_image(
                "AAAA",
                ImageDimensions {
                    width_px: 10.0,
                    height_px: 10.0
                },
                ImageRenderOptions::default()
            ),
            None
        );
    }
}
