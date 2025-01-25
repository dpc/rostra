{
  description = "Rostra";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox?rev=0fe114c0bbc3445e656b93fc1548d7b0924f00cb";
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
            just.importPaths = [ "justfile.rostra.just" ];
            just.rules.watch.enable = false;
          };
        };

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

        rostra-wrapper = pkgs.writeShellScriptBin "rostra" ''
          if [ -z "$1" ]; then
            ${multiBuild.rostra}/bin/rostra web-ui
          else
            ${multiBuild.rostra}/bin/rostra "$@"
          fi
        '';
      in
      {
        packages = {
          default = rostra-wrapper;
          rostra-raw = multiBuild.rostra;
        };

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          packages = [ pkgs.jq ];
        };
      }
    );
}
