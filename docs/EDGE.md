# Edge & Embedded Setup Guide

*Deploying WebAssembly capability components to ultra-low-power, resource-constrained ARM boards.*

Holon's single-binary host architecture (`holon-host`) and `wasm32-wasip2` component model allow modern cloud-native architectures to run directly on micro-boards where Docker, Kubernetes, or containerd immediately exhaust memory.

---

## 📐 Verified Hardware Profiles

We actively test and verify Holon on bare physical hardware:

| Device | Dimensions | Weight | Power Draw | CPU | RAM | Measured Holon RSS |
|---|---|---|---|---|---|---|
| **Raspberry Pi Zero 2 W** | **65 × 30 × 5 mm** | 15 g | ~1.2 W (active) | 4× Cortex-A53 @ 1.0 GHz (64-bit) | 512 MB LPDDR2 | **~18–25 MB** |
| **Raspberry Pi 5** | **85 × 56 × 17 mm** | 46 g | ~5.0 W (typical) | 4× Cortex-A76 @ 2.4 GHz (64-bit) | 4 GB – 16 GB | **~30–45 MB** |

## 🔬 Complete Hardware Technical Specifications (Tech Specs)

Detailed engineering dimensions and hardware capabilities of verified edge devices:

| Technical Dimension | Raspberry Pi Zero 2 W | Raspberry Pi 5 |
|---|---|---|
| **SoC** | Broadcom BCM2710A1 | Broadcom BCM2712 |
| **CPU Architecture** | Quad-core ARM Cortex-A53 (ARMv8-A, 64-bit) | Quad-core ARM Cortex-A76 (ARMv8.2-A, 64-bit) |
| **CPU Clock Speed** | 1.0 GHz (downclocked for passive thermal budget) | 2.4 GHz |
| **CPU Cache** | 32 KB L1I / 32 KB L1D per core, 512 KB shared L2 | 64 KB L1I / 64 KB L1D per core, 512 KB L2 per core, 2 MB shared L3 |
| **System Memory (RAM)** | 512 MB LPDDR2 SDRAM (shared with VideoCore IV GPU) | 4 GB, 8 GB, or 16 GB LPDDR4X-4267 SDRAM |
| **Memory Bandwidth** | ~3.2 GB/s (32-bit single-channel) | ~17.0 GB/s (32-bit dual-channel) |
| **Storage Subsystem** | MicroSD slot (SDR50 / UHS-I via SDIO bus) | MicroSD (SDR104) + PCIe 2.0 x1 connector (M.2 NVMe SSD) |
| **Wi-Fi Connectivity** | 2.4 GHz 802.11 b/g/n (Broadcom BCM43436, single-antenna) | 2.4 GHz & 5.0 GHz 802.11ac (Wi-Fi 5, dual-band) |
| **Bluetooth** | Bluetooth 4.2, Bluetooth Low Energy (BLE) | Bluetooth 5.0, Bluetooth Low Energy (BLE) |
| **USB Controller** | Synopsys DWC2 USB 2.0 OTG (480 Mbps, device/host modes) | 2× USB 3.0 (5 Gbps), 2× USB 2.0 via RP1 I/O southbridge |
| **I/O Expansion** | 40-pin GPIO header (3.3V logic, I2C, SPI, UART, PWM, I2S) | 40-pin GPIO header + 2× 4-lane MIPI camera/display transceivers |
| **Power Supply** | 5V / 2.5A DC via micro-USB | 5V / 5A DC via USB-C (Power Delivery) |
| **Active Power Consumption** | **~0.7 W idle, ~1.2 W – 2.0 W active load** | ~3.0 W idle, ~5.0 W – 12.0 W active load |
| **Physical Dimensions** | 65.0 mm × 30.0 mm × 5.2 mm (2.56″ × 1.18″ × 0.20″) | 85.0 mm × 56.0 mm × 17.0 mm (3.35″ × 2.20″ × 0.67″) |
| **Bare Board Weight** | 15 grams (0.53 oz) | 46 grams (1.62 oz) |
| **Thermal Dissipation** | Pure passive cooling (no fan needed, throttles at 80°C) | Active cooler recommended under sustained loads |

---

## 📊 Holon Runtime Resource Envelope & Sizing Limits

The software resource envelope measured on the Pi Zero 2 W under Debian 13 (`aarch64`):

| Metric / Parameter | Value / Limit | Description |
|---|---|---|
| **Host Binary Size** | ~32 MB (stripped static ELF) | `holon-host` (includes complete Wasmtime 45 engine and hyper HTTP stack) |
| **Host Idle RSS** | **18–25 MB** | Total resident set size with `--no-pool` on-demand allocator |
| **Base OS Memory** | ~140–160 MB | Debian 13 Lite 64-bit with systemd, NetworkManager, and cloud-init |
| **Available User Memory** | **~230–270 MB** | Free RAM available for tenant instances and file buffers |
| **Default Per-Instance Cap** | 64 MiB (`--mem-cap-mb 64`) | Enforced linear memory ceiling per tenant instance |
| **Execution Slice Quantum** | 50 ms (`--slice-ms 50`) | Yielding timeslice for multi-tenant fairness |
| **Cold Start Latency** | **&lt;5 ms** | Time from HTTP request arrival to WASM execution |
| **Typical App Artifact Size** | **908 KB** (`link_shortener.composed.wasm`) | Self-contained WebAssembly binary with all composed dependencies |
| **Concurrent HTTP Capacity** | 20–50 concurrent streams | Bounded by `--max-concurrent` before swap involvement |

