//! `notify`: one desktop notification per track change, grouped under its sender.
//!
//! The old in-tree plugin moved across the wire with its argv intact. The player now sends a
//! snapshot instead of lending `Now`, but the two rules that made it a plugin rather than a poll
//! are unchanged:
//!
//! * **the change is detected here** — a state arrives for a volume nudge too, so this keeps the
//!   last URI it announced and speaks only when that URI changes: one notification per track,
//!   never one per snapshot;
//! * **sending has no dependency** — the notification is `notify-send` if it is on `PATH`. A dry
//!   run (`AURORA_DRY_RUN`, `dry_run = true` in `[[plugin]]`, or `--no-plugins` with the plugin
//!   started by hand) prints the exact argv instead of spawning anything.
//!
//! Grouping is the argv and is documented on [`Notify::command`]: `--app-name` names the sender,
//! `x-dunst-stack-tag` names the group inside it, and `--print-id`/`--replace-id` make the next
//! track edit this bubble on the daemons that do not honour a stack tag.

use aurora_protocol::{Hello, Sidecar, Sink, State};

pub struct Notify {
    /// The URI that was last announced; empty until the first track.
    announced: String,
    /// Dry run: log the command instead of running it.
    dry: bool,
    /// Set once after a failed spawn, so a missing `notify-send` costs one line, not one per
    /// track.
    reported: bool,
    /// The sender: the name the daemon files the notification under and groups by (`--app-name`).
    app: String,
    /// The sender's group inside that name (`x-dunst-stack-tag`). One per plugin by default, so a
    /// second notifier would get a bubble of its own instead of editing this one.
    tag: String,
    /// Whether a new track replaces the sender's previous bubble instead of stacking under it.
    replace: bool,
    /// The id the daemon gave the sender's current notification (`--print-id`), if it answered.
    /// Forgotten when the replacement is turned off or the daemon answers with something else.
    id: Option<i32>,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            announced: String::new(),
            dry: false,
            reported: false,
            app: "aurora".to_string(),
            tag: "aurora:notify".to_string(),
            // What a "now playing" bubble wants: the track changes, the bubble follows it.
            replace: true,
            id: None,
        }
    }
}

impl Notify {
    /// The line to announce for this snapshot, if the track is new since the last one.
    ///
    /// Split out from [`Sidecar::state`] so the "one per change, never per poll" rule is a
    /// function a test can call thirteen times in a row.
    pub fn change(&mut self, state: &State) -> Option<String> {
        if state.is_empty() || self.announced == state.uri {
            return None;
        }
        self.announced = state.uri.clone();
        let label = state.label();
        Some(if state.album.is_empty() {
            label
        } else {
            format!("{label} · {}", state.album)
        })
    }

    /// The command this sender's notification is, as argv.
    ///
    /// A function instead of a `Command` built inline, because the *grouping* is the argv: the
    /// dry run prints this line, so a scripted run asserts on the sender and on whether the
    /// track replaces the previous bubble — the two things a picture of the player cannot show.
    pub fn command(&self, summary: &str, body: &str) -> Vec<String> {
        let mut argv = vec![
            "notify-send".to_string(),
            format!("--app-name={}", self.app),
            format!("--hint=string:x-dunst-stack-tag={}", self.tag),
            // Asked for every time, not only while replacing: the id is what makes the *next*
            // track a replacement, and the daemon that answers it is the one that supports it.
            "--print-id".to_string(),
        ];
        if self.replace {
            if let Some(id) = self.id {
                argv.push(format!("--replace-id={id}"));
            }
        }
        argv.push(summary.to_string());
        argv.push(body.to_string());
        argv
    }

    /// Keep the id `notify-send --print-id` printed, so the next track can replace this bubble.
    ///
    /// Anything that is not a number is a daemon that was not asked, or one that answered with
    /// something else; forgetting the id costs one extra bubble, never a wrong replacement.
    fn remember_id(&mut self, printed: &[u8]) {
        let text = String::from_utf8_lossy(printed);
        self.id = text.trim().parse::<i32>().ok();
    }

    /// The one line a dry run prints: the argv with arguments that contain whitespace quoted, or
    /// the printed line would be a different command from the one that would run.
    pub fn dry_line(argv: &[String]) -> String {
        let line: Vec<String> = argv
            .iter()
            .map(|arg| {
                if arg.chars().any(char::is_whitespace) {
                    format!("{arg:?}")
                } else {
                    arg.clone()
                }
            })
            .collect();
        format!("[notify] {}", line.join(" "))
    }
}

