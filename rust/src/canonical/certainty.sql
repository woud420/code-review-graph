UPDATE edges
   SET target_resolution = CASE WHEN EXISTS (
           SELECT 1 FROM nodes WHERE qualified_name = edges.target_qualified
       ) THEN 'direct' ELSE 'unresolved' END
 WHERE kind IN ('CALLS', 'REFERENCES');

WITH evidence AS (
    SELECT id,
           CASE WHEN kind = 'TESTED_BY'
                THEN NOT EXISTS (SELECT 1 FROM nodes WHERE qualified_name = edges.source_qualified)
                ELSE target_resolution = 'unresolved' END AS unbound,
           json_type(extra, '$.ambiguous_targets') IS NOT NULL AS ambiguous,
           EXISTS (
               SELECT 1 FROM json_each(edges.extra) WHERE key IN (
                   'bare_call_target', 'bare_tested_by_source', 'cpp_scoped_target', 'inherited_endpoint',
                   'rescript_open_module', 'scoped_resolved', 'spring_resolved', 'temporal_resolved'
               )
           ) AS inferred
      FROM edges
     WHERE kind IN ('CALLS', 'REFERENCES', 'TESTED_BY')
)
UPDATE edges AS e
   SET confidence_tier = CASE WHEN unbound AND ambiguous THEN 'AMBIGUOUS'
                              WHEN unbound THEN 'UNRESOLVED'
                              WHEN inferred THEN 'INFERRED'
                              WHEN confidence_tier IN ('AMBIGUOUS', 'UNRESOLVED') THEN 'EXTRACTED'
                              ELSE confidence_tier END
  FROM evidence WHERE evidence.id = e.id;

UPDATE edges
   SET confidence = CASE confidence_tier
       WHEN 'AMBIGUOUS' THEN 0.4 WHEN 'UNRESOLVED' THEN 0.2
       ELSE coalesce(json_extract(extra, '$.confidence'),
                     CASE WHEN confidence_tier = 'INFERRED' THEN 0.8 ELSE 1.0 END)
       END
 WHERE kind IN ('CALLS', 'REFERENCES', 'TESTED_BY');
