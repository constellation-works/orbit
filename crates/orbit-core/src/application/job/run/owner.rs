//! Owner process signalling, identity classification, and liveness probes.
//!
//! All Unix-specific; non-Unix shims return neutral outcomes without signalling.

use orbit_common::OrbitError;
/// Re-exported so the historical `owner::process_is_alive` path keeps working
/// for this module's callers and sibling tests; the probe itself now lives in
/// `orbit-common` so other run surfaces can share it [ORB-10496].
#[cfg(unix)]
pub(super) use orbit_common::process::identity::process_is_alive;
#[cfg(unix)]
use orbit_common::process::identity::{
    KernelLiveness, PidNamespaceScope, ProbeOutcome, is_stable_token, legacy_lstart_matches,
    pid_namespace_scope, probe_process_group_liveness, probe_process_start_identity,
    stable_tokens_match,
};
use orbit_types::workflow::{JobRun, JobRunState};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
pub(super) const RUN_OWNER_TERMINATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
pub(super) const RUN_OWNER_TERMINATION_POLL: Duration = Duration::from_millis(50);

/// Attempts to signal (TERM then KILL) the recorded owner process / process group
/// for a running job, returning a short outcome token for telemetry.
#[cfg(unix)]
pub(super) fn signal_run_owner_process(run: &JobRun) -> Result<String, OrbitError> {
    let Some(pid) = run.pid else {
        return Ok("no_pid".to_string());
    };
    if pid == std::process::id() {
        return Ok("self_not_signalled".to_string());
    }
    // [ORB-10594] Ahead of the liveness check: from another PID namespace the
    // owner is invisible, and reporting that as `already_exited` would claim a
    // cancellation that never reached the process.
    if pid_namespace_scope(run.pid_start_time.as_deref()) == PidNamespaceScope::Foreign {
        return Ok("foreign_pid_namespace".to_string());
    }
    if !process_is_alive(pid) {
        return Ok("already_exited".to_string());
    }
    // Capture the kernel-resolution start key before the initial token probe.
    // Persisted lstart tokens have only second resolution; a same-second reuse
    // between verification and binding must not silently adopt a replacement.
    let start_key = orbit_common::process::ancestry::process_start_key(pid);
    signal_run_owner_with_binding(run, classify_run_owner(run), |pid, token| {
        BoundTarget::bind(pid, token, start_key)
    })
}

// The binding seam forces exit/reuse after the initial identity check without
// consuming real PIDs or depending on scheduler timing.
#[cfg(unix)]
pub(super) fn signal_run_owner_with_binding<T: TeardownTarget>(
    run: &JobRun,
    identity: OwnerIdentity,
    bind: impl FnOnce(u32, Option<&str>) -> Result<Option<T>, OrbitError>,
) -> Result<String, OrbitError> {
    if identity != OwnerIdentity::Verified {
        return Ok("owner_identity_mismatch".into());
    }
    let Some(pid) = run.pid else {
        return Ok("no_pid".into());
    };
    let Some(target) = bind(pid, run.pid_start_time.as_deref())? else {
        return Ok("already_exited".into());
    };
    let grouped = target.pgid().is_some();
    let killed = terminate_target(&target, pid)?;
    Ok(match (grouped, killed) {
        (true, false) => "terminated_process_group",
        (true, true) => "killed_process_group",
        (false, false) => "terminated_owner",
        (false, true) => "killed_owner",
    }
    .into())
}

#[cfg(not(unix))]
pub(super) fn signal_run_owner_process(_run: &JobRun) -> Result<String, OrbitError> {
    Ok("unsupported_platform".to_string())
}

/// Why a running run's worker cannot be stopped and seen gone from here, or
/// `None` when it can — or is already gone. Asked before anything is decided
/// on the strength of the stop, such as releasing the claim the run works
/// for.
#[cfg(unix)]
pub(super) fn run_owner_unstoppable_reason(run: &JobRun) -> Option<&'static str> {
    let pid = run.pid?;
    if pid == std::process::id() {
        return Some("its worker is this process");
    }
    match classify_run_owner(run) {
        OwnerIdentity::Missing | OwnerIdentity::Mismatch => None,
        OwnerIdentity::Verified => match owner_process_group_id(pid) {
            Some(pgid) if pgid as u32 == pid && pgid == unsafe { libc::getpgrp() } => {
                Some("its worker shares this process's group")
            }
            _ => None,
        },
        OwnerIdentity::LegacyLiveUnverified | OwnerIdentity::ProbeUnavailable => {
            Some("its worker's process identity cannot be verified")
        }
        OwnerIdentity::ForeignPidNamespace => Some("its worker runs in another PID namespace"),
    }
}

