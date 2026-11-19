# aurora-plugins

The plugins that talk to the rest of the machine, kept out of the player's build:
**notify** (one desktop notification per track change), **scrobble** (listens to ListenBrainz),
**mpris** (media keys, desktop widgets, KDE Connect) and **remaining** (how much music is left in
the queue, as one status line in the bar).

Each one is a **separate program** that `aurora` starts and speaks to over a small JSON-lines
protocol (`docs/protocol.md`). That is the whole point of this repository: changing a plugin
rebuilds a plugin. It does not rebuild the player, does not add a dependency to the player's build
graph, and cannot take the player down with it.

```
aurora ──spawn──▶ aurora-notify   ── notify-send
       │          aurora-scrobble ── curl → ListenBrainz
       │          aurora-mpris    ── D-Bus session bus
       └────────▶ aurora-remaining ── one status line
         stdin/stdout: JSON objects, one per line
```

## Build and install

No dependencies, no network: the four binaries and the protocol crate are `std` only.

```sh
cargo build --release --offline
./install.sh                 # ~/.local/share/aurora/plugins/{notify,scrobble,mpris,remaining}
```

The player looks in `$AURORA_PLUGIN_DIR` (a `PATH`-style list of plugin roots) and falls back to
`$XDG_DATA_HOME/aurora/plugins`. Each plugin directory holds its `plugin.json` and its program; the
manifest's `exec` is looked for beside the manifest first, then on `PATH`.

A Nix package is in `nix/package.nix`:

```nix
aurora-plugins = pkgs.callPackage ./aurora-plugins/nix/package.nix { };
```

and the player needs one variable for it:

```nix
environment.sessionVariables.AURORA_PLUGIN_DIR = "${pkgs.neg.aurora-plugins}/lib/aurora/plugins";
```

## The four

| name | layer | what it does | dependencies |
|---|---|---|---|
| `notify` | service | `notify-send` per track change, grouped by sender; `--print-id`/`--replace-id` for daemons without stack tags | `notify-send` on `PATH` |
| `scrobble` | service | half the track or four minutes counts as a listen; one `curl` POST per listen | `curl`, `LISTENBRAINZ_TOKEN` |
| `mpris` | service | `org.mpris.MediaPlayer2.aurora` over the session bus; PlayPause/Next/Previous come back as commands | none (its own D-Bus wire codec) |
| `remaining` | status | `31 мин · 4 трека` on the bar's queue slot | none |

`scrobble` is off by default (`"default": false` in its manifest): a listener opts into publishing
their listening. The other three are on when installed; the player's PLUGINS section switches
them like any compiled-in plugin, and writes `[[plugin]]` tables into `skins.toml`.

Configuration lives in those tables and arrives in the protocol's `hello`:

```toml
[[plugin]]
name = "notify"
app = "aurora"
tag = "aurora:notify"
replace = "true"

[[plugin]]
name = "mpris"
bus = "org.mpris.MediaPlayer2.aurora"
```

The `mpris` block's `bus` key is the bus name; the plugin refuses one that does not start with
`org.mpris.MediaPlayer2.`. (`name` is the table's plugin name, so the bus name cannot use it.)

## Tests

```sh
cargo test --offline
```

The protocol crate tests the wire and the state parser; every plugin tests its own rule (`notify`'s
one-per-track and argv, `scrobble`'s listen arithmetic and JSON escaping, `remaining`'s plural and
clock, `mpris`'s D-Bus framing and handshake). Nothing needs a bus or a daemon.

## The protocol

`docs/protocol.md` is the normative description: messages, fields, the version rule, the manifest
and discovery. A plugin in another language is a program that reads and writes one JSON object per
line; it does not link against aurora and does not know Rust.
