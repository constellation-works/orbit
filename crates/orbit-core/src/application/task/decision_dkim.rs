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

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rsa::RsaPublicKey;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use sha2::{Digest, Sha256};

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

fn failed(message: impl Into<String>) -> DkimVerifyError {
    DkimVerifyError::Failed(message.into())
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

/// Verify the DKIM signature on `raw_message`. When `sender_domain` is
/// given the signing domain must align with it; otherwise it must align
/// with the message's `From` domain. Alignment is relaxed: the signing
/// domain equals the sender domain or is a parent or subdomain of it.
pub fn verify_dkim(
    raw_message: &[u8],
    sender_domain: Option<&str>,
    resolver: &dyn DkimTxtResolver,
) -> Result<(), DkimVerifyError> {
    let (headers, body) = parse_message(raw_message);
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
    let tags = parse_tags(&unfold(&signature_header.value));
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
    let (header_canon, body_canon) = canonicalization(tag("c").unwrap_or("simple/simple"))?;
    if let Some(expires) = tag("x").and_then(|value| value.trim().parse::<u64>().ok()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        if now > expires {
            return Err(failed("the DKIM signature has expired"));
        }
    }
    let mut canonical_body = canonicalize_body(body, body_canon);
    if let Some(length) = tag("l").and_then(|value| value.trim().parse::<usize>().ok()) {
        canonical_body.truncate(length);
    }
    let body_hash = BASE64.encode(Sha256::digest(&canonical_body));
    let claimed_hash = compact(tag("bh").ok_or_else(|| failed("DKIM-Signature has no bh= tag"))?);
    if body_hash != claimed_hash {
        return Err(failed("the DKIM body hash does not match the message body"));
    }
    let lookup_name = format!("{selector}._domainkey.{domain}");
    let records = resolver
        .lookup_txt(&lookup_name)
        .map_err(DkimVerifyError::Unavailable)?;
    let public_key = public_key_from_records(&records, &lookup_name)?;
    let header_data = signed_header_data(signature_header, headers, &signed_headers, header_canon);
    let signature_bytes = BASE64
        .decode(compact(
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
        let tags = parse_tags(record);
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
            .decode(compact(key_data))
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
    let value = unfold(&from.value);
    let address = value
        .split('<')
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .unwrap_or(&value);
    sender_domain_of(address)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Canon {
    Simple,
    Relaxed,
}

fn canonicalization(value: &str) -> Result<(Canon, Canon), DkimVerifyError> {
    let mut parts = value.split('/');
    let parse = |part: Option<&str>| match part.map(|part| part.trim().to_ascii_lowercase()) {
        None => Ok(Canon::Simple),
        Some(ref name) if name == "simple" => Ok(Canon::Simple),
        Some(ref name) if name == "relaxed" => Ok(Canon::Relaxed),
        Some(other) => Err(failed(format!(
            "unsupported DKIM canonicalization `{other}`"
        ))),
    };
    Ok((parse(parts.next())?, parse(parts.next())?))
}

struct Header {
    name: String,
    /// Everything after the colon, with folding (CRLF + WSP) preserved.
    value: String,
    /// `name:value` exactly as received, minus the terminating CRLF.
    raw: String,
}

fn parse_message(raw: &[u8]) -> (Vec<Header>, &[u8]) {
    let (header_bytes, body) = split_headers(raw);
    let text = String::from_utf8_lossy(header_bytes).replace("\r\n", "\n");
    let mut headers: Vec<Header> = Vec::new();
    for line in text.split('\n') {
        if line.starts_with([' ', '\t'])
            && let Some(current) = headers.last_mut()
        {
            current.raw.push_str("\r\n");
            current.raw.push_str(line);
            current.value.push_str("\r\n");
            current.value.push_str(line);
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push(Header {
            name: name.to_string(),
            value: value.to_string(),
            raw: line.to_string(),
        });
    }
    (headers, body)
}

fn split_headers(raw: &[u8]) -> (&[u8], &[u8]) {
    for (needle, skip) in [(b"\r\n\r\n".as_slice(), 4usize), (b"\n\n".as_slice(), 2)] {
        if let Some(index) = raw
            .windows(needle.len())
            .position(|window| window == needle)
        {
            return (&raw[..index], &raw[index + skip..]);
        }
    }
    (raw, &[])
}

fn unfold(value: &str) -> String {
    value.replace("\r\n", "").replace('\n', "")
}

fn compact(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn parse_tags(value: &str) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    for part in value.split(';') {
        let Some((name, tag_value)) = part.split_once('=') else {
            continue;
        };
        tags.insert(
            name.trim().to_ascii_lowercase(),
            tag_value.trim().to_string(),
        );
    }
    tags
}

fn canonicalize_header(header: &Header, canon: Canon) -> String {
    match canon {
        Canon::Simple => header.raw.clone(),
        Canon::Relaxed => {
            let name = header.name.trim().to_ascii_lowercase();
            let value = compress_wsp(&unfold(&header.value));
            format!("{name}:{}", value.trim())
        }
    }
}

fn compress_wsp(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_wsp = false;
    for ch in value.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_wsp {
                out.push(' ');
            }
            in_wsp = true;
        } else {
            out.push(ch);
            in_wsp = false;
        }
    }
    out
}

fn signed_header_data(
    signature_header: &Header,
    headers: &[Header],
    signed_names: &[String],
    canon: Canon,
) -> Vec<u8> {
    let mut used = vec![false; headers.len()];
    let mut data = String::new();
    for name in signed_names {
        // Bottom-up: the last header of this name not yet consumed.
        let index = headers
            .iter()
            .enumerate()
            .rev()
            .find(|(index, header)| !used[*index] && header.name.trim().eq_ignore_ascii_case(name))
            .map(|(index, _)| index);
        let Some(index) = index else { continue };
        used[index] = true;
        data.push_str(&canonicalize_header(&headers[index], canon));
        data.push_str("\r\n");
    }
    let unsigned = Header {
        name: signature_header.name.clone(),
        value: empty_b_tag(&signature_header.value),
        raw: empty_b_tag(&signature_header.raw),
    };
    data.push_str(&canonicalize_header(&unsigned, canon));
    data.into_bytes()
}

/// The DKIM-Signature value (or raw header) with the `b=` tag's value
/// removed, as it stood when the signer hashed it. RFC 6376 §3.7: the
/// value and all surrounding whitespace are deleted, so whitespace after
/// `=` (including folding CRLF + WSP) and before the following `;` or the
/// end of the value must not remain — under relaxed canonicalisation it
/// would otherwise compress to a space the signer never hashed, and
/// under simple canonicalisation it changes the signed bytes outright.
/// Legal whitespace around the tag syntax itself (before the tag name,
/// between the name and `=`, after the separating `;`) is not adjacent
/// to the value and is preserved. The tag is located on a tag boundary
/// (start, or a `;`/`:` once intervening whitespace is skipped) with
/// optional whitespace between `b` and `=`, so `bh=` and `b=` sequences
/// inside other tag values are not mistaken for the signature tag.
fn empty_b_tag(value: &str) -> String {
    let mut search_from = 0usize;
    while search_from < value.len() {
        let Some(found) = value[search_from..].find(['b', 'B']) else {
            return value.to_string();
        };
        let tag_start = search_from + found;
        let boundary = value[..tag_start]
            .chars()
            .rev()
            .find(|ch| !ch.is_whitespace())
            .is_none_or(|ch| ch == ';' || ch == ':');
        if boundary {
            let after_name = tag_start + 1;
            let equals = value[after_name..]
                .char_indices()
                .find(|(_, ch)| !ch.is_whitespace())
                .filter(|(_, ch)| *ch == '=')
                .map(|(index, _)| after_name + index);
            if let Some(equals) = equals {
                let value_start = equals + 1;
                let value_end = value[value_start..]
                    .find(';')
                    .map(|index| value_start + index)
                    .unwrap_or(value.len());
                return format!("{}{}", &value[..=equals], &value[value_end..]);
            }
        }
        search_from = tag_start + 1;
    }
    value.to_string()
}

fn canonicalize_body(body: &[u8], canon: Canon) -> Vec<u8> {
    let text = String::from_utf8_lossy(body).replace("\r\n", "\n");
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    // A trailing newline splits into a spurious final empty line.
    if matches!(lines.last(), Some(line) if line.is_empty()) {
        lines.pop();
    }
    let mut lines: Vec<String> = match canon {
        Canon::Simple => lines,
        Canon::Relaxed => lines
            .into_iter()
            .map(|line| compress_wsp(&line).trim_end().to_string())
            .collect(),
    };
    while matches!(lines.last(), Some(line) if line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return match canon {
            Canon::Simple => b"\r\n".to_vec(),
            Canon::Relaxed => Vec::new(),
        };
    }
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// The production resolver: a minimal DNS TXT query over UDP to the
/// nameservers in `/etc/resolv.conf` (falling back to 1.1.1.1 when none is
/// configured), with a bounded per-server timeout. It parses only what a
/// DKIM selector lookup needs — one question, TXT answers, name
/// compression — so no DNS crate enters the supply chain for this.
pub struct SystemDkimTxtResolver {
    timeout: Duration,
}

impl Default for SystemDkimTxtResolver {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
        }
    }
}

impl DkimTxtResolver for SystemDkimTxtResolver {
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        let servers = system_nameservers();
        let mut last_error = String::from("no nameserver is configured");
        for server in servers {
            match query_txt(server, name, self.timeout) {
                Ok(records) => return Ok(records),
                Err(error) => last_error = error,
            }
        }
        Err(format!("DNS TXT lookup for `{name}` failed: {last_error}"))
    }
}

fn system_nameservers() -> Vec<SocketAddr> {
    let mut servers = Vec::new();
    if let Ok(body) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in body.lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("nameserver")
                && let Some(address) = fields.next().and_then(|field| field.parse::<IpAddr>().ok())
            {
                servers.push(SocketAddr::new(address, 53));
            }
        }
    }
    if servers.is_empty() {
        servers.push(SocketAddr::new(IpAddr::from([1, 1, 1, 1]), 53));
    }
    servers.truncate(3);
    servers
}

