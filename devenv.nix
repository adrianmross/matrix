{ pkgs, ... }:

{
  packages = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    pkg-config
    openssl
    sqlite
    git
    curl
    jq
    ripgrep
  ];

  enterShell = ''
    echo "matrix dev env"
    echo "  cargo test"
    echo "  cargo clippy --all-targets -- -D warnings"
  '';

  tasks = {
    "dev:fmt".exec = "cargo fmt --check";
    "dev:test".exec = "cargo test";
    "dev:clippy".exec = "cargo clippy --all-targets -- -D warnings";
    "dev:build".exec = "cargo build --release";
    "dev:validate".exec = "cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings";
  };
}
