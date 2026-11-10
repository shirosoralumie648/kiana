//! CLI 行为（spec §3.1）：`--version`、usage、退出码、多余参数、stdin EOF。

mod common;

use common::{Daemon, TOKEN};
use std::process::ExitStatus;
use std::time::Duration;

fn code(status: ExitStatus) -> i32 {
    status.code().expect("kiana-env exits normally")
}

#[test]
fn prints_the_version_for_its_only_argument() {
    let mut daemon = Daemon::spawn_with_args(&["--version"]);
    assert_eq!(
        daemon.handshake(),
        format!("kiana-env {}\n", env!("KIANA_ENV_VERSION"))
    );
    daemon.close_stdin();
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 0);
}

#[test]
fn prints_the_version_when_more_arguments_follow() {
    // `--version` only counts as args[1]; extra arguments are ignored.
    let mut daemon = Daemon::spawn_with_args(&["--version", "extra"]);
    assert_eq!(
        daemon.handshake(),
        format!("kiana-env {}\n", env!("KIANA_ENV_VERSION"))
    );
    daemon.close_stdin();
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 0);
}

#[test]
fn prints_usage_without_arguments() {
    let mut daemon = Daemon::spawn_with_args(&[]);
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 2);
    assert_eq!(
        daemon.stderr_text(),
        "usage: kiana-env serve --token <hex>\n"
    );
}

#[test]
fn prints_usage_for_an_unknown_command() {
    let mut daemon = Daemon::spawn_with_args(&["foo"]);
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 2);
    assert_eq!(
        daemon.stderr_text(),
        "usage: kiana-env serve --token <hex>\n"
    );
}

#[test]
fn prints_usage_when_the_token_is_missing() {
    let mut daemon = Daemon::spawn_with_args(&["serve"]);
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 2);
    assert_eq!(
        daemon.stderr_text(),
        "usage: kiana-env serve --token <hex>\n"
    );
}

#[test]
fn prints_usage_when_the_token_has_no_value() {
    let mut daemon = Daemon::spawn_with_args(&["serve", "--token"]);
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 2);
    assert_eq!(
        daemon.stderr_text(),
        "usage: kiana-env serve --token <hex>\n"
    );
}

#[test]
fn serves_after_extra_arguments_and_finds_the_token_anywhere() {
    let mut daemon = Daemon::spawn_with_args(&["serve", "extra", "--token", TOKEN, "more"]);
    assert_eq!(daemon.handshake(), format!("KIANA-ENV {TOKEN}\n"));
    daemon.close_stdin();
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 0);
}

#[test]
fn exits_zero_at_the_end_of_stdin() {
    let mut daemon = Daemon::start(&[]);
    assert_eq!(daemon.handshake(), format!("KIANA-ENV {TOKEN}\n"));
    daemon.close_stdin();
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 0);
    assert_eq!(daemon.stderr_text(), "");
}

#[test]
fn treats_a_partial_length_header_as_a_clean_end() {
    // `read_exact` reports UnexpectedEof for 1..3 bytes too, so the daemon stops without an error.
    let mut daemon = Daemon::start(&[]);
    assert_eq!(daemon.handshake(), format!("KIANA-ENV {TOKEN}\n"));
    daemon.send_raw(&[0, 0, 0]);
    daemon.close_stdin();
    assert_eq!(code(daemon.wait(Duration::from_secs(10))), 0);
    assert_eq!(daemon.stderr_text(), "");
}
