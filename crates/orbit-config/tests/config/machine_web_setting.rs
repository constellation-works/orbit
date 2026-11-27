use orbit_config::load_machine_web_settings;
use std::fs;
use tempfile::tempdir;

#[test]
fn public_url_is_optional_and_read_from_global_machine_web_settings() {
    let global = tempdir().expect("global config directory");

    let unset = load_machine_web_settings(global.path()).expect("unset machine web settings");
    assert_eq!(unset.public_url, None);

    fs::write(
        global.path().join("config.toml"),
        "[machine.web]\npublic_url = 'https://node.tailnet.ts.net'\n",
    )
    .expect("write global config");
    let configured =
        load_machine_web_settings(global.path()).expect("configured machine web settings");
    assert_eq!(
        configured.public_url.as_deref(),
        Some("https://node.tailnet.ts.net")
    );
}

#[test]
fn public_url_accepts_http_and_https_origins_and_normalizes_them() {
    let global = tempdir().expect("global config directory");
    let config_path = global.path().join("config.toml");

    for (configured, expected) in [
        ("http://localhost:7878/", "http://localhost:7878"),
        (
            "HTTPS://Node.Tailnet.TS.NET:443/",
            "https://node.tailnet.ts.net",
        ),
    ] {
        fs::write(
            &config_path,
            format!("[machine.web]\npublic_url = '{configured}'\n"),
        )
        .expect("write global config");
        let settings = load_machine_web_settings(global.path()).expect("valid dashboard origin");
        assert_eq!(settings.public_url.as_deref(), Some(expected));
    }
}

#[test]
fn public_url_rejects_a_non_http_scheme_with_actionable_diagnostic() {
    let global = tempdir().expect("global config directory");
    fs::write(
        global.path().join("config.toml"),
        "[machine.web]\npublic_url = 'ftp://node.tailnet.ts.net'\n",
    )
    .expect("write global config");

    let error = load_machine_web_settings(global.path())
        .expect_err("dashboard origin must reject non-http schemes")
        .to_string();
    assert!(error.contains("machine.web.public_url"), "{error}");
    assert!(error.contains("http or https origin"), "{error}");
    assert!(
        error.contains("without a path, query, or fragment"),
        "{error}"
    );
}
