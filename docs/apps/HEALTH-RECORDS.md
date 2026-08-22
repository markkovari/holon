# health-records — Electronic Health Records
        
A composed WebAssembly application showcasing Electronic Health Records.

## Features
- **Auth**: `auth:identity` for login/registration
- **KV Store & Records**: `records:store` for data, `wasi:keyvalue:store` for usage measurement
- **RBAC**: Roles `['doctor', 'patient']`

## API
- `POST /api/register`
- `POST /api/login`
- `GET /api/me`
- `GET /api/items`
- `POST /api/items`

## Run
```bash
just compose-health-records
just host-health-records
just e2e-health-records
```
