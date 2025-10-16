extern crate agave_xdp;
extern crate clap;
extern crate caps;

use {
    agave_xdp::{
        device::{NetworkDevice, QueueId},
        netlink::MacAddress,
        relay_loop::relay_loop,
        set_cpu_affinity,
    },
    caps::{CapSet, Capability},
    clap::Parser,
    std::net::Ipv4Addr,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "relay", long_about = None)]
struct Opt {
    #[arg(short, long, default_value = "lo")]
    interface: String,

    #[arg(long)]
    dest_ip: Option<String>,

    #[arg(long)]
    dest_port: Option<u16>,

    #[arg(long)]
    dest_mac: Option<String>,

    #[arg(short, long)]
    zero_copy: bool,

    #[arg(long, default_value = "0")]
    queue: u64,

    #[arg(long, default_value = "2")]
    cpu: usize,

    // #[arg(long)]
    // decoder_cpu: Option<usize>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opt = Opt::parse();

    for cap in [
        Capability::CAP_NET_ADMIN,
        Capability::CAP_NET_RAW,
        Capability::CAP_BPF,
    ] {
        if let Err(e) = caps::raise(None, CapSet::Effective, cap) {
            eprintln!("failed to raise capability {:?}: {}", cap, e);
            eprintln!("run with: sudo -E cargo run --example relay -- <args>");
            std::process::exit(1);
        }
    }

    set_cpu_affinity([opt.cpu]).unwrap();

    let dev = NetworkDevice::new(&opt.interface)?;

    let (dest_ip, dest_port) = match (opt.dest_ip, opt.dest_port) {
        (Some(ip), Some(port)) => (Some(ip.parse::<Ipv4Addr>()?), Some(port)),
        (None, None) => (None, None),
        _ => {
            eprintln!("error: both --dest-ip and --dest-port must be specified together, or neither");
            std::process::exit(1);
        }
    };

    let dest_mac = if let Some(mac_str) = opt.dest_mac {
        let parts: Vec<&str> = mac_str.split(':').collect();
        if parts.len() != 6 {
            eprintln!("invalid MAC address format. use: aa:bb:cc:dd:ee:ff");
            std::process::exit(1);
        }
        let mac_bytes: Result<Vec<u8>, _> = parts
            .iter()
            .map(|p| u8::from_str_radix(p, 16))
            .collect();
        match mac_bytes {
            Ok(bytes) => Some(MacAddress([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]])),
            Err(_) => {
                eprintln!("invalid MAC address format. use hex: aa:bb:cc:dd:ee:ff");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    if let (Some(ip), Some(port)) = (dest_ip, dest_port) {
        println!("starting on {} forwarding to {}:{}", opt.interface, ip, port);
        if let Some(ref mac) = dest_mac {
            println!("destination MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac.0[0], mac.0[1], mac.0[2], mac.0[3], mac.0[4], mac.0[5]);
        }
    } else {
        println!("starting on {}", opt.interface);
    }
    println!("running on CPU {}", opt.cpu);
    println!("zero-copy mode: {}", opt.zero_copy);

    // if let Some(decoder_cpu) = opt.decoder_cpu {
    //     println!("data shred worker on CPU {}", decoder_cpu);
    // } else {
    //     println!("no shred processing worker");
    // }

    relay_loop(
        opt.cpu,
        &dev,
        QueueId(opt.queue),
        opt.zero_copy,
        dest_ip,
        dest_port,
        dest_mac,
        // opt.decoder_cpu
    );

    Ok(())
}
