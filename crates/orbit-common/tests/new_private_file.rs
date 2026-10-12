//! `write_new_private_text` publishes a whole file or nothing, and never
//! replaces one that is already there.
#![allow(missing_docs, clippy::unwrap_used)]

use std::io::ErrorKind;
use std::path::Path;
use std::sync::{Arc, Barrier};

use orbit_common::fs::io::write_new_private_text;
use tempfile::TempDir;

fn entries(dir: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn publishes_a_private_file_once_and_never_replaces_an_existing_one() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state").join("marker");

    write_new_private_text(&path, "first\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the published file is owner-only");
    }

    let error = write_new_private_text(&path, "second\n").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");

    // An existing empty file is not overwritten either: deciding what an
    // incomplete file means is the caller's.
    let empty = temp.path().join("state").join("empty");
    std::fs::File::create(&empty).unwrap();
    let error = write_new_private_text(&empty, "filled\n").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&empty).unwrap(), "");

    assert_eq!(
        entries(&temp.path().join("state")),
        ["empty", "marker"],
        "no staging sibling outlives a call, published or refused"
    );
}

#[test]
fn a_creator_that_loses_the_race_reads_the_winners_complete_file() {
    // Whatever order the creators run in, a loser's `AlreadyExists` means the
    // whole winning file is already there. A create-then-write publication
    // let a loser of the typed decisions boundary read it empty and take the
    // wrong cutoff.
    const CREATORS: usize = 4;
    let temp = TempDir::new().unwrap();
    let path = Arc::new(temp.path().join("marker"));
    let start = Arc::new(Barrier::new(CREATORS));
    let content = |index: usize| format!("creator {index}: {}\n", "x".repeat(64 * 1024));
    let outcomes = (0..CREATORS)
        .map(|index| {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                match write_new_private_text(&path, &content(index)) {
                    Ok(()) => (index, true, None),
                    Err(error) => {
                        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
                        (index, false, Some(std::fs::read_to_string(&*path).unwrap()))
                    }
                }
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|creator| creator.join().unwrap())
        .collect::<Vec<_>>();

    let winners = outcomes
        .iter()
        .filter(|(_, won, _)| *won)
        .map(|(index, _, _)| *index)
        .collect::<Vec<_>>();
    assert_eq!(winners.len(), 1, "exactly one creator publishes");
    let published = content(winners[0]);
    assert_eq!(std::fs::read_to_string(&*path).unwrap(), published);
    for (index, _, observed) in outcomes.iter().filter(|(_, won, _)| !won) {
        assert_eq!(
            observed.as_deref(),
            Some(published.as_str()),
            "creator {index} lost the race and must read the complete winning file"
        );
    }
    assert_eq!(entries(temp.path()), ["marker"]);
}
