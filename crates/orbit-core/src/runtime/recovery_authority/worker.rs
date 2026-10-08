//! Host process and PID-namespace bindings for managed workers.

use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::params;
#[cfg(unix)]
use rusqlite::{Connection, OptionalExtension};

use super::certificate::RecoveryAuthority;
use super::error::authority_error;
#[cfg(unix)]
use super::root::{
    AUTHORITY_DB, AUTHORITY_DIR, refuse_symlinked, refuse_symlinked_authority_files,
    validated_authority_global_root,
};

/// The kernel process table every binding probe reads.
#[cfg(target_os = "linux")]
pub(crate) const PROC_ROOT: &str = "/proc";

/// PID of the namespace leader a sandboxed worker shares with its host record.
#[cfg(target_os = "linux")]
const NAMESPACE_LEADER_PID: u32 = 1;

impl RecoveryAuthority {
    /// Bind a kernel process identity to the runtime's immutable attempt. The
    /// authority database is outside all managed-leaf write grants; neither a
    /// job-input edit nor an environment machine label can create this row.
    pub(crate) fn bind_worker_process(
        &self,
        pid: u32,
        binding: &orbit_types::tool::WorkerInvocation,
    ) -> Result<(), OrbitError> {
        binding.validate()?;
        let identity = orbit_common::process::identity::process_start_identity_token(pid)
            .ok_or_else(|| OrbitError::Execution("worker process identity unavailable".into()))?;
        self.connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS worker_process_binding (
            pid INTEGER NOT NULL, identity TEXT NOT NULL, binding_json TEXT NOT NULL,
            PRIMARY KEY(pid, identity)
        ) WITHOUT ROWID;",
            )
            .map_err(|error| authority_error("create worker binding table", error))?;
        let json =
            serde_json::to_string(binding).map_err(|error| OrbitError::Store(error.to_string()))?;
        self.connection.execute(
            "INSERT INTO worker_process_binding(pid, identity, binding_json) VALUES (?1,?2,?3) ON CONFLICT(pid,identity) DO NOTHING",
            params![pid, identity, json],
        ).map_err(|error| authority_error("bind worker process", error))?;
        let recorded: String = self
            .connection
            .query_row(
                "SELECT binding_json FROM worker_process_binding WHERE pid=?1 AND identity=?2",
                params![pid, identity],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read worker process binding", error))?;
        if recorded != json {
            return Err(OrbitError::PolicyDenied(
                "worker process binding is immutable".into(),
            ));
        }
        Ok(())
    }
}

/// Resolve a process or live ancestor against host-only authority. Environment
/// values can require a binding, but never supply one or choose its identity.
#[cfg(target_os = "linux")]
pub(crate) fn current_worker_binding(
    global_root: &Path,
) -> Result<Option<orbit_types::tool::WorkerInvocation>, OrbitError> {
    current_worker_binding_in(global_root, Path::new(PROC_ROOT))
}

/// macOS resolves the same process-identity rows through libproc ancestry
/// [ORB-13625]. There is no PID-namespace leg: macOS workers are confined by
/// `sandbox-exec`, which shares the host PID space, so the host-recorded
/// process identity of the worker or a live ancestor is what a child finds.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn current_worker_binding(
    global_root: &Path,
) -> Result<Option<orbit_types::tool::WorkerInvocation>, OrbitError> {
    resolve_worker_binding(global_root, &|| None, &|pid| {
        Ok(
            orbit_common::process::ancestry::process_start_and_parent(pid)
                .map(|(_, parent)| parent),
        )
    })
}

#[cfg(not(unix))]
pub(crate) fn current_worker_binding(
    _global_root: &Path,
) -> Result<Option<orbit_types::tool::WorkerInvocation>, OrbitError> {
    Ok(None)
}

