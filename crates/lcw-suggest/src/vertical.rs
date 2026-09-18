//! Heuristic industry-vertical detection (Manifesto section 5).
//!
//! With only a call graph we can't know the domain for certain, so we tally
//! keyword hits across symbol and module names (crate/framework fingerprints)
//! and pick the strongest signal. The semantic backend (hardening phase) will
//! sharpen this by resolving real dependencies. Pin `[project] vertical` in the
//! config to override detection entirely.

use lcw_core::{AnalysisReport, Vertical};

/// Per-vertical scores plus the winner.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerticalScores {
    pub embedded: u32,
    pub game_engine: u32,
    pub backend: u32,
    pub full_stack: u32,
}

impl VerticalScores {
    fn best(&self) -> (Vertical, u32) {
        let candidates = [
            (Vertical::Embedded, self.embedded),
            (Vertical::GameEngine, self.game_engine),
            (Vertical::Backend, self.backend),
            (Vertical::FullStack, self.full_stack),
        ];
        candidates
            .into_iter()
            .max_by_key(|&(_, score)| score)
            .unwrap_or((Vertical::Unknown, 0))
    }

    fn second_best(&self) -> u32 {
        let mut scores = [
            self.embedded,
            self.game_engine,
            self.backend,
            self.full_stack,
        ];
        scores.sort_unstable_by(|a, b| b.cmp(a));
        scores[1]
    }
}

/// Score every vertical from the graph's symbols and modules.
pub fn score(report: &AnalysisReport) -> VerticalScores {
    let mut s = VerticalScores::default();
    for node in report.graph.nodes() {
        tally(&mut s, &node.qualified_name);
        tally(&mut s, &node.module_path);
    }
    s
}

/// Detect the vertical, or [`Vertical::Unknown`] when the signal is weak or
/// ambiguous (winner must clear a floor and beat the runner-up).
pub fn detect(report: &AnalysisReport) -> Vertical {
    let scores = score(report);
    let (winner, top) = scores.best();
    if top >= 3 && top > scores.second_best() {
        winner
    } else {
        Vertical::Unknown
    }
}

fn tally(scores: &mut VerticalScores, text: &str) {
    for token in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let t = token.to_ascii_lowercase();
        if EMBEDDED.contains(&t.as_str()) {
            scores.embedded += 1;
        }
        if GAME_ENGINE.contains(&t.as_str()) {
            scores.game_engine += 1;
        }
        if BACKEND.contains(&t.as_str()) {
            scores.backend += 1;
        }
        if FULL_STACK.contains(&t.as_str()) {
            scores.full_stack += 1;
        }
    }
}

const EMBEDDED: &[&str] = &[
    "embedded",
    "hal",
    "cortex",
    "gpio",
    "peripheral",
    "interrupt",
    "isr",
    "firmware",
    "rtic",
    "rtos",
    "microcontroller",
    "stm32",
    "riscv",
    "nostd",
    "register",
    "mmio",
];

const GAME_ENGINE: &[&str] = &[
    "bevy", "wgpu", "winit", "glam", "sprite", "ecs", "shader", "mesh", "texture", "frame",
    "vertex", "renderer", "physics", "collider", "entity", "gameloop",
];

const BACKEND: &[&str] = &[
    "axum",
    "actix",
    "tokio",
    "hyper",
    "tonic",
    "sqlx",
    "diesel",
    "rocket",
    "warp",
    "router",
    "handler",
    "service",
    "repository",
    "grpc",
    "endpoint",
    "middleware",
    "database",
    "server",
];

const FULL_STACK: &[&str] = &[
    "leptos",
    "yew",
    "dioxus",
    "sycamore",
    "seed",
    "dom",
    "html",
    "wasm",
    "component",
    "hydrate",
    "jsx",
    "props",
    "signal",
    "reactive",
    "viewmodel",
];
