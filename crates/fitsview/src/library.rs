//! A library of calibration frames, and which of them suit a night.
//!
//! Darks and flats have to match the frames they correct: a dark of the same
//! exposure, gain, offset and temperature, a flat through the same filter.
//! Picking them by hand is where calibration goes wrong — a flat forgotten
//! leaves every stack of that filter vignetted, and nothing says so — so the
//! library is read once and the match made from what every frame's header
//! already records.
//!
//! Everything here works from headers alone, a few kilobytes a frame, so
//! reading a library of thousands of frames takes about a second. Which frames
//! make a master, and how, is left to the caller.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fits_core::{is_fits_path, read_fits_header, FitsHeader};

/// What a frame is for, from its `IMAGETYP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A picture of the sky.
    Light,
    /// The shutter closed, as long as a light: the sensor's own signal.
    Dark,
    /// An evenly lit field: vignetting and dust.
    Flat,
    /// A dark as long as a flat, subtracted from the flats.
    FlatDark,
    /// The shortest exposure the camera takes: its offset alone.
    Bias,
}

impl Kind {
    /// Reads the many ways capture programs write it: `LIGHT`, `Light Frame`,
    /// `Dark Frame`, `Flat Field`, `DARKFLAT`, `Flat Dark`, `Bias Frame` …
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        if text.contains("bias") || text.contains("offset") {
            Some(Self::Bias)
        } else if text.contains("dark") && text.contains("flat") {
            Some(Self::FlatDark)
        } else if text.contains("dark") {
            Some(Self::Dark)
        } else if text.contains("flat") {
            Some(Self::Flat)
        } else if text.contains("light") || text.contains("object") {
            Some(Self::Light)
        } else {
            None
        }
    }

    /// What to call it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Flat => "flat",
            Self::FlatDark => "flat dark",
            Self::Bias => "bias",
        }
    }
}

/// What a frame's header says about how it was taken, as far as matching
/// calibration goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Taken {
    /// Light, dark, flat and so on.
    pub kind: Kind,
    /// Exposure in seconds.
    pub exposure: Option<f64>,
    /// Sensor gain, in the camera's own units.
    pub gain: Option<f64>,
    /// Sensor offset, in the camera's own units.
    pub offset: Option<f64>,
    /// The temperature the cooler was asked for, in °C.
    pub set_temperature: Option<f64>,
    /// The temperature the sensor was at, in °C.
    pub temperature: Option<f64>,
    /// Pixels combined across and down.
    pub binning: (u32, u32),
    /// The filter, as the filter wheel names it.
    pub filter: Option<String>,
    /// The camera.
    pub camera: Option<String>,
    /// Width and height in pixels.
    pub size: (usize, usize),
    /// When it was taken, as days since 1970, from `DATE-OBS`.
    pub day: Option<i64>,
}

impl Taken {
    /// Reads it from a header, or `None` for a frame that does not say what
    /// it is or how big.
    #[must_use]
    pub fn from_header(header: &FitsHeader) -> Option<Self> {
        Self::read(header, Kind::parse(header.get("IMAGETYP")?)?)
    }

    /// Reads a light's header. Capture programs do not all record that a
    /// light is one, and a frame being stacked is a light whatever it says.
    #[must_use]
    pub fn of_light(header: &FitsHeader) -> Option<Self> {
        Self::read(header, Kind::Light)
    }

