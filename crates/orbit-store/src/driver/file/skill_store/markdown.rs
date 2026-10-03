//! SKILL.md frontmatter validation and section parsing.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use serde::Deserialize;

use super::types::SkillSections;
use crate::fs::yaml::parse_yaml_with;

const PURPOSE_SECTION: &str = "Purpose";

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

pub(super) fn parse_skill_markdown(raw: &str) -> Result<SkillSections, OrbitError> {
    validate_required_frontmatter(raw)?;

    let mut current_section: Option<String> = None;
    let mut section_map: BTreeMap<String, String> = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim_end();
        if let Some(section_name) = parse_section_heading(trimmed.trim()) {
            let _ = section_map.entry(section_name.clone()).or_default();
            current_section = Some(section_name);
            continue;
        }

        let Some(section_name) = current_section.clone() else {
            continue;
        };

        let Some(entry) = section_map.get_mut(&section_name) else {
            return Err(OrbitError::SkillValidation(format!(
                "section parsing error for heading '{section_name}'"
            )));
        };
        entry.push_str(trimmed);
        entry.push('\n');
    }

    Ok(SkillSections {
        purpose: section_map
            .get(PURPOSE_SECTION)
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        behavioral_constraints: section_map
            .get("Behavioral Constraints")
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        output_requirements: section_map
            .get("Output Requirements")
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        evaluation_focus: section_map
            .get("Evaluation Focus")
            .map(|v| v.trim().to_string()),
        prohibitions: section_map
            .get("Prohibitions")
            .map(|v| v.trim().to_string()),
        examples: section_map.get("Examples").map(|v| v.trim().to_string()),
    })
}

fn validate_required_frontmatter(raw: &str) -> Result<(), OrbitError> {
    let fm = parse_frontmatter(raw)?;

    let Some(name) = fm.name else {
        return Err(OrbitError::SkillValidation(
            "missing required frontmatter field 'name'".to_string(),
        ));
    };
    if name.trim().is_empty() {
        return Err(OrbitError::SkillValidation(
            "frontmatter field 'name' must not be empty".to_string(),
        ));
    }

    let Some(description) = fm.description else {
        return Err(OrbitError::SkillValidation(
            "missing required frontmatter field 'description'".to_string(),
        ));
    };
    if description.trim().is_empty() {
        return Err(OrbitError::SkillValidation(
            "frontmatter field 'description' must not be empty".to_string(),
        ));
    }

    Ok(())
}

fn parse_frontmatter(raw: &str) -> Result<SkillFrontmatter, OrbitError> {
    let mut lines = raw.lines();
    let Some(first_line) = lines.next() else {
        return Err(OrbitError::SkillValidation(
            "missing frontmatter block".to_string(),
        ));
    };
    if first_line.trim() != "---" {
        return Err(OrbitError::SkillValidation(
            "missing frontmatter block".to_string(),
        ));
    }

    let mut fm_lines: Vec<&str> = Vec::new();
    let mut found_end = false;
    for line in lines {
        if line.trim() == "---" {
            found_end = true;
            break;
        }
        fm_lines.push(line);
    }

    if !found_end {
        return Err(OrbitError::SkillValidation(
            "unterminated frontmatter block".to_string(),
        ));
    }

    let fm_raw = fm_lines.join("\n");
    parse_yaml_with(&fm_raw, Path::new("<skill frontmatter>"), |_, e| {
        OrbitError::SkillValidation(format!("invalid skill frontmatter: {e}"))
    })
}

fn parse_section_heading(raw: &str) -> Option<String> {
    if !raw.starts_with('#') {
        return None;
    }
    let title = raw.trim_start_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some(title.to_string())
}
