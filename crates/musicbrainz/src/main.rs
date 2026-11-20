//! `musicbrainz`: the release the playing recording belongs to, as one line in the bar.
//!
//! The old in-tree plugin moved across the wire with its division intact: the player says what
//! the tags are, and the program normalises them, builds the `ws/2/recording` query, runs one
//! `curl` and reads two fields out of the answer. It paints nothing, so `status` is the grant —
//! and `release` is the slot the in-frame plugin filled, the same fact in the same place.
//!
//! # Why a process and not a crate
//!
//! HTTPS needs a TLS stack, and a TLS stack is a dependency tree nobody votes for by installing a
//! status line. `curl` is the transport the machine already has, it uses the system CA store, and
//! one short-lived child per track change is a bounded cost. A dry run — every capture — never
//! spawns anything: it prints the query instead, which is the line a script asserts on.
//!
//! # The answer arrives between two states
//!
//! The in-frame plugin polled its child from every `frame`; a program has no frame, and the
//! protocol has no timer — a snapshot is sent when the now-playing facts move, and a paused
//! player moves nothing. So the child gets a thread of its own, which waits for it, reads the
//! reply and writes the line itself. The conversation is a handful of strings behind a lock; a
//! late answer for a track that is no longer playing is dropped, which is the same guard the
//! in-frame plugin had when it compared the answer's URI.

use aurora_protocol::{Hello, Sidecar, Sink, State};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub mod lookup;

/// The fact this line fills: the release of the playing recording.
const SLOT: &str = "release";

/// What the program remembers between snapshots. Shared with a fetch thread, because the answer
/// can land when no snapshot is coming.
#[derive(Default)]
struct Conversation {
    /// The URI the last query was about, so a snapshot is not a lookup.
    asked: String,
    /// The answer for the current track: `(title, first release date)`.
    found: Option<(String, String)>,
    /// The line sent last, so an unchanged answer costs no output.
    last: String,
}

impl Conversation {
    /// The answer to the last lookup, as one line: `2009 · The Incident`. Nothing while there is
    /// nothing to say.
    fn line(&self) -> String {
        let Some((title, date)) = self.found.as_ref() else {
            return String::new();
        };
        let year = date.get(..4).unwrap_or("");
        if year.is_empty() {
            title.clone()
        } else {
            format!("{year} · {title}")
        }
    }

    /// Take a reply that belongs to `uri`. `None` means the reply is for a track that is not
    /// playing any more: a late answer is dropped rather than shown against the current song.
    fn adopt(&mut self, uri: &str, body: Option<&str>) -> Option<String> {
        if uri != self.asked {
            return None;
        }
        self.found = body.and_then(lookup::first_release);
        Some(self.line())
    }

    /// Say `text` on the bar if it is not what was said last.
    fn say(&mut self, sink: &Sink, text: String) {
        if text == self.last {
            return;
        }
        sink.status(SLOT, text.clone());
        self.last = text;
    }
}

pub struct MusicBrainz {
    /// Print the query instead of fetching it: what a capture and a dry run do.
    dry: bool,
    conversation: Arc<Mutex<Conversation>>,
    /// Set once a missing `curl` was reported, so the message costs one line per run.
    reported: bool,
}

impl Default for MusicBrainz {
    fn default() -> Self {
        MusicBrainz {
            dry: false,
            conversation: Arc::default(),
            reported: false,
        }
    }
}

