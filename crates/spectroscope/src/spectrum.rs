//! The Spectroscope: RME's DigiCheck NG 2D analyser, as a program.
//!
//! The analysis and nothing else: the drawing and the wire live in `main.rs`, so a rule about
//! bands is tested without a process and a canvas is read without a spectrogram.
//!
//! # The reference, and what of it is here
//!
//! The instrument copied here is RME's **DigiCheck NG "Spectroscope"** — the 2D view, "new visual
//! representation for Sound Analysis in a two-dimensional graph view". The numbers are the ones its
//! own setup dialog prints, not guesses:
//!
//! | the reference's setting | value | here |
//! |---|---|---|
//! | FFT Size | 4096 | [`FFT`] — the cap; the core's 2048-sample window sets the real length |
//! | Reference Level | −6 dB | [`REFERENCE_DB`] |
//! | Display Range | 80 dB log | [`RANGE_DB`] |
//! | Frequency Range | 20 Hz – 20 kHz | [`LOW_HZ`] / [`HIGH_HZ`] |
//! | Colour | `blue-red-yellow-white` | the palette's own tokens, named in the canvas op |
//! | Show Grid | `hor. + 1 s` | one line per second of history |
//!
//! The frequency scale is printed on **both** sides, at the reference's own tick list and spellings
//! (`20k`, `10k` … `1k`, `900` … `50`), and a label that would collide with the one above it is
//! dropped — the reference staggers instead of overlapping, and in a column slot dropping is the
//! honest version of that.
//!
//! # What is deliberately not copied
//!
//! The window chrome (traffic lights, the settings dialog, the "processing active" title) belongs to
//! the host: a plugin slot is a block in a column, and a window inside one would be a second window
//! system. The switches the reference offers — reassignment, note cursor, stationary display — do
//! not change the picture this draws, so the display here is the one the reference ships.
//!
//! # The signal, and where the rate comes from
//!
//! The frame message carries the session's scope window (`Frame::samples`), the same history the
//! oscilloscope draws. The frame is Hann-windowed, transformed, and folded into log-spaced bands; a
//! band is as wide as the bins inside it, and a band with no bin takes the nearest one, so 20 kHz is
//! not a hole.
//!
//! The rate is **the daemon's own** — the snapshot's `rate`, out of MPD's `audio: 44100:24:2` line —
//! so the scale follows 44.1, 48 or 96 kHz without a setting anywhere. It is 0 before the daemon
//! answers; then, with nothing playing and nothing to analyse, the scale is drawn against
//! [`NOMINAL_RATE_HZ`] rather than against a wrong guess about the stream.

/// The transform's *cap*: the reference's FFT Size.
///
/// The transform is as long as the window the core hands over, rounded up to a power of two and
/// never longer than this. `Scope` holds four thousand and ninety-six samples — the same number —
/// so the picture is made from the reference's own 4096-point transform and its bins are
/// `rate / 4096` apart: 11.7 Hz at 48 kHz, which is what makes the bottom of the scale (where the
/// bands are narrower than a bin) a scale and not a smear.
pub const FFT: usize = 4096;

/// The rate used when the daemon has not said what it is playing. It is a *nominal* rate, not an
/// assumption about the music: with nothing playing there is nothing to analyse, and a scale that
/// has to put a number somewhere puts the most common one there.
pub const NOMINAL_RATE_HZ: f32 = 48_000.0;

/// Full scale: the reference's Reference Level. A signal at −6 dBFS is the top of the display.
pub const REFERENCE_DB: f32 = -6.0;

/// The window's height in decibels: the reference's Display Range.
pub const RANGE_DB: f32 = 80.0;

/// The frequency range, from the reference's setup dialog.
pub const LOW_HZ: f32 = 20.0;
pub const HIGH_HZ: f32 = 20_000.0;

/// Rows of history the picture holds, and how many of them pass a second. One row per frame at a
/// 60 Hz clock is the reference's 200 px/s at a 3 px row; the pair is what the grid's one-second
/// rule is derived from instead of being written twice.
pub const ROWS: usize = 90;
pub const ROWS_PER_SECOND: f32 = 60.0;

/// Bands across the picture: a column slot is ~300 logical px wide, so this is finer than the
/// display resolves and keeps the picture at 200 × 90 bytes.
pub const BANDS: usize = 200;

/// The label gutter, in logical units. The reference prints its scale on both sides.
pub const GUTTER: f32 = 22.0;