    fn read(header: &FitsHeader, kind: Kind) -> Option<Self> {
        let size = |key: &str| header.get_i64(key).and_then(|v| usize::try_from(v).ok());
        let text = |key: &str| {
            header
                .get(key)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let binning = |key: &str| {
            header
                .get_i64(key)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(1)
        };
        Some(Self {
            kind,
            exposure: header
                .get_f64("EXPTIME")
                .or_else(|| header.get_f64("EXPOSURE")),
            gain: header.get_f64("GAIN"),
            offset: header.get_f64("OFFSET"),
            set_temperature: header.get_f64("SET-TEMP"),
            temperature: header.get_f64("CCD-TEMP"),
            binning: (binning("XBINNING"), binning("YBINNING")),
            filter: text("FILTER"),
            camera: text("INSTRUME"),
            size: (size("NAXIS1")?, size("NAXIS2")?),
            day: header.get("DATE-OBS").and_then(day_of),
        })
    }

    /// The temperature to compare: what the cooler held, or failing that what
    /// the sensor read.
    #[must_use]
    pub fn cooled_to(&self) -> Option<f64> {
        self.set_temperature.or(self.temperature)
    }
}

/// Days since 1970 of a `DATE-OBS` value such as `2025-01-16T02:13:04.5`.
fn day_of(text: &str) -> Option<i64> {
    let date = text.trim().get(..10)?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days-from-civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    Some(era * 146_097 + of_era_days - 719_468)
}

/// Frames taken the same way, in the same folder: what a master is made from.
#[derive(Debug, Clone, PartialEq)]
pub struct Set {
    /// How they were taken; the temperature is their average.
    pub taken: Taken,
    /// The files, in name order.
    pub frames: Vec<PathBuf>,
}

impl Set {
    /// Where they are.
    #[must_use]
    pub fn folder(&self) -> Option<&Path> {
        self.frames.first().and_then(|p| p.parent())
    }

    /// A line saying what it is, such as `12 darks, 180 s, gain 100, −14 °C,
    /// 2024-12-22`.
    #[must_use]
    pub fn describe(&self) -> String {
        let taken = &self.taken;
        let plural = match (self.frames.len(), taken.kind) {
            (1, _) => "",
            (_, Kind::Bias) => "es",
            _ => "s",
        };
        let mut parts = vec![format!(
            "{} {}{plural}",
            self.frames.len(),
            taken.kind.name()
        )];
        if let Some(filter) = &taken.filter {
            if matches!(taken.kind, Kind::Flat) {
                parts.push(filter.clone());
            }
        }
        if let Some(exposure) = taken.exposure {
            parts.push(seconds(exposure));
        }
        if let Some(gain) = taken.gain {
            parts.push(format!("gain {gain}"));
        }
        // A flat's temperature does not matter, and saying it suggests it does.
        if let (Some(t), false) = (taken.cooled_to(), taken.kind == Kind::Flat) {
            parts.push(format!("{t:.0} °C"));
        }
        if let Some(day) = taken.day {
            parts.push(date_of(day));
        }
        parts.join(", ")
    }
}

/// An exposure for reading: `180 s`, `0.069 s`.
fn seconds(exposure: f64) -> String {
    if exposure >= 10.0 {
        format!("{exposure:.0} s")
    } else {
        format!("{exposure:.3} s").replace(".000 s", " s")
    }
}

/// A day since 1970 as `2024-12-22`.
fn date_of(days: i64) -> String {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * of_year + 2) / 153;
    let day = of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Every calibration frame found under a folder, gathered into sets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Library {
    /// The sets, in folder order.
    pub sets: Vec<Set>,
}

impl Library {
    /// Reads every FITS header under `root`, keeping the darks, flats, flat
    /// darks and biases. Lights are passed over, and so is anything that does
    /// not say what it is, anything unreadable, and anything hidden.
    #[must_use]
    pub fn scan(root: &Path) -> Self {
        let mut found = Vec::new();
        walk(root, &mut found);
        Self::from_frames(found)
    }

