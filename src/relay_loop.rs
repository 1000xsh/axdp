#![allow(clippy::arithmetic_side_effects)]

// uses AF_XDP for both rx and tx

use {
    crate::{
        load_xdp_program,
        program::insert_socket_into_xskmap,
        // shred_worker::{create_single_worker, publish_shred_zerocopy},
        device::{NetworkDevice, QueueId, RingSizes},
        netlink::MacAddress,
        packet::{
            write_eth_header, write_ip_header, write_udp_header, ETH_HEADER_SIZE, IP_HEADER_SIZE,
            UDP_HEADER_SIZE,
        },
        route::Router,
        set_cpu_affinity,
        // shred_processor::{parse_shred_type, ShredStats},
        socket::{Socket, Rx, Tx},
        umem::{Frame as _, FrameOffset, PageAlignedMemory, SliceUmem, SliceUmemFrame, Umem as _},
    },
    caps::{
        CapSet,
        Capability::{CAP_NET_ADMIN, CAP_NET_RAW, CAP_SYS_NICE},
    },
    libc::{sysconf, _SC_PAGESIZE},
    std::{
        io,
        net::{IpAddr, Ipv4Addr},
        os::fd::{AsFd, AsRawFd},
        // sync::Arc,
        // time::SystemTime,
    },
};

