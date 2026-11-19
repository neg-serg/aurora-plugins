//! `scrobble`: decide what counts as a listen, and submit it.
//!
//! The old in-tree plugin moved across the wire. It keeps the **rule** — the convention both
//! services share: a track counts once it has been played for half its length, or for four
//! minutes, whichever comes first — and the **payload**, and it posts with one `curl` per counted
//! listen. `curl` is on the machine, uses the system CA store, and a listen is a rare event (once
//! per track), not a per-frame cost. A missing token or a missing `curl` is reported once per run.
//!
//! The sidecar reads snapshots, so "played" is accumulated from the playhead the player observed:
//! a pause does not accumulate, and a backward seek does not count the same seconds twice. The
//! timestamp is the wall clock at the moment the track ends — the one thing a sidecar has that a
//! frame capture does not, and a submission is about the listener's calendar, not the frame's.
//!
//! The token is read from `LISTENBRAINZ_TOKEN`, so no secret ever lives in a config file.

use aurora_protocol::{Hello, Sidecar, Sink, State};

/// Real seconds of playback that always count as a listen, however long the track is.
const ALWAYS_AFTER: f64 = 240.0;

/// What one snapshot produced, if anything. The event only exists on a track change: a listen
/// that just ended, or the track that just started.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A new track is being watched; nothing is submitted for it yet.
    Watching(String),
    /// The track that just ended counted as a listen: this is what is POSTed.
    Submit { label: String, payload: String },
}

/// The facts about a track that outlive it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Track {
    uri: String,
    title: String,
    artist: String,
    album: String,
    duration: f64,
    /// The elapsed position when the last snapshot arrived, kept so `label` and `payload` can be
    /// built after the track is gone.
    elapsed: f64,
}

impl Track {
    fn remember(state: &State) -> Track {
        Track {
            uri: state.uri.clone(),
            title: state.title.clone(),
            artist: state.artist.clone(),
            album: state.album.clone(),
            duration: state.duration,
            elapsed: state.elapsed,
        }
    }

    fn is_empty(&self) -> bool {
        self.uri.is_empty()
    }

    fn label(&self) -> String {
        match (self.artist.is_empty(), self.title.is_empty()) {
            (false, false) => format!("{} — {}", self.artist, self.title),
            (true, false) => self.title.clone(),
            (false, true) => self.artist.clone(),
            (true, true) => String::new(),
        }
    }
}

pub struct Scrobble {
    /// The track being watched, remembered rather than borrowed: by the time it ends the model
    /// has already moved on to the next one, and a submission needs its tags.
    watching: Track,
    /// Seconds actually played while it was the current track.
    played: f64,
    /// The previous snapshot's playhead, so a jump is not mistaken for listening.
    last_elapsed: f64,
    /// `AURORA_DRY_RUN`, or `dry_run` in the config: log instead of submitting.
    dry: bool,
    /// Log the "watching" line too; `quiet = true` silences it but keeps the submissions.
    announce: bool,
    /// Set once a failed submission was reported, so a broken token costs one line per run.
    reported: bool,
}

impl Default for Scrobble {
    fn default() -> Self {
        Scrobble {
            watching: Track::default(),
            played: 0.0,
            last_elapsed: 0.0,
            dry: false,
            announce: true,
            reported: false,
        }
    }
}

/// Whether a listen counts: half the track, capped at four minutes.
///
/// Split out as a pure function because it is the whole meaning of the plugin, and a rule that
/// lives inside a loop cannot be tested without inventing a clock.
pub fn counts(played: f64, duration: f64) -> bool {
    if !(played > 0.0) {
        return false;
    }
    let need = if duration > 0.0 {
        (duration * 0.5).min(ALWAYS_AFTER)
    } else {
        // A stream with no length: the cap is the only rule there is.
        ALWAYS_AFTER
    };
    played >= need
}

/// The ListenBrainz submission, as JSON.
///
/// Written by hand: the shape is four fields and one nested object, and a serialiser would be the
/// first dependency in the plugin tree for a document this short. Everything that comes from a tag
/// is escaped — a title with a quote in it must not be able to break the document.
fn payload(track: &Track, listened_at: i64) -> String {
    format!(
        concat!(
            "{{\"listen_type\":\"single\",\"payload\":[{{\"listened_at\":{},",
            "\"track_metadata\":{{\"artist_name\":{},\"track_name\":{},\"release_name\":{},",
            "\"additional_info\":{{\"duration_ms\":{},\"media_player\":\"player\"}}}}}}]}}"
        ),
        listened_at,
        json(&track.artist),
        json(&track.title),
        json(&track.album),
        (track.duration * 1000.0) as i64
    )
}

