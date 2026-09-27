//! Minimal pass-through long-running filter (`filter.<driver>.process`, pkt-line protocol v2).
//!
//! Used by the R37 canary as the *trusted* `lfs` process command: it proves that a
//! trusted process driver installed through `GitCmd`'s `-c` overrides replaces the
//! repository's own command. It writes one marker line per request and returns the
//! content unchanged.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Result};

const MAX_DATA: usize = 65516;

enum Pkt {
    Flush,
    Data(Vec<u8>),
}

fn read_pkt(r: &mut impl Read) -> Result<Option<Pkt>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = usize::from_str_radix(std::str::from_utf8(&len)?, 16)?;
    if n == 0 {
        return Ok(Some(Pkt::Flush));
    }
    if n < 4 {
        bail!("bad pkt-line length {n}");
    }
    let mut data = vec![0u8; n - 4];
    r.read_exact(&mut data)?;
    Ok(Some(Pkt::Data(data)))
}

fn write_pkt(w: &mut impl Write, data: &[u8]) -> Result<()> {
    write!(w, "{:04x}", data.len() + 4)?;
    w.write_all(data)?;
    Ok(())
}

fn flush(w: &mut impl Write) -> Result<()> {
    w.write_all(b"0000")?;
    w.flush()?;
    Ok(())
}

fn read_text_until_flush(r: &mut impl Read) -> Result<Option<Vec<String>>> {
    let mut v = Vec::new();
    loop {
        match read_pkt(r)? {
            None => return Ok(if v.is_empty() { None } else { Some(v) }),
            Some(Pkt::Flush) => return Ok(Some(v)),
            Some(Pkt::Data(d)) => v.push(
                String::from_utf8_lossy(&d)
                    .trim_end_matches('\n')
                    .to_string(),
            ),
        }
    }
}

fn read_content_until_flush(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    loop {
        match read_pkt(r)? {
            None | Some(Pkt::Flush) => return Ok(v),
            Some(Pkt::Data(d)) => v.extend(d),
        }
    }
}

/// Serve the protocol until EOF. `marker` gets one line per command.
pub fn serve(r: &mut impl Read, w: &mut impl Write, marker: &Path) -> Result<()> {
    let hello = read_text_until_flush(r)?.unwrap_or_default();
    if !hello.iter().any(|l| l == "git-filter-client") || !hello.iter().any(|l| l == "version=2") {
        bail!("unexpected handshake {hello:?}");
    }
    write_pkt(w, b"git-filter-server\n")?;
    write_pkt(w, b"version=2\n")?;
    flush(w)?;
    let caps = read_text_until_flush(r)?.unwrap_or_default();
    for c in ["clean", "smudge"] {
        if caps.iter().any(|l| l == &format!("capability={c}")) {
            write_pkt(w, format!("capability={c}\n").as_bytes())?;
        }
    }
    flush(w)?;
    loop {
        let Some(meta) = read_text_until_flush(r)? else {
            return Ok(());
        };
        let cmd = meta
            .iter()
            .find_map(|l| l.strip_prefix("command="))
            .unwrap_or("?")
            .to_string();
        let content = read_content_until_flush(r)?;
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(marker)?;
            writeln!(f, "{cmd}")?;
        }
        write_pkt(w, b"status=success\n")?;
        flush(w)?;
        for chunk in content.chunks(MAX_DATA) {
            write_pkt(w, chunk)?;
        }
        flush(w)?;
        flush(w)?; // empty list = keep status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(s: &[u8]) -> Vec<u8> {
        let mut v = format!("{:04x}", s.len() + 4).into_bytes();
        v.extend_from_slice(s);
        v
    }

    #[test]
    fn roundtrip_clean_request() {
        let mut input = Vec::new();
        input.extend(pkt(b"git-filter-client\n"));
        input.extend(pkt(b"version=2\n"));
        input.extend(b"0000");
        input.extend(pkt(b"capability=clean\n"));
        input.extend(pkt(b"capability=smudge\n"));
        input.extend(b"0000");
        input.extend(pkt(b"command=clean\n"));
        input.extend(pkt(b"pathname=a.bin\n"));
        input.extend(b"0000");
        input.extend(pkt(b"\x00\x01binary"));
        input.extend(b"0000");
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("m");
        let mut out = Vec::new();
        serve(&mut input.as_slice(), &mut out, &marker).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("git-filter-server"));
        assert!(s.contains("capability=clean"));
        assert!(out.windows(8).any(|w| w == b"\x00\x01binary"));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "clean\n");
    }
}
