# device-radar — IoT Network Radar
        
A composed WebAssembly application showcasing a scanner for Bluetooth, WiFi, Zigbee, Thread, and Matter devices in range.

## Features
- **Capability Export**: A custom `iot:scanner/scanner` contract and component that returns mock hardware network states.
- **Auth**: `auth:identity` for login/registration
- **KV Store**: `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['admin', 'viewer']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/devices` -> Returns `[{id, name, protocol, rssi, connected}]`

## Run
```bash
just compose-device-radar
just host-device-radar
just e2e-device-radar
```
