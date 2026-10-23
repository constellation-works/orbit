//! The exclusive, TTL-bounded workspace claim [ADR-0352, ORB-10709].
//!
//! Workspace ownership binds a workspace to a *machine*; it does not say which
//! operator is driving it right now, and several operator sessions can reach one
//! workspace concurrently. This module is the arbitration for that: one claim
//! per workspace, held by one operator, presented as a bearer token, and a
//! precondition for governed workflow dispatch only.
//!
//! It deliberately shares `task_reservations` with the file reservations rather
//! than standing up a parallel table: acquisition atomicity, TTL, lazy expiry,
//! and a release escape hatch are already solved there. The two dimensions are
//! kept apart by [`TaskReservationScope`] rather than by path selectors — a
//! claim expressed as a whole-workspace file selector would block exactly the
//! worker reservations it is meant to leave alone.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{expire_reservations_in_scope, reservation_scope_clause};
use crate::{
    Store, TaskReservationReleaseReason, TaskReservationScope, WorkspaceClaimAcquireParams,
    WorkspaceClaimAcquireResult, WorkspaceClaimCheckParams, WorkspaceClaimCheckResult,
    WorkspaceClaimHolder, WorkspaceClaimReleaseParams, WorkspaceClaimReleaseResult,
    WorkspaceClaimStatusResult,
};

const CLAIM_ID_PREFIX: &str = "claim-";

impl Store {
    /// Take the workspace claim, or report the incumbent that refused it.
    ///
    /// The whole decision runs inside one `Immediate` transaction — expire,
    /// re-read, then insert — so two operators racing for a free workspace
    /// cannot both observe it as free. Contention never queues and never steals.
    pub fn acquire_workspace_claim(
        &self,
        params: &WorkspaceClaimAcquireParams,
    ) -> Result<WorkspaceClaimAcquireResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_claims = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::WorkspaceClaim,
            )?;

            if let Some(existing) = load_active_claim(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
            )? {
                return Ok(WorkspaceClaimAcquireResult {
                    acquired: false,
                    claim_token: None,
                    claim: None,
                    conflict: Some(existing.holder),
                    expired_claims,
                });
            }

            let claim_id = super::unique_row_id(CLAIM_ID_PREFIX);
            let claim_token = mint_claim_token();
            let expires_at =
                (Utc::now() + Duration::seconds(params.ttl_seconds as i64)).to_rfc3339();
            let diagnostics = diagnostics_json(
                params.machine_id.as_deref(),
                params.session_id.as_deref(),
            )?;

            tx.tx
                .execute(
                    "INSERT INTO task_reservations(
                        reservation_id,
                        workspace_orbit_dir,
                        workspace_id,
                        task_ids_json,
                        files_json,
                        actor,
                        created_at,
                        expires_at,
                        released_at,
                        owner_run_id,
                        owner_metadata_json,
                        release_reason,
                        release_metadata_json,
                        scope,
                        claim_token
                    ) VALUES (?1, ?2, ?3, '[]', '[]', ?4, ?5, ?6, NULL, NULL, ?7, NULL, NULL, ?8, ?9)",
                    params![
                        claim_id,
                        params.workspace_orbit_dir,
                        params.workspace_id.as_deref(),
                        params.actor,
                        now,
                        expires_at,
                        diagnostics,
                        TaskReservationScope::WorkspaceClaim.as_str(),
                        claim_token,
                    ],
                )
                .map_err(sqlite_store_error)?;

            Ok(WorkspaceClaimAcquireResult {
                acquired: true,
                claim_token: Some(claim_token),
                claim: Some(WorkspaceClaimHolder {
                    claim_id,
                    workspace_id: params.workspace_id.clone(),
                    actor: params.actor.clone(),
                    created_at: now,
                    expires_at,
                    machine_id: params.machine_id.clone(),
                    session_id: params.session_id.clone(),
                }),
                conflict: None,
                expired_claims,
            })
        })
    }

    /// Release the claim, either with its token or through the audited force
    /// escape hatch.
    ///
    /// A token release that does not match the incumbent returns
    /// `released: false` with the incumbent in `claim`, so the caller can tell
    /// "your token is stale" apart from "there was nothing to release".
    pub fn release_workspace_claim(
        &self,
        params: &WorkspaceClaimReleaseParams,
    ) -> Result<WorkspaceClaimReleaseResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_claims = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::WorkspaceClaim,
            )?;

            let Some(existing) = load_active_claim(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
            )?
            else {
                return Ok(WorkspaceClaimReleaseResult {
                    released: false,
                    forced: false,
                    released_at: None,
                    claim: None,
                    expired_claims,
                });
            };

            let token_matches = params
                .claim_token
                .as_deref()
                .is_some_and(|presented| existing.claim_token.as_deref() == Some(presented));
            if !token_matches && !params.force {
                return Ok(WorkspaceClaimReleaseResult {
                    released: false,
                    forced: false,
                    released_at: None,
                    claim: Some(existing.holder),
                    expired_claims,
                });
            }

            let forced = !token_matches;
            let released_at = crate::now_string();
            // The release record names both parties: who released it and, when
            // forced, whom they displaced. A force that cannot be attributed is
            // indistinguishable from the holder releasing its own claim.
            let release_metadata = serde_json::to_string(&serde_json::json!({
                "released_by": params.released_by,
                "forced": forced,
                "displaced_holder": existing.holder.actor,
                "displaced_claim_id": existing.holder.claim_id,
                "displaced_machine_id": existing.holder.machine_id,
                "displaced_session_id": existing.holder.session_id,
            }))
            .map_err(|error| {
                OrbitError::Store(format!(
                    "serialize workspace claim release metadata: {error}"
                ))
            })?;

            let sql = format!(
                "UPDATE task_reservations
                 SET released_at = ?4,
                     release_reason = ?5,
                     release_metadata_json = ?6,
                     claim_token = NULL
                 WHERE {}
                   AND reservation_id = ?3
                   AND released_at IS NULL",
                reservation_scope_clause(TaskReservationScope::WorkspaceClaim),
            );
            let affected = tx
                .tx
                .execute(
                    &sql,
                    params![
                        params.workspace_id.as_deref(),
                        params.workspace_orbit_dir,
                        existing.holder.claim_id,
                        released_at,
                        TaskReservationReleaseReason::Explicit.as_str(),
                        release_metadata,
                    ],
                )
                .map_err(sqlite_store_error)?;

            Ok(WorkspaceClaimReleaseResult {
                released: affected > 0,
                forced: forced && affected > 0,
                released_at: (affected > 0).then_some(released_at),
                claim: Some(existing.holder),
                expired_claims,
            })
        })
    }

    /// The active claim after lazy expiry, or `None` when unclaimed.
    pub fn show_workspace_claim(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<WorkspaceClaimStatusResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_claims = expire_reservations_in_scope(
                tx,
                workspace_orbit_dir,
                workspace_id,
                &now,
                TaskReservationScope::WorkspaceClaim,
            )?;
            let claim = load_active_claim(tx, workspace_orbit_dir, workspace_id, &now)?
                .map(|active| active.holder);
            Ok(WorkspaceClaimStatusResult {
                claim,
                expired_claims,
            })
        })
    }

    /// Whether `params.claim_token` satisfies the workspace's active claim.
    ///
    /// The comparison happens here rather than in the caller so the stored token
    /// never has to leave the store: a refusal carries the holder and the expiry
    /// instant, never the incumbent's token.
    pub fn check_workspace_claim(
        &self,
        params: &WorkspaceClaimCheckParams,
    ) -> Result<WorkspaceClaimCheckResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_claims = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::WorkspaceClaim,
            )?;
            let active = load_active_claim(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
            )?;
            let token_matches = active.as_ref().is_some_and(|active| {
                params
                    .claim_token
                    .as_deref()
                    .is_some_and(|presented| active.claim_token.as_deref() == Some(presented))
            });
            Ok(WorkspaceClaimCheckResult {
                claim: active.map(|active| active.holder),
                token_matches,
                expired_claims,
            })
        })
    }
}

