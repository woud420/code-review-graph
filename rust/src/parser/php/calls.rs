use super::*;
use crate::model::{Call, syntax::Site};
use serde_json::Value;

pub(super) fn extract(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    owner: &str,
    caller: &str,
    bindings: &mut HashMap<String, String>,
) {
    if class(n)
        && let Some(name) = field(n, "name", s)
    {
        let owner = qualified(owner, name);
        for c in children(n) {
            extract(c, s, file, &owner, caller, &mut HashMap::new());
        }
        return;
    }
    if method(n)
        && let Some(name) = field(n, "name", s)
    {
        let caller = qualified(owner, name);
        let mut local = HashMap::new();
        for c in children(n) {
            extract(c, s, file, owner, &caller, &mut local);
        }
        return;
    }
    if n.kind() == "assignment_expression"
        && let (Some(var), Some(right)) = (field(n, "left", s), n.child_by_field_name("right"))
    {
        if right.kind() == "object_creation_expression"
            && let Some(ty) = constructed(right, s)
        {
            bindings.insert(var.into(), ty.into());
        } else {
            bindings.remove(var);
        }
    }
    let mut extra = json!({});
    let mut target = None;
    let raw = match n.kind() {
        "function_call_expression" => {
            field(n, "function", s).map(|name| name.trim_start_matches('\\').to_owned())
        }
        "object_creation_expression" => constructed(n, s).map(str::to_owned),
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let receiver = field(n, "object", s);
            if let Some(r) = receiver.filter(|r| {
                r.starts_with('$') && r.chars().skip(1).all(|c| c.is_alphanumeric() || c == '_')
            }) {
                extra["receiver"] = json!(r);
                if let Some(t) = bindings.get(r) {
                    extra["receiver_type"] = json!(t);
                    extra["receiver_resolution"] = json!("constructed_receiver");
                    extra["receiver_scope"] = json!(file.php_imports.get(t).unwrap_or(t));
                }
            }
            field(n, "name", s).map(str::to_owned)
        }
        "scoped_call_expression" => {
            if let (Some(scope), Some(member)) = (field(n, "scope", s), field(n, "name", s)) {
                let ty = scope.trim_start_matches('\\');
                let implicit = ty.eq_ignore_ascii_case("self") || ty.eq_ignore_ascii_case("static");
                let local = qualified(if implicit { owner } else { ty }, member);
                if file.syntax.declarations.contains_key(&local) {
                    target = Some(local);
                    extra["scoped_resolution"] = json!(if implicit {
                        "enclosing_class"
                    } else {
                        "same_file"
                    });
                }
                Some(format!("{ty}::{member}"))
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(raw) = raw {
        if matches!(
            n.kind(),
            "function_call_expression" | "object_creation_expression"
        ) && file.syntax.declarations.contains_key(&raw)
        {
            target = Some(raw.clone());
        }
        save(file, n, caller, raw, target, extra);
    }
    for c in children(n) {
        extract(c, s, file, owner, caller, bindings);
    }
}
fn save(
    file: &mut ParsedFile,
    n: Node<'_>,
    caller: &str,
    raw: String,
    target: Option<String>,
    extra: Value,
) {
    let line = n.start_position().row + 1;
    let key = if let Some(r) = extra["receiver"].as_str() {
        format!("{r}.{raw}")
    } else {
        raw
    };
    file.calls.push(Call {
        caller: caller.into(),
        callee: key.clone(),
        line,
    });
    file.syntax.sites.entry(line).or_default().push(Site {
        caller: caller.into(),
        raw: key,
        target,
        extra,
    });
}
fn constructed<'s>(n: Node<'_>, s: &'s str) -> Option<&'s str> {
    children(n)
        .into_iter()
        .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_scope"))
        .map(|c| text(c, s).trim_start_matches('\\'))
}
