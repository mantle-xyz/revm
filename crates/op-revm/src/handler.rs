//!Handler related to Optimism chain
use crate::{
    api::exec::OpContextTr,
    constants::{BASE_FEE_RECIPIENT, GAS_ORACLE_CONTRACT},
    transaction::{deposit::DEPOSIT_TRANSACTION_TYPE, OpTransactionError, OpTxTr},
    BvmEth, L1BlockInfo, OpHaltReason, OpSpecId,
};
use revm::{
    context::{result::InvalidTransaction, LocalContextTr},
    context_interface::{
        result::{EVMError, ExecutionResult, FromStringError, ResultAndState},
        Block, Cfg, ContextTr, JournalTr, Transaction,
    },
    handler::{
        handler::EvmTrError, pre_execution::validate_account_nonce_and_code, EvmTr, Frame,
        FrameResult, Handler, ItemOrResult, MainnetHandler,
    },
    inspector::{Inspector, InspectorEvmTr, InspectorFrame, InspectorHandler},
    interpreter::{interpreter::EthInterpreter, FrameInput, Gas, InitialAndFloorGas},
    primitives::{hardfork::SpecId, Address, HashMap, TxKind, U256},
    state::Account,
    Database,
};
use std::boxed::Box;
use crate::fee::FeeModel;

pub struct OpHandler<EVM, ERROR, FRAME> {
    pub mainnet: MainnetHandler<EVM, ERROR, FRAME>,
    pub _phantom: core::marker::PhantomData<(EVM, ERROR, FRAME)>,
    pub fee_model: FeeModel,
}

impl<EVM, ERROR, FRAME> OpHandler<EVM, ERROR, FRAME> {
    pub fn new() -> Self {
        Self {
            mainnet: MainnetHandler::default(),
            _phantom: core::marker::PhantomData,
            fee_model: FeeModel::default(),
        }
    }
}

impl<EVM, ERROR, FRAME> Default for OpHandler<EVM, ERROR, FRAME> {
    fn default() -> Self {
        Self::new()
    }
}

pub trait IsTxError {
    fn is_tx_error(&self) -> bool;
}

impl<DB, TX> IsTxError for EVMError<DB, TX> {
    fn is_tx_error(&self) -> bool {
        matches!(self, EVMError::Transaction(_))
    }
}

