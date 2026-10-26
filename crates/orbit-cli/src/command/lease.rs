//! Registry-derived path-lease CLI with standard structured and human output,
//! plus `orbit lease token`, the lease listener user's token commands.

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenRecord, LeaseTokenScope, LeaseTokenVerb,
    PrincipalClass,
};
use orbit_auth::store::{LeaseTokenRevocation, LeaseTokenStore, lease_token_actor};
use orbit_common::governance::lease::LeaseVerb;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::Path;

use super::operation_args::Invocation;
use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::lease::LEASE.cli_about)]
pub struct LeaseCommand {
    #[command(subcommand)]
    pub command: LeaseSubcommand,
}

#[derive(Subcommand)]
pub enum LeaseSubcommand {
    // Request, renew, release, show, list and check, derived from the lease
    // operation registry.
    #[command(flatten)]
    Operation(Invocation<LeaseVerb>),
    /// Mint, list and revoke lease listener tokens (run as the listener user)
    #[command(long_about = LEASE_TOKEN_LONG_ABOUT)]
    Token(LeaseTokenCommand),
}

impl LeaseCommand {
    /// Whether this is `orbit lease token …`, which opens no workspace runtime
    /// and pins no executable generation: it runs as the lease listener user
    /// against that user's own token store.
    pub fn is_token_command(&self) -> bool {
        matches!(self.command, LeaseSubcommand::Token(_))
    }

    /// Run a token command. `workspace_selector` is the global
    /// `--workspace`, which scopes a new token.
    pub fn execute_without_runtime(
        self,
        workspace_selector: Option<&str>,
        root_override: Option<&Path>,
    ) -> CommandOut {
        match self.command {
            LeaseSubcommand::Token(command) => {
                command.execute_as_listener_user(workspace_selector, root_override)
            }
            LeaseSubcommand::Operation(_) => Err(OrbitError::Execution(
                "lease operations need a workspace runtime".to_string(),
            )),
        }
    }
}

impl Execute for LeaseCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let invocation = match self.command {
            LeaseSubcommand::Operation(invocation) => invocation,
            LeaseSubcommand::Token(_) => {
                return Err(OrbitError::Execution(
                    "lease token commands run without a workspace runtime".to_string(),
                ));
            }
        };
        let Invocation { spec, mut input } = invocation;
        if let Some(raw) = input
            .remove("ttl")
            .and_then(|value| value.as_str().map(str::to_owned))
        {
            input.insert(
                "ttl_seconds".into(),
                json!(super::locks::reservation_ttl_seconds(&raw)?),
            );
        }
        if input.remove("hard").and_then(|value| value.as_bool()) == Some(true) {
            input.insert("mode".into(), json!("hard"));
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let exit_code = if spec.verb == LeaseVerb::Request && value["granted"] == false {
            3
        } else {
            0
        };
        let blocks = match spec.verb {
            LeaseVerb::Request if value["granted"] == false => vec![
                Block::text("Lease refused: overlapping holders"),
                Block::table(lease_holders_table(&value["conflicts"])),
            ],
            LeaseVerb::Request | LeaseVerb::Renew | LeaseVerb::Show => {
                let mut blocks = vec![Block::text(lease_detail(&value["lease"]))];
                if let Some(notice) = value["notice"].as_str() {
                    blocks.push(Block::text(notice));
                }
                if value["overlaps"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
                {
                    blocks.push(Block::table(lease_holders_table(&value["overlaps"])));
                }
                blocks
            }
            LeaseVerb::Release => vec![Block::text(if value["released"] == true {
                format!("Released {}", lease_text(&value["lease"]["lease_id"]))
            } else {
                "Lease was unknown or already ended".into()
            })],
            LeaseVerb::List => {
                let mut table = Table::new(vec![
                    Column::new("LEASE").fixed(),
                    Column::new("MODE").fixed(),
                    Column::new("PRINCIPAL").fixed(),
                    Column::new("PRIORITY").fixed(),
                    Column::new("EXPIRES").fixed(),
                    Column::new("SELECTORS"),
                ])
                .keep_all_columns()
                .empty_message("no active leases");
                for lease in value["leases"].as_array().into_iter().flatten() {
                    table.add_row(vec![
                        lease_text(&lease["lease_id"]),
                        lease_text(&lease["mode"]),
                        lease_text(&lease["principal"]),
                        lease_text(&lease["priority"]),
                        lease_text(&lease["expires_at"]),
                        lease_selectors(&lease["selectors"]),
                    ]);
                }
                vec![Block::table(table)]
            }
            LeaseVerb::Check => {
                let mut blocks = vec![Block::text(format!(
                    "Caller lease active: {}",
                    value["lease_active"]
                ))];
                for path in value["paths"].as_array().into_iter().flatten() {
                    blocks.push(Block::text(format!(
                        "{}: covered={}, unlocked={}",
                        lease_text(&path["path"]),
                        path["covered"],
                        path["unlocked"]
                    )));
                    blocks.push(Block::table(lease_holders_table(&path["holders"])));
                }
                blocks
            }
        };
        Ok(Payload::blocks(value, blocks)
            .with_exit_code(exit_code)
            .into())
    }
}

