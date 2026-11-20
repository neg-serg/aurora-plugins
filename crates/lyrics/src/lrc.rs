//! The `.lrc` parser: timestamps and text, and nothing else.
//!
//! A `.lrc` file is a list of lines, most of them `[mm:ss.xx] text`, but the format has a few
//! habits worth handling because every real file uses some of them: several timestamps on one
//! line (a chorus reused at three points), metadata tags such as `[ar: ...]` that are not
//! timestamps at all, CRLF line endings from Windows editors, and lines that arrive out of order
//! because somebody appended one.
//!
//! Kept in its own module with no clock and no drawing so it can be tested as what it is: a
//! function from text to a sorted list of (seconds, line).

/// One line of lyrics and the moment it becomes the current one.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub at: f64,
    pub text: String,
}

/// Parse a whole `.lrc` file.
///
/// Lines without a timestamp are dropped (`[ar: ...]` metadata, comments, blank lines): they are
/// not lyrics, and treating them as "at time zero" would put a title above the first verse.
/// Empty text *with* a timestamp is kept — an instrumental break is a real line that empties the
/// display, and dropping it would leave the previous verse up through the whole gap.
pub fn parse(text: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for raw in text.lines() {
        let raw = raw.trim_end_matches('\r');
        let mut rest = raw.trim_start();
        let mut stamps: Vec<f64> = Vec::new();
        // Every leading `[...]` is either a timestamp or a tag; a tag stops the scan, and the
        // text that follows is whatever is left after the last timestamp.
        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else {
                break;
            };
            let inside = &after[..end];
            let Some(at) = timestamp(inside) else {
                // A tag (`ar:`, `offset:`): the whole line is metadata, so it is not lyrics.
                stamps.clear();
                rest = "";
                break;
            };
            stamps.push(at);
            rest = &after[end + 1..];
        }
        let text = rest.trim();
        for at in stamps {
            lines.push(Line {
                at,
                text: text.to_string(),
            });
        }
    }
    // Stable sort: two lines stamped at the same moment keep the order the file wrote them in,
    // which is the only ordering the author expressed.
    lines.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

/// `mm:ss.xx`, `mm:ss.xxx`, `mm:ss` and the `[h:mm:ss.xx]` variant some editors write.
///
/// The hours form was documented but refused: the old parser read two numbers and rejected a
/// third, so a file that used `[1:02:03.5]` silently lost every line. The two forms differ in
/// what the first number means, and both keep the seconds inside a minute.
fn timestamp(inside: &str) -> Option<f64> {
    let mut parts = inside.split(':');
    let first = parts.next()?.trim().parse::<f64>().ok()?;
    let second = parts.next()?.trim().parse::<f64>().ok()?;
    if !first.is_finite() || first < 0.0 || !second.is_finite() {
        return None;
    }
    let (hours, minutes, seconds) = match parts.next() {
        Some(third) if parts.next().is_none() => {
            let seconds = third.trim().parse::<f64>().ok()?;
            if !(0.0..60.0).contains(&second) {
                return None;
            }
            (first, second, seconds)
        }
        // A single number with no colon is not a timestamp (it is a tag such as `[3]`); more than
        // three parts is not a timestamp either.
        Some(_) => return None,
        None => (0.0, first, second),
    };
    if !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// The index of the line that is current at `elapsed`, or `None` before the first one.
///
/// Binary search over the sorted list: the frame asks this once per draw, and a linear scan of a
/// thousand lines on every frame of a song is work nobody asked for.
pub fn active_at(lines: &[Line], elapsed: f64) -> Option<usize> {
    if lines.is_empty() || !elapsed.is_finite() || elapsed < lines[0].at {
        // A non-finite clock (a daemon that has not answered, or a bad cast upstream) would make
        // every comparison false and `low` stay zero — the old `low - 1` then panicked.
        return None;
    }
    let mut low = 0usize;
    let mut high = lines.len();
    while low < high {
        let mid = (low + high) / 2;
        if lines[mid].at <= elapsed {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    Some(low - 1)
}



#[cfg(test)]
mod tests {
    use super::{active_at, parse, timestamp, Line};

    fn line(at: f64, text: &str) -> Line {
        Line {
            at,
            text: text.to_string(),
        }
    }

    #[test]
    fn every_timestamp_form_the_doc_names_parses() {
        assert_eq!(timestamp("00:12.50"), Some(12.5));
        assert_eq!(timestamp("00:12.500"), Some(12.5));
        assert_eq!(timestamp("00:12"), Some(12.0));
        // The hours variant: documented, and previously dropped by the two-part parser.
        assert_eq!(timestamp("1:02:03.5"), Some(3723.5));
        assert_eq!(timestamp("0:00:01"), Some(1.0));
        // Minutes may exceed 59 in the two-part form; the three-part form keeps them in range.
        assert_eq!(timestamp("100:00"), Some(6000.0));
        assert_eq!(timestamp("1:60:00"), None);
        assert_eq!(timestamp("00:60"), None);
        assert_eq!(timestamp("12"), None);
        assert_eq!(timestamp("ar:Artist"), None);
        assert_eq!(timestamp("1:2:3:4"), None);
        assert_eq!(timestamp("-1:00"), None);
        assert_eq!(timestamp("00:-1"), None);
        assert_eq!(timestamp("00:NaN"), None);
    }

    #[test]
    fn a_file_with_tags_multiple_stamps_and_windows_endings_parses() {
        let text = "[ar:Artist]\r\n[00:10.00]second\r\n[00:01.00][00:05.00]chorus\r\n[offset:100]\r\n[00:00.50]\r\nnot a line\r\n";
        let lines = parse(text);
        assert_eq!(
            lines,
            vec![
                line(0.5, ""),
                line(1.0, "chorus"),
                line(5.0, "chorus"),
                line(10.0, "second"),
            ]
        );
    }

    #[test]
    fn the_active_line_is_the_last_one_at_or_before_the_time() {
        let lines = parse("[00:10]a\n[00:10]b\n[00:20]c");
        assert_eq!(active_at(&lines, 0.0), None);
        assert_eq!(active_at(&lines, 9.9), None);
        // Two lines stamped at one instant: the stable sort keeps the file's order, and the
        // lookup takes the last of them, so the later line wins.
        assert_eq!(active_at(&lines, 10.0), Some(1));
        assert_eq!(active_at(&lines, 19.9), Some(1));
        assert_eq!(active_at(&lines, 20.0), Some(2));
        assert_eq!(active_at(&lines, 100.0), Some(2));
        assert_eq!(active_at(&[], 1.0), None);
        // A non-finite clock is "no line", not a subtraction below zero.
        assert_eq!(active_at(&lines, f64::NAN), None);
        assert_eq!(active_at(&lines, f64::INFINITY), None);
    }
}
