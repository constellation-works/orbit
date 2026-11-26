//! Human rendering of `orbit config show`: aligned label, value, state and description rows, fitted to the terminal width.

/// Aligned configuration text with trailing-prose boundaries supplied by its producer.
/// Offsets refer to UTF-8 line starts; columns count characters, as the formatter does.
#[derive(Debug, Default)]
pub struct ConfigShow {
    pub(crate) text: String,
    pub(crate) trailing_columns: std::collections::BTreeMap<usize, usize>,
}

impl ConfigShow {
    /// Append verbatim text without changing previously recorded boundaries.
    pub(crate) fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Mark the current position as this line's trailing prose column.
    /// Call after writing all padded cells and their final gutter.
    pub(crate) fn mark_trailing_column(&mut self) {
        let line_start = self.text.rfind('\n').map_or(0, |index| index + 1);
        let column = self.text[line_start..].chars().count();
        self.trailing_columns.insert(line_start, column);
    }
}

impl std::fmt::Write for ConfigShow {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_str(text);
        Ok(())
    }
}

/// Minimum columns a description column must leave before aligned wrapping
/// gives way to a shallower hanging indent.
const CONFIG_SHOW_MIN_TRAILING: usize = 24;

/// Fit `config show` text to `width` columns. Lines that fit are left exactly
/// as written, so aligned columns survive. A longer line wraps its trailing
/// prose at word boundaries with continuations aligned to that prose's
/// column: the blurb column of a heading (after the padded title), the
/// description column of a key row (after label/value/state), the trailing
/// column of a crew row, or the value column of a Paths row. When that column
/// would leave too little room — narrow terminals where the aligned columns
/// themselves approach the width — the whole line wraps with a shallow
/// hanging indent instead, so no line exceeds the width. Width `0` (no
/// terminal width) leaves the text whole.
pub(crate) fn config_show_text(text: &ConfigShow, width: u16) -> String {
    if width == 0 {
        return text.text.clone();
    }
    let mut line_start = 0;
    text.text
        .split('\n')
        .map(|line| {
            let column = text.trailing_columns.get(&line_start).copied();
            line_start += line.len() + 1;
            fit_config_show_line(line, usize::from(width), column)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fit_config_show_line(line: &str, width: usize, trailing_column: Option<usize>) -> String {
    if line.chars().count() <= width {
        return line.to_string();
    }
    // Row boundaries come from the command's padded cells, never word positions:
    // values, tags and path labels may all contain whitespace.
    let column = trailing_column.or_else(|| {
        // Headings retain their fixed padded title/blurb layout.
        if line.starts_with(' ') {
            None
        } else {
            find_blurb_start(line)
        }
    });
    let aligned = column.map(|column| {
        let split = byte_at(line, column);
        (line[..split].to_string(), line[split..].to_string(), column)
    });
    if let Some((prefix, trailing, column)) = aligned
        && prefix.chars().count() + 8 <= width
    {
        // Keep the row's columns on the first line. Continuations align to
        // the trailing column when it leaves room; on a narrow terminal
        // they fall back to a shallow indent so the prose gets full width.
        let indent = if column + CONFIG_SHOW_MIN_TRAILING <= width {
            column
        } else {
            4.min(width.saturating_sub(1))
        };
        return wrap_trailing(&prefix, &trailing, indent, width);
    }
    // Fallback: wrap the whole line with a hanging indent two columns deeper
    // than its own indent (column 0 for headings).
    let lead = line.chars().take_while(|c| *c == ' ').count();
    let indent = if lead > 0 { lead + 2 } else { 0 };
    let indent = if indent + 1 >= width {
        lead.min(width.saturating_sub(1))
    } else {
        indent
    };
    let lead = lead.min(width.saturating_sub(1));
    wrap_trailing(
        &" ".repeat(lead),
        line.trim_start_matches(' '),
        indent,
        width,
    )
}

/// Byte index of the `char_offset`-th char of `line` (its length when beyond).
fn byte_at(line: &str, char_offset: usize) -> usize {
    line.char_indices()
        .nth(char_offset)
        .map_or(line.len(), |(byte, _)| byte)
}

/// Char offset where a heading's blurb starts: the end of the first run of
/// two or more spaces, when any text follows it.
fn find_blurb_start(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let mut run = 0;
    for (index, ch) in chars.iter().enumerate() {
        if *ch == ' ' {
            run += 1;
            if run >= 2 {
                // End of this run.
                let mut end = index + 1;
                while end < chars.len() && chars[end] == ' ' {
                    end += 1;
                }
                if end < chars.len() {
                    return Some(end);
                }
                return None;
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Wrap `trailing` at word boundaries into lines of at most `width` chars:
/// the first follows `prefix`, continuations sit at `indent`. A word moves to
/// the next line whole when it fits there, even if it is too wide for the
/// first line's remainder; only a word wider than a continuation line is split.
fn wrap_trailing(prefix: &str, trailing: &str, indent: usize, width: usize) -> String {
    let available_first = width.saturating_sub(prefix.chars().count()).max(1);
    let available_rest = width.saturating_sub(indent).max(1);
    let mut wrapped: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut available = available_first;
    let push_word =
        |current: &mut String, wrapped: &mut Vec<String>, available: &mut usize, word: &str| {
            let mut rest = word;
            loop {
                let room = available.saturating_sub(current.chars().count());
                if current.is_empty() {
                    if rest.chars().count() <= *available {
                        current.push_str(rest);
                        return;
                    }
                    // A first word too wide for the first line's remainder moves
                    // whole to a continuation when it fits there; the first line
                    // then keeps only its prefix (see the assembly below).
                    if wrapped.is_empty() && rest.chars().count() <= available_rest {
                        wrapped.push(String::new());
                        *available = available_rest;
                        continue;
                    }
                    // Split a word wider than a whole line.
                    let take = *available;
                    let split = byte_at(rest, take);
                    current.push_str(&rest[..split]);
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                    rest = &rest[split..];
                } else if rest.chars().count() < room {
                    current.push(' ');
                    current.push_str(rest);
                    return;
                } else {
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                }
            }
        };
    for word in trailing.split_whitespace() {
        push_word(&mut current, &mut wrapped, &mut available, word);
    }
    wrapped.push(current);
    let mut out = String::new();
    for (index, chunk) in wrapped.iter().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
        } else if chunk.is_empty() && wrapped.len() > 1 {
            // The first word moved to a continuation: no trailing gutter is left behind.
            out.push_str(prefix.trim_end());
        } else {
            out.push_str(prefix);
        }
        out.push_str(chunk);
    }
    out
}
