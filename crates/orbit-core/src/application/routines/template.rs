//! Recognize and render shipped routine templates: which template family an
//! on-disk routine is a lifecycle-only variant of, and the binding it renders.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::write_text_with_parent;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::RoutineDefinition;

use super::rewrite_enabled_line;
use super::seed::{
    BASE_BRANCH_PLACEHOLDER, DEFAULT_ROUTINE_FILES, OWNER_MACHINE_PLACEHOLDER,
    RETIRED_ROUTINE_FILES, ROUTINE_NAME_PLACEHOLDER, RoutineSeedIdentity,
    SUPERSEDED_ROUTINE_TEMPLATES,
};
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout, ManagedAssetOutcome,
    ManagedAssetReconcileMode, ManagedAssetReconciliation, RoutineAssetProvenance,
    RoutineMaterializationBinding, load_managed_asset_manifest,
};

/// Whether `orbit workspace sync` would remove the routine definition at
/// `path` from `routines_dir`'s active catalog.
///
/// Every surface that tells an operator to run the sync asks this first: the
/// advice is only true for a file reconciliation can reach — one the manifest
/// tracks under a name this Orbit no longer ships, or an untracked copy of a
/// retired template. An operator's own definition is preserved by design, so
/// naming the sync for it would loop forever [DANI-10502].
pub(crate) fn sync_retires_routine(routines_dir: &Path, path: &Path) -> bool {
    if path.parent() != Some(routines_dir) {
        // A `local/` definition, or a file outside this catalog entirely:
        // seeding writes neither and retires neither.
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    if DEFAULT_ROUTINE_FILES.iter().any(|(name, _)| *name == stem) {
        return false;
    }
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    if load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
        .ok()
        .flatten()
        .is_some_and(|manifest| manifest.assets.contains_key(stem))
    {
        return true;
    }
    fs::read_to_string(path)
        .ok()
        .is_some_and(|existing| shipped_shape_of(stem, &existing) == Some(ShippedShape::Retired))
}

/// Which template family an on-disk managed routine is a lifecycle-only
/// variant of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShippedShape {
    /// Matches the template this Orbit ships for the stem.
    Current,
    /// Matches an earlier shape of a template this Orbit still ships.
    Superseded,
    /// Matches the last shape of a default this Orbit no longer ships.
    Retired,
}

/// The fields a shipped template owns. `enabled` is the operator's opt-in
/// knob, `hosts:` is a retired key operators were told to drop, and comments
/// are not fields at all — none of them counts as a local modification.
fn template_owned_shape(definition: &RoutineDefinition) -> RoutineDefinition {
    RoutineDefinition {
        enabled: false,
        legacy_hosts: None,
        ..definition.clone()
    }
}

/// Which shipped template `existing` — the on-disk document for `file_stem`
/// — differs from only in operator-owned lifecycle settings, rendered against
/// the document's own name. `None` means the document does not parse or a
/// template-owned field changed: a genuine local edit.
pub(crate) fn shipped_shape_of(file_stem: &str, existing: &str) -> Option<ShippedShape> {
    let definition = parse_routine_yaml(existing).ok()?;
    let binding = binding_of(&definition);
    let shape = template_owned_shape(&definition);
    // A template that needs an owner the document does not declare (a cron
    // document judged against the state template) cannot be that shape.
    let matches = |templates: &[(&str, &str)]| {
        templates
            .iter()
            .filter(|(stem, _)| *stem == file_stem)
            .any(|(_, template)| {
                render_routine_template(file_stem, template, &binding)
                    .ok()
                    .and_then(|rendered| parse_routine_yaml(&rendered).ok())
                    .is_some_and(|rendered| template_owned_shape(&rendered) == shape)
            })
    };
    if matches(DEFAULT_ROUTINE_FILES) {
        Some(ShippedShape::Current)
    } else if matches(SUPERSEDED_ROUTINE_TEMPLATES) {
        Some(ShippedShape::Superseded)
    } else if matches(RETIRED_ROUTINE_FILES) {
        Some(ShippedShape::Retired)
    } else {
        None
    }
}

