//! Core-side DKIM verification for decision-channel submissions: the only
//! mail vocabulary in core, kept in this one module. It verifies an
//! `rsa-sha256` DKIM signature (RFC 6376) on a raw message the transport
//! forwards: the selector's public key comes from DNS TXT records, the
//! body hash and the signed-header signature are checked, and the signing
//! domain must align with the sender's domain. DNS goes through
//! [`DkimTxtResolver`] so tests inject a fixture resolver and never touch
//! live DNS; production uses [`SystemDkimTxtResolver`], a minimal UDP TXT
//! client over the host's configured nameservers with a bounded timeout,
//! so this needs no DNS dependency beyond the standard library.

mod message;
mod model;
mod resolver;
mod verification;

pub use model::DkimTxtResolver;
pub use model::DkimVerifyError;
pub use resolver::SystemDkimTxtResolver;
pub use verification::sender_domain_of;
pub use verification::verify_dkim;
#[cfg(feature = "test-support")]
pub(crate) mod test_support;
