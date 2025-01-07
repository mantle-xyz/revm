//! Optimism-specific constants, types, and helpers.

mod bn128;
mod fast_lz;
mod handler_register;
mod l1block;

mod bvm_eth;

pub use handler_register::{
    deduct_caller, end, last_frame_return, load_precompiles, optimism_handle_register,
    output, refund, reimburse_caller, reward_beneficiary, validate_env, validate_initial_tx_gas,
    validate_tx_against_state,
};
pub use l1block::{
    L1BlockInfo, BASE_FEE_RECIPIENT, GAS_ORACLE_CONTRACT, L1_BLOCK_CONTRACT, L1_FEE_RECIPIENT, SEQUENCER_FEE_VAULT_ADDRESS,
};
