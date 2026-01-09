//!Handler related to Optimism chain
use crate::{
    api::exec::OpContextTr,
    constants::{
        BASE_FEE_RECIPIENT, GAS_ORACLE_CONTRACT, L1_FEE_RECIPIENT, OPERATOR_FEE_RECIPIENT,
    },
    transaction::{deposit::DEPOSIT_TRANSACTION_TYPE, OpTransactionError, OpTxTr},
    BvmEth, L1BlockInfo, OpHaltReason, OpSpecId,
};
use revm::{
    context::{journaled_state::JournalCheckpoint, result::InvalidTransaction, LocalContextTr},
    context_interface::{
        context::ContextError,
        result::{EVMError, ExecutionResult, FromStringError},
        Block, Cfg, ContextTr, JournalTr, Transaction,
    },
    handler::{
        evm::FrameTr,
        handler::EvmTrError,
        post_execution::{self, reimburse_caller},
        pre_execution::{calculate_caller_fee, validate_account_nonce_and_code_with_components},
        validation::validate_tx_env,
        EthFrame, EvmTr, FrameResult, Handler, MainnetHandler,
    },
    inspector::{Inspector, InspectorEvmTr, InspectorHandler},
    interpreter::{
        interpreter::EthInterpreter, interpreter_action::FrameInit, Gas, InitialAndFloorGas,
    },
    primitives::{hardfork::SpecId, U256},
};
use std::{boxed::Box, vec};

/// Optimism handler extends the [`Handler`] with Optimism specific logic.
#[derive(Debug, Clone)]
pub struct OpHandler<EVM, ERROR, FRAME> {
    /// Mainnet handler allows us to use functions from the mainnet handler inside optimism handler.
    /// So we dont duplicate the logic
    pub mainnet: MainnetHandler<EVM, ERROR, FRAME>,
}

impl<EVM, ERROR, FRAME> OpHandler<EVM, ERROR, FRAME> {
    /// Create a new Optimism handler.
    pub fn new() -> Self {
        Self {
            mainnet: MainnetHandler::default(),
        }
    }
}

impl<EVM, ERROR, FRAME> Default for OpHandler<EVM, ERROR, FRAME> {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait to check if the error is a transaction error.
///
/// Used in cache_error handler to catch deposit transaction that was halted.
pub trait IsTxError {
    /// Check if the error is a transaction error.
    fn is_tx_error(&self) -> bool;
}

impl<DB, TX> IsTxError for EVMError<DB, TX> {
    fn is_tx_error(&self) -> bool {
        matches!(self, EVMError::Transaction(_))
    }
}

impl<EVM, ERROR, FRAME> Handler for OpHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: OpContextTr, Frame = FRAME>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
    // TODO `FrameResult` should be a generic trait.
    // TODO `FrameInit` should be a generic.
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = OpHaltReason;

    fn validate_env(&self, evm: &mut Self::Evm) -> Result<(), Self::Error> {
        // Do not perform any extra validation for deposit transactions, they are pre-verified on L1.
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let tx_type = tx.tx_type();
        if tx_type == DEPOSIT_TRANSACTION_TYPE {
            // Do not allow for a system transaction to be processed if Regolith is enabled.
            if tx.is_system_transaction()
                && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH)
            {
                return Err(OpTransactionError::DepositSystemTxPostRegolith.into());
            }
            return Ok(());
        }

        // Check that non-deposit transactions have enveloped_tx set
        if tx.enveloped_tx().is_none() {
            return Err(OpTransactionError::MissingEnvelopedTx.into());
        }