#[cfg(not(unix))]
pub(super) fn run_owner_unstoppable_reason(run: &JobRun) -> Option<&'static str> {
    run.pid.map(|_| "this platform cannot stop a run's worker")
}

/// [`signal_run_owner_process`], failing unless the worker is seen gone
/// afterwards: an outcome that signalled nothing it could verify (another
/// PID namespace, an unverifiable identity, this process) is an error, so
/// the cancellation stops before the run is finalized.
pub(super) fn signal_run_owner_confirmed(run: &JobRun) -> Result<String, OrbitError> {
    let outcome = signal_run_owner_process(run)?;
    let stopped = matches!(
        outcome.as_str(),
        "terminated_process_group"
            | "killed_process_group"
            | "terminated_owner"
            | "killed_owner"
            | "already_exited"
            | "no_pid"
    );
    if stopped || run_owner_liveness(run) == RunOwnerLiveness::Stopped {
        return Ok(outcome);
    }
    Err(OrbitError::Execution(format!(
        "could not confirm that the worker of run {} stopped (signal outcome {outcome})",
        run.run_id
    )))
}

/// What became of one provider child a cancellation went looking for.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderStop {
    /// The provider was alive, verified as the recorded process, and is gone.
    Stopped,
    /// Nothing to stop: the recorded process had already exited (or its pid
    /// now belongs to a different process).
    NotRunning,
    /// Liveness or identity could not be established, so nothing was signalled.
    Unverified,
}

/// Stop a provider child recorded on a run's audit trail (TERM, then KILL).
///
/// Provider CLIs are spawned in their own process group, so signalling the
/// run owner's group never reaches them; without this a cancelled run leaves
/// its agent editing the worktree and spending provider budget after its
/// reservations were released. Only a process whose recorded start token
/// still matches is signalled, so a recycled pid is never touched.
#[cfg(unix)]
pub(super) fn stop_provider_process(
    pid: u32,
    pid_start_time: Option<&str>,
) -> Result<ProviderStop, OrbitError> {
    stop_provider_process_with_probe(
        pid,
        pid_start_time,
        orbit_common::process::identity::probe_process_liveness,
        probe_process_start_identity,
    )
}

// Separate probe seams let sibling fault-injection tests exercise a live PID
// whose identity cannot be read, without changing host probing permissions.
#[cfg(unix)]
pub(super) fn stop_provider_process_with_probe<L, I>(
    pid: u32,
    pid_start_time: Option<&str>,
    liveness_probe: L,
    identity_probe: I,
) -> Result<ProviderStop, OrbitError>
where
    L: Fn(u32, Option<&str>) -> orbit_common::process::identity::ProcessLiveness,
    I: Fn(u32) -> ProbeOutcome,
{
    let start_key = orbit_common::process::ancestry::process_start_key(pid);
    stop_provider_process_with_binding(
        pid,
        pid_start_time,
        liveness_probe,
        identity_probe,
        |pid, token| BoundTarget::bind(pid, token, start_key),
    )
}

