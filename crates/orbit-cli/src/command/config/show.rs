use clap::Args;
use orbit_config::EffectiveConfigValue;
use orbit_core::Core;
use orbit_core::app::config::{Command, ConfigShowData, ConfigShowRoots, Outcome, exec};
use serde_json::{Map, Value as JsonValue, json};

use crate::command::{Block, CommandOut, ExecuteCore, Payload};
use crate::output::config::ConfigShow;

use super::render;
use super::support::ConfigScopeArg;

#[derive(Args)]
pub struct ConfigShowArgs {
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
    /// List every key, including unconfigured advanced keys and sections
    /// whose keys are all unset, plus the Paths, Crew purposes and Review
    /// summaries.
    #[arg(long, visible_alias = "advanced")]
    pub all: bool,
}

impl ExecuteCore for ConfigShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Shown(shown) = exec(
            core,
            Command::Show {
                scope: self.scope.into(),
            },
        )?
        else {
            unreachable!("config show returned an unrelated outcome");
        };
        match *shown {
            ConfigShowData::Effective {
                values,
                review,
                crew_purposes,
                review_skip_tags,
                roots,
            } => {
                let review = review_switches(review);
                let mut json = effective_json(&roots, &values);
                json["review"] = review.json;
                let review_skip = review_skip_view(&review_skip_tags);
                json["review_skip_tags"] = review_skip.json;
                let mut text = effective_text(&roots, &values, self.all);
                if self.all || !review_skip.text.is_empty() {
                    text.push_str(&review_skip.text);
                }
                // Crew purposes and Review restate rows the sections above already
                // print. The default view omits them; `--all` and `--json` keep them.
                if self.all {
                    text.push_str(&crew_purposes_text(&crew_purposes));
                    text.push_str(&review.text);
                }
                json["crew_purposes"] = crew_purposes;
                Ok(Payload::blocks(json, vec![Block::config_show(text)]).into())
            }
            ConfigShowData::Scoped {
                store,
                snapshot,
                roots,
            } => {
                let settings = snapshot.all_values();
                Ok(Payload::blocks(
                    scoped_json(&roots, &store, &snapshot, &settings),
                    vec![Block::config_show(scoped_text(
                        &roots, &store, &snapshot, &settings, self.all,
                    ))],
                )
                .into())
            }
        }
    }
}

/// Every automatic-review switch with its source [ORB-13992] [ORB-20099]:
/// before-PR review (deprecated `review.timing = before_pr`, minutes, crew),
/// before-landing review (`review.timing = before_landing`) and after-landing
/// review (the `delivery-code-review` auto-task, its next batch and health).
/// The before-PR line marks its deprecated timing; deprecation alone is never
/// an error.
struct ReviewView {
    json: JsonValue,
    text: String,
}

/// The effective `[workflow.review_skip_tags]` mappings: each tag's
/// `paths` and optional `guard`. Empty text when none is mapped (the
/// JSON still carries an empty object).
struct ReviewSkipView {
    json: JsonValue,
    text: String,
}

fn review_switches(json: JsonValue) -> ReviewView {
    let text = match json["error"].as_str() {
        Some(error) => format!("\nReview: {error}\n"),
        None => format!(
            "\nReview:\n  before-PR: {}\n  before-landing: {}\n  after-landing: {}\n",
            json["before_pr"]["line"].as_str().unwrap_or_default(),
            json["before_landing"]["line"].as_str().unwrap_or_default(),
            json["after_landing"]["line"].as_str().unwrap_or_default(),
        ),
    };
    ReviewView { json, text }
}

fn review_skip_view(entries: &[(String, Vec<String>, Option<String>)]) -> ReviewSkipView {
    let mut map = Map::new();
    let mut text = String::new();
    if !entries.is_empty() {
        text.push_str("\nReview skip tags:\n");
        for (tag, paths, guard) in entries {
            map.insert(tag.clone(), json!({"paths": paths, "guard": guard}));
            let guard = guard.as_deref().unwrap_or("none");
            text.push_str(&format!(
                "  {tag}: paths=[{}] guard={guard}\n",
                paths.join(", ")
            ));
        }
    }
    ReviewSkipView {
        json: JsonValue::Object(map),
        text,
    }
}

