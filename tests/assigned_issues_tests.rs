#![cfg(test)]

use soroban_sdk::{
    testutils::Address as _, testutils::Ledger as _, Address, Env, IntoVal, Symbol, Val, Vec,
};
use stellarflow_contracts::{
    orders::limit::AssetPair,
    vaults::interest::{InterestRateConfig, PoolState},
    TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
};

fn setup_env() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin, &admin);
    (env, client, admin)
}

#[test]
fn test_interest_rate_controller_utilization() {
    let (_, client, _) = setup_env();
    let u = client.calculate_utilization(&80, &20);
    assert_eq!(u, 2000); // 20%
}

#[test]
fn test_interest_rate_controller_rates() {
    let (_, client, _) = setup_env();
    let config = InterestRateConfig {
        base_rate_bps: 200,            // 2%
        multiplier_bps: 1000,          // 10%
        jump_multiplier_bps: 5000,     // 50%
        optimal_utilization_bps: 8000, // 80%
        ledgers_per_year: 6307200,
    };

    // Below optimal: 50% utilization
    let rate_50 = client.calculate_interest_rate(&5000, &config);
    assert_eq!(rate_50, 700); // 2% + 5% = 7%

    // Above optimal: 90% utilization
    let rate_90 = client.calculate_interest_rate(&9000, &config);
    assert_eq!(rate_90, 1500); // 2% + 8% (base slope) + 5% (jump slope) = 15%
}

#[test]
fn test_interest_rate_controller_accrue() {
    let (env, client, _) = setup_env();
    let config = InterestRateConfig {
        base_rate_bps: 200,
        multiplier_bps: 1000,
        jump_multiplier_bps: 5000,
        optimal_utilization_bps: 8000,
        ledgers_per_year: 6307200,
    };

    let pool = PoolState {
        cash: 20,
        borrows: 80,
        last_accrued_ledger: 0,
        accumulated_interest_index: 1_000_000_000_000_000_000, // 1.0 scaled
    };

    env.ledger().set_sequence(1000);
    let (updated_pool, accrued) = client.accrue_interest(&pool, &config);
    assert!(accrued > 0);
    assert_eq!(updated_pool.last_accrued_ledger, 1000);
}

#[test]
fn test_bytesn_optimization_hashing() {
    let (env, client, _) = setup_env();
    let addr = Address::generate(&env);
    let hashed_addr = client.optimize_address(&addr);
    assert_eq!(hashed_addr.len(), 32);

    let s = soroban_sdk::String::from_str(&env, "event_topic");
    let hashed_str = client.optimize_string(&s);
    assert_eq!(hashed_str.len(), 32);
}

#[test]
fn test_liquidity_depth_lifecycle() {
    let (env, client, _) = setup_env();
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);

    let maker = Address::generate(&env);
    soroban_sdk::token::StellarAssetClient::new(&env, &sell_asset).mint(&maker, &2_000);

    let pair = AssetPair {
        sell_asset: sell_asset.clone(),
        buy_asset: buy_asset.clone(),
    };

    // Ask order
    let order = client.place_limit_order(&maker, &pair, &10_000_000, &1_000);
    let is_bid = sell_asset > buy_asset;
    let depth = client.get_liquidity_depth(&pair, &is_bid);
    assert_eq!(depth.len(), 1);
    assert_eq!(depth.get(0).unwrap().volume, 1_000);

    client.cancel_limit_order(&maker, &order.id);
    let depth_after = client.get_liquidity_depth(&pair, &is_bid);
    assert_eq!(depth_after.len(), 0);
}

#[test]
fn test_auth_context_isolation_guard() {
    let (env, client, _) = setup_env();
    let expected = Address::generate(&env);
    client.enforce_auth_isolation(&expected);

    // Call execute_isolated_call towards self
    let args: Vec<Val> = Vec::new(&env);
    let result = client.try_execute_isolated_call(
        &client.address,
        &Symbol::new(&env, "get_recovery_key"),
        &args,
    );
    assert!(result.is_ok());
}

// ============================================================================
// Issue #1020: TWAP Oracle Dynamic Sample Window Inspector
// ============================================================================

/// Pricing must revert while fewer than `Nmin = 10` observations are present.
#[test]
fn test_twap_price_reverts_below_min_observations() {
    let (env, client, _) = setup_env();
    let asset = Symbol::new(&env, "XLM");

    env.ledger().with_mut(|li| {
        li.timestamp = 1_000_000;
    });
    for _ in 0..9 {
        client.record_twap_observation(&asset, &1_000);
    }

    let inspection = client.inspect_twap_window(&asset);
    assert_eq!(inspection.sample_count, 9);
    assert!(!inspection.sufficient);

    // Pricing call must fail closed on the thin sample set.
    assert!(client.try_get_twap_price(&asset).is_err());
}

/// Pricing succeeds once exactly `Nmin = 10` observations are present.
#[test]
fn test_twap_price_succeeds_at_min_observations() {
    let (env, client, _) = setup_env();
    let asset = Symbol::new(&env, "XLM");

    env.ledger().with_mut(|li| {
        li.timestamp = 1_000_000;
    });
    for _ in 0..10 {
        client.record_twap_observation(&asset, &2_000);
    }

    let inspection = client.inspect_twap_window(&asset);
    assert!(inspection.sufficient);
    assert_eq!(inspection.window_secs, 900);
    assert_eq!(client.get_twap_price(&asset), 2_000);
}

/// High volatility expands the observation window from 15 to 60 minutes.
#[test]
fn test_twap_window_expands_during_high_volatility() {
    let (env, client, _) = setup_env();
    let asset = Symbol::new(&env, "XLM");
    let base = 1_000_000u64;

    // Twelve observations spaced 5 minutes apart (~55 min span) with 10 %
    // tick-to-tick moves push realized volatility well above the threshold.
    let mut price = 1_000i128;
    for i in 0..12u64 {
        env.ledger().with_mut(|li| {
            li.timestamp = base - 3_300 + i * 300;
        });
        client.record_twap_observation(&asset, &price);
        price += price / 10;
    }
    env.ledger().with_mut(|li| {
        li.timestamp = base;
    });

    let inspection = client.inspect_twap_window(&asset);
    assert!(inspection.high_volatility);
    assert_eq!(inspection.window_secs, 3_600);
    assert_eq!(inspection.sample_count, 12);
    assert!(inspection.sufficient);
}
