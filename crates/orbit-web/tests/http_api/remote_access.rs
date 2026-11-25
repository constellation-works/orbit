use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};

use super::support::{Fixture, isolated};

/// A `tailscale` stub: `serve status --json` prints `serve.json` beside it,
/// `whois --json <ip>` names the device `phone`.
fn tailscale_stub(fixture: &Fixture) -> std::path::PathBuf {
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("tailscale");
    fs::write(
        &script,
        "#!/bin/sh\ncase \"$1\" in\n\
         serve) cat \"$(dirname \"$0\")/serve.json\" ;;\n\
         whois) echo '{\"Node\":{\"ComputedName\":\"phone\",\"Name\":\"phone.tailnet.ts.net.\"},\"UserProfile\":{\"LoginName\":\"alice@example.com\"}}' ;;\n\
         *) exit 2 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Point Serve's `<name>` handler at the server's own port.
fn serve_json(bin: &std::path::Path, name: &str, origin: &str) {
    let port = origin.rsplit(':').next().unwrap();
    fs::write(
        bin.join("serve.json"),
        json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {format!("{name}:443"): {"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{port}")}}}},
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn remote_access_reports_policy_serve_check_and_only_trusted_host_traffic() {
    isolated(
        "remote_access::remote_access_reports_policy_serve_check_and_only_trusted_host_traffic",
        || {
            // Off: no trusted hosts; Serve proxying here is flagged.
            let fixture = Fixture::new();
            let bin = tailscale_stub(&fixture);
            let off = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &off.origin);
            let body = super::support::json_ok(off.get("/api/remote-access"));
            assert_eq!(body["enabled"], false, "{body}");
            assert_eq!(body["trusted_hosts"], json!([]));
            assert_eq!(body["restart_required"], false);
            assert_eq!(body["tailscale"]["state"], "ok", "{body}");
            assert_eq!(
                body["tailscale"]["handlers"][0]["to_dashboard"], true,
                "{body}"
            );
            assert_eq!(
                body["warnings"],
                json!([{"code": "serve_host_not_trusted", "host": "dash.tailnet.ts.net"}])
            );
            assert_eq!(body["connections"]["devices"], json!([]));
            assert_eq!(body["refusals"]["total"], 0);
            drop(off);

            // On: one trusted host and login.
            let config = fixture.global.join("config.toml");
            let base = fs::read_to_string(&config).unwrap();
            fs::write(
                &config,
                format!(
                    "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
                     trusted_logins = [\"alice@example.com\"]\n"
                ),
            )
            .unwrap();
            fs::remove_file(bin.join("serve.json")).unwrap();
            let on = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &on.origin);
            let api = "/api/tasks?workspace=ws_http_fixture";
            let trusted = |login: &str| {
                on.request("GET", api)
                    .header("host", "dash.tailnet.ts.net")
                    .header("tailscale-user-login", login)
                    .header("x-forwarded-for", "100.64.0.7")
                    .send()
                    .unwrap()
            };
            // Loopback traffic is never counted.
            assert!(on.get(api).status().is_success());
            assert!(trusted("alice@example.com").status().is_success());
            assert_eq!(trusted("mallory@example.com").status().as_u16(), 403);

            let body = super::support::json_ok(on.get("/api/remote-access"));
            assert_eq!(body["enabled"], true, "{body}");
            assert_eq!(
                body["trusted_hosts"],
                json!([{"host": "dash.tailnet.ts.net", "url": "https://dash.tailnet.ts.net"}])
            );
            assert_eq!(body["trusted_logins"], json!(["alice@example.com"]));
            assert_eq!(body["restart_required"], false);
            assert_eq!(body["warnings"], json!([]), "{body}");
            let connections = &body["connections"];
            assert!(connections["tracking_since"].is_string(), "{body}");
            assert_eq!(connections["active_devices"], 1);
            assert_eq!(connections["open_streams"], 0);
            let devices = connections["devices"].as_array().unwrap();
            assert_eq!(devices.len(), 1, "{body}");
            assert_eq!(devices[0]["ip"], "100.64.0.7");
            assert_eq!(devices[0]["device"], "phone");
            assert_eq!(devices[0]["login"], "alice@example.com");
            assert_eq!(devices[0]["requests_15m"], 1);
            let refusals = &body["refusals"];
            assert_eq!(refusals["total"], 1);
            let items: Vec<Value> = refusals["items"].as_array().unwrap().clone();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["reason"], "unlisted_login");
            assert_eq!(items[0]["login"], "mallory@example.com");
            assert_eq!(items[0]["count_24h"], 1);

            // The keys are read at start: an edit now needs a restart.
            fs::write(
                &config,
                format!(
                    "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
                     trusted_logins = [\"alice@example.com\", \"bob@example.com\"]\n"
                ),
            )
            .unwrap();
            let body = super::support::json_ok(on.get("/api/remote-access"));
            assert_eq!(body["restart_required"], true, "{body}");
        },
    );
}