/// The reference's tick list, high to low, in its own spellings.
pub const TICKS: [(f32, &str); 25] = [
    (20_000.0, "20k"),
    (10_000.0, "10k"),
    (9_000.0, "9k"),
    (8_000.0, "8k"),
    (7_000.0, "7k"),
    (6_000.0, "6k"),
    (5_000.0, "5k"),
    (4_000.0, "4k"),
    (3_000.0, "3k"),
    (2_000.0, "2k"),
    (1_000.0, "1k"),
    (900.0, "900"),
    (800.0, "800"),
    (700.0, "700"),
    (600.0, "600"),
    (500.0, "500"),
    (400.0, "400"),
    (300.0, "300"),
    (200.0, "200"),
    (100.0, "100"),
    (90.0, "90"),
    (80.0, "80"),
    (70.0, "70"),
    (60.0, "60"),
    (50.0, "50"),
];

pub struct Spectroscope {
    /// The picture's history, oldest row first: `ROWS` rows of `BANDS` levels.
    history: Vec<u8>,
    /// One frame's row, reused so a frame does not allocate.
    row: Vec<u8>,
    /// Transform scratch: the windowed samples, and the complex buffers.
    windowed: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Half the twiddles, `(cos, sin)` for `k·2π/N`, built once.
    twiddles: Vec<(f32, f32)>,
    /// The instant the next row is due, on the frame clock.
    next_row: f32,
    /// [`Spectroscope::history`] turned the way the display reads it: **time across, frequency
    /// down**. The history is one row per instant because that is the natural way to push it; the
    /// instrument's scale is on the sides, which is where a frequency axis lives, so the picture
    /// has to be turned a quarter before it is handed over. Kept as a buffer because this is a
    /// per-frame transpose of eighteen thousand bytes.
    picture: Vec<u8>,
}

impl Default for Spectroscope {
    fn default() -> Self {
        let mut twiddles = Vec::with_capacity(FFT / 2);
        for k in 0..FFT / 2 {
            let angle = -2.0 * std::f32::consts::PI * k as f32 / FFT as f32;
            twiddles.push((angle.cos(), angle.sin()));
        }
        Spectroscope {
            history: vec![0; ROWS * BANDS],
            row: vec![0; BANDS],
            windowed: vec![0.0; FFT],
            re: vec![0.0; FFT],
            im: vec![0.0; FFT],
            twiddles,
            next_row: 0.0,
            picture: vec![0; ROWS * BANDS],
        }
    }
}

impl Spectroscope {
    /// One row of levels from a window of samples, lowest frequency left.
    ///
    /// A level is the display's own 0..=255, with [`REFERENCE_DB`] at the top and
    /// `REFERENCE_DB − RANGE_DB` at the bottom. Silence is the floor, not an error.
    pub fn analyse(&mut self, samples: &[f32], rate: f32) -> &[u8] {
        self.row.fill(0);
        // The transform's length is the window rounded *up* to a power of two and capped at [`FFT`],
        // and it is decided here rather than inside `transform` because the bin width below has to be
        // the length that was actually transformed: 2048 samples transformed as 2048 have bins
        // `rate / 2048` apart, and calling them `rate / FFT` apart put every frequency at half its
        // value — a 1 kHz tone under the 500 Hz label, which is a scale that lies.
        let take = FFT.min(samples.len());
        let n = take.next_power_of_two().min(FFT);
        if n < 64 || !rate.is_finite() || rate <= 0.0 {
            return &self.row;
        }
        // The last `take` samples — the newest — laid at the start of the buffer; the rest of the
        // transform is zeros, which is what a history shorter than the reference's 4096 points is.
        let start = samples.len() - take;
        for (i, slot) in self.windowed.iter_mut().take(take).enumerate() {
            // Hann: a rectangular window's skirts would paint a floor twenty decibels too high
            // across the whole picture, which is the difference between a spectrogram and a smudge.
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (take - 1) as f32).cos();
            *slot = samples[start + i] * w;
        }
        self.windowed[take..].fill(0.0);
        self.transform(n);

