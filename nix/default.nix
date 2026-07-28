{ lib
, rustPlatform
, pkg-config
, makeBinaryWrapper
, wayland
, wayland-protocols
, libxkbcommon
, vulkan-loader
, libGL
, mesa
, libxcursor
, libxrandr
, libxi
}:

rustPlatform.buildRustPackage rec {
  pname = "huffi";
  version = "0.1.0";

  src = lib.cleanSource ./..;

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config makeBinaryWrapper ];

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
  ];

  postInstall = ''
    wrapProgram $out/bin/huffi-ui \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath buildInputs}
  '';

  meta = with lib; {
    description = "A launcher that learns what you meant, not just what you use";
    license = licenses.gpl3Only;
    platforms = platforms.linux;
    mainProgram = "huffi";
  };
}
