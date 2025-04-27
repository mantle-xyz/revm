use crate::api::exec::OpContextTr;
use crate::transaction::error::{db_error, BvmEthError, OpTransactionError};
use alloy_sol_types::SolValue;
use revm::{
    context::{JournalTr, Transaction},
    primitives::{
        address, fixed_bytes, keccak256, Address, Bytes, FixedBytes, Log, LogData, TxKind, U256,
    },
    Database,
};
use std::fmt::Display;

/// The native token of Mantle is MNT, and BVM_ETH is an ERC20 address that serves as a wrapper token for ETH
const BVM_ETH_ADDR: Address = address!("dEAddEaDdeadDEadDEADDEAddEADDEAddead1111");
/// keccak("Mint(address,uint256)") =
/// "0x0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885"
const MINT_SELECTOR: FixedBytes<32> =
    fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");
/// keccak("Transfer(address,address,uint256)") =
/// "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
const TRANSFER_SELECTOR: FixedBytes<32> =
    fixed_bytes!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// Get the storage key for a BVM ETH balance
/// References:
/// * <https://github.com/mantlenetworkio/op-geth/blob/v1.1.1/core/state_transition.go#L803>
fn get_bvm_eth_balance_slot(addr: Address) -> U256 {
    keccak256((addr, U256::ZERO).abi_encode()).into()
}

/// Get the storage key for the total supply of BVM ETH
/// References:
/// * https://github.com/mantlenetworkio/op-geth/blob/v1.1.1/core/state_transition.go#L812>
fn get_bvm_eth_total_supply_slot() -> U256 {
    U256::from_limbs([2u64, 0, 0, 0])
}

pub(crate) fn warm_bvm_eth_contract<CTX, DBError>(context: &mut CTX) -> Result<(), DBError>
where
    CTX: OpContextTr,
    CTX::Db: Database<Error = DBError>,
{
    context.journal().load_account(BVM_ETH_ADDR)?;
    Ok(())
}

pub(crate) fn touch_bvm_eth_contract<CTX, DBError>(context: &mut CTX) -> Result<(), DBError>
where
    CTX: OpContextTr,
    CTX::Db: Database<Error = DBError>,
{
    context.journal().touch_account(BVM_ETH_ADDR);
    Ok(())
}

/// Add the value of ETH to the total supply of BVM ETH
fn add_bvm_eth_total_supply<CTX, DBError>(context: &mut CTX, eth_value: U256) -> Result<(), DBError>
where
    CTX: OpContextTr,
    CTX::Db: Database<Error = DBError>,
{
    let total_supply_slot = get_bvm_eth_total_supply_slot();
    let value_supply = context
        .journal()
        .sload(BVM_ETH_ADDR, total_supply_slot)?
        .data;

    let new_value_supply = value_supply.saturating_add(eth_value);

    context
        .journal()
        .sstore(BVM_ETH_ADDR, total_supply_slot, new_value_supply)?;

    Ok(())
}

/// Generate a mint event for BVM ETH
fn generate_bvm_eth_mint_event(from: Address, eth_value: U256) -> Log {
    let topics = vec![MINT_SELECTOR, from.into_word()];
    let data = Bytes::from(eth_value.to_be_bytes_vec());
    Log {
        address: BVM_ETH_ADDR,
        data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
    }
}

/// Generate a transfer event for BVM ETH
fn generate_bvm_eth_transfer_event(from: Address, to: Address, eth_value: U256) -> Log {
    let topics = vec![TRANSFER_SELECTOR, from.into_word(), to.into_word()];
    let data = Bytes::from(eth_value.to_be_bytes_vec());
    Log {
        address: BVM_ETH_ADDR,
        data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
    }
}

pub fn mint_bvm_eth<CTX, DBError: Display>(
    context: &mut CTX,
    eth_value: U256,
) -> Result<(), OpTransactionError>
where
    CTX: OpContextTr,
    CTX::Db: Database<Error = DBError>,
{
    let from = context.tx().caller();
    let slot = get_bvm_eth_balance_slot(from);
    let value = context
        .journal()
        .sload(BVM_ETH_ADDR, slot)
        .map_err(db_error)?
        .data;
    let new_value = value.saturating_add(eth_value);

    context
        .journal()
        .sstore(BVM_ETH_ADDR, slot, new_value)
        .map_err(db_error)?;

    add_bvm_eth_total_supply(context, eth_value).map_err(db_error)?;

    let mint_log = generate_bvm_eth_mint_event(from, eth_value);
    context.journal().log(mint_log);

    Ok(())
}

pub(crate) fn transfer_bvm_eth<CTX, DBError: Display>(
    context: &mut CTX,
    eth_value: U256,
) -> Result<(), OpTransactionError>
where
    CTX: OpContextTr,
    CTX::Db: Database<Error = DBError>,
{
    let from = context.tx().caller();
    let to = match context.tx().kind() {
        TxKind::Call(caller) => caller,
        TxKind::Create => {
            // Increase nonce of caller and check if it overflows
            let Some(nonce) = context
                .journal()
                .inc_account_nonce(from)
                .map_err(db_error)?
            else {
                return Err(OpTransactionError::BvmEth(BvmEthError::NonceOverflow));
            };
            let old_nonce = nonce - 1;
            from.create(old_nonce)
        }
    };
    if from == to {
        // no need to transfer to self
        return Ok(());
    }

    let from_slot = get_bvm_eth_balance_slot(from);
    let to_slot = get_bvm_eth_balance_slot(to);

    let from_amount = context
        .journal()
        .sload(BVM_ETH_ADDR, from_slot)
        .map_err(db_error)?
        .data;
    let to_amount = context
        .journal()
        .sload(BVM_ETH_ADDR, to_slot)
        .map_err(db_error)?
        .data;

    if from_amount < eth_value {
        return Err(OpTransactionError::BvmEth(BvmEthError::InsufficientFunds));
    }
    let new_from_amount = from_amount.saturating_sub(eth_value);
    let new_to_amount = to_amount.saturating_add(eth_value);

    context
        .journal()
        .sstore(BVM_ETH_ADDR, from_slot, new_from_amount)
        .map_err(db_error)?;
    context
        .journal()
        .sstore(BVM_ETH_ADDR, to_slot, new_to_amount)
        .map_err(db_error)?;

    let transfer_log = generate_bvm_eth_transfer_event(from, to, eth_value);
    context.journal().log(transfer_log);

    Ok(())
}

mod tests {
    use super::*;
    use alloy_sol_types::{sol, SolEvent};
    use std::str::FromStr;

    sol! {
        interface ERC20Events {
            event Transfer(address indexed from, address indexed to, uint256 value);
            event Mint(address indexed to, uint256 value);
        }
    }

    #[test]
    fn test_selector_from_event() {
        let selector = ERC20Events::Transfer::SIGNATURE_HASH;
        assert_eq!(selector, TRANSFER_SELECTOR);

        let selector = ERC20Events::Mint::SIGNATURE_HASH;
        assert_eq!(selector, MINT_SELECTOR);
    }

    #[test]
    fn bvm_eth_balance_slot_test() {
        let addr = address!("667120e768cf024c2245dd6d9feece4b437c3518");
        let slot = get_bvm_eth_balance_slot(addr);
        let expected =
            U256::from_str("0xfe0b4acb70bd1e455f00a22786aa76d07a905b7f77d9cbab254e4dddcbb681c9")
                .unwrap();
        assert_eq!(slot, expected);
    }

    #[test]
    fn bvm_eth_total_supply_slot_test() {
        assert_eq!(
            get_bvm_eth_total_supply_slot(),
            U256::from_str("0x0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap()
        );
    }
}
