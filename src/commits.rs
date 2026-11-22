//! Commits mode: when `$DATA_DIR/commits` exists, print exactly one line per
//! commit found under it (to stdout and to `$OUTPUT_DIR/commits.txt`), then exit.
//!
//! The input layout is not pinned down, so every file is sniffed and split into
//! commit records:
//!   - JSON array of commits, or an object holding a `commits` array
//!   - JSON Lines / concatenated JSON objects (one commit each)
//!   - `git log` text (blocks starting with `commit <sha>`)
//!   - `git format-patch` mbox (blocks starting with `From <sha> `)
//!   - CSV/TSV with a header row (one commit per row, quote-aware)
//!   - `git log --oneline` style (each line starts with a hex id)
//!   - anything else: the whole file is one commit
//! Each line is `<id>` or `<id> <subject>`; ids are de-duplicated so a commit
//! listed twice still gets one line. Records with no id get `<file>#<n>`.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const ID_KEYS: [&str; 8] = ["sha", "hash", "commit_hash", "commit_sha", "oid", "id", "commit", "revision"];
const MSG_KEYS: [&str; 5] = ["subject", "message", "msg", "title", "summary"];

pub struct Commit {
    pub id: String,
    pub subject: String,
}

/// Runs commits mode if the commits directory exists. Returns false otherwise.
pub fn run_if_present(data: &str, out: &str) -> bool {
    let dir = Path::new(data).join("commits");
    if !dir.exists() {
        return false;
    }
    let commits = collect(&dir);
    let mut text = String::with_capacity(commits.len() * 64);
    for c in &commits {
        text.push_str(&c.id);
        if !c.subject.is_empty() {
            text.push(' ');
            text.push_str(&c.subject);
        }
        text.push('\n');
    }
    let so = std::io::stdout();
    let mut so = so.lock();
    let _ = so.write_all(text.as_bytes());
    let _ = so.flush();
    let _ = std::fs::create_dir_all(out);
    let _ = std::fs::write(Path::new(out).join("commits.txt"), text.as_bytes());
    true
}

pub fn collect(dir: &Path) -> Vec<Commit> {
    let mut files = Vec::new();
    if dir.is_file() {
        files.push(dir.to_path_buf());
    } else {
        walk(dir, &mut files);
    }
    files.sort();
    let mut seen = HashSet::new();
    let mut all = Vec::new();
    for f in &files {
        let Ok(b) = std::fs::read(f) else { continue };
        if b.is_empty() || b.contains(&0) {
            continue;
        }
        let s = String::from_utf8_lossy(&b);
        let name = f.strip_prefix(dir).unwrap_or(f).to_string_lossy().into_owned();
        for (n, mut c) in parse_file(&s, &name).into_iter().enumerate() {
            if c.id.is_empty() {
                c.id = format!("{name}#{}", n + 1);
            }
            if seen.insert(c.id.clone()) {
                all.push(c);
            }
        }
    }
    all
}

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let hidden = p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

pub fn parse_file(s: &str, name: &str) -> Vec<Commit> {
    let s = s.trim_start_matches('\u{feff}');
    let t = s.trim();
    if t.is_empty() {
        return vec![];
    }
    let b = t.as_bytes();
    if b[0] == b'[' || b[0] == b'{' {
        if let Some(v) = parse_json_records(t) {
            return v;
        }
    }
    let lines: Vec<&str> = t.lines().collect();
    if lines.iter().any(|l| git_log_header(l).is_some()) {
        return split_git_log(&lines);
    }
    if lines.iter().any(|l| mbox_header(l).is_some()) {
        return split_mbox(&lines);
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".tsv") || looks_like_csv(lines[0]) {
        if let Some(v) = parse_csv(t, if lower.ends_with(".tsv") || (lines[0].contains('\t') && !lines[0].contains(',')) { b'\t' } else { b',' }) {
            return v;
        }
    }
    let nonempty: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if nonempty.len() > 1 && nonempty.iter().all(|l| leading_hex(l).is_some()) {
        return nonempty
            .iter()
            .map(|l| {
                let id = leading_hex(l).unwrap();
                Commit { id: id.to_string(), subject: one_line(l[id.len()..].trim()) }
            })
            .collect();
    }
    // Single commit per file.
    let id = find_labeled_hex(t).or_else(|| file_stem_hex(name)).or_else(|| first_hex(t)).unwrap_or_default();
    let subject = nonempty.iter().find(|l| !l.contains(id.as_str()) || id.is_empty()).map(|l| one_line(l)).unwrap_or_default();
    vec![Commit { id, subject }]
}

// ---------- helpers: hex ids ----------

fn is_hex_id(w: &str) -> bool {
    (7..=64).contains(&w.len()) && w.bytes().all(|c| c.is_ascii_hexdigit()) && w.bytes().any(|c| c.is_ascii_digit())
}

