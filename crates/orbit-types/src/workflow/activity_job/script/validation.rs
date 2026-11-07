use thiserror::Error;

use super::{SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES, SCRIPT_OUTPUT_HARD_MAX_FILES, ScriptSpec};

/// A script manifest validation refusal. The message is written for the
/// operator who can open the manifest and fix it; the catalog loader adds
/// the manifest's file path around it.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScriptValidationError {
    #[error(
        "invalid script name `{0}` (want a lowercase letter then lowercase letters, digits, `_` or `-`, at most 64 bytes)"
    )]
    InvalidName(String),
    #[error("script `entry` must be a non-empty workspace-relative path, got `{0}`")]
    InvalidEntry(String),
    #[error("script interpreter element {index} is empty")]
    EmptyInterpreterElement { index: usize },
    #[error(
        "script interpreter `{program}` argument `{flag}` runs inline code, which a script manifest may not carry; put the code in the entry file"
    )]
    InlineCodeFlag { program: String, flag: String },
    #[error("script interpreter arguments are literal and may not contain templates: `{0}`")]
    TemplatedInterpreter(String),
    #[error("script argument `{0}` has an unbalanced or empty template")]
    InvalidArgTemplate(String),
    #[error(
        "script entry `#!` line is not an honored form (`#!/abs/path [one-arg]` or `#!/usr/bin/env <name>`): `{0}`"
    )]
    InvalidShebang(String),
    #[error(
        "script request `env_pass` name `{0}` is not a usable environment variable name, or names an `ORBIT_*` variable, which is never passed"
    )]
    InvalidEnvPass(String),
    #[error("script request `timeout_seconds` must be at least 1")]
    InvalidTimeout,
    #[error("script file input `as` must be a plain file name, got `{0}`")]
    InvalidFileInputName(String),
    #[error("script file input `as` name `{0}` is declared more than once")]
    DuplicateFileInputName(String),
    #[error(
        "script `files.outputs` caps must be at least 1 and at most {SCRIPT_OUTPUT_HARD_MAX_FILES} files of {SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES} bytes, got {0}"
    )]
    InvalidOutputCap(String),
}

