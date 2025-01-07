//! Optimism-specific constants, types, and helpers.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::Bytes;
use anyhow::{anyhow, Result};
use dotenv::dotenv;
use ethers_core::types::{Transaction, H256};
use ethers_providers::Middleware;
use ethers_providers::{Http, Provider};
use op_alloy_consensus::OpTxEnvelope;
use revm::db::{CacheDB, EthersDB};
use revm::inspectors::TracerEip3155;
use revm::primitives::{Address, OptimismFields, SpecId, TransactTo, TxEnv, TxKind, U256};
use revm::{inspector_handle_register, Database, Evm, L1_BLOCK_CONTRACT};
use std::fs::OpenOptions;
use std::io::BufWriter;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

macro_rules! local_fill {
    ($left:expr, $right:expr, $fun:expr) => {
        if let Some(right) = $right {
            $left = $fun(right.0)
        }
    };
    ($left:expr, $right:expr) => {
        if let Some(right) = $right {
            $left = Address::from(right.as_fixed_bytes())
        }
    };
}

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

struct CheckerRecord {
    total: u64,
    passed: u64,
    failed: u64,
}
impl CheckerRecord {
    fn new() -> Self {
        Self {
            total: 0,
            passed: 0,
            failed: 0,
        }
    }
    fn add(&mut self, passed: bool) {
        self.total += 1;
        if passed {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
    }
    fn print(&self) {
        println!(
            "Total: {}, Passed: {}, Failed: {}, {}% correct",
            self.total,
            self.passed,
            self.failed,
            self.passed as f64 / self.total as f64 * 100.0
        );
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // let start = 66450341; // contract creation
    let start = 71207577;
    let record = Arc::new(Mutex::new(CheckerRecord::new()));
    for block_number in start..start + 1 {
        range(block_number, record.clone()).await?;
    }
    record.lock().unwrap().print();
    Ok(())
}

async fn range(block_number: u64, record: Arc<Mutex<CheckerRecord>>) -> anyhow::Result<()> {
    dotenv().ok();
    let mantle_url = std::env::var("MANTLE_URL").unwrap();

    // Provider with debug tracing
    let client = Provider::<Http>::try_from(
        mantle_url,
    )?;
    let client = Arc::new(client);

    // Params
    let chain_id: u64 = 5000;
    let block_number = block_number;

    // Fetch the transaction-rich block
    let block = match client.get_block_with_txs(block_number).await {
        Ok(Some(block)) => block,
        Ok(None) => anyhow::bail!("Block not found"),
        Err(error) => anyhow::bail!("Error: {:?}", error),
    };
    println!("Fetched block number: {}", block.number.unwrap().0[0]);
    let previous_block_number = block_number - 1;

    // Use the previous block state as the db with caching
    let prev_id = previous_block_number.into();

    // SAFETY: This cannot fail since this is in the top-level tokio runtime
    let state_db = EthersDB::new(client.clone(), Some(prev_id)).expect("panic");
    let mut cache_db = CacheDB::new(state_db);

    let mut evm = Evm::builder()
        .with_db(&mut cache_db)
        .with_external_context(TracerEip3155::new(Box::new(std::io::stdout())))
        .modify_block_env(|b| {
            if let Some(number) = block.number {
                let nn = number.0[0];
                b.number = U256::from(nn);
            }
            local_fill!(b.coinbase, block.author);
            local_fill!(b.timestamp, Some(block.timestamp), U256::from_limbs);
            local_fill!(b.difficulty, Some(block.difficulty), U256::from_limbs);
            local_fill!(b.gas_limit, Some(block.gas_limit), U256::from_limbs);
            if let Some(base_fee) = block.base_fee_per_gas {
                local_fill!(b.basefee, Some(base_fee), U256::from_limbs);
            }
        })
        .with_spec_id(SpecId::SHANGHAI)
        .modify_cfg_env(|c| {
            c.chain_id = chain_id;
        })
        .optimism()
        .append_handler_register(inspector_handle_register)
        .build();

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    let start = Instant::now();

    // Create the traces directory if it doesn't exist
    std::fs::create_dir_all("traces").expect("Failed to create traces directory");

    for tx in block.transactions {
        let tx_number = tx.transaction_index.unwrap().0[0];
        // if tx_number != 1 {
        //     continue;
        // }
        println!("current tx_number: {:?}", tx_number);
        let tx_hash = tx.hash;
        let raw_tx = client
            .request::<&[H256; 1], Bytes>("debug_getRawTransaction", &[tx_hash.into()])
            .await
            .map_err(|e| anyhow!("Failed to fetch raw transaction: {e}"))?;
        let op_tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref())
            .map_err(|e| anyhow!("Failed to decode EIP-2718 transaction: {e}"))?;
        let env = prepare_tx_env(&op_tx, raw_tx.as_ref())?;
        evm = evm.modify().with_tx_env(env).build();

        println!(
            "--------------- {:?}: {:?}({:?}) ------------------",
            tx_number,
            tx_hash,
            op_tx.tx_type()
        );

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
        evm.context.external.set_writer(Box::new(writer));
        // let ResultAndState { result, state } = evm
        //     .transact()
        //     .map_err(|e| anyhow!("Failed to transact: {e}"))?;
        let result = evm
            .transact_commit()
            .map_err(|e| anyhow!("Failed to transact: {e}"))?;
        let gas_used = result.gas_used();

        let expected_gas_used = client
            .get_transaction_receipt(tx_hash)
            .await?
            .unwrap()
            .gas_used
            .unwrap();
        print!("Expected gas used: {:?}, ", expected_gas_used);
        print!("Actual gas used: {:?} ", gas_used);
        if expected_gas_used.as_u64() == gas_used {
            println!("--- passed✅");
        } else {
            println!("--- failed❌");
        }
        record
            .lock()
            .unwrap()
            .add(expected_gas_used.as_u64() == gas_used);

        // for (address, account) in &state {
        //     if account.is_touched() {
        //         println!("---------------------------------");
        //         println!("after transaction");
        //         let balance = evm.db_mut().basic(*address)?.map(|info| info.balance);
        //         println!("{:?}'s Balance: {:?}", address, balance);
        //     }
        // }
    }
    let elapsed = start.elapsed();
    println!(
        "Finished execution. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );
    drop(evm);
    Ok(())
}

async fn sync_system_storage(
    client: Arc<Provider<Http>>,
    block_num: u64,
    db: &mut CacheDB<EthersDB<Provider<Http>>>,
) -> () {
    let mut state_db = EthersDB::new(client.clone(), Some(block_num.into())).expect("panic");
    let l1_base_fee_slot = U256::from_limbs([1u64, 0, 0, 0]);
    let storage = state_db
        .storage(L1_BLOCK_CONTRACT, l1_base_fee_slot)
        .expect("panic");
    println!("storage is {:?}", storage);
    db.insert_account_storage(L1_BLOCK_CONTRACT, l1_base_fee_slot, storage)
        .expect("panic");
}

// inspired by kona:
//      - crates/executor/src/executor/mod.rs#151
//      - bin/host/src/fetcher/mod.rs@store_transactions
#[allow(dead_code)]
async fn convert_tx_to_op(
    txs: Vec<Transaction>,
    client: Arc<Provider<Http>>,
) -> Result<Vec<(OpTxEnvelope, Bytes)>> {
    // println!("Fetching all transactions...,len:{}", txs.len());
    let mut encoded_transactions = Vec::with_capacity(txs.len());
    for tx in txs {
        let tx_hash = tx.hash;
        println!("Fetching transaction: {:?}", tx_hash);
        let raw_tx = client
            .request::<&[H256; 1], Bytes>("debug_getRawTransaction", &[tx_hash.into()])
            .await
            .map_err(|e| anyhow!("Failed to fetch raw transaction: {e}"))?;
        encoded_transactions.push(raw_tx);
    }
    let decoded_txs = encoded_transactions
        .iter()
        .map(|raw_tx| {
            let tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref())
                .map_err(|e| anyhow!("Failed to decode EIP-2718 transaction: {e}"))?;
            Ok((tx, raw_tx.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(decoded_txs)
    // let enc
}

/// Prepares a [TxEnv] with the given [OpTxEnvelope].
///
/// ## Takes
/// - `transaction`: The transaction to prepare the environment for.
/// - `env`: The transaction environment to prepare.
///
/// ## Returns
/// - `Ok(())` if the environment was successfully prepared.
/// - `Err(_)` if an error occurred while preparing the environment.
pub fn prepare_tx_env(transaction: &OpTxEnvelope, encoded_transaction: &[u8]) -> Result<TxEnv> {
    let mut env = TxEnv::default();
    match transaction {
        OpTxEnvelope::Legacy(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.gas_price);
            env.gas_priority_fee = None;
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = tx.chain_id;
            env.nonce = Some(tx.nonce);
            env.access_list.clear();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Eip2930(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.gas_price);
            env.gas_priority_fee = None;
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = Some(tx.chain_id);
            env.nonce = Some(tx.nonce);
            env.access_list = tx.access_list.to_vec();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Eip1559(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.max_fee_per_gas);
            env.gas_priority_fee = Some(U256::from(tx.max_priority_fee_per_gas));
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = Some(tx.chain_id);
            env.nonce = Some(tx.nonce);
            env.access_list = tx.access_list.to_vec();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Deposit(tx) => {
            println!("Deposit transaction: {:?}", tx);
            env.caller = tx.from;
            env.access_list.clear();
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::ZERO;
            env.gas_priority_fee = None;
            match tx.to {
                TxKind::Call(to) => env.transact_to = TransactTo::Call(to),
                TxKind::Create => env.transact_to = TransactTo::Create,
            }
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = None;
            env.nonce = None;
            env.optimism = OptimismFields {
                source_hash: Some(tx.source_hash),
                mint: tx.mint,
                is_system_transaction: Some(tx.is_system_transaction),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: tx.eth_value,
                eth_tx_value: tx.eth_value,
            };
            Ok(env)
        }
        _ => Err(anyhow!(
            "Unsupported transaction type: {:?}",
            transaction.tx_type() as u8
        )),
    }
}