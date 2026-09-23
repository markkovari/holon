# smart-home — IoT Home Automation
        
A composed WebAssembly application showcasing IoT Home Automation.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: one role is checked, `admin`: a device's owner or an admin may toggle
  it, anyone else gets 403. Listing only ever returns the caller's own devices.

## API
- `POST /api/register`
- `POST /api/login`
- `POST /api/logout`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`
- `POST /api/items/{id}/toggle`

## Run
```bash
cargo xtask compose smart-home
cargo xtask host smart-home
cargo xtask compose smart-home && cargo test --manifest-path examples/smart-home/Cargo.toml
```
