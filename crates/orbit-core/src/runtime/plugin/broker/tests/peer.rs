//! Peer checks against live processes. The same-namespace and ancestry cases
//! run anywhere; a foreign PID namespace needs Bubblewrap and is covered by
//! the real-sandbox tests in `sandbox.rs`.

use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

use orbit_common::process::ancestry::{ProcessStartKey, process_start_key};

use super::super::peer::{PeerAnchor, authenticate, descends_from, reauthenticate};

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

    fn end(mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
fn the_anchor_process_itself_is_accepted() {
    assert_eq!(descends_from(std::process::id(), own_key()), Ok(()));
}

#[test]
fn a_descendant_of_the_anchor_is_accepted() {
    let child = Sleeper::spawn();

    assert_eq!(descends_from(child.0.id(), own_key()), Ok(()));
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

#[test]
fn a_connected_peer_is_authenticated_and_rechecked_against_its_anchor() {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    let anchor = PeerAnchor::Ancestor(own_key());

    let peer = authenticate(&ours, &anchor).expect("same process is inside its own tree");

    assert_eq!(peer.pid, std::process::id());
    assert!(reauthenticate(&peer, &anchor).is_ok());
    drop(theirs);
}

#[test]
fn a_connected_peer_outside_the_anchor_is_refused_with_its_pid() {
    let (ours, _theirs) = UnixStream::pair().expect("socket pair");
    let sibling = Sleeper::spawn();

    let refusal = authenticate(&ours, &PeerAnchor::Ancestor(sibling.key()))
        .expect_err("the test process is not the sleeper's descendant");

    assert_eq!(refusal.pid, Some(std::process::id()));
}

#[cfg(target_os = "linux")]
mod namespace {
    use super::*;
    use crate::runtime::plugin::broker::peer::NamespaceAnchor;

    #[test]
    fn a_peer_in_the_anchored_namespace_is_accepted() {
        let (ours, _theirs) = UnixStream::pair().expect("socket pair");
        let leader = Sleeper::spawn();
        let anchor = PeerAnchor::Namespace(
            NamespaceAnchor::for_leader(leader.0.id()).expect("anchor this namespace"),
        );

        let peer = authenticate(&ours, &anchor).expect("same PID namespace");

        assert!(reauthenticate(&peer, &anchor).is_ok());
    }

    #[test]
    fn a_namespace_whose_leader_ended_admits_nobody() {
        let (ours, _theirs) = UnixStream::pair().expect("socket pair");
        let leader = Sleeper::spawn();
        let anchor = PeerAnchor::Namespace(
            NamespaceAnchor::for_leader(leader.0.id()).expect("anchor this namespace"),
        );
        let peer = authenticate(&ours, &anchor).expect("admitted while the leader lives");

        leader.end();

        let refusal = reauthenticate(&peer, &anchor).expect_err("leader gone");
        assert!(refusal.reason.contains("ended"), "{}", refusal.reason);
        assert!(authenticate(&ours, &anchor).is_err());
    }
}
