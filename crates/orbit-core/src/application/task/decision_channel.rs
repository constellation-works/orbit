//! Delegated-human decision channels: host registration, per-request
//! tokens, and the governed submission that applies a human's answer in
//! core (design `docs/design/human-decision-channel/2_design.md` §3–§5).
//!
//! A channel is host config, not workspace config a task could carry: the
//! store is one file under `<global_root>/state/decision-channels/` holding
//! each channel's name, the human it speaks for, the plugin bound to it,
//! its sender allowlist, and its token key with its epoch. The key is
//! generated at registration, mirrored to the bound plugin's `channel_key`
//! secret (so the transport can mint tokens into what it sends), and kept
//! here because core verifies — the plugin presents evidence, core judges
//! it. Registration, revocation and rotation are operator-surface writes;
//! nothing in this module is reachable from a plugin callback or an agent
//! envelope except [`OrbitRuntime::submit_decision_via_channel`], whose
//! caller the tool boundary derives from the callback session or the
//! operator's resolved capability, never from tool input.
//!
//! Tokens are `base32(HMAC-SHA256(channel_key, request_id | task_id |
//! epoch | expires_at_unix_seconds))` truncated to 26 chars, where the key
//! is the hex-decoded `channel_key` secret. A token is single-use: a
//! successful apply records its SHA-256 fingerprint in the resolution
//! provenance, and a submission refused *after* its token authenticated
//! (superseded, expired, rotated epoch) writes a `decision_token_consumed`
//! history entry carrying the fingerprint. Parse-level refusals
//! (unparseable, answer not allowed) and unauthenticated ones (forged
//! sender, bad token, unknown request or channel) consume nothing and
//! write nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockGuard, FileLockOptions, acquire_exclusive_file_lock, create_private_dir_all,
    write_new_private_text,
};
use orbit_types::identity::is_automation_actor;
use orbit_types::plugin::is_valid_namespace;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_DIGEST_EXPIRY_WINDOW_HOURS, DECISION_TOKEN_CONSUMED_EVENT,
    DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState, TaskHistoryEntry,
    TaskStatus, current_candidate_identity,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::TaskRecordUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;
use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

/// The plugin secret a channel key is mirrored to.
pub const DECISION_CHANNEL_KEY_SECRET: &str = "channel_key";

const STORE_DIR: &str = "state/decision-channels";
const STORE_FILE: &str = "channels.json";
const STORE_SCHEMA_VERSION: u32 = 1;
const TOKEN_LEN: usize = 26;

/// Who is presenting a submission, as the tool boundary resolved it from
/// trusted seams (the plugin callback session, the operator's capability)
/// — never from tool input. `CallerProvenance` gains no variant for this:
/// the caller is still a plugin callback session or an operator; what is
/// new is that this one operation, after its checks, writes as the human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionSubmitCaller {
    /// The named plugin's host-issued callback session.
    PluginCallback {
        /// The plugin namespace the callback session belongs to.
        plugin: String,
    },
    /// The operator, testing a channel from the CLI.
    Operator,
    /// An agent session or agent-declared process envelope.
    Agent,
    /// Nothing identified the caller.
    Unknown,
}

/// One submission to [`OrbitRuntime::submit_decision_via_channel`]: the
/// answer plus the transport evidence core judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionSubmitParams {
    /// The registered channel the answer arrives through.
    pub channel: String,
    /// The decision request the token names.
    pub request_id: String,
    /// The chosen answer id; empty reads as an unparseable reply.
    pub answer_id: String,
    /// Free-text note kept with the answer.
    pub note: Option<String>,
    /// The per-request token presented with the answer.
    pub token: String,
    /// The transport sender. A plugin submission must present one; the
    /// operator's test submission may omit it.
    pub sender: Option<String>,
    /// The raw `Authentication-Results` header the transport forwards. A
    /// plugin submission must forward one (design §4 layer 5); the
    /// operator's test submission may omit it.
    pub auth_results: Option<String>,
    /// Who is presenting the submission (see [`DecisionSubmitCaller`]).
    pub caller: DecisionSubmitCaller,
}

