use std::collections::BTreeMap;

use super::model::DkimVerifyError;
use super::verification;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Canon {
    Simple,
    Relaxed,
}

pub(super) fn canonicalization(value: &str) -> Result<(Canon, Canon), DkimVerifyError> {
    let mut parts = value.split('/');
    let parse = |part: Option<&str>| match part.map(|part| part.trim().to_ascii_lowercase()) {
        None => Ok(Canon::Simple),
        Some(ref name) if name == "simple" => Ok(Canon::Simple),
        Some(ref name) if name == "relaxed" => Ok(Canon::Relaxed),
        Some(other) => Err(verification::failed(format!(
            "unsupported DKIM canonicalization `{other}`"
        ))),
    };
    Ok((parse(parts.next())?, parse(parts.next())?))
}

pub(super) struct Header {
    pub(super) name: String,
    /// Everything after the colon, with folding (CRLF + WSP) preserved.
    pub(super) value: String,
    /// `name:value` exactly as received, minus the terminating CRLF.
    pub(super) raw: String,
}

pub(super) fn parse_message(raw: &[u8]) -> (Vec<Header>, &[u8]) {
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

pub(super) fn unfold(value: &str) -> String {
    value.replace("\r\n", "").replace('\n', "")
}

pub(super) fn compact(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

pub(super) fn parse_tags(value: &str) -> BTreeMap<String, String> {
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
            let name = trim_wsp(&header.name).to_ascii_lowercase();
            let value = compress_wsp(&unfold(&header.value));
            format!("{name}:{}", trim_wsp(&value))
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

/// Trims the WSP (SP and HTAB) that relaxed canonicalisation deletes.
/// `str::trim` also strips Unicode whitespace such as U+00A0, which is
/// signed content and must reach the hash unchanged.
fn trim_wsp(value: &str) -> &str {
    value.trim_matches([' ', '\t'])
}

pub(super) fn signed_header_data(
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
            .find(|(index, header)| {
                !used[*index] && trim_wsp(&header.name).eq_ignore_ascii_case(name)
            })
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

pub(super) fn canonicalize_body(body: &[u8], canon: Canon) -> Vec<u8> {
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
            .map(|line| {
                compress_wsp(&line)
                    .trim_end_matches([' ', '\t'])
                    .to_string()
            })
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