/// An active claim row, with the stored token kept out of the public holder
/// projection so no caller can accidentally serialize it into a response.
struct ActiveClaim {
    holder: WorkspaceClaimHolder,
    claim_token: Option<String>,
}

fn load_active_claim(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    now: &str,
) -> Result<Option<ActiveClaim>, OrbitError> {
    let sql = format!(
        "SELECT reservation_id, workspace_id, actor, created_at, expires_at,
                owner_metadata_json, claim_token
         FROM task_reservations
         WHERE {}
           AND released_at IS NULL
           AND expires_at > ?3
         ORDER BY created_at ASC, reservation_id ASC
         LIMIT 1",
        reservation_scope_clause(TaskReservationScope::WorkspaceClaim),
    );
    let row = tx
        .tx
        .query_row(
            &sql,
            params![workspace_id, workspace_orbit_dir, now],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_store_error)?;

    let Some((claim_id, workspace_id, actor, created_at, expires_at, diagnostics, claim_token)) =
        row
    else {
        return Ok(None);
    };
    let (machine_id, session_id) = parse_diagnostics(diagnostics.as_deref());
    Ok(Some(ActiveClaim {
        holder: WorkspaceClaimHolder {
            claim_id,
            workspace_id,
            actor,
            created_at,
            expires_at,
            machine_id,
            session_id,
        },
        claim_token,
    }))
}

fn diagnostics_json(
    machine_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<String, OrbitError> {
    serde_json::to_string(&serde_json::json!({
        "machine_id": machine_id,
        "session_id": session_id,
    }))
    .map_err(|error| OrbitError::Store(format!("serialize workspace claim diagnostics: {error}")))
}

/// Diagnostics are best-effort by construction: a row written by an older
/// binary, or one whose metadata was truncated, still identifies a live holder
/// by actor and expiry, which is what a refusal needs.
fn parse_diagnostics(raw: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(value) = raw.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
    else {
        return (None, None);
    };
    let field = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    (field("machine_id"), field("session_id"))
}

/// Mint a bearer token for a freshly acquired claim.
///
/// `RandomState` is seeded from the OS, which is the entropy source available
/// here without taking a new dependency. The claim is an accident guard in the
/// same sense as the capability model — every agent on the box runs as the same
/// OS user and can bypass Orbit entirely — so the bar is "not guessable by a
/// contender", not "resistant to an attacker with the database".
fn mint_claim_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut seed = Vec::with_capacity(32);
    for salt in 0..2u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(salt);
        seed.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    seed.extend_from_slice(&nanos.to_le_bytes());
    format!("wsclaim-{}", blake3::hash(&seed).to_hex())
}
