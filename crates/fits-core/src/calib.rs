//! Dark-frame calibration.
//!
//! A sensor adds signal of its own: thermal current that accumulates with
//! exposure time, and pixels that read high regardless. A dark frame is an
//! exposure with the shutter closed, matching the lights in exposure time and
//! temperature, so it captures that added signal and nothing else. Subtracting
//! it removes the thermal pattern and the hot pixels.
//!
//! Combining several darks matters as much as taking them. A single dark
//! carries its own read noise, which subtraction would inject into every light.
//! The median of several rejects both the noise and the cosmic ray hits that
//! land on individual frames.

use std::sync::Arc;

use rayon::prelude::*;

use crate::header::FitsHeader;
use crate::image::FitsImage;

/// Rows handled by one rayon task when combining frames.
const ROW_CHUNK: usize = 64;

/// Why calibration could not be carried out.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum CalibError {
    /// No frames were supplied to combine.
    #[error("no frames to combine")]
    NoFrames,

    /// The frames to combine are not all the same shape.
    #[error("frame {index} is {found}, but the first is {expected}")]
    Mismatched {
        /// Which frame disagreed.
        index: usize,
        /// The shape of the first frame.
        expected: String,
        /// The shape of the offending frame.
        found: String,
    },

    /// A calibration frame does not match the image it would be applied to.
    #[error("the calibration frame is {frame}, but the image is {image}")]
    WrongSize {
        /// Shape of the calibration frame.
        frame: String,
        /// Shape of the light frame.
        image: String,
    },

    /// The combined flat has no usable signal, so it cannot become a gain map.
    ///
    /// Seen when the frames were taken with the lens cap on, or when the flat
    /// dark subtracted everything away.
    #[error("the flat has an average level of {mean:.3}, so it carries no signal")]
    FlatHasNoSignal {
        /// The average level that was measured.
        mean: f64,
    },
}

/// A combined calibration frame, held in memory and applied to many lights.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterFrame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// 1 for mono, 3 for colour.
    pub channels: usize,
    /// Combined samples, in the same layout as [`FitsImage::data`].
    pub data: Vec<f32>,
    /// How many frames were combined.
    pub source_count: usize,
    /// Exposure time in seconds, when the frames recorded one.
    pub exptime: Option<f64>,
    /// Sensor temperature, when the frames recorded one.
    pub temperature: Option<f64>,
}

impl MasterFrame {
    /// A human-readable shape, for error messages.
    #[must_use]
    pub fn shape(&self) -> String {
        format!("{}x{}x{}", self.width, self.height, self.channels)
    }

    /// Whether this frame can be applied to `image`.
    #[must_use]
    pub fn matches(&self, image: &FitsImage) -> bool {
        self.width == image.width && self.height == image.height && self.channels == image.channels
    }

    /// Turns the master back into an image, so it can be displayed or written.
    #[must_use]
    pub fn to_image(&self) -> FitsImage {
        let (min, max) = crate::image::finite_min_max(&self.data);
        FitsImage {
            width: self.width,
            height: self.height,
            channels: self.channels,
            data: self.data.clone(),
            header: self.describe(),
            min,
            max,
        }
    }

    /// A header describing how this master was made.
    fn describe(&self) -> FitsHeader {
        let mut cards = vec![("NCOMBINE".to_string(), self.source_count.to_string())];
        if let Some(exptime) = self.exptime {
            cards.push(("EXPTIME".to_string(), crate::header::format_f64(exptime)));
        }
        if let Some(temperature) = self.temperature {
            cards.push((
                "CCD-TEMP".to_string(),
                crate::header::format_f64(temperature),
            ));
        }
        FitsHeader { cards }
    }

    /// Builds a master from an image already on disk, such as one saved
    /// earlier. A single frame is used exactly as it is.
    #[must_use]
    pub fn from_image(image: &FitsImage) -> Self {
        Self {
            width: image.width,
            height: image.height,
            channels: image.channels,
            data: image.data.clone(),
            source_count: image.header.get_i64("NCOMBINE").unwrap_or(1).max(1) as usize,
            exptime: image.header.get_f64("EXPTIME"),
            temperature: image
                .header
                .get_f64("CCD-TEMP")
                .or_else(|| image.header.get_f64("SET-TEMP")),
        }
    }
}

/// Gain below this is treated as no data.
///
/// A heavily vignetted corner, or a flat taken with the lens cap left on,
/// produces gain values near zero. Dividing by them turns read noise into
/// enormous bright pixels, so those pixels are marked undefined instead. The
/// value allows a corner at one hundredth of centre brightness, which is far
/// darker than any usable optical system.
pub const MIN_GAIN: f32 = 0.01;

/// A master flat, normalised into a gain map whose average pixel is 1.0.
///
/// A flat records how sensitivity varies across the frame: vignetting, dust
/// shadows, and pixel-to-pixel differences. Normalising turns it into a map
/// that can be divided out, removing the variation while leaving overall
/// brightness alone.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterFlat {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// 1 for mono, 3 for colour.
    pub channels: usize,
    /// Gain per pixel, centred on 1.0. Pixels with no usable signal are `NaN`.
    pub gain: Vec<f32>,
    /// How many frames were combined.
    pub source_count: usize,
    /// Pixels whose gain was too low to use, and are therefore undefined.
    pub unusable: usize,
}

impl MasterFlat {
    /// A human-readable shape, for error messages.
    #[must_use]
    pub fn shape(&self) -> String {
        format!("{}x{}x{}", self.width, self.height, self.channels)
    }

    /// Whether this flat can be applied to `image`.
    #[must_use]
    pub fn matches(&self, image: &FitsImage) -> bool {
        self.width == image.width && self.height == image.height && self.channels == image.channels
    }

    /// The fraction of pixels that carry no usable signal.
    #[must_use]
    pub fn unusable_fraction(&self) -> f64 {
        if self.gain.is_empty() {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.unusable as f64 / self.gain.len() as f64
        }
    }