fn lease_detail(lease: &Value) -> String {
    let mut out = format!(
        "{} ({})\nMode: {}\nPrincipal: {}\nPriority: {}\nExpires: {}\nSelectors:",
        lease_text(&lease["lease_id"]),
        lease_text(&lease["state"]),
        lease_text(&lease["mode"]),
        lease_text(&lease["principal"]),
        lease_text(&lease["priority"]),
        lease_text(&lease["expires_at"])
    );
    for selector in lease["selectors"].as_array().into_iter().flatten() {
        let _ = write!(out, "\n  - {}", lease_text(selector));
    }
    out
}

fn lease_holders_table(holders: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("SELECTOR"),
        Column::new("HELD BY").fixed(),
        Column::new("HOLDER").fixed(),
        Column::new("MODE").fixed(),
        Column::new("PRINCIPAL").fixed(),
    ])
    .keep_all_columns()
    .empty_message("no overlapping holders");
    for holder in holders.as_array().into_iter().flatten() {
        table.add_row(vec![
            lease_text(&holder["file"]),
            lease_text(&holder["held_by"]),
            lease_text(&holder["held_by_id"]),
            lease_text(&holder["mode"]),
            lease_text(&holder["principal"]),
        ]);
    }
    table
}

fn lease_text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
fn lease_selectors(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

const LEASE_TOKEN_LONG_ABOUT: &str = "Mint, list and revoke lease listener tokens (run as the listener user)\n\n\
    A lease token lets a person or their agent use the lease listener, and nothing else. Tokens \
    live in lease-tokens.db in the listener user's own Orbit home, so run these commands as that \
    user, for example `sudo -H -u orbit-lease orbit lease token list`. Run as any other user they \
    find no store and refuse. They also refuse to run as root, from an agent context, or when the \
    executable or the Orbit home can be changed by another user.";

#[derive(Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct LeaseTokenCommand {
    #[command(subcommand)]
    pub command: LeaseTokenSubcommand,
}

#[derive(Subcommand)]
pub enum LeaseTokenSubcommand {
    /// Mint a token and print its secret once
    #[command(long_about = "Mint a token and print its secret once\n\n\
        The token is scoped to the workspaces named by the global --workspace (comma-separate \
        several). The secret is printed once and cannot be shown again; the store keeps only \
        its SHA-256.")]
    Create(LeaseTokenCreateArgs),
    /// List tokens with their scope and state, never their secrets
    List,
    /// Revoke a token; its next use is refused
    Revoke(LeaseTokenRevokeArgs),
}

