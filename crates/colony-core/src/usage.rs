//! Token counts and the spend ledger behind the cost rollups.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
}

impl Tokens {
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
    }

    pub fn is_zero(&self) -> bool {
        *self == Tokens::default()
    }

    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

/// Spend in one time bucket. `cost_usd` is an estimate and leaves out models
/// without a known price; `partial` says that happened.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    #[serde(default)]
    pub tokens: Tokens,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub partial: bool,
    /// The same spend split by model, in the order each first appeared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_model: Vec<ModelCost>,
}

/// What one model (see `model_key`) accounts for in a `Spend`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    pub model: String,
    /// All four kinds of token added up.
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub partial: bool,
    /// Advisor consultations (only on `advisor:` keys).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub consults: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Add to the entry for `model` in a per-model split, making it if need be.
pub fn file_under(by_model: &mut Vec<ModelCost>, model: &str, t: &Tokens, cost: Option<f64>, consults: u32) {
    let i = by_model.iter().position(|m| m.model == model).unwrap_or_else(|| {
        by_model.push(ModelCost { model: model.to_string(), ..Default::default() });
        by_model.len() - 1
    });
    let m = &mut by_model[i];
    m.tokens += t.total();
    m.consults += consults;
    match cost {
        Some(c) => m.cost_usd += c,
        None => m.partial = true,
    }
}

impl Spend {
    pub fn add(&mut self, t: &Tokens, cost: Option<f64>) {
        self.tokens.add(t);
        match cost {
            Some(c) => self.cost_usd += c,
            None => self.partial = true,
        }
    }

    /// As `add`, and filed under the model (`model_key`) that spent it.
    pub fn add_for(&mut self, model: &str, t: &Tokens, cost: Option<f64>, consults: u32) {
        self.add(t, cost);
        file_under(&mut self.by_model, model, t, cost, consults);
    }

    /// The entry for a model key, if it has spent anything.
    pub fn model(&self, model: &str) -> Option<&ModelCost> {
        self.by_model.iter().find(|m| m.model == model)
    }
}

/// Key for the per-model split: the family ("opus"), `advisor:` before it for
/// advisor consultations, "other" for a model that isn't a known family.
pub fn model_key(model: Option<&str>, advisor: bool) -> String {
    let name = model.and_then(crate::models::family).map_or("other", |f| f.key());
    if advisor {
        format!("advisor:{name}")
    } else {
        name.to_string()
    }
}

/// Width of a ledger bucket. Small enough that any time zone's midnight
/// (even one at :45) lands on a bucket edge, so the map can total "today".
pub const BUCKET_MS: u64 = 15 * 60_000;
/// Buckets older than this are dropped.
pub const LEDGER_KEEP_MS: u64 = 48 * 60 * 60_000;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSpend {
    pub name: String,
    /// Bucket number (unix ms / `BUCKET_MS`) to spend in it.
    pub buckets: std::collections::BTreeMap<u64, Spend>,
}

/// Most response times kept per project per bucket; later ones are dropped.
pub const LATENCY_SAMPLES_MAX: usize = 100;

/// How long agents waited on a human in one project. Each sample is a wait
/// that ended with a response, in ms, filed under the bucket it ended in.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLatency {
    pub name: String,
    /// Bucket number (unix ms / `BUCKET_MS`) to the waits that ended in it.
    pub buckets: std::collections::BTreeMap<u64, Vec<u64>>,
}
