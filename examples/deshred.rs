// simplified deshredding for low latency
// reconstructs solana entries from shreds

use {
    // itertools::Itertools,
    // ReedSolomonCache
    solana_ledger::shred::{Shred, ShredType, Shredder},
    solana_sdk::clock::Slot,
    std::collections::HashMap,
};

const MAX_DATA_SHREDS_PER_SLOT: usize = 32768;

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
enum ShredStatus {
    #[default]
    Unknown,
    NotDataComplete,
    DataComplete,
}

/// tracks per-slot shred information for data shreds
pub struct SlotShreds {
    pub slot: Slot,
    /// compact status of each data shred
    data_status: Vec<ShredStatus>,
    /// data shreds received
    data_shreds: Vec<Option<Shred>>,
    /// code shreds for FEC recovery
    code_shreds: Vec<Shred>,
}

impl SlotShreds {
    pub fn new(slot: Slot) -> Self {
        Self {
            slot,
            data_status: vec![ShredStatus::Unknown; MAX_DATA_SHREDS_PER_SLOT],
            data_shreds: vec![None; MAX_DATA_SHREDS_PER_SLOT],
            code_shreds: Vec::new(),
        }
    }

    /// add a shred to the slot
    /// returns true if this is a new shred
    pub fn add_shred(&mut self, shred: Shred) -> bool {
        let index = shred.index() as usize;

        match shred.shred_type() {
            ShredType::Data => {
                if index >= MAX_DATA_SHREDS_PER_SLOT {
                    return false;
                }

                if self.data_shreds[index].is_some() {
                    return false; // already have this shred
                }

                // check if this is a data complete shred using public methods
                let is_data_complete = shred.data_complete() || shred.last_in_slot();

                // debug: track DATA_COMPLETE markers
                if is_data_complete {
                    eprintln!("debug deshred: slot:{} idx:{} DATA_COMPLETE=true", self.slot, index);
                }

                self.data_status[index] = if is_data_complete {
                    ShredStatus::DataComplete
                } else {
                    ShredStatus::NotDataComplete
                };

                self.data_shreds[index] = Some(shred);
                true
            }
            ShredType::Code => {
                // check if we already have this code shred
                if self.code_shreds.iter().any(|s| s.index() == shred.index()) {
                    return false;
                }
                self.code_shreds.push(shred);
                true
            }
        }
    }

    /// try to reconstruct entries from available shreds
    /// returns (entries, deshredded_payload) if successful
    /// note: this function consumes the segment to prevent re-processing
    pub fn try_deshred(&mut self) -> Option<(Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        // find a complete segment [NotDataComplete*, DataComplete]
        let (start, end) = self.find_complete_segment()?;

        // get shreds for this segment
        let shreds = &self.data_shreds[start..=end];

        // check all shreds are present
        if shreds.iter().any(|s| s.is_none()) {
            let missing: Vec<usize> = (start..=end)
                .filter(|&i| self.data_shreds[i].is_none())
                .collect();
            eprintln!("debug_deshred: slot:{} range:{}..={} missing_indices:{:?}",
                      self.slot, start, end, missing);
            return None;
        }

        // deshred the payload
        let deshredded_payload = match Shredder::deshred(
            shreds.iter().map(|s| s.as_ref().unwrap().payload()),
        ) {
            Ok(payload) => payload,
            Err(_) => return None,
        };

        // deserialize to entries. fix me: wincode ftw -> https://crates.io/crates/wincode
        let entries: Vec<solana_entry::entry::Entry> =
            bincode::deserialize(&deshredded_payload).ok()?;

        // clear the processed segment to prevent re-deshredding
        // just array updates
        for i in start..=end {
            self.data_shreds[i] = None;
            self.data_status[i] = ShredStatus::Unknown;
        }

        Some((entries, deshredded_payload))
    }

    /// find first complete segment: [0+ NotDataComplete, DataComplete]
    fn find_complete_segment(&self) -> Option<(usize, usize)> {
        // find first DataComplete
        let end = self.data_status.iter().position(|s| *s == ShredStatus::DataComplete)?;
        eprintln!("debug_deshred_segment: found DataComplete at index:{} for slot:{}", end, self.slot);

        // find start (after previous DataComplete or beginning)
        let start = if end == 0 {
            0
        } else {
            // scan backwards
            let mut s = end;
            while s > 0 {
                s -= 1;
                match self.data_status[s] {
                    ShredStatus::DataComplete => return Some((s + 1, end)),
                    ShredStatus::Unknown => return None, // gap
                    ShredStatus::NotDataComplete => continue,
                }
            }
            0
        };

        Some((start, end))
    }
}

/// manages shreds across multiple slots
pub struct DeshredManager {
    slots: HashMap<Slot, SlotShreds>,
    // rs_cache: ReedSolomonCache,
}

impl DeshredManager {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
            // rs_cache: ReedSolomonCache::default(),
        }
    }

    /// add a shred and try to deshred if complete
    /// returns (slot, entries, payload) if successful
    pub fn add_shred(
        &mut self,
        shred: Shred,
    ) -> Option<(Slot, Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        let slot = shred.slot();

        // debug: track slot management
        static SLOT_DEBUG_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let count = SLOT_DEBUG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 1000 == 0 {
            eprintln!("debug_deshred_slot: processing slot:{} (total slots tracked:{})", slot, self.slots.len());
        }

        let slot_shreds = self.slots.entry(slot).or_insert_with(|| SlotShreds::new(slot));

        if !slot_shreds.add_shred(shred) {
            return None; // duplicate shred
        }

        // try to deshred
        slot_shreds.try_deshred().map(|(entries, payload)| (slot, entries, payload))
    }

    /// clean up old slots
    pub fn cleanup_old_slots(&mut self, current_slot: Slot, lookback: Slot) {
        let threshold = current_slot.saturating_sub(lookback);
        self.slots.retain(|slot, _| *slot >= threshold);
    }
}