---

## ⚡ The Golden Rule

> [!IMPORTANT]
> **Build on your workstation (Mac/Linux), run on the Pi.**
>
> Compiling 200+ Rust crates and running `wac` dependency graphs will quickly exhaust 512 MB of RAM. Instead, compile and compose on your laptop in seconds — which emits a tiny, portable `.composed.wasm` artifact (typically under 1 MB) — and ship it to the Pi.

---

## 🛠️ Step-by-Step Setup Walkthrough

### 1. Flash 64-Bit OS (Essential)
Wasmtime and `holon-host` require a 64-bit architecture (`aarch64`). Even though the Pi Zero 2 W CPU is 64-bit, the standard 32-bit Raspbian image will not work.

1. Insert your microSD card into your workstation.
2. In **Raspberry Pi Imager**:
   - Device: `Raspberry Pi Zero 2 W`
   - OS: `Raspberry Pi OS (other)` → **`Raspberry Pi OS Lite (64-bit)`**
3. In OS Customisation (Gear icon):
   - Set hostname: `rpizero2`
   - Set username: `markkovari`
   - Add your SSH public key
   - Configure your local Wi-Fi SSID and password

### 2. Enable USB Gadget Mode (Optional but Recommended)
If you want to connect directly to the Pi with a single micro-USB cable without relying on Wi-Fi:
1. Open the mounted `bootfs` partition on your card.
2. In `config.txt`, append under `[all]`:
   ```ini
   dtoverlay=dwc2
   ```
3. Plug the micro-USB cable into the **inner `USB` port** (the middle data port, not the corner power-only port) and connect it to your Mac. The Pi will boot and appear as a direct USB Ethernet network interface (`usb0`).

### 3. Install the Holon Static Binaries on the Pi
SSH into the Pi:
```bash
ssh markkovari@rpizero2.local
```
Run our automated multi-platform installer:
```bash
curl -fsSL https://raw.githubusercontent.com/markkovari/holon/main/install.sh | sh
```
This detects `linux-aarch64` and installs the binaries into `~/.holon/bin/`.

Verify the runtime:
```bash
~/.holon/bin/holon-host --help
```

---

## ⚠️ Memory Optimization: The `--no-pool` Flag

> [!WARNING]
> On 512 MB RAM boards like the **Pi Zero 2 W**, always run `holon-host` with `--no-pool`.

Wasmtime's default *pooling allocator* pre-allocates a multi-gigabyte virtual memory address space for lightning-fast slot reuse. On 512 MB Linux kernels without aggressive swap overcommit, this mmap reservation fails with `Cannot allocate memory (os error 12)`.

Passing `--no-pool` switches Wasmtime to its **on-demand allocator**:
- Eliminates the virtual address space reservation.
- Keeps idle memory footprint to **~18–25 MB RSS**.
- Leaves over **220 MB of free physical RAM** on a 512 MB board.

---

## 🚀 Deploying an Application as a Systemd Service

### Step 1: Compose on your Workstation
On your Mac/Linux laptop:
```bash
# Compose any app into a single root .wasm via WAC (e.g., link-shortener is only 908 KB)
cargo xtask compose link-shortener

# Copy the composed artifact to the Pi
scp components/target/link_shortener.composed.wasm markkovari@rpizero2.local:~/app.wasm
```

### Step 2: Configure systemd on the Pi
On the Pi, create `/etc/systemd/system/holon-app.service`:
```ini
[Unit]
Description=Holon Wasmtime Host
After=network.target

[Service]
Type=simple
User=markkovari
ExecStart=/home/markkovari/.holon/bin/holon-host --component /home/markkovari/app.wasm --addr 0.0.0.0:8080 --kv memory --no-pool
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

Enable and start the service:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now holon-app.service
```

Check service status:
```bash
systemctl status holon-app.service --no-pager
```

### Step 3: Test the Live Endpoint
From your workstation or phone:
```bash
# Health check / discovery
curl http://rpizero2.local:8080/

# Create a shortened URL
curl -X POST http://rpizero2.local:8080/api/links \
  -H "Content-Type: application/json" \
  -d '{"url": "https://github.com/markkovari/holon", "slug": "holon"}'

# Test redirect
curl -i http://rpizero2.local:8080/holon
```

---

## 🌐 Edge Architecture Patterns

### 1. Resilient Offline Webhook Buffer
Deploy `holon-host` with `--kv sqlite`. When an edge node loses uplink connectivity, incoming HTTP webhooks are stored durably to local SQLite and drained when uplink returns.

### 2. 24/7 Low-Power Pulse Relay (`holon-relay`)
A Pi Zero 2 W draws ~1 W of wall power. Running `holon-relay` on the Pi provides a dedicated, battery-backupable timer and cron event source (`sched:timer`, `cron:expr`) driving schedules across your homelab or cloud lattice over Tailscale.

### 3. Edge Leaf in a NATS Lattice (Lane 2)
Join the Pi to your central lattice by pointing `holon-host` at your NATS server:
```bash
holon-host --lattice-nats nats://nats.tailnet.internal:4222 --node rpizero2 --no-pool
```
The central reconciler can orchestrate, deploy, start, and stop components on the Pi remotely without needing SSH access.
