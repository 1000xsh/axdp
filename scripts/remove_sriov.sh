#!/bin/bash
# remove SR-IOV vfs

set -e

INTERFACE="${1:-enp193s0f1np1}"

echo "========================================="
echo "remove SR-IOV"
echo "========================================="
echo "iface: $INTERFACE"
echo ""

# check if interface exists
if [ ! -d "/sys/class/net/$INTERFACE" ]; then
    echo "error: iface $INTERFACE not found"
    exit 1
fi

# check current VF count
CURRENT_VFS=$(cat /sys/class/net/$INTERFACE/device/sriov_numvfs 2>/dev/null || echo "0")
echo "current VFs: $CURRENT_VFS"

if [ "$CURRENT_VFS" = "0" ]; then
    echo "no VFs to remove"
    exit 0
fi

# list VFs before removal
echo ""
echo "current VFs:"
ip link show $INTERFACE | grep vf || true

# disable all VFs
echo ""
echo "disabling all VFs..."
echo 0 | sudo tee /sys/class/net/$INTERFACE/device/sriov_numvfs > /dev/null

echo ""
echo "✅ SR-IOV VFs removed successfully"
echo ""