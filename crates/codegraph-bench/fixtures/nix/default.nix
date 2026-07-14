{ pkgs, lib }:
let
  helper = import ./foo.nix;
  pkg = pkgs.callPackage ./bar.nix { };
  greeting = "hello";
in
{
  inherit lib;

  imports = [ ./foo.nix ./bar.nix ];

  build = { src }: pkgs.mkDerivation {
    name = "demo";
    inherit src;
  };

  value = helper;
  named = greeting;
}
