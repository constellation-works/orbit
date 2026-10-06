//! Refuse a default-profile rustup install whose root is inside the workspace.
//!
//! The default and complete profiles write rust-docs, including
//! `share/doc/rust/html/core/macro.env.html`. `denyModify` `**/*.env.*` matches
//! that path even under `.orbit/tmp/` — the earlier `!.orbit/tmp/**` exception
//! does not win — and the Linux post-run guard then fails the run only after
//! the file exists. This check runs before the child is spawned.
//!
//! `--profile minimal` without a `rust-docs` component does not write those
//! files. `scripts/codeql-rust-local.sh` installs that way into an absolute
//! scratch `RUSTUP_HOME` and must keep working. An install whose resolved root
//! is outside the workspace (the usual `~/.rustup`) is unchanged.

use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::tracing;

const DENY_MODIFY_RULE: &str = "**/*.env.*";
const DOCS_RELATIVE: &str = "share/doc/rust/html/core/macro.env.html";
const SYMLINK_LIMIT: usize = 40;

/// Rustup proxy basenames. A match counts only when the binary is rustup
/// itself (symlink, hard link, or the resolved file name).
const PROXY_NAMES: &[&str] = &[
    "cargo",
    "cargo-clippy",
    "cargo-fmt",
    "cargo-miri",
    "clippy-driver",
    "rls",
    "rust-analyzer",
    "rust-gdb",
    "rust-gdbgui",
    "rust-lldb",
    "rustc",
    "rustdoc",
    "rustfmt",
];

/// Refuse a rustup install that would write rust-docs inside the workspace.
pub(super) fn enforce_no_workspace_default_rustup_install(
    tool_name: &str,
    program: &str,
    args: &[String],
    env: &[(String, String)],
    workspace_root: Option<&Path>,
) -> Result<(), OrbitError> {
    let Some(workspace) = workspace_root.filter(|path| !path.as_os_str().is_empty()) else {
        return Ok(());
    };
    let Some(install) = classify(program, args, env) else {
        return Ok(());
    };
    let home = rustup_home(env, workspace);
    if !rooted_inside_workspace(workspace, &home) {
        return Ok(());
    }
    if !install.writes_rust_docs(&home, env, workspace) {
        return Ok(());
    }

    tracing::warn!(
        target: "orbit.policy.deny",
        tool = tool_name,
        path = program,
        profile = "denyModify",
        matched_rule = DENY_MODIFY_RULE,
        rustup_home = %home.display(),
        "refusing default-profile rustup toolchain install rooted inside the workspace",
    );
    Err(OrbitError::PolicyDenied(format!(
        "refusing default-profile rustup toolchain install rooted inside the workspace at {}: \
         it would write {DOCS_RELATIVE}, which denyModify rule {DENY_MODIFY_RULE} forbids",
        home.display()
    )))
}

enum Install {
    /// `rustup toolchain install`, `install`, `update`, `default`, or `run`.
    Profiled {
        profile_flag: Option<String>,
        components: Vec<String>,
        /// Empty means "the active toolchain".
        toolchains: Vec<String>,
        plus: Option<String>,
    },
    /// `rustup component add`.
    ComponentAdd { components: Vec<String> },
    /// `rustup-init`, which installs a toolchain into `RUSTUP_HOME` directly.
    Init {
        profile_flag: Option<String>,
        components: Vec<String>,
        default_toolchain: Option<String>,
    },
    /// A rustup proxy (`cargo`, `rustc`, …) that may auto-install.
    Auto { plus: Option<String> },
}

impl Install {
    fn writes_rust_docs(&self, home: &Path, env: &[(String, String)], workspace: &Path) -> bool {
        match self {
            Self::ComponentAdd { components } => components_include_rust_docs(components),
            Self::Init {
                profile_flag,
                components,
                default_toolchain,
            } => init_writes_rust_docs(profile_flag.as_deref(), components, default_toolchain),
            Self::Profiled {
                profile_flag,
                components,
                toolchains,
                plus,
            } => profiled_writes_rust_docs(
                home,
                env,
                workspace,
                profile_flag.as_deref(),
                components,
                toolchains,
                plus.as_deref(),
            ),
            Self::Auto { plus } => auto_writes_rust_docs(home, env, workspace, plus.as_deref()),
        }
    }
}

