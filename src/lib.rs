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
use lava_schema::{Interface, InterfaceRegistry, SchemaError};
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
    #[error("composition rejected — stack `{stack}` requires output `{output}` from `{provider}`, not declared by the registered interface")]
    UnsatisfiedRequire {
        stack: String,
        provider: String,
        output: String,
    },
    #[error("composition rejected — stack `{stack}` references unknown provider interface `{provider}`")]
    UnknownProvider { stack: String, provider: String },
    #[error("input bag rejected for stack `{stack}`: {first}")]
    BadInputs { stack: String, first: SchemaError },
}

/// Typed cross-architecture dependency. The consuming stack declares
/// that it pulls `output` from another stack whose architecture
/// satisfies `provider_interface`. Composition typechecks at
/// `StackBundle::validate` — wrong output names, unknown provider
/// interfaces, or providers that don't actually declare the output
/// all fail before any plan/apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackRequirement {
    /// Stable name of the local slot the upstream output binds to.
    pub binding: String,
    /// Provider interface name (must be registered in the bundle's
    /// `InterfaceRegistry`).
    pub provider_interface: String,
    /// Outputs from the provider the consumer reads.
    pub outputs: Vec<String>,
}

/// Typed composition of multiple stacks + a typed interface registry.
/// The registry is the gate: every stack declares (a) which
/// interface its own architecture satisfies and (b) what cross-stack
/// requirements it consumes. `validate` rejects every typed mismatch
/// before plan time.
#[derive(Debug, Default)]
pub struct StackBundle {
    pub registry: InterfaceRegistry,
    pub stacks: IndexMap<String, BundledStack>,
}

/// One stack inside a [`StackBundle`] — wraps the typed [`Stack`]
/// with the interface assignments composition needs.
#[derive(Debug, Clone)]
pub struct BundledStack {
    pub stack: Stack,
    /// The architecture this stack ships satisfies this interface.
    /// `None` means the architecture is anonymous (no typed contract).
    pub satisfies: Option<String>,
    pub requires: Vec<StackRequirement>,
    /// Operator-supplied input bag the bundle's gate validates against
    /// the stack's own interface.
    pub inputs: IndexMap<String, String>,
}

