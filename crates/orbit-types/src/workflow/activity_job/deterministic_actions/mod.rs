//! Shared deterministic action names and their dispatch ownership.

mod admission;
mod ci;
mod delivery;
mod execution;
mod maintenance;
mod resident;
mod review;

use admission::admission_actions;
use ci::ci_actions;
use delivery::delivery_actions;
use execution::execution_actions;
use maintenance::maintenance_actions;
use resident::resident_actions;
use review::review_actions;

// Each domain appends declarations to the core and engine lists. Only the
// completed lists reach the generator, so both dispatch enums stay exhaustive.
macro_rules! deterministic_action_catalog {
    ($declare:ident) => {
        deterministic_action_catalog! {
            @collect $declare;
            [
                admission_actions ci_actions delivery_actions execution_actions
                maintenance_actions resident_actions review_actions
            ];
            [] []
        }
    };
    (@collect $declare:ident; [$domain:ident $($remaining:ident)*]; $core:tt $engine:tt) => {
        $domain! {
            deterministic_action_catalog,
            [$declare; [$($remaining)*]; $core $engine]
        }
    };
    (@collect $declare:ident; []; [$($core:tt)*] [$($engine:tt)*]) => {
        $declare! {
            core { $($core)* }
            engine { $($engine)* }
        }
    };
    (
        [$declare:ident; $remaining:tt; [$($core:tt)*] [$($engine:tt)*]];
        core { $($new_core:tt)* }
        engine { $($new_engine:tt)* }
    ) => {
        deterministic_action_catalog! {
            @collect $declare; $remaining;
            [$($core)* $($new_core)*] [$($engine)* $($new_engine)*]
        }
    };
}

// Keep existing discriminants stable when declarations move between domains.
// Assign new variants an unused discriminant in the owning enum.
macro_rules! define_deterministic_actions {
    (
        core { $( $core_variant:ident = $core_id:literal => $core_name:literal, )* }
        engine { $( $engine_variant:ident = $engine_id:literal => $engine_name:literal, )* }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CoreDeterministicAction {
            $( $core_variant = $core_id, )*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum EngineDeterministicAction {
            $( $engine_variant = $engine_id, )*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeterministicAction {
            Core(CoreDeterministicAction),
            Engine(EngineDeterministicAction),
        }

        impl CoreDeterministicAction {
            pub const fn name(self) -> &'static str {
                match self {
                    $( Self::$core_variant => $core_name, )*
                }
            }
        }

        impl EngineDeterministicAction {
            pub const fn name(self) -> &'static str {
                match self {
                    $( Self::$engine_variant => $engine_name, )*
                }
            }
        }

        impl DeterministicAction {
            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $( $core_name => Some(Self::Core(CoreDeterministicAction::$core_variant)), )*
                    $( $engine_name => Some(Self::Engine(EngineDeterministicAction::$engine_variant)), )*
                    _ => None,
                }
            }

            pub const fn name(self) -> &'static str {
                match self {
                    Self::Core(action) => action.name(),
                    Self::Engine(action) => action.name(),
                }
            }
        }
    };
}

deterministic_action_catalog!(define_deterministic_actions);
