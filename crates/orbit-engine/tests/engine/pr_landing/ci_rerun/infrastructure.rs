//! A failed job carrying a declared infrastructure signature gets the CI rerun,
//! and a workspace that declares none keeps its repair route.
use super::*;

const SIGNATURE: &str = "ci-download: outcome=failed class=transient";
const DOWNLOAD_504: &str = "\
test\tTests\tcurl: (22) The requested URL returned error: 504
test\tTests\tci-download: attempt=9 curl_exit=22 http='504' seconds=0.400
test\tTests\tci-download: failure=total budget cannot accommodate backoff
test\tTests\tci-download: outcome=failed class=transient attempts=9 total_seconds=57.100 budget_seconds=60.0
test\tTests\t##[error]Process completed with exit code 1.
";

fn failing_download(row: &Path) -> (Fixture, DeliveryHost) {
    let (fx, host, _) = fixture(row);
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["conclusions"] = json!({"lint": "success"});
    config["repeat"] = json!(true);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    for job in [1000, 1010] {
        fs::write(fx.forge.join(format!("retry-log-{job}")), DOWNLOAD_504).unwrap();
    }
    (fx, host)
}

fn complete_auto_fix(fx: &Fixture, host: &DeliveryHost) -> Res<Value> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(30);
    input["poll_interval_seconds"] = json!(5);
    complete_with_logical_clock(host, &input)
}

#[test]
fn declared_signature_reruns_without_auto_fix_and_undeclared_repairs() {
    isolated(
        "ci_rerun::infrastructure::declared_signature_reruns_without_auto_fix_and_undeclared_repairs",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let (fx, host) = failing_download(row);
                locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.to_string()];
                let error = complete_auto_fix(&fx, &host)
                    .expect_err("a job that keeps failing does not merge");
                assert_eq!(
                    fx.forge_lines("retry-sends"),
                    vec!["run rerun 700 --failed".to_string()]
                );
                assert!(
                    pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty(),
                    "no repair attempt: {:?}",
                    host.history(TASK_ID)
                );
                let message = error.to_string();
                assert!(
                    !orbit_types::workflow::is_pr_auto_fix_check_repair(&message),
                    "no repair hand-off: {message}"
                );
                assert!(message.contains(SIGNATURE), "{message}");
            });
            in_fresh_row(sandbox, |row| {
                let (fx, host) = failing_download(row);
                let error = complete_auto_fix(&fx, &host).expect_err("the repair hand-off");
                assert!(
                    orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                    "{error}"
                );
                assert_eq!(fx.forge_lines("retry-sends"), Vec::<String>::new());
            });
        },
    );
}

#[test]
fn a_quoted_or_scripted_signature_is_not_infrastructure() {
    isolated(
        "ci_rerun::infrastructure::a_quoted_or_scripted_signature_is_not_infrastructure",
        |sandbox| {
            for log in [
                "test\tTests\t##[group]Run grep 'ci-download: outcome=failed class=transient' log\ntest\tTests\tci-download: outcome=failed class=transient\ntest\tTests\t##[endgroup]\ntest\tTests\terror: source failure at src/feature.txt:1:1\n",
                "test\tTests\tassertion failed: stderr was \"ci-download: outcome=failed class=transient\"\ntest\tTests\terror: source failure at src/feature.txt:1:1\n",
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = failing_download(row);
                    locked(&host.pr_config).ci_infrastructure_signatures =
                        vec![SIGNATURE.to_string()];
                    fs::write(fx.forge.join("retry-log-1000"), log).unwrap();
                    let error = complete_auto_fix(&fx, &host).expect_err("the repair hand-off");
                    assert!(
                        orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                        "{log}: {error}"
                    );
                    assert_eq!(fx.forge_lines("retry-sends"), Vec::<String>::new(), "{log}");
                });
            }
        },
    );
}
