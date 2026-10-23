//! Streaming secret redaction (plan v0.6, R42 / review finding C9).
//!
//! Redacting each chunk on its own misses a secret that is split across two
//! chunks (`abc` + `def` never matches `abcdef`). [`StreamRedactor`] sits
//! directly on a child's pipe, keeps a carry-over suffix between chunks and only
//! emits bytes that can no longer be the start of a secret.
//!
//! Invariants (tested exhaustively in the unit tests below):
//! * every occurrence of a known secret, in literal or JSON-escaped form, is
//!   replaced by `[REDACTED:<name>]`, wherever the stream is split;
//! * emitted output never ends in the middle of a UTF-8 character;
//! * overlapping occurrences of different secrets are replaced as one union span, so
//!   no part of either secret survives (e.g. `tok-AAAA` + `AAAA-SECRET` in `tok-AAAA-SECRET`);
//! * `finish()` flushes the retained tail through the same matcher.
//!
//! Scope, as stated in the plan: known literal values and their JSON escape.
//! Other encodings (base64, URL-encoding, a model re-typing the value) are not
//! covered.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

/// One secret to redact: a display name (never the value) and its value.
#[derive(Clone)]
pub struct Secret {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("name", &self.name)
            .field("value", &"<hidden>")
            .finish()
    }
}

/// Immutable matcher shared by every stream of one process (cheap to clone via `Arc`).
#[derive(Clone)]
pub struct Redactions {
    ac: Option<std::sync::Arc<AhoCorasick>>,
    replacements: Vec<Vec<u8>>,
    max_len: usize,
    /// The length of the shortest pattern (0 without secrets): how much a replacement can add
    /// per byte of the text it replaces (M13-R3).
    min_len: usize,
    /// Some pattern contains a line break: an occurrence can then span lines, and a line
    /// ending is no safe place to release the stream (M12-04).
    multiline: bool,
}

impl std::fmt::Debug for Redactions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactions")
            .field("patterns", &self.replacements.len())
            .finish()
    }
}

