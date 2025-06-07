use std::{path::Path, sync::Arc};

use fedimint_core::bitcoin::{Network, bip32::Xpriv};
use fedimint_lightning_vend::Wallet;
use iroh::NodeAddr;
use machine::MachineProtocol;

const PROTOCOL_SUBDIR: &str = "protocol";
const FEDIMINT_SUBDIR: &str = "fedimint";

pub struct MachineFedimintProtocol {
    machine_protocol: Arc<MachineProtocol>,
    wallet: Arc<Wallet>,

    /// Task which moves claimable payments from `Self.wallet` to `Self.machine_protocol`.
    /// This is safe to cancel/abort at any time, since it copies payments over to the new
    /// location before deleting them from the old location, and all write operations are
    /// idempotent.
    syncer_task_handle: tokio::task::JoinHandle<()>,
}

impl MachineFedimintProtocol {
    pub async fn new(
        storage_path: &Path,
        xprivkey: &Xpriv,
        network: Network,
    ) -> anyhow::Result<Self> {
        let machine_protocol =
            Arc::new(MachineProtocol::new(&storage_path.join(PROTOCOL_SUBDIR)).await?);

        let wallet = Arc::new(Wallet::new(
            xprivkey,
            network,
            storage_path.join(FEDIMINT_SUBDIR),
        ));

        wallet.connect_to_joined_federations().await?;

        let machine_protocol_clone = machine_protocol.clone();
        let wallet_clone = wallet.clone();
        let syncer_task_handle = tokio::task::spawn(async move {
            loop {
                if let Ok(Some(machine_config)) = machine_protocol_clone.get_machine_config().await {
                    let federation_id = machine_config.federation_invite_code.federation_id();
                    let claimer_pk = machine_config.claimer_pk;

                    // Handle any change in the machine config.
                    let _ = wallet_clone.join_federation(machine_config.federation_invite_code).await;

                    // Move completed payments from the fedimint client
                    // to the iroh doc so they can sync to the manager.
                    if let Ok(claimable_contracts) = wallet_clone
                        .get_claimable_contracts(federation_id, claimer_pk, None)
                        .await
                    {
                        for claimable_contract in claimable_contracts {
                            if machine_protocol_clone
                                .write_payment_to_machine_doc(
                                    &federation_id,
                                    &claimable_contract.contract,
                                )
                                .await.is_ok() {
                                    // TODO: Remove contracts from `wallet_clone`.
                                }
                        }
                    }
                }
            }
        });

        Ok(Self {
            machine_protocol,
            wallet,
            syncer_task_handle,
        })
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.machine_protocol.shutdown().await?;

        // This task is safe to abort. See the property's documentation for why this is the case.
        self.syncer_task_handle.abort();

        Ok(())
    }

    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.machine_protocol.is_shutdown() && self.syncer_task_handle.is_finished()
    }

    pub async fn node_addr(&self) -> anyhow::Result<NodeAddr> {
        self.machine_protocol.node_addr().await
    }
}
