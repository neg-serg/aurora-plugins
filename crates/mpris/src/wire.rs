//! Just enough D-Bus to serve MPRIS on the session bus.
//!
//! # Why this exists instead of a dependency
//!
//! `zbus` would be the obvious choice for a D-Bus client, and adding it is a decision for the
//! owner (`docs/plan-plugins.md` §7). Until that decision is taken, this module does the part
//! MPRIS actually needs, which is small because the specification is small and regular:
//!
//! * the message header is fixed (12 bytes) followed by an array of `(byte, variant)` fields;
//! * every value is little-endian (the endianness is a flag, and everything on x86/ARM Linux
//!   writes `l`), and every value starts at an offset multiple of its own alignment;
//! * MPRIS uses six types: `s` string, `o` object path, `g` signature, `u` u32, `x` int64,
//!   `b` bool, `d` double, plus `v` variants, `a{sv}` and arrays of them.
//!
//! What this module is **not**: general. It marshals the handful of shapes above and parses the
//! handful of method calls MPRIS receives. That is the honest scope of "serve three interfaces",
//! and it is why the byte layout below is tested rather than trusted.

/// A message is little-endian, method calls and returns both.
pub const ENDIAN_LITTLE: u8 = b'l';

pub const TYPE_METHOD_CALL: u8 = 1;
pub const TYPE_METHOD_RETURN: u8 = 2;
pub const TYPE_ERROR: u8 = 3;
pub const TYPE_SIGNAL: u8 = 4;

/// Header field codes from the specification.
pub const FIELD_PATH: u8 = 1;
pub const FIELD_INTERFACE: u8 = 2;
pub const FIELD_MEMBER: u8 = 3;
pub const FIELD_ERROR: u8 = 4;
pub const FIELD_REPLY_SERIAL: u8 = 5;
pub const FIELD_DESTINATION: u8 = 6;
pub const FIELD_SENDER: u8 = 7;
pub const FIELD_SIGNATURE: u8 = 8;

/// One field of a message header.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Path(String),
    Interface(String),
    Member(String),
    Error(String),
    ReplySerial(u32),
    Destination(String),
    Sender(String),
    Signature(String),
}

impl Field {
    fn code(&self) -> u8 {
        match self {
            Field::Path(_) => FIELD_PATH,
            Field::Interface(_) => FIELD_INTERFACE,
            Field::Member(_) => FIELD_MEMBER,
            Field::Error(_) => FIELD_ERROR,
            Field::ReplySerial(_) => FIELD_REPLY_SERIAL,
            Field::Destination(_) => FIELD_DESTINATION,
            Field::Sender(_) => FIELD_SENDER,
            Field::Signature(_) => FIELD_SIGNATURE,
        }
    }
}

/// A message: a type, a serial, the header fields and an already-marshalled body.
#[derive(Clone, Debug)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub fields: Vec<Field>,
    pub body: Vec<u8>,
}

impl Message {
    pub fn new(kind: u8, serial: u32, fields: Vec<Field>, body: Vec<u8>) -> Message {
        Message {
            kind,
            flags: 0,
            serial,
            fields,
            body,
        }
    }

    /// A method return answering `call`.
    pub fn reply(call: &Message, serial: u32, signature: &str, body: Vec<u8>) -> Message {
        let mut fields = vec![
            Field::ReplySerial(call.serial),
            Field::Destination(call.sender().unwrap_or_default().to_string()),
        ];
        if !signature.is_empty() {
            fields.push(Field::Signature(signature.to_string()));
        }
        Message::new(TYPE_METHOD_RETURN, serial, fields, body)
    }

    /// An error reply.
    pub fn error(call: &Message, serial: u32, name: &str, text: &str) -> Message {
        let mut body = Writer::new();
        body.string(text);
        Message::new(
            TYPE_ERROR,
            serial,
            vec![
                Field::Error(name.to_string()),
                Field::ReplySerial(call.serial),
                Field::Destination(call.sender().unwrap_or_default().to_string()),
                Field::Signature("s".to_string()),
            ],
            body.finish(),
        )
    }

    /// A signal on `interface`.
    pub fn signal(
        serial: u32,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<u8>,
    ) -> Message {
        Message::new(
            TYPE_SIGNAL,
            serial,
            vec![
                Field::Path(path.to_string()),
                Field::Interface(interface.to_string()),
                Field::Member(member.to_string()),
                Field::Signature("sa{sv}as".to_string()),
            ],
            body,
        )
    }

