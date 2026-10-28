use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rsa::RsaPublicKey;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use sha2::{Digest, Sha256};

use super::message;
use super::message::Header;
use super::model::{DkimTxtResolver, DkimVerifyError};

pub(super) fn failed(message: impl Into<String>) -> DkimVerifyError {
    DkimVerifyError::Failed(message.into())
}

/// Verify the DKIM signature on `raw_message`. When `sender_domain` is
/// given the signing domain must align with it; otherwise it must align
/// with the message's `From` domain. Alignment is relaxed: the signing
/// domain equals the sender domain or is a parent or subdomain of it.
pub fn verify_dkim(
    raw_message: &[u8],
    sender_domain: Option<&str>,
    resolver: &dyn DkimTxtResolver,
) -> Result<(), DkimVerifyError> {
    let (headers, body) = message::parse_message(raw_message);
    let signatures: Vec<&Header> = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("dkim-signature"))
        .collect();
    if signatures.is_empty() {
        return Err(failed("the raw message carries no DKIM-Signature header"));
    }
    let expected_domain = match sender_domain {
        Some(domain) => domain.trim().trim_end_matches('.').to_ascii_lowercase(),
        None => from_domain(&headers)
            .ok_or_else(|| failed("the raw message has no usable From address"))?,
    };
    if expected_domain.is_empty() {
        return Err(failed("no sender domain to align the DKIM signature with"));
    }
    let mut last_failure = failed("no DKIM signature verified");
    for signature in signatures {
        match verify_one(signature, &headers, body, &expected_domain, resolver) {
            Ok(()) => return Ok(()),
            Err(DkimVerifyError::Unavailable(message)) => {
                return Err(DkimVerifyError::Unavailable(message));
            }
            Err(error) => last_failure = error,
        }
    }
    Err(last_failure)
}

fn verify_one(
    signature_header: &Header,
    headers: &[Header],
    body: &[u8],
    expected_domain: &str,
    resolver: &dyn DkimTxtResolver,
) -> Result<(), DkimVerifyError> {
    let tags = message::parse_tags(&message::unfold(&signature_header.value));
    let tag = |name: &str| tags.get(name).map(String::as_str);
    if tag("v") != Some("1") {
        return Err(failed("DKIM-Signature has no v=1 tag"));
    }
    if tag("a") != Some("rsa-sha256") {
        return Err(failed("only rsa-sha256 DKIM signatures are verified"));
    }
    let domain = tag("d")
        .ok_or_else(|| failed("DKIM-Signature has no d= tag"))?
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !domains_align(&domain, expected_domain) {
        return Err(failed(format!(
            "DKIM signing domain `{domain}` does not align with the sender domain `{expected_domain}`"
        )));
    }
    let selector = tag("s").ok_or_else(|| failed("DKIM-Signature has no s= tag"))?;
    let signed_headers: Vec<String> = tag("h")
        .ok_or_else(|| failed("DKIM-Signature has no h= tag"))?
        .split(':')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    if !signed_headers.iter().any(|name| name == "from") {
        return Err(failed("the DKIM signature does not sign the From header"));
    }
    let (header_canon, body_canon) =
        message::canonicalization(tag("c").unwrap_or("simple/simple"))?;
    if let Some(expires) = tag("x").and_then(|value| value.trim().parse::<u64>().ok()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        if now > expires {
            return Err(failed("the DKIM signature has expired"));
        }
    }
    let mut canonical_body = message::canonicalize_body(body, body_canon);
    if let Some(length) = tag("l").and_then(|value| value.trim().parse::<usize>().ok()) {
        canonical_body.truncate(length);
    }
    let body_hash = BASE64.encode(Sha256::digest(&canonical_body));
    let claimed_hash =
        message::compact(tag("bh").ok_or_else(|| failed("DKIM-Signature has no bh= tag"))?);
    if body_hash != claimed_hash {
        return Err(failed("the DKIM body hash does not match the message body"));
    }
    let lookup_name = format!("{selector}._domainkey.{domain}");
    let records = resolver
        .lookup_txt(&lookup_name)
        .map_err(DkimVerifyError::Unavailable)?;
    let public_key = public_key_from_records(&records, &lookup_name)?;
    let header_data =
        message::signed_header_data(signature_header, headers, &signed_headers, header_canon);
    let signature_bytes = BASE64
        .decode(message::compact(
            tag("b").ok_or_else(|| failed("DKIM-Signature has no b= tag"))?,
        ))
        .map_err(|error| failed(format!("the DKIM b= signature is not base64: {error}")))?;
    let signature = RsaSignature::try_from(signature_bytes.as_slice())
        .map_err(|error| failed(format!("the DKIM signature is malformed: {error}")))?;
    VerifyingKey::<Sha256>::new(public_key)
        .verify(&header_data, &signature)
        .map_err(|_| failed("the DKIM header signature does not verify"))
}

fn public_key_from_records(
    records: &[String],
    lookup_name: &str,
) -> Result<RsaPublicKey, DkimVerifyError> {
    for record in records {
        let tags = message::parse_tags(record);
        let Some(key_data) = tags.get("p") else {
            continue;
        };
        if let Some(version) = tags.get("v")
            && !version.eq_ignore_ascii_case("DKIM1")
        {
            continue;
        }
        if let Some(kind) = tags.get("k")
            && !kind.eq_ignore_ascii_case("rsa")
        {
            return Err(failed("the DKIM key record is not an RSA key"));
        }
        let der = BASE64
            .decode(message::compact(key_data))
            .map_err(|error| failed(format!("the DKIM p= key is not base64: {error}")))?;
        if der.is_empty() {
            return Err(failed("the DKIM key at the selector is revoked (empty p=)"));
        }
        return RsaPublicKey::from_public_key_der(&der)
            .or_else(|_| RsaPublicKey::from_pkcs1_der(&der))
            .map_err(|error| failed(format!("the DKIM public key is unreadable: {error}")));
    }
    Err(failed(format!(
        "no DKIM key record (p=) at `{lookup_name}`"
    )))
}

fn domains_align(signing: &str, sender: &str) -> bool {
    signing == sender
        || sender
            .strip_suffix(signing)
            .is_some_and(|prefix| prefix.ends_with('.'))
        || signing
            .strip_suffix(sender)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// The sender domain of an address such as `operator@example.com`, or the
/// domain itself when given a bare host name.
pub fn sender_domain_of(sender: &str) -> Option<String> {
    let sender = sender.trim().to_ascii_lowercase();
    let domain = match sender.rsplit_once('@') {
        Some((_, domain)) => domain.trim_end_matches(['.', '>', ' ']),
        None => sender.trim_end_matches('.'),
    };
    if domain.is_empty() || !domain.contains('.') {
        return None;
    }
    Some(domain.to_string())
}

fn from_domain(headers: &[Header]) -> Option<String> {
    let from = headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("from"))?;
    let value = message::unfold(&from.value);
    let address = value
        .split('<')
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .unwrap_or(&value);
    sender_domain_of(address)
}
