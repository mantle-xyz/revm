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
    pub(crate) const MINT_SELECTOR: FixedBytes<32> =
        fixed_bytes!("0f6798a560793a54c3bcfe86a93cde1e73087d944c0ea20544137d4121396885");

    /// keccak("Transfer(address,address,uint256)")
    pub(crate) const TRANSFER_SELECTOR: FixedBytes<32> =
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

        // Align with op-geth's st.state.SetState() semantics — pre-EVM state
        // mutations must not warm the access list. See doc on
        // JournalTr::mark_account_and_slots_cold.
        journal.mark_account_and_slots_cold(
            Self::ADDRESS,
            &[Self::get_total_supply_slot(), Self::get_balance_slot(from)],
        );
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

        // Align with op-geth's st.state.SetState() semantics — pre-EVM state
        // mutations must not warm the access list.
        let from = tx.caller();
        let to = match tx.kind() {
            TxKind::Call(addr) => addr,
            TxKind::Create => from.create(
                journal
                    .load_account(from)
                    .map_err(db_error)?
                    .data
                    .info
                    .nonce,
            ),
        };
        let slots: &[U256] = if from == to {
            // transfer_inner early-returned, no balance slots touched
            &[]
        } else {
            &[Self::get_balance_slot(from), Self::get_balance_slot(to)]
        };
        journal.mark_account_and_slots_cold(Self::ADDRESS, slots);
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

        let eth_value = tx.eth_value();
        let eth_tx_value = tx.eth_tx_value();

        // Only load and touch BVM_ETH account when there's actual work to do.
        // This avoids warming the contract address unnecessarily.
        let needs_mint = eth_value.is_some();
        let needs_transfer = !mint_only && eth_tx_value.is_some();

        if !needs_mint && !needs_transfer {
            return Ok(());
        }

        journal.load_account(Self::ADDRESS).map_err(db_error)?;

        // Handle mint if eth_value is present in the transaction
        if let Some(eth_value) = eth_value {
            let from = tx.caller();
            Self::mint_inner(journal, tx, from, U256::from(eth_value))?;
        }

        // Handle transfer if eth_tx_value is present in the transaction
        if let Some(eth_tx_value) = eth_tx_value {
            if !mint_only {
                Self::transfer_inner(journal, tx, U256::from(eth_tx_value))?;
            }
        }

        journal.touch_account(Self::ADDRESS);

        // Align with op-geth's st.state.SetState() semantics — pre-EVM state
        // mutations must not warm the access list. Without this, EVM
        // under-charges access cost for any subsequent access to BVM_ETH
        // (matching op-geth would require ~4500 of "cold" cost). The prior
        // implementation applied a flat 4500-gas compensation in the handler,
        // which over-charged when EVM never touched BVM_ETH (e.g. EOA target).
        //
        // TODO(v38 migration): revm v38's `EvmStorageSlot::mark_warm_with_transaction_id`
        // resets `original_value = present_value` on cold→warm. This cooling
        // approach must be re-designed when migrating — replace with inspector-
        // based access tracking (Path C) or patch `mark_warm` to preserve
        // `original_value`. See doc on `JournalTr::mark_account_and_slots_cold`.
        let from = tx.caller();
        let mut touched_slots: Vec<U256> = Vec::with_capacity(3);
        if needs_mint {
            touched_slots.push(Self::get_total_supply_slot());
            touched_slots.push(Self::get_balance_slot(from));
        }
        if needs_transfer {
            let to = match tx.kind() {
                TxKind::Call(addr) => addr,
                TxKind::Create => from.create(
                    journal
                        .load_account(from)
                        .map_err(db_error)?
                        .data
                        .info
                        .nonce,
                ),
            };
            if to != from {
                if !needs_mint {
                    // mint already added balance[from]; transfer touches it too
                    touched_slots.push(Self::get_balance_slot(from));
                }
                touched_slots.push(Self::get_balance_slot(to));
            }
        }
        journal.mark_account_and_slots_cold(Self::ADDRESS, &touched_slots);

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
    use crate::{
        api::default_ctx::DefaultOp, handler::OpHandler,
        transaction::deposit::DepositTransactionParts, L1BlockInfo, OpBuilder, OpSpecId,
        OpTransaction,
    };
    use alloy_sol_types::{sol, SolEvent};
    use revm::{
        context::{BlockEnv, Context, TxEnv},
        context_interface::result::{EVMError, ExecutionResult},
        database::{Cache, InMemoryDB},
        handler::{EthFrame, EvmTr, Handler},
        interpreter::interpreter::EthInterpreter,
        primitives::{hex, Address, Bytes, B256, U256},
        state::{AccountInfo, AccountStatus, Bytecode},
    };
    use rstest::rstest;
    use serde::{Deserialize, Serialize};
    use std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
        str::FromStr,
    };
    use tempfile::tempdir;

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

    #[test]
    fn test_process_eth_deposit_no_eth_value_no_eth_tx_value() {
        // When both eth_value and eth_tx_value are None, should return early
        // and not load BVM_ETH account
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = None;
                tx.deposit.eth_tx_value = None;
            });

        let result = BvmEth::process_eth_deposit(&mut ctx, false);
        assert!(result.is_ok());

        // BVM_ETH account should NOT be loaded
        assert!(
            !ctx.journaled_state
                .inner
                .state
                .contains_key(&BvmEth::ADDRESS),
            "BVM_ETH should not be loaded when eth_value and eth_tx_value are both None"
        );

        // No logs should be emitted
        assert!(ctx.journaled_state.inner.logs.is_empty());
    }

    #[test]
    fn test_process_eth_deposit_only_eth_value() {
        // When only eth_value is present, should mint BVM_ETH
        let eth_value = 1_000_000_000_000_000_000u128; // 1 ETH
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = Some(eth_value);
                tx.deposit.eth_tx_value = None;
            });

        let result = BvmEth::process_eth_deposit(&mut ctx, false);
        assert!(result.is_ok());

        // BVM_ETH account should be loaded
        assert!(
            ctx.journaled_state
                .inner
                .state
                .contains_key(&BvmEth::ADDRESS),
            "BVM_ETH should be loaded when eth_value is present"
        );

        // Should have Mint event
        let logs = &ctx.journaled_state.inner.logs;
        assert_eq!(logs.len(), 1, "Should have exactly 1 log (Mint event)");
        assert_eq!(logs[0].address, BvmEth::ADDRESS);
        assert_eq!(logs[0].topics()[0], BvmEth::MINT_SELECTOR);
    }

    #[test]
    fn test_process_eth_deposit_only_eth_tx_value() {
        // When only eth_tx_value is present, should transfer BVM_ETH
        let eth_tx_value = 500_000_000_000_000_000u128; // 0.5 ETH
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = None;
                tx.deposit.eth_tx_value = Some(eth_tx_value);
            });

        // First, give the caller some BVM_ETH balance for transfer
        use revm::context_interface::JournalTr;
        let balance_slot = BvmEth::get_balance_slot(caller);
        ctx.journaled_state
            .load_account(BvmEth::ADDRESS)
            .expect("load account");
        ctx.journaled_state
            .sstore(BvmEth::ADDRESS, balance_slot, U256::from(eth_tx_value))
            .expect("sstore");
        // Clear logs from setup
        ctx.journaled_state.inner.logs.clear();

        let result = BvmEth::process_eth_deposit(&mut ctx, false);
        assert!(result.is_ok());

        // BVM_ETH account should be loaded
        assert!(
            ctx.journaled_state
                .inner
                .state
                .contains_key(&BvmEth::ADDRESS),
            "BVM_ETH should be loaded when eth_tx_value is present"
        );

        // Should have Transfer event
        let logs = &ctx.journaled_state.inner.logs;
        assert_eq!(logs.len(), 1, "Should have exactly 1 log (Transfer event)");
        assert_eq!(logs[0].address, BvmEth::ADDRESS);
        assert_eq!(logs[0].topics()[0], BvmEth::TRANSFER_SELECTOR);
    }

    #[test]
    fn test_process_eth_deposit_both_values() {
        // When both eth_value and eth_tx_value are present, should mint and transfer
        let eth_value = 1_000_000_000_000_000_000u128; // 1 ETH
        let eth_tx_value = 500_000_000_000_000_000u128; // 0.5 ETH
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = Some(eth_value);
                tx.deposit.eth_tx_value = Some(eth_tx_value);
            });

        let result = BvmEth::process_eth_deposit(&mut ctx, false);
        assert!(result.is_ok());

        // BVM_ETH account should be loaded
        assert!(
            ctx.journaled_state
                .inner
                .state
                .contains_key(&BvmEth::ADDRESS),
            "BVM_ETH should be loaded when eth_value or eth_tx_value is present"
        );

        // Should have Mint and Transfer events
        let logs = &ctx.journaled_state.inner.logs;
        assert_eq!(
            logs.len(),
            2,
            "Should have 2 logs (Mint and Transfer events)"
        );

        // First log should be Mint
        assert_eq!(logs[0].address, BvmEth::ADDRESS);
        assert_eq!(logs[0].topics()[0], BvmEth::MINT_SELECTOR);

        // Second log should be Transfer
        assert_eq!(logs[1].address, BvmEth::ADDRESS);
        assert_eq!(logs[1].topics()[0], BvmEth::TRANSFER_SELECTOR);
    }

    #[test]
    fn test_process_eth_deposit_cools_bvm_eth_account_and_slots() {
        // After process_eth_deposit, BVM_ETH account and touched storage slots
        // must be cold in EVM's access list — matching op-geth's
        // st.state.SetState() semantics. Without this, EVM under-charges
        // access to BVM_ETH by ~4500 gas vs op-geth.
        let eth_value = 1_000_000_000_000_000_000u128;
        let caller = address!("1234567890123456789012345678901234567890");
        let recipient = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(recipient);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = Some(eth_value);
                tx.deposit.eth_tx_value = Some(eth_value);
            });

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH must be loaded");

        assert!(
            bvm.status.contains(AccountStatus::Cold),
            "BVM_ETH account must be cold after process_eth_deposit"
        );

        let supply_slot = BvmEth::get_total_supply_slot();
        assert!(
            bvm.storage
                .get(&supply_slot)
                .expect("total_supply slot present")
                .is_cold,
            "total_supply slot must be cold"
        );

        let caller_balance_slot = BvmEth::get_balance_slot(caller);
        assert!(
            bvm.storage
                .get(&caller_balance_slot)
                .expect("caller balance slot present")
                .is_cold,
            "caller balance slot must be cold"
        );

        let recipient_balance_slot = BvmEth::get_balance_slot(recipient);
        assert!(
            bvm.storage
                .get(&recipient_balance_slot)
                .expect("recipient balance slot present")
                .is_cold,
            "recipient balance slot must be cold"
        );
    }

    #[test]
    fn test_process_eth_deposit_eoa_target_does_not_overcharge_gas() {
        // Regression test for the Mantle Hoodi QA2 fork tx
        // 0x91bf3872a3582794a7df6120d18e3bac66491b96b14c3deca05fd40011860022:
        // a deposit with eth_value + eth_tx_value + non-empty input, but
        // targeting an EOA (sender == to). EVM never accesses BVM_ETH because
        // there is no code at the target. op-geth reports gas_used = 21320
        // (intrinsic-only). Prior to this fix, REVM applied a flat 4500 gas
        // "BVM_ETH compensation" and reported 25820 — a consensus-level fork.
        let eth_value = 0x38d7ea4c68000u128;
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(caller);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef01020304").unwrap());
                tx.deposit.source_hash = B256::from([1u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(eth_value);
                tx.deposit.eth_tx_value = Some(eth_value);
            });

        // Give the caller enough native balance for any fee-style accounting.
        ctx.journaled_state
            .database
            .insert_account_info(
                caller,
                revm::state::AccountInfo {
                    balance: U256::from(u128::MAX),
                    nonce: 0x56,
                    ..Default::default()
                },
            );

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");

        let gas_used = result.gas_used();
        // 21000 base + 8 non-zero calldata bytes * 16 = 21128; deposit type adds
        // a small per-tx overhead. The key invariant: gas_used must NOT include
        // a flat 4500 compensation. Allow some tolerance for deposit overhead
        // by asserting it's well below the previously-broken value (~25820)
        // and within an intrinsic-only window.
        assert!(
            gas_used < 25000,
            "EOA-target deposit must not include 4500 BVM_ETH compensation; got gas_used={}",
            gas_used
        );
    }

    // -----------------------------------------------------------------------
    // Cooling state assertions across the full process_eth_deposit scenario
    // matrix. Each test verifies that BVM_ETH and the slots actually touched
    // by the operation are cold afterwards (matching op-geth's
    // st.state.SetState() semantics).
    //
    // Scenario matrix (mint_only=false unless stated):
    //   A: no eth_value, no eth_tx_value             -> BVM_ETH not loaded
    //   B: eth_value, no eth_tx_value                -> mint only
    //   C: no eth_value, eth_tx_value, from==to      -> account loaded, no slots
    //   D: no eth_value, eth_tx_value, from!=to      -> transfer slots
    //   E: eth_value + eth_tx_value, from==to        -> mint slots only
    //   F: eth_value + eth_tx_value, from!=to        -> mint + transfer slots
    //   I: eth_value + eth_tx_value, mint_only=true  -> mint slots only
    //
    // Scenario F is covered by test_process_eth_deposit_cools_bvm_eth_account_and_slots.
    // -----------------------------------------------------------------------

    fn assert_bvm_cooled(state: &revm::state::EvmState, expected_cold_slots: &[U256]) {
        let bvm = state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH must be loaded");
        assert!(
            bvm.status.contains(AccountStatus::Cold),
            "BVM_ETH account must be cold"
        );
        for slot in expected_cold_slots {
            let s = bvm
                .storage
                .get(slot)
                .unwrap_or_else(|| panic!("slot {:?} must be in storage", slot));
            assert!(s.is_cold, "slot {:?} must be cold", slot);
        }
    }

    fn make_deposit_ctx(
        caller: Address,
        to_kind: revm::primitives::TxKind,
        eth_value: Option<u128>,
        eth_tx_value: Option<u128>,
    ) -> crate::api::default_ctx::OpContext<InMemoryDB> {
        Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = to_kind;
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = eth_value;
                tx.deposit.eth_tx_value = eth_tx_value;
            })
    }

    #[test]
    fn test_cooling_scenario_a_no_values_does_not_load_bvm_eth() {
        // Scenario A: no eth_value, no eth_tx_value -> early return, BVM_ETH not touched.
        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");
        let mut ctx = make_deposit_ctx(caller, revm::primitives::TxKind::Call(to), None, None);

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert!(
            !ctx.journaled_state.inner.state.contains_key(&BvmEth::ADDRESS),
            "BVM_ETH must not be loaded when there is nothing to mint/transfer"
        );
    }

    #[test]
    fn test_cooling_scenario_b_mint_only_path() {
        // Scenario B: eth_value only -> mint cools total_supply + balance[caller].
        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            None,
        );

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
            ],
        );
    }

    #[test]
    fn test_cooling_scenario_c_transfer_to_self_only_account_cooled() {
        // Scenario C: eth_tx_value only with from==to -> transfer_inner early-returns,
        // no balance slots touched. BVM_ETH account itself was warmed by load_account
        // and must be cooled.
        let caller = address!("1111111111111111111111111111111111111111");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(caller),
            None,
            Some(500_000_000_000_000_000u128),
        );

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH must be loaded");
        assert!(
            bvm.status.contains(AccountStatus::Cold),
            "BVM_ETH account must be cold"
        );
        // No balance slots should have been stored to (transfer_inner early-returned).
        assert!(
            bvm.storage.is_empty(),
            "no storage slots should be touched for self-transfer; got {:?}",
            bvm.storage.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_cooling_scenario_d_transfer_distinct_cools_both_balance_slots() {
        // Scenario D: eth_tx_value only with from!=to -> balance[from], balance[to] cooled.
        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");

        // transfer requires from to have BVM_ETH balance. Pre-seed via direct sstore.
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            None,
            Some(500_000_000_000_000_000u128),
        );
        {
            use revm::context_interface::JournalTr;
            ctx.journaled_state
                .load_account(BvmEth::ADDRESS)
                .expect("load BVM_ETH");
            ctx.journaled_state
                .sstore(
                    BvmEth::ADDRESS,
                    BvmEth::get_balance_slot(caller),
                    U256::from(1_000_000_000_000_000_000u128),
                )
                .expect("seed caller balance");
        }

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_balance_slot(caller),
                BvmEth::get_balance_slot(to),
            ],
        );
    }

    #[test]
    fn test_cooling_scenario_e_both_values_to_self_cools_mint_slots() {
        // Scenario E: eth_value+eth_tx_value, from==to -> mint runs, transfer
        // early-returns. Only mint slots end up touched and must be cold.
        let caller = address!("1111111111111111111111111111111111111111");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(caller),
            Some(1_000_000_000_000_000_000u128),
            Some(500_000_000_000_000_000u128),
        );

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
            ],
        );
    }

    #[test]
    fn test_cooling_scenario_i_mint_only_flag_cools_only_mint_slots() {
        // Scenario I: mint_only=true with both values -> transfer is skipped.
        // Only mint slots cooled; balance[to] not in storage.
        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            Some(500_000_000_000_000_000u128),
        );

        BvmEth::process_eth_deposit(&mut ctx, true).expect("process_eth_deposit mint_only");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
            ],
        );
        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .unwrap();
        assert!(
            !bvm.storage.contains_key(&BvmEth::get_balance_slot(to)),
            "balance[to] must not be touched when mint_only=true"
        );
    }

    // -----------------------------------------------------------------------
    // Public mint() / transfer() API cooling.
    // -----------------------------------------------------------------------

    #[test]
    fn test_mint_public_api_cools_bvm_eth() {
        let caller = address!("3333333333333333333333333333333333333333");
        let to = address!("4444444444444444444444444444444444444444");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            None,
            None,
        );

        BvmEth::mint(&mut ctx, U256::from(7_000_000_000_000_000_000u128)).expect("mint");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
            ],
        );
    }

    #[test]
    fn test_transfer_public_api_cools_distinct() {
        let caller = address!("3333333333333333333333333333333333333333");
        let to = address!("4444444444444444444444444444444444444444");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            None,
            None,
        );
        {
            use revm::context_interface::JournalTr;
            ctx.journaled_state
                .load_account(BvmEth::ADDRESS)
                .expect("load BVM_ETH");
            ctx.journaled_state
                .sstore(
                    BvmEth::ADDRESS,
                    BvmEth::get_balance_slot(caller),
                    U256::from(2_000_000_000_000_000_000u128),
                )
                .expect("seed caller balance");
        }

        BvmEth::transfer(&mut ctx, U256::from(1_000_000_000_000_000_000u128)).expect("transfer");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_balance_slot(caller),
                BvmEth::get_balance_slot(to),
            ],
        );
    }

    #[test]
    fn test_transfer_public_api_cools_self_transfer_account_only() {
        let caller = address!("3333333333333333333333333333333333333333");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(caller),
            None,
            None,
        );

        BvmEth::transfer(&mut ctx, U256::from(1_000_000_000_000_000_000u128)).expect("transfer");

        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH loaded");
        assert!(
            bvm.status.contains(AccountStatus::Cold),
            "BVM_ETH account must be cold even on self-transfer"
        );
        assert!(
            bvm.storage.is_empty(),
            "no balance slots should be touched for self-transfer"
        );
    }

    // -----------------------------------------------------------------------
    // EIP-2930 access-list carve-out: entries pre-warmed by the user's access
    // list must NOT be cooled by our op-geth-alignment logic.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cooling_preserves_eip2930_warm_account() {
        let caller = address!("5555555555555555555555555555555555555555");
        let to = address!("6666666666666666666666666666666666666666");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            None,
        );

        // Pre-warm BVM_ETH via the EIP-2930 access list.
        {
            use revm::context_interface::JournalTr;
            let mut access_list: revm::primitives::HashMap<
                Address,
                revm::primitives::HashSet<U256>,
            > = revm::primitives::HashMap::default();
            access_list.insert(BvmEth::ADDRESS, revm::primitives::HashSet::default());
            ctx.journaled_state.warm_access_list(access_list);
        }

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH loaded");
        assert!(
            !bvm.status.contains(AccountStatus::Cold),
            "BVM_ETH must remain warm because user put it in the EIP-2930 access list"
        );
    }

    #[test]
    fn test_cooling_preserves_eip2930_warm_slot() {
        let caller = address!("5555555555555555555555555555555555555555");
        let to = address!("6666666666666666666666666666666666666666");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            None,
        );

        let supply_slot = BvmEth::get_total_supply_slot();
        let balance_slot = BvmEth::get_balance_slot(caller);

        // Pre-warm only the total_supply slot via EIP-2930.
        {
            use revm::context_interface::JournalTr;
            let mut slots: revm::primitives::HashSet<U256> =
                revm::primitives::HashSet::default();
            slots.insert(supply_slot);
            let mut access_list: revm::primitives::HashMap<
                Address,
                revm::primitives::HashSet<U256>,
            > = revm::primitives::HashMap::default();
            access_list.insert(BvmEth::ADDRESS, slots);
            ctx.journaled_state.warm_access_list(access_list);
        }

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        let bvm = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH loaded");
        assert!(
            !bvm
                .storage
                .get(&supply_slot)
                .expect("supply slot stored")
                .is_cold,
            "total_supply must stay warm — user pre-warmed it via EIP-2930"
        );
        assert!(
            bvm.storage
                .get(&balance_slot)
                .expect("balance slot stored")
                .is_cold,
            "balance[caller] must be cold — not in user's access list"
        );
    }

    // -----------------------------------------------------------------------
    // End-to-end gas regression: contract target that DOES NOT touch BVM_ETH.
    //
    // This is the case that A1 (target-has-code heuristic) would have failed —
    // a deployed contract whose execution path never accesses BVM_ETH still
    // got the flat 4500 over-charge. With cooling + 4500 removed, the access
    // cost is determined by actual EVM behavior, so no over-charge occurs.
    // -----------------------------------------------------------------------

    #[test]
    fn test_deposit_gas_contract_target_without_bvm_eth_access_not_overcharged() {
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");
        let target = address!("000000000000000000000000000000000000c0de");

        // Target contract: a single STOP opcode. EVM enters, halts, returns
        // success. No BVM_ETH access.
        let stop_code = Bytecode::new_raw(Bytes::from(vec![0x00]));
        let code_hash = stop_code.hash_slow();
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            target,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(stop_code),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0x10,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(target);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef01020304").unwrap());
                tx.deposit.source_hash = B256::from([2u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(0x38d7ea4c68000u128);
                tx.deposit.eth_tx_value = Some(0x38d7ea4c68000u128);
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");
        let gas_used = result.gas_used();

        // Contract target with STOP body: ~ intrinsic + calldata cost +
        // negligible EVM exec (STOP = 0). With the old broken 4500
        // compensation: ~25620+. With the fix: < 22000.
        assert!(
            gas_used < 22000,
            "contract-target deposit (target does not touch BVM_ETH) must not include 4500 BVM_ETH compensation; got gas_used={}",
            gas_used
        );
    }

    // -----------------------------------------------------------------------
    // Direct BVM_ETH call synthetic tests.
    //
    // These complement the bridge fixture (89718944) by exercising direct
    // tx.to = BVM_ETH paths with controlled bytecode, so that cooling
    // regressions on BVM_ETH-direct calls are caught with a precise unit-
    // level assertion (not just a fixture gas mismatch).
    // -----------------------------------------------------------------------

    #[test]
    fn test_deposit_gas_direct_bvm_eth_call_stop_body() {
        // tx.to = BVM_ETH, BVM_ETH stub bytecode = STOP. EVM enters BVM_ETH
        // (auto-warm via EIP-2929 tx.to pre-warm), halts immediately, no
        // storage access. Verifies cooling does not break the EIP-2929
        // pre-warm of tx.to and that no 4500 over-charge is applied.
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");

        let stop_code = Bytecode::new_raw(Bytes::from(vec![0x00]));
        let code_hash = stop_code.hash_slow();
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            BvmEth::ADDRESS,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(stop_code),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(BvmEth::ADDRESS);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef").unwrap());
                tx.deposit.source_hash = B256::from([3u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(0x38d7ea4c68000u128);
                tx.deposit.eth_tx_value = None;
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");
        let gas_used = result.gas_used();

        // intrinsic(21000) + 4 calldata bytes nonzero (64) + STOP (0) ≈ 21064
        // plus small deposit overhead. With old 4500: ~25564+. With fix: < 22000.
        assert!(
            gas_used < 22000,
            "direct BVM_ETH call (STOP body) must not include 4500 compensation; got gas_used={}",
            gas_used
        );
    }

    #[test]
    fn test_deposit_gas_direct_bvm_eth_call_sload_pays_cold_cost() {
        // tx.to = BVM_ETH, BVM_ETH stub bytecode does SLOAD on slot 2
        // (total_supply, which was warmed then cooled by pre-EVM mint).
        // Verifies that cooling actually delivers cold storage access cost
        // when EVM accesses BVM_ETH storage — the core property the fix
        // depends on for op-geth alignment.
        //
        // Bytecode: PUSH1 0x02; SLOAD; POP; STOP
        //   = 60 02 54 50 00
        //   gas: 3 (PUSH1) + 2100 (cold SLOAD) + 2 (POP) + 0 (STOP) = 2105
        //
        // Without cooling, SLOAD would be warm (100 gas) — total EVM exec
        // ~105 instead of ~2105, a 2000-gas under-charge vs op-geth.
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");

        let sload_code = Bytecode::new_raw(Bytes::from(vec![0x60, 0x02, 0x54, 0x50, 0x00]));
        let code_hash = sload_code.hash_slow();
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            BvmEth::ADDRESS,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(sload_code),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(BvmEth::ADDRESS);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef").unwrap());
                tx.deposit.source_hash = B256::from([4u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(0x38d7ea4c68000u128);
                tx.deposit.eth_tx_value = None;
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");
        let gas_used = result.gas_used();

        // intrinsic(21000) + 4 nonzero calldata bytes (64) + EVM exec(~2105)
        // = ~23169. The lower bound > 22500 forces cold SLOAD to have been
        // paid; warm SLOAD would yield ~21169.
        assert!(
            gas_used > 22500,
            "direct BVM_ETH SLOAD must pay cold storage cost (~2100 gas) — slot must have been cooled before EVM; got gas_used={}",
            gas_used
        );
        // Upper bound rules out a stale 4500 compensation on top of cold cost.
        assert!(
            gas_used < 25000,
            "direct BVM_ETH SLOAD must not double-charge with 4500 compensation; got gas_used={}",
            gas_used
        );
    }

    // -----------------------------------------------------------------------
    // Revert-path tests. Three interactions to verify:
    //   * EVM REVERT inside a deposit: pre-EVM mint persists, gas_used not
    //     over-charged.
    //   * catch_error full revert + re-mint replay: cooling re-applied
    //     correctly so the next access pays cold cost.
    //   * EVM warming a cooled slot then reverting: post-revert the slot is
    //     cold again (via JournalEntry::StorageWarmed::revert).
    // -----------------------------------------------------------------------

    #[test]
    fn test_deposit_gas_aligned_when_target_reverts() {
        // R1: target contract whose bytecode is PUSH1 0; PUSH1 0; REVERT.
        // EVM enters, immediately reverts. Verifies gas_used is NOT
        // over-charged by a stale 4500 compensation (which the old
        // last_frame_result applied even on revert paths).
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");
        let target = address!("000000000000000000000000000000000000fade");

        let revert_code = Bytecode::new_raw(Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xfd]));
        let code_hash = revert_code.hash_slow();
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            target,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(revert_code),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(target);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef").unwrap());
                tx.deposit.source_hash = B256::from([5u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(0x38d7ea4c68000u128);
                tx.deposit.eth_tx_value = None;
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");

        // Must actually have reverted (otherwise we're testing the wrong path).
        assert!(
            matches!(result, ExecutionResult::Revert { .. }),
            "expected ExecutionResult::Revert; got {:?}",
            result
        );

        let gas_used = result.gas_used();
        assert!(
            gas_used < 22000,
            "deposit-with-revert must not include 4500 compensation; got gas_used={}",
            gas_used
        );
    }

    #[test]
    fn test_deposit_pre_evm_mint_persists_when_target_reverts() {
        // R2: same shape as R1 — top-level EVM REVERT — but verify the
        // pre-EVM BVM_ETH mint persists in the journal state after the
        // tx is fully processed. Per the OP deposit spec, mint is always
        // committed; the cooling fix must not interfere with that.
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");
        let target = address!("000000000000000000000000000000000000fade");
        let mint_amount = 0x38d7ea4c68000u128; // 1e15 wei

        let revert_code = Bytecode::new_raw(Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xfd]));
        let code_hash = revert_code.hash_slow();
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            target,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(revert_code),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(target);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::from(hex::decode("deadbeef").unwrap());
                tx.deposit.source_hash = B256::from([6u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(mint_amount);
                tx.deposit.eth_tx_value = None;
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let _ = handler.run(&mut evm).expect("handler.run");

        // After handler.run, journal state should reflect the pre-EVM mint.
        let balance_slot = BvmEth::get_balance_slot(caller);
        let supply_slot = BvmEth::get_total_supply_slot();
        let ctx_after = evm.ctx_mut();
        use revm::context_interface::JournalTr;
        let balance = ctx_after
            .journaled_state
            .sload(BvmEth::ADDRESS, balance_slot)
            .expect("sload balance")
            .data;
        let supply = ctx_after
            .journaled_state
            .sload(BvmEth::ADDRESS, supply_slot)
            .expect("sload supply")
            .data;
        assert_eq!(
            balance,
            U256::from(mint_amount),
            "BVM_ETH balance[caller] must reflect the pre-EVM mint despite EVM REVERT"
        );
        assert_eq!(
            supply,
            U256::from(mint_amount),
            "BVM_ETH total_supply must reflect the pre-EVM mint despite EVM REVERT"
        );
    }

    #[test]
    fn test_cooling_idempotent_across_catch_error_replay() {
        // R3: simulate the catch_error path. After a full revert
        // (checkpoint_revert(JournalCheckpoint::default())) and a re-mint
        // via process_eth_deposit(mint_only=true), BVM_ETH and the touched
        // slots must end up cold again, exactly as on the happy path.
        use revm::context::journaled_state::JournalCheckpoint;
        use revm::context_interface::JournalTr;

        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            Some(500_000_000_000_000_000u128),
        );

        // First call — mint + transfer, then cool.
        BvmEth::process_eth_deposit(&mut ctx, false).expect("first process_eth_deposit");
        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
                BvmEth::get_balance_slot(to),
            ],
        );

        // Simulate catch_error step 1: full revert back to tx start.
        ctx.journaled_state
            .checkpoint_revert(JournalCheckpoint::default());

        // After full revert, BVM_ETH account is in state but marked cold
        // (via the revert of AccountWarmed). Storage slots present-values
        // are restored to pre-tx (zero for fresh accounts).
        let bvm_after_revert = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .expect("BVM_ETH still in state after revert");
        assert!(
            bvm_after_revert.status.contains(AccountStatus::Cold),
            "BVM_ETH must be cold after full journal revert"
        );

        // Simulate catch_error step 2: re-mint via mint_only path.
        BvmEth::process_eth_deposit(&mut ctx, true).expect("re-mint after revert");

        // Verify cooling is re-applied for the mint slots; transfer slot
        // is NOT touched because mint_only skips transfer.
        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
            ],
        );

        // And the pre-mint values must be present at the journal level.
        let balance = ctx
            .journaled_state
            .sload(BvmEth::ADDRESS, BvmEth::get_balance_slot(caller))
            .expect("sload balance after re-mint")
            .data;
        assert_eq!(
            balance,
            U256::from(1_000_000_000_000_000_000u128),
            "balance[caller] must reflect the re-mint amount"
        );
    }

    #[test]
    fn test_cooling_restored_after_evm_warms_then_reverts_slot() {
        // R4: cooling state must survive the standard journal warm→revert
        // cycle. After process_eth_deposit (cool applied), simulate an EVM
        // frame that:
        //   * checkpoint A
        //   * sload BVM_ETH slot (which is cold from our cool) → warm-up
        //   * checkpoint_revert(A) → undoes the warm-up
        // After revert, the slot must be cold again (via
        // JournalEntry::StorageWarmed::revert calling slot.mark_cold).
        use revm::context_interface::JournalTr;

        let caller = address!("1111111111111111111111111111111111111111");
        let to = address!("2222222222222222222222222222222222222222");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Call(to),
            Some(1_000_000_000_000_000_000u128),
            None,
        );

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        // Sanity: post-cool the slot is cold.
        let supply_slot = BvmEth::get_total_supply_slot();
        {
            let slot = ctx
                .journaled_state
                .inner
                .state
                .get(&BvmEth::ADDRESS)
                .unwrap()
                .storage
                .get(&supply_slot)
                .unwrap();
            assert!(slot.is_cold, "supply slot must be cold after process_eth_deposit");
        }

        // Simulate an inner EVM frame: take a checkpoint, sload (which
        // marks the slot warm), then revert.
        let checkpoint = ctx.journaled_state.checkpoint();
        let _ = ctx
            .journaled_state
            .sload(BvmEth::ADDRESS, supply_slot)
            .expect("inner-frame sload");
        // After sload, slot should be warm.
        {
            let slot = ctx
                .journaled_state
                .inner
                .state
                .get(&BvmEth::ADDRESS)
                .unwrap()
                .storage
                .get(&supply_slot)
                .unwrap();
            assert!(!slot.is_cold, "supply slot must be warm after inner sload");
        }

        // Frame reverts.
        ctx.journaled_state.checkpoint_revert(checkpoint);

        // After revert, the slot must be cold again — JournalEntry::
        // StorageWarmed::revert calls slot.mark_cold.
        let slot = ctx
            .journaled_state
            .inner
            .state
            .get(&BvmEth::ADDRESS)
            .unwrap()
            .storage
            .get(&supply_slot)
            .unwrap();
        assert!(
            slot.is_cold,
            "supply slot must be cold after revert undoes the inner-frame warming"
        );
    }

    // -----------------------------------------------------------------------
    // Edge cases:
    //   * CREATE deposits (TxKind::Create): cooling code derives `to` from
    //     caller.create(nonce). Must hit the CREATE branch.
    //   * Multi-frame: outer contract that CALLs BVM_ETH stub. Verifies
    //     that EIP-2929 cold cost is charged for the first CALL into
    //     BVM_ETH from EVM, even when reached via a nested frame.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cooling_create_deposit_with_transfer_cools_create_address_slot() {
        // E1: CREATE deposit with eth_tx_value only. Cooling code must
        // derive the transfer destination as caller.create(nonce) and cool
        // both balance[caller] and balance[create_addr].
        use revm::context_interface::JournalTr;

        let caller = address!("1111111111111111111111111111111111111111");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Create,
            None,
            Some(500_000_000_000_000_000u128),
        );

        // Pre-seed caller's BVM_ETH balance for transfer (and don't bump
        // caller nonce yet — the cooling code reads nonce as-is).
        ctx.journaled_state
            .load_account(BvmEth::ADDRESS)
            .expect("load BVM_ETH");
        ctx.journaled_state
            .sstore(
                BvmEth::ADDRESS,
                BvmEth::get_balance_slot(caller),
                U256::from(1_000_000_000_000_000_000u128),
            )
            .expect("seed caller balance");

        // Snapshot the nonce the cooling code will see for create address
        // derivation.
        let caller_nonce = ctx
            .journaled_state
            .load_account(caller)
            .expect("load caller")
            .data
            .info
            .nonce;
        let create_addr = caller.create(caller_nonce);

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_balance_slot(caller),
                BvmEth::get_balance_slot(create_addr),
            ],
        );
    }

    #[test]
    fn test_cooling_create_deposit_with_mint_and_transfer_cools_all_slots() {
        // E2: CREATE deposit with BOTH eth_value (mint) and eth_tx_value
        // (transfer). Must cool total_supply, balance[caller],
        // balance[create_addr].
        use revm::context_interface::JournalTr;

        let caller = address!("1111111111111111111111111111111111111111");
        let mut ctx = make_deposit_ctx(
            caller,
            revm::primitives::TxKind::Create,
            Some(1_000_000_000_000_000_000u128),
            Some(500_000_000_000_000_000u128),
        );

        let caller_nonce = ctx
            .journaled_state
            .load_account(caller)
            .expect("load caller")
            .data
            .info
            .nonce;
        let create_addr = caller.create(caller_nonce);

        BvmEth::process_eth_deposit(&mut ctx, false).expect("process_eth_deposit");

        assert_bvm_cooled(
            &ctx.journaled_state.inner.state,
            &[
                BvmEth::get_total_supply_slot(),
                BvmEth::get_balance_slot(caller),
                BvmEth::get_balance_slot(create_addr),
            ],
        );
    }

    #[test]
    fn test_deposit_gas_aligned_through_nested_call_to_bvm_eth() {
        // E3: tx.to = outer contract whose code is:
        //   PUSH1 0 PUSH1 0 PUSH1 0 PUSH1 0 PUSH1 0 PUSH20 <BVM_ETH> GAS CALL STOP
        //
        // i.e. CALL(BVM_ETH, 0, 0, 0, 0, 0) then STOP.
        //
        // BVM_ETH stub: PUSH1 2; SLOAD; POP; STOP.
        //
        // Expected gas (post-cooling):
        //   intrinsic 21000
        //   + calldata 0 (no calldata for this test)
        //   + outer frame: 5*PUSH1 + PUSH20 + GAS = 5*3 + 3 + 2 = 20
        //   + CALL (cold BVM_ETH): 2600
        //   + inner frame: PUSH1 + cold SLOAD + POP + STOP = 3 + 2100 + 2 + 0 = 2105
        //   + STOP in outer: 0
        //   ≈ 25725
        //
        // Without cooling but without 4500 magic: warm CALL (100) + warm SLOAD (100)
        //   = ~21228 — under-charged by ~4500.
        // With cooling AND stale 4500 magic: ~30225 — over-charged.
        //
        // Assert 25000 < gas_used < 30000.
        let caller = address!("7466be349b17a0f966f97ddeefe393894b9faf06");
        let outer = address!("000000000000000000000000000000000000c0de");

        let mut outer_code: Vec<u8> = Vec::new();
        // PUSH1 0 (returnSize, returnOffset, argsSize, argsOffset, value): 5 of them
        for _ in 0..5 {
            outer_code.push(0x60);
            outer_code.push(0x00);
        }
        // PUSH20 <BVM_ETH>
        outer_code.push(0x73);
        outer_code.extend_from_slice(BvmEth::ADDRESS.as_slice());
        // GAS
        outer_code.push(0x5a);
        // CALL
        outer_code.push(0xf1);
        // STOP
        outer_code.push(0x00);

        let outer_bc = Bytecode::new_raw(Bytes::from(outer_code));
        let outer_hash = outer_bc.hash_slow();

        let bvm_stub = Bytecode::new_raw(Bytes::from(vec![0x60, 0x02, 0x54, 0x50, 0x00]));
        let bvm_hash = bvm_stub.hash_slow();

        let mut db = InMemoryDB::default();
        db.insert_account_info(
            outer,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash: outer_hash,
                code: Some(outer_bc),
            },
        );
        db.insert_account_info(
            BvmEth::ADDRESS,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash: bvm_hash,
                code: Some(bvm_stub),
            },
        );
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(u128::MAX),
                nonce: 0,
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(outer);
                tx.base.gas_limit = 0x186a0;
                tx.base.gas_price = 0;
                tx.base.data = Bytes::new();
                tx.deposit.source_hash = B256::from([7u8; 32]);
                tx.deposit.mint = Some(0);
                tx.deposit.eth_value = Some(0x38d7ea4c68000u128);
                tx.deposit.eth_tx_value = None;
            });

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();
        let result = handler.run(&mut evm).expect("handler.run");
        let gas_used = result.gas_used();

        // Lower bound rules out warm CALL / warm SLOAD (the missing-cooling
        // case): that would land near 21228.
        assert!(
            gas_used > 25000,
            "nested CALL to BVM_ETH must pay cold account + cold storage cost; got gas_used={}",
            gas_used
        );
        // Upper bound rules out stale 4500 compensation on top.
        assert!(
            gas_used < 30000,
            "nested CALL to BVM_ETH must not include stale 4500 compensation; got gas_used={}",
            gas_used
        );
    }

    #[test]
    fn test_process_eth_deposit_mint_only_flag() {
        // When mint_only is true and eth_tx_value is present, should only mint
        let eth_value = 1_000_000_000_000_000_000u128;
        let eth_tx_value = 500_000_000_000_000_000u128;
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = Some(eth_value);
                tx.deposit.eth_tx_value = Some(eth_tx_value);
            });

        let result = BvmEth::process_eth_deposit(&mut ctx, true); // mint_only = true
        assert!(result.is_ok());

        // Should only have Mint event, no Transfer
        let logs = &ctx.journaled_state.inner.logs;
        assert_eq!(logs.len(), 1, "Should have exactly 1 log (Mint event only)");
        assert_eq!(logs[0].topics()[0], BvmEth::MINT_SELECTOR);
    }

    #[test]
    fn test_process_eth_deposit_mint_only_no_eth_value() {
        // When mint_only is true but only eth_tx_value is present,
        // should return early because needs_transfer is false when mint_only=true
        let eth_tx_value = 500_000_000_000_000_000u128;
        let caller = address!("1234567890123456789012345678901234567890");
        let to = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");

        let mut ctx = Context::op()
            .with_db(InMemoryDB::default())
            .with_chain(L1BlockInfo::default())
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.base.kind = revm::primitives::TxKind::Call(to);
                tx.base.gas_limit = 1_000_000;
                tx.deposit.eth_value = None;
                tx.deposit.eth_tx_value = Some(eth_tx_value);
            });

        let result = BvmEth::process_eth_deposit(&mut ctx, true); // mint_only = true
        assert!(result.is_ok());

        // BVM_ETH account should NOT be loaded because:
        // - needs_mint = false (eth_value is None)
        // - needs_transfer = false (mint_only is true)
        assert!(
            !ctx.journaled_state
                .inner
                .state
                .contains_key(&BvmEth::ADDRESS),
            "BVM_ETH should not be loaded when mint_only=true and eth_value is None"
        );

        assert!(ctx.journaled_state.inner.logs.is_empty());
    }

    /// Test case data structure
    #[derive(Debug, Serialize, Deserialize)]
    struct BvmEthDepositTestCase {
        block_number: u64,
        tx_hash: String,
        from: String,
        to: String,
        source_hash: String,
        eth_value: String,
        eth_tx_value: String,
        gas_limit: u64,
        tx_input: String,
        expected_gas_used: u64,
        expected_logs: Vec<ExpectedLog>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct ExpectedLog {
        address: String,
        topics: Vec<String>,
        data: String,
    }

    /// Load cache_db data from exported JSON file and build InMemoryDB
    fn load_cache_db_from_file(fixture_dir: &Path, block_number: u64) -> InMemoryDB {
        let cache_db_file = fixture_dir.join(format!("block_{}.json", block_number));

        let json_content = fs::read_to_string(&cache_db_file).unwrap_or_else(|e| {
            panic!(
                "Failed to read cache_db file from {}: {}",
                cache_db_file.display(),
                e
            )
        });

        let cache: Cache = serde_json::from_str(&json_content)
            .unwrap_or_else(|e| panic!("Failed to parse cache_db JSON: {}", e));

        let mut db = InMemoryDB::default();

        // Load contracts into cache (they are indexed by code_hash)
        // Since InMemoryDB is CacheDB<EmptyDB>, we can access cache directly
        for (code_hash, bytecode) in &cache.contracts {
            db.cache.contracts.insert(*code_hash, bytecode.clone());
        }

        // Load accounts from cache
        for (address, db_account) in &cache.accounts {
            let account_info = db_account.info.clone();
            db.insert_account_info(*address, account_info);

            // Load storage
            let account = db.load_account(*address).unwrap();
            for (key, value) in &db_account.storage {
                account.storage.insert(*key, *value);
            }
        }

        db
    }

    /// Load test case data from JSON file
    fn load_test_case(fixture_dir: &Path, test_file: &str) -> BvmEthDepositTestCase {
        let test_case_path = fixture_dir.join(test_file);
        let json_content = fs::read_to_string(&test_case_path).unwrap_or_else(|e| {
            panic!(
                "Failed to read test case from {}: {}",
                test_case_path.display(),
                e
            )
        });

        serde_json::from_str(&json_content)
            .unwrap_or_else(|e| panic!("Failed to parse test case JSON: {}", e))
    }

    /// Run BVM_ETH deposit transaction test
    fn run_bvm_eth_deposit_test(fixture_dir: &Path, test_case: BvmEthDepositTestCase) {
        let from = Address::from_str(&test_case.from).unwrap();
        let to = Address::from_str(&test_case.to).unwrap();
        let source_hash = B256::from_str(&test_case.source_hash).unwrap();
        let eth_value =
            U256::from_str_radix(test_case.eth_value.trim_start_matches("0x"), 16).unwrap();
        let eth_tx_value =
            U256::from_str_radix(test_case.eth_tx_value.trim_start_matches("0x"), 16).unwrap();
        let tx_input = hex::decode(test_case.tx_input.trim_start_matches("0x")).unwrap();

        // Load cache_db data
        let db = load_cache_db_from_file(fixture_dir, test_case.block_number);

        // Create block_env
        let block_env = BlockEnv {
            number: U256::from(test_case.block_number),
            beneficiary: Address::from_str("0x4200000000000000000000000000000000000011").unwrap(),
            timestamp: U256::from(1735128000u64),
            gas_limit: 30_000_000u64,
            basefee: 1_000_000_000u64,
            ..Default::default()
        };

        // Create L1BlockInfo
        let l1_block_info = L1BlockInfo {
            l2_block: Some(U256::from(test_case.block_number)),
            token_ratio: U256::from(3040),
            l1_base_fee: U256::from(1_000_000_000),
            l1_fee_overhead: Some(U256::from(188)),
            l1_base_fee_scalar: U256::from(10000),
            ..Default::default()
        };

        // Build deposit transaction parts
        let deposit = DepositTransactionParts {
            source_hash,
            mint: Some(0),
            is_system_transaction: false,
            eth_value: Some(eth_value.to::<u128>()),
            eth_tx_value: Some(eth_tx_value.to::<u128>()),
        };

        // Build complete OpTransaction
        let op_tx = OpTransaction {
            base: TxEnv {
                caller: from,
                kind: revm::primitives::TxKind::Call(to),
                gas_limit: test_case.gas_limit,
                gas_price: 0,
                value: U256::ZERO,
                data: Bytes::from(tx_input),
                ..Default::default()
            },
            enveloped_tx: None,
            deposit,
        };

        let ctx = Context::op()
            .with_db(db)
            .with_chain(l1_block_info)
            .with_block(block_env)
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .with_tx(op_tx);

        let mut evm = ctx.build_op();
        let mut handler = OpHandler::<
            _,
            EVMError<_, crate::transaction::error::OpTransactionError>,
            EthFrame<EthInterpreter>,
        >::new();

        // handler.run() will internally call validate_against_state_and_deduct_caller
        // so we should not call it manually here to avoid duplicate process_eth_deposit
        let result = handler.run(&mut evm).unwrap();

        // Verify transaction succeeds
        let logs = match &result {
            ExecutionResult::Success { logs, .. } => logs,
            ExecutionResult::Halt { reason, gas_used } => {
                panic!(
                    "Transaction halted with reason: {:?}, gas_used: {}",
                    reason, gas_used
                );
            }
            ExecutionResult::Revert { output, gas_used } => {
                panic!(
                    "Transaction reverted with output: {:?}, gas_used: {}",
                    output, gas_used
                );
            }
        };

        // 1. Verify gas used (most important check - do this first)
        let actual_gas_used = result.gas_used();
        assert_eq!(
            actual_gas_used, test_case.expected_gas_used,
            "Gas used mismatch! Expected: {}, Actual: {}",
            test_case.expected_gas_used, actual_gas_used
        );

        // 2. Verify logs
        verify_logs(logs, &test_case.expected_logs);
    }

    /// Verify that expected logs exist in actual logs
    fn verify_logs(logs: &[revm::primitives::Log], expected: &[ExpectedLog]) {
        // First verify that the number of logs matches
        assert_eq!(
            logs.len(),
            expected.len(),
            "Log count mismatch. Expected: {}, Actual: {}",
            expected.len(),
            logs.len()
        );

        // Parse all expected logs first
        let expected_logs_parsed: Vec<_> = expected
            .iter()
            .map(|e| {
                let address = Address::from_str(&e.address).unwrap();
                let topics: Vec<B256> = e
                    .topics
                    .iter()
                    .map(|t| B256::from_str(t).unwrap())
                    .collect();
                let data = hex::decode(e.data.trim_start_matches("0x")).unwrap();
                (address, topics, data)
            })
            .collect();

        // Track which logs have been matched to avoid duplicate matches
        let mut matched_indices = HashSet::new();

        for (i, (expected_address, expected_topics, expected_data)) in
            expected_logs_parsed.iter().enumerate()
        {
            // Find matching log by address, all topics, and data
            let matching_log_idx = logs.iter().enumerate().find(|(idx, log)| {
                !matched_indices.contains(idx)
                    && log.address == *expected_address
                    && log.topics().len() == expected_topics.len()
                    && log.topics() == expected_topics.as_slice()
                    && log.data.data.as_ref() == expected_data.as_slice()
            });

            assert!(
                matching_log_idx.is_some(),
                "Expected log {} not found. Address: {:?}, Topics: {:?}, Data: {}",
                i,
                expected_address,
                expected_topics.iter().map(hex::encode).collect::<Vec<_>>(),
                hex::encode(expected_data)
            );

            matched_indices.insert(matching_log_idx.unwrap().0);
        }
    }

    /// Executes a BVM_ETH deposit test fixture stored at the passed `fixture_path` (tar.gz file)
    /// and asserts that the execution results match the expected values.
    async fn run_test_fixture(fixture_path: PathBuf) {
        // First, untar the fixture
        let fixture_dir = tempdir().expect("Failed to create temporary directory");
        let output = tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(fixture_path.as_path())
            .arg("-C")
            .arg(fixture_dir.path())
            .output()
            .await
            .expect("Failed to untar fixture");

        if !output.status.success() {
            panic!(
                "Failed to untar fixture: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Find all bvm_eth_deposit_*.json files in the fixture directory
        let test_files: Vec<_> = fs::read_dir(fixture_dir.path())
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_file() {
                    let file_name = path.file_name()?.to_str()?;
                    if file_name.starts_with("bvm_eth_deposit_") && file_name.ends_with(".json") {
                        return Some(file_name.to_string());
                    }
                }
                None
            })
            .collect();

        assert!(
            !test_files.is_empty(),
            "No bvm_eth_deposit_*.json files found in fixture directory: {}",
            fixture_dir.path().display()
        );

        // Run test for each test file
        for test_file in test_files {
            let test_case = load_test_case(fixture_dir.path(), &test_file);
            run_bvm_eth_deposit_test(fixture_dir.path(), test_case);
        }
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_bvm_eth_deposit(
        #[base_dir = "./src/test_data"]
        #[files("*.tar.gz")]
        path: PathBuf,
    ) {
        run_test_fixture(path).await;
    }
}
