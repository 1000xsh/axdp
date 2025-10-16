// zero-copy packet pool
// pre-allocates packets to eliminate heap allocations in hot path

use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

/// maximum packet size (jumbo frames)
const MAX_PACKET_SIZE: usize = 9000;

/// number of pre-allocated packets in the pool
const POOL_SIZE: usize = 65536;

/// pre-allocated packet buffer
#[repr(align(64))] // cache line aligned
pub struct PacketBuffer {
    data: [u8; MAX_PACKET_SIZE],
    len: usize,
}

impl PacketBuffer {
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    #[inline]
    pub fn set_data(&mut self, data: &[u8]) {
        let len = data.len().min(MAX_PACKET_SIZE);
        self.data[..len].copy_from_slice(&data[..len]);
        self.len = len;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
}

/// packet metadata without heap allocation
#[derive(Clone, Copy)]
pub struct PacketMeta {
    pub src_ip: [u8; 4],
    pub src_port: u16,
    pub dst_ip: [u8; 4],
    pub dst_port: u16,
    pub timestamp: SystemTime,
}

/// reference to a packet in the pool
pub struct PacketRef {
    pub buffer: &'static PacketBuffer,
    pub meta: PacketMeta,
    pool: &'static PacketPool,
    index: usize,
}

impl PacketRef {
    #[inline]
    pub fn payload(&self) -> &[u8] {
        self.buffer.as_slice()
    }
}

impl Drop for PacketRef {
    fn drop(&mut self) {
        // return packet to pool when dropped
        self.pool.release(self.index);
    }
}

/// lock-free packet pool using atomics
pub struct PacketPool {
    packets: Box<[UnsafeCell<PacketBuffer>; POOL_SIZE]>,
    meta: Box<[UnsafeCell<PacketMeta>; POOL_SIZE]>,
    // bitset for free packets (1 = free, 0 = in use)
    free_mask: Box<[AtomicUsize; POOL_SIZE / 64]>,
    next_search: AtomicUsize,
}

impl PacketPool {
    pub fn new() -> &'static Self {
        let packets = Box::new([(); POOL_SIZE].map(|_| UnsafeCell::new(PacketBuffer {
            data: [0u8; MAX_PACKET_SIZE],
            len: 0,
        })));

        let meta = Box::new([(); POOL_SIZE].map(|_| UnsafeCell::new(PacketMeta {
            src_ip: [0; 4],
            src_port: 0,
            dst_ip: [0; 4],
            dst_port: 0,
            timestamp: SystemTime::UNIX_EPOCH,
        })));

        let free_mask = Box::new([(); POOL_SIZE / 64].map(|_| AtomicUsize::new(!0)));

        Box::leak(Box::new(Self {
            packets,
            meta,
            free_mask,
            next_search: AtomicUsize::new(0),
        }))
    }

    /// acquire a packet from the pool (lock-free)
    #[inline]
    pub fn acquire(&'static self) -> Option<(&'static mut PacketBuffer, &'static mut PacketMeta, usize)> {
        let start_idx = self.next_search.load(Ordering::Relaxed) % (POOL_SIZE / 64);

        for offset in 0..POOL_SIZE / 64 {
            let idx = (start_idx + offset) % (POOL_SIZE / 64);
            let mask = &self.free_mask[idx];

            // try to find and claim a free bit
            loop {
                let current = mask.load(Ordering::Acquire);
                if current == 0 {
                    break; // no free packets
                }

                // find first set bit
                let bit_pos = current.trailing_zeros() as usize;
                if bit_pos >= 64 {
                    break;
                }

                // try to claim it
                let new_mask = current & !(1 << bit_pos);
                if mask.compare_exchange_weak(
                    current,
                    new_mask,
                    Ordering::Release,
                    Ordering::Relaxed,
                ).is_ok() {
                    let packet_idx = idx * 64 + bit_pos;
                    self.next_search.store((idx + 1) % (POOL_SIZE / 64), Ordering::Relaxed);

                    // safe because we have exclusive access via atomic bit
                    unsafe {
                        let packet = &mut *self.packets[packet_idx].get();
                        let meta = &mut *self.meta[packet_idx].get();
                        return Some((packet, meta, packet_idx));
                    }
                }
            }
        }

        None // pool exhausted
    }

    /// release a packet back to the pool
    #[inline]
    fn release(&self, index: usize) {
        let word_idx = index / 64;
        let bit_idx = index % 64;
        self.free_mask[word_idx].fetch_or(1 << bit_idx, Ordering::Release);
    }

    /// acquire with automatic return on drop
    #[inline]
    pub fn acquire_ref(&'static self, data: &[u8], meta: PacketMeta) -> Option<PacketRef> {
        let (buffer, meta_slot, index) = self.acquire()?;
        buffer.set_data(data);
        *meta_slot = meta;

        Some(PacketRef {
            buffer,
            meta: *meta_slot,
            pool: self,
            index,
        })
    }
}

// safety: PacketPool is Send + Sync because:
// - we only access packets when we have exclusive ownership via atomic bit
// - the atomic bitset ensures only one thread can access a packet at a time
unsafe impl Send for PacketPool {}
unsafe impl Sync for PacketPool {}

// global packet pool instance
// note: to use this, call PacketPool::new() once at startup and store the reference
// create your own instance