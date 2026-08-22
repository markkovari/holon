# smart-home — IoT Home Automation
        
A composed WebAssembly application showcasing IoT Home Automation.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['admin', 'guest']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`

## Run
```bash
just compose-smart-home
just host-smart-home
just e2e-smart-home
```
