// Retain the assembler entry point used by keys_head. Family fragments
// forward tokens to define_config_settings! in their original order.
macro_rules! with_tail_keys {
    ($($head:tt)*) => {
        with_housekeeping_keys! { $($head)* }
    };
}
