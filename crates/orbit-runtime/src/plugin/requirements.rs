//! A plugin's `requires` against this binary and machine (§4.8).

use orbit_tools::plugin::LoadedPlugin;
use orbit_types::plugin::{PLUGIN_HOST_API, SemverRange, Version};

/// `requires.orbit` / `requires.host_api` / `requires.platforms` against this
/// binary and machine. `None` means every requirement holds.
///
/// `host_api` one major behind [`PLUGIN_HOST_API`] is a requirement that
/// holds, not a mismatch (§4.8's compatibility window): the plugin still
/// registers, and [`host_api_deprecation`] is what surfaces the nudge to
/// rebuild for the current major. Every other value — ahead of this host, or
/// more than one major behind — refuses, same as an exact-match check would.
pub fn unmet_requirement(plugin: &LoadedPlugin) -> Option<String> {
    let requires = &plugin.manifest.spec.requires;
    if let Some(host_api) = requires.host_api
        && host_api != PLUGIN_HOST_API
        && Some(host_api) != PLUGIN_HOST_API.checked_sub(1)
    {
        return Some(format!(
            "plugin '{}' requires host_api {host_api}; this Orbit speaks {PLUGIN_HOST_API}. \
             Install a build of the plugin for this host API.",
            plugin.namespace()
        ));
    }
    if let Some(range) = &requires.orbit {
        let host_version = host_version();
        match SemverRange::parse(range) {
            Ok(range) if !range.matches(&host_version) => {
                return Some(format!(
                    "plugin '{}' requires orbit {range}; this host is {host_version}. Upgrade \
                     Orbit or install a plugin version that supports it.",
                    plugin.namespace()
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Some(format!(
                    "plugin '{}' declares an unreadable `requires.orbit`: {error}",
                    plugin.namespace()
                ));
            }
        }
    }
    let platform = current_platform();
    if !requires.platforms.is_empty()
        && !requires
            .platforms
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(platform))
    {
        return Some(format!(
            "plugin '{}' supports {} only; this machine is {platform}.",
            plugin.namespace(),
            requires.platforms.join(", ")
        ));
    }
    None
}

/// Whether `plugin` is running on [`unmet_requirement`]'s previous-major
/// grace: still active, but built for a `host_api` this host will refuse
/// once it drops the grace window. `orbit plugin doctor` reports this so the
/// deprecation is visible before the plugin actually breaks.
pub fn host_api_deprecation(plugin: &LoadedPlugin) -> Option<String> {
    let host_api = plugin.manifest.spec.requires.host_api?;
    if Some(host_api) == PLUGIN_HOST_API.checked_sub(1) {
        return Some(format!(
            "plugin '{}' declares host_api {host_api}; this Orbit speaks {PLUGIN_HOST_API} and \
             keeps the previous major working for now, but a future release will refuse it. \
             Install a build of the plugin for host_api {PLUGIN_HOST_API}.",
            plugin.namespace()
        ));
    }
    None
}

/// This binary's version, as `requires.orbit` compares against it.
pub fn host_version() -> Version {
    env!("CARGO_PKG_VERSION")
        .parse()
        .unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn current_platform() -> &'static str {
    std::env::consts::OS
}
