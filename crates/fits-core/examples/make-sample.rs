//! Writes sample FITS files for manual testing.
//!
//! The manual test checklist in `docs/manual-tests.md` needs files with known
//! properties, particularly one whose orientation is obvious so the vertical
//! flip can be checked by eye.
//!
//! ```text
//! cargo run --release --package fits-core --all-features --example make-sample -- /tmp/samples
//! ```

use std::path::Path;

use fits_core::testutil::{gaussian_background, write_synthetic, SyntheticSpec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir)?;

    // A full-frame light: 24 megapixels, unsigned 16-bit, the common case.
    // The bottom 200 FITS rows are bright, so orientation is obvious. Because
    // FITS row 0 is the bottom of the picture, a correct viewer shows the
    // bright band along the BOTTOM edge. A viewer that forgets to flip shows
    // it along the top.
    let (w, h) = (6000, 4000);
    let mut pixels = gaussian_background(w, h, 1200.0, 40.0, 11);
    for row in 0..200 {
        for x in 0..w {
            pixels[row * w + x] = 50_000.0;
        }
    }
    // A few hot pixels, so calibration in a later phase has something to remove.
    for i in 0..500 {
        pixels[(i * 7919) % (w * h)] = 65_000.0;
    }
    let spec = SyntheticSpec::new(w, h, 16)
        .with_scaling(32768.0, 1.0)
        .with_card("OBJECT", "'Orientation test'")
        .with_card("EXPTIME", "               120.0");
    let p = write_synthetic(dir, "light_orientation.fits", &spec, &pixels)?;
    println!("{}", p.display());

    // A small float image containing NaN, which must render black rather than
    // blanking the whole frame.
    let (w, h) = (64, 64);
    let mut pixels: Vec<f64> = (0..w * h).map(|i| (i % 256) as f64).collect();
    for i in 0..200 {
        pixels[i * 13 % (w * h)] = f64::NAN;
    }
    let spec = SyntheticSpec::new(w, h, -32).with_card("OBJECT", "'NaN test'");
    let p = write_synthetic(dir, "nan_test.fits", &spec, &pixels)?;
    println!("{}", p.display());

    // A three-plane colour image.
    let (w, h) = (256, 256);
    let mut pixels = vec![0.0; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            pixels[y * w + x] = x as f64; // red ramps right
            pixels[w * h + y * w + x] = y as f64; // green ramps up
            pixels[2 * w * h + y * w + x] = 128.0; // blue constant
        }
    }
    let spec = SyntheticSpec::new(w, h, 16)
        .with_channels(3)
        .with_card("OBJECT", "'Colour test'");
    let p = write_synthetic(dir, "colour_test.fits", &spec, &pixels)?;
    println!("{}", p.display());

    // Not a FITS file, to check that folder scanning ignores it in Phase 3.
    std::fs::write(dir.join("notes.txt"), b"not a fits file\n")?;
    println!("{}", dir.join("notes.txt").display());

    Ok(())
}
