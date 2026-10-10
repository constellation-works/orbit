use clap::Args;
use orbit_core::skill_catalog::LoadedSkill;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct SkillShowArgs {
    pub name: String,
}

impl Execute for SkillShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let skill = runtime.show_file_skill(&self.name)?;
        let doc = skill_to_json(&skill);

        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "Skill:         {}", skill.id);
        let _ = writeln!(out, "Path:          {}", skill.path.display());
        let _ = writeln!(out, "Content hash:  {}", skill.content_hash);
        let _ = writeln!(out, "\nBehavioral Contract (SKILL.md):");
        let _ = writeln!(out, "{}", skill.content);
        let _ = writeln!(out, "\nStructured Metadata (meta.json):");
        match &skill.meta_raw {
            Some(value) => {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(value)
                        .map_err(|e| OrbitError::Execution(e.to_string()))?
                );
            }
            None => {
                let _ = writeln!(out, "(none)");
            }
        }
        Ok(Payload::detail(doc, out).into())
    }
}

fn skill_to_json(skill: &LoadedSkill) -> Value {
    json!({
        "id": skill.id,
        "path": skill.path,
        "content_hash": skill.content_hash,
        "content": skill.content,
        "sections": {
            "purpose": skill.sections.purpose,
            "behavioral_constraints": skill.sections.behavioral_constraints,
            "output_requirements": skill.sections.output_requirements,
            "evaluation_focus": skill.sections.evaluation_focus,
            "prohibitions": skill.sections.prohibitions,
            "examples": skill.sections.examples,
        },
        "meta": skill.meta,
        "meta_raw": skill.meta_raw,
        "output_schema": skill.output_schema,
    })
}
