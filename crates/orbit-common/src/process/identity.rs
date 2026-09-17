//! Stable cross-process identity tokens for job-run owner verification.
//!
//! On Unix, the token is derived from `ps -o lstart=` with the child
//! environment forced to `TZ=UTC` / `LC_ALL=C` / `LANG=C` so the persisted
//! value does not depend on the caller's locale or timezone. Tokens written
//! by this helper carry a [`STABLE_TOKEN_PREFIX`] so readers can distinguish
//! them from legacy unversioned values.
//!
//! [ORB-10594] A PID only names a process *within a PID namespace*. Orbit
//! spawns sandboxed agents under `bwrap --unshare-all --proc /proc`, which
//! gives them a private PID namespace: inside it, the host PIDs that own live
//! job runs are invisible to both `ps` and `kill(pid, 0)`, and every liveness
//! probe run from there reports "process not found" for a perfectly healthy
//! worker. Sandboxed agents routinely call the Orbit CLI, and every CLI open
//! sweeps for orphaned runs — so the token records the *namespace it was
//! written in* and readers refuse to judge liveness across a namespace
//! boundary.

/// Prefix on versioned identity tokens. Persisted tokens that start with this
/// prefix were written by the stable strategy and must match exactly.
///
/// v2 adds the writer's PID namespace: `ps-lstart-utc-v2:pidns=<inode>:<lstart>`.
pub const STABLE_TOKEN_PREFIX: &str = "ps-lstart-utc-v2:";

/// Prefix on pre-[ORB-10594] versioned tokens, which carry no namespace field.
/// Still read (a run claimed by an older binary outlives its upgrade), never
/// written.
pub const STABLE_TOKEN_PREFIX_V1: &str = "ps-lstart-utc-v1:";

/// Field introducing the PID namespace inside a v2 token.
const PID_NAMESPACE_FIELD: &str = "pidns=";

/// Namespace value written when the host cannot report one (non-Linux, or an
/// unreadable `/proc/self/ns/pid`). Reads back as "namespace unknown".
const UNKNOWN_PID_NAMESPACE: &str = "-";

/// True for a token written by either versioned strategy.
pub fn is_stable_token(token: &str) -> bool {
    token.starts_with(STABLE_TOKEN_PREFIX) || token.starts_with(STABLE_TOKEN_PREFIX_V1)
}

/// The PID namespace a v2 token was written in, when it names one. `None` for
/// v1/legacy tokens and for v2 tokens whose writer could not resolve one.
pub fn token_pid_namespace(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(STABLE_TOKEN_PREFIX)?;
    let tail = rest.strip_prefix(PID_NAMESPACE_FIELD)?;
    let (namespace, _) = tail.split_once(':')?;
    (namespace != UNKNOWN_PID_NAMESPACE).then_some(namespace)
}

/// The process-start portion of a versioned token, with any namespace field
/// stripped. `None` when the token is not versioned.
pub fn token_start_identity(token: &str) -> Option<&str> {
    if let Some(rest) = token.strip_prefix(STABLE_TOKEN_PREFIX) {
        return Some(match rest.strip_prefix(PID_NAMESPACE_FIELD) {
            // The namespace value never contains ':', so the first separator
            // ends it and everything after is the (colon-bearing) lstart.
            Some(tail) => tail
                .split_once(':')
                .map(|(_, lstart)| lstart)
                .unwrap_or(tail),
            None => rest,
        });
    }
    token.strip_prefix(STABLE_TOKEN_PREFIX_V1)
}

