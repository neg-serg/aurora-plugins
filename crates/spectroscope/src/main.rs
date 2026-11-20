//! `spectroscope`: the session's spectrum, as RME's DigiCheck waterfall in the now-playing column.
//!
//! The old in-tree plugin moved across the wire with its analysis intact (`spectrum.rs` is that
//! code, with the drawing taken out): the player sends the scope window every frame and the
//! program answers with a **canvas** — a level picture, the second grid and the frequency scale —
//! which the core paints into the plugin's slot, clipped and capped like any other plugin's ops.
//!
//! # What the move costs, and why it is worth it
//!
//! The picture is eighteen kilobytes and the window is eight, so a frame carries ~35 KB through
//! the pipe at sixty frames a second. That is the price of a process boundary, and it is paid by a
//! program that rebuilds on its own: a change to the display never rebuilds the player. The
//! alternative — a stateful canvas that ships a texture update instead of a picture — would move
//! the picture's bookkeeping into the player, and two processes that disagree about a picture are
//! worse than a wide pipe.
//!
//! # The clock is the frame's
//!
//! A row is pushed on the frame clock the player sends (`Frame::time`), not on this process's wall
//! clock: a pinned capture substitutes that clock, so the same script draws the same picture. The
//! drawing itself happens on the *next* frame, because a program cannot answer inside the frame
//! that asked — one frame of lag, which a measurement does not notice.
//!
//! # The colours are names
//!
//! The canvas carries theme tokens, never values: `rule`, `text_faint`, and the four-token ramp of
//! the waterfall. A skin switch recolours the picture without this program being told, and a
//! program cannot introduce an ink the theme audit has not measured.

use aurora_protocol::{canvas::Canvas, Frame, Hello, Output, Sidecar, Sink, State};

mod spectrum;

use spectrum::{
    log_position, Spectroscope, BANDS, GUTTER, NOMINAL_RATE_HZ, ROWS, ROWS_PER_SECOND, TICKS,
};

/// The four tokens the waterfall is coloured by: the inset (the quiet floor), the second accent,
/// the working accent and the palette's brightest ink. RME's `blue-red-yellow-white` is RME's
/// identity, not this app's — what a program may choose is *which named colours*, and these four
/// already mean "quiet" to "peak" everywhere else in the frame.
const RAMP: [&str; 4] = ["panel_inset", "accent2", "accent", "text"];

#[derive(Default)]
struct Waterfall {
    scope: Spectroscope,
    /// The rate the daemon last reported. Kept beside the snapshots because a frame may arrive
    /// with no rate of its own (the scope window before the daemon answers).
    rate: f32,
}

impl Sidecar for Waterfall {
    fn hello(&mut self, hello: &Hello, _sink: &Sink) -> Result<(), String> {
        match hello.config.first() {
            // Nothing to configure: the rate is the daemon's and the scale is the reference's.
            Some((key, _)) => Err(format!("`spectroscope` does not know the key `{key}`")),
            None => Ok(()),
        }
    }

    fn state(&mut self, state: &State, _sink: &Sink) {
        if state.rate >= 1.0 {
            self.rate = state.rate as f32;
        }
    }

    fn frame(&mut self, frame: &Frame, sink: &Sink) {
        // The rate comes from the daemon, so the scale follows 44.1, 48 or 96 kHz without a
        // setting: the stream knows, the user should not have to.
        let rate = if frame.rate >= 1.0 {
            frame.rate
        } else if self.rate >= 1.0 {
            self.rate
        } else {
            NOMINAL_RATE_HZ
        };
        // Rows arrive on the *clock*, not on this process's timer: a pinned capture substitutes
        // the clock, and the same script lands on the same picture.
        self.scope.tick(frame.time, &frame.samples, rate);

        let plot_w = (frame.width - 2.0 * GUTTER).max(1.0);
        let plot_h = frame.height;
        let mut canvas = Canvas::new();
        // The picture is transposed: `ROWS` cells across (time) by `BANDS` down (frequency).
        canvas.waterfall(
            GUTTER,
            0.0,
            plot_w,
            plot_h,
            ROWS as u32,
            BANDS as u32,
            self.scope.picture(),
            RAMP,
        );

        // The grid: one line per second of history, `hor. + 1s` in the reference's setup — the
        // horizontal axis is time, so the lines are vertical.
        let column_px = plot_w / ROWS as f32;
        let mut second = 1.0f32;
        while second * ROWS_PER_SECOND * column_px < plot_w - 2.0 {
            let x = plot_w - second * ROWS_PER_SECOND * column_px;
            canvas.rect(GUTTER + x, 0.0, 1.0, plot_h, 0.0, "rule");
            second += 1.0;
        }

        // The frequency scale, both sides, at the reference's ticks. A label closer than one line
        // to the one above it is dropped: the reference staggers its scale, and a slot this
        // shallow can only drop.
        let mut last_y = f32::NEG_INFINITY;
        for (hz, text) in TICKS {
            let y = (1.0 - log_position(hz)) * (plot_h - 8.0);
            if y - last_y < 7.5 {
                continue;
            }
            last_y = y;
            canvas.text(text, 0.0, y, 8.0, "text_faint");
            canvas.text(text, frame.width - GUTTER + 3.0, y, 8.0, "text_faint");
        }

        sink.send(Output::Draw(canvas));
    }
}

fn main() {
    aurora_protocol::serve(Waterfall::default())
}
