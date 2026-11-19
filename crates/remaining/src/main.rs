//! `remaining`: how much music is left in the queue, said on the status bar.
//!
//! The plugin is the old in-tree `remaining` moved across the wire: the player hands it the
//! queue's two numbers, and it answers one line. It draws nothing and asks for no command; the
//! `status` permission is the right to speak in the bar, and the slot `queue` is the fact — two
//! plugins that count the same thing cannot both appear.
//!
//! An empty text is not "no answer": it is the answer "there is nothing to say", and the player
//! drops the line. That is what makes a cleared queue clear the bar instead of freezing the last
//! number on it.

use aurora_protocol::{Hello, Sidecar, Sink, State};

/// The fact this line fills. `queue-stats` asks the same question in the same words, so the two
/// can never both appear in the bar.
const SLOT: &str = "queue";

#[derive(Default)]
struct Remaining {
    /// The last line sent, so a state that did not move the number costs no output.
    last: String,
}

impl Sidecar for Remaining {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        if let Some((key, _)) = hello.config.first() {
            return Err(format!("`remaining` does not know the key `{key}`"));
        }
        Ok(())
    }

    fn state(&mut self, state: &State, sink: &Sink) {
        let text = if state.uri.is_empty() {
            String::new()
        } else {
            let left = human(state.queue_left);
            let tracks = match state.queue_len {
                0 | 1 => String::new(),
                n => format!(" · {}", tracks(n)),
            };
            format!("{left}{tracks}")
        };
        if text == self.last {
            return;
        }
        sink.status(SLOT, text.clone());
        self.last = text;
    }
}

/// `2 трека`, `5 треков`, `21 трек` — the Russian count, because the rest of this UI speaks it.
fn tracks(n: usize) -> String {
    plural(n as u64, "трек", "трека", "треков")
}

fn plural(n: u64, one: &str, few: &str, many: &str) -> String {
    let form = match (n % 10, n % 100) {
        (1, rest) if rest != 11 => one,
        (2..=4, rest) if !(12..=14).contains(&rest) => few,
        _ => many,
    };
    format!("{n} {form}")
}

/// `1 ч 02 мин`, `42 мин`, `38 с`, and `—` for nothing left.
///
/// Seconds below a minute are the difference between "one more track" and "a coffee", so they
/// are kept; above an hour the minutes are padded so the string does not change width while the
/// counter runs.
fn human(seconds: f64) -> String {
    if !(seconds >= 1.0) {
        return "—".to_string();
    }
    let total = seconds.round() as i64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours} ч {minutes:02} мин")
    } else if minutes > 0 {
        format!("{minutes} мин")
    } else {
        format!("{secs} с")
    }
}

fn main() {
    aurora_protocol::serve(Remaining::default())
}

#[cfg(test)]
mod tests {
    use super::{human, tracks};
    use aurora_protocol::{Engine, Output, Sidecar, Sink};
    use std::sync::{Arc, Mutex};

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

    fn state(uri: &str, left: f64, len: usize) -> String {
        format!(
            r#"{{"type":"state","state":{{"uri":"{uri}","queue_left":{left},"queue_len":{len}}}}}"#
        )
    }

    #[test]
    fn the_track_count_pluralizes_like_the_language() {
        assert_eq!(tracks(0), "0 треков");
        assert_eq!(tracks(1), "1 трек");
        assert_eq!(tracks(2), "2 трека");
        assert_eq!(tracks(5), "5 треков");
        assert_eq!(tracks(11), "11 треков");
        assert_eq!(tracks(14), "14 треков");
        assert_eq!(tracks(21), "21 трек");
        assert_eq!(tracks(22), "22 трека");
        assert_eq!(tracks(25), "25 треков");
        assert_eq!(tracks(111), "111 треков");
    }

    #[test]
    fn the_left_time_keeps_seconds_below_a_minute_and_pads_minutes_above_an_hour() {
        assert_eq!(human(0.0), "—");
        assert_eq!(human(0.9), "—");
        assert_eq!(human(-5.0), "—");
        assert_eq!(human(f64::NAN), "—");
        assert_eq!(human(1.0), "1 с");
        assert_eq!(human(59.4), "59 с");
        assert_eq!(human(59.6), "1 мин");
        assert_eq!(human(60.0), "1 мин");
        assert_eq!(human(3599.0), "59 мин");
        assert_eq!(human(3600.0), "1 ч 00 мин");
        assert_eq!(human(3661.0), "1 ч 01 мин");
    }

    #[test]
    fn a_state_says_the_line_once_per_change() {
        let outputs = run(
            super::Remaining::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"remaining","version":"0.1","dry_run":false,"config":{}}"#,
                &state("u1", 1234.0, 5),
                &state("u1", 1234.0, 5),
                &state("u1", 60.0, 2),
            ],
        );
        let statuses: Vec<&Output> = outputs
            .iter()
            .filter(|output| matches!(output, Output::Status { .. }))
            .collect();
        assert_eq!(statuses.len(), 2);
        assert_eq!(
            statuses[0],
            &Output::Status {
                slot: "queue".to_string(),
                text: "20 мин · 5 треков".to_string()
            }
        );
        assert_eq!(
            statuses[1],
            &Output::Status {
                slot: "queue".to_string(),
                text: "1 мин · 2 трека".to_string()
            }
        );
    }

    #[test]
    fn an_empty_queue_clears_the_line() {
        let outputs = run(
            super::Remaining::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"remaining","version":"0.1","dry_run":false,"config":{}}"#,
                &state("u1", 300.0, 3),
                r#"{"type":"state","state":{"uri":""}}"#,
            ],
        );
        let statuses: Vec<&Output> = outputs
            .iter()
            .filter(|output| matches!(output, Output::Status { .. }))
            .collect();
        assert_eq!(statuses.len(), 2);
        assert_eq!(
            statuses[1],
            &Output::Status {
                slot: "queue".to_string(),
                text: String::new()
            }
        );
    }

    #[test]
    fn an_unknown_key_is_a_configuration_error() {
        let outputs = run(
            super::Remaining::default(),
            &[
                r#"{"type":"hello","protocol":1,"plugin":"remaining","version":"0.1","dry_run":false,"config":{"nope":"1"}}"#,
            ],
        );
        assert!(matches!(
            &outputs[0],
            Output::Log { level, .. } if level == "error"
        ));
    }
}
