# nix/modules/home-manager.nix — auto-generated from lava-stack.caixa.lisp
{ config, lib, pkgs, ... }:
let cfg = config.programs.lava-stack; in {
  options.programs.lava-stack = {
    enable = lib.mkEnableOption "lava-stack";
    package = lib.mkOption { type = lib.types.package; default = pkgs.lava-stack or null; };
  };
  config = lib.mkIf cfg.enable { home.packages = [ cfg.package ]; };
}
