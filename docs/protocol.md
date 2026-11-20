# The plugin protocol, version 1

An external plugin is a program. The player starts it, writes JSON objects to its stdin and reads
JSON objects from its stdout; **one object per line, UTF-8, terminated by `\n`**. Nothing is shared
but the wire: no dynamic library, no Rust ABI, no dependency in the player's build.

The protocol version is the number `1`. A plugin and the player agree when both name it; a manifest
or a `ready` with another number is refused with the reason on stderr, and the plugin does not run.
Fields may be added to any object in a later version without a bump (a reader ignores what it does
not know); changing a field's meaning, or removing one, is a new version.

A line is at most 1 MiB. A larger one is logged and skipped.

## Lifecycle

```
player                                   plugin
  spawn ─────────────────────────────────▶
  hello ─────────────────────────────────▶  config, protocol, dry_run
         ◀──────────────────────────────── ready (protocol)
  state ─────────────────────────────────▶  the facts, on change (≤ 4/s)
         ◀──────────────────────────────── status / command / log
  state ─────────────────────────────────▶
  ...
  bye (or the pipe closes; it may be killed) ─▶  exit
```

1. The player starts the program with its working directory set to the plugin directory, stdin and
   stdout piped, stderr inherited (so a plugin's own diagnostics reach the player's terminal).
2. It writes `hello` and waits at most **500 ms** for `ready`. A plugin that does not answer is
   refused and its process is killed. A plugin that needs a socket does not make the handshake
   wait for it: `mpris` starts its bus thread in `hello` and `ready` is written at once, so a bus
   that is down is a retry in the background, never a player that does not start.
3. It writes `state` whenever the now-playing facts change, coalesced to at most one line per
   250 ms. A playing track's `elapsed` is quantised to a quarter of a second; the plugin
   interpolates the position if it needs a smooth clock. A paused or still session sends nothing.
4. At shutdown, or when the plugin is switched off, the player writes `bye` and then kills the
   process. A plugin must not depend on receiving `bye` to leave: the pipe may close or the process
   may be killed at any moment.

The player never waits for a plugin on a frame. The two exceptions are at start-up: `ready`, and a
status plugin's **first** line (at most 200 ms), so a captured first frame of a bar is
deterministic.

## Messages the player writes

### `hello`

Once, before anything else.

```json
{"type":"hello","protocol":1,"plugin":"notify","version":"0.1.0","dry_run":false,
 "config":{"app":"aurora","replace":"true"}}
```

