//! Which commands a plugin backend may reach Orbit through [ORB-12876].
//!
//! `main` refuses a recognized plugin child every command that does not
//! declare itself a callback entry point, so this table *is* the boundary.
//! `tests/plugin_child_cli_surface.rs` proves the refusal end to end against
//! the real binary; these assertions pin the two entry points that test
//! cannot reach — `orbit mcp serve`, which would block waiting on stdio — and
//! the neighbours they must not be confused with.

use clap::Parser;

use crate::command::Cli;

fn is_callback_entry_point(argv: &[&str]) -> bool {
    Cli::try_parse_from(argv)
        .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"))
        .command
        .operation()
        .plugin_callback_entry_point
}

#[test]
fn the_tool_call_entry_points_are_the_only_ones_a_plugin_backend_may_use() {
    for argv in [
        // The callback itself.
        &["orbit", "tool", "run", "orbit.task.list"][..],
        // A stdio server a backend starts for itself: every `tools/call` it
        // answers lands on the same `permissions.orbit_tools` allowlist.
        &["orbit", "mcp", "serve"][..],
    ] {
        assert!(
            is_callback_entry_point(argv),
            "{argv:?} must stay reachable from a plugin backend"
        );
    }

    for argv in [
        // The two reads that motivated the gate.
        &["orbit", "workspace", "list"][..],
        &["orbit", "run", "show", "jrun-20260101-0000-aa"][..],
        // The rest of `orbit tool` is administration, not a callback.
        &["orbit", "tool", "list"][..],
        &["orbit", "tool", "doctor"][..],
        // The rest of `orbit mcp` rewrites client configs or opens a port.
        &["orbit", "mcp", "listen"][..],
        &["orbit", "mcp", "init"][..],
        // Ordinary governed reads.
        &["orbit", "task", "list"][..],
        &["orbit", "plugin", "list"][..],
        // Replacing the host binary is emphatically not a callback.
        &["orbit", "update"][..],
    ] {
        assert!(
            !is_callback_entry_point(argv),
            "{argv:?} must be refused to a plugin backend"
        );
    }
}
