//! `orbit plugin test <dir>`: run a plugin's `spec.tests` goldens through the
//! real protocol and record the Orbit version they passed on (design §5).
//!
//! A temp directory stands in for the Orbit global root and the workspace, so
//! template paths (`{{workspace}}`, `{{plugin_root}}`, `{{plugin_state}}`,
//! `{{config.<key>}}`) do not touch the operator's Orbit state. The backend
//! runs under the profile the manifest requests for those template paths,
//! `network: loopback`, and `orbit_tools`: the run answers whether the plugin
//! would work once those grants are recorded.
//!
//! The run refuses, and the error prints the requested grant set, when the
//! manifest asks for an unconfined backend (`sandbox: none`), an `fs.write`
//! root that resolves outside the temp directory, `network: any`, or any
//! `env_pass`, unless the caller passes `--accept-requested` or a `--grant`
//! list that names each of those grants. Write roots are judged where the
//! backend would open them: rendered with the effective configuration,
//! resolved against the plugin root and followed through symlinks, so a
//! `{{config.<key>}}` default or a relative `../` root naming a host
//! directory needs consent like an absolute one. With that consent the run
//! uses the requested profile. Consent applies to this run only; it does not
//! record a host grant, and it never lifts the protected-path refusal.
//!
//! Secrets come from the goldens alone: a case's `secrets` map is delivered
//! the way the host delivers stored values (`context.secrets` /
//! `_meta.orbit.secrets`, each at version `fixture`), and the host's own
//! secret store is never opened, so a run cannot read — or certify against —
//! a real credential.
//!
//! Certification is written only when this host has the same plugin
//! installed at the same manifest digest: a directory that differs from the
//! installed tree says nothing about the tree the host would run.
//!
//! Inside a sandbox that refuses the backend's own (a macOS worker), no case
//! runs: the run defers to a host replay or refuses, typed
//! ([`gate`]). A deferred run certifies nothing.

mod golden;
mod model;
mod runner;

pub use gate::PluginTestValidationGate;
pub use model::PluginTestDeferral;
pub use model::PluginTestOptions;
pub use model::PluginTestOutcome;
pub use model::PluginTestReport;
pub use runner::test_plugin_dir;
mod gate;