impl StackBundle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, iface: Interface) {
        self.registry.register(iface);
    }

    /// Add a stack to the bundle. Idempotent on stack name.
    pub fn add_stack(&mut self, b: BundledStack) {
        self.stacks.insert(b.stack.name.clone(), b);
    }

    /// Run the typed composition checks. Returns the list of every
    /// violation; empty Ok means the bundle composes correctly.
    ///
    /// # Errors
    /// Returns a vector of typed [`StackError`] entries listing every
    /// mismatch. CI surfaces these as a single batched report.
    pub fn validate(&self) -> Result<(), Vec<StackError>> {
        let mut errors = Vec::new();

        for (name, bundled) in &self.stacks {
            // 1) The stack's *own* inputs must satisfy its declared
            //    interface (if it has one).
            if let Some(iface_name) = &bundled.satisfies {
                let Some(iface) = self.registry.get(iface_name) else {
                    errors.push(StackError::UnknownProvider {
                        stack: name.clone(),
                        provider: iface_name.clone(),
                    });
                    continue;
                };
                if let Err(es) = iface.validate_inputs(&bundled.inputs) {
                    if let Some(first) = es.into_iter().next() {
                        errors.push(StackError::BadInputs {
                            stack: name.clone(),
                            first,
                        });
                    }
                }
            }

            // 2) Every :requires clause names a registered provider
            //    interface and lists only outputs that interface
            //    actually declares.
            for req in &bundled.requires {
                let Some(provider) = self.registry.get(&req.provider_interface) else {
                    errors.push(StackError::UnknownProvider {
                        stack: name.clone(),
                        provider: req.provider_interface.clone(),
                    });
                    continue;
                };
                let missing = self.registry.provides(provider, &req.outputs);
                for m in missing {
                    errors.push(StackError::UnsatisfiedRequire {
                        stack: name.clone(),
                        provider: req.provider_interface.clone(),
                        output: m,
                    });
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl BundledStack {
    #[must_use]
    pub fn new(stack: Stack) -> Self {
        Self {
            stack,
            satisfies: None,
            requires: Vec::new(),
            inputs: IndexMap::new(),
        }
    }

    #[must_use]
    pub fn satisfies(mut self, iface_name: impl Into<String>) -> Self {
        self.satisfies = Some(iface_name.into());
        self
    }

    #[must_use]
    pub fn requires(mut self, req: StackRequirement) -> Self {
        self.requires.push(req);
        self
    }

    #[must_use]
    pub fn with_input(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.inputs.insert(k.into(), v.into());
        self
    }
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

    fn vpc_interface() -> Interface {
        use lava_schema::Field;
        use lava_schema::Interface;
        let mut iface = Interface::new("aws-vpc-network");
        iface
            .outputs
            .insert("vpc-id".to_string(), Field::strict(lava_types::Type::String));
        iface.outputs.insert(
            "public-subnet-ids".to_string(),
            Field::strict(lava_types::Type::ListOf {
                inner: Box::new(lava_types::Type::String),
                min_items: None,
                max_items: None,
            }),
        );
        iface
    }

    fn eks_interface() -> Interface {
        use lava_schema::Field;
        use lava_schema::Interface;
        let mut iface = Interface::new("aws-eks-cluster");
        iface
            .inputs
            .insert("cluster-name".to_string(), Field::strict(lava_types::Type::String));
        iface
            .outputs
            .insert("cluster-arn".to_string(), Field::strict(lava_types::Type::String));
        iface
    }

    #[test]
    fn bundle_validates_cross_stack_requires_against_provider_interface() {
        let mut bundle = StackBundle::new();
        bundle.register(vpc_interface());
        bundle.register(eks_interface());

        let vpc_stack = Stack::new(
            "prod-vpc",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/vpc".into() },
        );
        bundle.add_stack(BundledStack::new(vpc_stack).satisfies("aws-vpc-network"));

        let eks_stack = Stack::new(
            "prod-eks",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/eks".into() },
        );
        bundle.add_stack(
            BundledStack::new(eks_stack)
                .satisfies("aws-eks-cluster")
                .with_input("cluster-name", "prod-cluster")
                .requires(StackRequirement {
                    binding: "net".to_string(),
                    provider_interface: "aws-vpc-network".to_string(),
                    outputs: vec!["vpc-id".to_string(), "public-subnet-ids".to_string()],
                }),
        );

        assert!(bundle.validate().is_ok());
    }

    #[test]
    fn bundle_rejects_require_referencing_missing_output() {
        let mut bundle = StackBundle::new();
        bundle.register(vpc_interface());

        let eks_stack = Stack::new(
            "prod-eks",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/eks".into() },
        );
        bundle.add_stack(
            BundledStack::new(eks_stack).requires(StackRequirement {
                binding: "net".to_string(),
                provider_interface: "aws-vpc-network".to_string(),
                // VPC interface does NOT declare :kubeconfig — typed reject.
                outputs: vec!["vpc-id".to_string(), "kubeconfig".to_string()],
            }),
        );

        let errors = bundle.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                StackError::UnsatisfiedRequire { output, .. } if output == "kubeconfig"
            )),
            "expected UnsatisfiedRequire for kubeconfig, got {errors:?}"
        );
    }

    #[test]
    fn bundle_rejects_unknown_provider_interface() {
        let mut bundle = StackBundle::new();
        let eks_stack = Stack::new(
            "prod-eks",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/eks".into() },
        );
        bundle.add_stack(
            BundledStack::new(eks_stack).requires(StackRequirement {
                binding: "net".to_string(),
                provider_interface: "no-such-interface".to_string(),
                outputs: vec![],
            }),
        );

        let errors = bundle.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, StackError::UnknownProvider { provider, .. } if provider == "no-such-interface")));
    }

    #[test]
    fn bundle_rejects_bad_inputs_via_satisfies_gate() {
        use lava_schema::Field;
        let mut iface = Interface::new("strict-iface");
        iface
            .inputs
            .insert("must-have".to_string(), Field::strict(lava_types::Type::String));

        let mut bundle = StackBundle::new();
        bundle.register(iface);

        let stack = Stack::new(
            "prod-x",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/x".into() },
        );
        bundle.add_stack(BundledStack::new(stack).satisfies("strict-iface"));

        let errors = bundle.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, StackError::BadInputs { .. })));
    }

    #[test]
    fn bundle_aggregates_multiple_failures_into_one_validate_call() {
        let mut bundle = StackBundle::new();
        bundle.register(vpc_interface());

        let bad_stack = Stack::new(
            "bad",
            tiny_vpc_architecture(),
            "prod",
            BackendRef::Local { path: "/tmp/x".into() },
        );
        bundle.add_stack(
            BundledStack::new(bad_stack)
                .requires(StackRequirement {
                    binding: "a".to_string(),
                    provider_interface: "aws-vpc-network".to_string(),
                    outputs: vec!["nope-1".to_string(), "nope-2".to_string()],
                })
                .requires(StackRequirement {
                    binding: "b".to_string(),
                    provider_interface: "missing-iface".to_string(),
                    outputs: vec![],
                }),
        );
        let errors = bundle.validate().unwrap_err();
        assert!(errors.len() >= 3, "expected ≥3 errors, got {errors:?}");
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
