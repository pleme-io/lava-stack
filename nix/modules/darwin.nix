# nix/modules/darwin.nix — auto-generated from lava-stack.caixa.lisp
{ config, lib, pkgs, ... }:
let cfg = config.services.lava-stack; in {
  options.services.lava-stack = {
    enable = lib.mkEnableOption "lava-stack";
    package = lib.mkOption { type = lib.types.package; default = pkgs.lava-stack or null; };
  };
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
