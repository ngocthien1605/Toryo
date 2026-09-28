//! Plan §16 `redaction_split_boundaries` (C9 / R42), end to end on the ported
//! `StreamRedactor`: multibyte and overlapping secrets, literal and JSON-escaped, split at
//! every byte offset; a secret across the 64 KB truncation boundary; a child terminated
//! mid-stream. The emitted stream never contains a secret and is always valid UTF-8.

use toryo::process::redact::{truncate_utf8, Redactions, Secret, StreamRedactor};

fn secrets() -> Vec<Secret> {
    [
        ("MB", "пароль-秘密-Ω🔑"),                // multibyte
        ("A", "tok-AAAA"),                       // overlaps B
        ("B", "AAAA-SECRETPART"),
        ("Q", "q\"uo\\te-7"),                    // differs when JSON-escaped
    ]
    .iter()
    .map(|(n, v)| Secret {
        name: n.to_string(),
        value: v.to_string(),
    })
    .collect()
}

/// Every form that must never appear: literal and JSON-escaped.
fn forbidden() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for s in secrets() {
        v.push(s.value.as_bytes().to_vec());
        let e = serde_json::to_string(&s.value).unwrap();
        v.push(e.as_bytes()[1..e.len() - 1].to_vec());
    }
    v
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn assert_clean(out: &[u8], ctx: &str) {
    assert!(std::str::from_utf8(out).is_ok(), "not UTF-8: {ctx}");
    for f in forbidden() {
        assert!(!contains(out, &f), "leaked {:?} ({ctx})", String::from_utf8_lossy(&f));
    }
}

fn sample() -> Vec<u8> {
    let json_line = serde_json::to_string(&serde_json::json!({
        "type": "tool_result",
        "content": format!("env: KEY={} and {}", "пароль-秘密-Ω🔑", "q\"uo\\te-7"),
    }))
    .unwrap();
    format!(
        "あい tok-AAAA-SECRETPART う AAAA-SECRETPART え tok-AAAA お пароль-秘密-Ω🔑 か q\"uo\\te-7\n{json_line}\n"
    )
    .into_bytes()
}

fn stream(r: &Redactions, chunks: &[&[u8]]) -> Vec<u8> {
    let mut s = StreamRedactor::new(r.clone());
    let mut out = Vec::new();
    for c in chunks {
        let e = s.push(c);
        // what has been emitted so far is already valid UTF-8 (a line reader may print it)
        assert!(std::str::from_utf8(&e).is_ok(), "partial emission not UTF-8");
        out.extend(e);
    }
    out.extend(s.finish());
    out
}

#[test]
fn redaction_split_boundaries() {
    let r = Redactions::new(&secrets());
    let data = sample();
    let reference = r.redact_all(&data);
    assert_clean(&reference, "whole buffer");

    // 1-byte chunks
    let ones: Vec<&[u8]> = data.chunks(1).collect();
    assert_eq!(stream(&r, &ones), reference, "1-byte chunks");

    // every 2-chunk split
    for i in 0..=data.len() {
        let out = stream(&r, &[&data[..i], &data[i..]]);
        assert_eq!(out, reference, "split at {i}");
    }

    // a secret straddling the 64 KB diagnostics cap: truncation happens after redaction,
    // so no prefix of the secret survives
    const CAP: usize = 64 * 1024;
    for off in [3usize, 7, 12] {
        let mut big = vec![b'x'; CAP - off];
        big.extend_from_slice("пароль-秘密-Ω🔑 tail".as_bytes());
        let out = stream(&r, &[&big[..CAP / 2], &big[CAP / 2..]]);
        let cut = truncate_utf8(&out, CAP);
        assert_clean(cut, "64 KB cut");
        assert!(!contains(cut, "пароль".as_bytes()), "secret prefix at the cap (offset {off})");
    }
}

/// The child is terminated (stall limit) after printing a secret and part of another: the
/// captured stream holds no secret and is valid UTF-8.
#[cfg(unix)]
#[test]
fn redaction_when_child_is_terminated_mid_stream() {
    use std::collections::BTreeMap;
    use std::time::Duration;
    use toryo::process::env::build_env;
    use toryo::process::{spawn, Boundary, Chunk, SpawnSpec, WaitOptions};

    let r = Redactions::new(&secrets());
    let spec = SpawnSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "printf 'a tok-AA'; printf 'AA-SECRET'; printf 'PART b пароль-秘'; sleep 30".into(),
        ],
        cwd: std::env::temp_dir(),
        env: build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
        stdin: None,
        boundary: Boundary::None,
    };
    let child = spawn(&spec, &r).unwrap();
    let mut out = Vec::new();
    let info = child
        .wait(
            WaitOptions {
                stall: Some(Duration::from_millis(500)),
                ..Default::default()
            },
            |c| {
                let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
                out.extend(b)
            },
        )
        .unwrap();
    assert!(info.stalled, "{info:?}");
    assert_clean(&out, "terminated child");
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("a [REDACTED"), "{s}");
    assert!(!s.contains("SECRETPART"), "{s}");
}
