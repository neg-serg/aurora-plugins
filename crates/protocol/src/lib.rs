//! The wire between `aurora` and an external plugin.
//!
//! One object per line, UTF-8, both directions. The player starts the plugin's process, writes
//! `hello` and then `state` lines to its stdin, and reads `ready`, `status`, `command`, `log` and
//! `bye` lines from its stdout. The protocol is the whole ABI: nothing here links against the
//! player, and the player links against nothing here. `docs/protocol.md` is the normative
//! description, including the field list and the version rule.
//!
//! Three properties are the reason this is a process and not a shared library:
//!
//! * a plugin that crashes is a plugin that stops answering — the player keeps playing;
//! * a plugin may use any dependency it likes (a D-Bus client, an HTTP client) without entering
//!   the player's build graph;
//! * a plugin is a program, so it is tried from a shell by piping two lines into it.
//!
//! The runtime half ([`Sidecar`], [`Sink`], [`serve`]) is deliberately small: read a line, call
//! the plugin, write the answers. Everything a plugin does with its time between two states —
//! serving a bus, polling a daemon — is a thread it owns.

pub mod json;

use std::io::{BufRead, Write};
use std::sync::Arc;

/// The wire version this crate speaks. The player refuses a plugin whose manifest or `ready`
/// names another number, instead of feeding it a shape it will misread.
pub const PROTOCOL: u32 = 1;

/// One line longer than this is not a state and not a reply; it is logged and skipped rather
/// than parsed, so a broken writer cannot make the reader allocate the machine away.
pub const MAX_LINE: usize = 1 << 20;

/// The player's now-playing facts, as one snapshot.
///
/// The names are the player's own (`Now` in `aurora-plugin`): a plugin that needs "the album line
/// the bar prints" gets the bar's answer, not a second rule that derives one from `artist` and
/// `album`. Empty strings mean "the player has no such fact", which is a legal state — a stream
/// has no path, a file without tags has no genre — never a missing field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub uri: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The release line the footer prints (`album`, plus the artist when it says something new).
    pub album_line: String,
    pub year: String,
    pub genre: String,
    pub label: String,
    /// The file's format as the player formats it: `FLAC · 16/44.1`, or empty.
    pub format: String,
    /// The wall clock as the player formats it: `13:45:07`.
    pub clock: String,
    /// The date as the player formats it: `26 октября 2026`.
    pub date: String,
    /// The listener's vote: `1` liked, `-1` disliked, `0` no opinion.
    pub like: i8,
    /// `play`, `pause` or `stop`.
    pub playing: String,
    /// Seconds into the track, and its length. Zero when unknown; the sidecar may interpolate
    /// the position between two snapshots because a snapshot is taken at `received` time.
    pub elapsed: f64,
    pub duration: f64,
    pub queue_len: usize,
    /// Tracks after the playing one: the number a "keep the queue filled" plugin is about.
    pub queue_after: usize,
    /// Seconds of music left from here to the end of the queue.
    pub queue_left: f64,
    /// The stream's rate in hertz, `0` when the daemon has not answered yet.
    pub rate: f64,
    /// Whether the player is talking to the daemon this snapshot.
    pub connected: bool,
}

impl State {
    /// One `state` object from the player. Unknown fields are ignored (a newer player may send
    /// more), and a missing one is its empty value, not an error: the shape is additive.
    pub fn from_value(root: &json::Value) -> State {
        fn string(root: &json::Value, key: &str) -> String {
            root.get(key)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string()
        }
        fn number(root: &json::Value, key: &str) -> f64 {
            root.get(key).and_then(|value| value.as_f64()).unwrap_or(0.0)
        }
        State {
            uri: string(root, "uri"),
            path: string(root, "path"),
            title: string(root, "title"),
            artist: string(root, "artist"),
            album: string(root, "album"),
            album_line: string(root, "album_line"),
            year: string(root, "year"),
            genre: string(root, "genre"),
            label: string(root, "label"),
            format: string(root, "format"),
            clock: string(root, "clock"),
            date: string(root, "date"),
            like: number(root, "like").clamp(-1.0, 1.0) as i8,
            playing: string(root, "playing"),
            elapsed: number(root, "elapsed"),
            duration: number(root, "duration"),
            queue_len: number(root, "queue_len").max(0.0) as usize,
            queue_after: number(root, "queue_after").max(0.0) as usize,
            queue_left: number(root, "queue_left"),
            rate: number(root, "rate"),
            connected: root
                .get("connected")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }
    }

