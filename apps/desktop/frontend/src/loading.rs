//! The loading overlay's model: what an analyze in flight looks like, built
//! from the `{phase, message}` progress events `lcw-desktop-api` emits
//! (config → discover → read → parse → analyze → advise → graph → layout →
//! done; cached loads go config → load → layout → done). Pure, no DOM.

/// The overlay's state. `Idle` hides it.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadState {
    Idle,
    Running(Running),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Running {
    /// Loading the saved graph rather than parsing.
    pub cached: bool,
    pub phase: String,
    pub message: String,
    /// 0..=1 once a known phase was seen; `None` shows an indeterminate bar.
    pub fraction: Option<f32>,
}

impl Running {
    pub fn start(cached: bool) -> Self {
        Running {
            cached,
            phase: String::new(),
            message: if cached {
                "opening the saved graph…".into()
            } else {
                "starting analysis…".into()
            },
            fraction: None,
        }
    }

    /// Fold one progress event in. The bar never moves backwards, except that
    /// a cached load falling back to a full parse starts over.
    pub fn advance(&mut self, phase: &str, message: &str) {
        self.phase = phase.to_string();
        self.message = message.to_string();
        if self.cached && phase == "analyze" {
            self.cached = false;
            self.fraction = None;
            return;
        }
        if let Some(f) = stage_fraction(phase, message) {
            self.fraction = Some(self.fraction.map_or(f, |prev| prev.max(f)));
        }
    }

    /// The step chips, each with whether it is done / current.
    pub fn steps(&self) -> Vec<(&'static str, StepState)> {
        let names: &[&'static str] = if self.cached {
            &["config", "cache", "layout"]
        } else {
            &["discover", "parse", "lenses", "layout"]
        };
        let current = step_of(&self.phase);
        let at = current.and_then(|c| names.iter().position(|n| *n == c));
        names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let state = match at {
                    Some(a) if i < a => StepState::Done,
                    Some(a) if i == a => StepState::Active,
                    _ if self.phase == "done" => StepState::Done,
                    _ => StepState::Pending,
                };
                (*n, state)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Active,
    Done,
}

/// Which step chip a phase belongs to.
fn step_of(phase: &str) -> Option<&'static str> {
    Some(match phase {
        "config" => "config",
        "discover" | "read" => "discover",
        "parse" => "parse",
        "load" => "cache",
        "analyze" | "advise" | "graph" => "lenses",
        "layout" => "layout",
        _ => return None,
    })
}

/// Progress through an analyze at `phase` (same table as the VS Code
/// extension's `stageFraction`).
pub fn stage_fraction(phase: &str, message: &str) -> Option<f32> {
    Some(match phase {
        "config" => 0.05,
        "discover" => 0.12,
        "read" => 0.22,
        "parse" if message.starts_with("parsed") => 0.55,
        "parse" => 0.4,
        "load" => 0.4,
        "analyze" => 0.65,
        "advise" => 0.75,
        "graph" => 0.8,
        "layout" => 0.88,
        "done" => 1.0,
        _ => return None,
    })
}

/// Human label for a phase in the stage line.
pub fn phase_label(phase: &str) -> &str {
    match phase {
        "load" => "cache",
        "analyze" => "lenses",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_is_monotonic_and_reaches_one() {
        let mut r = Running::start(false);
        assert_eq!(r.fraction, None);
        let mut last = 0.0;
        for (p, m) in [
            ("config", ""),
            ("discover", "found 44 files"),
            ("read", "reading 44 files"),
            ("parse", "parsing 44 files"),
            ("parse", "parsed 10 nodes / 3 edges"),
            ("analyze", ""),
            ("advise", ""),
            ("graph", ""),
            ("layout", ""),
            ("done", ""),
        ] {
            r.advance(p, m);
            let f = r.fraction.unwrap();
            assert!(f > last, "{p} {m}: {f} <= {last}");
            last = f;
        }
        assert_eq!(r.fraction, Some(1.0));
    }

    #[test]
    fn cache_fallback_switches_steps_and_restarts_the_bar() {
        let mut r = Running::start(true);
        r.advance("load", "loading cached graph");
        assert_eq!(r.steps()[1], ("cache", StepState::Active));
        r.advance("analyze", "no saved graph at /x — running full analysis");
        assert!(!r.cached);
        assert_eq!(r.fraction, None);
        r.advance("discover", "scanning repository");
        assert_eq!(r.fraction, Some(0.12));
        assert_eq!(r.steps()[0], ("discover", StepState::Active));
    }

    #[test]
    fn steps_mark_done_and_pending() {
        let mut r = Running::start(false);
        r.advance("analyze", "running lenses & metrics");
        let s: Vec<StepState> = r.steps().into_iter().map(|(_, s)| s).collect();
        assert_eq!(
            s,
            [
                StepState::Done,
                StepState::Done,
                StepState::Active,
                StepState::Pending
            ]
        );
        assert_eq!(phase_label("analyze"), "lenses");
        assert_eq!(stage_fraction("mystery", ""), None);
    }
}