/// A JSON string literal with the escapes the format requires: quote, backslash, the three named
/// control characters and `\u00XX` for the rest.
fn json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Scrobble {
    /// Feed one snapshot. An event only comes out of a track change.
    ///
    /// `stamp` is the listen's timestamp: a real submission stamps the wall clock at the moment
    /// the track ended, and a dry run does not need it.
    pub fn watch(&mut self, state: &State, stamp: i64) -> Option<Event> {
        if state.uri == self.watching.uri {
            // Only forward progress while playing counts: a seek back would otherwise let the
            // same track be counted twice, and a pause is not listening.
            let delta = state.elapsed - self.last_elapsed;
            // A jump larger than the snapshot cadence is a seek, not listening: the host
            // sends a state about once a second while playing, so four seconds of slack is
            // generous and a seek to the middle of a track is not mistaken for having heard it.
            if state.playing == "play" && delta > 0.0 && delta < 5.0 {
                self.played += delta;
            }
            self.last_elapsed = state.elapsed;
            return None;
        }
        let finished = std::mem::replace(&mut self.watching, Track::remember(state));
        let played = std::mem::take(&mut self.played);
        self.last_elapsed = state.elapsed;
        if finished.is_empty() && state.is_empty() {
            return None;
        }
        if !finished.is_empty() && counts(played, finished.duration) {
            return Some(Event::Submit {
                label: finished.label(),
                payload: payload(&finished, stamp),
            });
        }
        Some(Event::Watching(state.label()))
    }

    /// Log the event, if this run is a dry one; POST it otherwise.
    pub fn emit(&mut self, event: &Event, sink: &Sink) {
        if !self.dry {
            if let Event::Submit { payload, .. } = event {
                match submit(payload) {
                    Ok(()) => {}
                    Err(e) if !self.reported => {
                        self.reported = true;
                        sink.log("warn", format!("[scrobble] not submitted: {e}"));
                    }
                    Err(_) => {}
                }
            }
            return;
        }
        match event {
            Event::Watching(label) if self.announce => {
                sink.log("info", format!("[scrobble] watching {label}"))
            }
            Event::Watching(_) => {}
            Event::Submit { label, payload } => sink.log(
                "info",
                format!("[scrobble] submit {label} ({} bytes)", payload.len()),
            ),
        }
    }
}

impl Sidecar for Scrobble {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
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
                "quiet" => self.announce = !matches!(value.as_str(), "true" | "on" | "yes"),
                other => return Err(format!("`scrobble` does not know the key `{other}`")),
            }
        }
        Ok(())
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        let stamp = now_seconds();
        if let Some(event) = self.watch(state, stamp) {
            self.emit(&event, sink);
        }
    }
}

/// Seconds since the Unix epoch, the calendar a submission is filed under.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

fn main() {
    aurora_protocol::serve(Scrobble::default())
}

/// Where a listen goes. ListenBrainz wants the token in the `Authorization` header; the token is
/// read from the environment so no secret ever lives in a config file this repo writes.
pub const ENDPOINT: &str = "https://api.listenbrainz.org/1/submit-listens";

/// The argv one submission runs with. `curl` reads the payload from stdin, so nothing is written
/// to disk and the process is one short pipe.
pub fn curl_args(token: &str) -> Vec<String> {
    vec![
        "-sS".to_string(),
        "--max-time".to_string(),
        "8".to_string(),
        "-X".to_string(),
        "POST".to_string(),
        "-H".to_string(),
        format!("Authorization: Token {token}"),
        "-H".to_string(),
        "Content-Type: application/json".to_string(),
        "--data-binary".to_string(),
        "@-".to_string(),
        ENDPOINT.to_string(),
    ]
}

