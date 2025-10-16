#![allow(clippy::arithmetic_side_effects)]

use aya::{programs::Xdp, Ebpf, include_bytes_aligned};
use aya::maps::XskMap;
// use std::os::fd::AsRawFd;

pub fn load_xdp_program(if_index: u32) -> Result<Ebpf, Box<dyn std::error::Error>> {
    // load the compiled eBPF bytecode with proper alignment
    // the include_bytes_aligned! macro ensures the bytes are properly aligned for eBPF loading
    let mut ebpf = Ebpf::load(include_bytes_aligned!(
        "../target/bpf/xdp-redirect"
    ))?;

    // debug: print all program names
    eprintln!("available programs in eBPF object:");
    for (name, _) in ebpf.programs() {
        eprintln!("  - {}", name);
    }

    // debug: get the XDP program - try different possible names
    let program_name = if ebpf.program("xdp_redirect").is_some() {
        "xdp_redirect"
    } else if ebpf.program("xdp").is_some() {
        "xdp"
    } else {
        return Err("no XDP program found in eBPF object".into());
    };

    eprintln!("using XDP program: {}", program_name);
    let p: &mut Xdp = ebpf.program_mut(program_name).unwrap().try_into()?;
    p.load()?;

    // try native mode first, fall back to SKB mode if it fails
    match p.attach_to_if_index(if_index, aya::programs::xdp::XdpFlags::DRV_MODE) {
        Ok(_) => {
            eprintln!("XDP program loaded and attached to if_index {} in DRV mode (native)", if_index);
        }
        Err(e) => {
            eprintln!("failed to attach in DRV mode: {}, trying SKB mode", e);
            p.attach_to_if_index(if_index, aya::programs::xdp::XdpFlags::SKB_MODE)?;
            eprintln!("XDP program loaded and attached to if_index {} in SKB mode (generic)", if_index);
        }
    }

    Ok(ebpf)
}

/// insert AF_XDP socket file descriptor into XSKMAP
/// this enables XDP_REDIRECT to route packets to the AF_XDP socket
pub fn insert_socket_into_xskmap(
    ebpf: &mut Ebpf,
    queue_id: u32,
    socket_fd: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    // debug: print all map names
    eprintln!("available maps in eBPF object:");
    for (name, _) in ebpf.maps() {
        eprintln!("  - {}", name);
    }

    // get the XSKS_MAP from the eBPF program
    let map = ebpf.map_mut("XSKS_MAP")
        .ok_or("XSKS_MAP not found in XDP program")?;
    let mut xskmap: XskMap<_> = map.try_into()?;

    // insert the socket FD into the map at the queue index
    xskmap.set(queue_id, socket_fd, 0)?;

    eprintln!("inserted socket FD {} into XSKS_MAP at queue {}", socket_fd, queue_id);

    Ok(())
}
