//! The window icon.
//!
//! Drawn in code rather than committed as an image file, so there is no binary
//! asset in the repository and no build step to generate one. It is a simple
//! mark: a dark sky with a bright star and a fainter companion, which reads
//! correctly at the sizes a dock or taskbar uses.

/// Edge length of the icon in pixels.
pub const SIZE: usize = 64;

/// A star to draw: centre, radius of the glow, and peak brightness.
struct Star {
    x: f32,
    y: f32,
    radius: f32,
    peak: f32,
}

/// The icon as RGBA bytes, four per pixel, row by row from the top.
#[must_use]
pub fn rgba() -> Vec<u8> {
    #[allow(clippy::cast_precision_loss)]
    let size = SIZE as f32;

    let stars = [
        Star {
            x: size * 0.38,
            y: size * 0.40,
            radius: size * 0.30,
            peak: 1.0,
        },
        Star {
            x: size * 0.68,
            y: size * 0.66,
            radius: size * 0.16,
            peak: 0.65,
        },
    ];

    let mut pixels = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            #[allow(clippy::cast_precision_loss)]
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);

            let mut brightness = 0.0f32;
            for star in &stars {
                let distance = ((px - star.x).powi(2) + (py - star.y).powi(2)).sqrt();
                if distance < star.radius {
                    // A smooth falloff, squared so the core stays tight and the
                    // glow fades rather than ending in a hard circle.
                    let t = 1.0 - distance / star.radius;
                    brightness += star.peak * t * t;
                }
            }
            let brightness = brightness.clamp(0.0, 1.0);

            // Slightly blue-white stars on a very dark ground.
            let base = 18.0;
            let red = base + brightness * (250.0 - base);
            let green = base + brightness * (250.0 - base);
            let blue = base + brightness * (255.0 - base);

            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                pixels.push(red.round() as u8);
                pixels.push(green.round() as u8);
                pixels.push(blue.round() as u8);
                pixels.push(255);
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_the_right_size_and_fully_opaque() {
        let pixels = rgba();
        assert_eq!(pixels.len(), SIZE * SIZE * 4);
        assert!(
            pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            "a transparent icon would show the desktop through it"
        );
    }

    #[test]
    fn the_icon_has_a_bright_star_on_a_dark_ground() {
        let pixels = rgba();
        let brightness = |x: usize, y: usize| u32::from(pixels[(y * SIZE + x) * 4]);

        // The corner is background, the main star's centre is bright.
        assert!(brightness(0, 0) < 40, "the ground should be dark");
        let star = brightness(SIZE * 38 / 100, SIZE * 40 / 100);
        assert!(star > 200, "the star should be bright, got {star}");
    }

    #[test]
    fn the_icon_is_not_a_flat_block_of_colour() {
        let pixels = rgba();
        let distinct: std::collections::HashSet<u8> =
            pixels.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert!(
            distinct.len() > 20,
            "expected a gradient, got {} levels",
            distinct.len()
        );
    }
}
