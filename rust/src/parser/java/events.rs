use super::{Input, child, children, field, text};
use serde_json::{Value, json};
use std::collections::HashSet;
use tree_sitter::Node;

fn type_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    if matches!(node.kind(), "type_identifier" | "scoped_type_identifier") {
        Some(text(node, source))
    } else {
        children(node)
            .into_iter()
            .find_map(|c| type_name(c, source))
    }
}
fn class_literals<'s>(node: Node<'_>, source: &'s str, names: &mut Vec<&'s str>) {
    if node.kind() == "class_literal"
        && let Some(name) = type_name(node, source)
    {
        names.push(name);
    }
    for c in children(node) {
        class_literals(c, source, names);
    }
}
impl Input<'_> {
    pub(super) fn events(&mut self, method: Node<'_>, owner: &str, extra: &mut Value) {
        let mut count = 0;
        for annotation in child(method, "modifiers")
            .into_iter()
            .flat_map(children)
            .filter(|n| {
                field(*n, "name", self.source)
                    .is_some_and(|name| name.rsplit('.').next() == Some("EventListener"))
            })
        {
            let mut names = Vec::new();
            class_literals(annotation, self.source, &mut names);
            if names.is_empty()
                && let Some(name) = child(method, "formal_parameters")
                    .and_then(|p| child(p, "formal_parameter"))
                    .and_then(|p| type_name(p, self.source))
            {
                names.push(name);
            }
            let mut seen = HashSet::new();
            for name in names {
                let id = self.type_identity(name, false);
                if !seen.insert(id.clone()) {
                    continue;
                }
                let mut data = json!({"event_type":id,"resolution":"spring_event_listener"});
                for pair in annotation
                    .child_by_field_name("arguments")
                    .into_iter()
                    .flat_map(children)
                {
                    if let (Some(key @ ("condition" | "id" | "defaultExecution")), Some(v)) = (
                        field(pair, "key", self.source),
                        pair.child_by_field_name("value"),
                    ) {
                        let raw = text(v, self.source);
                        data[key] = json!(if v.kind() == "string_literal" && raw.len() >= 2 {
                            &raw[1..raw.len() - 1]
                        } else {
                            raw
                        });
                    }
                }
                self.edge("HANDLES", owner, &format!("event::{id}"), annotation, data);
                count += 1;
            }
        }
        if count > 0 {
            extra["spring_event_listener"] = true.into();
            extra["spring_event_type_count"] = count.into();
        }
        if let Some(body) = child(method, "block") {
            let count = self.publishes(body, owner);
            if count > 0 {
                extra["spring_event_publisher"] = true.into();
                extra["spring_event_publish_count"] = count.into();
            }
        }
    }
    fn publishes(&mut self, n: Node<'_>, owner: &str) -> usize {
        let mut count = 0;
        if n.kind() == "method_invocation" && field(n, "name", self.source) == Some("publishEvent")
        {
            let receiver = n
                .child_by_field_name("object")
                .and_then(|r| match r.kind() {
                    "identifier" => Some(text(r, self.source)).filter(|&s| s != "publishEvent"),
                    "field_access"
                        if r.child_by_field_name("object")
                            .is_some_and(|o| o.kind() == "this") =>
                    {
                        field(r, "field", self.source)
                    }
                    _ => None,
                });
            for arg in n
                .child_by_field_name("arguments")
                .into_iter()
                .flat_map(children)
                .filter(|c| c.kind() == "object_creation_expression")
            {
                if let Some(name) = type_name(arg, self.source) {
                    let id = self.type_identity(name, false);
                    let mut extra = json!({"event_type":id,"resolution":"spring_publish_event"});
                    if let Some(r) = receiver {
                        extra["receiver"] = r.into();
                    }
                    self.edge("PUBLISHES", owner, &format!("event::{id}"), n, extra);
                    count += 1;
                }
            }
        }
        count
            + children(n)
                .into_iter()
                .map(|c| self.publishes(c, owner))
                .sum::<usize>()
    }
}
