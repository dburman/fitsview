//! FITS header parsing.
//!
//! A FITS header is a run of 80-byte ASCII "cards", 36 to a 2880-byte block,
//! ending at a card whose keyword is `END`. The header is then padded with
//! blanks to a whole number of blocks, and the data begins at the next block
//! boundary.
//!
//! A value card looks like this, with column numbers counted from zero:
//!
//! ```text
//! NAXIS1  =                 6000 / length of data axis 1
//! ^^^^^^^^  keyword, bytes 0..8
//!         ^^ value indicator, bytes 8..10, exactly "= "
//!           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ value and comment
//! ```
//!
//! The only genuinely fiddly part is that a `/` inside a quoted string is not
//! a comment separator, and that a quote inside a quoted string is written as
//! two quotes.

use crate::error::FitsError;
use crate::{BLOCK_SIZE, CARD_SIZE};

/// A parsed FITS header: the cards in the order they appeared.
///
/// Order is preserved because the header viewer shows it, and because writing a
/// calibrated file copies cards through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FitsHeader {
    /// `(keyword, value)` pairs. Comment-only cards are not stored.
    pub cards: Vec<(String, String)>,
}

impl FitsHeader {
    /// The value of the first card with this keyword, if any.
    ///
    /// Keywords are compared case-insensitively; the standard says they are
    /// upper case, but files in the wild are not always careful.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.cards
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// The value of `key` parsed as an integer.
    ///
    /// Returns `None` if the keyword is absent or does not parse. A value
    /// written as a float, such as `BZERO = 32768.0`, parses as an integer if
    /// it has no fractional part, because cameras write both forms.
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        let raw = self.get(key)?.trim();
        if let Ok(v) = raw.parse::<i64>() {
            return Some(v);
        }
        let f = raw.parse::<f64>().ok()?;
        if f.fract() == 0.0 && f.is_finite() && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
            #[allow(clippy::cast_possible_truncation)]
            Some(f as i64)
        } else {
            None
        }
    }

    /// The value of `key` parsed as a float.
    ///
    /// FITS permits Fortran-style exponents (`1.0D3`), which Rust will not
    /// parse, so `D` and `d` are normalised to `E` first.
    #[must_use]
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        let raw = self.get(key)?.trim();
        let normalised = raw.replace(['D', 'd'], "E");
        normalised.parse::<f64>().ok()
    }

    /// The value of `key` as a boolean. FITS writes these as bare `T` or `F`.
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)?.trim() {
            "T" | "t" => Some(true),
            "F" | "f" => Some(false),
            _ => None,
        }
    }
}

/// Where a header ended and its data begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedHeader {
    /// Byte offset of the first data byte, already aligned to a block boundary.
    pub data_start: usize,
}

/// Parses one header starting at `offset`.
///
/// Returns the header and the offset at which its data block begins. Stops at
/// the `END` card. Cards without a value indicator, such as `COMMENT` and
/// `HISTORY`, are skipped rather than stored, because nothing reads them and
/// keeping them complicates writing files back out.
///
/// # Errors
///
/// Returns [`FitsError::Truncated`] if the file ends before an `END` card, and
/// [`FitsError::BadHeader`] if a block is not a whole number of cards.
pub fn parse_at(bytes: &[u8], offset: usize) -> Result<(FitsHeader, ParsedHeader), FitsError> {
    if offset >= bytes.len() {
        return Err(FitsError::Truncated {
            what: "header",
            expected: offset + BLOCK_SIZE,
            found: bytes.len(),
        });
    }

    let mut header = FitsHeader::default();
    let mut pos = offset;

    loop {
        let block = bytes
            .get(pos..pos + BLOCK_SIZE)
            .ok_or(FitsError::Truncated {
                what: "header block",
                expected: pos + BLOCK_SIZE,
                found: bytes.len(),
            })?;

        for card in block.chunks_exact(CARD_SIZE) {
            let keyword = keyword_of(card);
            if keyword == "END" {
                let data_start = pos + BLOCK_SIZE;
                return Ok((header, ParsedHeader { data_start }));
            }
            if keyword.is_empty() {
                continue;
            }
            // Only cards with the "= " value indicator carry a value.
            if &card[8..10] != b"= " {
                continue;
            }
            let value = parse_value(&card[10..CARD_SIZE]);
            header.cards.push((keyword, value));
        }

        pos += BLOCK_SIZE;
    }
}

/// The keyword of a card: bytes 0..8, trimmed, non-ASCII rejected.
fn keyword_of(card: &[u8]) -> String {
    card[..8]
        .iter()
        .copied()
        .filter(|b| b.is_ascii_graphic())
        .map(char::from)
        .collect()
}

