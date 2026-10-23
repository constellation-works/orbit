use super::{CliAction, CliArgKind, CliBinding, Description, ParamSpec, ParamType, Redaction};

// Const builders the registries declare their parameters with.

/// A parameter on both MCP and the CLI.
pub(crate) const fn both(
    name: &'static str,
    param_type: ParamType,
    mcp_description: &'static str,
    kind: CliArgKind,
    cli_help: &'static str,
) -> ParamSpec {
    let mut param = mcp_only(name, param_type, mcp_description);
    param.cli = Some(CliBinding {
        kind,
        help: Description::Static(cli_help),
        required: false,
    });
    param
}

/// A parameter the tool accepts and the CLI does not bind.
pub(crate) const fn mcp_only(
    name: &'static str,
    param_type: ParamType,
    mcp_description: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        aliases: &[],
        param_type,
        required: false,
        preserve_empty: false,
        default: None,
        redact: Redaction::None,
        mcp_description: Some(Description::Static(mcp_description)),
        cli: None,
    }
}

/// A CLI-only flag the CLI surface table consumes before the call.
pub(crate) const fn cli_only(
    name: &'static str,
    param_type: ParamType,
    kind: CliArgKind,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        aliases: &[],
        param_type,
        required: false,
        preserve_empty: false,
        default: None,
        redact: Redaction::None,
        mcp_description: None,
        cli: Some(CliBinding {
            kind,
            help: Description::Static(help),
            required: false,
        }),
    }
}

/// Bind a multiplexed operation's selector to the command line as one
/// subcommand per action. A subcommand is required, so the binding is too;
/// each action's own help replaces the selector's.
pub(crate) const fn cli_actions(mut param: ParamSpec, actions: &'static [CliAction]) -> ParamSpec {
    param.cli = Some(CliBinding {
        kind: CliArgKind::Actions(actions),
        help: Description::Static(""),
        required: true,
    });
    param
}

/// Mark a parameter required.
pub(crate) const fn required(mut param: ParamSpec) -> ParamSpec {
    param.required = true;
    param
}

/// Require a parameter's CLI flag although the wire leaves it optional.
pub(crate) const fn cli_required(mut param: ParamSpec) -> ParamSpec {
    if let Some(binding) = &mut param.cli {
        binding.required = true;
    }
    param
}

/// Give a parameter the value the CLI sends when its flag is omitted.
pub(crate) const fn defaulted(mut param: ParamSpec, default: &'static str) -> ParamSpec {
    param.default = Some(default);
    param
}

/// Redact a parameter as free text.
pub(crate) const fn free_text(mut param: ParamSpec) -> ParamSpec {
    param.redact = Redaction::FreeText;
    param
}

/// Keep an explicitly empty string the CLI was given.
pub(crate) const fn preserve_empty(mut param: ParamSpec) -> ParamSpec {
    param.preserve_empty = true;
    param
}

/// Accept other wire spellings for a parameter.
pub(crate) const fn aliased(mut param: ParamSpec, aliases: &'static [&'static str]) -> ParamSpec {
    param.aliases = aliases;
    param
}
