//! Test-only implementations of individual host capabilities.

mod dispatch;

mod environment;

mod error;

mod execution;

mod lifecycle;

mod recovery;

mod review;

// Macro expansions use these paths so a fixture's crate need not declare
// dependencies solely for signatures of the default methods.
pub use chrono;
pub use orbit_common;
pub use orbit_common::OrbitError;
pub use orbit_exec;
pub use orbit_store;
pub use orbit_tools;
pub use orbit_types;
pub use serde_json::Value;

pub use error::{unsupported_dispatch_capability, unsupported_runtime_capability};
pub use execution::{
    baseline_activity_tools, overlay_crew_env, refuse_script_step, unavailable_deferral_probe,
    validate_input_crew,
};

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_impl {
    (
        impl $([$($generics:tt)*])? $capability:ident for $host:ty {
            $( $(#[$attribute:meta])* fn $name:ident $parameters:tt $(-> $result:ty)? {
                $($body:tt)*
            } )*
        };
        $($(#[$default_attribute:meta])* fn $default:ident $default_parameters:tt
            $(-> $default_result:ty)? { $($default_body:tt)* }
        )*
    ) => {
        const _: () = {
            use $crate::test_support::host as fixture;

            // A supplied method takes precedence over its fixture default.
            macro_rules! default_method {
                $(($name) => {}; )*
                $(
                    ($default) => {
                        $(#[$default_attribute])*
                        fn $default $default_parameters $(-> $default_result)? {
                            $($default_body)*
                        }
                    };
                )*
            }

            impl $($($generics)*)? $crate::$capability for $host {
                $( $(#[$attribute])* fn $name $parameters $(-> $result)? { $($body)* } )*
                $(default_method!($default);)*
            }
        };
    };
}

/// Implement one host capability for a fixture, supplying only the methods
/// the fixture customizes. Every other method uses that capability's fixture
/// answer. Each required capability has to be implemented separately.
///
/// The process-environment fixture captures the current process environment.
/// A fixture that supplies a different snapshot implements
/// `RuntimeHostProcessEnvironment` directly.
#[macro_export]
macro_rules! impl_test_host {
    (impl $([$($generics:tt)*])? RuntimeHostProcessEnvironment for $host:ty {}) => {
        impl $($($generics)*)? $crate::RuntimeHostProcessEnvironment for $host {
            fn process_env(&self) -> $crate::test_support::host::orbit_common::Env {
                $crate::test_support::host::orbit_common::Env::capture()
            }
        }
    };
    (impl $([$($generics:tt)*])? Dispatch $($rest:tt)*) => {
        $crate::__orbit_test_host_dispatch! { impl $([$($generics)*])? Dispatch $($rest)* }
    };
    (impl $([$($generics:tt)*])? HostEnvironment $($rest:tt)*) => {
        $crate::__orbit_test_host_environment! {
            impl $([$($generics)*])? HostEnvironment $($rest)*
        }
    };
    (impl $([$($generics:tt)*])? Recovery $($rest:tt)*) => {
        $crate::__orbit_test_host_recovery! { impl $([$($generics)*])? Recovery $($rest)* }
    };
    (impl $([$($generics:tt)*])? ReviewGate $($rest:tt)*) => {
        $crate::__orbit_test_host_review! { impl $([$($generics)*])? ReviewGate $($rest)* }
    };
    (impl $([$($generics:tt)*])? RunLifecycle $($rest:tt)*) => {
        $crate::__orbit_test_host_lifecycle! { impl $([$($generics)*])? RunLifecycle $($rest)* }
    };
}