    /// Gathers frames into sets: the same folder, kind, camera, size, binning,
    /// gain, offset, exposure and cooler setting, and for flats the same
    /// filter. A darks folder holding several exposures becomes several sets.
    #[must_use]
    pub fn from_frames(frames: Vec<(PathBuf, Taken)>) -> Self {
        let mut sets: BTreeMap<String, Set> = BTreeMap::new();
        for (path, taken) in frames {
            if taken.kind == Kind::Light {
                continue;
            }
            let filter = if taken.kind == Kind::Flat {
                taken
                    .filter
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
            } else {
                String::new()
            };
            let key = format!(
                "{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
                path.parent()
                    .map(Path::display)
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                taken.kind,
                taken.camera,
                taken.size,
                taken.binning,
                taken.gain,
                taken.offset,
                taken.exposure.map(|e| (e * 1000.0).round()),
                taken.set_temperature,
                filter,
            );
            let set = sets.entry(key).or_insert_with(|| Set {
                taken: taken.clone(),
                frames: Vec::new(),
            });
            set.frames.push(path);
            set.taken.day = match (set.taken.day, taken.day) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        let mut sets: Vec<Set> = sets.into_values().collect();
        for set in &mut sets {
            set.frames.sort();
        }
        Self { sets }
    }

    /// The best dark for `lights`, or why there is none.
    ///
    /// The same camera, size, binning, gain, offset and exposure, and a cooler
    /// setting within [`TEMPERATURE_TOLERANCE`]: a sensor's own signal roughly
    /// doubles every six degrees, and a dark from a warmer night over-corrects.
    /// Of those, the closest in temperature, then the most frames, then the
    /// nearest in date. Frames before date, because a cooled sensor's darks
    /// hardly change from one night to the next, and eight times the frames
    /// makes a master with a third of the noise: preferring a five-frame set
    /// taken a day nearer over a forty-frame one is the wrong way round.
    ///
    /// # Errors
    ///
    /// Says why, in terms the user can act on: what was missing, and what
    /// there is instead.
    pub fn dark_for(&self, lights: &Taken) -> Result<&Set, String> {
        let alike: Vec<&Set> = self
            .of_kind(Kind::Dark)
            .filter(|s| same_sensor(&s.taken, lights))
            .collect();
        if alike.is_empty() {
            return Err(format!("no darks {}", sensor_words(lights)));
        }
        let long: Vec<&Set> = alike
            .iter()
            .copied()
            .filter(|s| same_exposure(s.taken.exposure, lights.exposure))
            .collect();
        if long.is_empty() {
            return Err(format!(
                "no {} dark {}; there are darks of {}",
                lights.exposure.map_or_else(|| "matching".into(), seconds),
                sensor_words(lights),
                listed(alike.iter().filter_map(|s| s.taken.exposure).map(seconds))
            ));
        }
        let warmth = |s: &Set| match (s.taken.cooled_to(), lights.cooled_to()) {
            (Some(a), Some(b)) => (a - b).abs(),
            _ => 0.0,
        };
        let cool: Vec<&Set> = long
            .iter()
            .copied()
            .filter(|s| warmth(s) <= TEMPERATURE_TOLERANCE)
            .collect();
        if cool.is_empty() {
            let nearest = long
                .iter()
                .copied()
                .min_by(|a, b| warmth(a).total_cmp(&warmth(b)))
                .expect("some");
            return Err(format!(
                "no {} dark near {}; the nearest was taken at {}",
                lights.exposure.map_or_else(|| "matching".into(), seconds),
                lights
                    .cooled_to()
                    .map_or_else(|| "its temperature".into(), |t| format!("{t:.0} °C")),
                nearest
                    .taken
                    .cooled_to()
                    .map_or_else(|| "an unknown temperature".into(), |t| format!("{t:.0} °C")),
            ));
        }
        Ok(best(cool, lights, warmth, Order::FramesFirst))
    }

    /// The best flat for `lights`, or why there is none.
    ///
    /// The same camera, size, binning and filter; gain, offset, exposure and
    /// temperature do not matter, since a flat is divided out after its own
    /// dark is taken off. Of those, the nearest in date — dust moves, and a
    /// flat from the same night is worth most — then the most frames.
    ///
    /// # Errors
    ///
    /// Says what was missing, and which filters there are flats for.
    pub fn flat_for(&self, lights: &Taken) -> Result<&Set, String> {
        let alike: Vec<&Set> = self
            .of_kind(Kind::Flat)
            .filter(|s| same_frame(&s.taken, lights))
            .collect();
        let through: Vec<&Set> = alike
            .iter()
            .copied()
            .filter(|s| same_filter(s.taken.filter.as_deref(), lights.filter.as_deref()))
            .collect();
        if through.is_empty() {
            let filter = lights.filter.as_deref().unwrap_or("no filter");
            return Err(if alike.is_empty() {
                format!("no flats {}", frame_words(lights))
            } else {
                format!(
                    "no flats for {filter}; there are flats for {}",
                    listed(
                        alike.iter().map(|s| s
                            .taken
                            .filter
                            .clone()
                            .unwrap_or_else(|| "no filter".into()))
                    )
                )
            });
        }
        Ok(best(through, lights, |_| 0.0, Order::DateFirst))
    }

    /// What to take off a flat set before it is combined: flat darks of its
    /// exposure, or darks of its exposure, or failing both a bias, which is
    /// nearly as good for the short exposures flats usually are.
    #[must_use]
    pub fn dark_for_flats(&self, flats: &Taken) -> Option<&Set> {
        for kind in [Kind::FlatDark, Kind::Dark] {
            let matching: Vec<&Set> = self
                .of_kind(kind)
                .filter(|s| same_sensor(&s.taken, flats))
                .filter(|s| same_exposure(s.taken.exposure, flats.exposure))
                .collect();
            if !matching.is_empty() {
                return Some(best(matching, flats, |_| 0.0, Order::FramesFirst));
            }
        }
        let biases: Vec<&Set> = self
            .of_kind(Kind::Bias)
            .filter(|s| same_sensor(&s.taken, flats))
            .collect();
        (!biases.is_empty()).then(|| best(biases, flats, |_| 0.0, Order::FramesFirst))
    }

    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Set> {
        self.sets.iter().filter(move |s| s.taken.kind == kind)
    }
}

/// How far a dark's cooler setting may be from the lights', in °C.
pub const TEMPERATURE_TOLERANCE: f64 = 2.0;

/// Which counts for more once a candidate matches: its date, or its frames.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// Flats: dust moves, so the nearest night is worth most.
    DateFirst,
    /// Darks and biases: they hardly change, so more frames are worth most.
    FramesFirst,
}

/// Of candidates that all match, the one to use: least `warmth` to the
/// degree, then date and frames in `order`, then first in name order so the
/// choice is the same every time.
fn best<'a>(
    sets: Vec<&'a Set>,
    lights: &Taken,
    warmth: impl Fn(&Set) -> f64,
    order: Order,
) -> &'a Set {
    let apart = |s: &Set| match (s.taken.day, lights.day) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => i64::MAX,
    };
    // To the degree: a tenth of a degree between two sets set to the same
    // temperature is the sensor's reading wandering, not a difference.
    #[allow(clippy::cast_possible_truncation)]
    let degrees = |s: &Set| warmth(s).round() as i64;
    sets.into_iter()
        .min_by(|a, b| {
            let frames = b.frames.len().cmp(&a.frames.len());
            let date = apart(a).cmp(&apart(b));
            degrees(a)
                .cmp(&degrees(b))
                .then(match order {
                    Order::DateFirst => date.then(frames),
                    Order::FramesFirst => frames.then(date),
                })
                .then(a.frames.cmp(&b.frames))
        })
        .expect("never asked of nothing")
}

