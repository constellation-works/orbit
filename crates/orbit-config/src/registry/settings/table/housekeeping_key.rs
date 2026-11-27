// Housekeeping and attention settings surround the workflow family in the
// existing registry order. Forward both sides to the workflow fragment.
macro_rules! with_housekeeping_keys {
    ($($head:tt)*) => {
        with_workflow_keys! {
            [
                $($head)*
    machine_mcp_toolsets: Vec<String> => Vec<String> {
        key: "machine.mcp.toolsets", value_type: "array<string>",
        description: "MCP toolsets orbit mcp serve and listen advertise beside the role or default surface.",
        section: ConfigSection::Machine, order: 85,
        allowed: each_of(&orbit_types::tool::McpToolset::SELECTION_NAMES),
        resolve: |raw: Option<Vec<String>>| resolve_mcp_toolsets(raw),
    },
    machine_mcp_role: Option<String> => String {
        key: "machine.mcp.role", value_type: "string",
        description: "Default MCP role orbit mcp serve and listen serve; unset serves the default surface.",
        section: ConfigSection::Machine, order: 86,
        allowed: one_of(&orbit_types::tool::McpRole::NAMES),
        resolve: |raw: Option<String>| resolve_mcp_role(raw),
    },
    machine_web_trusted_hosts: Vec<String> => Vec<String> {
        key: "machine.web.trusted_hosts", value_type: "array<string>",
        description: "DNS names besides loopback that orbit web serve admits as Host, such as a Tailscale Serve name; each request needs a listed Tailscale-User-Login.",
        section: ConfigSection::Machine, order: 87,
        tier: ConfigKeyTier::Advanced,
        allowed: format("DNS names without scheme or port"),
        resolve: |raw: Option<Vec<String>>| resolve_web_trusted_hosts(raw),
    },
    machine_web_trusted_logins: Vec<String> => Vec<String> {
        key: "machine.web.trusted_logins", value_type: "array<string>",
        description: "Tailscale login names orbit web serve admits through a trusted host; required when machine.web.trusted_hosts is set.",
        section: ConfigSection::Machine, order: 88,
        tier: ConfigKeyTier::Advanced,
        allowed: format("Tailscale login names"),
        resolve: |raw: Option<Vec<String>>| resolve_web_trusted_logins(raw),
    },
    machine_web_public_url: Option<String> => String {
        key: "machine.web.public_url", value_type: "string",
        description: "HTTP or HTTPS origin readers use to open this machine's dashboard links.",
        section: ConfigSection::Machine, order: 89,
        allowed: format("http or https origin without a path, query, or fragment"),
        resolve: |raw: Option<String>| resolve_web_public_url(raw),
    },
    decision_channel_enabled: bool => bool {
        key: "decision_channel.enabled", value_type: "bool",
        description: "Raise decision requests for work held on a human decision.",
        section: ConfigSection::Housekeeping, order: 95,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
            ]
    experiments_enabled: bool => bool {
        key: "experiments.enabled", value_type: "bool",
        description: "Whether experiments may start in this workspace.",
        section: ConfigSection::Housekeeping, order: 160,
        resolve: |raw: Option<bool>| Ok::<_, OrbitError>(raw.unwrap_or(false)),
    },
    seeding_routines: crate::SeedingPolicy => crate::seeding::RawSeedingPolicy {
        key: "seeding.routines", value_type: "string|array<string>",
        description: "Which shipped default routines this workspace seeds.",
        section: ConfigSection::Housekeeping, order: 170,
        allowed: format("all, none, or a list of routine names"),
        resolve: |raw: Option<crate::seeding::RawSeedingPolicy>| crate::seeding::admit_routine_seeding(raw),
    },
    seeding_auto_tasks: crate::SeedingPolicy => crate::seeding::RawSeedingPolicy {
        key: "seeding.auto_tasks", value_type: "string|array<string>",
        description: "Which shipped default auto-tasks this workspace seeds.",
        section: ConfigSection::Housekeeping, order: 171,
        allowed: format("all, none, or a list of auto-task names"),
        resolve: |raw: Option<crate::seeding::RawSeedingPolicy>| crate::seeding::admit_auto_task_seeding(raw),
    },
    experiments_config_keys: Vec<String> => Vec<String> {
        key: "experiments.config_keys", value_type: "array<string>",
        description: "Config keys an experiment arm may override in this workspace.",
        section: ConfigSection::Housekeeping, order: 162,
        tier: ConfigKeyTier::Advanced,
        resolve: |raw: Option<Vec<String>>| Ok::<_, OrbitError>(raw.unwrap_or_default()),
    },
        }
    };
}
