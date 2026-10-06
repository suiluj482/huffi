{ config, lib, pkgs, ... }:

let
  cfg = config.programs.huffi;

  # ── RON rendering ──────────────────────────────────────────────────────────
  #
  # RON has two object forms: a *struct* is `( key: value )` with bare
  # identifiers as keys, and a *map* is `{ "key": value }` with string keys.
  # huffi's config is structs everywhere except `engine.provider.builtin`
  # (keyed by provider id) and the freeform `extra` blocks, which are maps, so
  # the renderer picks the form per position rather than guessing.

  # JSON string escaping is a subset of RON's, so `toJSON` is enough here.
  ronString = value: builtins.toJSON value;

  # Floats must keep a decimal point to stay floats; ints stay bare.
  ronNumber =
    value:
    if lib.isFloat value then
      let
        text = toString value;
      in
      if lib.hasInfix "." text || lib.hasInfix "e" text then text else "${text}.0"
    else
      toString value;

  # Every value is rendered at an explicit indent level, so nesting lines up
  # without re-indenting already-rendered text.
  padding = level: lib.concatStrings (lib.genList (_: "  ") level);

  ronLeaf =
    value:
    if value == null then
      "None"
    else if lib.isBool value then
      if value then "true" else "false"
    else if lib.isString value then
      ronString value
    else if lib.isInt value || lib.isFloat value then
      ronNumber value
    else
      throw "huffi: cannot render a ${builtins.typeOf value} as RON";

  # A RON struct: `( key: value, … )` with bare identifier keys. `fields` holds
  # `key`/`value` pairs whose values are already rendered.
  ronStruct =
    level: fields:
    let
      pad = padding level;
      body = lib.concatStrings (map (field: "${pad}  ${field.key}: ${field.value},\n") fields);
    in
    if fields == [ ] then "()" else "(\n${body}${pad})";

  # A RON map: `{ "key": value, … }` with string keys, as RON requires for
  # string-keyed maps.
  ronMap =
    level: entries:
    let
      pad = padding level;
      body = lib.concatStrings (map (entry: "${pad}  ${ronString entry.key}: ${entry.value},\n") entries);
    in
    if entries == [ ] then "{}" else "{\n${body}${pad}}";

  # Freeform values (the `extra` blocks): keys are always strings, so nested
  # maps keep the quoted form all the way down. A `null` here is a real JSON
  # null rather than an unset option, so it is kept.
  ronValue =
    level: value:
    if lib.isAttrs value then
      ronMap level (map (key: {
        inherit key;
        value = ronValue (level + 1) value.${key};
      }) (builtins.attrNames value))
    else if lib.isList value then
      "[${lib.concatStringsSep ", " (map (item: ronValue level item) value)}]"
    else
      ronLeaf value;

  # Struct fields for an option set at `level`; unset (null) options are
  # dropped so the field falls back to huffi's default instead of being
  # written as `None`.
  ronFields =
    level: attrs:
    map (key: {
      inherit key;
      value = ronValue (level + 1) attrs.${key};
    }) (builtins.attrNames (lib.filterAttrs (_: value: value != null) attrs));

  # ── Option types ──────────────────────────────────────────────────────────

  unset = lib.types.nullOr lib.types.str;

