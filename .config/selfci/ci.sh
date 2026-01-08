#!/usr/bin/env bash
set -eou pipefail


function job_lint() {
  set -x

  # Get zero-terminated list of non-symlink files (reusable)
  git_ls_files="$(git ls-files -z | while IFS= read -r -d '' file; do
    [ ! -L "$file" ] && printf '%s\0' "$file"
  done)"

  selfci step start "check leftover dbg!"
  while IFS= read -r -d '' path; do
    if grep -q 'dbg!(' "$path"; then
      >&2 echo "$path contains dbg! macro"
      selfci step fail
    fi
  done <<< "$git_ls_files"

  selfci step start "nixfmt"
  nix_files=()
  while IFS= read -r -d '' path; do
    [[ "$path" == *.nix ]] && nix_files+=("$path")
  done <<< "$git_ls_files"
  if [ ${#nix_files[@]} -gt 0 ]; then
    if ! nixfmt -c "${nix_files[@]}"; then
      selfci step fail
    fi
  fi

  selfci step start "cargo fmt"
  if ! cargo fmt --all --check ; then
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
