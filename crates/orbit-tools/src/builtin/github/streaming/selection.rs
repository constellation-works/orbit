use orbit_common::security::redaction::redact_all;

use super::model::WINDOW_TOKEN_CONTEXT_BYTES;

/// Inspect tokens with their original line prefix, including credential
/// introducers that a byte window may omit or cut. Only display bounds change;
/// source coordinates and selected counts continue to describe the raw stream.
#[derive(Default)]
pub(super) struct WindowBoundaryTokens {
    context: Vec<u8>,
    context_start: usize,
    token_start: usize,
    overlong: bool,
    pub(super) omit_prefix_until: usize,
    omit_suffix_from: Option<usize>,
}

impl WindowBoundaryTokens {
    pub(super) fn push(&mut self, chunk: &[u8], start: usize, from: usize, to: Option<usize>) {
        let mut offset = start;
        for part in chunk.split_inclusive(u8::is_ascii_whitespace) {
            // The token crossing the end still needs its complete source.
            if to.is_some_and(|end| self.token_start >= end) {
                break;
            }
            let whitespace = part.last().copied().filter(u8::is_ascii_whitespace);
            let token_part = &part[..part.len() - usize::from(whitespace.is_some())];
            self.extend_context(token_part);
            offset = offset.saturating_add(token_part.len());
            if let Some(whitespace) = whitespace {
                self.finish_token(offset, from, to);
                offset = offset.saturating_add(1);
                self.token_start = offset;
                // Bearer/header/assignment whitespace can cross a newline.
                // Keep a pending introducer, still under the same byte cap.
                let line_break = matches!(whitespace, b'\n' | b'\r');
                let pending_introducer = line_break
                    && self
                        .context
                        .split(u8::is_ascii_whitespace)
                        .rfind(|part| !part.is_empty())
                        .is_some_and(|part| {
                            part.eq_ignore_ascii_case(b"bearer")
                                || matches!(part.last(), Some(b':' | b'='))
                        });
                if line_break && !pending_introducer {
                    self.context.clear();
                    self.context_start = offset;
                    self.overlong = false;
                } else {
                    self.extend_context(&[whitespace]);
                }
            }
        }
    }

    fn extend_context(&mut self, bytes: &[u8]) {
        let take = WINDOW_TOKEN_CONTEXT_BYTES
            .saturating_sub(self.context.len())
            .min(bytes.len());
        self.context.extend_from_slice(&bytes[..take]);
        self.overlong |= take < bytes.len();
    }

    pub(super) fn finish_token(&mut self, end: usize, from: usize, to: Option<usize>) {
        if self.token_start == end || end <= from || to.is_some_and(|to| self.token_start >= to) {
            return;
        }
        let cuts_end = to.is_some_and(|to| self.token_start < to && to < end);
        let lost_prefix = self.context_start < from;
        if !cuts_end && !lost_prefix {
            return;
        }
        let token_index = self
            .token_start
            .saturating_sub(self.context_start)
            .min(self.context.len());
        let prefix = String::from_utf8_lossy(&self.context[..token_index]);
        let token = String::from_utf8_lossy(&self.context[token_index..]);
        let sensitive = self.overlong || Self::context_redacts_token(&prefix, &token);
        if !sensitive {
            return;
        }
        if cuts_end {
            self.omit_suffix_from = Some(
                self.omit_suffix_from
                    .map_or(self.token_start, |old| old.min(self.token_start)),
            );
        }
        if lost_prefix {
            // A cut in an introducer can leave the following value whole yet
            // unrecognizable. Check whether the retained prefix still gives
            // the redactor enough context before preserving that value.
            let visible_index = from.saturating_sub(self.context_start).min(token_index);
            let visible_prefix = String::from_utf8_lossy(&self.context[visible_index..token_index]);
            if self.overlong
                || self.token_start < from
                || !Self::context_redacts_token(&visible_prefix, &token)
            {
                self.omit_prefix_until = end;
            }
        }
    }

