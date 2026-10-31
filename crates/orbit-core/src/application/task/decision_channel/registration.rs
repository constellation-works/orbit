use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::plugin::is_valid_namespace;
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;
use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

use super::model::{DECISION_CHANNEL_KEY_SECRET, DecisionChannelInfo};
use super::token;

/// One registration as stored. Deliberately not `Debug`: it holds keys.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ChannelRegistration {
    pub(super) channel: String,
    pub(super) human: String,
    pub(super) plugin: String,
    #[serde(default)]
    pub(super) allowed_senders: Vec<String>,
    pub(super) epoch: u64,
    pub(super) key_hex: String,
    /// The immediately previous epoch and key, retained only so a token
    /// minted before a rotation is refused as consumed (the design's
    /// `token_consumed`-class refusal) rather than as a forgery. They
    /// verify nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) previous_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) previous_key_hex: Option<String>,
    pub(super) revoked: bool,
    #[serde(default)]
    pub(super) require_dkim_verification: bool,
    pub(super) registered_at: chrono::DateTime<chrono::Utc>,
}

impl ChannelRegistration {
    fn info(&self) -> DecisionChannelInfo {
        DecisionChannelInfo {
            channel: self.channel.clone(),
            human: self.human.clone(),
            plugin: self.plugin.clone(),
            allowed_senders: self.allowed_senders.clone(),
            epoch: self.epoch,
            key_fingerprint: token::sha256_hex(self.key_hex.as_bytes()),
            revoked: self.revoked,
            require_dkim_verification: self.require_dkim_verification,
            registered_at: self.registered_at,
        }
    }
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
        self.register_decision_channel_with_dkim_requirement(
            channel,
            human,
            plugin,
            allowed_senders,
            false,
        )
    }

    /// Register a decision channel, optionally requiring core-side DKIM
    /// verification: with `require_dkim_verification` set, every
    /// submission must carry the raw message and pass core's own DKIM
    /// check, or it is refused and nothing is applied.
    pub fn register_decision_channel_with_dkim_requirement(
        &self,
        channel: &str,
        human: &str,
        plugin: &str,
        allowed_senders: Vec<String>,
        require_dkim_verification: bool,
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
        let key_hex = token::fresh_key_hex()?;
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
            require_dkim_verification,
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
            OrbitError::decision_refused(
                DecisionRefusalCode::ChannelNotRegistered,
                format!("no decision channel `{channel}` on this host"),
            )
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
                OrbitError::decision_refused(
                    DecisionRefusalCode::ChannelNotRegistered,
                    format!("no decision channel `{channel}` on this host"),
                )
            })?;
            if registration.revoked {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::ChannelRevoked,
                    format!("decision channel `{channel}` is revoked; register it again to use it"),
                ));
            }
            registration.plugin.clone()
        };
        let key_hex = token::fresh_key_hex()?;
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

    pub(super) fn active_registration(
        &self,
        channel: &str,
    ) -> Result<ChannelRegistration, OrbitError> {
        let store = self.decision_channel_store()?;
        let registration = store.channels.get(channel).ok_or_else(|| {
            OrbitError::decision_refused(
                DecisionRefusalCode::ChannelNotRegistered,
                format!("no decision channel `{channel}` is registered on this host; nothing was applied"),
            )
        })?;
        if registration.revoked {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::ChannelRevoked,
                format!(
                    "decision channel `{channel}` is revoked on this host; nothing was applied"
                ),
            ));
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
}
