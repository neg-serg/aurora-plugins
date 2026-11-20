//! The query: tag soup in, a MusicBrainz search URL out — and the two fields of the reply.
//!
//! Kept with no clock, no process and no socket, so the rules can be tested as what they are:
//! functions from a snapshot to a string. Fetching lives in `main.rs`.

use aurora_protocol::State;

/// `https://musicbrainz.org/ws/2/recording?query=…&fmt=json&limit=3`
///
/// The fields are the ones a recording search indexes: `artist`, `recording`, `release` and `dur`.
/// `dur` is in **milliseconds** — the unit the index counts in, which is not the unit a tag
/// carries. The in-frame plugin sent the tag's seconds into that field, so every lookup matched
/// nothing and the line never appeared while the request itself looked right; the window here is a
/// second either side of the track's length.
pub fn lookup(state: &State) -> String {
    let mut terms: Vec<String> = Vec::new();
    let artist = normalise_artist(&state.artist);
    let title = normalise_title(&state.title);
    if !artist.is_empty() {
        terms.push(format!("artist:{}", quote(&artist)));
    }
    if !title.is_empty() {
        terms.push(format!("recording:{}", quote(&title)));
    }
    if !state.album.is_empty() {
        terms.push(format!("release:{}", quote(&state.album)));
    }
    if state.duration >= 1.0 {
        let ms = (state.duration * 1000.0).round() as i64;
        terms.push(format!("dur:[{} TO {}]", ms - 1000, ms + 1000));
    }
    let query = if terms.is_empty() {
        String::new()
    } else {
        terms.join(" AND ")
    };
    format!(
        "https://musicbrainz.org/ws/2/recording?query={}&fmt=json&limit=3",
        encode(&query)
    )
}

/// Drop the credits a library adds to `Artist`: `feat.`, `ft.`, `vs.`, and a collab list. What is
/// left is what a release credits. The **earliest** marker wins: `A & B feat. C` is a collab of A,
/// not "A & B".
///
/// The marker is searched on this string's own char boundaries: finding it in `to_lowercase()`
/// (whose byte offsets can differ — `ẞ` lowercases to the two-byte `ß`) and then truncating the
/// original at that offset cuts a character in half.
pub fn normalise_artist(artist: &str) -> String {
    let at = [" feat. ", " feat ", " ft. ", " ft ", " vs. ", " vs ", " & "]
        .iter()
        .filter_map(|marker| find_ci(artist, marker))
        .min();
    let text = at.map_or(artist, |at| &artist[..at]);
    text.trim().trim_end_matches(',').trim().to_string()
}

/// The byte offset of `marker` in `text`, case-insensitively, always on a char boundary.
fn find_ci(text: &str, marker: &str) -> Option<usize> {
    text.char_indices()
        .find(|(index, _)| text[*index..].to_lowercase().starts_with(marker))
        .map(|(index, _)| index)
}

/// What MusicBrainz stores as a separate tag rather than in the title.
const DECORATIONS: [&str; 6] = ["remaster", "live", "deluxe", "bonus", "mono", "stereo"];

fn decoration(text: &str) -> bool {
    let text = text.to_lowercase();
    DECORATIONS.iter().any(|word| text.contains(word))
}

/// Drop what a library adds to `Title`: a leading track number, and the bracketed decorations
/// (`(Remastered)`, `[Live]`, ` - 2011 Remaster`) that MusicBrainz stores separately.
///
/// The decorations may be stacked and may touch the title (`Song[Live]`, not just `Song [Live]`),
/// and a track number may be written `01. Title` or `01 - Title`.
pub fn normalise_title(title: &str) -> String {
    let mut text = title.trim();
    // `01. Title` and `01 - Title`: a track number is a position, not a name.
    if let Some(dot) = text.find(". ") {
        let head = &text[..dot];
        if !head.is_empty() && head.trim().chars().all(|c| c.is_ascii_digit()) {
            text = text[dot + 2..].trim();
        }
    }
    if let Some(dash) = text.find(" - ") {
        let head = &text[..dash];
        if !head.is_empty() && head.trim().chars().all(|c| c.is_ascii_digit()) {
            text = text[dash + 3..].trim();
        } else if decoration(&text[dash..]) {
            text = &text[..dash];
        }
    }
    // A trailing `(…)` or `[…]` decoration, repeated if the tags stacked, with or without the
    // space before it. The bracket's own contents are what is checked, so a title ending in a
    // plain `(` is left alone.
    loop {
        let Some((open, _)) = text
            .char_indices()
            .rev()
            .find(|(_, ch)| *ch == '(' || *ch == '[')
        else {
            break;
        };
        if decoration(&text[open..]) {
            text = text[..open].trim_end();
        } else {
            break;
        }
    }
    text.trim().to_string()
}

