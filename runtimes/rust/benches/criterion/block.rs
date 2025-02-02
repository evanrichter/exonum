// Copyright 2020 The Exonum Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Benchmarking performance of creating patches for blocks (`Blockchain::create_patch` method).
//!
//! What the benchmark measures:
//!
//! - Work with the storage (reading and writing from indexes).
//! - Cryptographic operations (`ProofMapIndex` in the `cryptocurrency` bench).
//!
//! What the benchmark doesn't measure:
//!
//! - Signature verification (transactions are written directly to the node's pool, bypassing
//!   signature checks and other verification).
//!
//! # Constituent benchmarks
//!
//! - `block_timestamping`: Valid no-op transactions. Measures the efficiency of operations
//!   on core indexes (e.g., recording transaction location and result).
//! - `block_timestamping_panic`: Panicking no-op transactions. Measures the efficiency of
//!   `panic` handling.
//! - `block_cryptocurrency`: Transferring cryptocurrency among random accounts. Accounts are
//!   stored in a `ProofMapIndex`.
//! - `block_cryptocurrency_no_proofs`: Transferring cryptocurrency among random accounts.
//!   Accounts are stored in a `MapIndex`.
//! - `block_cryptocurrency_rollback`: Transferring cryptocurrency among random accounts.
//!   Accounts are stored in a `MapIndex`. Transactions are rolled back 50% of the time.

use criterion::{BenchmarkId, Criterion, Throughput};
use exonum::{
    blockchain::{
        config::{GenesisConfig, GenesisConfigBuilder},
        ApiSender, BlockParams, BlockPatch, Blockchain, BlockchainBuilder, BlockchainMut,
        ConsensusConfig, ValidatorKeys,
    },
    crypto::{Hash, KeyPair},
    helpers::{Height, ValidatorId},
    merkledb::{Database, DbOptions, ObjectHash, RocksDB},
    messages::{AnyTx, Verified},
    runtime::SnapshotExt,
};
use rand::{rngs::StdRng, Rng, SeedableRng};
use tempfile::TempDir;

use std::{iter, sync::Arc};

use exonum_rust_runtime::{DefaultInstance, RustRuntime};

/// Number of transactions added to the blockchain before the bench begins.
const PREPARE_TRANSACTIONS: usize = 10_000;
/// Tested values for the number of transactions in an added block.
///
/// `PREPARE_TRANSACTIONS` should be divisible by all values.
const TXS_IN_BLOCK: &[usize] = &[10, 25, 50, 100];

fn gen_keypair_from_rng(rng: &mut StdRng) -> KeyPair {
    use exonum::crypto::{Seed, SEED_LENGTH};

    let mut bytes = [0_u8; SEED_LENGTH];
    rng.fill(&mut bytes);
    KeyPair::from_seed(&Seed::new(bytes))
}

fn create_rocksdb(tempdir: &TempDir) -> RocksDB {
    let options = DbOptions::default();
    RocksDB::open(tempdir.path(), &options).unwrap()
}

fn create_blockchain(
    db: impl Into<Arc<dyn Database>>,
    service: impl DefaultInstance + Clone,
) -> BlockchainMut {
    let (consensus_config, blockchain_base) = create_consensus_config_and_blockchain_base(db);

    let factory = service.clone();
    let rust_runtime = RustRuntime::builder()
        .with_factory(factory)
        .build_for_tests();
    let genesis_config = GenesisConfigBuilder::with_consensus_config(consensus_config)
        .with_artifact(service.artifact_id())
        .with_instance(service.default_instance())
        .build();

    create_blockchain_from_parts(blockchain_base, genesis_config, rust_runtime)
}

fn create_blockchain_from_parts(
    blockchain_base: Blockchain,
    genesis_config: GenesisConfig,
    rust_runtime: RustRuntime,
) -> BlockchainMut {
    BlockchainBuilder::new(blockchain_base)
        .with_genesis_config(genesis_config)
        .with_runtime(rust_runtime)
        .build()
}