    pub fn path(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Path(p) => Some(p.as_str()),
            _ => None,
        })
    }

    pub fn interface(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Interface(p) => Some(p.as_str()),
            _ => None,
        })
    }

    pub fn member(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Member(p) => Some(p.as_str()),
            _ => None,
        })
    }

    pub fn signature(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Signature(p) => Some(p.as_str()),
            _ => None,
        })
    }

    pub fn sender(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Sender(p) => Some(p.as_str()),
            _ => None,
        })
    }

    pub fn reply_serial(&self) -> Option<u32> {
        self.fields.iter().find_map(|f| match f {
            Field::ReplySerial(s) => Some(*s),
            _ => None,
        })
    }

    /// The D-Bus error name on an error reply, for the one line a failed call reports.
    pub fn error_name(&self) -> Option<&str> {
        self.get(|f| match f {
            Field::Error(name) => Some(name.as_str()),
            _ => None,
        })
    }

    /// The first field of one kind, borrowed from this message.
    fn get(&self, pick: fn(&Field) -> Option<&str>) -> Option<&str> {
        self.fields.iter().find_map(pick)
    }

    /// Marshal the whole message: fixed header, header fields, padding, body.
    pub fn encode(&self) -> Vec<u8> {
        let mut fields = Writer::new();
        for field in &self.fields {
            // Every `(byte, variant)` struct in the header array starts on an eight-byte
            // boundary. The buffer itself begins at message offset 16, which is aligned, so
            // padding inside it is padding in the message.
            fields.pad(8);
            fields.u8(field.code());
            match field {
                Field::Path(p) => fields.variant("o", &mut |w: &mut Writer| w.path(p)),
                Field::Interface(p) | Field::Member(p) | Field::Error(p) => {
                    fields.variant("s", &mut |w: &mut Writer| w.string(p))
                }
                Field::Destination(p) | Field::Sender(p) => {
                    fields.variant("s", &mut |w: &mut Writer| w.string(p))
                }
                Field::ReplySerial(s) => fields.variant("u", &mut |w: &mut Writer| w.u32(*s)),
                Field::Signature(s) => fields.variant("g", &mut |w: &mut Writer| w.signature(s)),
            }
        }
        let field_bytes = fields.finish();

        let mut out = Vec::with_capacity(16 + field_bytes.len() + self.body.len());
        out.push(ENDIAN_LITTLE);
        out.push(self.kind);
        out.push(self.flags);
        out.push(1); // protocol version
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&(field_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&field_bytes);
        // The body starts on an eight-byte boundary, whatever the header fields left behind.
        while out.len() % 8 != 0 {
            out.push(0);
        }
        out.extend_from_slice(&self.body);
        out
    }

    /// Parse one message out of `bytes`, returning it and how many bytes it took.
    ///
    /// `Err(())` means "not a whole message yet" — the socket gave us a prefix, which is normal
    /// for a stream and not an error. `Err(Some(reason))` would be a protocol violation; this
    /// codec treats both the same way and drops the buffer, because a half-parsed message is
    /// worse than a missed one.
    pub fn decode(bytes: &[u8]) -> Option<(Message, usize)> {
        if bytes.len() < 16 {
            return None;
        }
        if bytes[0] != ENDIAN_LITTLE {
            return None;
        }
        let kind = bytes[1];
        let flags = bytes[2];
        let body_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let serial = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let fields_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        let fields_end = 16 + fields_len;
        if bytes.len() < fields_end {
            return None;
        }
        // The body is aligned to eight bytes after the header fields.
        let mut body_at = fields_end;
        while body_at % 8 != 0 {
            body_at += 1;
        }
        if bytes.len() < body_at + body_len {
            return None;
        }

        let mut reader = Reader::new(&bytes[16..fields_end]);
        let mut fields = Vec::new();
        while !reader.done() {
            // Each `(byte, variant)` struct in the header array starts on an eight-byte
            // boundary; the slice begins at message offset 16, which is aligned, so aligning
            // here is aligning in the message.
            reader.align(8);
            if reader.done() {
                break;
            }
            let code = reader.u8()?;
            let signature = reader.signature()?;
            let field = match (code, signature.as_str()) {
                (FIELD_PATH, "o") => Field::Path(reader.string()?),
                (FIELD_INTERFACE, "s") => Field::Interface(reader.string()?),
                (FIELD_MEMBER, "s") => Field::Member(reader.string()?),
                (FIELD_ERROR, "s") => Field::Error(reader.string()?),
                (FIELD_REPLY_SERIAL, "u") => Field::ReplySerial(reader.u32()?),
                (FIELD_DESTINATION, "s") => Field::Destination(reader.string()?),
                (FIELD_SENDER, "s") => Field::Sender(reader.string()?),
                (FIELD_SIGNATURE, "g") => Field::Signature(reader.signature()?),
                // A field this client does not use: skip it by its own signature.
                (_, other) => {
                    reader.skip(other)?;
                    continue;
                }
            };
            fields.push(field);
        }

        let message = Message {
            kind,
            flags,
            serial,
            fields,
            body: bytes[body_at..body_at + body_len].to_vec(),
        };
        Some((message, body_at + body_len))
    }
}

