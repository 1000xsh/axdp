// lock-free sharded deshred manager
// each thread gets its own DeshredManager instance - no sharing, no locks

use {
    crate::shred_processor::DeshredTrait,
    solana_ledger::shred::{Shred, ShredType},
    solana_sdk::clock::Slot,
    std::{
        // collections::VecDeque,
        sync::atomic::{AtomicU64, Ordering},
    },
};

// use smaller arrays for better cache locality
const SLOT_WINDOW_SIZE: usize = 128;  // track 128 slots
const MAX_SHREDS_PER_SLOT: usize = 512;  // most slots use <100 shreds

/// compact shred tracking with better cache locality
pub struct SlotShrdsCompact {
    pub slot: Slot,
    // use bitset for tracking received shreds (8 u64s = 512 bits)
    received_mask: [u64; MAX_SHREDS_PER_SLOT / 64],
    // store only received shreds in a vec
    shreds: Vec<Option<Shred>>,
    // track segment boundaries
    segment_ends: Vec<u32>,  // indices of DataComplete shreds
    last_processed: u32,
}

impl SlotShrdsCompact {
    pub fn new(slot: Slot) -> Self {
        Self {
            slot,
            received_mask: [0; MAX_SHREDS_PER_SLOT / 64],
            shreds: Vec::with_capacity(100),  // pre-allocate typical size
            segment_ends: Vec::with_capacity(4),
            last_processed: 0,
        }
    }

    #[inline]
    pub fn add_shred(&mut self, shred: Shred) -> bool {
        let index = shred.index() as usize;
        if index >= MAX_SHREDS_PER_SLOT {
            return false;
        }

        // check if already received using bitset
        let word_idx = index / 64;
        let bit_idx = index % 64;
        let mask = 1u64 << bit_idx;

        if self.received_mask[word_idx] & mask != 0 {
            return false;  // already have this shred
        }

        // mark as received
        self.received_mask[word_idx] |= mask;

        // track DataComplete boundaries
        if shred.shred_type() == ShredType::Data && (shred.data_complete() || shred.last_in_slot()) {
            self.segment_ends.push(index as u32);
        }

        // ensure vec is large enough
        if index >= self.shreds.len() {
            self.shreds.resize(index + 1, None);
        }
        self.shreds[index] = Some(shred);

        true
    }

    /// O(1) segment finding using tracked boundaries
    #[inline]
    pub fn try_deshred_fast(&mut self) -> Option<(Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        // check if we have any complete segments
        if self.segment_ends.is_empty() {
            return None;
        }

        // get next segment end
        let end_idx = self.segment_ends[0] as usize;
        let start_idx = self.last_processed as usize;

        // check if all shreds in segment are present (using bitset)
        for idx in start_idx..=end_idx {
            let word_idx = idx / 64;
            let bit_idx = idx % 64;
            if self.received_mask[word_idx] & (1u64 << bit_idx) == 0 {
                return None;  // Missing shred
            }
        }

        // all present, deshred
        let shreds = &self.shreds[start_idx..=end_idx];

        // collect payloads
        let payloads: Vec<_> = shreds
            .iter()
            .filter_map(|s| s.as_ref().map(|shred| shred.payload()))
            .collect();

        if let Ok(deshredded) = solana_ledger::shred::Shredder::deshred(payloads.into_iter()) {
            // replace with wincode -> https://crates.io/crates/wincode
            if let Ok(entries) = bincode::deserialize::<Vec<solana_entry::entry::Entry>>(&deshredded) {
                // mark segment as processed
                self.segment_ends.remove(0);
                self.last_processed = (end_idx + 1) as u32;

                // clear processed shreds to save memory
                for idx in start_idx..=end_idx {
                    self.shreds[idx] = None;
                }

                return Some((entries, deshredded));
            }
        }

        None
    }
}

/// per-thread deshred manager - no locks needed
pub struct DeshredManagerLocal {
    // use fixed-size array indexed by slot % WINDOW_SIZE
    slots: [Option<SlotShrdsCompact>; SLOT_WINDOW_SIZE],
    current_slot: AtomicU64,
}

impl DeshredManagerLocal {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            current_slot: AtomicU64::new(0),
        }
    }

    /// add shred without any locking
    #[inline]
    pub fn add_shred(&mut self, shred: Shred) -> Option<(Slot, Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        let slot = shred.slot();
        let slot_idx = (slot as usize) % SLOT_WINDOW_SIZE;

        // update current slot
        self.current_slot.store(slot, Ordering::Relaxed);

        // get or create slot entry
        let slot_shreds = match &mut self.slots[slot_idx] {
            Some(s) if s.slot == slot => s,
            slot_entry => {
                // replace with new slot
                *slot_entry = Some(SlotShrdsCompact::new(slot));
                slot_entry.as_mut().unwrap()
            }
        };

        if !slot_shreds.add_shred(shred) {
            return None;  // duplicate
        }

        // try to deshred
        slot_shreds.try_deshred_fast()
            .map(|(entries, payload)| (slot, entries, payload))
    }

    /// cleanup old slots (using slot window)
    #[inline]
    pub fn cleanup_old_slots(&mut self, current_slot: Slot) {
        let threshold = current_slot.saturating_sub(SLOT_WINDOW_SIZE as u64);

        for slot_opt in &mut self.slots {
            if let Some(slot_shreds) = slot_opt {
                if slot_shreds.slot < threshold {
                    *slot_opt = None;
                }
            }
        }
    }
}

// trait for lock-free manager
impl DeshredTrait for DeshredManagerLocal {
    #[inline]
    fn add_shred(&mut self, shred: Shred) -> Option<(Slot, Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        self.add_shred(shred)
    }
}

/// global sharded manager for multi-threaded access
pub struct DeshredManagerSharded {
    // one manager per CPU core, no sharing
    managers: Vec<DeshredManagerLocal>,
}

impl DeshredManagerSharded {
    pub fn new(num_threads: usize) -> Self {
        Self {
            managers: (0..num_threads)
                .map(|_| DeshredManagerLocal::new())
                .collect(),
        }
    }

    /// get manager for specific thread (no locking)
    #[inline]
    pub fn get_local(&mut self, thread_id: usize) -> &mut DeshredManagerLocal {
        &mut self.managers[thread_id]
    }
}