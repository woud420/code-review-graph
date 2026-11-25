use super::syntax::{direct_target, field, record};
use crate::model::{
    Call, Export, Import, ParsedFile, PythonDefinition, PythonMember, PythonReceiver, PythonType,
    ScopedImport,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn decorators<'s>(node: Node<'_>, source: &'s str) -> Vec<&'s str> {
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
    else {
        return Vec::new();
    };
    children(parent)
        .into_iter()
        .filter(|n| n.kind() == "decorator")
        .map(|n| text(n, source).trim_start_matches('@').trim())
        .collect()
}

#[derive(Default)]
struct Frame {
    start: usize,
    end: usize,
    bindings: HashMap<String, Option<PythonType>>,
    receivers: HashMap<String, Option<PythonType>>,
    declared: HashMap<String, String>,
    class_scope: bool,
}

impl Frame {
    fn bind(&mut self, name: &str, evidence: Option<PythonType>) {
        self.receivers.insert(
            name.strip_prefix("self.").unwrap_or(name).to_owned(),
            evidence.clone(),
        );
        self.bindings.insert(name.to_owned(), evidence);
    }
}

struct Reader<'s> {
    source: &'s str,
    facts: ParsedFile,
    frames: Vec<Frame>,
    fields: HashMap<(String, String), Option<PythonType>>,
    identities: HashSet<String>,
    last_member_at: HashMap<(String, usize, String), String>,
}

