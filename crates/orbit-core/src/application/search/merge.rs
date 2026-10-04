use std::collections::VecDeque;

use super::GlobalSearchHit;

pub(in crate::application) fn merge_round_robin(
    branches: Vec<Vec<GlobalSearchHit>>,
    limit: usize,
) -> Vec<GlobalSearchHit> {
    let mut queues = branches
        .into_iter()
        .filter(|branch| !branch.is_empty())
        .map(|branch| branch.into_iter().collect::<VecDeque<_>>())
        .collect::<Vec<_>>();
    let mut out = Vec::with_capacity(limit);

    while out.len() < limit && !queues.is_empty() {
        let mut index = 0;
        while index < queues.len() && out.len() < limit {
            if let Some(hit) = queues[index].pop_front() {
                out.push(hit);
            }
            if queues[index].is_empty() {
                queues.remove(index);
            } else {
                index += 1;
            }
        }
    }

    out
}
