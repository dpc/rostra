{
  description = "Rostra";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox?rev=5e9ce550fb989f1311547ee09301315cc311ba3b";

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
          packages = [
            pkgs.jq
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