/// Resolution against an explicit `/proc` mount. Tests inject a proc root that
/// denies the namespace and ancestry probes; production always passes
/// [`PROC_ROOT`].
#[cfg(target_os = "linux")]
// Explicit proc-root seam shared with the denied-probe security fixture.
pub(super) fn current_worker_binding_in(
    global_root: &Path,
    proc_root: &Path,
) -> Result<Option<orbit_types::tool::WorkerInvocation>, OrbitError> {
    // Both probes are identity discovery, not authorization: a `/proc` entry
    // this process may not read (`EACCES` on a root-owned PID 1, a
    // `hidepid=2` mount, a restricted ancestor) means "no binding here",
    // never a refusal to open the runtime. `restore_process_binding` is what
    // fails closed for a managed child that requires one.
    resolve_worker_binding(
        global_root,
        &|| namespace_key(proc_root, NAMESPACE_LEADER_PID).ok(),
        &|pid| {
            let Ok(stat) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat"))
            else {
                // An ancestor that has exited or that this process may not
                // read ends the walk with no binding.
                return Ok(None);
            };
            stat.rsplit_once(')')
                .and_then(|(_, fields)| fields.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u32>().ok())
                .map(Some)
                .ok_or_else(|| OrbitError::Execution("worker process ancestry unavailable".into()))
        },
    )
}

/// The platform-neutral walk: the namespace leg when the platform has one,
/// then this process and each live ancestor by pid plus kernel start
/// identity. `parent_of` answers `Ok(None)` when an ancestor cannot be read,
/// which ends the walk unbound.
#[cfg(unix)]
fn resolve_worker_binding(
    global_root: &Path,
    namespace_key_probe: &dyn Fn() -> Option<String>,
    parent_of: &dyn Fn(u32) -> Result<Option<u32>, OrbitError>,
) -> Result<Option<orbit_types::tool::WorkerInvocation>, OrbitError> {
    if !global_root.try_exists()? {
        return Ok(None);
    }
    let root = validated_authority_global_root(global_root)?.join(AUTHORITY_DIR);
    if !root.try_exists()? {
        return Ok(None);
    }
    if std::fs::symlink_metadata(&root)?.file_type().is_symlink() {
        return Err(refuse_symlinked(&root));
    }
    refuse_symlinked_authority_files(&root)?;
    let path = root.join(AUTHORITY_DB);
    if !path.try_exists()? {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| authority_error("read worker authority", error))?;
    let namespace_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_namespace_binding')", [], |row| row.get(0),
    ).map_err(|error| authority_error("inspect namespace authority", error))?;
    if namespace_table && let Some(key) = namespace_key_probe() {
        let value: Option<String> = connection
            .query_row(
                "SELECT binding_json FROM worker_namespace_binding WHERE namespace_key=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| authority_error("resolve namespace authority", error))?;
        if let Some(value) = value {
            let binding: orbit_types::tool::WorkerInvocation = serde_json::from_str(&value)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            binding.validate()?;
            return Ok(Some(binding));
        }
    }
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_process_binding')", [], |row| row.get(0),
    ).map_err(|error| authority_error("inspect worker authority", error))?;
    if !exists {
        return Ok(None);
    }
    let mut pid = std::process::id();
    for _ in 0..256 {
        if let Some(identity) = orbit_common::process::identity::process_start_identity_token(pid) {
            let value: Option<String> = connection
                .query_row(
                    "SELECT binding_json FROM worker_process_binding WHERE pid=?1 AND identity=?2",
                    params![pid, identity],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| authority_error("resolve worker process", error))?;
            if let Some(value) = value {
                let binding: orbit_types::tool::WorkerInvocation = serde_json::from_str(&value)
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                binding.validate()?;
                return Ok(Some(binding));
            }
        }
        let Some(parent) = parent_of(pid)? else {
            return Ok(None);
        };
        if parent == 0 || parent == pid {
            return Ok(None);
        }
        pid = parent;
    }
    Err(OrbitError::Execution(
        "worker process ancestry limit exceeded".into(),
    ))
}

