//! [ORB-14451] Settings › Hosts' HTTP API, served by `orbit web serve` for a
//! local install whose remote hosts are real Orbit installations reached
//! through the fleet's fake `ssh` (see `host_fleet`).
//!
//! Every assertion compares the dashboard with `orbit host … --json` on the
//! same machine: the same rows, the same typed refusals, and the same
//! `hosts.toml` bytes.

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::{Value, json};

use crate::child_guard::ChildGuard;
use crate::git_repo;
use crate::host_fleet::{Fleet, host_row};

struct Dashboard {
    _server: ChildGuard,
    client: Client,
    origin: String,
}

impl Dashboard {
    /// Host-file edits are governed, so the operator's dashboard is served
    /// with `--operator`; without it the session has agent capability only.
    fn start(fleet: &Fleet, operator: bool) -> Self {
        let mut args = vec!["web", "serve", "--port", "0", "--no-open"];
        if operator {
            args.push("--operator");
        }
        let mut server = ChildGuard::new(
            fleet
                .process(&fleet.local.home, &fleet.local_repo, &args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn orbit web serve"),
        );
        let mut stdout = BufReader::new(server.stdout.take().expect("piped stdout"));
        let mut announcement = String::new();
        stdout
            .read_line(&mut announcement)
            .expect("read the dashboard announcement");
        // Keep draining so a chatty server never blocks on a full pipe.
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let authority = announcement
            .trim()
            .strip_prefix("Dashboard listening on http://")
            .unwrap_or_else(|| panic!("dashboard announcement: {announcement:?}"))
            .to_string();
        Self {
            _server: server,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(90))
                .build()
                .expect("HTTP client"),
            origin: format!("http://{authority}"),
        }
    }

    fn request(&self, method: &str, path: &str) -> RequestBuilder {
        self.client.request(
            method.parse().expect("HTTP method"),
            format!("{}{path}", self.origin),
        )
    }

    fn get(&self, path: &str) -> (u16, Value) {
        reply(self.request("GET", path).send().expect("GET"))
    }

    /// A mutation from the dashboard's own page: Origin matches the server.
    fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self.request(method, path).header("origin", &self.origin);
        if let Some(body) = body {
            request = request.json(&body);
        }
        reply(request.send().expect("mutation"))
    }
}

fn reply(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let text = response.text().expect("response body");
    let body = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    (status, body)
}

fn ok(status_and_body: (u16, Value)) -> Value {
    let (status, body) = status_and_body;
    assert!((200..300).contains(&status), "HTTP {status}: {body}");
    body
}

fn refused(status_and_body: (u16, Value), status: u16, code: &str) -> Value {
    let (actual, body) = status_and_body;
    assert_eq!(actual, status, "{body}");
    assert_eq!(body["code"], code, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "a refusal names what to do: {body}"
    );
    body
}

