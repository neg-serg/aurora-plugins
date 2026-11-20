#!/usr/bin/env bash
# Install the seven programs into the player's plugin directory.
#
# The layout is the one the player reads: one directory per plugin, with `plugin.json` and the
# program beside it. `AURORA_PLUGIN_DIR` may point anywhere; this default is where the player looks
# when the variable is unset.
set -euo pipefail
cd "$(dirname "$0")"

: "${XDG_DATA_HOME:=${HOME}/.local/share}"
dest="${AURORA_PLUGIN_DIR_INSTALL:-${XDG_DATA_HOME}/aurora/plugins}"

for name in notify scrobble mpris remaining autodj lyrics musicbrainz; do
  install -d "${dest}/${name}"
  install -m 0755 "target/release/aurora-${name}" "${dest}/${name}/aurora-${name}"
  install -m 0644 "crates/${name}/plugin.json" "${dest}/${name}/plugin.json"
  echo "installed ${dest}/${name}"
done

echo
echo "The player reads ${dest} by default; another root is AURORA_PLUGIN_DIR=${dest}."
echo "A plugin is also disabled in ~/.config/aurora/skins.toml if an older removal wrote"
echo "[[plugin]] enabled = false there — the PLUGINS section is the switch."
