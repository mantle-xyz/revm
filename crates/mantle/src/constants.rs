use revm::primitives::{address, Address, U256};

pub const ZERO_BYTE_COST: u64 = 4;
pub const NON_ZERO_BYTE_COST: u64 = 16;

pub const L1_BASE_FEE_SLOT: U256 = U256::from_limbs([1u64, 0, 0, 0]);
pub const L1_OVERHEAD_SLOT: U256 = U256::from_limbs([5u64, 0, 0, 0]);
pub const L1_SCALAR_SLOT: U256 = U256::from_limbs([6u64, 0, 0, 0]);
pub const TOKEN_RATIO_SLOT: U256 = U256::from_limbs([0u64, 0, 0, 0]);

// /// The address of L1 fee recipient.
// pub const L1_FEE_RECIPIENT: Address = address!("0x420000000000000000000000000000000000001A");

/// The address of the base fee recipient.
pub const BASE_FEE_RECIPIENT: Address = address!("0x4200000000000000000000000000000000000019");

/// The address of the L1Block contract.
pub const L1_BLOCK_CONTRACT: Address = address!("0x4200000000000000000000000000000000000015");

/// The address of the gas oracle contract.
pub const GAS_ORACLE_CONTRACT: Address = address!("420000000000000000000000000000000000000F");

/// The address of the sequencer fee wallet, which is block coinbase.
pub const SEQUENCER_FEE_VAULT_ADDRESS: Address = address!("4200000000000000000000000000000000000011");