{
  description = "Rostra";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox?rev=f96cbeafded56bc6f5c27fbd96e4fcc78b8a8861";

    bundlers = {
      url = "github:NixOS/bundlers";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      nixpkgs-unstable,
      flake-utils,
      flakebox,
      bundlers,
    }:
    {
      bundlers = bundlers.bundlers;
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs-unstable = nixpkgs-unstable.legacyPackages.${system};
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "rostra";

        flakeboxLib = flakebox.lib.${system} {
          config = {
            github.ci.buildOutputs = [ ".#ci.${projectName}" ];
            just.importPaths = [ "justfile.rostra.just" ];
            just.rules.watch.enable = false;
            toolchain.channel = "latest";
            rust.rustfmt.enable = false;
          };
        };

        toolchainArgs = {
          extraRustFlags = "-Z threads=0";
        };

        stdToolchains = (flakeboxLib.mkStdToolchains (toolchainArgs // { }));

        toolchainAll = (
          flakeboxLib.mkFenixToolchain (
            toolchainArgs
            // {
              targets = pkgs.lib.getAttrs [ "default" ] (flakeboxLib.mkStdTargets { });
            }
          )
        );

        buildPaths = [
          "Cargo.toml"
          "Cargo.lock"
          "crates"
          "crates/.*"
          ".*/Cargo.toml"
          ".*\.rs"
        ];

        buildSrc = flakeboxLib.filterSubPaths {
          root = builtins.path {
            name = projectName;
            path = ./.;
          };
          paths = buildPaths;
        };

        multiBuild =
          (flakeboxLib.craneMultiBuild {
            toolchains = stdToolchains;
          })
            (
              craneLib':
              let
                craneLib = (
                  craneLib'.overrideArgs {
                    pname = projectName;
                    src = buildSrc;
                    nativeBuildInputs = [ ];
                  }
                );
              in
              rec {
                rostraDeps = craneLib.buildDepsOnly { };
                rostra = craneLib.buildPackage {
                  meta.mainProgram = "rostra";
                  cargoArtifacts = rostraDeps;

                  preBuild = ''
                    export ROSTRA_SHARE_DIR=$out/share
                  '';
                };
              }
            );

        rostra-web-ui = pkgs.writeShellScriptBin "rostra-web-ui" ''
          ${multiBuild.rostra}/bin/rostra web-ui "$@"
        '';

        rostra-web-ui-tor = pkgs.writeShellScriptBin "rostra-web-ui-tor" ''
          ${rostra-tor}/bin/rostra-tor web-ui "$@"
        '';

        rostra-tor = pkgs.writeShellScriptBin "rostra-tor" ''
          set -e

          # Create temporary directory for Unix socket
          rostra_tmpdir=$(mktemp --tmpdir --directory rostra-ui-XXXX)
          export ROSTRA_LISTEN="''${rostra_tmpdir}/ui.sock"

          # Separate cleanup functions
          cleanup_tempdir() { rm -rf "''${rostra_tmpdir}" 2>/dev/null || true; }
          trap cleanup_tempdir EXIT


          # Start rostra web-ui with oniux (Tor proxy) in background
          ${pkgs-unstable.oniux}/bin/oniux ${multiBuild.rostra}/bin/rostra "$@" &
          rostra_pid=$!

          cleanup_rostra() { kill -9 "$rostra_pid" 2>/dev/null || true; }
          trap cleanup_rostra EXIT

          # Wait for Unix socket to be created
          timeout=30
          while [ $timeout -gt 0 ] && [ ! -S "''${ROSTRA_LISTEN}" ]; do
            sleep 0.1
            timeout=$((timeout - 1))
          done

          if [ ! -S "''${ROSTRA_LISTEN}" ]; then
            echo "Error: Unix socket was not created within timeout"
            exit 1
          fi

          # Find an available TCP port (starting from 3378)
          tcp_port=3378
          while ${pkgs.netcat}/bin/nc -z localhost $tcp_port 2>/dev/null; do
            tcp_port=$((tcp_port + 1))
          done

          echo "Forwarding TCP port $tcp_port to Unix socket"

          # Start socat to forward TCP to Unix socket
          ${pkgs.socat}/bin/socat TCP-LISTEN:$tcp_port,reuseaddr,fork UNIX-CONNECT:"''${ROSTRA_LISTEN}" &
          socat_pid=$!

          cleanup_socat() { kill -9 "$socat_pid" 2>/dev/null || true; }
          trap cleanup_socat EXIT

          # Give socat a moment to start
          sleep .1

          ${pkgs.xdg-utils}/bin/xdg-open "http://127.0.0.1:$tcp_port" || {
            echo "Failed to open browser. Please navigate to http://127.0.0.1:$tcp_port manually"
          }

          wait $rostra_pid
        '';
      in
      {
        packages = {
          inherit rostra-web-ui rostra-tor rostra-web-ui-tor;
          default = rostra-web-ui;
          rostra = multiBuild.rostra;
        };

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          toolchain = toolchainAll;
          packages = [
            pkgs.jq
            pkgs.systemfd
            (pkgs-unstable.callPackage ./nix/pkgs/wild.nix { })
          ];
          shellHook = ''
            export FLAKEBOX_GIT_LS_TEXT_IGNORE="crates/rostra-web-ui/assets/libs/|crates/rostra-web-ui/assets/icons"
            export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--ld-path=wild"
          '';
        };
      }
    );
}
