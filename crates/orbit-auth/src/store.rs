//! The lease token store: `lease-tokens.db` in the listener user's Orbit home.
//!
//! The store holds each token's SHA-256, never its secret, with its scope,
//! timestamps and an audit trail of mints, revocations and refused
//! verifications. It is opened through `orbit-common`'s SQLite support and is
//! never created here: root provisions it, `0600` in a `0700` Orbit home,
//! when it creates the listener user. A user without one (the operator, say)
//! finds no store and is told to run the command as the listener user, so no
//! store ever appears in the operator's home. Opening refuses a store or home
//! that is group- or world-accessible or owned by another user.

mod schema;

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::{open_private, sqlite_store_error};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::environment::{LISTENER_USER, check_listener_execution_environment};
use crate::scope::{LeaseTokenRecord, LeaseTokenRefusal, LeaseTokenScope, LeaseTokenVerb};
use crate::token::{
    LeaseTokenSecret, generate_lease_token_id, generate_lease_token_secret,
    is_lease_token_secret_shape, lease_token_digest,
};

/// File name of the token store in the running user's Orbit home.
pub const LEASE_TOKEN_STORE_FILE: &str = "lease-tokens.db";

const TOKEN_COLUMNS: &str = "token_id, principal, principal_class, workspaces, verbs, \
     max_priority, hard, created_at, expires_at, last_used_at, revoked_at";

/// A newly minted token. The secret is here once, to show to the operator; the
/// store has only its hash.
#[derive(Debug)]
pub struct MintedLeaseToken {
    /// The stored row.
    pub record: LeaseTokenRecord,
    /// The secret, shown once.
    pub secret: LeaseTokenSecret,
}

/// What revoking a token did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseTokenRevocation {
    /// The token was active and is now revoked.
    Revoked(LeaseTokenRecord),
    /// The token had already been revoked; nothing changed.
    AlreadyRevoked(LeaseTokenRecord),
}

/// The decision for a presented token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseTokenDecision {
    /// The token is valid for the request; here are its principal and scope.
    Allow(LeaseTokenRecord),
    /// The token is refused, and the refusal has been audited.
    Deny(LeaseTokenRefusal),
}

/// An open lease token store.
pub struct LeaseTokenStore {
    connection: Connection,
}

impl LeaseTokenStore {
    /// Run the execution-environment check, then open the store in the
    /// running user's Orbit home. Every listener-user surface opens it this
    /// way.
    pub fn open_for_listener_user() -> Result<Self, OrbitError> {
        let orbit_home = orbit_common::fs::path::global_orbit_dir()?;
        check_listener_execution_environment(&orbit_home)?;
        Self::open_in_orbit_home(&orbit_home)
    }

    /// Open the store in `orbit_home` without the execution-environment
    /// check; the home and store ownership and mode checks still apply.
    pub fn open_in_orbit_home(orbit_home: &Path) -> Result<Self, OrbitError> {
        let path = orbit_home.join(LEASE_TOKEN_STORE_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(no_store_here(&path));
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "cannot inspect {}: {error}",
                    path.display()
                )));
            }
        }
        private_access::require_owner_only(orbit_home, true)?;
        private_access::require_owner_only(&path, false)?;
        let opened = open_private(&path)?;
        if opened.read_only {
            return Err(OrbitError::PolicyDenied(format!(
                "the lease token store {} is read-only; it must be writable by the {LISTENER_USER} \
                 user",
                path.display()
            )));
        }
        let mut connection = opened.connection;
        schema::migrate_lease_token_store(&mut connection)?;
        Ok(Self { connection })
    }

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
                origin: None,
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
                origin: None,
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
        self.verify_with_origin(secret, Some(workspace), verb, now, None)
    }

    /// [`verify`](Self::verify) for a listener request from `origin`.
    ///
    /// `workspace` is `None` when the request names none: the token's only
    /// workspace is used, and a token with several is refused as
    /// [`LeaseTokenRefusal::WorkspaceUnnamed`]. A refusal's audit row also
    /// carries the request's remote address and route.
    pub fn verify_listener_request(
        &mut self,
        secret: &str,
        workspace: Option<&str>,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
        origin: &LeaseRequestOrigin,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        self.verify_with_origin(secret, workspace, verb, now, Some(origin))
    }

    fn verify_with_origin(
        &mut self,
        secret: &str,
        workspace: Option<&str>,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
        origin: Option<&LeaseRequestOrigin>,
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
                        origin,
                    },
                )?;
                Ok(LeaseTokenDecision::Deny(refusal))
            }
        }
    }

    /// Audit a listener refusal that no token decision covers: a missing
    /// token, an unresolvable client address, a rate limit, or a request
    /// refused after authentication without reaching Orbit.
    pub fn record_listener_refusal(
        &mut self,
        refusal: &LeaseListenerRefusal<'_>,
    ) -> Result<(), OrbitError> {
        insert_audit(
            &self.connection,
            &AuditRow {
                at: refusal.at,
                event: "listener_refused",
                token_id: refusal.token_id,
                principal: refusal.principal,
                workspace: refusal.workspace,
                verb: refusal.verb.map(LeaseTokenVerb::as_str),
                reason: Some(refusal.reason),
                actor: None,
                origin: Some(refusal.origin),
            },
        )
    }
}

