//! Byte offset → 1-based line number lookup.

use ruff_text_size::TextSize;

/// Start offsets of every line, for binary-search line lookup.
pub struct LineIndex {
    starts: Vec<TextSize>,
}

impl LineIndex {
    /// Index `source`, treating `\n`, `\r\n`, and a lone `\r` as line ends.
    #[must_use]
    pub fn new(source: &str) -> Self {
        let mut starts = vec![TextSize::default()];
        let bytes = source.as_bytes();
        for (index, &byte) in bytes.iter().enumerate() {
            let ends_line =
                byte == b'\n' || (byte == b'\r' && bytes.get(index + 1) != Some(&b'\n'));
            if ends_line && let Ok(next) = u32::try_from(index + 1) {
                starts.push(TextSize::new(next));
            }
        }
        Self { starts }
    }

    /// 1-based line containing `offset`.
    #[must_use]
    pub fn line(&self, offset: TextSize) -> u32 {
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
}
