use orbit_common::{Env, env_vars};
use std::ffi::OsStr;

#[test]
fn from_vars_serves_accessors_and_lists_without_the_process_environment() {
    let env = Env::from_vars([
        (env_vars::HOME, "/home/orbit"),
        ("PLUGIN_SECRET", "first"),
        ("PLUGIN_SECRET", "second"),
    ]);

    assert_eq!(env.home(), Some(OsStr::new("/home/orbit")));
    assert_eq!(env.cargo_home(), None);
    assert_eq!(env.var("PLUGIN_SECRET"), Some(OsStr::new("second")));
    assert_eq!(
        env.vars().collect::<Vec<_>>(),
        [
            (OsStr::new("HOME"), OsStr::new("/home/orbit")),
            (OsStr::new("PLUGIN_SECRET"), OsStr::new("second")),
        ]
    );
    assert!(!format!("{env:?}").contains("second"));
}