#[cfg(unix)]
pub(super) fn stop_provider_process_with_binding<L, I, T: TeardownTarget>(
    pid: u32,
    pid_start_time: Option<&str>,
    liveness_probe: L,
    identity_probe: I,
    bind: impl FnOnce(u32, Option<&str>) -> Result<Option<T>, OrbitError>,
) -> Result<ProviderStop, OrbitError>
where
    L: Fn(u32, Option<&str>) -> orbit_common::process::identity::ProcessLiveness,
    I: Fn(u32) -> ProbeOutcome,
{
    use orbit_common::process::identity::ProcessLiveness;

    if pid <= 1 || pid > i32::MAX as u32 || pid == std::process::id() {
        return Ok(ProviderStop::Unverified);
    }
    match liveness_probe(pid, pid_start_time) {
        ProcessLiveness::Exited => return Ok(ProviderStop::NotRunning),
        ProcessLiveness::Unknown => return Ok(ProviderStop::Unverified),
        ProcessLiveness::Alive => {}
    }
    // An unversioned token cannot prove the pid still names the provider.
    let Some(persisted) = pid_start_time.filter(|token| is_stable_token(token)) else {
        return Ok(ProviderStop::Unverified);
    };
    // Observational Alive is conservative when identity probing fails. It
    // cannot authorize signalling: require a positively matching live token.
    match identity_probe(pid) {
        ProbeOutcome::Token(current) if stable_tokens_match(persisted, &current) => {}
        ProbeOutcome::Token(_) => return Ok(ProviderStop::NotRunning),
        ProbeOutcome::NoProcess | ProbeOutcome::Unavailable => {
            return Ok(ProviderStop::Unverified);
        }
    }

    let Some(target) = bind(pid, pid_start_time)? else {
        return Ok(ProviderStop::NotRunning);
    };
    terminate_target(&target, pid)?;
    Ok(ProviderStop::Stopped)
}

/// One retained cancellation identity set, shared by owner and provider paths.
#[cfg(unix)]
pub(super) trait TeardownTarget {
    fn pgid(&self) -> Option<libc::pid_t>;
    fn signal(&self, signal: libc::c_int) -> Result<(), OrbitError>;
    fn liveness(&self) -> Result<(bool, bool), OrbitError>;
    fn wait(&self, timeout: Duration) -> Result<(), OrbitError>;
}

#[cfg(unix)]
fn terminate_target(target: &impl TeardownTarget, pid: u32) -> Result<bool, OrbitError> {
    target.signal(libc::SIGTERM)?;
    target.wait(RUN_OWNER_TERMINATION_GRACE)?;
    if target.liveness()? == (false, false) {
        return Ok(false);
    }
    target.signal(libc::SIGKILL)?;
    target.wait(RUN_OWNER_TERMINATION_GRACE)?;
    let (leader_alive, group_alive) = target.liveness()?;
    if leader_alive || group_alive {
        return Err(OrbitError::RunCancellationIncomplete {
            pid,
            pgid: target.pgid(),
            term_sent: true,
            kill_sent: true,
            leader_alive,
            group_alive,
        });
    }
    Ok(true)
}

#[cfg(unix)]
pub(super) struct BoundTarget {
    leader: orbit_common::process::signal::BoundProcess,
    members: Vec<orbit_common::process::signal::BoundProcess>,
    pgid: Option<libc::pid_t>,
}

#[cfg(unix)]
impl BoundTarget {
    pub(super) fn bind(
        pid: u32,
        token: Option<&str>,
        start_key: Option<orbit_common::process::ancestry::ProcessStartKey>,
    ) -> Result<Option<Self>, OrbitError> {
        use orbit_common::process::signal::{BoundProcess, process_group_members};
        let Some(start_key) = start_key else {
            return Err(OrbitError::Execution(
                "kernel process start identity unavailable for cancellation binding".into(),
            ));
        };
        let Some(leader) = BoundProcess::bind(pid, token)? else {
            return Ok(None);
        };
        if !leader.is_alive()? {
            return Ok(None);
        }
        match orbit_common::process::ancestry::process_start_key(pid) {
            Some(current) if current == start_key => {}
            Some(_) => return Ok(None),
            None => {
                return Err(OrbitError::Execution(
                    "kernel process identity became unreadable during cancellation binding".into(),
                ));
            }
        }
        let pgid = leader
            .process_group()?
            .filter(|group| *group > 1 && *group as u32 == pid);
        if pgid == Some(unsafe { libc::getpgrp() }) {
            return Err(OrbitError::Execution(
                "owner process group matches current process".into(),
            ));
        }
        let mut members = Vec::new();
        if let Some(pgid) = pgid {
            for member_pid in process_group_members(pgid)? {
                if member_pid == pid {
                    continue;
                }
                if let Some(member) = BoundProcess::bind(member_pid, None)?
                    && member.process_group()? == Some(pgid)
                {
                    members.push(member);
                }
            }
            // A reused group may have been enumerated if the leader exited
            // during the snapshot. Reject it before delivering any signal.
            if !leader.is_alive()? || leader.process_group()? != Some(pgid) {
                return Err(OrbitError::Execution("owner exited or left its group during identity binding; group teardown unverified".into()));
            }
        }
        Ok(Some(Self {
            leader,
            members,
            pgid,
        }))
    }
}

