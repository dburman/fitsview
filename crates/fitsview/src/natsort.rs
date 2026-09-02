//! Natural ordering for file names.
//!
//! Capture programs number frames without padding, so a plain byte-wise sort
//! puts `light_10` before `light_2` and the list is useless for stepping
//! through a session in order. This compares runs of digits by value instead.
//!
//! Written here rather than pulled from a crate because the behaviour that
//! matters is specific: case-insensitive, stable for names differing only in
//! case or leading zeros, and safe for digit runs longer than any integer type.

use std::cmp::Ordering;

/// Compares two names in natural order.
///
/// Digit runs compare by numeric value, everything else compares
/// case-insensitively. Ties are broken by the ordinary byte comparison so the
/// result is a total order and sorting is deterministic.
///
/// ```
/// # use fitsview::natsort::natural_cmp;
/// # use std::cmp::Ordering;
/// assert_eq!(natural_cmp("light_2.fits", "light_10.fits"), Ordering::Less);
/// assert_eq!(natural_cmp("Light_1.fits", "light_1.fits"), Ordering::Less);
/// ```
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();

    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                if l.is_ascii_digit() && r.is_ascii_digit() {
                    let ln = take_digits(&mut left);
                    let rn = take_digits(&mut right);
                    match compare_number_runs(&ln, &rn) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                } else {
                    left.next();
                    right.next();
                    let lc = l.to_ascii_lowercase();
                    let rc = r.to_ascii_lowercase();
                    match lc.cmp(&rc) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                }
            }
        }
    }

    // Everything compared equal case-insensitively. Fall back to the byte
    // ordering so that names differing only in case still have a stable order.
    a.cmp(b)
}

/// Consumes a run of digits from the iterator.
fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut out = String::new();
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            out.push(c);
            it.next();
        } else {
            break;
        }
    }
    out
}

/// Compares two digit runs by value, without parsing them into an integer.
///
/// Parsing would overflow on a long run of digits, which is exactly the sort of
/// input a corrupt or machine-generated name provides. After stripping leading
/// zeros, the longer run is the larger number, and equal lengths compare
/// lexically.
fn compare_number_runs(a: &str, b: &str) -> Ordering {
    let sa = a.trim_start_matches('0');
    let sb = b.trim_start_matches('0');
    match sa.len().cmp(&sb.len()) {
        Ordering::Equal => sa.cmp(sb),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorts with the natural comparator and returns the order.
    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn numbers_order_by_value_not_by_text() {
        // The whole reason this module exists.
        assert_eq!(
            sorted(&["light_10.fits", "light_2.fits", "light_1.fits"]),
            vec!["light_1.fits", "light_2.fits", "light_10.fits"]
        );
    }

    #[test]
    fn a_long_capture_session_sorts_correctly() {
        let names: Vec<String> = (1..=120).map(|i| format!("light_{i}.fits")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut shuffled = refs.clone();
        shuffled.reverse();
        let got = sorted(&shuffled);
        assert_eq!(got, names);
    }

    #[test]
    fn leading_zeros_do_not_change_the_value() {
        assert_eq!(natural_cmp("f_007.fits", "f_7.fits"), Ordering::Less);
        assert_eq!(natural_cmp("f_7.fits", "f_08.fits"), Ordering::Less);
        // Same value, so the tie-break decides, and it must be consistent.
        let a = natural_cmp("f_007.fits", "f_7.fits");
        let b = natural_cmp("f_7.fits", "f_007.fits");
        assert_eq!(a.reverse(), b, "comparator must be antisymmetric");
    }

    #[test]
    fn comparison_is_case_insensitive_but_still_a_total_order() {
        assert_eq!(natural_cmp("Light_1.fits", "light_2.fits"), Ordering::Less);
        assert_eq!(natural_cmp("LIGHT_2.fits", "light_10.fits"), Ordering::Less);
        // Differing only in case: ordered, but never reported equal.
        assert_ne!(natural_cmp("Light.fits", "light.fits"), Ordering::Equal);
    }

    #[test]
    fn multiple_number_runs_are_compared_in_turn() {
        assert_eq!(
            sorted(&["s2_f10.fits", "s10_f1.fits", "s2_f2.fits"]),
            vec!["s2_f2.fits", "s2_f10.fits", "s10_f1.fits"]
        );
    }

    #[test]
    fn absurdly_long_digit_runs_do_not_overflow() {
        // Parsing these into an integer would fail or wrap.
        let small = format!("f_{}.fits", "9".repeat(40));
        let large = format!("f_{}.fits", "9".repeat(41));
        assert_eq!(natural_cmp(&small, &large), Ordering::Less);

        let zeros = format!("f_{}1.fits", "0".repeat(60));
        assert_eq!(natural_cmp(&zeros, "f_2.fits"), Ordering::Less);
    }

    #[test]
    fn names_without_numbers_sort_alphabetically() {
        assert_eq!(
            sorted(&["zebra.fits", "apple.fits", "Mango.fits"]),
            vec!["apple.fits", "Mango.fits", "zebra.fits"]
        );
    }

    #[test]
    fn a_prefix_sorts_before_the_longer_name() {
        assert_eq!(natural_cmp("light", "light_1"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn the_comparator_is_a_valid_total_order() {
        // Sorting panics in debug builds if the comparator is inconsistent, so
        // check the three laws directly on a set chosen to stress the
        // tie-breaking paths.
        let names = [
            "light_1.fits",
            "Light_1.fits",
            "light_01.fits",
            "light_2.fits",
            "light_10.fits",
            "dark_1.fits",
            "a",
            "",
            "f_000.fits",
            "f_0.fits",
        ];
        for a in names {
            assert_eq!(natural_cmp(a, a), Ordering::Equal, "not reflexive: {a}");
            for b in names {
                assert_eq!(
                    natural_cmp(a, b).reverse(),
                    natural_cmp(b, a),
                    "not antisymmetric: {a} vs {b}"
                );
                for c in names {
                    let (ab, bc, ac) = (natural_cmp(a, b), natural_cmp(b, c), natural_cmp(a, c));
                    if ab == Ordering::Less && bc == Ordering::Less {
                        assert_eq!(ac, Ordering::Less, "not transitive: {a} {b} {c}");
                    }
                }
            }
        }
    }

    #[test]
    fn unicode_names_do_not_panic() {
        for (a, b) in [("café.fits", "cafe.fits"), ("日本_1.fits", "日本_2.fits")] {
            let _ = natural_cmp(a, b);
            let _ = natural_cmp(b, a);
        }
    }
}
