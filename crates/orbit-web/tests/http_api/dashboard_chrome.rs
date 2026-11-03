//! Poll-budget repair: combining reads preserves the public component contracts.

use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

#[test]
fn combined_poll_chrome_retains_payloads_and_independent_refusals() {
    isolated(
        "dashboard_chrome::combined_poll_chrome_retains_payloads_and_independent_refusals",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);
            let combined = json_ok(server.get("/api/dashboard/chrome?routines=true&failures=true"));
            for (component, path) in [
                ("notices", "/api/notices"),
                ("summary", "/api/audit/summary?since=24h"),
                ("routines", "/api/routines"),
                ("failures", "/api/jobs/failure-streaks"),
            ] {
                let direct = server.get(path);
                assert_eq!(combined[component]["status"], direct.status().as_u16());
                let mut direct: Value = direct.json().unwrap();
                let mut bundled: Value =
                    serde_json::from_str(combined[component]["body"].as_str().unwrap()).unwrap();
                // The routine report dates each response; all returned facts,
                // ordering and capability/refusal fields must stay identical.
                if component == "routines" {
                    direct.as_object_mut().unwrap().remove("generated_at");
                    bundled.as_object_mut().unwrap().remove("generated_at");
                }
                assert_eq!(bundled, direct, "{component} lost its direct-read contract");
            }
            let light = json_ok(server.get("/api/dashboard/chrome"));
            assert_eq!(light["routines"], Value::Null);
            assert_eq!(light["failures"], Value::Null);

            let refused = json_ok(
                server.get("/api/dashboard/chrome?workspace=missing&routines=true&failures=true"),
            );
            // A workspace refusal must not discard the host-wide routine read.
            assert_eq!(refused["routines"]["status"], json!(200));
            for (component, path) in [
                ("notices", "/api/notices?workspace=missing"),
                ("failures", "/api/jobs/failure-streaks?workspace=missing"),
            ] {
                let direct = server.get(path);
                assert_eq!(refused[component]["status"], direct.status().as_u16());
                assert_eq!(
                    serde_json::from_str::<Value>(refused[component]["body"].as_str().unwrap())
                        .unwrap(),
                    direct.json::<Value>().unwrap(),
                    "{component} changed its refusal",
                );
            }
        },
    );
}
