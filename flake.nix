{
  description = "dpc-note-taker";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    flakebox.url = "github:rustshop/flakebox";
  };

  outputs = { self, nixpkgs, flake-utils, flakebox }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "dpc-note-taker";

        flakeboxLib = flakebox.lib.mkLib pkgs {
          config = {
            github.ci.buildOutputs = [ ".#ci.${projectName}" ];
            just.importPaths = [ "justfile.custom.just" ];
            just.rules.watch.enable = false;
          };
        };

        buildPaths = [
          "Cargo.toml"
          "Cargo.lock"
          "crates"
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
            craneLib = craneLib'.overrideArgs {
              pname = projectName;
              src = buildSrc;
              nativeBuildInputs = [ pkgs.pkg-config ];
              buildInputs = [
                pkgs.libxkbcommon
                pkgs.wayland
                pkgs.xorg.libX11
                pkgs.xorg.libXcursor
                pkgs.xorg.libXrandr
                pkgs.xorg.libXi
                pkgs.libGL
                pkgs.vulkan-loader
              ];
            };
          in
          rec {
            workspaceDeps = craneLib.buildWorkspaceDepsOnly { };

            workspace = craneLib.buildWorkspace {
              cargoArtifacts = workspaceDeps;
            };

            tests = craneLib.cargoNextest {
              cargoArtifacts = workspace;
            };

            clippy = craneLib.cargoClippy {
              cargoArtifacts = workspaceDeps;
            };

            ${projectName} = craneLib.buildPackage {
              cargoArtifacts = workspaceDeps;
            };
          }
        );
      in
      {
        packages.default = multiBuild.${projectName};
        legacyPackages = multiBuild;

        devShells = flakeboxLib.mkShells {
          packages = [
            pkgs.pkg-config
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.xorg.libX11
            pkgs.xorg.libXcursor
            pkgs.xorg.libXrandr
            pkgs.xorg.libXi
            pkgs.libGL
            pkgs.vulkan-loader
          ];
          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
              pkgs.libxkbcommon
              pkgs.wayland
              pkgs.libGL
              pkgs.vulkan-loader
              pkgs.xorg.libX11
              pkgs.xorg.libXcursor
              pkgs.xorg.libXrandr
              pkgs.xorg.libXi
            ]}:$LD_LIBRARY_PATH"
          '';
        };
      }
    );
}