    /// The same object the player writes. Here for fixtures and for a plugin that wants to log
    /// what it was handed; the player builds its own from `Now`, it does not depend on this crate.
    pub fn to_value(&self) -> json::Value {
        use json::Value;
        Value::Obj(vec![
            ("uri".into(), Value::Str(self.uri.clone())),
            ("path".into(), Value::Str(self.path.clone())),
            ("title".into(), Value::Str(self.title.clone())),
            ("artist".into(), Value::Str(self.artist.clone())),
            ("album".into(), Value::Str(self.album.clone())),
            ("album_line".into(), Value::Str(self.album_line.clone())),
            ("year".into(), Value::Str(self.year.clone())),
            ("genre".into(), Value::Str(self.genre.clone())),
            ("label".into(), Value::Str(self.label.clone())),
            ("format".into(), Value::Str(self.format.clone())),
            ("clock".into(), Value::Str(self.clock.clone())),
            ("date".into(), Value::Str(self.date.clone())),
            ("like".into(), Value::Num(self.like as f64)),
            ("playing".into(), Value::Str(self.playing.clone())),
            ("elapsed".into(), Value::Num(self.elapsed)),
            ("duration".into(), Value::Num(self.duration)),
            ("queue_len".into(), Value::Num(self.queue_len as f64)),
            ("queue_after".into(), Value::Num(self.queue_after as f64)),
            ("queue_left".into(), Value::Num(self.queue_left)),
            ("rate".into(), Value::Num(self.rate)),
            ("connected".into(), Value::Bool(self.connected)),
        ])
    }

    /// `Artist — Title`, or whichever of the two exists; the player's own `Now::label` rule, so a
    /// notification and the status bar name one track the same way.
    pub fn label(&self) -> String {
        match (self.artist.is_empty(), self.title.is_empty()) {
            (false, false) => format!("{} — {}", self.artist, self.title),
            (true, false) => self.title.clone(),
            (false, true) => self.artist.clone(),
            (true, true) => String::new(),
        }
    }

    /// Whether something is playing at all. A plugin has to handle the empty case: there is a
    /// snapshot before the daemon answers, and one after the queue runs out.
    pub fn is_empty(&self) -> bool {
        self.uri.is_empty()
    }
}

/// The one message the player sends before any state: who the plugin is, the wire version, the
/// plugin's own `[[plugin]]` table, and whether this run is a dry one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub plugin: String,
    pub version: String,
    pub dry_run: bool,
    /// The `[[plugin]]` keys in file order, exactly as `Plugin::init` used to receive them.
    pub config: Vec<(String, String)>,
}

impl Hello {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.config
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }
}

/// A message from the player.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Hello(Hello),
    State(State),
    Bye,
}

/// Parse one line from the player's stdin.
pub fn parse_incoming(line: &str) -> Result<Incoming, String> {
    let root = json::parse(line)?;
    let kind = root
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| String::from("no `type` field"))?;
    match kind {
        "hello" => Ok(Incoming::Hello(Hello {
            protocol: root
                .get("protocol")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0) as u32,
            plugin: text(&root, "plugin"),
            version: text(&root, "version"),
            dry_run: root
                .get("dry_run")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            config: pairs(root.get("config")),
        })),
        "state" => Ok(Incoming::State(State::from_value(
            root.get("state").unwrap_or(&json::Value::Null),
        ))),
        "bye" => Ok(Incoming::Bye),
        other => Err(format!("unknown message `{other}`")),
    }
}

fn text(root: &json::Value, key: &str) -> String {
    root.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

fn pairs(value: Option<&json::Value>) -> Vec<(String, String)> {
    let Some(json::Value::Obj(members)) = value else {
        return Vec::new();
    };
    members
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect()
}

/// One action a plugin may ask the player to take.
///
/// A closed set, the same one `aurora_plugin::Command` carries: a plugin that had the player's
/// whole command enum could clear the queue or change the volume under the listener, and a
/// background action is not a keystroke.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    PlayPause,
    Next,
    Previous,
    /// Move the playhead to `share` of the track, `0.0..=1.0`. The player owns the duration and
    /// clamps the number, so a stale snapshot cannot seek past the end.
    SeekShare(f32),
    QueueArtist { artist: String, count: u8 },
    QueueSimilar { count: u8 },
    QueueMood { mood: String, count: u8 },
    AppendMood { mood: String, count: u8 },
    OpenFacet { tag: String },
}