/// True when a persisted token and a freshly probed one describe the same
/// process. Version-tolerant: a v1 token persisted before [ORB-10594] still
/// verifies against a v2 probe on its process-start value, so upgrading the
/// binary does not invalidate the owners of in-flight runs.
pub fn stable_tokens_match(persisted: &str, probed: &str) -> bool {
    let (Some(persisted_start), Some(probed_start)) = (
        token_start_identity(persisted),
        token_start_identity(probed),
    ) else {
        return false;
    };
    if persisted_start != probed_start {
        return false;
    }
    match (token_pid_namespace(persisted), token_pid_namespace(probed)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

/// Where the observer stands relative to the PID namespace a token was
/// written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidNamespaceScope {
    /// Observer and writer share a PID namespace: PIDs mean the same thing on
    /// both sides, so liveness probes are authoritative.
    Same,
    /// Observer is in a different PID namespace than the writer. The recorded
    /// PID names a different process here, or none at all; no probe run from
    /// this side can say anything about the recorded process.
    Foreign,
    /// One side did not record a namespace (v1/legacy token, no token, or a
    /// host that cannot report one). Probes are treated as authoritative, as
    /// they were before [ORB-10594].
    Unknown,
}

/// The observer's own PID namespace, as the inode number Linux reports for
/// `/proc/self/ns/pid` (`pid:[4026531836]` → `4026531836`).
///
/// Constant for the lifetime of the process — a process cannot leave its PID
/// namespace — so it is resolved once and cached; the orphan sweep asks per
/// run.
pub fn current_pid_namespace() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        static NAMESPACE: OnceLock<Option<String>> = OnceLock::new();
        NAMESPACE
            .get_or_init(|| {
                let link = std::fs::read_link("/proc/self/ns/pid").ok()?;
                let rendered = link.to_string_lossy();
                let (_, tail) = rendered.split_once('[')?;
                let (inode, _) = tail.split_once(']')?;
                (!inode.is_empty()).then(|| inode.to_string())
            })
            .as_deref()
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Classify the observer against the namespace recorded in `persisted`.
///
/// Deliberately conservative in one direction only: a missing namespace on
/// either side yields [`PidNamespaceScope::Unknown`], never `Foreign`. Treating
/// "unknown" as foreign would disable genuine-orphan detection outright for
/// any deployment whose workers legitimately run inside a container.
pub fn pid_namespace_scope(persisted: Option<&str>) -> PidNamespaceScope {
    match (
        persisted.and_then(token_pid_namespace),
        current_pid_namespace(),
    ) {
        (Some(recorded), Some(current)) if recorded == current => PidNamespaceScope::Same,
        (Some(_), Some(_)) => PidNamespaceScope::Foreign,
        _ => PidNamespaceScope::Unknown,
    }
}

/// Outcome of a single process-start identity probe.
///
/// Readers must distinguish a *probe failure* (`Unavailable`) from a
/// *process-not-found* result (`NoProcess`): the former indicates we cannot
/// tell whether the PID is alive (transient `ps` spawn error, etc.) and must
/// not be enough to terminalize a still-running worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// `ps` succeeded and produced a non-empty versioned token.
    Token(String),
    /// `ps` exited non-zero or returned an empty token; the kernel has no
    /// process with this PID.
    NoProcess,
    /// `Command::output()` itself errored (spawn/IO failure). Identity cannot
    /// be probed; defer to other liveness signals.
    Unavailable,
}

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
fn lstart_raw(pid: u32, stable_env: bool) -> Result<Option<String>, io::Error> {
    let mut cmd = Command::new("ps");
    cmd.args(["-o", "lstart=", "-p", &pid.to_string()]);
    if stable_env {
        cmd.env("TZ", "UTC").env("LC_ALL", "C").env("LANG", "C");
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!token.is_empty()).then_some(token))
}

/// Backs [`crate::test_env::start_identity_probe_blocker`]: the reason the
/// stable probe cannot produce a token for the current process, if any.
#[cfg(unix)]
pub(crate) fn self_start_identity_probe_blocker() -> Option<String> {
    match lstart_raw(std::process::id(), true) {
        Ok(Some(_)) => None,
        Ok(None) => {
            Some("`ps -o lstart=` reported no process for this process's own pid".to_string())
        }
        Err(error) => Some(format!(
            "`ps` cannot be executed from this process (an agent sandbox that denies `ps`?): {error}"
        )),
    }
}

#[cfg(not(unix))]
pub(crate) fn self_start_identity_probe_blocker() -> Option<String> {
    Some("process-start identity probes are Unix-only".to_string())
}