        let spec = ctx.cfg().spec();
        if spec.is_enabled_in(OpSpecId::ARSIA) {
            self.mainnet.validate_env(evm)
        } else {
            validate_tx_env(ctx, spec.into_eth_spec()).map_err(Into::into)
        }
    }

    /**
     * Validates and calculates the initial gas requirements for a transaction.
     *
     * For deposit transactions, this simply returns the base gas calculation from the mainnet handler.
     * For non-deposit transactions, this method:
     * 1. Updates L1 block info in the following scenarios:
     *    - When the current transaction belongs to a different block than previously cached
     *    - When the transaction target is the gas oracle contract, indicating a token ratio update
     *      that needs to be reloaded for the next transaction
     * 2. Adjusts both initial_gas and floor_gas by multiplying with the token ratio
     * 3. If ARSIA is enabled, we don't need to multiply the initial gas and floor gas by the token ratio
     *
     * The token ratio adjustment is essential for properly accounting for the price difference
     * between ETH and MNT.
     */
    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<InitialAndFloorGas, Self::Error> {
        let mut initial_gas = self.mainnet.validate_initial_tx_gas(evm)?;

        let (block, tx, cfg, journal, chain, _) = evm.ctx().all_mut();
        let spec = cfg.spec();
        if tx.tx_type() == DEPOSIT_TRANSACTION_TYPE {
            Ok(initial_gas)
        } else {
            // L1 block info is stored in the context for later use.
            // and it will be reloaded from the database if it is not for the current block.
            if chain.l2_block != Some(block.number()) {
                *chain = L1BlockInfo::try_fetch(journal.db_mut(), block.number(), spec)?;
            }

            // Reset the l2_block if the tx is set token ratio, we need reload token ratio from the database in next transaction
            if tx.kind().to() == Some(&GAS_ORACLE_CONTRACT) {
                chain.reset_l2_block();
            }

            if !cfg.spec().is_enabled_in(OpSpecId::ARSIA) {
                // if the tx is not a deposit transaction and ARSIA is not enabled, we need to multiply the initial gas by the token ratio
                let token_ratio = chain.token_ratio;
                initial_gas.initial_gas = initial_gas
                    .initial_gas
                    .checked_mul(token_ratio.try_into().unwrap())
                    .ok_or(InvalidTransaction::CallerGasLimitMoreThanBlock)?;

                initial_gas.floor_gas = initial_gas
                    .floor_gas
                    .checked_mul(token_ratio.try_into().unwrap())
                    .ok_or(InvalidTransaction::CallerGasLimitMoreThanBlock)?;
            }

            Ok(initial_gas)
        }
    }

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();
        let is_deposit = ctx.tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;

        if is_deposit {
            // Process ETH deposit by minting and transferring BVM_ETH tokens.
            BvmEth::process_eth_deposit(ctx, false).map_err(ERROR::from)?;
        }

        let (block, tx, cfg, journal, chain, _) = evm.ctx().all_mut();
        let spec = cfg.spec();

        if is_deposit {
            let basefee = block.basefee() as u128;
            let blob_price = block.blob_gasprice().unwrap_or_default();
            // deposit skips max fee check and just deducts the effective balance spending.

            let mut caller = journal.load_account_with_code_mut(tx.caller())?.data;

            let effective_balance_spending = tx
                .effective_balance_spending(basefee, blob_price)
                .expect("Deposit transaction effective balance spending overflow")
                - tx.value();

            // Mind value should be added first before subtracting the effective balance spending.
            let mut new_balance = caller
                .balance()
                .saturating_add(U256::from(tx.mint().unwrap_or_default()))
                .saturating_sub(effective_balance_spending);

            if cfg.is_balance_check_disabled() {
                // Make sure the caller's balance is at least the value of the transaction.
                // this is not consensus critical, and it is used in testing.
                new_balance = new_balance.max(tx.value());
            }

            // set the new balance and bump the nonce if it is a call
            caller.set_balance(new_balance);
            if tx.kind().is_call() {
                caller.bump_nonce();
            }

            return Ok(());
        }

        let mut caller_account = journal.load_account_with_code_mut(tx.caller())?.data;

        // validates account nonce and code
        validate_account_nonce_and_code_with_components(&caller_account.info, tx, cfg)?;

        // check additional cost and deduct it from the caller's balances
        let mut balance = caller_account.info.balance;

        // if ARSIA is enabled, we need to calculate the additional cost and deduct it from the caller's balances
        if !cfg.is_fee_charge_disabled() && cfg.spec().is_enabled_in(OpSpecId::ARSIA) {
            let Some(additional_cost) = chain.tx_cost_with_tx(tx, spec) else {
                return Err(ERROR::from_string(
                    "[OPTIMISM] Failed to load enveloped transaction.".into(),
                ));
            };
            let Some(new_balance) = balance.checked_sub(additional_cost) else {
                return Err(InvalidTransaction::LackOfFundForMaxFee {
                    fee: Box::new(additional_cost),
                    balance: Box::new(balance),
                }
                .into());
            };
            balance = new_balance
        }

        let balance = calculate_caller_fee(balance, tx, block, cfg)?;

        // make changes to the account
        caller_account.set_balance(balance);
        if tx.kind().is_call() {
            caller_account.bump_nonce();
        }

        Ok(())
    }

    fn last_frame_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let tx_gas_limit = tx.gas_limit();
        let is_regolith = ctx.cfg().spec().is_enabled_in(OpSpecId::REGOLITH);

        let instruction_result = frame_result.interpreter_result().result;
        let gas = frame_result.gas_mut();
        let remaining = gas.remaining();
        let refunded = gas.refunded();

        // Spend the gas limit. Gas is reimbursed when the tx returns successfully.
        *gas = Gas::new_spent(tx_gas_limit);

        if instruction_result.is_ok() {
            // On Optimism, deposit transactions report gas usage uniquely to other
            // transactions due to them being pre-paid on L1.
            //
            // Hardfork Behavior:
            // - Bedrock (success path):
            //   - Deposit transactions (non-system) report their gas limit as the usage.
            //     No refunds.
            //   - Deposit transactions (system) report 0 gas used. No refunds.
            //   - Regular transactions report gas usage as normal.
            // - Regolith (success path):
            //   - Deposit transactions (all) report their gas used as normal. Refunds
            //     enabled.
            //   - Regular transactions report their gas used as normal.
            if !is_deposit || is_regolith {
                // For regular transactions prior to Regolith and all transactions after
                // Regolith, gas is reported as normal.
                gas.erase_cost(remaining);
                gas.record_refund(refunded);
            } else if is_deposit {
                let tx = ctx.tx();
                if tx.is_system_transaction() {
                    // System transactions were a special type of deposit transaction in
                    // the Bedrock hardfork that did not incur any gas costs.
                    gas.erase_cost(tx_gas_limit);
                }
            }
        } else if instruction_result.is_revert() {
            // On Optimism, deposit transactions report gas usage uniquely to other
            // transactions due to them being pre-paid on L1.
            //
            // Hardfork Behavior:
            // - Bedrock (revert path):
            //   - Deposit transactions (all) report the gas limit as the amount of gas
            //     used on failure. No refunds.
            //   - Regular transactions receive a refund on remaining gas as normal.
            // - Regolith (revert path):
            //   - Deposit transactions (all) report the actual gas used as the amount of
            //     gas used on failure. Refunds on remaining gas enabled.
            //   - Regular transactions receive a refund on remaining gas as normal.
            if !is_deposit || is_regolith {
                gas.erase_cost(remaining);
            }
        }
        Ok(())
    }

    fn reimburse_caller(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let mut additional_refund = U256::ZERO;

        if evm.ctx().tx().tx_type() != DEPOSIT_TRANSACTION_TYPE
            && !evm.ctx().cfg().is_fee_charge_disabled()
        {
            let spec = evm.ctx().cfg().spec();
            additional_refund = evm
                .ctx()
                .chain()
                .operator_fee_refund(frame_result.gas(), spec);
        }

        reimburse_caller(evm.ctx(), frame_result.gas(), additional_refund).map_err(From::from)
    }

    fn execution(
        &mut self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &InitialAndFloorGas,
    ) -> Result<FrameResult, Self::Error> {
        let (block, tx, cfg, _, chain, _) = evm.ctx().all_mut();

        if cfg.spec().is_enabled_in(OpSpecId::ARSIA) {
            return self.mainnet.execution(evm, init_and_floor_gas);
        }

        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let mut gas_limit = tx.gas_limit() - init_and_floor_gas.initial_gas;

        // l1cost = l1cost / effective_gas_price
        // gas_limit = gas_limit - l1cost
        // gas_limit = gas_limit / token_ratio
        if !is_deposit {
            let spec = cfg.spec();

            let enveloped_tx = tx
                .enveloped_tx()
                .expect("all not deposit tx have enveloped tx")
                .clone();
            let basefee = block.basefee() as u128;
            let mut tx_l1_cost = chain.calculate_tx_l1_cost(&enveloped_tx, spec);
            let effective_gas_price = tx.effective_gas_price(basefee);

            if effective_gas_price > 0 {
                tx_l1_cost = tx_l1_cost.wrapping_div(U256::from(effective_gas_price));
            }

            if tx_l1_cost.gt(&U256::from(gas_limit)) {
                return Err(ERROR::from(OpTransactionError::Base(
                    InvalidTransaction::CallGasCostMoreThanGasLimit {
                        initial_gas: init_and_floor_gas.initial_gas,
                        gas_limit,
                    },
                )));
            }

            // Edge case: if token ratio is zero, set it to 1.
            // This is only possible if the token ratio is not set at all.
            let token_ratio = chain.token_ratio.max(U256::from(1));
            gas_limit = gas_limit
                .wrapping_sub(tx_l1_cost.try_into().unwrap())
                .wrapping_div(token_ratio.try_into().unwrap());
        }

        // Create first frame action
        let first_frame_input = self.first_frame_input(evm, gas_limit)?;

        // Run execution loop
        let mut frame_result = self.run_exec_loop(evm, first_frame_input)?;

        // Handle last frame result
        self.last_frame_result(evm, &mut frame_result)?;
        Ok(frame_result)
    }

    fn refund(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
        eip7702_refund: i64,
    ) {
        frame_result.gas_mut().record_refund(eip7702_refund);
        let (_, tx, cfg, _, chain, _) = evm.ctx().all_mut();
        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_regolith = cfg.spec().is_enabled_in(OpSpecId::REGOLITH);
        let is_london = cfg.spec().into_eth_spec().is_enabled_in(SpecId::LONDON);
        let is_gas_refund_disabled = is_deposit && !is_regolith;
        if cfg.spec().is_enabled_in(OpSpecId::ARSIA) && !is_gas_refund_disabled {
            // Prior to Regolith, deposit transactions did not receive gas refunds.
            frame_result.gas_mut().set_final_refund(is_london);
        } else {
            let is_system = tx.is_system_transaction();
            let gas = frame_result.gas_mut();

            if tx.eth_value().is_some() && !tx.input().is_empty() {
                gas.set_remaining(gas.remaining().saturating_sub(4500));
            }

            let limit = gas.limit();
            let token_ratio_u64: u64 = chain.token_ratio.try_into().unwrap();

            assert!(
                token_ratio_u64 <= i64::MAX as u64,
                "token_ratio {token_ratio_u64} exceeds i64::MAX"
            );

            if !is_system && !is_deposit {
                // limit = limit / token_ratio
                if token_ratio_u64 > 0 {
                    gas.set_limit(gas.limit().saturating_div(token_ratio_u64));
                }
            } else {
                gas.set_refund(0);
            }

            // Prior to Regolith, deposit transactions did not receive gas refunds.
            if !is_gas_refund_disabled {
                gas.set_final_refund(is_london);
            }

            if !is_system && !is_deposit {
                // refund = refund * token_ratio
                // remaining = remaining * token_ratio
                gas.set_refund(gas.refunded().saturating_mul(token_ratio_u64 as i64));
                gas.set_remaining(gas.remaining().saturating_mul(token_ratio_u64));

                // restore the original gas limit
                gas.set_limit(limit);
            }
        }
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        frame_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;

        // Transfer fee to coinbase/beneficiary.
        if is_deposit {
            return Ok(());
        }

        self.mainnet.reward_beneficiary(evm, frame_result)?;
        let basefee = evm.ctx().block().basefee() as u128;

        // If the transaction is not a deposit transaction, fees are paid out
        // to both the Base Fee Vault as well as the L1 Fee Vault.
        let ctx = evm.ctx();
        let enveloped = ctx.tx().enveloped_tx().cloned();
        let spec = ctx.cfg().spec();
        let l1_block_info = ctx.chain_mut();

        let Some(enveloped_tx) = &enveloped else {
            return Err(ERROR::from_string(
                "[OPTIMISM] Failed to load enveloped transaction.".into(),
            ));
        };

        let l1_cost = l1_block_info.calculate_tx_l1_cost(enveloped_tx, spec);
        let operator_fee_cost = if spec.is_enabled_in(OpSpecId::ARSIA) {
            l1_block_info.operator_fee_charge(enveloped_tx, U256::from(frame_result.gas().used()))
        } else {
            U256::ZERO
        };
        let base_fee_amount = U256::from(basefee.saturating_mul(frame_result.gas().used() as u128));

        // Send fees to their respective recipients
        let mut recipients = vec![(BASE_FEE_RECIPIENT, base_fee_amount)];
        if spec.is_enabled_in(OpSpecId::ARSIA) {
            recipients.extend([
                (L1_FEE_RECIPIENT, l1_cost),
                (OPERATOR_FEE_RECIPIENT, operator_fee_cost),
            ]);
        }
        for (recipient, amount) in recipients {
            ctx.journal_mut().balance_incr(recipient, amount)?;
        }

        Ok(())
    }

    fn execution_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        match core::mem::replace(evm.ctx().error(), Ok(())) {
            Err(ContextError::Db(e)) => return Err(e.into()),
            Err(ContextError::Custom(e)) => return Err(Self::Error::from_string(e)),
            Ok(_) => (),
        }

        let exec_result =
            post_execution::output(evm.ctx(), frame_result).map_haltreason(OpHaltReason::Base);

        if exec_result.is_halt() {
            // Post-regolith, if the transaction is a deposit transaction and it halts,
            // we bubble up to the global return handler. The mint value will be persisted
            // and the caller nonce will be incremented there.
            let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
            if is_deposit && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH) {
                return Err(ERROR::from(OpTransactionError::HaltedDepositPostRegolith));
            }
        }
        evm.ctx().journal_mut().commit_tx();
        evm.ctx().chain_mut().clear_tx_l1_cost();
        evm.ctx().local_mut().clear();
        evm.frame_stack().clear();

        Ok(exec_result)
    }

    fn catch_error(
        &self,
        evm: &mut Self::Evm,
        error: Self::Error,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let output = if error.is_tx_error() && is_deposit {
            let ctx = evm.ctx();
            let spec = ctx.cfg().spec();
            let tx = ctx.tx();
            let caller = tx.caller();
            let mint = tx.mint();
            let is_system_tx = tx.is_system_transaction();
            let gas_limit = tx.gas_limit();
            let journal = evm.ctx().journal_mut();

            // discard all changes of this transaction
            // Default JournalCheckpoint is the first checkpoint and will wipe all changes.
            journal.checkpoint_revert(JournalCheckpoint::default());

            // If the transaction is a deposit transaction and it failed
            // for any reason, the caller nonce must be bumped, and the
            // gas reported must be altered depending on the Hardfork. This is
            // also returned as a special Halt variant so that consumers can more
            // easily distinguish between a failed deposit and a failed
            // normal transaction.

            // Increment sender nonce and account balance for the mint amount. Deposits
            // always persist the mint amount, even if the transaction fails.
            let mut acc = journal.load_account_mut(caller)?;
            acc.bump_nonce();
            acc.incr_balance(U256::from(mint.unwrap_or_default()));

            // We can now commit the changes.
            journal.commit_tx();

            // If the transaction failed, we only mint the BVM_ETH tokens.
            // We do not transfer the BVM_ETH tokens.
            BvmEth::process_eth_deposit(evm.ctx(), true).map_err(ERROR::from)?;

            // The gas used of a failed deposit post-regolith is the gas
            // limit of the transaction. pre-regolith, it is the gas limit
            // of the transaction for non system transactions and 0 for system
            // transactions.
            let gas_used = if spec.is_enabled_in(OpSpecId::REGOLITH) || !is_system_tx {
                gas_limit
            } else {
                0
            };
            // clear the journal
            Ok(ExecutionResult::Halt {
                reason: OpHaltReason::FailedDeposit,
                gas_used,
            })
        } else {
            Err(error)
        };
        // do the cleanup
        evm.ctx().chain_mut().clear_tx_l1_cost();
        evm.ctx().local_mut().clear();
        evm.frame_stack().clear();

        output
    }
}

