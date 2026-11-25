use super::*;

fn annotations(n: Node<'_>) -> Vec<Node<'_>> {
    child(n, "modifiers")
        .into_iter()
        .flat_map(children)
        .filter(|c| matches!(c.kind(), "annotation" | "marker_annotation"))
        .collect()
}
pub(super) fn decorators(n: Node<'_>, s: &str) -> Vec<String> {
    annotations(n)
        .into_iter()
        .map(|c| text(c, s).trim_start_matches('@').to_owned())
        .collect()
}
fn annotation_names<'s>(n: Node<'_>, s: &'s str) -> Vec<&'s str> {
    annotations(n)
        .into_iter()
        .filter_map(|c| child(c, "identifier").map(|v| text(v, s)))
        .collect()
}
fn has_modifier(n: Node<'_>, key: &str) -> bool {
    child(n, "modifiers").is_some_and(|m| m.children(&mut m.walk()).any(|c| c.kind() == key))
}
fn inject(names: &[&str]) -> bool {
    names
        .iter()
        .any(|n| matches!(*n, "Inject" | "Autowired" | "Resource"))
}

impl Input<'_> {
    pub(super) fn class_metadata(&mut self, n: Node<'_>, class: &str, extra: &mut Value) {
        let s = self.source;
        let names = annotation_names(n, s);
        if !self.package.is_empty() {
            extra["java_package"] = json!(self.package);
        }
        extra["java_kind"] = json!(if n.kind() == "interface_declaration" {
            "interface"
        } else if has_modifier(n, "abstract") {
            "abstract"
        } else {
            "concrete"
        });
        if !names.is_empty() {
            extra["spring_annotations"] = json!(names);
        }
        if let Some(stereotype) = names.iter().find(|x| {
            matches!(
                **x,
                "Component"
                    | "Service"
                    | "Repository"
                    | "Controller"
                    | "RestController"
                    | "Configuration"
                    | "Indexed"
                    | "ControllerAdvice"
                    | "RestControllerAdvice"
                    | "EventListener"
            )
        }) {
            extra["spring_stereotype"] = json!(stereotype);
        }
        if names.contains(&"WorkflowInterface") {
            extra["temporal_role"] = json!("workflow_interface");
        } else if names.contains(&"ActivityInterface") {
            extra["temporal_role"] = json!("activity_interface");
        }
        for inheritance in children(n).into_iter().filter(|c| {
            matches!(
                c.kind(),
                "superclass" | "super_interfaces" | "extends_interfaces"
            )
        }) {
            let mut types = children(inheritance);
            if let Some(list) = child(inheritance, "type_list") {
                types = children(list);
            }
            for t in types {
                let base = base_type(text(t, s));
                self.edge(
                    "INHERITS",
                    class,
                    base,
                    n,
                    json!({"java_type":self.type_identity(base,false)}),
                );
            }
        }
        if let Some(body) = child(n, "class_body") {
            for f in children(body)
                .into_iter()
                .filter(|f| f.kind() == "field_declaration")
            {
                self.field_edges(f, class, &names);
            }
        }
    }
    fn field_edges(&mut self, n: Node<'_>, class: &str, names: &[&str]) {
        let s = self.source;
        let Some(t) = n.child_by_field_name("type") else {
            return;
        };
        if has_modifier(n, "static")
            || !matches!(t.kind(), "generic_type" | "array_type" | "type_identifier")
        {
            return;
        }
        let ty = base_type(text(t, s));
        let fields: Vec<_> = children(n)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        let annotations = annotation_names(n, s);
        for &d in &fields {
            let injection = if inject(&annotations) {
                Some("field")
            } else if names.contains(&"AllArgsConstructor") {
                Some("constructor_lombok_all")
            } else if names.contains(&"RequiredArgsConstructor")
                && d.child_by_field_name("value").is_none()
                && (has_modifier(n, "final") || annotations.contains(&"NonNull"))
            {
                Some("constructor_lombok")
            } else {
                None
            };
            if let (Some(kind), Some(name)) = (injection, field(d, "name", s)) {
                self.edge("INJECTS",class,ty,n,json!({"field_name":name,"injection_type":kind,"java_type":self.type_identity(ty,true)}));
            }
        }
        let Some(name) = fields.last().and_then(|&d| field(d, "name", s)) else {
            return;
        };
        if t.kind() == "type_identifier" && (ty.ends_with("Activity") || ty.ends_with("Workflow")) {
            self.edge("TEMPORAL_STUB",class,ty,n,json!({"field_name":name,"stub_type":if ty.ends_with("Activity") {"activity"} else {"workflow"}}));
        }
        let kind = match ty {
            "KafkaReceiver"
            | "ReactiveKafkaConsumerTemplate"
            | "MessageListenerContainer"
            | "ConcurrentMessageListenerContainer" => "CONSUMES",
            "KafkaTemplate"
            | "KafkaOperations"
            | "ReactiveKafkaProducerTemplate"
            | "KafkaSender" => "PRODUCES",
            _ => return,
        };
        let mut extra = json!({"field_name":name,"kafka_type":ty});
        let args: Vec<_> = child(t, "type_arguments")
            .into_iter()
            .flat_map(children)
            .filter(|a| a.kind() == "type_identifier")
            .collect();
        if args.len() > 1 {
            extra["message_type"] = json!(text(*args.last().unwrap(), s));
        }
        self.edge(kind, class, "kafka:config", n, extra);
    }
    pub(super) fn method_metadata(
        &mut self,
        n: Node<'_>,
        class: &str,
        identity: &str,
        extra: &mut Value,
    ) {
        let s = self.source;
        let names = annotation_names(n, s);
        if n.kind() == "constructor_declaration" {
            extra["java_constructor"] = json!(true);
            if inject(&names) {
                for p in n
                    .child_by_field_name("parameters")
                    .into_iter()
                    .flat_map(children)
                {
                    if let (Some(t), Some(name)) =
                        (child(p, "type_identifier"), field(p, "name", s))
                    {
                        let ty = text(t, s);
                        self.edge("INJECTS",class,ty,p,json!({"field_name":name,"injection_type":"constructor","java_type":self.type_identity(ty,true)}));
                    }
                }
            }
        }
        let decorators = decorators(n, s);
        if let Some(role) = decorators.iter().find(|n| {
            matches!(
                n.as_str(),
                "WorkflowMethod" | "ActivityMethod" | "SignalMethod" | "QueryMethod"
            )
        }) {
            extra["temporal_role"] = json!(role.to_lowercase());
        }
        for a in annotations(n) {
            let Some(name) = field(a, "name", s) else {
                continue;
            };
            if !matches!(name, "KafkaListener" | "KafkaHandler") {
                continue;
            }
            extra["kafka_listener"] = json!(true);
            let mut topics = Vec::new();
            for pair in a
                .child_by_field_name("arguments")
                .into_iter()
                .flat_map(children)
            {
                if !matches!(
                    field(pair, "key", s),
                    Some("topics" | "topicPattern" | "value")
                ) {
                    continue;
                }
                if let Some(v) = pair.child_by_field_name("value") {
                    let values = if v.kind() == "string_literal" {
                        vec![v]
                    } else {
                        children(v)
                    };
                    topics.extend(
                        values
                            .into_iter()
                            .filter(|v| v.kind() == "string_literal")
                            .map(|v| text(v, s).trim_matches('"'))
                            .filter(|v| !v.is_empty()),
                    );
                }
            }
            if topics.is_empty() {
                self.edge(
                    "CONSUMES",
                    identity,
                    "kafka:config",
                    n,
                    json!({"kafka_type":name}),
                );
            }
            for topic in topics {
                self.edge(
                    "CONSUMES",
                    identity,
                    &format!("kafka:{topic}"),
                    n,
                    json!({"kafka_type":"KafkaListener","topic":topic}),
                );
            }
        }
    }
}