/// What one decision digest returns: the approval requests a transport
/// presents for every proposed and review task, which of them this call
/// raised, and every still-open request expiring within the digest
/// window ([`DECISION_DIGEST_EXPIRY_WINDOW_HOURS`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionDigest {
    /// Every request the digest carries, oldest first: `raised`, the
    /// already-open approval requests, and `expiring`, deduplicated.
    pub requests: Vec<DecisionRequest>,
    /// Requests this call raised.
    pub raised: Vec<DecisionRequest>,
    /// Still-open requests expiring within the digest window.
    pub expiring: Vec<DecisionRequest>,
}

/// A registered decision channel as operator surfaces show it: everything
/// but key material, whose fingerprint stands in for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionChannelInfo {
    /// Channel name, unique on the host.
    pub channel: String,
    /// The human identity the channel speaks for.
    pub human: String,
    /// The plugin namespace allowed to submit through the channel.
    pub plugin: String,
    /// Senders allowed to present answers; empty means the channel
    /// registers no sender restriction at core.
    pub allowed_senders: Vec<String>,
    /// Current key epoch; tokens minted under any other epoch are refused.
    pub epoch: u64,
    /// SHA-256 fingerprint of the current key, hex.
    pub key_fingerprint: String,
    /// Whether the channel is revoked.
    pub revoked: bool,
    /// When the channel was registered.
    pub registered_at: chrono::DateTime<chrono::Utc>,
}

/// One registration as stored. Deliberately not `Debug`: it holds keys.
#[derive(Clone, Serialize, Deserialize)]
struct ChannelRegistration {
    channel: String,
    human: String,
    plugin: String,
    #[serde(default)]
    allowed_senders: Vec<String>,
    epoch: u64,
    key_hex: String,
    /// The immediately previous epoch and key, retained only so a token
    /// minted before a rotation is refused as consumed (the design's
    /// `token_consumed`-class refusal) rather than as a forgery. They
    /// verify nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_key_hex: Option<String>,
    revoked: bool,
    registered_at: chrono::DateTime<chrono::Utc>,
}