#[inline(never)]
pub fn relay_loop(
    cpu_id: usize,
    dev: &NetworkDevice,
    queue_id: QueueId,
    zero_copy: bool,
    dest_ip: Option<Ipv4Addr>,
    dest_port: Option<u16>,
    dest_mac_override: Option<MacAddress>,
    // decoder_cpu: Option<usize>,
) {
    log::info!(
        "starting relay loop on {} queue {queue_id:?} cpu {cpu_id}",
        dev.name()
    );

    // pin to CPU core
    set_cpu_affinity([cpu_id]).unwrap();

    let src_mac = dev.mac_addr().expect("device must have a MAC address");
    let src_ip = dev.ipv4_addr().expect("device must have an IPv4 address");

    let frame_size = unsafe { sysconf(_SC_PAGESIZE) } as usize;

    let queue = dev
        .open_queue(queue_id)
        .expect("failed to open queue for AF_XDP socket");
    let RingSizes {
        rx: rx_size,
        tx: tx_size,
    } = queue.ring_sizes().unwrap_or_default();

    // allocate UMEM for both rx and tx
    let frame_count = (rx_size + tx_size) * 2;

    // allocate huge pages for UMEM
    const HUGE_2MB: usize = 2 * 1024 * 1024;
    let mut memory =
        PageAlignedMemory::alloc_with_page_size(frame_size, frame_count, HUGE_2MB, true)
            .or_else(|_| PageAlignedMemory::alloc(frame_size, frame_count))
            .unwrap();
    let umem = SliceUmem::new(&mut memory, frame_size as u32).unwrap();

    // raise caps for socket creation
    for cap in [CAP_NET_ADMIN, CAP_NET_RAW, CAP_SYS_NICE] {
        caps::raise(None, CapSet::Effective, cap).unwrap();
    }

    let (_min, max) = fifo_priority_bounds().unwrap();
    set_current_thread_sched_fifo(max).unwrap();

    // load XDP program with XSKMAP for zero-copy redirection
    eprintln!("loading XDP_REDIRECT program on interface {} (if_index: {})", dev.name(), dev.if_index());
    let mut xdp_program = match load_xdp_program(dev.if_index()) {
        Ok(prog) => {
            eprintln!("XDP program loaded successfully");
            prog
        },
        Err(e) => {
            eprintln!("failed to load XDP program: {}", e);
            eprintln!(" make sure you have CAP_BPF and CAP_NET_ADMIN capabilities");
            eprintln!(" try running with: sudo -E cargo run --example relay -- <args>");
            panic!("cannot continue without XDP program");
        }
    };

    // create bidirectional AF_XDP socket for both RX and TX
    eprintln!("creating bidirectional AF_XDP socket on queue {}", queue_id.0);
    let Ok((mut socket, rx, tx)) = Socket::new(
        queue,
        umem,
        zero_copy,
        rx_size,     // rx fill ring size
        rx_size,     // rx ring size
        tx_size * 2, // tx completion ring size
        tx_size,     // tx ring size
    ) else {
        panic!("failed to create bidirectional AF_XDP socket on queue {queue_id:?}");
    };
    eprintln!("AF_XDP socket created successfully");

    // get socket file descriptor and insert into XSKMAP
    // this binds the AF_XDP socket to this queue for XDP_REDIRECT
    let socket_fd = socket.as_fd().as_raw_fd();
    eprintln!("inserting socket FD {} into XSKMAP for queue {}", socket_fd, queue_id.0);
    match insert_socket_into_xskmap(&mut xdp_program, queue_id.0 as u32, socket_fd) {
        Ok(()) => eprintln!("socket successfully bound to XDP program via XSKMAP"),
        Err(e) => {
            eprintln!("failed to insert socket into XSKMAP: {}", e);
            panic!("cannot redirect packets without XSKMAP binding");
        }
    };

    let umem = socket.umem();

    // get UMEM base pointer for zero-copy access
    let umem_base = umem.as_ptr();

    let Rx { mut fill, ring: rx_ring } = rx;
    let mut rx_ring = rx_ring.expect("RX ring must exist");

    let Tx { mut completion, ring: tx_ring } = tx;
    let mut tx_ring = tx_ring.expect("TX ring must exist");

    let router = Router::new().expect("failed to create router");

    // drop caps after socket creation
    for cap in [CAP_NET_ADMIN, CAP_NET_RAW] {
        caps::drop(None, CapSet::Effective, cap).unwrap();
    }

    let dest_mac = if let Some(ip) = dest_ip {
        dest_mac_override.or_else(|| {
            let next_hop = router.route(IpAddr::V4(ip)).ok()?;
            next_hop.mac_addr
        })
    } else {
        None
    };

    // pre-fill rx fill ring with frames for the kernel to use
    // the fill ring needs to have frames available for incoming packets
    fill.sync(false);
    let frames_to_add = rx_size.min(umem.available());
    eprintln!("pre-filling rx fill ring with {} frames", frames_to_add);
    let mut added = 0;
    for _ in 0..frames_to_add {
        if let Some(frame) = umem.reserve() {
            if fill.write(frame).is_err() {
                break;
            }
            added += 1;
        } else {
            break;
        }
    }
    fill.commit();
    eprintln!("added {} frames to fill ring", added);

    // create single shred worker with UMEM access
    // let stats = Arc::new(ShredStats::new());
    // let mut shred_producer = if let Some(decoder_cpu) = decoder_cpu {
    //     const RING_SIZE: usize = 16384;
    //     Some(create_single_worker_zerocopy(
    //         decoder_cpu,
    //         RING_SIZE,
    //         Arc::clone(&stats),
    //         umem_base,  // Pass UMEM base pointer
    //     ))
    // } else {
    //     None
    // };

    // main loop
    const BATCH_SIZE: usize = 32;
    let mut batch_count = 0;
    let mut total_packets = 0usize;
    // let mut total_shreds = 0usize;

    eprintln!("waiting for packets on {} queue {}...", dev.name(), queue_id.0);

    // debug: print initial ring states
    eprintln!("initial ring states:");
    eprintln!("  rx ring capacity: {}, available: {}", rx_ring.capacity(), rx_ring.available());
    eprintln!("  fill ring available: {}", fill.available());
    eprintln!("  tx ring capacity: {}, available: {}", tx_ring.capacity(), tx_ring.available());
    eprintln!("  UMEM base pointer: {:p}", umem_base);

    // let mut debug_counter = 0u64;

    loop {
        // sync rings
        rx_ring.sync(false);
        tx_ring.sync(false);
        completion.sync(false);
        fill.sync(false);

        // debug output every 1000 iterations
        // debug_counter += 1;
        // if debug_counter % 1000 == 0 {
        //     eprintln!("loop iteration {}: rx available: {}, fill available: {}, total packets: {}, total shreds: {}",
        //              debug_counter, rx_ring.available(), fill.available(), total_packets, total_shreds);
        // }

        // process completed tx frames
        while let Some(frame_offset) = completion.read() {
            umem.release(frame_offset);
        }

        // process received packets (zero-copy)
        while let Some(desc) = rx_ring.read() {
            let umem_offset = desc.addr as usize;
            let packet_len = desc.len as usize;

            total_packets += 1;

            // debug logging every 1000 packets. add total_shreds
            if total_packets % 1000 == 0 {
                eprintln!(" received {} packets", total_packets);
            }

            const HEADER_SIZE: usize = ETH_HEADER_SIZE + IP_HEADER_SIZE + UDP_HEADER_SIZE;

            // filter small packets before processing. this will not work, since every shred is 1245 bytes big. we need to decode the tx size to determine if thats a vote. relevant for trading?
            const VOTE_SIZE_THRESHOLD: usize = 400;
            if packet_len < HEADER_SIZE + VOTE_SIZE_THRESHOLD {
                // return frame to fill ring immediately
                let frame = SliceUmemFrame::from_offset(FrameOffset(umem_offset), 0);
                if fill.write(frame).is_err() {
                    umem.release(FrameOffset(umem_offset));
                }
                continue;
            }

            // let timestamp = SystemTime::now();

            // parse packet headers directly in UMEM (zero-copy)
            let packet_ptr = unsafe { umem_base.add(umem_offset) };
            let packet = unsafe { std::slice::from_raw_parts(packet_ptr, packet_len) };

            let ip_header = &packet[ETH_HEADER_SIZE..];

            // check for UDP (protocol 17)
            const IPPROTO_UDP: u8 = 17;
            if ip_header[9] != IPPROTO_UDP {
                // return frame to fill ring
                let frame = SliceUmemFrame::from_offset(FrameOffset(umem_offset), 0);
                if fill.write(frame).is_err() {
                    umem.release(FrameOffset(umem_offset));
                }
                continue;
            }

            // let src_ip_bytes = &ip_header[12..16];
            // let dst_ip_bytes = &ip_header[16..20];

            // let udp_header = &packet[ETH_HEADER_SIZE + IP_HEADER_SIZE..];
            // let src_port = u16::from_be_bytes([udp_header[0], udp_header[1]]);
            // let dst_port = u16::from_be_bytes([udp_header[2], udp_header[3]]);

            // let payload_offset = HEADER_SIZE;
            let payload_len = packet_len - HEADER_SIZE;
            // let udp_payload = &packet[payload_offset..]; // packets

            // let src_ip_arr: [u8; 4] = src_ip_bytes.try_into().unwrap();
            // let dst_ip_arr: [u8; 4] = dst_ip_bytes.try_into().unwrap();
            // for debug only, disable in prod.
            // eprintln!(
            //     "umem: {}, payload: {}, pay_len {}, pkt_len {}, src: {}, src_port: {},dst: {}, dst_port {}",
            //     umem_offset,
            //     payload_offset,
            //     payload_len,
            //     packet_len,
            //     format!("{}.{}.{}.{}", src_ip_arr[0], src_ip_arr[1], src_ip_arr[2], src_ip_arr[3]),
            //     src_port,
            //     format!("{}.{}.{}.{}", dst_ip_arr[0], dst_ip_arr[1], dst_ip_arr[2], dst_ip_arr[3]),
            //     dst_port
            //     // timestamp
            // );            

            // once decoded (slow), we can filter based on fees or block any spammer directly or just decode the shreds
            // dont parse directly and instead use disruptor, below is an example
            // parse shred type
            // let shred_type = parse_shred_type(udp_payload);

            // // process data shreds
            // if shred_type == Some(solana_ledger::shred::ShredType::Data) {
            //     total_shreds += 1;

            //     // if let Some(ref mut producer) = shred_producer {
            //     //     let src_ip_arr: [u8; 4] = src_ip_bytes.try_into().unwrap();
            //     //     let dst_ip_arr: [u8; 4] = dst_ip_bytes.try_into().unwrap();

            //     //     // publish without copying - just pass UMEM offset
            //     //     // publish_shred_zerocopy(
            //     //     //     producer,
            //     //     //     umem_offset,
            //     //     //     payload_offset,
            //     //     //     payload_len,
            //     //     //     packet_len,
            //     //     //     src_ip_arr,
            //     //     //     src_port,
            //     //     //     dst_ip_arr,
            //     //     //     dst_port,
            //     //     //     timestamp,
            //     //     //     shred_type,
            //     //     // );
            //     // }
            // }

            // forward packet if configured (reuse same UMEM frame)
            if let (Some(dest_ip), Some(dest_port), Some(dest_mac)) = (dest_ip, dest_port, dest_mac) {
                // modify headers in-place (zero-copy)
                // safety: we have exclusive access to this UMEM frame
                let packet_mut = unsafe { std::slice::from_raw_parts_mut(packet_ptr as *mut u8, packet_len) };

                // Update Ethernet header
                write_eth_header(packet_mut, &src_mac.0, &dest_mac.0);

                // update IP header
                write_ip_header(
                    &mut packet_mut[ETH_HEADER_SIZE..],
                    &src_ip,
                    &dest_ip,
                    (UDP_HEADER_SIZE + payload_len) as u16,
                );

                // update UDP header
                write_udp_header(
                    &mut packet_mut[ETH_HEADER_SIZE + IP_HEADER_SIZE..],
                    &src_ip,
                    12345,
                    &dest_ip,
                    dest_port,
                    payload_len as u16,
                    false,
                );

                // queue same frame for tx (zero-copy forwarding)
                let tx_frame = SliceUmemFrame::from_offset(FrameOffset(umem_offset), packet_len);
                if tx_ring.write(tx_frame, 0).is_err() {
                    // tx ring full, return to fill ring
                    let frame = SliceUmemFrame::from_offset(FrameOffset(umem_offset), 0);
                    if fill.write(frame).is_err() {
                        umem.release(FrameOffset(umem_offset));
                    }
                }
            } else {
                // not forwarding, return frame to fill ring
                let frame = SliceUmemFrame::from_offset(FrameOffset(umem_offset), 0);
                if fill.write(frame).is_err() {
                    umem.release(FrameOffset(umem_offset));
                }
            }

            // batch commit
            batch_count += 1;
            if batch_count >= BATCH_SIZE {
                rx_ring.commit();
                tx_ring.commit();
                fill.commit();
                if tx_ring.needs_wakeup() {
                    let _ = tx_ring.wake();
                }
                batch_count = 0;
            }
        }

        // refill rx ring
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

        // final commits if needed
        if batch_count > 0 {
            rx_ring.commit();
            tx_ring.commit();
            fill.commit();
            batch_count = 0;
        }
    }
}

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
