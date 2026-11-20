# aurora-plugins

The plugins that talk to the rest of the machine, kept out of the player's build:
**notify** (one desktop notification per track change), **scrobble** (listens to ListenBrainz),
**mpris** (media keys, desktop widgets, KDE Connect), **remaining** (how much music is left in
the queue, as one status line in the bar), **autodj** (tops a short queue up with the playing
track's neighbours), **lyrics** (the `.lrc` beside the playing file, as one status line),
**musicbrainz** (the playing recording's release, looked up with one `curl`) and **spectroscope**
(the session's spectrum as an RME-style waterfall, drawn through the canvas).

Each one is a **separate program** that `aurora` starts and speaks to over a small JSON-lines
protocol (`docs/protocol.md`). That is the whole point of this repository: changing a plugin
rebuilds a plugin. It does not rebuild the player, does not add a dependency to the player's build
graph, and cannot take the player down with it.

```
aurora ──spawn──▶ aurora-notify      ── notify-send
       │          aurora-scrobble    ── curl → ListenBrainz
       │          aurora-mpris       ── D-Bus session bus
       │          aurora-autodj      ── queue_similar → the player's own index
       │          aurora-musicbrainz ── curl → MusicBrainz
       │          aurora-spectroscope ── a canvas: the waterfall, its grid and its scale
       └────────▶ aurora-remaining   ── one status line
                  aurora-lyrics      ── one status line
         stdin/stdout: JSON objects, one per line
```

## Build and install

No dependencies, no network: the eight binaries and the protocol crate are `std` only.

```sh
cargo build --release --offline
./install.sh                 # ~/.local/share/aurora/plugins/{notify,scrobble,mpris,remaining,autodj,lyrics,musicbrainz,spectroscope}
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

## The eight

| name | layer | what it does | dependencies |
|---|---|---|---|
| `notify` | service | `notify-send` per track change, grouped by sender; `--print-id`/`--replace-id` for daemons without stack tags | `notify-send` on `PATH` |
| `scrobble` | service | half the track or four minutes counts as a listen; one `curl` POST per listen | `curl`, `LISTENBRAINZ_TOKEN` |
| `mpris` | service | `org.mpris.MediaPlayer2.aurora` over the session bus; PlayPause/Next/Previous come back as commands | none (its own D-Bus wire codec) |
| `remaining` | status | `31 мин · 4 трека` on the bar's queue slot | none |
| `autodj` | service | one `queue_similar` per track when fewer than `min` tracks are left after the playing one | none (the player ranks the neighbours) |
| `lyrics` | status | the verse the playhead is in, out of `<file>.lrc`; an empty line clears the bar instead of freezing the last verse | none |
| `musicbrainz` | status | the playing recording's release: one `curl` per track change, two fields out of the reply | `curl` |
| `spectroscope` | canvas | RME's DigiCheck waterfall: a `draw` of a 90 × 200 level picture, the one-second grid and the frequency scale; the player sends the scope window, and the analysis is this program's own | the `audio` and `draw` grants |

`scrobble` is off by default (`"default": false` in its manifest): a listener opts into publishing
their listening. `autodj` is off as well — it changes what plays next, which is a taste a run that
says nothing must not assume — and so are `musicbrainz` (a lookup is a network call nobody asked
for by installing a status line) and `spectroscope` (an instrument is a choice of column, and the
default column carries the analyser). The other four are on when installed; the player's PLUGINS
section switches them like any compiled-in plugin, and writes `[[plugin]]` tables into
`skins.toml`.

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
clock, `mpris`'s D-Bus framing and handshake, `lyrics`'s `.lrc` parser and the verse the playhead
picks, `musicbrainz`'s tag normalisation, escaping and reply, `spectroscope`'s FFT, its bands and
its level scale). Nothing needs a bus or a daemon.

## The protocol

`docs/protocol.md` is the normative description: messages, fields, the version rule, the manifest
and discovery. A plugin in another language is a program that reads and writes one JSON object per
line; it does not link against aurora and does not know Rust.
