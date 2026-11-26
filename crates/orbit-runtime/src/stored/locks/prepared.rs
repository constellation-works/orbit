//! A lock grant prepared up to the point where it judges conflicts.

use orbit_common::OrbitError;

/// How many reservations with a possibly stale owner one grant reconciles.
pub const STALE_OWNER_RECONCILE_LIMIT: usize = 32;

/// A task reservation or hard-lease grant that has validated its request and
/// resolved its surface, and has not yet judged conflicts on it.
///
/// A reservation whose owner run is gone would refuse the grant, and whether
/// a run is gone is a job use case, not a lock mechanism. So the caller
/// reconciles the owners holding [`Self::stale_owner_surface`], then calls
/// [`Self::grant`], which judges conflicts and writes.
#[must_use = "a prepared grant holds nothing until `grant` runs"]
pub struct PreparedGrant<'a, T> {
    stale_owner_surface: Option<Vec<String>>,
    grant: Box<dyn FnOnce() -> Result<T, OrbitError> + 'a>,
}

impl<'a, T: 'a> PreparedGrant<'a, T> {
    pub(super) fn new(
        stale_owner_surface: Option<Vec<String>>,
        grant: impl FnOnce() -> Result<T, OrbitError> + 'a,
    ) -> Self {
        Self {
            stale_owner_surface,
            grant: Box::new(grant),
        }
    }

    /// The files whose reservations must not be held by a stale owner when
    /// the grant is judged; `None` for a grant that judges no reservation.
    pub fn stale_owner_surface(&self) -> Option<&[String]> {
        self.stale_owner_surface.as_deref()
    }

    /// Judge conflicts and write the grant, the refusal or the queue entry.
    pub fn grant(self) -> Result<T, OrbitError> {
        (self.grant)()
    }

    pub(super) fn map<U: 'a>(self, output: impl FnOnce(T) -> U + 'a) -> PreparedGrant<'a, U> {
        let grant = self.grant;
        PreparedGrant {
            stale_owner_surface: self.stale_owner_surface,
            grant: Box::new(move || grant().map(output)),
        }
    }
}