/// Extracts the value from the part of a card after the `= ` indicator.
///
/// Handles quoted strings, including a `/` inside them and the doubled-quote
/// escape, then strips the trailing comment and surrounding whitespace.
fn parse_value(rest: &[u8]) -> String {
    let text: String = rest
        .iter()
        .copied()
        .map(|b| if b.is_ascii() { char::from(b) } else { '?' })
        .collect();
    let trimmed = text.trim_start();

    if let Some(body) = trimmed.strip_prefix('\'') {
        // Quoted string. Scan for the closing quote, treating '' as an escaped
        // quote rather than a terminator.
        let mut out = String::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    break; // closing quote
                }
            } else {
                out.push(c);
            }
        }
        // Trailing blanks inside a FITS string are not significant.
        out.trim_end().to_string()
    } else {
        // Unquoted: the value runs to the comment slash or the end of the card.
        let end = trimmed.find('/').unwrap_or(trimmed.len());
        trimmed[..end].trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_align;

    /// Builds a header block from card texts, padding each to 80 bytes and the
    /// whole thing to a 2880-byte block.
    fn header_bytes(cards: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in cards {
            let mut card = c.as_bytes().to_vec();
            assert!(card.len() <= CARD_SIZE, "card too long: {c}");
            card.resize(CARD_SIZE, b' ');
            out.extend_from_slice(&card);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        out.extend_from_slice(&end);
        let padded = block_align(out.len()).unwrap();
        out.resize(padded, b' ');
        out
    }

    #[test]
    fn parses_a_simple_header() {
        let bytes = header_bytes(&[
            "SIMPLE  =                    T / conforms to FITS standard",
            "BITPIX  =                   16 / bits per pixel",
            "NAXIS   =                    2",
            "NAXIS1  =                  100",
            "NAXIS2  =                   50",
        ]);
        let (h, p) = parse_at(&bytes, 0).unwrap();
        assert_eq!(p.data_start, BLOCK_SIZE);
        assert_eq!(h.get_bool("SIMPLE"), Some(true));
        assert_eq!(h.get_i64("BITPIX"), Some(16));
        assert_eq!(h.get_i64("NAXIS1"), Some(100));
        assert_eq!(h.get_i64("NAXIS2"), Some(50));
        assert_eq!(h.get("MISSING"), None);
    }

    #[test]
    fn keyword_lookup_is_case_insensitive() {
        let bytes = header_bytes(&["BITPIX  =                   16"]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get_i64("bitpix"), Some(16));
        assert_eq!(h.get_i64("BiTpIx"), Some(16));
    }

    #[test]
    fn a_slash_inside_a_quoted_string_is_not_a_comment() {
        let bytes = header_bytes(&["FILENAME= 'a/b/c.fits' / the original path"]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get("FILENAME"), Some("a/b/c.fits"));
    }

    #[test]
    fn doubled_quotes_inside_a_string_are_unescaped() {
        let bytes = header_bytes(&["OBJECT  = 'Barnard''s Star' / target"]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get("OBJECT"), Some("Barnard's Star"));
    }

    #[test]
    fn trailing_blanks_in_strings_are_dropped() {
        let bytes = header_bytes(&["TELESCOP= 'RC8     '"]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get("TELESCOP"), Some("RC8"));
    }

    #[test]
    fn comment_and_history_cards_are_skipped() {
        let bytes = header_bytes(&[
            "BITPIX  =                   16",
            "COMMENT this is not a value card",
            "HISTORY calibrated yesterday",
            "NAXIS   =                    2",
        ]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.cards.len(), 2);
        assert_eq!(h.get("COMMENT"), None);
        assert_eq!(h.get("HISTORY"), None);
    }

    #[test]
    fn integer_valued_floats_parse_as_integers() {
        // Cameras write BZERO both ways.
        let bytes = header_bytes(&[
            "BZERO   =              32768.0",
            "OTHER   =                  1.5",
        ]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get_i64("BZERO"), Some(32768));
        assert_eq!(h.get_f64("BZERO"), Some(32768.0));
        // 1.5 has a fractional part, so it is not an integer.
        assert_eq!(h.get_i64("OTHER"), None);
        assert_eq!(h.get_f64("OTHER"), Some(1.5));
    }

    #[test]
    fn fortran_style_exponents_parse() {
        let bytes = header_bytes(&["EXPTIME =              1.5D2"]);
        let (h, _) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.get_f64("EXPTIME"), Some(150.0));
    }

    #[test]
    fn headers_can_span_several_blocks() {
        // 40 cards forces a second block, since a block holds 36.
        let cards: Vec<String> = (0..40).map(|i| format!("KEY{i:<5}= {i:>20}")).collect();
        let refs: Vec<&str> = cards.iter().map(String::as_str).collect();
        let bytes = header_bytes(&refs);
        let (h, p) = parse_at(&bytes, 0).unwrap();
        assert_eq!(h.cards.len(), 40);
        assert_eq!(p.data_start, 2 * BLOCK_SIZE);
        assert_eq!(h.get_i64("KEY39"), Some(39));
    }

    #[test]
    fn a_header_with_no_end_card_is_truncated_not_a_panic() {
        let mut bytes = vec![b' '; BLOCK_SIZE];
        bytes[..6].copy_from_slice(b"SIMPLE");
        let err = parse_at(&bytes, 0).unwrap_err();
        assert!(matches!(err, FitsError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn an_empty_input_is_truncated_not_a_panic() {
        let err = parse_at(&[], 0).unwrap_err();
        assert!(matches!(err, FitsError::Truncated { .. }), "got {err:?}");
    }

    #[test]
    fn parsing_can_start_at_a_later_offset() {
        // Simulates a second header unit following a primary one.
        let first = header_bytes(&["SIMPLE  =                    T"]);
        let second = header_bytes(&["XTENSION= 'IMAGE   '"]);
        let mut bytes = first.clone();
        bytes.extend_from_slice(&second);
        let (h, p) = parse_at(&bytes, first.len()).unwrap();
        assert_eq!(h.get("XTENSION"), Some("IMAGE"));
        assert_eq!(p.data_start, first.len() + BLOCK_SIZE);
    }
}