impl Sidecar for Notify {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        // The same four spellings for every switch: a table that says `on` in one key and `yes`
        // in another is a table nobody can remember.
        fn on_off(key: &str, value: &str) -> Result<bool, String> {
            match value.trim() {
                "true" | "on" | "yes" => Ok(true),
                "false" | "off" | "no" => Ok(false),
                other => Err(format!("{key} expects true/false, got `{other}`")),
            }
        }
        self.dry = hello.dry_run;
        for (key, value) in &hello.config {
            match key.as_str() {
                "dry_run" => self.dry = on_off(key, value)?,
                "replace" => self.replace = on_off(key, value)?,
                // The sender's name and its group: a notifier filed under something else (a
                // host's name, a second player) says so here.
                "app" => self.app = value.trim().to_string(),
                "tag" => self.tag = value.trim().to_string(),
                other => return Err(format!("`notify` does not know the key `{other}`")),
            }
        }
        if self.app.is_empty() {
            return Err("app must not be empty: it is the sender the daemon groups by".to_string());
        }
        if self.tag.is_empty() {
            return Err("tag must not be empty: it is the group inside the sender".to_string());
        }
        Ok(())
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        let Some(message) = self.change(state) else {
            return;
        };
        let argv = self.command(&state.title, &message);
        if self.dry {
            sink.log("info", Notify::dry_line(&argv));
            return;
        }
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output();
        match output {
            Ok(output) if output.status.success() => self.remember_id(&output.stdout),
            // One line per *run*, not per track: a missing `notify-send` is a property of the
            // machine, and repeating it for every song is noise in a log the user reads.
            _ if !self.reported => {
                self.reported = true;
                sink.log(
                    "warn",
                    "[notify] `notify-send` is not usable; the track was not announced",
                );
            }
            _ => {}
        }
    }
}

fn main() {
    aurora_protocol::serve(Notify::default())
}

#[cfg(test)]
mod tests {
    use super::Notify;
    use aurora_protocol::{Engine, Output, Sidecar, Sink, State};
    use std::sync::{Arc, Mutex};

    fn state(uri: &str, artist: &str, title: &str, album: &str) -> State {
        State {
            uri: uri.to_string(),
            artist: artist.to_string(),
            title: title.to_string(),
            album: album.to_string(),
            ..State::default()
        }
    }

    /// The sender is the argv: a group name, a stack tag, and an id that only the daemon can give.
    #[test]
    fn a_change_is_announced_once_per_track() {
        let mut notify = Notify::default();
        let first = state("u1", "A", "T", "Album");
        assert_eq!(
            notify.change(&first).as_deref(),
            Some("A — T · Album")
        );
        // The same track on the next poll is not a change.
        assert_eq!(notify.change(&first), None);
        let second = state("u2", "B", "S", "");
        assert_eq!(notify.change(&second).as_deref(), Some("B — S"));
        // Nothing playing is not a track: no bubble and no remembered uri.
        let empty = State::default();
        assert_eq!(notify.change(&empty), None);
        assert_eq!(
            notify.change(&first).as_deref(),
            Some("A — T · Album")
        );
    }

    #[test]
    fn a_sender_groups_and_replaces_its_own_bubble() {
        let mut notify = Notify::default();
        let first = notify.command("Artist — Title", "Artist — Title · Album");
        assert!(first.contains(&"--app-name=aurora".to_string()));
        assert!(first.contains(&"--hint=string:x-dunst-stack-tag=aurora:notify".to_string()));
        assert!(first.contains(&"--print-id".to_string()));
        assert!(!first.iter().any(|arg| arg.starts_with("--replace-id=")));

        // The daemon's answer makes the next track a replacement of the same bubble.
        notify.remember_id(b"42\n");
        let second = notify.command("A", "B");
        assert!(second.iter().any(|arg| arg == "--replace-id=42"));

        // A daemon that answered with something else is not a replacement we can trust: the
        // bubble is new, which is one extra notification and never a wrong one.
        notify.remember_id(b"Error\n");
        assert!(!notify
            .command("A", "B")
            .iter()
            .any(|arg| arg.starts_with("--replace-id=")));

        // `replace = false` keeps the sender and the group and turns the editing off.
        notify.remember_id(b"7\n");
        notify.replace = false;
        assert!(!notify
            .command("A", "B")
            .iter()
            .any(|arg| arg.starts_with("--replace-id=")));
    }

    #[test]
    fn an_empty_sender_is_a_configuration_error() {
        let mut notify = Notify::default();
        assert!(notify
            .hello(
                &aurora_protocol::Hello {
                    protocol: 1,
                    plugin: "notify".to_string(),
                    version: "0.1.0".to_string(),
                    dry_run: false,
                    config: vec![("app".to_string(), "  ".to_string())],
                },
                &noop_sink(),
            )
            .is_err());
    }

    fn noop_sink() -> Sink {
        Sink::new(|_| {})
    }

    #[test]
    fn a_dry_run_prints_the_argv_and_touches_no_daemon() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_for_plugin = seen.clone();
        let sink = Sink::new(move |output| sink_for_plugin.lock().unwrap().push(output));
        let mut engine = Engine::new(Notify::default());
        engine
            .line(
                r#"{"type":"hello","protocol":1,"plugin":"notify","version":"0.1.0","dry_run":true,"config":{}}"#,
                &sink,
            )
            .unwrap();
        engine
            .line(
                r#"{"type":"state","state":{"uri":"u1","artist":"A","title":"T","album":"Al"}}"#,
                &sink,
            )
            .unwrap();
        let outputs = seen.lock().unwrap();
        let line = outputs
            .iter()
            .find_map(|output| match output {
                Output::Log { text, .. } if text.starts_with("[notify] notify-send") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("the dry run prints the command");
        assert!(line.contains("--app-name=aurora"));
        assert!(line.contains("\"A — T · Al\""));
    }
}
