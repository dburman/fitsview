//! End-to-end rendering checks.
//!
//! These go from a file on disk all the way to the texture that would be
//! uploaded to the GPU, which is as close to "what the user sees" as it is
//! possible to get without a window and a screenshot. They exist mainly to
//! guard orientation, the single easiest thing in an image viewer to get
//! backwards and the hardest to notice in a narrower test.

use egui::Color32;
use fits_core::read_fits;
use fits_core::testutil::{gaussian_background, write_synthetic, SyntheticSpec};
use fitsview::app::{Action, Model};
use fitsview::texture::{self, Mapping};
use std::time::{Duration, Instant};

/// Pumps the model until the selected image is on screen.
///
/// Loading moved to a worker thread in Phase 3, so opening a path no longer
/// produces an image by the time `handle` returns.
fn settle(model: &mut Model) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        model.poll();
        if !model.loading && (model.loaded.is_some() || model.error.is_some()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("image never arrived");
}

/// Average brightness of one texture row.
fn row_brightness(image: &egui::ColorImage, y: usize) -> f64 {
    let w = image.size[0];
    let row = &image.pixels[y * w..(y + 1) * w];
    row.iter().map(|p| f64::from(p.r())).sum::<f64>() / w as f64
}

/// Renders an image exactly the way the application does.
fn render(image: &fits_core::FitsImage) -> egui::ColorImage {
    let factor = texture::downsample_factor(image.width, image.height, texture::MAX_TEXTURE_EDGE);
    texture::to_color_image(image, &Mapping::linear(image), factor)
}

#[test]
fn a_bright_first_fits_row_appears_at_the_bottom_of_the_picture() {
    // The whole orientation question in one test. FITS stores the bottom row of
    // the image first, so making the FIRST rows bright must produce a picture
    // with a bright band along its BOTTOM edge.
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (200usize, 100usize);
    let mut pixels = vec![100.0f64; w * h];
    for row in 0..10 {
        for x in 0..w {
            pixels[row * w + x] = 60_000.0;
        }
    }
    let spec = SyntheticSpec::new(w, h, 16).with_scaling(32768.0, 1.0);
    let path = write_synthetic(dir.path(), "orientation.fits", &spec, &pixels).unwrap();

    let image = read_fits(&path).unwrap();
    let rendered = render(&image);

    let top = row_brightness(&rendered, 0);
    let bottom = row_brightness(&rendered, rendered.size[1] - 1);

    assert!(
        bottom > 250.0,
        "the bright FITS rows belong at the bottom, but the bottom row averaged {bottom}"
    );
    assert!(
        top < 5.0,
        "the top of the picture should be dark, but it averaged {top}"
    );
}

#[test]
fn a_full_frame_image_is_downsampled_but_keeps_its_orientation() {
    // The same check at a size that triggers downsampling, because the flip and
    // the box filter interact and could cancel out or be applied twice.
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (6000usize, 4000usize);
    let mut pixels = gaussian_background(w, h, 1200.0, 40.0, 11);
    for row in 0..200 {
        for x in 0..w {
            pixels[row * w + x] = 50_000.0;
        }
    }
    let spec = SyntheticSpec::new(w, h, 16).with_scaling(32768.0, 1.0);
    let path = write_synthetic(dir.path(), "big.fits", &spec, &pixels).unwrap();

    let image = read_fits(&path).unwrap();
    assert_eq!((image.width, image.height), (6000, 4000));

    let rendered = render(&image);
    assert!(
        rendered.size[0] <= texture::MAX_TEXTURE_EDGE
            && rendered.size[1] <= texture::MAX_TEXTURE_EDGE,
        "texture {:?} exceeds the size limit",
        rendered.size
    );

    let bottom = row_brightness(&rendered, rendered.size[1] - 1);
    let top = row_brightness(&rendered, 0);
    assert!(
        bottom > top * 4.0,
        "bright band lost or flipped: top {top}, bottom {bottom}"
    );
}

#[test]
fn nan_pixels_render_black_without_blanking_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (32usize, 32usize);
    let mut pixels: Vec<f64> = (0..w * h).map(|i| (i % 256) as f64).collect();
    pixels[0] = f64::NAN;
    pixels[5] = f64::NAN;

    let spec = SyntheticSpec::new(w, h, -32);
    let path = write_synthetic(dir.path(), "nan.fits", &spec, &pixels).unwrap();
    let image = read_fits(&path).unwrap();
    let rendered = render(&image);

    // The image must still have contrast. A single NaN reaching the statistics
    // unguarded would flatten everything to one value.
    let distinct: std::collections::HashSet<u8> = rendered.pixels.iter().map(Color32::r).collect();
    assert!(
        distinct.len() > 10,
        "image collapsed to {} distinct values",
        distinct.len()
    );

    // FITS row 0 is the bottom of the picture, so sample 0 lands bottom-left.
    let bottom_left = rendered.pixels[(rendered.size[1] - 1) * rendered.size[0]];
    assert_eq!(
        bottom_left,
        Color32::from_gray(0),
        "NaN should render black"
    );
}

