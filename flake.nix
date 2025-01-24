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
      in
      {
        packages.default = multiBuild.rostra;

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          packages = [ pkgs.jq ];
        };
      }
    );
}
