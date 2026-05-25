//! lava-stack — deployment instance layer for the lava suite.
//!
//! A [`Stack`] is one instantiation of an architecture into a specific
//! environment (prod / staging / dev). It pairs:
//!
//! - the **architecture** (typed [`lava_core::Architecture`] produced
//!   by `lava-arch` from a deflava-architecture form);
//! - the **backend** (state file location + locking — local file / S3 /
//!   GCS / Azure blob) — [`lava_core::BackendRef`];
//! - the **workspace** name (Terraform's per-state-file partition,
//!   matching tofu/terraform workspace semantics);
//! - any **variable overrides** for this instance.
//!
//! ## Tatara-lisp surface
//!
//! ```lisp
//! (deflava-stack prod-us-east-2
//!   :architecture (aws-vpc-network :cidr "10.0.0.0/16")
//!   :backend (s3-backend :bucket "pleme-tf-state"
//!                        :key "prod-us-east-2/vpc.tfstate"
//!                        :region "us-east-2")
//!   :workspace "prod"
//!   :variables (:enable-flow-logs #t :nat-gateway-multi-az #t))
//! ```
//!
//! The evaluated form produces a typed [`Stack`] value. magma's
//! orchestrator reads the Stack + drives `magma plan` / `magma apply`
//! against the named backend + workspace.

#![allow(clippy::module_name_repetitions)]

use indexmap::IndexMap;
use lava_core::{Architecture, BackendRef, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One deployable stack. Same architecture can ship into many stacks
/// (prod/staging/dev × region) by varying backend + workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    pub name: String,
    pub architecture: Architecture,
    pub workspace: String,
    pub backend: BackendRef,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub variables: IndexMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<StackTag>,
}

/// Operator-visible label on the stack. Drives drift policy,
/// notification routing, approval gates, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackTag {
    pub key: String,
    pub value: String,
}

/// Stack-level configuration knob — typed surface for an env's
/// rollout policy. Maps 1:1 to fields pangea-architectures consume
/// via the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackConfig {
    pub auto_approve: bool,
    pub destroy_protection: bool,
    pub notification_channel: Option<String>,
    pub max_concurrent_resources: Option<u32>,
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            auto_approve: false,
            destroy_protection: true,
            notification_channel: None,
            max_concurrent_resources: None,
        }
    }
}

impl Stack {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        architecture: Architecture,
        workspace: impl Into<String>,
        backend: BackendRef,
    ) -> Self {
        Self {
            name: name.into(),
            architecture,
            workspace: workspace.into(),
            backend,
            variables: IndexMap::new(),
            tags: Vec::new(),
        }
    }

    /// Set a variable override. Pangea consumers pass the analogous
    /// hash to `architecture.build(synth, vars)`; lava holds the
    /// overrides on the Stack so they persist across magma plans.
    pub fn variable(&mut self, key: impl Into<String>, value: Value) {
        self.variables.insert(key.into(), value);
    }

    /// Apply a Tag. Multi-call to layer tags.
    pub fn tag(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tags.push(StackTag {
            key: key.into(),
            value: value.into(),
        });
    }

    /// Render this stack's complete `terraform.json` (architecture
    /// resources + backend block + variable overrides spliced in).
    /// magma applies this directly.
    pub fn render_terraform_json(&self) -> Result<serde_json::Value, StackError> {
        let mut root = self
            .architecture
            .render_terraform_json()
            .map_err(|e| StackError::Render(e.to_string()))?;
        let root_obj = root
            .as_object_mut()
            .ok_or_else(|| StackError::Render("expected object root".to_string()))?;

        // Variable overrides → terraform `variable` block.
        if !self.variables.is_empty() {
            let mut variables = serde_json::Map::new();
            for (k, v) in &self.variables {
                let mut entry = serde_json::Map::new();
                entry.insert("default".to_string(), v.clone().into_json());
                variables.insert(k.clone(), serde_json::Value::Object(entry));
            }
            root_obj.insert("variable".to_string(), serde_json::Value::Object(variables));
        }

        // Backend block — terraform { backend "<kind>" { ... } }.
        let backend_block = render_backend(&self.backend);
        let mut terraform = serde_json::Map::new();
        terraform.insert("backend".to_string(), backend_block);
        root_obj.insert("terraform".to_string(), serde_json::Value::Object(terraform));

        Ok(root)
    }
}