/// Whether an on-disk managed routine is Orbit's: byte-identical to the
/// digest the manifest recorded, or a lifecycle-only variant of a template
/// this or a prior release shipped for `file_stem`.
pub(crate) fn is_orbit_written_routine(
    file_stem: &str,
    recorded_digest: &str,
    existing: &str,
) -> bool {
    sha256_hex(existing.as_bytes()) == recorded_digest
        || shipped_shape_of(file_stem, existing).is_some()
}

/// Reconcile a tracked managed routine whose bytes no longer match its
/// recorded digest. A lifecycle-only variant of the current template is
/// adopted as-is; one of a superseded (or retired) template is refreshed onto
/// the current template with the operator's `enabled` kept. Returns the
/// provenance to record, or `None` for a genuine local edit the caller
/// preserves.
#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_lifecycle_variant(
    identity: &RoutineSeedIdentity,
    file_stem: &str,
    template: &str,
    template_digest: &str,
    path: &Path,
    existing: &str,
    mode: ManagedAssetReconcileMode,
    result: &mut ManagedAssetReconciliation,
) -> Result<Option<RoutineAssetProvenance>, OrbitError> {
    let Some(shape) = shipped_shape_of(file_stem, existing) else {
        return Ok(None);
    };
    // `shipped_shape_of` parsed the document, so this cannot fail. A cron
    // document refreshed onto a state template takes this host as its owner.
    let definition = parse_routine_yaml(existing)?;
    let binding = identity.complete(file_stem, template, &binding_of(&definition));
    match shape {
        ShippedShape::Current => {
            result.actions.push(ManagedAssetAction {
                name: file_stem.to_string(),
                path: path.to_path_buf(),
                outcome: ManagedAssetOutcome::Migrated,
                detail: Some(
                    "adopted the operator's lifecycle settings (`enabled`, dropped `hosts:`) on an otherwise current managed routine"
                        .to_string(),
                ),
            });
            Ok(Some(RoutineAssetProvenance {
                template_digest: template_digest.to_string(),
                rendered_digest: sha256_hex(existing.as_bytes()),
                binding,
            }))
        }
        ShippedShape::Superseded | ShippedShape::Retired => {
            let rendered = render_refresh(file_stem, template, &binding, existing)?;
            if mode == ManagedAssetReconcileMode::Apply {
                write_text_with_parent(path, &rendered)?;
            }
            result.refreshed += 1;
            result.actions.push(ManagedAssetAction {
                name: file_stem.to_string(),
                path: path.to_path_buf(),
                outcome: ManagedAssetOutcome::Refreshed,
                detail: Some(
                    "shipped routine template changed; refreshed a prior release's routine and kept its `enabled` setting"
                        .to_string(),
                ),
            });
            Ok(Some(RoutineAssetProvenance {
                template_digest: template_digest.to_string(),
                rendered_digest: sha256_hex(rendered.as_bytes()),
                binding,
            }))
        }
    }
}

/// Render the current template for `binding`, keeping the `enabled` choice of
/// the on-disk document being refreshed. A document that does not parse has
/// no choice to keep and gets the template default.
pub(super) fn render_refresh(
    file_stem: &str,
    template: &str,
    binding: &RoutineMaterializationBinding,
    existing: &str,
) -> Result<String, OrbitError> {
    let rendered = render_routine_template(file_stem, template, binding)?;
    let Ok(existing) = parse_routine_yaml(existing) else {
        return Ok(rendered);
    };
    let template_enabled = parse_routine_yaml(&rendered)?.enabled;
    if existing.enabled == template_enabled {
        return Ok(rendered);
    }
    let refreshed = rewrite_enabled_line(&rendered, existing.enabled)?;
    let definition = parse_routine_yaml(&refreshed).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` failed validation after keeping enabled={}: {error}",
            existing.enabled
        ))
    })?;
    if definition.enabled != existing.enabled {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` did not keep the operator's enabled={} setting",
            existing.enabled
        )));
    }
    Ok(refreshed)
}

