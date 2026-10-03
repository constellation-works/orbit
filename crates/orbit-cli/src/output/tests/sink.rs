//! Sink resolution tests.
//!
//! Every sink here is built with [`OutputSink::resolve`], never
//! [`OutputSink::from_process`]: `make ci` runs without a TTY, so a test that
//! read the ambient environment would assert the piped path while appearing to
//! test the terminal path.

use crate::output::sink::{OutputSink, SinkEnv};

/// A `SinkEnv` with every variable unset.
fn empty_env() -> SinkEnv {
    SinkEnv::default()
}

fn sink(is_tty: bool, env: &SinkEnv, terminal_width: Option<u16>) -> OutputSink {
    OutputSink::resolve(is_tty, env, terminal_width, None, false)
}

#[test]
fn non_tty_sink_has_zero_width_and_no_color_however_loudly_the_env_claims_otherwise() {
    let env = SinkEnv {
        columns: Some("200".to_string()),
        clicolor_force: Some("1".to_string()),
        ..empty_env()
    };

    let sink = sink(false, &env, Some(120));

    assert!(!sink.is_tty());
    assert_eq!(
        sink.width(),
        0,
        "a redirected stream is never width-adapted, even with COLUMNS set"
    );
    assert!(
        !sink.color_allowed(),
        "a redirected stream is never styled, even with CLICOLOR_FORCE set"
    );
}