impl MusicBrainz {
    /// The conversation, under its lock. A panic in a fetch thread poisons the lock; the data is a
    /// few strings, and a stale line is a worse answer than a fresh one.
    fn talk(&self) -> MutexGuard<'_, Conversation> {
        self.conversation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Sidecar for MusicBrainz {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        // The in-frame plugin asked the environment and the player's argv; a program is told, and
        // the table can still override the answer.
        self.dry = hello.dry_run;
        for (key, value) in &hello.config {
            match key.as_str() {
                "dry_run" => {
                    self.dry = match value.as_str() {
                        "true" | "on" | "yes" => true,
                        "false" | "off" | "no" => false,
                        other => return Err(format!("dry_run expects true/false, got `{other}`")),
                    }
                }
                other => return Err(format!("`musicbrainz` does not know the key `{other}`")),
            }
        }
        Ok(())
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        // Nothing playing: the bar keeps no release from the track that just ended.
        if state.is_empty() {
            let mut talk = self.talk();
            if talk.asked.is_empty() {
                return;
            }
            talk.asked.clear();
            talk.found = None;
            talk.say(sink, String::new());
            return;
        }

        // A track change clears the previous answer at once, before the new lookup has one.
        let fresh = {
            let mut talk = self.talk();
            if talk.asked == state.uri {
                false
            } else {
                talk.asked = state.uri.clone();
                talk.found = None;
                talk.say(sink, String::new());
                true
            }
        };
        if !fresh {
            return;
        }

        let url = lookup::lookup(state);
        if self.dry {
            sink.log("info", format!("[musicbrainz] {url}"));
            return;
        }
        match Fetch::start(&url) {
            Ok(fetch) => {
                let conversation = Arc::clone(&self.conversation);
                let sink = sink.clone();
                let uri = state.uri.clone();
                std::thread::spawn(move || {
                    let body = fetch.finish();
                    let mut talk = conversation
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if let Some(line) = talk.adopt(&uri, body.as_deref()) {
                        talk.say(&sink, line);
                    }
                });
            }
            Err(error) => {
                if !self.reported {
                    self.reported = true;
                    sink.log("warn", format!("[musicbrainz] not looking up: {error}"));
                }
            }
        }
    }
}

/// How a lookup is run when it is not a dry run: one `curl` process per track change.
struct Fetch {
    child: std::process::Child,
    out: PathBuf,
}

/// One answer file per request, so two players on one machine cannot write over each other's.
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

impl Fetch {
    fn start(url: &str) -> std::io::Result<Fetch> {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!(
            "aurora-musicbrainz-{}-{sequence}.json",
            std::process::id()
        ));
        let child = std::process::Command::new("curl")
            .args(lookup::curl_args(url, &out))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(Fetch { child, out })
    }

    /// Wait for the answer and read it out of the file `curl` wrote. Blocking, because it runs on
    /// the request's own thread: `curl` is bounded by `--max-time 8`, and a failed request leaves
    /// no file, which is the same "nothing to say" as an empty reply.
    fn finish(self) -> Option<String> {
        let mut child = self.child;
        let _ = child.wait();
        let body = std::fs::read_to_string(&self.out).ok();
        let _ = std::fs::remove_file(&self.out);
        body
    }
}

fn main() {
    aurora_protocol::serve(MusicBrainz::default())
}

#[cfg(test)]
mod tests {
    use super::MusicBrainz;
    use aurora_protocol::{Engine, Output, Sidecar, Sink};
    use std::sync::{Arc, Mutex};

