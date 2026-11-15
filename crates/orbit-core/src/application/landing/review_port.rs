//! Landing's implementation of the review-owned dispatch port.

use crate::OrbitRuntime;
use crate::application::review::ReviewLandingDispatchPort;

impl ReviewLandingDispatchPort for OrbitRuntime {
    fn dispatch_recorded_landing_authority(&self) {
        super::dispatch_recorded_authority(self);
    }
}
