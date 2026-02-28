#!/usr/bin/env bash
set -eou pipefail


function job_lint() {
  selfci step start "treefmt"
  if ! treefmt --ci ; then
    selfci step fail
  fi
}

# check the things involving cargo
# We're using Nix + crane + flakebox,
# this gives us caching between different
# builds and decent isolation.
function job_cargo() {
    selfci step start "cargo.lock up to date"
    if ! cargo update --workspace --locked -q; then
      selfci step fail
    fi

    # there's not point continuing if we can't build
    selfci step start "build"
    nix build -L .#ci.workspace

    selfci step start "clippy"
    if ! nix build -L .#ci.clippy ; then
      selfci step fail
    fi

    selfci step start "nextest"
    if ! nix build -L .#ci.tests ; then
      selfci step fail
    fi
}

function job_core_features() {
    selfci step start "rostra-core no-features"
    nix build -L .#ci.rostraCoreNoFeatures

    selfci step start "rostra-core bincode"
    nix build -L .#ci.rostraCoreBincode

    selfci step start "rostra-core ed25519"
    nix build -L .#ci.rostraCoreEd25519

    selfci step start "rostra-core serde"
    nix build -L .#ci.rostraCoreSerde

    selfci step start "rostra-core ed25519+bincode"
    nix build -L .#ci.rostraCoreEd25519Bincode

    selfci step start "rostra-core ed25519+serde"
    nix build -L .#ci.rostraCoreEd25519Serde

    selfci step start "rostra-core serde+bincode"
    nix build -L .#ci.rostraCoreSerdeBincode

    selfci step start "rostra-core all-features"
    nix build -L .#ci.rostraCoreAllFeatures
}

case "$SELFCI_JOB_NAME" in
  main)
    selfci job start "lint"
    selfci job start "cargo"
    selfci job start "core-features"
    ;;

  cargo)
    job_cargo
    ;;

  core-features)
    job_core_features
    ;;

  lint)
    # use develop shell to ensure all the tools are provided at pinned versions
    export -f job_lint
    nix develop -c bash -c "job_lint"
    ;;


  *)
    echo "Unknown job: $SELFCI_JOB_NAME"
    exit 1
esac
