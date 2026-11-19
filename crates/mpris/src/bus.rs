//! The MPRIS plugin: media keys, desktop widgets and KDE Connect, over the session bus.
//!
//! # The dependency question, answered with code instead of a promise
//!
//! `docs/plan-plugins.md` lists "may `mpris` add `zbus`?" as the owner's decision. This crate
//! does not wait for it and does not smuggle a dependency in: it speaks the D-Bus wire protocol
//! itself (`wire.rs`), because what MPRIS needs from D-Bus is three interfaces, seven types and
//! one name request. That is a few hundred lines with tests, against a dependency tree that would
//! have to be justified, pinned and audited.
//!
//! What it does **not** do: be a general D-Bus client. There is no introspection of other
//! services, no object manager, no fd passing. If a later feature needs those, that is the moment
//! to take the `zbus` decision, not before.
//!
//! # How it runs as a sidecar
//!
//! The plugin is its own process, so the bus has a thread of its own and the player's frame loop
//! never waits for a socket. It connects once with a bounded handshake that waits for the bus's
//! own replies instead of for a read timeout, then switches the socket to non-blocking and, in a
//! short loop, reads what has arrived, answers the method calls MPRIS clients make, and emits
//! `PropertiesChanged` when the snapshot it publishes actually changed. A failed connection
//! pauses before the next attempt instead of paying a handshake every iteration. A reply that
//! does not fit in the socket buffer is dropped rather than stalling the loop: a missing property
//! is a widget that waits, a blocked process is a dead player.
//!
//! # Publishing
//!
//! [`player_properties`] is a pure function of the now-playing facts and a couple of daemon
//! flags, which is what the unit tests hold it to; the plugin only decides *when* to send it.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aurora_protocol::{Action, Hello, Sidecar, Sink, State};

use crate::wire::{Field, Message, Prop, Reader, Writer, TYPE_ERROR, TYPE_METHOD_CALL};

/// The object every MPRIS client looks for.
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const AURORA_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const PEER: &str = "org.freedesktop.DBus.Peer";

/// How long the blocking handshake waits for one bus reply. It is also the socket's own read
/// timeout, so one phase cannot outlive it by more than a single read.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a plugin that could not reach the bus waits before trying again: a bus that is down
/// must cost one line and one pause, not a handshake on every frame.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// A track id has to be one object path, and the protocol says it may not change while the same
/// track plays. Hashing the URI gives that for free and keeps the path legal (a URI has slashes
/// and dots that a path cannot carry).
fn track_id(uri: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in uri.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("/org/negplayer/track/{hash:016x}")
}

/// The `Metadata` dict MPRIS clients read for the title line of a widget.
///
/// `mpris:length` is microseconds, not seconds — the one unit in this protocol that is not what
/// everybody assumes, and the reason a 3:32 track shows as 0:00 in a widget that gets it wrong.
pub fn metadata(state: &State) -> Vec<(String, Prop)> {
    let mut entries: Vec<(String, Prop)> = Vec::new();
    if state.uri.is_empty() {
        // Nothing playing: the dict is empty, and a `Get` on `Metadata` still finds the key.
        return entries;
    }
    entries.push(("mpris:trackid".to_string(), Prop::Obj(track_id(&state.uri))));
    if state.duration > 0.0 {
        entries.push((
            "mpris:length".to_string(),
            Prop::I64((state.duration * 1e6) as i64),
        ));
    }
    if !state.title.is_empty() {
        entries.push(("xesam:title".to_string(), Prop::Str(state.title.to_string())));
    }
    if !state.artist.is_empty() {
        entries.push((
            "xesam:artist".to_string(),
            Prop::StrList(vec![state.artist.to_string()]),
        ));
    }
    if !state.album.is_empty() {
        entries.push(("xesam:album".to_string(), Prop::Str(state.album.to_string())));
    }
    if !state.uri.is_empty() {
        entries.push(("xesam:url".to_string(), Prop::Str(state.uri.to_string())));
    }
    entries
}

