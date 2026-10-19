//! `orbit config show` descriptions stay one scannable sentence.

use crate::CONFIG_KEY_REGISTRY;
use crate::registry::CONFIG_SHOW_DESCRIPTION_MAX_CHARS;

#[test]
fn registry_descriptions_are_one_scannable_sentence() {
    for entry in CONFIG_KEY_REGISTRY {
        let description = entry.description;
        assert!(
            one_sentence(description),
            "readability rule: each config-show description is one sentence of at most \
             {CONFIG_SHOW_DESCRIPTION_MAX_CHARS} characters so the overview stays scannable; \
             longer detail belongs in docs/CONFIG.md ({} is {} characters: {description})",
            entry.key,
            description.chars().count(),
        );
    }
}

/// One plain sentence: no line break, no second sentence, and within the
/// registry bound.
fn one_sentence(description: &str) -> bool {
    if description.contains(['\n', '!', '?']) || !description.ends_with('.') {
        return false;
    }
    let chars = description.chars().count();
    chars > 0
        && chars <= CONFIG_SHOW_DESCRIPTION_MAX_CHARS
        && !description[..description.len() - 1].contains(". ")
}
