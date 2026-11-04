{ config, lib, pkgs, ... }:

let
  cfg = config.services.palisade;

  portalType = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "Display name shown in the app.";
      };
      portal = lib.mkOption {
        type = lib.types.str;
        example = "vpn.example.com";
        description = "GlobalProtect portal hostname.";
      };
      username = lib.mkOption {
        type = lib.types.str;
        description = "Login username.";
      };
      auth = lib.mkOption {
        type = lib.types.enum [ "password" "saml" ];
        default = "password";
        description = ''
          Authentication mode: `password` (password from the OS keyring) or
          `saml` (SSO via an external browser).
        '';
      };
    };
  };

  # Spawns openconnect through sudo so the app itself runs unprivileged.
  # Only installed/used when allowedUsers is non-empty.
  sudoWrapper = pkgs.writeShellScriptBin "palisade-openconnect" ''
    exec /run/wrappers/bin/sudo -n ${cfg.openconnectPackage}/bin/openconnect "$@"
  '';

  openconnectBin =
    if cfg.allowedUsers != [] then "${sudoWrapper}/bin/palisade-openconnect" else "";

  appConfig = {
    portals = map (p: {
      inherit (p) name portal username;
      auth = if p.auth == "saml" then "Saml" else "Password";
    }) cfg.portals;
    last_portal = null;
    settings = {
      reconnect_on_drop = true;
      autostart = false;
      openconnect_bin = openconnectBin;
    };
  };
in
{
  options.services.palisade = {
    enable = lib.mkEnableOption "Palisade, an open-source GlobalProtect VPN GUI client";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./nix/package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./nix/package.nix { }";
      description = "The palisade package to install.";
    };

    openconnectPackage = lib.mkPackageOption pkgs "openconnect" { };

    keyring = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable GNOME keyring (Secret Service provider for stored passwords).";
    };

    allowedUsers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Users allowed to run OpenConnect without a sudo password. When empty,
        the app must be configured to use an openconnect binary the user may
        execute with root rights by other means.
      '';
    };

    portals = lib.mkOption {
      type = lib.types.listOf portalType;
      default = [ ];
      description = ''
        Declaratively provisioned portals, written to
        /etc/palisade/config.json. The app falls back to this file when the
        user has no config of their own. Passwords are never stored here —
        they live in the login keyring only.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ] ++ lib.optional (cfg.allowedUsers != [ ]) sudoWrapper;

    # openconnect needs /dev/net/tun.
    boot.kernelModules = [ "tun" ];

    services.gnome.gnome-keyring.enable = lib.mkIf cfg.keyring (lib.mkDefault true);

    security.sudo.extraRules = map (user: {
      users = [ user ];
      commands = [
        {
          command = "${cfg.openconnectPackage}/bin/openconnect";
          options = [ "NOPASSWD" ];
        }
      ];
    }) cfg.allowedUsers;

    environment.etc."palisade/config.json".text = builtins.toJSON appConfig;
  };
}