impl<EVM, ERROR, FRAME> Handler for OpHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: OpContextTr>,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
    // TODO `FrameResult` should be a generic trait.
    // TODO `FrameInit` should be a generic.
    FRAME: Frame<Evm = EVM, Error = ERROR, FrameResult = FrameResult, FrameInit = FrameInput>,
{
    type Evm = EVM;
    type Error = ERROR;
    type Frame = FRAME;
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
        Ok(())
        // [TODO] temporarily remove for test
        // self.mainnet.validate_env(evm)
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
     *
     * The token ratio adjustment is essential for properly accounting for the price difference
     * between ETH and MNT.
     */
    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<InitialAndFloorGas, Self::Error> {
        let mut initial_gas = self.mainnet.validate_initial_tx_gas(evm)?;

        let context = evm.ctx();
        let block_number = context.block().number();
        let spec = context.cfg().spec();
        if context.tx().tx_type() == DEPOSIT_TRANSACTION_TYPE {
            Ok(initial_gas)
        } else {
            let to = match context.tx().kind() {
                TxKind::Call(to) => to,
                TxKind::Create => Address::ZERO,
            };
            // The L1-cost fee is only computed for Optimism non-deposit transactions.
            if context.chain().l2_block != block_number {
                // L1 block info is stored in the context for later use.
                // and it will be reloaded from the database if it is not for the current block or the token ratio is updated.(when the gas oracle is updated)
                *context.chain() = L1BlockInfo::try_fetch(context.db(), block_number, spec)?;
            }

            // Reset the l2_block if the tx is set token ratio, we need reload token ratio from the database in next transaction
            // [TODO] verify selector of the tx
            if to == GAS_ORACLE_CONTRACT {
                context.chain().reset_l2_block();
            }

            // if the tx is not a deposit transaction, we need to multiply the initial gas by the token ratio
            let token_ratio = context.chain().get_token_ratio();
            initial_gas.initial_gas = initial_gas
                .initial_gas
                .checked_mul(token_ratio.try_into().unwrap())
                .ok_or(InvalidTransaction::CallerGasLimitMoreThanBlock)?;

            initial_gas.floor_gas = initial_gas
                .floor_gas
                .checked_mul(token_ratio.try_into().unwrap())
                .ok_or(InvalidTransaction::CallerGasLimitMoreThanBlock)?;
            Ok(initial_gas)
        }
    }

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();

        let basefee = ctx.block().basefee() as u128;
        let blob_price = ctx.block().blob_gasprice().unwrap_or_default();
        let is_deposit = ctx.tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_balance_check_disabled = ctx.cfg().is_balance_check_disabled();
        let is_eip3607_disabled = ctx.cfg().is_eip3607_disabled();
        let is_nonce_check_disabled = ctx.cfg().is_nonce_check_disabled();
        let mint = ctx.tx().mint();

        let additional_cost = U256::ZERO;

        if is_deposit {
            if let Some(eth_value) = ctx.tx().eth_value() {
                BvmEth::mint(ctx, U256::from(eth_value)).map_err(ERROR::from)?;
            }
            if let Some(eth_tx_value) = ctx.tx().eth_tx_value() {
                BvmEth::transfer(ctx, U256::from(eth_tx_value)).map_err(ERROR::from)?;
            }
        }

        let (tx, journal) = ctx.tx_journal();

        let caller_account = journal.load_account_code(tx.caller())?.data;

        // If the transaction is a deposit with a `mint` value, add the mint value
        // in wei to the caller's balance. This should be persisted to the database
        // prior to the rest of execution.
        if is_deposit {
            if let Some(mint) = mint {
                caller_account.info.balance =
                    caller_account.info.balance.saturating_add(U256::from(mint));
            }
            if tx.kind().is_call() {
                caller_account.info.nonce = caller_account.info.nonce.saturating_add(1);
            }
        } else {
            // validates account nonce and code
            validate_account_nonce_and_code(
                &mut caller_account.info,
                tx.nonce(),
                tx.kind().is_call(),
                is_eip3607_disabled,
                is_nonce_check_disabled,
            )?;
        }

        let max_balance_spending = tx.max_balance_spending()?.saturating_add(additional_cost);

        // Check if account has enough balance for `gas_limit * max_fee`` and value transfer.
        // Transfer will be done inside `*_inner` functions.
        if is_balance_check_disabled {
            // Make sure the caller's balance is at least the value of the transaction.
            // this is not consensus critical, and it is used in testing.
            caller_account.info.balance = caller_account.info.balance.max(tx.value());
        } else if !is_deposit && max_balance_spending > caller_account.info.balance {
            // skip max balance check for deposit transactions.
            // this check for deposit was skipped previously in `validate_tx_against_state` function
            return Err(InvalidTransaction::LackOfFundForMaxFee {
                fee: Box::new(max_balance_spending),
                balance: Box::new(caller_account.info.balance),
            }
            .into());
        } else {
            let effective_balance_spending =
                tx.effective_balance_spending(basefee, blob_price).expect(
                    "effective balance is always smaller than max balance so it can't overflow",
                );

            // subtracting max balance spending with value that is going to be deducted later in the call.
            let gas_balance_spending = effective_balance_spending - tx.value();

            // If the transaction is not a deposit transaction, subtract the L1 data fee from the
            // caller's balance directly after minting the requested amount of ETH.
            // Additionally deduct the operator fee from the caller's account.
            //
            // In case of deposit additional cost will be zero.
            let op_gas_balance_spending = gas_balance_spending.saturating_add(additional_cost);

            caller_account.info.balance = caller_account
                .info
                .balance
                .saturating_sub(op_gas_balance_spending);
        }

        // Touch account so we know it is changed.
        caller_account.mark_touch();
        Ok(())
    }

    fn last_frame_result(
        &mut self,
        evm: &mut Self::Evm,
        frame_result: &mut <Self::Frame as Frame>::FrameResult,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let tx_gas_limit = tx.gas_limit();
        let is_regolith = ctx.cfg().spec().is_enabled_in(OpSpecId::REGOLITH);
        let is_limb = ctx.cfg().spec().is_enabled_in(OpSpecId::LIMB);

        let instruction_result = frame_result.interpreter_result().result;
        let gas = frame_result.gas_mut();
        let remaining = gas.remaining();
        let mut refunded = gas.refunded();
        println!("before, gas limit {}", tx_gas_limit);
        println!("before, gas remaining {}", remaining);
        println!("before, refunded {}", refunded);
        
        
        if is_limb {
            let gas_used = tx_gas_limit - remaining;
            println!("before, gas used {}", gas_used);
            println!("before, l1cost {}", self.fee_model.rollup_cost);
            println!("before, fee {}", self.fee_model.operator_cost);
            println!("before, total cost {}", self.fee_model.total_cost());
            let l2_gas_used = gas_used - self.fee_model.total_cost();
            let spec = ctx.cfg().spec();
            let mut operator_fee_refunded = ctx.chain().operator_fee_refund(tx_gas_limit,l2_gas_used, spec);
            let basefee = ctx.block().basefee() as u128;

            let effective_gas_price = ctx.tx().effective_gas_price(basefee);

            if effective_gas_price > 0 {
                operator_fee_refunded = operator_fee_refunded.wrapping_div(U256::from(effective_gas_price));
                refunded = U256::from(refunded).saturating_add(operator_fee_refunded).saturating_to();
            }
        }
        
        
        
        
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
                // [TODO] Maybe we should move the [refund] logic to this function.
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
        exec_result: &mut <Self::Frame as Frame>::FrameResult,
    ) -> Result<(), Self::Error> {
        let context = evm.ctx();
        if context.tx().tx_type() != DEPOSIT_TRANSACTION_TYPE {
            self.mainnet.reimburse_caller(evm, exec_result)?;
        }

        Ok(())
    }

    fn execution(
        &mut self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &InitialAndFloorGas,
    ) -> Result<FrameResult, Self::Error> {
        self.fee_model = Default::default();
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let gas_limit = evm.ctx().tx().gas_limit();
        let mut gas_remaining = gas_limit - init_and_floor_gas.initial_gas;
        // l1cost = l1cost / effective_gas_price
        // gas_limit = gas_limit - l1cost
        // gas_limit = gas_limit / token_ratio
        if !is_deposit {
            let ctx = evm.ctx();
            let spec = ctx.cfg().spec();

            let token_ratio = ctx.chain().get_token_ratio();

            let enveloped_tx = ctx
                .tx()
                .enveloped_tx()
                .expect("all not deposit tx have enveloped tx")
                .clone();
            let basefee = ctx.block().basefee() as u128;
            let mut tx_l1_cost = ctx.chain().calculate_tx_l1_cost(&enveloped_tx, spec);
            let effective_gas_price = ctx.tx().effective_gas_price(basefee);

            if effective_gas_price > 0 {
                tx_l1_cost = tx_l1_cost.wrapping_div(U256::from(effective_gas_price));
            }

            if tx_l1_cost.gt(&U256::from(gas_remaining)) {
                return Err(ERROR::from(OpTransactionError::Base(
                    InvalidTransaction::CallGasCostMoreThanGasLimit {
                        initial_gas: init_and_floor_gas.initial_gas,
                        gas_limit,
                    },
                )));
            }

            gas_remaining = gas_remaining.wrapping_sub(tx_l1_cost.try_into().unwrap());
            self.fee_model.rollup_cost = tx_l1_cost.try_into().unwrap();
            // compute operator fee
            if spec.is_enabled_in(OpSpecId::LIMB) {
                let tx_gas_limit = U256::from(ctx.tx().gas_limit());
                let mut operator_fee_charge = ctx.chain().operator_fee_charge(&enveloped_tx,tx_gas_limit);
                if effective_gas_price > 0 {
                    operator_fee_charge = operator_fee_charge.wrapping_div(U256::from(effective_gas_price));
                }
                if operator_fee_charge.gt(&U256::from(gas_remaining)) {
                    return Err(ERROR::from(OpTransactionError::Base(
                        InvalidTransaction::CallGasCostMoreThanGasLimit {
                            initial_gas: init_and_floor_gas.initial_gas,
                            gas_limit,
                        },
                    )));
                }
                gas_remaining = gas_remaining.wrapping_sub(operator_fee_charge.try_into().unwrap());
                self.fee_model.operator_cost = operator_fee_charge.try_into().unwrap();
            }
            gas_remaining = gas_remaining.wrapping_div(token_ratio.try_into().unwrap());
        }

        let first_frame_input = self.first_frame_input(evm, gas_remaining)?;
        let first_frame = self.first_frame_init(evm, first_frame_input)?;
        let mut frame_result = match first_frame {
            ItemOrResult::Item(frame) => self.run_exec_loop(evm, frame)?,
            ItemOrResult::Result(result) => result,
        };

        self.last_frame_result(evm, &mut frame_result)?;
        Ok(frame_result)
    }

    fn refund(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <Self::Frame as Frame>::FrameResult,
        eip7702_refund: i64,
    ) {
        exec_result.gas_mut().record_refund(eip7702_refund);
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let is_deposit = tx.tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let is_system = tx.is_system_transaction();
        let gas = exec_result.gas_mut();

        let is_eth_mint = tx.eth_value().is_some();
        if is_eth_mint && !tx.input().is_empty() {
            gas.set_remaining(gas.remaining().saturating_sub(4500));
        }

        let limit = gas.limit();
        let token_ratio_u64: u64 = ctx.chain().get_token_ratio().try_into().unwrap();

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

        let is_regolith = ctx.cfg().spec().is_enabled_in(OpSpecId::REGOLITH);
        // Prior to Regolith, deposit transactions did not receive gas refunds.
        let is_gas_refund_disabled = is_deposit && !is_regolith;
        if !is_gas_refund_disabled {
            gas.set_final_refund(
                ctx.cfg()
                    .spec()
                    .into_eth_spec()
                    .is_enabled_in(SpecId::LONDON),
            );
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

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <Self::Frame as Frame>::FrameResult,
    ) -> Result<(), Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;

        // Transfer fee to coinbase/beneficiary.
        if !is_deposit {
            self.mainnet.reward_beneficiary(evm, exec_result)?;
            let basefee = evm.ctx().block().basefee() as u128;

            // Send the base fee of the transaction to the Base Fee Vault.
            let mut base_fee_vault_account =
                evm.ctx().journal().load_account(BASE_FEE_RECIPIENT)?;
            base_fee_vault_account.mark_touch();
            base_fee_vault_account.info.balance += U256::from(basefee.saturating_mul(
                (exec_result.gas().spent() - exec_result.gas().refunded() as u64) as u128,
            ));
        }
        Ok(())
    }

    fn output(
        &self,
        evm: &mut Self::Evm,
        result: <Self::Frame as Frame>::FrameResult,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let result = self.mainnet.output(evm, result)?;
        let result = result.map_haltreason(OpHaltReason::Base);
        if result.result.is_halt() {
            // Post-regolith, if the transaction is a deposit transaction and it halts,
            // we bubble up to the global return handler. The mint value will be persisted
            // and the caller nonce will be incremented there.
            let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
            if is_deposit && evm.ctx().cfg().spec().is_enabled_in(OpSpecId::REGOLITH) {
                return Err(ERROR::from(OpTransactionError::HaltedDepositPostRegolith));
            }
        }
        evm.ctx().chain().clear_tx_l1_cost();
        Ok(result)
    }

    fn catch_error(
        &self,
        evm: &mut Self::Evm,
        error: Self::Error,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let output = if error.is_tx_error() && is_deposit {
            let ctx = evm.ctx();
            let spec = ctx.cfg().spec();
            let tx = ctx.tx();
            let caller = tx.caller();
            let mint = tx.mint();
            let is_system_tx = tx.is_system_transaction();
            let gas_limit = tx.gas_limit();
            // If the transaction is a deposit transaction and it failed
            // for any reason, the caller nonce must be bumped, and the
            // gas reported must be altered depending on the Hardfork. This is
            // also returned as a special Halt variant so that consumers can more
            // easily distinguish between a failed deposit and a failed
            // normal transaction.

            // Increment sender nonce and account balance for the mint amount. Deposits
            // always persist the mint amount, even if the transaction fails.
            let account = {
                let mut acc = Account::from(
                    evm.ctx()
                        .db()
                        .basic(caller)
                        .unwrap_or_default()
                        .unwrap_or_default(),
                );
                acc.info.nonce = acc.info.nonce.saturating_add(1);
                acc.info.balance = acc
                    .info
                    .balance
                    .saturating_add(U256::from(mint.unwrap_or_default()));
                acc.mark_touch();
                acc
            };
            let state = HashMap::from_iter([(caller, account)]);

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
            Ok(ResultAndState {
                result: ExecutionResult::Halt {
                    reason: OpHaltReason::FailedDeposit,
                    gas_used,
                },
                state,
            })
        } else {
            Err(error)
        };
        // do the cleanup
        evm.ctx().chain().clear_tx_l1_cost();
        evm.ctx().journal().clear();
        evm.ctx().local().clear();

        output
    }
}