    /// Turns the gain map back into an image, so it can be displayed or saved.
    #[must_use]
    pub fn to_image(&self) -> FitsImage {
        let (min, max) = crate::image::finite_min_max(&self.gain);
        FitsImage {
            width: self.width,
            height: self.height,
            channels: self.channels,
            data: self.gain.clone(),
            header: FitsHeader {
                cards: vec![
                    ("NCOMBINE".to_string(), self.source_count.to_string()),
                    ("FITSVFLT".to_string(), "T".to_string()),
                ],
            },
            min,
            max,
        }
    }

    /// Rebuilds a gain map from an image saved earlier.
    ///
    /// A file already normalised by this crate is used as it is. Anything else
    /// is normalised on load, so that any single frame can serve as a flat.
    #[must_use]
    pub fn from_image(image: &FitsImage) -> Self {
        let already_normalised = image.header.get_bool("FITSVFLT").unwrap_or(false);
        let source_count = image.header.get_i64("NCOMBINE").unwrap_or(1).max(1) as usize;

        let gain = if already_normalised {
            image.data.clone()
        } else {
            match normalise(&image.data) {
                Ok(gain) => gain,
                // A file with no signal cannot be a gain map; a flat one at
                // least leaves the image untouched rather than destroying it.
                Err(_) => vec![1.0; image.data.len()],
            }
        };
        let unusable = gain.iter().filter(|v| !v.is_finite()).count();

        Self {
            width: image.width,
            height: image.height,
            channels: image.channels,
            gain,
            source_count,
            unusable,
        }
    }
}

/// Divides samples by their own average, producing a gain map centred on 1.0.
///
/// Colour images are normalised by a **single** average across all three
/// planes, not one per plane. Normalising each plane separately would divide
/// out the camera's colour response along with the vignetting and render the
/// image grey, which is the same mistake the stretch had to be corrected for.
///
/// # Errors
///
/// Returns [`CalibError::FlatHasNoSignal`] if the average is not positive.
fn normalise(data: &[f32]) -> Result<Vec<f32>, CalibError> {
    let (sum, count) = data
        .par_iter()
        .filter(|v| v.is_finite())
        .fold(|| (0.0f64, 0usize), |(s, n), v| (s + f64::from(*v), n + 1))
        .reduce(|| (0.0, 0), |a, b| (a.0 + b.0, a.1 + b.1));

    if count == 0 {
        return Err(CalibError::FlatHasNoSignal { mean: 0.0 });
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / count as f64;
    if !mean.is_finite() || mean <= 0.0 {
        return Err(CalibError::FlatHasNoSignal { mean });
    }

    #[allow(clippy::cast_possible_truncation)]
    let mean32 = mean as f32;
    Ok(data
        .par_iter()
        .map(|v| {
            let gain = v / mean32;
            // Too little signal to divide by: mark it rather than amplifying
            // noise into an enormous bright pixel.
            if gain.is_finite() && gain >= MIN_GAIN {
                gain
            } else {
                f32::NAN
            }
        })
        .collect())
}

/// Combines flat frames into a gain map.
///
/// A flat must itself be calibrated before use: flats are short exposures that
/// still carry the sensor's read offset, so `flat_dark` should be a dark of the
/// same exposure as the flats, or a bias. Bias frames work in this slot exactly
/// as flat darks do, because nothing here inspects the exposure time.
///
/// # Errors
///
/// Returns [`CalibError::NoFrames`] for an empty list,
/// [`CalibError::Mismatched`] if the frames disagree in shape,
/// [`CalibError::WrongSize`] if `flat_dark` does not match, and
/// [`CalibError::FlatHasNoSignal`] if the combined flat is blank.
pub fn build_master_flat(
    flats: &[Arc<FitsImage>],
    flat_dark: Option<&MasterFrame>,
) -> Result<MasterFlat, CalibError> {
    let combined = build_master_median(flats)?;

    let mut data = combined.data;
    if let Some(dark) = flat_dark {
        if dark.width != combined.width
            || dark.height != combined.height
            || dark.channels != combined.channels
        {
            return Err(CalibError::WrongSize {
                frame: dark.shape(),
                image: format!(
                    "{}x{}x{}",
                    combined.width, combined.height, combined.channels
                ),
            });
        }
        data.par_iter_mut()
            .zip(dark.data.par_iter())
            .for_each(|(sample, offset)| {
                if sample.is_finite() && offset.is_finite() {
                    *sample = (*sample - *offset).max(0.0);
                } else {
                    *sample = f32::NAN;
                }
            });
    }

    let gain = normalise(&data)?;
    let unusable = gain.iter().filter(|v| !v.is_finite()).count();

    Ok(MasterFlat {
        width: combined.width,
        height: combined.height,
        channels: combined.channels,
        gain,
        source_count: combined.source_count,
        unusable,
    })
}

/// Divides a light frame by a gain map.
///
/// Call this **after** subtracting the dark, never before. See [`calibrate`].
///
/// # Errors
///
/// Returns [`CalibError::WrongSize`] if the frames disagree in shape.
pub fn divide_flat(light: &FitsImage, flat: &MasterFlat) -> Result<FitsImage, CalibError> {
    if !flat.matches(light) {
        return Err(CalibError::WrongSize {
            frame: flat.shape(),
            image: format!("{}x{}x{}", light.width, light.height, light.channels),
        });
    }

    let mut data = light.data.clone();
    data.par_iter_mut()
        .zip(flat.gain.par_iter())
        .for_each(|(sample, gain)| {
            if sample.is_finite() && gain.is_finite() && *gain >= MIN_GAIN {
                *sample /= *gain;
            } else {
                *sample = f32::NAN;
            }
        });

    let (min, max) = crate::image::finite_min_max(&data);
    Ok(FitsImage {
        width: light.width,
        height: light.height,
        channels: light.channels,
        data,
        header: light.header.clone(),
        min,
        max,
    })
}

/// How well a calibration frame matches the light it will be applied to.
///
/// A mismatch here does not stop calibration. The dimensions must agree, but
/// exposure and temperature are advisory: plenty of usable dark libraries are
/// slightly off, and the user is better placed to judge than the software.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compatibility {
    /// Reasons the frames may not suit each other, in plain language.
    pub warnings: Vec<String>,
}