fn query_txt(server: SocketAddr, name: &str, timeout: Duration) -> Result<Vec<String>, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0)
    } else {
        SocketAddr::new(IpAddr::from([0u16; 8]), 0)
    };
    let socket = UdpSocket::bind(bind).map_err(|error| format!("bind UDP socket: {error}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .connect(server)
        .map_err(|error| format!("connect to {server}: {error}"))?;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() ^ u32::from(std::process::id() as u16))
        .unwrap_or(0x5eed);
    let id = (seed & 0xffff) as u16;
    let query = build_query(id, name)?;
    socket
        .send(&query)
        .map_err(|error| format!("send DNS query: {error}"))?;
    let mut buffer = vec![0u8; 4096];
    let received = socket
        .recv(&mut buffer)
        .map_err(|error| format!("receive DNS answer: {error}"))?;
    buffer.truncate(received);
    parse_txt_answer(&buffer, id)
}

fn build_query(id: u16, name: &str) -> Result<Vec<u8>, String> {
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    query.extend_from_slice(&1u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("invalid DNS name `{name}`"));
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0);
    query.extend_from_slice(&16u16.to_be_bytes()); // TXT
    query.extend_from_slice(&1u16.to_be_bytes()); // IN
    Ok(query)
}

fn read_u16(packet: &[u8], offset: usize) -> Result<u16, String> {
    packet
        .get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| "truncated DNS packet".to_string())
}

