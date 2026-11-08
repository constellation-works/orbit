//! Native TLS at the listener settings boundary. All credentials are
//! generated for the fixture; no certificate or private key is stored.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use orbit_web::lease_listener::{
    LeaseListenerConfig, LeaseListenerSettings, LeaseListenerTlsSettings,
};

fn generate_pair(home: &Path) {
    let pair = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(home.join("cert.pem"), pair.cert.pem()).unwrap();
    std::fs::write(home.join("key.pem"), pair.signing_key.serialize_pem()).unwrap();
    std::fs::set_permissions(home.join("key.pem"), std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn settings(
    home: &Path,
    bind: &str,
    tls: Option<LeaseListenerTlsSettings>,
) -> LeaseListenerSettings {
    LeaseListenerSettings {
        bind: Some(bind.to_string()),
        tls,
        intake_socket: Some(home.join("intake.sock").to_str().unwrap().to_string()),
        ..LeaseListenerSettings::default()
    }
}

fn tls_files(cert: PathBuf, key: PathBuf) -> LeaseListenerTlsSettings {
    LeaseListenerTlsSettings {
        cert: cert.to_str().unwrap().to_string(),
        key: key.to_str().unwrap().to_string(),
    }
}

fn tls_setting(home: &Path) -> LeaseListenerTlsSettings {
    tls_files(home.join("cert.pem"), home.join("key.pem"))
}

#[test]
fn valid_tls_allows_an_unspecified_bind() {
    let tmp = tempfile::tempdir().unwrap();
    generate_pair(tmp.path());
    let settings = settings(tmp.path(), "0.0.0.0:0", Some(tls_setting(tmp.path())));
    LeaseListenerConfig::from_settings(&settings).expect("valid native TLS must start");
}

#[test]
fn unspecified_bind_still_requires_tls_or_a_trusted_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    for bind in ["0.0.0.0:0", "[::]:0"] {
        let error = LeaseListenerConfig::from_settings(&settings(tmp.path(), bind, None))
            .unwrap_err()
            .to_string();
        assert!(error.contains("listens on every interface"), "{error}");
        let proxied = LeaseListenerSettings {
            behind_proxy: true,
            trusted_proxy: vec!["127.0.0.1".to_string()],
            ..settings(tmp.path(), bind, None)
        };
        LeaseListenerConfig::from_settings(&proxied).unwrap();
    }
}

#[test]
fn unusable_tls_material_and_permissive_keys_refuse_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let missing = home.join("missing.pem");
    for (cert, key) in [(true, false), (false, true)] {
        generate_pair(home);
        let material = tls_files(
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
        assert_tls_refused(home, material);
    }
    for file in ["cert.pem", "key.pem"] {
        generate_pair(home);
        std::fs::set_permissions(home.join(file), std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_tls_refused(home, tls_setting(home));
        std::fs::set_permissions(home.join(file), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(home.join(file), "not PEM material").unwrap();
        assert_tls_refused(home, tls_setting(home));
    }
    generate_pair(home);
    let other = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(home.join("key.pem"), other.signing_key.serialize_pem()).unwrap();
    assert_tls_refused(home, tls_setting(home));

    for mode in [0o640, 0o604, 0o660, 0o606] {
        generate_pair(home);
        std::fs::set_permissions(home.join("key.pem"), std::fs::Permissions::from_mode(mode))
            .unwrap();
        assert_tls_refused(home, tls_setting(home));
    }
}

fn assert_tls_refused(home: &Path, tls: LeaseListenerTlsSettings) {
    let error = LeaseListenerConfig::from_settings(&settings(home, "0.0.0.0:0", Some(tls)))
        .expect_err("unusable TLS must refuse startup")
        .to_string();
    assert!(
        error.contains("refusing to start the lease listener: tls:"),
        "{error}"
    );
}