fn create_consensus_config_and_blockchain_base(
    db: impl Into<Arc<dyn Database>>,
) -> (ConsensusConfig, Blockchain) {
    let service_keypair = KeyPair::random();
    let consensus_keypair = KeyPair::random();
    let mut consensus_config = ConsensusConfig::default();
    consensus_config.validator_keys = vec![ValidatorKeys::new(
        consensus_keypair.public_key(),
        service_keypair.public_key(),
    )];

    let api_sender = ApiSender::closed();
    let blockchain_base = Blockchain::new(db, service_keypair, api_sender);

    (consensus_config, blockchain_base)
}

fn execute_block(blockchain: &BlockchainMut, txs: &[Hash]) -> BlockPatch {
    let block_params = BlockParams::new(ValidatorId(0), Height(100), txs);
    blockchain.create_patch(block_params, &())
}

mod timestamping {
    use exonum::{
        crypto::Hash,
        merkledb::ObjectHash,
        messages::Verified,
        runtime::{AnyTx, ExecutionContext, ExecutionError, InstanceId},
    };
    use exonum_derive::{exonum_interface, ServiceDispatcher, ServiceFactory};
    use exonum_rust_runtime::{DefaultInstance, Service};
    use rand::rngs::StdRng;

    use super::gen_keypair_from_rng;

    const TIMESTAMPING_SERVICE_ID: InstanceId = 254;

    #[exonum_interface(auto_ids)]
    pub trait TimestampingInterface<Ctx> {
        type Output;
        fn timestamp(&self, ctx: Ctx, arg: Hash) -> Self::Output;
        fn timestamp_panic(&self, ctx: Ctx, arg: Hash) -> Self::Output;
    }

    #[derive(Debug, ServiceFactory, ServiceDispatcher, Clone)]
    #[service_dispatcher(implements("TimestampingInterface"))]
    #[service_factory(artifact_name = "timestamping", proto_sources = "crate::proto")]
    pub struct Timestamping;

    impl TimestampingInterface<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;

        fn timestamp(&self, _ctx: ExecutionContext<'_>, _arg: Hash) -> Self::Output {
            Ok(())
        }

        fn timestamp_panic(&self, _ctx: ExecutionContext<'_>, _arg: Hash) -> Self::Output {
            panic!("panic text");
        }
    }

    impl Service for Timestamping {}

    impl DefaultInstance for Timestamping {
        const INSTANCE_ID: InstanceId = TIMESTAMPING_SERVICE_ID;
        const INSTANCE_NAME: &'static str = "timestamping";
    }

    pub fn transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        (0_u32..).map(move |i| {
            gen_keypair_from_rng(&mut rng).timestamp(TIMESTAMPING_SERVICE_ID, i.object_hash())
        })
    }

    pub fn panicking_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        (0_u32..).map(move |i| {
            gen_keypair_from_rng(&mut rng).timestamp(TIMESTAMPING_SERVICE_ID, i.object_hash())
        })
    }
}

mod cryptocurrency {
    use exonum::{
        crypto::PublicKey,
        merkledb::access::AccessExt,
        messages::Verified,
        runtime::{AnyTx, ExecutionContext, ExecutionError, InstanceId},
    };
    use exonum_derive::{
        exonum_interface, BinaryValue, ObjectHash, ServiceDispatcher, ServiceFactory,
    };
    use exonum_proto::ProtobufConvert;
    use exonum_rust_runtime::{DefaultInstance, Service};
    use rand::{rngs::StdRng, seq::SliceRandom};
    use serde_derive::{Deserialize, Serialize};

    use super::gen_keypair_from_rng;
    use crate::proto;

    const CRYPTOCURRENCY_SERVICE_ID: InstanceId = 255;

    // Number of generated accounts.
    const KEY_COUNT: usize = super::PREPARE_TRANSACTIONS / 10;
    // Initial balance of each account.
    const INITIAL_BALANCE: u64 = 100;

