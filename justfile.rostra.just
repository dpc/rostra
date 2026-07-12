# run mutation testing on a package (e.g., just mutants rostra-client-db)
mutants PACKAGE *ARGS:
    cargo mutants --package {{PACKAGE}} {{ARGS}}

# list available mutations for a package
mutants-list PACKAGE *ARGS:
    cargo mutants --list --package {{PACKAGE}} {{ARGS}}

# mutation testing for rostra-client-db
alias mutants-db := _mutants-db
_mutants-db *ARGS:
    cargo mutants --package rostra-client-db {{ARGS}}

# run and restart on changes, then open the site in the default browser
dev PORT="2345":
    just _dev "{{PORT}}" 1

# run and restart on changes without opening a browser
dev-no-open PORT="2345":
    just _dev "{{PORT}}" 0

[private]
_dev PORT OPEN_BROWSER:
    #!/usr/bin/env bash
    set -euo pipefail

    if [ ! -f Cargo.toml ]; then
      cd {{invocation_directory()}}
    fi

    export ROSTRA_DATA_DIR
    ROSTRA_DATA_DIR=dev/{{PORT}}

    umask 077
    mkdir -p "$ROSTRA_DATA_DIR"
    chmod 700 "$ROSTRA_DATA_DIR"
    if [[ -L "$ROSTRA_DATA_DIR/secret" ]] \
      || [[ -e "$ROSTRA_DATA_DIR/secret" && ! -f "$ROSTRA_DATA_DIR/secret" ]]; then
      echo "Refusing non-regular development secret: $ROSTRA_DATA_DIR/secret" >&2
      exit 1
    fi
    if [ ! -e "$ROSTRA_DATA_DIR/secret" ]; then
      cargo run gen-id | jq -r '.secret' > "$ROSTRA_DATA_DIR/secret"
    fi
    chmod 600 "$ROSTRA_DATA_DIR/secret"

    if [[ "{{OPEN_BROWSER}}" == 1 ]]; then
      ( sleep 1 ; xdg-open "http://[::1]:{{PORT}}" ) &
    fi

    env \
      ROSTRA_DEV_MODE=1 \
      RUST_LOG=${RUST_LOG:-rostra=debug,info,iroh=error,mainline=error} \
      systemfd --no-pid -s http::[::1]:{{PORT}} -- \
      cargo watch \
        -d .1 \
        -i 'dev/**' \
        -i 'crates/rostra-web-ui/assets/**' \
        -i 'assets/**' \
        -s " \
          cargo build --all-targets; \
          ./target/debug/rostra \
            --data-dir "$ROSTRA_DATA_DIR" \
            web-ui \
            --listen [::1]:{{PORT}} \
            --skip-xdg-open \
            --reuseport \
            --default-profile rs64xtoy61x4grdcd4d75pazf8dcwrjrm4krqfcfz7oxhmm33ci74o \
          "

# inspect the live development UI with an isolated Chromium profile
ui-inspect *ARGS:
    cargo run --locked -p rostra-ui-preview -- {{ARGS}}