fn skip_name(packet: &[u8], mut offset: usize) -> Result<usize, String> {
    loop {
        let length = *packet
            .get(offset)
            .ok_or_else(|| "truncated DNS name".to_string())? as usize;
        if length == 0 {
            return Ok(offset + 1);
        }
        if length & 0xc0 == 0xc0 {
            return Ok(offset + 2);
        }
        offset = offset
            .checked_add(length + 1)
            .filter(|next| *next <= packet.len())
            .ok_or_else(|| "DNS name overruns the packet".to_string())?;
    }
}

fn parse_txt_answer(packet: &[u8], expected_id: u16) -> Result<Vec<String>, String> {
    if packet.len() < 12 {
        return Err("DNS answer is shorter than a header".to_string());
    }
    if read_u16(packet, 0)? != expected_id {
        return Err("DNS answer id does not match the query".to_string());
    }
    let flags = read_u16(packet, 2)?;
    let rcode = flags & 0x000f;
    if rcode == 3 {
        return Ok(Vec::new()); // NXDOMAIN: no key record exists.
    }
    if rcode != 0 {
        return Err(format!("DNS server returned rcode {rcode}"));
    }
    let questions = read_u16(packet, 4)? as usize;
    let answers = read_u16(packet, 6)? as usize;
    let mut offset = 12usize;
    for _ in 0..questions {
        offset = skip_name(packet, offset)?
            .checked_add(4)
            .ok_or("overflow")?;
    }
    let mut records = Vec::new();
    for _ in 0..answers {
        offset = skip_name(packet, offset)?;
        let record_type = read_u16(packet, offset)?;
        let data_len = read_u16(packet, offset + 8)? as usize;
        let data_start = offset + 10;
        let data_end = data_start
            .checked_add(data_len)
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| "DNS answer overruns the packet".to_string())?;
        if record_type == 16 {
            let mut record = Vec::new();
            let mut cursor = data_start;
            while cursor < data_end {
                let chunk = packet[cursor] as usize;
                cursor += 1;
                let chunk_end = cursor
                    .checked_add(chunk)
                    .filter(|end| *end <= data_end)
                    .ok_or_else(|| "TXT string overruns its record".to_string())?;
                record.extend_from_slice(&packet[cursor..chunk_end]);
                cursor = chunk_end;
            }
            records.push(String::from_utf8_lossy(&record).to_string());
        }
        offset = data_end;
    }
    Ok(records)
}
