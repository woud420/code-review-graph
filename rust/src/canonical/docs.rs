use tree_sitter::Node;

pub(super) fn python(node: Node<'_>, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let statement = body
        .named_children(&mut body.walk())
        .find(|n| n.kind() != "comment")?;
    let mut expression = statement.named_child(0)?;
    if statement.kind() != "expression_statement" {
        return None;
    }
    while expression.kind() == "parenthesized_expression" {
        expression = expression.named_child(0)?;
    }
    let mut raw = String::new();
    if expression.kind() == "concatenated_string" {
        for part in expression
            .named_children(&mut expression.walk())
            .filter(|n| n.kind() != "comment")
        {
            raw.push_str(&literal(part, source)?);
        }
    } else {
        raw = literal(expression, source)?;
    }
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraph = Vec::new();
    for line in normalized
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty())
    {
        if line.is_empty()
            || (!paragraph.is_empty()
                && ["@param", "@return", "\\param", "\\return"]
                    .iter()
                    .any(|prefix| line.starts_with(prefix)))
        {
            break;
        }
        paragraph.extend(line.split_whitespace());
    }
    let result: String = paragraph.join(" ").chars().take(400).collect();
    (!result.is_empty()).then_some(result)
}

fn literal(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let start = node.named_child(0)?;
    let end = node.named_child(u32::try_from(node.named_child_count().checked_sub(1)?).ok()?)?;
    if start.kind() != "string_start" || end.kind() != "string_end" {
        return None;
    }
    let prefix = source[start.byte_range()]
        .trim_end_matches(['\'', '"'])
        .to_ascii_lowercase();
    let value = &source[start.end_byte()..end.start_byte()];
    if prefix == "r" {
        return Some(value.to_owned());
    }
    if !matches!(prefix.as_str(), "" | "u") {
        return None;
    }
    let mut chars = value.chars().peekable();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next()? {
            '\n' => {}
            c @ ('\'' | '"' | '\\') => result.push(c),
            'a' => result.push('\x07'),
            'b' => result.push('\x08'),
            'f' => result.push('\x0c'),
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            'v' => result.push('\x0b'),
            c @ ('x' | 'u' | 'U') => {
                let n = match c {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let mut code = 0_u32;
                for _ in 0..n {
                    code = code
                        .checked_mul(16)?
                        .checked_add(chars.next()?.to_digit(16)?)?;
                }
                result.push(char::from_u32(code)?);
            }
            c @ '0'..='7' => {
                let mut code = c.to_digit(8)?;
                for _ in 0..2 {
                    if let Some(next) = chars.next_if(|n| n.is_digit(8)) {
                        code = code * 8 + next.to_digit(8)?;
                    } else {
                        break;
                    }
                }
                result.push(char::from_u32(code)?);
            }
            c => {
                result.push('\\');
                result.push(c);
            }
        }
    }
    Some(result)
}