# huffi reads these as floats, and RON/serde accepts an integer literal there
  # too, so `confidence_k = 2` is just as valid as `confidence_k = 2.0`.
  real = lib.types.nullOr (lib.types.either lib.types.float lib.types.int);

  pathsModule =
    { ... }:
    {
      options = {
        data_dir = lib.mkOption {
          type = unset;
          default = null;
          description = ''
            Huffi data folder. History lives at `<data_dir>/history.json` and
            each provider gets its own `<data_dir>/providers/<provider id>/`
            folder. Defaults to `$XDG_DATA_HOME/huffi`, falling back to
            `~/.local/share/huffi`.
          '';
        };

        socket = lib.mkOption {
          type = unset;
          default = null;
          description = ''
            Control socket path. Defaults to `$XDG_RUNTIME_DIR/huffi.sock`,
            falling back to `/tmp/huffi.sock`.
          '';
        };
      };
    };

  uiModule =
    { ... }:
    {
      options = {
        width = lib.mkOption {
          type = lib.types.nullOr lib.types.ints.unsigned;
          default = null;
          description = "Overlay panel width in pixels. Defaults to 600.";
        };

        height = lib.mkOption {
          type = lib.types.nullOr lib.types.ints.unsigned;
          default = null;
          description = "Overlay panel height in pixels. Defaults to 400.";
        };

        page_size = lib.mkOption {
          type = lib.types.nullOr lib.types.ints.unsigned;
          default = null;
          description = "Results shown per page. Defaults to 10.";
        };

        icon_size = lib.mkOption {
          type = lib.types.nullOr lib.types.ints.unsigned;
          default = null;
          description = "Entry icon size in pixels. Defaults to 24.";
        };

        theme = lib.mkOption {
          type = unset;
          default = null;
          description = ''
            Theme name, resolved from `$XDG_CONFIG_HOME/huffi/themes/<name>/`
            on top of the embedded default theme. Defaults to `"default"`.
          '';
        };
      };
    };

  scoringModule =
    { ... }:
    {
      options = {
        boost_weight = lib.mkOption {
          type = real;
          default = null;
          description = ''
            Weight of a manual boost relative to a normal launch. Boosts push
            ranking hard without counting as that many launches of evidence.
            Defaults to 10.0.
          '';
        };

        boost_samples = lib.mkOption {
          type = lib.types.nullOr lib.types.ints.unsigned;
          default = null;
          description = ''
            Synthetic launch samples a boost counts as toward confidence.
            Defaults to 5.
          '';
        };

        half_life_days = lib.mkOption {
          type = real;
          default = null;
          description = "History half-life in days (exponential decay). Defaults to 14.0.";
        };

        confidence_k = lib.mkOption {
          type = real;
          default = null;
          description = ''
            Confidence smoothing constant k in `n / (n + k)`: how much evidence
            a prefix needs before history is trusted over text score. Defaults
            to 3.0.
          '';
        };

        empty_query_score = lib.mkOption {
          type = real;
          default = null;
          description = "Base score for results shown while the query is empty. Defaults to 0.8.";
        };
      };
    };

  providerModule =
    { ... }:
    {
      options = {
        name = lib.mkOption {
          type = unset;
          default = null;
          description = "Display name shown in the UI. Defaults to the provider id.";
        };

        enabled = lib.mkOption {
          type = lib.types.nullOr lib.types.bool;
          default = null;
          description = "Whether the provider participates in queries. Defaults to true.";
        };

        prefixes = lib.mkOption {
          type = lib.types.nullOr (lib.types.listOf lib.types.str);
          default = null;
          description = ''
            Trigger prefixes. An empty list means the provider is always
            active; the defaults are `["="]` for the calculator and `[":"]`
            for the Unicode provider.
          '';
        };

        prefix_only = lib.mkOption {
          type = lib.types.nullOr lib.types.bool;
          default = null;
          description = ''
            Only call the provider when the query matches one of its prefixes.
          '';
        };

        extra = lib.mkOption {
          type = lib.types.nullOr lib.types.json;
          default = null;
          example = lib.literalExpression ''{ "weight_comment" = 0.9; }'';
          description = ''
            Provider-specific tuning knobs, passed through to the provider
            verbatim. The keys of this map must be quoted strings, because RON
            distinguishes string-keyed maps from structs.
          '';
        };
      };
    };

  providerConfigModule =
    { ... }:
    {
      options = {
        builtin = lib.mkOption {
          type = lib.types.attrsOf (lib.types.submodule providerModule);
          default = { };
          example = lib.literalExpression ''
            {
              desktop = {
                name = "Applications";
                extra."weight_comment" = 0.9;
              };
            }
          '';
          description = "Per-provider overrides, keyed by built-in provider id.";
        };
      };
    };

  externalModule =
    { ... }:
    {
      options = {
        terminal = lib.mkOption {
          type = lib.types.nullOr (lib.types.listOf lib.types.str);
          default = null;
          description = ''
            Argv items prepended to a `Terminal=true` desktop entry's command:
            the terminal binary plus whatever flags it expects before a
            command. Terminals differ here — kitty and gnome-terminal use
            `[ "kitty" "--" ]`, alacritty and xterm use `[ "alacritty" "-e" ]`,
            foot takes none. Defaults to `[ "kitty" "--" ]`.
          '';
        };

        clipboard = lib.mkOption {
          type = unset;
          default = null;
          description = ''
            Clipboard tool used by the calculator and meta providers. Defaults
            to `"wl-copy"`.
          '';
        };
      };
    };

  engineModule =
    { ... }:
    {
      options = {
        scoring = lib.mkOption {
          type = lib.types.submodule scoringModule;
          default = { };
          description = "Ranking-model tuning.";
        };

        provider = lib.mkOption {
          type = lib.types.submodule providerConfigModule;
          default = { };
          description = "Per-provider settings.";
        };

        external = lib.mkOption {
          type = lib.types.submodule externalModule;
          default = { };
          description = "External binaries huffi shells out to.";
        };
      };
    };

  # ── Rendering ──────────────────────────────────────────────────────────────

  # Every block except `engine.provider.builtin` is a RON struct; that one is a
  # map keyed by provider id, and each provider's `extra` is a freeform map.
  settingsToRon =
    settings:
    let
      engine = settings.engine;
      builtin = engine.provider.builtin;
    in
    ronStruct 0 [
      {
        key = "paths";
        value = ronStruct 1 (ronFields 1 settings.paths);
      }
      {
        key = "ui";
        value = ronStruct 1 (ronFields 1 settings.ui);
      }
      {
        key = "engine";
        value = ronStruct 1 [
          {
            key = "scoring";
            value = ronStruct 2 (ronFields 2 engine.scoring);
          }
          {
            key = "provider";
            value = ronStruct 2 [
              {
                key = "builtin";
                value = ronMap 3 (map (id: {
                  key = id;
                  value = ronStruct 4 (ronFields 4 builtin.${id});
                }) (builtins.attrNames builtin));
              }
              {
                key = "external";
                value = ronStruct 3 (ronFields 3 engine.external);
              }
            ];
          }
        ];
      }
    ];
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
      example = lib.literalExpression "./huffi/config.ron";
      description = ''
        Path to a huffi config file (RON) to install to
        `~/.config/huffi/config.ron`. Takes precedence over
        [](#opt-programs.huffi.settings).
      '';
    };

    settings = lib.mkOption {
      type = lib.types.submodule ({ ... }: {
        options = {
          paths = lib.mkOption {
            type = lib.types.submodule pathsModule;
            default = { };
            description = "File system locations: data folder and control socket.";
          };

          ui = lib.mkOption {
            type = lib.types.submodule uiModule;
            default = { };
            description = "Window geometry and entry-list rendering.";
          };

          engine = lib.mkOption {
            type = lib.types.submodule engineModule;
            default = { };
            description = "Ranking, providers, and external binaries.";
          };
        };
      });
      default = { };
      example = lib.literalExpression ''
        {
          ui = {
            width = 700;
            page_size = 15;
          };
          engine = {
            scoring.boost_weight = 4.0;
            provider.builtin.desktop.extra."weight_comment" = 0.9;
            external.terminal = [ "foot" "--" ];
          };
        }
      '';
      description = ''
        huffi configuration, rendered as RON to `~/.config/huffi/config.ron`.
        Only the keys you set are written; everything else keeps its default.
        Ignored when [](#opt-programs.huffi.configFile) is set.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.file.".config/huffi/config.ron" = lib.mkIf (cfg.configFile != null || cfg.settings != { }) (
      if cfg.configFile != null then
        { source = cfg.configFile; }
      else
        { source = pkgs.writeText "config.ron" (settingsToRon cfg.settings); }
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
