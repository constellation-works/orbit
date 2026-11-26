use super::*;

pub(super) fn render_acceptance(criteria: &[String]) -> String {
    if criteria.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for criterion in criteria {
        out.push_str("- [ ] ");
        out.push_str(criterion.trim());
        out.push('\n');
    }
    out
}

pub(super) fn parse_acceptance(content: &str) -> Vec<OrbitId> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.strip_prefix("- [ ] ")
                .or_else(|| line.strip_prefix("- [x] "))
                .or_else(|| line.strip_prefix("- [X] "))
                .or_else(|| line.strip_prefix("* [ ] "))
                .or_else(|| line.strip_prefix("* [x] "))
                .or_else(|| line.strip_prefix("* [X] "))
                .or_else(|| line.strip_prefix("- "))
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
                .to_string()
        })
        .collect()
}