fn render_backend(b: &BackendRef) -> serde_json::Value {
    let mut backend = serde_json::Map::new();
    match b {
        BackendRef::Local { path } => {
            let mut body = serde_json::Map::new();
            body.insert("path".to_string(), serde_json::Value::String(path.clone()));
            backend.insert("local".to_string(), serde_json::Value::Object(body));
        }
        BackendRef::S3 { bucket, key, region } => {
            let mut body = serde_json::Map::new();
            body.insert("bucket".to_string(), serde_json::Value::String(bucket.clone()));
            body.insert("key".to_string(), serde_json::Value::String(key.clone()));
            body.insert("region".to_string(), serde_json::Value::String(region.clone()));
            backend.insert("s3".to_string(), serde_json::Value::Object(body));
        }
        BackendRef::Gcs { bucket, prefix } => {
            let mut body = serde_json::Map::new();
            body.insert("bucket".to_string(), serde_json::Value::String(bucket.clone()));
            body.insert("prefix".to_string(), serde_json::Value::String(prefix.clone()));
            backend.insert("gcs".to_string(), serde_json::Value::Object(body));
        }
        BackendRef::AzureBlob {
            storage_account,
            container,
            key,
        } => {
            let mut body = serde_json::Map::new();
            body.insert(
                "storage_account_name".to_string(),
                serde_json::Value::String(storage_account.clone()),
            );
            body.insert(
                "container_name".to_string(),
                serde_json::Value::String(container.clone()),
            );
            body.insert("key".to_string(), serde_json::Value::String(key.clone()));
            backend.insert("azurerm".to_string(), serde_json::Value::Object(body));
        }
    }
    serde_json::Value::Object(backend)
}

#[derive(Debug, Error)]
pub enum StackError {
    #[error("render failed: {0}")]
    Render(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use lava_arch::Builder;
    use lava_core::Resource;

    fn tiny_vpc_architecture() -> Architecture {
        let mut b = Builder::new("vpc-only");
        let mut attrs = IndexMap::new();
        attrs.insert("cidr_block".to_string(), Value::s("10.0.0.0/16"));
        b.add_resource(Resource {
            type_id: "aws_vpc".to_string(),
            name: "main".to_string(),
            attributes: attrs,
            depends_on: vec![],
            provider: None,
            multiplicity: None,
        });
        b.finish()
    }

    #[test]
    fn stack_with_s3_backend_renders_backend_block() {
        let mut stack = Stack::new(
            "prod-us-east-2",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::S3 {
                bucket: "pleme-tf-state".to_string(),
                key: "prod/vpc.tfstate".to_string(),
                region: "us-east-2".to_string(),
            },
        );
        stack.tag("env", "prod");
        let json = stack.render_terraform_json().unwrap();
        assert_eq!(
            json["terraform"]["backend"]["s3"]["bucket"],
            "pleme-tf-state"
        );
        assert_eq!(json["terraform"]["backend"]["s3"]["region"], "us-east-2");
        // Architecture resources flowed through.
        assert_eq!(json["resource"]["aws_vpc"]["main"]["cidr_block"], "10.0.0.0/16");
    }

    #[test]
    fn stack_variable_overrides_render_as_terraform_variable_block() {
        let mut stack = Stack::new(
            "dev",
            tiny_vpc_architecture(),
            "dev",
            BackendRef::Local {
                path: "/tmp/dev.tfstate".to_string(),
            },
        );
        stack.variable("enable_flow_logs", Value::b(false));
        stack.variable("nat_gateway_count", Value::n(1));
        let json = stack.render_terraform_json().unwrap();
        assert_eq!(json["variable"]["enable_flow_logs"]["default"], false);
        assert_eq!(json["variable"]["nat_gateway_count"]["default"], 1);
    }

    #[test]
    fn stack_round_trips_through_serde() {
        let stack = Stack::new(
            "x",
            tiny_vpc_architecture(),
            "default",
            BackendRef::Local {
                path: "/tmp/x.tfstate".to_string(),
            },
        );
        let json = serde_json::to_string(&stack).unwrap();
        let parsed: Stack = serde_json::from_str(&json).unwrap();
        assert_eq!(stack, parsed);
    }
}