/// Where a listener request came from, for its audit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRequestOrigin {
    /// The resolved client address, or the TCP peer when none resolved.
    pub remote_address: String,
    /// Method and path, such as `POST /api/leases`.
    pub route: String,
}

/// A listener refusal to audit. It never carries a secret.
#[derive(Debug, Clone)]
pub struct LeaseListenerRefusal<'a> {
    /// When it was refused.
    pub at: DateTime<Utc>,
    /// The request.
    pub origin: &'a LeaseRequestOrigin,
    /// Stable reason, such as `missing_token` or `rate_limited`.
    pub reason: &'a str,
    /// The authenticated token's id, when known.
    pub token_id: Option<&'a str>,
    /// The authenticated token's principal, when known.
    pub principal: Option<&'a str>,
    /// The workspace the request addressed, when known.
    pub workspace: Option<&'a str>,
    /// The verb the request needed, when known.
    pub verb: Option<LeaseTokenVerb>,
}

/// Who is running a token command, for the audit row: the `sudo` caller when
/// there is one, else the running user. Attribution only, not authority.
pub fn lease_token_actor() -> String {
    let named = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    match (named("SUDO_USER"), named("USER")) {
        (Some(sudo_user), Some(user)) => format!("{sudo_user} as {user}"),
        (Some(sudo_user), None) => sudo_user,
        (None, Some(user)) => user,
        (None, None) => "unknown".to_string(),
    }
}

fn no_store_here(path: &Path) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "no lease token store at {}: lease token commands run as the lease listener user \
         ({LISTENER_USER} by convention), whose Orbit home holds the store. Run them as that \
         user, for example `sudo -H -u {LISTENER_USER} orbit lease token list`. Setting up the \
         user and its store is described in the orbit-setup skill's remote-access reference.",
        PathBuf::from(path).display()
    ))
}

struct AuditRow<'a> {
    at: DateTime<Utc>,
    event: &'a str,
    token_id: Option<&'a str>,
    principal: Option<&'a str>,
    workspace: Option<&'a str>,
    verb: Option<&'a str>,
    reason: Option<&'a str>,
    actor: Option<&'a str>,
    origin: Option<&'a LeaseRequestOrigin>,
}

