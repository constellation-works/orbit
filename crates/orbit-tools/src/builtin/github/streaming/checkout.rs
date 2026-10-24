use std::collections::HashSet;

use orbit_common::security::redaction::redact_all;
use orbit_common::text::ceil_char_boundary;

use super::model::{
    CHECKOUT_MARKERS, CheckoutEvidence, MAX_CHECKOUT_EVIDENCE_LINE_BYTES, MAX_SHA_LEN, MIN_SHA_LEN,
};

pub(super) struct CheckoutEvidenceCollector {
    max_lines: usize,
    scan_limit: usize,
    scanned_bytes: usize,
    source_truncated: bool,
    pending_line: String,
    dropping_line: bool,
    commits: Vec<String>,
    seen_commits: HashSet<String>,
    /// Of the observed tokens, the ones a line actually named as the commit
    /// that was checked out — as opposed to a merge parent or a fetched ref
    /// that happened to share the line.
    named_commits: HashSet<String>,
    /// A recognized command whose immediately following output line may name
    /// the checkout. Keep only one bounded line: an unrelated intervening
    /// line consumes it instead of letting a later SHA inherit its authority.
    pending_checkout_command: Option<String>,
    lines: Vec<String>,
    complete: bool,
    display_truncated: bool,
}

impl CheckoutEvidenceCollector {
    pub(super) fn new(max_lines: usize, scan_limit: usize) -> Self {
        Self {
            max_lines,
            scan_limit,
            scanned_bytes: 0,
            source_truncated: false,
            pending_line: String::new(),
            dropping_line: false,
            commits: Vec::new(),
            seen_commits: HashSet::new(),
            named_commits: HashSet::new(),
            pending_checkout_command: None,
            lines: Vec::new(),
            complete: true,
            display_truncated: false,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        let remaining = self.scan_limit.saturating_sub(self.scanned_bytes);
        let scanned = &chunk[..chunk.len().min(remaining)];
        self.scanned_bytes += scanned.len();
        if scanned.len() < chunk.len() {
            self.source_truncated = true;
            self.complete = false;
        }
        for part in String::from_utf8_lossy(scanned).split_inclusive('\n') {
            if self.dropping_line {
                if part.ends_with('\n') {
                    self.dropping_line = false;
                }
                continue;
            }
            if self.pending_line.len() + part.len() > MAX_CHECKOUT_EVIDENCE_LINE_BYTES {
                let identity_bearing = overlong_line_is_identity_bearing(&self.pending_line, part);
                self.pending_line.clear();
                self.dropping_line = !part.ends_with('\n');
                // A dropped physical line still consumes a preceding
                // `git log` command. Otherwise a later unrelated SHA could
                // inherit authority that belonged only to this line.
                self.pending_checkout_command = None;
                if identity_bearing {
                    self.complete = false;
                } else {
                    self.display_truncated = true;
                }
                continue;
            }
            self.pending_line.push_str(part);
            if self.pending_line.ends_with('\n') {
                self.observe_pending_line();
            }
        }
    }

    pub(super) fn finish(mut self) -> CheckoutEvidence {
        if !self.pending_line.is_empty() && !self.dropping_line {
            self.observe_pending_line();
        }
        CheckoutEvidence {
            commits: canonical_checkout_commits(&self.commits, &self.named_commits),
            lines: self.lines,
            complete: self.complete,
            scanned_bytes: self.scanned_bytes,
            source_truncated: self.source_truncated,
            display_truncated: self.display_truncated,
        }
    }

    fn observe_pending_line(&mut self) {
        let mut line = std::mem::take(&mut self.pending_line);
        self.observe_line(&line);
        line.clear();
        self.pending_line = line;
    }

    fn observe_line(&mut self, line: &str) {
        // Neither a checkout marker nor a recognized command nor a bare SHA
        // fits below this length. Even an unrelated short line consumes the
        // immediately preceding checkout command's output context.
        if line.len() < MIN_SHA_LEN {
            self.pending_checkout_command = None;
            return;
        }
        let (step, payload) = split_log_line(line);
        let lowered = payload.to_ascii_lowercase();
        let marked = CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker));
        let in_checkout_step = step.to_ascii_lowercase().contains("checkout");
        let command_output = self
            .pending_checkout_command
            .take()
            .filter(|_| is_bare_commit_sha(payload));
        let command = checkout_identity_command(payload);
        if !(marked || in_checkout_step && is_bare_commit_sha(payload) || command_output.is_some())
        {
            self.pending_checkout_command = command;
            return;
        }
        if let Some(command) = &command_output {
            self.record_evidence_line(command);
        }
        self.record_evidence_line(payload);
        let named = named_checkout_commit(
            payload,
            &lowered,
            in_checkout_step || command_output.is_some(),
        );
        for token in commit_sha_tokens(payload) {
            if named == Some(token) {
                self.named_commits.insert(token.to_string());
            }
            if self.seen_commits.contains(token) {
                continue;
            }
            if self.commits.len() == self.max_lines {
                self.display_truncated = true;
                continue;
            }
            let token = token.to_string();
            self.seen_commits.insert(token.clone());
            self.commits.push(token);
        }
        self.pending_checkout_command = command;
    }

    fn record_evidence_line(&mut self, payload: &str) {
        if self.lines.len() < self.max_lines {
            self.lines.push(redact_all(payload.trim()));
        } else {
            self.display_truncated = true;
        }
    }
}

