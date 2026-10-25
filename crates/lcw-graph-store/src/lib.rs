//! # lcw-graph-store
//!
//! Persists a [`GraphSnapshot`] in an embedded **SQLite** database (`graph.sqlite`)
//! beside the fragment cache. Analysis still runs entirely in RAM; this store is
//! for durable inspection, tooling, and future incremental loads.

use std::path::Path;

use lcw_core::{EdgeExport, GraphSnapshot, Node, NodeKind, EdgeKind, SourceSpan};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// On-disk file name inside the per-repo cache directory.
pub const DB_FILENAME: &str = "graph.sqlite";

/// Bump when table layout or serialization changes.
const STORE_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unsupported store version {found} (expected {STORE_VERSION})")]
    VersionMismatch { found: u32 },
    #[error("corrupt store: {0}")]
    Corrupt(String),
}

/// Lightweight metadata without loading the full graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSummary {
    pub nodes: usize,
    pub edges: usize,
    pub files: usize,
}

/// Write `snapshot` to `path`, replacing any previous contents.
pub fn write(path: &Path, snapshot: &GraphSnapshot) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    let tx = conn.transaction()?;
    init_schema(&tx)?;
    tx.execute("DELETE FROM edges", [])?;
    tx.execute("DELETE FROM nodes", [])?;
    tx.execute("DELETE FROM files", [])?;
    tx.execute("DELETE FROM meta", [])?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params!["version", STORE_VERSION.to_string()],
    )?;
    for (i, file) in snapshot.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO files (id, path) VALUES (?1, ?2)",
            params![i as i64, file.to_string_lossy()],
        )?;
    }
    for node in &snapshot.nodes {
        let (flags_json, stats_json) = node_payload(node)?;
        let span = &node.span;
        tx.execute(
            "INSERT INTO nodes (
                id, name, qualified_name, module_path, kind,
                file_id, start_line, start_col, end_line, end_col,
                flags_json, stats_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                node.id.0 as i64,
                node.name,
                node.qualified_name,
                node.module_path,
                kind_tag(node.kind),
                span.file as i64,
                span.start_line as i64,
                span.start_col as i64,
                span.end_line as i64,
                span.end_col as i64,
                flags_json,
                stats_json,
            ],
        )?;
    }
    for e in &snapshot.edges {
        let span = &e.call_site;
        tx.execute(
            "INSERT INTO edges (
                from_id, to_id, kind,
                file_id, start_line, start_col, end_line, end_col,
                count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.from.0 as i64,
                e.to.0 as i64,
                edge_kind_tag(e.kind),
                span.file as i64,
                span.start_line as i64,
                span.start_col as i64,
                span.end_line as i64,
                span.end_col as i64,
                e.count as i64,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Load a full snapshot from `path`.
pub fn read(path: &Path) -> Result<GraphSnapshot, StoreError> {
    let conn = Connection::open(path)?;
    check_version(&conn)?;
    let files = load_files(&conn)?;
    let nodes = load_nodes(&conn)?;
    let edges = load_edges(&conn)?;
    Ok(GraphSnapshot { files, nodes, edges })
}

/// Count rows without deserializing node bodies.
pub fn summary(path: &Path) -> Result<GraphSummary, StoreError> {
    let conn = Connection::open(path)?;
    check_version(&conn)?;
    let nodes: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edges: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    Ok(GraphSummary {
        nodes: nodes as usize,
        edges: edges as usize,
        files: files as usize,
    })
}

fn init_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS nodes (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            qualified_name TEXT NOT NULL,
            module_path TEXT NOT NULL,
            kind TEXT NOT NULL,
            file_id INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            start_col INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            end_col INTEGER NOT NULL,
            flags_json TEXT NOT NULL,
            stats_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS edges (
            from_id INTEGER NOT NULL,
            to_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            file_id INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            start_col INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            end_col INTEGER NOT NULL,
            count INTEGER NOT NULL,
            PRIMARY KEY (from_id, to_id, kind)
        );
        ",
    )?;
    Ok(())
}

fn check_version(conn: &Connection) -> Result<(), StoreError> {
    init_schema(conn)?;
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match version {
        Some(v) => {
            let found = v.parse::<u32>().map_err(|_| {
                StoreError::Corrupt(format!("meta.version is not a number: {v}"))
            })?;
            if found != STORE_VERSION {
                return Err(StoreError::VersionMismatch { found });
            }
        }
        None => return Err(StoreError::Corrupt("missing meta.version".into())),
    }
    Ok(())
}

fn load_files(conn: &Connection) -> Result<Vec<std::path::PathBuf>, StoreError> {
    let mut stmt = conn.prepare("SELECT path FROM files ORDER BY id ASC")?;
    let rows = stmt.query_map([], |r| {
        let p: String = r.get(0)?;
        Ok(std::path::PathBuf::from(p))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

fn load_nodes(conn: &Connection) -> Result<Vec<Node>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, qualified_name, module_path, kind,
                file_id, start_line, start_col, end_line, end_col,
                flags_json, stats_json
         FROM nodes ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: i64 = r.get(0)?;
        let kind: String = r.get(4)?;
        let flags_json: String = r.get(10)?;
        let stats_json: String = r.get(11)?;
        Ok((
            lcw_core::NodeId(id as u32),
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            parse_node_kind(&kind)?,
            SourceSpan {
                file: r.get::<_, i64>(5)? as u32,
                start_line: r.get::<_, i64>(6)? as u32,
                start_col: r.get::<_, i64>(7)? as u32,
                end_line: r.get::<_, i64>(8)? as u32,
                end_col: r.get::<_, i64>(9)? as u32,
            },
            flags_json,
            stats_json,
        ))
    })?;
    let mut nodes = Vec::new();
    for row in rows {
        let (id, name, qualified_name, module_path, kind, span, flags_json, stats_json) =
            row.map_err(StoreError::from)?;
        nodes.push(Node {
            id,
            name,
            qualified_name,
            module_path,
            kind,
            span,
            flags: serde_json::from_str(&flags_json)?,
            stats: serde_json::from_str(&stats_json)?,
        });
    }
    Ok(nodes)
}

fn load_edges(conn: &Connection) -> Result<Vec<EdgeExport>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT from_id, to_id, kind, file_id, start_line, start_col, end_line, end_col, count
         FROM edges",
    )?;
    let rows = stmt.query_map([], |r| {
        let kind: String = r.get(2)?;
        Ok(EdgeExport {
            from: lcw_core::NodeId(r.get::<_, i64>(0)? as u32),
            to: lcw_core::NodeId(r.get::<_, i64>(1)? as u32),
            kind: parse_edge_kind(&kind)?,
            call_site: SourceSpan {
                file: r.get::<_, i64>(3)? as u32,
                start_line: r.get::<_, i64>(4)? as u32,
                start_col: r.get::<_, i64>(5)? as u32,
                end_line: r.get::<_, i64>(6)? as u32,
                end_col: r.get::<_, i64>(7)? as u32,
            },
            count: r.get::<_, i64>(8)? as u32,
        })
    })?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