        // The bin width is `rate / n` — the number of samples actually transformed — and not
        // `rate / FFT`: the core's scope window is 2048 samples against the reference's 4096-point
        // transform, so reading the bins as if there were 4096 of them showed every frequency at
        // half its value (a 1 kHz tone under the 500 Hz label), which is the one number a scale
        // must not get wrong. `n` is the transform's length here, and the twiddles are computed for
        // `FFT` — exact for the power-of-two `n` the window is rounded to below.
        let bin_hz = rate / n as f32;
        for band in 0..BANDS {
            let lo = band_hz(band, BANDS);
            let hi = band_hz(band + 1, BANDS);
            let first = ((lo / bin_hz).floor() as usize).max(1);
            let last = ((hi / bin_hz).ceil() as usize).max(first).min(n / 2 - 1);
            let first = first.min(last);
            let mut peak = 0.0f32;
            for bin in first..=last {
                let power = self.re[bin] * self.re[bin] + self.im[bin] * self.im[bin];
                peak = peak.max(power);
            }
            // `2·|X|/N` is the amplitude of a sine at that bin; the Hann window costs half of it, so
            // the reference is halved as well, and a half-scale sine lands on 0 dB of the display.
            let amplitude = 2.0 * peak.sqrt() / n as f32;
            self.row[band] = db_to_level(20.0 * amplitude.max(1e-9).log10());
        }
        &self.row
    }

    /// Push one row: the picture scrolls by one, oldest at the top, newest at the bottom.
    pub fn push(&mut self, row: &[u8]) {
        self.history.copy_within(BANDS.., 0);
        let tail = (ROWS - 1) * BANDS;
        self.history[tail..].copy_from_slice(&row[..BANDS]);
    }

    /// Push a row if the player's clock says one is due, and answer whether it did.
    ///
    /// The gate is the frame clock the player sends, not this process's timer: a pinned capture
    /// substitutes that clock, so the same script pushes the same rows at the same instants and
    /// the picture is reproducible.
    pub fn tick(&mut self, time: f32, samples: &[f32], rate: f32) -> bool {
        if time < self.next_row {
            return false;
        }
        let row = self.analyse(samples, rate).to_vec();
        self.push(&row);
        self.next_row = time + 1.0 / ROWS_PER_SECOND;
        true
    }

    /// The picture as the core paints it: row-major from the top, and **the axes this instrument
    /// labels**. Time runs along the bottom (oldest at the left, newest at the right) and frequency
    /// up the sides (20 kHz at the top, [`LOW_HZ`] at the bottom), because that is where the scale
    /// is printed and what the one-second grid lines measure.
    ///
    /// The history is the other way round — one row per instant, lowest frequency left — because
    /// that is how a frame of spectrum arrives. Handing it over as it was laid the picture a
    /// quarter turn off its own scale: a steady tone drew a *vertical* stripe where the 1 kHz label
    /// says a horizontal line belongs, which is a picture that cannot be read.
    pub fn picture(&mut self) -> Vec<u8> {
        for t in 0..ROWS {
            for band in 0..BANDS {
                let x = t;
                let y = BANDS - 1 - band;
                self.picture[y * ROWS + x] = self.history[t * BANDS + band];
            }
        }
        self.picture.clone()
    }

    /// In-place iterative radix-2 complex transform over the first `n` samples of `windowed`.
    fn transform(&mut self, n: usize) {
        let n = n.next_power_of_two().min(FFT);
        self.re[..n].copy_from_slice(&self.windowed[..n]);
        self.im[..n].fill(0.0);
        let bits = n.trailing_zeros();
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                self.re.swap(i, j);
                self.im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            // The table holds `exp(-2πi·k/FFT)`, so the twiddle this level needs —
            // `exp(-2πi·k/len)` — is the entry `k·FFT/len` along it. It used to be `k·n/len`,
            // which is the same number only when `n == FFT`: a 2048-sample window was then
            // rotated at half the rate the transform needed, so the spectrum came out a
            // scrambled half-scale picture instead of a spectrum.
            let step = FFT / len;
            let half = len / 2;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = self.twiddles[k * step];
                    let a = start + k;
                    let b = a + half;
                    let tr = self.re[b] * wr - self.im[b] * wi;
                    let ti = self.re[b] * wi + self.im[b] * wr;
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

/// Where a frequency sits on the picture: 0 at [`LOW_HZ`], 1 at [`HIGH_HZ`], logarithmically.
pub fn log_position(hz: f32) -> f32 {
    (hz.max(LOW_HZ) / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln()
}

/// The lower edge of band `index` of `bands`, in hertz, spaced logarithmically.
pub fn band_hz(index: usize, bands: usize) -> f32 {
    let t = index as f32 / bands as f32;
    LOW_HZ * (HIGH_HZ / LOW_HZ).powf(t)
}

/// A level in dBFS as the display's 0..=255.
pub fn db_to_level(db: f32) -> u8 {
    let t = (db - (REFERENCE_DB - RANGE_DB)) / RANGE_DB;
    (t.clamp(0.0, 1.0) * 255.0) as u8
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_scale_puts_the_reference_at_the_top() {
        assert_eq!(db_to_level(REFERENCE_DB), 255);
        assert_eq!(db_to_level(REFERENCE_DB + 20.0), 255);
        assert_eq!(db_to_level(REFERENCE_DB - RANGE_DB), 0);
        assert_eq!(db_to_level(REFERENCE_DB - RANGE_DB - 20.0), 0);
        let middle = db_to_level(REFERENCE_DB - RANGE_DB / 2.0) as i32;
        assert!((middle - 128).abs() <= 1, "middle {middle}");
    }

    #[test]
    fn the_bands_span_the_displayed_range() {
        assert!((band_hz(0, BANDS) - LOW_HZ).abs() < 1e-3);
        assert!((band_hz(BANDS, BANDS) - HIGH_HZ).abs() < 0.1);
        assert!(band_hz(1, BANDS) > band_hz(0, BANDS));
        assert!(band_hz(BANDS - 1, BANDS) < band_hz(BANDS, BANDS));
    }

    #[test]
    fn silence_is_the_floor_not_a_hole() {
        let mut scope = Spectroscope::default();
        let row = scope.analyse(&[0.0; 2048], 48_000.0).to_vec();
        assert_eq!(row.len(), BANDS);
        assert!(row.iter().all(|&level| level == 0), "{row:?}");
    }

    #[test]
    fn a_one_khz_tone_peaks_in_the_band_that_contains_one_khz() {
        let mut scope = Spectroscope::default();
        let rate = 48_000.0f32;
        let samples: Vec<f32> = (0..2048)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate).sin())
            .collect();
        let row = scope.analyse(&samples, rate).to_vec();
        let peak = row
            .iter()
            .enumerate()
            .max_by_key(|(_, level)| **level)
            .map(|(index, _)| index)
            .expect("a row");
        // The band edges are nominal: a band's bins are `floor(lo/bin)..=ceil(hi/bin)`, so the
        // boundary bins belong to both neighbours and the peak can sit one band over.
        let expected = (0..BANDS)
            .find(|&index| band_hz(index, BANDS) <= 1000.0 && 1000.0 <= band_hz(index + 1, BANDS))
            .expect("the 1 kHz band");
        assert!(
            peak.abs_diff(expected) <= 1,
            "peak {peak}, the 1 kHz band is {expected}"
        );
        assert!(row[peak] > 0, "the tone is above the floor");
        // Not at half the frequency: the transform's twiddles must follow the window's length,
        // not the reference's 4096-point cap.
        let half = (0..BANDS)
            .find(|&index| band_hz(index, BANDS) <= 500.0 && 500.0 <= band_hz(index + 1, BANDS))
            .expect("the 500 Hz band");
        assert!(
            row[peak] > row[half],
            "1 kHz level {} vs 500 Hz level {}",
            row[peak],
            row[half]
        );
    }

    #[test]
    fn the_picture_turns_time_across_and_frequency_down() {
        let mut scope = Spectroscope::default();
        let mut row = vec![0u8; BANDS];
        row[BANDS - 1] = 200; // the highest band
        scope.push(&row);
        let picture = scope.picture();
        assert_eq!(picture.len(), ROWS * BANDS);
        // The newest instant is the right column; the highest band is the top row.
        assert_eq!(picture[ROWS - 1], 200);
        scope.push(&vec![0u8; BANDS]);
        let picture = scope.picture();
        // A new instant scrolls the old one left.
        assert_eq!(picture[ROWS - 2], 200);
        assert_eq!(picture[ROWS - 1], 0);
    }

    #[test]
    fn a_short_or_unrateable_window_is_an_empty_row() {
        let mut scope = Spectroscope::default();
        assert!(scope.analyse(&[0.5; 10], 48_000.0).iter().all(|&v| v == 0));
        assert!(scope.analyse(&[0.5; 2048], 0.0).iter().all(|&v| v == 0));
        assert!(scope
            .analyse(&[0.5; 2048], f32::NAN)
            .iter()
            .all(|&v| v == 0));
    }

}
