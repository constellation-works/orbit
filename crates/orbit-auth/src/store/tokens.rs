//! Lease token creation, listing, revocation, verification and limit audits.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::scope::{LeaseTokenRecord, LeaseTokenRefusal, LeaseTokenScope, LeaseTokenVerb};
use crate::token::{
    generate_lease_token_id, generate_lease_token_secret, is_lease_token_secret_shape,
    lease_token_digest,
};

use super::audit::{AuditRow, audit_text, insert_audit, joined_verbs, timestamp};
use super::rows::{token_from_offset_row, token_from_row};
use super::{
    LeaseTokenDecision, LeaseTokenRevocation, LeaseTokenStore, MintedLeaseToken, TOKEN_COLUMNS,
};

impl LeaseTokenStore {
    /// Mint a token for `scope` that expires at `expires_at`. `actor` names
    /// who minted it in the audit row.
    pub fn mint(
        &mut self,
        scope: LeaseTokenScope,
        expires_at: DateTime<Utc>,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<MintedLeaseToken, OrbitError> {
        let scope = scope.validated()?;
        if expires_at <= now {
            return Err(OrbitError::InvalidInput(
                "a lease token must expire after it is minted".to_string(),
            ));
        }
        let secret = generate_lease_token_secret()?;
        let digest = secret.digest();
        let record = LeaseTokenRecord {
            token_id: generate_lease_token_id()?,
            scope,
            created_at: now,
            expires_at,
            last_used_at: None,
            revoked_at: None,
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_store_error)?;
        transaction
            .execute(
                "INSERT INTO lease_tokens (token_id, token_sha256, principal, principal_class, \
                 workspaces, verbs, max_priority, hard, created_at, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.token_id,
                    digest.as_bytes().as_slice(),
                    record.scope.principal,
                    record.scope.principal_class.as_str(),
                    record.scope.workspaces.join(","),
                    joined_verbs(&record.scope.verbs),
                    record.scope.max_priority.as_str(),
                    record.scope.hard.as_str(),
                    timestamp(now),
                    timestamp(expires_at),
                ],
            )
            .map_err(sqlite_store_error)?;
        insert_audit(
            &transaction,
            &AuditRow {
                at: now,
                event: "mint",
                token_id: Some(&record.token_id),
                principal: Some(&record.scope.principal),
                workspace: None,
                verb: None,
                reason: None,
                actor: Some(actor),
            },
        )?;
        transaction.commit().map_err(sqlite_store_error)?;
        Ok(MintedLeaseToken { record, secret })
    }