impl Compatibility {
    /// Whether anything looked wrong.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }
}

/// Exposure times differing by more than this fraction are worth mentioning.
const EXPTIME_TOLERANCE: f64 = 0.05;

/// Temperatures differing by more than this many degrees are worth mentioning.
const TEMPERATURE_TOLERANCE: f64 = 2.0;

/// Compares a master against a light frame and reports anything suspicious.
#[must_use]
pub fn check_compatibility(master: &MasterFrame, light: &FitsImage) -> Compatibility {
    let mut warnings = Vec::new();

    if let (Some(master_exp), Some(light_exp)) = (master.exptime, light.header.get_f64("EXPTIME")) {
        if master_exp > 0.0 && light_exp > 0.0 {
            let difference = (master_exp - light_exp).abs() / light_exp;
            if difference > EXPTIME_TOLERANCE {
                warnings.push(format!(
                    "exposure differs: the dark is {master_exp:.1} s, the light {light_exp:.1} s"
                ));
            }
        }
    }

    let light_temperature = light
        .header
        .get_f64("CCD-TEMP")
        .or_else(|| light.header.get_f64("SET-TEMP"));
    if let (Some(master_temp), Some(light_temp)) = (master.temperature, light_temperature) {
        if (master_temp - light_temp).abs() > TEMPERATURE_TOLERANCE {
            warnings.push(format!(
                "temperature differs: the dark is {master_temp:.1}, the light {light_temp:.1}"
            ));
        }
    }

    Compatibility { warnings }
}

/// Combines frames pixel by pixel into a master.
///
/// Uses the median, which discards a cosmic ray that struck one frame instead
/// of averaging a ninth of it into every calibrated light. With one or two
/// frames there is no majority to take, so the mean is used instead; two frames
/// is already thin, and the caller should say so.
///
/// # Errors
///
/// Returns [`CalibError::NoFrames`] for an empty list, and
/// [`CalibError::Mismatched`] if the frames are not all the same shape.
pub fn build_master_median(frames: &[Arc<FitsImage>]) -> Result<MasterFrame, CalibError> {
    let first = frames.first().ok_or(CalibError::NoFrames)?;
    let (width, height, channels) = (first.width, first.height, first.channels);
    let expected = format!("{width}x{height}x{channels}");

    for (index, frame) in frames.iter().enumerate().skip(1) {
        if frame.width != width || frame.height != height || frame.channels != channels {
            return Err(CalibError::Mismatched {
                index,
                expected,
                found: format!("{}x{}x{}", frame.width, frame.height, frame.channels),
            });
        }
    }

    let count = first.data.len();
    let mut data = vec![0.0f32; count];
    let row = width.max(1);

    data.par_chunks_mut(row * ROW_CHUNK)
        .enumerate()
        .for_each(|(chunk_index, out)| {
            let base = chunk_index * row * ROW_CHUNK;
            // Reused across the chunk so the combine does not allocate per pixel.
            let mut values: Vec<f32> = Vec::with_capacity(frames.len());
            for (offset, slot) in out.iter_mut().enumerate() {
                let index = base + offset;
                values.clear();
                values.extend(
                    frames
                        .iter()
                        .filter_map(|f| f.data.get(index).copied())
                        .filter(|v| v.is_finite()),
                );
                *slot = combine(&mut values);
            }
        });

    Ok(MasterFrame {
        width,
        height,
        channels,
        data,
        source_count: frames.len(),
        exptime: median_header_value(frames, "EXPTIME"),
        temperature: median_header_value(frames, "CCD-TEMP")
            .or_else(|| median_header_value(frames, "SET-TEMP")),
    })
}

/// Combines the samples for one pixel across frames.
fn combine(values: &mut [f32]) -> f32 {
    match values.len() {
        // Every frame was undefined here, so the master is too.
        0 => f32::NAN,
        1 => values[0],
        // Two values have no middle; the mean is the best available.
        2 => (values[0] + values[1]) / 2.0,
        n => {
            let middle = n / 2;
            values.select_nth_unstable_by(middle, |a, b| {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            });
            let upper = values[middle];
            if n % 2 == 1 {
                upper
            } else {
                // An even count has two middle values; average them so the
                // result does not depend on which side the partition landed.
                let lower = values[..middle]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                (lower + upper) / 2.0
            }
        }
    }
}

/// The median of a header value across frames, ignoring frames that lack it.
fn median_header_value(frames: &[Arc<FitsImage>], keyword: &str) -> Option<f64> {
    let mut values: Vec<f64> = frames
        .iter()
        .filter_map(|f| f.header.get_f64(keyword))
        .filter(|v| v.is_finite())
        .collect();
    if values.is_empty() {
        return None;
    }
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(values[middle])
}

