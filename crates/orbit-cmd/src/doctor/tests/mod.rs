//! Sibling tests for destructive workspace doctor repairs.

use std::fs;
use std::path::Path;

use chrono::Utc;
use fs2::FileExt;
use orbit_core::OrbitRuntime;

use crate::doctor::DoctorCommands;

mod automation;
mod task;
mod workspace;

use workspace::*;
