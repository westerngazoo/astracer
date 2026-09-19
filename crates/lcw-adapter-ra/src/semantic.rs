//! rust-analyzer backed call-graph extraction.
//!
//! Pipeline:
//! 1. Locate the Cargo workspace root from the provided source paths and load
//!    it into rust-analyzer's analysis database (`load_workspace_at`).
//! 2. Walk every file that belongs to a **local** (workspace-member) crate.
//! 3. For each function/method in a file (`Analysis::file_structure`), ask
//!    rust-analyzer for its resolved **outgoing calls**
//!    (`Analysis::outgoing_calls`) and record an edge to each callee.
//!
//! Nodes are de-duplicated by their definition identity `(file, name-range)`,
//! which is stable between "seen as a caller" and "seen as a callee", so the
//! caller graph stitches together across files without double-counting.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lcw_core::{
    AdapterError, CodeGraph, Edge, EdgeKind, FileId as LcwFileId, Node, NodeFlags, NodeId,
    NodeKind, NodeStats, SourceFile, SourceSpan,
};

use ra_ap_base_db::{Crate, CrateOrigin};
use ra_ap_ide::{
    AnalysisHost, CallHierarchyConfig, FileId as RaFileId, FilePosition, FileStructureConfig,
    NavigationTarget, RaFixtureConfig, StructureNode, StructureNodeKind, SymbolKind, TextRange,
};
use ra_ap_ide_db::line_index::LineIndex;
use ra_ap_ide_db::RootDatabase;
use ra_ap_load_cargo::{load_workspace_at, LoadCargoConfig, ProcMacroServerChoice};
use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_vfs::Vfs;

/// Stable identity of a definition: its file plus the byte range of its name.
type NavKey = (RaFileId, u32, u32);

fn range_key(file: RaFileId, r: TextRange) -> NavKey {
    (file, u32::from(r.start()), u32::from(r.end()))
}

/// Convert a rust-analyzer byte range into our line/column [`SourceSpan`]
/// (1-based lines, 0-based columns).
fn span_of(file: LcwFileId, li: &LineIndex, r: TextRange) -> SourceSpan {
    let s = li.line_col(r.start());
    let e = li.line_col(r.end());
    SourceSpan::new(
        file,
        s.line.saturating_add(1),
        s.col,
        e.line.saturating_add(1),
        e.col,
    )
}

fn loc_of(span: &SourceSpan) -> u32 {
    span.end_line
        .saturating_sub(span.start_line)
        .saturating_add(1)
}

/// Extract just the type/module name from a `file_structure` parent label,
/// e.g. `"impl Foo"` -> `"Foo"`, `"impl Trait for Foo"` -> `"Trait"`.
fn container_of(label: &str) -> String {
    let l = label.trim();
    let l = l.strip_prefix("impl ").unwrap_or(l);
    l.split([' ', '<']).next().unwrap_or(l).trim().to_string()
}

/// Strip any signature noise from a structure-node label, keeping the bare name.
fn clean_name(label: &str) -> String {
    label
        .split(['(', '<', ' '])
        .next()
        .unwrap_or(label)
        .trim()
        .to_string()
}

fn ra_path(vfs: &Vfs, fid: RaFileId) -> Option<PathBuf> {
    vfs.file_path(fid)
        .as_path()
        .map(|abs| PathBuf::from(abs.as_str()))
}

/// Join a crate name, container (type/trait/module), and symbol name into a
/// Rust-style path, skipping any empty segment: `lcw_engine::Engine::analyze`.
/// This is what makes semantic nodes precisely *targetable* by `flow`/`explain`
/// (fast mode already qualifies names; without this, semantic nodes were bare
/// `analyze`/`new`, indistinguishable across crates).
fn qualify(crate_name: Option<&str>, container: Option<&str>, name: &str) -> String {
    let mut path = String::new();
    for seg in [crate_name, container, Some(name)].into_iter().flatten() {
        if seg.is_empty() {
            continue;
        }
        if !path.is_empty() {
            path.push_str("::");
        }
        path.push_str(seg);
    }
    path
}

/// The crate a file belongs to, as `(display_name, is_workspace_local)`. The
/// name uses the path form (underscores), matching how fast mode qualifies.
fn crate_info(
    analysis: &ra_ap_ide::Analysis,
    db: &RootDatabase,
    local: &HashSet<Crate>,
    fid: RaFileId,
) -> (Option<String>, bool) {
    let crates = analysis.crates_for(fid).unwrap_or_default();
    let is_local = crates.iter().any(|c| local.contains(c));
    let name = crates.first().and_then(|k| {
        k.extra_data(db)
            .display_name
            .as_ref()
            .map(|d| d.crate_name().to_string())
    });
    (name, is_local)
}

/// Accumulates the [`CodeGraph`] while mapping rust-analyzer identities onto our
/// own ids.
struct Registry {
    graph: CodeGraph,
    ra_to_lcw_file: HashMap<RaFileId, LcwFileId>,
    nav_to_node: HashMap<NavKey, NodeId>,
    used_qnames: HashMap<String, u32>,
}

