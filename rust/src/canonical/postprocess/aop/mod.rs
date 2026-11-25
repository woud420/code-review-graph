mod pointcut;

use anyhow::Result;
use pointcut::{Pointcut, annotation, argument};
use rusqlite::{Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    aspects_indexed: usize,
    advice_resolved: usize,
    calls_created: usize,
    stale_calls_removed: usize,
}
#[derive(Default)]
struct Class {
    package: String,
    aspect: bool,
    methods: Vec<Method>,
}
struct Method {
    name: String,
    endpoint: String,
    subject: String,
    test_or_constructor: bool,
    decorators: Vec<String>,
}
type Classes = BTreeMap<(String, String), Class>;

fn classes(tx: &Transaction<'_>) -> Result<Classes> {
    let mut classes = Classes::new();
    let mut query=tx.prepare("SELECT name, file_path, extra FROM nodes WHERE kind='Class' AND language='java' ORDER BY id")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let extra: Value =
            serde_json::from_str(&row.get::<_, Option<String>>(2)?.unwrap_or_default())
                .unwrap_or_default();
        classes.insert(
            (row.get(0)?, row.get(1)?),
            Class {
                aspect: extra["spring_annotations"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "Aspect")),
                package: extra["java_package"].as_str().unwrap_or("").into(),
                methods: vec![],
            },
        );
    }
    if !classes.values().any(|class| class.aspect) {
        return Ok(classes);
    }
    let mut query=tx.prepare("SELECT name,qualified_name,parent_name,file_path,extra,kind FROM nodes WHERE kind IN ('Function','Test') AND parent_name IS NOT NULL AND language='java' ORDER BY id")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let parent: String = row.get(2)?;
        let class = classes.entry((parent.clone(), row.get(3)?)).or_default();
        let name: String = row.get(0)?;
        let extra: Value =
            serde_json::from_str(&row.get::<_, Option<String>>(4)?.unwrap_or_default())
                .unwrap_or_default();
        let scope = if class.package.is_empty() {
            parent
        } else {
            format!("{}.{parent}", class.package)
        };
        let subject = format!("{scope}.{name}");
        class.methods.push(Method {
            name,
            endpoint: row.get(1)?,
            subject,
            test_or_constructor: row.get::<_, String>(5)? == "Test"
                || extra["java_constructor"] == true,
            decorators: serde_json::from_value(extra["decorators"].clone()).unwrap_or_default(),
        });
    }
    Ok(classes)
}

pub(super) fn resolve(tx: &Transaction<'_>) -> Result<Summary> {
    let mut report = Summary::default();
    if !tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes WHERE language='java')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(report);
    }
    report.stale_calls_removed=tx.execute("DELETE FROM edges WHERE kind='CALLS' AND extra LIKE '%aop_resolved%' AND CASE WHEN json_valid(extra) THEN json_extract(extra,'$.aop_resolved') END",[])?;
    let classes = classes(tx)?;
    let targets: Vec<_> = classes
        .values()
        .filter(|c| !c.aspect)
        .flat_map(|c| &c.methods)
        .filter(|m| !m.test_or_constructor)
        .collect();
    let mut update=tx.prepare("UPDATE edges SET extra=?4,confidence=0.7,confidence_tier='INFERRED',updated_at=unixepoch('subsec') WHERE kind='CALLS' AND source_qualified=?1 AND target_qualified=?2 AND file_path=?3 AND line=0")?;
    let mut insert=tx.prepare("INSERT INTO edges(kind,source_qualified,target_qualified,file_path,line,extra,confidence,confidence_tier,target_resolution,updated_at) VALUES('CALLS',?1,?2,?3,0,?4,0.7,'INFERRED','direct',unixepoch('subsec'))")?;
    for ((_, path), aspect) in classes.iter().filter(|(_, c)| c.aspect) {
        report.aspects_indexed += 1;
        let mut named = HashMap::new();
        let mut advice = vec![];
        for method in &aspect.methods {
            for deco in &method.decorators {
                let Some(expr) = argument(deco) else {
                    continue;
                };
                let kind = annotation(deco);
                match kind {
                    "Pointcut" => {
                        named.insert(method.name.as_str(), expr);
                    }
                    "Before" | "After" | "Around" | "AfterReturning" | "AfterThrowing" => {
                        advice.push((method, kind, expr))
                    }
                    _ => {}
                }
            }
        }
        for (source, kind, expr) in advice {
            let Some(pc) = Pointcut::parse(expr, &named) else {
                continue;
            };
            let mut resolved = false;
            let extra=json!({"aop_resolved":true,"pointcut_kind":pc.kind(),"pointcut_expr":pc.expr,"advice_kind":kind,"confidence":0.7,"confidence_tier":"INFERRED"}).to_string();
            for target in &targets {
                if pc.applies(&target.name, &target.subject, &target.decorators) {
                    resolved = true;
                    let params = params![source.endpoint, target.endpoint, path, extra];
                    if update.execute(params)? == 0 {
                        insert.execute(params)?;
                    }
                    report.calls_created += 1;
                }
            }
            report.advice_resolved += usize::from(resolved);
        }
    }
    Ok(report)
}
