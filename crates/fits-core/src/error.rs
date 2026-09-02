//! Error type for everything this crate can fail at.

/// Every way reading or writing a FITS file can fail.
///
/// All variants carry enough context to show the user something actionable
/// without them needing to read a log.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum FitsError {
    /// The file does not begin with the `SIMPLE` keyword.
    #[error("not a FITS file: it does not start with SIMPLE")]
    NotFits,

    /// `BITPIX` was absent, unparseable, or not one of the six legal values.
    #[error("unsupported BITPIX {0} (expected 8, 16, 32, 64, -32 or -64)")]
    UnsupportedBitpix(i64),

    /// The file ends before the header or the data block does.
    #[error("truncated: expected {expected} bytes of {what}, found {found}")]
    Truncated {
        /// What was being read when the file ran out.
        what: &'static str,
        /// Bytes the header said should be present.
        expected: usize,
        /// Bytes actually available.
        found: usize,
    },

    /// The header is structurally wrong: no `END`, a missing required keyword,
    /// or a value that will not parse.
    #[error("bad header: {0}")]
    BadHeader(String),

    /// The image geometry is legal FITS but not something this viewer handles,
    /// such as a one-dimensional spectrum or a four-plane cube.
    #[error("unsupported image geometry: {0}")]
    UnsupportedGeometry(String),

    /// Dimensions from the header would overflow `usize` when multiplied out.
    /// Seen with corrupt files, and worth rejecting rather than allocating.
    #[error("image dimensions are implausibly large and overflowed: {0}")]
    DimensionOverflow(String),

    /// The underlying file could not be read or written.
    #[error("io error on {path}: {source}")]
    Io {
        /// The file being read or written.
        path: std::path::PathBuf,
        /// The underlying operating system error.
        #[source]
        source: std::io::Error,
    },
}

impl FitsError {
    /// Attaches a path to an [`std::io::Error`], because a bare "file not
    /// found" with no filename is useless in a viewer that opens many files.
    pub(crate) fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        FitsError::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_problem() {
        assert!(FitsError::NotFits.to_string().contains("SIMPLE"));
        assert!(FitsError::UnsupportedBitpix(7)
            .to_string()
            .contains("BITPIX 7"));
        let t = FitsError::Truncated {
            what: "data",
            expected: 100,
            found: 40,
        };
        let msg = t.to_string();
        assert!(msg.contains("100") && msg.contains("40") && msg.contains("data"));
    }

    #[test]
    fn io_errors_carry_the_path() {
        let e = FitsError::io(
            "/tmp/example.fits",
            std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        );
        assert!(e.to_string().contains("example.fits"));
    }
}
