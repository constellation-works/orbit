/// The artifact digest preimage is the same whatever order the outputs were
/// declared or copied in, so two hosts building one commit reproducibly
/// record one value (§3.6).
#[test]
fn artifact_digest_preimage_is_order_independent() {
    use crate::plugin::{PluginBuildOutputRecord, artifact_digest_preimage};
    let a = PluginBuildOutputRecord {
        to: "bin/a".into(),
        mode: 0o755,
        sha256: "aa".into(),
    };
    let b = PluginBuildOutputRecord {
        to: "lib/b".into(),
        mode: 0o644,
        sha256: "bb".into(),
    };
    let forward = artifact_digest_preimage(&[a.clone(), b.clone()]);
    assert_eq!(forward, artifact_digest_preimage(&[b, a]));
    assert_eq!(
        forward,
        "orbit.plugin.build.v1\nbin/a\u{0}755\u{0}aa\nlib/b\u{0}644\u{0}bb\n"
    );
}
