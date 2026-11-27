//! Human rendering of `orbit doctor` findings: the non-ok checks, in severity order, shown after the terminal table.

/// Full non-ok diagnostics in severity order. Long findings and repair
/// actions wrap onto indented lines when exceeding the terminal width.
pub(crate) fn doctor_findings(
    rows: &[orbit_core::app::doctor::WorkspaceDoctorResult],
    width: u16,
) -> String {
    use orbit_core::app::doctor::WorkspaceDoctorStatus as Status;
    let mut findings = Vec::new();
    for status in [Status::Error, Status::Warning, Status::Skipped] {
        for row in rows.iter().filter(|row| row.status == status) {
            let label = match status {
                Status::Error => "ERROR",
                Status::Warning => "WARN",
                Status::Skipped => "SKIP",
                // Informational rows are facts, not findings; the table
                // shows them.
                Status::Ok | Status::Info => continue,
            };
            let mut finding = format!(
                "{}  {label}\n{}",
                row.check_name,
                wrap_finding_message(&row.message, width)
            );
            if let Some(fix) = &row.remediation {
                finding.push('\n');
                finding.push_str(&wrap_action(fix, width));
            }
            findings.push(finding);
        }
    }
    if findings.is_empty() {
        String::new()
    } else {
        format!("Findings:\n\n{}", findings.join("\n\n"))
    }
}

/// Wrap a finding message onto indented lines, keeping each line within the terminal width.
fn wrap_finding_message(message: &str, width: u16) -> String {
    let indent = "  ";
    if width == 0 {
        return message
            .lines()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let available = width.saturating_sub(indent.len() as u16).max(1);
    let wrapped = wrap_message(message, available);
    wrapped
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap repair action text at the finding's two-space indent, with wrapped
/// lines on a four-space hanging indent so the remedy reads as part of its
/// finding.
fn wrap_action(fix: &str, width: u16) -> String {
    let indent = "  ";
    let hanging = "    ";
    let prefix = format!("{indent}Fix: ");
    let single_line = format!("{indent}Fix: {fix}");
    if width == 0 || single_line.chars().count() <= width as usize {
        return single_line;
    }
    let first_available = width.saturating_sub(prefix.chars().count() as u16).max(1) as usize;
    let rest_available = width.saturating_sub(hanging.len() as u16).max(1) as usize;

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_max = first_available;

    for word in fix.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.chars().count() + 1 + word.chars().count() <= current_max {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            current_max = rest_available;
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    let mut out = String::new();
    let mut iter = lines.into_iter();
    if let Some(first) = iter.next() {
        if format!("{indent}Fix: {first}").chars().count() <= width as usize {
            out.push_str(&format!("{indent}Fix: {first}"));
        } else {
            out.push_str(&format!("{indent}Fix:\n{hanging}{first}"));
        }
    }
    for line in iter {
        out.push('\n');
        out.push_str(hanging);
        out.push_str(&line);
    }
    out
}

/// Use the table library's display-width-aware wrapping without the list's
/// one-line cap. No message text is truncated.
fn wrap_message(message: &str, width: u16) -> String {
    use comfy_table::{Cell, ColumnConstraint, ContentArrangement, Table, Width, presets};
    if width == 0 {
        return message.to_string();
    }
    let mut prose = Table::new();
    prose.load_style(presets::NOTHING);
    prose.force_no_tty();
    prose.set_content_arrangement(ContentArrangement::Disabled);
    prose.add_row(vec![Cell::new(message)]);
    if let Some(column) = prose.column_mut(0) {
        column.set_padding((0, 0));
        column.set_constraint(ColumnConstraint::Absolute(Width::Fixed(width)));
    }
    prose
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
