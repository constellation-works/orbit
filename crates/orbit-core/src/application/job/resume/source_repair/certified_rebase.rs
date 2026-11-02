//! Certified base chases that carried a retained branch past its publication.
//!
//! After the source published head P, its own completion may rebase the
//! retained branch onto an advanced base and certify each clean chase in the
//! host-only recovery authority (`certify_chase`). When the base then keeps
//! moving, the chase stops with `base_chase_exhausted` and writes nothing, so
//! the branch is left on the last certified tip T, which was never pushed.
//! Resume adopts T only when those certificates chain P to T without a gap;
//! the remote branch and pull request stay bound to P, and the ordinary push
//! step publishes T under a lease on P.
use super::*;
use crate::runtime::recovery_authority::RecoveryAuthority;

/// The unpublished tip the source run's certified chases carried the
/// published head to, and the base the last chase landed it on.
pub(super) struct CertifiedRebaseTip {
    pub(super) head: String,
    pub(super) target_base: String,
}

/// One certified chase: where it started, where the remote stood, and the
/// tip and base it produced.
struct ChaseLink {
    head_before: String,
    remote_before: Option<String>,
    head: String,
    target_base: String,
}

fn exact_sha(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
}

/// The fields of a payload exactly as `certify_chase` writes it for
/// `source_id`'s step `step_id`, or `None` for any other shape or run.
fn chase_link(source_id: &str, step_id: &str, checkpoint: &Value) -> Option<ChaseLink> {
    let target_base = exact_sha(&checkpoint["target_base_sha"])?;
    let head = exact_sha(&checkpoint["head_sha"])?;
    let shaped = checkpoint["run_id"] == source_id
        && checkpoint["step_id"] == step_id
        && checkpoint["base_sha"] == checkpoint["target_base_sha"]
        && exact_sha(&checkpoint["chased_from"]).is_some()
        && checkpoint["rewritten"] == true
        && checkpoint["companion_paths"] == json!([])
        && checkpoint["recovery_attempt"].as_u64().is_some();
    shaped.then(|| ChaseLink {
        head_before: exact_sha(&checkpoint["head_sha_before"]).unwrap_or_default(),
        remote_before: exact_sha(&checkpoint["remote_sha_before"]),
        head,
        target_base,
    })
}

impl OrbitRuntime {
    /// Follow `source_id`'s own host-certified chase checkpoints from the
    /// published head. The first link starts at `published` (its pre-rewrite
    /// head, or the remote head it observed); each later link starts at the
    /// tip before it. A checkpoint the recovery authority does not vouch for
    /// as current, another run's checkpoint, and an ambiguous fork end the
    /// chain. `None` when no certified chase moved the head.
    pub(super) fn certified_rebase_tip(
        &self,
        source_id: &str,
        state: &PipelineState,
        published: &str,
    ) -> Result<Option<CertifiedRebaseTip>, OrbitError> {
        let shaped = state
            .rebase_recovery_checkpoints
            .iter()
            .filter_map(|(step_id, checkpoint)| {
                chase_link(source_id, step_id, checkpoint).map(|link| (step_id, checkpoint, link))
            })
            .collect::<Vec<_>>();
        if shaped.is_empty() {
            return Ok(None);
        }
        let authority = RecoveryAuthority::open(&self.global_root())?;
        let mut links = Vec::new();
        for (step_id, checkpoint, link) in shaped {
            if authority.verify(source_id, step_id, checkpoint)? {
                links.push(link);
            }
        }
        let mut head = published.to_string();
        let mut target_base = None;
        let mut first = true;
        while !links.is_empty() {
            let next = links
                .iter()
                .enumerate()
                .filter(|(_, link)| {
                    link.head_before == head
                        || (first && link.remote_before.as_deref() == Some(head.as_str()))
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let [index] = next.as_slice() else {
                // No continuation ends the chain; two make it ambiguous, and
                // an ambiguous chain certifies nothing.
                if next.len() > 1 {
                    return Ok(None);
                }
                break;
            };
            let link = links.swap_remove(*index);
            head = link.head;
            target_base = Some(link.target_base);
            first = false;
        }
        Ok(target_base
            .filter(|_| head != published)
            .map(|target_base| CertifiedRebaseTip { head, target_base }))
    }
}