impl ChannelRegistration {
    fn info(&self) -> DecisionChannelInfo {
        DecisionChannelInfo {
            channel: self.channel.clone(),
            human: self.human.clone(),
            plugin: self.plugin.clone(),
            allowed_senders: self.allowed_senders.clone(),
            epoch: self.epoch,
            key_fingerprint: sha256_hex(self.key_hex.as_bytes()),
            revoked: self.revoked,
            registered_at: self.registered_at,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ChannelStoreFile {
    schema_version: u32,
    #[serde(default)]
    channels: BTreeMap<String, ChannelRegistration>,
}

impl OrbitRuntime {
    /// Register a decision channel on this host: generate its key, mirror
    /// it to the bound plugin's `channel_key` secret, and persist the
    /// registration at epoch 1 (or one past a revoked registration it
    /// replaces, so a token from the revoked era never verifies again).
    /// Operator-surface only; the CLI refuses agent envelopes before
    /// reaching here, and no tool maps to this method.
    pub fn register_decision_channel(
        &self,
        channel: &str,
        human: &str,
        plugin: &str,
        allowed_senders: Vec<String>,
    ) -> Result<DecisionChannelInfo, OrbitError> {
        let channel = valid_channel_name(channel)?;
        let human = valid_human_label(human)?;
        let plugin = plugin.trim().to_string();
        if !is_valid_namespace(&plugin) {
            return Err(OrbitError::InvalidInput(format!(
                "'{plugin}' is not a valid plugin namespace"
            )));
        }
        let allowed_senders: Vec<String> = allowed_senders
            .into_iter()
            .map(|sender| sender.trim().to_ascii_lowercase())
            .filter(|sender| !sender.is_empty())
            .collect();
        let _lock = self.lock_decision_channel_store()?;
        let mut store = self.decision_channel_store()?;
        let epoch = match store.channels.get(&channel) {
            Some(existing) if !existing.revoked => {
                return Err(OrbitError::InvalidInput(format!(
                    "decision channel `{channel}` is already registered on this host; revoke it first, or rotate its key"
                )));
            }
            Some(existing) => existing.epoch + 1,
            None => 1,
        };
        let key_hex = fresh_key_hex()?;
        self.mirror_channel_key(&plugin, &key_hex)?;
        let registration = ChannelRegistration {
            channel: channel.clone(),
            human,
            plugin,
            allowed_senders,
            epoch,
            key_hex,
            previous_epoch: None,
            previous_key_hex: None,
            revoked: false,
            registered_at: chrono::Utc::now(),
        };
        let info = registration.info();
        store.channels.insert(channel, registration);
        self.write_decision_channel_store(&store)?;
        Ok(info)
    }

    /// This host's decision channels, by name, without key material.
    pub fn list_decision_channels(&self) -> Result<Vec<DecisionChannelInfo>, OrbitError> {
        Ok(self
            .decision_channel_store()?
            .channels
            .values()
            .map(ChannelRegistration::info)
            .collect())
    }

    /// Revoke a channel immediately: its outstanding tokens stop
    /// verifying and every submission through it is refused.
    pub fn revoke_decision_channel(
        &self,
        channel: &str,
    ) -> Result<DecisionChannelInfo, OrbitError> {
        let _lock = self.lock_decision_channel_store()?;
        let mut store = self.decision_channel_store()?;
        let registration = store.channels.get_mut(channel).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "channel_not_registered: no decision channel `{channel}` on this host"
            ))
        })?;
        registration.revoked = true;
        registration.previous_epoch = None;
        registration.previous_key_hex = None;
        let info = registration.info();
        self.write_decision_channel_store(&store)?;
        Ok(info)
    }

    /// Rotate a channel's key: fresh key, epoch bumped, mirrored to the
    /// bound plugin's secret. Every outstanding token is invalidated
    /// (§4); one minted under the previous epoch is refused
    /// `token_consumed` — the previous key is retained only to say so.
    pub fn rotate_decision_channel_key(
        &self,
        channel: &str,
    ) -> Result<DecisionChannelInfo, OrbitError> {
        let _lock = self.lock_decision_channel_store()?;
        let mut store = self.decision_channel_store()?;
        let plugin = {
            let registration = store.channels.get(channel).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "channel_not_registered: no decision channel `{channel}` on this host"
                ))
            })?;
            if registration.revoked {
                return Err(OrbitError::InvalidInput(format!(
                    "channel_revoked: decision channel `{channel}` is revoked; register it again to use it"
                )));
            }
            registration.plugin.clone()
        };
        let key_hex = fresh_key_hex()?;
        self.mirror_channel_key(&plugin, &key_hex)?;
        let registration = store
            .channels
            .get_mut(channel)
            .ok_or_else(|| OrbitError::Execution("decision channel vanished".into()))?;
        registration.previous_epoch = Some(registration.epoch);
        registration.previous_key_hex = Some(std::mem::replace(&mut registration.key_hex, key_hex));
        registration.epoch += 1;
        let info = registration.info();
        self.write_decision_channel_store(&store)?;
        Ok(info)
    }

    /// Mint the token for one request under a channel's current key and
    /// epoch. Core's half of the token contract: a transport holding the
    /// mirrored `channel_key` secret mints the same value itself; this
    /// method serves the operator testing a channel and tests. It is not
    /// exposed as a tool — a token must never be listable by the agents
    /// who may read the request it answers.
    pub fn mint_decision_token(
        &self,
        channel: &str,
        request_id: &str,
    ) -> Result<String, OrbitError> {
        let registration = self.active_registration(channel)?;
        let request = self.get_decision_request(request_id)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "request_not_found: no decision request `{request_id}` in this workspace"
            ))
        })?;
        mint_token(&registration.key_hex, registration.epoch, &request)
    }

    /// Submit a human's answer through a registered channel, applying it
    /// in core as the registered human when — and only when — every
    /// check passes, in the design's order: the caller is the channel's
    /// bound plugin (or the operator testing it), the channel is
    /// registered and unrevoked, the presented sender and authentication
    /// evidence pass, the request exists, the token verifies under the
    /// current epoch and is unconsumed, the answer is allowed, and the
    /// request is still open against its binding. Each refusal names
    /// itself (`caller_not_allowed`, `channel_not_registered`,
    /// `channel_revoked`, `sender_not_allowed`, `authentication_failed`,
    /// `request_not_found`, `token_invalid`, `token_consumed`,
    /// `answer_not_allowed`, `request_expired`, `request_superseded`)
    /// and applies nothing.
    pub fn submit_decision_via_channel(
        &self,
        params: DecisionSubmitParams,
    ) -> Result<DecisionRequest, OrbitError> {
        match &params.caller {
            DecisionSubmitCaller::PluginCallback { .. } | DecisionSubmitCaller::Operator => {}
            DecisionSubmitCaller::Agent | DecisionSubmitCaller::Unknown => {
                return Err(OrbitError::PolicyDenied(
                    "caller_not_allowed: submitting a decision is a registered channel's or the operator's; an agent session never answers for a human".into(),
                ));
            }
        }
        if !self.decision_channel_enabled() {
            return Err(OrbitError::PolicyDenied(
                "decision_channel_disabled: the decision channel is not enabled in this workspace; nothing was applied".into(),
            ));
        }
        // Held from the registration read through the apply: a revoke or
        // rotation cannot land between the check and the apply, and two
        // presentations of one token must not both pass the single-use check.
        // Nothing this path reaches takes the same lock.
        let _submission = self.lock_decision_channel_store()?;
        let registration = self.active_registration(&params.channel)?;
        if let DecisionSubmitCaller::PluginCallback { plugin } = &params.caller
            && plugin != &registration.plugin
        {
            return Err(OrbitError::PolicyDenied(format!(
                "channel_not_registered: decision channel `{}` is not bound to plugin `{plugin}`; nothing was applied",
                params.channel
            )));
        }
        // The transport evidence design §3.3 step 4 verifies. A plugin
        // submission must carry it; the operator's own test submission may
        // omit it. A registered allowlist applies to any sender presented.
        let transport = matches!(params.caller, DecisionSubmitCaller::PluginCallback { .. });
        let presented = params
            .sender
            .as_deref()
            .map(|sender| sender.trim().to_ascii_lowercase())
            .filter(|sender| !sender.is_empty());
        if transport && presented.is_none() {
            return Err(OrbitError::PolicyDenied(format!(
                "sender_not_allowed: the transport presented no sender for decision channel `{}`; nothing was applied",
                params.channel
            )));
        }
        if let Some(sender) = presented.as_deref()
            && !registration.allowed_senders.is_empty()
            && !registration
                .allowed_senders
                .iter()
                .any(|allowed| allowed == sender)
        {
            return Err(OrbitError::PolicyDenied(format!(
                "sender_not_allowed: the presented sender is not on decision channel `{}`'s allowlist; nothing was applied",
                params.channel
            )));
        }
        let evidence_passes = match params.auth_results.as_deref() {
            Some(auth_results) => authentication_results_pass(auth_results),
            None => !transport,
        };
        if !evidence_passes {
            return Err(OrbitError::PolicyDenied(
                "authentication_failed: the transport did not forward Authentication-Results showing dkim=pass and spf=pass without dmarc=fail; nothing was applied".into(),
            ));
        }
        let request = self.get_decision_request(&params.request_id)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "request_not_found: no decision request `{}` in this workspace; nothing was applied",
                params.request_id
            ))
        })?;
        let token = params.token.trim().to_ascii_uppercase();
        let fingerprint = sha256_hex(token.as_bytes());
        if !token_well_formed(&token)
            || !token_matches(&token, &registration.key_hex, registration.epoch, &request)?
        {
            if token_well_formed(&token)
                && let (Some(epoch), Some(key)) = (
                    registration.previous_epoch,
                    registration.previous_key_hex.as_deref(),
                )
                && token_matches(&token, key, epoch, &request)?
            {
                self.consume_decision_token(&request, &fingerprint, "key_rotated")?;
                return Err(OrbitError::InvalidInput(format!(
                    "token_consumed: the token for decision request `{}` was minted under key epoch {epoch}, which rotation invalidated; nothing was applied",
                    request.id
                )));
            }
            return Err(OrbitError::PolicyDenied(format!(
                "token_invalid: the token does not verify for decision request `{}` under channel `{}`; nothing was applied",
                request.id, params.channel
            )));
        }
        if self.decision_token_consumed(&request, &fingerprint)? {
            return Err(OrbitError::InvalidInput(format!(
                "token_consumed: the token for decision request `{}` was already presented and judged; the original outcome stands and nothing was applied again",
                request.id
            )));
        }
        let answer_id = params.answer_id.trim();
        if answer_id.is_empty() {
            return Err(answer_not_allowed(&request, "the reply carried no answer"));
        }
        if !request.allows(answer_id) {
            return Err(answer_not_allowed(
                &request,
                &format!("`{answer_id}` is not an allowed answer"),
            ));
        }
        let note = params
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        if request.gate == DecisionGate::PilotAdmission
            && answer_id == "evaluated"
            && note.is_none()
        {
            return Err(answer_not_allowed(
                &request,
                "an `evaluated` verdict needs its evaluation as the note",
            ));
        }
        match request.state {
            DecisionRequestState::Open => {}
            DecisionRequestState::Expired => {
                self.consume_decision_token(&request, &fingerprint, "request_expired")?;
                return Err(OrbitError::InvalidInput(format!(
                    "request_expired: decision request `{}` expired at {}; nothing was applied",
                    request.id,
                    request.expires_at.to_rfc3339()
                )));
            }
            DecisionRequestState::Answered | DecisionRequestState::Superseded => {
                self.consume_decision_token(&request, &fingerprint, "request_superseded")?;
                return Err(OrbitError::InvalidInput(format!(
                    "request_superseded: decision request `{}` no longer matches the hold on task {}; nothing was applied",
                    request.id, request.task_id
                )));
            }
        }
        let provenance = format!(
            "decision-channel {} (request {}, token sha256:{fingerprint})",
            registration.channel, request.id
        );
        self.answer_decision_request_as_human(
            &request.id,
            answer_id,
            note.map(str::to_string),
            registration.human.clone(),
            Some(provenance),
        )
    }

    /// The decision digest one transport call renders (design §7):
    /// raise the approval request every `proposed` task is missing (and
    /// every `review` task's, bound to its candidate), then return those
    /// requests beside every still-open request expiring within
    /// [`DECISION_DIGEST_EXPIRY_WINDOW_HOURS`].
    ///
    /// Raising is idempotent per (task, gate, binding): a task whose
    /// current binding already has an open request raises nothing and
    /// the digest returns that request, so a second call returns the
    /// same request ids. The caller is the channel's bound plugin
    /// through its callback session, or the operator — an agent session
    /// is refused `caller_not_allowed`, and a plugin the named channel
    /// is not bound to is refused `channel_not_registered`, exactly as
    /// for submission. With `[decision_channel]` disabled the digest
    /// raises nothing, writes nothing and returns no requests.
    pub fn decision_digest(
        &self,
        channel: &str,
        caller: DecisionSubmitCaller,
    ) -> Result<DecisionDigest, OrbitError> {
        match &caller {
            DecisionSubmitCaller::PluginCallback { .. } | DecisionSubmitCaller::Operator => {}
            DecisionSubmitCaller::Agent | DecisionSubmitCaller::Unknown => {
                return Err(OrbitError::PolicyDenied(
                    "caller_not_allowed: the decision digest is a registered channel's or the operator's; an agent session never raises requests for a human".into(),
                ));
            }
        }
        if !self.decision_channel_enabled() {
            return Ok(DecisionDigest {
                requests: Vec::new(),
                raised: Vec::new(),
                expiring: Vec::new(),
            });
        }
        let registration = self.active_registration(channel)?;
        if let DecisionSubmitCaller::PluginCallback { plugin } = &caller
            && plugin != &registration.plugin
        {
            return Err(OrbitError::PolicyDenied(format!(
                "channel_not_registered: decision channel `{channel}` is not bound to plugin `{plugin}`; nothing was raised"
            )));
        }
        self.ensure_coordination_task_write_permitted()?;
        let mut raised = Vec::new();
        let mut approvals = Vec::new();
        for (status, gate) in [
            (TaskStatus::Proposed, DecisionGate::ProposedApproval),
            (TaskStatus::Review, DecisionGate::ReviewApproval),
        ] {
            for task in self.list_tasks_filtered(Some(status), None, None, None, None, None)? {
                if let Some((request, fresh)) =
                    self.raise_approval_decision_request(&task.id, gate)?
                {
                    if fresh {
                        raised.push(request.clone());
                    }
                    approvals.push(request);
                }
            }
        }
        let horizon =
            chrono::Utc::now() + chrono::Duration::hours(DECISION_DIGEST_EXPIRY_WINDOW_HOURS);
        let expiring: Vec<DecisionRequest> = self
            .list_decision_requests(None, None, false)?
            .into_iter()
            .filter(|request| request.expires_at <= horizon)
            .collect();
        let mut requests = approvals;
        for request in &expiring {
            if !requests.iter().any(|carried| carried.id == request.id) {
                requests.push(request.clone());
            }
        }
        requests.sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(DecisionDigest {
            requests,
            raised,
            expiring,
        })
    }

    /// The one open approval request for `task_id`'s current binding on
    /// `gate`, raising it (`raised_by: digest`) when the binding has
    /// none. The second element says whether this call raised it.
    /// `None` when the task left the gate's status; the caller has
    /// already checked the channel is enabled.
    fn raise_approval_decision_request(
        &self,
        task_id: &str,
        gate: DecisionGate,
    ) -> Result<Option<(DecisionRequest, bool)>, OrbitError> {
        let mut outcome: Option<(DecisionRequest, bool)> = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                let expected = match gate {
                    DecisionGate::ProposedApproval => TaskStatus::Proposed,
                    DecisionGate::ReviewApproval => TaskStatus::Review,
                    DecisionGate::BlockTriage | DecisionGate::PilotAdmission => {
                        return Ok(());
                    }
                };
                if task.status != expected {
                    return Ok(());
                }
                let candidate = if gate == DecisionGate::ReviewApproval {
                    current_candidate_identity(&self.get_task_history(task_id)?)
                } else {
                    None
                };
                let binding = DecisionBinding::for_task(&task, candidate);
                let existing =
                    self.get_task_decision_requests(task_id)?
                        .into_iter()
                        .find(|request| {
                            request.gate == gate
                                && request.binding == binding
                                && request.state == DecisionRequestState::Open
                        });
                if let Some(existing) = existing {
                    outcome = Some((existing, false));
                    return Ok(());
                }
                let request = DecisionRequest::new(
                    task_id.to_string(),
                    gate,
                    gate.approval_answers(),
                    binding,
                    "digest".to_string(),
                    chrono::Utc::now(),
                );
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        append_history: vec![request.history_entry(SYSTEM_ACTOR_LABEL)],
                        ..Default::default()
                    },
                )?;
                let stored = self
                    .get_task_decision_requests(task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request.id)
                    .unwrap_or(request);
                outcome = Some((stored, true));
                Ok(())
            })?;
        Ok(outcome)
    }

    fn active_registration(&self, channel: &str) -> Result<ChannelRegistration, OrbitError> {
        let store = self.decision_channel_store()?;
        let registration = store.channels.get(channel).ok_or_else(|| {
            OrbitError::PolicyDenied(format!(
                "channel_not_registered: no decision channel `{channel}` is registered on this host; nothing was applied"
            ))
        })?;
        if registration.revoked {
            return Err(OrbitError::PolicyDenied(format!(
                "channel_revoked: decision channel `{channel}` is revoked on this host; nothing was applied"
            )));
        }
        Ok(registration.clone())
    }

    fn mirror_channel_key(&self, plugin: &str, key_hex: &str) -> Result<(), OrbitError> {
        let store = PluginSecretStore::new(&self.global_root());
        store.put(
            plugin,
            DECISION_CHANNEL_KEY_SECRET,
            &PluginSecretValue::new(key_hex.to_string())?,
        )?;
        Ok(())
    }

    /// Whether `fingerprint` already judged a submission for `request`:
    /// the applied answer's provenance carries it, or a post-authentication
    /// refusal consumed it.
    fn decision_token_consumed(
        &self,
        request: &DecisionRequest,
        fingerprint: &str,
    ) -> Result<bool, OrbitError> {
        let history = self.get_task_history(&request.task_id)?;
        Ok(history.iter().any(|entry| {
            (entry.event == DECISION_TOKEN_CONSUMED_EVENT || entry.event == DECISION_ANSWERED_EVENT)
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains(fingerprint))
        }))
    }

    fn consume_decision_token(
        &self,
        request: &DecisionRequest,
        fingerprint: &str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        let entry = TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: DECISION_TOKEN_CONSUMED_EVENT.to_string(),
            note: Some(
                serde_json::json!({
                    "request_id": request.id,
                    "token_sha256": fingerprint,
                    "reason": reason,
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: Some(request.id.clone()),
            decision: None,
        };
        self.stores().task_records().update(
            &request.task_id,
            TaskRecordUpdateParams {
                actor: SYSTEM_ACTOR_LABEL.to_string(),
                append_history: vec![entry],
                ..Default::default()
            },
        )?;
        Ok(())
    }

    fn decision_channel_dir(&self) -> PathBuf {
        self.global_root().join(STORE_DIR)
    }

    fn decision_channel_store(&self) -> Result<ChannelStoreFile, OrbitError> {
        let path = self.decision_channel_dir().join(STORE_FILE);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ChannelStoreFile {
                    schema_version: STORE_SCHEMA_VERSION,
                    channels: BTreeMap::new(),
                });
            }
            Err(error) => {
                return Err(OrbitError::Io(format!("read {}: {error}", path.display())));
            }
        };
        serde_json::from_str(&body).map_err(|error| {
            OrbitError::Execution(format!(
                "decision channel store at {} is unreadable: {error}",
                path.display()
            ))
        })
    }

    fn lock_decision_channel_store(&self) -> Result<FileLockGuard, OrbitError> {
        let lock_path = self.decision_channel_dir().join("channels.lock");
        acquire_exclusive_file_lock(&lock_path, "decision channels", FileLockOptions::default())
            .map_err(|error| OrbitError::Io(format!("lock {}: {error}", lock_path.display())))
    }

    /// Callers hold `lock_decision_channel_store` across their whole
    /// read-modify-write; this writer does not take it again.
    fn write_decision_channel_store(&self, store: &ChannelStoreFile) -> Result<(), OrbitError> {
        let dir = self.decision_channel_dir();
        create_private_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
        let path = dir.join(STORE_FILE);
        let body = serde_json::to_string(store).map_err(|error| {
            OrbitError::Execution(format!("serialize decision channels: {error}"))
        })?;
        let staged = dir.join(format!(".{STORE_FILE}.{}.tmp", fresh_key_hex()?));
        let result =
            write_new_private_text(&staged, &body).and_then(|()| std::fs::rename(&staged, &path));
        if let Err(error) = result {
            let _ = std::fs::remove_file(&staged);
            return Err(OrbitError::Io(format!("write {}: {error}", path.display())));
        }
        Ok(())
    }
}

