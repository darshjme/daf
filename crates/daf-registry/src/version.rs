//! Semantic versioning: parsing, comparison, and constraint satisfaction.
//!
//! Implements a subset of the [SemVer 2.0.0](https://semver.org/) specification
//! with constraint operators (`>=`, `^`, `~`, ranges) used for capability
//! matching. This is the foundation of DAF's compatibility engine — when an
//! orchestrator asks "who can do X at version Y?", this module answers.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A parsed semantic version: `MAJOR.MINOR.PATCH[-pre][+build]`.
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (e.g. `alpha.1`). Empty means a release version.
    pub pre_release: Option<String>,
    /// Build metadata (e.g. `20260407`). Ignored during comparison per spec.
    pub build_metadata: Option<String>,
}

impl SemVer {
    /// Create a release version with no pre-release or build metadata.
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre_release: None,
            build_metadata: None,
        }
    }

    /// Attach a pre-release tag.
    pub fn with_pre(mut self, pre: impl Into<String>) -> Self {
        self.pre_release = Some(pre.into());
        self
    }

    /// Attach build metadata.
    pub fn with_build(mut self, build: impl Into<String>) -> Self {
        self.build_metadata = Some(build.into());
        self
    }

    /// Returns `true` if this is a pre-release version.
    pub fn is_prerelease(&self) -> bool {
        self.pre_release.is_some()
    }

    /// Returns `true` if the major version is zero (initial development).
    pub fn is_initial_development(&self) -> bool {
        self.major == 0
    }

    /// Check whether `self` is API-compatible with `other` using caret semantics.
    ///
    /// Caret compatibility (`^`): same major for `>=1.0.0`, same major.minor for
    /// `0.x` versions.
    pub fn is_compatible_with(&self, other: &SemVer) -> bool {
        if self.major != other.major {
            return false;
        }
        if self.major == 0 {
            // In 0.x, minor is treated as breaking.
            return self.minor == other.minor;
        }
        true
    }

    /// Numeric tuple for ordering (ignoring pre-release and build).
    fn numeric_tuple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl PartialEq for SemVer {
    fn eq(&self, other: &Self) -> bool {
        // Build metadata is ignored per SemVer spec.
        self.major == other.major
            && self.minor == other.minor
            && self.patch == other.patch
            && self.pre_release == other.pre_release
    }
}

impl std::hash::Hash for SemVer {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.major.hash(state);
        self.minor.hash(state);
        self.patch.hash(state);
        self.pre_release.hash(state);
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.numeric_tuple().cmp(&other.numeric_tuple()) {
            Ordering::Equal => {}
            ord => return ord,
        }
        // Pre-release versions have *lower* precedence than the release.
        match (&self.pre_release, &other.pre_release) {
            (None, None) => Ordering::Equal,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(ref pre) = self.pre_release {
            write!(f, "-{pre}")?;
        }
        if let Some(ref build) = self.build_metadata {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

impl FromStr for SemVer {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Split off build metadata first (after `+`).
        let (version_pre, build_metadata) = match s.rsplit_once('+') {
            Some((vp, bm)) => (vp, Some(bm.to_string())),
            None => (s, None),
        };

        // Split off pre-release (after `-`).
        let (version, pre_release) = match version_pre.split_once('-') {
            Some((v, pr)) => (v, Some(pr.to_string())),
            None => (version_pre, None),
        };

        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(VersionParseError(format!(
                "expected MAJOR.MINOR[.PATCH], got '{s}'"
            )));
        }

        let major = parts[0]
            .parse::<u64>()
            .map_err(|_| VersionParseError(format!("invalid major: '{}'", parts[0])))?;
        let minor = parts[1]
            .parse::<u64>()
            .map_err(|_| VersionParseError(format!("invalid minor: '{}'", parts[1])))?;
        let patch = if parts.len() == 3 {
            parts[2]
                .parse::<u64>()
                .map_err(|_| VersionParseError(format!("invalid patch: '{}'", parts[2])))?
        } else {
            0
        };

        Ok(SemVer {
            major,
            minor,
            patch,
            pre_release,
            build_metadata,
        })
    }
}

/// Error returned when a version string cannot be parsed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("version parse error: {0}")]
pub struct VersionParseError(pub String);

// ---------------------------------------------------------------------------
// VersionConstraint
// ---------------------------------------------------------------------------

/// A constraint on acceptable versions.
///
/// Used in capability requirements to express "I need version X of capability Y
/// where X satisfies this constraint."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionConstraint {
    /// Matches exactly this version.
    Exact(SemVer),
    /// Matches versions `>= v`.
    Gte(SemVer),
    /// Matches versions `< v`.
    Lt(SemVer),
    /// Caret: `^1.2.3` means `>=1.2.3, <2.0.0`; `^0.2.3` means `>=0.2.3, <0.3.0`.
    Caret(SemVer),
    /// Tilde: `~1.2.3` means `>=1.2.3, <1.3.0`.
    Tilde(SemVer),
    /// Closed range: `>=low, <high`.
    Range { low: SemVer, high: SemVer },
    /// Matches any version.
    Any,
}

