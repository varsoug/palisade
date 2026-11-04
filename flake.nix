{
  description = "Palisade — open-source GUI client for Palo Alto GlobalProtect VPN";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = self.packages.${system}.palisade;
          palisade = pkgs.callPackage ./nix/package.nix { };
        });

      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = pkgs.callPackage ./nix/dev-shell.nix { };
        });

      nixosModules.default = self.nixosModules.palisade;
      nixosModules.palisade = import ./module.nix;

      checks = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # Eval-only check: building the toplevel drv is not requested, only
          # its .drvPath string is interpolated, so `nix flake check` stays
          # cheap while still type-checking the module and its options.
          eval = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.palisade
              {
                boot.isContainer = true;
                nixpkgs.hostPlatform = system;
                system.stateVersion = "24.05";
                services.palisade = {
                  enable = true;
                  allowedUsers = [ "tester" ];
                  portals = [
                    {
                      name = "Work";
                      portal = "vpn.example.com";
                      username = "alice";
                      auth = "saml";
                    }
                  ];
                };
              }
            ];
          };
        in
        {
          inherit (self.packages.${system}) palisade;
          module-eval = pkgs.runCommand "palisade-module-eval" { } ''
            echo "${eval.config.system.build.toplevel.drvPath}" > "$out"
          '';
        });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixpkgs-fmt);
    };
}
