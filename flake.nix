{
  description = "tierkey: secrets in git with SOPS + age, shared with your team through the repository, with an optional key server on Tailscale";

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
          TIERKEY_REQUIRE_SOPS = "1";
          nativeBuildInputs = pkgs.lib.optional wrap pkgs.makeWrapper;
          # tierkey calls sops; the wrapper puts the nixpkgs one on its PATH.
          postInstall = pkgs.lib.optionalString wrap ''
            wrapProgram $out/bin/tierkey --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.sops ]}
          '';
          meta.mainProgram = pname;
        };
    in {
      packages = forEachSystem (pkgs: rec {
        tierkey = build pkgs { pname = "tierkey"; package = "tierkey"; wrap = true; };
        tierkey-server = build pkgs { pname = "tierkey-server"; package = "tierkey-server"; };
        default = tierkey;
      });

      apps = forEachSystem (pkgs: {
        default = { type = "app"; program = "${self.packages.${pkgs.system}.tierkey}/bin/tierkey"; };
      });

      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer pkgs.sops pkgs.age ];
          TIERKEY_REQUIRE_SOPS = "1";
        };
      });
    };
}