impl Registry {
    fn new() -> Self {
        Registry {
            graph: CodeGraph::new(),
            ra_to_lcw_file: HashMap::new(),
            nav_to_node: HashMap::new(),
            used_qnames: HashMap::new(),
        }
    }

    fn intern_file(&mut self, fid: RaFileId, vfs: &Vfs) -> LcwFileId {
        if let Some(&f) = self.ra_to_lcw_file.get(&fid) {
            return f;
        }
        let path = ra_path(vfs, fid).unwrap_or_else(|| PathBuf::from(format!("<ra:{fid:?}>")));
        let f = self.graph.intern_file(path);
        self.ra_to_lcw_file.insert(fid, f);
        f
    }

    /// A `qualified_name` guaranteed unique across the graph, so `CodeGraph`'s
    /// name-based interning never merges two distinct definitions that happen
    /// to share a `crate::Type::method` path.
    fn unique_qname(
        &mut self,
        crate_name: Option<&str>,
        container: Option<&str>,
        name: &str,
    ) -> String {
        let base = qualify(crate_name, container, name);
        let n = self.used_qnames.entry(base.clone()).or_insert(0);
        let out = if *n == 0 {
            base.clone()
        } else {
            format!("{base}#{n}")
        };
        *n += 1;
        out
    }

    /// Get-or-create the node for a function we are iterating as a *caller*.
    fn caller_node(
        &mut self,
        lcw_file: LcwFileId,
        ra_file: RaFileId,
        node: &StructureNode,
        crate_name: Option<&str>,
        container: Option<&str>,
        li: &LineIndex,
    ) -> NodeId {
        let key = range_key(ra_file, node.navigation_range);
        if let Some(&id) = self.nav_to_node.get(&key) {
            return id;
        }
        let name = clean_name(&node.label);
        let qualified_name = self.unique_qname(crate_name, container, &name);
        let span = span_of(lcw_file, li, node.node_range);
        let kind = match node.kind {
            StructureNodeKind::SymbolKind(SymbolKind::Method) => NodeKind::Method,
            _ => NodeKind::Function,
        };
        let id = self.graph.add_node(Node {
            id: NodeId(u32::MAX),
            name,
            qualified_name,
            module_path: qualify(crate_name, container, ""),
            kind,
            span,
            flags: NodeFlags {
                is_method: matches!(kind, NodeKind::Method),
                ..Default::default()
            },
            stats: NodeStats {
                lines_of_code: loc_of(&span),
                ..Default::default()
            },
        });
        self.nav_to_node.insert(key, id);
        id
    }

    /// Get-or-create the node for a resolved *callee* target. `crate_name` and
    /// `is_local` come from the target's own file: non-local targets (std, deps)
    /// become [`NodeKind::External`] so flows don't wander into library code and
    /// `explain` can flag them.
    fn target_node(
        &mut self,
        target: &NavigationTarget,
        crate_name: Option<&str>,
        is_local: bool,
        vfs: &Vfs,
        analysis: &ra_ap_ide::Analysis,
    ) -> NodeId {
        let name_range = target.focus_range.unwrap_or(target.full_range);
        let key = range_key(target.file_id, name_range);
        if let Some(&id) = self.nav_to_node.get(&key) {
            return id;
        }
        let lcw_file = self.intern_file(target.file_id, vfs);
        let name = target.name.as_str().to_string();
        let container = target
            .container_name
            .as_ref()
            .map(|s| s.as_str().to_string());
        let qualified_name = self.unique_qname(crate_name, container.as_deref(), &name);
        let kind = if !is_local {
            NodeKind::External
        } else {
            match target.kind {
                Some(SymbolKind::Method) => NodeKind::Method,
                _ => NodeKind::Function,
            }
        };
        // Deref-coerces the (rust-analyzer) `Arc<LineIndex>` to `&LineIndex`;
        // we never name the Arc so its exact flavour doesn't matter.
        let span = analysis
            .file_line_index(target.file_id)
            .ok()
            .map(|li| span_of(lcw_file, &li, target.full_range))
            .unwrap_or_else(|| SourceSpan::new(lcw_file, 0, 0, 0, 0));
        let id = self.graph.add_node(Node {
            id: NodeId(u32::MAX),
            name,
            qualified_name,
            module_path: qualify(crate_name, container.as_deref(), ""),
            kind,
            span,
            flags: NodeFlags {
                is_method: matches!(kind, NodeKind::Method),
                ..Default::default()
            },
            stats: NodeStats {
                lines_of_code: loc_of(&span),
                ..Default::default()
            },
        });
        self.nav_to_node.insert(key, id);
        id
    }
}

