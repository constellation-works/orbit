//! The `RuntimeHost` capability traits implemented for `OrbitRuntime`, one
//! file per trait.

mod dispatch;

mod environment;

mod lifecycle;

mod private_vcs;

mod recovery;

mod review;

use orbit_engine::RuntimeHostProcessEnvironment;

use crate::OrbitRuntime;

impl RuntimeHostProcessEnvironment for OrbitRuntime {
    fn process_env(&self) -> orbit_common::Env {
        self.env().clone()
    }
}
