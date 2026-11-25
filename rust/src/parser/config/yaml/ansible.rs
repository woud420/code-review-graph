use super::*;

fn meta(key: &str) -> bool {
    key.starts_with("with_")
        || matches!(
            key,
            "name"
                | "when"
                | "loop"
                | "loop_control"
                | "register"
                | "notify"
                | "tags"
                | "become"
                | "become_user"
                | "become_method"
                | "ignore_errors"
                | "vars"
                | "no_log"
                | "check_mode"
                | "environment"
                | "any_errors_fatal"
                | "run_once"
                | "delegate_to"
                | "delegate_facts"
                | "block"
                | "rescue"
                | "always"
                | "changed_when"
                | "failed_when"
                | "retries"
                | "delay"
                | "until"
                | "listen"
                | "connection"
                | "timeout"
        )
}

fn play(item: Node<'_>, text: &str) -> bool {
    value(item, text, "import_playbook").is_some()
        || (value(item, text, "hosts").is_some()
            && entries(item)
                .into_iter()
                .filter_map(|(k, _)| scalar(k, text))
                .any(|k| {
                    matches!(
                        k,
                        "tasks"
                            | "handlers"
                            | "pre_tasks"
                            | "post_tasks"
                            | "roles"
                            | "gather_facts"
                            | "become"
                            | "become_user"
                            | "become_method"
                            | "serial"
                            | "strategy"
                            | "vars_files"
                            | "vars_prompt"
                            | "any_errors_fatal"
                            | "max_fail_percentage"
                            | "ignore_errors"
                    )
                }))
}

fn marker(text: &str) -> bool {
    let Some(first) = text.lines().find(|s| {
        let v = s.trim();
        !v.is_empty() && v != "---" && !v.starts_with('#')
    }) else {
        return false;
    };
    if !(first.starts_with("- ") || first == "-") {
        return false;
    }
    let mut name = false;
    let mut module = false;
    for l in text.lines().filter(|l| l.starts_with(char::is_whitespace)) {
        let Some((key, _)) = l.trim_start().split_once(':') else {
            continue;
        };
        let key = key.trim().trim_start_matches("- ");
        if matches!(key, "hosts" | "import_playbook" | "tasks" | "handlers") {
            return true;
        }
        name |= key == "name";
        module |= key.starts_with("ansible.")
            || matches!(
                key,
                "apt"
                    | "yum"
                    | "dnf"
                    | "package"
                    | "pip"
                    | "copy"
                    | "template"
                    | "file"
                    | "service"
                    | "systemd"
                    | "command"
                    | "shell"
                    | "raw"
                    | "git"
                    | "user"
                    | "stat"
                    | "include_tasks"
                    | "import_tasks"
                    | "include_role"
                    | "import_role"
                    | "set_fact"
                    | "debug"
                    | "fail"
                    | "assert"
                    | "wait_for"
                    | "pause"
                    | "lineinfile"
                    | "blockinfile"
                    | "get_url"
                    | "uri"
                    | "unarchive"
                    | "add_host"
                    | "group_by"
                    | "include_vars"
            );
    }
    name && module
}
fn arg<'a>(node: Node<'_>, text: &'a str, keys: &[&str]) -> Option<&'a str> {
    scalar(node, text)
        .or_else(|| {
            keys.iter()
                .find_map(|k| value(node, text, k).and_then(|v| scalar(v, text)))
        })
        .filter(|s| !s.is_empty())
}

