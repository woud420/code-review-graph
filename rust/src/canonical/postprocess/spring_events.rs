use super::super::{NodeRow, write_node};
use anyhow::Result;
use rusqlite::{Transaction, params};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Default, Serialize)]
pub struct EventResolution {
    pub events_indexed: usize,
    pub calls_emitted: usize,
    pub stale_calls_removed: usize,
}

pub(super) fn resolve(tx: &Transaction<'_>) -> Result<EventResolution> {
    let stale_calls_removed = tx.execute(
        "DELETE FROM edges WHERE kind='CALLS' AND json_extract(extra,'$.spring_event_resolved')=1",
        [],
    )?;
    tx.execute(
        "DELETE FROM nodes WHERE kind='Event' AND file_path='event'",
        [],
    )?;
    let mut events=tx.prepare("SELECT target_qualified,coalesce(nullif(json_extract(extra,'$.event_type'),''),substr(target_qualified,8)) FROM edges WHERE kind IN ('PUBLISHES','HANDLES') AND target_qualified GLOB 'event::*' GROUP BY target_qualified ORDER BY target_qualified")?;
    let mut rows = events.query([])?;
    let mut events_indexed = 0;
    while let Some(row) = rows.next()? {
        let target: String = row.get(0)?;
        let name: String = row.get(1)?;
        write_node(
            tx,
            "event",
            "java",
            "",
            NodeRow {
                test_file: false,
                name: &name,
                qualified: &target,
                kind: "Event",
                parent: None,
                start: 0,
                end: 0,
                declaration: None,
                extra: Some(&json!({"event_type":name,"virtual":true})),
            },
        )?;
        events_indexed += 1;
    }
    let mut links=tx.prepare("SELECT p.source_qualified,h.source_qualified,p.file_path,p.line,json_extract(n.extra,'$.event_type')
          FROM edges p JOIN edges h ON h.kind='HANDLES' AND h.target_qualified=p.target_qualified
          JOIN nodes n ON n.kind='Event' AND n.qualified_name=p.target_qualified WHERE p.kind='PUBLISHES' ORDER BY p.id,h.id")?;
    let mut update=tx.prepare("UPDATE edges SET extra=?5,confidence=0.95,confidence_tier='INFERRED',target_resolution='direct',updated_at=unixepoch('subsec')
        WHERE id=(SELECT id FROM edges WHERE kind='CALLS' AND source_qualified=?1 AND target_qualified=?2 AND file_path=?3 AND line=?4 AND json_extract(extra,'$.source_offset') IS NULL LIMIT 1)")?;
    let mut insert=tx.prepare("INSERT INTO edges(kind,source_qualified,target_qualified,file_path,line,extra,confidence,confidence_tier,target_resolution,updated_at)
        VALUES('CALLS',?1,?2,?3,?4,?5,0.95,'INFERRED','direct',unixepoch('subsec'))")?;
    let mut calls_emitted = 0;
    let mut rows = links.query([])?;
    while let Some(row) = rows.next()? {
        let source: String = row.get(0)?;
        let target: String = row.get(1)?;
        let file: String = row.get(2)?;
        let line: i64 = row.get(3)?;
        let identity: String = row.get(4)?;
        let data=json!({"spring_event_resolved":true,"event_type":identity,"resolution":"spring_application_event","confidence":0.95,"confidence_tier":"INFERRED"}).to_string();
        let values = params![source, target, file, line, data];
        if update.execute(values)? == 0 {
            insert.execute(values)?;
        }
        calls_emitted += 1;
    }
    Ok(EventResolution {
        events_indexed,
        calls_emitted,
        stale_calls_removed,
    })
}