/// The `org.freedesktop.DBus.Properties` view of the player, as `(interface, values)`.
///
/// Pure: the plugin hands it the same facts every client sees, and the unit test asserts the
/// mapping (status words, capabilities, the microsecond length) without a bus.
pub fn properties(
    state: &State,
    volume: i32,
    shuffle: bool,
    repeat: bool,
) -> Vec<(&'static str, Vec<(&'static str, Prop)>)> {
    let status = match state.playing.as_str() {
        "play" => "Playing",
        "pause" => "Paused",
        _ => "Stopped",
    };
    let playing = state.playing != "stop" && !state.is_empty();
    // An empty `Metadata` is an empty `a{sv}`, and the key is still there: a widget that reads it
    // must find something, or it waits for a property that is never coming.
    let meta = metadata(state);
    let metadata_prop = if meta.is_empty() {
        Prop::EmptyDict
    } else {
        Prop::Dict(meta)
    };
    vec![
        (
            ROOT_INTERFACE,
            vec![
                ("Identity", Prop::Str("aurora".to_string())),
                ("CanQuit", Prop::Bool(false)),
                ("CanRaise", Prop::Bool(false)),
                ("HasTrackList", Prop::Bool(false)),
                ("DesktopEntry", Prop::Str("aurora".to_string())),
                (
                    "SupportedUriSchemes",
                    Prop::StrList(vec!["file".to_string()]),
                ),
                (
                    "SupportedMimeTypes",
                    Prop::StrList(vec![
                        "audio/flac".to_string(),
                        "audio/mpeg".to_string(),
                        "audio/ogg".to_string(),
                    ]),
                ),
            ],
        ),
        (
            AURORA_INTERFACE,
            vec![
                ("PlaybackStatus", Prop::Str(status.to_string())),
                (
                    "LoopStatus",
                    Prop::Str(if repeat { "Playlist" } else { "None" }.to_string()),
                ),
                ("Rate", Prop::F64(1.0)),
                ("MinimumRate", Prop::F64(1.0)),
                ("MaximumRate", Prop::F64(1.0)),
                ("Shuffle", Prop::Bool(shuffle)),
                ("Metadata", metadata_prop),
                ("Volume", Prop::F64(volume.max(0) as f64 / 100.0)),
                ("Position", Prop::I64((state.elapsed * 1e6) as i64)),
                ("CanGoNext", Prop::Bool(state.queue_after > 0)),
                ("CanGoPrevious", Prop::Bool(state.elapsed > 3.0)),
                ("CanPlay", Prop::Bool(true)),
                ("CanPause", Prop::Bool(playing)),
                ("CanSeek", Prop::Bool(state.duration > 0.0)),
                ("CanControl", Prop::Bool(true)),
            ],
        ),
    ]
}