#[test]
fn opening_a_real_file_through_the_model_produces_a_drawable_texture() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SyntheticSpec::new(800, 600, 16).with_scaling(32768.0, 1.0);
    let pixels: Vec<f64> = (0..800 * 600).map(|i| (i % 60_000) as f64).collect();
    let path = write_synthetic(dir.path(), "light.fits", &spec, &pixels).unwrap();

    let mut model = Model::new();
    model.handle(Action::Open(path));
    settle(&mut model);

    let loaded = model.loaded.as_ref().expect("should have loaded");
    let rendered = render(&loaded.image);
    assert_eq!(rendered.size, [800, 600]);
    assert_eq!(rendered.pixels.len(), 800 * 600);
    assert!(model.status_text().contains("800x600"));
}

#[test]
fn a_colour_image_renders_its_three_planes_into_rgb() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (16usize, 16usize);
    let mut pixels = vec![0.0f64; w * h * 3];
    // Red plane bright, green and blue left at zero.
    for v in pixels.iter_mut().take(w * h) {
        *v = 1000.0;
    }
    let spec = SyntheticSpec::new(w, h, 16).with_channels(3);
    let path = write_synthetic(dir.path(), "rgb.fits", &spec, &pixels).unwrap();

    let image = read_fits(&path).unwrap();
    let rendered = render(&image);
    let p = rendered.pixels[0];
    assert_eq!(p.r(), 255, "red plane should be saturated");
    assert_eq!(p.g(), 0, "green plane should be black");
    assert_eq!(p.b(), 0, "blue plane should be black");
}

#[test]
fn the_stretch_lifts_a_dark_sky_background_into_view() {
    // The reason the feature exists. A raw frame is a faint background just
    // above black with a few bright stars, which linearly shows as nothing.
    use fits_core::stretch::StretchParams;

    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (300usize, 300usize);
    let mut pixels = gaussian_background(w, h, 1000.0, 30.0, 21);
    // A handful of stars, which set the top of the range.
    for i in 0..20 {
        pixels[(i * 4177) % (w * h)] = 60_000.0;
    }
    let spec = SyntheticSpec::new(w, h, 16).with_scaling(32768.0, 1.0);
    let path = write_synthetic(dir.path(), "sky.fits", &spec, &pixels).unwrap();
    let image = read_fits(&path).unwrap();

    let factor = texture::downsample_factor(image.width, image.height, texture::MAX_TEXTURE_EDGE);
    let linear = texture::to_color_image(&image, &Mapping::linear(&image), factor);
    let stretched = texture::to_color_image(
        &image,
        &Mapping::stretched(&image, &StretchParams::default()),
        factor,
    );

    let mean = |img: &egui::ColorImage| {
        img.pixels.iter().map(|p| f64::from(p.r())).sum::<f64>() / img.pixels.len() as f64
    };
    let linear_mean = mean(&linear);
    let stretched_mean = mean(&stretched);

    assert!(
        linear_mean < 10.0,
        "a linear view of a raw frame should be nearly black, got {linear_mean}"
    );
    assert!(
        stretched_mean > 50.0,
        "the stretch should make the background visible, got {stretched_mean}"
    );
}

#[test]
fn the_stretch_leaves_nan_pixels_black() {
    use fits_core::stretch::StretchParams;

    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (64usize, 64usize);
    let mut pixels = gaussian_background(w, h, 900.0, 25.0, 4);
    pixels[0] = f64::NAN;
    let spec = SyntheticSpec::new(w, h, -32);
    let path = write_synthetic(dir.path(), "nan.fits", &spec, &pixels).unwrap();
    let image = read_fits(&path).unwrap();

    let rendered = texture::to_color_image(
        &image,
        &Mapping::stretched(&image, &StretchParams::default()),
        1,
    );
    // FITS row 0 is the bottom of the picture, so sample 0 lands bottom-left.
    let bottom_left = rendered.pixels[(rendered.size[1] - 1) * rendered.size[0]];
    assert_eq!(bottom_left, Color32::from_gray(0), "NaN should stay black");
}

#[test]
fn a_stretched_colour_image_keeps_its_colour() {
    // Each plane is stretched with its own table, but all share one
    // normalisation, so a red-dominant frame must not come out grey.
    use fits_core::stretch::StretchParams;

    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (32usize, 32usize);
    let mut pixels = vec![0.0f64; w * h * 3];
    let red = gaussian_background(w, h, 4000.0, 50.0, 31);
    let green = gaussian_background(w, h, 1500.0, 50.0, 32);
    let blue = gaussian_background(w, h, 800.0, 50.0, 33);
    pixels[..w * h].copy_from_slice(&red);
    pixels[w * h..2 * w * h].copy_from_slice(&green);
    pixels[2 * w * h..].copy_from_slice(&blue);

    let spec = SyntheticSpec::new(w, h, 16).with_channels(3);
    let path = write_synthetic(dir.path(), "colour.fits", &spec, &pixels).unwrap();
    let image = read_fits(&path).unwrap();

    let rendered = texture::to_color_image(
        &image,
        &Mapping::stretched(&image, &StretchParams::default()),
        1,
    );
    let mean = |f: fn(&Color32) -> u8| {
        rendered.pixels.iter().map(|p| f64::from(f(p))).sum::<f64>() / rendered.pixels.len() as f64
    };
    let (r, g, b) = (mean(|p| p.r()), mean(|p| p.g()), mean(|p| p.b()));
    assert!(
        r > g && g > b,
        "channel order should survive the stretch: r={r:.1} g={g:.1} b={b:.1}"
    );
}
