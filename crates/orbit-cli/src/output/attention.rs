//! Human rendering of `orbit attention` items: a labelled block per item on a terminal, and one tab-separated record per item in plain mode.

/// The values used to render one operator attention item in human output.
#[derive(Debug)]
pub struct AttentionRow {
    pub(crate) severity: String,
    pub(crate) kind: String,
    pub(crate) host: Option<String>,
    pub(crate) workspace: String,
    pub(crate) reference: String,
    pub(crate) age: String,
    pub(crate) reason: String,
    pub(crate) next_command: String,
}

impl AttentionRow {
    /// Format a labeled item block for terminal display.
    pub(crate) fn attention_terminal_text(&self) -> String {
        let mut heading = format!("{} {}", self.severity, self.kind);
        if let Some(host) = self.host.as_ref().filter(|host| !host.is_empty()) {
            heading.push_str(&format!(" on {host}"));
        }
        if !self.workspace.is_empty() {
            heading.push_str(&format!(" — {}", self.workspace));
        }
        if !self.reference.is_empty() {
            heading.push_str(&format!(" ({})", self.reference));
        }
        if !self.age.is_empty() {
            heading.push_str(&format!(" [{}]", self.age));
        }

        let mut text = format!("{heading}\n  Reason: {}", self.reason);
        if !self.next_command.is_empty() {
            text.push_str(&format!("\n  Next: {}", self.next_command));
        }
        text
    }

    /// Fields for the stable plain record form, omitting an absent next action.
    pub(crate) fn attention_plain_fields(&self) -> Vec<&str> {
        let mut fields = vec![
            self.severity.as_str(),
            self.kind.as_str(),
            self.workspace.as_str(),
            self.reference.as_str(),
            self.age.as_str(),
            self.reason.as_str(),
        ];
        if let Some(host) = self.host.as_deref() {
            fields.insert(2, host);
        }
        if !self.next_command.is_empty() {
            fields.push(self.next_command.as_str());
        }
        fields
    }

    /// The tab-separated plain-form header for `rows`: one label per field
    /// that [`Self::attention_plain_fields`] emits, so the global `--header`
    /// labels attention the way it labels every other plain table.
    pub(crate) fn attention_plain_header(rows: &[AttentionRow]) -> String {
        let mut labels = vec!["SEVERITY", "KIND"];
        if rows.iter().any(|row| row.host.is_some()) {
            labels.push("HOST");
        }
        labels.extend(["WORKSPACE", "REF", "AGE", "REASON"]);
        if rows.iter().any(|row| !row.next_command.is_empty()) {
            labels.push("NEXT");
        }
        labels.join("\t")
    }
}