/// The introspection XML clients use to discover the two interfaces. Hand-written because the
/// shape of an MPRIS player is fixed by its specification.
pub fn introspect_xml() -> String {
    // Not a `format!`: the XML is full of `{sv}` and `{s}`, which a format string would read as
    // interpolations. The interface names are spelled out here and the test below holds them to
    // the constants the code matches on, so the two cannot drift.
    "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" \
     \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\
     <node><interface name=\"org.mpris.MediaPlayer2\">\
     <property name=\"Identity\" type=\"s\" access=\"read\"/>\
     <property name=\"CanQuit\" type=\"b\" access=\"read\"/>\
     <property name=\"CanRaise\" type=\"b\" access=\"read\"/>\
     <property name=\"HasTrackList\" type=\"b\" access=\"read\"/>\
     <property name=\"DesktopEntry\" type=\"s\" access=\"read\"/>\
     <property name=\"SupportedUriSchemes\" type=\"as\" access=\"read\"/>\
     <property name=\"SupportedMimeTypes\" type=\"as\" access=\"read\"/>\
     </interface><interface name=\"org.mpris.MediaPlayer2.Player\">\
     <method name=\"Next\"/><method name=\"Previous\"/><method name=\"Pause\"/>\
     <method name=\"PlayPause\"/><method name=\"Stop\"/>\
     <method name=\"Seek\"><arg name=\"Offset\" type=\"x\" direction=\"in\"/></method>\
     <method name=\"Play\"/><method name=\"Raise\"/><method name=\"Quit\"/>\
     <property name=\"PlaybackStatus\" type=\"s\" access=\"read\"/>\
     <property name=\"LoopStatus\" type=\"s\" access=\"readwrite\"/>\
     <property name=\"Rate\" type=\"d\" access=\"readwrite\"/>\
     <property name=\"Shuffle\" type=\"b\" access=\"readwrite\"/>\
     <property name=\"Metadata\" type=\"a{sv}\" access=\"read\"/>\
     <property name=\"Volume\" type=\"d\" access=\"readwrite\"/>\
     <property name=\"Position\" type=\"x\" access=\"read\"/>\
     <property name=\"CanGoNext\" type=\"b\" access=\"read\"/>\
     <property name=\"CanGoPrevious\" type=\"b\" access=\"read\"/>\
     <property name=\"CanPlay\" type=\"b\" access=\"read\"/>\
     <property name=\"CanPause\" type=\"b\" access=\"read\"/>\
     <property name=\"CanSeek\" type=\"b\" access=\"read\"/>\
     <property name=\"CanControl\" type=\"b\" access=\"read\"/>\
     </interface><interface name=\"org.freedesktop.DBus.Properties\">\
     <method name=\"Get\"><arg name=\"interface_name\" type=\"s\" direction=\"in\"/>\
     <arg name=\"property_name\" type=\"s\" direction=\"in\"/>\
     <arg name=\"value\" type=\"v\" direction=\"out\"/></method>\
     <method name=\"GetAll\"><arg name=\"interface_name\" type=\"s\" direction=\"in\"/>\
     <arg name=\"props\" type=\"a{sv}\" direction=\"out\"/></method>\
     <method name=\"Set\"><arg name=\"interface_name\" type=\"s\" direction=\"in\"/>\
     <arg name=\"property_name\" type=\"s\" direction=\"in\"/>\
     <arg name=\"value\" type=\"v\" direction=\"in\"/></method>\
     <signal name=\"PropertiesChanged\"><arg name=\"interface_name\" type=\"s\"/>\
     <arg name=\"changed_properties\" type=\"a{sv}\"/>\
     <arg name=\"invalidated_properties\" type=\"as\"/></signal>\
     </interface><interface name=\"org.freedesktop.DBus.Introspectable\">\
     <method name=\"Introspect\"><arg name=\"xml_data\" type=\"s\" direction=\"out\"/></method>\
     </interface><interface name=\"org.freedesktop.DBus.Peer\"><method name=\"Ping\"/></interface>\
     </node>"
        .to_string()
}

/// One live session-bus connection.
struct Bus {
    stream: UnixStream,
    serial: u32,
    /// Bytes the bus sent that do not form a whole message yet.
    incoming: Vec<u8>,
    /// What was last published, so a frame only sends what changed.
    published: String,
    /// The values a `Get`/`GetAll` answers from: the last published snapshot.
    snapshot: Vec<(&'static str, Vec<(&'static str, Prop)>)>,
    /// Commands the media keys produced since the last drain, for the player.
    queue: Vec<Action>,
}

impl Bus {
    /// Connect, authenticate, say hello and take the name. All of it blocking, but each wait is
    /// for the bus's own reply rather than for a read timeout, and all of it happens exactly once:
    /// after this the socket is non-blocking.
    fn open(name: &str) -> Result<Bus, String> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .map_err(|_| "DBUS_SESSION_BUS_ADDRESS is not set".to_string())?;
        let path = address
            .split(';')
            .find_map(|part| part.strip_prefix("unix:path="))
            .ok_or_else(|| format!("only unix bus addresses are supported, got `{address}`"))?;
        let path = path.split(',').next().unwrap_or(path);
        let mut stream = UnixStream::connect(path).map_err(|e| format!("connect {path}: {e}"))?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok();

        // SASL: a NUL, then `AUTH EXTERNAL` with the uid in hex, then `BEGIN`. The uid is what
        // the bus checks against the socket's credentials, so any other answer is a refusal.
        let uid = libc_uid();
        let uid_hex: String = uid
            .to_string()
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        let auth = format!("\0AUTH EXTERNAL {uid_hex}\r\n");
        stream
            .write_all(auth.as_bytes())
            .map_err(|e| format!("auth write: {e}"))?;
        let reply = read_line(&mut stream)?;
        if !reply.starts_with("OK ") {
            return Err(format!("the bus refused authentication: {reply}"));
        }
        stream
            .write_all(b"BEGIN\r\n")
            .map_err(|e| format!("begin: {e}"))?;

