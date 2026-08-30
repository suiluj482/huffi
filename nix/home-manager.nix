{ config, lib, pkgs, ... }:

let
  cfg = config.programs.huffi;
  tomlFormat = pkgs.formats.toml { };
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

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = lib.literalExpression "./huffi/config.toml";
      description = ''
        Path to a huffi config file (TOML) to install to
        `~/.config/huffi/config.toml`. Takes precedence over
        [](#opt-programs.huffi.settings).
      '';
    };

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = { };
      example = {
        ui.width = 700;
        ui.page_size = 15;
        engine.scoring.boost_weight = 4.0;
        engine.provider.desktop.weight_comment = 0.9;
        engine.external.terminal = [ "foot", "--" ];
      };
      description = ''
        huffi configuration, rendered as TOML to `~/.config/huffi/config.toml`.
        The keys mirror the sections of huffi's config file (`paths`, `ui`,
        and the `[engine.*]` tables for scoring, providers, and external
        binaries). Ignored when
        [](#opt-programs.huffi.configFile) is set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.file.".config/huffi/config.toml" = lib.mkIf (cfg.configFile != null || cfg.settings != { }) (
      if cfg.configFile != null then
        { source = cfg.configFile; }
      else
        { source = tomlFormat.generate "config.toml" cfg.settings; }
    );

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
