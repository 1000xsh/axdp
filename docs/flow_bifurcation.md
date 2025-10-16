### packet flow

1. **packet arrives at NIC**
2. **hardware classifier checks**:
   - is it UDP port 8000-8020? -> queue 0
   - anything else? -> queues 1-N (RSS distribution)
3. **queue 0**: XDP_REDIRECT -> AF_XDP -> app
4. **other queues**: direct to kernel

## setup instructions

### 1. check NIC support

```bash
# check if your NIC supports flow steering
sudo ethtool -k enp193s0f1np1 | grep ntuple

# should show:
# ntuple-filters: on
```

### 2. run setup script

```bash
sudo ./setup_flow_bifurcation.sh enp193s0f1np1 0 8000 8020
```

parameters:
- iface name (enp193s0f1np1)
- XDP queue number (0)
- port start (8000)
- port end (8020)

### 3. verify rules

```bash
sudo ethtool -n enp193s0f1np1
```

you should see rules like:
```
Filter: 1
    Rule Type: UDP over IPv4
    Dst port: 8000
    Action: Direct to queue 0

Filter: 2
    Rule Type: UDP over IPv4
    Dst port: 8001
    Action: Direct to queue 0
...
```


### monitor active connections

```bash
# watch for rpc traffic
sudo tcpdump -i enp193s0f1np1 -nn 'udp and port >= 8000 and port <= 8020' -c 100
```

## port range rules

some NICs support range rules:

```bash
# single rule for port range (if supported)
sudo ethtool -N enp193s0f1np1 flow-type udp4 dst-port 8000 m 0xfff0 action 0
```

### 1. dedicated queues for XDP

```bash
# ensure queue 0 is dedicated
# set RSS to use queues 1-N only
sudo ethtool -X enp193s0f1np1 equal 1 start 1
```

### 2. queue size

```bash
# increase queue size for better throughput
sudo ethtool -G enp193s0f1np1 rx 4096 tx 4096
```

### 3. interrupt coalescing

```bash
# disable coalescing on XDP queue for lowest latency
sudo ethtool -C enp193s0f1np1 rx-usecs 0 rx-frames 1
```

## verification

```bash
# 1. check flow rules are active
sudo ethtool -n enp193s0f1np1

# 2. monitor queue 0 traffic
sudo ethtool -S enp193s0f1np1 | grep "rx_queue_0"

# 3. send test packet
echo "test" | nc -u localhost 8000

# 4. check if it hits queue 0
sudo ethtool -S enp193s0f1np1 | grep "rx_queue_0_packets"
```
