//! Industry verticals (Manifesto section 5). The suggestion engine adapts its
//! advice to the detected (or declared) vertical.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Vertical {
    /// Embedded / IoT: absolute control of hardware resources.
    Embedded,
    /// Game engines: max performance, real-time (60+ FPS), data-oriented.
    GameEngine,
    /// Backend & microservices: prefer the modular monolith.
    Backend,
    /// Full stack / UI: reactivity, efficient rendering.
    FullStack,
    /// Not enough signal to decide.
    #[default]
    Unknown,
}

impl Vertical {
    pub fn as_str(self) -> &'static str {
        match self {
            Vertical::Embedded => "embedded",
            Vertical::GameEngine => "game-engine",
            Vertical::Backend => "backend",
            Vertical::FullStack => "full-stack",
            Vertical::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Vertical {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
