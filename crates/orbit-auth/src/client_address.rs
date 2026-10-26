//! The lease listener's client address: the one place that reads forwarding
//! headers, for audit rows and rate limits alike.
//!
//! Without a proxy the client is the TCP peer and `X-Forwarded-For` is
//! ignored. Behind a proxy, the header is read only when the TCP peer is a
//! trusted proxy: every `X-Forwarded-For` header is joined in order, entries
//! are walked from the right, trusted proxies are skipped, and the first
//! other entry is the client. Entries to its left are whatever the client
//! sent and are never used. A malformed entry, or a chain with no untrusted
//! entry, is refused rather than replaced by the proxy's own address, which
//! would put every client in one bucket. `Forwarded` (RFC 7239) is never
//! read, so a client cannot choose which header is believed.

use std::fmt;
use std::net::IpAddr;

use orbit_common::OrbitError;

/// One trusted-proxy entry: an address, or a CIDR range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AddressRange {
    network: IpAddr,
    prefix: u8,
}

impl AddressRange {
    fn parse(raw: &str) -> Result<Self, OrbitError> {
        let invalid = || {
            OrbitError::InvalidInput(format!(
                "lease.listener.trusted_proxy entry '{raw}' is not an IP address or CIDR range \
                 (for example 10.0.0.5 or 10.0.0.0/24)"
            ))
        };
        let trimmed = raw.trim();
        let (address, prefix) = match trimmed.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (trimmed, None),
        };
        let network = address
            .parse::<IpAddr>()
            .map_err(|_| invalid())?
            .to_canonical();
        let width = address_bits(network);
        let prefix = match prefix {
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|bits| *bits <= width)
                .ok_or_else(invalid)?,
            None => width,
        };
        Ok(Self { network, prefix })
    }

    fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address.to_canonical()) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

fn address_bits(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// The parsed `lease.listener.trusted_proxy` list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<AddressRange>);

impl TrustedProxies {
    /// Parse addresses and CIDR ranges, refusing any entry that is neither.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, OrbitError> {
        entries
            .iter()
            .map(|entry| AddressRange::parse(entry.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Whether no proxy is trusted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `address` is a trusted proxy.
    pub fn contains(&self, address: IpAddr) -> bool {
        self.0.iter().any(|range| range.contains(address))
    }
}

/// How the listener finds a request's client address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseClientAddressPolicy {
    /// The TCP peer is the client; forwarding headers are ignored.
    Direct,
    /// A terminating proxy is in front; `X-Forwarded-For` is read only from
    /// these trusted proxies.
    BehindProxy(TrustedProxies),
}

/// Why no client address could be resolved. The request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAddressRefusal {
    /// The rightmost untrusted `X-Forwarded-For` entry is not an IP address.
    MalformedForwardedFor,
    /// Every `X-Forwarded-For` entry, if there are any, is a trusted proxy.
    NoUntrustedForwardedFor,
}

impl ClientAddressRefusal {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedForwardedFor => "malformed_forwarded_for",
            Self::NoUntrustedForwardedFor => "no_untrusted_forwarded_for",
        }
    }
}

impl fmt::Display for ClientAddressRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Resolve a request's client address from its TCP `peer` and its
/// `X-Forwarded-For` header values in arrival order. See the module
/// documentation for the rule.
pub fn resolve_lease_client_address(
    policy: &LeaseClientAddressPolicy,
    peer: IpAddr,
    forwarded_for: &[&[u8]],
) -> Result<IpAddr, ClientAddressRefusal> {
    let peer = peer.to_canonical();
    let trusted = match policy {
        LeaseClientAddressPolicy::BehindProxy(trusted) if trusted.contains(peer) => trusted,
        _ => return Ok(peer),
    };
    let mut entries = Vec::new();
    for value in forwarded_for {
        let text =
            std::str::from_utf8(value).map_err(|_| ClientAddressRefusal::MalformedForwardedFor)?;
        entries.extend(text.split(','));
    }
    for entry in entries.iter().rev() {
        let address = entry
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| ClientAddressRefusal::MalformedForwardedFor)?
            .to_canonical();
        if !trusted.contains(address) {
            return Ok(address);
        }
    }
    Err(ClientAddressRefusal::NoUntrustedForwardedFor)
}
