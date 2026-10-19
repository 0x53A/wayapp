{ pkgs ? import <nixpkgs> {}, withHeadless ? false }:
pkgs.mkShell {
  nativeBuildInputs = (with pkgs; [ pkg-config rustc cargo rustfmt clippy ripgrep ])
    ++ pkgs.lib.optionals withHeadless (with pkgs; [ sway grim ]);
  buildInputs = with pkgs; [ libxkbcommon wayland vulkan-loader libglvnd ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland vulkan-loader libglvnd ])
    + ":/run/opengl-driver/lib";
}
