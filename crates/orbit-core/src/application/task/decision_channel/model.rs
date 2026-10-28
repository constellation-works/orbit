use orbit_types::task::DecisionRequest;
use serde::{Deserialize, Serialize};

/// The plugin secret a channel key is mirrored to.
pub const DECISION_CHANNEL_KEY_SECRET: &str = "channel_key";

/// Largest raw message a submission may carry for core-side DKIM
/// verification (256 KiB). A larger one is refused
/// `raw_message_too_large` and nothing is applied.
pub const MAX_DECISION_RAW_MESSAGE_BYTES: usize = 256 * 1024;

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

/// One submission to [`crate::OrbitRuntime::submit_decision_via_channel`]: the
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
    /// The raw reply message, when the transport forwards one. When
    /// present, core verifies its DKIM signature itself and refuses
    /// `dkim_verification_failed` when that contradicts the forwarded
    /// verdicts; when the channel requires core-side verification, a
    /// submission without one is refused `dkim_verification_required`.
    /// At most [`MAX_DECISION_RAW_MESSAGE_BYTES`] bytes.
    pub raw_message: Option<String>,
    /// Who is presenting the submission (see [`DecisionSubmitCaller`]).
    pub caller: DecisionSubmitCaller,
}

/// What one decision digest returns: the approval requests a transport
/// presents for every proposed and review task, which of them this call
/// raised, and every still-open request expiring within the digest
/// window ([`orbit_types::task::DECISION_DIGEST_EXPIRY_WINDOW_HOURS`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionDigest {
    /// Every request the digest carries, oldest first: `raised`, the
    /// already-open approval requests, and `expiring`, deduplicated.
    pub requests: Vec<DecisionRequest>,
    /// Requests this call raised.
    pub raised: Vec<DecisionRequest>,
    /// Still-open requests expiring within the digest window.
    pub expiring: Vec<DecisionRequest>,
    /// Corrupt task bundles omitted from the digest.
    pub warnings: Vec<String>,
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
    /// Whether core-side DKIM verification of a forwarded raw message is
    /// required for every submission on this channel.
    #[serde(default)]
    pub require_dkim_verification: bool,
    /// When the channel was registered.
    pub registered_at: chrono::DateTime<chrono::Utc>,
}
