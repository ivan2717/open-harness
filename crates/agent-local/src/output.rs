use crate::paths::{OUTPUT_LIMIT, bounded};
use std::collections::VecDeque;

/// Drain child pipes completely, but retain a bounded beginning and ending.
/// The ending usually contains test failures and summaries.
#[derive(Default)]
pub(crate) struct CommandOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}
impl CommandOutput {
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        const HALF: usize = OUTPUT_LIMIT / 2 - 64;
        self.total = self.total.saturating_add(bytes.len());
        let head_count = bytes.len().min(HALF.saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..head_count]);
        for byte in &bytes[head_count..] {
            if self.tail.len() == HALF {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }

    pub fn text(&self) -> String {
        let mut bytes = self.head.clone();
        if self.total > self.head.len() + self.tail.len() {
            bytes.extend_from_slice(b"\n[output truncated; beginning and ending retained]\n");
        }
        bytes.extend(self.tail.iter());
        bounded(&String::from_utf8_lossy(&bytes), OUTPUT_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_failure_summary_after_large_output() {
        let mut output = CommandOutput::default();
        output.extend_from_slice(b"START\n");
        output.extend_from_slice(&vec![b'x'; OUTPUT_LIMIT * 4]);
        output.extend_from_slice(b"\nFAILURE SUMMARY");
        let text = output.text();
        assert!(text.starts_with("START\n"));
        assert!(text.contains("output truncated"));
        assert!(text.ends_with("FAILURE SUMMARY"));
        assert!(text.len() <= OUTPUT_LIMIT);
    }
}
