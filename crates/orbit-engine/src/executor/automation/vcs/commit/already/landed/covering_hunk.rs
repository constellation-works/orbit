//! Content proof for a scoped file edited after a covering delivery.

use std::ops::Range;
use std::path::Path;

use orbit_common::OrbitError;

use crate::executor::automation::vcs::git;

struct FileHunk {
    before: Range<usize>,
    after: Range<usize>,
}

/// Protect the postimage of each hunk, including context at a deletion seam.
/// Each before/after image must be unique. Verify against each later version
/// of the file descended from the covering commit so an edit followed by a restore
/// cannot hide behind a disjoint net diff.
pub(super) fn covering_hunks_survive(
    env: &orbit_common::Env,
    workspace: &Path,
    covering: &str,
    head: &str,
    path: &str,
    head_changed: bool,
) -> Result<bool, OrbitError> {
    const MAX_COVERING_HUNKS: usize = 128;
    const MAX_LATER_HUNKS: usize = 1024;
    const MAX_LATER_REVISIONS: usize = 32;

    let pathspec = format!(":(top,literal){path}");
    let revisions = git::git_output(
        env,
        workspace,
        &[
            "--no-replace-objects",
            "rev-list",
            "--ancestry-path",
            "--full-history",
            "--ignore-submodules=none",
            &format!("--max-count={}", MAX_LATER_REVISIONS + 1),
            &format!("{covering}..{head}"),
            "--",
            &pathspec,
        ],
    )?;
    if revisions.lines().count() > MAX_LATER_REVISIONS {
        return Ok(false);
    }
    let mut later_edits = Vec::new();
    for revision in std::iter::once(head)
        .filter(|_| head_changed)
        .chain(revisions.lines().filter(|rev| *rev != head))
    {
        let Some(edits) = file_hunks(env, workspace, covering, revision, &pathspec, "--unified=0")?
        else {
            return Ok(false);
        };
        if (revision == head && edits.is_empty())
            || later_edits.len() + edits.len() > MAX_LATER_HUNKS
        {
            return Ok(false);
        }
        later_edits.extend(edits.into_iter().map(|edit| edit.before));
    }
    if later_edits.is_empty() {
        return Ok(true);
    }

    let parent = format!("{covering}^1");
    let Some(guards) = file_hunks(env, workspace, &parent, covering, &pathspec, "--unified=3")?
    else {
        return Ok(false);
    };
    if guards.is_empty() || guards.len() > MAX_COVERING_HUNKS {
        return Ok(false);
    }
    if later_edits.iter().any(|edit| {
        guards.iter().any(|guard| {
            let guard = &guard.after;
            if edit.is_empty() {
                (guard.start..=guard.end).contains(&edit.start)
            } else {
                edit.start < guard.end && guard.start < edit.end
            }
        })
    }) {
        return Ok(false);
    }
    let (Some(before), Some(after), Some(current)) = (
        read_proof_blob(env, workspace, &parent, path)?,
        read_proof_blob(env, workspace, covering, path)?,
        read_proof_blob(env, workspace, head, path)?,
    ) else {
        return Ok(false);
    };
    let before_offsets = line_offsets(&before);
    let after_offsets = line_offsets(&after);
    Ok(guards.iter().all(|guard| {
        hunk_image(&before, &before_offsets, &guard.before)
            .is_some_and(|image| unique_image(&before, image))
            && hunk_image(&after, &after_offsets, &guard.after)
                .is_some_and(|image| unique_image(&after, image) && unique_image(&current, image))
    }))
}

fn read_proof_blob(
    env: &orbit_common::Env,
    workspace: &Path,
    revision: &str,
    path: &str,
) -> Result<Option<String>, OrbitError> {
    let object = format!("{revision}:{path}");
    let size = git::git_output(
        env,
        workspace,
        &["--no-replace-objects", "cat-file", "-s", &object],
    )?;
    // Bound the exact byte reads used to decide whether a hunk has one match.
    if !size.parse::<usize>().is_ok_and(|size| size <= 1024 * 1024) {
        return Ok(None);
    }
    let blob = git::git_output_bytes(
        env,
        workspace,
        &["--no-replace-objects", "cat-file", "blob", &object],
    )?;
    Ok(String::from_utf8(blob)
        .ok()
        .filter(|text| !text.contains('\0')))
}

fn line_offsets(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
        .chain((!text.ends_with('\n') && !text.is_empty()).then_some(text.len()))
        .collect()
}

fn hunk_image<'a>(text: &'a str, offsets: &[usize], range: &Range<usize>) -> Option<&'a str> {
    text.get(*offsets.get(range.start)?..*offsets.get(range.end)?)
}

fn unique_image(text: &str, image: &str) -> bool {
    if image.is_empty() {
        return false;
    }
    text.find(image).is_some_and(|start| {
        (start == 0 || text.as_bytes()[start - 1] == b'\n')
            && (image.ends_with('\n') || start + image.len() == text.len())
            && text.rfind(image) == Some(start)
    })
}

/// Read the coordinates of one regular file's text patch. A nonempty diff
/// needs a same-mode index record and at least one parsable hunk.
fn file_hunks(
    env: &orbit_common::Env,
    workspace: &Path,
    before: &str,
    after: &str,
    pathspec: &str,
    context: &str,
) -> Result<Option<Vec<FileHunk>>, OrbitError> {
    let patch = git::git_output_raw(
        env,
        workspace,
        &[
            "--no-replace-objects",
            "diff",
            "--patch",
            context,
            "--inter-hunk-context=0",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--ignore-submodules=none",
            "--diff-algorithm=myers",
            "--no-indent-heuristic",
            "--output-indicator-old=-",
            "--output-indicator-new=+",
            "--output-indicator-context= ",
            before,
            after,
            "--",
            pathspec,
        ],
    )?;
    if patch.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut lines = patch.lines();
    let mode = lines.nth(1).and_then(|line| line.strip_prefix("index "));
    if !mode.is_some_and(|line| line.ends_with(" 100644") || line.ends_with(" 100755")) {
        return Ok(None);
    }
    let hunks: Option<Vec<_>> = lines
        .filter_map(|line| line.strip_prefix("@@ -"))
        .map(|header| {
            let (old, rest) = header.split_once(" +")?;
            let (new, _) = rest.split_once(" @@")?;
            Some(FileHunk {
                before: hunk_range(old)?,
                after: hunk_range(new)?,
            })
        })
        .collect();
    Ok(hunks.filter(|hunks| !hunks.is_empty()))
}

fn hunk_range(range: &str) -> Option<Range<usize>> {
    let (line, count) = range.split_once(',').unwrap_or((range, "1"));
    let line: usize = line.parse().ok()?;
    let count: usize = count.parse().ok()?;
    // A zero-length side names a gap after `line` lines, including before
    // the first line. Nonempty sides use one-based line numbers.
    let start = if count == 0 {
        line
    } else {
        line.checked_sub(1)?
    };
    Some(start..start.checked_add(count)?)
}