#[cfg(unix)]
impl TeardownTarget for BoundTarget {
    fn pgid(&self) -> Option<libc::pid_t> {
        self.pgid
    }

    fn signal(&self, signal: libc::c_int) -> Result<(), OrbitError> {
        // Never kill(-pgid): bind each initial member before TERM and retain
        // those identities for KILL. A replacement PID or PGID is never used.
        // Stop members first so a cooperative leader can reap its children.
        for member in &self.members {
            member.signal(signal)?;
        }
        self.leader.signal(signal)?;
        Ok(())
    }

    fn liveness(&self) -> Result<(bool, bool), OrbitError> {
        let leader_alive = self.leader.is_alive()?;
        let mut group_alive = false;
        for member in &self.members {
            group_alive |= member.is_alive()?;
        }
        // New descendants and group reuse cannot be distinguished after all
        // captured identities exit. A live/unknown numeric group is only
        // evidence of incomplete teardown, never authority for a signal.
        group_alive |= self.pgid.is_some_and(process_group_is_alive);
        Ok((leader_alive, group_alive))
    }

    fn wait(&self, timeout: Duration) -> Result<(), OrbitError> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if self.liveness()? == (false, false) {
                return Ok(());
            }
            thread::sleep(RUN_OWNER_TERMINATION_POLL);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn owner_process_group_id(pid: u32) -> Option<libc::pid_t> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    (pgid > 0).then_some(pgid)
}

#[cfg(unix)]
fn process_group_is_alive(pgid: libc::pid_t) -> bool {
    !matches!(probe_process_group_liveness(pgid), KernelLiveness::Exited)
}

/// Returns true only for Running runs whose owner is conclusively stale
/// (Mismatch or Missing). Other classifications keep the run live.
#[cfg(unix)]
pub(super) fn running_run_owner_is_stale(run: &JobRun) -> bool {
    running_run_owner_stale_reason(run).is_some()
}

/// [ORB-10070] Grace window before an unclaimed `pending` run (no recorded
/// owner pid) may be treated as orphaned. Pipeline workers claim their queued
/// run within seconds of spawn, so the window only shields (a) a reconcile
/// racing that claim and (b) still-live queued runs submitted by binaries
/// predating the pending-owner claim. A claimed pending run needs no grace:
/// its owner liveness is probed exactly like a running run's.
pub(super) const PENDING_RUN_UNCLAIMED_GRACE_MINUTES: i64 = 30;

/// A worker can dispatch a child before another process can reliably probe
/// its newly recorded PID. Give that running owner a short window to settle
/// before an orphan sweep is allowed to finalize it.
#[cfg(unix)]
pub(super) const RUNNING_RUN_STARTUP_GRACE_SECONDS: i64 = 2;

/// Why a `pending` run is conclusively orphaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingStaleReason {
    /// A worker claimed the run and that owner process is conclusively gone.
    #[cfg(unix)]
    Owner(OwnerIdentity),
    /// No worker ever claimed the run and the claim window has long passed
    /// (e.g. the spawned worker died before claiming, or a host reboot killed
    /// queued workers left by an older binary that never recorded owners).
    NeverClaimed,
}

impl PendingStaleReason {
    /// Stable machine-readable vocabulary persisted in interrupted pipeline
    /// steps. The diagnostic message may evolve independently of these codes.
    pub(super) const fn error_code(self) -> &'static str {
        match self {
            #[cfg(unix)]
            Self::Owner(identity) => owner_identity_error_code(Some(identity)),
            Self::NeverClaimed => "never_claimed",
        }
    }
}