// ---------------------------------------------------------------- writer

/// Little-endian marshaller with the specification's alignment rule: every value starts at a
/// multiple of its own alignment, counted from the start of the buffer it lives in.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { buf: Vec::new() }
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Pad with NULs until the offset is a multiple of `align`. Public because a caller that
    /// marshals into a buffer which does not start at offset 0 (a message body inside a header)
    /// still owns the padding rule at that seam.
    pub fn pad(&mut self, align: usize) {
        while self.buf.len() % align != 0 {
            self.buf.push(0);
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.pad(8);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.pad(8);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// `b` is a u32 that must be 0 or 1.
    pub fn bool(&mut self, v: bool) {
        self.u32(u32::from(v));
    }

    pub fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    pub fn path(&mut self, s: &str) {
        self.string(s);
    }

    /// `g`: a length byte, the bytes, a NUL.
    pub fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// `v`: the signature of the value, then the value at its own alignment.
    pub fn variant(&mut self, signature: &str, value: &mut dyn FnMut(&mut Writer)) {
        self.signature(signature);
        value(self);
    }

    /// `a…`: a byte length, padding to the element alignment, then the elements.
    pub fn array(&mut self, element_align: usize, write: &mut dyn FnMut(&mut Writer)) {
        // The length field is written last, so its offset has to be recorded *after* aligning
        // for the u32 itself: taking it before the padding writes the length over the padding
        // and leaves a zero where the length should be, which is a protocol violation the bus
        // answers by dropping the connection.
        self.pad(4);
        let length_at = self.buf.len();
        self.u32(0);
        self.pad(element_align);
        let start = self.buf.len();
        write(self);
        let length = (self.buf.len() - start) as u32;
        self.buf[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
    }

    /// `(...)`: a struct, whose fields follow the same alignment rule as everything else.
    pub fn structure(&mut self, align: usize, write: &mut dyn FnMut(&mut Writer)) {
        self.pad(align);
        write(self);
    }

    /// `a{sv}`: what MPRIS answers to `Properties.GetAll`, and what a `PropertiesChanged` signal
    /// carries.
    pub fn dict_of_variants(&mut self, entries: &[(&str, &Prop)]) {
        self.array(8, &mut |w: &mut Writer| {
            for (name, value) in entries {
                w.pad(8);
                w.string(name);
                match value {
                    Prop::Str(s) => w.variant("s", &mut |w: &mut Writer| w.string(s)),
                    Prop::Obj(s) => w.variant("o", &mut |w: &mut Writer| w.path(s)),
                    Prop::Bool(b) => w.variant("b", &mut |w: &mut Writer| w.bool(*b)),
                    Prop::U32(n) => w.variant("u", &mut |w: &mut Writer| w.u32(*n)),
                    Prop::I64(n) => w.variant("x", &mut |w: &mut Writer| w.i64(*n)),
                    Prop::F64(n) => w.variant("d", &mut |w: &mut Writer| w.f64(*n)),
                    Prop::StrList(items) => w.variant("as", &mut |w: &mut Writer| {
                        w.array(4, &mut |w: &mut Writer| {
                            for item in items {
                                w.string(item);
                            }
                        })
                    }),
                    Prop::Dict(items) => w.variant("a{sv}", &mut |w: &mut Writer| {
                        let pairs: Vec<(&str, &Prop)> =
                            items.iter().map(|(n, v)| (n.as_str(), v)).collect();
                        w.dict_of_variants(&pairs)
                    }),
                    Prop::EmptyDict => w.variant("a{sv}", &mut |w: &mut Writer| {
                        w.array(8, &mut |_: &mut Writer| {})
                    }),
                }
            }
        });
    }
}

/// The property values MPRIS uses, already resolved to what goes on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    Str(String),
    Obj(String),
    Bool(bool),
    U32(u32),
    I64(i64),
    F64(f64),
    StrList(Vec<String>),
    /// `a{sv}` with values inside: what `Metadata` is.
    Dict(Vec<(String, Prop)>),
    /// An empty `a{sv}`: `Metadata` for a player with nothing loaded, key present.
    EmptyDict,
}

impl Prop {
    /// The D-Bus signature of this value, for the variant wrapper.
    pub fn signature(&self) -> &'static str {
        match self {
            Prop::Str(_) => "s",
            Prop::Obj(_) => "o",
            Prop::Bool(_) => "b",
            Prop::U32(_) => "u",
            Prop::I64(_) => "x",
            Prop::F64(_) => "d",
            Prop::StrList(_) => "as",
            Prop::Dict(_) => "a{sv}",
            Prop::EmptyDict => "a{sv}",
        }
    }

    pub fn write(&self, w: &mut Writer) {
        match self {
            Prop::Str(s) => w.string(s),
            Prop::Obj(s) => w.path(s),
            Prop::Bool(b) => w.bool(*b),
            Prop::U32(n) => w.u32(*n),
            Prop::I64(n) => w.i64(*n),
            Prop::F64(n) => w.f64(*n),
            Prop::StrList(items) => w.array(4, &mut |w: &mut Writer| {
                for item in items {
                    w.string(item);
                }
            }),
            Prop::Dict(items) => {
                let pairs: Vec<(&str, &Prop)> =
                    items.iter().map(|(n, v)| (n.as_str(), v)).collect();
                w.dict_of_variants(&pairs)
            }
            Prop::EmptyDict => w.array(8, &mut |_: &mut Writer| {}),
        }
    }
}

