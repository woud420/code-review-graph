use super::*;
use crate::model::{Call, syntax::Site};

impl Input<'_> {
    pub(super) fn calls(
        &mut self,
        n: Node<'_>,
        class: &str,
        caller: &str,
        locals: &mut HashMap<String, String>,
    ) {
        let s = self.source;
        if is_type(n) {
            let owner = self.type_name(n, class);
            for c in children(n) {
                self.calls(c, &owner, caller, &mut HashMap::new());
            }
            return;
        }
        if is_method(n) {
            let identity = self.type_name(n, class);
            let mut bindings = HashMap::new();
            for p in n
                .child_by_field_name("parameters")
                .into_iter()
                .flat_map(children)
            {
                if let (Some(name), Some(ty)) = (named(p, "name", s), named(p, "type", s)) {
                    bindings.insert(name, ty);
                }
            }
            for c in children(n) {
                self.calls(c, class, &identity, &mut bindings);
            }
            return;
        }
        if n.kind() == "variable_declaration"
            && let Some(ty) = named(n, "type", s)
        {
            for d in children(n)
                .into_iter()
                .filter(|d| d.kind() == "variable_declarator")
            {
                if let Some(name) = named(d, "name", s) {
                    locals.insert(name, ty.clone());
                }
            }
        }
        if n.kind() == "invocation_expression"
            && let Some(f) = n.child_by_field_name("function")
        {
            self.invocation(n, f, class, caller, locals);
        }
        for c in children(n) {
            self.calls(c, class, caller, locals);
        }
    }
    fn invocation(
        &mut self,
        n: Node<'_>,
        f: Node<'_>,
        class: &str,
        caller: &str,
        locals: &HashMap<String, String>,
    ) {
        let s = self.source;
        let raw_member = if f.kind() == "identifier" {
            compact(text(f, s))
        } else if let Some(name) = named(f, "name", s) {
            name
        } else {
            return;
        };
        let mut key = raw_member.clone();
        let mut extra = json!({"source_offset":n.start_byte(),"csharp_scopes":scopes(n,s),"csharp_containing_type":(!class.is_empty()).then_some(class),"csharp_call_kind":if f.kind()=="identifier"{"unqualified"}else{"member"},"unresolved_targets":[]});
        if let Some(o) = f.child_by_field_name("expression") {
            let obj = compact(text(o, s));
            let receiver = obj.strip_prefix("this.").unwrap_or(&obj);
            if receiver != "base"
                && (o.kind() == "identifier" || obj == "this" || obj.starts_with("this."))
            {
                extra["receiver"] = json!(receiver);
                let ty = locals
                    .get(receiver)
                    .or_else(|| self.fields.get(&(class.into(), receiver.into())));
                if let Some(t) = ty.map(String::as_str).or_else(|| {
                    (receiver.starts_with(char::is_uppercase) || receiver == "this")
                        .then_some(receiver)
                }) {
                    extra["receiver_type"] = json!(t);
                    extra["receiver_resolution"] = json!(if ty.is_some() {
                        "typed_receiver"
                    } else {
                        "class_receiver"
                    });
                    extra["receiver_scope"] = json!(t);
                    key = format!("{t}::{raw_member}");
                }
            }
        }
        extra["csharp_raw_target"] = json!(key);
        let raw = format!("{key}@{}", n.start_byte());
        let line = n.start_position().row + 1;
        self.file.calls.push(Call {
            caller: caller.into(),
            callee: raw.clone(),
            line,
        });
        self.file.syntax.sites.entry(line).or_default().push(Site {
            caller: caller.into(),
            raw,
            target: None,
            extra,
        });
    }
}
