//! Strict finding validation against the immutable changed-file snapshot.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub path: String,
    pub line: u32,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutput {
    pub findings: Vec<Finding>,
}

pub fn validate_output(
    value: Value,
    files: &BTreeMap<String, u32>,
) -> Result<Vec<Finding>, String> {
    let output: ReviewOutput =
        serde_json::from_value(value).map_err(|e| format!("invalid review schema: {e}"))?;
    if output.findings.len() > 1000 {
        return Err("too many findings (maximum 1000)".into());
    }
    for finding in &output.findings {
        if finding.path.starts_with('/')
            || finding.path.contains('\\')
            || finding
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(format!("invalid relative finding path: {:?}", finding.path));
        }
        let lines = files.get(&finding.path).ok_or_else(|| {
            format!(
                "finding path is not a changed reviewable file: {:?}",
                finding.path
            )
        })?;
        if finding.line == 0 || finding.line > *lines {
            return Err(format!(
                "finding line {} outside {:?} (1..={lines})",
                finding.line, finding.path
            ));
        }
        if finding.title.trim().is_empty()
            || finding.title.len() > 500
            || finding.body.trim().is_empty()
            || finding.body.len() > 16_384
        {
            return Err("finding title/body must be nonempty and bounded (500/16384 bytes)".into());
        }
    }
    Ok(deduplicate(output.findings))
}

/// Collapse exact duplicates only; differing evidence is retained for review.
pub fn deduplicate(findings: Vec<Finding>) -> Vec<Finding> {
    let mut seen = HashSet::new();
    findings
        .into_iter()
        .filter(|finding| seen.insert(finding.clone()))
        .collect()
}

/// Provider-facing schema; semantic path/line constraints are checked locally.
pub fn review_schema() -> Value {
    serde_json::json!({"type":"object","additionalProperties":false,"required":["findings"],"properties":{
        "findings":{"type":"array","maxItems":1000,"items":{"type":"object","additionalProperties":false,
            "required":["path","line","severity","title","body"],"properties":{
                "path":{"type":"string","minLength":1},"line":{"type":"integer","minimum":1},
                "severity":{"type":"string","enum":["low","medium","high","critical"]},
                "title":{"type":"string","minLength":1,"maxLength":500},
                "body":{"type":"string","minLength":1,"maxLength":16384}
            }}}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn valid() -> Value {
        json!({"findings":[{"path":"src/a.rs","line":2,"severity":"high","title":"Bad branch","body":"The changed branch returns the wrong value."}]})
    }
    fn files() -> BTreeMap<String, u32> {
        BTreeMap::from([("src/a.rs".into(), 3)])
    }
    #[test]
    fn accepts_and_deduplicates_exact_findings() {
        let mut value = valid();
        let duplicate = value["findings"][0].clone();
        value["findings"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(validate_output(value, &files()).unwrap().len(), 1);
        assert!(
            validate_output(json!({"findings":[]}), &files())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn rejects_untrusted_locations_and_schema() {
        for (field, value) in [
            ("path", json!("../src/a.rs")),
            ("path", json!("/src/a.rs")),
            ("path", json!("other.rs")),
            ("path", json!("src\\a.rs")),
            ("line", json!(0)),
            ("line", json!(4)),
            ("line", json!(-1)),
            ("severity", json!("urgent")),
            ("title", json!(" ")),
            ("body", json!("")),
            ("unexpected", json!(true)),
        ] {
            let mut output = valid();
            output["findings"][0][field] = value;
            assert!(
                validate_output(output, &files()).is_err(),
                "accepted invalid {field}"
            );
        }
        let mut output = valid();
        output["extra"] = json!(true);
        assert!(validate_output(output, &files()).is_err());
        assert!(validate_output(json!({}), &files()).is_err());
    }
    #[test]
    fn different_evidence_is_not_silently_collapsed() {
        let mut value = valid();
        let mut second = value["findings"][0].clone();
        second["body"] = json!("Different evidence");
        value["findings"].as_array_mut().unwrap().push(second);
        assert_eq!(validate_output(value, &files()).unwrap().len(), 2);
    }
}
