use std::fs;
use std::path::Path;

use super::super::tmp::{TmpGcResult, filesystem};

#[test]
fn preview_skips_entries_removed_after_the_listing() {
    let checkout = tempfile::tempdir().expect("create checkout fixture");
    let tmp = checkout.path().join(".orbit/tmp");
    fs::create_dir_all(tmp.join("nested")).expect("create scratch tree");
    fs::write(tmp.join("vanished"), b"gone").expect("create top-level scratch entry");
    fs::write(tmp.join("remaining"), b"kept").expect("create remaining scratch entry");
    fs::write(tmp.join("nested/vanished-child"), b"gone").expect("create nested scratch entry");
    fs::write(tmp.join("nested/remaining-child"), b"kept").expect("create remaining nested entry");

    let mut result = TmpGcResult {
        path: tmp.clone(),
        dry_run: true,
        entries_removed: 0,
        bytes_reclaimable: 0,
        bytes_reclaimed: 0,
        reports: Vec::new(),
    };
    let mut listing = 0;
    filesystem::collect_with_after_listing(
        checkout.path(),
        &mut result,
        || Ok(()),
        || {
            listing += 1;
            match listing {
                1 => remove(&tmp.join("vanished")),
                2 => remove(&tmp.join("nested/vanished-child")),
                _ => {}
            }
        },
    )
    .expect("preview must survive entries removed after listing (ORB-14751)");

    assert_eq!(
        result.bytes_reclaimable, 8,
        "ORB-14751: count only entries still present during preview"
    );
    assert_eq!(
        result
            .reports
            .iter()
            .map(|report| (
                report
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                report.bytes_reclaimable
            ))
            .collect::<Vec<_>>(),
        [("nested".to_string(), 4), ("remaining".to_string(), 4)],
        "ORB-14751: omit vanished entries and retain reports for surviving siblings",
    );
}

fn remove(path: &Path) {
    fs::remove_file(path).expect("remove listed scratch entry to force the listing-to-stat race");
}
