# freight-tracker — Logistics and Freight Tracking
        
A composed WebAssembly application showcasing Logistics and Freight Tracking.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['dispatcher', 'driver']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`

## Run
```bash
just compose-freight-tracker
just host-freight-tracker
just e2e-freight-tracker
```