/// Recover the materialization binding of an on-disk routine that predates
/// routine provenance, so it can be adopted instead of reported as a
/// collision forever.
///
/// `None` means the file is not one Orbit can manage from this template:
/// either it does not parse as a routine, or its recorded binding could not be
/// re-rendered later. Refusing those keeps a future reconcile from
/// hard-failing the whole workspace sync on a binding Orbit adopted but cannot
/// use.
pub(super) fn adoptable_binding(
    identity: &RoutineSeedIdentity,
    file_stem: &str,
    template: &str,
    existing: &str,
) -> Option<RoutineMaterializationBinding> {
    let definition = parse_routine_yaml(existing).ok()?;
    let binding = identity.complete(file_stem, template, &binding_of(&definition));
    render_routine_template(file_stem, template, &binding).ok()?;
    Some(binding)
}

/// The binding an on-disk document declares: its name, and the owner and
/// branch of its state trigger when it has one.
pub(super) fn binding_of(definition: &RoutineDefinition) -> RoutineMaterializationBinding {
    let state = definition.trigger.state.as_ref();
    RoutineMaterializationBinding {
        name: definition.name.clone(),
        owner_machine: state.map(|trigger| trigger.owner_machine.clone()),
        branch: state.map(|trigger| trigger.branch.clone()),
    }
}

impl RoutineMaterializationBinding {
    /// The values this binding renders, for a drift report.
    pub(super) fn describe(&self) -> String {
        let mut described = format!("name '{}'", self.name);
        if let Some(owner_machine) = &self.owner_machine {
            described.push_str(&format!(", owner '{owner_machine}'"));
        }
        if let Some(branch) = &self.branch {
            described.push_str(&format!(", branch '{branch}'"));
        }
        described
    }
}

/// Render `template` against `binding`, failing closed on any placeholder the
/// binding cannot resolve and on a document that does not reproduce the
/// binding it was rendered from.
pub(super) fn render_routine_template(
    file_stem: &str,
    template: &str,
    binding: &RoutineMaterializationBinding,
) -> Result<String, OrbitError> {
    let mut rendered = template.replace(ROUTINE_NAME_PLACEHOLDER, &binding.name);
    if let Some(owner_machine) = &binding.owner_machine {
        rendered = rendered.replace(OWNER_MACHINE_PLACEHOLDER, owner_machine);
    }
    if let Some(branch) = &binding.branch {
        rendered = rendered.replace(BASE_BRANCH_PLACEHOLDER, branch);
    }
    if let Some(placeholder) = [OWNER_MACHINE_PLACEHOLDER, BASE_BRANCH_PLACEHOLDER]
        .into_iter()
        .find(|placeholder| rendered.contains(placeholder))
    {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` needs `{placeholder}` resolved, which its recorded materialization binding does not carry"
        )));
    }
    let definition = parse_routine_yaml(&rendered).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` failed validation with recorded name '{}': {error}",
            binding.name
        ))
    })?;
    let reproduced = binding_of(&definition);
    let reproduces = reproduced.name == binding.name
        && binding
            .owner_machine
            .as_ref()
            .is_none_or(|owner| reproduced.owner_machine.as_ref() == Some(owner))
        && binding
            .branch
            .as_ref()
            .is_none_or(|branch| reproduced.branch.as_ref() == Some(branch));
    if !reproduces {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` did not reproduce its recorded materialization binding"
        )));
    }
    Ok(rendered)
}

pub(super) fn sanitize_routine_name_part(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out.trim_matches(|ch| ch == '-' || ch == '_').to_string()
}
