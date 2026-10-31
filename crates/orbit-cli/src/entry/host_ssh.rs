/// Whether a string is a plain shell word that needs no quoting in a POSIX shell.
fn is_plain_shell_word(word: &str) -> bool {
    !word.is_empty()
        && word.bytes().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'=' | b','
                )
        })
}

/// `ssh <target> orbit <args without --host>` for a command that rejected
/// `--host`, when the named host resolves to a remote entry.
pub(super) fn host_ssh_command(args: &[std::ffi::OsString]) -> Option<String> {
    let args = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut host = None;
    let mut rest = Vec::new();
    let mut needs_local_quoting = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--host" {
            host = iter.next();
        } else if let Some(value) = arg.strip_prefix("--host=") {
            host = Some(value.to_string());
        } else {
            let quoted = if is_plain_shell_word(&arg) {
                arg
            } else {
                needs_local_quoting = true;
                orbit_common::process::shell::quote_posix_arg(&arg)
            };
            rest.push(quoted);
        }
    }
    let global_root = orbit_core::runtime::resolve_global_root().ok()?;
    let target = orbit_cmd::hosts::host_ssh_target(&global_root, host?.trim()).ok()??;
    let command = if rest.is_empty() {
        "orbit".to_string()
    } else {
        format!("orbit {}", rest.join(" "))
    };
    // Preserve the remote argument quotes through the local shell: SSH joins
    // its command arguments into a string for the remote login shell to parse.
    let command = if needs_local_quoting {
        orbit_common::process::shell::quote_posix_arg(&command)
    } else {
        command
    };
    Some(format!("ssh {target} {command}"))
}
