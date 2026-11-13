{
  description = "keyko: encrypted secrets in git with SOPS + age, shared with your team";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      build = pkgs: { pname, package, wrap ? false }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" package ];
          # The integration tests need sops; run them for the CLI, where it is wrapped in anyway.
          nativeCheckInputs = [ pkgs.sops pkgs.git ];
          cargoTestFlags = [ "-p" package ];
          KEYKO_REQUIRE_SOPS = "1";
          nativeBuildInputs = pkgs.lib.optional wrap pkgs.makeWrapper;
          # keyko calls sops; the wrapper puts the nixpkgs one on its PATH.
          postInstall = pkgs.lib.optionalString wrap ''
            wrapProgram $out/bin/keyko --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.sops ]}
          '';
          meta.mainProgram = pname;
        };
    in {
      packages = forEachSystem (pkgs: rec {
        keyko = build pkgs { pname = "keyko"; package = "keyko"; wrap = true; };
        keyko-server = build pkgs { pname = "keyko-server"; package = "keyko-server"; };
        default = keyko;
      });

      apps = forEachSystem (pkgs: {
        default = { type = "app"; program = "${self.packages.${pkgs.system}.keyko}/bin/keyko"; };
      });

      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer pkgs.sops pkgs.age ];
          KEYKO_REQUIRE_SOPS = "1";
        };
      });
    };
}
