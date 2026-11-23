//! Shipped skill trees: the ordered asset catalog, their materialization
//! into a skills root, and the skill catalog and discovery-link doctor.

mod catalog;
mod doctor;
mod materialize;

pub(crate) use catalog::{DEFAULT_SKILL_FILES, default_skill_ids};
pub use doctor::{SkillDoctorResult, SkillDoctorStatus};
pub(crate) use doctor::{dangling_client_skill_links, skill_link_remediation, skill_link_roots};
pub(crate) use materialize::{
    inject_skill_template_tokens, is_default_skill_tree_for_root, seed_default_skills,
};
