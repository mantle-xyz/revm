use crate::{
    primitives::{
        address, db::Database, fixed_bytes, Address, Bytes, FixedBytes, LogData, TxKind, U256,
    },
    Context,
};
use alloy_primitives::Keccak256;
use revm_interpreter::Host;
use revm_precompile::{utilities::left_pad, Log};

const BVM_ETH_ADDR: Address = address!("dEAddEaDdeadDEadDEADDEAddEADDEAddead1111");
/// keccak("Mint(address,uint256)") =
/// "0x0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885"
const MINT_SELECTOR: FixedBytes<32> =
    fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");
/// keccak("Transfer(address,address,uint256)") =
/// "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
const TRANSFER_SELECTOR: FixedBytes<32> =
    fixed_bytes!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// Get the key for the BVM ETH balance.
/// references:
///  * <https://github.com/mantlenetworkio/op-geth/blob/develop/core/state_transition.go#L799>
fn get_bvm_eth_balance_key(addr: Address) -> U256 {
    let mut hasher = Keccak256::new();
    let position = [0u8; 32];
    let padding_addr = left_pad::<32>(addr.as_slice()).into_owned();
    hasher.update(padding_addr.as_ref()); // Prefix padding for address
    hasher.update(position.as_ref()); // Position
    U256::from_be_slice(&hasher.finalize().as_slice())
}

pub(crate) fn warm_bvm_eth_contract<EXT, DB: Database>(context: &mut Context<EXT, DB>) {
    let _ = context.load_account_delegated(BVM_ETH_ADDR).unwrap();
    // let _ = context.load_account_delegated(context.evm.inner.env.tx.caller).unwrap();
}

fn add_bvm_eth_total_supply<EXT, DB: Database>(context: &mut Context<EXT, DB>, eth_value: U256) {
    // add bvm eth total supply
    let bvm_eth_total_supply_key = U256::from(2);
    let mut value_supply = context
        .sload(BVM_ETH_ADDR, bvm_eth_total_supply_key)
        .unwrap()
        .data;
    println!("value_supply: {:?}", value_supply);
    value_supply = value_supply.saturating_add(eth_value);
    println!("value_supply: {:?}", value_supply);
    let _ = context
        .sstore(BVM_ETH_ADDR, bvm_eth_total_supply_key, value_supply)
        .unwrap();
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

pub(crate) fn mint_bvm_eth<EXT, DB: Database>(context: &mut Context<EXT, DB>, eth_value: U256) {
    let checkpoint = context.evm.journaled_state.checkpoint();
    let from = context.evm.inner.env.tx.caller;
    let key = get_bvm_eth_balance_key(from);
    let mut value = context.sload(BVM_ETH_ADDR, key).unwrap().data;
    value = value.saturating_add(eth_value);

    let Some(_) = context.sstore(BVM_ETH_ADDR, key, value) else {
        context.evm.journaled_state.checkpoint_revert(checkpoint);
        return;
    };

    add_bvm_eth_total_supply(context, eth_value);

    context.evm.touch(&BVM_ETH_ADDR);
    context.evm.touch(&from);
    context.evm.journaled_state.checkpoint_commit();

    let mint_log = generate_bvm_eth_mint_event(from, eth_value);
    context.log(mint_log);
}

pub(crate) fn transfer_bvm_eth<EXT, DB: Database>(context: &mut Context<EXT, DB>, eth_value: U256) {
    let checkpoint = context.evm.journaled_state.checkpoint();
    let from = context.evm.inner.env.tx.caller;
    let to = match context.evm.inner.env.tx.transact_to {
        TxKind::Call(caller) => caller,
        TxKind::Create => Address::ZERO,
    };

    if from == to {
        return;
    }

    let from_key = get_bvm_eth_balance_key(from);
    let to_key = get_bvm_eth_balance_key(to);

    let mut from_amount = context.sload(BVM_ETH_ADDR, from_key).unwrap().data;
    let mut to_amount = context.sload(BVM_ETH_ADDR, to_key).unwrap().data;

    // mock, modify it
    if from_amount < eth_value {
        return;
    }

    from_amount = from_amount.saturating_sub(eth_value);
    to_amount = to_amount.saturating_add(eth_value);

    let _ = context.sstore(BVM_ETH_ADDR, from_key, from_amount).unwrap();
    let _ = context.sstore(BVM_ETH_ADDR, to_key, to_amount).unwrap();

    context.evm.touch(&BVM_ETH_ADDR);
    context.evm.touch(&from);
    context.evm.touch(&to);
    context.evm.journaled_state.checkpoint_commit();

    let transfer_log = generate_bvm_eth_transfer_event(from, to, eth_value);
    context.log(transfer_log);
}

mod tests {
    use super::*;
    use core::str::FromStr;
    #[test]
    fn bvm_eth_balance_key_test() {
        let address: Address = address!("667120e768cf024c2245dd6d9feece4b437c3518");
        let key = get_bvm_eth_balance_key(address);
        let expected_key =
            U256::from_str("0xfe0b4acb70bd1e455f00a22786aa76d07a905b7f77d9cbab254e4dddcbb681c9")
                .unwrap();
        assert_eq!(key, expected_key);
    }
}
