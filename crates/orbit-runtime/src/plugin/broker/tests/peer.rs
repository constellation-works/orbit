//! Peer checks against live processes. The same-namespace and ancestry cases
//! run anywhere; a foreign PID namespace needs Bubblewrap and is covered by
//! the real-sandbox tests in `sandbox.rs`.

use std::process::{Child, Command, Stdio};

use orbit_common::process::ancestry::{ProcessStartKey, process_start_key};

use super::super::peer::descends_from;

struct Sleeper(Child);

impl Sleeper {
    fn spawn() -> Self {
        Self(
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .spawn()
                .expect("spawn sleep"),
        )
    }

    fn key(&self) -> ProcessStartKey {
        process_start_key(self.0.id()).expect("sleeper start key")
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn own_key() -> ProcessStartKey {
    process_start_key(std::process::id()).expect("own start key")
}

#[test]
fn a_process_that_does_not_descend_from_the_anchor_is_refused() {
    let sibling = Sleeper::spawn();

    let refusal = descends_from(std::process::id(), sibling.key()).expect_err("not a descendant");

    assert!(refusal.contains("does not descend"), "{refusal}");
}

#[test]
fn an_anchor_whose_pid_was_reused_is_refused() {
    let mut stale = own_key();
    stale.starttime = stale.starttime.saturating_sub(1);

    let refusal = descends_from(std::process::id(), stale).expect_err("recycled anchor pid");

    assert!(refusal.contains("ended"), "{refusal}");
}

#[cfg(target_os = "linux")]
mod namespace {}