/// Exclude runner metadata from credential context, retaining the original
/// payload bytes (including invalid UTF-8 and line endings) for byte accounting.
pub(super) fn runner_log_payload(line: &[u8]) -> &[u8] {
    let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
    let without_timestamp = |bytes: &[u8]| {
        bytes
            .iter()
            .position(|byte| *byte == b' ')
            .filter(|&space| bytes[..space].contains(&b'T') && bytes[..space].ends_with(b"Z"))
    };
    // Raw job-API lines may contain tabs in their payload. Recognize their
    // timestamp before interpreting tabs as job/step column separators.
    if let Some(space) = without_timestamp(line) {
        return &line[space + 1..];
    }
    let mut columns = line.splitn(3, |byte| *byte == b'\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    match without_timestamp(rest) {
        Some(space) => &rest[space + 1..],
        None => rest,
    }
}

/// Split one `gh run view --log` line into its step column and its payload.
///
/// The line format is `job<TAB>step<TAB>timestamp payload`. The split matters:
/// the *step* column names a pinned action (`actions/checkout@<sha>`), and
/// harvesting that SHA would report the checkout action's own commit as the
/// commit under test — the exact conflation this scan exists to prevent.
pub(super) fn split_log_line(line: &str) -> (&str, &str) {
    let line = line.trim_start_matches('\u{feff}');
    let mut columns = line.splitn(3, '\t');
    let (step, rest) = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(step), Some(rest)) => (step, rest),
        _ => ("", line),
    };
    // Drop the leading ISO-8601 runner timestamp when there is one.
    let payload = match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    };
    (step, payload)
}

