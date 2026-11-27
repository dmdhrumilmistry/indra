//! What an agent's tokens cost, for agents that report tokens but not
//! dollars: Codex (`turn.completed` usage), Gemini CLI (`result` stats), and
//! Claude Code when it reports no cost (a subscription login). Budgets and
//! account limits then hold for every agent and every kind of login.
//!
//! Prices are US dollars per million tokens, matched by the longest model
//! prefix. The built-in table is a list price estimate; `[[price]]` entries
//! in indra.toml come first and can correct or extend it:
//!
//! ```toml
//! [[price]]
//! model  = "gpt-5"        # a prefix: gpt-5, gpt-5-codex, gpt-5.1 ...
//! input  = 1.25
//! cached = 0.125          # cached input (default: input / 10)
//! output = 10
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceCfg {
    pub model: String,
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<f64>,
    /// Writing to the cache (Anthropic), default 1.25 x input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl PriceCfg {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |x: f64| x.is_finite() && (0.0..=10_000.0).contains(&x);
        if self.model.trim().is_empty() || self.model.len() > 128 {
            return Err("a [[price]] needs a model (a name or its prefix)".into());
        }
        if !ok(self.input)
            || !ok(self.output)
            || !self.cached.map(ok).unwrap_or(true)
            || !self.cache_write.map(ok).unwrap_or(true)
        {
            return Err(format!(
                "[[price]] {}: prices are US dollars per million tokens, 0 to 10000",
                self.model
            ));
        }
        Ok(())
    }
}

/// (prefix, input, cached input, output): list prices, per million tokens.
const BUILTIN: &[(&str, f64, f64, f64)] = &[
    // OpenAI (Codex)
    ("gpt-5-nano", 0.05, 0.005, 0.40),
    ("gpt-5-mini", 0.25, 0.025, 2.0),
    ("gpt-5", 1.25, 0.125, 10.0),
    ("codex-mini", 1.5, 0.375, 6.0),
    ("gpt-4.1-nano", 0.10, 0.025, 0.40),
    ("gpt-4.1-mini", 0.40, 0.10, 1.60),
    ("gpt-4.1", 2.0, 0.50, 8.0),
    ("o4-mini", 1.10, 0.275, 4.40),
    ("o3", 2.0, 0.50, 8.0),
    // Google (Gemini CLI)
    ("gemini-2.5-flash-lite", 0.10, 0.025, 0.40),
    ("gemini-2.5-flash", 0.30, 0.075, 2.50),
    ("gemini-2.5-pro", 1.25, 0.31, 10.0),
    ("gemini-3-pro", 2.0, 0.20, 12.0),
    ("gemini-3-flash", 0.50, 0.05, 3.0),
    // Anthropic (Claude Code)
    ("claude-opus-4-5", 5.0, 0.50, 25.0),
    ("claude-opus-4", 15.0, 1.50, 75.0),
    ("claude-sonnet", 3.0, 0.30, 15.0),
    ("claude-3-7-sonnet", 3.0, 0.30, 15.0),
    ("claude-haiku-4", 1.0, 0.10, 5.0),
    ("claude-3-5-haiku", 0.80, 0.08, 4.0),
    ("opus", 5.0, 0.50, 25.0),
    ("sonnet", 3.0, 0.30, 15.0),
    ("haiku", 1.0, 0.10, 5.0),
];

/// Token counts from one report.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Input tokens not read from a cache.
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub output: u64,
}

/// The price of `model` (input, cached, cache write, output), from the
/// configured table first, then the built-in one; None when unknown.
pub fn price_of(model: &str, configured: &[PriceCfg]) -> Option<(f64, f64, f64, f64)> {
    let m = model.trim().to_ascii_lowercase();
    // Provider prefixes: models/gemini-2.5-pro, openai/gpt-5, us.anthropic.claude-...
    let m = m.rsplit('/').next().unwrap_or(&m);
    let m = m.rsplit("anthropic.").next().unwrap_or(m).to_string();
    let best = configured
        .iter()
        .filter(|p| m.starts_with(&p.model.trim().to_ascii_lowercase()))
        .max_by_key(|p| p.model.len());
    if let Some(p) = best {
        return Some((
            p.input,
            p.cached.unwrap_or(p.input / 10.0),
            p.cache_write.unwrap_or(p.input * 1.25),
            p.output,
        ));
    }
    BUILTIN
        .iter()
        .filter(|(pre, ..)| m.starts_with(pre))
        .max_by_key(|(pre, ..)| pre.len())
        .map(|(_, i, c, o)| (*i, *c, *i * 1.25, *o))
}

/// What `usage` on `model` costs, in US dollars, when its price is known.
pub fn cost(model: &str, u: Usage, configured: &[PriceCfg]) -> Option<f64> {
    let (i, c, w, o) = price_of(model, configured)?;
    let m = 1_000_000.0;
    Some(
        u.input as f64 * i / m
            + u.cached as f64 * c / m
            + u.cache_write as f64 * w / m
            + u.output as f64 * o / m,
    )
}

/// The `[[price]]` table this process uses: the one in the settings the
/// console loaded last (spend is counted on the hub, from what each agent
/// reports).
static TABLE: std::sync::RwLock<Vec<PriceCfg>> = std::sync::RwLock::new(Vec::new());

pub fn set_table(t: Vec<PriceCfg>) {
    *TABLE.write().unwrap_or_else(|e| e.into_inner()) = t;
}

pub fn table() -> Vec<PriceCfg> {
    TABLE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_by_longest_prefix_configured_first() {
        let u = Usage {
            input: 1_000_000,
            cached: 0,
            cache_write: 0,
            output: 100_000,
        };
        // gpt-5-mini, not gpt-5.
        assert_eq!(cost("gpt-5-mini", u, &[]), Some(0.25 + 0.2));
        assert!((cost("gpt-5-codex", u, &[]).unwrap() - 2.25).abs() < 1e-9);
        assert!((cost("models/gemini-2.5-pro", u, &[]).unwrap() - 2.25).abs() < 1e-9);
        assert!((cost("claude-sonnet-4-5-20250929", u, &[]).unwrap() - 4.5).abs() < 1e-9);
        assert_eq!(cost("some-local-model", u, &[]), None);
        let mine = vec![PriceCfg {
            model: "gpt-5".into(),
            input: 2.0,
            output: 20.0,
            ..Default::default()
        }];
        assert!((cost("gpt-5.1-codex", u, &mine).unwrap() - 4.0).abs() < 1e-9);
        // Cached input defaults to a tenth of input.
        let c = Usage {
            cached: 1_000_000,
            ..Default::default()
        };
        assert!((cost("gpt-5", c, &mine).unwrap() - 0.2).abs() < 1e-9);
        assert!(PriceCfg {
            model: "x".into(),
            input: -1.0,
            output: 1.0,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