/// Probe a running process and classify the outcome.
#[cfg(unix)]
pub fn probe_process_start_identity(pid: u32) -> ProbeOutcome {
    match lstart_raw(pid, true) {
        Ok(Some(raw)) => {
            let namespace = current_pid_namespace().unwrap_or(UNKNOWN_PID_NAMESPACE);
            ProbeOutcome::Token(format!(
                "{STABLE_TOKEN_PREFIX}{PID_NAMESPACE_FIELD}{namespace}:{raw}"
            ))
        }
        Ok(None) => ProbeOutcome::NoProcess,
        Err(_) => ProbeOutcome::Unavailable,
    }
}

#[cfg(not(unix))]
pub fn probe_process_start_identity(_pid: u32) -> ProbeOutcome {
    ProbeOutcome::Unavailable
}

/// Versioned, locale/timezone-stable process-start identity token. Writers
/// (and readers that only need a `String`) call this; consumers that need to
/// distinguish probe-not-found from probe-unavailable call
/// [`probe_process_start_identity`] instead.
pub fn process_start_identity_token(pid: u32) -> Option<String> {
    match probe_process_start_identity(pid) {
        ProbeOutcome::Token(s) => Some(s),
        ProbeOutcome::NoProcess | ProbeOutcome::Unavailable => None,
    }
}

/// Returns true when `persisted` is a legacy unversioned token whose value
/// matches either the caller-environment `ps -o lstart=` output or the
/// stable-environment one for this PID. Versioned tokens always return false
/// here so callers route them through [`process_start_identity_token`].
#[cfg(unix)]
pub fn legacy_lstart_matches(pid: u32, persisted: &str) -> bool {
    if is_stable_token(persisted) {
        return false;
    }
    if let Ok(Some(stable_raw)) = lstart_raw(pid, true)
        && stable_raw == persisted
    {
        return true;
    }
    if let Ok(Some(ambient)) = lstart_raw(pid, false)
        && ambient == persisted
    {
        return true;
    }
    false
}

#[cfg(not(unix))]
pub fn legacy_lstart_matches(_pid: u32, _persisted: &str) -> bool {
    false
}

/// Liveness of a PID recorded earlier in a run, judged against the identity
/// token captured when the PID was recorded [ORB-10496].
///
/// The distinction that matters to an operator is `Alive` vs `Exited`: a
/// long-running agent subprocess and a dead one otherwise look identical from
/// outside the process tree. `Unknown` means the host cannot answer (non-Unix),
/// and must never be read as "dead".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLiveness {
    /// The PID exists and is not a reaped zombie. When an identity token was
    /// recorded it also still matches, so this is the same process.
    Alive,
    /// The PID is gone, is an unreaped zombie, or is now held by a different
    /// process (recorded identity token no longer matches).
    Exited,
    /// Liveness cannot be probed on this platform.
    Unknown,
}

impl ProcessLiveness {
    /// Stable wire token for JSON/table surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessLiveness::Alive => "alive",
            ProcessLiveness::Exited => "exited",
            ProcessLiveness::Unknown => "unknown",
        }
    }
}

/// Probe whether a recorded PID is still the live process it was recorded as.
///
/// `pid_start_time` is the token captured alongside the PID (see
/// [`process_start_identity_token`]). It guards against PID reuse: a live PID
/// whose versioned token disagrees with the current process is reported
/// `Exited`, because the recorded process is gone even though the number was
/// recycled. A live PID with no token, or one whose probe could not run (`ps`
/// unavailable in a sandbox), stays `Alive` — a probe that cannot answer must
/// not be enough to declare a running process dead.
///
/// [ORB-10594] A PID recorded in another PID namespace is `Unknown`, not
/// `Exited`: from here the number names nothing, which is not evidence that
/// the recorded process stopped.
#[cfg(unix)]
pub fn probe_process_liveness(pid: u32, pid_start_time: Option<&str>) -> ProcessLiveness {
    if pid_namespace_scope(pid_start_time) == PidNamespaceScope::Foreign {
        return ProcessLiveness::Unknown;
    }
    if !process_is_alive(pid) {
        return ProcessLiveness::Exited;
    }
    let Some(persisted) = pid_start_time else {
        return ProcessLiveness::Alive;
    };
    if !is_stable_token(persisted) {
        // A legacy unversioned token that cannot be re-derived is unverifiable,
        // not falsified. The PID is alive either way.
        return ProcessLiveness::Alive;
    }
    match probe_process_start_identity(pid) {
        ProbeOutcome::Token(current) if stable_tokens_match(persisted, &current) => {
            ProcessLiveness::Alive
        }
        ProbeOutcome::Token(_) => ProcessLiveness::Exited,
        // `kill(pid, 0)` above already saw the PID; a `ps` that disagrees is a
        // race or a blocked probe, not proof of death.
        ProbeOutcome::NoProcess | ProbeOutcome::Unavailable => ProcessLiveness::Alive,
    }
}