fn answer_not_allowed(request: &DecisionRequest, why: &str) -> OrbitError {
    let allowed = request
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    OrbitError::InvalidInput(format!(
        "answer_not_allowed: {why} for decision request `{}` (allowed: {allowed}); the token was not consumed and nothing was applied",
        request.id
    ))
}

fn valid_channel_name(channel: &str) -> Result<String, OrbitError> {
    let channel = channel.trim().to_string();
    let valid = !channel.is_empty()
        && channel.len() <= 64
        && channel
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        && channel
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase());
    if !valid {
        return Err(OrbitError::InvalidInput(format!(
            "'{channel}' is not a valid decision channel name; use lowercase letters, digits, '-' or '_', starting with a letter"
        )));
    }
    Ok(channel)
}

fn valid_human_label(human: &str) -> Result<String, OrbitError> {
    let human = human.trim().to_string();
    if human.is_empty() || human.chars().any(char::is_control) {
        return Err(OrbitError::InvalidInput(
            "the human a decision channel speaks for must be a non-empty label".into(),
        ));
    }
    if is_automation_actor(&human) {
        return Err(OrbitError::InvalidInput(format!(
            "'{human}' is an automation actor, not a human; a decision channel must speak for a human whose writes count as human decisions"
        )));
    }
    Ok(human)
}