/// Whether two frames share a camera, size and binning. A camera not named
/// is taken to be the same one; the size will usually tell otherwise.
fn same_frame(a: &Taken, b: &Taken) -> bool {
    a.size == b.size
        && a.binning == b.binning
        && match (&a.camera, &b.camera) {
            (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
            _ => true,
        }
}

/// Whether two frames were also read out the same way: gain and offset.
fn same_sensor(a: &Taken, b: &Taken) -> bool {
    let same = |x: Option<f64>, y: Option<f64>| match (x, y) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-6,
        _ => true,
    };
    same_frame(a, b) && same(a.gain, b.gain) && same(a.offset, b.offset)
}

/// Whether two exposures are the same, to a hundredth of a second or a
/// hundredth of the exposure.
fn same_exposure(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= (0.01 * a.max(b)).max(0.01),
        _ => false,
    }
}

/// Whether two filter names are the same filter, whatever the case.
fn same_filter(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.trim().eq_ignore_ascii_case(b.trim()),
        (None, None) => true,
        _ => false,
    }
}

/// "at gain 100, offset 0, 1×1, 9576×6388" and so on, for saying what was
/// looked for.
fn sensor_words(taken: &Taken) -> String {
    let mut words = Vec::new();
    if let Some(gain) = taken.gain {
        words.push(format!("gain {gain}"));
    }
    if let Some(offset) = taken.offset {
        words.push(format!("offset {offset}"));
    }
    words.push(frame_words(taken).trim_start_matches("at ").to_string());
    format!("at {}", words.join(", "))
}