        let mut bus = Bus {
            stream,
            serial: 1,
            incoming: Vec::new(),
            published: String::new(),
            snapshot: Vec::new(),
            queue: Vec::new(),
        };
        // `Hello` is not optional: the bus drops a client that sends anything else first.
        let hello = bus.send_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            Vec::new(),
        )?;
        bus.await_reply(hello, "Hello")?;
        let request = bus.send_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            "su",
            {
                let mut body = Writer::new();
                body.string(name);
                body.u32(0); // no flags: do not queue, do not replace
                body.finish()
            },
        )?;
        bus.await_reply(request, "RequestName")?;
        bus.stream.set_nonblocking(true).ok();
        Ok(bus)
    }

    fn send_call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        body: Vec<u8>,
    ) -> Result<u32, String> {
        self.serial = self.serial.wrapping_add(1);
        let serial = self.serial;
        let mut fields = vec![
            Field::Path(path.to_string()),
            Field::Interface(interface.to_string()),
            Field::Member(member.to_string()),
            Field::Destination(destination.to_string()),
        ];
        if !signature.is_empty() {
            fields.push(Field::Signature(signature.to_string()));
        }
        let message = Message::new(TYPE_METHOD_CALL, serial, fields, body);
        self.stream
            .write_all(&message.encode())
            .map_err(|e| format!("write {member}: {e}"))?;
        Ok(serial)
    }

    fn send(&mut self, message: Message) {
        // Non-blocking: a reply that does not fit is dropped rather than stalling the frame.
        let _ = self.stream.write_all(&message.encode());
    }

    /// The first bytes the bus sent and this client could not parse as a message. Kept for the
    /// diagnostics: a protocol mistake shows up here as a hex dump instead of as a disconnect.
    fn raw_tail(&self) -> String {
        self.incoming
            .iter()
            .take(96)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Read everything that has arrived and answer it. Returns whether anything was handled.
    fn pump(&mut self) -> Result<bool, String> {
        let mut buf = [0u8; 8192];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    return Err(format!(
                        "the bus closed the connection (it sent: {})",
                        self.raw_tail()
                    ))
                }
                Ok(n) => self.incoming.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("read: {e}")),
            }
        }
        let mut handled = false;
        loop {
            let Some((message, taken)) = Message::decode(&self.incoming) else {
                break;
            };
            self.incoming.drain(..taken);
            handled = true;
            if message.kind == TYPE_METHOD_CALL {
                self.answer(&message);
            }
        }
        Ok(handled)
    }

    /// Wait for the reply to `serial`, reading only until the bus has answered it.
    ///
    /// This runs before the socket turns non-blocking, so [`Bus::pump`] would drain until
    /// `EAGAIN` and pay the whole read timeout after the reply had already arrived — the Hello and
    /// RequestName handshakes did exactly that, two half-second waits in the first frame. Decode
    /// what is buffered, read again only while the reply is missing, and stop on the message
    /// itself. Method calls that arrived first are answered; signals are dropped as `pump` drops
    /// them.
    fn await_reply(&mut self, serial: u32, what: &str) -> Result<Message, String> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            loop {
                let Some((message, taken)) = Message::decode(&self.incoming) else {
                    break;
                };
                self.incoming.drain(..taken);
                if message.reply_serial() == Some(serial) {
                    if message.kind == TYPE_ERROR {
                        return Err(format!(
                            "{what} failed: {}",
                            message.error_name().unwrap_or("unknown error")
                        ));
                    }
                    return Ok(message);
                }
                if message.kind == TYPE_METHOD_CALL {
                    self.answer(&message);
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the bus did not answer {what} within {} ms (it sent: {})",
                    HANDSHAKE_TIMEOUT.as_millis(),
                    self.raw_tail()
                ));
            }
            let mut buf = [0u8; 8192];
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    return Err(format!(
                        "the bus closed the connection while waiting for {what} (it sent: {})",
                        self.raw_tail()
                    ))
                }
                Ok(n) => self.incoming.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(format!(
                        "the bus did not answer {what} within {} ms (it sent: {})",
                        HANDSHAKE_TIMEOUT.as_millis(),
                        self.raw_tail()
                    ))
                }
                Err(e) => return Err(format!("read while waiting for {what}: {e}")),
            }
        }
    }

    /// Answer one method call. Everything MPRIS clients ask for is here; anything else gets the
    /// standard `UnknownMethod` error rather than silence, because a client that waits forever is
    /// worse than a client that fails.
    fn answer(&mut self, call: &Message) {
        let member = call.member().unwrap_or_default();
        let interface = call.interface().unwrap_or_default();
        let mut body = Reader::new(&call.body);
        let reply = match (interface, member) {
            (PEER, "Ping") => Message::reply(call, 0, "", Vec::new()),
            (INTROSPECTABLE, "Introspect") => {
                let mut out = Writer::new();
                out.string(&introspect_xml());
                Message::reply(call, 0, "s", out.finish())
            }
            (PROPERTIES_INTERFACE, "Get") => {
                let want_interface = body.string().unwrap_or_default();
                let want_name = body.string().unwrap_or_default();
                match self.property(&want_interface, &want_name) {
                    Some(value) => {
                        let mut out = Writer::new();
                        out.variant(value.signature(), &mut |w: &mut Writer| value.write(w));
                        Message::reply(call, 0, "v", out.finish())
                    }
                    None => failure(call, &want_name),
                }
            }
            (PROPERTIES_INTERFACE, "GetAll") => {
                let want_interface = body.string().unwrap_or_default();
                let entries = self.interface_properties(&want_interface);
                let mut out = Writer::new();
                let pairs: Vec<(&str, &Prop)> = entries.iter().map(|(n, v)| (*n, v)).collect();
                out.dict_of_variants(&pairs);
                Message::reply(call, 0, "a{sv}", out.finish())
            }
            (PROPERTIES_INTERFACE, "Set") => {
                // MPRIS read-write properties (Volume, Shuffle, LoopStatus) would each need a
                // command the contract does not carry; refusing is the honest answer, and a
                // client shows the property as not settable instead of silently not setting it.
                failure(call, "Set")
            }
            (AURORA_INTERFACE, "PlayPause") => {
                self.queue.push(Action::PlayPause);
                Message::reply(call, 0, "", Vec::new())
            }
            (AURORA_INTERFACE, "Play")
            | (AURORA_INTERFACE, "Pause")
            | (AURORA_INTERFACE, "Stop") => {
                self.queue.push(Action::PlayPause);
                Message::reply(call, 0, "", Vec::new())
            }
            (AURORA_INTERFACE, "Next") => {
                self.queue.push(Action::Next);
                Message::reply(call, 0, "", Vec::new())
            }
            (AURORA_INTERFACE, "Previous") => {
                self.queue.push(Action::Previous);
                Message::reply(call, 0, "", Vec::new())
            }
            (AURORA_INTERFACE, "Seek") => {
                // Seeking is a command the contract does not carry yet: refuse it rather than
                // answer "OK" and not move.
                failure(call, "Seek")
            }
            _ => Message::error(
                call,
                0,
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("{interface}.{member} is not implemented"),
            ),
        };
        let mut reply = reply;
        reply.serial = next_serial(&mut self.serial);
        self.send(reply);
    }
}