/// MusicBrainz's search syntax quotes a phrase with `"`, so any `"` inside a term has to go and
/// the whole thing is percent-encoded for the URL.
fn quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', ""))
}

/// Percent-encode everything a URL cannot carry literally. Space becomes `%20` rather than `+`
/// because that is what the search endpoint documents.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The argv a lookup runs with: the URL, the file the answer lands in, and the two switches that
/// make it safe to run behind a player (a timeout and the user agent MusicBrainz requires).
pub fn curl_args(url: &str, out: &std::path::Path) -> Vec<String> {
    vec![
        "-sS".to_string(),
        "--max-time".to_string(),
        "8".to_string(),
        // MusicBrainz asks for an identifying user agent and rate-limits anonymous clients
        // harder; one request per track change is far inside its limit either way.
        "-A".to_string(),
        concat!(
            "aurora-musicbrainz/",
            env!("CARGO_PKG_VERSION"),
            " ( https://github.com/neg-player )"
        )
        .to_string(),
        "-o".to_string(),
        out.to_string_lossy().into_owned(),
        url.to_string(),
    ]
}

/// `first-release-date` and `title` out of a MusicBrainz recording search reply.
///
/// Hand-parsed because the document is a known, shallow JSON shape and the fields are quoted
/// strings: this is not a JSON parser, it is a search for two keys in the first result object. A
/// missing key is `None`, never an error — MusicBrainz answers plenty of recordings with no
/// release date at all.
pub fn first_release(body: &str) -> Option<(String, String)> {
    // The *recording's* title is the first `"title"` in a search reply — the release's title comes
    // later, inside the releases array. And the first release date is whichever release the reply
    // listed first, which is MusicBrainz's own ranking.
    let title = value_of(body, "\"title\":")?;
    let date = value_of(body, "\"first-release-date\":");
    Some((title, date.unwrap_or_default()))
}

