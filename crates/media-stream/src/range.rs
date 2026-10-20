#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeError {
    Malformed,
    Multiple,
    Unsatisfiable,
}

/// Resolves one HTTP byte range to a checked half-open interval.
/// Only outer HTTP optional whitespace (SP / HTAB) is accepted.
pub fn resolve_range(header: Option<&str>, size: u64) -> Result<Option<ByteRange>, RangeError> {
    let Some(header) = header else {
        return Ok(None);
    };
    let value = header
        .trim_matches([' ', '\t'])
        .strip_prefix("bytes=")
        .ok_or(RangeError::Malformed)?;
    if value.contains(',') {
        return Err(RangeError::Multiple);
    }
    let (first, last) = value.split_once('-').ok_or(RangeError::Malformed)?;
    let range = if first.is_empty() {
        let suffix = number(last)?;
        if suffix == 0 || size == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let len = suffix.min(size);
        ByteRange {
            start: size - len,
            len,
        }
    } else {
        let start = number(first)?;
        // Parse syntax even when a range is beyond EOF or media is empty.
        let end = if last.is_empty() {
            None
        } else {
            Some(number(last)?)
        };
        if start >= size || end.is_some_and(|end| end < start) {
            return Err(RangeError::Unsatisfiable);
        }
        let end = end.unwrap_or(size - 1).min(size - 1);
        let len = end
            .checked_sub(start)
            .and_then(|n| n.checked_add(1))
            .ok_or(RangeError::Unsatisfiable)?;
        ByteRange { start, len }
    };
    range
        .start
        .checked_add(range.len)
        .filter(|&end| end <= size)
        .ok_or(RangeError::Unsatisfiable)?;
    Ok(Some(range))
}

fn number(input: &str) -> Result<u64, RangeError> {
    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RangeError::Malformed);
    }
    input.parse().map_err(|_| RangeError::Malformed)
}

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed byte range",
            Self::Multiple => "multiple byte ranges are unsupported",
            Self::Unsatisfiable => "unsatisfiable byte range",
        })
    }
}
impl std::error::Error for RangeError {}
