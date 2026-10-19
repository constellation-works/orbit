use super::{CliGroup, OperationSpec};

/// Registration of a noun's tools that have no MCP scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOnlyRegistration {
    /// Advertise the tool on the active local tool surface, without MCP.
    Active,
    /// Keep the tool reachable but hidden from the active and MCP surfaces.
    Inactive,
}

/// One noun: the strings every surface needs about it, and its operations.
#[derive(Debug, Clone, Copy)]
pub struct NounSpec<V: 'static> {
    /// The CLI command (`friction`, `auto-task`).
    pub cli_name: &'static str,
    /// The tool-name prefix (`orbit.friction`, `orbit.auto_task`). Separate
    /// from `cli_name` because shipped surfaces already spell them differently.
    pub tool_prefix: &'static str,
    /// Registration for operations whose `mcp_scope` is `None`.
    pub tool_only_registration: ToolOnlyRegistration,
    /// `orbit <noun> --help` summary.
    pub cli_about: &'static str,
    /// The audit `target_type` the noun's CLI rows carry.
    pub audit_target_type: &'static str,
    /// The parameter whose value is the audit `target_id`, in every verb that
    /// declares it (`id`, `name`).
    pub audit_target_param: &'static str,
    /// Intermediate CLI subcommands, in `--help` order.
    pub cli_groups: &'static [CliGroup],
    /// Every operation, in `--help` order.
    pub operations: &'static [OperationSpec<V>],
}

/// A noun's verb enum, as the generic surface adapters see it.
///
/// Implemented by the crate-internal `operation_registry!` macro, which emits
/// the enum, this impl and the ordered operations list from one declaration.
pub trait OperationVerb: Copy + Eq + std::fmt::Debug + Send + Sync + 'static {
    /// The noun this verb belongs to.
    fn noun() -> &'static NounSpec<Self>;

    /// This verb's specification.
    fn spec(self) -> &'static OperationSpec<Self>;

    /// The verb's fully-qualified tool name (`orbit.friction.add`).
    fn tool_name(self) -> &'static str {
        self.spec().tool_name
    }
}

/// Declare a noun's verb enum, its [`OperationVerb`] impl and its ordered
/// operations list from one list of `Variant => SPEC` entries.
///
/// The three cannot disagree: list order is both enum order and `--help`
/// order, and `spec()` is an exhaustive match generated from the same entries.
macro_rules! operation_registry {
    (
        noun: $noun:ident;
        $(#[$enum_meta:meta])*
        $enum_vis:vis enum $verb:ident;
        $(#[$list_meta:meta])*
        $list_vis:vis const $list:ident;
        {
            $( $(#[$variant_meta:meta])* $variant:ident => $spec:ident ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $enum_vis enum $verb {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $crate::governance::operation::OperationVerb for $verb {
            fn noun() -> &'static $crate::governance::operation::NounSpec<Self> {
                &$noun
            }

            fn spec(self) -> &'static $crate::governance::operation::OperationSpec<Self> {
                match self {
                    $( $verb::$variant => &$spec, )+
                }
            }
        }

        $(#[$list_meta])*
        $list_vis const $list: &[$crate::governance::operation::OperationSpec<$verb>] =
            &[ $( $spec ),+ ];
    };
}

pub(crate) use operation_registry;
