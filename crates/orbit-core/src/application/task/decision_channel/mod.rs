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
//! it. The store therefore holds key material: no plugin child may read it
//! and an agent sandbox masks it, so every read and write here runs on the
//! host and refuses a hidden store rather than seeing no channel.
//! Registration, revocation and rotation are operator-surface writes;
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

mod digest;
mod model;
mod registration;
mod store;
mod submission;
mod token;

pub use model::DECISION_CHANNEL_KEY_SECRET;
pub use model::DecisionChannelInfo;
pub use model::DecisionDigest;
pub use model::DecisionSubmitCaller;
pub use model::DecisionSubmitParams;
pub use model::MAX_DECISION_RAW_MESSAGE_BYTES;

pub(super) mod dkim;
