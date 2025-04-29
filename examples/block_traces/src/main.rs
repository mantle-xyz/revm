//! Optimism-specific constants, types, and helpers.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_consensus::{TxEip1559, TxEip2930, TxEip7702, TxLegacy};
use alloy_eips::{BlockId, Decodable2718, Typed2718};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::{network::primitives::BlockTransactions, Provider, ProviderBuilder};
use dotenv::dotenv;
use indicatif::ProgressBar;
use mantle_revm::{
    api::{builder::OpBuilder, default_ctx::DefaultOp},
    spec::OpSpecId,
    transaction::deposit::DepositTransactionParts,
    OpTransaction,
};
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use op_alloy_network::Optimism;
use revm::{
    context::tx::TxEnv,
    database::{AlloyDB, CacheDB, StateBuilder},
    database_interface::WrapDatabaseAsync,
    inspector::{inspectors::TracerEip3155, InspectEvm},
    primitives::TxKind,
    Context,
};
use std::fs::OpenOptions;
use std::io::BufWriter;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

struct FlushWriter {
    writer: Arc<Mutex<BufWriter<std::fs::File>>>,
}

impl FlushWriter {
    fn new(writer: Arc<Mutex<BufWriter<std::fs::File>>>) -> Self {
        Self { writer }
    }
}

impl Write for FlushWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.lock().unwrap().flush()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up the HTTP transport which is consumed by the RPC client.
    dotenv().ok();
    let mantle_url = std::env::var("MANTLE_URL").unwrap();
    let rpc_url = mantle_url.parse()?;

    // Create a provider
    let client = ProviderBuilder::<_, _, Optimism>::default().on_http(rpc_url);

    // Params
    let chain_id: u64 = 5000;
    let block_number = 78896111;

    // Fetch the transaction-rich block
    let block = client
        .get_block_by_number(block_number.into())
        .await
        .expect("Failed to get parent block")
        .expect("Block not found");

    println!("Fetched block number: {}", block.header.number);
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
            b.number = block.header.number;
            b.beneficiary = block.header.beneficiary;
            b.timestamp = block.header.timestamp;

            b.difficulty = block.header.difficulty;
            b.gas_limit = block.header.gas_limit;
            b.basefee = block.header.base_fee_per_gas.unwrap_or_default();
        })
        .modify_cfg_chained(|c| {
            c.chain_id = chain_id;
            c.spec = OpSpecId::CANYON;
        });

    let write = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("traces/0.json");
    let inner = Arc::new(Mutex::new(BufWriter::new(
        write.expect("Failed to open file"),
    )));
    let writer = FlushWriter::new(Arc::clone(&inner));
    let mut evm = ctx.build_op_with_inspector(TracerEip3155::new(Box::new(writer)));

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    let console_bar = Arc::new(ProgressBar::new(txs as u64));
    let start = Instant::now();

    // Create the traces directory if it doesn't exist
    std::fs::create_dir_all("traces").expect("Failed to create traces directory");

    // Fill in CfgEnv
    let BlockTransactions::Hashes(transactions) = block.transactions else {
        panic!("Wrong transaction type")
    };

    for (i, tx_hash) in transactions.iter().enumerate() {
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

        let tx_number = i;
        let file_name = format!("traces/{}.json", tx_number);
        let write = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_name);
        let inner = Arc::new(Mutex::new(BufWriter::new(
            write.expect("Failed to open file"),
        )));
        let writer = FlushWriter::new(Arc::clone(&inner));

        // Inspect and commit the transaction to the EVM
        let res = evm.inspect_replay_with_inspector(TracerEip3155::new(Box::new(writer)));

        if let Err(error) = res {
            println!("Got error: {:?}", error);
        }

        // Flush the file writer
        inner.lock().unwrap().flush().expect("Failed to flush file");

        console_bar.inc(1);
    }

    console_bar.finish_with_message("Finished all transactions.");

    let elapsed = start.elapsed();
    println!(
        "Finished execution. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// NOTE: TEMPORARY FUNCTION
pub fn prepare_tx_env(tx: &OpTxEnvelope, caller: Address, encoded: Bytes) -> OpTransaction<TxEnv> {
    let base = match tx {
        OpTxEnvelope::Legacy(tx) => from_recovered_tx_legacy(tx.tx(), caller),
        OpTxEnvelope::Eip1559(tx) => from_recovered_tx_eip1559(tx.tx(), caller),
        OpTxEnvelope::Eip2930(tx) => from_recovered_tx_eip2930(tx.tx(), caller),
        OpTxEnvelope::Eip7702(tx) => from_recovered_tx_eip7702(tx.tx(), caller),
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
            mint: tx.mint,
            is_system_transaction: tx.is_system_transaction,
            eth_value: None,
            eth_tx_value: None,
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

fn from_recovered_tx_legacy(tx: &TxLegacy, caller: Address) -> TxEnv {
    let TxLegacy {
        chain_id,
        nonce,
        gas_price,
        gas_limit,
        to,
        value,
        input,
    } = tx;
    TxEnv {
        tx_type: tx.ty(),
        caller,
        gas_limit: *gas_limit,
        gas_price: *gas_price,
        kind: *to,
        value: *value,
        data: input.clone(),
        nonce: *nonce,
        chain_id: *chain_id,
        ..Default::default()
    }
}

fn from_recovered_tx_eip1559(tx: &TxEip1559, caller: Address) -> TxEnv {
    let TxEip1559 {
        chain_id,
        nonce,
        gas_limit,
        to,
        value,
        input,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        access_list,
    } = tx;
    TxEnv {
        tx_type: tx.ty(),
        caller,
        gas_limit: *gas_limit,
        gas_price: *max_fee_per_gas,
        kind: *to,
        value: *value,
        data: input.clone(),
        nonce: *nonce,
        chain_id: Some(*chain_id),
        gas_priority_fee: Some(*max_priority_fee_per_gas),
        access_list: access_list.clone(),
        ..Default::default()
    }
}

fn from_recovered_tx_eip2930(tx: &TxEip2930, caller: Address) -> TxEnv {
    let TxEip2930 {
        chain_id,
        nonce,
        gas_price,
        gas_limit,
        to,
        value,
        access_list,
        input,
    } = tx;
    TxEnv {
        tx_type: tx.ty(),
        caller,
        gas_limit: *gas_limit,
        gas_price: *gas_price,
        kind: *to,
        value: *value,
        data: input.clone(),
        chain_id: Some(*chain_id),
        nonce: *nonce,
        access_list: access_list.clone(),
        ..Default::default()
    }
}

fn from_recovered_tx_eip7702(tx: &TxEip7702, caller: Address) -> TxEnv {
    let TxEip7702 {
        chain_id,
        nonce,
        gas_limit,
        to,
        value,
        input,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        access_list,
        authorization_list,
    } = tx;
    TxEnv {
        tx_type: tx.ty(),
        caller,
        gas_limit: *gas_limit,
        gas_price: *max_fee_per_gas,
        kind: TxKind::Call(*to),
        value: *value,
        data: input.clone(),
        nonce: *nonce,
        chain_id: Some(*chain_id),
        gas_priority_fee: Some(*max_priority_fee_per_gas),
        access_list: access_list.clone(),
        authorization_list: authorization_list.clone(),
        ..Default::default()
    }
}
