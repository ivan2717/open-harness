//! Deterministic synthetic corpus shared by opt-in boundary evaluators.
use agent_runtime::{ModelMessage, ModelRequest};
use std::fmt::Write as _;

pub const OUTPUT_RESERVE: usize = 8192;
pub const CALIBRATION_ROWS: usize = 4096;
pub const FIXTURE_VERSION: &str = "ascii-fixed-records-v1";
const INSTRUCTIONS: &str = "Extract exact marker values from synthetic records. Return only the requested JSON object. Do not explain your answer.";

pub fn scaled_rows(anchor_rows: usize, anchor_tokens: usize, target_tokens: usize) -> usize {
    ((anchor_rows as u128 * target_tokens as u128) / anchor_tokens as u128).max(256) as usize
}

pub fn fixture_request(rows: usize) -> ModelRequest {
    let mut content = String::with_capacity(rows * 36 + 512);
    content.push_str("Read the synthetic records below. Return only a JSON object with keys start, middle, end containing the exact values of NEEDLE_START, NEEDLE_MIDDLE, NEEDLE_END. Ignore ordinary filler records.\nBEGIN SYNTHETIC RECORDS\n");
    for row in 0..rows {
        if row == rows / 100 {
            content.push_str("NEEDLE_START = MINT-R4\n");
        } else if row == rows / 2 {
            content.push_str("NEEDLE_MIDDLE = COBALT-P8\n");
        } else if row == rows - rows / 100 {
            content.push_str("NEEDLE_END = LARCH-V2\n");
        } else {
            // Fixed-width decimal IDs keep extrapolation stable as row count grows.
            writeln!(&mut content, "row{row:08} amber cedar slate.").unwrap();
        }
    }
    content.push_str(
        "END SYNTHETIC RECORDS\nReturn the three marker values as the requested JSON now.",
    );
    ModelRequest {
        instructions: INSTRUCTIONS.into(),
        messages: vec![ModelMessage::user(content)],
        tools: Vec::new(),
        max_output_tokens: Some(OUTPUT_RESERVE),
        provider_context: None,
    }
}
