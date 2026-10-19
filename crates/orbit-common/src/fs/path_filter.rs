//! Relative path filters: globs that admit paths, and `!`-prefixed globs that
//! exclude them.

use crate::OrbitError;

use super::path_glob::RelativePathGlob;

/// A list of [`RelativePathGlob`] patterns in which a pattern written with a
/// leading `!` excludes what it matches. A path passes when some including
/// pattern matches it and no excluding pattern does, whatever their order.
#[derive(Debug, Clone, Default)]
pub struct PathFilter {
    include: Vec<RelativePathGlob>,
    exclude: Vec<RelativePathGlob>,
}

impl PathFilter {
    /// Compile `patterns`, refusing one that is not a confined relative glob.
    /// `key` names the setting the patterns come from in the refusal.
    pub fn new(key: &str, patterns: &[String]) -> Result<Self, OrbitError> {
        let mut filter = Self::default();
        for pattern in patterns {
            let (excluded, glob) = match pattern.strip_prefix('!') {
                Some(glob) => (true, glob),
                None => (false, pattern.as_str()),
            };
            let glob = RelativePathGlob::new(glob)
                .map_err(|error| OrbitError::InvalidInput(format!("{key}: {error}")))?;
            if excluded {
                filter.exclude.push(glob);
            } else {
                filter.include.push(glob);
            }
        }
        Ok(filter)
    }

    /// Whether `path`, slash-separated and relative, passes the filter.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.include.iter().any(|glob| glob.matches(path))
            && !self.exclude.iter().any(|glob| glob.matches(path))
    }

    /// The paths among `paths` that do not pass, in their order.
    #[must_use]
    pub fn outside<'a>(&self, paths: &'a [String]) -> Vec<&'a str> {
        paths
            .iter()
            .map(String::as_str)
            .filter(|path| !self.matches(path))
            .collect()
    }
}