/// Subtracts a master dark from a light frame.
///
/// The result is clamped at zero. A pixel darker than the dark predicts is
/// noise, not negative light, and letting it go negative would drag the
/// background statistics the stretch depends on.
///
/// # Errors
///
/// Returns [`CalibError::WrongSize`] if the frames disagree in shape.
pub fn subtract_dark(light: &FitsImage, dark: &MasterFrame) -> Result<FitsImage, CalibError> {
    if !dark.matches(light) {
        return Err(CalibError::WrongSize {
            frame: dark.shape(),
            image: format!("{}x{}x{}", light.width, light.height, light.channels),
        });
    }

    let mut data = light.data.clone();
    data.par_iter_mut()
        .zip(dark.data.par_iter())
        .for_each(|(sample, offset)| {
            // An undefined sample stays undefined; an undefined dark pixel
            // means that pixel cannot be calibrated.
            if sample.is_finite() && offset.is_finite() {
                *sample = (*sample - *offset).max(0.0);
            } else if !offset.is_finite() {
                *sample = f32::NAN;
            }
        });

    let (min, max) = crate::image::finite_min_max(&data);
    Ok(FitsImage {
        width: light.width,
        height: light.height,
        channels: light.channels,
        data,
        header: light.header.clone(),
        min,
        max,
    })
}

/// Applies whatever calibration is available to a light frame.
///
/// The single entry point the application calls, so the order of operations
/// lives in exactly one place. That order is:
///
/// 1. subtract the dark, clamping at zero;
/// 2. divide by the flat's gain map.
///
/// **Subtraction before division, always.** Dividing first would scale the
/// dark's own signal by the gain map and smear it across the frame, in a way
/// nothing later can undo. The mistake is silent, which is why there is a test
/// asserting the wrong order gives a measurably different answer.
///
/// # Errors
///
/// Returns [`CalibError::WrongSize`] if a frame does not match the light.
pub fn calibrate(
    light: &FitsImage,
    dark: Option<&MasterFrame>,
    flat: Option<&MasterFlat>,
) -> Result<FitsImage, CalibError> {
    let subtracted = match dark {
        Some(dark) => subtract_dark(light, dark)?,
        None => light.clone(),
    };
    match flat {
        Some(flat) => divide_flat(&subtracted, flat),
        None => Ok(subtracted),
    }
}