impl VersionConstraint {
    /// Check whether the given version satisfies this constraint.
    pub fn satisfies(&self, v: &SemVer) -> bool {
        match self {
            Self::Exact(target) => v == target,

            Self::Gte(min) => v >= min,

            Self::Lt(max) => v < max,

            Self::Caret(base) => {
                if v < base {
                    return false;
                }
                if base.major == 0 {
                    if base.minor == 0 {
                        // ^0.0.z: only exact patch
                        v.major == 0 && v.minor == 0 && v.patch == base.patch
                    } else {
                        // ^0.y.z: same minor
                        v.major == 0 && v.minor == base.minor
                    }
                } else {
                    // ^x.y.z: same major
                    v.major == base.major
                }
            }

            Self::Tilde(base) => {
                if v < base {
                    return false;
                }
                v.major == base.major && v.minor == base.minor
            }

            Self::Range { low, high } => v >= low && v < high,

            Self::Any => true,
        }
    }

    /// Parse a constraint from a string like `"^1.2.0"`, `">=2.0"`, `"~0.3"`,
    /// `"=1.0.0"`, `"*"`.
    pub fn parse(s: &str) -> Result<Self, VersionParseError> {
        let s = s.trim();

        if s == "*" || s.is_empty() {
            return Ok(Self::Any);
        }

        if let Some(rest) = s.strip_prefix("^") {
            return Ok(Self::Caret(rest.parse()?));
        }
        if let Some(rest) = s.strip_prefix("~") {
            return Ok(Self::Tilde(rest.parse()?));
        }
        if let Some(rest) = s.strip_prefix(">=") {
            return Ok(Self::Gte(rest.trim().parse()?));
        }
        if let Some(rest) = s.strip_prefix("<") {
            return Ok(Self::Lt(rest.trim().parse()?));
        }
        if let Some(rest) = s.strip_prefix('=') {
            return Ok(Self::Exact(rest.trim().parse()?));
        }

        // Bare version string → treat as exact.
        Ok(Self::Exact(s.parse()?))
    }
}