fn node_payload(node: &Node) -> Result<(String, String), StoreError> {
    Ok((
        serde_json::to_string(&node.flags)?,
        serde_json::to_string(&node.stats)?,
    ))
}

fn kind_tag(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

fn edge_kind_tag(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::DirectCall => "direct_call",
        EdgeKind::MethodCall => "method_call",
        EdgeKind::AssociatedCall => "associated_call",
        EdgeKind::MacroCall => "macro_call",
        EdgeKind::TraitDispatch => "trait_dispatch",
        EdgeKind::Unresolved => "unresolved",
        EdgeKind::Spawn => "spawn",
    }
}

fn parse_node_kind(s: &str) -> Result<NodeKind, rusqlite::Error> {
    match s {
        "function" => Ok(NodeKind::Function),
        "method" => Ok(NodeKind::Method),
        "closure" => Ok(NodeKind::Closure),
        "external" => Ok(NodeKind::External),
        _ => Err(rusqlite::Error::InvalidColumnType(
            4,
            "kind".into(),
            rusqlite::types::Type::Text,
        )),
    }
}

fn parse_edge_kind(s: &str) -> Result<EdgeKind, rusqlite::Error> {
    match s {
        "direct_call" => Ok(EdgeKind::DirectCall),
        "method_call" => Ok(EdgeKind::MethodCall),
        "associated_call" => Ok(EdgeKind::AssociatedCall),
        "macro_call" => Ok(EdgeKind::MacroCall),
        "trait_dispatch" => Ok(EdgeKind::TraitDispatch),
        "unresolved" => Ok(EdgeKind::Unresolved),
        "spawn" => Ok(EdgeKind::Spawn),
        _ => Err(rusqlite::Error::InvalidColumnType(
            2,
            "kind".into(),
            rusqlite::types::Type::Text,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{CodeGraph, NodeFlags, NodeStats};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("lcw-graph-db-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn roundtrip_snapshot() {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let a = g.add_node(Node {
            id: lcw_core::NodeId(0),
            name: "a".into(),
            qualified_name: "m::a".into(),
            module_path: "m".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, 1, 0, 3, 1),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        });
        let b = g.add_node(Node {
            id: lcw_core::NodeId(0),
            name: "b".into(),
            qualified_name: "m::b".into(),
            module_path: "m".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, 5, 0, 6, 1),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        });
        g.add_edge(a, b, lcw_core::Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 2, 4, 2, 5)));

        let snap = g.snapshot();
        let path = temp_db();
        write(&path, &snap).unwrap();
        let back = read(&path).unwrap();
        let g2 = CodeGraph::from_snapshot(back);
        assert_eq!(g2.node_count(), 2);
        assert_eq!(g2.edge_count(), 1);
        let sum = summary(&path).unwrap();
        assert_eq!(sum.nodes, 2);
        assert_eq!(sum.edges, 1);
        assert_eq!(sum.files, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_graph_roundtrip() {
        let snap = GraphSnapshot::default();
        let path = temp_db();
        write(&path, &snap).unwrap();
        let back = read(&path).unwrap();
        assert!(back.nodes.is_empty());
        assert!(back.edges.is_empty());
        let _ = std::fs::remove_file(path);
    }
}
