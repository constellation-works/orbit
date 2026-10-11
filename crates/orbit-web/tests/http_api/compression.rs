use std::io::Read;

use flate2::read::GzDecoder;
use orbit_core::JobRunState;
use serde_json::Value;

use super::support::{Fixture, isolated};

#[test]
#[allow(clippy::print_stdout)] // Transfer-size evidence in the fixture log.
fn json_gzip_negotiation_preserves_the_body_and_reduces_transfer_size() {
    isolated(
        "compression::json_gzip_negotiation_preserves_the_body_and_reduces_transfer_size",
        || {
            let fixture = Fixture::new();
            for index in 0..25 {
                fixture.seed_run(
                    &format!("jrun-compression-{index}"),
                    "task_pipeline",
                    JobRunState::Success,
                );
            }
            let server = fixture.server(false);
            let path = "/api/job-runs?limit=25&workspace=ws_http_fixture";
            let identity = server.get(path);
            assert_eq!(identity.status(), 200);
            assert!(identity.headers().get("content-encoding").is_none());
            assert_eq!(identity.headers()["vary"], "accept-encoding");
            let identity = identity.bytes().unwrap();
            let expected: Value = serde_json::from_slice(&identity).unwrap();
            assert_eq!(expected["items"].as_array().unwrap().len(), 25);

            let response = server
                .request("GET", path)
                .header("accept-encoding", "gzip")
                .send()
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-encoding"], "gzip");
            assert_eq!(response.headers()["vary"], "accept-encoding");
            assert!(response.headers().get("content-length").is_none());
            let compressed = response.bytes().unwrap();
            let mut decoded = Vec::new();
            GzDecoder::new(compressed.as_ref())
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&decoded).unwrap(), expected);
            assert!(compressed.len() * 4 < identity.len());
            println!(
                "JSON transfer: identity={} bytes, gzip={} bytes, ratio={:.2}",
                identity.len(),
                compressed.len(),
                identity.len() as f64 / compressed.len() as f64,
            );

            for encoding in ["gzip;q=0, identity", "br", "identity"] {
                let response = server
                    .request("GET", path)
                    .header("accept-encoding", encoding)
                    .send()
                    .unwrap();
                assert!(response.headers().get("content-encoding").is_none());
                assert_eq!(response.bytes().unwrap(), identity, "{encoding}");
            }
        },
    );
}