impl Redactions {
    /// Build the matcher. Empty values are ignored. Each secret contributes its
    /// literal bytes and, when different, its JSON-string-escaped bytes.
    pub fn new(secrets: &[Secret]) -> Self {
        let mut patterns: Vec<Vec<u8>> = Vec::new();
        let mut replacements: Vec<Vec<u8>> = Vec::new();
        for s in secrets {
            if s.value.is_empty() {
                continue;
            }
            // M13-03: a value of line breaks only is not a secret — as a pattern it would
            // redact every line end
            if s.value.bytes().all(|b| b == b'\n') {
                // (M13-R8: once per process and name — every spawn builds its matcher again)
                static NOTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
                let name = sanitize_name(&s.name);
                let mut noted = NOTED.lock().unwrap_or_else(|e| e.into_inner());
                if !noted.contains(&name) {
                    crate::errln!(
                        "note: the secret `{name}` holds only line breaks — it is not redacted"
                    );
                    noted.push(name);
                }
                continue;
            }
            let repl = format!("[REDACTED:{}]", sanitize_name(&s.name)).into_bytes();
            let literal = s.value.as_bytes().to_vec();
            let escaped_full = serde_json::to_string(&s.value).expect("string serialises");
            let escaped = escaped_full.as_bytes()[1..escaped_full.len() - 1].to_vec();
            if !patterns.contains(&literal) {
                patterns.push(literal.clone());
                replacements.push(repl.clone());
            }
            if escaped != literal && !patterns.contains(&escaped) {
                patterns.push(escaped);
                replacements.push(repl);
            }
        }
        if patterns.is_empty() {
            return Self {
                ac: None,
                replacements,
                max_len: 0,
                min_len: 0,
                multiline: false,
            };
        }
        let max_len = patterns.iter().map(|p| p.len()).max().unwrap_or(0);
        let min_len = patterns.iter().map(|p| p.len()).min().unwrap_or(0);
        // a trailing line break (a secret read from a file) ends a line, it does not span lines.
        // M13-03: exactly one — the "a complete line is final" rule holds only when a line
        // break can be nothing but the last byte of a pattern (`ab\n\n` has one inside)
        let multiline = patterns.iter().any(|p| p[..p.len() - 1].contains(&b'\n'));
        let ac = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard) // overlapping search; spans are unioned below
            .build(&patterns)
            .expect("aho-corasick build");
        Self {
            ac: Some(std::sync::Arc::new(ac)),
            replacements,
            max_len,
            min_len,
            multiline,
        }
    }

    pub fn empty() -> Self {
        Self::new(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.ac.is_none()
    }

    /// The length of the longest pattern (0 without secrets): text cut within that many bytes
    /// of a secret's start can hold a fragment of it (M13-04).
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// M13-R3: how many bytes of **redacted** text can stand where a secret's fragment of at most
    /// `max_len - 1` raw bytes (plus two for a torn character) was: a shorter secret inside the
    /// fragment becomes its marker, which can be longer than it was. Never less than
    /// `max_len + 2` plus every marker once; and for repeats (`bbbb…` inside the fragment) the
    /// raw length times the most a replacement adds per byte. 0 without secrets.
    pub fn cut_hold(&self) -> usize {
        if self.max_len == 0 {
            return 0;
        }
        let raw = self.max_len + 2;
        let markers: usize = self.replacements.iter().map(|r| r.len()).sum();
        let longest = self.replacements.iter().map(|r| r.len()).max().unwrap_or(0);
        let per_byte = longest.div_ceil(self.min_len.max(1)).max(1);
        (raw + markers).max(raw * per_byte)
    }

    /// Redact a complete, already-buffered value (e.g. a stored event payload).
    /// Defence in depth only: streams must go through [`StreamRedactor`].
    pub fn redact_all(&self, input: &[u8]) -> Vec<u8> {
        let mut r = StreamRedactor::new(self.clone());
        let mut out = r.push(input);
        out.extend(r.finish());
        out
    }

    pub fn redact_str(&self, input: &str) -> String {
        String::from_utf8_lossy(&self.redact_all(input.as_bytes())).into_owned()
    }

    /// R23-21: a JSON value with every secret redacted, leaf by leaf — strings (and keys) as
    /// text; a number whose digits hold a secret becomes the redacted string, that leaf alone.
    /// Was: the whole value was redacted as one JSON text and parsed again; a secret that
    /// matched outside a string (a number) made the text unparseable, and the whole payload
    /// became one string — an `InvalidOutput` with no word why.
    pub fn redact_value(&self, v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value as V;
        match v {
            V::String(s) => V::String(self.redact_str(s)),
            V::Number(n) => {
                let text = n.to_string();
                let red = self.redact_str(&text);
                if red == text {
                    v.clone()
                } else {
                    V::String(red)
                }
            }
            V::Array(items) => V::Array(items.iter().map(|i| self.redact_value(i)).collect()),
            V::Object(o) => V::Object(
                o.iter()
                    .map(|(k, v)| (self.redact_str(k), self.redact_value(v)))
                    .collect(),
            ),
            V::Bool(_) | V::Null => v.clone(),
        }
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Stateful redactor for one byte stream.
pub struct StreamRedactor {
    r: Redactions,
    buf: Vec<u8>,
    finished: bool,
}

impl StreamRedactor {
    pub fn new(r: Redactions) -> Self {
        Self {
            r,
            buf: Vec::new(),
            finished: false,
        }
    }

    /// Feed a chunk; returns the bytes that are now safe to emit.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        assert!(!self.finished, "push after finish");
        self.buf.extend_from_slice(chunk);
        self.process(false)
    }

    /// End of stream (EOF, pipe closed by Job termination, …): flush the tail.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        self.process(true)
    }

    /// Bytes currently held back (never emitted if the process dies now).
    pub fn retained(&self) -> usize {
        self.buf.len()
    }

    fn process(&mut self, eof: bool) -> Vec<u8> {
        let Some(ac) = self.r.ac.clone() else {
            // No secrets: still respect UTF-8 boundaries so downstream decoding is stable.
            let cut = if eof {
                self.buf.len()
            } else {
                utf8_safe_cut(&self.buf, self.buf.len(), 0)
            };
            return self.buf.drain(..cut).collect();
        };
        let len = self.buf.len();
        // A secret occurrence that is still incomplete must start at or after
        // `len - (max_len - 1)`; everything before that point is final.
        let mut final_before = if eof {
            len
        } else {
            len.saturating_sub(self.r.max_len.saturating_sub(1))
        };
        // M12-04 (H5): no secret holds a line break, so nothing that starts in a line can
        // reach into the next one: everything up to and including the last `\n` is final. A
        // line that ends an event must not wait for the next chunk — an adapter that waits
        // for "command started" would see a long silent command as stalled.
        if !eof && !self.r.multiline {
            if let Some(nl) = self.buf.iter().rposition(|b| *b == b'\n') {
                final_before = final_before.max(nl + 1);
            }
        }
        // Union of all (overlapping) occurrences, sorted by start.
        // Overlapping search reports matches by end position: sort by start (longest first).
        let mut raw: Vec<(usize, usize, usize)> = ac
            .find_overlapping_iter(&self.buf)
            .map(|m| (m.start(), m.end(), m.pattern().as_usize()))
            .collect();
        raw.sort_by_key(|&(s, e, _)| (s, std::cmp::Reverse(e)));
        let mut spans: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, pattern of the longest member)
        for (s, e, p) in raw {
            match spans.last_mut() {
                Some(last) if s < last.1 => {
                    if e - s > last.1 - last.0 {
                        last.2 = p;
                    }
                    last.1 = last.1.max(e);
                }
                _ => spans.push((s, e, p)),
            }
        }
        let mut cut = final_before;
        let mut out = Vec::with_capacity(len);
        let mut last = 0usize;
        for (s, e, p) in spans {
            if s >= cut {
                break;
            }
            if e > cut && !eof {
                // A future (not yet complete) occurrence could still overlap this span:
                // hold the whole span back.
                cut = s;
                break;
            }
            out.extend_from_slice(&self.buf[last..s]);
            out.extend_from_slice(&self.r.replacements[p]);
            last = e;
        }
        if last > cut {
            cut = last;
        }
        if !eof {
            cut = utf8_safe_cut(&self.buf, cut, last);
        }
        out.extend_from_slice(&self.buf[last..cut]);
        self.buf.drain(..cut);
        out
    }
}

