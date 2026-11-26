#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "macro_rules! resident_actions must precede its pub(super) use re-export in lexical macro scope"
)]

macro_rules! resident_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                ResidentFinalize = 28 => "resident_finalize",
                ResidentObserve = 29 => "resident_observe",
            }
            engine {}
        }
    };
}

pub(super) use resident_actions;
