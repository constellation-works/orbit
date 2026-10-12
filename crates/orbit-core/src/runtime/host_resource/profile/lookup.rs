//! Which profile describes a delivery: its own key, or the fallback chain.

use std::sync::Arc;

use orbit_types::workflow::AUTO_TASK_TAG_PREFIX;
use serde::Serialize;

use super::{BaseClass, ProfileSet, ResourceProfile};

/// The delivery a profile is wanted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLookup {
    pub workspace_id: String,
    /// `auto-task:<template>`, `no-diff` or `change`.
    pub work_class: String,
    /// Ignored for a template.
    pub complexity: Option<String>,
    pub base_class: BaseClass,
}

impl ProfileLookup {
    /// A lookup for work of `work_class`. `no_diff_expected` says whether a
    /// template's tasks carry `no-diff-expected`; the `no-diff` and `change`
    /// classes are their own base.
    pub fn new(
        workspace_id: impl Into<String>,
        work_class: impl Into<String>,
        complexity: Option<String>,
        no_diff_expected: bool,
    ) -> Self {
        let work_class = work_class.into();
        let base_class = if work_class.starts_with(AUTO_TASK_TAG_PREFIX) {
            if no_diff_expected {
                BaseClass::NoDiff
            } else {
                BaseClass::Change
            }
        } else if work_class == BaseClass::NoDiff.as_str() {
            BaseClass::NoDiff
        } else {
            BaseClass::Change
        };
        Self {
            workspace_id: workspace_id.into(),
            work_class,
            complexity,
            base_class,
        }
    }

    fn is_template(&self) -> bool {
        self.work_class.starts_with(AUTO_TASK_TAG_PREFIX)
    }
}

/// The step of the fallback chain that answered a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    /// The delivery's own key.
    Exact,
    /// Step 1: the same workspace and class over every complexity.
    WithoutComplexity,
    /// Step 2, templates only: the template's base class in the same
    /// workspace.
    BaseClass,
    /// Step 3: the heaviest profile of the same base class on the host.
    HeaviestOfClass,
    /// Step 4: the heaviest profile on the host, so unknown work is assumed
    /// expensive.
    HeaviestOnHost,
    /// Step 5: the host has no profiles; admission stays as it is today.
    None,
}

/// A lookup's answer.
#[derive(Debug, Clone)]
pub struct ProfileMatch {
    pub source: ProfileSource,
    pub profile: Option<Arc<ResourceProfile>>,
}

impl ProfileSet {
    /// The profile for `lookup`, stopping at the first step whose profile has
    /// at least `min_profile_deliveries`:
    ///
    /// 0. the delivery's own key;
    /// 1. drop complexity (not for a template, whose key has none);
    /// 2. for a template, its base class in the same workspace, including
    ///    template deliveries;
    /// 3. the heaviest profile of the base class on the host, including
    ///    template profiles of that base class;
    /// 4. the heaviest profile on the host, sparse or not;
    /// 5. none, only on a host with no profiles.
    ///
    /// Steps 1–3 stay within the base class, so `no-diff` work never borrows
    /// a `change` profile before step 4.
    pub fn lookup(&self, lookup: &ProfileLookup) -> ProfileMatch {
        let found = |source, profile: Option<&Arc<ResourceProfile>>| {
            profile
                .filter(|profile| !profile.sparse)
                .map(|profile| ProfileMatch {
                    source,
                    profile: Some(Arc::clone(profile)),
                })
        };
        let complexity = if lookup.is_template() {
            None
        } else {
            lookup.complexity.as_deref()
        };
        let exact = self.profiles.iter().find(|profile| {
            profile.key.workspace_id == lookup.workspace_id
                && profile.key.work_class == lookup.work_class
                && profile.key.complexity.as_deref() == complexity
        });
        if let Some(answer) = found(ProfileSource::Exact, exact) {
            return answer;
        }
        let class_wide = |work_class: &str| {
            self.class_profiles.iter().find(|profile| {
                profile.key.workspace_id == lookup.workspace_id
                    && profile.key.work_class == work_class
            })
        };
        let step = if lookup.is_template() {
            found(
                ProfileSource::BaseClass,
                class_wide(lookup.base_class.as_str()),
            )
        } else {
            found(
                ProfileSource::WithoutComplexity,
                class_wide(&lookup.work_class),
            )
        };
        if let Some(answer) = step {
            return answer;
        }
        let every = || self.profiles.iter().chain(self.class_profiles.iter());
        let of_class = heaviest(
            every().filter(|profile| !profile.sparse && profile.base_class == lookup.base_class),
        );
        if let Some(answer) = found(ProfileSource::HeaviestOfClass, of_class) {
            return answer;
        }
        let on_host =
            heaviest(every().filter(|profile| !profile.sparse)).or_else(|| heaviest(every()));
        match on_host {
            Some(profile) => ProfileMatch {
                source: ProfileSource::HeaviestOnHost,
                profile: Some(Arc::clone(profile)),
            },
            None => ProfileMatch {
                source: ProfileSource::None,
                profile: None,
            },
        }
    }
}

/// The profile with the most expected core-minutes, then the highest memory
/// peak.
fn heaviest<'a>(
    profiles: impl Iterator<Item = &'a Arc<ResourceProfile>>,
) -> Option<&'a Arc<ResourceProfile>> {
    profiles.max_by(|a, b| {
        a.expected_core_minutes
            .total_cmp(&b.expected_core_minutes)
            .then(a.peak_memory_bytes.value.cmp(&b.peak_memory_bytes.value))
    })
}
