#### work in progress

### build ebpf
```chmod +x ./xdp-bpf/build_ebpf.sh```

```./xdp-bpf/build_ebpf.sh```

### run as sudo
```sudo env "PATH=$PATH" cargo run --example relay -- --interface IFACE --cpu 2 --zero-copy --queue 0```


#### references, the perf goat of solana
https://github.com/alessandrod/solana/tree/xdp-examples/xdp/src