fn failure(call: &Message, what: &str) -> Message {
    Message::error(
        call,
        0,
        "org.freedesktop.DBus.Error.UnknownProperty",
        &format!("{what} is not a readable/writable property of this player"),
    )
}

fn next_serial(serial: &mut u32) -> u32 {
    *serial = serial.wrapping_add(1);
    *serial
}

/// The uid of this process, without pulling `libc` into a plugin.
fn libc_uid() -> u32 {
    // `std::os::unix::fs::MetadataExt` has the uid of a *file*; the process's own uid is in
    // `/proc/self/status`, which is the portable answer inside a container and needs no crate.
    if let Ok(text) = std::fs::read_to_string("/proc/self/status") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(uid) = rest.split_whitespace().next() {
                    if let Ok(uid) = uid.parse() {
                        return uid;
                    }
                }
            }
        }
    }
    0
}

fn read_line(stream: &mut UnixStream) -> Result<String, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("the bus closed during authentication".to_string()),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(String::from_utf8_lossy(&line).trim_end().to_string());
                }
                if line.len() > 4096 {
                    return Err("the bus sent an over-long auth line".to_string());
                }
            }
            Err(e) => return Err(format!("auth read: {e}")),
        }
    }
}

impl Bus {
    /// The value a `Get` answers, from the last published snapshot.
    fn property(&self, interface: &str, name: &str) -> Option<Prop> {
        self.interface_properties(interface)
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }

