{
  description = "Rostra";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox?rev=f721e70163c9c9434d59e811dc09cdc4c7660dba";

    bundlers = {
      url = "github:NixOS/bundlers";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
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
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "rostra";

        flakeboxLib = flakebox.lib.${system} {
          config = {
            github.ci.buildOutputs = [ ".#ci.${projectName}" ];
            just.importPaths = [ "justfile.rostra.just" ];
            just.rules.watch.enable = false;
            toolchain.channel = "latest";
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
      in
      {
        packages = {
          inherit rostra-web-ui;
          default = rostra-web-ui;
          rostra = multiBuild.rostra;
        };

        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          toolchain = toolchainAll;
          packages = [ pkgs.jq ];
          shellHook = ''
            export FLAKEBOX_GIT_LS_TEXT_IGNORE="crates/rostra-web-ui/assets/libs/|crates/rostra-web-ui/assets/icons"
          '';
        };
      }
    );
}
