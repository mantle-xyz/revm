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
use std::vec;

pub trait BvmEthContextTrait: OpContextTr {
    type DbError: Display;
}

impl<T, E> BvmEthContextTrait for T
where
    T: OpContextTr,
    T::Db: Database<Error = E>,
    E: Display,
{
    type DbError = E;
}

pub struct BvmEth;

impl BvmEth {
    /// The native token of Mantle is MNT, and BVM_ETH is an ERC20 address that serves as a wrapper token for ETH
    pub const ADDRESS: Address = address!("dEAddEaDdeadDEadDEADDEAddEADDEAddead1111");

    /// keccak("Mint(address,uint256)")
    const MINT_SELECTOR: FixedBytes<32> =
        fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");

    /// keccak("Transfer(address,address,uint256)")
    const TRANSFER_SELECTOR: FixedBytes<32> =
        fixed_bytes!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

    /// Get the storage key for a BVM ETH balance
    pub fn get_balance_slot(addr: Address) -> U256 {
        keccak256((addr, U256::ZERO).abi_encode()).into()
    }

    /// Get the storage key for the total supply of BVM ETH
    pub fn get_total_supply_slot() -> U256 {
        U256::from_limbs([2u64, 0, 0, 0])
    }

    /// Mint BVM ETH for a given context and amount
    pub fn mint<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        context
            .journal()
            .load_account(Self::ADDRESS)
            .map_err(db_error)?;

        let from = context.tx().caller();
        Self::mint_inner(context, from, eth_value)?;

        context.journal().touch_account(Self::ADDRESS);
        Ok(())
    }

    /// Transfer BVM ETH for a given context and amount
    pub fn transfer<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        context
            .journal()
            .load_account(Self::ADDRESS)
            .map_err(db_error)?;

        Self::transfer_inner(context, eth_value)?;

        context.journal().touch_account(Self::ADDRESS);
        Ok(())
    }

    /// Add the value of ETH to the total supply of BVM ETH
    fn add_total_supply<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        let total_supply_slot = Self::get_total_supply_slot();
        let value_supply = context
            .journal()
            .sload(Self::ADDRESS, total_supply_slot)
            .map_err(db_error)?
            .data;

        let new_value_supply = value_supply.saturating_add(eth_value);

        context
            .journal()
            .sstore(Self::ADDRESS, total_supply_slot, new_value_supply)
            .map_err(db_error)?;

        Ok(())
    }

    /// Generate a mint event for BVM ETH
    fn generate_mint_event(to: Address, eth_value: U256) -> Log {
        let topics = vec![Self::MINT_SELECTOR, to.into_word()];
        let data = Bytes::from(eth_value.to_be_bytes_vec());
        Log {
            address: Self::ADDRESS,
            data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
        }
    }

    /// Generate a transfer event for BVM ETH
    fn generate_transfer_event(from: Address, to: Address, eth_value: U256) -> Log {
        let topics = vec![Self::TRANSFER_SELECTOR, from.into_word(), to.into_word()];
        let data = Bytes::from(eth_value.to_be_bytes_vec());
        Log {
            address: Self::ADDRESS,
            data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
        }
    }

    /// Update account balance
    fn update_balance<CTX>(
        context: &mut CTX,
        account: Address,
        amount: U256,
    ) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        let slot = Self::get_balance_slot(account);
        context
            .journal()
            .sstore(Self::ADDRESS, slot, amount)
            .map_err(db_error)?;

        Ok(())
    }

    /// Get account balance
    fn get_balance<CTX>(context: &mut CTX, account: Address) -> Result<U256, OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        let slot = Self::get_balance_slot(account);
        let balance = context
            .journal()
            .sload(Self::ADDRESS, slot)
            .map_err(db_error)?
            .data;

        Ok(balance)
    }

    /// Inner implementation of mint
    fn mint_inner<CTX>(
        context: &mut CTX,
        to: Address,
        eth_value: U256,
    ) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        let current_balance = Self::get_balance(context, to)?;
        let new_balance = current_balance.saturating_add(eth_value);

        Self::update_balance(context, to, new_balance)?;

        Self::add_total_supply(context, eth_value)?;

        let mint_log = Self::generate_mint_event(to, eth_value);
        context.journal().log(mint_log);

        Ok(())
    }

    /// Inner implementation of transfer
    fn transfer_inner<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: BvmEthContextTrait,
    {
        let from = context.tx().caller();
        let to = match context.tx().kind() {
            TxKind::Call(address) => address,
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
            return Ok(());
        }

        let from_amount = Self::get_balance(context, from)?;
        let to_amount = Self::get_balance(context, to)?;

        if from_amount < eth_value {
            return Err(OpTransactionError::BvmEth(BvmEthError::InsufficientFunds));
        }

        let new_from_amount = from_amount.saturating_sub(eth_value);
        let new_to_amount = to_amount.saturating_add(eth_value);

        Self::update_balance(context, from, new_from_amount)?;
        Self::update_balance(context, to, new_to_amount)?;

        let transfer_log = Self::generate_transfer_event(from, to, eth_value);
        context.journal().log(transfer_log);

        Ok(())
    }
}

#[cfg(test)]
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
        assert_eq!(selector, BvmEth::TRANSFER_SELECTOR);

        let selector = ERC20Events::Mint::SIGNATURE_HASH;
        assert_eq!(selector, BvmEth::MINT_SELECTOR);
    }

    #[test]
    fn bvm_eth_balance_slot_test() {
        let addr = address!("667120e768cf024c2245dd6d9feece4b437c3518");
        let slot = BvmEth::get_balance_slot(addr);
        let expected =
            U256::from_str("0xfe0b4acb70bd1e455f00a22786aa76d07a905b7f77d9cbab254e4dddcbb681c9")
                .unwrap();
        assert_eq!(slot, expected);
    }

    #[test]
    fn bvm_eth_total_supply_slot_test() {
        assert_eq!(
            BvmEth::get_total_supply_slot(),
            U256::from_str("0x0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap()
        );
    }
}
