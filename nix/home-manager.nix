{ config, lib, pkgs, ... }:

let
  cfg = config.programs.huffi;
in
{
  options.programs.huffi = {
    enable = lib.mkEnableOption "huffi, a launcher with query-dependent history";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.huffi;
      defaultText = lib.literalExpression "huffi";
      description = "The huffi package to use.";
    };

    enablePreloading = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable huffi preloading via a systemd user service.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.huffi = lib.mkIf cfg.enablePreloading {
      Unit = {
        Description = "Huffi launcher (resident instance)";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };

      Service = {
        ExecStart = "${lib.getExe' cfg.package "huffi"} preload";
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
