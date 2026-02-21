//! Integration tests for the multi-AMM engine.
//! These tests run without any external shared library by using a pure-CPAMM
//! strategy implemented directly as a StrategyRunner substitute.

#[cfg(test)]
mod integration {
    use prop_amm_engine::capital::{risk_adjusted_score, softmax_weights};
    use prop_amm_engine::market::{
        gbm_step, generate_retail_orders, cpamm_output, route_order_n_amms, MarketParams,
    };
    use prop_amm_engine::types::{AmmState, SimConfig, SCALE, SCALE_F};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    // ── Unit: GBM ─────────────────────────────────────────────────────────────

    #[test]
    fn gbm_price_stays_positive() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut price = 100.0_f64;
        for _ in 0..10_000 {
            price = gbm_step(price, 0.005, &mut rng);
            assert!(price > 0.0, "price went non-positive: {price}");
        }
    }

    // ── Unit: CPAMM output monotone + concave ─────────────────────────────────

    #[test]
    fn cpamm_output_monotone_and_concave() {
        let rx = 100 * SCALE;
        let ry = 10_000 * SCALE;
        let fee_bps = 30;
        let step = SCALE / 10;
        let outputs: Vec<u64> = (1..=50)
            .map(|i| cpamm_output(i * step, rx, ry, fee_bps))
            .collect();

        // Monotone
        for w in outputs.windows(2) {
            assert!(w[1] >= w[0], "not monotone: {} < {}", w[1], w[0]);
        }

        // Concave (marginals non-increasing)
        let marginals: Vec<f64> = outputs.windows(2)
            .map(|w| (w[1] as f64 - w[0] as f64) / step as f64)
            .collect();
        for m in marginals.windows(2) {
            assert!(m[1] <= m[0] + 1e-8, "not concave: {} > {}", m[1], m[0]);
        }
    }

    // ── Unit: N-way router conserves total input ──────────────────────────────

    #[test]
    fn n_way_router_conserves_input() {
        // 3 identical CPAMMs: should split evenly
        let amms: Vec<AmmState> = (0..3)
            .map(|i| AmmState::new(100 * SCALE, 10_000 * SCALE, i as u8, &format!("AMM{i}")))
            .collect();

        let total_input = 100.0; // 100 Y, unscaled

        // CPAMM compute_swap for all three (identical parameters → identical outputs)
        let compute = |_amm_idx: usize, is_buy: bool, input: u64, rx: u64, ry: u64| -> u64 {
            if is_buy { cpamm_output(input, ry, rx, 30) }
            else       { cpamm_output(input, rx, ry, 30) }
        };

        let result = route_order_n_amms(&amms, true, total_input, compute);

        // Total allocation ≈ total_input
        let total_allocated: f64 = result.allocations.iter()
            .map(|&(inp, _)| inp as f64 / SCALE_F)
            .sum();
        assert!(
            (total_allocated - total_input).abs() < 0.1,
            "input not conserved: allocated={total_allocated:.4} vs input={total_input}"
        );

        // Symmetric split: each gets ~1/3
        for &(inp, _) in &result.allocations {
            let frac = inp as f64 / SCALE_F / total_input;
            assert!(
                (frac - 1.0/3.0).abs() < 0.05,
                "unequal split for symmetric AMMs: frac={frac:.3}"
            );
        }
    }

    // ── Unit: Capital allocation ──────────────────────────────────────────────

    #[test]
    fn capital_rebalance_favors_winner() {
        let scores = [500.0, 100.0, -50.0];
        let lambda = 2.0;
        let risk_scores: Vec<f64> = scores.iter().map(|&s| risk_adjusted_score(s, lambda)).collect();
        // risk_scores = [500, 100, -150]

        let weights = softmax_weights(&risk_scores, 1.0, 0.02);
        assert_eq!(weights.len(), 3);
        assert!(weights[0] > weights[1], "winner should have more capital");
        assert!(weights[1] > weights[2], "loser should have less capital");
        assert!(weights[2] >= 0.02, "min weight floor violated");

        let sum: f64 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    // ── Unit: Retail order generation is Poisson ──────────────────────────────

    #[test]
    fn retail_orders_approximately_poisson() {
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let params = MarketParams {
            sigma: 0.003,
            lambda: 0.8,
            order_size_mean: 20.0,
            norm_fee_bps: 30,
            norm_liquidity_mult: 1.0,
        };

        let n_steps = 10_000;
        let total_orders: usize = (0..n_steps)
            .map(|_| generate_retail_orders(&params, &mut rng).len())
            .sum();

        let mean = total_orders as f64 / n_steps as f64;
        // Should be close to lambda=0.8
        assert!(
            (mean - 0.8).abs() < 0.05,
            "mean orders/step = {mean:.3}, expected ≈ 0.8"
        );
    }

    // ── Integration: full epoch + rebalance ───────────────────────────────────

    #[test]
    fn epoch_rebalance_preserves_total_capital() {
        use prop_amm_engine::capital::rebalance_capital;

        let config = SimConfig::default();
        let mut amms: Vec<AmmState> = (0..4).map(|i| {
            let mut a = AmmState::new(100 * SCALE, 10_000 * SCALE, i as u8, &format!("S{i}"));
            a.epoch_edge = [200.0, 100.0, 50.0, -30.0][i]; // varied performance
            a
        }).collect();

        // Total Y capital before rebalance
        let total_y_before: u64 = amms.iter().map(|a| a.reserve_y * 2).sum();

        rebalance_capital(&mut amms, &config, 0);

        let total_y_after: u64 = amms.iter().map(|a| a.reserve_y * 2).sum();

        // Capital is conserved (within 1% rounding)
        let ratio = total_y_after as f64 / total_y_before as f64;
        assert!(
            (ratio - 1.0).abs() < 0.01,
            "capital not conserved: before={total_y_before} after={total_y_after} ratio={ratio:.4}"
        );

        // Epoch accumulators reset
        for amm in &amms {
            assert_eq!(amm.epoch_trade_count, 0);
            assert_eq!(amm.epoch_edge, 0.0);
        }

        // Best performer got more capital
        assert!(
            amms[0].capital_weight > amms[3].capital_weight,
            "winner should have more weight: {} vs {}",
            amms[0].capital_weight, amms[3].capital_weight
        );
    }
}
