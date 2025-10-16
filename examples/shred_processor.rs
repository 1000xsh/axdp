// 
// shreds from UDP payloads
//
// common header (83 bytes):
//   0x00 (64B): Ed25519 signature
//   0x40 ( 1B): variant (shred type + auth mechanism)
//   0x41 ( 8B): slot
//   0x49 ( 4B): shred_index
//   0x4d ( 2B): shred_version
//   0x4f ( 4B): fec_set_index

use {
    crate::deshred::DeshredManager,
    solana_ledger::shred::{Shred, ShredType},
    solana_sdk::clock::Slot,
    std::{
        sync::{atomic::{AtomicUsize, Ordering}, Mutex},
        time::SystemTime,
    },
};

/// trait for deshred managers (allows both locked and lock-free implementations)
pub trait DeshredTrait {
    fn add_shred(&mut self, shred: Shred) -> Option<(Slot, Vec<solana_entry::entry::Entry>, Vec<u8>)>;
}

// trait for Mutex<DeshredManager>. fix me
impl DeshredTrait for Mutex<DeshredManager> {
    fn add_shred(&mut self, shred: Shred) -> Option<(Slot, Vec<solana_entry::entry::Entry>, Vec<u8>)> {
        self.lock().unwrap().add_shred(shred)
    }
}

/// extract slot number from raw shred bytes (offset 65-72, little-endian)
/// just 8-byte read, no parsing. low latency for filtering future slots
#[inline]
pub fn extract_slot_fast(payload: &[u8]) -> Option<Slot> {
    if payload.len() < 73 {
        return None;
    }
    let bytes: [u8; 8] = payload[65..73].try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// shred type detection without full deserialization
/// parses just the variant byte at offset 0x40
///
/// solana merkle shred encoding (from solana-ledger-3.0.5):
/// - MerkleCode: 0b01??_???? (0x40-0x7F)
///   - 0b0100_???? = MerkleCode
///   - 0b0110_???? = MerkleCode chained
///   - 0b0111_???? = MerkleCode chained resigned
/// - MerkleData: 0b10??_???? (0x80-0xBF)
///   - 0b1000_???? = MerkleData
///   - 0b1001_???? = MerkleData chained
///   - 0b1011_???? = MerkleData chained resigned
/// - legacy variants (0x5a, 0xa5) are REJECTED by solana parser
pub fn parse_shred_type(data: &[u8]) -> Option<ShredType> {
    // minimum shred size is 83 bytes (common header)
    if data.len() < 83 {
        return None;
    }

    // variant byte at offset 0x40 (after 64-byte signature)
    let variant = data[0x40];
    let upper_nibble = variant & 0xF0;

    // check upper 4 bits for merkle variant type
    // MerkleData: 0x80-0xBF (upper nibble: 0x80, 0x90, 0xA0, 0xB0)
    if upper_nibble == 0x80 || upper_nibble == 0x90 || upper_nibble == 0xB0 {
        Some(ShredType::Data)
    }
    // MerkleCode: 0x40-0x7F (upper nibble: 0x40, 0x60, 0x70)
    else if upper_nibble == 0x40 || upper_nibble == 0x60 || upper_nibble == 0x70 {
        Some(ShredType::Code)
    }
    // reject legacy variants and invalid bytes
    else {
        None
    }
}

/// packet data sent from relay loop to decoder thread
pub struct PacketDataRef<'a> {
    pub payload: &'a [u8],
    pub packet_len: usize,
    pub src_ip: [u8; 4],
    pub src_port: u16,
    pub dst_ip: [u8; 4],
    pub dst_port: u16,
    pub timestamp: SystemTime,
    pub shred_type: Option<ShredType>, // pre-parsed to avoid double parsing
}

/// packet data with heap allocation
#[derive(Debug, Clone)]
pub struct PacketData {
    pub payload: Vec<u8>,
    pub src_ip: [u8; 4],
    pub src_port: u16,
    pub dst_ip: [u8; 4],
    pub dst_port: u16,
    pub timestamp: SystemTime,
}

/// statistics for shred processing
pub struct ShredStats {
    pub received: AtomicUsize,
    pub decoded: AtomicUsize,
    pub errors: AtomicUsize,
    pub data_shreds: AtomicUsize,
    pub code_shreds: AtomicUsize,
    pub code_drops: AtomicUsize, // code shreds dropped due to channel overflow
}

