{ lib
, rustPlatform
, pkg-config
, makeBinaryWrapper
, wrapGAppsHook4
, wayland
, wayland-protocols
, libxkbcommon
, vulkan-loader
, libGL
, mesa
, libxcursor
, libxrandr
, libxi
, gtk4
, gtk4-layer-shell
, librsvg
}:

rustPlatform.buildRustPackage rec {
  pname = "huffi";
  version = "0.1.0";

  src = lib.cleanSource ./..;

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeBinaryWrapper
    wrapGAppsHook4
  ];

  buildInputs = [
    wayland
    wayland-protocols
    libxkbcommon
    vulkan-loader
    libGL
    mesa
    libxcursor
    libxrandr
    libxi
    gtk4
    gtk4-layer-shell
    librsvg
  ];

  postFixup = ''
    wrapProgram $out/bin/huffi-ui \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath buildInputs}
    wrapProgram $out/bin/huffi-ui-gtk \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath buildInputs} \
      --set GSK_RENDERER gl
  '';

  meta = with lib; {
    description = "A launcher that learns what you meant, not just what you use";
    license = licenses.gpl3Only;
    platforms = platforms.linux;
    mainProgram = "huffi";
  };
}
