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

# run and restart on changes
dev PORT="2345":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ ! -f Cargo.toml ]; then
      cd {{invocation_directory()}}
    fi

    export ROSTRA_DATA_DIR
    ROSTRA_DATA_DIR=dev/{{PORT}}

    mkdir -p "$ROSTRA_DATA_DIR"
    if [ ! -e "$ROSTRA_DATA_DIR/secret" ]; then
      cargo run gen-id | jq -r '.secret' > "$ROSTRA_DATA_DIR/secret"
    fi

    ( sleep 1 ; xdg-open "http://[::1]:{{PORT}}" ) &

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