/// The `HISTORY` lines describing what calibration was applied.
#[must_use]
pub fn history_for(dark: Option<&MasterFrame>, flat: Option<&MasterFlat>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dark) = dark {
        let exposure = dark
            .exptime
            .map_or_else(String::new, |e| format!(", {e:.1} s"));
        out.push(format!(
            "fitsview: dark subtracted (master of {} frame{}{exposure})",
            dark.source_count,
            if dark.source_count == 1 { "" } else { "s" }
        ));
    }
    if let Some(flat) = flat {
        out.push(format!(
            "fitsview: flat divided (master of {} frame{}, {} unusable pixel{})",
            flat.source_count,
            if flat.source_count == 1 { "" } else { "s" },
            flat.unusable,
            if flat.unusable == 1 { "" } else { "s" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_fits_from_bytes;
    use crate::testutil::{synthetic_fits, SyntheticSpec};

    /// An image from physical values.
    fn image(width: usize, height: usize, pixels: &[f64]) -> Arc<FitsImage> {
        let spec = SyntheticSpec::new(width, height, -32);
        Arc::new(read_fits_from_bytes(&synthetic_fits(&spec, pixels).unwrap()).unwrap())
    }

    /// An image carrying exposure and temperature metadata.
    fn image_with(
        width: usize,
        height: usize,
        pixels: &[f64],
        exptime: f64,
        temperature: f64,
    ) -> Arc<FitsImage> {
        let spec = SyntheticSpec::new(width, height, -32)
            .with_card("EXPTIME", &format!("{exptime:.1}"))
            .with_card("CCD-TEMP", &format!("{temperature:.1}"));
        Arc::new(read_fits_from_bytes(&synthetic_fits(&spec, pixels).unwrap()).unwrap())
    }

    #[test]
    fn the_median_rejects_an_outlier_in_one_frame() {
        // The reason several darks are combined rather than one being used: a
        // cosmic ray strikes a single frame, and the median must ignore it.
        let frames = vec![
            image(2, 2, &[100.0, 100.0, 100.0, 100.0]),
            image(2, 2, &[102.0, 100.0, 100.0, 100.0]),
            image(2, 2, &[9999.0, 100.0, 100.0, 100.0]), // the cosmic ray
        ];
        let master = build_master_median(&frames).unwrap();

        assert_eq!(master.source_count, 3);
        assert!(
            (master.data[0] - 102.0).abs() < 0.001,
            "the outlier should be rejected, got {}",
            master.data[0]
        );
        // The mean would have been over 3000.
        assert!(master.data[0] < 200.0);
    }

    #[test]
    fn a_single_frame_is_used_as_it_is() {
        let master = build_master_median(&[image(2, 2, &[1.0, 2.0, 3.0, 4.0])]).unwrap();
        assert_eq!(master.data, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(master.source_count, 1);
    }

    #[test]
    fn two_frames_are_averaged_since_there_is_no_middle_value() {
        let master = build_master_median(&[image(1, 1, &[10.0]), image(1, 1, &[20.0])]).unwrap();
        assert!(
            (master.data[0] - 15.0).abs() < 0.001,
            "got {}",
            master.data[0]
        );
    }

    #[test]
    fn an_even_number_of_frames_averages_the_two_middle_values() {
        // Otherwise the result would depend on which side the partition landed.
        let master = build_master_median(&[
            image(1, 1, &[1.0]),
            image(1, 1, &[2.0]),
            image(1, 1, &[4.0]),
            image(1, 1, &[100.0]),
        ])
        .unwrap();
        assert!(
            (master.data[0] - 3.0).abs() < 0.001,
            "got {}",
            master.data[0]
        );
    }

    #[test]
    fn an_odd_number_of_frames_takes_the_middle_value() {
        let master = build_master_median(&[
            image(1, 1, &[1.0]),
            image(1, 1, &[5.0]),
            image(1, 1, &[100.0]),
        ])
        .unwrap();
        assert!(
            (master.data[0] - 5.0).abs() < 0.001,
            "got {}",
            master.data[0]
        );
    }

    #[test]
    fn combining_no_frames_is_an_error() {
        assert_eq!(build_master_median(&[]).unwrap_err(), CalibError::NoFrames);
    }

    #[test]
    fn frames_of_different_sizes_are_refused_with_the_offending_index() {
        let err = build_master_median(&[
            image(2, 2, &[1.0; 4]),
            image(2, 2, &[1.0; 4]),
            image(3, 3, &[1.0; 9]),
        ])
        .unwrap_err();
        match err {
            CalibError::Mismatched { index, .. } => assert_eq!(index, 2),
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn undefined_pixels_are_ignored_when_combining() {
        let frames = vec![
            image(1, 1, &[f64::NAN]),
            image(1, 1, &[10.0]),
            image(1, 1, &[12.0]),
        ];
        let master = build_master_median(&frames).unwrap();
        assert!(
            (master.data[0] - 11.0).abs() < 0.001,
            "got {}",
            master.data[0]
        );
    }

    #[test]
    fn a_pixel_undefined_in_every_frame_stays_undefined() {
        let master =
            build_master_median(&[image(1, 1, &[f64::NAN]), image(1, 1, &[f64::NAN])]).unwrap();
        assert!(master.data[0].is_nan());
    }

    #[test]
    fn combining_works_across_rayon_chunks() {
        // Larger than one row chunk, so the parallel split is exercised and any
        // indexing error between chunks would show.
        let (w, h) = (32usize, ROW_CHUNK * 3);
        let a: Vec<f64> = (0..w * h).map(|i| i as f64).collect();
        let b: Vec<f64> = (0..w * h).map(|i| i as f64 + 2.0).collect();
        let c: Vec<f64> = (0..w * h).map(|i| i as f64 + 4.0).collect();

        let master =
            build_master_median(&[image(w, h, &a), image(w, h, &b), image(w, h, &c)]).unwrap();
        assert_eq!(master.data.len(), w * h);
        for (i, v) in master.data.iter().enumerate() {
            assert!((v - (i as f32 + 2.0)).abs() < 0.01, "pixel {i} was {v}");
        }
    }

    #[test]
    fn exposure_and_temperature_are_taken_from_the_frames() {
        let frames = vec![
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
        ];
        let master = build_master_median(&frames).unwrap();
        assert_eq!(master.exptime, Some(300.0));
        assert_eq!(master.temperature, Some(-10.0));
    }

    #[test]
    fn frames_without_metadata_leave_it_unknown() {
        let master = build_master_median(&[image(1, 1, &[1.0])]).unwrap();
        assert_eq!(master.exptime, None);
        assert_eq!(master.temperature, None);
    }

    #[test]
    fn subtracting_a_dark_removes_the_offset() {
        let light = image(2, 2, &[110.0, 120.0, 130.0, 140.0]);
        let dark = build_master_median(&[image(2, 2, &[10.0, 20.0, 30.0, 40.0])]).unwrap();

        let out = subtract_dark(&light, &dark).unwrap();
        assert_eq!(out.data, vec![100.0, 100.0, 100.0, 100.0]);
    }

    #[test]
    fn subtraction_clamps_at_zero_rather_than_going_negative() {
        // A pixel below what the dark predicts is noise, not negative light.
        let light = image(1, 3, &[5.0, 50.0, 100.0]);
        let dark = build_master_median(&[image(1, 3, &[10.0, 10.0, 10.0])]).unwrap();

        let out = subtract_dark(&light, &dark).unwrap();
        assert_eq!(out.data, vec![0.0, 40.0, 90.0]);
        assert!(out.data.iter().all(|v| *v >= 0.0));
    }

    #[test]
    fn subtraction_removes_a_hot_pixel() {
        // What calibration is for, in one assertion.
        let mut light_pixels = vec![100.0; 16];
        light_pixels[5] = 60_000.0; // a pixel that always reads high
        let mut dark_pixels = vec![10.0; 16];
        dark_pixels[5] = 59_900.0;

        let light = image(4, 4, &light_pixels);
        let dark = build_master_median(&[image(4, 4, &dark_pixels)]).unwrap();
        let out = subtract_dark(&light, &dark).unwrap();

        assert!(
            (out.data[5] - 100.0).abs() < 1.0,
            "the hot pixel should come down to the background, got {}",
            out.data[5]
        );
    }

    #[test]
    fn a_dark_of_the_wrong_size_is_refused() {
        let light = image(4, 4, &[1.0; 16]);
        let dark = build_master_median(&[image(2, 2, &[1.0; 4])]).unwrap();
        let err = subtract_dark(&light, &dark).unwrap_err();
        assert!(matches!(err, CalibError::WrongSize { .. }), "got {err:?}");
    }

    #[test]
    fn a_dark_with_the_wrong_channel_count_is_refused() {
        let spec = SyntheticSpec::new(2, 2, -32).with_channels(3);
        let colour =
            Arc::new(read_fits_from_bytes(&synthetic_fits(&spec, &[1.0; 12]).unwrap()).unwrap());
        let mono = build_master_median(&[image(2, 2, &[1.0; 4])]).unwrap();
        assert!(subtract_dark(&colour, &mono).is_err());
    }

    #[test]
    fn undefined_pixels_survive_subtraction() {
        let light = image(1, 2, &[f64::NAN, 100.0]);
        let dark = build_master_median(&[image(1, 2, &[10.0, 10.0])]).unwrap();
        let out = subtract_dark(&light, &dark).unwrap();
        assert!(out.data[0].is_nan(), "an undefined light stays undefined");
        assert_eq!(out.data[1], 90.0);
    }

    #[test]
    fn a_pixel_the_dark_cannot_describe_becomes_undefined() {
        let light = image(1, 2, &[100.0, 100.0]);
        let dark = build_master_median(&[image(1, 2, &[f64::NAN, 10.0])]).unwrap();
        let out = subtract_dark(&light, &dark).unwrap();
        assert!(out.data[0].is_nan(), "no dark value means no calibration");
        assert_eq!(out.data[1], 90.0);
    }

    #[test]
    fn statistics_are_recomputed_after_subtraction() {
        let light = image(1, 3, &[110.0, 150.0, 200.0]);
        let dark = build_master_median(&[image(1, 3, &[10.0, 10.0, 10.0])]).unwrap();
        let out = subtract_dark(&light, &dark).unwrap();
        assert_eq!((out.min, out.max), (100.0, 190.0));
    }

    #[test]
    fn calibrating_without_a_dark_returns_the_light_unchanged() {
        let light = image(2, 2, &[1.0, 2.0, 3.0, 4.0]);
        let out = calibrate(&light, None, None).unwrap();
        assert_eq!(out.data, light.data);
    }

    #[test]
    fn matching_metadata_produces_no_warnings() {
        let light = image_with(2, 2, &[1.0; 4], 300.0, -10.0);
        let dark = build_master_median(&[image_with(2, 2, &[1.0; 4], 300.0, -10.0)]).unwrap();
        assert!(check_compatibility(&dark, &light).is_clean());
    }

    #[test]
    fn a_mismatched_exposure_warns_without_blocking() {
        let light = image_with(2, 2, &[1.0; 4], 300.0, -10.0);
        let dark = build_master_median(&[image_with(2, 2, &[1.0; 4], 60.0, -10.0)]).unwrap();

        let check = check_compatibility(&dark, &light);
        assert!(!check.is_clean());
        assert!(
            check.warnings[0].contains("exposure"),
            "{:?}",
            check.warnings
        );
        // Advisory only: the subtraction still works.
        assert!(subtract_dark(&light, &dark).is_ok());
    }

    #[test]
    fn a_small_exposure_difference_is_tolerated() {
        let light = image_with(2, 2, &[1.0; 4], 300.0, -10.0);
        let dark = build_master_median(&[image_with(2, 2, &[1.0; 4], 302.0, -10.0)]).unwrap();
        assert!(check_compatibility(&dark, &light).is_clean());
    }

    #[test]
    fn a_mismatched_temperature_warns() {
        let light = image_with(2, 2, &[1.0; 4], 300.0, -10.0);
        let dark = build_master_median(&[image_with(2, 2, &[1.0; 4], 300.0, 5.0)]).unwrap();
        let check = check_compatibility(&dark, &light);
        assert!(
            check.warnings.iter().any(|w| w.contains("temperature")),
            "{:?}",
            check.warnings
        );
    }

    #[test]
    fn missing_metadata_produces_no_warnings_rather_than_false_alarms() {
        let light = image(2, 2, &[1.0; 4]);
        let dark = build_master_median(&[image(2, 2, &[1.0; 4])]).unwrap();
        assert!(check_compatibility(&dark, &light).is_clean());
    }

    #[test]
    fn a_master_round_trips_through_an_image() {
        let frames = vec![
            image_with(3, 2, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 300.0, -10.0),
            image_with(3, 2, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 300.0, -10.0),
            image_with(3, 2, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 300.0, -10.0),
        ];
        let master = build_master_median(&frames).unwrap();

        let encoded = crate::reader::encode_fits(&master.to_image(), &[]).unwrap();
        let reloaded = read_fits_from_bytes(&encoded).unwrap();
        let back = MasterFrame::from_image(&reloaded);

        assert_eq!(back.data, master.data);
        assert_eq!(back.source_count, 3, "the frame count must survive saving");
        assert_eq!(back.exptime, Some(300.0));
        assert_eq!(back.temperature, Some(-10.0));
    }

    /// A gain pattern shaped like vignetting: bright centre, dark corners.
    fn vignette(width: usize, height: usize, corner: f64) -> Vec<f64> {
        let (cx, cy) = ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
        let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
        (0..width * height)
            .map(|i| {
                let (x, y) = ((i % width) as f64, (i / width) as f64);
                let r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / max_r;
                1.0 - (1.0 - corner) * r
            })
            .collect()
    }

    #[test]
    fn a_master_flat_normalises_to_an_average_of_one() {
        // The definition of a gain map: dividing by it must not change overall
        // brightness, only even it out.
        let (w, h) = (16, 16);
        let pattern: Vec<f64> = vignette(w, h, 0.5).iter().map(|g| g * 20_000.0).collect();
        let flat = build_master_flat(&[image(w, h, &pattern)], None).unwrap();

        let mean: f64 =
            flat.gain.iter().map(|v| f64::from(*v)).sum::<f64>() / flat.gain.len() as f64;
        assert!((mean - 1.0).abs() < 1e-4, "average gain was {mean}");
        assert_eq!(flat.unusable, 0);
    }

    #[test]
    fn dividing_by_the_flat_recovers_an_even_field() {
        // What flat calibration is for. A uniformly lit sky seen through a
        // vignetting optical system comes back uniform.
        let (w, h) = (24, 24);
        let gains = vignette(w, h, 0.4);

        let flat_pixels: Vec<f64> = gains.iter().map(|g| g * 30_000.0).collect();
        let light_pixels: Vec<f64> = gains.iter().map(|g| g * 1000.0).collect();

        let flat = build_master_flat(&[image(w, h, &flat_pixels)], None).unwrap();
        let out = divide_flat(&image(w, h, &light_pixels), &flat).unwrap();

        let first = out.data[0];
        for (i, v) in out.data.iter().enumerate() {
            assert!(
                (v - first).abs() < first * 0.001,
                "pixel {i} was {v}, expected about {first}"
            );
        }

        // Normalising by the mean preserves the frame's AVERAGE brightness, not
        // its peak. The even field therefore sits at the light's own mean, which
        // is below the bright centre it started from. That is the correct and
        // conventional result: a gain map centred on 1.0 neither brightens nor
        // darkens the frame overall.
        let light_mean: f64 = light_pixels.iter().sum::<f64>() / light_pixels.len() as f64;
        assert!(
            (f64::from(first) - light_mean).abs() < light_mean * 0.001,
            "calibrated level {first} should equal the light's mean {light_mean}"
        );
    }

    #[test]
    fn the_dark_must_be_subtracted_before_the_flat_divides() {
        // The order that matters. Dividing first scales the dark's own signal
        // by the gain map and smears it across the frame. The mistake is
        // silent, so this asserts the two orders really do differ.
        let (w, h) = (16, 16);
        let gains = vignette(w, h, 0.4);

        let flat_pixels: Vec<f64> = gains.iter().map(|g| g * 20_000.0).collect();
        let flat = build_master_flat(&[image(w, h, &flat_pixels)], None).unwrap();

        // A light of even sky, seen through the vignetting, plus a constant
        // dark offset that is NOT attenuated by the optics.
        let offset = 500.0;
        let light_pixels: Vec<f64> = gains.iter().map(|g| g * 1000.0 + offset).collect();
        let light = image(w, h, &light_pixels);
        let dark = build_master_median(&[image(w, h, &vec![offset; w * h])]).unwrap();

        // Correct order: subtract, then divide.
        let right = calibrate(&light, Some(&dark), Some(&flat)).unwrap();

        // Wrong order: divide, then subtract.
        let divided = divide_flat(&light, &flat).unwrap();
        let wrong = subtract_dark(&divided, &dark).unwrap();

        let spread = |img: &FitsImage| {
            let finite: Vec<f32> = img.data.iter().copied().filter(|v| v.is_finite()).collect();
            let mean = finite.iter().map(|v| f64::from(*v)).sum::<f64>() / finite.len() as f64;
            let variance = finite
                .iter()
                .map(|v| (f64::from(*v) - mean).powi(2))
                .sum::<f64>()
                / finite.len() as f64;
            variance.sqrt()
        };

        let right_spread = spread(&right);
        let wrong_spread = spread(&wrong);

        assert!(
            right_spread < 1.0,
            "the correct order should give an even field, spread was {right_spread}"
        );
        assert!(
            wrong_spread > right_spread * 10.0,
            "the wrong order should leave a visible gradient: right {right_spread}, wrong {wrong_spread}"
        );
    }

    #[test]
    fn a_flat_dark_is_subtracted_before_normalising() {
        // Flats are short exposures and still carry the read offset, so the
        // gain map must be built from the signal alone.
        let (w, h) = (8, 8);
        let signal: Vec<f64> = (0..w * h).map(|i| 1000.0 + i as f64 * 10.0).collect();
        let offset = 200.0;
        let raw: Vec<f64> = signal.iter().map(|v| v + offset).collect();

        let flat_dark = build_master_median(&[image(w, h, &vec![offset; w * h])]).unwrap();
        let with_dark = build_master_flat(&[image(w, h, &raw)], Some(&flat_dark)).unwrap();
        let without_dark = build_master_flat(&[image(w, h, &signal)], None).unwrap();

        for (a, b) in with_dark.gain.iter().zip(without_dark.gain.iter()) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn a_flat_dark_of_the_wrong_size_is_refused() {
        let flat_dark = build_master_median(&[image(4, 4, &[1.0; 16])]).unwrap();
        let err = build_master_flat(&[image(8, 8, &[100.0; 64])], Some(&flat_dark)).unwrap_err();
        assert!(matches!(err, CalibError::WrongSize { .. }), "got {err:?}");
    }

    #[test]
    fn a_blank_flat_is_refused_rather_than_producing_an_all_undefined_image() {
        // The lens cap case. Every pixel would divide by nothing.
        let err = build_master_flat(&[image(4, 4, &[0.0; 16])], None).unwrap_err();
        assert!(
            matches!(err, CalibError::FlatHasNoSignal { .. }),
            "got {err:?}"
        );

        let err = build_master_flat(&[image(4, 4, &[-5.0; 16])], None).unwrap_err();
        assert!(
            matches!(err, CalibError::FlatHasNoSignal { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_flat_of_only_undefined_pixels_is_refused() {
        let err = build_master_flat(&[image(2, 2, &[f64::NAN; 4])], None).unwrap_err();
        assert!(
            matches!(err, CalibError::FlatHasNoSignal { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn pixels_with_too_little_gain_become_undefined_and_are_counted() {
        // Dividing by a near-zero gain would turn read noise into an enormous
        // bright pixel, which looks like a star.
        let (w, h) = (4, 4);
        let mut pixels = vec![10_000.0; w * h];
        pixels[0] = 0.0; // a completely dead corner
        pixels[1] = 1.0; // and one almost dead

        let flat = build_master_flat(&[image(w, h, &pixels)], None).unwrap();
        assert_eq!(flat.unusable, 2, "both should be marked unusable");
        assert!(flat.gain[0].is_nan());
        assert!(flat.gain[1].is_nan());
        assert!(flat.unusable_fraction() > 0.0);

        let out = divide_flat(&image(w, h, &vec![500.0; w * h]), &flat).unwrap();
        assert!(out.data[0].is_nan(), "no gain means no calibrated value");
        assert!(out.data[1].is_nan());
        assert!(
            out.data.iter().all(|v| !v.is_infinite()),
            "dividing must never produce an infinity"
        );

        // The dead pixels drag the average down, so the surviving pixels have a
        // gain slightly above 1 and come out correspondingly brighter. Assert
        // against the gain map rather than against the input, since that is the
        // relationship the division actually promises.
        let expected = 500.0 / flat.gain[5];
        assert!(
            (out.data[5] - expected).abs() < 0.01,
            "good pixel was {}, expected {expected}",
            out.data[5]
        );
        assert!(out.data[5].is_finite());
    }

    #[test]
    fn a_colour_flat_is_normalised_by_one_global_average() {
        // Normalising each plane separately would divide out the camera's
        // colour response and leave a grey image, the same mistake the stretch
        // had to be corrected for.
        let (w, h) = (8, 8);
        let mut pixels = vec![0.0; w * h * 3];
        for (i, v) in pixels.iter_mut().enumerate() {
            *v = match i / (w * h) {
                0 => 30_000.0,
                1 => 20_000.0,
                _ => 10_000.0,
            };
        }
        let spec = SyntheticSpec::new(w, h, -32).with_channels(3);
        let colour =
            Arc::new(read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap());

        let flat = build_master_flat(&[colour], None).unwrap();
        assert_eq!(flat.channels, 3);

        // Global mean is 20000, so the three planes keep their ratio.
        assert!(
            (flat.gain[0] - 1.5).abs() < 0.01,
            "red gain {}",
            flat.gain[0]
        );
        assert!(
            (flat.gain[w * h] - 1.0).abs() < 0.01,
            "green gain {}",
            flat.gain[w * h]
        );
        assert!(
            (flat.gain[2 * w * h] - 0.5).abs() < 0.01,
            "blue gain {}",
            flat.gain[2 * w * h]
        );
    }

    #[test]
    fn a_flat_of_the_wrong_size_is_refused_when_dividing() {
        let flat = build_master_flat(&[image(2, 2, &[100.0; 4])], None).unwrap();
        let err = divide_flat(&image(4, 4, &[1.0; 16]), &flat).unwrap_err();
        assert!(matches!(err, CalibError::WrongSize { .. }), "got {err:?}");
    }

    #[test]
    fn every_combination_of_dark_and_flat_behaves() {
        let (w, h) = (8, 8);
        let light = image(w, h, &vec![1000.0; w * h]);
        let dark = build_master_median(&[image(w, h, &vec![100.0; w * h])]).unwrap();
        // A uniform flat has a gain of exactly 1 everywhere, so it changes
        // nothing and the four cases are easy to reason about.
        let flat = build_master_flat(&[image(w, h, &vec![5000.0; w * h])], None).unwrap();

        let neither = calibrate(&light, None, None).unwrap();
        assert!((neither.data[0] - 1000.0).abs() < 0.01);

        let dark_only = calibrate(&light, Some(&dark), None).unwrap();
        assert!((dark_only.data[0] - 900.0).abs() < 0.01);

        let flat_only = calibrate(&light, None, Some(&flat)).unwrap();
        assert!((flat_only.data[0] - 1000.0).abs() < 0.01);

        let both = calibrate(&light, Some(&dark), Some(&flat)).unwrap();
        assert!((both.data[0] - 900.0).abs() < 0.01);
    }

    #[test]
    fn a_gain_map_round_trips_through_an_image() {
        let (w, h) = (8, 8);
        let pattern: Vec<f64> = vignette(w, h, 0.6).iter().map(|g| g * 15_000.0).collect();
        let flat = build_master_flat(
            &[
                image(w, h, &pattern),
                image(w, h, &pattern),
                image(w, h, &pattern),
            ],
            None,
        )
        .unwrap();

        let encoded = crate::reader::encode_fits(&flat.to_image(), &[]).unwrap();
        let reloaded = read_fits_from_bytes(&encoded).unwrap();
        let back = MasterFlat::from_image(&reloaded);

        assert_eq!(back.source_count, 3, "the frame count must survive saving");
        for (a, b) in back.gain.iter().zip(flat.gain.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn an_ordinary_image_loaded_as_a_flat_is_normalised_on_the_way_in() {
        // So that any single frame can serve as a flat without being combined.
        let img = image(4, 4, &[8000.0; 16]);
        let flat = MasterFlat::from_image(&img);
        for gain in &flat.gain {
            assert!((gain - 1.0).abs() < 1e-4, "gain {gain}");
        }
    }

    #[test]
    fn a_blank_image_loaded_as_a_flat_falls_back_to_no_correction() {
        // Refusing to open the file would be worse than applying nothing.
        let img = image(4, 4, &[0.0; 16]);
        let flat = MasterFlat::from_image(&img);
        assert!(flat.gain.iter().all(|g| (g - 1.0).abs() < f32::EPSILON));
    }

    #[test]
    fn undefined_light_pixels_stay_undefined_through_the_flat() {
        let flat = build_master_flat(&[image(1, 2, &[100.0, 100.0])], None).unwrap();
        let out = divide_flat(&image(1, 2, &[f64::NAN, 500.0]), &flat).unwrap();
        assert!(out.data[0].is_nan());
        assert!((out.data[1] - 500.0).abs() < 0.01);
    }

    #[test]
    fn history_describes_what_was_applied() {
        let dark = build_master_median(&[
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
        ])
        .unwrap();
        let history = history_for(Some(&dark), None);
        assert_eq!(history.len(), 1);
        assert!(history[0].contains("3 frames"), "{}", history[0]);
        assert!(history[0].contains("300.0 s"), "{}", history[0]);

        assert!(history_for(None, None).is_empty());
    }

    #[test]
    fn history_records_the_flat_and_its_unusable_pixels() {
        let mut pixels = vec![10_000.0; 16];
        pixels[0] = 0.0;
        let flat = build_master_flat(&[image(4, 4, &pixels)], None).unwrap();

        let history = history_for(None, Some(&flat));
        assert_eq!(history.len(), 1);
        assert!(history[0].contains("flat divided"), "{}", history[0]);
        assert!(history[0].contains("1 unusable pixel"), "{}", history[0]);

        // Both together, in the order they are applied.
        let dark = build_master_median(&[image(4, 4, &[1.0; 16])]).unwrap();
        let both = history_for(Some(&dark), Some(&flat));
        assert_eq!(both.len(), 2);
        assert!(both[0].contains("dark"), "dark should be recorded first");
        assert!(both[1].contains("flat"));
    }
}
