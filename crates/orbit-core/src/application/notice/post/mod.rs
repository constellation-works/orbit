//! `orbit.notice.post`: validate the request, then decide created, updated
//! or corroborated inside the store's posting transaction, where the
//! one-open-notice-per-key rule, the open counts and the post rates are
//! checked against the same snapshot the write applies to.

mod posting;
mod request;
mod transaction;

pub use posting::NoticePostResult;
pub use request::{NoticePostRawFields, NoticePostRequest};
