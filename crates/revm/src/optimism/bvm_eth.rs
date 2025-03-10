use crate::{
    primitives::{
        address, db::Database, fixed_bytes, Address, Bytes, EVMError, FixedBytes, LogData, TxKind,
        U256,
    },
    Context,
};
use revm_interpreter::Host;
use revm_precompile::{utilities::left_pad, Log};
use revm_primitives::alloy_primitives::Keccak256;
use std::string::{String, ToString};
use std::vec::Vec;

const BVM_ETH_ADDR: Address = address!("dEAddEaDdeadDEadDEADDEAddEADDEAddead1111");
/// keccak("Mint(address,uint256)") =
/// "0x0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885"
const MINT_SELECTOR: FixedBytes<32> =
    fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");
/// keccak("Transfer(address,address,uint256)") =
/// "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
const TRANSFER_SELECTOR: FixedBytes<32> =
    fixed_bytes!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
const TOTAL_SUPPLY_KEY: U256 = U256::from_limbs([2u64, 0, 0, 0]);

/// Custom error types for BVM ETH operations
#[derive(Debug)]
pub enum BvmEthError<DBError> {
    EthTxValueTooLarge,
    StorageFailure,
    NonceOverflow,
    Custom(String),
    Database(DBError),
}

impl<DBError> From<BvmEthError<DBError>> for EVMError<DBError> {
    fn from(err: BvmEthError<DBError>) -> Self {
        match err {
            BvmEthError::EthTxValueTooLarge => {
                EVMError::Custom("eth tx value is too large".to_string())
            }
            BvmEthError::StorageFailure => EVMError::Custom("Storage operation failed".to_string()),
            BvmEthError::NonceOverflow => EVMError::Custom("Nonce overflow".to_string()),
            BvmEthError::Custom(msg) => EVMError::Custom(msg),
            BvmEthError::Database(db_err) => EVMError::Database(db_err),
        }
    }
}

/// Get the storage key for a BVM ETH balance
/// References:
/// * <https://github.com/mantlenetworkio/op-geth/blob/develop/core/state_transition.go#L799>
fn get_bvm_eth_balance_key(addr: Address) -> U256 {
    let mut hasher = Keccak256::new();
    let position = [0u8; 32];
    let padding_addr = left_pad::<32>(addr.as_slice()).into_owned();
    hasher.update(padding_addr.as_ref()); // Prefix padding for address
    hasher.update(position.as_ref()); // Position
    U256::from_be_slice(&hasher.finalize().as_slice())
}

pub(crate) fn warm_bvm_eth_contract<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
) -> Result<(), EVMError<DB::Error>> {
    context.evm.inner.load_account(BVM_ETH_ADDR)?;
    Ok(())
}

pub(crate) fn touch_bvm_eth_contract<EXT, DB: Database>(context: &mut Context<EXT, DB>) {
    context.evm.journaled_state.touch(&BVM_ETH_ADDR);
}

fn add_bvm_eth_total_supply<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    eth_value: U256,
) -> Result<(), EVMError<DB::Error>> {
    let mut value_supply = context.sload(BVM_ETH_ADDR, TOTAL_SUPPLY_KEY).unwrap().data;
    value_supply = value_supply.saturating_add(eth_value);
    context
        .sstore(BVM_ETH_ADDR, TOTAL_SUPPLY_KEY, value_supply)
        .ok_or(BvmEthError::StorageFailure)?;
    Ok(())
}

fn generate_bvm_eth_mint_event(from: Address, eth_value: U256) -> Log {
    let mut topics = Vec::with_capacity(2);
    topics.push(MINT_SELECTOR);
    topics.push(from.into_word());
    let data = Bytes::from(eth_value.to_be_bytes_vec());
    Log {
        address: BVM_ETH_ADDR,
        data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
    }
}

fn generate_bvm_eth_transfer_event(from: Address, to: Address, eth_value: U256) -> Log {
    let mut topics = Vec::with_capacity(3);
    topics.push(TRANSFER_SELECTOR);
    topics.push(from.into_word());
    topics.push(to.into_word());
    let data = Bytes::from(eth_value.to_be_bytes_vec());
    Log {
        address: BVM_ETH_ADDR,
        data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
    }
}

pub(crate) fn mint_bvm_eth<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    eth_value: U256,
) -> Result<(), EVMError<DB::Error>> {
    let from = context.evm.inner.env.tx.caller;
    let key = get_bvm_eth_balance_key(from);
    let mut value = context.sload(BVM_ETH_ADDR, key).unwrap().data;
    value = value.saturating_add(eth_value);

    context
        .sstore(BVM_ETH_ADDR, key, value)
        .ok_or(BvmEthError::StorageFailure)?;

    add_bvm_eth_total_supply(context, eth_value)?;

    let mint_log = generate_bvm_eth_mint_event(from, eth_value);
    context.log(mint_log);

    Ok(())
}

pub(crate) fn transfer_bvm_eth<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    eth_value: U256,
) -> Result<(), EVMError<DB::Error>> {
    let from = context.evm.inner.env.tx.caller;
    let to = match context.evm.inner.env.tx.transact_to {
        TxKind::Call(caller) => caller,
        TxKind::Create => {
            // Increase nonce of caller and check if it overflows
            let Some(nonce) = context.evm.journaled_state.inc_nonce(from) else {
                // can't happen on mainnet.
                return Err(BvmEthError::NonceOverflow.into());
            };
            let old_nonce = nonce - 1;
            from.create(old_nonce)
        }
    };
    if from == to {
        // no need to transfer to self
        return Ok(());
    }

    let from_key = get_bvm_eth_balance_key(from);
    let to_key = get_bvm_eth_balance_key(to);

    let mut from_amount = context.sload(BVM_ETH_ADDR, from_key).unwrap().data;
    let mut to_amount = context.sload(BVM_ETH_ADDR, to_key).unwrap().data;

    if from_amount < eth_value {
        return Err(BvmEthError::EthTxValueTooLarge.into());
    }

    from_amount = from_amount.saturating_sub(eth_value);
    to_amount = to_amount.saturating_add(eth_value);

    context
        .sstore(BVM_ETH_ADDR, from_key, from_amount)
        .ok_or(BvmEthError::StorageFailure)?;
    context
        .sstore(BVM_ETH_ADDR, to_key, to_amount)
        .ok_or(BvmEthError::StorageFailure)?;

    let transfer_log = generate_bvm_eth_transfer_event(from, to, eth_value);
    context.log(transfer_log);

    Ok(())
}

mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn bvm_eth_balance_key_test() {
        let addr = address!("667120e768cf024c2245dd6d9feece4b437c3518");
        let key = get_bvm_eth_balance_key(addr);
        let expected =
            U256::from_str("0xfe0b4acb70bd1e455f00a22786aa76d07a905b7f77d9cbab254e4dddcbb681c9")
                .unwrap();
        assert_eq!(key, expected);
    }

    #[test]
    fn bvm_eth_total_supply_key_test() {
        assert_eq!(
            TOTAL_SUPPLY_KEY,
            U256::from_str("0x0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap()
        );
    }
}