impl Collector<'_> {
    fn ansible_node(
        &mut self,
        node: Node<'_>,
        label: &str,
        kind: &str,
        owner: &str,
        extra: Value,
    ) -> String {
        let join = |n: &str| {
            if owner == "<module>" {
                n.to_owned()
            } else {
                format!("{owner}.{n}")
            }
        };
        let mut name = label.to_string();
        if self.file.yaml.declarations.contains_key(&join(&name)) {
            name = format!("{label}@line{}", line(node));
            let base = name.clone();
            let mut suffix = 2;
            while self.file.yaml.declarations.contains_key(&join(&name)) {
                name = format!("{base}.{suffix}");
                suffix += 1;
            }
        }
        let id = join(&name);
        self.declare(
            Symbol {
                name: id.clone(),
                kind: kind.into(),
                start_line: line(node),
                end_line: last_line(node, self.text),
            },
            Declaration {
                name,
                parent: (owner != "<module>").then(|| owner.into()),
                extra,
            },
            owner,
        );
        id
    }
    fn tasks(&mut self, seq: Node<'_>, owner: &str, handlers: bool) {
        for node in items(seq) {
            let pairs = entries(node);
            if pairs.is_empty() {
                continue;
            }
            let (module, args) = pairs
                .iter()
                .find_map(|(k, v)| scalar(*k, self.text).filter(|k| !meta(k)).map(|s| (s, *v)))
                .unwrap_or(("", None));
            let short = module.rsplit('.').next().unwrap_or("");
            let label = value(node, self.text, "name")
                .and_then(|v| scalar(v, self.text))
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "{}@line{}",
                        if short.is_empty() { "task" } else { short },
                        line(node)
                    )
                });
            let mut extra = json!({"ansible_kind":if handlers{"handler"}else{"task"},"ansible_module":module,"ansible_name":label});
            if handlers
                && let Some(listen) =
                    value(node, self.text, "listen").and_then(|v| scalar(v, self.text))
            {
                extra["ansible_listen"] = json!(listen);
            }
            let id = self.ansible_node(node, &label, "Function", owner, extra);
            if let Some(notify) = value(node, self.text, "notify") {
                for t in std::iter::once(notify)
                    .chain(items(notify))
                    .filter_map(|n| scalar(n, self.text))
                    .filter(|s| !s.is_empty())
                {
                    self.edge(
                        "CALLS",
                        &id,
                        t,
                        false,
                        line(notify),
                        json!({"ansible_kind":"notify"}),
                    );
                }
            }
            let keys: &[&str] = match short {
                "import_tasks" | "include_tasks" | "include_vars" => &["file", "_raw_params"],
                "import_role" | "include_role" => &["name"],
                _ => &[],
            };
            if !keys.is_empty()
                && let Some(target) = args.and_then(|n| arg(n, self.text, keys))
            {
                self.edge(
                    "IMPORTS_FROM",
                    &id,
                    target,
                    false,
                    line(node),
                    json!({"ansible_kind":short}),
                );
            }
            for k in ["block", "rescue", "always"] {
                if let Some(children) = value(node, self.text, k) {
                    self.tasks(children, owner, handlers);
                }
            }
        }
    }

    fn playbook(&mut self, doc: Node<'_>) {
        for node in items(doc) {
            if let Some(p) =
                value(node, self.text, "import_playbook").and_then(|v| scalar(v, self.text))
            {
                self.edge(
                    "IMPORTS_FROM",
                    "<module>",
                    p,
                    false,
                    line(node),
                    json!({"ansible_kind":"import_playbook"}),
                );
                continue;
            }
            if !play(node, self.text) {
                continue;
            }
            let label = value(node, self.text, "name")
                .and_then(|v| scalar(v, self.text))
                .map(str::to_owned)
                .or_else(|| {
                    value(node, self.text, "hosts")
                        .and_then(|v| scalar(v, self.text))
                        .map(|s| format!("play[{s}]"))
                })
                .unwrap_or_else(|| format!("play@line{}", line(node)));
            let owner = self.ansible_node(
                node,
                &label,
                "Class",
                "<module>",
                json!({"ansible_kind":"play"}),
            );
            for (k, kind, keys) in [
                ("vars_files", "vars_files", &[][..]),
                ("roles", "role_reference", &["role", "name"][..]),
            ] {
                if let Some(values) = value(node, self.text, k) {
                    for n in items(values) {
                        if let Some(target) = arg(n, self.text, keys) {
                            self.edge(
                                "IMPORTS_FROM",
                                &owner,
                                target,
                                false,
                                line(n),
                                json!({"ansible_kind":kind}),
                            );
                        }
                    }
                }
            }
            for k in ["pre_tasks", "tasks", "post_tasks", "handlers"] {
                if let Some(seq) = value(node, self.text, k) {
                    self.tasks(seq, &owner, k == "handlers");
                }
            }
        }
    }
    fn bind_handlers(&mut self) {
        use std::collections::HashMap;
        let mut names = HashMap::new();
        for (id, d) in &self.file.yaml.declarations {
            if d.extra["ansible_kind"] != "handler" {
                continue;
            }
            let labels: HashSet<_> = [
                Some(d.name.as_str()),
                d.extra["ansible_name"].as_str(),
                d.extra["ansible_listen"].as_str(),
            ]
            .into_iter()
            .flatten()
            .collect();
            for label in labels {
                names
                    .entry((&d.parent, label))
                    .or_insert_with(Vec::new)
                    .push(id);
            }
        }
        for edge in &mut self.file.yaml.edges {
            if edge.kind != "CALLS" {
                continue;
            }
            let parent = &self.file.yaml.declarations[&edge.source].parent;
            if let Some(v) = names.get(&(parent, edge.target.as_str()))
                && v.len() == 1
            {
                edge.target = v[0].clone();
                edge.local = true;
            }
            self.file.calls.push(Call {
                caller: edge.source.clone(),
                callee: edge.target.clone(),
                line: edge.line,
            });
        }
    }
}

pub(super) fn parse(path: &Path, doc: Node<'_>, out: &mut Collector<'_>) -> bool {
    let parts: Vec<_> = path
        .components()
        .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let name = parts.last().map(String::as_str).unwrap_or("");
    let has = |v: &str| parts.iter().any(|p| p == v);
    let layout = parts.iter().any(|p| {
        matches!(
            p.as_str(),
            "playbooks" | "roles" | "tasks" | "handlers" | "host_vars" | "group_vars"
        )
    }) || matches!(
        name.split('.').next(),
        Some("site" | "main" | "install" | "deploy")
    );
    let role_meta = layout && has("meta") && matches!(name, "main.yml" | "main.yaml");
    let vars = layout
        && ["group_vars", "host_vars", "defaults", "vars"]
            .iter()
            .any(|k| has(k));
    if !(layout || name.starts_with("application.") || name.starts_with("application-"))
        || (!role_meta && !vars && !marker(out.text))
    {
        return false;
    }
    out.file.yaml.language = "ansible".into();
    out.file.yaml.file = json!({});
    if role_meta {
        if let Some(deps) = value(doc, out.text, "dependencies") {
            for n in items(deps) {
                if let Some(target) = arg(n, out.text, &["role", "name"]) {
                    out.edge(
                        "DEPENDS_ON",
                        "<module>",
                        target,
                        false,
                        line(n),
                        json!({"ansible_kind":"role_dependency"}),
                    );
                }
            }
        }
    } else if !vars {
        if !has("tasks") && !has("handlers") && items(doc).into_iter().any(|n| play(n, out.text)) {
            out.playbook(doc);
        } else {
            out.tasks(doc, "<module>", has("handlers"));
        }
        out.bind_handlers();
    }
    true
}
