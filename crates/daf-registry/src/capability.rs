//! Capability matching engine.
//!
//! Agents advertise [`Capability`] descriptors; task requirements express
//! [`CapabilityRequirement`] constraints. This module bridges the two with
//! semver-aware matching, set operations on capability collections, and a
//! best-match scoring algorithm that ranks agents by how well they satisfy
//! a set of requirements.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::version::{SemVer, VersionConstraint};

// ---------------------------------------------------------------------------
// Capability
// ---------------------------------------------------------------------------

/// A concrete capability that an agent provides.
///
/// This is the registry-level capability with full metadata — richer than
/// `daf_core::AgentCapability` which is the wire-level descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Machine-readable name, e.g. `"code_review"`, `"security_scan"`.
    pub name: String,
    /// Semantic version of this capability implementation.
    pub version: SemVer,
    /// Human-readable description.
    pub description: String,
    /// Capability-specific parameters (input schema, config knobs, etc.).
    pub parameters: Value,
}

impl Capability {
    /// Create a new capability with no parameters.
    pub fn new(
        name: impl Into<String>,
        version: SemVer,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version,
            description: description.into(),
            parameters: Value::Null,
        }
    }

    /// Attach parameters.
    pub fn with_parameters(mut self, params: Value) -> Self {
        self.parameters = params;
        self
    }

    /// Check whether this capability satisfies the given requirement.
    pub fn matches(&self, req: &CapabilityRequirement) -> bool {
        if self.name != req.name {
            return false;
        }
        req.version_constraint.satisfies(&self.version)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

impl PartialEq for Capability {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.version == other.version
    }
}

impl Eq for Capability {}

impl std::hash::Hash for Capability {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.version.hash(state);
    }
}

// ---------------------------------------------------------------------------
// CapabilityRequirement
// ---------------------------------------------------------------------------

/// A constraint on what capability an agent must provide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    /// The capability name to match against.
    pub name: String,
    /// Version constraint (semver).
    pub version_constraint: VersionConstraint,
    /// If `true`, the agent *must* have this capability to be considered.
    /// If `false`, having it is a bonus that improves the match score.
    pub required: bool,
}

impl CapabilityRequirement {
    /// Create a required capability requirement.
    pub fn required(name: impl Into<String>, constraint: VersionConstraint) -> Self {
        Self {
            name: name.into(),
            version_constraint: constraint,
            required: true,
        }
    }

    /// Create an optional (preferred) capability requirement.
    pub fn optional(name: impl Into<String>, constraint: VersionConstraint) -> Self {
        Self {
            name: name.into(),
            version_constraint: constraint,
            required: false,
        }
    }
}

impl fmt::Display for CapabilityRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = if self.required { "required" } else { "optional" };
        write!(f, "{}({} {})", tag, self.name, self.version_constraint)
    }
}

// ---------------------------------------------------------------------------
// CapabilitySet
// ---------------------------------------------------------------------------

/// An indexed collection of capabilities with set operations.
///
/// Internally indexes capabilities by name for O(1) lookup during matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Capabilities indexed by name. Multiple versions of the same capability
    /// are supported (the vec stores each version).
    capabilities: HashMap<String, Vec<Capability>>,
}

impl CapabilitySet {
    /// Create an empty capability set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a set from a list of capabilities.
    pub fn from_capabilities(caps: impl IntoIterator<Item = Capability>) -> Self {
        let mut set = Self::new();
        for cap in caps {
            set.insert(cap);
        }
        set
    }

    /// Insert a capability. If the same name+version already exists, it is
    /// replaced.
    pub fn insert(&mut self, cap: Capability) {
        let entry = self.capabilities.entry(cap.name.clone()).or_default();
        // Replace if same version exists.
        if let Some(existing) = entry.iter_mut().find(|c| c.version == cap.version) {
            *existing = cap;
        } else {
            entry.push(cap);
        }
    }

    /// Remove all versions of a capability by name.
    pub fn remove(&mut self, name: &str) -> Vec<Capability> {
        self.capabilities.remove(name).unwrap_or_default()
    }

    /// Look up capabilities by name (all versions).
    pub fn get(&self, name: &str) -> &[Capability] {
        self.capabilities
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check if the set contains at least one version of the named capability.
    pub fn contains(&self, name: &str) -> bool {
        self.capabilities.contains_key(name)
    }

    /// Total number of distinct (name, version) pairs.
    pub fn len(&self) -> usize {
        self.capabilities.values().map(|v| v.len()).sum()
    }

    /// Returns `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// Iterate over all capabilities (flat).
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.values().flat_map(|v| v.iter())
    }

