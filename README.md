# lava-stack

Deployment instance layer for the [lava](https://github.com/pleme-io) suite.

A **stack** is one instantiation of an architecture into a specific
environment. The same architecture ships into many stacks — prod / staging /
dev, × region — by varying only the backend and workspace.

A `Stack` pairs four things:

| Part | What it is |
|---|---|
| **architecture** | a typed `lava_core::Architecture`, produced by `lava-arch` from a `deflava-architecture` form |
| **backend** | `lava_core::BackendRef` — where state lives and how it locks (local file / S3 / GCS / Azure blob) |
| **workspace** | the per-state-file partition, matching tofu/terraform workspace semantics |
| **variables** | per-instance overrides |

## Tatara-lisp surface

```lisp
(deflava-stack prod-us-east-2
  :architecture (aws-vpc-network :cidr "10.0.0.0/16")
  :backend (s3-backend :bucket "pleme-tf-state"
                       :key "prod-us-east-2/vpc.tfstate"
                       :region "us-east-2")
  :workspace "prod"
  :variables (:enable-flow-logs #t :nat-gateway-multi-az #t))
```

The evaluated form produces a typed `Stack`. magma's orchestrator reads it and
drives `magma plan` / `magma apply` against the named backend and workspace.

## Usage

```toml
[dependencies]
lava-stack = "0.1"
```

## Where it sits

`lava-arch` builds the architecture · **`lava-stack` binds it to an
environment** · `magma` executes the result. `lava-prelude` re-exports the
suite for consumers who want all of it at once.

## License

MIT
