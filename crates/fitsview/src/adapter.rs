//! Choosing which graphics adapter to draw with.
//!
//! The default choice is "the most powerful one", which is right until the most
//! powerful one cannot draw to the window. Over Remote Desktop on Windows that
//! is exactly what happens: the real card enumerates and reports itself, but it
//! cannot present to a remote session, while Microsoft's software renderer can.
//! Asked for the most powerful adapter and given no alternative, the
//! application found nothing it could use and exited without opening a window.
//!
//! So presenting comes first and speed second. Software rendering is slow, and
//! slow is worth having when the alternative is nothing at all.

/// What kind of device an adapter is, in the order we would rather have them.
///
/// Mirrors `wgpu::DeviceType` so this can be reasoned about and tested without
/// a graphics device to hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Anything the driver would not name.
    Other,
    /// A software rasteriser: Microsoft's Basic Render Driver, or lavapipe.
    Cpu,
    /// A GPU belonging to a virtual machine's host.
    VirtualGpu,
    /// A GPU sharing memory with the processor.
    IntegratedGpu,
    /// A GPU of its own.
    DiscreteGpu,
}

/// One adapter, reduced to what the choice depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// What kind of device it is.
    pub kind: Kind,
    /// Whether it can actually draw to this window.
    pub presents: bool,
}

/// Picks an adapter, returning its position in the list.
///
/// An adapter that cannot present is no use whatever its speed, so those are
/// excluded before anything else is considered. Among the rest the most capable
/// wins, and ties go to the earlier one, which keeps the choice stable from one
/// run to the next.
///
/// Returns `None` only when nothing can present, which is worth reporting
/// rather than papering over: at that point there is no way to draw at all.
#[must_use]
pub fn choose(candidates: &[Candidate]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.presents)
        .max_by_key(|(index, c)| (c.kind, std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(kind: Kind, presents: bool) -> Candidate {
        Candidate { kind, presents }
    }

    #[test]
    fn the_fastest_adapter_wins_when_they_can_all_draw() {
        let list = [
            candidate(Kind::Cpu, true),
            candidate(Kind::IntegratedGpu, true),
            candidate(Kind::DiscreteGpu, true),
        ];
        assert_eq!(choose(&list), Some(2));
    }

    #[test]
    fn an_adapter_that_cannot_draw_is_skipped_however_fast_it_is() {
        // Remote Desktop on Windows: the real card enumerates but cannot
        // present to the session, and only the software renderer can. Choosing
        // by speed alone found nothing usable and the window never opened.
        let list = [
            candidate(Kind::IntegratedGpu, false),
            candidate(Kind::Cpu, true),
        ];
        assert_eq!(
            choose(&list),
            Some(1),
            "software rendering beats no window at all"
        );
    }

    #[test]
    fn nothing_is_chosen_when_nothing_can_draw() {
        let list = [
            candidate(Kind::DiscreteGpu, false),
            candidate(Kind::Cpu, false),
        ];
        assert_eq!(choose(&list), None, "this is worth reporting, not hiding");
    }

    #[test]
    fn no_adapters_at_all_chooses_nothing() {
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn the_earlier_adapter_wins_a_tie() {
        // Two of the same kind: the answer must not change between runs.
        let list = [
            candidate(Kind::DiscreteGpu, true),
            candidate(Kind::DiscreteGpu, true),
        ];
        assert_eq!(choose(&list), Some(0));
        assert_eq!(choose(&list), Some(0));
    }

    #[test]
    fn a_real_gpu_is_preferred_to_a_virtual_one_and_that_to_software() {
        let list = [
            candidate(Kind::Cpu, true),
            candidate(Kind::VirtualGpu, true),
            candidate(Kind::IntegratedGpu, true),
        ];
        assert_eq!(choose(&list), Some(2));

        let without_real = [
            candidate(Kind::Cpu, true),
            candidate(Kind::VirtualGpu, true),
        ];
        assert_eq!(choose(&without_real), Some(1));
    }
}
