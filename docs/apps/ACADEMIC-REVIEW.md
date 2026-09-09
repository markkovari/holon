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
cargo xtask compose academic-review
cargo xtask host academic-review
cargo xtask compose academic-review && cargo test --manifest-path examples/academic-review/Cargo.toml
```