/// Whether `name` may name a script (its manifest file is `<name>.yaml`).
pub fn is_valid_script_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The inline-code spellings refused for one interpreter program basename,
/// if the program is one whose inline form is known. The tuple is
/// (inline short-option letters, inline long options, other short-option
/// letters that take an argument). An argument-taking letter ends a
/// single-dash cluster: the rest of that element is its argument, so the
/// `c` in `python3 -Xpycache_prefix=x` and the `e` in `perl -MExporter`
/// are argument text, not inline flags.
fn inline_code_flag(
    program_basename: &str,
) -> Option<(&'static [u8], &'static [&'static str], &'static [u8])> {
    match program_basename {
        "sh" | "bash" | "zsh" | "dash" => Some((b"c", &[], b"o")),
        "perl" => Some((b"eE", &[], b"0CDFIMim")),
        "ruby" => Some((b"e", &[], b"0CEFIKTWri")),
        "node" => Some((b"ep", &["--eval", "--print"], b"r")),
        other if other.starts_with("python") => Some((b"c", &[], b"mWX")),
        _ => None,
    }
}

fn program_basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Refuse an interpreter argv that could run inline code instead of the
/// entry file. Every element is checked: a known interpreter named
/// anywhere in a launcher list may not be followed by its inline flag, in
/// the separate (`-c`), glued (`-c…`), bundled (`-ec`, `-xc`) or long
/// (`--eval`) spelling.
pub fn validate_script_interpreter(interpreter: &[String]) -> Result<(), ScriptValidationError> {
    for (index, element) in interpreter.iter().enumerate() {
        if element.is_empty() {
            return Err(ScriptValidationError::EmptyInterpreterElement { index });
        }
        if element.contains("{{") {
            return Err(ScriptValidationError::TemplatedInterpreter(element.clone()));
        }
    }
    for (index, element) in interpreter.iter().enumerate() {
        let Some((inline_letters, long_flags, argument_letters)) =
            inline_code_flag(program_basename(element))
        else {
            continue;
        };
        for argument in &interpreter[index + 1..] {
            if argument == "--" {
                break;
            }
            if let Some(long) = argument.strip_prefix("--") {
                let is_inline = long_flags.iter().any(|flag| {
                    long == flag.trim_start_matches('-')
                        || long
                            .strip_prefix(flag.trim_start_matches('-'))
                            .is_some_and(|rest| rest.starts_with('='))
                });
                if is_inline {
                    return Err(ScriptValidationError::InlineCodeFlag {
                        program: element.clone(),
                        flag: argument.clone(),
                    });
                }
                continue;
            }
            let Some(cluster) = argument.strip_prefix('-').filter(|rest| !rest.is_empty()) else {
                continue;
            };
            for letter in cluster.bytes() {
                if inline_letters.contains(&letter) {
                    return Err(ScriptValidationError::InlineCodeFlag {
                        program: element.clone(),
                        flag: argument.clone(),
                    });
                }
                if argument_letters.contains(&letter) {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// The interpreter argv an entry `#!` line declares.
///
/// Honored forms are `#!/abs/path`, `#!/abs/path <one-arg>` and
/// `#!/usr/bin/env <name>`; the result is treated exactly as if it were
/// written in the manifest's `interpreter:` list, so the same inline-code
/// refusal applies. `Ok(None)` means the first line is not a shebang and
/// the entry is a native executable.
pub fn script_interpreter_from_shebang(
    first_line: &str,
) -> Result<Option<Vec<String>>, ScriptValidationError> {
    let Some(rest) = first_line.strip_prefix("#!") else {
        return Ok(None);
    };
    let rest = rest.trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let program = parts.next().unwrap_or_default();
    let argument = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !program.starts_with('/') {
        return Err(ScriptValidationError::InvalidShebang(
            first_line.to_string(),
        ));
    }
    let interpreter = if program == "/usr/bin/env" {
        let Some(name) = argument else {
            return Err(ScriptValidationError::InvalidShebang(
                first_line.to_string(),
            ));
        };
        if name.contains(char::is_whitespace) || name.starts_with('-') {
            return Err(ScriptValidationError::InvalidShebang(
                first_line.to_string(),
            ));
        }
        vec![name.to_string()]
    } else {
        let mut interpreter = vec![program.to_string()];
        if let Some(argument) = argument {
            interpreter.push(argument.to_string());
        }
        interpreter
    };
    validate_script_interpreter(&interpreter)?;
    Ok(Some(interpreter))
}

/// Check one manifest argument's templates: every `{{ … }}` is balanced
/// and non-empty. A rendered value still becomes exactly one argv element.
pub fn validate_script_arg_template(argument: &str) -> Result<(), ScriptValidationError> {
    let mut rest = argument;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(ScriptValidationError::InvalidArgTemplate(
                argument.to_string(),
            ));
        };
        if after_open[..close].trim().is_empty() {
            return Err(ScriptValidationError::InvalidArgTemplate(
                argument.to_string(),
            ));
        }
        rest = &after_open[close + 2..];
    }
    Ok(())
}

/// Validate a parsed script manifest and its metadata name, without
/// touching the filesystem. `entry` resolution (the path must stay inside
/// the workspace) is the catalog loader's half of this contract.
pub fn validate_script_spec(name: &str, spec: &ScriptSpec) -> Result<(), ScriptValidationError> {
    if !is_valid_script_name(name) {
        return Err(ScriptValidationError::InvalidName(name.to_string()));
    }
    if spec.entry.trim().is_empty() || spec.entry.starts_with('/') {
        return Err(ScriptValidationError::InvalidEntry(spec.entry.clone()));
    }
    validate_script_interpreter(&spec.interpreter)?;
    for argument in &spec.args {
        validate_script_arg_template(argument)?;
    }
    for (index, input) in spec.files.inputs.iter().enumerate() {
        validate_script_arg_template(&input.from)?;
        let as_name = input.as_.as_str();
        if as_name.is_empty()
            || as_name.contains('/')
            || as_name.contains('\0')
            || as_name == ".."
            || as_name == "."
        {
            return Err(ScriptValidationError::InvalidFileInputName(
                input.as_.clone(),
            ));
        }
        if spec.files.inputs[..index]
            .iter()
            .any(|earlier| earlier.as_ == input.as_)
        {
            return Err(ScriptValidationError::DuplicateFileInputName(
                input.as_.clone(),
            ));
        }
    }
    let outputs = &spec.files.outputs;
    if let Some(max_files) = outputs.max_files
        && !(1..=SCRIPT_OUTPUT_HARD_MAX_FILES).contains(&max_files)
    {
        return Err(ScriptValidationError::InvalidOutputCap(format!(
            "max_files {max_files}"
        )));
    }
    if let Some(max_file_bytes) = outputs.max_file_bytes
        && !(1..=SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES).contains(&max_file_bytes)
    {
        return Err(ScriptValidationError::InvalidOutputCap(format!(
            "max_file_bytes {max_file_bytes}"
        )));
    }
    for env_name in &spec.requests.env_pass {
        let valid = !env_name.is_empty()
            && !env_name.starts_with("ORBIT_")
            && env_name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && env_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(ScriptValidationError::InvalidEnvPass(env_name.clone()));
        }
    }
    if spec.requests.timeout_seconds == Some(0) {
        return Err(ScriptValidationError::InvalidTimeout);
    }
    Ok(())
}
