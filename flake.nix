{
  description = "Palisade — open-source GUI client for Palo Alto GlobalProtect VPN";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      evalConfig = { system, extraModules }: nixpkgs.lib.nixosSystem {
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
        ] ++ extraModules;
      };
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
          # Eval-only checks: assertions are forced via checkAssertWarn without
          # building a toplevel, so `nix flake check` stays cheap while still
          # type-checking the module and verifying generated config fields.
          defaultEval = evalConfig {
            inherit system;
            extraModules = [
              ({ config, ... }:
                let
                  text = builtins.unsafeDiscardStringContext config.environment.etc."palisade/config.json".text;
                  json = builtins.fromJSON text;
                  portal = builtins.head json.portals;
                in
                {
                  assertions = [
                    { assertion = text == builtins.toJSON json; message = "default palisade config text must equal generated JSON"; }
                    { assertion = portal.auth == "Saml"; message = "default portal must use Saml auth"; }
                    { assertion = portal.backend == "Openconnect"; message = "default portal must use Openconnect"; }
                    { assertion = portal.reuse_credentials == true; message = "default portal must reuse credentials"; }
                    { assertion = json.settings.openconnect_bin != "" && nixpkgs.lib.hasSuffix "/bin/palisade-openconnect" json.settings.openconnect_bin; message = "default openconnect_bin must use the sudo wrapper"; }
                    { assertion = json.settings.gpclient_bin == ""; message = "default gpclient_bin must be empty"; }
                    { assertion = json.last_portal == null; message = "last_portal must stay null"; }
                  ];
                })
            ];
          };

          gpEval = evalConfig {
            inherit system;
            extraModules = [
              {
                services.palisade = {
                  backend = "gpclient";
                  reuseCredentials = false;
                  connectAtStartup = true;
                  closeToTray = true;
                  portals = nixpkgs.lib.mkForce [
                    {
                      name = "GpPortal";
                      portal = "vpn.example.com";
                      username = "alice";
                    }
                    {
                      name = "OcPortal";
                      portal = "vpn2.example.com";
                      username = "bob";
                      backend = "openconnect";
                    }
                  ];
                };
              }
              ({ config, ... }:
                let
                  text = builtins.unsafeDiscardStringContext config.environment.etc."palisade/config.json".text;
                  json = builtins.fromJSON text;
                  portals = json.portals;
                  settings = json.settings;
                in
                {
                  assertions = [
                    { assertion = text == builtins.toJSON json; message = "gpclient palisade config text must equal generated JSON"; }
                    { assertion = builtins.length portals == 2; message = "gpclient eval must provision two portals"; }
                    { assertion = (builtins.elemAt portals 0).backend == "Gpclient"; message = "portal without override must use service-level Gpclient backend"; }
                    { assertion = (builtins.elemAt portals 1).backend == "Openconnect"; message = "portal with override must use Openconnect"; }
                    { assertion = nixpkgs.lib.all (p: p.reuse_credentials == false) portals; message = "both portals must not reuse credentials"; }
                    { assertion = settings.connect_at_startup == true; message = "connect_at_startup must be enabled"; }
                    { assertion = settings.close_to_tray == true; message = "close_to_tray must be enabled"; }
                    { assertion = settings.gpclient_bin != ""; message = "gpclient_bin must be set when gpclient backend is enabled"; }
                    { assertion = json.last_portal == null; message = "last_portal must stay null"; }
                  ];
                })
            ];
          };
          # Force NixOS assertion checking without building a toplevel.
          checked = nixpkgs.lib.asserts.checkAssertWarn
            (defaultEval.config.assertions ++ gpEval.config.assertions)
            (defaultEval.config.warnings ++ gpEval.config.warnings)
            "assertions passed";
        in
        {
          inherit (self.packages.${system}) palisade;
          module-eval = pkgs.runCommand "palisade-module-eval" { } ''
            echo ${nixpkgs.lib.escapeShellArg checked} > "$out"
            echo ${nixpkgs.lib.escapeShellArg (builtins.unsafeDiscardStringContext defaultEval.config.environment.etc."palisade/config.json".text)} >> "$out"
            echo ${nixpkgs.lib.escapeShellArg (builtins.unsafeDiscardStringContext gpEval.config.environment.etc."palisade/config.json".text)} >> "$out"
          '';
        });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixpkgs-fmt);
    };
}