/// Returns `Some(reason)` only when a `pending` run is conclusively orphaned:
/// its claimed owner process is gone (Mismatch/Missing), or it was never
/// claimed and is older than [`PENDING_RUN_UNCLAIMED_GRACE_MINUTES`].
/// Inconclusive owner probes keep the run pending, mirroring the running-run
/// policy.
#[cfg(unix)]
pub(super) fn pending_run_stale_reason(run: &JobRun) -> Option<PendingStaleReason> {
    if run.state != JobRunState::Pending {
        return None;
    }
    if run.pid.is_some() {
        return match classify_run_owner(run) {
            identity @ (OwnerIdentity::Mismatch | OwnerIdentity::Missing) => {
                Some(PendingStaleReason::Owner(identity))
            }
            OwnerIdentity::Verified
            | OwnerIdentity::LegacyLiveUnverified
            | OwnerIdentity::ProbeUnavailable
            | OwnerIdentity::ForeignPidNamespace => None,
        };
    }
    pending_run_unclaimed_past_grace(run).then_some(PendingStaleReason::NeverClaimed)
}

/// Non-Unix: owner liveness cannot be probed, so a claimed pending run is
/// always presumed live; only the never-claimed grace window applies.
#[cfg(not(unix))]
pub(super) fn pending_run_stale_reason(run: &JobRun) -> Option<PendingStaleReason> {
    if run.state != JobRunState::Pending || run.pid.is_some() {
        return None;
    }
    pending_run_unclaimed_past_grace(run).then_some(PendingStaleReason::NeverClaimed)
}

fn pending_run_unclaimed_past_grace(run: &JobRun) -> bool {
    chrono::Utc::now().signed_duration_since(run.created_at)
        > chrono::Duration::minutes(PENDING_RUN_UNCLAIMED_GRACE_MINUTES)
}

/// Builds the diagnostic message recorded in the interrupted step when an
/// orphaned `pending` run is reconciled.
pub(super) fn stale_pending_run_message(run: &JobRun, reason: PendingStaleReason) -> String {
    let reason_str = reason.error_code();
    format!(
        "queued job run marked interrupted because no live worker process owns it (reason={}, pid={}, pid_start_time={}, created_at={}, unclaimed_grace_minutes={})",
        reason_str,
        run.pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string()),
        run.pid_start_time.as_deref().unwrap_or("-"),
        run.created_at.to_rfc3339(),
        PENDING_RUN_UNCLAIMED_GRACE_MINUTES,
    )
}

#[cfg(not(unix))]
pub(super) fn running_run_owner_is_stale(_run: &JobRun) -> bool {
    false
}

/// Returns `Some(reason)` only when a running run's owner is conclusively
/// either mismatched or missing — those are the two states that warrant
/// finalizing the run as failed. `ProbeUnavailable` and `LegacyLiveUnverified`
/// classifications never appear here: they keep the run Running.
#[cfg(unix)]
pub(super) fn running_run_owner_stale_reason(run: &JobRun) -> Option<OwnerIdentity> {
    if run.state != JobRunState::Running {
        return None;
    }
    if run.started_at.is_some_and(|started_at| {
        chrono::Utc::now().signed_duration_since(started_at)
            < chrono::Duration::seconds(RUNNING_RUN_STARTUP_GRACE_SECONDS)
    }) {
        return None;
    }
    match classify_run_owner(run) {
        identity @ (OwnerIdentity::Mismatch | OwnerIdentity::Missing) => Some(identity),
        OwnerIdentity::Verified
        | OwnerIdentity::LegacyLiveUnverified
        | OwnerIdentity::ProbeUnavailable
        | OwnerIdentity::ForeignPidNamespace => None,
    }
}

#[cfg(not(unix))]
pub(super) fn running_run_owner_stale_reason(_run: &JobRun) -> Option<()> {
    None
}

