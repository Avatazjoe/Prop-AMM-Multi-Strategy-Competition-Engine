use crate::types::{AmmState, EpochSummary, SimConfig, SCALE};

/// Compute risk-adjusted score for a strategy's epoch performance.
///
/// score = epoch_edge - λ · max(0, -epoch_edge)
///
/// This penalizes strategies with negative epochs harder than it rewards positive ones,
/// implementing an asymmetric downside-averse capital allocation rule.
/// When epoch_edge >= 0, score = epoch_edge.
/// When epoch_edge < 0, score = epoch_edge * (1 + λ) = epoch_edge · (λ+1)
/// with λ=2.0 (default), a loss of -X scores as -3X.
pub fn risk_adjusted_score(epoch_edge: f64, lambda: f64) -> f64 {
    epoch_edge - lambda * f64::max(0.0, -epoch_edge)
}

/// Compute new capital weights via temperature-scaled softmax of risk-adjusted scores.
///
/// w_i = exp(score_i / T) / Σ exp(score_j / T)
///
/// Then clip each weight to [min_weight, 1.0] and renormalize.
/// High T → more uniform weights (exploration). Low T → winner-take-most (exploitation).
pub fn softmax_weights(scores: &[f64], temperature: f64, min_weight: f64) -> Vec<f64> {
    let n = scores.len();
    if n == 0 { return vec![]; }

    // Numerically stable softmax: subtract max before exp
    let max_score = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min_score = scores.iter().cloned().fold(f64::INFINITY, f64::min);
    let spread_scale = ((max_score - min_score) / 40.0).max(1.0);
    let exps: Vec<f64> = scores
        .iter()
        .map(|&s| ((s - max_score) / (temperature * spread_scale)).exp())
        .collect();
    let sum_exp: f64 = exps.iter().sum();

    let raw_weights: Vec<f64> = exps.iter().map(|&e| e / sum_exp).collect();

    let floor_total = min_weight * n as f64;
    let mut weights = if min_weight > 0.0 && floor_total < 1.0 {
        let remaining = 1.0 - floor_total;
        raw_weights
            .iter()
            .map(|&w| min_weight + remaining * w)
            .collect::<Vec<f64>>()
    } else {
        raw_weights
    };

    // Final normalization guard
    let total: f64 = weights.iter().sum();
    weights.iter_mut().for_each(|w| *w /= total);
    weights
}

/// Rebalance AMM reserves at an epoch boundary.
///
/// 1. Compute risk-adjusted scores for each strategy.
/// 2. Derive new capital weights via softmax.
/// 3. Scale each AMM's reserves to reflect its new weight (total capital is conserved).
///
/// Returns the list of epoch summaries (one per AMM), updated in-place.
pub fn rebalance_capital(
    amms: &mut [AmmState],
    config: &SimConfig,
    epoch_number: u32,
) -> Vec<EpochSummary> {
    // ── 1. Gather epoch stats ──────────────────────────────────────────────────
    let summaries: Vec<EpochSummary> = amms.iter().map(|amm| {
        let score = risk_adjusted_score(amm.epoch_edge, config.lambda);
        EpochSummary {
            epoch_number,
            edge: amm.epoch_edge,
            trade_count: amm.epoch_trade_count,
            arb_losses: f64::min(0.0, amm.epoch_edge),  // crude; engine can track separately
            retail_gains: f64::max(0.0, amm.epoch_edge),
            risk_adjusted_score: score,
        }
    }).collect();

    // ── 2. Compute new weights ─────────────────────────────────────────────────
    let scores: Vec<f64> = summaries.iter().map(|s| s.risk_adjusted_score).collect();
    let new_weights = softmax_weights(&scores, config.softmax_temperature, config.min_capital_weight);

    // ── 3. Compute total capital currently in the system (sum of each AMM's USD value)
    //    Capital of AMM i = 2 * reserve_y_i (assuming spot ≈ fair, so X value ≈ Y value)
    //    We conserve total Y-denominated capital.
    let total_capital_y: u128 = amms.iter()
        .map(|a| a.reserve_y as u128 * 2) // 2× because X+Y reserves are balanced at fair
        .sum();

    // ── 4. Rebalance: scale each AMM's reserves to match its new weight ─────────
    for (i, amm) in amms.iter_mut().enumerate() {
        let target_capital_y = (total_capital_y as f64 * new_weights[i]) as u128;
        // Each pool gets target_capital_y / 2 in Y reserves, and the same value in X
        let new_reserve_y = (target_capital_y / 2).max(SCALE as u128) as u64;
        // Actually: preserve the spot price. If spot = ry/rx, and we want new_ry:
        //   new_rx = new_ry / spot
        let spot = amm.reserve_y as f64 / amm.reserve_x as f64;
        let new_rx = (new_reserve_y as f64 / spot).max(1.0) as u64;

        amm.reserve_x = new_rx;
        amm.reserve_y = new_reserve_y;
        amm.capital_weight = new_weights[i];

        // Reset epoch accumulators
        amm.epoch_edge = 0.0;
        amm.epoch_trade_count = 0;
    }

    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_weights_sum_to_one() {
        let scores = vec![100.0, 200.0, 50.0, -50.0];
        let weights = softmax_weights(&scores, 1.0, 0.02);
        let sum: f64 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-10, "weights sum = {}", sum);
        assert!(weights.iter().all(|&w| w >= 0.019), "min weight violated");
    }

    #[test]
    fn risk_score_asymmetric() {
        let lambda = 2.0;
        assert_eq!(risk_adjusted_score(100.0, lambda), 100.0);
        assert_eq!(risk_adjusted_score(-50.0, lambda), -50.0 - 2.0 * 50.0);
        assert_eq!(risk_adjusted_score(0.0, lambda), 0.0);
    }

    #[test]
    fn uniform_scores_produce_near_uniform_weights() {
        let scores = vec![0.0; 5];
        let weights = softmax_weights(&scores, 1.0, 0.01);
        for w in &weights {
            assert!((w - 0.2).abs() < 1e-8);
        }
    }
}