#[cfg(not(unix))]
pub fn probe_process_liveness(_pid: u32, _pid_start_time: Option<&str>) -> ProcessLiveness {
    ProcessLiveness::Unknown
}

/// Kernel-level activity state for a PID or process group.
///
/// `Unknown` is deliberately distinct from `Exited`: callers which make a
/// safety decision must preserve the possibility that work is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLiveness {
    Alive,
    Exited,
    Unknown,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalPresence {
    Present,
    Missing,
    Unknown,
}

#[cfg(unix)]
fn signal_presence(target: libc::pid_t) -> SignalPresence {
    // Safety: signal 0 performs existence/permission checking only.
    let rc = unsafe { libc::kill(target, 0) };
    if rc == 0 {
        return SignalPresence::Present;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => SignalPresence::Present,
        Some(libc::ESRCH) => SignalPresence::Missing,
        _ => SignalPresence::Unknown,
    }
}

/// True when `pid` names a process that can still run.
///
/// Linux and macOS keep unreaped zombies visible to `kill(pid, 0)`; native
/// state probes report those processes dead here because they can no longer do
/// work. An unavailable native probe and `EPERM` remain conservatively alive.
#[cfg(unix)]
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(target_os = "linux")]
    if matches!(linux_process_state(pid), Some(('Z', _))) {
        return false;
    }
    #[cfg(target_os = "macos")]
    if let Some((is_zombie, _)) = darwin_process_state(pid) {
        return !is_zombie;
    }
    !matches!(signal_presence(pid as libc::pid_t), SignalPresence::Missing)
}

#[cfg(not(unix))]
pub fn process_is_alive(_pid: u32) -> bool {
    false
}

/// Linux `/proc/<pid>/stat` run state and process-group id.
///
/// Exposed so callers that need the zombie distinction for a *group* (not just
/// a single PID) can reuse the same parse instead of re-deriving it.
#[cfg(target_os = "linux")]
pub fn linux_process_state(pid: u32) -> Option<(char, libc::pid_t)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(')')?;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent_pid = fields.next()?;
    let process_group = fields.next()?.parse().ok()?;
    Some((state, process_group))
}

/// Native Darwin process state and process-group id.
///
/// `proc_pidinfo(PROC_PIDTBSDINFO)` is a syscall-backed libproc query, so the
/// 50 ms cancellation poll never spawns `ps`. A failed query is inconclusive;
/// callers retain `kill(..., 0)` as a conservative existence/permission probe.
#[cfg(target_os = "macos")]
fn darwin_process_state(pid: u32) -> Option<(bool, libc::pid_t)> {
    use std::mem::{MaybeUninit, size_of};

    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let expected = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // Safety: `info` points to `expected` writable bytes and is initialized
    // only when libproc reports that it filled the complete structure. The
    // non-zero argument asks XNU's proc_info path to include zombie records;
    // without it, proc_pidinfo returns no record once the process is zombified.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if written != expected {
        return None;
    }
    // Safety: the full structure was written above.
    let info = unsafe { info.assume_init() };
    let process_group = libc::pid_t::try_from(info.pbi_pgid).ok()?;
    Some((info.pbi_status == libc::SZOMB, process_group))
}