impl Reader<'_> {
    fn define(&mut self, node: Node<'_>, class: &str, name: &str, kind: &str) -> String {
        let base = if class.is_empty() {
            name.to_owned()
        } else {
            format!("{class}.{name}")
        };
        let parent = (!class.is_empty()).then(|| class.to_owned());
        let params = field(node, "parameters", self.source).map(str::to_owned);
        let returns = field(node, "return_type", self.source).map(str::to_owned);
        let decorators = decorators(node, self.source);
        let modifiers = (!decorators.is_empty()).then(|| decorators.join(","));
        let mut qualified = base.clone();
        if !self.identities.insert(base.clone()) {
            self.facts.python.ambiguities.insert(base.clone());
            let payload = serde_json::to_string(&(
                kind,
                name,
                &parent,
                "python",
                &params,
                &returns,
                &modifiers,
                kind == "Test",
            ))
            .unwrap();
            let ascii: String = payload
                .chars()
                .map(|c| {
                    if c.is_ascii() {
                        c.to_string()
                    } else {
                        c.encode_utf16(&mut [0; 2])
                            .iter()
                            .map(|u| format!("\\u{u:04x}"))
                            .collect()
                    }
                })
                .collect();
            let digest = Sha256::digest(ascii.as_bytes())[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let semantic = format!("{base}:S{}", &digest[..12]);
            qualified = semantic.clone();
            let mut ordinal = 2;
            while !self.identities.insert(qualified.clone()) {
                qualified = format!("{semantic}:{ordinal}");
                ordinal += 1;
            }
        }
        let bases = node
            .child_by_field_name("superclasses")
            .into_iter()
            .flat_map(children)
            .filter(|n| matches!(n.kind(), "attribute" | "identifier"))
            .map(|n| text(n, self.source).to_owned())
            .collect();
        self.facts.python.definitions.insert(
            qualified.clone(),
            PythonDefinition {
                module_level: self.frames.len() == 1,
                parent,
                params,
                returns,
                modifiers,
                bases,
                decorators: decorators.into_iter().map(str::to_owned).collect(),
            },
        );
        record(&mut self.facts, node, &qualified, kind);
        qualified
    }

    fn export(&mut self, name: &str, local: &str) {
        self.facts.exports.push(Export {
            name: name.to_owned(),
            local: local.to_owned(),
            module: None,
        });
    }

    fn imports(&mut self, node: Node<'_>) {
        let from = field(node, "module_name", self.source);
        let mut cursor = node.walk();
        for entry in node.children_by_field_name("name", &mut cursor) {
            let name = field(entry, "name", self.source).unwrap_or(text(entry, self.source));
            let local = field(entry, "alias", self.source).unwrap_or(name);
            let binding = Import {
                local: local.to_owned(),
                imported: from.map_or("*", |_| name).to_owned(),
                module: from.unwrap_or(name).to_owned(),
            };
            if self.frames.len() == 1 {
                self.export(local, local);
                self.facts.imports.push(binding);
            } else {
                let frame = self.frames.last().unwrap();
                self.facts.python.scoped_imports.push(ScopedImport {
                    start: frame.start,
                    end: frame.end,
                    binding,
                });
            }
        }
    }

    fn evidence(&self, node: Node<'_>, class: &str) -> Option<PythonType> {
        let expr = text(node, self.source).trim();
        if let Some(name) = expr.strip_prefix("self.") {
            if let Some(ty) = self.frames.last().and_then(|f| f.bindings.get(expr)) {
                return ty.clone();
            }
            return self
                .fields
                .get(&(class.to_owned(), name.to_owned()))
                .cloned()
                .flatten();
        }
        match node.kind() {
            "identifier" => {
                if matches!(expr, "self" | "cls") && !class.is_empty() {
                    return Some(PythonType::Named(class.to_owned()));
                }
                for frame in self.frames.iter().rev() {
                    if let Some(ty) = frame.bindings.get(expr) {
                        return ty.clone();
                    }
                }
                None
            }
            "call" => {
                let fun = node.child_by_field_name("function")?;
                Some(PythonType::Returned {
                    callee: direct_target(fun, self.source, class)?,
                    element: None,
                })
            }
            "concatenated_string"
            | "dictionary"
            | "dictionary_comprehension"
            | "false"
            | "float"
            | "generator_expression"
            | "integer"
            | "list"
            | "list_comprehension"
            | "set"
            | "set_comprehension"
            | "string"
            | "true"
            | "tuple" => Some(PythonType::Builtin),
            "parenthesized_expression" => self
                .evidence(node.named_child(0)?, class)
                .filter(|ty| !matches!(ty, PythonType::Builtin)),
            _ => None,
        }
    }

    fn collect_binding(&mut self, node: Node<'_>, class: &str) {
        let (left, value, annotation) = match node.kind() {
            "assignment" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
                field(node, "type", self.source),
            ),
            "typed_parameter" | "typed_default_parameter" => (
                node.child_by_field_name("name")
                    .or_else(|| node.named_child(0)),
                None,
                field(node, "type", self.source),
            ),
            "as_pattern" => (node.child_by_field_name("alias"), node.named_child(0), None),
            _ => return,
        };
        let Some(lhs) = left else {
            return;
        };
        if value.is_some_and(|v| v.kind() == "none") && annotation.is_none() {
            return;
        }
        let ty = annotation
            .map(|s| {
                if node.kind().contains("parameter") {
                    PythonType::Parameter(s.to_owned())
                } else {
                    PythonType::Named(s.to_owned())
                }
            })
            .or_else(|| {
                let value = value?;
                if value.kind() == "identifier" {
                    None
                } else {
                    self.evidence(value, class)
                }
            });
        if matches!(lhs.kind(), "pattern_list" | "tuple_pattern") {
            for (i, field) in children(lhs).into_iter().enumerate() {
                let component = match &ty {
                    Some(PythonType::Returned { callee, .. }) => Some(PythonType::Returned {
                        callee: callee.clone(),
                        element: Some(i),
                    }),
                    _ => None,
                };
                self.frames
                    .last_mut()
                    .unwrap()
                    .bind(text(field, self.source), component);
            }
        } else {
            let name = text(lhs, self.source);
            if let Some(ty_name) = annotation
                && !name.contains('.')
            {
                self.frames
                    .last_mut()
                    .unwrap()
                    .declared
                    .insert(name.to_owned(), ty_name.to_owned());
            }
            let class_field = if self.frames.last().unwrap().class_scope {
                Some(name)
            } else {
                name.strip_prefix("self.")
            };
            if let Some(field) = class_field
                && !field.contains('.')
                && let Some(ty) = &ty
            {
                let types = self
                    .facts
                    .python
                    .fields
                    .entry(format!("{class}.{field}"))
                    .or_default();
                if !types.contains(ty) {
                    types.push(ty.clone());
                }
            }
            if let Some(field) = name.strip_prefix("self.")
                && !field.contains('.')
            {
                self.frames.last_mut().unwrap().bind(name, ty.clone());
                let entry = self
                    .fields
                    .entry((class.to_owned(), field.to_owned()))
                    .or_insert_with(|| ty.clone());
                if *entry != ty {
                    let same = match (entry.as_ref(), ty.as_ref()) {
                        (
                            Some(PythonType::Named(n)),
                            Some(PythonType::Returned {
                                callee,
                                element: None,
                            }),
                        ) => n
                            .split('|')
                            .map(str::trim)
                            .filter(|s| *s != "None")
                            .map(|s| s.split('[').next().unwrap_or(s))
                            .eq(std::iter::once(callee.as_str())),
                        _ => false,
                    };
                    if !same {
                        *entry = None;
                    }
                }
            } else {
                let prior = self.evidence(lhs, class);
                let same_type = match (&prior, &ty) {
                    (
                        Some(PythonType::Named(owner) | PythonType::Parameter(owner)),
                        Some(PythonType::Returned {
                            callee,
                            element: None,
                        }),
                    ) => callee.rsplit_once('.').is_some_and(|(receiver, method)| {
                        receiver == name
                            && self
                                .facts
                                .python
                                .definitions
                                .get(&format!("{owner}.{method}"))
                                .and_then(|definition| definition.returns.as_deref())
                                .map(|ret| ret.trim_matches(['\'', '"']))
                                .is_some_and(|ret| {
                                    ret == owner || matches!(ret, "Self" | "typing.Self")
                                })
                    }),
                    _ => false,
                };
                self.frames
                    .last_mut()
                    .unwrap()
                    .bind(name, if same_type { prior } else { ty });
            }
        }
    }

    fn visit(&mut self, node: Node<'_>, class: &str, caller: &str) {
        match node.kind() {
            "function_definition" | "class_definition" => {
                let Some(name) = field(node, "name", self.source) else {
                    return;
                };
                let is_class = node.kind() == "class_definition";
                let kind = if is_class {
                    "Class"
                } else if name.starts_with("test_")
                    || name.starts_with("Test")
                    || name.ends_with("_test")
                    || name.ends_with("_spec")
                    || decorators(node, self.source).iter().any(|n| {
                        matches!(
                            *n,
                            "test" | "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory"
                        )
                    })
                {
                    "Test"
                } else {
                    "Function"
                };
                let id = self.define(node, class, name, kind);
                if self.frames.len() == 1 && class.is_empty() {
                    self.export(name, &id);
                }
                let frame = Frame {
                    class_scope: is_class,
                    start: node.start_position().row + 1,
                    end: node.end_position().row + 1,
                    ..Default::default()
                };
                self.frames.push(frame);
                for child in children(node) {
                    self.visit(
                        child,
                        if is_class { name } else { class },
                        if is_class { "<module>" } else { &id },
                    );
                }
                self.frames.pop();
                return;
            }
            "import_statement" | "import_from_statement" => self.imports(node),
            "assignment" => {
                for child in children(node) {
                    self.visit(child, class, caller);
                }
                self.collect_binding(node, class);
                return;
            }
            "typed_parameter" | "typed_default_parameter" | "as_pattern" => {
                self.collect_binding(node, class)
            }
            "attribute" => {
                if node.parent().is_some_and(|p| {
                    p.kind() == "call" && p.child_by_field_name("function") == Some(node)
                }) && let Some(name) = field(node, "attribute", self.source)
                {
                    self.last_member_at.insert(
                        (
                            caller.to_owned(),
                            node.start_position().row + 1,
                            name.to_owned(),
                        ),
                        text(node, self.source).to_owned(),
                    );
                }
                if let (Some(receiver), Some(member)) = (
                    node.child_by_field_name("object"),
                    field(node, "attribute", self.source),
                ) && let Some(ty) = self.evidence(receiver, class)
                {
                    let property = !node.parent().is_some_and(|p| {
                        p.kind() == "call" && p.child_by_field_name("function") == Some(node)
                    });
                    self.facts.python.members.push(PythonMember {
                        caller: caller.to_owned(),
                        raw: text(node, self.source).to_owned(),
                        attr: member.to_owned(),
                        receiver: ty,
                        line: node.start_position().row + 1,
                        property,
                    });
                }
            }
            "call" => {
                if let Some(fun) = node.child_by_field_name("function")
                    && let Some(callee) = direct_target(fun, self.source, class).or_else(|| {
                        field(fun, "attribute", self.source)
                            .map(|method| format!("<expression>.{method}"))
                    })
                {
                    if let Some(object) = fun.child_by_field_name("object") {
                        let name = match object.kind() {
                            "identifier" => Some(text(object, self.source).to_owned()),
                            "attribute" if field(object, "object", self.source) == Some("self") => {
                                field(object, "attribute", self.source).map(str::to_owned)
                            }
                            _ => None,
                        };
                        let declared = name
                            .as_deref()
                            .and_then(|n| self.frames.iter().rev().find_map(|f| f.declared.get(n)))
                            .cloned();
                        let receiver_type = if object.kind() == "attribute" {
                            name.as_deref()
                                .and_then(|name| self.frames.last()?.receivers.get(name).cloned())
                                .unwrap_or_else(|| self.evidence(object, class))
                        } else {
                            self.evidence(object, class)
                        };
                        self.facts.python.receivers.push(PythonReceiver {
                            declared,
                            caller: caller.to_owned(),
                            line: node.start_position().row + 1,
                            raw: callee.clone(),
                            name,
                            binding: receiver_type,
                        });
                    }
                    self.facts.calls.push(Call {
                        caller: caller.to_owned(),
                        callee,
                        line: node.start_position().row + 1,
                    });
                }
            }
            _ => {}
        }
        for child in children(node) {
            self.visit(child, class, caller);
        }
    }
}

pub(super) fn extract(root: Node<'_>, source: &str) -> ParsedFile {
    let mut reader = Reader {
        source,
        facts: ParsedFile::default(),
        frames: vec![Frame::default()],
        fields: HashMap::new(),
        identities: HashSet::new(),
        last_member_at: HashMap::new(),
    };
    reader.visit(root, "", "<module>");
    // Python's pre-resolution edge identity contains the line and bare method.
    // Calls resolved only by return-type evidence inherit the last site's receiver.
    reader.facts.python.members.retain(|m| {
        m.property
            || !matches!(m.receiver, PythonType::Returned { .. })
            || reader
                .last_member_at
                .get(&(m.caller.clone(), m.line, m.attr.clone()))
                == Some(&m.raw)
    });
    for member in &mut reader.facts.python.members {
        if !member
            .raw
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c == '_' || c.is_alphanumeric()))
        {
            member.raw = format!("<expression>.{}", member.attr);
        }
    }
    reader.facts
}
