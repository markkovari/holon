# real-estate-escrow — Real Estate Escrow Management
        
A composed WebAssembly application showcasing Real Estate Escrow Management.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['agent', 'buyer', 'escrow_officer']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`

## Run
```bash
just compose-real-estate-escrow
just host-real-estate-escrow
just e2e-real-estate-escrow
```