    /// Run the plugin against the lines the player would write and collect what it answers.
    fn run(plugin: impl Sidecar, lines: &[&str]) -> Vec<Output> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_for_plugin = seen.clone();
        let sink = Sink::new(move |output| sink_for_plugin.lock().unwrap().push(output));
        let mut engine = Engine::new(plugin);
        for line in lines {
            engine.line(line, &sink).expect("a line the plugin can read");
        }
        let outputs = seen.lock().unwrap().clone();
        outputs
    }

    const HELLO_DRY: &str = r#"{"type":"hello","protocol":1,"plugin":"musicbrainz","version":"0.1","dry_run":true,"config":{}}"#;

    fn state(uri: &str) -> String {
        format!(
            r#"{{"type":"state","state":{{"uri":"{uri}","artist":"Artist","title":"Song","album":"Album","duration":240.0}}}}"#
        )
    }

    fn statuses(outputs: &[Output]) -> Vec<(String, String)> {
        outputs
            .iter()
            .filter_map(|output| match output {
                Output::Status { slot, text } => Some((slot.clone(), text.clone())),
                _ => None,
            })
            .collect()
    }

    fn logs(outputs: &[Output]) -> Vec<String> {
        outputs
            .iter()
            .filter_map(|output| match output {
                Output::Log { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A plugin that has already looked the first track up: a dry run, so nothing is spawned.
    fn answered() -> MusicBrainz {
        let plugin = MusicBrainz {
            dry: true,
            ..MusicBrainz::default()
        };
        {
            let mut talk = plugin.talk();
            talk.asked = "u1".to_string();
            talk.found = Some(("The Incident".to_string(), "2009-05-01".to_string()));
            talk.last = "2009 · The Incident".to_string();
        }
        plugin
    }

    #[test]
    fn a_late_answer_is_dropped_for_the_track_it_was_asked_about() {
        let mut talk = super::Conversation {
            asked: "u2".to_string(),
            ..super::Conversation::default()
        };
        // The reply belongs to the track that is no longer playing.
        assert_eq!(talk.adopt("u1", Some(r#"{"recordings":[{"title":"Song"}]}"#)), None);
        assert_eq!(talk.line(), "");
        let line = talk.adopt("u2", Some(r#"{"recordings":[{"title":"Song","first-release-date":"2020-01-01"}]}"#));
        assert_eq!(line.as_deref(), Some("2020 · Song"));
    }

    #[test]
    fn a_release_without_a_date_is_the_title_alone() {
        let mut talk = super::Conversation::default();
        talk.asked = "u1".to_string();
        assert_eq!(talk.adopt("u1", Some(r#"{"recordings":[{"title":"Song"}]}"#)).as_deref(), Some("Song"));
        assert_eq!(talk.adopt("u1", Some("{}")).as_deref(), Some(""));
    }

    #[test]
    fn a_new_track_clears_the_previous_answer() {
        let outputs = run(answered(), &[HELLO_DRY, &state("u2")]);
        assert_eq!(
            statuses(&outputs),
            vec![("release".to_string(), String::new())]
        );
    }

    #[test]
    fn a_snapshot_that_did_not_move_the_track_says_nothing() {
        // The same track twice: the second snapshot is not a lookup and not a question.
        let outputs = run(answered(), &[HELLO_DRY, &state("u1")]);
        assert!(statuses(&outputs).is_empty());
        assert!(logs(&outputs).is_empty());
    }

    #[test]
    fn an_empty_playhead_clears_the_line() {
        let outputs = run(
            answered(),
            &[HELLO_DRY, r#"{"type":"state","state":{"uri":""}}"#],
        );
        assert_eq!(
            statuses(&outputs),
            vec![("release".to_string(), String::new())]
        );
    }

    #[test]
    fn a_dry_run_prints_the_query_instead_of_fetching() {
        let outputs = run(MusicBrainz::default(), &[HELLO_DRY, &state("u1")]);
        assert!(statuses(&outputs).is_empty());
        let logs = logs(&outputs);
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(
            logs[0].starts_with("[musicbrainz] https://musicbrainz.org/ws/2/recording?query="),
            "{}",
            logs[0]
        );
    }

    #[test]
    fn the_table_can_force_a_dry_run() {
        // `hello` says a live run; the plugin's own `dry_run` key overrides it, which is how a
        // capture kept working when the switch lived in `skins.toml`.
        let outputs = run(
            MusicBrainz::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"musicbrainz","version":"0.1","dry_run":false,"config":{"dry_run":"true"}}"#,
                &state("u1"),
            ],
        );
        assert!(statuses(&outputs).is_empty());
        assert!(logs(&outputs)[0].contains("musicbrainz.org"));
    }

    #[test]
    fn an_empty_playhead_asks_nothing() {
        let outputs = run(
            MusicBrainz::default(),
            &[HELLO_DRY, r#"{"type":"state","state":{"uri":""}}"#],
        );
        assert!(statuses(&outputs).is_empty());
        assert!(logs(&outputs).is_empty());
    }

    #[test]
    fn an_unknown_key_is_a_configuration_error() {
        let outputs = run(
            MusicBrainz::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"musicbrainz","version":"0.1","dry_run":false,"config":{"nope":"1"}}"#,
            ],
        );
        assert!(matches!(
            &outputs[0],
            Output::Log { level, .. } if level == "error"
        ));
    }
}
