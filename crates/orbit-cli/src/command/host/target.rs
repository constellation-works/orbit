use clap::Args;

// The HOST argument that `show`, `online` and `offline` take.
#[derive(Args)]
pub struct HostShowArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
}
