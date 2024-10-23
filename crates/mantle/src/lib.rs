//! Mantle-specific constants, types, and helpers.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc as std;

pub mod bn128;
pub mod fast_lz;
pub mod handler_register;
pub mod l1block;
pub mod result;
pub mod spec;
pub mod transaction;
pub mod wiring;

pub use handler_register::{
    deduct_caller, end, last_frame_return, load_precompiles, mantle_handle_register, output,
    refund, reimburse_caller, reward_beneficiary, validate_env, validate_initial_tx_gas,
    validate_tx_against_state,
};
pub use l1block::{L1BlockInfo, BASE_FEE_RECIPIENT, L1_BLOCK_CONTRACT};
pub use result::MantleHaltReason;
pub use spec::*;
pub use transaction::{error::OpTransactionError, OpTransaction, OpTransactionType};
