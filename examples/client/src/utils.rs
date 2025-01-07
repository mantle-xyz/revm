use super::HintType;
use alloy_primitives::{hex, Bytes};
use anyhow::{anyhow, Result};

/// Parses a hint from a string.
///
/// Hints are of the format `<hint_type> <hint_data>`, where `<hint_type>` is a string that
/// represents the type of hint, and `<hint_data>` is the data associated with the hint
/// (bytes encoded as hex UTF-8).
pub(crate) fn parse_hint(s: &str) -> Result<(HintType, Bytes)> {
    let mut parts = s.split(' ').collect::<Vec<_>>();

    if parts.len() != 2 {
        anyhow::bail!("Invalid hint format: {}", s);
    }

    let hint_type = HintType::try_from(parts.remove(0))?;
    let hint_data = hex::decode(parts.remove(0)).map_err(|e| anyhow!(e))?.into();

    Ok((hint_type, hint_data))
}