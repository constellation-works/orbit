//! How an executor gets image files to its model (`spec.image_input`), and
//! the canary contract that proves the model saw them.
//!
//! Images never travel in prompt text. They are staged as files in the run's
//! scratch directory, and the executor's declaration says how they reach the
//! model: its own file-read tool (`native_read`), or argv at launch
//! (`attach_args`, scratch paths only, never content). An executor without
//! the declaration cannot take images, and an activity that asks for them
//! on it is refused before launch.
//!
//! Every image-input invocation is first given a canary: a generated PNG of a
//! random code the agent must return as `result.canary`. A result without the
//! code is refused as [`IMAGE_NOT_SEEN_MARKER`], a retryable failure.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The token `attach_args` replaces with a staged image's path.
pub const IMAGE_PATH_PLACEHOLDER: &str = "{path}";

/// The activity input field that asks for image input. Its value is an object
/// whose `images` lists the staged image paths (possibly none: the canary is
/// always staged first).
pub const IMAGE_INPUT_FIELD: &str = "image_input";

/// The envelope `result` field the agent returns the canary code in.
pub const IMAGE_CANARY_RESULT_FIELD: &str = "canary";

/// The error code of an image-input activity refused before launch: its
/// executor declares no `image_input`, or its crew sets `vision = false`.
pub const IMAGE_INPUT_UNAVAILABLE_ERROR_CODE: &str = "image_input_unavailable";

/// The error code of a result that did not return the canary code.
pub const IMAGE_NOT_SEEN_ERROR_CODE: &str = "image_not_seen";

/// The bracketed marker form of [`IMAGE_NOT_SEEN_ERROR_CODE`].
pub const IMAGE_NOT_SEEN_MARKER: &str = "[image_not_seen]";

/// Consecutive canary misses that hold a crew for visual purposes.
pub const VISUAL_HOLD_CONSECUTIVE_MISSES: usize = 2;

/// How an executor delivers staged images to its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageInputMode {
    /// The CLI's own file-read tool presents image files to the model; the
    /// prompt lists the staged paths and asks the agent to read each one.
    NativeRead,
    /// The images are attached at launch through argv, once per image.
    AttachArgs,
}

impl ImageInputMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeRead => "native_read",
            Self::AttachArgs => "attach_args",
        }
    }
}

impl fmt::Display for ImageInputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An image file format an executor declares it can deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageFormat {
    /// The format a staged file's extension names, if any.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Gif => "gif",
        }
    }
}

/// `spec.image_input` on an executor definition, validated when it loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawExecutorImageInput")]
pub struct ExecutorImageInput {
    pub mode: ImageInputMode,
    /// The formats this lane delivers. Always includes `png`, the canary's.
    pub formats: Vec<ImageFormat>,
    /// Most images one invocation may receive, canary included.
    pub max_images: u32,
    /// `attach_args` only: the argv appended once per image, with
    /// [`IMAGE_PATH_PLACEHOLDER`] standing for the staged file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_args: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExecutorImageInput {
    mode: ImageInputMode,
    formats: Vec<ImageFormat>,
    max_images: u32,
    #[serde(default)]
    attach_args: Option<Vec<String>>,
}

impl TryFrom<RawExecutorImageInput> for ExecutorImageInput {
    type Error = String;

    fn try_from(raw: RawExecutorImageInput) -> Result<Self, Self::Error> {
        let invalid = |reason: &str| format!("invalid spec.image_input: {reason}");
        if raw.formats.is_empty() {
            return Err(invalid(
                "formats must list at least one of png, jpeg, webp, gif",
            ));
        }
        let mut seen = BTreeSet::new();
        for format in &raw.formats {
            if !seen.insert(*format) {
                return Err(invalid(&format!(
                    "formats lists `{}` twice",
                    format.as_str()
                )));
            }
        }
        if !seen.contains(&ImageFormat::Png) {
            return Err(invalid(
                "formats must include png, the format of the canary every image-input run receives",
            ));
        }
        if raw.max_images < 2 {
            return Err(invalid(
                "max_images must be at least 2: the canary plus one image",
            ));
        }
        match (raw.mode, raw.attach_args.as_deref()) {
            (ImageInputMode::NativeRead, Some(_)) => {
                return Err(invalid(
                    "attach_args applies only to mode attach_args; remove it for native_read",
                ));
            }
            (ImageInputMode::AttachArgs, None) => {
                return Err(invalid("mode attach_args requires attach_args"));
            }
            (ImageInputMode::AttachArgs, Some(args)) => {
                if args.iter().any(|arg| arg.trim().is_empty()) {
                    return Err(invalid("attach_args must not contain an empty argument"));
                }
                if !args.iter().any(|arg| arg.contains(IMAGE_PATH_PLACEHOLDER)) {
                    return Err(invalid(&format!(
                        "attach_args must contain `{IMAGE_PATH_PLACEHOLDER}`, the staged image's path"
                    )));
                }
            }
            (ImageInputMode::NativeRead, None) => {}
        }
        Ok(Self {
            mode: raw.mode,
            formats: raw.formats,
            max_images: raw.max_images,
            attach_args: raw.attach_args,
        })
    }
}

impl ExecutorImageInput {
    /// Whether this lane delivers `format`.
    #[must_use]
    pub fn accepts(&self, format: ImageFormat) -> bool {
        self.formats.contains(&format)
    }

    /// The argv that attaches `paths` at launch, in order: empty for
    /// `native_read`.
    #[must_use]
    pub fn attach_argv(&self, paths: &[String]) -> Vec<String> {
        let Some(template) = self.attach_args.as_deref() else {
            return Vec::new();
        };
        paths
            .iter()
            .flat_map(|path| {
                template
                    .iter()
                    .map(move |arg| arg.replace(IMAGE_PATH_PLACEHOLDER, path))
            })
            .collect()
    }
}

/// The image paths an activity input asks to stage, when it asks for image
/// input at all ([`IMAGE_INPUT_FIELD`] present). `Some(vec![])` asks for the
/// canary alone.
#[must_use]
pub fn requested_images(input: &Value) -> Option<Vec<String>> {
    let request = input.get(IMAGE_INPUT_FIELD)?;
    if request.is_null() {
        return None;
    }
    Some(
        request
            .get("images")
            .and_then(Value::as_array)
            .map(|images| {
                images
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Whether a step failure says its result did not return the canary.
#[must_use]
pub fn is_image_not_seen(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(IMAGE_NOT_SEEN_ERROR_CODE)
        || message.is_some_and(|message| message.contains(IMAGE_NOT_SEEN_MARKER))
}