impl Action {
    /// Parse the object inside `{"type":"command","command":<this>}`.
    pub fn from_value(root: &json::Value) -> Result<Action, String> {
        let kind = root
            .get("type")
            .and_then(|value| value.as_str())
            .ok_or_else(|| String::from("command without `type`"))?;
        let count = || {
            root.get("count")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 255.0) as u8
        };
        let word = |key: &str| text(root, key);
        Ok(match kind {
            "play_pause" => Action::PlayPause,
            "next" => Action::Next,
            "previous" => Action::Previous,
            "seek_share" => Action::SeekShare(
                root.get("share")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0) as f32,
            ),
            "queue_artist" => Action::QueueArtist {
                artist: word("artist"),
                count: count(),
            },
            "queue_similar" => Action::QueueSimilar { count: count() },
            "queue_mood" => Action::QueueMood {
                mood: word("mood"),
                count: count(),
            },
            "append_mood" => Action::AppendMood {
                mood: word("mood"),
                count: count(),
            },
            "open_facet" => Action::OpenFacet { tag: word("tag") },
            other => return Err(format!("unknown command `{other}`")),
        })
    }

    pub fn to_value(&self) -> json::Value {
        use json::Value;
        let obj = |members: Vec<(&str, Value)>| {
            Value::Obj(
                members
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            )
        };
        match self {
            Action::PlayPause => obj(vec![("type", Value::Str("play_pause".into()))]),
            Action::Next => obj(vec![("type", Value::Str("next".into()))]),
            Action::Previous => obj(vec![("type", Value::Str("previous".into()))]),
            Action::SeekShare(share) => obj(vec![
                ("type", Value::Str("seek_share".into())),
                ("share", Value::Num(*share as f64)),
            ]),
            Action::QueueArtist { artist, count } => obj(vec![
                ("type", Value::Str("queue_artist".into())),
                ("artist", Value::Str(artist.clone())),
                ("count", Value::Num(*count as f64)),
            ]),
            Action::QueueSimilar { count } => obj(vec![
                ("type", Value::Str("queue_similar".into())),
                ("count", Value::Num(*count as f64)),
            ]),
            Action::QueueMood { mood, count } => obj(vec![
                ("type", Value::Str("queue_mood".into())),
                ("mood", Value::Str(mood.clone())),
                ("count", Value::Num(*count as f64)),
            ]),
            Action::AppendMood { mood, count } => obj(vec![
                ("type", Value::Str("append_mood".into())),
                ("mood", Value::Str(mood.clone())),
                ("count", Value::Num(*count as f64)),
            ]),
            Action::OpenFacet { tag } => obj(vec![
                ("type", Value::Str("open_facet".into())),
                ("tag", Value::Str(tag.clone())),
            ]),
        }
    }
}

/// One message from the plugin.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// The plugin understood `hello` and speaks this wire version.
    Ready { protocol: u32 },
    /// One line for the status bar: the fact it fills and the words.
    Status { slot: String, text: String },
    Command(Action),
    /// A line for the player's log (`info`, `warn`, `error`).
    Log { level: String, text: String },
    Bye,
}

impl Output {
    pub fn to_value(&self) -> json::Value {
        use json::Value;
        let obj = |members: Vec<(&str, Value)>| {
            Value::Obj(
                members
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            )
        };
        match self {
            Output::Ready { protocol } => obj(vec![
                ("type", Value::Str("ready".into())),
                ("protocol", Value::Num(*protocol as f64)),
            ]),
            Output::Status { slot, text } => obj(vec![
                ("type", Value::Str("status".into())),
                ("slot", Value::Str(slot.clone())),
                ("text", Value::Str(text.clone())),
            ]),
            Output::Command(action) => obj(vec![
                ("type", Value::Str("command".into())),
                ("command", action.to_value()),
            ]),
            Output::Log { level, text } => obj(vec![
                ("type", Value::Str("log".into())),
                ("level", Value::Str(level.clone())),
                ("text", Value::Str(text.clone())),
            ]),
            Output::Bye => obj(vec![("type", Value::Str("bye".into()))]),
        }
    }

    /// The line as it goes to the player: one object, no newline.
    pub fn line(&self) -> String {
        self.to_value().to_json()
    }
}

/// Where a plugin writes its answers.
///
/// A cloneable handle, because the work that produces an answer often runs on a thread the
/// plugin owns: `mpris` answers a `PlayPause` method from its bus loop, not from the loop that
/// reads the player's states.
#[derive(Clone)]
pub struct Sink(Arc<dyn Fn(Output) + Send + Sync>);

impl Sink {
    pub fn new(write: impl Fn(Output) + Send + Sync + 'static) -> Sink {
        Sink(Arc::new(write))
    }

    pub fn send(&self, output: Output) {
        (self.0)(output)
    }

