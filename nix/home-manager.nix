{ config, lib, pkgs, ... }:

let
  cfg = config.programs.huffi;
in
{
  options.programs.huffi = {
    enable = lib.mkEnableOption "huffi, an adaptive application launcher";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.huffi;
      defaultText = lib.literalExpression "huffi";
      description = "The huffi package to use.";
    };

    daemon.enable = lib.mkEnableOption "huffi-daemon as a systemd user service";
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.huffi-daemon = lib.mkIf cfg.daemon.enable {
      Unit = {
        Description = "Huffi launcher daemon";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };

      Service = {
        ExecStart = lib.getExe' cfg.package "huffi-daemon";
        Restart = "on-failure";
        RestartSec = 1;
        KillMode = "process";
      };

      Install = {
        WantedBy = [ "graphical-session.target" ];
      };
    };
  };
}
