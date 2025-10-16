
### bios
VT-d (intel) or AMD-Vi (AMD)

### enable IOMMU in kernel** (add to boot parameters):
```bash
# intel:
intel_iommu=on iommu=pt

# AMD:
amd_iommu=on iommu=pt
```
### configure IP Addresses (if needed)

If you need the VFs to have IP addresses:

```bash
# VF0 (for your app)
sudo ip addr add 192.168.1.10/24 dev <VF0_NAME>

# VF1 (for kernel)
sudo ip addr add 192.168.1.11/24 dev <VF1_NAME>
```