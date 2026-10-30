//! Image input for a CLI provider run (`spec.image_input`).
//!
//! An activity input that carries `image_input` asks for images. Before any
//! session starts, the run is admitted against its executor's declaration and
//! its crew's `vision` setting, and refused with
//! [`DispatchError::ImageInputUnavailable`] when either rules images out.
//! At launch a fresh canary is rendered and staged first, beside copies of
//! the requested images, in a private directory under the run's scratch
//! directory; only those scratch paths reach the prompt or argv. After the
//! provider finishes, a result that does not return the canary code fails as
//! `image_not_seen`, a retryable failure, and the check is audited so two
//! misses in a row hold the crew for visual purposes.

mod canary;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use orbit_types::workflow::activity_job::{AgentLoopSpec, V2AuditEventKind};
use orbit_types::workflow::{
    ExecutorImageInput, IMAGE_CANARY_RESULT_FIELD, IMAGE_NOT_SEEN_MARKER, ImageFormat,
    ImageInputMode, requested_images,
};
use serde_json::Value;

use super::super::audit_writer::V2AuditWriter;
use super::super::dispatcher::{DispatchError, DispatchOutcome};

/// An admitted image-input request: the lane's declaration and the images
/// to stage after the canary.
pub(super) struct ImageInputPlan {
    declaration: ExecutorImageInput,
    images: Vec<PathBuf>,
}

