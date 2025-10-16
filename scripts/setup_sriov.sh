#!/bin/bash
# setup SR-IOV virtual functions for packet mirroring

set -e

INTERFACE="${1:-enp193s0f1np1}"
NUM_VFS=2

echo "========================================="
echo "SR-IOV setup for XDP"
echo "========================================="
echo "iface: $INTERFACE"
echo "virtual functions: $NUM_VFS"
echo ""

# find the PCI device for the interface
PCI_DEVICE=$(readlink -f /sys/class/net/$INTERFACE/device | xargs basename)
echo "PCI device: $PCI_DEVICE"

# check if SR-IOV is supported
if [ ! -f "/sys/class/net/$INTERFACE/device/sriov_totalvfs" ]; then
    echo "error: NIC does not support SR-IOV"
    exit 1
fi

MAX_VFS=$(cat /sys/class/net/$INTERFACE/device/sriov_totalvfs)
echo "maximum VFs supported: $MAX_VFS"

if [ $NUM_VFS -gt $MAX_VFS ]; then
    echo "error: requested $NUM_VFS VFs but NIC only supports $MAX_VFS"
    exit 1
fi

# disable existing VFs first
echo "disabling existing VFs..."
echo 0 | sudo tee /sys/class/net/$INTERFACE/device/sriov_numvfs > /dev/null

# enable SR-IOV with requested number of VFs
echo "enabling $NUM_VFS virtual functions..."
echo $NUM_VFS | sudo tee /sys/class/net/$INTERFACE/device/sriov_numvfs > /dev/null

# wait for VFs to be created
sleep 2

# list created VFs
echo ""
echo "created virtual functions:"
ip link show | grep -A1 "vf 0\|vf 1"

# get VF interface names
VF0=$(ls /sys/class/net/$INTERFACE/device/virtfn0/net/ 2>/dev/null || echo "")
VF1=$(ls /sys/class/net/$INTERFACE/device/virtfn1/net/ 2>/dev/null || echo "")

echo ""
echo "VF ifaces:"
echo "  VF0: $VF0 (will be used for XDP app)"
echo "  VF1: $VF1 (will be used for kernel)"

# Bring up the VF interfaces
if [ -n "$VF0" ]; then
    echo "bringing up $VF0..."
    sudo ip link set $VF0 up
fi

if [ -n "$VF1" ]; then
    echo "bringing up $VF1..."
    sudo ip link set $VF1 up
fi

# configure MAC addresses (optional, for isolation)
# sudo ip link set $INTERFACE vf 0 mac aa:bb:cc:dd:ee:00
# sudo ip link set $INTERFACE vf 1 mac aa:bb:cc:dd:ee:01

# try to enable VF mirroring (may not work on all models)
echo ""
echo "attempting to configure VF mirroring (may not be supported)..."
if ip link set $INTERFACE vf 0 mirror 1 2>/dev/null; then
    echo "✅ VF mirroring enabled: VF0 will mirror traffic to VF1"
else
    echo "⚠️  VF mirroring not supported by this NIC"
fi

echo ""
echo "========================================="
echo "SR-IOV setup complete!"
echo "========================================="
echo ""

