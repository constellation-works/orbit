//! Visual review budget selection.

use serde::{Deserialize, Serialize};

use super::image::visual_image_tokens;

/// One mock's images as the budget sees them: the mock, its capture and
/// the side-by-side, each with its dimensions when they could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetMock {
    pub mock: String,
    pub mock_dimensions: Option<(u32, u32)>,
    pub capture_dimensions: Option<(u32, u32)>,
    pub side_by_side_dimensions: Option<(u32, u32)>,
}

/// How one shown mock reaches the reviewer (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualShownForm {
    /// Mock and capture as two images.
    Pair,
    /// Only the side-by-side, because both images would not fit.
    SideBySide,
}

/// One mock the review invocation is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualShownMock {
    pub mock: String,
    pub form: VisualShownForm,
    pub images: u32,
    pub estimated_image_tokens: u64,
}

/// One mock the budget kept out of the invocation, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualNotShownMock {
    pub mock: String,
    /// `max_images`, `max_image_tokens` or `unreadable_image`.
    pub reason: String,
}

/// The budget selection for one invocation (§10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetSelection {
    pub shown: Vec<VisualShownMock>,
    pub not_shown: Vec<VisualNotShownMock>,
    /// Images staged, the canary included.
    pub images: u32,
    pub estimated_image_tokens: u64,
}

/// Choose which of `mocks` (already in §10 order: acceptance-criteria
/// mocks first, then artifact name) one invocation is shown, checked
/// before dispatch. Each mock costs two images (mock and capture); the
/// side-by-side is sent only when both would not fit. The canary is
/// first, so one image of `max_images` is reserved for it. What does
/// not fit is listed as not shown, never silently dropped.
#[must_use]
pub fn select_visual_budget(
    mocks: &[VisualBudgetMock],
    provider: &str,
    max_images: u32,
    max_image_tokens: u64,
) -> VisualBudgetSelection {
    let mut selection = VisualBudgetSelection {
        shown: Vec::new(),
        not_shown: Vec::new(),
        images: 1, // the canary
        estimated_image_tokens: 0,
    };
    let fits = |selection: &VisualBudgetSelection, images: u32, tokens: u64| {
        selection.images + images <= max_images
            && selection.estimated_image_tokens + tokens <= max_image_tokens
    };
    for mock in mocks {
        let tokens =
            |dimensions: (u32, u32)| visual_image_tokens(provider, dimensions.0, dimensions.1);
        let pair = mock
            .mock_dimensions
            .zip(mock.capture_dimensions)
            .map(|(mock_dims, capture_dims)| tokens(mock_dims) + tokens(capture_dims));
        let side = mock.side_by_side_dimensions.map(tokens);
        if let Some(pair_tokens) = pair
            && fits(&selection, 2, pair_tokens)
        {
            selection.images += 2;
            selection.estimated_image_tokens += pair_tokens;
            selection.shown.push(VisualShownMock {
                mock: mock.mock.clone(),
                form: VisualShownForm::Pair,
                images: 2,
                estimated_image_tokens: pair_tokens,
            });
            continue;
        }
        if let Some(side_tokens) = side
            && fits(&selection, 1, side_tokens)
        {
            selection.images += 1;
            selection.estimated_image_tokens += side_tokens;
            selection.shown.push(VisualShownMock {
                mock: mock.mock.clone(),
                form: VisualShownForm::SideBySide,
                images: 1,
                estimated_image_tokens: side_tokens,
            });
            continue;
        }
        let reason = if pair.is_none() && side.is_none() {
            "unreadable_image"
        } else if selection.images + 1 > max_images {
            "max_images"
        } else {
            "max_image_tokens"
        };
        selection.not_shown.push(VisualNotShownMock {
            mock: mock.mock.clone(),
            reason: reason.to_string(),
        });
    }
    selection
}