    pub fn status(&self, slot: &str, text: impl Into<String>) {
        self.send(Output::Status {
            slot: slot.to_string(),
            text: text.into(),
        })
    }

    pub fn command(&self, action: Action) {
        self.send(Output::Command(action))
    }

    pub fn log(&self, level: &str, text: impl Into<String>) {
        self.send(Output::Log {
            level: level.to_string(),
            text: text.into(),
        })
    }
}

/// What a plugin implements. The three methods are the whole protocol; the default bodies are
/// for a plugin that does not care about that half (a notifier has no status line, a status line
/// has no commands).
pub trait Sidecar {
    /// Read the `[[plugin]]` table. An error is reported as a `log` line and ends the process:
    /// a plugin that does not understand its own configuration is not half-alive.
    fn hello(&mut self, _hello: &Hello, _sink: &Sink) -> Result<(), String> {
        Ok(())
    }

    /// One snapshot. Called on every state the player sends, in order.
    fn state(&mut self, _state: &State, _sink: &Sink) {}

    /// The host said `bye` (or closed the pipe). The process is about to exit; a plugin with a
    /// thread may use this to leave cleanly.
    fn bye(&mut self, _sink: &Sink) {}
}

/// The line loop, testable without a process: feed it what the player would write and read what
/// the plugin says. `Ok(false)` means a `bye` arrived (or the protocol was refused) — the caller
/// should exit.
pub struct Engine<P: Sidecar> {
    plugin: P,
    greeted: bool,
    complained: bool,
}

impl<P: Sidecar> Engine<P> {
    pub fn new(plugin: P) -> Engine<P> {
        Engine {
            plugin,
            greeted: false,
            complained: false,
        }
    }

    pub fn line(&mut self, line: &str, sink: &Sink) -> Result<bool, String> {
        match parse_incoming(line)? {
            Incoming::Hello(hello) => {
                if hello.protocol != PROTOCOL {
                    sink.log(
                        "error",
                        format!(
                            "the player speaks protocol {}, this plugin speaks {PROTOCOL}",
                            hello.protocol
                        ),
                    );
                    return Ok(false);
                }
                if let Err(reason) = self.plugin.hello(&hello, sink) {
                    sink.log("error", reason);
                    return Ok(false);
                }
                self.greeted = true;
                sink.send(Output::Ready {
                    protocol: PROTOCOL,
                });
                Ok(true)
            }
            Incoming::State(state) => {
                if !self.greeted && !self.complained {
                    // A player that sends a state before `hello` is a player this plugin cannot
                    // configure; one warning, then the state is still used (it may be a newer
                    // host with a shorter handshake).
                    self.complained = true;
                    sink.log("warn", "state before hello; using the defaults");
                }
                self.plugin.state(&state, sink);
                Ok(true)
            }
            Incoming::Bye => {
                self.plugin.bye(sink);
                Ok(false)
            }
        }
    }

    /// The host closed the pipe without a `bye`.
    pub fn finish(&mut self, sink: &Sink) {
        self.plugin.bye(sink);
    }
}

/// Run a plugin until the player says `bye` or closes the pipe. Never returns.
pub fn serve<P: Sidecar>(plugin: P) -> ! {
    let sink = Sink::new(|output| write_line(&output));
    let stdin = std::io::stdin();
    let mut engine = Engine::new(plugin);
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        if line.len() > MAX_LINE {
            sink.log("error", "a line longer than 1 MiB was ignored");
            continue;
        }
        match engine.line(&line, &sink) {
            Ok(true) => {}
            Ok(false) => break,
            // One malformed line is a bug, not a reason to drop the plugin: the next state is
            // usually fine, and a player that upgrades its shape should not take the plugin down.
            Err(reason) => sink.log("error", format!("protocol: {reason}")),
        }
    }
    engine.finish(&sink);
    sink.send(Output::Bye);
    std::process::exit(0)
}

