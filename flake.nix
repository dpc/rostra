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
                env ROSTRA_DEV_MODE=1 RUST_LOG=info,iroh=error cargo watch -i 'crates/rostra/assets/**' -s "cargo run web-ui --listen [::1]:2345 --reuseport {{ARGS}} --skip-xdg-open"
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

        devShells = flakeboxLib.mkShells { };
      }
    );
}