// ---------------------------------------------------------------- reader

/// Little-endian reader with the same alignment rule as the writer.
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, at: 0 }
    }

    pub fn done(&self) -> bool {
        self.at >= self.buf.len()
    }

    /// Pad the reader to a multiple of `align`, the same rule the writer follows.
    pub fn align(&mut self, align: usize) {
        while self.at % align != 0 {
            self.at += 1;
        }
    }

    fn need(&self, n: usize) -> Option<()> {
        (self.at + n <= self.buf.len()).then_some(())
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.need(1)?;
        let v = self.buf[self.at];
        self.at += 1;
        Some(v)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.align(4);
        self.need(4)?;
        let v = u32::from_le_bytes(self.buf[self.at..self.at + 4].try_into().ok()?);
        self.at += 4;
        Some(v)
    }

    pub fn i64(&mut self) -> Option<i64> {
        self.align(8);
        self.need(8)?;
        let v = i64::from_le_bytes(self.buf[self.at..self.at + 8].try_into().ok()?);
        self.at += 8;
        Some(v)
    }

    pub fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        self.need(len + 1)?;
        let s = String::from_utf8_lossy(&self.buf[self.at..self.at + len]).into_owned();
        self.at += len + 1;
        Some(s)
    }

    pub fn signature(&mut self) -> Option<String> {
        let len = self.u8()? as usize;
        self.need(len + 1)?;
        let s = String::from_utf8_lossy(&self.buf[self.at..self.at + len]).into_owned();
        self.at += len + 1;
        Some(s)
    }

    /// Skip one value of `signature`, which is what a header field this client does not use
    /// needs.
    pub fn skip(&mut self, signature: &str) -> Option<()> {
        let mut chars = signature.chars();
        self.skip_one(&mut chars)
    }

    fn skip_one(&mut self, sig: &mut std::str::Chars<'_>) -> Option<()> {
        let ch = sig.next()?;
        match ch {
            's' | 'o' => {
                self.string()?;
            }
            'g' => {
                self.signature()?;
            }
            'u' => {
                self.u32()?;
            }
            'b' => {
                self.u32()?;
            }
            'x' | 't' | 'd' => {
                self.i64()?;
            }
            'v' => {
                let inner = self.signature()?;
                let mut chars = inner.chars();
                self.skip_one(&mut chars)?;
            }
            'a' => {
                // An array's elements are skipped one by one inside its byte range.
                let len = self.u32()? as usize;
                let align = element_align(sig.clone());
                self.align(align);
                let end = self.at + len;
                while self.at < end {
                    self.skip_one(&mut sig.clone())?;
                }
            }
            '(' | '{' => {
                // A struct or a dictionary entry is not one value: its fields have to be read
                // until the matching close, or the next field is read out of the middle of it.
                // The old arm consumed only the opening bracket, which left an `a{sv}` field
                // half-read and made the whole message undecodable.
                let close = if ch == '(' { ')' } else { '}' };
                while sig.clone().next() != Some(close) {
                    self.skip_one(sig)?;
                }
                sig.next()?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// Element alignment for the array whose element signature starts at the reader's position.
fn element_align(mut sig: std::str::Chars<'_>) -> usize {
    match sig.next() {
        Some('s') | Some('o') | Some('g') | Some('u') | Some('b') | Some('a') => 4,
        Some('x') | Some('t') | Some('d') | Some('(') | Some('{') => 8,
        Some('v') => 1,
        _ => 4,
    }
}





#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    #[test]
    fn the_writer_aligns_each_value() {
        let mut writer = Writer::new();
        writer.u8(1);
        writer.u32(0x1122_3344);
        writer.i64(7);
        let bytes = writer.finish();
        assert_eq!(&bytes[1..4], &[0, 0, 0]);
        assert_eq!(u32_at(&bytes, 4), 0x1122_3344);
        assert_eq!(i64::from_le_bytes(bytes[8..16].try_into().unwrap()), 7);
    }

    #[test]
    fn the_primitives_round_trip() {
        let mut writer = Writer::new();
        writer.string("héllo");
        writer.path("/org/mpris/MediaPlayer2");
        writer.signature("a{sv}");
        writer.bool(true);
        writer.i64(-3);
        writer.f64(1.5);
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.string().as_deref(), Some("héllo"));
        assert_eq!(reader.string().as_deref(), Some("/org/mpris/MediaPlayer2"));
        assert_eq!(reader.signature().as_deref(), Some("a{sv}"));
        assert_eq!(reader.u32(), Some(1));
        assert_eq!(reader.i64(), Some(-3));
        // The reader has no f64: it is the same eight bytes a `d` is.
        let bits = reader.i64().expect("the double's bytes");
        assert_eq!(f64::from_bits(bits as u64), 1.5);
        assert!(reader.done());
    }

    #[test]
    fn an_array_carries_its_byte_length_and_its_elements() {
        let mut writer = Writer::new();
        writer.array(4, &mut |w: &mut Writer| {
            w.string("a");
            w.string("bc");
        });
        let bytes = writer.finish();
        // The length counts the elements from after the length field; the second string starts
        // on the next four-byte boundary, which is inside that count.
        assert_eq!(u32_at(&bytes, 0), (bytes.len() - 4) as u32);
        assert_eq!(u32_at(&bytes, 0), 15);
        assert_eq!(bytes.len(), 19);
    }

    #[test]
    fn the_properties_dictionary_is_aligned_entries() {
        let mut writer = Writer::new();
        writer.dict_of_variants(&[
            ("PlaybackStatus", &Prop::Str("Playing".to_string())),
            ("Volume", &Prop::F64(0.5)),
            ("CanGoNext", &Prop::Bool(true)),
        ]);
        let bytes = writer.finish();
        // The length counts the entries from the element alignment, not the length field
        // itself (and not the padding between them).
        let len = u32_at(&bytes, 0) as usize;
        assert_eq!(len, bytes.len() - 8);
        assert_eq!(u32_at(&bytes, 8), 14);
        assert_eq!(&bytes[12..26], b"PlaybackStatus");
    }

    #[test]
    fn a_message_round_trips_through_the_wire() {
        let call = Message::new(
            TYPE_METHOD_CALL,
            7,
            vec![
                Field::Path("/org/mpris/MediaPlayer2".to_string()),
                Field::Interface("org.mpris.MediaPlayer2.Player".to_string()),
                Field::Member("SetPosition".to_string()),
                Field::Destination("org.mpris.MediaPlayer2.player".to_string()),
                Field::Signature("ox".to_string()),
            ],
            {
                let mut writer = Writer::new();
                writer.path("/org/mpris/MediaPlayer2/Track/1");
                writer.i64(1234);
                writer.finish()
            },
        );
        let bytes = call.encode();
        let (decoded, used) = Message::decode(&bytes).expect("a whole message");
        assert_eq!(used, bytes.len());
        assert_eq!(decoded.kind, TYPE_METHOD_CALL);
        assert_eq!(decoded.serial, 7);
        assert_eq!(decoded.path(), Some("/org/mpris/MediaPlayer2"));
        assert_eq!(
            decoded.interface(),
            Some("org.mpris.MediaPlayer2.Player")
        );
        assert_eq!(decoded.member(), Some("SetPosition"));
        assert_eq!(decoded.signature(), Some("ox"));
        assert_eq!(decoded.fields, call.fields);
        assert_eq!(decoded.body, call.body);
    }

    #[test]
    fn a_prefix_is_not_a_message() {
        let bytes = Message::new(
            TYPE_METHOD_CALL,
            1,
            vec![Field::Member("Next".to_string())],
            Vec::new(),
        )
        .encode();
        for cut in 0..bytes.len() {
            assert!(Message::decode(&bytes[..cut]).is_none(), "cut {cut}");
        }
        let mut bad = bytes.clone();
        bad[0] = b'B';
        assert!(Message::decode(&bad).is_none());
    }

    #[test]
    fn the_reply_error_and_signal_frames_carry_their_fields() {
        let call = Message::new(
            TYPE_METHOD_CALL,
            3,
            vec![Field::Sender(":1.5".to_string())],
            Vec::new(),
        );
        let reply = Message::reply(
            &call,
            9,
            "s",
            {
                let mut writer = Writer::new();
                writer.string("ok");
                writer.finish()
            },
        );
        assert_eq!(reply.reply_serial(), Some(3));
        assert_eq!(reply.signature(), Some("s"));
        let error = Message::error(&call, 10, "org.freedesktop.DBus.Error.InvalidArgs", "bad");
        let (decoded, _) = Message::decode(&error.encode()).expect("an error frame");
        assert_eq!(decoded.kind, TYPE_ERROR);
        assert_eq!(decoded.reply_serial(), Some(3));
        assert_eq!(
            decoded.error_name(),
            Some("org.freedesktop.DBus.Error.InvalidArgs")
        );
        let signal = Message::signal(
            11,
            "/org/mpris/MediaPlayer2",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Vec::new(),
        );
        assert_eq!(signal.path(), Some("/org/mpris/MediaPlayer2"));
        assert_eq!(signal.member(), Some("PropertiesChanged"));
        assert_eq!(signal.signature(), Some("sa{sv}as"));
    }

    #[test]
    fn an_unknown_field_closes_over_structs_and_dicts() {
        // An `a{sv}` header field this client does not use must be skipped as a whole.
        let mut writer = Writer::new();
        writer.dict_of_variants(&[
            ("a", &Prop::Bool(true)),
            ("b", &Prop::Str("x".to_string())),
        ]);
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.skip("a{sv}"), Some(()));
        assert!(reader.done());

        let mut writer = Writer::new();
        writer.structure(8, &mut |w: &mut Writer| {
            w.string("x");
            w.u32(7);
        });
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.skip("(su)"), Some(()));
        assert!(reader.done());

        // A variant whose value is a struct.
        let mut writer = Writer::new();
        writer.variant("(su)", &mut |w: &mut Writer| {
            w.pad(8);
            w.string("x");
            w.u32(7);
        });
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.skip("v"), Some(()));
        assert!(reader.done());
    }

    #[test]
    fn every_property_has_its_signature() {
        let cases = [
            (Prop::Str("s".to_string()), "s"),
            (Prop::Obj("/o".to_string()), "o"),
            (Prop::Bool(true), "b"),
            (Prop::U32(1), "u"),
            (Prop::I64(1), "x"),
            (Prop::F64(1.0), "d"),
            (Prop::StrList(vec!["a".to_string()]), "as"),
            (
                Prop::Dict(vec![("k".to_string(), Prop::U32(1))]),
                "a{sv}",
            ),
            (Prop::EmptyDict, "a{sv}"),
        ];
        for (prop, signature) in cases {
            assert_eq!(prop.signature(), signature);
            let mut writer = Writer::new();
            prop.write(&mut writer);
            let bytes = writer.finish();
            if signature == "a{sv}" {
                let len = u32_at(&bytes, 0) as usize;
                assert_eq!(len, bytes.len() - 8, "{prop:?}");
            }
        }
    }
}