    #[exonum_interface(auto_ids)]
    pub trait CryptocurrencyInterface<Ctx> {
        type Output;

        /// Transfers one unit of currency from `from` to `to`.
        fn transfer(&self, ctx: Ctx, arg: Tx) -> Self::Output;
        /// Same as `transfer`, but without cryptographic proofs in `execute`.
        fn transfer_without_proof(&self, ctx: Ctx, arg: Tx) -> Self::Output;
        /// Same as `transfer_without_proof`, but signals an error 50% of the time.
        fn transfer_error_sometimes(&self, ctx: Ctx, arg: Tx) -> Self::Output;
    }

    #[derive(Debug, ServiceFactory, ServiceDispatcher, Clone)]
    #[service_dispatcher(implements("CryptocurrencyInterface"))]
    #[service_factory(artifact_name = "cryptocurrency", proto_sources = "crate::proto")]
    pub struct Cryptocurrency;

    impl CryptocurrencyInterface<ExecutionContext<'_>> for Cryptocurrency {
        type Output = Result<(), ExecutionError>;

        fn transfer(&self, ctx: ExecutionContext<'_>, arg: Tx) -> Self::Output {
            let from = ctx.caller().author().unwrap();
            let mut index = ctx.service_data().get_proof_map("provable_balances");

            let from_balance = index.get(&from).unwrap_or(INITIAL_BALANCE);
            let to_balance = index.get(&arg.to).unwrap_or(INITIAL_BALANCE);
            index.put(&from, from_balance - 1);
            index.put(&arg.to, to_balance + 1);

            Ok(())
        }

        fn transfer_without_proof(&self, ctx: ExecutionContext<'_>, arg: Tx) -> Self::Output {
            let from = ctx.caller().author().unwrap();
            let mut index = ctx.service_data().get_map("balances");

            let from_balance = index.get(&from).unwrap_or(INITIAL_BALANCE);
            let to_balance = index.get(&arg.to).unwrap_or(INITIAL_BALANCE);
            index.put(&from, from_balance - 1);
            index.put(&arg.to, to_balance + 1);

            Ok(())
        }

        fn transfer_error_sometimes(&self, ctx: ExecutionContext<'_>, arg: Tx) -> Self::Output {
            let from = ctx.caller().author().unwrap();
            let mut index = ctx.service_data().get_map("balances");

            let from_balance = index.get(&from).unwrap_or(INITIAL_BALANCE);
            let to_balance = index.get(&arg.to).unwrap_or(INITIAL_BALANCE);
            index.put(&from, from_balance - 1);
            index.put(&arg.to, to_balance + 1);

            // We deliberately perform the check *after* reads/writes in order
            // to check efficiency of rolling the changes back.
            if arg.seed % 2 == 0 {
                Ok(())
            } else {
                Err(ExecutionError::service(15, ""))
            }
        }
    }

    impl Service for Cryptocurrency {}

    impl DefaultInstance for Cryptocurrency {
        const INSTANCE_ID: InstanceId = CRYPTOCURRENCY_SERVICE_ID;
        const INSTANCE_NAME: &'static str = "cryptocurrency";
    }

    /// Transfers one unit of currency from `from` to `to`.
    #[derive(Clone, Debug)]
    #[derive(Serialize, Deserialize)]
    #[derive(ProtobufConvert, BinaryValue, ObjectHash)]
    #[protobuf_convert(source = "proto::CurrencyTx")]
    pub struct Tx {
        to: PublicKey,
        seed: u32,
    }

    pub fn provable_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        let keys: Vec<_> = (0..KEY_COUNT)
            .map(|_| gen_keypair_from_rng(&mut rng))
            .collect();

        (0..).map(move |seed| {
            let sender_and_receiver: Vec<_> = keys.choose_multiple(&mut rng, 2).collect();
            match &sender_and_receiver[..] {
                [from, to] => from.transfer(
                    CRYPTOCURRENCY_SERVICE_ID,
                    Tx {
                        to: to.public_key(),
                        seed,
                    },
                ),
                _ => unreachable!(),
            }
        })
    }

    pub fn unprovable_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        let keys: Vec<_> = (0..KEY_COUNT)
            .map(|_| gen_keypair_from_rng(&mut rng))
            .collect();

        (0..).map(move |seed| {
            let sender_and_receiver: Vec<_> = keys.choose_multiple(&mut rng, 2).collect();
            match &sender_and_receiver[..] {
                [from, to] => from.transfer_without_proof(
                    CRYPTOCURRENCY_SERVICE_ID,
                    Tx {
                        to: to.public_key(),
                        seed,
                    },
                ),
                _ => unreachable!(),
            }
        })
    }

    pub fn rollback_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        let keys: Vec<_> = (0..KEY_COUNT)
            .map(|_| gen_keypair_from_rng(&mut rng))
            .collect();

        (0..).map(move |seed| {
            let sender_and_receiver: Vec<_> = keys.choose_multiple(&mut rng, 2).collect();
            match &sender_and_receiver[..] {
                [from, to] => from.transfer_error_sometimes(
                    CRYPTOCURRENCY_SERVICE_ID,
                    Tx {
                        to: to.public_key(),
                        seed,
                    },
                ),
                _ => unreachable!(),
            }
        })
    }
}

