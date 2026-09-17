//! P2: `Range: bytes=<start>-<end>` parsing, exactly the one shape H4's own brief names --
//! not the full RFC 9111 grammar (`bytes=<start>-`, `bytes=-<suffix-length>`, or a
//! multi-range list). Anything other than `bytes=<start>-<end>` (both bounds present, both
//! decimal, `start <= end`) is [`RangeHeader::Unparseable`], and [`crate::core::handle`]
//! treats that identically to no `Range` header at all: **served as `200`**, per RFC 9110
//! section 14.2's own rule that a server "MAY ignore the Range header field" on a range
//! request it cannot make sense of, rather than refusing the request outright -- a client
//! that sent a `Range` header this crate does not understand still gets the resource it
//! asked for, just not partially. A syntactically valid range that does not fit the
//! resource's actual length is a DIFFERENT case, [`RangeHeader::Unsatisfiable`], and that
//! one IS refused (`416`, counted) -- the difference this module exists to draw: "I could
//! not read what you meant" (ignored) vs. "I read exactly what you meant, and it does not
//! fit" (refused).

/// The result of [`parse`] against a resource of a known `len`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeHeader {
    /// No `Range` header was present at all.
    Absent,
    /// A `Range` header was present but was not the one shape this module parses -- ignored
    /// (see this module's own doc).
    Unparseable,
    /// `bytes=<start>-<end>`, syntactically valid, and `start` is within `[0, len)` (`end` is
    /// clamped to `len - 1` if it names a byte past the end -- RFC 9110's own rule: a range
    /// that starts inside the resource but names an end past it is still satisfiable, for
    /// exactly as much of the resource as exists).
    Satisfiable { start: usize, end_inclusive: usize },
    /// `bytes=<start>-<end>`, syntactically valid, but `start > end` or `start >= len` --
    /// nothing in `[0, len)` can satisfy it.
    Unsatisfiable,
}

/// Parses a raw `Range` header value against a resource of `len` bytes. `header` is exactly
/// the header's value (no `"Range: "` prefix).
pub fn parse(header: Option<&str>, len: usize) -> RangeHeader {
    let Some(header) = header else {
        return RangeHeader::Absent;
    };
    let Some(spec) = header.strip_prefix("bytes=") else {
        return RangeHeader::Unparseable;
    };
    let Some((start_s, end_s)) = spec.split_once('-') else {
        return RangeHeader::Unparseable;
    };
    let (Ok(start), Ok(end)) = (start_s.parse::<usize>(), end_s.parse::<usize>()) else {
        return RangeHeader::Unparseable;
    };
    if start > end || start >= len {
        return RangeHeader::Unsatisfiable;
    }
    RangeHeader::Satisfiable { start, end_inclusive: end.min(len.saturating_sub(1)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_header_is_absent() {
        assert_eq!(parse(None, 100), RangeHeader::Absent);
    }

    #[test]
    fn a_well_formed_range_within_bounds_is_satisfiable() {
        assert_eq!(parse(Some("bytes=0-9"), 100), RangeHeader::Satisfiable { start: 0, end_inclusive: 9 });
        assert_eq!(parse(Some("bytes=10-19"), 100), RangeHeader::Satisfiable { start: 10, end_inclusive: 19 });
    }

    #[test]
    fn an_end_past_the_resources_length_is_clamped_not_unsatisfiable() {
        assert_eq!(parse(Some("bytes=90-999"), 100), RangeHeader::Satisfiable { start: 90, end_inclusive: 99 });
    }

    #[test]
    fn a_start_at_or_past_the_length_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=100-200"), 100), RangeHeader::Unsatisfiable);
        assert_eq!(parse(Some("bytes=500-600"), 100), RangeHeader::Unsatisfiable);
    }

    #[test]
    fn a_start_after_the_end_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=20-10"), 100), RangeHeader::Unsatisfiable);
    }

    #[test]
    fn missing_the_bytes_prefix_is_unparseable() {
        assert_eq!(parse(Some("0-9"), 100), RangeHeader::Unparseable);
    }

    #[test]
    fn an_open_ended_range_is_unparseable_this_crate_only_parses_start_dash_end() {
        assert_eq!(parse(Some("bytes=0-"), 100), RangeHeader::Unparseable);
        assert_eq!(parse(Some("bytes=-500"), 100), RangeHeader::Unparseable);
    }

    #[test]
    fn garbage_is_unparseable() {
        assert_eq!(parse(Some("not a range at all"), 100), RangeHeader::Unparseable);
        assert_eq!(parse(Some("bytes=abc-def"), 100), RangeHeader::Unparseable);
    }
}