impl<EVM, ERROR> InspectorHandler for OpHandler<EVM, ERROR, EthFrame<EthInterpreter>>
where
    EVM: InspectorEvmTr<
        Context: OpContextTr,
        Frame = EthFrame<EthInterpreter>,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
{
    type IT = EthInterpreter;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::default_ctx::OpContext,
        constants::{
            BASE_FEE_SCALAR_OFFSET, ECOTONE_L1_BLOB_BASE_FEE_SLOT, ECOTONE_L1_FEE_SCALARS_SLOT,
            L1_BASE_FEE_SLOT, L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT, L1_SCALAR_SLOT,
            OPERATOR_FEE_SCALARS_SLOT, TOKEN_RATIO_SLOT,
        },
        DefaultOp, OpBuilder, OpTransaction,
    };
    use alloy_primitives::uint;
    use revm::{
        context::{BlockEnv, Context, TxEnv},
        context_interface::result::InvalidTransaction,
        database::InMemoryDB,
        database_interface::EmptyDB,
        handler::EthFrame,
        interpreter::{CallOutcome, InstructionResult, InterpreterResult},
        primitives::{bytes, hex, Address, Bytes, B256},
        state::AccountInfo,
    };
    use rstest::rstest;
    use std::{boxed::Box, str::FromStr};

    /// Creates frame result.
    fn call_last_frame_return(
        ctx: OpContext<EmptyDB>,
        instruction_result: InstructionResult,
        gas: Gas,
    ) -> Gas {
        let mut evm = ctx.build_op();

        let mut exec_result = FrameResult::Call(CallOutcome::new(
            InterpreterResult {
                result: instruction_result,
                output: Bytes::new(),
                gas,
            },
            0..0,
        ));

        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        handler
            .last_frame_result(&mut evm, &mut exec_result)
            .unwrap();
        handler.refund(&mut evm, &mut exec_result, 0);
        *exec_result.gas()
    }

    #[test]
    fn test_revert_gas() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::BEDROCK);

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, Gas::new(90));
        assert_eq!(gas.remaining(), 0);
        assert_eq!(gas.spent(), 100);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_revert_gas_arsia() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA);
        let gas = call_last_frame_return(ctx, InstructionResult::Revert, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA);

        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_with_refund() {
        // Use non-deposit transaction to test refund logic
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::ZERO)
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build()
                    .unwrap(),
            )
            .with_chain(L1BlockInfo {
                token_ratio: U256::from(1),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut ret_gas = Gas::new(90);
        ret_gas.record_refund(20);

        let gas = call_last_frame_return(ctx.clone(), InstructionResult::Stop, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 2); // min(20, 10/5)

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_deposit_tx() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::from([1u8; 32]))
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::BEDROCK);
        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 0);
        assert_eq!(gas.spent(), 100);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_sys_deposit_tx() {
        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::from([1u8; 32]))
                    .is_system_transaction()
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::BEDROCK);
        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 100);
        assert_eq!(gas.spent(), 0);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_commit_mint_value() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1000),
                ..Default::default()
            },
        );

        let mut ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);
        ctx.modify_tx(|tx| {
            tx.deposit.source_hash = B256::from([1u8; 32]);
            tx.deposit.mint = Some(10);
        });

        let mut evm = ctx.build_op();

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1010));
    }

    #[test]
    fn test_remove_l1_cost_non_deposit() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1610),
                ..Default::default()
            },
        );
        // Set up L1 block contract storage for ARSIA
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, U256::from(1_000));
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, U256::ZERO);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_FEE_SCALARS_SLOT, U256::from(1_000) << 128); // base_fee_scalar = 1000
        let gas_oracle_contract = db.load_account(GAS_ORACLE_CONTRACT).unwrap();
        gas_oracle_contract
            .storage
            .insert(TOKEN_RATIO_SLOT, U256::from(1)); // token_ratio = 1

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_base_fee_scalar: U256::from(1_000),
                l1_blob_base_fee: Some(U256::ZERO),
                l1_blob_base_fee_scalar: Some(U256::ZERO),
                l2_block: Some(U256::from(0)),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                token_ratio: U256::from(1),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA)
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .source_hash(B256::ZERO)
                    .build()
                    .unwrap(),
            );

        let mut evm = ctx.build_op();

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(10)); // 1610 - 1600 = 10
    }

    #[test]
    fn test_reload_l1_block_info_isthmus() {
        const BLOCK_NUM: U256 = uint!(100_U256);
        const L1_BASE_FEE: U256 = uint!(1_U256);

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        db.insert_account_info(
            Address::ZERO,
            AccountInfo {
                balance: U256::from(10_000_000_000_000u64),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM + U256::from(1)),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS);

        let mut evm = ctx.build_op();

        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        // Call validate_initial_tx_gas first to load L1BlockInfo
        handler.validate_initial_tx_gas(&mut evm).unwrap();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_base_fee_scalar: U256::ZERO,
                l1_fee_overhead: Some(U256::ZERO),
                token_ratio: U256::ZERO,
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_parse_da_footprint_gas_scalar_jovian() {
        const BLOCK_NUM: U256 = uint!(100_U256);
        const L1_BASE_FEE: U256 = uint!(1_U256);
        const L1_BLOB_BASE_FEE: U256 = uint!(2_U256);
        const L1_BASE_FEE_SCALAR: u64 = 3;
        const L1_BLOB_BASE_FEE_SCALAR: u64 = 4;
        const L1_FEE_SCALARS: U256 = U256::from_limbs([
            0,
            (L1_BASE_FEE_SCALAR << (64 - BASE_FEE_SCALAR_OFFSET * 2)) | L1_BLOB_BASE_FEE_SCALAR,
            0,
            0,
        ]);
        const OPERATOR_FEE_SCALAR: u8 = 5;
        const OPERATOR_FEE_CONST: u8 = 6;
        const DA_FOOTPRINT_GAS_SCALAR: u8 = 7;
        let mut operator_fee_and_da_footprint = [0u8; 32];
        operator_fee_and_da_footprint[31] = OPERATOR_FEE_CONST;
        operator_fee_and_da_footprint[23] = OPERATOR_FEE_SCALAR;
        operator_fee_and_da_footprint[19] = DA_FOOTPRINT_GAS_SCALAR;
        let operator_fee_and_da_footprint_u256 = U256::from_be_bytes(operator_fee_and_da_footprint);

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, L1_BLOB_BASE_FEE);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_FEE_SCALARS_SLOT, L1_FEE_SCALARS);
        l1_block_contract.storage.insert(
            OPERATOR_FEE_SCALARS_SLOT,
            operator_fee_and_da_footprint_u256,
        );
        db.insert_account_info(
            Address::ZERO,
            AccountInfo {
                balance: U256::from(20_000_000),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM + U256::from(1)), // ahead by one block
                operator_fee_scalar: Some(U256::from(2)),
                operator_fee_constant: Some(U256::from(50)),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA)
            // set the operator fee to a low value
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(30_000))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            );

        let mut evm = ctx.build_op();

        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        // Call validate_initial_tx_gas first to load L1BlockInfo
        handler.validate_initial_tx_gas(&mut evm).unwrap();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                l1_blob_base_fee: Some(L1_BLOB_BASE_FEE),
                l1_blob_base_fee_scalar: Some(U256::from(L1_BLOB_BASE_FEE_SCALAR)),
                empty_ecotone_scalars: false,
                l1_fee_overhead: None,
                operator_fee_scalar: Some(U256::from(OPERATOR_FEE_SCALAR)),
                operator_fee_constant: Some(U256::from(OPERATOR_FEE_CONST)),
                tx_l1_cost: Some(U256::ZERO),
                da_footprint_gas_scalar: Some(DA_FOOTPRINT_GAS_SCALAR as u16),
                token_ratio: U256::ZERO,
            }
        );
    }

    #[test]
    fn test_reload_l1_block_info_regolith() {
        const BLOCK_NUM: U256 = uint!(200_U256);
        const L1_BASE_FEE: U256 = uint!(7_U256);
        const L1_FEE_OVERHEAD: U256 = uint!(9_U256);
        const L1_BASE_FEE_SCALAR: u64 = 11;

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        // Pre-ecotone bedrock/regolith slots
        use crate::constants::{L1_OVERHEAD_SLOT, L1_SCALAR_SLOT};
        l1_block_contract
            .storage
            .insert(L1_OVERHEAD_SLOT, L1_FEE_OVERHEAD);
        l1_block_contract
            .storage
            .insert(L1_SCALAR_SLOT, U256::from(L1_BASE_FEE_SCALAR));

        db.insert_account_info(
            Address::ZERO,
            AccountInfo {
                balance: U256::from(1000),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM + U256::from(1)),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        // Call validate_initial_tx_gas first to load L1BlockInfo
        handler.validate_initial_tx_gas(&mut evm).unwrap();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_fee_overhead: Some(L1_FEE_OVERHEAD),
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                token_ratio: U256::ZERO,
                tx_l1_cost: None,
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_reload_l1_block_info_ecotone_pre_isthmus() {
        const BLOCK_NUM: U256 = uint!(300_U256);
        const L1_BASE_FEE: U256 = uint!(13_U256);
        const L1_BLOB_BASE_FEE: U256 = uint!(17_U256);
        const L1_BASE_FEE_SCALAR: u64 = 19;
        const L1_BLOB_BASE_FEE_SCALAR: u64 = 23;
        const L1_FEE_SCALARS: U256 = U256::from_limbs([
            0,
            (L1_BASE_FEE_SCALAR << (64 - BASE_FEE_SCALAR_OFFSET * 2)) | L1_BLOB_BASE_FEE_SCALAR,
            0,
            0,
        ]);

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, L1_BLOB_BASE_FEE);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_FEE_SCALARS_SLOT, L1_FEE_SCALARS);
        db.insert_account_info(
            Address::ZERO,
            AccountInfo {
                balance: U256::from(1000),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM + U256::from(1)),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ECOTONE);

        let mut evm = ctx.build_op();
        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        // Call validate_initial_tx_gas first to load L1BlockInfo
        handler.validate_initial_tx_gas(&mut evm).unwrap();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_base_fee_scalar: U256::ZERO,
                l1_fee_overhead: Some(U256::ZERO),
                token_ratio: U256::ZERO,
                tx_l1_cost: None,
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_load_l1_block_info_isthmus_none() {
        const BLOCK_NUM: U256 = uint!(100_U256);
        const L1_BASE_FEE: U256 = uint!(1_U256);

        let mut db = InMemoryDB::default();
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        db.insert_account_info(
            Address::ZERO,
            AccountInfo {
                balance: U256::from(10_000_000_000_000u64),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS);

        let mut evm = ctx.build_op();

        assert_ne!(evm.ctx().chain().l2_block, Some(BLOCK_NUM));

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();
        // Call validate_initial_tx_gas first to load L1BlockInfo
        handler.validate_initial_tx_gas(&mut evm).unwrap();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert_eq!(
            *evm.ctx().chain(),
            L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                l1_base_fee: L1_BASE_FEE,
                l1_base_fee_scalar: U256::ZERO,
                l1_fee_overhead: Some(U256::ZERO),
                tx_l1_cost: None,
                token_ratio: U256::ZERO,
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_remove_l1_cost() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1601),
                ..Default::default()
            },
        );
        // Set up L1 block contract storage for ARSIA
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, U256::from(1_000));
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, U256::ZERO);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_FEE_SCALARS_SLOT, U256::from(1_000) << 128); // base_fee_scalar = 1000
        let gas_oracle_contract = db.load_account(GAS_ORACLE_CONTRACT).unwrap();
        gas_oracle_contract
            .storage
            .insert(TOKEN_RATIO_SLOT, U256::from(1)); // token_ratio = 1

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_base_fee_scalar: U256::from(1_000),
                l1_blob_base_fee: Some(U256::ZERO),
                l1_blob_base_fee_scalar: Some(U256::ZERO),
                l2_block: Some(U256::from(0)),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                token_ratio: U256::from(1),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA)
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(100))
                    .source_hash(B256::ZERO)
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build()
                    .unwrap(),
            );

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // l1block cost is 1600 fee.
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1)); // 1601 - 1600 = 1
    }

    #[test]
    fn test_remove_operator_cost_isthmus() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(151),
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                operator_fee_scalar: Some(U256::from(10_000_000)),
                operator_fee_constant: Some(U256::from(50)),
                l2_block: Some(U256::from(0)),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS)
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(10))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            );

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Under Isthmus the operator fee cost is operator_fee_scalar * gas_limit / 1e6 + operator_fee_constant
        // 10_000_000 * 10 / 1_000_000 + 50 = 150
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(151));
    }

    #[test]
    fn test_remove_operator_cost_jovian() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(2_051),
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                operator_fee_scalar: Some(U256::from(2)),
                operator_fee_constant: Some(U256::from(50)),
                l2_block: Some(U256::from(0)),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA)
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(10))
                    .enveloped_tx(Some(bytes!("FACADE")))
                    .build_fill(),
            );

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Under Jovian the operator fee cost is operator_fee_scalar * gas_limit * 100 + operator_fee_constant
        // 2 * 10 * 100 + 50 = 2_050
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        let account = evm.ctx().journal_mut().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1));
    }

    #[test]
    fn test_remove_l1_cost_lack_of_funds() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(48),
                ..Default::default()
            },
        );

        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, U256::from(1_000));
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_BLOB_BASE_FEE_SLOT, U256::ZERO);
        l1_block_contract
            .storage
            .insert(ECOTONE_L1_FEE_SCALARS_SLOT, U256::from(1_000) << 128); // base_fee_scalar = 1000
        let gas_oracle_contract = db.load_account(GAS_ORACLE_CONTRACT).unwrap();
        gas_oracle_contract
            .storage
            .insert(TOKEN_RATIO_SLOT, U256::from(1)); // token_ratio = 1

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_base_fee_scalar: U256::from(1_000),
                l1_blob_base_fee: Some(U256::ZERO),
                l1_blob_base_fee_scalar: Some(U256::ZERO),
                l2_block: Some(U256::from(0)),
                operator_fee_scalar: Some(U256::ZERO),
                operator_fee_constant: Some(U256::ZERO),
                token_ratio: U256::from(1),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA)
            .modify_tx_chained(|tx| {
                tx.enveloped_tx = Some(bytes!("FACADE"));
            });

        // l1block cost is 1048 fee.
        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // l1block cost is 1600 fee.
        assert_eq!(
            handler.validate_against_state_and_deduct_caller(&mut evm),
            Err(EVMError::Transaction(
                InvalidTransaction::LackOfFundForMaxFee {
                    fee: Box::new(U256::from(1600)),
                    balance: Box::new(U256::from(48)),
                }
                .into(),
            ))
        );
    }

    #[test]
    fn test_validate_sys_tx() {
        // mark the tx as a system transaction.
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.deposit.source_hash = B256::from([1u8; 32]);
                tx.deposit.is_system_transaction = true;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        assert_eq!(
            handler.validate_env(&mut evm),
            Err(EVMError::Transaction(
                OpTransactionError::DepositSystemTxPostRegolith
            ))
        );

        evm.ctx().modify_cfg(|cfg| cfg.spec = OpSpecId::BEDROCK);

        // Pre-regolith system transactions should be allowed.
        assert!(handler.validate_env(&mut evm).is_ok());
    }

    #[test]
    fn test_validate_deposit_tx() {
        // Set source hash.
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.deposit.source_hash = B256::from([1u8; 32]);
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        assert!(handler.validate_env(&mut evm).is_ok());
    }

    #[test]
    fn test_validate_tx_against_state_deposit_tx() {
        // Set source hash.
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.deposit.source_hash = B256::from([1u8; 32]);
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Nonce and balance checks should be skipped for deposit transactions.
        assert!(handler.validate_env(&mut evm).is_ok());
    }

    #[test]
    fn test_halted_deposit_tx_post_regolith() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                // Set up as deposit transaction by having a deposit with source_hash
                tx.deposit.source_hash = B256::from([1u8; 32]);
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let mut handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        assert_eq!(
            handler.execution_result(
                &mut evm,
                FrameResult::Call(CallOutcome {
                    result: InterpreterResult {
                        result: InstructionResult::OutOfGas,
                        output: Default::default(),
                        gas: Default::default(),
                    },
                    memory_offset: Default::default(),
                })
            ),
            Err(EVMError::Transaction(
                OpTransactionError::HaltedDepositPostRegolith
            ))
        )
    }

    #[test]
    fn test_tx_zero_value_touch_caller() {
        let ctx = Context::op();

        let mut evm = ctx.build_op();

        assert!(!evm
            .0
            .ctx
            .journal_mut()
            .load_account(Address::ZERO)
            .unwrap()
            .is_touched());

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        assert!(evm
            .0
            .ctx
            .journal_mut()
            .load_account(Address::ZERO)
            .unwrap()
            .is_touched());
    }

    #[rstest]
    #[case::deposit(true)]
    #[case::dyn_fee(false)]
    fn test_operator_fee_refund(#[case] is_deposit: bool) {
        const SENDER: Address = Address::ZERO;
        const GAS_PRICE: u128 = 0xFF;
        const OP_FEE_MOCK_PARAM: u128 = 0xFFFF;

        let ctx = Context::op()
            .with_tx(
                OpTransaction::builder()
                    .base(
                        TxEnv::builder()
                            .gas_price(GAS_PRICE)
                            .gas_priority_fee(None)
                            .caller(SENDER),
                    )
                    .enveloped_tx(if is_deposit {
                        None
                    } else {
                        Some(bytes!("FACADE"))
                    })
                    .source_hash(if is_deposit {
                        B256::from([1u8; 32])
                    } else {
                        B256::ZERO
                    })
                    .build_fill(),
            )
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ARSIA);

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Set the operator fee scalar & constant to non-zero values in the L1 block info.
        evm.ctx().chain.operator_fee_scalar = Some(U256::from(OP_FEE_MOCK_PARAM));
        evm.ctx().chain.operator_fee_constant = Some(U256::from(OP_FEE_MOCK_PARAM));

        let mut gas = Gas::new(100);
        gas.set_spent(10);
        let mut exec_result = FrameResult::Call(CallOutcome::new(
            InterpreterResult {
                result: InstructionResult::Return,
                output: Default::default(),
                gas,
            },
            0..0,
        ));

        // Reimburse the caller for the unspent portion of the fees.
        handler
            .reimburse_caller(&mut evm, &mut exec_result)
            .unwrap();

        // Compute the expected refund amount. If the transaction is a deposit, the operator fee refund never
        // applies. If the transaction is not a deposit, the operator fee refund is added to the refund amount.
        let mut expected_refund =
            U256::from(GAS_PRICE * (gas.remaining() + gas.refunded() as u64) as u128);
        let op_fee_refund = evm.ctx().chain().operator_fee_refund(&gas, OpSpecId::ARSIA);
        assert!(op_fee_refund > U256::ZERO);

        if !is_deposit {
            expected_refund += op_fee_refund;
        }

        // Check that the caller was reimbursed the correct amount of ETH.
        let account = evm.ctx().journal_mut().load_account(SENDER).unwrap();
        assert_eq!(account.info.balance, expected_refund);
    }

    #[test]
    fn test_tx_low_balance_nonce_unchanged() {
        let ctx = Context::op().with_tx(
            OpTransaction::builder()
                .base(TxEnv::builder().value(U256::from(1000)))
                .build_fill(),
        );

        let mut evm = ctx.build_op();

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        let result = handler.validate_against_state_and_deduct_caller(&mut evm);

        assert!(matches!(
            result.err().unwrap(),
            EVMError::Transaction(OpTransactionError::Base(
                InvalidTransaction::LackOfFundForMaxFee { .. }
            ))
        ));
        assert_eq!(
            evm.0
                .ctx
                .journal_mut()
                .load_account(Address::ZERO)
                .unwrap()
                .info
                .nonce,
            0
        );
    }

    #[test]
    fn test_validate_missing_enveloped_tx() {
        use crate::transaction::deposit::DepositTransactionParts;

        // Create a non-deposit transaction without enveloped_tx
        let ctx = Context::op().with_tx(OpTransaction {
            base: TxEnv::builder().build_fill(),
            enveloped_tx: None, // Missing enveloped_tx for non-deposit transaction
            deposit: DepositTransactionParts::default(), // No source_hash means non-deposit
        });

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        assert_eq!(
            handler.validate_env(&mut evm),
            Err(EVMError::Transaction(
                OpTransactionError::MissingEnvelopedTx
            ))
        );
    }

    #[test]
    fn test_halted_deposit_bvm_eth_mint_only() {
        let caller = Address::from([0x01; 20]);
        let mint_amount = 100u64;
        let transfer_amount = 50u64;

        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.caller = caller;
                tx.deposit.source_hash = B256::from([1u8; 32]);
                tx.deposit.mint = Some(mint_amount.into());
                tx.deposit.eth_value = Some(mint_amount.into());
                tx.deposit.eth_tx_value = Some(transfer_amount.into());
                tx.base.value = U256::from(transfer_amount);
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Simulate the error that happens when a deposit transaction halts.
        let error = EVMError::Transaction(OpTransactionError::HaltedDepositPostRegolith);

        // catch_error handles the cleanup and BVM_ETH minting logic for failed deposits.
        let result = handler.catch_error(&mut evm, error).unwrap();

        match result {
            ExecutionResult::Halt { reason, .. } => {
                assert_eq!(reason, OpHaltReason::FailedDeposit);
            }
            _ => panic!("Expected Halt result"),
        }

        // Verify BVM_ETH was minted.
        // We calculate the storage slot for the caller's balance in the BvmEth contract
        let slot = BvmEth::get_balance_slot(caller);
        let balance = evm
            .ctx()
            .journal_mut()
            .sload(BvmEth::ADDRESS, slot)
            .unwrap()
            .data;

        assert_eq!(balance, U256::from(mint_amount));
    }

    #[test]
    fn test_mantle_block_89689906_token_ratio_update() {
        // rig
        //
        // Test case based on Mantle block 89689906
        // <https://mantlescan.xyz/block/89689906>
        //
        // This block contains 6 transactions:
        // 1. Deposit transaction - should not multiply by token ratio
        //    <https://mantlescan.xyz/tx/0x9334f65ee1716e7f374b3a499ff82899a0f0e168fd61d2c316bf0eda5c3ea212>
        // 4. Set token ratio transaction - calls gas oracle contract, resets l2_block
        //    <https://mantlescan.xyz/tx/0x0ca2cfb8a879f0688913c19cf153969edbb9fb587503b12cef5e920d8aa70283>
        // 5. Regular transaction - should reload and use new token ratio
        //    <https://mantlescan.xyz/tx/0xd398169756466a2d1b3fb4c377972d6ecefb9c26ae30ee84aeaa215f2cf77780>
        //
        // The token ratio changed at block 89689906:
        // - Previous block (89689905): 3045 (0xbe5)
        // - After tx4: 3040 (0xbe0)
        //
        // All transactions use ISTHMUS spec

        const BLOCK_NUM: U256 = uint!(89689906_U256);
        
        // Values extracted from block 89689906
        // L1_BLOCK_CONTRACT storage slots and GAS_ORACLE_CONTRACT storage slots
        // Token ratio at block start (from previous block 89689905): 3045 (0xbe5)
        const INITIAL_TOKEN_RATIO: u64 = 3045;
        // Token ratio after tx4 (set token ratio transaction): 3040 (0xbe0)
        const NEW_TOKEN_RATIO: u64 = 3040;
        const L1_BASE_FEE: U256 = uint!(54869431_U256);
        const L1_FEE_OVERHEAD: U256 = uint!(188_U256);
        const L1_BASE_FEE_SCALAR: u64 = 10000;

        let mut db = InMemoryDB::default();
        
        // Set up L1 block contract storage
        let l1_block_contract = db.load_account(L1_BLOCK_CONTRACT).unwrap();
        l1_block_contract
            .storage
            .insert(L1_BASE_FEE_SLOT, L1_BASE_FEE);
        l1_block_contract
            .storage
            .insert(L1_OVERHEAD_SLOT, L1_FEE_OVERHEAD);
        l1_block_contract
            .storage
            .insert(L1_SCALAR_SLOT, U256::from(L1_BASE_FEE_SCALAR));

        // Set initial token ratio in gas oracle contract
        let gas_oracle_contract = db.load_account(GAS_ORACLE_CONTRACT).unwrap();
        gas_oracle_contract
            .storage
            .insert(TOKEN_RATIO_SLOT, U256::from(INITIAL_TOKEN_RATIO));

        // Set up caller accounts with sufficient balance
        // Addresses from block 89689906 transactions
        let deposit_sender = Address::from_str("0xdeaddeaddeaddeaddeaddeaddeaddeaddead0001").unwrap();
        let token_ratio_sender = Address::from_str("0xe8bf1c5750354694ed75f97b549cf570fa516725").unwrap();
        let regular_tx_sender = Address::from_str("0xd99ac0681b904991169a4f398b9043781adbe0c3").unwrap();

        db.insert_account_info(
            deposit_sender,
            AccountInfo {
                balance: U256::from(100_000_000),
                ..Default::default()
            },
        );
        db.insert_account_info(
            token_ratio_sender,
            AccountInfo {
                balance: U256::from(100_000_000),
                ..Default::default()
            },
        );
        db.insert_account_info(
            regular_tx_sender,
            AccountInfo {
                balance: U256::from(100_000_000),
                ..Default::default()
            },
        );

        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                token_ratio: U256::from(INITIAL_TOKEN_RATIO),
                l1_base_fee: L1_BASE_FEE,
                l1_fee_overhead: Some(L1_FEE_OVERHEAD),
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS);

        let handler =
            OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<EthInterpreter>>::new();

        // Transaction 1: Deposit transaction
        // Transaction hash: 0x9334f65ee1716e7f374b3a499ff82899a0f0e168fd61d2c316bf0eda5c3ea212
        const DEPOSIT_TX_RLP: &[u8] = &hex!("015d8eb9000000000000000000000000000000000000000000000000000000000170a5b400000000000000000000000000000000000000000000000000000000695a10370000000000000000000000000000000000000000000000000000000003453db78236bc1a30d9f0a2c237de8235d815dab3258124d87a238d6559ddbb208f012a00000000000000000000000000000000000000000000000000000000000000010000000000000000000000002f40d796917ffb642bd2e2bdd2c762a5e40fd74900000000000000000000000000000000000000000000000000000000000000bc0000000000000000000000000000000000000000000000000000000000002710");
        let deposit_source_hash = B256::from_str("0x35e1ede43236480e80925f8a1f4d91c209ab586d058cf7af2b48c754915282c7").unwrap();
        let mut evm1 = ctx
            .clone()
            .with_tx(
                OpTransaction::builder()
                    .base(TxEnv::builder().gas_limit(1_000_000))
                    .source_hash(deposit_source_hash)
                    .enveloped_tx(Some(Bytes::from(DEPOSIT_TX_RLP)))
                    .build_fill(),
            )
            .build_op();

        // Deposit transactions don't multiply by token ratio
        let initial_gas1 = handler.validate_initial_tx_gas(&mut evm1).unwrap();
        // Deposit tx should not multiply by token ratio
        assert_eq!(
            initial_gas1.initial_gas,
            21000,
            "Deposit transaction should not multiply by token ratio"
        );

        // Verify token ratio is still initial value
        assert_eq!(
            evm1.ctx().chain().token_ratio,
            U256::from(INITIAL_TOKEN_RATIO),
            "Token ratio should remain unchanged after deposit transaction"
        );

        // Transaction 2: Set token ratio transaction (calls gas oracle contract)
        // Update token ratio in storage - this simulates the actual transaction updating the storage
        let gas_oracle_contract = evm1
            .ctx()
            .journal_mut()
            .db_mut()
            .load_account(GAS_ORACLE_CONTRACT)
            .unwrap();
        gas_oracle_contract
            .storage
            .insert(TOKEN_RATIO_SLOT, U256::from(NEW_TOKEN_RATIO));

        // Extract the updated db from evm1 and use it for subsequent transactions
        let updated_db = evm1.ctx().journal().db().clone();
        let ctx_with_updated_db = Context::op()
            .with_db(updated_db)
            .with_chain(L1BlockInfo {
                l2_block: Some(BLOCK_NUM),
                token_ratio: U256::from(INITIAL_TOKEN_RATIO),
                l1_base_fee: L1_BASE_FEE,
                l1_fee_overhead: Some(L1_FEE_OVERHEAD),
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                ..Default::default()
            })
            .with_block(BlockEnv {
                number: BLOCK_NUM,
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::ISTHMUS);

        // Transaction 4: Set token ratio transaction
        // Transaction hash: 0x0ca2cfb8a879f0688913c19cf153969edbb9fb587503b12cef5e920d8aa70283
        // Calls gas oracle contract to set token ratio
        const SET_TOKEN_RATIO_TX_RLP: &[u8] = &hex!("e38e91f90000000000000000000000000000000000000000000000000000000000000be0");
        let mut evm2 = ctx_with_updated_db
            .clone()
            .with_tx(
                OpTransaction::builder()
                    .base(
                        TxEnv::builder()
                            .gas_limit(129_500_000)
                            .caller(token_ratio_sender)
                            .to(GAS_ORACLE_CONTRACT),
                    )
                    .enveloped_tx(Some(Bytes::from(SET_TOKEN_RATIO_TX_RLP)))
                    .build()
                    .unwrap(),
            )
            .build_op();

        // This transaction calls gas oracle contract, which should reset l2_block
        // It should use old token ratio (INITIAL_TOKEN_RATIO) because the chain still has the old value cached
        let initial_gas2 = handler.validate_initial_tx_gas(&mut evm2).unwrap();
        
        // Calculate expected gas with old token ratio (3045)
        // The base gas (before multiplying by token_ratio) should be consistent
        let base_gas_before = initial_gas2.initial_gas / INITIAL_TOKEN_RATIO;
        let expected_gas_before = base_gas_before * INITIAL_TOKEN_RATIO;
        
        // Should use old token ratio (3045) before the reset
        assert_eq!(
            initial_gas2.initial_gas,
            expected_gas_before,
            "Transaction 4 should use old token ratio (3045) before reset. Gas: {}, Base: {}, Token ratio: {}",
            initial_gas2.initial_gas,
            base_gas_before,
            INITIAL_TOKEN_RATIO
        );
        
        // Verify the gas calculation uses INITIAL_TOKEN_RATIO
        assert_eq!(
            initial_gas2.initial_gas % INITIAL_TOKEN_RATIO,
            0,
            "Gas should be divisible by INITIAL_TOKEN_RATIO"
        );

        // Verify that l2_block was reset
        assert_eq!(
            evm2.ctx().chain().l2_block,
            None,
            "l2_block should be reset after calling gas oracle contract"
        );

        // Verify token ratio is still old value (not reloaded yet)
        assert_eq!(
            evm2.ctx().chain().token_ratio,
            U256::from(INITIAL_TOKEN_RATIO),
            "Token ratio should still be old value (3045) before reload"
        );

        // Transaction 5: Regular transaction - should use new token ratio
        // Transaction hash: 0xd398169756466a2d1b3fb4c377972d6ecefb9c26ae30ee84aeaa215f2cf77780
        // Create a new context with l2_block reset to None so it will reload from database
        const REGULAR_TX_RLP: &[u8] = &hex!("38899935000000000000000000000000428ef0f8209be073ae7ccf4c2586942e2d21820f000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000412d3b8b4ed1764ae8529350b87648aca1fbf2f4c8e22fcaed899a050292caf9ba4e68c61e5baa496e7ab64b3fbca12a65584142a17228ac9e5457055d4412fa051b00000000000000000000000000000000000000000000000000000000000000");
        let regular_tx_to = Address::from_str("0x5523985926aa12ba58dc5ad00ddca99678d7227e").unwrap();
        let mut evm3 = ctx_with_updated_db
            .with_chain(L1BlockInfo {
                l2_block: None, // Reset to None so it will reload from database
                token_ratio: U256::from(INITIAL_TOKEN_RATIO), // This will be overwritten when reloaded
                l1_base_fee: L1_BASE_FEE,
                l1_fee_overhead: Some(L1_FEE_OVERHEAD),
                l1_base_fee_scalar: U256::from(L1_BASE_FEE_SCALAR),
                ..Default::default()
            })
            .with_tx(
                OpTransaction::builder()
                    .base(
                        TxEnv::builder()
                            .gas_limit(585_000_000)
                            .caller(regular_tx_sender)
                            .to(regular_tx_to),
                    )
                    .enveloped_tx(Some(Bytes::from(REGULAR_TX_RLP)))
                    .build_fill(),
            )
            .build_op();

        // This transaction should reload L1BlockInfo and use new token ratio
        let initial_gas3 = handler.validate_initial_tx_gas(&mut evm3).unwrap();
        
        // Calculate expected gas with new token ratio (3040)
        // The base gas (before multiplying by token_ratio) should be the same as before
        // Only the token_ratio multiplier changes from 3045 to 3040
        let base_gas_after = initial_gas3.initial_gas / NEW_TOKEN_RATIO;
        let expected_gas_after = base_gas_after * NEW_TOKEN_RATIO;
        
        // Should use new token ratio (3040) after reload
        assert_eq!(
            initial_gas3.initial_gas,
            expected_gas_after,
            "Transaction 5 should use new token ratio (3040) after reload. Gas: {}, Base: {}, Token ratio: {}",
            initial_gas3.initial_gas,
            base_gas_after,
            NEW_TOKEN_RATIO
        );
        
        // Verify that gas calculation changed due to token ratio change
        // Since token_ratio decreased from 3045 to 3040, gas should decrease
        assert!(
            initial_gas3.initial_gas < initial_gas2.initial_gas,
            "Gas should decrease after token ratio change: {} (with ratio {}) -> {} (with ratio {})",
            initial_gas2.initial_gas,
            INITIAL_TOKEN_RATIO,
            initial_gas3.initial_gas,
            NEW_TOKEN_RATIO
        );
        
        // Verify the base gas calculation and token ratio multiplier
        let base_gas_before = initial_gas2.initial_gas / INITIAL_TOKEN_RATIO;
        let base_gas_after = initial_gas3.initial_gas / NEW_TOKEN_RATIO;
        
        // Calculate the actual difference due to token ratio change
        let gas_diff = initial_gas2.initial_gas - initial_gas3.initial_gas;
        let ratio_diff = INITIAL_TOKEN_RATIO - NEW_TOKEN_RATIO;
        
        // Verify that the gas difference is consistent with token ratio difference
        // If base gas were the same, the difference would be: base_gas * (3045 - 3040) = base_gas * 5
        let expected_diff_if_same_base = base_gas_before * ratio_diff;
        
        // Verify the gas difference matches the expected difference when base gas is the same
        if base_gas_before == base_gas_after {
            assert_eq!(
                gas_diff,
                expected_diff_if_same_base,
                "Gas difference should match token ratio difference when base gas is the same"
            );
        }
        
        // Verify token ratio multiplier is correctly applied
        assert_eq!(
            initial_gas2.initial_gas % INITIAL_TOKEN_RATIO,
            0,
            "Gas should be divisible by INITIAL_TOKEN_RATIO"
        );
        assert_eq!(
            initial_gas3.initial_gas % NEW_TOKEN_RATIO,
            0,
            "Gas should be divisible by NEW_TOKEN_RATIO"
        );

        // Verify that L1BlockInfo was reloaded with new token ratio
        assert_eq!(
            evm3.ctx().chain().token_ratio,
            U256::from(NEW_TOKEN_RATIO),
            "Token ratio should be reloaded from database (changed from 3045 to 3040)"
        );
        assert_eq!(
            evm3.ctx().chain().l2_block,
            Some(BLOCK_NUM),
            "l2_block should be set after reload"
        );

        // Verify that ISTHMUS spec doesn't load ecotone/isthmus/jovian fields
        assert_eq!(
            evm3.ctx().chain().l1_blob_base_fee,
            None,
            "ISTHMUS should not load l1_blob_base_fee"
        );
        assert_eq!(
            evm3.ctx().chain().operator_fee_scalar,
            None,
            "ISTHMUS should not load operator_fee_scalar"
        );
        assert_eq!(
            evm3.ctx().chain().da_footprint_gas_scalar,
            None,
            "ISTHMUS should not load da_footprint_gas_scalar"
        );

        // Verify L1 cost calculation uses the correct token ratio
        // For ISTHMUS spec, L1 cost calculation should use the reloaded token ratio
        let mut chain_info = evm3.ctx().chain().clone();
        chain_info.clear_tx_l1_cost();
        let l1_cost_with_new_ratio = chain_info.calculate_tx_l1_cost(REGULAR_TX_RLP, OpSpecId::ISTHMUS);
        
        // Verify that L1 cost calculation uses NEW_TOKEN_RATIO
        // L1 cost formula for ISTHMUS: (data_gas + overhead) * l1_base_fee * l1_base_fee_scalar * token_ratio / 1_000_000
        let data_gas = chain_info.data_gas(REGULAR_TX_RLP, OpSpecId::ISTHMUS);
        let expected_l1_cost = (data_gas
            .saturating_add(L1_FEE_OVERHEAD))
            .saturating_mul(L1_BASE_FEE)
            .saturating_mul(U256::from(L1_BASE_FEE_SCALAR))
            .saturating_mul(U256::from(NEW_TOKEN_RATIO))
            .wrapping_div(U256::from(1_000_000));
        
        assert_eq!(
            l1_cost_with_new_ratio,
            expected_l1_cost,
            "L1 cost should be calculated with NEW_TOKEN_RATIO (3040)"
        );
    }

}
