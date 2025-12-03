//! Contains the `[BvmEth]` type and its implementation.
//!
//! BVM_ETH is an ERC20 token that represents ETH on the Mantle network.
//! Since MNT is the native token of Mantle, ETH needs to be wrapped as an ERC20 token (BVM_ETH) for proper handling.
use crate::api::exec::OpContextTr;
use crate::transaction::{
    error::{db_error, BvmEthError, OpTransactionError},
    OpTxTr,
};
use alloy_sol_types::SolValue;
use revm::{
    context::{JournalTr, Transaction},
    primitives::{
        address, fixed_bytes, keccak256, Address, Bytes, FixedBytes, Log, LogData, TxKind, U256,
    },
};
use std::vec;

/// BVM_ETH ERC20 token implementation.
///
/// BVM_ETH is an ERC20 token that represents ETH on the Mantle network.
/// It allows users to hold and transfer ETH as an ERC20 token, since MNT is the native token of Mantle.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BvmEth;

impl BvmEth {
    /// The contract address for BVM_ETH ERC20 token.
    /// BVM_ETH is an ERC20 token that represents ETH on the Mantle network.
    /// Since MNT is the native token of Mantle, ETH is wrapped as BVM_ETH (ERC20) for proper handling.
    pub const ADDRESS: Address = address!("dEAddEaDdeadDEadDEADDEAddEADDEAddead1111");

    /// keccak("Mint(address,uint256)")
    const MINT_SELECTOR: FixedBytes<32> =
        fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");

    /// keccak("Transfer(address,address,uint256)")
    const TRANSFER_SELECTOR: FixedBytes<32> =
        fixed_bytes!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

    /// Get the storage key for a BVM_ETH balance in the ERC20 contract
    pub fn get_balance_slot(addr: Address) -> U256 {
        keccak256((addr, U256::ZERO).abi_encode()).into()
    }

    /// Get the storage key for the total supply of BVM_ETH ERC20 token
    pub fn get_total_supply_slot() -> U256 {
        U256::from_limbs([2u64, 0, 0, 0])
    }

    /// Mint BVM_ETH ERC20 tokens for a given context and amount.
    /// This is called when ETH is deposited and needs to be represented as BVM_ETH tokens.
    pub fn mint<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: OpContextTr,
    {
        let (_, tx, _, journal, _, _) = context.all_mut();

        journal.load_account(Self::ADDRESS).map_err(db_error)?;

        let from = tx.caller();
        Self::mint_inner(journal, tx, from, eth_value)?;

        journal.touch_account(Self::ADDRESS);
        Ok(())
    }

    /// Transfer BVM_ETH ERC20 tokens for a given context and amount.
    /// This is called when ETH needs to be transferred between accounts as BVM_ETH tokens.
    pub fn transfer<CTX>(context: &mut CTX, eth_value: U256) -> Result<(), OpTransactionError>
    where
        CTX: OpContextTr,
    {
        let (_, tx, _, journal, _, _) = context.all_mut();

        journal.load_account(Self::ADDRESS).map_err(db_error)?;

        Self::transfer_inner(journal, tx, eth_value)?;

        journal.touch_account(Self::ADDRESS);
        Ok(())
    }

    /// Process ETH deposit by minting and transferring BVM_ETH tokens.
    /// This handles the conversion of ETH deposits into BVM_ETH ERC20 tokens.
    pub fn process_eth_deposit<CTX>(
        context: &mut CTX,
        mint_only: bool,
    ) -> Result<(), OpTransactionError>
    where
        CTX: OpContextTr,
    {
        let (_, tx, _, journal, _, _) = context.all_mut();

        journal.load_account(Self::ADDRESS).map_err(db_error)?;

        // Handle mint if eth_value is present in the transaction
        if let Some(eth_value) = tx.eth_value() {
            let from = tx.caller();
            Self::mint_inner(journal, tx, from, U256::from(eth_value))?;
        }

        if !mint_only {
            // Handle transfer if eth_tx_value is present in the transaction
            if let Some(eth_tx_value) = tx.eth_tx_value() {
                Self::transfer_inner(journal, tx, U256::from(eth_tx_value))?;
            }
        }

        journal.touch_account(Self::ADDRESS);
        Ok(())
    }