/// SSH targets the fake `ssh` was asked to reach, in order.
fn ssh_calls(fleet: &Fleet) -> usize {
    fs::read_to_string(fleet.routes.with_extension("calls"))
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

#[test]
fn dashboard_host_api_runs_the_cli_operations_with_its_shape_errors_and_bytes() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    let beta = fleet.install("beta", "beta", "BE");
    fleet.route("alpha", &alpha, "plain");
    fleet.route("alpha-again", &alpha, "plain");
    fleet.route("beta", &beta, "plain");
    fleet.route("myself", &fleet.local, "plain");
    let dashboard = Dashboard::start(&fleet, true);

    // Mutations need the dashboard's own loopback Origin; a refused request
    // never reaches the operation, so nothing is probed or written.
    let calls = ssh_calls(&fleet);
    for request in [
        dashboard
            .request("POST", "/api/hosts")
            .json(&json!({ "ssh": "alpha" })),
        dashboard
            .request("POST", "/api/hosts")
            .header("origin", "http://evil.example")
            .json(&json!({ "ssh": "alpha" })),
        dashboard
            .request("DELETE", "/api/hosts/alpha")
            .header("origin", "http://localhost:1"),
    ] {
        let (status, body) = reply(request.send().expect("cross-origin request"));
        assert_eq!(status, 403, "the origin guard refuses it: {body}");
    }
    assert_eq!(
        ssh_calls(&fleet),
        calls,
        "a refused origin opens no session"
    );
    assert!(fleet.hosts_bytes().is_none(), "and writes nothing");

    // A session without the operator capability lists hosts read-only, and
    // its edits are refused before the operation runs.
    let agent = Dashboard::start(&fleet, false);
    let listing = ok(agent.get("/api/hosts?probe=false"));
    assert_eq!(listing["host_edit"]["authorized"], false, "{listing}");
    for (method, path, body) in [
        ("POST", "/api/hosts", Some(json!({ "ssh": "alpha" }))),
        (
            "PATCH",
            "/api/hosts/alpha",
            Some(json!({ "name": "renamed" })),
        ),
        ("DELETE", "/api/hosts/alpha?force=true", None),
    ] {
        refused(agent.send(method, path, body), 403, "authorization_denied");
    }
    drop(agent);
    assert_eq!(ssh_calls(&fleet), calls, "a denied edit opens no session");
    assert!(fleet.hosts_bytes().is_none(), "and writes nothing");
    let listing = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(listing["host_edit"]["authorized"], true, "{listing}");

    // The ssh value is an SSH destination and nothing else, refused before
    // any process starts; the body takes no command or extra fields.
    for body in [
        json!({ "ssh": "-oProxyCommand=evil" }),
        json!({ "ssh": "alpha beta" }),
        json!({ "ssh": "alpha\tbeta" }),
        json!({ "ssh": "alpha;touch pwned" }),
        json!({ "ssh": "$(id)" }),
        json!({ "ssh": "alpha|cat" }),
        json!({ "ssh": "" }),
    ] {
        refused(
            dashboard.send("POST", "/api/hosts", Some(body)),
            400,
            "invalid_input",
        );
    }
    for body in [
        json!({ "ssh": "alpha", "command": "orbit mcp serve" }),
        json!({ "ssh": "alpha", "options": ["-oProxyCommand=evil"] }),
        json!({ "name": "alpha" }),
    ] {
        refused(
            dashboard.send("POST", "/api/hosts", Some(body)),
            400,
            "invalid_input",
        );
    }
    assert_eq!(ssh_calls(&fleet), calls, "no ssh process was started");
    assert!(fleet.hosts_bytes().is_none());

    // Add: the CLI's change report, and the file the CLI writes.
    let added = ok(dashboard.send("POST", "/api/hosts", Some(json!({ "ssh": "alpha" }))));
    assert_eq!(added["action"], "added", "{added}");
    assert_eq!(
        added["entry"],
        json!({ "name": "alpha", "machine_id": alpha.machine_id, "ssh": "alpha", "task_prefix": "AL" }),
        "{added}"
    );
    assert_eq!(added["host"]["reachable"], true, "{added}");
    let dashboard_bytes = fleet.hosts_bytes();
    fleet.json(&["host", "remove", "alpha", "--json"]);
    fleet.json(&["host", "add", "alpha", "--json"]);
    assert_eq!(
        fleet.hosts_bytes(),
        dashboard_bytes,
        "the dashboard leaves hosts.toml byte-identical to the CLI"
    );

    let before = fleet.hosts_bytes();
    for (body, status, code) in [
        (json!({ "ssh": "alpha-again" }), 409, "host_exists"),
        (
            json!({ "ssh": "beta", "name": "ALPHA" }),
            409,
            "host_name_conflict",
        ),
        (json!({ "ssh": "myself" }), 409, "host_is_local"),
        (json!({ "ssh": "nowhere" }), 502, "unreachable_destination"),
    ] {
        let ssh = body["ssh"].as_str().expect("ssh");
        let mut args = vec!["host", "add", ssh];
        if let Some(name) = body["name"].as_str() {
            args.extend(["--name", name]);
        }
        let (cli_code, _) = fleet.refused(&args);
        assert_eq!(cli_code, code, "the CLI refuses {body} the same way");
        refused(
            dashboard.send("POST", "/api/hosts", Some(body.clone())),
            status,
            code,
        );
        assert_eq!(fleet.hosts_bytes(), before, "{body} leaves the file intact");
    }
    ok(dashboard.send(
        "POST",
        "/api/hosts",
        Some(json!({ "ssh": "beta", "name": "build-box" })),
    ));

    // List and show: the rows `orbit host list|show --json` prints.
    let listed = ok(dashboard.get("/api/hosts"));
    let cli = fleet.json(&["host", "list", "--json"]);
    assert_eq!(listed["hosts"], cli["hosts"], "same rows as the CLI");
    assert_eq!(listed["host_file"], cli["host_file"]);
    assert_eq!(listed["legacy"], cli["legacy"]);
    assert!(listed["load_error"].is_null(), "{listed}");
    assert_eq!(listed["hosts"][0]["local"], true, "the local host first");
    assert_eq!(host_row(&listed, "alpha")["workspaces"][0]["role"], "owner");

    fleet.unroute("beta");
    let listed = ok(dashboard.get("/api/hosts"));
    let beta_row = host_row(&listed, "build-box");
    assert_eq!(beta_row["reachable"], false, "{beta_row}");
    assert_eq!(beta_row["error"]["code"], "unreachable_destination");
    assert_eq!(
        beta_row["machine_id"],
        beta.machine_id.as_str(),
        "an unreachable host keeps its cached row"
    );
    fleet.route("beta", &beta, "plain");

    let calls = ssh_calls(&fleet);
    let cached = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(ssh_calls(&fleet), calls, "probe=false opens no session");
    assert_eq!(
        cached["hosts"],
        fleet.json(&["host", "list", "--no-probe", "--json"])["hosts"]
    );
    assert!(host_row(&cached, "alpha")["reachable"].is_null());

    let shown = ok(dashboard.get("/api/hosts/ALPHA"));
    assert_eq!(shown, fleet.json(&["host", "show", "alpha", "--json"]));
    assert_eq!(
        shown["dependents"],
        json!({ "replica_checkouts": [], "pull_drains": [] })
    );
    refused(dashboard.get("/api/hosts/alp"), 404, "unknown_host");

    // Rename.
    let renamed = ok(dashboard.send(
        "PATCH",
        "/api/hosts/alpha",
        Some(json!({ "name": "primary" })),
    ));
    assert_eq!(renamed["action"], "renamed", "{renamed}");
    assert_eq!(renamed["previous_name"], "alpha");
    assert_eq!(renamed["entry"]["name"], "primary");
    let before = fleet.hosts_bytes();
    refused(
        dashboard.send(
            "PATCH",
            "/api/hosts/primary",
            Some(json!({ "name": "BUILD-BOX" })),
        ),
        409,
        "host_name_conflict",
    );
    refused(
        dashboard.send(
            "PATCH",
            "/api/hosts/local-box",
            Some(json!({ "name": "other" })),
        ),
        409,
        "host_is_local",
    );
    assert_eq!(fleet.hosts_bytes(), before);

    // Remove: refused while a replica checkout routes to the host, with its
    // dependents; forced, it reports what lost its route.
    let replica = fleet.temp.path().join("replica-of-primary");
    git_repo::init(&replica);
    fleet.orbit_ok(
        &replica,
        &[
            "workspace",
            "init",
            "--name",
            "primary-replica",
            "--role",
            "replica",
            "--owner",
            &alpha.machine_id,
        ],
    );
    let shown = ok(dashboard.get("/api/hosts/primary"));
    assert_eq!(
        shown["dependents"]["replica_checkouts"][0]["workspace_name"], "primary-replica",
        "{shown}"
    );
    let in_use = refused(
        dashboard.send("DELETE", "/api/hosts/primary", None),
        409,
        "host_in_use",
    );
    assert_eq!(
        in_use["dependents"]["replica_checkouts"][0]["workspace_name"], "primary-replica",
        "the refusal lists its dependents: {in_use}"
    );
    let (cli_code, cli_message) = fleet.refused(&["host", "remove", "primary"]);
    assert_eq!(cli_code, "host_in_use");
    assert_eq!(
        in_use["error"],
        cli_message.as_str(),
        "the CLI's own message"
    );
    assert_eq!(fleet.hosts_bytes(), before);
    let removed = ok(dashboard.send("DELETE", "/api/hosts/primary?force=true", None));
    assert_eq!(removed["action"], "removed", "{removed}");
    assert_eq!(
        removed["orphaned"]["replica_checkouts"][0]["workspace_name"],
        "primary-replica"
    );
    refused(
        dashboard.send("DELETE", "/api/hosts/local-box", None),
        409,
        "host_is_local",
    );
    let names: Vec<_> = ok(dashboard.get("/api/hosts?probe=false"))["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .map(|row| row["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("local-box"), json!("build-box")]);
}