/// The identity of `pid`'s PID namespace as seen through `proc_root`: the
/// namespace link, the start time of `pid` and the boot ID. For a namespace
/// leader this names one namespace for its whole life, because the namespace
/// ends when its leader does and a recycled inode comes with a new leader.
#[cfg(target_os = "linux")]
pub(crate) fn namespace_key(proc_root: &Path, pid: u32) -> Result<String, OrbitError> {
    let process = proc_root.join(pid.to_string());
    let namespace = std::fs::read_link(process.join("ns/pid"))?;
    let stat = std::fs::read_to_string(process.join("stat"))?;
    let start = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .ok_or_else(|| {
            OrbitError::Execution("namespace leader start identity unavailable".into())
        })?;
    let boot = std::fs::read_to_string(proc_root.join("sys/kernel/random/boot_id"))?;
    Ok(format!("{}:{}:{}", namespace.display(), start, boot.trim()))
}

/// Host PID of the PID-namespace leader Bubblewrap creates beneath `root_pid`
/// (the spawned `bwrap`). Waits up to five seconds for the namespace to exist.
#[cfg(target_os = "linux")]
pub(crate) fn worker_namespace_leader(root_pid: u32) -> Result<u32, OrbitError> {
    let own = std::fs::read_link("/proc/self/ns/pid")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut pending = vec![root_pid];
        for _ in 0..256 {
            let Some(pid) = pending.pop() else { break };
            if let Ok(namespace) = std::fs::read_link(format!("/proc/{pid}/ns/pid"))
                && namespace != own
                && std::fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
                    status.lines().any(|line| {
                        line.starts_with("NSpid:") && line.split_whitespace().last() == Some("1")
                    })
                })
            {
                return Ok(pid);
            }
            if let Ok(children) =
                std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            {
                pending.extend(
                    children
                        .split_whitespace()
                        .filter_map(|value| value.parse::<u32>().ok()),
                );
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(OrbitError::PolicyDenied(
                "worker PID namespace authority unavailable".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

impl RecoveryAuthority {
    /// Bind Bubblewrap's namespace leader as observed from the host. A process
    /// inside that namespace sees this same kernel namespace, boot and start
    /// identity even though all host PIDs have disappeared from its /proc.
    #[cfg(target_os = "linux")]
    pub(crate) fn bind_worker_namespace(
        &self,
        root_pid: u32,
        binding: &orbit_types::tool::WorkerInvocation,
    ) -> Result<(), OrbitError> {
        let leader = worker_namespace_leader(root_pid)?;
        self.record_worker_namespace(&namespace_key(Path::new(PROC_ROOT), leader)?, binding)
    }

    #[cfg(target_os = "linux")]
    // Namespace persistence seam shared with the immutable-binding security fixture.
    pub(super) fn record_worker_namespace(
        &self,
        key: &str,
        binding: &orbit_types::tool::WorkerInvocation,
    ) -> Result<(), OrbitError> {
        self.connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS worker_namespace_binding (
            namespace_key TEXT PRIMARY KEY, binding_json TEXT NOT NULL
        ) WITHOUT ROWID;",
            )
            .map_err(|error| authority_error("create worker namespace table", error))?;
        let json =
            serde_json::to_string(binding).map_err(|error| OrbitError::Store(error.to_string()))?;
        self.connection.execute("INSERT INTO worker_namespace_binding(namespace_key,binding_json) VALUES (?1,?2) ON CONFLICT(namespace_key) DO NOTHING", params![key,json])
            .map_err(|error| authority_error("record worker namespace", error))?;
        let stored: String = self
            .connection
            .query_row(
                "SELECT binding_json FROM worker_namespace_binding WHERE namespace_key=?1",
                params![key],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read worker namespace", error))?;
        if stored != json {
            return Err(OrbitError::PolicyDenied(
                "worker namespace binding is immutable".into(),
            ));
        }
        Ok(())
    }
}