/// Outcome of comparing a persisted owner identity against the live process.
///
/// Only `Mismatch` and `Missing` warrant finalizing the run as failed.
///
/// - `Verified` — versioned token (or legacy token re-derived under either
///   environment) matches the live process: the worker is the original owner.
/// - `Mismatch` — versioned persisted token disagrees with the live process's
///   current token: a different process is holding the PID. Stale.
/// - `LegacyLiveUnverified` — a legacy unversioned token cannot
///   be re-derived under either environment, but `kill(pid, 0)` confirms the
///   PID is still alive. Stays Running; cancellation still refuses to signal
///   it (PID-reuse protection).
/// - `ProbeUnavailable` — kernel data could not be read or the fallback
///   `ps` invocation failed, and `kill(pid, 0)` confirms the PID is still alive.
///   A transient probe failure must never terminalize a live worker.
/// - `Missing` — no PID recorded, or both the probe and `kill(pid, 0)`
///   agree the PID is gone. Stale.
/// - `ForeignPidNamespace` — the owner was recorded in a different PID
///   namespace than this observer's [ORB-10594]. The recorded PID names a
///   different process here, or none; nothing probed from this side is
///   evidence about the owner. Stays Running, and cancellation refuses to
///   signal (the number would name the wrong process, if any).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerIdentity {
    Verified,
    Mismatch,
    LegacyLiveUnverified,
    ProbeUnavailable,
    ForeignPidNamespace,
    Missing,
}

#[cfg(unix)]
pub(super) fn classify_run_owner(run: &JobRun) -> OwnerIdentity {
    classify_run_owner_with_probes(
        run.pid,
        run.pid_start_time.as_deref(),
        pid_namespace_scope(run.pid_start_time.as_deref()),
        start_identity_probe,
        |pid| legacy_lstart_matches(pid, run.pid_start_time.as_deref().unwrap_or_default()),
        process_is_alive,
    )
}

/// Completion only blocks on a running run whose recorded PID and start-time
/// token identify the process still executing it. An unknown or unverified owner
/// is not proof that this particular implementation is alive.
#[cfg(unix)]
pub(crate) fn running_run_has_verified_owner(run: &JobRun) -> bool {
    run.state == JobRunState::Running && classify_run_owner(run) == OwnerIdentity::Verified
}

#[cfg(not(unix))]
pub(crate) fn running_run_has_verified_owner(_run: &JobRun) -> bool {
    false
}

#[cfg(unix)]
fn start_identity_probe(pid: u32) -> ProbeOutcome {
    probe_process_start_identity(pid)
}

/// Inner, testable form of [`classify_run_owner`] with the probes injected.
/// Production callers go through [`classify_run_owner`]; tests pass
/// deterministic closures to exercise rare probe states (Unavailable,
/// NoProcess-but-alive race, cross-namespace observation) without needing real
/// misbehaving processes or a real sandbox.
///
/// `scope` is passed in rather than derived so the classification stays a pure
/// function of its inputs: a test asserting genuine-orphan detection must not
/// change verdict because the test runner itself happens to be sandboxed.
#[cfg(unix)]
pub(super) fn classify_run_owner_with_probes<P, L, A>(
    pid: Option<u32>,
    persisted: Option<&str>,
    scope: PidNamespaceScope,
    probe: P,
    legacy_match: L,
    is_alive: A,
) -> OwnerIdentity
where
    P: FnOnce(u32) -> ProbeOutcome,
    L: FnOnce(u32) -> bool,
    A: FnOnce(u32) -> bool,
{
    // [ORB-10594] Ordered ahead of every probe: inside a private PID namespace
    // both the identity probe and `kill(pid, 0)` answer wrongly about a PID
    // that belongs to another namespace.
    if scope == PidNamespaceScope::Foreign {
        return OwnerIdentity::ForeignPidNamespace;
    }
    let Some(pid) = pid else {
        return OwnerIdentity::Missing;
    };
    let Some(persisted) = persisted else {
        return if is_alive(pid) {
            OwnerIdentity::LegacyLiveUnverified
        } else {
            OwnerIdentity::Missing
        };
    };
    if is_stable_token(persisted) {
        return match probe(pid) {
            ProbeOutcome::Token(current) if stable_tokens_match(persisted, &current) => {
                OwnerIdentity::Verified
            }
            ProbeOutcome::Token(_) => OwnerIdentity::Mismatch,
            ProbeOutcome::NoProcess => {
                if is_alive(pid) {
                    // Race: the probe returned no-process but `kill(pid, 0)` still
                    // sees the PID. Defer finalization until the probe agrees.
                    OwnerIdentity::ProbeUnavailable
                } else {
                    OwnerIdentity::Missing
                }
            }
            ProbeOutcome::Unavailable => {
                if is_alive(pid) {
                    OwnerIdentity::ProbeUnavailable
                } else {
                    OwnerIdentity::Missing
                }
            }
        };
    }
    if legacy_match(pid) {
        OwnerIdentity::Verified
    } else if is_alive(pid) {
        OwnerIdentity::LegacyLiveUnverified
    } else {
        OwnerIdentity::Missing
    }
}