fn write_line(output: &Output) {
    let line = output.line();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collector() -> (Sink, Arc<Mutex<Vec<Output>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_sink = seen.clone();
        let sink = Sink::new(move |output| seen_for_sink.lock().unwrap().push(output));
        (sink, seen)
    }

    struct Probe {
        hello: Option<Hello>,
        states: Vec<State>,
    }

    impl Sidecar for Probe {
        fn hello(&mut self, hello: &Hello, sink: &Sink) -> Result<(), String> {
            self.hello = Some(hello.clone());
            sink.status("queue", "ready");
            Ok(())
        }

        fn state(&mut self, state: &State, sink: &Sink) {
            self.states.push(state.clone());
            if state.playing == "pause" {
                sink.command(Action::PlayPause);
            }
        }
    }

    #[test]
    fn a_hello_is_answered_and_the_config_reaches_the_plugin() {
        let (sink, seen) = collector();
        let mut engine = Engine::new(Probe {
            hello: None,
            states: Vec::new(),
        });
        assert!(engine
            .line(
                r#"{"type":"hello","protocol":1,"plugin":"probe","version":"0.1","dry_run":true,"config":{"app":"aurora","tag":"aurora:x"}}"#,
                &sink
            )
            .unwrap());
        let outputs = seen.lock().unwrap();
        assert_eq!(
            outputs[0],
            Output::Status {
                slot: "queue".to_string(),
                text: "ready".to_string()
            }
        );
        assert_eq!(outputs[1], Output::Ready { protocol: PROTOCOL });
        let hello = engine.plugin.hello.as_ref().expect("the hello");
        assert_eq!(hello.get("app"), Some("aurora"));
        assert_eq!(hello.get("tag"), Some("aurora:x"));
        assert!(hello.dry_run);
    }

    #[test]
    fn a_state_is_parsed_with_the_players_names_and_unknown_keys_are_ignored() {
        let state = State::from_value(&json::parse(
            r#"{"uri":"u","title":"T","artist":"A","album":"Al","album_line":"Al — A",
                "year":"2009","genre":"IDM","label":"n5MD","format":"FLAC · 16/44.1",
                "like":1,"playing":"play","elapsed":12.5,"duration":240,"queue_len":7,
                "queue_after":3,"queue_left":1234.5,"rate":44100,"connected":true,
                "future_field":42}"#,
        )
        .unwrap());
        assert_eq!(state.label(), "A — T");
        assert_eq!(state.year, "2009");
        assert_eq!(state.playing, "play");
        assert_eq!(state.like, 1);
        assert_eq!(state.queue_after, 3);
        assert!(!state.is_empty());
    }

    #[test]
    fn a_missing_field_is_its_empty_value_not_an_error() {
        let state = State::from_value(&json::parse(r#"{"uri":"u"}"#).unwrap());
        assert_eq!(state.title, "");
        assert_eq!(state.queue_len, 0);
        assert_eq!(state.playing, "");
        assert!(state.connected == false);
    }

    #[test]
    fn an_action_round_trips_through_the_wire() {
        for action in [
            Action::PlayPause,
            Action::Next,
            Action::Previous,
            Action::SeekShare(0.42),
            Action::QueueArtist {
                artist: "A — B".to_string(),
                count: 3,
            },
            Action::QueueSimilar { count: 2 },
            Action::QueueMood {
                mood: "dark".to_string(),
                count: 5,
            },
            Action::AppendMood {
                mood: "calm".to_string(),
                count: 1,
            },
            Action::OpenFacet {
                tag: "year".to_string(),
            },
        ] {
            let parsed = Action::from_value(&action.to_value()).expect("its own shape parses");
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn a_command_is_one_object_on_one_line() {
        let line = Output::Command(Action::SeekShare(0.5)).line();
        assert_eq!(
            line,
            r#"{"type":"command","command":{"type":"seek_share","share":0.5}}"#
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn another_protocol_number_is_refused_and_the_reason_is_said() {
        let (sink, seen) = collector();
        let mut engine = Engine::new(Probe {
            hello: None,
            states: Vec::new(),
        });
        assert!(!engine
            .line(r#"{"type":"hello","protocol":9,"plugin":"probe"}"#, &sink)
            .unwrap());
        let outputs = seen.lock().unwrap();
        assert!(matches!(
            &outputs[0],
            Output::Log { level, text } if level == "error" && text.contains("protocol 9")
        ));
    }

    #[test]
    fn bye_finishes_the_plugin() {
        let (sink, _seen) = collector();
        let mut engine = Engine::new(Probe {
            hello: None,
            states: Vec::new(),
        });
        assert!(!engine.line(r#"{"type":"bye"}"#, &sink).unwrap());
    }

    #[test]
    fn the_hello_shape_the_player_writes_parses() {
        let incoming = parse_incoming(
            r#"{"type":"hello","protocol":1,"plugin":"notify","version":"0.1.0","dry_run":false,"config":{"replace":"true","app":"aurora"}}"#,
        )
        .unwrap();
        let Incoming::Hello(hello) = incoming else {
            panic!("not a hello");
        };
        assert_eq!(hello.plugin, "notify");
        assert_eq!(hello.config.len(), 2);
        assert_eq!(hello.config[0], ("replace".to_string(), "true".to_string()));
    }
}