#[derive(Args)]
pub struct LeaseTokenCreateArgs {
    /// Person (alice) or a person's agent (alice/claude-code) the token names
    #[arg(long, value_name = "NAME")]
    pub principal: String,
    /// Principal class; an agent is named under its person
    #[arg(long, value_parser = ["human", "agent"], default_value = "human")]
    pub class: String,
    /// lease (request, renew and release, and read) or read only
    #[arg(long, value_parser = ["read", "lease"], default_value = "lease")]
    pub verbs: String,
    /// Highest priority the token may request [default: high for human, normal for agent]
    #[arg(long, value_name = "PRIORITY", value_parser = ["low", "normal", "high"])]
    pub max_priority: Option<String>,
    /// Whether the token may hold hard leases (never grants them as advisory)
    #[arg(long, value_parser = ["allow", "never"], default_value = "allow")]
    pub hard: String,
    /// Lifetime from now, e.g. 12h, 30d or 8w (maximum 365d)
    #[arg(long, value_name = "DURATION", default_value = "30d")]
    pub expires: String,
}

#[derive(Args)]
pub struct LeaseTokenRevokeArgs {
    /// Token id, as `orbit lease token list` shows it
    pub token_id: String,
}

/// Longest lifetime a token may be minted with.
const LEASE_TOKEN_MAX_LIFETIME_SECONDS: u64 = 365 * 86_400;

impl LeaseTokenCommand {
    fn execute_as_listener_user(
        self,
        workspace_selector: Option<&str>,
        root_override: Option<&Path>,
    ) -> CommandOut {
        // A guard that gives an agent a clear message, not the boundary: the
        // boundary is that only the listener user holds the store.
        if orbit_common::governance::authorization::agent_context_declared() {
            return Err(OrbitError::PolicyDenied(
                "lease token commands are refused in an agent context: minting and revoking \
                 lease tokens is the operator's, run as the lease listener user \
                 (`sudo -H -u orbit-lease orbit lease token …`)"
                    .to_string(),
            ));
        }
        if root_override.is_some() {
            return Err(OrbitError::InvalidInput(
                "--root does not apply to lease token commands: they use the token store in \
                 the running user's own Orbit home"
                    .to_string(),
            ));
        }
        let mut store = LeaseTokenStore::open_for_listener_user()?;
        let now = Utc::now();
        match self.command {
            LeaseTokenSubcommand::Create(args) => {
                let scope = args.scope(workspace_selector)?;
                let expires_at = token_expiry(&args.expires, now)?;
                let minted = store.mint(scope, expires_at, &lease_token_actor(), now)?;
                let secret = minted.secret.expose_secret();
                let doc = json!({
                    "token": token_json(&minted.record)?,
                    "secret": secret,
                });
                let detail = format!(
                    "Created lease token {}\n{}\n\nSecret (shown once; it cannot be shown again):\n{secret}",
                    minted.record.token_id,
                    token_detail(&minted.record)
                );
                Ok(Payload::blocks(doc, vec![Block::text(detail)]).into())
            }
            LeaseTokenSubcommand::List => {
                let tokens = store.list()?;
                let mut table = Table::new(vec![
                    Column::new("ID").fixed(),
                    Column::new("PRINCIPAL"),
                    Column::new("CLASS").fixed(),
                    Column::new("WORKSPACES"),
                    Column::new("VERBS").fixed(),
                    Column::new("CEILING").fixed(),
                    Column::new("HARD").fixed(),
                    Column::new("EXPIRES").fixed(),
                    Column::new("LAST USED").fixed(),
                    Column::new("REVOKED").fixed(),
                ])
                .keep_all_columns()
                .empty_message("no lease tokens");
                let mut rows = Vec::with_capacity(tokens.len());
                for token in &tokens {
                    table.add_row(vec![
                        token.token_id.clone(),
                        token.scope.principal.clone(),
                        token.scope.principal_class.as_str().to_string(),
                        token.scope.workspaces.join(", "),
                        token_verbs(token),
                        token.scope.max_priority.as_str().to_string(),
                        token.scope.hard.as_str().to_string(),
                        token_time(Some(token.expires_at)),
                        token_time(token.last_used_at),
                        token_time(token.revoked_at),
                    ]);
                    rows.push(token_json(token)?);
                }
                Ok(Payload::blocks(json!({ "tokens": rows }), vec![Block::table(table)]).into())
            }
            LeaseTokenSubcommand::Revoke(args) => {
                let (record, already) =
                    match store.revoke(&args.token_id, &lease_token_actor(), now)? {
                        LeaseTokenRevocation::Revoked(record) => (record, false),
                        LeaseTokenRevocation::AlreadyRevoked(record) => (record, true),
                    };
                let text = if already {
                    format!(
                        "Lease token {} ({}) was already revoked at {}",
                        record.token_id,
                        record.scope.principal,
                        token_time(record.revoked_at)
                    )
                } else {
                    format!(
                        "Revoked lease token {} ({}); its next use is refused",
                        record.token_id, record.scope.principal
                    )
                };
                let doc = json!({
                    "token": token_json(&record)?,
                    "already_revoked": already,
                });
                Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
            }
        }
    }
}