/// Move `cut` back (not below `floor`) so that `buf[..cut]` does not end inside a
/// multi-byte UTF-8 sequence. Invalid UTF-8 is passed through unchanged.
fn utf8_safe_cut(buf: &[u8], cut: usize, floor: usize) -> usize {
    let lower = cut.saturating_sub(4).max(floor);
    let mut p = cut;
    while p > lower {
        p -= 1;
        let b = buf[p];
        if b & 0b1100_0000 == 0b1000_0000 {
            continue; // continuation byte: keep looking for the lead
        }
        if b & 0b1000_0000 == 0 {
            return cut; // ASCII lead: cut is on a boundary
        }
        let need = if b >= 0xF0 {
            4
        } else if b >= 0xE0 {
            3
        } else {
            2
        };
        return if cut - p >= need { cut } else { p };
    }
    cut
}

/// Truncate after redaction, at a UTF-8 boundary (plan: diagnostics capped at 64 KB).
pub fn truncate_utf8(bytes: &[u8], cap: usize) -> &[u8] {
    if bytes.len() <= cap {
        return bytes;
    }
    let mut c = cap;
    while c > 0 && bytes[c] & 0b1100_0000 == 0b1000_0000 {
        c -= 1;
    }
    &bytes[..c]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(name: &str, v: &str) -> Secret {
        Secret {
            name: name.into(),
            value: v.into(),
        }
    }

    /// R23-21: a secret in a number leaf of a structured output — that leaf alone becomes the
    /// redacted string; the rest of the payload keeps its shape (was: one string, unparseable).
    #[test]
    fn a_json_value_is_redacted_leaf_by_leaf() {
        let r = Redactions::new(&[sec("N", "47114711"), sec("K", "s3cret-value")]);
        let v = serde_json::json!({
            "decision": "approved",
            "count": 47114711,
            "other": 12,
            "issues": [{"description": "the key is s3cret-value"}],
        });
        let out = r.redact_value(&v);
        assert_eq!(out["decision"], "approved");
        assert_eq!(out["count"], "[REDACTED:N]");
        assert_eq!(out["other"], 12);
        assert_eq!(out["issues"][0]["description"], "the key is [REDACTED:K]");
        // the old way: the whole payload became one string
        let text = r.redact_str(&v.to_string());
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
    }

    fn run_chunks(r: &Redactions, chunks: &[&[u8]]) -> Vec<u8> {
        let mut s = StreamRedactor::new(r.clone());
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.push(c));
        }
        out.extend(s.finish());
        out
    }

    fn contains(h: &[u8], n: &[u8]) -> bool {
        h.windows(n.len()).any(|w| w == n)
    }

    const SECRETS: &[(&str, &str)] = &[
        ("KEY_A", "abcdef0123456789"),
        ("KEY_B", "abcdef"),         // overlaps with KEY_A (prefix)
        ("KEY_C", "5678XYZ"),        // overlaps with KEY_A (suffix region)
        ("JP", "秘密のトークン🔑x"), // multibyte, incl. 4-byte char
        ("QUOTE", "pa\"ss\\word"),   // JSON escaping differs
    ];

    fn all() -> Redactions {
        Redactions::new(&SECRETS.iter().map(|(n, v)| sec(n, v)).collect::<Vec<_>>())
    }

    fn sample() -> Vec<u8> {
        let s = "start abcdef0123456789 mid abcdef end 5678XYZ; 日本語テキスト 秘密のトークン🔑x \
                 {\"k\":\"pa\\\"ss\\\\word\"} raw pa\"ss\\word tail abcdef0123456789";
        s.as_bytes().to_vec()
    }

    fn assert_clean(out: &[u8]) {
        for (_, v) in SECRETS {
            assert!(
                !contains(out, v.as_bytes()),
                "leaked {v:?} in {:?}",
                String::from_utf8_lossy(out)
            );
            let esc = serde_json::to_string(v).unwrap();
            let esc = &esc.as_bytes()[1..esc.len() - 1];
            assert!(!contains(out, esc), "leaked escaped {v:?}");
        }
        assert!(
            std::str::from_utf8(out).is_ok(),
            "output must be valid UTF-8"
        );
    }

    #[test]
    fn whole_buffer_is_redacted() {
        let out = all().redact_all(&sample());
        assert_clean(&out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("[REDACTED:KEY_A]"));
        assert!(s.contains("[REDACTED:KEY_B]"));
        assert!(s.contains("[REDACTED:JP]"));
        assert!(s.contains("[REDACTED:QUOTE]"));
        assert!(s.contains("日本語テキスト"));
    }

    #[test]
    fn one_byte_chunks() {
        let data = sample();
        let chunks: Vec<&[u8]> = data.chunks(1).collect();
        let out = run_chunks(&all(), &chunks);
        assert_clean(&out);
        assert_eq!(
            out,
            all().redact_all(&data),
            "chunking must not change the result"
        );
    }

    #[test]
    fn every_two_way_split() {
        let data = sample();
        let reference = all().redact_all(&data);
        for i in 0..=data.len() {
            let out = run_chunks(&all(), &[&data[..i], &data[i..]]);
            assert_clean(&out);
            assert_eq!(out, reference, "split at {i}");
        }
    }

    #[test]
    fn every_three_way_split_small() {
        let r = Redactions::new(&[sec("S", "abcdef"), sec("T", "cdefgh")]);
        let data = b"xxabcdefghyyabcdefgh";
        let reference = r.redact_all(data);
        for i in 0..=data.len() {
            for j in i..=data.len() {
                let out = run_chunks(&r, &[&data[..i], &data[i..j], &data[j..]]);
                assert!(!contains(&out, b"abcdef") && !contains(&out, b"cdefgh"));
                assert_eq!(out, reference, "split {i},{j}");
            }
        }
    }

    #[test]
    fn prefix_secret_does_not_leak_suffix_of_longer_one() {
        // "abcdef" is a prefix of "abcdef0123456789": the longer one must win when it is
        // complete, even if the stream is cut right after the shorter one.
        let r = all();
        let out = run_chunks(&r, &[b"..abcdef", b"0123456789.."]);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "..[REDACTED:KEY_A]..");
    }

    #[test]
    fn eof_mid_secret_is_redacted_or_withheld() {
        // Stream ends (e.g. Job terminated) in the middle of a secret: nothing of the
        // partial secret is a full secret, so it is flushed; a full one at the tail is redacted.
        let r = all();
        let out = run_chunks(&r, &[b"head ", b"abcdef"]);
        assert_eq!(String::from_utf8(out).unwrap(), "head [REDACTED:KEY_B]");
    }

    /// M12-04 AC-1 (H5): with a secret configured, a chunk that ends in a line break leaves
    /// nothing held back — an event line reaches the adapter at once.
    #[test]
    fn a_complete_line_is_released_at_once() {
        let r = all();
        let mut s = StreamRedactor::new(r);
        let out = s.push(b"{\"type\":\"command_started\"}\n");
        assert_eq!(s.retained(), 0);
        assert_eq!(out, b"{\"type\":\"command_started\"}\n");
        // a line and the start of the next: the tail is held back only as far as a secret
        // could still begin in it
        let out = s.push(b"second line\nabcdef0123");
        assert!(out.starts_with(b"second line\n"), "{out:?}");
        assert_eq!(s.retained(), "abcdef0123".len());
        // with no line break the old rule stands: at most max_len-1 bytes are held
        let mut s = StreamRedactor::new(all());
        let _ = s.push(&vec![b'z'; 10_000]);
        assert!(s.retained() < all().max_len);
    }

    /// M12-04 AC-2: a secret split across two chunks inside one line is still redacted, and
    /// so is one before the line break of the same chunk.
    #[test]
    fn a_secret_split_inside_a_line_is_still_redacted() {
        let r = all();
        let out = run_chunks(
            &r,
            &[b"line one\nuse abcdef01", b"23456789 now\nline three\n"],
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "line one\nuse [REDACTED:KEY_A] now\nline three\n"
        );
        let out = run_chunks(&r, &[b"token abcdef0123456789\nnext", b" line\n"]);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "token [REDACTED:KEY_A]\nnext line\n"
        );
        // every split of a multi-line sample is still clean
        let sample = b"a abcdef0123456789 b\nabcdef c 5678XYZ\nend 5678XYZ\n".to_vec();
        for cut in 0..=sample.len() {
            let out = run_chunks(&r, &[&sample[..cut], &sample[cut..]]);
            assert_clean(&out);
        }
    }

    /// M12-04 review: a trailing line break (a secret read from a file) ends a line, it does
    /// not span lines — the stream is still released at a line end.
    #[test]
    fn a_trailing_newline_in_a_secret_does_not_hold_the_stream() {
        let r = Redactions::new(&[sec("FILE", "file-secret-value\n")]);
        let mut s = StreamRedactor::new(r);
        let out = s.push(b"{\"type\":\"command_started\"}\n");
        assert_eq!(s.retained(), 0, "{out:?}");
        let out = run_chunks(
            &Redactions::new(&[sec("FILE", "file-secret-value\n")]),
            &[b"x file-secret-va", b"lue\ny\n"],
        );
        assert_eq!(String::from_utf8(out).unwrap(), "x [REDACTED:FILE]y\n");
    }

    /// M12-04: a secret that contains a line break can span lines, so the stream is not
    /// released at a line end — as before.
    #[test]
    fn a_secret_with_a_line_break_keeps_the_old_hold_back() {
        let r = Redactions::new(&[sec("PEM", "-----BEGIN\nMIIE-----END")]);
        let mut s = StreamRedactor::new(r.clone());
        let out = s.push(b"x\n-----BEGIN\n");
        assert!(s.retained() > 0, "{out:?}");
        let rest = s.push(b"MIIE-----END\ndone\n");
        let mut all_out = out;
        all_out.extend(rest);
        all_out.extend(s.finish());
        assert_eq!(
            String::from_utf8(all_out).unwrap(),
            "x\n[REDACTED:PEM]\ndone\n"
        );
    }

    /// M13-03: a line break may only be the **last** byte of a secret for the "a complete line
    /// is final" rule to hold. A value that ends in two line breaks has one inside it: with the
    /// first chunk ending at that inner break the secret used to leave in clear.
    #[test]
    fn a_secret_ending_in_two_line_breaks_is_not_released_in_halves() {
        let r = Redactions::new(&[sec("K", "ab\n\n")]);
        let mut s = StreamRedactor::new(r);
        let mut out = s.push(b"zab\n");
        out.extend(s.push(b"\n"));
        out.extend(s.finish());
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("ab"), "{text:?}");
        assert_eq!(text, "z[REDACTED:K]");
        // a single trailing break is still a line end, not a line span (M12-04 review)
        let one = Redactions::new(&[sec("K", "ab\n")]);
        assert!(!one.multiline);
        let mut s = StreamRedactor::new(one);
        let out = s.push(b"zab\nnext\n");
        assert_eq!(s.retained(), 0);
        assert_eq!(String::from_utf8(out).unwrap(), "z[REDACTED:K]next\n");
        // a secret of line breaks only is not a secret: dropped, nothing is redacted
        let only = Redactions::new(&[sec("N", "\n\n"), sec("M", "\n")]);
        assert!(only.is_empty());
        assert_eq!(only.redact_str("a\n\nb\n"), "a\n\nb\n");
    }

    #[test]
    fn retained_tail_is_bounded() {
        let r = all();
        let mut s = StreamRedactor::new(r.clone());
        let _ = s.push(&vec![b'z'; 10_000]);
        assert!(s.retained() < r.max_len, "retains at most max_len-1 bytes");
    }

    #[test]
    fn multibyte_never_split_in_output() {
        let r = Redactions::new(&[sec("K", "zzzzzzzzzzzz")]);
        let data = "あいうえお🔑かきくけこ".as_bytes().to_vec();
        for i in 0..=data.len() {
            let mut s = StreamRedactor::new(r.clone());
            let a = s.push(&data[..i]);
            assert!(
                std::str::from_utf8(&a).is_ok(),
                "first emission at split {i} not UTF-8"
            );
            let mut out = a;
            out.extend(s.push(&data[i..]));
            out.extend(s.finish());
            assert_eq!(out, data);
        }
    }

    #[test]
    fn truncation_after_redaction_cannot_expose_secret_prefix() {
        let r = all();
        let data = sample();
        for cap in 0..data.len() {
            let red = r.redact_all(&data);
            let t = truncate_utf8(&red, cap);
            assert!(std::str::from_utf8(t).is_ok());
            // The redacted text contains no secret, so no prefix of it can either.
            assert!(!contains(t, b"abcdef0"));
        }
    }

    #[test]
    fn overlapping_secrets_leave_no_part_of_either() {
        let r = Redactions::new(&[sec("A", "tok-AAAA"), sec("B", "AAAA-SECRETPART")]);
        let data = b"x tok-AAAA-SECRETPART y AAAA-SECRETPART z tok-AAAA w";
        let reference = r.redact_all(data);
        let s = String::from_utf8(reference.clone()).unwrap();
        assert!(
            !s.contains("SECRETPART") && !s.contains("tok-AAAA") && !s.contains("AAAA"),
            "{s}"
        );
        for i in 0..=data.len() {
            for j in i..=data.len() {
                let out = run_chunks(&r, &[&data[..i], &data[i..j], &data[j..]]);
                assert_eq!(out, reference, "split {i},{j}");
            }
        }
    }

    #[test]
    fn chained_overlaps_are_held_back_not_split() {
        // A second secret starting inside the first must not be emitted half-raw
        // when it arrives in a later chunk.
        let r = Redactions::new(&[sec("A", "abcdef"), sec("B", "defghijk")]);
        let out = run_chunks(&r, &[b"..abcdef", b"ghijk.."]);
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("ghijk") && !s.contains("abc"), "{s}");
    }

    /// Randomised: small alphabet so secrets overlap and repeat a lot; random chunking must
    /// equal whole-buffer redaction and never contain any secret.
    #[test]
    fn fuzz_random_secrets_and_splits() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let alphabet = b"abcd";
        for _round in 0..400 {
            let nsec = 1 + rnd(3) as usize;
            let secrets: Vec<Secret> = (0..nsec)
                .map(|i| {
                    let len = 3 + rnd(5) as usize;
                    let v: String = (0..len)
                        .map(|_| alphabet[rnd(4) as usize] as char)
                        .collect();
                    sec(&format!("S{i}"), &v)
                })
                .collect();
            let r = Redactions::new(&secrets);
            let dlen = 10 + rnd(60) as usize;
            let data: Vec<u8> = (0..dlen).map(|_| alphabet[rnd(4) as usize]).collect();
            let reference = r.redact_all(&data);
            for s in &secrets {
                assert!(
                    !contains(&reference, s.value.as_bytes()),
                    "reference leaks {:?} in {:?}",
                    s.value,
                    String::from_utf8_lossy(&reference)
                );
            }
            for _ in 0..20 {
                let mut chunks = Vec::new();
                let mut i = 0;
                while i < data.len() {
                    let n = 1 + rnd(8) as usize;
                    let j = (i + n).min(data.len());
                    chunks.push(&data[i..j]);
                    i = j;
                }
                assert_eq!(run_chunks(&r, &chunks), reference);
            }
        }
    }

    #[test]
    fn no_secrets_is_passthrough() {
        let r = Redactions::empty();
        assert_eq!(
            run_chunks(&r, &[b"hello ", "世界".as_bytes()]),
            "hello 世界".as_bytes()
        );
    }

    /// M13-R3: the hold at a cut covers what the replacements of shorter secrets add to a
    /// fragment of a longer one — also when the short one repeats inside the fragment.
    #[test]
    fn the_cut_hold_covers_a_fragment_full_of_markers() {
        assert_eq!(Redactions::empty().cut_hold(), 0);
        let a = format!("{}X", "b".repeat(20));
        let r = Redactions::new(&[sec("A", &a), sec("B", "b")]);
        let fragment = &a.as_bytes()[..a.len() - 1];
        let redacted = r.redact_all(fragment);
        assert!(redacted.len() > 200, "twenty markers: {}", redacted.len());
        assert!(
            r.cut_hold() >= redacted.len(),
            "{} < {}",
            r.cut_hold(),
            redacted.len()
        );
        // never less than before
        let one = Redactions::new(&[sec("S", "SECRET-VALUE-20CHARS")]);
        assert!(one.cut_hold() >= one.max_len() + 2);
    }

    #[test]
    fn debug_never_prints_value() {
        let s = sec("X", "supersecretvalue");
        assert!(!format!("{s:?}").contains("supersecret"));
    }
}
