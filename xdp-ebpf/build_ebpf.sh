#!/bin/bash
set -e

# build the eBPF program
echo "building XDP eBPF program..."

cd xdp-ebpf

# install bpf-linker if not already installed
if ! command -v bpf-linker &> /dev/null; then
    echo "installing bpf-linker..."
    cargo install bpf-linker
fi

rustup toolchain install nightly --component rust-src

# build the eBPF program
cargo +nightly build -Z build-std --release

# copy the compiled eBPF program to a known location
mkdir -p ../target/bpf
cp target/bpfel-unknown-none/release/xdp-redirect ../target/bpf

echo "eBPF program built successfully at target/bpf/xdp-redirect"         

# section headers
# llvm-objdump --section-headers ./target/bpf/xdp-redirect

# check elf headers and dump
# readelf -h ./target/bpf/xdp-redirect | grep -E "Class|Machine|Version"
# llvm-objdump -t ./target/bpf/xdp-redirect | grep -E "XSKS_MAP|xdp_redirect|maps"
