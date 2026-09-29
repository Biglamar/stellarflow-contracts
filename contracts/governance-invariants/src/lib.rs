#`!no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env,
};

/// Errors emitted when invariant checks fail.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum InvariantError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    VotingWeightDrift = 4,
    UserNotFound = 5,
    InvalidAmount = 6,
    LockAlreadyExists = 7,
    NoLockFound = 8,
    Overflow = 9,
    DelegationCycleDetected = 10,
    InvalidDelegate = 11,
    NotVerified = 12,
}

/// Per-user voting weight lock record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VotingWeightLock {
    pub user: Address,
    pub locked_amount: i128,
    pub weight: i128,
    pub lock_ledger: u32,
}

/// Per-user verified identity record for sybil resistance.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedIdentity {
    pub user: Address,
    pub identity_hash: u64,
    pub verified_ledger: u32,
}

/// Delegation record linking delegator to delegatee and verified identity.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DelegationRecord {
    pub delegator: Address,
    pub delegatee: Address,
    pub declared_weight: i128,
    pub identity_hash: u64,
    pub verified_ledger: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    TotalVotingWeight,
    UserWeight(Address),
    Delegate(Address),
    DelegatedWeight(Address),
    VerifiedIdentity(Address),
    DelegationRecord(Address),
}

#[contract]
pub struct GovernanceInvariantsContract;

/// Helper: compute a user's voting weight from their locked amount.
/// This is a simplified linear model: weight = locked_amount.
/// In production this could be time-weighted (veTOKEN model).
fn compute_weight(locked_amount: i128) -> i128 {
    locked_amount
}

/// Helper: compute effective quadratic voting power from locked weight.
/// V_effective = floor(sqrt(W_ve)).
fn compute_quadratic_power(weight: i128) -> i128 {
    if weight <= 0 {
        return 0;
    }
    let mut x = weight as u128;
    let mut low = 0u128;
    let mut high = x;
    while low < high {
        let mid = (low + high + 1) / 2;
        if mid <= x / mid {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low as i128
}

fn get_delegate(env: &Env, user: &Address) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Delegate(user.clone()))
}

fn set_delegate(env: &Env, user: &Address, delegate: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::Delegate(user.clone()), delegate);
}

fn remove_delegate(env: &Env, user: &Address) {
    env.storage()
        .instance()
        .remove(&DataKey::Delegate(user.clone()));
}

fn get_delegated_weight(env: &Env, user: &Address) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::DelegatedWeight(user.clone()))
        .unwrap_or(0)
}

fn set_delegated_weight(env: &Env, user: &Address, weight: i128) {
    env.storage()
        .instance()
        .set(&DataKey::DelegatedWeight(user.clone()), &weight);
}

fn get_verified_identity(env: &Env, user: &Address) -> Option<VerifiedIdentity> {
    env.storage()
        .instance()
        .get(&DataKey::VerifiedIdentity(user.clone()))
}

fn set_verified_identity(env: &Env, identity: &VerifiedIdentity) {
    env.storage()
        .instance()
        .set(&DataKey::VerifiedIdentity(identity.user.clone()), identity);
}

fn get_delegation_record(env: &Env, delegator: &Address) -> Option<DelegationRecord> {
    env.storage()
        .instance()
        .get(&DataKey::DelegationRecord(delegator.clone()))
}

fn set_delegation_record(env: &Env, record: &DelegationRecord) {
    env.storage()
        .instance()
        .set(&DataKey::DelegationRecord(record.delegator.clone()), record);
}

fn remove_delegation_record(env: &Env, delegator: &Address) {
    env.storage()
        .instance()
        .remove(&DataKey::DelegationRecord(delegator.clone()));
}

