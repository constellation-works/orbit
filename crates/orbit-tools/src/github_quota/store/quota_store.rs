#[cfg(any(test, feature = "test-support"))]
use super::clock::refusal_clock;
use super::{clock::*, record::*, types::*};
use orbit_common::ProviderReadThrottle;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;
/// The directory of host-shared quota records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubQuotaStore {
    directory: PathBuf,
    /// The monotonic instant a finite request's budget runs out. Every record
    /// lock wait is cut off there; `None` keeps the shared lock helper's default.
    deadline: Option<Instant>,
}

/// Directory under the Orbit data root.
const STORE_DIRECTORY: [&str; 2] = ["state", "github-quota"];

impl GithubQuotaStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            deadline: None,
        }
    }

    /// This store with every record lock wait bounded by `deadline`, so a
    /// contended record cannot outlast the request that asked. `None` leaves
    /// the lock helper's default wait.
    #[must_use]
    pub fn bounded_by(&self, deadline: Option<Instant>) -> Self {
        Self {
            directory: self.directory.clone(),
            deadline,
        }
    }

    /// The lock acquisition policy: the helper's default, shortened to what is
    /// left of the request's budget (none left tries the lock once).
    fn lock_options(&self) -> orbit_common::fs::io::FileLockOptions {
        let mut options = orbit_common::fs::io::FileLockOptions::default();
        if let Some(deadline) = self.deadline {
            options.timeout = options
                .timeout
                .min(deadline.saturating_duration_since(Instant::now()));
        }
        options
    }

    /// The store under the Orbit data root: `ORBIT_ROOT` when set, else the
    /// machine-global `~/.orbit`.
    pub fn in_data_root() -> Option<Self> {
        let root = match std::env::var("ORBIT_ROOT") {
            Ok(root) if !root.trim().is_empty() => PathBuf::from(root),
            _ => orbit_common::fs::path::global_orbit_dir().ok()?,
        };
        Some(Self::new(
            STORE_DIRECTORY
                .iter()
                .fold(root, |path, part| path.join(part)),
        ))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn record_path(&self, account: &str) -> PathBuf {
        self.directory.join(format!("{account}.json"))
    }

    fn clipped_release_path(&self, account: &str, generation: u64) -> PathBuf {
        self.directory
            .join(format!(".{account}.clipped.{generation}.json"))
    }

    fn is_claim_active_for_generation(&self, account: &str, generation: u64) -> bool {
        match Self::load(&self.record_path(account)) {
            Ok(record) => record.refresh_in_flight && record.refresh_generation == generation,
            Err(_) => true,
        }
    }

    /// Persist a lock-free release marker for a clipped probe whose completion
    /// lock was contended at its deadline, so the next lock holder restores the
    /// prior claim without blocking this caller.
    fn stage_clipped_release(
        &self,
        account: &str,
        generation: u64,
        previous_claim: Option<u64>,
    ) -> io::Result<()> {
        if !self.is_claim_active_for_generation(account, generation) {
            return Ok(());
        }
        let path = self.clipped_release_path(account, generation);
        let payload = serde_json::to_string(&ClippedClaimRelease {
            generation,
            previous_claim,
        })
        .map_err(io::Error::other)?;
        orbit_common::fs::io::atomic_write_text_volatile(&path, &payload)?;
        // A newer claim that raced with the lock-free write may have moved the
        // record past `generation` before the next update sees it; unlink now so
        // markers more than one generation behind cannot linger.
        if !self.is_claim_active_for_generation(account, generation) {
            let _ = std::fs::remove_file(&path);
        }
        Ok(())
    }

    fn read_clipped_release(
        &self,
        path: &Path,
        expected_generation: u64,
    ) -> Option<ClippedClaimRelease> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    path = %path.display(),
                    "staged clipped quota release marker unreadable");
                return None;
            }
        };
        let release = match serde_json::from_str::<ClippedClaimRelease>(&text) {
            Ok(release) => release,
            Err(error) => {
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    path = %path.display(),
                    "staged clipped quota release marker malformed; preserving it");
                return None;
            }
        };
        if release.generation != expected_generation {
            tracing::warn!(target: "orbit_tools::github_quota",
                marker_generation = release.generation,
                active_generation = expected_generation,
                path = %path.display(),
                "staged clipped quota release marker ownership is ambiguous; preserving it");
            return None;
        }
        Some(release)
    }

    /// Reconcile staged clipped-claim releases under the exclusive lock, returning
    /// the marker paths to unlink after the updated record is committed. A marker
    /// is unlinked only when its payload decodes as a release for its own
    /// generation: the active generation's release also settles its claim, and
    /// the previous generation's is superseded. Malformed or generation-mismatched
    /// bytes stay on disk for diagnosis.
    ///
    /// Accesses known generation paths directly by name so reconciliation stays
    /// O(1) independent of unrelated directory entries and succeeds when the
    /// quota directory denies listing.
    fn reconcile_clipped_releases(&self, account: &str, record: &mut QuotaRecord) -> Vec<PathBuf> {
        let mut markers = Vec::new();
        if record.refresh_generation > 1 {
            let superseded = record.refresh_generation - 1;
            let path = self.clipped_release_path(account, superseded);
            if self.read_clipped_release(&path, superseded).is_some() {
                markers.push(path);
            }
        }
        if record.refresh_generation == 0 {
            return markers;
        }
        let path = self.clipped_release_path(account, record.refresh_generation);
        let Some(release) = self.read_clipped_release(&path, record.refresh_generation) else {
            return markers;
        };
        if record.refresh_in_flight {
            release_clipped_claim(record, release.previous_claim);
        }
        markers.push(path);
        markers
    }

    /// Read `path`, distinguishing a missing record (`NotFound`, initialized as
    /// empty) from an existing unreadable or malformed record (returned so
    /// callers preserve the bytes on disk).
    fn load(path: &Path) -> io::Result<QuotaRecord> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(QuotaRecord::default());
            }
            Err(error) => return Err(error),
        };
        serde_json::from_str(&text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed quota record JSON: {error}"),
            )
        })
    }

    /// Read-modify-write `account`'s record under its exclusive lock, waiting for it
    /// no longer than the store's deadline.
    fn update<T>(
        &self,
        account: &str,
        change: impl FnOnce(&mut QuotaRecord) -> T,
    ) -> io::Result<T> {
        let path = self.record_path(account);
        orbit_common::fs::io::create_private_dir_all(&self.directory)?;
        orbit_common::fs::io::with_exclusive_file_lock_options(
            &path,
            "github quota record",
            self.lock_options(),
            || {
                let mut record = Self::load(&path)?;
                let markers = self.reconcile_clipped_releases(account, &mut record);
                let output = change(&mut record);
                let text = serde_json::to_string(&record).map_err(io::Error::other)?;
                orbit_common::fs::io::atomic_write_text_volatile(&path, &text)?;
                for marker in markers {
                    let _ = std::fs::remove_file(marker);
                }
                Ok(output)
            },
        )
    }

    /// Admit or refuse one read of `resource` by `caller`, first refreshing
    /// the record through `refresh` when no process on the host has read the
    /// rate-limit endpoint within the policy's interval. An admitted read is
    /// counted against its class and the recorded remaining count.
    pub fn admit(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        caller: GithubCaller,
        resource: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) -> Result<(), QuotaRefusal> {
        self.admit_charging(policy, account, caller, resource, now, refresh, true)
    }

    /// [`Self::admit`] for a caller that never spawns its protected call once
    /// the store's deadline has passed: a read admitted at or after that
    /// deadline is not counted or debited, since no call follows it. A refusal
    /// is returned uncounted: the caller delivers it only while its budget
    /// remains and then records it with [`Self::record_refusal`], so a deadline
    /// crossed between the decision and delivery leaves no refusal behind.
    pub fn admit_unless_expired(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        caller: GithubCaller,
        resource: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) -> Result<(), QuotaRefusal> {
        self.admit_charging(policy, account, caller, resource, now, refresh, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_charging(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        caller: GithubCaller,
        resource: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
        charge_expired: bool,
    ) -> Result<(), QuotaRefusal> {
        self.refresh_if_due(policy, account, now, refresh);
        let decided = self.update(account, |record| {
            let refresh_in_flight = record.refresh_in_flight;
            let entry = record.resources.entry(resource.to_string()).or_default();
            entry.roll_window(now);
            let refusal = entry.refusal(policy, caller, resource, now);
            if let Some(refusal) = refusal {
                // A deadline-bound caller counts the refusal itself once it
                // knows the budget survived to deliver it.
                if charge_expired {
                    entry.refused.bump(caller);
                }
                return Err(refusal);
            }
            // The deadline is read under the record lock, after the refusal is
            // evaluated, so a read that ran out of budget while waiting for it,
            // probing or deciding is not charged: its caller returns the
            // timeout instead.
            if !charge_expired && self.deadline.is_some_and(budget_spent) {
                return Ok(());
            }
            entry.calls.bump(caller);
            if refresh_in_flight {
                entry.in_flight_debits = entry.in_flight_debits.saturating_add(1);
            }
            if entry.current(now).is_some() {
                entry.remaining = entry.remaining.map(|left| left.saturating_sub(1));
            }
            Ok(())
        });
        #[cfg(any(test, feature = "test-support"))]
        if matches!(decided, Ok(Err(_))) {
            refusal_clock::after_refusal_decision();
        }
        match decided {
            Ok(decision) => decision,
            Err(error) => {
                // A process that may read but not write the record (a
                // sandboxed lane) still honours it, uncounted. One that cannot
                // read it preserves the file and admits the read.
                match Self::load(&self.record_path(account)) {
                    Ok(record) => {
                        tracing::warn!(target: "orbit_tools::github_quota", %error,
                            "GitHub quota record not writable; deciding from a read-only copy");
                        match record.resources.get(resource) {
                            Some(entry) => match entry.refusal(policy, caller, resource, now) {
                                Some(refusal) => Err(refusal),
                                None => Ok(()),
                            },
                            None => Ok(()),
                        }
                    }
                    Err(read_error) => {
                        tracing::warn!(target: "orbit_tools::github_quota", error = %read_error,
                            "GitHub quota record unreadable; preserving record and admitting read");
                        Ok(())
                    }
                }
            }
        }
    }

    /// Count one read of `resource` by `caller` that [`Self::admit_unless_expired`]
    /// refused and its caller is delivering. An unwritable record counts
    /// nothing, as for any refusal the store decides from a read-only copy.
    pub fn record_refusal(&self, account: &str, caller: GithubCaller, resource: &str, now: u64) {
        let counted = self.update(account, |record| {
            let entry = record.resources.entry(resource.to_string()).or_default();
            entry.roll_window(now);
            entry.refused.bump(caller);
        });
        if let Err(error) = counted {
            tracing::warn!(target: "orbit_tools::github_quota", %error,
                "GitHub quota record unavailable; refusal not counted");
        }
    }

    /// Read the rate-limit endpoint through `refresh` when no process on the
    /// host has within the interval. The claim is taken under the record's
    /// lock before the read, so concurrent callers refresh at most once, and a
    /// caller whose sampled `now` is older than the active claim never moves
    /// it backward.
    pub fn refresh_if_due(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) {
        let interval = policy.refresh_seconds();
        let mut previous_claim = None;
        let claim = match self.update(account, |record| {
            // `now` was sampled before the lock, so a caller that waited
            // behind a later claimant can hold an older time: that is
            // inside the claim's interval, not due. Only a claim further
            // ahead than one interval (a clock stepped back) is stale.
            let due = record.refresh_claimed_at.is_none_or(|at| {
                if now < at {
                    at - now > interval
                } else {
                    now - at >= interval
                }
            });
            if due {
                previous_claim = record.refresh_claimed_at;
                record.refresh_claimed_at = Some(now);
                record.refresh_generation = record.refresh_generation.wrapping_add(1);
                record.refresh_in_flight = true;
                for entry in record.resources.values_mut() {
                    entry.in_flight_debits = 0;
                }
                Some(record.refresh_generation)
            } else {
                None
            }
        }) {
            Ok(claim) => claim,
            Err(error) => {
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    "GitHub quota record unavailable; skipping refresh claim");
                return;
            }
        };
        let Some(claim_generation) = claim else {
            return;
        };
        let answer = refresh();
        let clipped = matches!(answer, RateLimitAnswer::Clipped(_));
        if let Err(error) = self.update(account, |record| {
            apply_answer(record, answer, now, claim_generation, previous_claim)
        }) {
            if clipped {
                if let Err(stage_error) =
                    self.stage_clipped_release(account, claim_generation, previous_claim)
                {
                    tracing::warn!(target: "orbit_tools::github_quota", error = %stage_error,
                        lock_error = %error,
                        "GitHub quota record unavailable; clipped refresh claim release not persisted");
                }
            } else {
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    "GitHub quota record unavailable; refresh answer not persisted");
            }
        }
    }

    /// The latest reset among `resources` that the record shows exhausted
    /// (a current observation with nothing remaining, which a primary
    /// rate-limit answer also leaves), after [`Self::refresh_if_due`] through
    /// `refresh`. A caller that needs a reset therefore shares the interval
    /// with admission instead of reading the endpoint itself. `None` when the
    /// record shows none exhausted or the quota is unknown.
    pub fn exhausted_reset(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        resources: &[&str],
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) -> Option<u64> {
        self.refresh_if_due(policy, account, now, refresh);
        let record = match Self::load(&self.record_path(account)) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    "GitHub quota record unreadable; treating exhausted reset as unknown");
                return None;
            }
        };
        resources
            .iter()
            .filter_map(|name| record.resources.get(*name)?.current(now))
            .filter(|(_, remaining, _)| *remaining == 0)
            .map(|(_, _, reset)| reset)
            .max()
    }

    /// Record an authenticated primary rate-limit answer for `resource`:
    /// every read of it on the host is held until the reset GitHub named,
    /// else its `Retry-After`, else the recorded reset, else one refresh
    /// interval.
    pub fn record_throttle(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        resource: &str,
        throttle: &ProviderReadThrottle,
        now: u64,
    ) {
        if let Err(error) = self.update(account, |record| {
            let entry = record.resources.entry(resource.to_string()).or_default();
            let until = throttle
                .reset_epoch_seconds
                .filter(|reset| *reset > now)
                .or_else(|| throttle.retry_after_seconds.map(|wait| now + wait))
                .or_else(|| entry.reset_epoch_seconds.filter(|reset| *reset > now))
                .unwrap_or(now + policy.refresh_seconds());
            entry.held_until = Some(entry.held_until.map_or(until, |held| held.max(until)));
            if entry.limit.is_some() {
                entry.remaining = Some(0);
            }
        }) {
            tracing::warn!(target: "orbit_tools::github_quota", %error,
                "GitHub quota record unavailable; throttle hold not persisted");
        }
    }

    /// The operator's view of `account`'s record at `now`.
    pub fn status(&self, policy: &GithubQuotaPolicy, account: &str, now: u64) -> GithubQuotaStatus {
        let (record, load_error) = match Self::load(&self.record_path(account)) {
            Ok(record) => (record, None),
            Err(error) => (
                QuotaRecord::default(),
                Some(bounded_error(&format!("quota record unreadable: {error}"))),
            ),
        };
        let resources = TRACKED_RESOURCES
            .iter()
            .map(|name| {
                let mut entry = record.resources.get(*name).cloned().unwrap_or_default();
                if entry.window_ends_at.is_some_and(|end| now >= end) {
                    entry.calls = CallCounts::default();
                    entry.refused = CallCounts::default();
                }
                let current = entry.current(now);
                let refused = |caller| entry.refusal(policy, caller, name, now).is_some();
                GithubResourceStatus {
                    resource: (*name).to_string(),
                    state: if current.is_some() {
                        "known"
                    } else {
                        "unknown"
                    },
                    limit: current.map(|(limit, _, _)| limit),
                    remaining: current.map(|(_, remaining, _)| remaining),
                    reset_at: current.and_then(|(_, _, reset)| rfc3339(reset)),
                    observed_at: entry.observed_at.and_then(rfc3339),
                    held_until: entry.active_hold(now).and_then(rfc3339),
                    agent_reads_refused: refused(GithubCaller::Agent),
                    background_reads_refused: refused(GithubCaller::Background),
                    delivery_reads_refused: refused(GithubCaller::Delivery),
                    calls: entry.calls,
                    refused: entry.refused,
                }
            })
            .collect();
        GithubQuotaStatus {
            account: account.to_string(),
            refreshed_at: record.refreshed_at.and_then(rfc3339),
            refresh_error: load_error.or(record.refresh_error),
            agent_reserve_percent: policy.agent_reserve_percent,
            background_reserve_percent: policy.background_reserve_percent,
            resources,
        }
    }
}
