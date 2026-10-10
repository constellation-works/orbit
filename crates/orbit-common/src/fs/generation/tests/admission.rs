//! Releasing a pin while an updater probes who holds the generation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use fs2::FileExt;

use super::super::admission::{GenerationGuard, PROBE_SETTLE, Participant};
use super::super::identity::{Access, CompatibilityIdentity, LedgerCompatibility};
use super::super::paths::ADMISSION_LOCK;
use super::super::records::open;
use super::super::registry::ParticipantRole;
use super::super::update::GenerationUpdate;

fn identity() -> CompatibilityIdentity {
    let ledger = LedgerCompatibility {
        version: 1,
        writer_floor: 0,
        reader_floor: 0,
    };
    CompatibilityIdentity {
        store_schema: ledger,
        workspace_layout: ledger,
        features: BTreeMap::new(),
        feature_floors: BTreeMap::new(),
    }
}

/// Wait until an updater holds admission exclusively, so it is probing the
/// generation lock.
fn await_updater_admission(root: &Path) {
    let admission = open(root, ADMISSION_LOCK).expect("admission lock");
    let deadline = Instant::now() + Duration::from_secs(60);
    while FileExt::try_lock_shared(&admission.file).is_ok() {
        FileExt::unlock(&admission.file).expect("unlock admission");
        assert!(
            Instant::now() < deadline,
            "the updater never took admission"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Criterion 2, deterministic interleaving [ORB-14869]: a registered clock
/// tick paused after withdrawing its record from the live set, but before
/// releasing the generation lock, is still a registered participant, so an
/// updater waits for it instead of refusing it as an unregistered
/// `executable-generation-v1` holder. A guard that withdrew its record first
/// left exactly this state, and a tick descheduled in it for longer than the
/// updater's settle refused an update in a coverage run.
#[test]
fn an_update_waits_for_a_participant_paused_mid_release() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    let identity = identity();
    let digest = format!("{:064x}", 1);
    let tick = GenerationGuard::join(
        &root,
        &Participant {
            digest: &digest,
            identity: &identity,
            role: ParticipantRole::Clock,
            access: Access::Write,
            handover: None,
        },
        Duration::ZERO,
        || Ok(1),
    )
    .expect("the tick joins");

    let (paused_tx, paused_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let releasing = std::thread::spawn(move || {
        tick.release_pausing(|| {
            paused_tx.send(()).expect("report the pause");
            resume_rx.recv().expect("resume the release");
        });
    });
    paused_rx.recv().expect("the tick pauses mid-release");

    let (outcome_tx, outcome_rx) = mpsc::channel();
    let updater_root = root.clone();
    let updater = std::thread::spawn(move || {
        let outcome = GenerationUpdate::acquire(&updater_root).map(drop);
        outcome_tx.send(outcome).expect("report the update");
    });
    await_updater_admission(&root);
    // The updater refuses an unregistered holder after one settle; give it
    // many while the tick stays paused.
    match outcome_rx.recv_timeout(PROBE_SETTLE * 25) {
        Err(RecvTimeoutError::Timeout) => {}
        Ok(outcome) => panic!(
            "the update did not wait for the releasing tick: {:?}",
            outcome.err()
        ),
        Err(RecvTimeoutError::Disconnected) => panic!("the updater thread panicked"),
    }

    resume_tx.send(()).expect("resume the tick");
    releasing.join().expect("tick thread");
    outcome_rx
        .recv()
        .expect("the updater reports")
        .expect("the update takes the generation once the tick released it");
    updater.join().expect("updater thread");
}
