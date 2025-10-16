#!/bin/bash
# remove flow bifurcation rules and restore default traffic routing

set -e

INTERFACE="${1:-enp193s0f1np1}"

echo "========================================="
echo "remove flow bifurcation"
echo "========================================="
echo "iface: $INTERFACE"
echo ""

if [ ! -d "/sys/class/net/$INTERFACE" ]; then
    echo "error: iface $INTERFACE not found"
    exit 1
fi

echo "current flow rules:"
sudo ethtool -n $INTERFACE 2>/dev/null || echo "no rules found"

echo ""
echo "removing all ntuple filters..."

# get all rule IDs and delete them
RULE_COUNT=$(ethtool -n $INTERFACE 2>/dev/null | grep "Filter:" | wc -l || echo "0")

if [ "$RULE_COUNT" -eq 0 ]; then
    echo "no rules to remove"
else
    echo "found $RULE_COUNT rules, removing..."
    ethtool -n $INTERFACE 2>/dev/null | grep "Filter:" | awk '{print $2}' | while read -r rule_id; do
        if sudo ethtool -N $INTERFACE delete $rule_id 2>/dev/null; then
            echo "  feleted rule $rule_id"
        else
            echo "  failed to delete rule $rule_id"
        fi
    done
fi

echo ""
echo "flow bifurcation rules removed"
echo ""