#[test]
fn dashboard_host_list_follows_cli_edits_and_keeps_the_last_valid_host_file() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    fleet.route("alpha", &alpha, "plain");
    let dashboard = Dashboard::start(&fleet, true);

    let first = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(first["hosts"].as_array().map(Vec::len), Some(1), "{first}");

    // A CLI edit shows on the next refresh, without a restart.
    fleet.json(&["host", "add", "alpha", "--json"]);
    let added = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(
        host_row(&added, "alpha")["machine_id"],
        alpha.machine_id.as_str()
    );
    let generation = added["generation"].as_u64().expect("generation");
    assert!(generation > first["generation"].as_u64().expect("generation"));
    assert!(added["load_error"].is_null());
    let unchanged = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(
        unchanged["generation"], generation,
        "an unchanged file is not reloaded"
    );

    // A file that fails to load is reported beside the last valid snapshot.
    let valid = fleet.hosts_bytes().expect("hosts.toml");
    fs::write(
        fleet.hosts_file(),
        [valid.as_slice(), b"surprise = true\n"].concat(),
    )
    .expect("break hosts.toml");
    let broken = ok(dashboard.get("/api/hosts?probe=false"));
    assert_eq!(broken["load_error"]["code"], "invalid_input", "{broken}");
    assert!(
        broken["load_error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("hosts.toml")),
        "{broken}"
    );
    assert_eq!(broken["generation"], generation, "{broken}");
    assert_eq!(
        host_row(&broken, "alpha")["machine_id"],
        alpha.machine_id.as_str(),
        "the last valid snapshot stays in use"
    );
    let (code, _) = fleet.refused(&["host", "list"]);
    assert_eq!(
        broken["load_error"]["code"],
        code.as_str(),
        "the CLI's code"
    );

    fs::write(fleet.hosts_file(), &valid).expect("repair hosts.toml");
    let repaired = ok(dashboard.get("/api/hosts?probe=false"));
    assert!(repaired["load_error"].is_null(), "{repaired}");
    assert!(repaired["generation"].as_u64().expect("generation") > generation);
}
