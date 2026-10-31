/// How a DKIM verification can end without a pass. `Failed` is a verdict
/// core reached about the message; `Unavailable` means core could not
/// reach a verdict (DNS timeout or error) and the caller may retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DkimVerifyError {
    /// The signature, body hash, key record or domain alignment failed.
    Failed(String),
    /// The DNS lookup failed or timed out; retryable, applies nothing.
    Unavailable(String),
}

/// Resolves DNS TXT records for DKIM selector lookups. Implementations
/// return one string per TXT record (its character strings joined); an
/// empty result means no record exists, and `Err` means the lookup itself
/// failed (timeout, server error) and may be retried.
pub trait DkimTxtResolver: Send + Sync {
    /// Look up the TXT records at `name` (for example
    /// `selector._domainkey.example.com`).
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String>;
}