fn leading_hex(l: &str) -> Option<&str> {
    let w = l.split(|c: char| c.is_whitespace() || c == ',' || c == '\t' || c == '|' || c == ':').next()?;
    is_hex_id(w).then_some(w)
}

fn first_hex(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| is_hex_id(w)).map(|w| w.to_string())
}

fn find_labeled_hex(s: &str) -> Option<String> {
    for l in s.lines() {
        let low = l.trim().to_ascii_lowercase();
        for k in ID_KEYS {
            if low.starts_with(k) {
                let rest = &l.trim()[k.len()..];
                if let Some(h) = first_hex(rest) {
                    return Some(h);
                }
            }
        }
    }
    None
}

fn file_stem_hex(name: &str) -> Option<String> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    is_hex_id(stem).then(|| stem.to_string())
}

fn one_line(s: &str) -> String {
    s.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

// ---------- git log / mbox ----------

fn git_log_header(l: &str) -> Option<&str> {
    let r = l.strip_prefix("commit ")?;
    let w = r.split_whitespace().next()?;
    is_hex_id(w).then_some(w)
}

fn split_git_log(lines: &[&str]) -> Vec<Commit> {
    let mut v: Vec<Commit> = Vec::new();
    let mut in_msg = false;
    for l in lines {
        if let Some(id) = git_log_header(l) {
            v.push(Commit { id: id.to_string(), subject: String::new() });
            in_msg = false;
            continue;
        }
        let Some(c) = v.last_mut() else { continue };
        if l.trim().is_empty() {
            in_msg = true;
            continue;
        }
        if in_msg && c.subject.is_empty() && (l.starts_with(' ') || l.starts_with('\t')) {
            c.subject = l.trim().to_string();
        }
    }
    v
}

fn mbox_header(l: &str) -> Option<&str> {
    let r = l.strip_prefix("From ")?;
    let w = r.split_whitespace().next()?;
    is_hex_id(w).then_some(w)
}

fn split_mbox(lines: &[&str]) -> Vec<Commit> {
    let mut v: Vec<Commit> = Vec::new();
    for l in lines {
        if let Some(id) = mbox_header(l) {
            v.push(Commit { id: id.to_string(), subject: String::new() });
            continue;
        }
        if let Some(c) = v.last_mut() {
            if c.subject.is_empty() {
                if let Some(s) = l.strip_prefix("Subject:") {
                    let s = s.trim();
                    let s = if s.starts_with("[PATCH") { s.splitn(2, ']').nth(1).unwrap_or(s).trim() } else { s };
                    c.subject = s.to_string();
                }
            }
        }
    }
    v
}

// ---------- CSV ----------

fn looks_like_csv(header: &str) -> bool {
    let low = header.to_ascii_lowercase();
    (low.contains(',') || low.contains('\t')) && ID_KEYS.iter().any(|k| low.split(|c| c == ',' || c == '\t').any(|f| f.trim().trim_matches('"') == *k))
}

fn csv_rows(s: &str, sep: u8) -> Vec<Vec<String>> {
    let b = s.as_bytes();
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), Vec::new());
    let (mut i, mut q) = (0, false);
    while i < b.len() {
        let c = b[i];
        if q {
            if c == b'"' {
                if b.get(i + 1) == Some(&b'"') {
                    field.push(b'"');
                    i += 1;
                } else {
                    q = false;
                }
            } else {
                field.push(c);
            }
        } else if c == b'"' {
            q = true;
        } else if c == sep {
            row.push(String::from_utf8_lossy(&field).into_owned());
            field.clear();
        } else if c == b'\n' || c == b'\r' {
            if c == b'\r' && b.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            row.push(String::from_utf8_lossy(&field).into_owned());
            field.clear();
            if row.iter().any(|f| !f.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else {
            field.push(c);
        }
        i += 1;
    }
    row.push(String::from_utf8_lossy(&field).into_owned());
    if row.iter().any(|f| !f.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

fn parse_csv(s: &str, sep: u8) -> Option<Vec<Commit>> {
    let rows = csv_rows(s, sep);
    let hdr: Vec<String> = rows.first()?.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
    let has_header = !hdr.iter().any(|h| is_hex_id(h));
    let idc = if has_header { ID_KEYS.iter().find_map(|k| hdr.iter().position(|h| h == k)) } else { None };
    let msgc = if has_header { MSG_KEYS.iter().find_map(|k| hdr.iter().position(|h| h == k)) } else { None };
    let body = if has_header { &rows[1..] } else { &rows[..] };
    Some(
        body.iter()
            .map(|r| {
                let id = match idc {
                    Some(i) => r.get(i).map(|x| x.trim().to_string()).unwrap_or_default(),
                    None => r.iter().find(|f| is_hex_id(f.trim())).map(|f| f.trim().to_string()).unwrap_or_default(),
                };
                let subject = msgc.and_then(|i| r.get(i)).map(|m| one_line(m)).unwrap_or_default();
                Commit { id, subject }
            })
            .collect(),
    )
}

// ---------- minimal JSON scanning ----------

fn ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn str_end(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index just past the JSON value starting at `i`.
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => str_end(b, i),
        b'{' | b'[' => {
            let mut d = 0i32;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = str_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => d += 1,
                    b'}' | b']' => {
                        d -= 1;
                        if d == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

fn array_elems(b: &[u8], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut i = ws(b, start + 1);
    while i < end && b[i] != b']' {
        let Some(e) = value_end(b, i) else { break };
        v.push((i, e));
        i = ws(b, e);
        if i < end && b[i] == b',' {
            i = ws(b, i + 1);
        }
    }
    v
}

fn obj_fields(b: &[u8], start: usize, end: usize) -> Vec<(String, (usize, usize))> {
    let mut v = Vec::new();
    let mut i = ws(b, start + 1);
    while i < end && b[i] == b'"' {
        let Some(ke) = str_end(b, i) else { break };
        let key = unescape(&b[i + 1..ke - 1]).to_ascii_lowercase();
        i = ws(b, ke);
        if i >= end || b[i] != b':' {
            break;
        }
        i = ws(b, i + 1);
        let Some(ve) = value_end(b, i) else { break };
        v.push((key, (i, ve)));
        i = ws(b, ve);
        if i < end && b[i] == b',' {
            i = ws(b, i + 1);
        }
    }
    v
}

fn unescape(s: &[u8]) -> String {
    let mut o = String::with_capacity(s.len());
    let t = String::from_utf8_lossy(s);
    let mut it = t.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some('r') => o.push('\r'),
            Some('u') => {
                let h: String = it.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    o.push(ch);
                }
            }
            Some(x) => o.push(x),
            None => {}
        }
    }
    o
}

fn json_str(b: &[u8], (s, e): (usize, usize)) -> Option<String> {
    match b[s] {
        b'"' => Some(unescape(&b[s + 1..e - 1])),
        b'{' | b'[' => None,
        _ => Some(String::from_utf8_lossy(&b[s..e]).into_owned()),
    }
}

fn commit_from_obj(b: &[u8], span: (usize, usize)) -> Commit {
    let f = obj_fields(b, span.0, span.1);
    let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
    let mut id = String::new();
    for k in ID_KEYS {
        if let Some(v) = get(k) {
            if let Some(s) = json_str(b, v) {
                if !s.is_empty() && s != "null" {
                    id = s;
                    break;
                }
            }
        }
    }
    let mut subject = String::new();
    for k in MSG_KEYS {
        if let Some(s) = get(k).and_then(|v| json_str(b, v)) {
            subject = one_line(&s);
            break;
        }
    }
    // GitHub API shape: {"sha":..., "commit": {"message": ...}}
    if let Some(v) = get("commit") {
        if b[v.0] == b'{' {
            let inner = commit_from_obj(b, v);
            if id.is_empty() {
                id = inner.id;
            }
            if subject.is_empty() {
                subject = inner.subject;
            }
        }
    }
    if id.is_empty() {
        id = first_hex(&String::from_utf8_lossy(&b[span.0..span.1])).unwrap_or_default();
    }
    Commit { id, subject }
}

fn records_from_value(b: &[u8], span: (usize, usize), out: &mut Vec<Commit>) {
    match b[span.0] {
        b'[' => {
            for e in array_elems(b, span.0, span.1) {
                records_from_value(b, e, out);
            }
        }
        b'{' => {
            let f = obj_fields(b, span.0, span.1);
            let list = f.iter().find(|(k, v)| matches!(k.as_str(), "commits" | "data" | "items" | "values" | "results") && b[v.0] == b'[');
            match list {
                Some((_, v)) => records_from_value(b, *v, out),
                None => out.push(commit_from_obj(b, span)),
            }
        }
        _ => {
            if let Some(s) = json_str(b, span) {
                out.push(Commit { id: s.trim().to_string(), subject: String::new() });
            }
        }
    }
}

fn parse_json_records(t: &str) -> Option<Vec<Commit>> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut i = ws(b, 0);
    let mut any = false;
    while i < b.len() {
        if b[i] != b'{' && b[i] != b'[' {
            return if any { Some(out) } else { None };
        }
        let e = value_end(b, i)?;
        records_from_value(b, (i, e), &mut out);
        any = true;
        i = ws(b, e);
        if i < b.len() && b[i] == b',' {
            i = ws(b, i + 1);
        }
    }
    Some(out)
}
