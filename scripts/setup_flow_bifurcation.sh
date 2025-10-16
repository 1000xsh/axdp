#!/bin/bash
# setup flow bifurcation for zero-copy packet routing
# routes specific traffic to XDP queue, other traffic to kernel queues
# lower latency then SR-IOV

set -e

INTERFACE="${1:-enp193s0f1np1}"
XDP_QUEUE="${2:-0}"
TURBINE_PORT_START="${3:-8000}"
TURBINE_PORT_END="${4:-8020}"

echo "========================================="
echo "flow bifurcation setup"
echo "========================================="
echo "interface: $INTERFACE"
echo "XDP queue: $XDP_QUEUE (for your app)"
echo "turbine ports: $TURBINE_PORT_START-$TURBINE_PORT_END"
echo ""

# check if interface exists
if [ ! -d "/sys/class/net/$INTERFACE" ]; then
    echo "error: interface $INTERFACE not found"
    exit 1
fi

# check NIC capabilities
DRIVER=$(ethtool -i $INTERFACE | grep "^driver:" | awk '{print $2}')
echo "driver: $DRIVER"

# get current number of queues
QUEUES=$(ethtool -l $INTERFACE 2>/dev/null | grep "Combined" | head -1 | awk '{print $2}')
echo "available queues: $QUEUES"

if [ -z "$QUEUES" ] || [ "$QUEUES" -lt 2 ]; then
    echo "warning: NIC has fewer than 2 queues, flow bifurcation may not work optimally"
fi

echo ""
echo "configuring flow rules..."

# clear existing flow rules (optional)
echo "clearing existing ntuple filters..."
RULE_COUNT=$(ethtool -n $INTERFACE 2>/dev/null | grep "Filter:" | wc -l || echo "0")
if [ "$RULE_COUNT" -gt 0 ]; then
    echo "found $RULE_COUNT existing rules, clearing..."
    ethtool -n $INTERFACE 2>/dev/null | grep "Filter:" | awk '{print $2}' | while read -r rule_id; do
        sudo ethtool -N $INTERFACE delete $rule_id 2>/dev/null || true
    done
fi

# enable ntuple filters (required for flow steering)
echo "enabling ntuple filters..."
sudo ethtool -K $INTERFACE ntuple on 2>/dev/null || {
    echo "warning: failed to enable ntuple filters"
    echo "your NIC may not support flow bifurcation"
    echo "continuing anyway..."
}

echo ""
echo "setting up flow rules for solana turbine traffic..."

# route traffic (UDP ports 8000, rpc/vali cli args, tpu port) to XDP queue
for port in $(seq $TURBINE_PORT_START $TURBINE_PORT_END); do
    if sudo ethtool -N $INTERFACE flow-type udp4 dst-port $port action $XDP_QUEUE 2>/dev/null; then
        echo "✅ port $port -> queue $XDP_QUEUE"
    else
        echo "⚠️  failed to set rule for port $port (may not be supported)"
        break
    fi
done

# alternative: use a single range rule if NIC supports it
# sudo ethtool -N $INTERFACE flow-type udp4 dst-port $TURBINE_PORT_START action $XDP_QUEUE

echo ""
echo "verifying flow rules..."
sudo ethtool -n $INTERFACE | head -20

echo ""
echo "========================================="
echo "flow bifurcation setup complete!"
echo "========================================="
echo ""
echo "traffic routing:"
echo "  * UDP ports $TURBINE_PORT_START-$TURBINE_PORT_END -> queue $XDP_QUEUE (XDP app)"
echo "  * all other traffic -> other queues (kernel)"
echo ""
echo "next steps:"
echo "1. run $XDP_QUEUE:"
echo "   sudo ./target/release/examples/relay \\"
echo "        --interface $INTERFACE \\"
echo "        --queue $XDP_QUEUE \\"
echo "        --zero-copy \\"
echo "        --cpu 2 \\"
echo "        --decoder-cpu 3"
echo ""
echo "to remove flow rules: ./remove_flow_bifurcation.sh $INTERFACE"
echo ""
