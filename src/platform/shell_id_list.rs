//! Owned Shell identity lists for virtual items that cannot be re-parsed.

use std::path::{Path, PathBuf};

const PREFIX: &str = "shell:pidl:";
const MAX_BYTES: usize = 65_536;

pub(crate) fn encode(bytes: &[u8]) -> PathBuf {
    use std::fmt::Write;
    let mut target = String::from(PREFIX);
    for byte in bytes {
        write!(target, "{byte:02x}").expect("writing to String cannot fail");
    }
    PathBuf::from(target)
}

/// Validate every SHITEMID length and the final terminator before passing a
/// cached identity list to Windows. Return aligned storage owned by the caller.
pub(crate) fn decode(target: &Path) -> Option<Vec<u16>> {
    let text = target.to_str()?.strip_prefix(PREFIX)?;
    if text.len() < 4 || text.len() > MAX_BYTES * 2 || !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |c: u8| char::from(c).to_digit(16).map(|n| n as u8);
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut offset = 0;
    loop {
        let size = u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if size == 0 {
            if offset + 2 != bytes.len() || offset == 0 {
                return None;
            }
            break;
        }
        if size < 2 {
            return None;
        }
        offset += size;
    }
    Some(
        bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_reject_malformed_cached_identity_lists() {
        let bytes = [4, 0, 1, 2, 0, 0];
        assert_eq!(decode(&encode(&bytes)), Some(vec![4, 513, 0]));
        assert_eq!(decode(&encode(&[3, 0, 42, 0, 0])), Some(vec![3, 42, 0]));
        for text in [
            "shell:pidl:0000",
            "shell:pidl:01000000",
            "shell:pidl:04000102",
            "shell:pidl:ffff0000",
            "shell:pidl:zzzz0000",
        ] {
            assert!(decode(Path::new(text)).is_none(), "{text}");
        }
    }
}
