use super::*;

pub(in crate::cli) struct Diff {
    pub path: String,
    pub old_path: String,
    pub hunks: Vec<Hunk>,
}
pub(in crate::cli) struct Hunk {
    pub start: i64,
    pub count: i64,
    pub old_start: i64,
    pub old_count: i64,
}
impl Hunk {
    pub(in crate::cli) fn range(&self) -> (i64, i64) {
        (self.start, self.start + (self.count - 1).max(0))
    }
}

fn header_path(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.split('\t').next()?.trim_end_matches('\r');
    let decoded = if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let mut bytes = inner.bytes().peekable();
        let mut out = Vec::new();
        while let Some(byte) = bytes.next() {
            if byte != b'\\' {
                out.push(byte);
                continue;
            }
            let Some(esc) = bytes.next() else {
                out.push(byte);
                break;
            };
            if (b'0'..=b'7').contains(&esc) {
                let mut value = esc - b'0';
                for _ in 0..2 {
                    if let Some(next) = bytes.next_if(|n| (b'0'..=b'7').contains(n)) {
                        value = value.wrapping_mul(8) + (next - b'0');
                    } else {
                        break;
                    }
                }
                out.push(value);
            } else {
                out.push(match esc {
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b'a' => 7,
                    b'b' => 8,
                    b'f' => 12,
                    b'v' => 11,
                    other => other,
                });
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    } else {
        raw.to_string()
    };
    decoded
        .strip_prefix(prefix)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(in crate::cli) fn read_diff(root: &Path, base: &str) -> Result<Vec<Diff>> {
    let (ok, bytes) = git_timed(
        root,
        &["diff", "--unified=0", base, "--"],
        "CRG_GIT_TIMEOUT",
        30.,
    )?;
    if !ok {
        return Ok(vec![]);
    }
    let pattern = regex::Regex::new(r"^@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@")?;
    let source = String::from_utf8_lossy(&bytes);
    let mut current = None;
    let mut old = None;
    let mut files: Vec<Diff> = Vec::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("diff --git ") {
            current = None;
            old = None;
            continue;
        }
        if let Some(raw) = line.strip_prefix("--- ")
            && lines.peek().is_some_and(|s| s.starts_with("+++ "))
        {
            old = header_path(raw, "a/");
            continue;
        }
        if let Some(raw) = line.strip_prefix("+++ ") {
            current = header_path(raw, "b/").map(|path| {
                let old_path = old.take().unwrap_or_else(|| path.clone());
                files.push(Diff {
                    path,
                    old_path,
                    hunks: vec![],
                });
                files.len() - 1
            });
            continue;
        }
        if let Some(id) = current
            && let Some(m) = pattern.captures(line)
        {
            let number = |i| {
                m.get(i)
                    .and_then(|v| v.as_str().parse::<i64>().ok())
                    .unwrap_or(1)
            };
            files[id].hunks.push(Hunk {
                old_start: number(1),
                old_count: number(2),
                start: number(3),
                count: number(4),
            });
        }
    }
    files.retain(|f| !f.hunks.is_empty());
    Ok(files)
}
