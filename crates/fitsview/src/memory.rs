//! How much memory there is, and whether a stack fits in it.
//!
//! Rejecting outliers keeps twenty-two bytes of figures for every sample of the
//! stack, beside eight of running totals: five and a half gigabytes for 61
//! megapixels in colour. That does not fit beside everything else on a 16 GB
//! machine, let alone an 8 GB one, and a stack that makes the machine swap
//! takes many times as long as one that does not. So a colour stack whose
//! figures would not fit is stacked a colour at a time instead: a third of the
//! memory, for reading every frame three times as often. The result is the same
//! to the bit either way.

/// What the system says about its memory, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// All of it.
    pub total: u64,
    /// What could be given to a new task without making others swap, or zero
    /// if the system will not say.
    pub available: u64,
}

impl Memory {
    /// As the system reports it now.
    ///
    /// The one query fitsview makes that the standard library cannot: it goes
    /// through `sysinfo`, which keeps the platform calls behind a safe
    /// interface.
    #[must_use]
    pub fn now() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        Self {
            total: system.total_memory(),
            available: system.available_memory(),
        }
    }

    /// How much a stack may take.
    ///
    /// Half the machine's memory, and four fifths of what is free. Half,
    /// because the viewer, the system and whatever else is open need the rest;
    /// and the free figure too, because on a busy machine half of all of it
    /// may already be spoken for. macOS reports far more free than it can give
    /// without compressing and swapping, which is why the half is needed and
    /// the free figure alone will not do.
    #[must_use]
    pub fn budget(self) -> u64 {
        let half = self.total / 2;
        if self.available == 0 {
            half
        } else {
            half.min(self.available / 5 * 4)
        }
    }
}

/// Bytes a rejecting stack keeps for every sample: a twenty-byte record, a
/// two-byte count, and eight of running totals.
const REJECTING_SAMPLE: u64 = 30;

/// Bytes a plain stack keeps for every sample: its running totals.
const PLAIN_SAMPLE: u64 = 8;

/// Bytes a pixel of the frame being worked on takes while it is: raw, as
/// calibrated, in colour, half placed, and what finding its stars needs. The
/// stand-alone stack of the Barnard's Loop night measured 7.46 GB resident,
/// which this and the figures above account for.
const WORKING_PIXEL: u64 = 32;

/// What a stack of `channels` at a time needs, in bytes.
#[must_use]
pub fn stack_needs(width: usize, height: usize, channels: usize, reject: bool) -> u64 {
    let pixels = (width as u64) * (height as u64);
    let sample = if reject {
        REJECTING_SAMPLE
    } else {
        PLAIN_SAMPLE
    };
    pixels * (channels as u64) * sample + pixels * WORKING_PIXEL
}

/// The room a stack has: the budget, less what the application already holds —
/// the images the viewer has decoded, which stay while the stack runs and came
/// to 1.2 GB beside the Barnard's Loop night.
#[must_use]
pub fn room(memory: Memory, held: u64) -> u64 {
    memory.budget().saturating_sub(held)
}

/// Whether a frame's `channels` can be stacked together in `room` bytes.
///
/// A frame of one channel always can: there is nothing to split.
#[must_use]
pub fn together_fits(
    width: usize,
    height: usize,
    channels: usize,
    reject: bool,
    room: u64,
) -> bool {
    channels <= 1 || stack_needs(width, height, channels, reject) <= room
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    /// A 61-megapixel colour camera, as the Barnard's Loop night used.
    const FRAME: (usize, usize) = (9576, 6388);

    fn machine(total: u64, available: u64) -> Memory {
        Memory {
            total: total * GB,
            available: available * GB,
        }
    }

    /// The room on a machine whose viewer holds what it held beside that
    /// night: the frame on screen, in colour, and a neighbour.
    fn room_on(total: u64, available: u64) -> u64 {
        room(machine(total, available), 1_200_000_000)
    }

    #[test]
    fn what_a_stack_needs_matches_what_one_was_measured_to_take() {
        // 7.46 GB resident, stand-alone, for that night rejecting in colour.
        let need = stack_needs(FRAME.0, FRAME.1, 3, true);
        #[allow(clippy::cast_precision_loss)]
        let gb = need as f64 / GB as f64;
        assert!((7.2..7.7).contains(&gb), "{gb:.2} GB");
        // A colour at a time, a third of the figures.
        #[allow(clippy::cast_precision_loss)]
        let apart = stack_needs(FRAME.0, FRAME.1, 1, true) as f64 / GB as f64;
        assert!(apart < 4.0, "{apart:.2} GB");
    }

    #[test]
    fn a_large_machine_stacks_the_colours_together() {
        assert!(together_fits(FRAME.0, FRAME.1, 3, true, room_on(64, 40)));
        assert!(together_fits(FRAME.0, FRAME.1, 3, true, room_on(32, 20)));
    }

    #[test]
    fn a_small_machine_stacks_a_colour_at_a_time() {
        assert!(!together_fits(FRAME.0, FRAME.1, 3, true, room_on(16, 12)));
        assert!(!together_fits(FRAME.0, FRAME.1, 3, true, room_on(8, 6)));
    }

    #[test]
    fn a_busy_machine_does_too_however_much_it_has() {
        assert!(!together_fits(FRAME.0, FRAME.1, 3, true, room_on(64, 6)));
    }

    #[test]
    fn a_plain_stack_is_small_enough_almost_anywhere() {
        assert!(together_fits(FRAME.0, FRAME.1, 3, false, room_on(16, 12)));
    }

    #[test]
    fn a_mono_stack_is_never_split() {
        assert!(together_fits(FRAME.0, FRAME.1, 1, true, 0));
    }

    #[test]
    fn a_system_that_will_not_say_what_is_free_is_given_half() {
        assert_eq!(machine(16, 0).budget(), 8 * GB);
        assert_eq!(
            machine(16, 40).budget(),
            8 * GB,
            "half, however much is said to be free"
        );
        assert_eq!(
            machine(64, 10).budget(),
            8 * GB,
            "four fifths of what is free"
        );
    }

    #[test]
    fn what_the_viewer_holds_comes_out_of_the_room() {
        assert_eq!(room(machine(16, 12), 2 * GB), 6 * GB);
        assert_eq!(room(machine(2, 2), 5 * GB), 0, "never below nothing");
    }

    #[test]
    fn this_machine_answers() {
        let memory = Memory::now();
        assert!(memory.total > GB, "{memory:?}");
    }
}
