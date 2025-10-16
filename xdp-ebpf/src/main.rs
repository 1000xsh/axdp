#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::xdp_action,
    macros::{map, xdp},
    maps::XskMap,
    programs::XdpContext,
};

// XDP_REDIRECT is mutually exclusive - packet goes to AF_XDP or kernel
// packets for both XDP_PASS (copy)
// XSKMAP that holds AF_XDP socket file descriptors
// maps queue_id -> socket_fd
// we pass one quque only, for all packets:
// multiple AF_XDP sockets (one per queue)
// insert all into XSKMAP

#[map]
static XSKS_MAP: XskMap = XskMap::with_max_entries(64, 0);

#[xdp]
pub fn xdp_redirect(ctx: XdpContext) -> u32 {
    match try_xdp_redirect(ctx) {
        Ok(ret) => ret,
        Err(_) => xdp_action::XDP_PASS,
    }
}

#[inline(always)]
fn try_xdp_redirect(ctx: XdpContext) -> Result<u32, ()> {
    // get the queue index from the context
    // this tells us which hardware queue received the packet
    let queue_id = unsafe { (*ctx.ctx).rx_queue_index };

    // redirect the packet to the AF_XDP socket bound to this queue
    // the XSKS_MAP contains socket FDs inserted by the userspace program
    XSKS_MAP.redirect(queue_id, 0).map_err(|_| ())?;

    // return XDP_REDIRECT to tell the kernel to redirect the packet
    Ok(xdp_action::XDP_REDIRECT)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}