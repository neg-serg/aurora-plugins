//! `lyrics`: the `.lrc` sitting next to the playing file, as one line on the status bar.
//!
//! The old in-tree plugin moved across the wire with its rule intact: the player sends the file's
//! path and the playhead, and the program reads the file itself. It paints nothing — `status` is
//! the grant that lets it speak in the bar — and it answers an empty text when there is nothing to
//! say, which the player drops. That is what makes a track with no `.lrc` clear the line instead
//! of freezing the previous verse on it.
//!
//! # The clock is the player's
//!
//! A snapshot arrives when the now-playing facts move, at most four times a second, with `elapsed`
//! quantised to a quarter of a second. The verse is therefore a function of the snapshot in hand:
//! no clock, no timer, no thread of this plugin's own. The line may trail the audio by one
//! snapshot, which a bar reading a verse does not notice, and a capture of the same state shows
//! the same verse in every run — which is the property the in-frame plugin had.

use aurora_protocol::{Hello, Sidecar, Sink, State};

pub mod lrc;

use lrc::Line;

/// The fact this line fills. One plugin owns a slot, so a second lyric source can never put two
/// verses on the bar at once.
const SLOT: &str = "lyric";

#[derive(Default)]
pub struct Lyrics {
    /// The path the parsed lines belong to — a track change is a path change, so one string
    /// compares both.
    path: String,
    lines: Vec<Line>,
    /// The line sent last, so a snapshot that did not move the verse costs no output.
    last: String,
}

impl Lyrics {
    /// Load the file next to `path` when the track changes. A missing file is not an error: most
    /// tracks have no lyrics, and the line simply is not there.
    fn reload(&mut self, path: &str) {
        if self.path == path {
            return;
        }
        self.path = path.to_string();
        self.lines.clear();
        if path.is_empty() {
            return;
        }
        // `<file>.lrc` next to the audio file, which is where every tagger and every download puts
        // it. `Path::with_extension` would replace the audio extension, so the suffix is appended
        // to the whole name instead.
        let candidate = format!("{path}.lrc");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            self.lines = lrc::parse(&text);
        }
    }

    /// The verse the playhead is in, or nothing. An untimed intro, a track with no `.lrc` and an
    /// instrumental break (a timed line with no words) all say "nothing here": the bar is not the
    /// place to show a blank.
    fn verse(&self, elapsed: f64) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let Some(active) = lrc::active_at(&self.lines, elapsed) else {
            return String::new();
        };
        self.lines
            .get(active)
            .map(|line| line.text.trim().to_string())
            .unwrap_or_default()
    }
}

impl Sidecar for Lyrics {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        match hello.config.first() {
            // Nothing to configure: the file is where the audio file is.
            Some((key, _)) => Err(format!("`lyrics` does not know the key `{key}`")),
            None => Ok(()),
        }
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        self.reload(&state.path);
        let text = self.verse(state.elapsed);
        if text == self.last {
            return;
        }
        sink.status(SLOT, text.clone());
        self.last = text;
    }
}

fn main() {
    aurora_protocol::serve(Lyrics::default())
}

#[cfg(test)]
mod tests {
    use super::Lyrics;
    use aurora_protocol::{Engine, Output, Sidecar, Sink};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// A track path with a `.lrc` beside it, in the temporary directory. The audio file itself is
    /// never opened — only `<path>.lrc` is — so a name is enough. The name carries the test's own
    /// word, because the tests run at once.
    struct Fixture {
        audio: PathBuf,
        lrc: PathBuf,
    }

    impl Fixture {
        fn new(word: &str, text: &str) -> Fixture {
            let audio = std::env::temp_dir().join(format!(
                "aurora-lyrics-{}-{word}.flac",
                std::process::id()
            ));
            let lrc = PathBuf::from(format!("{}.lrc", audio.display()));
            std::fs::write(&lrc, text).expect("a fixture file");
            Fixture { audio, lrc }
        }

        fn path(&self) -> String {
            self.audio.display().to_string()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.lrc);
        }
    }

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

    const HELLO: &str = r#"{"type":"hello","protocol":1,"plugin":"lyrics","version":"0.1","dry_run":false,"config":{}}"#;

    fn state(path: &str, elapsed: f64) -> String {
        format!(r#"{{"type":"state","state":{{"uri":"u1","path":"{path}","elapsed":{elapsed}}}}}"#)
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

    fn line(text: &str) -> (String, String) {
        ("lyric".to_string(), text.to_string())
    }

    #[test]
    fn the_verse_follows_the_playhead() {
        let fixture = Fixture::new("verse", "[00:00]first\n[00:10]second\n[00:20]third\n");
        let path = fixture.path();
        let outputs = run(
            Lyrics::default(),
            &[
                HELLO,
                &state(&path, 1.0),
                &state(&path, 11.0),
                &state(&path, 25.0),
            ],
        );
        assert_eq!(
            statuses(&outputs),
            vec![line("first"), line("second"), line("third")]
        );
    }

    #[test]
    fn a_snapshot_that_did_not_move_the_verse_says_nothing() {
        let fixture = Fixture::new("same", "[00:00]first\n[00:10]second\n");
        let path = fixture.path();
        let outputs = run(
            Lyrics::default(),
            &[HELLO, &state(&path, 2.0), &state(&path, 2.0), &state(&path, 2.1)],
        );
        assert_eq!(statuses(&outputs), vec![line("first")]);
    }

    #[test]
    fn a_track_without_lyrics_clears_the_line() {
        let fixture = Fixture::new("cleared", "[00:00]a verse\n");
        let path = fixture.path();
        let silent = std::env::temp_dir().join(format!(
            "aurora-lyrics-{}-nothing.flac",
            std::process::id()
        ));
        let outputs = run(
            Lyrics::default(),
            &[
                HELLO,
                &state(&path, 1.0),
                &state(&silent.display().to_string(), 1.0),
            ],
        );
        assert_eq!(statuses(&outputs), vec![line("a verse"), line("")]);
    }

    #[test]
    fn an_instrumental_line_clears_the_verse() {
        let fixture = Fixture::new("break", "[00:00]words\n[00:10]\n");
        let path = fixture.path();
        let outputs = run(
            Lyrics::default(),
            &[HELLO, &state(&path, 1.0), &state(&path, 11.0)],
        );
        assert_eq!(statuses(&outputs), vec![line("words"), line("")]);
    }

    #[test]
    fn a_track_before_the_first_timestamp_says_nothing() {
        let fixture = Fixture::new("intro", "[00:10]late\n");
        let path = fixture.path();
        let outputs = run(Lyrics::default(), &[HELLO, &state(&path, 1.0)]);
        assert!(statuses(&outputs).is_empty());
    }

    #[test]
    fn an_unknown_key_is_a_configuration_error() {
        let outputs = run(
            Lyrics::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"lyrics","version":"0.1","dry_run":false,"config":{"nope":"1"}}"#,
            ],
        );
        assert!(matches!(
            &outputs[0],
            Output::Log { level, .. } if level == "error"
        ));
    }
}