fn classify(program: &str, args: &[String], env: &[(String, String)]) -> Option<Install> {
    let name = basename(program);
    if name.eq_ignore_ascii_case("rustup-init") {
        return classify_init(args);
    }
    if name.eq_ignore_ascii_case("rustup") {
        return classify_rustup(args);
    }
    let resolved = resolve_program(program, env);
    if !is_rustup_proxy(program, resolved.as_deref(), env) {
        return None;
    }
    Some(Install::Auto {
        plus: proxy_plus(args),
    })
}

fn classify_init(args: &[String]) -> Option<Install> {
    let parsed = parse_flags(args);
    if parsed.help {
        return None;
    }
    Some(Install::Init {
        profile_flag: parsed.profile,
        components: parsed.components,
        default_toolchain: parsed.default_toolchain,
    })
}

fn classify_rustup(args: &[String]) -> Option<Install> {
    let (plus, rest) = split_globals(args);
    let (command, tail) = rest.split_first()?;
    match command.as_str() {
        "install" | "update" => profiled(plus, tail),
        "default" => {
            let parsed = parse_flags(tail);
            if parsed.help || parsed.positionals.is_empty() {
                None
            } else {
                Some(Install::Profiled {
                    profile_flag: parsed.profile,
                    components: parsed.components,
                    toolchains: parsed.positionals,
                    plus,
                })
            }
        }
        "run" => {
            let parsed = parse_flags(tail);
            if parsed.help {
                return None;
            }
            let toolchain = parsed.positionals.first()?.clone();
            Some(Install::Profiled {
                profile_flag: parsed.profile,
                components: parsed.components,
                toolchains: vec![toolchain],
                plus,
            })
        }
        "component" => classify_component(tail),
        "toolchain" => {
            let (sub, sub_tail) = tail.split_first()?;
            match sub.as_str() {
                "install" => profiled(plus, sub_tail),
                _ => None,
            }
        }
        _ => None,
    }
}

fn classify_component(tail: &[String]) -> Option<Install> {
    let (sub, sub_tail) = tail.split_first()?;
    if sub != "add" {
        return None;
    }
    let parsed = parse_flags(sub_tail);
    if parsed.help {
        return None;
    }
    let mut components = parsed.components;
    components.extend(parsed.positionals);
    if components.is_empty() {
        return None;
    }
    Some(Install::ComponentAdd { components })
}

fn profiled(plus: Option<String>, args: &[String]) -> Option<Install> {
    let parsed = parse_flags(args);
    if parsed.help {
        return None;
    }
    Some(Install::Profiled {
        profile_flag: parsed.profile,
        components: parsed.components,
        toolchains: parsed.positionals,
        plus,
    })
}

struct ParsedFlags {
    help: bool,
    profile: Option<String>,
    components: Vec<String>,
    positionals: Vec<String>,
    default_toolchain: Option<String>,
}