| field | type | meaning |
|---|---|---|
| `protocol` | number | the wire version |
| `plugin` | string | the catalogue name (the manifest's `name`) |
| `version` | string | the plugin's own version, from its manifest |
| `dry_run` | bool | the player is a capture/dry run: do not touch the machine |
| `config` | object of strings | the plugin's own `[[plugin]]` keys, in file order |

`config` is the same table the in-tree plugins received in `init`: unknown keys are the plugin's
own error to report. A plugin that cannot understand its configuration logs an `error` and exits.

### `state`

The player's now-playing facts, whatever moved.

```json
{"type":"state","state":{
  "uri":"file:///music/a.flac","path":"/music/a.flac","title":"Track","artist":"Artist",
  "album":"Album","album_line":"Album","year":"2009","genre":"IDM","label":"n5MD",
  "format":"FLAC · 16/44.1","clock":"13:45:07","date":"26 октября 2026",
  "like":1,"playing":"play","elapsed":12.25,"duration":240.0,
  "queue_len":7,"queue_after":3,"queue_left":1234.5,"rate":44100,"connected":true}}
```

| field | type | meaning |
|---|---|---|
| `uri` | string | the daemon's URI; empty means nothing is playing |
| `path` | string | the local file, when the URI maps to one; empty for a stream |
| `title`,`artist`,`album` | string | the tags as the player reads them |
| `album_line` | string | the footer's release line (the album, plus the artist when it says something new) |
| `year`,`genre`,`label` | string | the tags the bar prints as links; empty when absent |
| `format` | string | `FLAC · 16/44.1`; empty for a stream or a file with no extension |
| `clock`,`date` | string | the player's own wall-clock formatting (`13:45:07`, `26 октября 2026`) |
| `like` | number | `1` liked, `-1` disliked, `0` no opinion |
| `playing` | string | `play`, `pause` or `stop` |
| `elapsed`,`duration` | number | seconds; `elapsed` is quantised to 0.25 s |
| `queue_len` | number | tracks in the queue |
| `queue_after` | number | tracks after the playing one |
| `queue_left` | number | seconds of music left in the queue |
| `rate` | number | the stream's rate in hertz; `0` before the daemon answers |
| `connected` | bool | the player is talking to the daemon this snapshot |

A missing field is its empty value (`""`, `0`, `false`) — the shape is additive. A field this
version does not name is ignored.

### `bye`

```json
{"type":"bye"}
```

The process is about to end. The player may kill it immediately anyway.

### `frame`

To a plugin with the `draw` permission, every frame the player draws:

```json
{"type":"frame","canvas":{"w":260.0,"h":190.0},"time":12.4333,
 "samples":{"rate":48000.0,"data":"AAAA..."}}
```

| field | type | meaning |
|---|---|---|
| `canvas.w`, `canvas.h` | number | the slot's size in logical units — the space a draw op is written in |
| `time` | number | seconds since start: the animation clock, frame-driven in a pinned capture |
| `samples.rate` | number | the stream's rate in hertz (`0` before the daemon answers) |
| `samples.data` | string | the scope window, base64 of little-endian `f32`, oldest first |

`samples` is present only for a plugin that asked for the `audio` permission: it is the same window
the in-tree instruments analyse, and a plugin that never asked is never sent a sample. The player
never waits for the frame to be answered, and the protocol has no timer — a plugin that needs one
runs a thread of its own. The window is sent every frame while it is drawn: eighteen kilobytes of
picture and eight of samples are the price of the process boundary, and a stateful canvas two
processes could disagree about would be worse.

## Messages the plugin writes

### `ready`

The first line, in answer to `hello`.

```json
{"type":"ready","protocol":1}
```

A `ready` naming another protocol is refused. A plugin that cannot start (`config` is wrong, there
is no bus it must have) writes a `log` line with level `error` and exits; the player reports it.

### `status`

One line in the player's status bar. For a `status`-layer plugin only.

```json
{"type":"status","slot":"queue","text":"31 мин · 4 трека"}
```

The `slot` is the *fact*, not a label: two plugins that answer the same question cannot both
appear, and the player's first loaded plugin takes the slot. An empty `text` clears the line; a
status plugin that stops producing a value must say so rather than freeze the last one. The slot in
the manifest and in the message should agree; the manifest's is what the player reserves.

The text is drawn by the bar: the plugin owns the words, never the ink, size or place.

### `command`

Ask the player to do one thing. The set is closed and small — the player's own command enum would
let a plugin clear the queue or change the volume under the listener, and a background action is not
a keystroke.

```json
{"type":"command","command":{"type":"play_pause"}}
```

| `type` | fields | what it means |
|---|---|---|
| `play_pause` | — | play or pause, one step |
| `next` | — | the next track |
| `previous` | — | the previous track |
| `seek_share` | `share` (0.0–1.0) | seek to that share of the track; the player clamps and owns the duration |
| `queue_artist` | `artist`, `count` (0–255) | queue up to `count` more tracks by `artist` |
| `queue_similar` | `count` | queue tracks like the playing one |
| `queue_mood` | `mood`, `count` | queue tracks the tag stack filed under `mood` |
| `append_mood` | `mood`, `count` | append them to the saved playlist named after `mood` |
| `open_facet` | `tag` (`year`/`genre`/`label`) | open the facet sheet for one tag of the playing track |

The player queues at most four commands per plugin per frame; the rest are counted and dropped. An
unknown command is logged, never fatal. A `status`-layer plugin has no frame context and therefore
cannot run commands; the player drops them and says so once.

### `log`

One line on the player's stderr.

```json
{"type":"log","level":"warn","text":"[notify] `notify-send` is not usable"}
```

`level` is `info`, `warn` or `error`; the player prints the text as it is (a plugin prefixes its own
name, which is why the example has `[notify]`). An `error` during `hello` disables the plugin for
the session.

### `draw`

What a painting plugin answers a `frame` with: one frame's primitives, in paint order, in its own
slot-local space. The player paints the newest drawing it has on the **next** frame and drops older
ones; a drawing replaces the previous one entirely, so a plugin that stops answering stops painting
instead of freezing its last picture over the frame.

```json
{"type":"draw","ops":[
  {"op":"waterfall","x":22,"y":0,"w":439,"h":190,"width":90,"height":200,
   "levels":"<base64>","ramp":["panel_inset","accent2","accent","text"]},
  {"op":"rect","x":141,"y":0,"w":1,"h":190,"radius":0,"color":"rule"},
  {"op":"text","text":"20k","x":0,"y":18,"size":8,"color":"text_faint"}
]}
```

| op | fields |
|---|---|
| `rect` | `x`, `y`, `w`, `h`, `radius`, `color` |
| `text` | `text`, `x`, `y`, `size`, `color` |
| `waterfall` | `x`, `y`, `w`, `h`, `width`, `height`, `levels` (base64 of `width × height` bytes, 0..=255), `ramp` (four token names, quiet floor to peak) |

Coordinates and sizes are in the slot's logical units. `color` is a **theme token by name** —
`text`, `text_dim`, `text_faint`, `accent`, `accent2`, `selection`, `panel_inset`, `rule`,
`mood_bright`, `mood_energy`, `mood_calm`, `mood_dark`, `mood_love`, `mood_work` — and an op that
names an unknown one is skipped: a program cannot introduce an ink the theme audit has not measured.
The player clips to the slot and caps the op count exactly as it does for an in-tree plugin, and an
op outside the slot is dropped and counted.

A `waterfall` is a level picture, not a colour picture: the player rasterises it through the four
tokens, so a skin switch recolours it without the plugin being told.

## The manifest

Every plugin directory holds a `plugin.json` beside the program:

```json
{
  "name": "remaining",
  "version": "0.1.0",
  "protocol": 1,
  "layer": "status",
  "slot": "queue",
  "permissions": ["model", "status"],
  "group": "ВИД",
  "about": "сколько осталось до конца очереди",
  "default": true,
  "exec": "aurora-remaining"
}
```

| key | required | meaning |
|---|---|---|
| `name` | yes | the catalogue name: `a-z`, `0-9`, `-`, `_`; unique |
| `version` | no | the plugin's version, quoted in `hello` |
| `protocol` | yes | `1` for this wire |
| `layer` | no | `service` (default: a zero-height block that reacts to states) or `status` (one line in the bar) |
| `slot` | for `status` | the fact the line fills (`queue` for remaining) |
| `permissions` | no | `model`, `commands`, `status`, `config`, `draw`, `audio`; anything else is refused, because a program has no pointer, no keyboard and no files |
| `height` | for `draw` | the slot's height in logical units: the column measures its stack before the first frame arrives |
| `instrument` | no | `{"label": "спектрограф", "press": "whole_track"}`: the word the column prints for the block and what a press on it means |
| `group` | no | the PLUGINS section's category (`ВИД`, `СВЯЗЬ`, …); `ВНЕШНИЕ` when absent |
| `about` | no | the one line under the card's name |
| `default` | no | load it in a run that says nothing (default `false`) |
| `exec` | yes | the program: looked for beside the manifest first, then on `PATH` |

A manifest is user-installed code: the directory is trusted the way `~/.local/bin` is. The
permission list is a declaration the player checks against what it grants — it is not a sandbox the
plugin runs in.

## Discovery

The player reads **plugin roots**:

1. `AURORA_PLUGIN_DIR`, a `PATH`-style list (`:` on Unix, `;` on Windows). An explicitly empty
   value means "no external plugins" — a capture sets it so it does not depend on the machine.
2. otherwise `$XDG_DATA_HOME/aurora/plugins` (`$HOME/.local/share/aurora/plugins`).

A root either *is* a plugin directory (it has `plugin.json`) or contains plugin directories. Each
is read once, in name order. A manifest that does not parse, asks for an unsupported permission or
names a protocol this player does not speak is one line on stderr and a skipped plugin. A name that
collides with a compiled-in plugin, or with an earlier root, is skipped the same way. A missing root
is silence: most machines have no external plugins.

## Determinism

A plugin is not part of `cargo test`'s picture and must not make a capture depend on its own
scheduling. The player helps: `dry_run` disables everything that touches the machine, `ready` is
waited for at start-up, and a status plugin's first line is waited for on the first frame. A plugin
that draws nothing (every one here but `remaining` and `spectroscope`) cannot move a pixel;
`remaining` moves one string, and that string is a function of the queue.

A painting plugin is one frame behind by construction — it is asked about a frame and its drawing is
painted by the next one — and that lag is fixed, not racy: the canvas is a function of the frame
message. The one thing that cannot be pinned is the `audio` window, because its producer is live: a
capture of a spectrogram is a picture of a moment, and the instrument's own record in the player's
repository says so.
