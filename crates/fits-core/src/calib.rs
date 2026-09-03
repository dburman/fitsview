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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
/// lives in one place. Flat division joins it in Phase 7, after the dark
/// subtraction, never before.
///
/// # Errors
///
/// Returns [`CalibError::WrongSize`] if a frame does not match the light.
pub fn calibrate(light: &FitsImage, dark: Option<&MasterFrame>) -> Result<FitsImage, CalibError> {
    match dark {
        Some(dark) => subtract_dark(light, dark),
        None => Ok(light.clone()),
    }
}

/// The `HISTORY` lines describing what calibration was applied.
#[must_use]
pub fn history_for(dark: Option<&MasterFrame>) -> Vec<String> {
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
        let out = calibrate(&light, None).unwrap();
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

    #[test]
    fn history_describes_what_was_applied() {
        let dark = build_master_median(&[
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
            image_with(1, 1, &[1.0], 300.0, -10.0),
        ])
        .unwrap();
        let history = history_for(Some(&dark));
        assert_eq!(history.len(), 1);
        assert!(history[0].contains("3 frames"), "{}", history[0]);
        assert!(history[0].contains("300.0 s"), "{}", history[0]);

        assert!(history_for(None).is_empty());
    }
}