fn submit(payload: &str) -> Result<(), String> {
    use std::io::Write;
    let token = std::env::var("LISTENBRAINZ_TOKEN")
        .map_err(|_| "LISTENBRAINZ_TOKEN is not set".to_string())?;
    let mut child = std::process::Command::new("curl")
        .args(curl_args(&token))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(payload.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
    }
    // Waits for the child: a counted listen happens once per track, and a submission that is
    // abandoned half-way is worse than a few milliseconds of the sidecar's own time.
    child.wait().map_err(|e| format!("wait: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listen_rule_is_half_the_track_capped_at_four_minutes() {
        assert!(!counts(0.0, 300.0));
        assert!(!counts(-1.0, 300.0));
        assert!(!counts(f64::NAN, 300.0));
        // Half of a short track.
        assert!(!counts(44.9, 90.0));
        assert!(counts(45.0, 90.0));
        // A long track counts at the four-minute cap, not at half its length.
        assert!(!counts(239.9, 0.0));
        assert!(counts(240.0, 0.0));
        assert!(!counts(239.9, 3600.0));
        assert!(counts(240.0, 3600.0));
        // A negative/NaN length is "unknown", so the cap is the only rule.
        assert!(!counts(100.0, -5.0));
        assert!(counts(240.0, f64::NAN));
    }

    #[test]
    fn the_payload_escapes_the_tags_and_keeps_the_shape() {
        let track = Track {
            artist: "A \"B\"".to_string(),
            title: "Line\nTwo".to_string(),
            album: "C\\D".to_string(),
            duration: 1.5,
            ..Track::default()
        };
        let body = payload(&track, 42);
        assert!(
            body.starts_with("{\"listen_type\":\"single\",\"payload\":[{\"listened_at\":42,"),
            "{body}"
        );
        assert!(body.contains("\"artist_name\":\"A \\\"B\\\"\""), "{body}");
        assert!(body.contains("\"track_name\":\"Line\\nTwo\""), "{body}");
        assert!(body.contains("\"release_name\":\"C\\\\D\""), "{body}");
        assert!(body.contains("\"duration_ms\":1500"), "{body}");
        assert!(body.contains("\"media_player\":\"player\""), "{body}");
    }

    #[test]
    fn the_curl_argv_carries_the_token_and_the_stdin_payload() {
        let args = curl_args("tok");
        assert_eq!(args[0], "-sS");
        assert!(args.contains(&"Authorization: Token tok".to_string()));
        assert!(args.contains(&"@-".to_string()));
        assert!(args.contains(&ENDPOINT.to_string()));
        assert!(args.windows(2).any(|pair| pair == ["--max-time", "8"]));
    }

    /// Half a track's playhead, then the next track: one submission, for the first one.
    #[test]
    fn a_finished_track_is_counted_and_a_skipped_one_is_not() {
        let mut scrobble = Scrobble::default();
        let mut first = State {
            uri: "u1".to_string(),
            artist: "A".to_string(),
            title: "T".to_string(),
            album: "Al".to_string(),
            duration: 100.0,
            elapsed: 0.0,
            playing: "play".to_string(),
            ..State::default()
        };
        assert_eq!(
            scrobble.watch(&first, 0),
            Some(Event::Watching("A — T".to_string()))
        );
        for second in 1..=60 {
            first.elapsed = second as f64;
            assert_eq!(scrobble.watch(&first, 0), None);
        }
        let second = State {
            uri: "u2".to_string(),
            ..State::default()
        };
        let event = scrobble.watch(&second, 1000).expect("a transition");
        let Event::Submit { label, payload } = event else {
            panic!("half a track counts");
        };
        assert_eq!(label, "A — T");
        assert!(payload.contains("\"listened_at\":1000"));
    }

    /// A pause does not accumulate and a backward seek does not count the same seconds twice.
    #[test]
    fn only_forward_progress_while_playing_counts() {
        let mut scrobble = Scrobble::default();
        let mut state = State {
            uri: "u1".to_string(),
            duration: 60.0,
            elapsed: 0.0,
            playing: "play".to_string(),
            ..State::default()
        };
        scrobble.watch(&state, 0);
        state.elapsed = 1.0;
        scrobble.watch(&state, 0);
        assert_eq!(scrobble.played, 1.0);
        // Paused: the same playhead, then a jump the player did not play through.
        state.playing = "pause".to_string();
        state.elapsed = 2.0;
        scrobble.watch(&state, 0);
        assert_eq!(scrobble.played, 1.0);
        // A seek backwards is not negative listening.
        state.playing = "play".to_string();
        state.elapsed = 0.5;
        scrobble.watch(&state, 0);
        assert_eq!(scrobble.played, 1.0);
    }
}
