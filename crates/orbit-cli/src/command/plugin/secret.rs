//! `orbit plugin secret set|list|rm`: the operator's side of a plugin's
//! declared `spec.secrets`.
//!
//! A value only ever arrives on stdin or at a no-echo terminal prompt. It is
//! never an argument: argv is visible to every process on the host and lands
//! in shell history. Nothing this module prints contains a value.

use std::io::Read;

use clap::{Args, Subcommand};
use orbit_core::adapter::command::{
    MAX_PLUGIN_SECRET_BYTES, PluginSecretStatus, PluginSecretValue,
};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct PluginSecretCommand {
    #[command(subcommand)]
    pub command: PluginSecretSubcommand,
}

impl Execute for PluginSecretCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            PluginSecretSubcommand::Set(args) => args.execute(runtime),
            PluginSecretSubcommand::List(args) => args.execute(runtime),
            PluginSecretSubcommand::Rm(args) => args.execute(runtime),
        }
    }
}

#[derive(Subcommand)]
pub enum PluginSecretSubcommand {
    /// Set a declared secret from stdin, or from a prompt that does not echo
    Set(PluginSecretSetArgs),
    /// List a plugin's secrets and whether each is set (never the values)
    List(PluginSecretListArgs),
    /// Delete one of a plugin's secrets
    Rm(PluginSecretRmArgs),
}

impl PluginSecretSubcommand {
    /// The `(subcommand, plugin)` pair the audit row records. Never a value.
    pub(crate) fn audit_identity(&self) -> (&'static str, &str) {
        match self {
            Self::Set(args) => ("secret-set", &args.plugin),
            Self::List(args) => ("secret-list", &args.plugin),
            Self::Rm(args) => ("secret-rm", &args.plugin),
        }
    }
}

#[derive(Args)]
pub struct PluginSecretSetArgs {
    /// Plugin namespace
    pub plugin: String,
    /// Secret name, as the plugin declares it in `spec.secrets`
    pub name: String,
    /// Anything after the name. A value is never accepted here, so this only
    /// exists to refuse one without echoing it back the way a parse error
    /// would.
    #[arg(
        hide = true,
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub rejected: Vec<String>,
}

impl Execute for PluginSecretSetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if !self.rejected.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "a secret value is never taken from the command line, where other processes and \
                 shell history can read it; pipe it on stdin (`orbit plugin secret set {} {} < \
                 file`) or run the command in a terminal to be prompted",
                self.plugin, self.name
            )));
        }
        let value = read_secret_value(&self.plugin, &self.name)?;
        let status = runtime.set_plugin_secret(&self.plugin, &self.name, &value)?;
        let text = format!("Set secret '{}' for plugin '{}'.", status.name, self.plugin);
        Ok(Payload::detail(secret_record(&self.plugin, &status), text).into())
    }
}

/// The value from a piped stdin, or from a no-echo prompt when stdin is a
/// terminal. One trailing newline is dropped: `echo token |` and a typed
/// line both end with one that is not part of the value.
fn read_secret_value(plugin: &str, name: &str) -> Result<PluginSecretValue, OrbitError> {
    let mut raw = if crate::output::sink::stdin_is_terminal() {
        prompt_without_echo(&format!("Value for {plugin} secret '{name}': "))?
    } else {
        let mut raw = String::new();
        std::io::stdin()
            .take((MAX_PLUGIN_SECRET_BYTES + 2) as u64)
            .read_to_string(&mut raw)
            .map_err(|error| {
                OrbitError::InvalidInput(format!("read the secret from stdin: {error}"))
            })?;
        raw
    };
    if raw.ends_with('\n') {
        raw.pop();
        if raw.ends_with('\r') {
            raw.pop();
        }
    }
    PluginSecretValue::new(raw)
}

/// Read one line from the terminal with echo off, restoring the terminal
/// whatever happens.
#[cfg(unix)]
fn prompt_without_echo(prompt: &str) -> Result<String, OrbitError> {
    use std::io::{BufRead, Write};
    use std::os::fd::AsRawFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    // SAFETY: `termios` is plain data that `tcgetattr` fills in completely
    // before it is read.
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is this process's stdin for the whole call, and
    // `original` is a valid, writable `termios`.
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return Err(OrbitError::Io(format!(
            "read the terminal settings: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut silent = original;
    silent.c_lflag &= !libc::ECHO;
    silent.c_lflag |= libc::ECHONL;
    // SAFETY: as above; `silent` is a copy of the settings just read.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &silent) } != 0 {
        return Err(OrbitError::Io(format!(
            "turn off terminal echo: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "{prompt}");
    let _ = stderr.flush();
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    // SAFETY: restores the settings read above on the same descriptor.
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    read.map_err(|error| OrbitError::InvalidInput(format!("read the secret: {error}")))?;
    Ok(line)
}

#[cfg(not(unix))]
fn prompt_without_echo(_prompt: &str) -> Result<String, OrbitError> {
    Err(OrbitError::InvalidInput(
        "this platform has no no-echo prompt; pipe the secret on stdin".to_string(),
    ))
}

#[derive(Args)]
pub struct PluginSecretListArgs {
    /// Plugin namespace
    pub plugin: String,
}

impl Execute for PluginSecretListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        use crate::output::table::{Column, Table};
        use comfy_table::Cell;

        let secrets = runtime.list_plugin_secrets(&self.plugin)?;
        let mut table = Table::new(vec![
            Column::new("SECRET").fixed(),
            Column::new("STATE").fixed(),
            Column::new("UPDATED").fixed(),
            Column::new("DESCRIPTION"),
        ])
        .empty_message("this plugin declares no secrets");
        for secret in &secrets {
            table.add_row(vec![
                Cell::new(&secret.name),
                Cell::new(secret_state(secret)),
                Cell::new(secret.updated_at.as_deref().unwrap_or("-")),
                Cell::new(&secret.description),
            ]);
        }
        let records = secrets
            .iter()
            .map(|secret| secret_record(&self.plugin, secret))
            .collect::<Vec<_>>();
        Ok(Payload::list(records, table).into())
    }
}

fn secret_state(secret: &PluginSecretStatus) -> &'static str {
    match (secret.declared, secret.set) {
        (true, true) => "set",
        (true, false) => "unset",
        (false, _) => "undeclared",
    }
}

fn secret_record(plugin: &str, secret: &PluginSecretStatus) -> serde_json::Value {
    json!({
        "plugin": plugin,
        "name": secret.name,
        "description": secret.description,
        "rotatable": secret.rotatable,
        "declared": secret.declared,
        "set": secret.set,
        "updated_at": secret.updated_at,
    })
}

#[derive(Args)]
pub struct PluginSecretRmArgs {
    /// Plugin namespace
    pub plugin: String,
    /// Secret name
    pub name: String,
}

impl Execute for PluginSecretRmArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let removed = runtime.remove_plugin_secret(&self.plugin, &self.name)?;
        let text = if removed {
            format!(
                "Removed secret '{}' from plugin '{}'.",
                self.name, self.plugin
            )
        } else {
            format!(
                "Plugin '{}' has no secret '{}' set; nothing was removed.",
                self.plugin, self.name
            )
        };
        Ok(Payload::detail(
            json!({ "plugin": self.plugin, "name": self.name, "removed": removed }),
            text,
        )
        .into())
    }
}
