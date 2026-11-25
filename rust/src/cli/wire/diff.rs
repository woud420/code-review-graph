use std::collections::HashMap;
use std::fmt::Write;

fn lines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut pos = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if matches!(
            ch,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            let mut end = at + ch.len_utf8();
            if ch == '\r' && chars.next_if(|(_, c)| *c == '\n').is_some() {
                end += 1;
            }
            result.push(&text[pos..end]);
            pos = end;
        }
    }
    if pos < text.len() {
        result.push(&text[pos..]);
    }
    result
}
fn matches(a: &[&str], b: &[&str]) -> Vec<(usize, usize, usize)> {
    let mut index = HashMap::<_, Vec<_>>::new();
    for (j, line) in b.iter().enumerate() {
        index.entry(*line).or_default().push(j);
    }
    if b.len() >= 200 {
        index.retain(|_, v| v.len() <= b.len() / 100 + 1);
    }
    let mut found = Vec::new();
    let mut stack = vec![(0, a.len(), 0, b.len())];
    while let Some((lo, hi, low, high)) = stack.pop() {
        let (mut ai, mut bi, mut count) = (lo, low, 0);
        let mut last = HashMap::new();
        for (i, line) in a.iter().enumerate().take(hi).skip(lo) {
            let mut next = HashMap::new();
            for &j in index
                .get(line)
                .into_iter()
                .flatten()
                .skip_while(|j| **j < low)
                .take_while(|j| **j < high)
            {
                let len = j
                    .checked_sub(1)
                    .and_then(|j| last.get(&j))
                    .copied()
                    .unwrap_or(0)
                    + 1;
                next.insert(j, len);
                if len > count {
                    ai = i + 1 - len;
                    bi = j + 1 - len;
                    count = len;
                }
            }
            last = next;
        }
        while ai > lo && bi > low && a[ai - 1] == b[bi - 1] {
            ai -= 1;
            bi -= 1;
            count += 1;
        }
        while ai + count < hi && bi + count < high && a[ai + count] == b[bi + count] {
            count += 1;
        }
        if count > 0 {
            found.push((ai, bi, count));
            if lo < ai && low < bi {
                stack.push((lo, ai, low, bi));
            }
            if ai + count < hi && bi + count < high {
                stack.push((ai + count, hi, bi + count, high));
            }
        }
    }
    found.sort_unstable();
    found.push((a.len(), b.len(), 0));
    found
}
fn range(start: usize, len: usize) -> String {
    match len {
        0 => format!("{start},0"),
        1 => (start + 1).to_string(),
        _ => format!("{},{}", start + 1, len),
    }
}
pub(in crate::cli) fn unified(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let (a, b) = (lines(before), lines(after));
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bi, n) in matches(&a, &b) {
        for line in &a[i..ai] {
            ops.push(('-', *line));
        }
        for line in &b[j..bi] {
            ops.push(('+', *line));
        }
        for line in &a[ai..ai + n] {
            ops.push((' ', *line));
        }
        i = ai + n;
        j = bi + n;
    }
    let mut groups = Vec::<(usize, usize)>::new();
    for (i, (kind, _)) in ops.iter().enumerate() {
        if *kind == ' ' {
            continue;
        }
        let start = i.saturating_sub(3);
        let end = (i + 4).min(ops.len());
        if let Some(last) = groups.last_mut().filter(|g| g.1 >= start) {
            last.1 = end;
        } else {
            groups.push((start, end));
        }
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let (mut old, mut new, mut position) = (0, 0, 0);
    for (start, end) in groups {
        old += start - position;
        new += start - position;
        let hunk = &ops[start..end];
        let count_a = hunk.iter().filter(|(k, _)| *k != '+').count();
        let count_b = hunk.iter().filter(|(k, _)| *k != '-').count();
        writeln!(
            out,
            "@@ -{} +{} @@",
            range(old, count_a),
            range(new, count_b)
        )
        .unwrap();
        for &(kind, line) in hunk {
            out.push(kind);
            out.push_str(line);
        }
        old += count_a;
        new += count_b;
        position = end;
    }
    out
}