    /// All distinct capability names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.capabilities.keys().map(|s| s.as_str())
    }

    /// Find the best-matching capability for a requirement (highest version
    /// that satisfies the constraint).
    pub fn find_match(&self, req: &CapabilityRequirement) -> Option<&Capability> {
        self.capabilities
            .get(&req.name)?
            .iter()
            .filter(|cap| req.version_constraint.satisfies(&cap.version))
            .max_by(|a, b| a.version.cmp(&b.version))
    }

    /// Intersection: capabilities that appear in both sets (by name).
    /// When both sets have the same capability, the version from `self` is used.
    pub fn intersection(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut result = CapabilitySet::new();
        for (name, caps) in &self.capabilities {
            if other.contains(name) {
                for cap in caps {
                    result.insert(cap.clone());
                }
            }
        }
        result
    }

    /// Union: capabilities from both sets. On conflict (same name+version),
    /// `self` wins.
    pub fn union(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut result = other.clone();
        for cap in self.iter() {
            result.insert(cap.clone());
        }
        result
    }

    /// Difference: capabilities in `self` but not in `other` (by name).
    pub fn difference(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut result = CapabilitySet::new();
        for (name, caps) in &self.capabilities {
            if !other.contains(name) {
                for cap in caps {
                    result.insert(cap.clone());
                }
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Match scoring
// ---------------------------------------------------------------------------

/// Result of scoring an agent against a set of requirements.
#[derive(Debug, Clone)]
pub struct MatchScore {
    /// The agent this score belongs to (opaque, caller interprets).
    pub agent_index: usize,
    /// Fraction of *required* requirements satisfied (0.0..=1.0).
    pub required_score: f64,
    /// Fraction of *optional* requirements satisfied (0.0..=1.0).
    pub optional_score: f64,
    /// Combined weighted score. Required requirements are weighted 3x.
    pub total_score: f64,
    /// Names of unmet required capabilities.
    pub missing_required: Vec<String>,
    /// Names of unmet optional capabilities.
    pub missing_optional: Vec<String>,
}

impl MatchScore {
    /// Returns `true` if all required capabilities are met.
    pub fn all_required_met(&self) -> bool {
        self.missing_required.is_empty()
    }
}

/// Score a single capability set against a list of requirements.
pub fn score_capabilities(
    caps: &CapabilitySet,
    requirements: &[CapabilityRequirement],
) -> MatchScore {
    let mut required_met = 0usize;
    let mut required_total = 0usize;
    let mut optional_met = 0usize;
    let mut optional_total = 0usize;
    let mut missing_required = Vec::new();
    let mut missing_optional = Vec::new();

    for req in requirements {
        let matched = caps.find_match(req).is_some();
        if req.required {
            required_total += 1;
            if matched {
                required_met += 1;
            } else {
                missing_required.push(req.name.clone());
            }
        } else {
            optional_total += 1;
            if matched {
                optional_met += 1;
            } else {
                missing_optional.push(req.name.clone());
            }
        }
    }

    let required_score = if required_total > 0 {
        required_met as f64 / required_total as f64
    } else {
        1.0
    };
    let optional_score = if optional_total > 0 {
        optional_met as f64 / optional_total as f64
    } else {
        0.0
    };
    // Required capabilities are weighted 3x.
    let total_score = (required_score * 3.0 + optional_score) / 4.0;

    MatchScore {
        agent_index: 0,
        required_score,
        optional_score,
        total_score,
        missing_required,
        missing_optional,
    }
}

/// Find the best match among multiple capability sets.
///
/// Returns the index and score of the agent whose capabilities best satisfy
/// the requirements. Only agents that meet *all* required capabilities are
/// considered. Among those, the one with the highest total score wins.
pub fn best_match(
    capability_sets: &[CapabilitySet],
    requirements: &[CapabilityRequirement],
) -> Option<MatchScore> {
    let mut best: Option<MatchScore> = None;

    for (idx, caps) in capability_sets.iter().enumerate() {
        let mut score = score_capabilities(caps, requirements);
        score.agent_index = idx;

        if !score.all_required_met() {
            continue;
        }

        let dominated = best
            .as_ref()
            .map(|b| score.total_score > b.total_score)
            .unwrap_or(true);
        if dominated {
            best = Some(score);
        }
    }

    best
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::SemVer;

    fn cap(name: &str, ver: &str) -> Capability {
        Capability::new(name, ver.parse::<SemVer>().unwrap(), "")
    }

    #[test]
    fn capability_matches_requirement() {
        let c = cap("lint", "1.2.0");
        let req = CapabilityRequirement::required("lint", VersionConstraint::parse("^1.0.0").unwrap());
        assert!(c.matches(&req));
    }

    #[test]
    fn capability_does_not_match_wrong_name() {
        let c = cap("lint", "1.2.0");
        let req = CapabilityRequirement::required("deploy", VersionConstraint::parse("^1.0.0").unwrap());
        assert!(!c.matches(&req));
    }

    #[test]
    fn capability_does_not_match_wrong_version() {
        let c = cap("lint", "0.9.0");
        let req = CapabilityRequirement::required("lint", VersionConstraint::parse(">=1.0.0").unwrap());
        assert!(!c.matches(&req));
    }

    #[test]
    fn capability_set_insert_and_get() {
        let mut set = CapabilitySet::new();
        set.insert(cap("lint", "1.0.0"));
        set.insert(cap("lint", "1.1.0"));
        set.insert(cap("deploy", "2.0.0"));

        assert_eq!(set.len(), 3);
        assert_eq!(set.get("lint").len(), 2);
        assert!(set.contains("lint"));
        assert!(!set.contains("build"));
    }

    #[test]
    fn capability_set_intersection() {
        let a = CapabilitySet::from_capabilities(vec![
            cap("lint", "1.0.0"),
            cap("deploy", "1.0.0"),
        ]);
        let b = CapabilitySet::from_capabilities(vec![
            cap("lint", "2.0.0"),
            cap("test", "1.0.0"),
        ]);

        let inter = a.intersection(&b);
        assert_eq!(inter.len(), 1);
        assert!(inter.contains("lint"));
        assert!(!inter.contains("deploy"));
    }

    #[test]
    fn capability_set_union() {
        let a = CapabilitySet::from_capabilities(vec![cap("lint", "1.0.0")]);
        let b = CapabilitySet::from_capabilities(vec![cap("deploy", "1.0.0")]);

        let u = a.union(&b);
        assert!(u.contains("lint"));
        assert!(u.contains("deploy"));
    }

    #[test]
    fn capability_set_difference() {
        let a = CapabilitySet::from_capabilities(vec![
            cap("lint", "1.0.0"),
            cap("deploy", "1.0.0"),
        ]);
        let b = CapabilitySet::from_capabilities(vec![cap("lint", "1.0.0")]);

        let diff = a.difference(&b);
        assert!(!diff.contains("lint"));
        assert!(diff.contains("deploy"));
    }

    #[test]
    fn capability_set_find_match_highest_version() {
        let set = CapabilitySet::from_capabilities(vec![
            cap("lint", "1.0.0"),
            cap("lint", "1.5.0"),
            cap("lint", "1.3.0"),
        ]);
        let req = CapabilityRequirement::required("lint", VersionConstraint::parse("^1.0.0").unwrap());
        let m = set.find_match(&req).unwrap();
        assert_eq!(m.version, "1.5.0".parse::<SemVer>().unwrap());
    }

    #[test]
    fn score_all_required_met() {
        let caps = CapabilitySet::from_capabilities(vec![
            cap("lint", "1.0.0"),
            cap("test", "2.0.0"),
        ]);
        let reqs = vec![
            CapabilityRequirement::required("lint", VersionConstraint::parse("^1.0.0").unwrap()),
            CapabilityRequirement::required("test", VersionConstraint::parse(">=1.0.0").unwrap()),
        ];
        let score = score_capabilities(&caps, &reqs);
        assert!(score.all_required_met());
        assert!((score.required_score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn score_missing_required() {
        let caps = CapabilitySet::from_capabilities(vec![cap("lint", "1.0.0")]);
        let reqs = vec![
            CapabilityRequirement::required("lint", VersionConstraint::parse("^1.0.0").unwrap()),
            CapabilityRequirement::required("deploy", VersionConstraint::parse(">=1.0.0").unwrap()),
        ];
        let score = score_capabilities(&caps, &reqs);
        assert!(!score.all_required_met());
        assert_eq!(score.missing_required, vec!["deploy"]);
    }

    #[test]
    fn best_match_picks_highest_scorer() {
        let sets = vec![
            // Agent 0: only lint
            CapabilitySet::from_capabilities(vec![cap("lint", "1.0.0")]),
            // Agent 1: lint + deploy (required) + test (optional)
            CapabilitySet::from_capabilities(vec![
                cap("lint", "1.0.0"),
                cap("deploy", "1.0.0"),
                cap("test", "1.0.0"),
            ]),
            // Agent 2: lint + deploy (required) but no optional
            CapabilitySet::from_capabilities(vec![
                cap("lint", "1.0.0"),
                cap("deploy", "1.0.0"),
            ]),
        ];
        let reqs = vec![
            CapabilityRequirement::required("lint", VersionConstraint::parse("^1.0.0").unwrap()),
            CapabilityRequirement::required("deploy", VersionConstraint::parse(">=1.0.0").unwrap()),
            CapabilityRequirement::optional("test", VersionConstraint::parse("*").unwrap()),
        ];

        let result = best_match(&sets, &reqs).unwrap();
        assert_eq!(result.agent_index, 1); // Agent 1 has the optional too.
        assert!(result.all_required_met());
    }

    #[test]
    fn best_match_none_when_required_unmet() {
        let sets = vec![CapabilitySet::from_capabilities(vec![cap("lint", "1.0.0")])];
        let reqs = vec![CapabilityRequirement::required(
            "deploy",
            VersionConstraint::parse(">=1.0.0").unwrap(),
        )];
        assert!(best_match(&sets, &reqs).is_none());
    }
}
