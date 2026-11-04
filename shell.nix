# Compatibility wrapper so plain `nix-shell` gets the same environment as
# `nix develop` (for flake-less workflows).
{ pkgs ? import <nixpkgs> { } }:

pkgs.callPackage ./nix/dev-shell.nix { }