    fn context_redacts_token(prefix: &str, token: &str) -> bool {
        // Assignments are credential introducers even when the generic text
        // redactor only recognizes JSON/header spellings. The separator may
        // precede this token (key = value) or belong to it (key=value/key =value).
        let assignment_key = if let Some(key) = prefix.trim_end().strip_suffix('=') {
            key.split_whitespace().last()
        } else {
            token.split_once('=').and_then(|(key, _)| {
                if key.is_empty() {
                    prefix.split_whitespace().last()
                } else {
                    Some(key)
                }
            })
        };
        let api_key_assignment = assignment_key.is_some_and(|key| {
            let key = key.trim_matches(['\'', '"', '[', ']', '{', '}', '(', ')']);
            // Normalize separators and case for one suffix check, while
            // retaining source positions to distinguish `capiKey` from a
            // prefixed API-key segment.
            let normalized: Vec<_> = key
                .char_indices()
                .filter_map(|(index, character)| {
                    character
                        .is_ascii_alphanumeric()
                        .then_some((character.to_ascii_lowercase(), index))
                })
                .collect();
            if normalized.len() < 6
                || !normalized
                    .iter()
                    .map(|(character, _)| *character)
                    .collect::<String>()
                    .ends_with("apikey")
            {
                return false;
            }

            let api_start_in_normalized = normalized.len() - 6;
            if api_start_in_normalized == 0 {
                return true;
            }
            let api_start = normalized[api_start_in_normalized].1;
            let prefix_end = normalized[api_start_in_normalized - 1].1;
            let previous = key.as_bytes()[prefix_end];
            let api_a = key.as_bytes()[api_start];
            let api_p = key.as_bytes()[normalized[api_start_in_normalized + 1].1];
            let separated = key[prefix_end + 1..api_start]
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric());
            let camel_case_boundary = api_a.is_ascii_uppercase()
                && ((previous.is_ascii_lowercase() || previous.is_ascii_digit())
                    || (previous.is_ascii_uppercase() && api_p.is_ascii_lowercase()));
            let uppercase_api = normalized[api_start_in_normalized..api_start_in_normalized + 3]
                .iter()
                .all(|(_, index)| key.as_bytes()[*index].is_ascii_uppercase());

            // Accept separator-delimited keys, case transitions (including
            // an acronym followed by `Api`), and an all-caps `API` segment.
            separated || camel_case_boundary || uppercase_api
        });
        api_key_assignment
            || redact_all(&format!("{prefix}{token}")) != format!("{}{token}", redact_all(prefix))
    }

    pub(super) fn safe_slice<'a>(
        &self,
        bytes: &'a [u8],
        offset: usize,
        window_start: usize,
    ) -> &'a [u8] {
        let start = self.omit_prefix_until.saturating_sub(window_start);
        let end = self
            .omit_suffix_from
            .map_or(usize::MAX, |end| end.saturating_sub(window_start));
        let from = start.saturating_sub(offset).min(bytes.len());
        let to = end.saturating_sub(offset).min(bytes.len());
        &bytes[from.min(to)..to]
    }
}

/// Whether `adjacent` belongs to the same whitespace-delimited token as the
/// excerpt byte on the other side of the cut.
pub(super) fn cut_continues_token(adjacent: Option<u8>) -> bool {
    adjacent.is_some_and(|byte| byte.is_ascii() && !byte.is_ascii_whitespace())
}

/// Drop a token split by the end of the retained head.
///
/// Pattern redaction matches a whole credential (`ghp_` plus 36 token
/// characters, and the other fixed shapes). The head cut keeps a prefix of
/// that credential and the tail cut keeps the suffix without its prefix, so
/// neither window matches. Removing the partial token leaves every complete
/// token in the window for `redact_all`.
pub(super) fn excerpt_without_partial_suffix(bytes: &[u8], cut_token: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    if !cut_token {
        return text.into_owned();
    }
    match text.rfind(char::is_whitespace) {
        Some(index) => {
            let len = text[index..].chars().next().map_or(0, char::len_utf8);
            text[..index + len].to_string()
        }
        None => String::new(),
    }
}

/// Drop a token split by the start of the retained tail. See
/// [`excerpt_without_partial_suffix`].
pub(super) fn excerpt_without_partial_prefix(bytes: &[u8], cut_token: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    if !cut_token {
        return text.into_owned();
    }
    match text.find(char::is_whitespace) {
        Some(index) => text[index..].to_string(),
        None => String::new(),
    }
}

