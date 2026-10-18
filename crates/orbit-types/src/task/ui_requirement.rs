//! The `ui` tag as a host capability: a task whose change a user sees needs a
//! host with a verified headless browser kit to render it and take its
//! screenshots.
//!
//! Only the exact `ui` tag routes, ignoring case and surrounding whitespace.
//! Topical tags such as `dashboard` or `ux` route nothing. Local admission and
//! pull admission both read [`requires_browser_kit`], so a drain, ship
//! discovery, `orbit run ship` and an owner handing out claims cannot disagree
//! about which tasks wait for the kit.

/// The tag of a task whose change a user sees.
pub const UI_TAG: &str = "ui";

/// What a host without a verified kit runs to get one: it installs the kit
/// and runs the smoke test that verifies it.
pub const BROWSER_KIT_INSTALL_COMMAND: &str = "orbit browser-kit install";

/// Whether `tags` carry the `ui` tag, so only a host with a verified browser
/// kit may start the task.
#[must_use]
pub fn requires_browser_kit(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| tag.trim().eq_ignore_ascii_case(UI_TAG))
}

/// What a `ui` task waits for on a host without a verified kit, naming the
/// command that gives a host one.
#[must_use]
pub fn browser_kit_wait() -> String {
    format!(
        "waits for a host with the browser kit (ui); run `{BROWSER_KIT_INSTALL_COMMAND}` on a \
         host that runs workers to give it one"
    )
}