impl ShredStats {
    pub fn new() -> Self {
        Self {
            received: AtomicUsize::new(0),
            decoded: AtomicUsize::new(0),
            errors: AtomicUsize::new(0),
            data_shreds: AtomicUsize::new(0),
            code_shreds: AtomicUsize::new(0),
            code_drops: AtomicUsize::new(0),
        }
    }

    pub fn print_stats(&self) {
        let received = self.received.load(Ordering::Relaxed);
        let decoded = self.decoded.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let data = self.data_shreds.load(Ordering::Relaxed);
        let code = self.code_shreds.load(Ordering::Relaxed);
        let drops = self.code_drops.load(Ordering::Relaxed);

        println!(
            "shred stats - received: {}, decoded: {}, errors: {}, data: {}, code: {}, code drops: {}",
            received, decoded, errors, data, code, drops
        );
    }
}

/// parse and decode a shred from UDP payload
/// the public Shred::new_from_serialized_shred internally:
/// 1. checks variant byte to determine Merkle vs Legacy
/// 2. rejects legacy shreds (returns InvalidShredVariant)
/// 3. creates merkle shreds via merkle::ShredData/ShredCode::from_payload
/// 4. handles size by truncating to SIZE_OF_PAYLOAD (1203 for data, 1228 for code)
fn parse_shred(data: &[u8]) -> Result<Shred, Box<dyn std::error::Error>> {
    // minimum shred size is 83 bytes (common header)
    if data.len() < 83 {
        return Err("packet too small to be a shred".into());
    }

    // public api - it handles everything internally
    let shred = Shred::new_from_serialized_shred(data.to_vec())?;
    Ok(shred)
}

/// nanosecond precision
fn format_timestamp(timestamp: SystemTime) -> String {
    use std::time::UNIX_EPOCH;

    match timestamp.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            let nanos = duration.subsec_nanos();

            // convert to hours, minutes, seconds
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;

            format!(
                "{:02}:{:02}:{:02}.{:09}",
                hours, minutes, seconds, nanos
            )
        }
        Err(_) => "INVALID_TIME".to_string(),
    }
}

