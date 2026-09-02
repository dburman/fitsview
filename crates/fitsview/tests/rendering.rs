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