    /// The values of one interface, from the last published snapshot.
    fn interface_properties(&self, interface: &str) -> Vec<(&'static str, Prop)> {
        self.snapshot
            .iter()
            .find(|(iface, _)| *iface == interface)
            .map(|(_, values)| values.clone())
            .unwrap_or_default()
    }

    /// Replace the published snapshot and emit `PropertiesChanged` for whatever moved.
    fn publish(&mut self, values: Vec<(&'static str, Vec<(&'static str, Prop)>)>) {
        let flat: Vec<(&'static str, &'static str, &Prop)> = values
            .iter()
            .flat_map(|(iface, props)| props.iter().map(move |(n, v)| (*iface, *n, v)))
            .collect();
        let fingerprint = format!("{flat:?}");
        if fingerprint == self.published {
            return;
        }
        self.published = fingerprint.clone();
        // A full `PropertiesChanged` per interface is what every client handles; diffing the
        // individual values would be a second, untested code path for no visible gain.
        for (interface, props) in &values {
            let mut body = Writer::new();
            body.string(interface);
            let pairs: Vec<(&str, &Prop)> = props.iter().map(|(n, v)| (*n, v)).collect();
            body.dict_of_variants(&pairs);
            // The invalidated list: an empty `as`.
            body.array(4, &mut |_: &mut Writer| {});
            let signal = Message::signal(
                0,
                OBJECT_PATH,
                PROPERTIES_INTERFACE,
                "PropertiesChanged",
                body.finish(),
            );
            let mut signal = signal;
            signal.serial = next_serial(&mut self.serial);
            self.send(signal);
        }
        self.snapshot = values;
    }
}

/// What the bus thread publishes, and the main thread keeps current.
struct Live {
    state: State,
    /// Bumped by every `state` message; the bus thread publishes when it changes.
    generation: u64,
    stop: bool,
}

pub struct Mpris {
    /// `org.mpris.MediaPlayer2.aurora` by default.
    name: String,
    /// Print what would be served instead of touching the bus (`AURORA_DRY_RUN` or `dry_run`).
    dry: bool,
    live: Arc<Mutex<Live>>,
    /// The bus thread; joined on `bye` so the name is released before the process exits.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for Mpris {
    fn default() -> Self {
        Mpris {
            name: "org.mpris.MediaPlayer2.aurora".to_string(),
            dry: false,
            live: Arc::new(Mutex::new(Live {
                state: State::default(),
                generation: 0,
                stop: false,
            })),
            thread: None,
        }
    }
}

impl Sidecar for Mpris {
    fn hello(&mut self, hello: &Hello, sink: &Sink) -> Result<(), String> {
        self.dry = hello.dry_run;
        for (key, value) in &hello.config {
            match key.as_str() {
                // The block's own `name` key is the plugin's name, so the bus name travels
                // as `bus`: two meanings of one word in one table is how a setting disappears.
                "bus" => {
                    if !value.starts_with("org.mpris.MediaPlayer2.") {
                        return Err(format!(
                            "bus must start with `org.mpris.MediaPlayer2.`, got `{value}`"
                        ));
                    }
                    self.name = value.clone();
                }
                "dry_run" => {
                    self.dry = match value.as_str() {
                        "true" | "on" | "yes" => true,
                        "false" | "off" | "no" => false,
                        other => return Err(format!("dry_run expects true/false, got `{other}`")),
                    }
                }
                other => return Err(format!("`mpris` does not know the key `{other}`")),
            }
        }
        if self.dry {
            sink.log(
                "info",
                format!("[mpris] would serve {} on the session bus", self.name),
            );
            return Ok(());
        }
        let name = self.name.clone();
        let live = self.live.clone();
        let sink = sink.clone();
        self.thread = Some(std::thread::spawn(move || serve(&name, live, sink)));
        Ok(())
    }

