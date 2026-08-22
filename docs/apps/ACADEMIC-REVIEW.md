# academic-review — Peer Review System
        
A composed WebAssembly application showcasing Peer Review System.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['editor', 'reviewer', 'author']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`

## Run
```bash
just compose-academic-review
just host-academic-review
just e2e-academic-review
```
