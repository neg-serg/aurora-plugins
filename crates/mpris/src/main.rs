//! `mpris`: media keys, desktop widgets and KDE Connect, over the session bus.
//!
//! The old in-tree plugin moved into its own process, which is what makes its bus thread free:
//! the player's frame loop no longer has to keep a D-Bus socket awake, and a bus that goes away
//! costs this process a retry rather than the player a frame. The wire codec and the three
//! interfaces are the ones that were tested in the tree (`wire.rs`, `bus.rs`).

#[cfg(unix)]
mod bus;
#[cfg(unix)]
mod wire;

#[cfg(unix)]
fn main() {
    aurora_protocol::serve(bus::Mpris::default());
}

#[cfg(not(unix))]
fn main() {
    aurora_protocol::serve(NoBus);
}

/// MPRIS is D-Bus over a Unix socket; a host without one says so instead of linking dead code.
#[cfg(not(unix))]
struct NoBus;

#[cfg(not(unix))]
impl aurora_protocol::Sidecar for NoBus {
    fn hello(
        &mut self,
        _hello: &aurora_protocol::Hello,
        sink: &aurora_protocol::Sink,
    ) -> Result<(), String> {
        sink.log(
            "error",
            "MPRIS is D-Bus over a Unix socket; this build has no session bus to serve",
        );
        Err("no session bus".to_string())
    }
}