impl fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(v) => write!(f, "={v}"),
            Self::Gte(v) => write!(f, ">={v}"),
            Self::Lt(v) => write!(f, "<{v}"),
            Self::Caret(v) => write!(f, "^{v}"),
            Self::Tilde(v) => write!(f, "~{v}"),
            Self::Range { low, high } => write!(f, ">={low}, <{high}"),
            Self::Any => write!(f, "*"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_version() {
        let v: SemVer = "1.2.3".parse().unwrap();
        assert_eq!(v, SemVer::new(1, 2, 3));
    }

    #[test]
    fn parse_with_pre_and_build() {
        let v: SemVer = "1.0.0-alpha.1+build.42".parse().unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.pre_release.as_deref(), Some("alpha.1"));
        assert_eq!(v.build_metadata.as_deref(), Some("build.42"));
    }

    #[test]
    fn parse_two_part() {
        let v: SemVer = "2.5".parse().unwrap();
        assert_eq!(v, SemVer::new(2, 5, 0));
    }

    #[test]
    fn ordering() {
        let a: SemVer = "1.0.0".parse().unwrap();
        let b: SemVer = "1.0.1".parse().unwrap();
        let c: SemVer = "1.1.0".parse().unwrap();
        let d: SemVer = "2.0.0".parse().unwrap();
        assert!(a < b);
        assert!(b < c);
        assert!(c < d);
    }

    #[test]
    fn prerelease_lower_than_release() {
        let pre: SemVer = "1.0.0-alpha".parse().unwrap();
        let rel: SemVer = "1.0.0".parse().unwrap();
        assert!(pre < rel);
    }

    #[test]
    fn display_roundtrip() {
        let v = SemVer::new(3, 2, 1)
            .with_pre("rc.1")
            .with_build("sha.abc123");
        assert_eq!(v.to_string(), "3.2.1-rc.1+sha.abc123");
    }

    #[test]
    fn constraint_exact() {
        let c = VersionConstraint::parse("=1.2.3").unwrap();
        assert!(c.satisfies(&"1.2.3".parse().unwrap()));
        assert!(!c.satisfies(&"1.2.4".parse().unwrap()));
    }

    #[test]
    fn constraint_gte() {
        let c = VersionConstraint::parse(">=1.0.0").unwrap();
        assert!(c.satisfies(&"1.0.0".parse().unwrap()));
        assert!(c.satisfies(&"2.0.0".parse().unwrap()));
        assert!(!c.satisfies(&"0.9.9".parse().unwrap()));
    }

    #[test]
    fn constraint_lt() {
        let c = VersionConstraint::parse("<2.0.0").unwrap();
        assert!(c.satisfies(&"1.9.9".parse().unwrap()));
        assert!(!c.satisfies(&"2.0.0".parse().unwrap()));
    }

    #[test]
    fn constraint_caret_major() {
        let c = VersionConstraint::parse("^1.2.3").unwrap();
        assert!(c.satisfies(&"1.2.3".parse().unwrap()));
        assert!(c.satisfies(&"1.9.0".parse().unwrap()));
        assert!(!c.satisfies(&"2.0.0".parse().unwrap()));
        assert!(!c.satisfies(&"1.2.2".parse().unwrap()));
    }

    #[test]
    fn constraint_caret_zero_minor() {
        let c = VersionConstraint::parse("^0.2.0").unwrap();
        assert!(c.satisfies(&"0.2.0".parse().unwrap()));
        assert!(c.satisfies(&"0.2.9".parse().unwrap()));
        assert!(!c.satisfies(&"0.3.0".parse().unwrap()));
    }

    #[test]
    fn constraint_tilde() {
        let c = VersionConstraint::parse("~1.2.0").unwrap();
        assert!(c.satisfies(&"1.2.0".parse().unwrap()));
        assert!(c.satisfies(&"1.2.9".parse().unwrap()));
        assert!(!c.satisfies(&"1.3.0".parse().unwrap()));
    }

    #[test]
    fn constraint_any() {
        let c = VersionConstraint::parse("*").unwrap();
        assert!(c.satisfies(&"0.0.1".parse().unwrap()));
        assert!(c.satisfies(&"99.99.99".parse().unwrap()));
    }

    #[test]
    fn constraint_range() {
        let c = VersionConstraint::Range {
            low: "1.0.0".parse().unwrap(),
            high: "2.0.0".parse().unwrap(),
        };
        assert!(c.satisfies(&"1.5.0".parse().unwrap()));
        assert!(!c.satisfies(&"0.9.0".parse().unwrap()));
        assert!(!c.satisfies(&"2.0.0".parse().unwrap()));
    }

    #[test]
    fn compatibility_check() {
        let a: SemVer = "1.2.0".parse().unwrap();
        let b: SemVer = "1.5.0".parse().unwrap();
        let c: SemVer = "2.0.0".parse().unwrap();
        assert!(a.is_compatible_with(&b));
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn zero_major_compatibility() {
        let a: SemVer = "0.2.0".parse().unwrap();
        let b: SemVer = "0.2.5".parse().unwrap();
        let c: SemVer = "0.3.0".parse().unwrap();
        assert!(a.is_compatible_with(&b));
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn constraint_display() {
        assert_eq!(VersionConstraint::parse("^1.2.0").unwrap().to_string(), "^1.2.0");
        assert_eq!(VersionConstraint::parse(">=2.0").unwrap().to_string(), ">=2.0.0");
        assert_eq!(VersionConstraint::parse("*").unwrap().to_string(), "*");
    }
}
