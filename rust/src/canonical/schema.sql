-- SQLite v15 schema from the reference GraphStore. FTS5 creates its own shadow tables.
CREATE TABLE nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,          -- File, Class, Function, Type, Test
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE,
    file_path TEXT NOT NULL,
    line_start INTEGER,
    line_end INTEGER,
    language TEXT,
    parent_name TEXT,
    params TEXT,
    return_type TEXT,
    modifiers TEXT,
    is_test INTEGER DEFAULT 0,
    file_hash TEXT,
    extra TEXT DEFAULT '{}',
    symbol TEXT,
    docstring TEXT,
    name_tokens TEXT,
    updated_at REAL NOT NULL,
    signature TEXT,
    community_id INTEGER
);

CREATE TABLE edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,           -- CALLS, IMPORTS_FROM, INHERITS, REFERENCES, etc.
    source_qualified TEXT NOT NULL,
    target_qualified TEXT NOT NULL,
    file_path TEXT NOT NULL,
    line INTEGER DEFAULT 0,
    extra TEXT DEFAULT '{}',
    confidence REAL DEFAULT 1.0,
    confidence_tier TEXT DEFAULT 'EXTRACTED',
    target_resolution TEXT,
    updated_at REAL NOT NULL
);

CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE flows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    entry_point_id INTEGER NOT NULL,
    depth INTEGER NOT NULL,
    node_count INTEGER NOT NULL,
    file_count INTEGER NOT NULL,
    criticality REAL NOT NULL DEFAULT 0.0,
    path_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE flow_memberships (
    flow_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (flow_id, node_id)
);

CREATE TABLE communities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    level INTEGER NOT NULL DEFAULT 0,
    parent_id INTEGER,
    cohesion REAL NOT NULL DEFAULT 0.0,
    size INTEGER NOT NULL DEFAULT 0,
    dominant_language TEXT,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE nodes_fts_state (
    node_id INTEGER PRIMARY KEY,
    name TEXT,
    qualified_name TEXT,
    file_path TEXT,
    signature TEXT,
    docstring TEXT,
    name_tokens TEXT
);

CREATE VIRTUAL TABLE nodes_fts USING fts5(
    name, qualified_name, file_path, signature, docstring, name_tokens,
    content='nodes', content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE INDEX idx_nodes_file ON nodes(file_path);

CREATE INDEX idx_nodes_kind ON nodes(kind);

CREATE INDEX idx_nodes_name ON nodes(name);

CREATE INDEX idx_edges_kind ON edges(kind);

CREATE INDEX idx_edges_target_kind ON edges(target_qualified, kind);

CREATE INDEX idx_edges_source_kind ON edges(source_qualified, kind);

CREATE INDEX idx_edges_file ON edges(file_path);

CREATE INDEX idx_flows_criticality ON flows(criticality DESC);

CREATE INDEX idx_flows_entry ON flows(entry_point_id);

CREATE INDEX idx_flow_memberships_node ON flow_memberships(node_id);

CREATE INDEX idx_nodes_community ON nodes(community_id);

CREATE INDEX idx_communities_parent ON communities(parent_id);

CREATE INDEX idx_communities_cohesion ON communities(cohesion DESC);

CREATE INDEX idx_edges_composite
ON edges(kind, source_qualified, target_qualified, file_path, line);

CREATE INDEX idx_nodes_symbol ON nodes(symbol);

CREATE INDEX idx_edges_kind_target_resolution ON edges(kind, target_resolution);

CREATE INDEX idx_nodes_fts_state_file ON nodes_fts_state(file_path);
