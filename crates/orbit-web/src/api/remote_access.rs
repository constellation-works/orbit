//! `GET /api/remote-access`: the trusted-host policy, the Tailscale Serve
//! check, and the connections and refusals seen through trusted hosts
//! (Observe › Fleet › Remote access). Read-only; it writes no config and
//! never runs a Tailscale command that changes state.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::process::{Command, Stdio};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::Env;
use serde_json::{Value, json};

use super::blocking;
use crate::state::{
    ACTIVE_WINDOW_MINUTES, DashboardState, REFUSAL_WINDOW_HOURS, RemoteAccess, TailscaleReading,
};

/// How long a `tailscale serve status` reading is reused.
const SERVE_STATUS_TTL: Duration = Duration::from_secs(30);
/// How long a `tailscale whois` reading is reused.
const WHOIS_TTL: Duration = Duration::from_secs(10 * 60);
/// Deadline for one `tailscale` command.
const TAILSCALE_DEADLINE: Duration = Duration::from_secs(3);
/// Output kept from one `tailscale` command.
const TAILSCALE_OUTPUT_LIMIT: usize = 256 * 1024;
/// New `tailscale whois` lookups per request; the rest wait for a later poll.
const WHOIS_PER_REQUEST: usize = 8;

pub(super) async fn remote_access(State(state): State<DashboardState>) -> Response {
    match blocking("remote access", move || Ok(remote_access_json(&state))).await {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}

/// The endpoint body. Runs `tailscale` (bounded, cached), so call it off the
/// request worker.
fn remote_access_json(state: &DashboardState) -> Value {
    let access = state.remote_access();
    let policy = access.trusted_hosts();
    let hosts: Vec<&str> = policy.hosts().collect();
    let logins: Vec<&str> = policy.logins().collect();

    // The keys are read at serve start; the file may have changed since.
    let (configured, config_error) =
        match orbit_config::load_machine_web_settings(state.global_root()) {
            Ok(web) => (Some(web), None),
            Err(error) => (None, Some(error.to_string())),
        };
    let configured_policy_error = configured.as_ref().and_then(|web| {
        crate::state::TrustedHosts::new(web.trusted_hosts.clone(), web.trusted_logins.clone())
            .err()
            .map(|error| error.to_string())
    });
    let restart_required = configured_policy_error.is_none()
        && access.is_from_config()
        && configured
            .as_ref()
            .is_some_and(|web| !policy.matches(&web.trusted_hosts, &web.trusted_logins));

    let tailscale = serve_status(&access, state.env());
    let served = served_names(&tailscale, access.bound_addr());
    let warnings = warnings(&hosts, &tailscale, &served);

    let activity = access.snapshot();
    let mut whois_budget = WHOIS_PER_REQUEST;
    let devices: Vec<Value> = activity
        .devices
        .iter()
        .map(|device| {
            let ip = device
                .ip
                .as_deref()
                .and_then(|ip| ip.parse::<IpAddr>().ok());
            let name = ip.and_then(|ip| device_name(&access, state.env(), ip, &mut whois_budget));
            json!({
                "ip": device.ip,
                "device": name,
                "login": device.login,
                "open_streams": device.open_streams,
                "requests_15m": device.requests_15m,
                "last_seen": device.last_seen,
            })
        })
        .collect();
    let open_streams: u32 = activity
        .devices
        .iter()
        .map(|device| device.open_streams)
        .sum();
    let active_devices = activity.active_devices();
    let refused: u64 = activity.refusals.iter().map(|item| item.count_24h).sum();

    json!({
        "enabled": !hosts.is_empty(),
        "from_config": access.is_from_config(),
        "trusted_hosts": hosts.iter().map(|host| json!({"host": host, "url": format!("https://{host}")})).collect::<Vec<_>>(),
        "trusted_logins": logins,
        "restart_required": restart_required,
        "configured": configured.map(|web| json!({
            "trusted_hosts": web.trusted_hosts,
            "trusted_logins": web.trusted_logins,
        })),
        "configured_policy_error": configured_policy_error,
        "config_error": config_error,
        "dashboard_port": access.bound_port(),
        "tailscale": tailscale_json(&tailscale, &served),
        "warnings": warnings,
        "connections": {
            "tracking_since": access.tracking_since(),
            "window_minutes": ACTIVE_WINDOW_MINUTES,
            "open_streams": open_streams,
            "active_devices": active_devices,
            "devices": devices,
        },
        "refusals": {
            "window_hours": REFUSAL_WINDOW_HOURS,
            "total": refused,
            "items": activity.refusals,
        },
    })
}

/// Run `tailscale <args>` with only `PATH` in its environment, bounded in
/// time and output, and read its stdout as JSON.
fn tailscale(env: &Env, args: &[&str]) -> TailscaleReading {
    let mut command = Command::new("tailscale");
    command.args(args).env_clear().stdin(Stdio::null());
    if let Some(path) = env.var("PATH") {
        command.env("PATH", path);
    }
    match orbit_common::process::run_bounded_capped_typed(
        &mut command,
        TAILSCALE_DEADLINE,
        TAILSCALE_OUTPUT_LIMIT,
    ) {
        Err(orbit_common::process::BoundedRunError::Spawn(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            TailscaleReading::Missing
        }
        Err(error) => TailscaleReading::Error(orbit_common::OrbitError::from(error).to_string()),
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            TailscaleReading::Error(format!(
                "tailscale {} exited with {}{}{}",
                args.first().copied().unwrap_or_default(),
                output.status,
                if line.is_empty() { "" } else { ": " },
                line.chars().take(200).collect::<String>()
            ))
        }
        Ok(output) => serde_json::from_slice(&output.stdout).map_or_else(
            |error| TailscaleReading::Error(format!("unreadable tailscale output: {error}")),
            TailscaleReading::Json,
        ),
    }
}

fn serve_status(access: &RemoteAccess, env: &Env) -> TailscaleReading {
    access.serve_status_or_probe(SERVE_STATUS_TTL, || {
        tailscale(env, &["serve", "status", "--json"])
    })
}

/// The device's tailnet name from `tailscale whois`, cached per IP. A new
/// lookup spends one of the request's `budget`.
fn device_name(access: &RemoteAccess, env: &Env, ip: IpAddr, budget: &mut usize) -> Option<String> {
    let reading = access.whois_or_probe(ip, WHOIS_TTL, budget, || {
        tailscale(env, &["whois", "--json", &ip.to_string()])
    })?;
    let TailscaleReading::Json(whois) = reading else {
        return None;
    };
    let node = &whois["Node"];
    node["ComputedName"]
        .as_str()
        .filter(|name| !name.is_empty())
        .or_else(|| node["Name"].as_str()?.split('.').next())
        .map(ToOwned::to_owned)
}

/// One Serve web handler: `https://<name>:<port><path>` proxied to `proxy`.
struct Handler {
    name: String,
    port: u16,
    path: String,
    proxy: String,
    funnel: bool,
    to_dashboard: bool,
}

/// Every web handler in a `tailscale serve status --json` config, including
/// foreground sessions.
fn handlers(config: &Value, dashboard: Option<SocketAddr>, out: &mut Vec<Handler>) {
    if let Some(web) = config["Web"].as_object() {
        for (host_port, site) in web {
            let (name, port) = host_port
                .rsplit_once(':')
                .and_then(|(name, port)| Some((name, port.parse().ok()?)))
                .unwrap_or((host_port.as_str(), 443));
            let funnel = config["AllowFunnel"][host_port].as_bool() == Some(true);
            for (path, handler) in site["Handlers"].as_object().into_iter().flatten() {
                let proxy = handler["Proxy"].as_str().unwrap_or_default().to_string();
                out.push(Handler {
                    name: name.to_ascii_lowercase(),
                    port,
                    path: path.clone(),
                    to_dashboard: dashboard.is_some_and(|dashboard| proxies_to(&proxy, dashboard)),
                    proxy,
                    funnel,
                });
            }
        }
    }
    for session in config["Foreground"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
    {
        handlers(session, dashboard, out);
    }
}

/// Whether a Serve proxy target (`7878`, `localhost:7878`,
/// `http://127.0.0.1:7878`, `http://[::1]:7878`) is this dashboard's listener.
/// A bare port or `localhost` matches the listener's port on either family.
/// An explicit IP must be the listener's own address: the other loopback
/// family on the same port is a different socket.
fn proxies_to(proxy: &str, dashboard: SocketAddr) -> bool {
    if let Ok(bare) = proxy.parse::<u16>() {
        return bare == dashboard.port();
    }
    let target = if proxy.contains("://") {
        proxy.to_string()
    } else {
        format!("http://{proxy}")
    };
    url::Url::parse(&target).is_ok_and(|url| {
        let same_host = match url.host() {
            Some(url::Host::Ipv4(ip)) => dashboard.ip() == IpAddr::V4(ip),
            Some(url::Host::Ipv6(ip)) => dashboard.ip() == IpAddr::V6(ip),
            Some(url::Host::Domain(name)) => name == "localhost",
            None => false,
        };
        url.scheme() == "http" && url.port_or_known_default() == Some(dashboard.port()) && same_host
    })
}

/// Serve's handlers, when the reading is JSON.
fn served_names(reading: &TailscaleReading, dashboard: Option<SocketAddr>) -> Vec<Handler> {
    let mut out = Vec::new();
    if let TailscaleReading::Json(config) = reading {
        handlers(config, dashboard, &mut out);
    }
    out
}

fn tailscale_json(reading: &TailscaleReading, handlers: &[Handler]) -> Value {
    let (state, error) = match reading {
        TailscaleReading::Json(_) => ("ok", None),
        TailscaleReading::Missing => ("missing", None),
        TailscaleReading::Error(error) => ("error", Some(error.as_str())),
    };
    json!({
        "state": state,
        "error": error,
        "handlers": handlers.iter().map(|handler| json!({
            "url": if handler.port == 443 {
                format!("https://{}{}", handler.name, handler.path)
            } else {
                format!("https://{}:{}{}", handler.name, handler.port, handler.path)
            },
            "host": handler.name,
            "proxy": handler.proxy,
            "funnel": handler.funnel,
            "to_dashboard": handler.to_dashboard,
        })).collect::<Vec<_>>(),
    })
}

/// Serve and policy disagreements. Only judged when Serve could be read.
fn warnings(hosts: &[&str], reading: &TailscaleReading, handlers: &[Handler]) -> Vec<Value> {
    if !matches!(reading, TailscaleReading::Json(_)) {
        return Vec::new();
    }
    let dashboard: Vec<&Handler> = handlers
        .iter()
        .filter(|handler| handler.to_dashboard)
        .collect();
    let served: BTreeSet<&str> = dashboard
        .iter()
        .filter(|handler| handler.port == 443)
        .map(|handler| handler.name.as_str())
        .collect();
    let mut warnings = Vec::new();
    for name in &served {
        if !hosts.contains(name) {
            warnings.push(json!({"code": "serve_host_not_trusted", "host": name}));
        }
    }
    for host in hosts {
        if !served.contains(host) {
            warnings.push(json!({"code": "trusted_host_not_served", "host": host}));
        }
    }
    let funnels: BTreeSet<&str> = dashboard
        .iter()
        .filter(|handler| handler.funnel)
        .map(|handler| handler.name.as_str())
        .collect();
    for name in funnels {
        warnings.push(json!({"code": "funnel_enabled", "host": name}));
    }
    warnings
}