    /// Add the value of ETH to the total supply of BVM_ETH ERC20 tokens.
    /// This increases the total supply when new BVM_ETH tokens are minted.
    fn add_total_supply<J>(journal: &mut J, eth_value: U256) -> Result<(), OpTransactionError>
    where
        J: JournalTr,
    {
        let total_supply_slot = Self::get_total_supply_slot();
        let value_supply = journal
            .sload(Self::ADDRESS, total_supply_slot)
            .map_err(db_error)?
            .data;

        let new_value_supply = value_supply.saturating_add(eth_value);

        journal
            .sstore(Self::ADDRESS, total_supply_slot, new_value_supply)
            .map_err(db_error)?;

        Ok(())
    }

    /// Generate a mint event for BVM_ETH ERC20 tokens.
    /// This emits an ERC20 Mint event when new BVM_ETH tokens are created.
    fn generate_mint_event(to: Address, eth_value: U256) -> Log {
        let topics = vec![Self::MINT_SELECTOR, to.into_word()];
        let data = Bytes::from(eth_value.to_be_bytes_vec());
        Log {
            address: Self::ADDRESS,
            data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
        }
    }

    /// Generate a transfer event for BVM_ETH ERC20 tokens.
    /// This emits an ERC20 Transfer event when BVM_ETH tokens are transferred between accounts.
    fn generate_transfer_event(from: Address, to: Address, eth_value: U256) -> Log {
        let topics = vec![Self::TRANSFER_SELECTOR, from.into_word(), to.into_word()];
        let data = Bytes::from(eth_value.to_be_bytes_vec());
        Log {
            address: Self::ADDRESS,
            data: LogData::new(topics, data).expect("LogData should have <=4 topics"),
        }
    }

    /// Update account balance for BVM_ETH ERC20 tokens.
    /// This updates the balance of a specific account in the BVM_ETH contract.
    fn update_balance<J>(
        journal: &mut J,
        account: Address,
        amount: U256,
    ) -> Result<(), OpTransactionError>
    where
        J: JournalTr,
    {
        let slot = Self::get_balance_slot(account);
        journal
            .sstore(Self::ADDRESS, slot, amount)
            .map_err(db_error)?;

        Ok(())
    }

    /// Get account balance for BVM_ETH ERC20 tokens.
    /// This retrieves the balance of a specific account from the BVM_ETH contract.
    fn get_balance<J>(journal: &mut J, account: Address) -> Result<U256, OpTransactionError>
    where
        J: JournalTr,
    {
        let slot = Self::get_balance_slot(account);
        let balance = journal.sload(Self::ADDRESS, slot).map_err(db_error)?.data;

        Ok(balance)
    }

    /// Inner implementation of mint for BVM_ETH ERC20 tokens.
    /// This handles the actual minting logic including balance updates, total supply increase, and event emission.
    fn mint_inner<J, T>(
        journal: &mut J,
        _tx: &T,
        to: Address,
        eth_value: U256,
    ) -> Result<(), OpTransactionError>
    where
        J: JournalTr,
        T: Transaction,
    {
        let current_balance = Self::get_balance(journal, to)?;
        let new_balance = current_balance.saturating_add(eth_value);

        Self::update_balance(journal, to, new_balance)?;

        Self::add_total_supply(journal, eth_value)?;

        let mint_log = Self::generate_mint_event(to, eth_value);
        journal.log(mint_log);

        Ok(())
    }

    /// Inner implementation of transfer for BVM_ETH ERC20 tokens.
    /// This handles the actual transfer logic including balance updates and event emission.
    fn transfer_inner<J, T>(
        journal: &mut J,
        tx: &T,
        eth_value: U256,
    ) -> Result<(), OpTransactionError>
    where
        J: JournalTr,
        T: Transaction,
    {
        let from = tx.caller();
        let to = match tx.kind() {
            TxKind::Call(address) => address,
            TxKind::Create => {
                let nonce = journal
                    .load_account(from)
                    .map_err(db_error)?
                    .data
                    .info
                    .nonce;
                from.create(nonce)
            }
        };

        if from == to {
            return Ok(());
        }

        let from_amount = Self::get_balance(journal, from)?;
        let to_amount = Self::get_balance(journal, to)?;

        if from_amount < eth_value {
            return Err(OpTransactionError::BvmEth(BvmEthError::InsufficientFunds));
        }

        let new_from_amount = from_amount.saturating_sub(eth_value);
        let new_to_amount = to_amount.saturating_add(eth_value);

        Self::update_balance(journal, from, new_from_amount)?;
        Self::update_balance(journal, to, new_to_amount)?;

        let transfer_log = Self::generate_transfer_event(from, to, eth_value);
        journal.log(transfer_log);

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
