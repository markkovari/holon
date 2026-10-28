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

```text
Raspberry Pi Zero 2 W (Actual Physical Profile)
┌──────────────────────────────────────────────┐ ── 30 mm
│ [PWR] [USB-DATA]                     [ACT] █ │
│                                              │
│         Broadcom BCM2710A1 (64-bit)          │
│               512 MB LPDDR2                  │
│                                              │
│ [MicroSD]                          [MiniHDMI]│
└──────────────────────────────────────────────┘ ──
 ├─────────────────── 65 mm ──────────────────┤   Thickness: 5 mm
 Weight: 15 grams | Power Draw: ~1.2 W active | Host RSS: 18-25 MB
```

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
