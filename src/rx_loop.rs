#![allow(clippy::arithmetic_side_effects)]

use {
    crate::{
        device::{NetworkDevice, QueueId, RingSizes},
        set_cpu_affinity,
        socket::Socket,
        umem::{Frame as _, PageAlignedMemory, SliceUmem, Umem as _},
    },
    caps::{
        CapSet,
        Capability::{CAP_NET_ADMIN, CAP_NET_RAW, CAP_SYS_NICE},
    },
    libc::{sysconf, _SC_PAGESIZE},
    std::{
        io,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
    },
};

pub struct RxStats {
    pub rx_packets: AtomicUsize,
    pub rx_bytes: AtomicUsize,
}

#[inline(never)]
pub fn rx_loop(
    cpu_id: usize,
    dev: &NetworkDevice,
    queue_id: QueueId,
    zero_copy: bool,
    stats: Arc<RxStats>,
    exit: Arc<AtomicBool>,
) {
    log::info!(
        "starting xdp rx loop on {} queue {queue_id:?} cpu {cpu_id}",
        dev.name()
    );

    // bind to CPU core
    set_cpu_affinity([cpu_id]).unwrap();

    // some drivers require frame_size=page_size
    let frame_size = unsafe { sysconf(_SC_PAGESIZE) } as usize;

    let queue = dev
        .open_queue(queue_id)
        .expect("failed to open queue for AF_XDP socket");

    let RingSizes { rx: rx_size, .. } = queue.ring_sizes().unwrap_or_else(|| {
        log::info!(
            "using default ring sizes for {} queue {queue_id:?}",
            dev.name()
        );
        RingSizes::default()
    });

    let frame_count = rx_size * 2; // double for rx

    // try to allocate huge pages first, then fall back to regular pages
    const HUGE_2MB: usize = 2 * 1024 * 1024;
    let mut memory =
        PageAlignedMemory::alloc_with_page_size(frame_size, frame_count, HUGE_2MB, true)
            .or_else(|_| {
                log::warn!("huge page alloc failed, falling back to regular page size");
                PageAlignedMemory::alloc(frame_size, frame_count)
            })
            .unwrap();
    let umem = SliceUmem::new(&mut memory, frame_size as u32).unwrap();

    // we need NET_ADMIN and NET_RAW for the socket
    for cap in [CAP_NET_ADMIN, CAP_NET_RAW, CAP_SYS_NICE] {
        caps::raise(None, CapSet::Effective, cap).unwrap();
    }

    let (_min, max) = fifo_priority_bounds().unwrap();
    set_current_thread_sched_fifo(max).unwrap();

    let Ok((mut socket, rx)) = Socket::rx(queue, umem, zero_copy, rx_size, rx_size) else {
        panic!("failed to create AF_XDP socket on queue {queue_id:?}");
    };

    let umem = socket.umem();
    let mut fill = rx.fill;
    let mut rx_ring = rx.ring.unwrap();

    // we dont need higher caps?
    for cap in [CAP_NET_ADMIN, CAP_NET_RAW] {
        caps::drop(None, CapSet::Effective, cap).unwrap();
    }

    // pre-fill the RX ring
    while fill.available() > 0 {
        if let Some(frame) = umem.reserve() {
            let offset = frame.offset();
            if fill.write(frame).is_err() {
                umem.release(offset);
                break;
            }
        } else {
            break;
        }
    }
    fill.commit();

    // main receive loop
    loop {
        if exit.load(Ordering::Relaxed) {
            break;
        }

        // sync rx ring
        rx_ring.sync(false);

        // process received packets
        while let Some(desc) = rx_ring.read() {
            let packet_len = desc.len as usize;

            // update stats
            stats.rx_packets.fetch_add(1, Ordering::Relaxed);
            stats.rx_bytes.fetch_add(packet_len, Ordering::Relaxed);

            // release frame back to fill ring
            umem.release(crate::umem::FrameOffset(desc.addr as usize));
        }

        // commit RX ring
        rx_ring.commit();

        // refill rx ring
        fill.sync(false);
        while fill.available() > 0 {
            if let Some(frame) = umem.reserve() {
                let offset = frame.offset();
                if fill.write(frame).is_err() {
                    umem.release(offset);
                    break;
                }
            } else {
                break;
            }
        }
        fill.commit();
    }
}

/// return min/max valid priorities for SCHED_FIFO on this system.
fn fifo_priority_bounds() -> io::Result<(i32, i32)> {
    unsafe {
        let min = libc::sched_get_priority_min(libc::SCHED_FIFO);
        let max = libc::sched_get_priority_max(libc::SCHED_FIFO);
        if min == -1 || max == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok((min, max))
    }
}

/// set the calling thread to SCHED_FIFO with the given priority.
pub fn set_current_thread_sched_fifo(priority: i32) -> io::Result<()> {
    unsafe {
        let param = libc::sched_param {
            sched_priority: priority,
        };
        let rc = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
