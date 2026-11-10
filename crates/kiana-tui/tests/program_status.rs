//! `test/program-status.test.ts` 的逐条移植（5 个 it）。
//!
//! 断言原样保留：编码格式、控制字符替换、UTF-8 截断与查询回复识别。

use kiana_tui::program_status::{format_program_status, is_program_status_reply};
use kiana_tui::terminal_colors::{ProgramStatus, ProgramStatusKind, ProgramStatusState};

fn status(state: ProgramStatusState) -> ProgramStatus {
    ProgramStatus {
        state,
        app: None,
        kind: None,
        message: None,
    }
}

fn decode_message(sequence: &str) -> Option<String> {
    use base64::Engine as _;
    let start = sequence.find(":msg=")? + ":msg=".len();
    let rest = &sequence[start..];
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=')))
        .unwrap_or(rest.len());
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&rest.as_bytes()[..end])
        .expect("valid base64");
    Some(String::from_utf8(decoded).expect("valid utf8"))
}

/// `encodes state, app, kind, and a base64 message`
#[test]
fn encodes_state_app_kind_and_a_base64_message() {
    use base64::Engine as _;
    let mut blocked = status(ProgramStatusState::Blocked);
    blocked.app = Some("kiana".into());
    blocked.kind = Some(ProgramStatusKind::Permission);
    blocked.message = Some("Allow bash?".into());
    assert_eq!(
        format_program_status(&blocked),
        format!(
            "\x1b]7501;state=blocked:app=kiana:kind=permission:msg={}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(b"Allow bash?")
        )
    );
    assert_eq!(
        format_program_status(&status(ProgramStatusState::Clear)),
        "\x1b]7501;state=clear\x1b\\"
    );
}

/// `omits kind outside blocked, invalid app names, and empty messages`
#[test]
fn omits_kind_outside_blocked_invalid_app_names_and_empty_messages() {
    let mut working = status(ProgramStatusState::Working);
    working.app = Some("my app".into());
    working.kind = Some(ProgramStatusKind::Auth);
    working.message = Some(" \n ".into());
    assert_eq!(
        format_program_status(&working),
        "\x1b]7501;state=working\x1b\\"
    );

    let mut long_app = status(ProgramStatusState::Idle);
    long_app.app = Some("a".repeat(32));
    assert!(format_program_status(&long_app).contains(&format!(":app={}\x1b", "a".repeat(32))));

    let mut too_long = status(ProgramStatusState::Idle);
    too_long.app = Some("a".repeat(33));
    assert!(!format_program_status(&too_long).contains("app="));
}

/// `replaces control characters, which make terminals discard the report`
#[test]
fn replaces_control_characters_which_make_terminals_discard_the_report() {
    let mut error = status(ProgramStatusState::Error);
    error.message = Some("first\nsecond\x1b[31m\u{9b}third\t".into());
    let sequence = format_program_status(&error);
    assert_eq!(
        decode_message(&sequence).as_deref(),
        Some("first second [31m third")
    );
}

/// `cuts long messages at a UTF-8 boundary within the spec limits`
#[test]
fn cuts_long_messages_at_a_utf8_boundary_within_the_spec_limits() {
    let mut working = status(ProgramStatusState::Working);
    working.app = Some("kiana".into());
    working.message = Some("é".repeat(2000));
    let sequence = format_program_status(&working);
    let message = decode_message(&sequence).expect("message present");
    assert_eq!(message, "é".repeat(1024));
    assert!(message.len() <= 2048);
    assert!(sequence.len() <= 4096);
}

/// `accepts the query echo with either terminator and future pairs`
#[test]
fn accepts_the_query_echo_with_either_terminator_and_future_pairs() {
    assert!(is_program_status_reply("\x1b]7501;?\x1b\\"));
    assert!(is_program_status_reply("\x1b]7501;?\x07"));
    assert!(is_program_status_reply("\x1b]7501;?version=2\x1b\\"));
    assert!(!is_program_status_reply("\x1b]7501;state=idle\x1b\\"));
    assert!(!is_program_status_reply("\x1b]11;rgb:0000/0000/0000\x07"));
}
