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
          cargo run -- \
            --data-dir "$ROSTRA_DATA_DIR" \
            web-ui \
            --listen [::1]:{{PORT}} \
            --skip-xdg-open \
            --default-profile rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy \
            --secret-file "$ROSTRA_DATA_DIR/secret" \
            --reuseport \
          "
