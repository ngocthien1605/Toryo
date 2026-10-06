//! M12-15 (X23): what a tool printed, as text. A tool on a Japanese Windows prints its
//! diagnostics in the OEM code page (cp932), not UTF-8; reading those bytes lossily gives
//! `C:\���\�\�[�X\…` — and the trail bytes of 能 and ソ (`0x5C`) look like path separators.
//!
//! Line by line: a line that is valid UTF-8 stays as it is; any other line is decoded with the
//! OEM code page (`GetOEMCP`) on Windows, lossily elsewhere. Order everywhere: decode → strip
//! the colour codes → redact.

/// `bytes` as text (see the module documentation).
pub fn decode_output(bytes: &[u8]) -> String {
    decode_output_with(bytes, oem_code_page())
}

/// [`decode_output`] with the code page given — for the tests; `code_page` is ignored off
/// Windows (and when it is UTF-8, 65001: nothing else to try).
pub fn decode_output_with(bytes: &[u8], code_page: u32) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        match std::str::from_utf8(line) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push_str(&decode_line(line, code_page)),
        }
    }
    out
}

/// The OEM code page of this machine (the console's: what the tools print in). 65001 off
/// Windows. `TORYO_OEM_CODE_PAGE` (tests only, M13-14) says another one: a test can then send
/// cp932 bytes through a gate on a machine whose code page is not 932.
pub fn oem_code_page() -> u32 {
    #[cfg(windows)]
    {
        if let Some(page) = std::env::var("TORYO_OEM_CODE_PAGE")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            return page;
        }
        // SAFETY: no arguments, no pointers
        unsafe { windows_sys::Win32::Globalization::GetOEMCP() }
    }
    #[cfg(not(windows))]
    {
        65001
    }
}

/// Whether Windows knows `code_page` (the data of 932 ships with every Windows, but a
/// test says so rather than assume).
#[cfg(windows)]
pub fn code_page_installed(code_page: u32) -> bool {
    // SAFETY: no pointers
    unsafe { windows_sys::Win32::Globalization::IsValidCodePage(code_page) != 0 }
}

#[cfg(windows)]
fn decode_line(line: &[u8], code_page: u32) -> String {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    if code_page == 65001 || line.is_empty() || line.len() > i32::MAX as usize {
        return String::from_utf8_lossy(line).into_owned();
    }
    let len = line.len() as i32;
    // SAFETY: `line` is `len` readable bytes; the output buffer is sized by the first call
    unsafe {
        let n = MultiByteToWideChar(code_page, 0, line.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(line).into_owned();
        }
        let mut wide = vec![0u16; n as usize];
        let m = MultiByteToWideChar(code_page, 0, line.as_ptr(), len, wide.as_mut_ptr(), n);
        if m <= 0 {
            return String::from_utf8_lossy(line).into_owned();
        }
        String::from_utf16_lossy(&wide[..m as usize])
    }
}

#[cfg(not(windows))]
fn decode_line(line: &[u8], _code_page: u32) -> String {
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const LINE: &str = "C:\\作業\\ソース\\能力表.xml(12,5): error XPP0001: x\r\n";

    /// [`LINE`] in cp932 (Shift-JIS): the trail bytes of ソ (0x83 0x5C) and 能 (0x94 0x5C)
    /// and 表 (0x95 0x5C) are `\`.
    fn cp932_line() -> Vec<u8> {
        let mut b = b"C:\\".to_vec();
        b.extend([0x8D, 0xEC, 0x8B, 0xC6]); // 作業
        b.push(b'\\');
        b.extend([0x83, 0x5C, 0x81, 0x5B, 0x83, 0x58]); // ソース
        b.push(b'\\');
        b.extend([0x94, 0x5C, 0x97, 0xCD, 0x95, 0x5C]); // 能力表
        b.extend(b".xml(12,5): error XPP0001: x\r\n");
        b
    }

    /// AC-3: valid UTF-8 comes back unchanged, whatever code page is asked for.
    #[test]
    fn utf8_is_unchanged() {
        let t = "C:\\作業\\ソース\\能力表.xml(12,5): error\r\nplain\n";
        assert_eq!(decode_output_with(t.as_bytes(), 932), t);
        assert_eq!(decode_output_with(b"", 932), "");
    }

    /// AC-1 (Windows): the cp932 bytes decoded with 932 give the text back. Where this
    /// Windows has no code page 932 the test says so and checks nothing.
    #[cfg(windows)]
    #[test]
    fn cp932_gives_the_text_back() {
        if !code_page_installed(932) {
            eprintln!("skipped: this Windows has no code page 932 installed");
            return;
        }
        assert_eq!(decode_output_with(&cp932_line(), 932), LINE);
    }

    /// AC-2 (Windows): a buffer that mixes UTF-8 lines and cp932 lines — each line is decoded
    /// on its own.
    #[cfg(windows)]
    #[test]
    fn utf8_and_cp932_lines_mix() {
        if !code_page_installed(932) {
            eprintln!("skipped: this Windows has no code page 932 installed");
            return;
        }
        let mut b = "first: 日本語 in UTF-8\n".as_bytes().to_vec();
        b.extend(cp932_line());
        b.extend("last line\n".as_bytes());
        let got = decode_output_with(&b, 932);
        assert_eq!(got, format!("first: 日本語 in UTF-8\n{LINE}last line\n"));
    }

    /// AC-1 (Windows): the X++ line parser finds the file in the decoded line — the path is a
    /// path again, the trail bytes of ソ / 能 / 表 are not separators.
    #[cfg(windows)]
    #[test]
    fn the_xpp_parser_finds_the_file_in_a_decoded_cp932_line() {
        if !code_page_installed(932) {
            eprintln!("skipped: this Windows has no code page 932 installed");
            return;
        }
        let text = decode_output_with(&cp932_line(), 932);
        let d = crate::engine::xpp::diagnostics(&text, std::path::Path::new("C:\\作業"));
        assert_eq!(d.errors_total, 1, "{text}");
        let e = &d.errors[0];
        assert_eq!(e.file.as_deref(), Some("ソース/能力表.xml"), "{e:?}");
        assert_eq!((e.line, e.column), (Some(12), Some(5)));
        assert_eq!(e.code.as_deref(), Some("XPP0001"));
    }

    /// Off Windows an invalid line is lossy, as before.
    #[cfg(not(windows))]
    #[test]
    fn elsewhere_invalid_bytes_are_lossy() {
        let got = decode_output_with(&cp932_line(), 932);
        assert!(
            got.contains('\u{fffd}') && got.ends_with("error XPP0001: x\r\n"),
            "{got}"
        );
    }
}
