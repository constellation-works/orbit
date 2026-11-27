//! The `RuntimeHost` capability traits implemented for [`EngineHost`], one
//! file per trait.

mod dispatch;

mod environment;

mod lifecycle;

mod private_vcs;

mod recovery;

mod review;

use orbit_engine::RuntimeHostProcessEnvironment;

use crate::OrbitRuntime;
use crate::application::contracts::EngineHostPort;

/// Adapter that implements `orbit-engine`'s host traits over a borrowed
/// [`OrbitRuntime`].
#[derive(Clone, Copy)]
pub struct EngineHost<'a> {
    runtime: &'a OrbitRuntime,
}

impl<'a> EngineHost<'a> {
    /// Wrap `runtime` in the engine-host adapter.
    pub fn new(runtime: &'a OrbitRuntime) -> Self {
        Self { runtime }
    }
}

impl RuntimeHostProcessEnvironment for EngineHost<'_> {
    fn process_env(&self) -> orbit_common::Env {
        self.runtime.env().clone()
    }
}

impl EngineHostPort for OrbitRuntime {
    type Host<'a> = EngineHost<'a>;

    fn engine_host(&self) -> EngineHost<'_> {
        EngineHost::new(self)
    }
}
