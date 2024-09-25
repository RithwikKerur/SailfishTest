use std::collections::BTreeSet;

// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header};
use crate::primary::{HeaderMessage, HeaderType};
use futures::future::try_join_all;
use futures::stream::futures_unordered::FuturesUnordered;
use futures::stream::StreamExt as _;
use log::error;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};

/// Waits to receive all the ancestors of a certificate before looping it back to the `Core`
/// for further processing.
pub struct CertificateWaiter {
    /// The persistent storage.
    store: Store,
    /// Receives sync commands from the `Synchronizer`.
    rx_synchronizer: Receiver<Certificate>,
    /// Loops back to the core certificates for which we got all parents.
    tx_core: Sender<Certificate>,
}

impl CertificateWaiter {
    pub fn spawn(
        store: Store,
        rx_synchronizer: Receiver<Certificate>,
        tx_core: Sender<Certificate>,
    ) {
        tokio::spawn(async move {
            Self {
                store,
                rx_synchronizer,
                tx_core,
            }
            .run()
            .await
        });
    }

    /// Helper function. It waits for particular data to become available in the storage
    /// and then delivers the specified header.
    async fn waiter(
        mut missing: Vec<(Vec<u8>, Store)>,
        deliver: Certificate,
    ) -> DagResult<Certificate> {
        let waiting: Vec<_> = missing
            .iter_mut()
            .map(|(x, y)| y.notify_read(x.to_vec()))
            .collect();

        try_join_all(waiting)
            .await
            .map(|_| deliver)
            .map_err(DagError::from)
    }

    async fn run(&mut self) {
        let mut waiting = FuturesUnordered::new();

        loop {
            tokio::select! {
                Some(certificate) = self.rx_synchronizer.recv() => {
                    // Add the certificate to the waiter pool. The waiter will return it to us
                    // when all its parents are in the store.

                    let key = certificate.header_id.to_vec();

                    if let Some(res) = self.store.read(key.clone()).await.unwrap() {
                        let header_msg = bincode::deserialize(&res).unwrap();

                        let parents: Vec<_>;
                        match header_msg {
                            HeaderType::Header(header) => {
                                parents = header.parents;
                            }
                            HeaderType::HeaderInfo(header_info) => {
                                parents = header_info.parents;
                            }
                        }

                        let wait_for = parents
                        .iter()
                        .cloned()
                        .map(|x| (x.to_vec(), self.store.clone()))
                        .collect();

                        let fut = Self::waiter(wait_for, certificate);
                        waiting.push(fut);
                    }else{
                        let wait_for = vec![(key, self.store.clone())];
                        let fut = Self::waiter(wait_for, certificate);
                        waiting.push(fut);
                    }
                }
                Some(result) = waiting.next() => match result {
                    Ok(certificate) => {
                        self.tx_core.send(certificate).await.expect("Failed to send certificate");
                    },
                    Err(e) => {
                        error!("{}", e);
                        panic!("Storage failure: killing node.");
                    }
                },
            }
        }
    }
}
