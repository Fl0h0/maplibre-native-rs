{
  description = "Rust dev shell with TLS + Vulkan";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default;

        vulkanLibPath = pkgs.lib.makeLibraryPath [
          pkgs.vulkan-loader
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain

            # TLS
            pkgs.openssl
            pkgs.pkg-config
            pkgs.cacert

            # HTTP / compression dependencies
            pkgs.curl
            pkgs.zlib

            # Vulkan
            pkgs.vulkan-loader
            pkgs.vulkan-headers
            pkgs.vulkan-validation-layers

            # Often handy for Vulkan/graphics work
            pkgs.shaderc
          ];

          LD_LIBRARY_PATH = vulkanLibPath;

          # Helpful env vars
          shellHook = ''
            export RUST_BACKTRACE=1

            # Make sure Rust TLS crates can find system certs
            export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
            export SSL_CERT_DIR=${pkgs.cacert}/etc/ssl/certs

            # Enable Vulkan validation layers in debug runs (optional)
            export VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation
          '';
        };
      }
    );
}