mod foreign_interface_call {
    use exonum::{
        blockchain::{
            config::{GenesisConfigBuilder, InstanceInitParams},
            BlockchainMut,
        },
        crypto::Hash,
        merkledb::ObjectHash,
        messages::Verified,
        runtime::{AnyTx, ExecutionContext, ExecutionError, InstanceId},
    };
    use exonum_derive::{exonum_interface, ServiceDispatcher, ServiceFactory};
    use exonum_rust_runtime::{RustRuntime, Service, ServiceFactory as _};
    use rand::rngs::StdRng;
    use tempfile::TempDir;

    use super::{
        create_blockchain_from_parts, create_consensus_config_and_blockchain_base, create_rocksdb,
        gen_keypair_from_rng,
    };

    const SELF_INTERFACE_SERVICE_ID: InstanceId = 254;
    const FOREIGN_INTERFACE_SERVICE_ID: InstanceId = 255;

    #[exonum_interface(auto_ids)]
    pub trait SelfInterface<Ctx> {
        type Output;
        fn timestamp(&self, ctx: Ctx, arg: Hash) -> Self::Output;
        fn call_foreign(&self, ctx: Ctx, arg: Hash) -> Self::Output;
    }

    #[exonum_interface(auto_ids)]
    pub trait ForeignInterface<Ctx> {
        type Output;
        fn foreign_timestamp(&self, ctx: Ctx, arg: Hash) -> Self::Output;
    }

    #[exonum_interface(interface = "Configure", auto_ids)]
    pub trait Configure<Ctx> {
        type Output;
    }

    #[exonum_interface(interface = "Events", auto_ids)]
    pub trait Events<Ctx> {
        type Output;
    }

    #[exonum_interface(interface = "ERC30Tokens", auto_ids)]
    pub trait ERC30Tokens<Ctx> {
        type Output;
    }