/// processes shred without allocations
/// uses pre-parsed shred type to avoid double parsing
#[inline]
pub fn process_shred_ref<T>(packet: &PacketDataRef, stats: &ShredStats, deshred_mgr: &mut T)
where
    T: DeshredTrait,
{
    // #[cfg(feature = "debug")]
    stats.received.fetch_add(1, Ordering::Relaxed);

    // skip full parsing if we already know its not a shred
    if packet.shred_type.is_none() {
        // #[cfg(feature = "debug")]
        stats.errors.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // use pre-parsed type to avoid double parsing
    match Shred::new_from_serialized_shred(packet.payload.to_vec()) {
        Ok(shred) => {
            // #[cfg(feature = "debug")]
            {
                eprintln!("debug_shred_processor_parse: parsed shred slot:{} index:{}", shred.slot(), shred.index());
                stats.decoded.fetch_add(1, Ordering::Relaxed);

                match packet.shred_type.unwrap() {
                    ShredType::Data => stats.data_shreds.fetch_add(1, Ordering::Relaxed),
                    ShredType::Code => stats.code_shreds.fetch_add(1, Ordering::Relaxed),
                };
            }

            let _slot = shred.slot();

            // try to deshred
            if let Some((slot, entries, _payload)) = deshred_mgr.add_shred(shred) {
                let txn_count: usize = entries.iter().map(|e| e.transactions.len()).sum();

                // only format timestamp when actually printing
                let ts = format_timestamp(packet.timestamp);
                let src_ip = std::net::Ipv4Addr::from(packet.src_ip);
                let dst_ip = std::net::Ipv4Addr::from(packet.dst_ip);
                eprintln!(
                    "deshred [{}] {}:{} -> {}:{} slot:{} entries:{} txns:{}",
                    ts,
                    src_ip,
                    packet.src_port,
                    dst_ip,
                    packet.dst_port,
                    slot,
                    entries.len(),
                    txn_count
                );

                // extract and log transaction signatures (filter out votes)
                for (entry_idx, entry) in entries.iter().enumerate() {
                    for transaction in &entry.transactions {
                        // check transaction size first. replace with wincode -> https://crates.io/crates/wincode
                        let txn_size = bincode::serialized_size(transaction).unwrap_or(0);

                        // FILTER: skip small transactions (votes are ~352 bytes)
                        const VOTE_TXN_SIZE_THRESHOLD: u64 = 400;
                        if txn_size < VOTE_TXN_SIZE_THRESHOLD {
                            continue; // Skip votes
                        }

                        if !transaction.signatures.is_empty() {
                            let sig = transaction.signatures[0];
                            eprintln!(
                                "tx [{}] {}:{} -> {}:{} slot:{} entry:{} pkt:{} txn:{} sig: https://solscan.io/tx/{}",
                                ts,
                                src_ip,
                                packet.src_port,
                                dst_ip,
                                packet.dst_port,
                                slot,
                                entry_idx,
                                packet.packet_len,
                                txn_size,
                                sig
                            );
                        }
                    }
                }
            }

            // cleanup old slots periodically (without checking atomic)
            // Note: cleanup_old_slots should be implemented in the trait if needed
        }
        Err(_e) => {
            // #[cfg(feature = "debug")]
            {
                static ERROR_COUNTER: AtomicUsize = AtomicUsize::new(0);
                let err_count = ERROR_COUNTER.fetch_add(1, Ordering::Relaxed);
                if err_count % 100 == 0 {
                    eprintln!("debug_shred_processor_error #{}: {:?}", err_count, _e);
                }
                stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// legacy process function
pub fn process_shred(packet: &PacketData, stats: &ShredStats, deshred_mgr: &Mutex<DeshredManager>) {
    stats.received.fetch_add(1, Ordering::Relaxed);

    //  eprintln!("process_shred payload:{:?}", packet.payload);

    match parse_shred(&packet.payload) {
        Ok(shred) => {
            eprintln!("debug_shred_processor_parse: parsed shred slot:{} index:{}", shred.slot(), shred.index());
            stats.decoded.fetch_add(1, Ordering::Relaxed);

            // update type-specific counters
            match shred.shred_type() {
                ShredType::Data => stats.data_shreds.fetch_add(1, Ordering::Relaxed),
                ShredType::Code => stats.code_shreds.fetch_add(1, Ordering::Relaxed),
            };

            let ts = format_timestamp(packet.timestamp);
            let slot = shred.slot();
            let index = shred.index();
            let shred_type = shred.shred_type();

            // try to deshred
            let mut mgr = deshred_mgr.lock().unwrap();
            if let Some((slot, entries, _payload)) = mgr.add_shred(shred) {
                let txn_count: usize = entries.iter().map(|e| e.transactions.len()).sum();

                eprintln!(
                    "deshred [{}] slot:{} entries:{} txns:{}",
                    ts,
                    slot,
                    entries.len(),
                    txn_count
                );

                // extract and log transaction signatures
                for (entry_idx, entry) in entries.iter().enumerate() {
                    for transaction in &entry.transactions {
                        if !transaction.signatures.is_empty() {
                            let sig = transaction.signatures[0];
                            eprintln!(
                                "TX [{}] slot:{} entry:{} sig:{}",
                                ts,
                                slot,
                                entry_idx,
                                sig
                            );
                        }
                    }
                }
            } else {
                // just received a shred, not complete yet
                // print every 100th shred to avoid overwhelming output
                static SHRED_COUNTER: AtomicUsize = AtomicUsize::new(0);
                let shred_count = SHRED_COUNTER.fetch_add(1, Ordering::Relaxed);
                if shred_count % 100 == 0 {
                    eprintln!(
                        "shred [{}] from {}:{} → slot:{} index:{} type:{:?} size:{}",
                        ts,
                        packet.src_ip[0],
                        packet.src_port,
                        slot,
                        index,
                        shred_type,
                        packet.payload.len(),
                    );
                }
            }

            // cleanup old slots periodically
            if stats.decoded.load(Ordering::Relaxed) % 1000 == 0 {
                mgr.cleanup_old_slots(slot, 50);
            }
        }
        Err(e) => {
            // not a valid shred - could be gossip or other chain traffic
            static ERROR_COUNTER: AtomicUsize = AtomicUsize::new(0);
            let err_count = ERROR_COUNTER.fetch_add(1, Ordering::Relaxed);
            if err_count % 100 == 0 {
                eprintln!("debug_shred_processor_error #{}: {:?}", err_count, e);
            }
            stats.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// decoder thread worker
/// continuously processes packets from the channel
pub fn decoder_worker(
    rx: crossbeam_channel::Receiver<PacketData>,
    stats: std::sync::Arc<ShredStats>,
) {
    let deshred_mgr = Mutex::new(DeshredManager::new());

    loop {
        match rx.recv() {
            Ok(packet) => {
                process_shred(&packet, &stats, &deshred_mgr);
            }
            Err(_) => {
                // channel closed, exit thread
                break;
            }
        }
    }
}
