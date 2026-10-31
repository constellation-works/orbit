use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::bounded_line_queue;

const TICK: Duration = Duration::from_secs(10);

#[test]
fn a_reader_stays_blocked_until_the_consumer_drains_bytes() {
    let (sender, receiver) = bounded_line_queue(10);
    let sent = Arc::new(AtomicUsize::new(0));
    let progress = Arc::clone(&sent);
    let reader = std::thread::spawn(move || {
        for _ in 0..4 {
            sender.send("xxxxxx\n".to_string()).expect("consumer alive");
            progress.fetch_add(1, Ordering::SeqCst);
        }
    });
    // Each line is 7 bytes, so only one fits under the 10-byte budget at a
    // time: the reader cannot finish until the consumer has taken lines.
    let mut received = 0;
    while received < 4 {
        let line = receiver.recv_timeout(TICK).expect("a line arrives");
        assert_eq!(line, "xxxxxx\n");
        received += 1;
        assert!(
            sent.load(Ordering::SeqCst) <= received + 1,
            "the reader ran ahead of the byte budget"
        );
    }
    reader.join().expect("reader exits");
}

#[test]
fn dropping_the_consumer_releases_a_blocked_reader() {
    let (sender, receiver) = bounded_line_queue(10);
    sender
        .send("xxxxxx\n".to_string())
        .expect("first line queues");
    let reader = std::thread::spawn(move || sender.send("yyyyyy\n".to_string()));
    drop(receiver);
    assert!(reader.join().expect("reader exits").is_err());
}
