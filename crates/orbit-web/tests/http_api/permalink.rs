use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};

use super::support::{Fixture, Server, isolated};

const LOCAL_ID_PREFIX: &str = "HF";

fn write_host(fixture: &Fixture, name: &str, machine_id: &str, prefix: &str) {
    let text = format!(
        "schema_version = 1\n\n[[hosts]]\nname = \"{name}\"\nmachine_id = \"{machine_id}\"\nssh = \"{name}.example\"\ntask_prefix = \"{prefix}\"\n"
    );
    orbit_common::fs::io::atomic_write_text(&fixture.global.join("hosts.toml"), &text).unwrap();
}

/// A client that reports the redirect instead of following it.
fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

fn get(server: &Server, path: &str) -> Response {
    client()
        .get(format!("{}{path}", server.origin))
        .send()
        .unwrap()
}

fn location(response: &Response) -> &str {
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    response.headers()["location"].to_str().unwrap()
}

fn not_found_body(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().get("location").is_none());
    response.text().unwrap()
}

#[test]
fn task_permalinks_name_the_host_their_prefix_selects() {
    isolated(
        "permalink::task_permalinks_name_the_host_their_prefix_selects",
        || {
            let fixture = Fixture::new();
            let remote = Fixture::host("hostb", "HB");
            write_host(&fixture, "hostb", &remote.machine_id(), "HB");
            let server = fixture.server(false);

            // A local task names the serving machine with an empty host, so a
            // browser that last chose a remote host does not restore it.
            let local = get(&server, &format!("/t/{LOCAL_ID_PREFIX}-00042"));
            assert_eq!(location(&local), "/?host=&workspace=all#tasks/HF-00042");

            let registered = get(&server, "/t/HB-00007");
            assert_eq!(
                location(&registered),
                "/?host=hostb&workspace=all#tasks/HB-00007"
            );

            let unknown = not_found_body(get(&server, "/t/ZZ-00001"));
            assert!(unknown.contains("Task ZZ-00001 not found"), "{unknown}");
            assert!(unknown.contains("prefix ZZ"), "{unknown}");

            let malformed = not_found_body(get(&server, "/t/nonsense"));
            assert!(
                malformed.contains("nonsense is not a task id"),
                "{malformed}"
            );
        },
    );
}

#[test]
fn run_permalinks_pin_the_machine_and_never_fall_back_to_local() {
    isolated(
        "permalink::run_permalinks_pin_the_machine_and_never_fall_back_to_local",
        || {
            let fixture = Fixture::new();
            let remote = Fixture::host("hostb", "HB");
            write_host(&fixture, "hostb", &remote.machine_id(), "HB");
            let server = fixture.server(false);
            let id = "jrun-20261126-0531-c3";

            // The bare form is a lookup on the serving machine only.
            let bare = get(&server, &format!("/r/{id}"));
            assert_eq!(location(&bare), format!("/?host=&workspace=all#runs/{id}"));

            // The exact form selects the registered host and its workspace.
            let remote_run = get(
                &server,
                &format!(
                    "/r/{id}?machine={}&workspace=ws_remote",
                    remote.machine_id()
                ),
            );
            assert_eq!(
                location(&remote_run),
                format!("/?host=hostb&workspace=ws_remote#runs/{id}")
            );

            // The serving machine's own id selects it, whatever its name.
            let local_run = get(
                &server,
                &format!(
                    "/r/{id}?machine={}&workspace=a%20b%26c",
                    fixture.machine_id()
                ),
            );
            assert_eq!(
                location(&local_run),
                format!("/?host=&workspace=a+b%26c#runs/{id}")
            );

            // An unregistered machine is named, and no run opens.
            let unknown = not_found_body(get(
                &server,
                &format!("/r/{id}?machine=hm_unregistered&workspace=ws_remote"),
            ));
            assert!(
                unknown.contains(&format!("Run {id} not found")),
                "{unknown}"
            );
            assert!(unknown.contains("hm_unregistered"), "{unknown}");

            let malformed = not_found_body(get(&server, "/r/..%2Fx"));
            assert!(malformed.contains("is not a run id"), "{malformed}");
        },
    );
}

#[test]
fn permalinks_keep_the_dashboard_host_gate() {
    isolated("permalink::permalinks_keep_the_dashboard_host_gate", || {
        let fixture = Fixture::new();
        let server = fixture.server(false);
        for path in ["/t/HF-00001", "/r/jrun-20261126-0531-c3"] {
            let response = client()
                .get(format!("{}{path}", server.origin))
                .header("host", "dashboard.attacker.example")
                .send()
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            let response = client()
                .get(format!("{}{path}", server.origin))
                .header("origin", "http://dashboard.attacker.example")
                .send()
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
    });
}
