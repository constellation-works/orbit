//! `orbit-common`'s responsibility modules at their public API: filesystem
//! confinement, private files and advisory locks; caller authorization;
//! document migration; log-field redaction; bounded process output; child and
//! operator environments, redaction and release verification; blob and SQLite
//! storage. One binary holds every surface as a module so each does not pay
//! its own link.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

mod fs;
mod governance;
mod migration;
mod observability;
#[cfg(unix)]
mod process;
mod security;
#[cfg(feature = "sqlite")]
mod sqlite;
mod storage;

/// `value` as the body of a JSON string, without its quotes.
fn json_string_body(value: &str) -> String {
    let encoded = serde_json::to_string(value).expect("json string");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("json quotes")
        .to_string()
}

/// `value` as the body of a Rust `Debug` string, without its quotes.
fn debug_string_body(value: &str) -> String {
    let encoded = format!("{value:?}");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("debug quotes")
        .to_string()
}
