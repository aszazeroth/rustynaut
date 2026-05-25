use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Instant;

/// State of a file transfer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TransferState {
    /// Offer sent, waiting for acceptors.
    Offered,
    /// Transfer in progress.
    Transferring,
}

/// An active file transfer after at least one accept.
#[derive(Debug)]
pub(crate) struct FileTransfer {
    pub(crate) sender: SocketAddr,
    pub(crate) sender_username: String,
    pub(crate) room: String,
    pub(crate) filename_b64: String,
    pub(crate) size: u64,
    pub(crate) acceptors: HashSet<SocketAddr>,
    pub(crate) state: TransferState,
}

/// Owns pending offers and active file transfer state.
pub(crate) struct FileTransfers {
    pending_offers: HashMap<OfferKey, PendingOffer>,
    active_transfers: HashMap<u64, FileTransfer>,
    next_transfer_id: u64,
}

/// A pending file offer before any accepts.
#[derive(Debug, Clone)]
struct PendingOffer {
    sender: SocketAddr,
    sender_username: String,
    room: String,
    filename_b64: String,
    size: u64,
    created_at: Instant,
}

/// Unique key for a pending offer: room, sender username, filename.
type OfferKey = (String, String, String);

impl FileTransfers {
    pub(crate) fn new() -> Self {
        Self {
            pending_offers: HashMap::new(),
            active_transfers: HashMap::new(),
            next_transfer_id: 0,
        }
    }

    pub(crate) fn register_offer(
        &mut self,
        sender: SocketAddr,
        sender_username: &str,
        room: &str,
        filename_b64: &str,
        size: u64,
    ) {
        let key = offer_key(room, sender_username, filename_b64);
        self.pending_offers.insert(
            key,
            PendingOffer {
                sender,
                sender_username: sender_username.to_string(),
                room: room.to_string(),
                filename_b64: filename_b64.to_string(),
                size,
                created_at: Instant::now(),
            },
        );
    }

    pub(crate) fn find_latest_offer(&self, room: &str, sender_username: &str) -> Option<String> {
        self.pending_offers
            .values()
            .filter(|offer| offer.room == room && offer.sender_username == sender_username)
            .max_by_key(|offer| offer.created_at)
            .map(|offer| offer.filename_b64.clone())
    }

    pub(crate) fn list_offers(&self, room: &str) -> Vec<(&str, &str, u64)> {
        self.pending_offers
            .values()
            .filter(|offer| offer.room == room)
            .map(|offer| {
                (
                    offer.sender_username.as_str(),
                    offer.filename_b64.as_str(),
                    offer.size,
                )
            })
            .collect()
    }

    pub(crate) fn accept_offer(
        &mut self,
        acceptor: SocketAddr,
        room: &str,
        sender_username: &str,
        filename_b64: &str,
    ) -> Option<(u64, SocketAddr)> {
        for (transfer_id, transfer) in &mut self.active_transfers {
            if transfer.room == room
                && transfer.sender_username == sender_username
                && transfer.filename_b64 == filename_b64
                && transfer.state == TransferState::Offered
            {
                transfer.acceptors.insert(acceptor);
                return Some((*transfer_id, transfer.sender));
            }
        }

        let offer = self
            .pending_offers
            .remove(&offer_key(room, sender_username, filename_b64))?;
        self.next_transfer_id += 1;
        let transfer_id = self.next_transfer_id;

        let mut acceptors = HashSet::new();
        acceptors.insert(acceptor);

        self.active_transfers.insert(
            transfer_id,
            FileTransfer {
                sender: offer.sender,
                sender_username: offer.sender_username,
                room: offer.room,
                filename_b64: offer.filename_b64,
                size: offer.size,
                acceptors,
                state: TransferState::Offered,
            },
        );

        Some((transfer_id, offer.sender))
    }

    pub(crate) fn get_transfer(&self, transfer_id: u64) -> Option<&FileTransfer> {
        self.active_transfers.get(&transfer_id)
    }

    pub(crate) fn get_transfer_mut(&mut self, transfer_id: u64) -> Option<&mut FileTransfer> {
        self.active_transfers.get_mut(&transfer_id)
    }

    pub(crate) fn remove_transfer(&mut self, transfer_id: u64) -> Option<FileTransfer> {
        self.active_transfers.remove(&transfer_id)
    }

    pub(crate) fn insert_transfer(&mut self, transfer_id: u64, transfer: FileTransfer) {
        self.active_transfers.insert(transfer_id, transfer);
    }

    #[cfg(test)]
    pub(crate) fn pending_offer_count(&self) -> usize {
        self.pending_offers.len()
    }

    #[cfg(test)]
    pub(crate) fn next_transfer_id(&self) -> u64 {
        self.next_transfer_id
    }
}

fn offer_key(room: &str, sender_username: &str, filename_b64: &str) -> OfferKey {
    (
        room.to_string(),
        sender_username.to_string(),
        filename_b64.to_string(),
    )
}