/// Drop a partial tail token, then a following whole token whose introducer
/// the excerpt cut left in the display lookbehind.
///
/// `redact_all` only matches a value while its introducer is still in the
/// same window. A value that is already a whole token at the tail start
/// (the preceding byte is whitespace, or the cut token was only the
/// introducer's suffix) would otherwise be returned in clear. Tokens that
/// the redactor or the assignment check recognize on their own stay, so a
/// complete credential is masked in place instead of disappearing. A newline
/// reset before that token drops the lookbehind, so ordinary text after a
/// long non-credential span stays readable.
pub(super) fn excerpt_without_orphaned_tail_token(bytes: &[u8], lookbehind: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let continues = cut_continues_token(lookbehind.last().copied());
    if continues && !text.contains(char::is_whitespace) {
        return String::new();
    }
    let split_at = if continues {
        text.find(char::is_whitespace).unwrap_or(0)
    } else {
        0
    };
    let partial = &text[..split_at];
    let rest = &text[split_at..];
    let Some(ws_len) = rest.find(|character: char| !character.is_whitespace()) else {
        return rest.to_string();
    };
    let prefix = tail_token_prefix(lookbehind, partial, &rest[..ws_len]);
    // Walk an introducer chain (e.g. `Authorization: Bearer <value>` left as
    // `Bearer <value>`, or `api_key` left as `= <value>`). Dropping only the
    // first token would strand the credential value without its introducer,
    // so `redact_all` in `finish` could no longer mask it. Drop every
    // introducer token and the value that follows it; a non-introducer
    // sensitive token is the value itself, so the walk stops after it.
    let mut chain_prefix = prefix;
    let mut cursor = 0usize;
    let mut dropped_sensitive = false;
    loop {
        let remaining = &rest[cursor..];
        let Some(next_ws) = remaining.find(|character: char| !character.is_whitespace()) else {
            return if dropped_sensitive {
                remaining.to_string()
            } else {
                rest.to_string()
            };
        };
        let body = &remaining[next_ws..];
        let token_len = body.find(char::is_whitespace).unwrap_or(body.len());
        let token = &body[..token_len];
        let after_token = &body[token_len..];
        let sensitive_alone = WindowBoundaryTokens::context_redacts_token("", token);
        if sensitive_alone {
            // A token the redactor masks on its own stays (in place) when it
            // is first; after an introducer was dropped it also stays, as
            // the redactor still masks it without the introducer.
            return if cursor == 0 {
                rest.to_string()
            } else {
                remaining.to_string()
            };
        }
        let sensitive_with_introducer =
            WindowBoundaryTokens::context_redacts_token(&chain_prefix, token);
        let introducer = token_is_introducer(token);
        if !sensitive_with_introducer && !introducer {
            return if dropped_sensitive {
                remaining.to_string()
            } else {
                rest.to_string()
            };
        }
        if !sensitive_with_introducer && cursor > 0 && !dropped_sensitive {
            // A tentative introducer chain that does not lead to a sensitive
            // value (only reachable for a leading api-key key token) keeps
            // the original tail.
            return rest.to_string();
        }
        if !sensitive_with_introducer && cursor == 0 && !token_is_api_key_key(token) {
            // The visible lookbehind has to be what makes the token
            // sensitive. Guessing at an introducer beyond the retained
            // window would also drop ordinary tail markers.
            return rest.to_string();
        }
        // This token is dropped: either it is sensitive with its introducer,
        // or it is an introducer (possibly a leading api-key key) whose value
        // still has to be checked against the extended chain.
        dropped_sensitive |= sensitive_with_introducer;
        if !introducer {
            return after_token.to_string();
        }
        let after_ws = after_token
            .find(|character: char| !character.is_whitespace())
            .unwrap_or(after_token.len());
        chain_prefix = format!("{chain_prefix}{token}{}", &after_token[..after_ws]);
        cursor += next_ws + token_len;
        if after_token.is_empty() {
            return String::new();
        }
    }
}

/// Whether `token` can introduce a credential value rather than be one:
/// an auth scheme, a `key:`/`key=` separator, a bare `=`/`:` separator, or
/// an api-key/authorization key name.
fn token_is_introducer(token: &str) -> bool {
    let stripped = token.trim_matches(|c| matches!(c, '"' | '\'' | '`' | ',' | ';'));
    let core = stripped.trim_matches(|c| matches!(c, '[' | ']' | '{' | '}' | '(' | ')'));
    if core.eq_ignore_ascii_case("bearer") || core.eq_ignore_ascii_case("basic") {
        return true;
    }
    if core == "=" || core == ":" || core.ends_with(':') || core.ends_with('=') {
        return true;
    }
    token_is_api_key_key(token)
        || core.eq_ignore_ascii_case("authorization")
        || core.eq_ignore_ascii_case("x-api-key")
}

/// Whether `token` names an api-key key (any spelling the assignment check
/// in [`WindowBoundaryTokens::context_redacts_token`] accepts), e.g.
/// `api_key`, `apiKey`, `"api_key":`.
fn token_is_api_key_key(token: &str) -> bool {
    let key = token.trim_matches(|c| {
        matches!(
            c,
            '"' | '\'' | '`' | ',' | ';' | '[' | ']' | '{' | '}' | '(' | ')' | ':' | '='
        )
    });
    let normalized: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    normalized == "apikey" || normalized.ends_with("apikey")
}

/// Line-prefix context that applies to the tail's first whole token.
///
/// `partial` is the cut token's suffix, already removed from the tail.
/// `leading_ws` is the whitespace that separates it from the next token,
/// including a newline that resets context unless the cut token is a
/// pending introducer.
fn tail_token_prefix(lookbehind: &[u8], partial: &str, leading_ws: &str) -> String {
    let mut tokens = WindowBoundaryTokens::default();
    if !lookbehind.is_empty() {
        tokens.push(lookbehind, 0, 0, None);
    }
    let mut extra = Vec::with_capacity(partial.len() + leading_ws.len());
    extra.extend_from_slice(partial.as_bytes());
    extra.extend_from_slice(leading_ws.as_bytes());
    if !extra.is_empty() {
        tokens.push(&extra, lookbehind.len(), 0, None);
    }
    String::from_utf8_lossy(&tokens.context).into_owned()
}
