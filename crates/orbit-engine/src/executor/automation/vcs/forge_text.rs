//! Neutralize accidental account mentions in text Orbit publishes to a forge.

/// Keep the visible wording and Markdown intact by separating `@` from a
/// mention name with a zero-width joiner. The same representation works in
/// plain-text titles/commit subjects and prose, and repeated rendering is safe.
/// Emails with a dotted domain, `scheme://` URLs, Markdown link destinations,
/// co-author trailers and Markdown code are kept.
pub(super) fn neutralize_mentions(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut fence = None;
    let mut prose_start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let protected = if let Some(open) = fence {
            if closes_fence(line, open) {
                fence = None;
            }
            true
        } else if let Some(open) = opens_fence(line) {
            fence = Some(open);
            true
        } else {
            line.trim().is_empty()
                || line
                    .trim_start()
                    .get(..15)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Co-authored-by:"))
        };
        if protected {
            append_neutralized_prose(&text[prose_start..offset], &mut output);
            output.push_str(line);
            prose_start = offset + line.len();
        }
        offset += line.len();
    }
    append_neutralized_prose(&text[prose_start..], &mut output);
    output
}

/// Backtick runs pair only with a run of the same length. Backslashes escape
/// openers in prose, but are literal within code spans. Unmatched runs remain
/// prose so an agent's stray backtick cannot suppress mention protection.
/// An `@` inside a bare `scheme://` URL or a Markdown link destination `](...)`
/// is part of an address rather than a mention, so it is left unchanged. A
/// destination only starts after a matched `[label]`, a bare URL only after a
/// valid scheme, and both exemptions end when their span closes, so a mention
/// immediately after a closed destination (or in malformed prose such as
/// `://@x` or `](@x)`) is still neutralized.
fn append_neutralized_prose(prose: &str, output: &mut String) {
    let mut rest = prose;
    let mut in_url = false;
    let mut url_paren_depth = 0_usize;
    let mut destination_depth = 0_usize;
    let mut label_depth = 0_usize;
    while !rest.is_empty() {
        if rest.starts_with('`') {
            let run = rest.bytes().take_while(|byte| *byte == b'`').count();
            let mut end = run;
            let mut close_end = None;
            while let Some(next) = rest[end..].find('`') {
                end += next;
                let close = rest[end..].bytes().take_while(|byte| *byte == b'`').count();
                end += close;
                if close == run {
                    close_end = Some(end);
                    break;
                }
            }
            if let Some(end) = close_end {
                output.push_str(&rest[..end]);
                rest = &rest[end..];
                continue;
            }
            output.push_str(&rest[..run]);
            rest = &rest[run..];
            continue;
        }
        if destination_depth == 0 && label_depth > 0 && rest.starts_with("](") {
            output.push_str("](");
            destination_depth = 1;
            label_depth -= 1;
            rest = &rest[2..];
            continue;
        }
        let Some(character) = rest.chars().next() else {
            break;
        };
        if rest.starts_with("://") && has_scheme_before(prose, prose.len() - rest.len()) {
            in_url = true;
            url_paren_depth = 0;
        }
        if character.is_whitespace() || matches!(character, '<' | '>') {
            in_url = false;
        }
        if in_url && destination_depth == 0 {
            // A `)` with no matching `(` inside the URL is prose punctuation
            // closing a parenthesized URL, so the URL span ends there.
            match character {
                '(' => url_paren_depth += 1,
                ')' if url_paren_depth > 0 => url_paren_depth -= 1,
                ')' => in_url = false,
                _ => {}
            }
        }
        if destination_depth > 0 {
            match character {
                '(' => destination_depth += 1,
                ')' => {
                    destination_depth -= 1;
                    if destination_depth == 0 {
                        in_url = false;
                    }
                }
                _ if character.is_whitespace() => destination_depth = 0,
                _ => {}
            }
        } else {
            match character {
                '[' => label_depth += 1,
                ']' if label_depth > 0 => label_depth -= 1,
                _ => {}
            }
        }
        output.push(character);
        if character == '\\' {
            // An escaped backtick is not a code-span opener. Still handle an
            // escaped @ normally: GitHub need not treat it as a code span.
            // Escaped brackets/parens likewise open no link or destination.
            if rest[1..].starts_with(['`', '\\', '[', ']', '(', ')']) {
                output.push_str(&rest[1..2]);
                rest = &rest[2..];
                continue;
            }
        } else if character == '@'
            && !in_url
            && destination_depth == 0
            && rest
                .as_bytes()
                .get(1)
                .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_')
            && !is_email_at(prose, prose.len() - rest.len())
        {
            output.push('\u{200d}');
        }
        rest = &rest[character.len_utf8()..];
    }
}

/// Whether the `:` at `colon` in `text` is preceded by a URL scheme: a
/// letter followed by letters, digits, `+`, `-` or `.`. Bare `://` in prose
/// has no scheme and creates no URL exemption.
fn has_scheme_before(text: &str, colon: usize) -> bool {
    let bytes = &text.as_bytes()[..colon];
    let mut start = bytes.len();
    while start > 0
        && (bytes[start - 1].is_ascii_alphanumeric()
            || matches!(bytes[start - 1], b'+' | b'-' | b'.'))
    {
        start -= 1;
    }
    bytes.get(start).is_some_and(u8::is_ascii_alphabetic)
}

/// A crew reference such as `opus@auto` is deliberately not an email: a
/// domain must have at least two nonempty labels. Accept local-part symbols
/// and international letters without interpreting surrounding Markdown.
fn is_email_at(text: &str, at: usize) -> bool {
    let local = text[..at].chars().next_back();
    if !local.is_some_and(|c| c.is_alphanumeric() || ".!#$%&'*+-/=?^_`{|}~\"".contains(c)) {
        return false;
    }
    let domain = &text[at + 1..];
    let end = domain
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.'))
        .unwrap_or(domain.len());
    let domain = domain[..end].trim_end_matches('.');
    domain.contains('.')
        && domain.split('.').all(|label| {
            label.chars().next().is_some_and(char::is_alphanumeric)
                && label.chars().next_back().is_some_and(char::is_alphanumeric)
        })
}

/// A fenced code block's delimiter run: the marker character and its length.
/// A block closes only on a run of the same marker at least as long.
#[derive(Clone, Copy)]
pub(super) struct Fence {
    marker: char,
    len: usize,
}

/// The fence a template line opens. A backtick fence's info string cannot
/// hold a backtick, else the line is inline code.
pub(super) fn opens_fence(line: &str) -> Option<Fence> {
    let (fence, info) = fence_run(line)?;
    (fence.marker != '`' || !info.contains('`')).then_some(fence)
}

/// Whether a template line closes the open fence: the same marker, a run at
/// least as long as the opener, and nothing but whitespace after the run.
pub(super) fn closes_fence(line: &str, open: Fence) -> bool {
    fence_run(line).is_some_and(|(fence, info)| {
        fence.marker == open.marker && fence.len >= open.len && info.trim().is_empty()
    })
}

/// The delimiter run a line starts with and the text after it, when the line
/// indents at most three spaces, as an ATX heading does.
fn fence_run(line: &str) -> Option<(Fence, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    (len >= 3).then(|| (Fence { marker, len }, &trimmed[len..]))
}
