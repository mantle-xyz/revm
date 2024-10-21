use crate::{
    mantle_handle_register,
    transaction::{OpTransaction, OpTransactionType, OpTxTrait},
    L1BlockInfo, OpTransactionError, MantleHaltReason, MantleSpecId,
};
use core::marker::PhantomData;
use revm::{
    database_interface::Database,
    handler::register::HandleRegisters,
    wiring::default::{block::BlockEnv, TxEnv},
    wiring::EvmWiring,
    EvmHandler,
};

pub trait MantleContextTrait {
    /// A reference to the cached L1 block info.
    fn l1_block_info(&self) -> Option<&L1BlockInfo>;

    /// A mutable reference to the cached L1 block info.
    fn l1_block_info_mut(&mut self) -> &mut Option<L1BlockInfo>;
}

/// Trait for an Mantle chain spec.
pub trait MantleWiring:
    revm::EvmWiring<
    ChainContext: MantleContextTrait,
    Hardfork = MantleSpecId,
    HaltReason = MantleHaltReason,
    Transaction: OpTxTrait<
        TransactionType = OpTransactionType,
        TransactionError = OpTransactionError,
    >,
>
{
}

impl<EvmWiringT> MantleWiring for EvmWiringT where
    EvmWiringT: revm::EvmWiring<
        ChainContext: MantleContextTrait,
        Hardfork = MantleSpecId,
        HaltReason = MantleHaltReason,
        Transaction: OpTxTrait<
            TransactionType = OpTransactionType,
            TransactionError = OpTransactionError,
        >,
    >
{
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MantleEvmWiring<DB: Database, EXT> {
    _phantom: PhantomData<(DB, EXT)>,
}

impl<DB: Database, EXT> EvmWiring for MantleEvmWiring<DB, EXT> {
    type Block = BlockEnv;
    type Database = DB;
    type ChainContext = Context;
    type ExternalContext = EXT;
    type Hardfork = MantleSpecId;
    type HaltReason = MantleHaltReason;
    type Transaction = OpTransaction<TxEnv>;
}

impl<DB: Database, EXT> revm::EvmWiring for MantleEvmWiring<DB, EXT> {
    fn handler<'evm>(hardfork: Self::Hardfork) -> EvmHandler<'evm, Self>
    where
        DB: Database,
    {
        let mut handler = EvmHandler::mainnet_with_spec(hardfork);

        handler.append_handler_register(HandleRegisters::Plain(mantle_handle_register::<Self>));

        handler
    }
}

/// Context for the Mantle chain.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Context {
    l1_block_info: Option<L1BlockInfo>,
}

impl MantleContextTrait for Context {
    fn l1_block_info(&self) -> Option<&L1BlockInfo> {
        self.l1_block_info.as_ref()
    }

    fn l1_block_info_mut(&mut self) -> &mut Option<L1BlockInfo> {
        &mut self.l1_block_info
    }
}