    /// Every token, oldest first, without secrets or hashes.
    pub fn list(&self) -> Result<Vec<LeaseTokenRecord>, OrbitError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {TOKEN_COLUMNS} FROM lease_tokens ORDER BY created_at, token_id"
            ))
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map([], token_from_row)
            .map_err(sqlite_store_error)?;
        rows.map(|row| row.map_err(sqlite_store_error)?).collect()
    }

    /// Revoke `token_id`. It is refused on its next verification.
    pub fn revoke(
        &mut self,
        token_id: &str,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<LeaseTokenRevocation, OrbitError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_store_error)?;
        let Some(record) = transaction
            .query_row(
                &format!("SELECT {TOKEN_COLUMNS} FROM lease_tokens WHERE token_id = ?1"),
                [token_id],
                token_from_row,
            )
            .optional()
            .map_err(sqlite_store_error)?
            .transpose()?
        else {
            return Err(OrbitError::InvalidInput(format!(
                "no lease token '{token_id}'; `orbit lease token list` shows the ids"
            )));
        };
        if record.revoked_at.is_some() {
            return Ok(LeaseTokenRevocation::AlreadyRevoked(record));
        }
        transaction
            .execute(
                "UPDATE lease_tokens SET revoked_at = ?1 WHERE token_id = ?2",
                params![timestamp(now), token_id],
            )
            .map_err(sqlite_store_error)?;
        insert_audit(
            &transaction,
            &AuditRow {
                at: now,
                event: "revoke",
                token_id: Some(token_id),
                principal: Some(&record.scope.principal),
                workspace: None,
                verb: None,
                reason: None,
                actor: Some(actor),
            },
        )?;
        transaction.commit().map_err(sqlite_store_error)?;
        Ok(LeaseTokenRevocation::Revoked(LeaseTokenRecord {
            revoked_at: Some(now),
            ..record
        }))
    }

    /// Decide whether `secret` may use `verb` on `workspace` at `now`.
    ///
    /// The token is looked up by the secret's SHA-256 and the stored hash is
    /// compared in constant time. An allowed use records `last_used_at`; a
    /// refusal is audited with its reason, never with the secret.
    pub fn verify(
        &mut self,
        secret: &str,
        workspace: &str,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        self.verify_lease_token_request(secret, Some(workspace), verb, now, true)
    }

    fn verify_lease_token_request(
        &mut self,
        secret: &str,
        workspace: Option<&str>,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
        audit_refusal: bool,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        let found = if is_lease_token_secret_shape(secret) {
            let digest = lease_token_digest(secret);
            self.connection
                .query_row(
                    &format!(
                        "SELECT token_sha256, {TOKEN_COLUMNS} FROM lease_tokens \
                         WHERE token_sha256 = ?1"
                    ),
                    [digest.as_bytes().as_slice()],
                    |row| {
                        let stored: Vec<u8> = row.get(0)?;
                        Ok((stored, token_from_offset_row(row, 1)?))
                    },
                )
                .optional()
                .map_err(sqlite_store_error)?
                .filter(|(stored, _)| digest.matches_stored(stored))
                .map(|(_, record)| record)
                .transpose()?
        } else {
            None
        };
        let decision = match (&found, workspace) {
            (None, _) => Err(LeaseTokenRefusal::Unknown),
            (Some(record), Some(workspace)) => record.decide(workspace, verb, now),
            (Some(record), None) => match record.scope.workspaces.as_slice() {
                [only] => record.decide(only, verb, now),
                _ if record.revoked_at.is_some() => Err(LeaseTokenRefusal::Revoked),
                _ if record.expires_at <= now => Err(LeaseTokenRefusal::Expired),
                _ => Err(LeaseTokenRefusal::WorkspaceUnnamed),
            },
        };
        match decision {
            Ok(()) => {
                let record = found.ok_or_else(|| {
                    OrbitError::Execution("allowed lease token has no record".to_string())
                })?;
                self.connection
                    .execute(
                        "UPDATE lease_tokens SET last_used_at = ?1 WHERE token_id = ?2",
                        params![timestamp(now), record.token_id],
                    )
                    .map_err(sqlite_store_error)?;
                Ok(LeaseTokenDecision::Allow(LeaseTokenRecord {
                    last_used_at: Some(now),
                    ..record
                }))
            }
            Err(refusal) => {
                if audit_refusal {
                    insert_audit(
                        &self.connection,
                        &AuditRow {
                            at: now,
                            event: "verify_refused",
                            token_id: found.as_ref().map(|record| record.token_id.as_str()),
                            principal: found.as_ref().map(|record| record.scope.principal.as_str()),
                            workspace,
                            verb: Some(verb.as_str()),
                            reason: Some(refusal.as_str()),
                            actor: None,
                        },
                    )?;
                }
                Ok(LeaseTokenDecision::Deny(refusal))
            }
        }
    }

    /// Authenticate for a rate-limited surface that coalesces its own refusals.
    /// The surface must record every denial with the keyed limiter.
    pub fn verify_listener_coalesced(
        &mut self,
        secret: &str,
        workspace: Option<&str>,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        self.verify_lease_token_request(secret, workspace, verb, now, false)
    }

    /// Persist a bounded batch of aggregate snapshots, updating each window's
    /// existing row rather than appending one row per refused request.
    pub fn record_limit_audits(
        &mut self,
        rows: &[crate::limiter::LimitAudit],
    ) -> Result<(), OrbitError> {
        let transaction = self.connection.transaction().map_err(sqlite_store_error)?;
        for row in rows {
            transaction.execute(
                "INSERT INTO lease_listener_limit_audit (limit_key, limit_name, first_at, last_at, count)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (limit_key, limit_name, first_at) DO UPDATE
                 SET last_at = max(last_at, excluded.last_at), count = max(count, excluded.count)",
                params![audit_text(&row.key), row.limit,
                    row.first_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
                    row.last_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
                    row.count.min(i64::MAX as u64) as i64],
            ).map_err(sqlite_store_error)?;
        }
        transaction.commit().map_err(sqlite_store_error)
    }
}
