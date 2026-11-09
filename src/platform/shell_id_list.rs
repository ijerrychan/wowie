//! Owned Shell identity lists for virtual items that cannot be re-parsed.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use std::path::{Path, PathBuf};

const PREFIX: &str = "shell:pidl:";
const COMPACT_PREFIX: &str = "shell:pidl64:";
const MAX_BYTES: usize = 65_536;

pub(crate) fn encode(bytes: &[u8]) -> PathBuf {
    let mut target = String::with_capacity(COMPACT_PREFIX.len() + bytes.len().div_ceil(3) * 4);
    target.push_str(COMPACT_PREFIX);
    URL_SAFE_NO_PAD.encode_string(bytes, &mut target);
    PathBuf::from(target)
}

/// Validate every SHITEMID length and the final terminator before passing a
/// cached identity list to Windows. Return aligned storage owned by the caller.
pub(crate) fn decode(target: &Path) -> Option<Vec<u16>> {
    let target = target.to_str()?;
    let bytes = if let Some(text) = target.strip_prefix(COMPACT_PREFIX) {
        if text.len() > MAX_BYTES.div_ceil(3) * 4 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
        if bytes.len() > MAX_BYTES {
            return None;
        }
        bytes
    } else {
        // Existing favorites and old caches can still contain hexadecimal PIDLs.
        let text = target.strip_prefix(PREFIX)?;
        if text.len() < 4 || text.len() > MAX_BYTES * 2 || !text.len().is_multiple_of(2) {
            return None;
        }
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let digit = |c: u8| char::from(c).to_digit(16).map(|n| n as u8);
                Some(digit(pair[0])? * 16 + digit(pair[1])?)
            })
            .collect::<Option<Vec<_>>>()?
    };
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
        assert_eq!(
            decode(Path::new("shell:pidl:040001020000")),
            Some(vec![4, 513, 0])
        );
        assert_eq!(decode(&encode(&[3, 0, 42, 0, 0])), Some(vec![3, 42, 0]));
        for text in [
            "shell:pidl:0000",
            "shell:pidl:01000000",
            "shell:pidl:04000102",
            "shell:pidl:ffff0000",
            "shell:pidl:zzzz0000",
            "shell:pidl64:!invalid",
            "shell:pidl64:AAA",
        ] {
            assert!(decode(Path::new(text)).is_none(), "{text}");
        }
    }

    #[test]
    fn compact_identity_lists_reduce_long_target_storage() {
        let mut bytes = vec![42; 4096];
        bytes[..2].copy_from_slice(&4094u16.to_le_bytes());
        bytes[4094..].fill(0);
        let target = encode(&bytes);
        assert!(target.as_os_str().len() < PREFIX.len() + bytes.len() * 2);
        assert_eq!(decode(&target).unwrap().len(), 2048);
    }
}