/// Walk up from the source files to the top-most ancestor containing a
/// `Cargo.toml` — the workspace root rust-analyzer should load.
fn workspace_root(files: &[SourceFile]) -> Option<PathBuf> {
    let first = files.iter().find_map(|f| f.path.canonicalize().ok())?;
    let mut dir = first.parent()?.to_path_buf();
    let mut best = None;
    loop {
        if dir.join("Cargo.toml").is_file() {
            best = Some(dir.clone());
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    best
}

/// Entry point: build a [`CodeGraph`] for the workspace the sources live in.
pub fn parse(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    let root = workspace_root(files).ok_or_else(|| {
        AdapterError::Other(
            "could not locate a Cargo.toml workspace root from the provided sources".to_string(),
        )
    })?;

    let cargo_config = CargoConfig {
        all_targets: true,
        sysroot: Some(RustLibSource::Discover),
        sysroot_src: None,
        rustc_source: None,
        ..Default::default()
    };
    let worker_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    let load_config = LoadCargoConfig {
        // Don't invoke `cargo check`: much faster startup and no network/build.
        load_out_dirs_from_check: false,
        with_proc_macro_server: ProcMacroServerChoice::None,
        prefill_caches: false,
        num_worker_threads: worker_threads,
        proc_macro_processes: 0,
    };
    let progress = |_msg: String| {};

    let (db, vfs, _proc_macro) = load_workspace_at(&root, &cargo_config, &load_config, &progress)
        .map_err(|e| {
        AdapterError::Other(format!(
            "rust-analyzer failed to load workspace at {}: {e}",
            root.display()
        ))
    })?;

    let host = AnalysisHost::with_database(db);
    let analysis = host.analysis();
    let db_ref = host.raw_database();

    // Workspace-member crates (skip registry/std deps).
    let local: HashSet<Crate> = ra_ap_base_db::all_crates(db_ref)
        .iter()
        .copied()
        .filter(|k| matches!(k.data(db_ref).origin, CrateOrigin::Local { .. }))
        .collect();

    // Restrict to the exact files the engine discovered (honours config filters).
    let provided: HashSet<PathBuf> = files
        .iter()
        .filter_map(|f| f.path.canonicalize().ok())
        .collect();

    let fsc = FileStructureConfig {
        exclude_locals: true,
    };
    let chc = CallHierarchyConfig {
        exclude_tests: false,
        ra_fixture: RaFixtureConfig::default(),
    };

    let mut reg = Registry::new();

    for (fid, vpath) in vfs.iter() {
        let Some(p) = vpath.as_path() else { continue };
        if !p.as_str().ends_with(".rs") {
            continue;
        }
        if !provided.is_empty() {
            let pb = PathBuf::from(p.as_str());
            let included = provided.contains(&pb)
                || pb
                    .canonicalize()
                    .ok()
                    .is_some_and(|c| provided.contains(&c));
            if !included {
                continue;
            }
        }
        let in_local = analysis
            .crates_for(fid)
            .unwrap_or_default()
            .iter()
            .any(|c| local.contains(c));
        if !in_local {
            continue;
        }
        process_file(&mut reg, &analysis, db_ref, &local, &vfs, fid, &fsc, &chc);
    }

    Ok(reg.graph)
}

#[allow(clippy::too_many_arguments)]
fn process_file(
    reg: &mut Registry,
    analysis: &ra_ap_ide::Analysis,
    db: &RootDatabase,
    local: &HashSet<Crate>,
    vfs: &Vfs,
    fid: RaFileId,
    fsc: &FileStructureConfig,
    chc: &CallHierarchyConfig<'_>,
) {
    let Ok(structure) = analysis.file_structure(fsc, fid) else {
        return;
    };
    let Ok(li) = analysis.file_line_index(fid) else {
        return;
    };
    let lcw_file = reg.intern_file(fid, vfs);
    // Every caller here is in a workspace-local file (parse() filters), so we
    // only need this file's crate name.
    let (caller_crate, _) = crate_info(analysis, db, local, fid);

    for node in &structure {
        let is_callable = matches!(
            node.kind,
            StructureNodeKind::SymbolKind(SymbolKind::Function | SymbolKind::Method)
        );
        if !is_callable {
            continue;
        }

        let container = node
            .parent
            .and_then(|p| structure.get(p))
            .map(|parent| container_of(&parent.label));

        let from = reg.caller_node(
            lcw_file,
            fid,
            node,
            caller_crate.as_deref(),
            container.as_deref(),
            &li,
        );

        let pos = FilePosition {
            file_id: fid,
            offset: node.navigation_range.start(),
        };
        let Ok(Some(calls)) = analysis.outgoing_calls(chc, pos) else {
            continue;
        };

        for call in calls {
            let (target_crate, target_local) = crate_info(analysis, db, local, call.target.file_id);
            let to = reg.target_node(
                &call.target,
                target_crate.as_deref(),
                target_local,
                vfs,
                analysis,
            );

            let kind = match call.target.kind {
                Some(SymbolKind::Method) => EdgeKind::MethodCall,
                _ => EdgeKind::DirectCall,
            };
            let count = call.ranges.len().max(1) as u32;
            let call_site = call
                .ranges
                .first()
                .filter(|fr| fr.file_id == fid)
                .map(|fr| span_of(lcw_file, &li, fr.range))
                .unwrap_or_else(|| SourceSpan::new(lcw_file, 0, 0, 0, 0));

            let mut edge = Edge::new(kind, call_site);
            edge.count = count;
            reg.graph.add_edge(from, to, edge);
        }
    }
}
