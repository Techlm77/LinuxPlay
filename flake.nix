{
  description = "LinuxPlay — low-latency remote desktop for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in
    {
      packages = forAllSystems (pkgs:
        let
          gst = pkgs.gst_all_1;
          # GStreamer plugins the binaries load at runtime (capture, codecs,
          # audio). The wrapper below puts them on GST_PLUGIN_SYSTEM_PATH.
          gstPlugins = [
            gst.gstreamer
            gst.gst-plugins-base
            gst.gst-plugins-good
            gst.gst-plugins-bad
            gst.gst-plugins-ugly
            gst.gst-libav
            pkgs.pipewire
          ];
        in
        rec {
          linuxplay = pkgs.rustPlatform.buildRustPackage {
            pname = "linuxplay";
            version = "2.0.0-dev";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = with pkgs; [
              pkg-config
              cmake
              wrapGAppsHook4
              rustPlatform.bindgenHook
            ];
            buildInputs = with pkgs; [
              gtk4
              libadwaita
              gst.gstreamer
              gst.gst-plugins-base
              libGL
            ];

            # Make the runtime GStreamer plugins discoverable.
            postFixup = ''
              for bin in $out/bin/linuxplay $out/bin/linuxplay-host; do
                [ -e "$bin" ] || continue
                wrapProgram "$bin" \
                  --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : \
                  "${pkgs.lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins}"
              done
            '';

            meta = with pkgs.lib; {
              description = "Low-latency remote desktop for Linux (WebRTC + GStreamer)";
              homepage = "https://github.com/Techlm77/LinuxPlay";
              license = licenses.gpl3Plus;
              platforms = platforms.linux;
            };
          };
          default = linuxplay;
        });

      devShells = forAllSystems (pkgs:
        let gst = pkgs.gst_all_1; in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              cargo
              rustc
              pkg-config
              cmake
              rustPlatform.bindgenHook
            ];
            buildInputs = with pkgs; [
              gtk4
              libadwaita
              gst.gstreamer
              gst.gst-plugins-base
              gst.gst-plugins-good
              gst.gst-plugins-bad
              gst.gst-libav
              pkgs.pipewire
              libGL
            ];
          };
        });
    };
}
