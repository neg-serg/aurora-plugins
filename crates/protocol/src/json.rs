//! The smallest JSON reader and writer this project needs.
//!
//! The wire is objects, arrays, strings, numbers and booleans, and this crate has no dependencies
//! on purpose: a parser for those is a few hundred lines instead of a dependency tree, and a plugin
//! is a program that should build with `cargo build` on a machine with no network. The codec came
//! from aurora's model layer (`camilla-model/src/json.rs`), where the player uses the same one.
//!
//! Two properties matter more than speed here:
//!
//! * **errors say where** — a reply that does not parse is a bug in the writer and the byte
//!   offset is what turns it into a five-minute fix;
//! * **nothing panics on hostile input** — the parser is recursive, so it is depth-limited: a line
//!   is not allowed to overflow the stack, however deep it nests.

#![forbid(unsafe_code)]

/// One JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Key order is kept: a config is displayed in the order the daemon serialized it, and
    /// `filter` lists that reshuffle between frames are unreadable.
    Obj(Vec<(String, Value)>),
}

impl Default for Value {
    /// `Null`: a config that was never read is empty, not a dead value.
    fn default() -> Self {
        Value::Null
    }
}

impl Value {
    /// The member `key`, when this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at a chain of keys, for the shapes a config nests (devices.capture.type).
    pub fn at(&self, path: &[&str]) -> Option<&Value> {
        let mut value = self;
        for key in path {
            value = value.get(key)?;
        }
        Some(value)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Obj(members) => Some(members),
            _ => None,
        }
    }

    /// A string as the daemon sends it, a number formatted, a boolean worded: what a readout wants.
    pub fn text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Str(text) => text.clone(),
            Value::Bool(value) => value.to_string(),
            Value::Num(value) => number_text(*value),
            Value::Arr(_) | Value::Obj(_) => self.to_json(),
        }
    }

    /// A short string literal for a request: the inverse of [`Value::as_str`].
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Num(value) => {
                // `NaN` and the infinities are not JSON; a null is the only honest encoding.
                if value.is_finite() {
                    out.push_str(&number_text(*value));
                } else {
                    out.push_str("null");
                }
            }
            Value::Str(text) => write_string(text, out),
            Value::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Obj(members) => {
                out.push('{');
                for (i, (key, item)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    item.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// How a number is written in a request.
///
/// Whole values lose the `.0` (CamillaDSP's setters accept both, but `-6` reads like the number the
/// user typed) and everything else keeps Rust's shortest round-trip form. Past 2^53 the integer
/// cast is not exact, so the float form is used instead — a channel count never gets there, and a
/// value that does should not be quietly rewritten.
fn number_text(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// A quoted string with the escapes JSON requires.
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// How deep a reply may nest before it is refused. A config is four or five levels deep; the limit
/// is for the reply that is not one.
const MAX_DEPTH: u32 = 96;

/// Parse one JSON document.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    parser.space();
    let value = parser.value(0)?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(parser.problem("trailing bytes after the document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn problem(&self, what: &str) -> String {
        format!("{what} at byte {}", self.at)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.problem("document nests too deeply"));
        }
        match self.peek() {
            None => Err(self.problem("expected a value, found the end")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(other) => Err(self.problem(&format!("unexpected byte {}", other as char))),
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.problem(&format!("expected `{word}`")))
        }
    }

    fn object(&mut self, depth: u32) -> Result<Value, String> {
        self.at += 1; // `{`
        let mut members = Vec::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Obj(members));
        }
        loop {
            self.space();
            if self.peek() != Some(b'"') {
                return Err(self.problem("expected a member name"));
            }
            let key = self.string()?;
            self.space();
            if self.bump() != Some(b':') {
                return Err(self.problem("expected `:` after a member name"));
            }
            self.space();
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.space();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => return Ok(Value::Obj(members)),
                _ => return Err(self.problem("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self, depth: u32) -> Result<Value, String> {
        self.at += 1; // `[`
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            self.space();
            items.push(self.value(depth + 1)?);
            self.space();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => return Ok(Value::Arr(items)),
                _ => return Err(self.problem("expected `,` or `]`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.problem("unterminated string")),
                Some(b'"') => return Ok(out),
                Some(b'\\') => {
                    let escaped = self.bump().ok_or_else(|| self.problem("unterminated escape"))?;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.codepoint()?),
                        other => {
                            return Err(self.problem(&format!(
                                "unknown escape \\{}",
                                other as char
                            )))
                        }
                    }
                }
                Some(byte) if byte < 0x20 => {
                    return Err(self.problem("a control byte in a string"));
                }
                Some(byte) if byte < 0x80 => out.push(byte as char),
                // A multi-byte character: hand the rest of the sequence back to the string decoder
                // rather than decoding UTF-8 by hand.
                Some(byte) => {
                    self.at -= 1;
                    let rest = std::str::from_utf8(&self.bytes[self.at..])
                        .map_err(|_| self.problem("a string is not UTF-8"))?;
                    let ch = rest
                        .chars()
                        .next()
                        .ok_or_else(|| self.problem("a string ends inside a character"))?;
                    out.push(ch);
                    self.at += ch.len_utf8();
                    let _ = byte;
                }
            }
        }
    }

    /// The four hex digits after `\u`, including a surrogate pair when one follows.
    fn codepoint(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&first) {
            // A high surrogate is only legal with its low half; without one the replacement
            // character is the least surprising answer, and it is what a JSON library does.
            if self.bytes[self.at..].starts_with(b"\\u") {
                self.at += 2;
                let second = self.hex4()?;
                if (0xDC00..0xE000).contains(&second) {
                    0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                } else {
                    0xFFFD
                }
            } else {
                0xFFFD
            }
        } else if (0xDC00..0xE000).contains(&first) {
            0xFFFD
        } else {
            first
        };
        char::from_u32(code).ok_or_else(|| self.problem("a codepoint that is not a character"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.bump().ok_or_else(|| self.problem("short \\u escape"))?;
            let digit = (byte as char)
                .to_digit(16)
                .ok_or_else(|| self.problem("a \\u escape with a non-hex digit"))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let mut digits = 0;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
            digits += 1;
        }
        if digits == 0 {
            return Err(self.problem("a number with no digits"));
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            let mut fraction = 0;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
                fraction += 1;
            }
            if fraction == 0 {
                return Err(self.problem("a number with a point and no digits after it"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            let mut exponent = 0;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
                exponent += 1;
            }
            if exponent == 0 {
                return Err(self.problem("an exponent with no digits"));
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| self.problem("a number that is not ASCII"))?;
        text.parse::<f64>()
            .map(Value::Num)
            .map_err(|_| self.problem("a number that does not parse"))
    }
}

/// Convenience for a request body: `obj(&[("SetVolume", Value::Num(-3.0))])`.
pub fn obj(members: &[(&str, Value)]) -> Value {
    Value::Obj(
        members
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_envelope_the_daemon_sends() {
        let value = parse(r#"{"GetState":{"result":"Ok","value":"Running"}}"#).unwrap();
        assert_eq!(
            value.at(&["GetState", "result"]).and_then(Value::as_str),
            Some("Ok")
        );
        assert_eq!(
            value.at(&["GetState", "value"]).and_then(Value::as_str),
            Some("Running")
        );
    }

    #[test]
    fn reads_levels_and_escapes() {
        let value = parse(r#"{"a":[-26.5,0,-1000],"b":"a\"b\\c\n\u00e9\ud83d\ude00"}"#).unwrap();
        let levels: Vec<f64> = value
            .get("a")
            .and_then(Value::as_arr)
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert_eq!(levels, vec![-26.5, 0.0, -1000.0]);
        assert_eq!(
            value.get("b").and_then(Value::as_str),
            Some("a\"b\\c\né😀")
        );
    }

    #[test]
    fn writes_what_it_reads() {
        let text = r#"{"title":"Aurora","devices":{"samplerate":48000},"filters":{},"pipeline":[{"channels":[0,1],"names":["bass"]}],"on":true,"off":null}"#;
        let value = parse(text).unwrap();
        assert_eq!(value.to_json(), text);
    }

    #[test]
    fn whole_numbers_lose_the_point() {
        assert_eq!(Value::Num(-6.0).to_json(), "-6");
        assert_eq!(Value::Num(-6.5).to_json(), "-6.5");
        assert_eq!(Value::Num(0.0).to_json(), "0");
        assert_eq!(Value::Num(f64::NAN).to_json(), "null");
    }

    #[test]
    fn refuses_what_is_not_json() {
        assert!(parse("{").is_err());
        assert!(parse("{} x").is_err());
        assert!(parse(r#"{"a":}"#).is_err());
        assert!(parse(r#"{"a":"unterminated}"#).is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse("1e").is_err());
        assert!(parse("").is_err());
        // The depth limit is the only defence a recursive parser has on a 4 GB reply.
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn a_string_keeps_non_ascii() {
        let value = parse("\"привет, 世界\"").unwrap();
        assert_eq!(value.as_str(), Some("привет, 世界"));
        assert_eq!(value.to_json(), "\"привет, 世界\"");
    }

    #[test]
    fn obj_builds_a_request() {
        assert_eq!(
            obj(&[("SetVolume", Value::Num(-3.0))]).to_json(),
            r#"{"SetVolume":-3}"#
        );
    }
}