fn frame_words(taken: &Taken) -> String {
    format!(
        "at {}×{} binning, {}×{} pixels",
        taken.binning.0, taken.binning.1, taken.size.0, taken.size.1
    )
}

/// A list without repeats, in order: "1 s, 3 s and 300 s".
fn listed(items: impl Iterator<Item = String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    match seen.len() {
        0 => "none".into(),
        1 => seen.remove(0),
        n => format!("{} and {}", seen[..n - 1].join(", "), seen[n - 1]),
    }
}

/// Collects every calibration frame's header under `dir`.
fn walk(dir: &Path, found: &mut Vec<(PathBuf, Taken)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&path, found);
        } else if kind.is_file() && is_fits_path(&path) {
            if let Some(taken) = read_fits_header(&path)
                .ok()
                .as_ref()
                .and_then(Taken::from_header)
            {
                found.push((path, taken));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};

    /// A frame of the Barnard's Loop night's camera, as `kind`, changed by
    /// `change`.
    fn frame(kind: Kind, change: impl FnOnce(&mut Taken)) -> Taken {
        let mut taken = Taken {
            kind,
            exposure: Some(180.0),
            gain: Some(100.0),
            offset: Some(0.0),
            set_temperature: Some(-14.0),
            temperature: Some(-13.9),
            binning: (1, 1),
            filter: Some(if kind == Kind::Dark { "Darks" } else { "L-Pro" }.into()),
            camera: Some("ZWO ASI6200MC Pro".into()),
            size: (9576, 6388),
            day: day_of("2025-01-16"),
        };
        change(&mut taken);
        taken
    }

    /// A library of one set for each frame given, in folders named for them.
    fn library(sets: &[(&str, Taken)]) -> Library {
        Library::from_frames(
            sets.iter()
                .map(|(name, taken)| {
                    (
                        PathBuf::from(format!("/astro/{name}/f.fits")),
                        taken.clone(),
                    )
                })
                .collect(),
        )
    }

    fn folder_of(set: &Set) -> String {
        set.folder().unwrap().display().to_string()
    }

    #[test]
    fn frame_types_are_read_however_they_are_written() {
        for (text, kind) in [
            ("LIGHT", Kind::Light),
            ("Light Frame", Kind::Light),
            ("DARK", Kind::Dark),
            ("Dark Frame", Kind::Dark),
            ("FLAT", Kind::Flat),
            ("Flat Field", Kind::Flat),
            ("DARKFLAT", Kind::FlatDark),
            ("Flat Dark", Kind::FlatDark),
            ("BIAS", Kind::Bias),
            ("Bias Frame", Kind::Bias),
        ] {
            assert_eq!(Kind::parse(text), Some(kind), "{text}");
        }
        assert_eq!(Kind::parse("Tricolour"), None);
    }

    #[test]
    fn dates_are_counted_in_days() {
        assert_eq!(day_of("1970-01-01T00:00:00"), Some(0));
        assert_eq!(day_of("2000-03-01"), Some(11_017));
        for date in ["2024-02-29", "2024-12-22", "2025-01-16", "1999-12-31"] {
            assert_eq!(date_of(day_of(date).unwrap()), date);
        }
        assert_eq!(day_of("2025-13-01"), None);
        assert_eq!(day_of("yesterday"), None);
    }

    #[test]
    fn a_header_says_how_a_frame_was_taken() {
        let header = FitsHeader {
            cards: [
                ("IMAGETYP", "DARK"),
                ("EXPTIME", "180.0"),
                ("GAIN", "100"),
                ("OFFSET", "0"),
                ("SET-TEMP", "-14.0"),
                ("CCD-TEMP", "-13.8"),
                ("XBINNING", "1"),
                ("YBINNING", "1"),
                ("FILTER", "Darks"),
                ("INSTRUME", "ZWO ASI6200MC Pro"),
                ("NAXIS1", "9576"),
                ("NAXIS2", "6388"),
                ("DATE-OBS", "2024-12-22T05:21:07.123"),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        };
        let taken = Taken::from_header(&header).unwrap();
        assert_eq!(taken.kind, Kind::Dark);
        assert_eq!(taken.exposure, Some(180.0));
        assert_eq!(taken.cooled_to(), Some(-14.0));
        assert_eq!(taken.size, (9576, 6388));
        assert_eq!(taken.day.map(date_of).as_deref(), Some("2024-12-22"));
    }

    #[test]
    fn the_dark_of_the_same_exposure_and_temperature_is_chosen() {
        let lights = frame(Kind::Light, |_| {});
        let library = library(&[
            ("short", frame(Kind::Dark, |t| t.exposure = Some(10.0))),
            (
                "warm",
                frame(Kind::Dark, |t| t.set_temperature = Some(-4.0)),
            ),
            ("other-gain", frame(Kind::Dark, |t| t.gain = Some(200.0))),
            (
                "binned",
                frame(Kind::Dark, |t| {
                    t.binning = (2, 2);
                    t.size = (4788, 3194);
                }),
            ),
            ("right", frame(Kind::Dark, |t| t.day = day_of("2024-12-22"))),
        ]);
        assert_eq!(
            folder_of(library.dark_for(&lights).unwrap()),
            "/astro/right"
        );
    }

    #[test]
    fn of_two_good_darks_the_closer_in_temperature_wins_then_the_closer_in_date() {
        let lights = frame(Kind::Light, |_| {});
        let library = library(&[
            ("older", frame(Kind::Dark, |t| t.day = day_of("2024-06-01"))),
            ("newer", frame(Kind::Dark, |t| t.day = day_of("2024-12-22"))),
            (
                "a-degree-off",
                frame(Kind::Dark, |t| {
                    t.set_temperature = Some(-15.0);
                    t.day = day_of("2025-01-16");
                }),
            ),
        ]);
        assert_eq!(
            folder_of(library.dark_for(&lights).unwrap()),
            "/astro/newer"
        );
    }

    #[test]
    fn more_dark_frames_beat_a_date_a_day_nearer() {
        // The Sh2 220 night: its own five darks from the next morning, and the
        // library's forty from a day later. The forty make the quieter master.
        let lights = frame(Kind::Light, |t| {
            t.exposure = Some(300.0);
            t.day = day_of("2024-12-20");
        });
        let dark = |days: &str| {
            frame(Kind::Dark, |t| {
                t.exposure = Some(300.0);
                t.day = day_of(days);
            })
        };
        let few: Vec<(PathBuf, Taken)> = (0..5)
            .map(|i| {
                (
                    PathBuf::from(format!("/astro/own/d{i}.fits")),
                    dark("2024-12-21"),
                )
            })
            .collect();
        let many: Vec<(PathBuf, Taken)> = (0..40)
            .map(|i| {
                (
                    PathBuf::from(format!("/astro/library/d{i:02}.fits")),
                    dark("2024-12-22"),
                )
            })
            .collect();
        let library = Library::from_frames(few.into_iter().chain(many).collect());
        let chosen = library.dark_for(&lights).unwrap();
        assert_eq!(chosen.frames.len(), 40, "{}", chosen.describe());
    }

    #[test]
    fn a_set_says_what_it_is_in_plain_words() {
        let set = |kind: Kind, n: usize| Set {
            taken: frame(kind, |t| t.exposure = Some(2.197)),
            frames: (0..n)
                .map(|i| PathBuf::from(format!("/f{i}.fits")))
                .collect(),
        };
        assert!(set(Kind::Bias, 128).describe().starts_with("128 biases"));
        assert!(set(Kind::Bias, 1).describe().starts_with("1 bias,"));
        let flats = set(Kind::Flat, 4).describe();
        assert_eq!(flats, "4 flats, L-Pro, 2.197 s, gain 100, 2025-01-16");
        assert!(set(Kind::Dark, 40).describe().contains("-14 °C"));
    }

    #[test]
    fn a_missing_dark_is_explained() {
        let lights = frame(Kind::Light, |_| {});
        let none = library(&[]);
        assert!(none
            .dark_for(&lights)
            .unwrap_err()
            .starts_with("no darks at gain 100"));

        let other_lengths = library(&[
            ("a", frame(Kind::Dark, |t| t.exposure = Some(300.0))),
            ("b", frame(Kind::Dark, |t| t.exposure = Some(10.0))),
        ]);
        let why = other_lengths.dark_for(&lights).unwrap_err();
        assert!(why.contains("no 180 s dark"), "{why}");
        assert!(why.contains("300 s") && why.contains("10 s"), "{why}");

        let warm = library(&[("a", frame(Kind::Dark, |t| t.set_temperature = Some(-4.0)))]);
        let why = warm.dark_for(&lights).unwrap_err();
        assert!(
            why.contains("near -14 °C") && why.contains("-4 °C"),
            "{why}"
        );
    }

    #[test]
    fn the_flat_through_the_same_filter_nearest_in_date_is_chosen() {
        let lights = frame(Kind::Light, |_| {});
        let library = library(&[
            ("l", frame(Kind::Flat, |t| t.filter = Some("L".into()))),
            ("old", frame(Kind::Flat, |t| t.day = day_of("2024-10-05"))),
            (
                "recent",
                frame(Kind::Flat, |t| {
                    t.filter = Some("l-pro".into());
                    t.day = day_of("2025-01-10");
                    // Flats are taken warm and short; neither matters.
                    t.set_temperature = Some(30.0);
                    t.exposure = Some(2.2);
                }),
            ),
            (
                "binned",
                frame(Kind::Flat, |t| {
                    t.binning = (2, 2);
                    t.size = (4788, 3194);
                    t.day = day_of("2025-01-16");
                }),
            ),
        ]);
        assert_eq!(
            folder_of(library.flat_for(&lights).unwrap()),
            "/astro/recent"
        );
    }

    #[test]
    fn a_missing_flat_says_which_filters_there_are() {
        let lights = frame(Kind::Light, |t| t.filter = Some("Ha".into()));
        let library = library(&[
            ("l", frame(Kind::Flat, |t| t.filter = Some("L".into()))),
            ("pro", frame(Kind::Flat, |_| {})),
        ]);
        let why = library.flat_for(&lights).unwrap_err();
        assert!(why.starts_with("no flats for Ha"), "{why}");
        assert!(why.contains('L') && why.contains("L-Pro"), "{why}");
        assert!(library
            .flat_for(&frame(Kind::Light, |t| t.size = (100, 100)))
            .unwrap_err()
            .starts_with("no flats at"));
    }

    #[test]
    fn a_flat_set_s_dark_is_a_flat_dark_then_a_dark_then_a_bias() {
        let flats = frame(Kind::Flat, |t| t.exposure = Some(2.2));
        let bias = ("bias", frame(Kind::Bias, |t| t.exposure = Some(0.0001)));
        let dark = ("dark", frame(Kind::Dark, |t| t.exposure = Some(2.2)));
        let flat_dark = (
            "flat-dark",
            frame(Kind::FlatDark, |t| t.exposure = Some(2.2)),
        );
        let wrong = ("wrong", frame(Kind::FlatDark, |t| t.exposure = Some(5.0)));

        let all = library(&[bias.clone(), dark.clone(), flat_dark, wrong.clone()]);
        assert_eq!(
            folder_of(all.dark_for_flats(&flats).unwrap()),
            "/astro/flat-dark"
        );
        let no_flat_dark = library(&[bias.clone(), dark, wrong.clone()]);
        assert_eq!(
            folder_of(no_flat_dark.dark_for_flats(&flats).unwrap()),
            "/astro/dark"
        );
        let only_bias = library(&[bias, wrong.clone()]);
        assert_eq!(
            folder_of(only_bias.dark_for_flats(&flats).unwrap()),
            "/astro/bias"
        );
        assert!(library(&[wrong]).dark_for_flats(&flats).is_none());
    }

    #[test]
    fn frames_are_gathered_into_sets_by_how_they_were_taken() {
        let frames = vec![
            (
                PathBuf::from("/d/a1.fits"),
                frame(Kind::Dark, |t| t.exposure = Some(1.0)),
            ),
            (
                PathBuf::from("/d/a2.fits"),
                frame(Kind::Dark, |t| t.exposure = Some(1.0)),
            ),
            (
                PathBuf::from("/d/b1.fits"),
                frame(Kind::Dark, |t| t.exposure = Some(3.0)),
            ),
            (PathBuf::from("/f/p.fits"), frame(Kind::Flat, |_| {})),
            (
                PathBuf::from("/f/u.fits"),
                frame(Kind::Flat, |t| t.filter = Some("L-Ultimate".into())),
            ),
            (PathBuf::from("/l/x.fits"), frame(Kind::Light, |_| {})),
        ];
        let library = Library::from_frames(frames);
        let mut sizes: Vec<(Kind, usize)> = library
            .sets
            .iter()
            .map(|s| (s.taken.kind, s.frames.len()))
            .collect();
        sizes.sort();
        assert_eq!(
            sizes,
            [
                (Kind::Dark, 1),
                (Kind::Dark, 2),
                (Kind::Flat, 1),
                (Kind::Flat, 1)
            ]
        );
    }

    #[test]
    fn a_library_is_read_from_the_frames_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let write = |folder: &str, name: &str, cards: &[(&str, &str)]| {
            let path = dir.path().join(folder);
            std::fs::create_dir_all(&path).unwrap();
            let mut spec = SyntheticSpec::new(4, 3, 16);
            for (key, value) in cards {
                spec = spec.with_card(key, value);
            }
            write_synthetic(&path, name, &spec, &[1.0; 12]).unwrap();
        };
        let dark = [
            ("IMAGETYP", "'DARK'"),
            ("EXPTIME", "180.0"),
            ("GAIN", "100"),
        ];
        write("DARK/2024-12-21/RAW_180", "d1.fits", &dark);
        write("DARK/2024-12-21/RAW_180", "d2.fits", &dark);
        write(
            "FLAT/2024-12-19",
            "f1.fits",
            &[("IMAGETYP", "'FLAT'"), ("FILTER", "'L'")],
        );
        write("LIGHT/M42", "l1.fits", &[("IMAGETYP", "'LIGHT'")]);
        write(".cache", "d9.fits", &dark);
        write("MISC", "unknown.fits", &[]);

        let library = Library::scan(dir.path());
        assert_eq!(library.sets.len(), 2, "{:#?}", library.sets);
        let darks = library
            .sets
            .iter()
            .find(|s| s.taken.kind == Kind::Dark)
            .unwrap();
        assert_eq!(darks.frames.len(), 2, "the hidden folder is passed over");
        assert!(
            darks.describe().starts_with("2 darks, 180 s, gain 100"),
            "{}",
            darks.describe()
        );
    }
}