fn parse_flags(args: &[String]) -> ParsedFlags {
    let mut parsed = ParsedFlags {
        help: false,
        profile: None,
        components: Vec::new(),
        positionals: Vec::new(),
        default_toolchain: None,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            parsed.positionals.extend(args[index + 1..].iter().cloned());
            break;
        }
        if arg == "--help" || arg == "-h" || arg == "--version" || arg == "-V" {
            parsed.help = true;
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--profile=") {
            parsed.profile = Some(value.to_string());
            index += 1;
            continue;
        }
        if consume_value(args, &mut index, arg, "--profile", |value| {
            parsed.profile = Some(value);
        }) {
            continue;
        }
        if let Some(value) = arg
            .strip_prefix("--component=")
            .or_else(|| arg.strip_prefix("-c="))
        {
            push_components(&mut parsed.components, value);
            index += 1;
            continue;
        }
        if arg == "--component" || arg == "-c" {
            index += 1;
            if let Some(value) = args.get(index) {
                push_components(&mut parsed.components, value);
                index += 1;
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix("--default-toolchain=") {
            parsed.default_toolchain = Some(value.to_string());
            index += 1;
            continue;
        }
        if consume_value(args, &mut index, arg, "--default-toolchain", |value| {
            parsed.default_toolchain = Some(value);
        }) {
            continue;
        }
        if arg == "--target" || arg == "-t" || arg == "--toolchain" || arg.starts_with("--target=")
        {
            if !arg.contains('=') {
                index += 1;
            }
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        parsed.positionals.push(arg.clone());
        index += 1;
    }
    parsed
}

fn consume_value(
    args: &[String],
    index: &mut usize,
    arg: &str,
    flag: &str,
    mut store: impl FnMut(String),
) -> bool {
    if arg != flag {
        return false;
    }
    *index += 1;
    if let Some(value) = args.get(*index) {
        store(value.clone());
        *index += 1;
    }
    true
}

fn push_components(components: &mut Vec<String>, value: &str) {
    components.extend(
        value
            .split(',')
            .map(str::trim)
            .filter(|component| !component.is_empty())
            .map(str::to_string),
    );
}

fn split_globals(args: &[String]) -> (Option<String>, &[String]) {
    let mut index = 0;
    let mut plus = None;
    while let Some(arg) = args.get(index) {
        if let Some(name) = arg.strip_prefix('+') {
            if !name.is_empty() && plus.is_none() {
                plus = Some(name.to_string());
            }
            index += 1;
            continue;
        }
        if matches!(arg.as_str(), "-v" | "--verbose" | "-q" | "--quiet") {
            index += 1;
            continue;
        }
        break;
    }
    (plus, &args[index..])
}

fn proxy_plus(args: &[String]) -> Option<String> {
    args.first()
        .and_then(|arg| arg.strip_prefix('+'))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn init_writes_rust_docs(
    profile_flag: Option<&str>,
    components: &[String],
    default_toolchain: &Option<String>,
) -> bool {
    if components_include_rust_docs(components) {
        return true;
    }
    if default_toolchain
        .as_deref()
        .is_some_and(|toolchain| toolchain.eq_ignore_ascii_case("none"))
    {
        return false;
    }
    profile_includes_rust_docs(profile_flag.unwrap_or("default"))
}

fn profiled_writes_rust_docs(
    home: &Path,
    env: &[(String, String)],
    workspace: &Path,
    profile_flag: Option<&str>,
    components: &[String],
    toolchains: &[String],
    plus: Option<&str>,
) -> bool {
    if components_include_rust_docs(components) {
        return true;
    }
    let profile = effective_profile(home, profile_flag, None);
    if !profile_includes_rust_docs(&profile) {
        return false;
    }
    let targets: Vec<String> = if toolchains.is_empty() {
        match active_toolchain(home, env, workspace, plus) {
            ActiveToolchain::Named(name) => vec![name],
            // No selected toolchain: rustup reports an error and does not install.
            ActiveToolchain::None => return false,
            // An override database entry can name a toolchain this check cannot
            // see. A docs-writing profile then has to be refused.
            ActiveToolchain::Hidden => return true,
        }
    } else {
        toolchains.to_vec()
    };
    targets
        .iter()
        .any(|toolchain| !toolchain_installed(home, toolchain))
}

fn auto_writes_rust_docs(
    home: &Path,
    env: &[(String, String)],
    workspace: &Path,
    plus: Option<&str>,
) -> bool {
    if !auto_install_enabled(env, home) {
        return false;
    }
    let file_profile = match nearest_toolchain_file(workspace) {
        OverrideFile::Unusable => return false,
        OverrideFile::Parsed(spec) => Some(spec),
        OverrideFile::None => None,
    };
    match active_toolchain(home, env, workspace, plus) {
        ActiveToolchain::None => false,
        ActiveToolchain::Hidden => profile_includes_rust_docs(&effective_profile(home, None, None)),
        ActiveToolchain::Named(name) => {
            if toolchain_installed(home, &name) {
                return false;
            }
            let file = file_profile
                .filter(|_| plus.is_none() && env_value(env, "RUSTUP_TOOLCHAIN").is_none());
            let components = file
                .as_ref()
                .map(|spec| spec.components.as_slice())
                .unwrap_or(&[]);
            if components_include_rust_docs(components) {
                return true;
            }
            let profile = effective_profile(home, None, file.and_then(|spec| spec.profile));
            profile_includes_rust_docs(&profile)
        }
    }
}

enum ActiveToolchain {
    Named(String),
    /// Rustup will not install because nothing selects a toolchain.
    None,
    /// A settings override can select a toolchain this process cannot name.
    Hidden,
}

fn active_toolchain(
    home: &Path,
    env: &[(String, String)],
    workspace: &Path,
    plus: Option<&str>,
) -> ActiveToolchain {
    if let Some(name) = plus.map(str::trim).filter(|name| !name.is_empty()) {
        return ActiveToolchain::Named(name.to_string());
    }
    if let Some(name) = env_value(env, "RUSTUP_TOOLCHAIN") {
        return ActiveToolchain::Named(name);
    }
    // `[overrides]` can name a toolchain this process does not parse. A
    // docs-writing profile is refused rather than guessed.
    if settings_overrides_present(home) {
        return ActiveToolchain::Hidden;
    }
    match nearest_toolchain_file(workspace) {
        OverrideFile::Unusable => ActiveToolchain::None,
        OverrideFile::Parsed(spec) => spec
            .channel
            .filter(|channel| !channel.is_empty())
            .map_or_else(
                || {
                    settings_value(home, "default_toolchain")
                        .map(ActiveToolchain::Named)
                        .unwrap_or(ActiveToolchain::None)
                },
                ActiveToolchain::Named,
            ),
        OverrideFile::None => settings_value(home, "default_toolchain")
            .map(ActiveToolchain::Named)
            .unwrap_or(ActiveToolchain::None),
    }
}

fn effective_profile(home: &Path, flag: Option<&str>, file_profile: Option<String>) -> String {
    flag.map(str::to_string)
        .or(file_profile)
        .or_else(|| configured_profile(home))
        .unwrap_or_else(|| "default".to_string())
}

fn profile_includes_rust_docs(profile: &str) -> bool {
    !profile.eq_ignore_ascii_case("minimal")
}

fn components_include_rust_docs(components: &[String]) -> bool {
    components.iter().any(|component| {
        let name = component.trim();
        name.eq_ignore_ascii_case("rust-docs")
            || name
                .get(.."rust-docs-".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("rust-docs-"))
    })
}

fn auto_install_enabled(env: &[(String, String)], home: &Path) -> bool {
    if let Some(mode) = env_raw(env, "RUSTUP_AUTO_INSTALL") {
        return mode != "0";
    }
    !settings_value(home, "auto_install").is_some_and(|mode| mode.eq_ignore_ascii_case("disable"))
}

fn configured_profile(home: &Path) -> Option<String> {
    settings_value(home, "profile")
}

fn toolchain_installed(home: &Path, name: &str) -> bool {
    let requested = Path::new(name);
    if requested.is_absolute() {
        return requested.is_dir();
    }
    let toolchains = home.join("toolchains");
    if toolchains.join(name).exists() {
        return true;
    }
    toolchains
        .join(format!("{name}-{}", rustup_host_triple()))
        .exists()
}

fn rustup_host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => {
            let abi = if cfg!(target_env = "musl") {
                "musl"
            } else {
                "gnu"
            };
            format!("{arch}-unknown-linux-{abi}")
        }
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => {
            let abi = if cfg!(target_env = "gnu") {
                "gnu"
            } else {
                "msvc"
            };
            format!("{arch}-pc-windows-{abi}")
        }
        other => format!("{arch}-unknown-{other}"),
    }
}

fn rustup_home(env: &[(String, String)], workspace: &Path) -> PathBuf {
    if let Some(value) = env_value(env, "RUSTUP_HOME") {
        let path = PathBuf::from(&value);
        return if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        };
    }
    env_value(env, "HOME")
        .or_else(|| env_value(env, "USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".rustup"))
        .unwrap_or_else(|| PathBuf::from("/.rustup-unset"))
}

fn rooted_inside_workspace(workspace: &Path, home: &Path) -> bool {
    let Ok(workspace) = workspace.canonicalize() else {
        return false;
    };
    let resolved = resolve_path(home).unwrap_or_else(|| lexical_absolute(home, &workspace));
    resolved.starts_with(&workspace)
}

fn resolve_path(path: &Path) -> Option<PathBuf> {
    fn walk(path: &Path, depth: usize) -> Option<PathBuf> {
        if depth > SYMLINK_LIMIT {
            return None;
        }
        let mut missing = Vec::new();
        let mut cursor = path.to_path_buf();
        loop {
            if cursor.as_os_str().is_empty() {
                return None;
            }
            match fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    let target = fs::read_link(&cursor).ok()?;
                    let combined = if target.is_absolute() {
                        target
                    } else {
                        cursor.parent()?.join(target)
                    };
                    let mut full = walk(&combined, depth + 1)?;
                    for part in missing.iter().rev() {
                        full.push(part);
                    }
                    return Some(full);
                }
                Ok(_) => {
                    let mut full = cursor.canonicalize().ok()?;
                    for part in missing.iter().rev() {
                        full.push(part);
                    }
                    return Some(full);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(cursor.file_name()?.to_os_string());
                    if !cursor.pop() {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }
    walk(path, 0)
}

fn lexical_absolute(path: &Path, base: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

struct ToolchainSpec {
    channel: Option<String>,
    profile: Option<String>,
    components: Vec<String>,
}

enum OverrideFile {
    None,
    Unusable,
    Parsed(ToolchainSpec),
}

enum DirFile {
    Absent,
    Unusable,
    Parsed(ToolchainSpec),
}

fn nearest_toolchain_file(start: &Path) -> OverrideFile {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut dir = Some(start.as_path());
    while let Some(current) = dir {
        match read_toolchain_dir(current) {
            DirFile::Absent => {}
            DirFile::Unusable => return OverrideFile::Unusable,
            DirFile::Parsed(spec) => return OverrideFile::Parsed(spec),
        }
        dir = current.parent();
    }
    OverrideFile::None
}

fn read_toolchain_dir(dir: &Path) -> DirFile {
    let plain = dir.join("rust-toolchain");
    let toml = dir.join("rust-toolchain.toml");
    // Rustup prefers the plain file when both exist.
    if plain.is_file() {
        return parse_toolchain_file(&plain, true);
    }
    if toml.is_file() {
        return parse_toolchain_file(&toml, false);
    }
    DirFile::Absent
}

fn parse_toolchain_file(path: &Path, plain_may_be_channel: bool) -> DirFile {
    let Ok(text) = fs::read_to_string(path) else {
        return DirFile::Unusable;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DirFile::Unusable;
    }
    if plain_may_be_channel && !trimmed.contains('\n') && !trimmed.starts_with('[') {
        return DirFile::Parsed(ToolchainSpec {
            channel: Some(trimmed.to_string()),
            profile: None,
            components: Vec::new(),
        });
    }
    parse_toml_toolchain(trimmed)
}

fn parse_toml_toolchain(text: &str) -> DirFile {
    let mut in_toolchain = false;
    let mut channel = None;
    let mut profile = None;
    let mut components = Vec::new();
    let mut collecting = false;
    for line in text.lines() {
        let uncommented = strip_toml_comment(line).trim();
        if uncommented.starts_with('[') {
            in_toolchain = table_header_name(uncommented) == Some("toolchain");
            collecting = false;
            continue;
        }
        let trimmed = uncommented;
        if !in_toolchain || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if collecting {
            if let Some(item) = array_item(trimmed) {
                components.push(item);
            }
            if trimmed.contains(']') {
                collecting = false;
            }
            continue;
        }
        let Some((key, value)) = split_toml_assign(trimmed) else {
            continue;
        };
        match key {
            "channel" => channel = Some(value),
            "profile" => profile = Some(value),
            "components" => {
                let Some(rest) = value.strip_prefix('[') else {
                    continue;
                };
                if let Some(end) = rest.find(']') {
                    components.extend(array_items(&rest[..end]));
                } else {
                    components.extend(array_items(rest));
                    collecting = true;
                }
            }
            _ => {}
        }
    }
    if channel.is_none() && profile.is_none() && components.is_empty() {
        DirFile::Unusable
    } else {
        DirFile::Parsed(ToolchainSpec {
            channel,
            profile,
            components,
        })
    }
}

fn array_items(value: &str) -> Vec<String> {
    value.split(',').filter_map(array_item).collect()
}

fn array_item(value: &str) -> Option<String> {
    let item = value.trim().trim_matches(|ch| ch == '[' || ch == ']');
    let item = unquote(item.trim());
    if item.is_empty() { None } else { Some(item) }
}

fn settings_overrides_present(home: &Path) -> bool {
    let Ok(text) = fs::read_to_string(home.join("settings.toml")) else {
        return false;
    };
    let mut in_overrides = false;
    for line in text.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') {
            in_overrides = table_header_name(trimmed) == Some("overrides");
            continue;
        }
        if in_overrides && !trimmed.is_empty() && !trimmed.starts_with('#') {
            return true;
        }
    }
    false
}

fn settings_value(home: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(home.join("settings.toml")).ok()?;
    for line in text.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (found, value) = split_toml_assign(trimmed)?;
        if found == key {
            return Some(value);
        }
    }
    None
}

fn split_toml_assign(line: &str) -> Option<(&str, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    Some((key, unquote(value.trim())))
}

fn table_header_name(line: &str) -> Option<&str> {
    let name = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    let name = name
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
        .or_else(|| {
            name.strip_prefix('\'')
                .and_then(|name| name.strip_suffix('\''))
        })
        .unwrap_or(name);
    Some(name.trim())
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        match quote {
            Some('"') if ch == '\\' && !escaped => escaped = true,
            Some(delimiter) if ch == delimiter && !escaped => quote = None,
            Some(_) => escaped = false,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return &line[..index],
            None => {}
        }
        if ch != '\\' || !matches!(quote, Some('"')) {
            escaped = false;
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return value[1..value.len() - 1].to_string();
    }
    value
        .split_once('#')
        .map(|(before, _)| before.trim())
        .unwrap_or(value)
        .to_string()
}

fn env_value(env: &[(String, String)], name: &str) -> Option<String> {
    env_raw(env, name)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn env_raw<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn basename(program: &str) -> &str {
    let file_name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    file_name.strip_suffix(".exe").unwrap_or(file_name)
}

fn is_rustup_proxy(program: &str, resolved: Option<&Path>, env: &[(String, String)]) -> bool {
    if !PROXY_NAMES
        .iter()
        .any(|name| basename(program).eq_ignore_ascii_case(name))
    {
        return false;
    }
    let Some(resolved) = resolved else {
        return false;
    };
    if basename_path(resolved).eq_ignore_ascii_case("rustup") {
        return true;
    }
    if fs::read_link(program)
        .is_ok_and(|target| basename_path(&target).eq_ignore_ascii_case("rustup"))
    {
        return true;
    }
    #[cfg(unix)]
    {
        if resolved
            .parent()
            .is_some_and(|dir| same_file(resolved, &dir.join("rustup")))
        {
            return true;
        }
        if resolve_program("rustup", env).is_some_and(|rustup| same_file(resolved, &rustup)) {
            return true;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = env;
    }
    false
}

fn basename_path(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}

fn resolve_program(program: &str, env: &[(String, String)]) -> Option<PathBuf> {
    let requested = Path::new(program);
    if requested.components().count() > 1 {
        return fs::canonicalize(requested).ok();
    }
    let path = env_raw(env, "PATH")?;
    std::env::split_paths(path).find_map(|dir| fs::canonicalize(dir.join(requested)).ok())
}

#[cfg(unix)]
fn same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(left) = fs::metadata(left) else {
        return false;
    };
    let Ok(right) = fs::metadata(right) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}
