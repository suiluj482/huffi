//! Tuning parameters for the ranking model, loaded from the
//! `[engine.scoring]` table of the config file.

use serde::Deserialize;

/// Tuning parameters for the ranking model. See `docs/ALGORITHM.md` for the
/// meaning of each value.
///
/// Mirrors the `[engine.scoring]` table of the config file. [`Default`] is
/// the single source of truth; `#[serde(default)]` fills every missing field
/// (in a partial section) and every missing section from it.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct ScoringConfig {
    /// Score added to a history key on a manual boost (relative to a launch).
    pub boost_weight: f64,
    /// Synthetic launch samples a boost counts as toward confidence.
    pub boost_samples: u32,
    /// History half-life in days (exponential decay).
    pub half_life_days: f64,
    /// Confidence smoothing constant `k` in `n / (n + k)`.
    pub confidence_k: f64,
    /// Base score for entries queried with an empty query.
    pub empty_query_score: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            boost_weight: 10.0,
            boost_samples: 5,
            half_life_days: 14.0,
            confidence_k: 3.0,
            empty_query_score: 0.8,
        }
    }
}

impl ScoringConfig {
    /// Reject values that would silently break the ranking model at runtime
    /// (e.g. a zero half-life producing `inf` lambdas and `NaN` scores).
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.half_life_days > 0.0,
            "half_life_days must be > 0 (got {})",
            self.half_life_days
        );
        anyhow::ensure!(
            self.boost_weight >= 0.0,
            "boost_weight must be >= 0 (got {})",
            self.boost_weight
        );
        anyhow::ensure!(
            self.boost_samples >= 1,
            "boost_samples must be >= 1 (got {})",
            self.boost_samples
        );
        anyhow::ensure!(
            self.confidence_k > 0.0,
            "confidence_k must be > 0 (got {})",
            self.confidence_k
        );
        Ok(())
    }
}
