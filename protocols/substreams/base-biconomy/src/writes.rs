//! What one transaction wrote to the storage the venue's pricing depends on: the executor's
//! boards, anchors and pause flags (matched to the maker and pair its events name), the venue's
//! fee and maker list, the providers' signer and executor and the vaults' router, and the
//! tracked token balances and allowances.

use std::collections::{BTreeMap, HashMap};

use substreams_ethereum::pb::eth;

use crate::{
    biconomy::{
        decode_executor_log, paused_attribute, paused_slot, touched_slots, venue_slots, Address,
        ExecutorEvent, Touch, Word,
    },
    modules::config::Config,
};

/// Attribute name to its final value in this transaction.
pub type Writes = BTreeMap<String, Vec<u8>>;

/// The final value of every watched slot this transaction wrote on `address`. A parent call
/// that writes after its child returns comes before the child in `calls`, so writes are ordered
/// by their ordinal and the last one wins.
fn watched_writes(
    tx: &eth::v2::TransactionTrace,
    address: &Address,
    slots: &HashMap<Word, String>,
    out: &mut Writes,
) {
    if slots.is_empty() {
        return;
    }
    let mut changes: Vec<&eth::v2::StorageChange> = tx
        .calls
        .iter()
        .filter(|call| !call.state_reverted)
        .flat_map(|call| call.storage_changes.iter())
        .filter(|change| change.address == address)
        .collect();
    changes.sort_by_key(|change| change.ordinal);
    for change in changes {
        let Ok(key) = <Word>::try_from(change.key.as_slice()) else { continue };
        if let Some(name) = slots.get(&key) {
            out.insert(name.clone(), change.new_value.clone());
        }
    }
}

/// Executor slots this transaction's executor events name, for makers `listed` returns true
/// for and pairs of tracked tokens. Commits are permissionless, so anything else is ignored.
fn executor_slots(
    tx: &eth::v2::TransactionTrace,
    config: &Config,
    listed: &dyn Fn(&Address) -> bool,
) -> HashMap<Word, String> {
    let mut slots = HashMap::new();
    for (log, _) in tx.logs_with_calls() {
        if log.address != config.executor {
            continue;
        }
        match decode_executor_log(&log.topics, &log.data) {
            Some(ExecutorEvent::Board(touch)) if tracked(&touch, config, listed) => {
                slots.extend(touched_slots(&touch))
            }
            Some(ExecutorEvent::Paused(mm)) if listed(&mm) => {
                slots.insert(paused_slot(&mm), paused_attribute(&mm));
            }
            _ => {}
        }
    }
    slots
}

fn tracked(touch: &Touch, config: &Config, listed: &dyn Fn(&Address) -> bool) -> bool {
    listed(&touch.mm) && config.tracks_token(&touch.token_a) && config.tracks_token(&touch.token_b)
}

/// Board, anchor and pause words this transaction wrote for listed makers on tracked pairs.
pub fn executor_writes(
    tx: &eth::v2::TransactionTrace,
    config: &Config,
    listed: &dyn Fn(&Address) -> bool,
) -> Writes {
    let mut out = Writes::new();
    watched_writes(tx, &config.executor, &executor_slots(tx, config, listed), &mut out);
    out
}

/// Precomputed slot lookups for the venue, the providers and vaults, and the tracked tokens.
pub struct Watch {
    venue: HashMap<Word, String>,
    providers: Vec<(Address, HashMap<Word, String>)>,
    tokens: Vec<(Address, HashMap<Word, String>)>,
}

impl Watch {
    pub fn new(config: &Config) -> Self {
        Self {
            venue: venue_slots().into_iter().collect(),
            providers: config
                .provider_state_slots()
                .into_iter()
                .map(|(address, slots)| (address, slots.into_iter().collect()))
                .collect(),
            tokens: config
                .tokens
                .iter()
                .map(|token| {
                    (
                        token.address,
                        config
                            .inventory_slots(token)
                            .into_iter()
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    /// Everything this transaction wrote to the venue, provider and vault slots, and with
    /// `tokens` the tracked token storage. Token storage is skipped in the creation block,
    /// whose token state the bootstrap values already describe.
    pub fn state_writes(
        &self,
        tx: &eth::v2::TransactionTrace,
        config: &Config,
        tokens: bool,
    ) -> Writes {
        let mut out = Writes::new();
        watched_writes(tx, &config.venue, &self.venue, &mut out);
        for (address, slots) in &self.providers {
            watched_writes(tx, address, slots, &mut out);
        }
        if tokens {
            for (token, slots) in &self.tokens {
                watched_writes(tx, token, slots, &mut out);
            }
        }
        out
    }
}
