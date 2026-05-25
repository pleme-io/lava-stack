(defcaixa
  :name
  "lava-stack"
  :kind
  :Biblioteca
  :ecosystem
  :rust-single-crate
  :package
  {:name "lava-stack"
   :version "0.1.0"
   :description "Deployment instance layer for the lava suite. deflava-stack form. Maps architecture + workspace + backend to one Terraform workspace. Variable overrides per environment. Pangea-stack analog."
   :license "MIT"
   :repository "https://github.com/pleme-io/lava-stack"}
  :ci-config
  {:bump {:default-type "patch"}
   :publish {:no-verify true}}
  :workflows
  [:auto-release :pre-merge-gate :security-gate])
