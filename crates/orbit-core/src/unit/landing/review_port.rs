//! Landing's adapter for the review-owned dispatch port.

use crate::OrbitRuntime;

pub(crate) struct ReviewPort<'a>(pub(crate) &'a OrbitRuntime);
