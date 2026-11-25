mod xml;
mod xpath;
use super::super::syntax::{call, child, edge};
use super::declare;
use crate::model::{
    ParsedFile,
    syntax::{Definition, Site},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;
use xml::{Element, Scope};
use xpath::steps;

struct Template<'t> {
    element: Element<'t>,
    id: String,
    name: String,
    modes: BTreeSet<String>,
    steps: BTreeSet<String>,
    matches: bool,
    named: bool,
}
fn clip(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

pub(super) fn extract(root: Node<'_>, s: &str, file: &mut ParsedFile) {
    if s.len() > 4_194_304 {
        file.syntax.file = json!({"xslt_truncated":true});
        return;
    }
    let Some(root) = child(root, "element") else {
        return;
    };
    let root = Element::new(root, s, &Scope::from([("xsl".into(), xml::NS.into())]));
    let mut templates = Vec::new();
    if !matches!(root.name.as_str(), "stylesheet" | "transform") {
        body(&root, s, file, None, &templates, &BTreeMap::new());
        return;
    }
    for el in root.elements(s) {
        match el.name.as_str() {
            "include" | "import" => {
                let href = el.attr("href");
                let scheme = href.split_once(':').is_some_and(|(prefix, _)| {
                    prefix.starts_with(|ch: char| ch.is_ascii_alphabetic())
                        && prefix
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"+.-".contains(&c))
                });
                if !href.is_empty() && !scheme {
                    edge(file, el.location(), "IMPORTS_FROM", "<module>", &href);
                    file.syntax.edges.last_mut().unwrap().extra = json!({"xslt_import":el.name});
                }
            }
            "function" | "template" => {
                let declared = el.attr("name");
                let pattern = el.attr("match");
                let mode = el.attr("mode");
                if declared.is_empty() && pattern.is_empty() {
                    continue;
                }
                let kind = if el.name == "function" {
                    "function"
                } else if !declared.is_empty() {
                    "named_template"
                } else {
                    "match_template"
                };
                let mut extra = json!({"xslt_kind":kind});
                if !pattern.is_empty() {
                    extra["match"] = json!(clip(&pattern, 512));
                }
                if !mode.is_empty() {
                    extra["mode"] = json!(clip(&mode, 512));
                }
                let priority = el.attr("priority");
                if !priority.is_empty() {
                    extra["priority"] = json!(clip(&priority, 32));
                }
                let name = clip(
                    &if !declared.is_empty() {
                        declared
                    } else if mode.is_empty() {
                        format!("match={pattern}")
                    } else {
                        format!("match={pattern} mode={mode}")
                    },
                    200,
                );
                let mut id = name.clone();
                if file.syntax.declarations.contains_key(&id) {
                    id = format!("{name}@L{}", el.location().start_position().row + 1);
                    if file.syntax.declarations.contains_key(&id) {
                        id = format!("{id}C{}", el.location().start_position().column + 1);
                    }
                }
                let params: Vec<_> = el
                    .elements(s)
                    .iter()
                    .filter(|e| e.name == "param")
                    .map(|e| e.attr("name"))
                    .collect();
                file.syntax.display_names.insert(id.clone(), name.clone());
                declare(
                    file,
                    el.location(),
                    &id,
                    "Function",
                    Definition {
                        params: (!params.is_empty()).then(|| format!("({})", params.join(", "))),
                        extra,
                        ..Default::default()
                    },
                );
                let modes = if mode.is_empty() {
                    BTreeSet::from(["#default".into()])
                } else {
                    mode.split_whitespace().map(str::to_owned).collect()
                };
                templates.push(Template {
                    element: el,
                    name,
                    id,
                    modes,
                    steps: steps(&pattern),
                    matches: kind != "function" && !pattern.is_empty(),
                    named: kind == "named_template",
                });
            }
            _ => {}
        }
    }
    let named = templates
        .iter()
        .filter(|t| t.named)
        .map(|t| (t.name.as_str(), t.id.as_str()))
        .collect();
    for t in &templates {
        body(&t.element, s, file, Some(t), &templates, &named);
    }
}

fn body(
    el: &Element<'_>,
    s: &str,
    file: &mut ParsedFile,
    owner: Option<&Template<'_>>,
    templates: &[Template<'_>],
    named: &BTreeMap<&str, &str>,
) {
    let caller = owner.map_or("<module>", |t| &t.id);
    let mut stack = el.elements(s);
    while let Some(e) = stack.pop() {
        match e.name.as_str() {
            "call-template" => {
                let name = e.attr("name");
                if !name.is_empty() {
                    site(
                        file,
                        e.location(),
                        caller,
                        &name,
                        named.get(name.as_str()).copied(),
                        json!({"xslt_call":"call-template"}),
                    );
                }
            }
            "apply-templates" => apply(&e, file, owner, templates),
            _ => {}
        }
        stack.extend(e.elements(s));
    }
}

fn site(
    file: &mut ParsedFile,
    n: Node<'_>,
    caller: &str,
    name: &str,
    target: Option<&str>,
    extra: Value,
) {
    call(file, n, caller, name);
    file.syntax
        .sites
        .entry(n.start_position().row + 1)
        .or_default()
        .push(Site {
            caller: caller.into(),
            raw: name.into(),
            target: target.map(str::to_owned),
            extra,
        });
}

fn apply(
    e: &Element<'_>,
    file: &mut ParsedFile,
    owner: Option<&Template<'_>>,
    templates: &[Template<'_>],
) {
    let select = e.attr("select");
    let mode = e.attr("mode");
    let modes = if mode == "#current"
        && let Some(t) = owner
    {
        t.modes.clone()
    } else {
        BTreeSet::from([if mode.is_empty() || mode == "#current" {
            "#default".into()
        } else {
            mode.clone()
        }])
    };
    let in_mode: Vec<_> = templates
        .iter()
        .filter(|t| t.matches && (t.modes.contains("#all") || !t.modes.is_disjoint(&modes)))
        .collect();
    let steps = steps(&select);
    let available: Vec<_> = if !steps.is_empty() {
        in_mode
            .into_iter()
            .filter(|t| !t.steps.is_disjoint(&steps))
            .collect()
    } else if !mode.is_empty() && mode != "#default" && in_mode.len() <= 20 {
        in_mode
    } else {
        vec![]
    };
    let mut extra = json!({"xslt_call":"apply-templates"});
    if !select.is_empty() {
        extra["select"] = json!(clip(&select, 512));
    }
    let shown = modes.into_iter().collect::<Vec<_>>().join(" ");
    if shown != "#default" {
        extra["mode"] = json!(clip(&shown, 512));
    }
    let caller = owner.map_or("<module>", |t| &t.id);
    if !available.is_empty() {
        for t in available {
            site(
                file,
                e.location(),
                caller,
                &t.id,
                Some(&t.id),
                extra.clone(),
            );
        }
    } else if !steps.is_empty() {
        let suffix = if shown == "#default" {
            String::new()
        } else {
            format!(" mode={shown}")
        };
        for name in steps {
            site(
                file,
                e.location(),
                caller,
                &format!("match={name}{suffix}"),
                None,
                extra.clone(),
            );
        }
    } else if let Some(t) = owner {
        let data = &mut file.syntax.declarations.get_mut(&t.id).unwrap().extra;
        data["dynamic_apply_count"] = json!(data["dynamic_apply_count"].as_u64().unwrap_or(0) + 1);
    }
}
