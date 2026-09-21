//! Typed (lenient) mirror of Terraform's state v4 JSON schema — just
//! enough to identify resources, their instances, and their attribute
//! values. Attribute values themselves stay as raw `serde_json::Value`
//! since state can hold arbitrarily nested provider-specific shapes.

use std::collections::BTreeMap;

use serde::Deserialize;

fn default_mode() -> String {
    "managed".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TfState {
    #[serde(default)]
    pub terraform_version: String,
    #[serde(default)]
    pub serial: u64,
    #[serde(default)]
    pub lineage: String,
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputValue>,
    #[serde(default)]
    pub resources: Vec<ResourceState>,
}

#[derive(Debug, Deserialize)]
pub struct OutputValue {
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Deserialize)]
pub struct ResourceState {
    #[serde(default)]
    pub module: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(rename = "type", default)]
    pub resource_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub instances: Vec<InstanceState>,
}

#[derive(Debug, Deserialize)]
pub struct InstanceState {
    #[serde(default)]
    pub index_key: Option<serde_json::Value>,
    #[serde(default)]
    pub attributes: serde_json::Value,
    #[serde(default)]
    pub sensitive_attributes: Vec<serde_json::Value>,
}

/// Renders a resource+instance's real Terraform-style address, e.g.
/// `module.vpc.aws_subnet.private["us-east-1a"]` or
/// `data.aws_ami.ubuntu`.
pub fn resource_address(resource: &ResourceState, index_key: Option<&serde_json::Value>) -> String {
    let mut addr = String::new();
    if !resource.module.is_empty() {
        addr.push_str(&resource.module);
        addr.push('.');
    }
    if resource.mode == "data" {
        addr.push_str("data.");
    }
    addr.push_str(&resource.resource_type);
    addr.push('.');
    addr.push_str(&resource.name);
    match index_key {
        Some(serde_json::Value::String(s)) => addr.push_str(&format!("[\"{s}\"]")),
        Some(serde_json::Value::Number(n)) => addr.push_str(&format!("[{n}]")),
        _ => {}
    }
    addr
}

pub fn parse(json: &str) -> serde_json::Result<TfState> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(module: &str, mode: &str, ty: &str, name: &str) -> ResourceState {
        ResourceState {
            module: module.to_string(),
            mode: mode.to_string(),
            resource_type: ty.to_string(),
            name: name.to_string(),
            instances: vec![],
        }
    }

    #[test]
    fn parses_minimal_state() {
        let state = parse(r#"{"resources":[]}"#).unwrap();
        assert!(state.resources.is_empty());
    }

    #[test]
    fn mode_defaults_to_managed_when_absent() {
        let state = parse(r#"{"resources":[{"type":"aws_instance","name":"web","instances":[]}]}"#)
            .unwrap();
        assert_eq!(state.resources[0].mode, "managed");
    }

    #[test]
    fn resource_address_for_plain_root_resource() {
        let r = resource("", "managed", "aws_instance", "web");
        assert_eq!(resource_address(&r, None), "aws_instance.web");
    }

    #[test]
    fn resource_address_for_data_source() {
        let r = resource("", "data", "aws_ami", "ubuntu");
        assert_eq!(resource_address(&r, None), "data.aws_ami.ubuntu");
    }

    #[test]
    fn resource_address_includes_module_prefix() {
        let r = resource("module.vpc", "managed", "aws_subnet", "private");
        assert_eq!(resource_address(&r, None), "module.vpc.aws_subnet.private");
    }

    #[test]
    fn resource_address_appends_string_index_key_for_each() {
        let r = resource("", "managed", "aws_subnet", "private");
        let key = serde_json::Value::String("us-east-1a".to_string());
        assert_eq!(
            resource_address(&r, Some(&key)),
            "aws_subnet.private[\"us-east-1a\"]"
        );
    }

    #[test]
    fn resource_address_appends_numeric_index_key_for_count() {
        let r = resource("", "managed", "aws_instance", "worker");
        let key = serde_json::Value::Number(2.into());
        assert_eq!(resource_address(&r, Some(&key)), "aws_instance.worker[2]");
    }

    #[test]
    fn resource_address_combines_module_and_data_prefix() {
        let r = resource("module.net", "data", "aws_ami", "ubuntu");
        assert_eq!(resource_address(&r, None), "module.net.data.aws_ami.ubuntu");
    }
}