/// Whether a line dropped for exceeding [`MAX_CHECKOUT_EVIDENCE_LINE_BYTES`]
/// could plausibly have carried checkout identity, judged from whatever
/// prefix was captured before the drop.
///
/// The job/step columns and runner timestamp sit at the very start of a
/// `gh run view --log` line, long before any payload could reach this
/// threshold, so the accumulated prefix (or, when nothing had accumulated yet,
/// a short probe from the start of the new part) is enough to classify it
/// without buffering the whole overlong line.
fn overlong_line_is_identity_bearing(pending: &str, part: &str) -> bool {
    let probe = if pending.is_empty() {
        let cap = ceil_char_boundary(part, part.len().min(512));
        &part[..cap]
    } else {
        pending
    };
    let (step, payload) = split_log_line(probe);
    let lowered = payload.to_ascii_lowercase();
    step.to_ascii_lowercase().contains("checkout")
        || CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

/// Whether a payload is a bare commit SHA on its own line.
///
/// `actions/checkout` prints the resolved commit this way, with no surrounding
/// prose, which is often the only place a branch-mode checkout records what it
/// actually landed on.
fn is_bare_commit_sha(payload: &str) -> bool {
    let trimmed = payload.trim();
    trimmed.len() == MAX_SHA_LEN && trimmed.bytes().all(is_hex)
}

/// Recognize the runner command that prints the checked-out `HEAD` as its
/// next line. This intentionally accepts only `git log -1 --format=%H`, not
/// a generic bare SHA after any command: ordinary test output and arbitrary
/// repository probes must not become checkout identity.
fn checkout_identity_command(payload: &str) -> Option<String> {
    let command = payload.strip_prefix("[command]")?.trim();
    let mut words = command.split_ascii_whitespace();
    let program = words.next()?;
    if program.rsplit('/').next()? != "git"
        || !matches!(words.next(), Some("log"))
        || !matches!(words.next(), Some("-1"))
        || !matches!(words.next(), Some("--format=%H"))
        || words.next().is_some()
    {
        return None;
    }
    Some(command.to_string())
}

/// The prose `actions/checkout` prints in front of the commit it landed on.
const HEAD_IS_NOW_AT: &str = "head is now at";

/// The commit this line *names* as checked out, if it names one.
///
/// `HEAD is now at 7dcd45b Merge 4968f13 into abc1234` reports one checked-out
/// commit and then quotes the merge's two parents inside the commit subject.
/// Only the token directly after the marker is identity; the rest is prose
/// that happens to be hex. Everything else — a fetched ref, a merge summary —
/// names no commit and stays incidental evidence.
fn named_checkout_commit<'a>(
    payload: &'a str,
    lowered: &str,
    in_checkout_step: bool,
) -> Option<&'a str> {
    if let Some(marker) = lowered.find(HEAD_IS_NOW_AT) {
        // `to_ascii_lowercase` rewrites ASCII bytes in place and leaves every
        // multi-byte sequence alone, so offsets into `lowered` index `payload`.
        return commit_sha_tokens(&payload[marker + HEAD_IS_NOW_AT.len()..])
            .into_iter()
            .next();
    }
    (in_checkout_step && is_bare_commit_sha(payload)).then(|| payload.trim())
}

/// Reduce the observed SHA tokens to the distinct commits actually checked out.
///
/// Two reductions, neither of which relaxes identity. A line that *names* the
/// checked-out commit outranks one that merely quotes a merge parent or a
/// fetched ref. An abbreviation and the full SHA it prefixes are one commit,
/// not two — the same runner routinely prints both forms. A real disagreement
/// between two checkout steps survives both reductions and is still reported
/// as two commits, and an abbreviation with more than one distinct expansion
/// is left abbreviated rather than resolved to a guess.
fn canonical_checkout_commits(observed: &[String], named: &HashSet<String>) -> Vec<String> {
    let identity: Vec<&String> = if observed.iter().any(|commit| named.contains(commit)) {
        observed
            .iter()
            .filter(|commit| named.contains(*commit))
            .collect()
    } else {
        observed.iter().collect()
    };

    let mut canonical: Vec<String> = Vec::new();
    for commit in identity {
        // `observed` holds distinct tokens, so two expansions of the same
        // length are two different commits and the abbreviation cannot be
        // resolved to either of them.
        let mut longest: Option<&String> = None;
        let mut contested = false;
        for candidate in observed
            .iter()
            .filter(|candidate| candidate.len() > commit.len() && candidate.starts_with(&**commit))
        {
            match longest {
                Some(current) if current.len() > candidate.len() => {}
                Some(current) if current.len() == candidate.len() => contested = true,
                _ => {
                    longest = Some(candidate);
                    contested = false;
                }
            }
        }
        let resolved = if contested {
            commit
        } else {
            longest.unwrap_or(commit)
        };
        if !canonical.contains(resolved) {
            canonical.push(resolved.clone());
        }
    }
    canonical
}

/// Collect every lowercase-hex token of commit-SHA length in `payload`.
///
/// A token preceded by `@` is skipped: that is the `owner/action@sha` pin
/// form, which identifies the action, not the commit under test.
fn commit_sha_tokens(payload: &str) -> Vec<&str> {
    let bytes = payload.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let token = &payload[start..index];
        let pinned = start > 0 && bytes[start - 1] == b'@';
        if !pinned
            && (MIN_SHA_LEN..=MAX_SHA_LEN).contains(&token.len())
            && token.bytes().all(is_hex)
        {
            tokens.push(token);
        }
    }
    tokens
}
