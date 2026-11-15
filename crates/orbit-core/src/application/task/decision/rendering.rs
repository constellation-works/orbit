//! Render and edit curated rule Markdown while preserving surrounding text.

use super::rules::{
    CorpusDecisionRule, CorpusRuleStatus, OPERATOR_AUTO, fence_is_unclosed, rule_fences,
    rules_section, valid_slug,
};

pub(super) fn rewrite_rules_section(markdown: &str, rules: &[CorpusDecisionRule]) -> String {
    let Some(section) = rules_section(markdown) else {
        let mut out = markdown.trim_end().to_string();
        out.push_str("\n\n## Rules\n\n");
        out.push_str(&render_rules(rules));
        return out;
    };
    let section_start = section.as_ptr() as usize - markdown.as_ptr() as usize;
    let mut out = markdown[..section_start].to_string();
    let mut cursor = 0;
    let mut used = 0;
    let newline = if markdown.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    for (start, end, original) in rule_fences(section) {
        out.push_str(&section[cursor..start]);
        if let Some(rule) = rules.get(used) {
            if *rule == original {
                out.push_str(&section[start..end]);
                if fence_is_unclosed(section, start, end) {
                    if !out.ends_with('\n') {
                        out.push_str(newline);
                    }
                    out.push_str("```");
                    out.push_str(newline);
                }
            } else {
                let rendered = render_rules(std::slice::from_ref(rule));
                out.push_str(
                    rendered
                        .trim_end_matches('\n')
                        .replace('\n', newline)
                        .as_str(),
                );
                out.push_str(newline);
            }
            used += 1;
        }
        cursor = end;
    }
    out.push_str(&section[cursor..]);
    if used < rules.len() {
        if !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(newline);
        out.push_str(&render_rules(&rules[used..]).replace('\n', newline));
    }
    out.push_str(&markdown[section_start + section.len()..]);
    out
}

fn render_rules(rules: &[CorpusDecisionRule]) -> String {
    let mut out = String::new();
    for rule in rules {
        out.push_str("```yaml\n");
        out.push_str(&render_rule(rule));
        out.push_str("```\n\n");
    }
    out
}

fn render_rule(rule: &CorpusDecisionRule) -> String {
    let mut out = String::new();
    push_scalar(&mut out, "id", &rule.id);
    push_scalar(&mut out, "question", &rule.question);
    push_scalar(&mut out, "answer", &rule.answer);
    push_scalar(&mut out, "why", &rule.why);
    out.push_str("citations:\n");
    if rule.citations.is_empty() {
        out.push_str("  []\n");
    } else {
        for citation in &rule.citations {
            out.push_str(&format!("  - task: {}\n", yaml_quote(&citation.task)));
            out.push_str(&format!("    date: {}\n", yaml_quote(&citation.date)));
            out.push_str(&format!("    option: {}\n", yaml_quote(&citation.option)));
        }
    }
    out.push_str("depends_on:\n");
    if rule.depends_on.is_empty() {
        out.push_str("  []\n");
    } else {
        for dependency in &rule.depends_on {
            out.push_str(&format!("  - kind: {}\n", yaml_quote(&dependency.kind)));
            out.push_str(&format!("    name: {}\n", yaml_quote(&dependency.name)));
            out.push_str(&format!("    digest: {}\n", yaml_quote(&dependency.digest)));
        }
    }
    push_scalar(&mut out, "status", rule.status.as_str());
    push_scalar(&mut out, "review_by", &rule.review_by);
    out.push_str(&format!(
        "auto: {}\n",
        if rule.auto { "true" } else { "false" }
    ));
    if rule.auto
        && let Some(actor) = &rule.auto_set_by
    {
        push_scalar(&mut out, "auto_set_by", actor);
    }
    out
}

fn push_scalar(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&yaml_quote(value));
    out.push('\n');
}

fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Replace `auto` on the rule `rule_ref` (`stem` or `stem/id`) inside `markdown`.
/// Returns the rewritten document and the rule. `auto_set_by` is `operator`
/// only when `auto` is true; turning auto off clears it.
pub fn set_rule_auto_in_markdown(
    markdown: &str,
    rule_ref: &str,
    auto: bool,
) -> Option<(String, CorpusDecisionRule)> {
    let (stem_hint, rule_id) = rule_ref.split_once('/').unwrap_or((rule_ref, rule_ref));
    if !valid_slug(rule_id) || (rule_ref.contains('/') && !valid_slug(stem_hint)) {
        return None;
    }
    let section = rules_section(markdown)?;
    let (start, end, mut rule) = rule_fences(section)
        .into_iter()
        .find(|(_, _, rule)| rule.id == rule_id)?;
    if rule.status != CorpusRuleStatus::Current {
        return None;
    }
    rule.auto = auto;
    rule.auto_set_by = auto.then(|| OPERATOR_AUTO.to_string());
    let fence = &section[start..end];
    let newline = if fence.contains("\r\n") { "\r\n" } else { "\n" };
    let mut replacement = String::new();
    let mut found_auto = false;
    let mut found_actor = false;
    let mut found_closing = false;
    for line in fence.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if content.starts_with("auto:") {
            replacement.push_str(if auto { "auto: true" } else { "auto: false" });
            replacement.push_str(ending);
            found_auto = true;
        } else if content.starts_with("auto_set_by:") {
            if auto {
                replacement.push_str("auto_set_by: \"operator\"");
                replacement.push_str(ending);
            }
            found_actor = true;
        } else if content.trim() == "```" {
            if !found_auto {
                replacement.push_str(if auto { "auto: true" } else { "auto: false" });
                replacement.push_str(newline);
            }
            if auto && !found_actor {
                replacement.push_str("auto_set_by: \"operator\"");
                replacement.push_str(newline);
            }
            replacement.push_str(line);
            found_closing = true;
        } else {
            replacement.push_str(line);
        }
    }
    if !found_closing {
        if !replacement.ends_with('\n') {
            replacement.push_str(newline);
        }
        if !found_auto {
            replacement.push_str(if auto { "auto: true" } else { "auto: false" });
            replacement.push_str(newline);
        }
        if auto && !found_actor {
            replacement.push_str("auto_set_by: \"operator\"");
            replacement.push_str(newline);
        }
        replacement.push_str("```");
        replacement.push_str(newline);
    }
    // Refuse forms we cannot safely edit as top-level lines (e.g. flow YAML).
    let replacement_rule = rule_fences(&replacement).into_iter().next()?.2;
    if replacement_rule != rule {
        return None;
    }
    let section_start = section.as_ptr() as usize - markdown.as_ptr() as usize;
    let mut out = markdown[..section_start + start].to_string();
    out.push_str(&replacement);
    out.push_str(&markdown[section_start + end..]);
    Some((out, rule))
}
