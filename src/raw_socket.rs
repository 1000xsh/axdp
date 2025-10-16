// raw AF_PACKET socket for receiving packets in promiscuous mode
// this allows the kernel to continue processing packets (solana rpc or validator. even better spin up gossip with staked key and listen for shreds)

use {
    libc::{
        c_int, c_void, sockaddr, sockaddr_ll, socket, setsockopt, bind,
        AF_PACKET, ETH_P_ALL, SOCK_RAW, SOL_SOCKET, SO_RCVBUF,
        PACKET_ADD_MEMBERSHIP, packet_mreq, PACKET_MR_PROMISC,
        sa_family_t,
    },
    std::{
        io::{self, Error},
        mem,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    },
};

// socket option constants for busy-polling, needs a fix
const SO_BUSY_POLL: c_int = 46;
const SO_PREFER_BUSY_POLL: c_int = 69;

pub struct RawSocket {
    fd: OwnedFd,
    if_index: u32,
}

impl RawSocket {
    /// create a raw packet socket on the given interface
    /// sets promiscuous mode to receive all packets while kernel also processes them
    pub fn new(if_index: u32, recv_buf_size: usize) -> io::Result<Self> {
        unsafe {
            // create AF_PACKET raw socket to receive all ethernet frames
            let fd = socket(AF_PACKET, SOCK_RAW, (ETH_P_ALL as u16).to_be() as c_int);
            if fd < 0 {
                return Err(Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);

            // increase receive buffer to avoid drops
            let buf_size = recv_buf_size as c_int;
            if setsockopt(
                fd.as_raw_fd(),
                SOL_SOCKET,
                SO_RCVBUF,
                &buf_size as *const _ as *const c_void,
                mem::size_of::<c_int>() as u32,
            ) < 0
            {
                return Err(Error::last_os_error());
            }

            // bind to the interface
            let sll = sockaddr_ll {
                sll_family: AF_PACKET as sa_family_t,
                sll_protocol: (ETH_P_ALL as u16).to_be(),
                sll_ifindex: if_index as c_int,
                sll_hatype: 0,
                sll_pkttype: 0,
                sll_halen: 0,
                sll_addr: [0; 8],
            };

            if bind(
                fd.as_raw_fd(),
                &sll as *const _ as *const sockaddr,
                mem::size_of::<sockaddr_ll>() as u32,
            ) < 0
            {
                return Err(Error::last_os_error());
            }

            // enable promiscuous mode to receive all packets on the interface
            let mreq = packet_mreq {
                mr_ifindex: if_index as c_int,
                mr_type: PACKET_MR_PROMISC as u16,
                mr_alen: 0,
                mr_address: [0; 8],
            };

            if setsockopt(
                fd.as_raw_fd(),
                SOL_SOCKET,
                PACKET_ADD_MEMBERSHIP,
                &mreq as *const _ as *const c_void,
                mem::size_of::<packet_mreq>() as u32,
            ) < 0
            {
                return Err(Error::last_os_error());
            }

            Ok(RawSocket { fd, if_index })
        }
    }

    /// enable busy-polling for low latency
    /// kernel will busy-poll NIC for specified microseconds before blocking
    pub fn set_busy_poll(&self, micros: u32) -> io::Result<()> {
        unsafe {
            let val = micros as c_int;
            if setsockopt(
                self.fd.as_raw_fd(),
                SOL_SOCKET,
                SO_BUSY_POLL,
                &val as *const _ as *const c_void,
                mem::size_of::<c_int>() as u32,
            ) < 0
            {
                return Err(Error::last_os_error());
            }
            Ok(())
        }
    }

    /// enable prefer busy-poll mode
    /// with this set, kernel will prefer busy-polling over blocking
    pub fn set_prefer_busy_poll(&self, enable: bool) -> io::Result<()> {
        unsafe {
            let val = if enable { 1 } else { 0 } as c_int;
            if setsockopt(
                self.fd.as_raw_fd(),
                SOL_SOCKET,
                SO_PREFER_BUSY_POLL,
                &val as *const _ as *const c_void,
                mem::size_of::<c_int>() as u32,
            ) < 0
            {
                // ignore error if kernel doesnt support this option
                // (it was added in linux 5.11)
                let err = Error::last_os_error();
                if err.raw_os_error() == Some(92) {  // ENOPROTOOPT
                    eprintln!("SO_PREFER_BUSY_POLL not supported by kernel");
                    return Ok(());
                }
                return Err(err);
            }
            Ok(())
        }
    }

    /// receive a packet into the provided buffer
    /// returns the number of bytes received
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let len = libc::recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                0,
            );
            if len < 0 {
                Err(Error::last_os_error())
            } else {
                Ok(len as usize)
            }
        }
    }

    /// non-blocking receive
    pub fn recv_nonblock(&self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let len = libc::recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                libc::MSG_DONTWAIT,
            );
            if len < 0 {
                Err(Error::last_os_error())
            } else {
                Ok(len as usize)
            }
        }
    }

    pub fn if_index(&self) -> u32 {
        self.if_index
    }
}

impl AsRawFd for RawSocket {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}
