//! Each [`RuntimeHost`](crate::context::hosts::RuntimeHost) capability trait implemented for every
//! [`NullRuntimeHost`] by forwarding to its default answer.

mod dispatch;

mod environment;

mod lifecycle;

mod recovery;

mod review;

use crate::context::hosts::{NullRuntimeHost, RuntimeHostProcessEnvironment};

impl<T: NullRuntimeHost> RuntimeHostProcessEnvironment for T {
    fn process_env(&self) -> orbit_common::Env {
        NullRuntimeHost::process_env(self)
    }
}
