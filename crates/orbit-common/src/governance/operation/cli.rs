use super::{Description, ParamSpec};
#[cfg(doc)]
use super::{OperationSpec, ParamType};

/// How a parameter appears on the command line.
#[derive(Debug, Clone, Copy)]
pub enum CliArgKind {
    /// A positional argument, optional (`[ID]`) when its parameter is.
    Positional,
    /// A `--long` flag taking one value.
    Flag {
        /// The flag spelling, without leading dashes. This is intentionally
        /// separate from the parameter name: `friction add` takes `--tag` but
        /// sends `tags` over the wire.
        long: &'static str,
        /// When set, one occurrence may carry several delimiter-separated
        /// values (`--tag a,b`). Only meaningful for [`ParamType::StringList`].
        delimiter: Option<char>,
        /// Other flag spellings accepted and hidden from `--help`.
        aliases: &'static [&'static str],
        /// Other flag spellings accepted and listed by `--help`.
        visible_aliases: &'static [&'static str],
        /// The `<VALUE_NAME>` `--help` shows, when it is not the parameter name
        /// in SCREAMING_SNAKE.
        value_name: Option<&'static str>,
        /// Whether one occurrence takes every value up to the next flag
        /// (`--fields id title`), shown as `<VALUE_NAME>...`. Only meaningful
        /// for list parameters.
        multiple: bool,
    },
    /// A multiplexed operation's action selector, as one subcommand per
    /// action. Each subcommand sends its action's value for this parameter
    /// and takes the arguments the action declares; the operation's other
    /// command-line arguments are inherited by every action.
    Actions(&'static [CliAction]),
}

/// One subcommand of a [`CliArgKind::Actions`] selector
/// (`orbit task reconcile-review submit`).
///
/// This is the command line's view of a multiplexed tool only. Which fields
/// an action accepts stays with the handler that serves it, as for every
/// other surface.
#[derive(Debug, Clone, Copy)]
pub struct CliAction {
    /// The subcommand name (`accept-baseline`).
    pub name: &'static str,
    /// The selector value the subcommand sends (`accept_baseline`).
    pub value: &'static str,
    /// `--help` summary for the subcommand.
    pub cli_about: &'static str,
    /// The subcommand's own arguments, in `--help` order. Each is bound to
    /// the operation's wire parameter of the same name, so it carries no MCP
    /// description of its own.
    pub params: &'static [ParamSpec],
}

impl CliArgKind {
    /// A `--<long>` flag with no aliases, delimiter or value-name override.
    pub const fn flag(long: &'static str) -> Self {
        CliArgKind::Flag {
            long,
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: None,
            multiple: false,
        }
    }
}

/// A parameter's command-line binding: how it is spelled and how it is helped.
#[derive(Debug, Clone, Copy)]
pub struct CliBinding {
    /// Positional or flag.
    pub kind: CliArgKind,
    /// Help text shown by `--help`. Frequently differs from the MCP description
    /// because the audiences differ; both are contract once shipped.
    pub help: Description,
    /// Whether the CLI requires the flag although the wire does not, as for
    /// a field only another mode of the operation may omit.
    pub required: bool,
}

/// A relationship between two CLI argument names.
#[derive(Debug, Clone, Copy)]
pub enum CliConstraint {
    /// The arguments may not be supplied together.
    ConflictsWith(&'static str, &'static str),
    /// The first argument is required unless the second is supplied.
    RequiredUnlessPresent(&'static str, &'static str),
}

/// A verb's `--help` beyond its summary and its parameters' help.
#[derive(Debug, Clone, Copy)]
pub struct CliHelp {
    /// The description `--help` prints in place of
    /// [`OperationSpec::cli_about`]; `-h` keeps the summary.
    pub long_about: Option<&'static str>,
    /// Text `--help` prints after the options, such as examples.
    pub after_help: Option<&'static str>,
    /// CLI parameter names in `--help` order, when that differs from the
    /// declaration order the MCP schema follows. Empty keeps declaration
    /// order; otherwise it names every CLI-bound parameter once.
    pub flag_order: &'static [&'static str],
    /// Argument relationships enforced by the CLI parser.
    pub constraints: &'static [CliConstraint],
}

impl CliHelp {
    /// No help beyond the summary, in declaration order.
    pub const NONE: Self = Self {
        long_about: None,
        after_help: None,
        flag_order: &[],
        constraints: &[],
    };
}

/// An intermediate CLI subcommand that groups verbs (`task artifact`).
///
/// The CLI adapter builds one intermediate group for two-segment paths,
/// including its help and inherited CLI-only options. Friction declares none.
#[derive(Debug, Clone, Copy)]
pub struct CliGroup {
    /// The group's subcommand name, matching the first `cli_path` segment of
    /// the verbs it holds.
    pub name: &'static str,
    /// `--help` summary for the group.
    pub cli_about: &'static str,
    /// CLI-only global arguments inherited by this group's verbs.
    pub params: &'static [ParamSpec],
}