impl LeaseTokenCreateArgs {
    fn scope(&self, workspace_selector: Option<&str>) -> Result<LeaseTokenScope, OrbitError> {
        let workspaces: Vec<String> = workspace_selector
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_string)
            .collect();
        if workspaces.is_empty() {
            return Err(OrbitError::InvalidInput(
                "name the token's workspaces with --workspace <ws> (comma-separate several)"
                    .to_string(),
            ));
        }
        let principal_class: PrincipalClass = self.class.parse()?;
        let verb: LeaseTokenVerb = self.verbs.parse()?;
        let max_priority = match &self.max_priority {
            Some(raw) => raw.parse::<LeasePriorityCeiling>()?,
            None => principal_class.default_max_priority(),
        };
        Ok(LeaseTokenScope {
            principal: self.principal.trim().to_string(),
            principal_class,
            workspaces,
            verbs: verb.granted_by(),
            max_priority,
            hard: self.hard.parse::<HardLeasePermission>()?,
        })
    }
}

fn token_expiry(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, OrbitError> {
    let seconds = orbit_common::protocol::tool_input::parse_duration_seconds(raw)?;
    if seconds == 0 || seconds > LEASE_TOKEN_MAX_LIFETIME_SECONDS {
        return Err(OrbitError::InvalidInput(format!(
            "--expires must be longer than zero and at most 365d, not '{raw}'"
        )));
    }
    let seconds = i64::try_from(seconds)
        .map_err(|_| OrbitError::InvalidInput(format!("--expires '{raw}' is too large")))?;
    Ok(now + chrono::Duration::seconds(seconds))
}

fn token_json(record: &LeaseTokenRecord) -> Result<Value, OrbitError> {
    serde_json::to_value(record)
        .map_err(|error| OrbitError::Execution(format!("serialize lease token: {error}")))
}

fn token_detail(record: &LeaseTokenRecord) -> String {
    format!(
        "Principal: {} ({})\nWorkspaces: {}\nVerbs: {}\nMax priority: {}\nHard leases: {}\nExpires: {}",
        record.scope.principal,
        record.scope.principal_class.as_str(),
        record.scope.workspaces.join(", "),
        token_verbs(record),
        record.scope.max_priority.as_str(),
        record.scope.hard.as_str(),
        token_time(Some(record.expires_at)),
    )
}

fn token_verbs(record: &LeaseTokenRecord) -> String {
    record
        .scope
        .verbs
        .iter()
        .map(|verb| verb.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn token_time(at: Option<DateTime<Utc>>) -> String {
    at.map_or_else(
        || "-".to_string(),
        |at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}
