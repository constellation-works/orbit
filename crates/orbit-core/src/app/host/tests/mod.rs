use super::{REFUSE_START, in_parallel};

#[test]
fn probe_workers_refused_still_return_ordered_results() {
    let items: Vec<_> = (0..19).collect();
    let caller = std::thread::current().id();
    for refusal in [Some(2), Some(usize::MAX)] {
        REFUSE_START.with(|value| value.set(refusal));
        let results = in_parallel(&items, |item| {
            (*item, std::thread::current().id() == caller)
        });
        REFUSE_START.with(|value| value.set(None));
        for (index, (item, on_caller)) in results.into_iter().enumerate() {
            assert_eq!(item, index);
            assert_eq!(
                on_caller,
                refusal == Some(index) || refusal == Some(usize::MAX)
            );
        }
    }
}
