{
  description = "Flakebox Project template";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      flakebox,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "rostra";

        flakeboxLib = flakebox.lib.${system} {
          config = {
            github.ci.buildOutputs = [ ".#ci.${projectName}" ];
            just.rules.watch.content = pkgs.lib.mkForce ''
              # run and restart on changes
              watch *ARGS="":
                #!/usr/bin/env bash
                set -euo pipefail

                if [ ! -f Cargo.toml ]; then
                  cd {{invocation_directory()}}
                fi

                mkdir -p dev
                if [ ! -e "dev/secret" ]; then
                  cargo run gen-id | jq -r '.secret' > dev/secret
                fi

                env \
                  ROSTRA_DEV_MODE=1 \
                  RUST_LOG=rostra=debug,info,iroh=error,mainline=error \
                  cargo watch \
                    -i 'dev/**' \
                    -i 'crates/rostra/assets/**' \
                    -s " \
                      cargo run -- \
                        --data-dir ./dev/ \
                        web-ui \
                        --listen [::1]:2345 \
                        --skip-xdg-open \
                        --secret-file dev/secret \
                        --reuseport {{ARGS}} \
                      "
            '';

          };
        };

        buildPaths = [
          "Cargo.toml"
          "Cargo.lock"
          "src"
        ];

        buildSrc = flakeboxLib.filterSubPaths {
          root = builtins.path {
            name = projectName;
            path = ./.;
          };
          paths = buildPaths;
        };

        multiBuild = (flakeboxLib.craneMultiBuild { }) (
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
          {
            ${projectName} = craneLib.buildPackage { };
          }
        );
      in
      {
        packages.default = multiBuild.${projectName};

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          packages = [ pkgs.jq ];
        };
      }
    );
}