/// Probe whether a process group contains at least one process that can still
/// run. Zombie-only groups are `Exited`; failures to enumerate or inspect an
/// existing member are `Unknown`, never evidence of death.
#[cfg(unix)]
pub fn probe_process_group_liveness(pgid: libc::pid_t) -> KernelLiveness {
    if pgid <= 1 {
        return KernelLiveness::Exited;
    }
    #[cfg(target_os = "linux")]
    if let Some(liveness) = linux_process_group_liveness(pgid) {
        return liveness;
    }
    #[cfg(target_os = "macos")]
    if let Some(liveness) = darwin_process_group_liveness(pgid) {
        return liveness;
    }
    match signal_presence(-pgid) {
        SignalPresence::Present => KernelLiveness::Alive,
        SignalPresence::Missing => KernelLiveness::Exited,
        SignalPresence::Unknown => KernelLiveness::Unknown,
    }
}

#[cfg(not(unix))]
pub fn probe_process_group_liveness(_pgid: libc::pid_t) -> KernelLiveness {
    KernelLiveness::Unknown
}

/// Linux `/proc` group scan which distinguishes zombie-only groups from live
/// groups without changing the generic fail-safe behavior when `/proc` is
/// unavailable or incomplete.
#[cfg(target_os = "linux")]
fn linux_process_group_liveness(pgid: libc::pid_t) -> Option<KernelLiveness> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut found_group_member = false;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Some((state, member_pgid)) = linux_process_state(pid) else {
            continue;
        };
        if member_pgid != pgid {
            continue;
        }
        found_group_member = true;
        if state != 'Z' {
            return Some(KernelLiveness::Alive);
        }
    }
    found_group_member.then_some(KernelLiveness::Exited)
}

/// Darwin libproc group scan. The buffer is retried when libproc fills it,
/// avoiding a truncated scan being mistaken for a zombie-only group.
#[cfg(target_os = "macos")]
fn darwin_process_group_liveness(pgid: libc::pid_t) -> Option<KernelLiveness> {
    use std::ffi::c_void;
    use std::mem::size_of;

    // Safety: a null/zero probe asks the wrapper for the required PID count.
    // Unlike `proc_listpids`, `proc_listpgrppids` divides the kernel's byte
    // count by `sizeof(int)` before returning.
    let required = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    if required <= 0 {
        return match signal_presence(-pgid) {
            SignalPresence::Missing => Some(KernelLiveness::Exited),
            SignalPresence::Present | SignalPresence::Unknown => None,
        };
    }

    let pid_size = size_of::<libc::pid_t>();
    let mut slots = usize::try_from(required).ok()?.saturating_mul(2).max(16);
    for _ in 0..3 {
        let mut pids = vec![0 as libc::pid_t; slots];
        let buffer_size = i32::try_from(pids.len().saturating_mul(pid_size)).ok()?;
        // Safety: `pids` is writable for exactly `buffer_size` bytes.
        let written_count = unsafe {
            libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast::<c_void>(), buffer_size)
        };
        if written_count < 0 {
            return None;
        }
        let written_count = usize::try_from(written_count).ok()?;
        if written_count == pids.len() {
            slots = slots.saturating_mul(2);
            continue;
        }
        pids.truncate(written_count);
        return Some(darwin_group_liveness_from_pids(pgid, &pids));
    }
    None
}

#[cfg(target_os = "macos")]
fn darwin_group_liveness_from_pids(pgid: libc::pid_t, pids: &[libc::pid_t]) -> KernelLiveness {
    let mut found_zombie = false;
    let mut unknown_member = false;
    for &pid in pids.iter().filter(|pid| **pid > 0) {
        let Ok(pid_u32) = u32::try_from(pid) else {
            unknown_member = true;
            continue;
        };
        match darwin_process_state(pid_u32) {
            Some((false, member_pgid)) if member_pgid == pgid => return KernelLiveness::Alive,
            Some((true, member_pgid)) if member_pgid == pgid => found_zombie = true,
            Some(_) => {}
            None => match signal_presence(pid) {
                SignalPresence::Missing => {}
                SignalPresence::Present | SignalPresence::Unknown => unknown_member = true,
            },
        }
    }
    if unknown_member {
        KernelLiveness::Unknown
    } else if found_zombie {
        KernelLiveness::Exited
    } else {
        match signal_presence(-pgid) {
            SignalPresence::Missing => KernelLiveness::Exited,
            SignalPresence::Present | SignalPresence::Unknown => KernelLiveness::Unknown,
        }
    }
}