/// The string after `key` in a JSON document: quoted, with the escapes MusicBrainz uses (`\"` and
/// `\\`) undone.
fn value_of(text: &str, key: &str) -> Option<String> {
    let at = text.find(key)? + key.len();
    let rest = text[at..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    // A non-BMP character is two `\u` escapes; recombine the surrogate pair
                    // instead of dropping the whole answer on the lone high half.
                    let code = if (0xD800..=0xDBFF).contains(&code) {
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let hex: String = chars.by_ref().take(4).collect();
                        let low = u32::from_str_radix(&hex, 16).ok()?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return None;
                        }
                        0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                    } else {
                        code
                    };
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_artist_loses_its_credit() {
        assert_eq!(normalise_artist("Artist feat. Guest"), "Artist");
        assert_eq!(normalise_artist("Artist FT. Guest"), "Artist");
        assert_eq!(normalise_artist("Artist ft Guest"), "Artist");
        assert_eq!(normalise_artist("Artist vs. Other"), "Artist");
        // The earliest marker wins: a collab of A, not "A & B".
        assert_eq!(normalise_artist("A & B feat. C"), "A");
        assert_eq!(normalise_artist("Artist, "), "Artist");
        assert_eq!(normalise_artist("Just Artist"), "Just Artist");
        // The marker offset is found on this string's char boundaries: a lowercase find followed
        // by a truncate cuts a multi-byte character in half.
        assert_eq!(normalise_artist("ẞ feat. X"), "ẞ");
        assert_eq!(normalise_artist("İ feat. X"), "İ");
    }

    #[test]
    fn the_title_loses_the_stacked_decorations() {
        assert_eq!(normalise_title("01. Song"), "Song");
        assert_eq!(normalise_title("01 - Song"), "Song");
        assert_eq!(normalise_title("Song (Remastered)"), "Song");
        assert_eq!(normalise_title("Song[Live]"), "Song");
        assert_eq!(normalise_title("Song(Live)"), "Song");
        assert_eq!(normalise_title("Song [Live] [Remastered]"), "Song");
        assert_eq!(normalise_title("Song (Live) [Remastered]"), "Song");
        assert_eq!(normalise_title("Song - Remastered"), "Song");
        assert_eq!(normalise_title("Song (feat. Guest)"), "Song (feat. Guest)");
        assert_eq!(normalise_title("01. Song - 2011 Remaster"), "Song");
        assert_eq!(normalise_title("Rock - Paper"), "Rock - Paper");
        assert_eq!(normalise_title(""), "");
    }

    #[test]
    fn the_url_encodes_and_escapes_the_rest() {
        assert_eq!(encode("a b&c/d~e-f_g.h"), "a%20b%26c%2Fd~e-f_g.h");
        assert_eq!(
            encode("Мельница"),
            "%D0%9C%D0%B5%D0%BB%D1%8C%D0%BD%D0%B8%D1%86%D0%B0"
        );
        assert_eq!(quote("A \"B\" C"), "\"A B C\"");
    }

    #[test]
    fn a_search_reply_names_the_title_and_date() {
        let body = r#"{"recordings":[{"title":"Song \"quoted\"","first-release-date":"2020-01-01"}]}"#;
        assert_eq!(
            first_release(body),
            Some(("Song \"quoted\"".to_string(), "2020-01-01".to_string()))
        );
        let no_date = r#"{"recordings":[{"title":"Song"}]}"#;
        assert_eq!(
            first_release(no_date),
            Some(("Song".to_string(), String::new()))
        );
        assert_eq!(first_release("{}"), None);
        // MusicBrainz escapes a non-BMP character as a surrogate pair; the parser must recombine
        // it instead of dropping the whole answer on the lone high half.
        let emoji = r#"{"recordings":[{"title":"Note \ud83c\udfb5","first-release-date":"2021"}]}"#;
        assert_eq!(
            first_release(emoji),
            Some(("Note 🎵".to_string(), "2021".to_string()))
        );
    }

    #[test]
    fn the_lookup_names_the_artist_and_the_title() {
        let state = State {
            artist: "Artist".to_string(),
            title: "A Song".to_string(),
            ..State::default()
        };
        let url = lookup(&state);
        assert!(
            url.starts_with("https://musicbrainz.org/ws/2/recording?query="),
            "{url}"
        );
        assert!(url.contains("Artist") && url.contains("A%20Song"), "{url}");
        assert!(url.contains("limit=3"), "{url}");
    }

    #[test]
    fn the_duration_goes_into_the_query_in_milliseconds() {
        // The index counts milliseconds; a tag's seconds in that field matched nothing, and a
        // well-formed URL with no results is the hardest kind of bug to see.
        let state = State {
            artist: "Artist".to_string(),
            title: "Song".to_string(),
            duration: 240.0,
            ..State::default()
        };
        let url = lookup(&state);
        assert!(
            url.contains("dur%3A%5B239000%20TO%20241000%5D"),
            "the duration term is not milliseconds: {url}"
        );
    }

    #[test]
    fn the_curl_argv_pins_the_timeout_and_the_answer_file() {
        let out = std::path::Path::new("/tmp/answer.json");
        let args = curl_args("https://example.test/", out);
        assert!(args.contains(&"--max-time".to_string()));
        assert!(args.contains(&"8".to_string()));
        assert!(args.contains(&"/tmp/answer.json".to_string()));
        assert_eq!(args.last().unwrap(), "https://example.test/");
        // An identifying user agent is a MusicBrainz requirement, not a politeness.
        assert!(args.iter().any(|arg| arg.starts_with("aurora-musicbrainz/")));
    }
}
