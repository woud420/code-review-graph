use super::*;
use crate::model::{Call, syntax::Site};

impl Input<'_> {
    pub(super) fn collect_calls(
        &mut self,
        n: Node<'_>,
        class: &str,
        caller: &str,
        vars: &mut HashMap<String, String>,
    ) {
        let s = self.source;
        if named_type(n) {
            let owner =
                field(n, "name", s).map_or_else(|| class.into(), |name| qualified(class, name));
            for c in children(n) {
                self.collect_calls(c, &owner, caller, &mut HashMap::new());
            }
            return;
        }
        if methods(n) {
            let name = field(n, "name", s).unwrap_or("");
            let identity = qualified(class, name);
            let caller = self
                .file
                .symbols
                .iter()
                .find(|sym| {
                    sym.start_line == n.start_position().row + 1
                        && (sym.name == identity || sym.name.starts_with(&format!("{identity}:S")))
                })
                .map(|sym| sym.name.clone())
                .unwrap_or(identity);
            let mut locals = HashMap::new();
            for p in n
                .child_by_field_name("parameters")
                .into_iter()
                .flat_map(children)
            {
                if let (Some(name), Some(t)) = (field(p, "name", s), field(p, "type", s)) {
                    locals.insert(name.into(), base_type(t).into());
                }
            }
            for c in children(n) {
                self.collect_calls(c, class, &caller, &mut locals);
            }
            return;
        }
        if n.kind() == "local_variable_declaration"
            && let Some(t) = field(n, "type", s)
        {
            for d in children(n)
                .into_iter()
                .filter(|c| c.kind() == "variable_declarator")
            {
                if let Some(name) = field(d, "name", s) {
                    vars.insert(name.into(), base_type(t).into());
                }
            }
        }
        match n.kind() {
            "method_invocation" | "method_reference" | "object_creation_expression" => {
                self.record_call(n, class, caller, vars)
            }
            _ => {}
        }
        for c in children(n) {
            if n.kind() == "if_statement"
                && let Some(cond) = field(n, "condition", s)
            {
                let condition =
                    cond.trim_matches(|c: char| matches!(c, '(' | ')') || c.is_whitespace());
                if (condition == "false" && n.child_by_field_name("consequence") == Some(c))
                    || (condition == "true" && n.child_by_field_name("alternative") == Some(c))
                {
                    continue;
                }
            }
            self.collect_calls(c, class, caller, vars);
        }
    }
    fn record_call(
        &mut self,
        n: Node<'_>,
        class: &str,
        caller: &str,
        vars: &HashMap<String, String>,
    ) {
        let s = self.source;
        let reference = n.kind() == "method_reference";
        let constructor = n.kind() == "object_creation_expression";
        let method = if reference {
            n.named_child(1).map(|v| text(v, s))
        } else if constructor {
            field(n, "type", s).map(base_type)
        } else {
            field(n, "name", s)
        };
        let Some(method) = method else {
            return;
        };
        let obj = if reference {
            n.named_child(0)
        } else {
            n.child_by_field_name("object")
        };
        let receiver = obj.and_then(|o| match o.kind() {
            "identifier" | "this" => Some(text(o, s)),
            "field_access" if field(o, "object", s) == Some("this") => field(o, "field", s),
            _ => None,
        });
        let mut extra = json!({});
        if reference {
            extra["call_syntax"] = json!("method_reference");
        }
        let mut target = None;
        if let Some(r) = receiver {
            extra["receiver"] = json!(r);
            let ty = if r == "this" {
                Some(class)
            } else {
                vars.get(r)
                    .or_else(|| self.fields.get(&(class.into(), r.into())))
                    .map(String::as_str)
            };
            if let Some(t) = ty {
                let member = qualified(t, method);
                if self.file.syntax.declarations.contains_key(&member) {
                    extra["receiver_type"] = json!(t);
                    extra["receiver_resolution"] = json!("typed_receiver");
                    target = Some(member);
                }
            }
        }
        if target.is_none()
            && receiver.is_none()
            && !obj.is_some_and(|o| {
                !matches!(o.kind(), "this" | "super" | "object_creation_expression")
            })
        {
            target = self
                .file
                .symbols
                .iter()
                .find(|sym| sym.name.rsplit('.').next() == Some(method))
                .map(|sym| sym.name.clone());
        }
        let raw = receiver.map_or_else(|| method.to_owned(), |r| format!("{r}.{method}"));
        let line = n.start_position().row + 1;
        self.file.calls.push(Call {
            caller: caller.into(),
            callee: raw.clone(),
            line,
        });
        self.file.syntax.sites.entry(line).or_default().push(Site {
            caller: caller.into(),
            raw,
            target,
            extra,
        });
    }
}
