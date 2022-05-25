// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    counters,
    epoch_manager::EpochManager,
    network::NetworkTask,
    network_interface::{ConsensusNetworkEvents, ConsensusNetworkSender},
    payload_manager::QuorumStoreProxy,
    persistent_liveness_storage::StorageWriteProxy,
    state_computer::ExecutionProxy,
    txn_notifier::MempoolNotifier,
    util::time_service::ClockTimeService,
};
use aptos_config::config::NodeConfig;
use aptos_logger::prelude::*;
use aptos_mempool::TxnNotificationRequest;
use aptos_vm::AptosVM;
use consensus_notifications::ConsensusNotificationSender;
use consensus_types::request_response::ConsensusRequest;
use event_notifications::ReconfigNotificationListener;
use executor::block_executor::BlockExecutor;
use futures::channel::mpsc;
use network::application::storage::PeerMetadataStorage;
use std::sync::Arc;
use storage_interface::DbReaderWriter;
use tokio::runtime::{self, Runtime};

/// Helper function to start consensus based on configuration and return the runtime
pub fn start_consensus(
    node_config: &NodeConfig,
    mut network_sender: ConsensusNetworkSender,
    network_events: ConsensusNetworkEvents,
    state_sync_notifier: Arc<dyn ConsensusNotificationSender>,
    executor_to_mempool_sender: mpsc::Sender<TxnNotificationRequest>,
    consensus_to_quorum_store_sender: mpsc::Sender<ConsensusRequest>,
    aptos_db: DbReaderWriter,
    reconfig_events: ReconfigNotificationListener,
    peer_metadata_storage: Arc<PeerMetadataStorage>,
) -> Runtime {
    let runtime = runtime::Builder::new_multi_thread()
        .thread_name("consensus")
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime!");
    let storage = Arc::new(StorageWriteProxy::new(node_config, aptos_db.reader.clone()));
    let payload_manager = Arc::new(QuorumStoreProxy::new(
        consensus_to_quorum_store_sender,
        node_config.consensus.quorum_store_poll_count,
        node_config.consensus.quorum_store_pull_timeout_ms,
    ));
    let txn_notifier = Arc::new(MempoolNotifier::new(
        executor_to_mempool_sender,
        node_config.consensus.mempool_executed_txn_timeout_ms,
    ));

    let state_computer = Arc::new(ExecutionProxy::new(
        Box::new(BlockExecutor::<AptosVM>::new(aptos_db)),
        txn_notifier,
        state_sync_notifier,
        runtime.handle(),
    ));

    let time_service = Arc::new(ClockTimeService::new(runtime.handle().clone()));

    let (timeout_sender, timeout_receiver) = channel::new(1_024, &counters::PENDING_ROUND_TIMEOUTS);
    let (self_sender, self_receiver) = channel::new(1_024, &counters::PENDING_SELF_MESSAGES);
    network_sender.initialize(peer_metadata_storage);

    let epoch_mgr = EpochManager::new(
        node_config,
        time_service,
        self_sender,
        network_sender,
        timeout_sender,
        payload_manager,
        state_computer,
        storage,
        reconfig_events,
    );

    let (network_task, network_receiver) = NetworkTask::new(network_events, self_receiver);

    runtime.spawn(network_task.start());
    runtime.spawn(epoch_mgr.start(timeout_receiver, network_receiver));

    debug!("Consensus started.");
    runtime
}