/// The `Crew purposes:` block `orbit config show` prints.
fn crew_purposes_text(view: &JsonValue) -> String {
    let mut text = String::from("\nCrew purposes:\n");
    for row in view.as_array().into_iter().flatten() {
        let field = |name: &str| row[name].as_str().unwrap_or("-");
        let pool = row["pool"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let line = if row["configured"].as_bool() == Some(true) {
            format!("{pool} ({}, {})", field("key"), field("source"))
        } else if field("resolved_from") == field("key") {
            format!("none ({} is empty or unset)", field("key"))
        } else if row["source"].is_null() {
            format!("unset; falls back to {}", field("resolved_from"))
        } else if pool.is_empty() {
            format!(
                "unset; falls back to {} (empty, so workflow.default_crew)",
                field("resolved_from")
            )
        } else {
            format!(
                "unset; falls back to {} [{pool}] ({})",
                field("resolved_from"),
                field("source")
            )
        };
        text.push_str(&format!("  {}: {line}\n", field("purpose")));
    }
    text
}

fn effective_json(roots: &ConfigShowRoots, values: &[EffectiveConfigValue]) -> JsonValue {
    let mut settings = Map::new();
    let mut provenance = Map::new();
    for entry in values {
        settings.insert(entry.key.clone(), entry.value.clone());
        // `provenance` is the per-key record: the pre-existing `scope`/`path`
        // fields keep their meaning and position, and the grouping, registry
        // description, three-way state, and shadowed layers are added
        // alongside them rather than in a parallel map.
        provenance.insert(entry.key.clone(), render::effective_provenance_json(entry));
    }
    let global_path = roots.global_root.join("config.toml");
    let workspace_path = roots.shared_root.join("config.toml");
    let global_exists = global_path.exists();
    let workspace_exists = workspace_path.exists() && roots.has_workspace_layer;
    let config_path = if workspace_exists {
        &workspace_path
    } else {
        &global_path
    };

    json!({
        "source": {
            "scope": "effective",
            "global_path": global_path.to_string_lossy(),
            "global_exists": global_exists,
            "workspace_path": workspace_path.to_string_lossy(),
            "workspace_exists": workspace_exists,
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings,
        "provenance": provenance,
        "workspace_binding": render::workspace_binding_json(roots),
        "execution_env_inherit": false,
        "global_root": roots.global_root.to_string_lossy(),
        "shared_root": roots.shared_root.to_string_lossy(),
        "local_root": roots.local_root.to_string_lossy(),
        "config_path": config_path.to_string_lossy(),
        "persistence": roots.persistence,
    })
}

pub(super) fn effective_text(
    roots: &ConfigShowRoots,
    values: &[EffectiveConfigValue],
    all: bool,
) -> ConfigShow {
    render::effective_text(roots, values, all)
}

fn scoped_json(
    roots: &ConfigShowRoots,
    store: &orbit_config::ConfigStore,
    snapshot: &orbit_config::ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
) -> JsonValue {
    let mut settings_obj = Map::new();
    let mut provenance = Map::new();
    for (key, value) in settings {
        settings_obj.insert((*key).to_string(), value.clone());
        provenance.insert(
            (*key).to_string(),
            render::scoped_provenance_json(store, key, value),
        );
    }

    // `global_root`/`shared_root`/`local_root`/`config_path`/`persistence`
    // stay top-level (not nested under a `derived` object) because an
    // existing contract test (`worktree_resolution.rs`) already asserts on
    // `shared_root`/`local_root` at the top level of `config show --json`,
    // and explicitly forbids reintroducing renamed aliases for them. The
    // grouped `Paths` section the task asks for is rendered in the human-
    // readable text output below; the JSON shape keeps these pre-existing
    // field names and positions unchanged.
    json!({
        "source": {
            "scope": store.scope().label(),
            "path": store.path().to_string_lossy(),
            "exists": store.path().exists(),
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings_obj,
        "provenance": provenance,
        "workspace_binding": render::workspace_binding_json(roots),
        "execution_env_inherit": snapshot.execution_env_inherit,
        "global_root": roots.global_root.to_string_lossy(),
        "shared_root": roots.shared_root.to_string_lossy(),
        "local_root": roots.local_root.to_string_lossy(),
        "config_path": store.path().to_string_lossy(),
        "persistence": roots.persistence,
    })
}

pub(super) fn scoped_text(
    roots: &ConfigShowRoots,
    store: &orbit_config::ConfigStore,
    snapshot: &orbit_config::ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
    all: bool,
) -> ConfigShow {
    render::scoped_text(roots, store, snapshot, settings, all)
}