    #[derive(Debug, ServiceFactory, ServiceDispatcher, Clone)]
    #[service_dispatcher(implements(
        "SelfInterface",
        "ForeignInterface",
        "Configure",
        "Events",
        "ERC30Tokens"
    ))]
    #[service_factory(artifact_name = "timestamping", proto_sources = "crate::proto")]
    pub struct Timestamping;

    impl SelfInterface<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;

        fn timestamp(&self, _ctx: ExecutionContext<'_>, _arg: Hash) -> Self::Output {
            Ok(())
        }

        fn call_foreign(&self, mut ctx: ExecutionContext<'_>, arg: Hash) -> Self::Output {
            ctx.foreign_timestamp(FOREIGN_INTERFACE_SERVICE_ID, arg)
        }
    }

    impl ForeignInterface<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;

        fn foreign_timestamp(&self, ctx: ExecutionContext<'_>, _arg: Hash) -> Self::Output {
            assert_eq!(
                ctx.caller().as_service().unwrap(),
                SELF_INTERFACE_SERVICE_ID
            );
            Ok(())
        }
    }

    impl Configure<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;
    }

    impl Events<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;
    }

    impl ERC30Tokens<ExecutionContext<'_>> for Timestamping {
        type Output = Result<(), ExecutionError>;
    }

    impl Service for Timestamping {}

    fn default_instance(id: InstanceId, name: &str) -> InstanceInitParams {
        Timestamping.artifact_id().into_default_instance(id, name)
    }

    pub fn build_blockchain() -> BlockchainMut {
        let tempdir = TempDir::new().unwrap();
        let db = create_rocksdb(&tempdir);
        let (consensus_config, blockchain_base) = create_consensus_config_and_blockchain_base(db);

        let rust_runtime = RustRuntime::builder()
            .with_factory(Timestamping)
            .build_for_tests();
        let genesis_config = GenesisConfigBuilder::with_consensus_config(consensus_config)
            .with_artifact(Timestamping.artifact_id())
            .with_instance(default_instance(SELF_INTERFACE_SERVICE_ID, "timestamping"))
            .with_instance(default_instance(
                FOREIGN_INTERFACE_SERVICE_ID,
                "timestamping-foreign",
            ))
            .build();

        create_blockchain_from_parts(blockchain_base, genesis_config, rust_runtime)
    }

    pub fn self_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        (0_u32..).map(move |i| {
            gen_keypair_from_rng(&mut rng).timestamp(SELF_INTERFACE_SERVICE_ID, i.object_hash())
        })
    }

    pub fn foreign_transactions(mut rng: StdRng) -> impl Iterator<Item = Verified<AnyTx>> {
        (0_u32..).map(move |i| {
            gen_keypair_from_rng(&mut rng).call_foreign(SELF_INTERFACE_SERVICE_ID, i.object_hash())
        })
    }
}

/// Writes transactions to the pool and returns their hashes.
fn prepare_txs(blockchain: &mut BlockchainMut, transactions: Vec<Verified<AnyTx>>) -> Vec<Hash> {
    // In the case of the block within `Bencher::iter()`, some transactions
    // may already be present in the pool. We don't particularly care about this.
    let tx_hashes = transactions.iter().map(|tx| tx.object_hash()).collect();
    blockchain.add_transactions_into_pool(transactions);
    tx_hashes
}

/// Verifies that transactions with the specified hashes are present in the pool.
///
/// We do this to ensure proper transaction processing. The assertions are performed before
/// the benchmark and do not influence its timings.
fn assert_transactions_in_pool(blockchain: &Blockchain, tx_hashes: &[Hash]) {
    let snapshot = blockchain.snapshot();
    let schema = snapshot.for_core();

    assert!(tx_hashes
        .iter()
        .all(|hash| schema.transactions_pool().contains(hash)
            && !schema.transactions_locations().contains(hash)));
    assert_eq!(tx_hashes.len() as u64, schema.transactions_pool_len());
}

fn prepare_blockchain(
    blockchain: &mut BlockchainMut,
    generator: impl Iterator<Item = Verified<AnyTx>>,
    blockchain_height: usize,
    txs_in_block: usize,
) {
    let transactions: Vec<_> = generator.take(txs_in_block * blockchain_height).collect();

    for i in 0..blockchain_height {
        let start = txs_in_block * i;
        let end = txs_in_block * (i + 1);
        let tx_hashes = prepare_txs(blockchain, transactions[start..end].to_vec());
        assert_transactions_in_pool(blockchain.as_ref(), &tx_hashes);

        let patch = execute_block(blockchain, &tx_hashes);
        // We make use of the fact that `Blockchain::commit()` doesn't check
        // precommits in any way (they are checked beforehand by the consensus algorithm).
        blockchain.commit(patch, iter::empty()).unwrap();
    }
}

