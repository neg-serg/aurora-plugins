# What this repository does. `just` with no arguments lists the recipes.
#
# No recipe pins `CARGO_HOME` or fetches from the network: there are no third-party crates, so
# every build is `cargo ... --offline` and a machine with no network can build the plugins.

# Build the four release binaries.
build:
    cargo build --release --offline

# The protocol crate and each plugin's own rule (listen arithmetic, plural, argv, D-Bus framing).
test:
    cargo test --offline

# Install the programs where the player looks when AURORA_PLUGIN_DIR is unset.
install: build
    ./install.sh

# `just show remaining` — the wire, as it comes: paste a hello and a state, read the answer.
show name:
    #!/usr/bin/env bash
    set -eu
    cargo build --release --offline --bin aurora-{{name}}
    printf '%s\n' '{"type":"hello","protocol":1,"plugin":"{{name}}","version":"0.1.0","dry_run":true,"config":{}}'
    printf '%s\n' '{"type":"state","state":{"uri":"file:///tmp/a.flac","title":"Track","artist":"Artist","album":"Album","playing":"play","elapsed":40,"duration":180,"queue_len":3,"queue_left":420}}'
    printf '%s\n' '{"type":"bye"}'
