// zero-copy event type for packet processing
// stores UMEM offsets instead of copying packet data

use {
    solana_ledger::shred::ShredType,
    std::time::SystemTime,
};

/// ring buffer
/// contains only metadata and UMEM offsets
#[repr(align(64))] // cache line alignment for optimal performance
pub struct PacketEventZeroCopy {
    /// offset in UMEM where packet starts
    pub umem_offset: usize,
    /// offset within packet to UDP payload (skip ETH+IP+UDP headers)
    pub payload_offset: usize,
    /// length of UDP payload only
    pub payload_len: usize,
    /// total packet length (including all headers)
    pub packet_len: usize,
    /// source IP address (4 bytes for IPv4)
    pub src_ip: [u8; 4],
    /// source port
    pub src_port: u16,
    /// destination IP address
    pub dst_ip: [u8; 4],
    /// destination port
    pub dst_port: u16,
    /// packet receive timestamp
    pub timestamp: SystemTime,
    /// pre-parsed shred type (avoid double parsing)
    pub shred_type: Option<ShredType>,
    /// validity flag (true = packet contains valid data)
    pub valid: bool,
}

impl PacketEventZeroCopy {
    /// factory function for initializing ring buffer events
    pub fn factory() -> Self {
        Self {
            umem_offset: 0,
            payload_offset: 0,
            payload_len: 0,
            packet_len: 0,
            src_ip: [0; 4],
            src_port: 0,
            dst_ip: [0; 4],
            dst_port: 0,
            timestamp: SystemTime::UNIX_EPOCH,
            shred_type: None,
            valid: false,
        }
    }

    /// reset event to initial state (for reuse)
    #[inline]
    pub fn reset(&mut self) {
        self.valid = false;
        self.shred_type = None;
    }

    /// set event data from UMEM without copying packet data
    #[inline]
    pub fn set_from_umem(
        &mut self,
        umem_offset: usize,
        payload_offset: usize,
        payload_len: usize,
        packet_len: usize,
        src_ip: [u8; 4],
        src_port: u16,
        dst_ip: [u8; 4],
        dst_port: u16,
        timestamp: SystemTime,
        shred_type: Option<ShredType>,
    ) {
        self.umem_offset = umem_offset;
        self.payload_offset = payload_offset;
        self.payload_len = payload_len;
        self.packet_len = packet_len;
        self.src_ip = src_ip;
        self.src_port = src_port;
        self.dst_ip = dst_ip;
        self.dst_port = dst_port;
        self.timestamp = timestamp;
        self.shred_type = shred_type;
        self.valid = true;
    }

    /// get payload slice from UMEM base pointer
    /// # safety
    /// caller must ensure:
    /// - umem_base is valid for the lifetime of the returned slice
    /// - umem_base points to the start of UMEM region
    /// - offsets are within valid UMEM bounds
    #[inline]
    pub unsafe fn payload_slice<'a>(&self, umem_base: *const u8) -> &'a [u8] {
        // safety: caller guarantees umem_base is valid and offsets are within bounds
        unsafe {
            let ptr = umem_base.add(self.umem_offset).add(self.payload_offset);
            std::slice::from_raw_parts(ptr, self.payload_len)
        }
    }

    /// get full packet slice from UMEM base pointer
    /// # safety
    /// same safety requirements as payload_slice
    #[inline]
    pub unsafe fn packet_slice<'a>(&self, umem_base: *const u8) -> &'a [u8] {
        // safety: caller guarantees umem_base is valid and offset is within bounds
        unsafe {
            let ptr = umem_base.add(self.umem_offset);
            std::slice::from_raw_parts(ptr, self.packet_len)
        }
    }
}

// ensure struct fits in reasonable size (should be much smaller - 9KB buffer)
const _: () = assert!(std::mem::size_of::<PacketEventZeroCopy>() <= 128);