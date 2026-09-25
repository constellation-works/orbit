//! Parse the program an installed launchd or systemd clock unit runs.

use std::fs;
use std::path::{Path, PathBuf};

use super::install::{launchd_plist_path, systemd_service_path};
use super::manager::ClockPlatform;

type ClockUnitProgram = (PathBuf, PathBuf, bool);
type ClockUnitParseError = (PathBuf, String);

pub(super) fn discover_clock_unit_program(
    home: &Path,
    platform: ClockPlatform,
) -> Option<Result<ClockUnitProgram, ClockUnitParseError>> {
    let unit_path = match platform {
        ClockPlatform::Launchd => launchd_plist_path(home),
        ClockPlatform::Systemd => systemd_service_path(home),
    };
    if !unit_path.exists() {
        return None;
    }
    let contents = match fs::read_to_string(&unit_path) {
        Ok(contents) => contents,
        Err(error) => {
            return Some(Err((
                unit_path,
                format!("could not read unit file: {error}"),
            )));
        }
    };
    let program = match platform {
        ClockPlatform::Launchd => parse_launchd_program(&contents),
        ClockPlatform::Systemd => parse_systemd_exec_start(&contents),
    };
    let legacy_invocation = match platform {
        ClockPlatform::Launchd => launchd_arguments(&contents)
            .is_some_and(|arguments| arguments.get(1).is_some_and(|arg| arg == "sweep")),
        ClockPlatform::Systemd => systemd_arguments(&contents)
            .is_some_and(|arguments| arguments.first().is_some_and(|arg| arg == "sweep")),
    };
    match program {
        Some(program) if !program.is_empty() => {
            Some(Ok((unit_path, PathBuf::from(program), legacy_invocation)))
        }
        _ => Some(Err((
            unit_path,
            "unit file does not name an orbit program path".to_string(),
        ))),
    }
}

fn parse_launchd_program(plist: &str) -> Option<String> {
    if let Some(program) = launchd_arguments(plist).and_then(|args| args.into_iter().next()) {
        return Some(program);
    }
    plist
        .split("<key>Program</key>")
        .nth(1)
        .and_then(first_plist_string)
}

fn launchd_arguments(plist: &str) -> Option<Vec<String>> {
    let args = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let array = args.split("<array>").nth(1)?.split("</array>").next()?;
    let mut values = Vec::new();
    let mut remaining = array;
    while let Some(start) = remaining.find("<string>") {
        remaining = &remaining[start + "<string>".len()..];
        let end = remaining.find("</string>")?;
        values.push(plist_unescape(remaining[..end].trim()));
        remaining = &remaining[end + "</string>".len()..];
    }
    (!values.is_empty()).then_some(values)
}

fn first_plist_string(fragment: &str) -> Option<String> {
    let start = fragment.find("<string>")? + "<string>".len();
    let end = fragment[start..].find("</string>")?;
    let value = fragment[start..start + end].trim();
    (!value.is_empty()).then(|| plist_unescape(value))
}

/// Reverse the entity escaping `clock::plist_string` applies.
fn plist_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn parse_systemd_exec_start(unit: &str) -> Option<String> {
    split_systemd_exec_start(unit).map(|(program, _)| program)
}

fn systemd_arguments(unit: &str) -> Option<Vec<String>> {
    let (_, rest) = split_systemd_exec_start(unit)?;
    Some(rest.split_whitespace().map(ToString::to_string).collect())
}

/// Split the first `ExecStart=` line into its unescaped program and the raw
/// arguments after it, reversing `clock::systemd_exec_program`.
pub(super) fn split_systemd_exec_start(unit: &str) -> Option<(String, &str)> {
    let line = unit
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ExecStart="))?
        .trim();
    let (program, rest) = match line.strip_prefix('"') {
        Some(quoted) => {
            let mut program = String::new();
            let mut chars = quoted.char_indices();
            let mut end = None;
            while let Some((index, ch)) = chars.next() {
                match ch {
                    '\\' => program.extend(chars.next().map(|(_, escaped)| escaped)),
                    '"' => {
                        end = Some(index + 1);
                        break;
                    }
                    _ => program.push(ch),
                }
            }
            (program, &quoted[end?..])
        }
        None => {
            let (program, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            (program.to_string(), rest)
        }
    };
    let program = program.replace("%%", "%");
    (!program.is_empty()).then_some((program, rest))
}

pub(super) fn same_program(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

pub(super) fn normalize_version(raw: &str) -> String {
    let line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    line.split_whitespace()
        .rev()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(line.trim())
        .trim_start_matches('v')
        .to_string()
}

pub(super) fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

pub(super) fn display_opt_path(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string())
}
