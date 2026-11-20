//! `autodj`: keep the queue from running out.
//!
//! The old in-tree plugin moved across the wire with its rules intact, and only its clock
//! changed: the player sends a snapshot when the now-playing facts move instead of calling a
//! `frame` on every tick, so "once per poll" became "once per state" — which is the same rule
//! seen from the other side of the pipe.
//!
//! # Who decides what to play
//!
//! The plugin decides **when**: the queue has fewer than `min` tracks after the playing one, so it
//! is time to add some. The player decides **what**: it owns the library, the queue and the
//! daemon, and a program that picked tracks itself would need a snapshot of the whole library to
//! pick from. So the plugin asks for `queue_similar` and the player resolves it — the playing
//! track's nearest neighbours in the `local-music-ai` MERT index, or the same artist when a
//! machine has no index or the track is newer than it. That fallback used to *be* the feature;
//! the command is the same either way, and the plugin does not branch on the stack.
//!
//! # Once per track, not once per snapshot
//!
//! A snapshot arrives for a volume nudge, a pause and a queue change too, and a plugin that
//! reacted to "the queue is short" on every one of them would ask for more music four times a
//! second and fill the queue with the same twenty tracks. So the plugin remembers the track it
//! topped up around and stays quiet until the playhead moves: one top-up per track, and the queue
//! growing is itself the next answer.

use aurora_protocol::{Action, Hello, Sidecar, Sink, State};

/// Fill the queue when fewer tracks than this are left after the playing one.
const DEFAULT_MIN: usize = 2;
/// How many to ask for in one go.
const DEFAULT_FILL: u8 = 3;

pub struct Autodj {
    /// Fill the queue when fewer tracks than this are left after the playing one.
    min_after: usize,
    /// How many to ask for in one go.
    fill: u8,
    /// The URI the queue was last topped up around.
    filled_for: String,
}

impl Default for Autodj {
    fn default() -> Self {
        Autodj {
            min_after: DEFAULT_MIN,
            fill: DEFAULT_FILL,
            filled_for: String::new(),
        }
    }
}

impl Autodj {
    /// The command this snapshot should ask for, if any.
    ///
    /// Split out from [`Sidecar::state`] so the "short queue once, not on every snapshot" rule is
    /// a function a test can call thirteen times in a row.
    pub fn plan(&mut self, state: &State) -> Option<Action> {
        // A track with no artist still has neighbours: the index keys on the file, not on the tag,
        // so the only "nothing to top up around" is an empty playhead.
        if state.is_empty() {
            return None;
        }
        if state.queue_after >= self.min_after {
            return None;
        }
        if self.filled_for == state.uri {
            return None;
        }
        self.filled_for = state.uri.clone();
        Some(Action::QueueSimilar { count: self.fill })
    }
}

impl Sidecar for Autodj {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        for (key, value) in &hello.config {
            match key.as_str() {
                "min" => {
                    let min: usize = value
                        .parse()
                        .map_err(|_| format!("min expects a count, got `{value}`"))?;
                    if min > 50 {
                        return Err(format!("min must be 0..=50, got {min}"));
                    }
                    self.min_after = min;
                }
                "fill" => {
                    let fill: u8 = value
                        .parse()
                        .map_err(|_| format!("fill expects a count, got `{value}`"))?;
                    if fill == 0 || fill > 50 {
                        return Err(format!("fill must be 1..=50, got {fill}"));
                    }
                    self.fill = fill;
                }
                other => return Err(format!("`autodj` does not know the key `{other}`")),
            }
        }
        Ok(())
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        if let Some(action) = self.plan(state) {
            sink.command(action);
        }
    }
}

fn main() {
    aurora_protocol::serve(Autodj::default())
}

#[cfg(test)]
mod tests {
    use super::Autodj;
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

    const HELLO: &str = r#"{"type":"hello","protocol":1,"plugin":"autodj","version":"0.1","dry_run":false,"config":{}}"#;

    fn state(uri: &str, queue_after: usize) -> String {
        format!(r#"{{"type":"state","state":{{"uri":"{uri}","queue_after":{queue_after}}}}}"#)
    }

    #[test]
    fn a_short_queue_is_topped_up_once_per_track() {
        let mut autodj = Autodj::default();
        let short = |uri: &str| {
            let mut state = aurora_protocol::State::default();
            state.uri = uri.to_string();
            state.queue_after = 1;
            state
        };
        assert_eq!(
            autodj.plan(&short("u1")),
            Some(aurora_protocol::Action::QueueSimilar { count: 3 })
        );
        // The next snapshot sees the same track and stays quiet; only a new track may ask again.
        assert_eq!(autodj.plan(&short("u1")), None);
        assert_eq!(
            autodj.plan(&short("u2")),
            Some(aurora_protocol::Action::QueueSimilar { count: 3 })
        );
    }

    #[test]
    fn a_deep_or_empty_queue_is_left_alone() {
        let mut autodj = Autodj::default();
        let mut deep = aurora_protocol::State::default();
        deep.uri = "u1".to_string();
        deep.queue_after = 2;
        assert_eq!(autodj.plan(&deep), None);
        assert_eq!(autodj.plan(&aurora_protocol::State::default()), None);
        // No artist is not "nothing to top up around": the index keys on the file.
        let mut untagged = aurora_protocol::State::default();
        untagged.uri = "u1".to_string();
        assert_eq!(
            autodj.plan(&untagged),
            Some(aurora_protocol::Action::QueueSimilar { count: 3 })
        );
    }

    #[test]
    fn the_config_sets_the_thresholds() {
        let mut autodj = Autodj::default();
        autodj
            .hello(
                &aurora_protocol::Hello {
                    protocol: 1,
                    plugin: "autodj".to_string(),
                    version: "0.1".to_string(),
                    dry_run: false,
                    config: vec![
                        ("min".to_string(), "5".to_string()),
                        ("fill".to_string(), "2".to_string()),
                    ],
                },
                &Sink::new(|_| {}),
            )
            .expect("valid settings");
        let mut state = aurora_protocol::State::default();
        state.uri = "u1".to_string();
        state.queue_after = 4;
        assert_eq!(
            autodj.plan(&state),
            Some(aurora_protocol::Action::QueueSimilar { count: 2 })
        );
        let mut deep = aurora_protocol::State::default();
        deep.uri = "u2".to_string();
        deep.queue_after = 5;
        assert_eq!(autodj.plan(&deep), None);
    }

    #[test]
    fn an_unknown_key_is_a_configuration_error() {
        let outputs = run(
            Autodj::default(),
            &[r#"{"type":"hello","protocol":1,"plugin":"autodj","version":"0.1","dry_run":false,"config":{"nope":"1"}}"#],
        );
        assert!(matches!(
            &outputs[0],
            Output::Log { level, .. } if level == "error"
        ));
    }

    #[test]
    fn a_state_becomes_one_command_and_a_repeat_becomes_none() {
        let outputs = run(
            Autodj::default(),
            &[HELLO, &state("u1", 1), &state("u1", 1), &state("u2", 1)],
        );
        let commands: Vec<&Output> = outputs
            .iter()
            .filter(|output| matches!(output, Output::Command(_)))
            .collect();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0],
            &Output::Command(aurora_protocol::Action::QueueSimilar { count: 3 })
        );
    }

    #[test]
    fn a_deep_queue_says_nothing() {
        let outputs = run(Autodj::default(), &[HELLO, &state("u1", 7)]);
        assert!(!outputs
            .iter()
            .any(|output| matches!(output, Output::Command(_))));
    }
}
