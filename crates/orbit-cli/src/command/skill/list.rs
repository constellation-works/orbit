use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::skill_catalog::LoadedSkill;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct SkillListArgs {}

impl Execute for SkillListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let skills = runtime.list_file_skills()?;
        let values = skills.iter().map(skill_summary_json).collect::<Vec<_>>();

        use crate::output::table::{Column, Table};
        // `orbit skill show <id>` prints a skill's untruncated summary.
        let mut table = Table::new(vec![
            Column::new("ID").fixed(),
            Column::new("HASH").fixed(),
            Column::new("TAGS").number(),
            Column::new("SUMMARY"),
        ])
        .empty_message("no file skills installed");
        for skill in skills {
            let summary = skill
                .meta
                .as_ref()
                .and_then(|meta| meta.summary.clone())
                .unwrap_or_default();
            let tags = skill.meta.as_ref().map(|meta| meta.tags.len()).unwrap_or(0);
            table.add_row(vec![
                skill.id.clone(),
                skill.content_hash[..10].to_string(),
                tags.to_string(),
                summary,
            ]);
        }
        Ok(Payload::list(values, table).into())
    }
}

fn skill_summary_json(skill: &LoadedSkill) -> Value {
    json!({
        "id": skill.id,
        "content_hash": skill.content_hash,
        "path": skill.path,
        "meta": skill.meta,
    })
}
