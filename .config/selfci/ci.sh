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

case "$SELFCI_JOB_NAME" in
  main)
    selfci job start "lint"
    selfci job start "cargo"
    ;;

  cargo)
    job_cargo
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
