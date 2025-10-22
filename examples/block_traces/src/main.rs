//! Example that show how to replay a block and trace the execution of each transaction.
//!
//! The EIP3155 trace of each transaction is saved into file `traces/{tx_number}.json`.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_consensus::{transaction::SignerRecoverable, TxEip1559, TxEip2930, TxEip7702, TxLegacy};
use alloy_eips::{BlockId, Decodable2718, Typed2718};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_provider::{network::primitives::BlockTransactions, Provider, ProviderBuilder};
use dotenv::dotenv;
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use op_alloy_network::Optimism;
use op_revm::{
    api::{builder::OpBuilder, default_ctx::DefaultOp},
    spec::OpSpecId,
    transaction::deposit::DepositTransactionParts,
    OpTransaction,
};
use revm::{
    context::tx::TxEnv,
    context_interface::either::Either,
    database::{AlloyDB, CacheDB, StateBuilder},
    database_interface::WrapDatabaseAsync,
    primitives::TxKind,
    Context, ExecuteCommitEvm,
};
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up the HTTP transport which is consumed by the RPC client.
    dotenv().ok();
    let mantle_url = std::env::var("MANTLE_URL").unwrap();
    let chain_id = std::env::var("CHAIN_ID").unwrap().parse()?;
    let rpc_url = mantle_url.parse()?;

    // Create a provider
    let client = ProviderBuilder::<_, _, Optimism>::default().connect_http(rpc_url);

    // Params
    let start_block = 86486915;
    let end_block = 86486917;

    for i in start_block..=end_block {
        println!("Processing block number: {i}");
        process_block(i, chain_id, client.clone()).await?;
    }

    Ok(())
}

async fn process_block(
    block_number: u64,
    chain_id: u64,
    client: impl Provider<Optimism> + Clone,
) -> anyhow::Result<()> {
    // Fetch the transaction-rich block
    let block = client
        .get_block_by_number(block_number.into())
        .await
        .expect("Failed to get parent block")
        .expect("Block not found");

    println!("Fetched block number: {block_number}");
    let previous_block_number = block_number - 1;

    // Use the previous block state as the db with caching
    let prev_id: BlockId = previous_block_number.into();
    // SAFETY: This cannot fail since this is in the top-level tokio runtime

    let state_db = WrapDatabaseAsync::new(AlloyDB::new(client.clone(), prev_id)).unwrap();
    let cache_db: CacheDB<_> = CacheDB::new(state_db);
    let mut state = StateBuilder::new_with_database(cache_db).build();
    let ctx = Context::op()
        .with_db(&mut state)
        .modify_block_chained(|b| {
            b.number = U256::from(block.header.number);
            b.beneficiary = block.header.beneficiary;
            b.timestamp = U256::from(block.header.timestamp);

            b.difficulty = block.header.difficulty;
            b.gas_limit = block.header.gas_limit;
            b.basefee = block.header.base_fee_per_gas.unwrap_or_default();
        })
        .modify_cfg_chained(|c| {
            c.chain_id = chain_id;
            c.spec = OpSpecId::HOLOCENE;
        });

    let mut evm = ctx.build_op();

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    // let console_bar = Arc::new(ProgressBar::new(txs as u64));
    let start = Instant::now();

    // Fill in CfgEnv
    let BlockTransactions::Hashes(transactions) = block.transactions else {
        panic!("Wrong transaction type")
    };

    for tx_hash in transactions.iter() {
        println!("tx_hash: {tx_hash}");
        let raw_tx = client
            .clone()
            .client()
            .request::<&[B256; 1], Bytes>("debug_getRawTransaction", &[*tx_hash])
            .await
            .expect("Block not found");
        let tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref()).unwrap();
        
        let optx = prepare_tx_env(&tx, tx.recover_signer().unwrap(), raw_tx);
        evm.0.modify_tx(|etx| {
            *etx = optx;
        });

        let is_deposit = tx.is_deposit();
        println!("is_deposit: {is_deposit}");
        
        let res = evm.replay_commit();

        if let Err(ref res) = res {
            println!("Got error: {res:?}");
        }

        let expected_gas_used = client
            .clone()
            .get_transaction_receipt(*tx_hash)
            .await
            .unwrap()
            .unwrap()
            .inner
            .gas_used;

        let actual_gas_used = res.unwrap().gas_used();
        println!("Expected gas used: {expected_gas_used}, Actual gas used: {actual_gas_used}");
        if expected_gas_used == actual_gas_used {
            println!("--- passed✅");
        } else {
            println!("--- failed❌");
        }
    }

    let elapsed = start.elapsed();
    println!(
        "Finished block {block_number}. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// Prepare the transaction environment for the given transaction.
pub fn prepare_tx_env(tx: &OpTxEnvelope, caller: Address, encoded: Bytes) -> OpTransaction<TxEnv> {
    let base = match tx {
        OpTxEnvelope::Legacy(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip1559(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip2930(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip7702(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Deposit(tx) => {
            let TxDeposit {
                to,
                value,
                gas_limit,
                input,
                source_hash: _,
                from: _,
                mint: _,
                is_system_transaction: _,
                eth_value: _,
                eth_tx_value: _,
            } = tx.inner();
            TxEnv {
                tx_type: tx.ty(),
                caller,
                gas_limit: *gas_limit,
                kind: *to,
                value: *value,
                data: input.clone(),
                ..Default::default()
            }
        }
    };

    let deposit = if let OpTxEnvelope::Deposit(tx) = tx {
        DepositTransactionParts {
            source_hash: tx.source_hash,
            mint: Some(tx.mint),
            is_system_transaction: tx.is_system_transaction,
            eth_value: Some(tx.eth_value),
            eth_tx_value: tx.eth_tx_value,
        }
    } else {
        Default::default()
    };

    OpTransaction {
        base,
        enveloped_tx: Some(encoded),
        deposit,
    }
}

trait ToTxEnv {
    fn to_tx_env(&self, caller: Address) -> TxEnv;
}

impl ToTxEnv for TxLegacy {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: self.chain_id,
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip1559 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.max_fee_per_gas,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: Some(self.chain_id),
            gas_priority_fee: Some(self.max_priority_fee_per_gas),
            access_list: self.access_list.clone(),
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip2930 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            chain_id: Some(self.chain_id),
            nonce: self.nonce,
            access_list: self.access_list.clone(),
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip7702 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.max_fee_per_gas,
            kind: TxKind::Call(self.to),
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: Some(self.chain_id),
            gas_priority_fee: Some(self.max_priority_fee_per_gas),
            access_list: self.access_list.clone(),
            authorization_list: self
                .authorization_list
                .iter()
                .map(|auth| Either::Left(auth.clone()))
                .collect(),
            ..Default::default()
        }
    }
}
