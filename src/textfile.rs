//! M12-10 (X22): the text files Toryo reads are UTF-8. A byte-order mark is ignored; anything
//! else that is not UTF-8 is refused with the file's name — never turned into `�` silently.
//! Used for `toryo.yaml`, the machine config, the org chart, role files, `TASK.md`, skills and
//! every context file.

use anyhow::{Context, Result};
use std::path::Path;

/// `bytes` of the file `path` as text: a leading UTF-8 byte-order mark is dropped; UTF-16 (a
/// mark, or NUL bytes) and any other invalid UTF-8 is an error naming the file.
pub fn decode(path: &Path, bytes: &[u8]) -> Result<String> {
    let utf16_mark = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    if utf16_mark || bytes.contains(&0) {
        anyhow::bail!(
            "{} is not UTF-8 (it looks like UTF-16): save it as UTF-8",
            path.display()
        );
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.strip_prefix('\u{feff}').unwrap_or(s).to_string()),
        Err(_) => anyhow::bail!(
            "{} is not UTF-8 (it looks like a code-page file, such as Shift-JIS or Windows-1252): save it as UTF-8",
            path.display()
        ),
    }
}

/// Read `path` as text (see [`decode`]).
pub fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_mark_is_ignored_and_other_encodings_are_refused_by_name() {
        let p = Path::new("dir/SKILL.md");
        assert_eq!(decode(p, b"\xEF\xBB\xBFhello").unwrap(), "hello");
        assert_eq!(decode(p, "héllo".as_bytes()).unwrap(), "héllo");
        let u16: Vec<u8> = [0xFF, 0xFE, b'#', 0, b' ', 0, b'T', 0].to_vec();
        let e = decode(p, &u16).unwrap_err().to_string();
        assert!(e.contains("SKILL.md") && e.contains("UTF-16"), "{e}");
        // UTF-16 without a mark: NUL bytes
        assert!(decode(p, &[b'#', 0, b' ', 0]).is_err());
        // Shift-JIS "日本語"
        let e = decode(p, &[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA])
            .unwrap_err()
            .to_string();
        assert!(e.contains("SKILL.md") && e.contains("code-page"), "{e}");
    }
}
