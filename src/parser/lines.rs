//! Byte offset → 1-based line number lookup.

use ruff_text_size::TextSize;

/// Start offsets of every line, for binary-search line lookup.
pub(crate) struct LineIndex {
    starts: Vec<TextSize>,
}

impl LineIndex {
    /// Index `source`, treating `\n`, `\r\n`, and a lone `\r` as line ends.
    #[must_use]
    pub(crate) fn new(source: &str) -> Self {
        let mut starts = vec![TextSize::default()];
        let bytes = source.as_bytes();
        // Every file is indexed once per parse; memchr skips line bodies
        // instead of testing each byte (#761).
        for index in memchr::memchr2_iter(b'\n', b'\r', bytes) {
            let crlf = bytes.get(index) == Some(&b'\r') && bytes.get(index + 1) == Some(&b'\n');
            if !crlf && let Ok(next) = u32::try_from(index + 1) {
                starts.push(TextSize::new(next));
            }
        }
        Self { starts }
    }

    /// 1-based line containing `offset`.
    #[must_use]
    pub(crate) fn line(&self, offset: TextSize) -> u32 {
        let line = self.starts.partition_point(|&start| start <= offset);
        u32::try_from(line).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_every_line_ending_style() {
        let index = LineIndex::new("a\nb\r\nc\rd");
        let lines: Vec<u32> = [0, 2, 5, 7]
            .into_iter()
            .map(|offset| index.line(TextSize::new(offset)))
            .collect();
        assert_eq!(lines, vec![1, 2, 3, 4]);
    }

    #[test]
    fn line_starts_follow_runs_of_mixed_endings() {
        let index = LineIndex::new("\r\r\n\n\u{e9}\r\n\r");
        let starts: Vec<u32> = index.starts.iter().map(|&start| start.to_u32()).collect();
        assert_eq!(starts, vec![0, 1, 3, 4, 8, 9]);
    }
}
