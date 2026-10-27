//! Native TLS at the public settings and listener boundaries. All credentials
//! are generated for the fixture; no certificate or private key is stored.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use orbit_web::lease_listener::LeaseListenerConfig;

fn generate_pair(home: &Path) {
    let pair = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(home.join("cert.pem"), pair.cert.pem()).unwrap();
    std::fs::write(home.join("key.pem"), pair.signing_key.serialize_pem()).unwrap();
    std::fs::set_permissions(home.join("key.pem"), std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn write_settings(home: &Path, bind: &str, tls: &str) {
    let config = format!(
        "[lease.listener]\nbind = {bind:?}\nintake_socket = {socket:?}\n{tls}\n",
        socket = home.join("intake.sock").to_str().unwrap(),
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
}

fn tls_setting(home: &Path) -> String {
    format!(
        "tls = {{ cert = {cert:?}, key = {key:?} }}",
        cert = home.join("cert.pem").to_str().unwrap(),
        key = home.join("key.pem").to_str().unwrap(),
    )
}

#[test]
fn valid_tls_allows_an_unspecified_bind() {
    let tmp = tempfile::tempdir().unwrap();
    generate_pair(tmp.path());
    write_settings(tmp.path(), "0.0.0.0:0", &tls_setting(tmp.path()));
    let settings = orbit_config::load_lease_listener_settings(tmp.path()).unwrap();
    LeaseListenerConfig::from_settings(&settings).expect("valid native TLS must start");
    // The general registry and listener-only reader must agree on the table.
    let roots = orbit_config::ConfigRoots::new(tmp.path(), tmp.path().join("workspace"));
    let resolved = orbit_config::ResolvedConfig::load(&roots).unwrap();
    assert_eq!(
        resolved.snapshot.value_for("lease.listener.tls").unwrap(),
        serde_json::json!({"cert": settings.tls.as_ref().unwrap().cert,
                           "key": settings.tls.as_ref().unwrap().key}),
    );
}

#[test]
fn unspecified_bind_still_requires_tls_or_a_trusted_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    for bind in ["0.0.0.0:0", "[::]:0"] {
        write_settings(tmp.path(), bind, "");
        let settings = orbit_config::load_lease_listener_settings(tmp.path()).unwrap();
        let error = LeaseListenerConfig::from_settings(&settings)
            .unwrap_err()
            .to_string();
        assert!(error.contains("lease.listener.bind"), "{error}");
        write_settings(
            tmp.path(),
            bind,
            "behind_proxy = true\ntrusted_proxy = ['127.0.0.1']",
        );
        let settings = orbit_config::load_lease_listener_settings(tmp.path()).unwrap();
        LeaseListenerConfig::from_settings(&settings).unwrap();
    }
}

#[test]
fn unusable_tls_material_and_permissive_keys_refuse_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let missing = home.join("missing.pem");
    for (cert, key) in [(true, false), (false, true)] {
        generate_pair(home);
        let material = format!(
            "tls = {{ cert = {:?}, key = {:?} }}",
            if cert {
                missing.clone()
            } else {
                home.join("cert.pem")
            },
            if key {
                missing.clone()
            } else {
                home.join("key.pem")
            },
        );
        assert_tls_refused(home, &material);
    }
    for file in ["cert.pem", "key.pem"] {
        generate_pair(home);
        std::fs::set_permissions(home.join(file), std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_tls_refused(home, &tls_setting(home));
        std::fs::set_permissions(home.join(file), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(home.join(file), "not PEM material").unwrap();
        assert_tls_refused(home, &tls_setting(home));
    }
    generate_pair(home);
    let other = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(home.join("key.pem"), other.signing_key.serialize_pem()).unwrap();
    assert_tls_refused(home, &tls_setting(home));

    for mode in [0o640, 0o604, 0o660, 0o606] {
        generate_pair(home);
        std::fs::set_permissions(home.join("key.pem"), std::fs::Permissions::from_mode(mode))
            .unwrap();
        assert_tls_refused(home, &tls_setting(home));
    }
    for malformed in [
        "tls = true",
        "tls = 'placeholder'",
        "tls = {}",
        "tls = { cert = 'cert.pem' }",
        "tls = { cert = '', key = 'key.pem' }",
        "tls = { cert = 'cert.pem', key = 'key.pem', typo = true }",
    ] {
        assert_tls_refused(home, malformed);
    }
}

fn assert_tls_refused(home: &Path, tls: &str) {
    write_settings(home, "0.0.0.0:0", tls);
    let error = orbit_config::load_lease_listener_settings(home)
        .and_then(|settings| LeaseListenerConfig::from_settings(&settings))
        .expect_err("unusable TLS must refuse startup")
        .to_string();
    assert!(error.contains("lease.listener.tls"), "{error}");
}