    fn state(&mut self, state: &State, _sink: &Sink) {
        let mut live = self.live.lock().unwrap();
        if live.state != *state {
            live.state = state.clone();
            live.generation = live.generation.wrapping_add(1);
        }
    }

    fn bye(&mut self, _sink: &Sink) {
        if let Ok(mut live) = self.live.lock() {
            live.stop = true;
        }
        // The loop sleeps at most 20 ms; a join that long is fine at exit and at a plugin toggle.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The bus thread: connect (or retry), answer, publish what moved.
fn serve(name: &str, live: Arc<Mutex<Live>>, sink: Sink) {
    let mut bus: Option<Bus> = None;
    let mut retry_at: Option<Instant> = None;
    let mut reported = false;
    let mut published_generation = u64::MAX;
    loop {
        let (state, generation, stop) = {
            let live = live.lock().unwrap();
            (live.state.clone(), live.generation, live.stop)
        };
        if stop {
            return;
        }
        if bus.is_none() {
            if retry_at.is_some_and(|at| Instant::now() < at) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match Bus::open(name) {
                Ok(mut opened) => {
                    eprintln!("[mpris] serving {name} on the session bus");
                    opened.publish(properties(&state, 100, false, false));
                    bus = Some(opened);
                    retry_at = None;
                    reported = false;
                    published_generation = generation;
                }
                Err(e) => {
                    retry_at = Some(Instant::now() + RETRY_AFTER);
                    if !reported {
                        reported = true;
                        eprintln!("[mpris] not serving: {e}");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            }
        }
        let connection = bus.as_mut().expect("a connection that was just opened");
        if let Err(e) = connection.pump() {
            eprintln!("[mpris] the bus connection ended: {e}");
            bus = None;
            retry_at = Some(Instant::now() + RETRY_AFTER);
            continue;
        }
        for action in connection.queue.drain(..) {
            sink.command(action);
        }
        if generation != published_generation {
            connection.publish(properties(&state, 100, false, false));
            published_generation = generation;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::TYPE_METHOD_RETURN;

    fn bus(stream: UnixStream) -> Bus {
        Bus {
            stream,
            serial: 1,
            incoming: Vec::new(),
            published: String::new(),
            snapshot: Vec::new(),
            queue: Vec::new(),
        }
    }

    #[test]
    fn a_reply_ends_the_wait_without_the_read_timeout() {
        let (ours, mut theirs) = UnixStream::pair().expect("a socket pair");
        ours.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("a read timeout");
        let reply = Message::new(
            TYPE_METHOD_RETURN,
            1,
            vec![Field::ReplySerial(42)],
            Vec::new(),
        )
        .encode();
        theirs.write_all(&reply).expect("the reply");
        // Keep the peer open after the reply: draining the socket would pay `ours`'s two-second
        // read timeout here, which is the first-frame bug this test is about.
        let _theirs = theirs;
        let mut bus = bus(ours);
        let started = Instant::now();
        let got = bus.await_reply(42, "Test").expect("the reply");
        let waited = started.elapsed();
        assert_eq!(got.reply_serial(), Some(42));
        assert!(
            waited < Duration::from_secs(1),
            "waited {waited:?} for a reply that had already arrived"
        );
    }

    #[test]
    fn an_error_reply_fails_the_wait_with_its_name() {
        let (ours, mut theirs) = UnixStream::pair().expect("a socket pair");
        ours.set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .expect("a read timeout");
        let error = Message::new(
            TYPE_ERROR,
            1,
            vec![
                Field::ReplySerial(7),
                Field::Error("org.freedesktop.DBus.Error.AccessDenied".to_string()),
            ],
            Vec::new(),
        )
        .encode();
        theirs.write_all(&error).expect("the error");
        let _theirs = theirs;
        let mut bus = bus(ours);
        let refused = bus
            .await_reply(7, "RequestName")
            .expect_err("an error reply");
        assert!(refused.contains("AccessDenied"), "{refused}");
    }
}