fn authentication_results_pass(header: &str) -> bool {
    let header = header.to_ascii_lowercase();
    header.contains("dkim=pass") && header.contains("spf=pass") && !header.contains("dmarc=fail")
}

fn fresh_key_hex() -> Result<String, OrbitError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("draw decision channel key: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn key_bytes(key_hex: &str) -> Result<Vec<u8>, OrbitError> {
    if !key_hex.len().is_multiple_of(2) || !key_hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(OrbitError::Execution(
            "decision channel key is not hex; rotate the channel key".into(),
        ));
    }
    (0..key_hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&key_hex[index..index + 2], 16)
                .map_err(|error| OrbitError::Execution(format!("decision channel key: {error}")))
        })
        .collect()
}

fn token_message(epoch: u64, request: &DecisionRequest) -> Vec<u8> {
    format!(
        "{}|{}|{}|{}",
        request.id,
        request.task_id,
        epoch,
        request.expires_at.timestamp()
    )
    .into_bytes()
}

fn mint_token(key_hex: &str, epoch: u64, request: &DecisionRequest) -> Result<String, OrbitError> {
    let mac = hmac_sha256(&key_bytes(key_hex)?, &token_message(epoch, request));
    Ok(base32(&mac).chars().take(TOKEN_LEN).collect())
}

fn token_matches(
    presented: &str,
    key_hex: &str,
    epoch: u64,
    request: &DecisionRequest,
) -> Result<bool, OrbitError> {
    let expected = mint_token(key_hex, epoch, request)?;
    Ok(constant_time_eq(presented.as_bytes(), expected.as_bytes()))
}

fn token_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN && token.chars().all(|ch| matches!(ch, 'A'..='Z' | '2'..='7'))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// HMAC-SHA256 (RFC 2104) over the workspace's `sha2`, so the token
/// contract needs no further dependency.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(key_block.iter().map(|byte| byte ^ 0x36).collect::<Vec<_>>());
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(key_block.iter().map(|byte| byte ^ 0x5c).collect::<Vec<_>>());
    outer.update(inner_hash);
    outer.finalize().into()
}

/// RFC 4648 base32 without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}