/// [ORB-10597] Whether a run's recorded owner process is still executing,
/// asked *independently of the run's persisted state*.
///
/// Marking a run `interrupted` attaches no teardown, so a terminal state is not
/// by itself evidence that the work stopped. Callers that must distinguish
/// "terminal and stopped" from "terminal and still executing" ask this instead
/// of reading `JobRunState::is_terminal`.
///
/// Deliberately three-valued: `Unknown` is not a synonym for either answer, and
/// each caller picks its own fail-safe direction for it — releasing an
/// `overlap:forbid` slot requires `Stopped`, refusing a resume requires `Alive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunOwnerLiveness {
    /// A process with the recorded identity answers a liveness probe. Work may
    /// still be in flight no matter what the run record says.
    Alive,
    /// Conclusively gone: no PID was ever recorded, the PID is absent, or it
    /// now names a different process than the one that claimed the run.
    Stopped,
    /// Nothing observable from here is evidence either way — the owner was
    /// recorded in a foreign PID namespace [ORB-10594], or this platform has no
    /// probe at all.
    Unknown,
}

#[cfg(unix)]
pub(crate) fn run_owner_liveness(run: &JobRun) -> RunOwnerLiveness {
    match classify_run_owner(run) {
        // `ProbeUnavailable` reaches here only when `kill(pid, 0)` succeeded,
        // so some process holds the PID even though the probe could not confirm the
        // identity token. That is enough to refuse to treat the owner as gone.
        OwnerIdentity::Verified
        | OwnerIdentity::LegacyLiveUnverified
        | OwnerIdentity::ProbeUnavailable => RunOwnerLiveness::Alive,
        OwnerIdentity::Missing | OwnerIdentity::Mismatch => RunOwnerLiveness::Stopped,
        OwnerIdentity::ForeignPidNamespace => RunOwnerLiveness::Unknown,
    }
}

#[cfg(not(unix))]
pub(crate) fn run_owner_liveness(_run: &JobRun) -> RunOwnerLiveness {
    RunOwnerLiveness::Unknown
}

/// Builds the diagnostic message recorded in the failure step when a stale
/// owner causes a Running run to be reconciled to Failed.
#[cfg(unix)]
pub(super) fn stale_job_run_message(run: &JobRun, reason: Option<OwnerIdentity>) -> String {
    let reason_str = owner_identity_error_code(reason);
    format!(
        "job run marked interrupted because recorded worker process is no longer alive (reason={}, pid={}, pid_start_time={})",
        reason_str,
        run.pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string()),
        run.pid_start_time.as_deref().unwrap_or("-")
    )
}

#[cfg(unix)]
pub(super) const fn owner_identity_error_code(reason: Option<OwnerIdentity>) -> &'static str {
    match reason {
        Some(OwnerIdentity::Mismatch) => "token_mismatch",
        Some(OwnerIdentity::Missing) => "process_not_found",
        // ProbeUnavailable / Verified / LegacyLiveUnverified never reach the
        // stale-message path, but a future caller could; keep them tagged so
        // the diagnostic is never silently wrong.
        Some(OwnerIdentity::ProbeUnavailable) => "probe_unavailable",
        Some(OwnerIdentity::Verified) => "verified",
        Some(OwnerIdentity::LegacyLiveUnverified) => "legacy_live_unverified",
        Some(OwnerIdentity::ForeignPidNamespace) => "foreign_pid_namespace",
        None => "unknown",
    }
}

#[cfg(not(unix))]
pub(super) const fn owner_identity_error_code(_reason: Option<()>) -> &'static str {
    "unknown"
}

#[cfg(not(unix))]
pub(super) fn stale_job_run_message(run: &JobRun, _reason: Option<()>) -> String {
    format!(
        "job run marked interrupted because recorded worker process is no longer alive (reason=unknown, pid={}, pid_start_time={})",
        run.pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string()),
        run.pid_start_time.as_deref().unwrap_or("-")
    )
}
