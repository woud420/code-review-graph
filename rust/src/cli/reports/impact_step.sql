INSERT INTO _impact_next (node_qn, score)
SELECT node_qn, MAX(score)
FROM (
    SELECT e.target_qualified AS node_qn,
           f.score * COALESCE(p.weight, 0.5) * ?1 AS score
    FROM _impact_frontier f
    JOIN edges e ON e.source_qualified = f.node_qn
    LEFT JOIN _impact_policies p ON p.kind = e.kind
    WHERE CASE WHEN e.extra LIKE '%"aop_resolved"%' THEN 'outgoing' ELSE COALESCE(p.direction, 'incoming') END = 'outgoing' AND (?2=0 OR e.kind NOT IN ('CALLS','REFERENCES') OR coalesce(e.target_resolution,CASE WHEN EXISTS(SELECT 1 FROM nodes WHERE qualified_name=e.target_qualified) THEN 'direct' ELSE 'unresolved' END)='direct')
    UNION ALL
    SELECT e.source_qualified AS node_qn,
           f.score * COALESCE(p.weight, 0.5) * ?1 AS score
    FROM _impact_frontier f
    JOIN edges e ON e.target_qualified = f.node_qn
    LEFT JOIN _impact_policies p ON p.kind = e.kind
    WHERE CASE WHEN e.extra LIKE '%"aop_resolved"%' THEN 'outgoing' ELSE COALESCE(p.direction, 'incoming') END = 'incoming' AND (?2=0 OR e.kind NOT IN ('CALLS','REFERENCES') OR coalesce(e.target_resolution,CASE WHEN EXISTS(SELECT 1 FROM nodes WHERE qualified_name=e.target_qualified) THEN 'direct' ELSE 'unresolved' END)='direct')
    UNION ALL
    SELECT e.source_qualified AS node_qn,
           d.score * COALESCE(p.weight, 0.5) * ?1 AS score
    FROM _impact_frontier_dirs d
    CROSS JOIN edges e INDEXED BY idx_edges_target_kind
      ON e.target_qualified = d.dir AND e.kind = 'IMPORTS_FROM'
    LEFT JOIN _impact_policies p ON p.kind = e.kind
    WHERE COALESCE(p.direction, 'incoming') = 'incoming'
) candidates
WHERE score > ?3
GROUP BY node_qn