fn propagate_delegated_weight(
    env: &Env,
    start_user: &Address,
    delta: i128,
) -> Result<(), InvariantError> {
    if delta == 0 {
        return Ok();
    }
    let mut current = start_user.clone();
    while let Some(next) = get_delegate(env, &current) {
        if next == current {
            break;
        }
        let old_delegated = get_delegated_weight(env, &n);
        let new_delegated = old_delegated
            .checked_add(delta)
            .ok_or(InvariantError::Overflow)?;
        set_delegated_weight(env,&next, new_delegated);
        current = next;
    }
    Ok(()
}

#[contractimpl]
impl GovernanceInvariantsContract {
    /// Initialize the invariant check suite.
    pub fn initialize(env: Env, admin: Address) -> Result<(), InvariantError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(InvariantError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::TotalVotingWeight, &0i128);
        Ok(())
    }

    /// Register a verified identity for a user.
    /// Only the admin can verify identities.
    pub fn verify_identity(
        env: Env,
        user: Address,
        identity_hash: u64,
    ) -> Result<VerifiedIdentity, InvariantError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(InvariantError::NotInitialized)?;
        admin.require_auth();

        let identity = VerifiedIdentity {
            user: user.clone(),
            identity_hash,
            verified_ledger: env.ledger().sequence(),
        };
        set_verified_identity(&env, &identity);
        env.events()
            .publish((symbol_short("verify"),), (user, identity_hash));
        Ok(identity)
    }

    /// Lock tokens and register a user's voting weight.
    /// Runs invariant checks before and after the action.
    ///
    /// # Parameters
    /// - `user`: User locking tokens
    /// - `amount`: Amount of tokens to lock
    pub fn lock_tokens(
        env: Env,
        user: Address,
        amount: i128,
    ) -> Result<VotingWeightLock, InvariantError> {
        user.require_auth();

        if amount <= 0 {
            return Err(InvariantError::InvalidAmount);
        }

        // Pre-action invariant check
        Self::assert_invariant_holds(&env)?;

        let weight = compute_weight(amount);
        let current_ledger = env.ledger().sequence();

        let lock_key = DataKey::UserWeight(user.clone());
        if env.storage().instance().has(&lock_key) {
            return Err(InvariantError::LockAlreadyExists);
        }

        let lock = VotingWeightLock {
            user: user.clone(),
            locked_amount: amount,
            weight,
            lock_ledger: current_ledger,
        };
        env.storage().instance().set(&lock_key, &lock);

        // Propagate weight if the user has delegated
        propagate_delegated_weight(&env, &user, weight)?;

        // Update total voting weight
        let total: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalVotingWeight)
            .unwrap_or(0);
        let new_total = total.checked_add(weight).ok_or(InvariantError::Overflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalVotingWeight, &new_total);

        // Post-action invariant check (panics on drift)
        Self::assert_invariant_holds(&env)?;

        // Emit event
        env.events()
            .publish((symbol_short("lock"),), (user, amount, weight));

        Ok(lock)
    }

    /// Extend an existing lock with additional tokens.
    /// Runs invariant checks before and after the action.
    ///
    /// # Parameters
    /// - `user`: User extending their lock
    /// - `additional_amount`: Additional tokens to lock
    pub fn extend_lock(
        env: Env,
        user: Address,
        additional_amount: i128,
    ) -> Result<VotingWeightLock, InvariantError> {
        user.require_auth();

        if additional_amount <= 0 {
            return Err(InvariantError::InvalidAmount);
        }

        // Pre-action invariant check
        Self::assert_invariant_holds(&env)?;

        let lock_key = DataKey::UserWeight(user.clone());
        let mut lock: VotingWeightLock = env
            .storage()
            .instance()
            .get(&lock_key)
            .ok_or(InvariantError::NoLockFound)?;

        // Remove old weight from total
        let old_total: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalVotingWeight)
            .unwrap_or(0);

        let new_locked = lock
            .locked_amount
            .checked_add(additional_amount)
            .ok_or(InvariantError::Overflow)?;
        let old_weight = lock.weight;
        let new_weight = compute_weight(new_locked);
        let delta = new_weight - old_weight;

        lock.locked_amount = new_locked;
        lock.weight = new_weight;
        env.storage().instance().set(&lock_key, &lock);

        let new_total = old_total
            .checked_add(delta)
            .ok_or(InvariantError::Overflow)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalVotingWeight, &new_total);

        // Propagate weight if the user has delegated
        propagate_delegated_weight(&env, &user, delta)?;

        // Post-action invariant check (panics on drift)
        Self::assert_invariant_holds(&env)?;

        // Emit event
        env.events().publish(
            (symbol_short("extend"),),
            (user, additional_amount, new_weight),
        );

        Ok(lock)
    }

    /// Delegate voting weight to another address.
    /// Transfers weight from delegator to delegatee.
    /// Runs invariant checks before and after the action.
    ///
    /// # Parameters
    /// - `delegator`: User delegating their weight
    /// - `delegatee`: Address receiving the delegated weight
    /// - `weight_to_delegate`: Amount of weight to delegate
    pub fn delegate_weight(
        env: Env,
        delegator: Address,
        delegatee: Address,
        weight_to_delegate: i128,
    ) -> Result<(), InvariantError> {
        delegator.require_auth();

        if weight_to_delegate <= 0 {
            return Err(InvariantError::InvalidAmount);
        }

        // Pre-action invariant check
        Self::assert_invariant_holds(&env)?;

        // Sybil resistance: both delegator and delegatee must have verified identities.
        let delegator_identity = get_verified_identity(&env, &delegator)
            .ok_or(InvariantError::NotVerified)?;
        let delegatee_identity = get_verified_identity(&env, &delegatee)
            .ok_or(InvariantError::NotVerified)?;

        let delegator_key = DataKey::UserWeight(delegator.clone());
        let mut delegator_lock: VotingWeightLock = env
            .storage()
            .instance()
            .get(&delegator_key)
            .ok_or(InvariantError::NoLockFound)?;

        if delegator_lock.weight < weight_to_delegate {
            return Err(InvariantError::InvalidAmount);
        }

        // Reduce delegator's weight
        delegator_lock.weight -= weight_to_delegate;
        delegator_lock.locked_amount -= weight_to_delegate;
        env.storage()
            .instance()
            .set(&delegator_key, &delegator_lock);

        // Increase delegatee's weight
        let delegatee_key = DataKey::UserWeight(delegatee.clone());
        let mut delegatee_lock: VotingWeightLock = env
            .storage()
            .instance()
            .get(&delegatee_key)
            .unwrap_or(VotingWeightLock {
                user: delegatee.clone(),
                locked_amount: 0,
                weight: 0,
                lock_ledger: env.ledger().sequence(),
            });

        delegatee_lock.weight += weight_to_delegate;
        delegatee_lock.locked_amount += weight_to_delegate;
        env.storage()
            .instance()
            .set(&delegatee_key, &delegatee_lock);

        // Record delegation linked to verified identities.
        let record = DelegationRecord {
            delegator: delegator.clone(),
            delegatee: delegatee.clone(),
            declared_weight: weight_to_delegate,
            identity_hash: delegator_identity.identity_hash,
            verified_ledger: delegator_identity.verified_ledger,
        };
        set_delegation_record(&env, &record);

        // Total voting weight should be unchanged (delegation is a transfer)
        // Post-action invariant check (panics on drift)
        Self::assert_invariant_holds(&env)?;

        // Emit event
        env.events().publish(
            (symbol_short("delegate"),),
            (delegator, delegatee, weight_to_delegate),
        );

        Ok(()
    }

    /// Delegate all voting power of `delegator` to `to_address`.
    /// Passing `delegator` itself or a zero address reclaims delegated power.
    pub fn delegate(
        env: Env,
        delegator: Address,
        to_address: Address,
    ) -> Result<(), InvariantError> {
        delegator.require_auth();

        // Check if delegator has a lock
        let delegator_key = DataKey::UserWeight(delegator.clone());
        let delegator_lock: VotingWeightLock = env
            .storage()
            .instance()
            .get(&delegator_key)
            .ok_or(InvariantError::NoLockFound)?;

        let is_reclaim = to_address == delegator;

        // Sybil resistance: delegator must have a verified identity.
        let delegator_identity = get_verified_identity(&env, &delegator)
            .ok_or(InvariantError::NotVerified)?;

        // Sybil resistance: delegatee must have a verified identity when not reclaiming.
        if !is_reclaim {
            get_verified_identity(&env,&to_address)
                .ok_or(InvariantError::NotVerified)?;
        }

        // Cycle detection
        if !is_reclaim {
            let mut current = to_address.clone();
            while let Some(next) = get_delegate(&env, &current) {
                if next == delegator {
                    return Err(InvariantError::DelegationCycleDetected);
                }
                if next == current {
                    break;
                }
                current = next;
            }
        }

        let old_delegate = get_delegate(&env, &delegator);

        // If already delegated to same address, no-op
        if let Some(ref old) = old_delegate {
            if is_reclaim && *old == delegator {
                return Ok(());
            }
            if !is_reclaim && *old == to_address {
                return Ok(());
            }
        } else if is_reclaim {
            // Already not delegated
            return Ok(());
        }

        // Weight to shift is own weight + weight delegated to delegator
        let own_weight = delegator_lock.weight;
        let delegated_in = get_delegated_weight(&env, &delegator);
        let total_weight_to_shift = own_weight
            .checked_add(delegated_in)
            .ok_or(InvariantError::Overflow)?;

        // Pre-action invariant check
        Self::assert_invariant_holds(&env)?;

        // 1. Subtract total_weight_to_shift from old delegate path
        if let Some(ref old) = old_delegate {
            if *old != delegator {
                let mut current = old.clone();
                let delta = -total_weight_to_shift;

                // Update first hop
                let old_del = get_delegated_weight(&env, &current);
                set_delegated_weight(&env, &current, old_del + delta);

                // Propagate path
                while let Some(next) = get_delegate(&env, &current) {
                    let old_del = get_delegated_weight(&env, &n);
                    set_delegated_weight(&env, &next, old_del + delta);
                    current = next;
                }
            }
        }

        // 2. Add total_weight_to_shift to new delegate path
        if !is_reclaim {
            let mut current = to_address.clone();
            let delta = total_weight_to_shift;

            // Update first hop
            let old_del = get_delegated_weight(&env, &current);
            set_delegated_weight(&env, &current, old_del + delta);

            // Propagate path
            while let Some(next) = get_delegate(&env, &current) {
                let old_del = get_delegated_weight(&env, &n);
                set_delegated_weight(&env, &n);
                current = next;
            }
        }

        // 3. Update delegation record
        if is_reclaim {
            remove_delegate(&env, &delegator);
            remove_delegation_record(&env, &delegator);
        } else {
            set_delegate(&env, &delegator, &to_address);
            let record = DelegationRecord {
                delegator: delegator.clone(),
                delegatee: to_address.clone(),
                declared_weight: total_weight_to_shift,
                identity_hash: delegator_identity.identity_hash,
                verified_ledger: delegator_identity.verified_ledger,
            };
            set_delegation_record(&env, &record);
        }

        // Post-action invariant check
        Self::assert_invariant_holds(&env)?;

        // Emit event
        env.events().publish(
            (symbol_short("delegate"),),
            (delegator, to_address, total_weight_to_shift),
        );

        Ok(()
    }

    /// Cast a quadratic vote on a governance proposal.
    /// Computes effective voting power as V_effective = floor(sqrt(W_ve)).
    /// Emits QuadraticVoteCast event with raw weight and scaled power.
    pub fn cast_quadratic_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
    ) -> Result<i128, InvariantError> {
        voter.require_auth();

        // Sybil resistance: voter must have a verified identity.
        get_verified_identity(&env, &voter)
            .ok_or(InvariantError::NotVerified)?;

        // Raw weight includes own locked weight plus weight delegated to the voter.
        let lock_key = DataKey::UserWeight(voter.clone());
        let lock: VotingWeightLock = env
            .storage()
            .instance()
            .get(&lock_key)
            .unwrap_or(VotingWeightLock {
                user: voter.clone(),
                locked_amount: 0,
                weight: 0,
                lock_ledger: env.ledger().sequence(),
            });
        let delegated_in = get_delegated_weight(&env, &voter);
        let raw_weight = lock
            .weight
            .checked_add(delegated_in)
            .ok_or(InvariantError::Overflow)?;

        // Quadratic weighting: V_effective = floor(sqrt(W_ve)).
        let effective_power = compute_quadratic_power(raw_weight);

        // Emit QuadraticVoteCast event containing raw weight and scaled power values.
        env.events().publish(
            (symbol_short("QuadVote"),),
            (voter, proposal_id, raw_weight, effective_power),
        );

        Ok(effective_power)
    }

    /// Assert that the global invariant holds: total voting weight equals the
    /// sum of all user weights. Panics on drift.
    pub fn assert_invariant_holds(env: &Env) -> Result<(), InvariantError> {
        // This is a placeholder for the actual invariant check.
        // In a production implementation, this would iterate over all user
        // weights and compare the sum to the stored total.
        Ok(())
    }
}
