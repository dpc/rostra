use crate::native::{MAX_BLOCK_OPENERS, MAX_INPUT_BYTES, NativeRenderError};

/// Guard the eager recursive block pass before Parser::new can run.
///
/// Audited against jotup 0.10.0 block::{TreeParser::parse_block,
/// TreeParser::parse_container, IdentifiedBlock::new}. Only quote, list
/// (including task/description), footnote and div blocks recurse. Each entry
/// consumes a distinct opener in the original source. Removing an outer prefix
/// exposes a position at a line boundary, after ASCII whitespace, or
/// immediately after a footnote's colon. We overcount candidates at all those
/// positions, including siblings and literal code. Tables do not recurse;
/// inline syntax nesting uses an explicit opener stack. The inline iterator's
/// recursive skips are bounded: placeholders always precede a nonempty
/// delimiter/Enter/quote, and a skipped empty math prefix precedes
/// Placeholder+Enter, not another arbitrary skip run. Re-audit these
/// assumptions when upgrading jotup.
pub(crate) fn check(source: &str) -> Result<(), NativeRenderError> {
    if source.len() > MAX_INPUT_BYTES {
        return Err(NativeRenderError::InputLimit);
    }
    let bytes = source.as_bytes();
    let mut openers = 0;
    let mut hashes = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        // Heading levels are converted to u16 inside the eager parser. Bound
        // runs conservatively before that conversion, even in literal text.
        hashes = if byte == b'#' { hashes + 1 } else { 0 };
        if hashes > usize::from(u16::MAX) {
            return Err(NativeRenderError::ComplexityLimit);
        }
        if i != 0 && !bytes[i - 1].is_ascii_whitespace() && bytes[i - 1] != b':' {
            continue;
        }
        let tail = &bytes[i..];
        let next = tail.get(1).copied();
        let space_or_end = next.is_none_or(|b| b.is_ascii_whitespace());
        let opener = match byte {
            b'>' => space_or_end,
            b'-' | b'*' | b'+' => next.is_none_or(|b| b == b' '),
            b':' => space_or_end || tail.starts_with(b":::"),
            // Count any footnote-looking prefix without scanning its label.
            b'[' => tail.starts_with(b"[^"),
            _ => ordered_list(tail),
        };
        openers += usize::from(opener);
        if openers > MAX_BLOCK_OPENERS {
            return Err(NativeRenderError::ComplexityLimit);
        }
    }
    Ok(())
}

fn ordered_list(mut bytes: &[u8]) -> bool {
    if bytes.first() == Some(&b'(') {
        bytes = &bytes[1..];
    }
    let n = bytes
        .iter()
        .take(19)
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if n == 0 {
        return false;
    }
    let number = &bytes[..n];
    let valid = number.iter().all(u8::is_ascii_digit)
        || n == 1
        || number.iter().all(|b| b"ivxlcdmIVXLCDM".contains(b));
    valid
        && matches!(bytes.get(n), Some(b'.' | b')'))
        && bytes.get(n + 1).is_none_or(u8::is_ascii_whitespace)
}