/// Admit an image-input run before executor work begins. `None` when the
/// input asks for no images.
pub(super) fn admit_image_input(
    provider: &str,
    spec: &AgentLoopSpec,
    input: &Value,
    declared: Option<&ExecutorImageInput>,
) -> Result<Option<ImageInputPlan>, DispatchError> {
    let Some(images) = requested_images(input) else {
        return Ok(None);
    };
    let crew = input.get("crew").and_then(Value::as_str).unwrap_or("");
    if spec.crew_vision_disabled {
        return Err(unavailable(
            provider,
            &format!(
                "crew `{crew}` sets `vision = false` in [crews.{crew}], so it is never given \
                 images; select a crew that may see them"
            ),
        ));
    }
    let declaration = declared.ok_or_else(|| {
        unavailable(
            provider,
            "it declares no `spec.image_input`, so its crews cannot take images; declare one \
             on the executor once its canary probe passes, or select another crew",
        )
    })?;
    let staged = images.len().saturating_add(1);
    if staged > declaration.max_images as usize {
        return Err(unavailable(
            provider,
            &format!(
                "{staged} images (the canary included) exceed its `spec.image_input.max_images` \
                 of {}",
                declaration.max_images
            ),
        ));
    }
    let mut paths = Vec::with_capacity(images.len());
    for image in images {
        let path = PathBuf::from(&image);
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(ImageFormat::from_extension);
        if !format.is_some_and(|format| declaration.accepts(format)) {
            return Err(unavailable(
                provider,
                &format!(
                    "image `{image}` is not one of its `spec.image_input.formats` ({})",
                    declaration
                        .formats
                        .iter()
                        .map(|format| format.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        paths.push(path);
    }
    Ok(Some(ImageInputPlan {
        declaration: declaration.clone(),
        images: paths,
    }))
}

fn unavailable(executor: &str, reason: &str) -> DispatchError {
    DispatchError::ImageInputUnavailable {
        executor: executor.to_string(),
        reason: reason.to_string(),
    }
}

/// The staged images of one invocation. Dropping it removes them.
pub(super) struct StagedImages {
    _directory: tempfile::TempDir,
    mode: ImageInputMode,
    code: String,
    paths: Vec<String>,
    /// argv appended at launch (`attach_args` only).
    pub(super) args: Vec<String>,
    /// The prompt section that tells the agent where the images are.
    pub(super) instruction: String,
}

impl ImageInputPlan {
    /// Render the canary and stage it, then a copy of each requested image
    /// relative to `cwd`, in a new private directory under `scratch`.
    pub(super) fn stage(
        &self,
        scratch: &Path,
        cwd: Option<&Path>,
    ) -> Result<StagedImages, DispatchError> {
        let permanent = |message: String| DispatchError::CliInvocationPermanent(message);
        let directory = tempfile::Builder::new()
            .prefix("images-")
            .tempdir_in(scratch)
            .map_err(|error| permanent(format!("create image staging directory: {error}")))?;
        let code = canary::random_code()
            .ok_or_else(|| permanent("no randomness source for the image canary".to_string()))?;
        let canary_path = directory.path().join("01-canary.png");
        std::fs::File::create(&canary_path)
            .and_then(|mut file| file.write_all(&canary::render_png(&code)))
            .map_err(|error| permanent(format!("write the image canary: {error}")))?;
        let mut paths = vec![canary_path.display().to_string()];
        for (index, image) in self.images.iter().enumerate() {
            let source = match cwd {
                Some(cwd) if image.is_relative() => cwd.join(image),
                _ => image.clone(),
            };
            let name = source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target = directory.path().join(format!("{:02}-{name}", index + 2));
            std::fs::copy(&source, &target).map_err(|error| {
                permanent(format!("stage image `{}`: {error}", image.display()))
            })?;
            paths.push(target.display().to_string());
        }
        Ok(StagedImages {
            mode: self.declaration.mode,
            args: self.declaration.attach_argv(&paths),
            instruction: instruction(self.declaration.mode, &paths),
            _directory: directory,
            code,
            paths,
        })
    }
}

fn instruction(mode: ImageInputMode, paths: &[String]) -> String {
    let delivery = match mode {
        ImageInputMode::NativeRead => {
            "Open each file below with your own file-read tool, which presents image files to \
             you as images, before you answer. Do not describe an image you did not open."
        }
        ImageInputMode::AttachArgs => {
            "They are attached to this prompt as images, in the order below."
        }
    };
    let mut text = format!(
        "Image input: {} image file(s) are staged for this run. {delivery}\n\nThe first image \
         is a canary showing a six-character code. Return that code, exactly as shown, as \
         `result.{IMAGE_CANARY_RESULT_FIELD}` in your response envelope. If you cannot see the \
         image, return `\"{IMAGE_CANARY_RESULT_FIELD}\": null`; never guess.\n",
        paths.len()
    );
    for (index, path) in paths.iter().enumerate() {
        let label = if index == 0 { " (canary)" } else { "" };
        text.push_str(&format!("\n{}. {path}{label}", index + 1));
    }
    text
}

impl StagedImages {
    /// Refuse a finished turn whose result does not return the canary code,
    /// and audit the check. A run that already failed is left as it is: its
    /// failure is not a canary miss.
    pub(super) fn verify(
        &self,
        outcome: &mut DispatchOutcome,
        audit: &V2AuditWriter,
        step_id: &str,
        activity_name: &str,
        provider: &str,
        crew: Option<&str>,
    ) {
        if !outcome.success {
            return;
        }
        let returned = outcome
            .output
            .get(IMAGE_CANARY_RESULT_FIELD)
            .and_then(Value::as_str)
            .map(|code| code.trim().to_string());
        let seen = returned
            .as_deref()
            .is_some_and(|code| code.eq_ignore_ascii_case(&self.code));
        audit.emit_lossy(V2AuditEventKind::ImageCanaryChecked {
            step_id: step_id.to_string(),
            activity_name: activity_name.to_string(),
            provider: provider.to_string(),
            crew: crew.map(ToOwned::to_owned),
            mode: self.mode,
            seen,
        });
        if let Some(object) = outcome.output.as_object_mut() {
            object.insert(
                "image_canary".to_string(),
                serde_json::json!({
                    "seen": seen,
                    "mode": self.mode,
                    "images": self.paths.len(),
                }),
            );
        }
        if !seen {
            outcome.success = false;
            outcome.message = Some(format!(
                "{IMAGE_NOT_SEEN_MARKER} provider `{provider}` finished without returning the \
                 canary code in `result.{IMAGE_CANARY_RESULT_FIELD}` ({}); the model may not \
                 have seen the staged images",
                match returned.as_deref() {
                    Some(code) if !code.is_empty() => "it returned a different code",
                    _ => "it returned none",
                }
            ));
        }
    }
}