impl<EVM, ERROR, FRAME> InspectorHandler for OpHandler<EVM, ERROR, FRAME>
where
    EVM: InspectorEvmTr<
        Context: OpContextTr,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM> + From<OpTransactionError> + FromStringError + IsTxError,
    // TODO `FrameResult` should be a generic trait.
    // TODO `FrameInit` should be a generic.
    FRAME: InspectorFrame<
        Evm = EVM,
        Error = ERROR,
        FrameResult = FrameResult,
        FrameInit = FrameInput,
        IT = EthInterpreter,
    >,
{
    type IT = EthInterpreter;

    fn inspect_execution(
        &mut self,
        evm: &mut Self::Evm,
        init_and_floor_gas: &InitialAndFloorGas,
    ) -> Result<FrameResult, Self::Error> {
        self.fee_model = Default::default();
        let is_deposit = evm.ctx().tx().tx_type() == DEPOSIT_TRANSACTION_TYPE;
        let gas_limit = evm.ctx().tx().gas_limit();
        let mut gas_remaining = gas_limit - init_and_floor_gas.initial_gas;
        // l1cost = l1cost / effective_gas_price
        // gas_limit = gas_limit - l1cost
        // gas_limit = gas_limit / token_ratio
        if !is_deposit {
            let ctx = evm.ctx();
            let spec = ctx.cfg().spec();

            let token_ratio = ctx.chain().get_token_ratio();

            let enveloped_tx = ctx
                .tx()
                .enveloped_tx()
                .expect("all not deposit tx have enveloped tx")
                .clone();
            let basefee = ctx.block().basefee() as u128;
            let mut tx_l1_cost = ctx.chain().calculate_tx_l1_cost(&enveloped_tx, spec);
            let effective_gas_price = ctx.tx().effective_gas_price(basefee);

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

            gas_remaining = gas_remaining.wrapping_sub(tx_l1_cost.try_into().unwrap());
            self.fee_model.rollup_cost = tx_l1_cost.try_into().unwrap();
            //compute operator fee
            if spec.is_enabled_in(OpSpecId::LIMB) {
                let tx_gas_limit = U256::from(ctx.tx().gas_limit());
                let mut operator_fee_charge = ctx.chain().operator_fee_charge(&enveloped_tx,tx_gas_limit);
                if effective_gas_price > 0 {
                    operator_fee_charge = operator_fee_charge.wrapping_div(U256::from(effective_gas_price));
                }
                if operator_fee_charge.gt(&U256::from(gas_remaining)) {
                    return Err(ERROR::from(OpTransactionError::Base(
                        InvalidTransaction::CallGasCostMoreThanGasLimit {
                            initial_gas: init_and_floor_gas.initial_gas,
                            gas_limit,
                        },
                    )));
                }
                gas_remaining = gas_remaining.wrapping_sub(operator_fee_charge.try_into().unwrap());
                self.fee_model.operator_cost = operator_fee_charge.try_into().unwrap();
            }
            gas_remaining = gas_remaining.wrapping_div(token_ratio.try_into().unwrap());
        }

        // Create first frame action
        let first_frame_input = self.first_frame_input(evm, gas_limit)?;
        let first_frame = self.inspect_first_frame_init(evm, first_frame_input)?;

        let mut frame_result = match first_frame {
            ItemOrResult::Item(frame) => self.inspect_run_exec_loop(evm, frame)?,
            ItemOrResult::Result(result) => result,
        };

        self.last_frame_result(evm, &mut frame_result)?;
        Ok(frame_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::default_ctx::OpContext, DefaultOp, OpBuilder};
    use revm::{
        context::Context,
        database::InMemoryDB,
        database_interface::EmptyDB,
        handler::EthFrame,
        interpreter::{CallOutcome, InstructionResult, InterpreterResult},
        primitives::{bytes, Address, Bytes, B256},
        state::AccountInfo,
    };

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

        let mut handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        handler
            .last_frame_result(&mut evm, &mut exec_result)
            .unwrap();
        handler.refund(&mut evm, &mut exec_result, 0);
        *exec_result.gas()
    }

    #[test]
    fn test_revert_gas() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.gas_limit = 100;
                tx.enveloped_tx = None;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::BEDROCK);

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.gas_limit = 100;
                tx.deposit.source_hash = B256::ZERO;
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_with_refund() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.gas_limit = 100;
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.deposit.source_hash = B256::ZERO;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut ret_gas = Gas::new(90);
        ret_gas.record_refund(20);

        let gas = call_last_frame_return(ctx.clone(), InstructionResult::Stop, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0); // min(20, 10/5)

        let gas = call_last_frame_return(ctx, InstructionResult::Revert, ret_gas);
        assert_eq!(gas.remaining(), 90);
        assert_eq!(gas.spent(), 10);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_deposit_tx() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.base.gas_limit = 100;
                tx.deposit.source_hash = B256::ZERO;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::BEDROCK);
        let gas = call_last_frame_return(ctx, InstructionResult::Stop, Gas::new(90));
        assert_eq!(gas.remaining(), 0);
        assert_eq!(gas.spent(), 100);
        assert_eq!(gas.refunded(), 0);
    }

    #[test]
    fn test_consume_gas_sys_deposit_tx() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.base.gas_limit = 100;
                tx.deposit.source_hash = B256::ZERO;
                tx.deposit.is_system_transaction = true;
            })
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
            tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
            tx.deposit.source_hash = B256::ZERO;
            tx.deposit.mint = Some(10);
        });

        let mut evm = ctx.build_op();

        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1010));
    }

    #[test]
    fn test_remove_l1_cost_non_deposit() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1000),
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH)
            .modify_tx_chained(|tx| {
                tx.base.gas_limit = 100;
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.deposit.mint = Some(10);
                tx.enveloped_tx = Some(bytes!("FACADE"));
                tx.deposit.source_hash = B256::ZERO;
            });

        let mut evm = ctx.build_op();

        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1010));
    }

    #[test]
    fn test_remove_l1_cost() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1049),
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH)
            .modify_tx_chained(|tx| {
                tx.base.gas_limit = 100;
                tx.deposit.source_hash = B256::ZERO;
                tx.enveloped_tx = Some(bytes!("FACADE"));
            });

        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        // l1block cost is 1048 fee.
        handler
            .validate_against_state_and_deduct_caller(&mut evm)
            .unwrap();

        // Check the account balance is updated.
        let account = evm.ctx().journal().load_account(caller).unwrap();
        assert_eq!(account.info.balance, U256::from(1049));
    }

    #[test]
    fn test_remove_l1_cost_full_gas() {
        let caller = Address::ZERO;
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(48),
                ..Default::default()
            },
        );
        let ctx = Context::op()
            .with_db(db)
            .with_chain(L1BlockInfo {
                l1_base_fee: U256::from(1_000),
                l1_fee_overhead: Some(U256::from(1_000)),
                l1_base_fee_scalar: U256::from(1_000),
                ..Default::default()
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH)
            .modify_tx_chained(|tx| {
                tx.enveloped_tx = Some(bytes!("FACADE"));
            });

        // l1block cost is 1048 fee.
        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        assert_eq!(
            handler.validate_against_state_and_deduct_caller(&mut evm),
            Ok(())
        );
    }

    #[test]
    fn test_validate_sys_tx() {
        // mark the tx as a system transaction.
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.deposit.is_system_transaction = true;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

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
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.deposit.source_hash = B256::ZERO;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        assert!(handler.validate_env(&mut evm).is_ok());
    }

    #[test]
    fn test_validate_tx_against_state_deposit_tx() {
        // Set source hash.
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
                tx.deposit.source_hash = B256::ZERO;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        // Nonce and balance checks should be skipped for deposit transactions.
        assert!(handler.validate_env(&mut evm).is_ok());
    }

    #[test]
    fn test_halted_deposit_tx_post_regolith() {
        let ctx = Context::op()
            .modify_tx_chained(|tx| {
                tx.base.tx_type = DEPOSIT_TRANSACTION_TYPE;
            })
            .modify_cfg_chained(|cfg| cfg.spec = OpSpecId::REGOLITH);

        let mut evm = ctx.build_op();
        let handler = OpHandler::<_, EVMError<_, OpTransactionError>, EthFrame<_, _, _>>::new();

        assert_eq!(
            handler.output(
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
}