fn insert_audit(connection: &Connection, row: &AuditRow<'_>) -> Result<(), OrbitError> {
    connection
        .execute(
            "INSERT INTO lease_token_audit \
             (at, event, token_id, principal, workspace, verb, reason, actor, remote_address, \
             route) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp(row.at),
                row.event,
                row.token_id,
                row.principal,
                row.workspace,
                row.verb,
                row.reason,
                row.actor,
                row.origin.map(|origin| origin.remote_address.as_str()),
                row.origin.map(|origin| origin.route.as_str()),
            ],
        )
        .map(|_| ())
        .map_err(sqlite_store_error)
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn joined_verbs(verbs: &[LeaseTokenVerb]) -> String {
    verbs
        .iter()
        .map(|verb| verb.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// A row read from `lease_tokens`; decoding failures surface as a corrupt
/// store rather than a SQLite error.
type DecodedRow = Result<LeaseTokenRecord, OrbitError>;

fn token_from_row(row: &Row<'_>) -> rusqlite::Result<DecodedRow> {
    token_from_offset_row(row, 0)
}

fn token_from_offset_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<DecodedRow> {
    let text = |index: usize| row.get::<_, String>(offset + index);
    let optional_text = |index: usize| row.get::<_, Option<String>>(offset + index);
    let stored = StoredToken {
        token_id: text(0)?,
        principal: text(1)?,
        principal_class: text(2)?,
        workspaces: text(3)?,
        verbs: text(4)?,
        max_priority: text(5)?,
        hard: text(6)?,
        created_at: text(7)?,
        expires_at: text(8)?,
        last_used_at: optional_text(9)?,
        revoked_at: optional_text(10)?,
    };
    Ok(stored.decode())
}

/// A `lease_tokens` row as stored text.
struct StoredToken {
    token_id: String,
    principal: String,
    principal_class: String,
    workspaces: String,
    verbs: String,
    max_priority: String,
    hard: String,
    created_at: String,
    expires_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

impl StoredToken {
    fn decode(self) -> DecodedRow {
        let token_id = self.token_id;
        let corrupt = |detail: String| {
            OrbitError::Store(format!(
                "lease token store row {token_id} is unreadable: {detail}"
            ))
        };
        let parse_time = |raw: &str| {
            DateTime::parse_from_rfc3339(raw)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|error| corrupt(format!("timestamp '{raw}': {error}")))
        };
        let scope = LeaseTokenScope {
            principal: self.principal,
            principal_class: self
                .principal_class
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
            workspaces: self.workspaces.split(',').map(str::to_string).collect(),
            verbs: self
                .verbs
                .split(',')
                .map(str::parse)
                .collect::<Result<_, OrbitError>>()
                .map_err(|error| corrupt(error.to_string()))?,
            max_priority: self
                .max_priority
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
            hard: self
                .hard
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
        };
        let created_at = parse_time(&self.created_at)?;
        let expires_at = parse_time(&self.expires_at)?;
        let last_used_at = self.last_used_at.as_deref().map(parse_time).transpose()?;
        let revoked_at = self.revoked_at.as_deref().map(parse_time).transpose()?;
        Ok(LeaseTokenRecord {
            token_id,
            scope,
            created_at,
            expires_at,
            last_used_at,
            revoked_at,
        })
    }
}

#[cfg(unix)]
mod private_access {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use orbit_common::OrbitError;

    /// Refuse a store or home that is not the running user's alone.
    pub(super) fn require_owner_only(path: &Path, directory: bool) -> Result<(), OrbitError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            OrbitError::Io(format!("cannot inspect {}: {error}", path.display()))
        })?;
        let kind_ok = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !kind_ok {
            return Err(refused(
                path,
                if directory {
                    "is not a directory"
                } else {
                    "is not a regular file"
                },
            ));
        }
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            return Err(refused(path, "is owned by another user"));
        }
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(OrbitError::PolicyDenied(format!(
                "refusing the lease token store: {} has mode {mode:03o}, which other users can \
                 access; it must be {}",
                path.display(),
                if directory { "0700" } else { "0600" }
            )));
        }
        Ok(())
    }

    fn refused(path: &Path, why: &str) -> OrbitError {
        OrbitError::PolicyDenied(format!(
            "refusing the lease token store: {} {why}",
            path.display()
        ))
    }
}

#[cfg(not(unix))]
mod private_access {
    use std::path::Path;

    use orbit_common::OrbitError;

    pub(super) fn require_owner_only(_path: &Path, _directory: bool) -> Result<(), OrbitError> {
        Err(OrbitError::PolicyDenied(
            "the lease token store needs a Unix host".to_string(),
        ))
    }
}
