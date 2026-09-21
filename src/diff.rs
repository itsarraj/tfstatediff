//! Pure diffing logic: flattens nested attribute JSON into dotted
//! paths, compares two states resource-by-resource and output-by-output,
//! and redacts values on sensitive paths rather than ever printing them.

use std::collections::{BTreeMap, BTreeSet};

use crate::state::{resource_address, InstanceState, OutputValue, ResourceState, TfState};

const REDACTED: &str = "(sensitive value, redacted)";

/// Key names that heuristically indicate a sensitive value even when a
/// state file's `sensitive_attributes` list doesn't cover them (older
/// state format versions stored that list inconsistently, or a
/// provider simply didn't mark a field sensitive that should be).
const SENSITIVE_KEYWORDS: &[&str] = &[
    "password",
    "secret",
    "token",
    "private_key",
    "access_key",
    "credential",
    "api_key",
];

fn last_path_segment(path: &str) -> &str {
    let after_dot = path.rsplit('.').next().unwrap_or(path);
    after_dot.split('[').next().unwrap_or(after_dot)
}

pub fn is_sensitive_path(path: &str) -> bool {
    let key = last_path_segment(path).to_lowercase();
    SENSITIVE_KEYWORDS.iter().any(|kw| key.contains(kw))
}

/// Flattens a JSON value into `(dotted.path, rendered value)` pairs.
/// Objects become `.key` segments, arrays become `[index]` segments;
/// only leaf scalars (and `null`) are emitted, matching how a human
/// would name a changed field.
pub fn flatten(prefix: &str, value: &serde_json::Value, out: &mut BTreeMap<String, String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&path, v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                flatten(&path, v, out);
            }
        }
        serde_json::Value::Null => {
            out.insert(prefix.to_string(), "null".to_string());
        }
        serde_json::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrChange {
    pub path: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Diffs two attribute trees leaf-by-leaf. Any value on a path that
/// `is_sensitive_path` flags is replaced with a fixed redaction marker
/// before it's ever placed in the output, on both sides.
pub fn diff_attributes(old: &serde_json::Value, new: &serde_json::Value) -> Vec<AttrChange> {
    let mut old_flat = BTreeMap::new();
    let mut new_flat = BTreeMap::new();
    flatten("", old, &mut old_flat);
    flatten("", new, &mut new_flat);

    let all_paths: BTreeSet<&String> = old_flat.keys().chain(new_flat.keys()).collect();
    all_paths
        .into_iter()
        .filter_map(|path| {
            let old_v = old_flat.get(path);
            let new_v = new_flat.get(path);
            if old_v == new_v {
                return None;
            }
            let redact = is_sensitive_path(path);
            let render = |v: Option<&String>| -> Option<String> {
                v.map(|s| {
                    if redact {
                        REDACTED.to_string()
                    } else {
                        s.clone()
                    }
                })
            };
            Some(AttrChange {
                path: path.clone(),
                old: render(old_v),
                new: render(new_v),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceChangeKind {
    Created,
    Destroyed,
    Changed,
}

#[derive(Debug, Clone)]
pub struct ResourceDiff {
    pub address: String,
    pub kind: ResourceChangeKind,
    pub attr_changes: Vec<AttrChange>,
}

fn index_instances(resources: &[ResourceState]) -> BTreeMap<String, &InstanceState> {
    let mut map = BTreeMap::new();
    for r in resources {
        for instance in &r.instances {
            let addr = resource_address(r, instance.index_key.as_ref());
            map.insert(addr, instance);
        }
    }
    map
}

pub fn diff_resources(old: &[ResourceState], new: &[ResourceState]) -> Vec<ResourceDiff> {
    let old_map = index_instances(old);
    let new_map = index_instances(new);
    let all_addrs: BTreeSet<&String> = old_map.keys().chain(new_map.keys()).collect();

    let mut diffs = Vec::new();
    for addr in all_addrs {
        match (old_map.get(addr), new_map.get(addr)) {
            (None, Some(_)) => diffs.push(ResourceDiff {
                address: addr.clone(),
                kind: ResourceChangeKind::Created,
                attr_changes: vec![],
            }),
            (Some(_), None) => diffs.push(ResourceDiff {
                address: addr.clone(),
                kind: ResourceChangeKind::Destroyed,
                attr_changes: vec![],
            }),
            (Some(old_inst), Some(new_inst)) => {
                let changes = diff_attributes(&old_inst.attributes, &new_inst.attributes);
                if !changes.is_empty() {
                    diffs.push(ResourceDiff {
                        address: addr.clone(),
                        kind: ResourceChangeKind::Changed,
                        attr_changes: changes,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    diffs
}

#[derive(Debug, Clone)]
pub struct OutputDiff {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

pub fn diff_outputs(
    old: &BTreeMap<String, OutputValue>,
    new: &BTreeMap<String, OutputValue>,
) -> Vec<OutputDiff> {
    let all_names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    all_names
        .into_iter()
        .filter_map(|name| {
            let old_v = old.get(name);
            let new_v = new.get(name);
            // Compare the raw value (and sensitivity flag), not the
            // already-redacted display string — two different sensitive
            // values both render as the same redaction marker, which
            // would otherwise hide a real change to a sensitive output.
            let raw_equal = match (old_v, new_v) {
                (Some(o), Some(n)) => o.value == n.value && o.sensitive == n.sensitive,
                (None, None) => true,
                _ => false,
            };
            if raw_equal {
                return None;
            }
            Some(OutputDiff {
                name: name.clone(),
                old: old_v.map(render_output),
                new: new_v.map(render_output),
            })
        })
        .collect()
}

fn render_output(output: &OutputValue) -> String {
    if output.sensitive {
        REDACTED.to_string()
    } else {
        match &output.value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

pub struct StateComparison {
    pub lineage_matches: bool,
    pub old_lineage: String,
    pub new_lineage: String,
    pub old_serial: u64,
    pub new_serial: u64,
    pub resource_diffs: Vec<ResourceDiff>,
    pub output_diffs: Vec<OutputDiff>,
}

pub fn compare(old: &TfState, new: &TfState) -> StateComparison {
    StateComparison {
        lineage_matches: old.lineage == new.lineage
            || old.lineage.is_empty()
            || new.lineage.is_empty(),
        old_lineage: old.lineage.clone(),
        new_lineage: new.lineage.clone(),
        old_serial: old.serial,
        new_serial: new.serial,
        resource_diffs: diff_resources(&old.resources, &new.resources),
        output_diffs: diff_outputs(&old.outputs, &new.outputs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_sensitive_path_matches_common_keyword_case_insensitively() {
        assert!(is_sensitive_path("attributes.Password"));
        assert!(is_sensitive_path("db.master_password"));
        assert!(is_sensitive_path("tags.access_key"));
        assert!(!is_sensitive_path("tags.Name"));
    }

    #[test]
    fn flatten_handles_nested_objects_and_arrays() {
        let value = json!({"a": {"b": 1}, "c": [10, 20]});
        let mut out = BTreeMap::new();
        flatten("", &value, &mut out);
        assert_eq!(out.get("a.b"), Some(&"1".to_string()));
        assert_eq!(out.get("c[0]"), Some(&"10".to_string()));
        assert_eq!(out.get("c[1]"), Some(&"20".to_string()));
    }

    #[test]
    fn diff_attributes_reports_only_changed_leaves() {
        let old = json!({"instance_type": "t2.micro", "tags": {"Name": "web"}});
        let new = json!({"instance_type": "t2.large", "tags": {"Name": "web"}});
        let changes = diff_attributes(&old, &new);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "instance_type");
        assert_eq!(changes[0].old.as_deref(), Some("t2.micro"));
        assert_eq!(changes[0].new.as_deref(), Some("t2.large"));
    }

    #[test]
    fn diff_attributes_redacts_sensitive_paths_on_both_sides() {
        let old = json!({"password": "hunter2"});
        let new = json!({"password": "correct-horse-battery-staple"});
        let changes = diff_attributes(&old, &new);
        assert_eq!(changes[0].old.as_deref(), Some(REDACTED));
        assert_eq!(changes[0].new.as_deref(), Some(REDACTED));
    }

    #[test]
    fn diff_attributes_reports_added_and_removed_leaves() {
        let old = json!({"a": "1"});
        let new = json!({"b": "2"});
        let changes = diff_attributes(&old, &new);
        assert_eq!(changes.len(), 2);
        let added = changes.iter().find(|c| c.path == "b").unwrap();
        assert_eq!(added.old, None);
        assert_eq!(added.new.as_deref(), Some("2"));
        let removed = changes.iter().find(|c| c.path == "a").unwrap();
        assert_eq!(removed.new, None);
    }

    fn make_resource(ty: &str, name: &str, attrs: serde_json::Value) -> ResourceState {
        ResourceState {
            module: String::new(),
            mode: "managed".to_string(),
            resource_type: ty.to_string(),
            name: name.to_string(),
            instances: vec![InstanceState {
                index_key: None,
                attributes: attrs,
                sensitive_attributes: vec![],
            }],
        }
    }

    #[test]
    fn diff_resources_detects_created_resource() {
        let old = vec![];
        let new = vec![make_resource("aws_instance", "web", json!({}))];
        let diffs = diff_resources(&old, &new);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, ResourceChangeKind::Created);
        assert_eq!(diffs[0].address, "aws_instance.web");
    }

    #[test]
    fn diff_resources_detects_destroyed_resource() {
        let old = vec![make_resource("aws_instance", "web", json!({}))];
        let new = vec![];
        let diffs = diff_resources(&old, &new);
        assert_eq!(diffs[0].kind, ResourceChangeKind::Destroyed);
    }

    #[test]
    fn diff_resources_detects_attribute_drift() {
        let old = vec![make_resource(
            "aws_instance",
            "web",
            json!({"instance_type": "t2.micro"}),
        )];
        let new = vec![make_resource(
            "aws_instance",
            "web",
            json!({"instance_type": "t2.large"}),
        )];
        let diffs = diff_resources(&old, &new);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, ResourceChangeKind::Changed);
        assert_eq!(diffs[0].attr_changes.len(), 1);
    }

    #[test]
    fn diff_resources_omits_identical_resources() {
        let old = vec![make_resource(
            "aws_instance",
            "web",
            json!({"instance_type": "t2.micro"}),
        )];
        let new = vec![make_resource(
            "aws_instance",
            "web",
            json!({"instance_type": "t2.micro"}),
        )];
        assert!(diff_resources(&old, &new).is_empty());
    }

    #[test]
    fn diff_outputs_redacts_sensitive_flag_regardless_of_key_name() {
        let mut old = BTreeMap::new();
        old.insert(
            "db_endpoint_url".to_string(),
            OutputValue {
                value: json!("old-host:5432"),
                sensitive: true,
            },
        );
        let mut new = BTreeMap::new();
        new.insert(
            "db_endpoint_url".to_string(),
            OutputValue {
                value: json!("new-host:5432"),
                sensitive: true,
            },
        );
        let diffs = diff_outputs(&old, &new);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].old.as_deref(), Some(REDACTED));
        assert_eq!(diffs[0].new.as_deref(), Some(REDACTED));
    }

    #[test]
    fn compare_flags_lineage_mismatch() {
        let old = TfState {
            terraform_version: String::new(),
            serial: 1,
            lineage: "aaa".to_string(),
            outputs: BTreeMap::new(),
            resources: vec![],
        };
        let new = TfState {
            lineage: "bbb".to_string(),
            ..old_state_like(&old)
        };
        let comparison = compare(&old, &new);
        assert!(!comparison.lineage_matches);
    }

    fn old_state_like(s: &TfState) -> TfState {
        TfState {
            terraform_version: s.terraform_version.clone(),
            serial: s.serial,
            lineage: s.lineage.clone(),
            outputs: BTreeMap::new(),
            resources: vec![],
        }
    }
}