fn execute_block_rocksdb(
    criterion: &mut Criterion,
    bench_name: &'static str,
    service: impl DefaultInstance + Clone,
    tx_generator: impl Iterator<Item = Verified<AnyTx>>,
) {
    let tempdir = TempDir::new().unwrap();
    let db = create_rocksdb(&tempdir);
    let blockchain = create_blockchain(db, service);

    execute_block_rocksdb_with_blockchain(criterion, bench_name, blockchain, tx_generator);
}

fn execute_block_rocksdb_with_blockchain(
    criterion: &mut Criterion,
    bench_name: &'static str,
    mut blockchain: BlockchainMut,
    mut tx_generator: impl Iterator<Item = Verified<AnyTx>>,
) {
    // We don't particularly care how transactions are distributed in the blockchain
    // in the preparation phase.
    prepare_blockchain(
        &mut blockchain,
        tx_generator.by_ref(),
        PREPARE_TRANSACTIONS / TXS_IN_BLOCK[2],
        TXS_IN_BLOCK[2],
    );

    // Pre-cache transactions for the created block.
    let txs: Vec<_> = tx_generator
        .take(TXS_IN_BLOCK[TXS_IN_BLOCK.len() - 1])
        .collect();

    let tx_hashes = prepare_txs(&mut blockchain, txs);
    assert_transactions_in_pool(blockchain.as_ref(), &tx_hashes);

    // Because execute_block is not really "micro benchmark"
    // executing it as regular benches, with 100 samples,
    // lead to relatively big testing time.
    // That's why the number of samples was decreased in each test.
    let mut group = criterion.benchmark_group(bench_name);

    for txs_in_block in TXS_IN_BLOCK {
        group
            .bench_with_input(
                BenchmarkId::from_parameter(txs_in_block),
                txs_in_block,
                |bencher, txs_in_block| {
                    bencher.iter(|| {
                        execute_block(&blockchain, &tx_hashes[..*txs_in_block]);
                    })
                },
            )
            .sample_size(100)
            .throughput(Throughput::Elements(*txs_in_block as u64));
    }
}

pub fn bench_block(criterion: &mut Criterion) {
    use log::LevelFilter;
    use std::panic;

    log::set_max_level(LevelFilter::Off);

    execute_block_rocksdb(
        criterion,
        "block/timestamping",
        timestamping::Timestamping,
        timestamping::transactions(SeedableRng::from_seed([2; 32])),
    );

    // We expect lots of panics here, so we switch their reporting off.
    let panic_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| ()));
    execute_block_rocksdb(
        criterion,
        "block/timestamping_panic",
        timestamping::Timestamping,
        timestamping::panicking_transactions(SeedableRng::from_seed([2; 32])),
    );
    panic::set_hook(panic_hook);

    execute_block_rocksdb(
        criterion,
        "block/cryptocurrency",
        cryptocurrency::Cryptocurrency,
        cryptocurrency::provable_transactions(SeedableRng::from_seed([3; 32])),
    );

    execute_block_rocksdb(
        criterion,
        "block/cryptocurrency_no_proofs",
        cryptocurrency::Cryptocurrency,
        cryptocurrency::unprovable_transactions(SeedableRng::from_seed([4; 32])),
    );

    execute_block_rocksdb(
        criterion,
        "block/cryptocurrency_rollback",
        cryptocurrency::Cryptocurrency,
        cryptocurrency::rollback_transactions(SeedableRng::from_seed([4; 32])),
    );

    execute_block_rocksdb_with_blockchain(
        criterion,
        "block/foreign_interface_call/self_tx",
        foreign_interface_call::build_blockchain(),
        foreign_interface_call::self_transactions(SeedableRng::from_seed([2; 32])),
    );

    execute_block_rocksdb_with_blockchain(
        criterion,
        "block/foreign_interface_call/foreign_tx",
        foreign_interface_call::build_blockchain(),
        foreign_interface_call::foreign_transactions(SeedableRng::from_seed([2; 32])),
    );
}
