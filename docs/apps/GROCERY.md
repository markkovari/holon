# grocery — retail store & inventory (scan barcodes, dual-audience RBAC)

Two roles. A **shopper** scans item barcodes (EAN-13, EAN-8, UPC-A, Code-128)
with their device camera or uploaded barcode images, manages a live basket, and checks
out to deduct stock. An **admin** reviews low-stock warnings across the catalog,
adjusts inventory levels live via persistent updates, and registers new
products by scanning intake labels.

Built on the pure compute **`barcode:read`** WebAssembly capability component
([`components/barcode-read`](../../components/barcode-read)), with RBAC enforcement
on every endpoint. The frontend is a bundled **React + TypeScript + Vite** Single Page Application
embedded directly inside the WebAssembly component (`grocery-assets` exporting `ui:assets/files@0.1.0`),
running on `comp-host` with zero mock dictionaries.

![The grocery app on a phone: a shopper signs in, scans EAN-13 and UPC-A barcodes, adds items to cart and checks out; then an admin signs in, inspects low-stock alerts, adjusts live stock levels, and scans a Code-128 label to register a new product. A live recording of the running app at a mobile viewport.](../media/grocery.gif)

The shopper UI provides **Live Barcode Scanner** (supporting direct camera/file PNG upload and fixture sweeps) + **Catalog Aisles** + **Slide-out Basket** with real-time tax and total calculations. The admin UI provides **Low-Stock Alerts Banner** + **Intake Barcode Verification** + **Live Inventory Management** with quick stock adjustments (+/-).

## The capability model & RBAC Authentication

**Two distinct user groups** with real token-based session authentication backed by `wasi:keyvalue/store`:

| capability | shopper | admin | endpoint |
|---|:--:|:--:|---|
| scan / look up products | ✓ | ✓ | `POST /api/scan` |
| browse catalog | ✓ | ✓ | `GET /api/products` |
| manage cart / checkout | ✓ | ✗ (forbidden) | `POST /api/checkout` |
| view low-stock alerts | ✗ (403) | ✓ | `GET /api/alerts` |
| adjust inventory quantities | ✗ (403) | ✓ | `PATCH /api/products/:barcode/stock` |
| register new products | ✗ (403) | ✓ | `POST /api/products` |
| view system users list | ✗ (403) | ✓ | `GET /api/admin/users` |
| promote / demote user role | ✗ (403) | ✓ | `PATCH /api/admin/users/:id/role` |
| delete / deactivate user | ✗ (403) | ✓ | `DELETE /api/admin/users/:id` |

Every protected route verifies `Authorization: Bearer <token>`; role violations return `403 Forbidden` with `{ "error": "Forbidden: Requires 'admin' role" }`.

### Authentication & User Management Endpoints

- `POST /api/auth/register`: Register new account with role selection (`shopper` or `admin`). Returns session token and user profile.
- `POST /api/auth/login`: Authenticate credentials (`username`, `password`), returning session token.
- `GET /api/auth/me`: Inspect active user session.
- `POST /api/auth/logout`: Invalidate session token.
- `GET /api/admin/users`: Admin-only listing of all registered accounts.
- `PATCH /api/admin/users/:id/role`: Admin-only promotion or demotion of user roles.
- `DELETE /api/admin/users/:id`: Admin-only deletion of user accounts.

**Default Seeded Accounts:**
- Shopper: `shopper` / `shopper123` (Token: `tok_shopper`)
- Store Manager: `admin` / `admin123` (Token: `tok_admin`)

## Barcode Decoding Integration (Zero Mocking)

`grocery` directly exercises the **`barcode:read`** pure-compute component:
- **EAN-13**: Grocery box items (e.g. `4006381333931` Organic Extra Virgin Olive Oil)
- **EAN-8**: Compact European packaging (e.g. `96385074` Farm Fresh Whole Milk)
- **UPC-A**: North American products (e.g. `0036000291452` Artisan Sourdough Loaf)
- **Code-128**: Shelf & price labels (e.g. `SHELF-A17` Organic Hass Avocados)

All reads enforce check-digit validation in WebAssembly so damaged codes or misreads return `not-found` rather than false inventory matches.

## The data model

- **products**: `{barcode, symbology, name, category, price_cents, stock, icon, description}`
- **baskets**: per-user session map of `{barcode -> quantity}`
- **orders**: immutable record of purchase `{order_id, items, total_cents, timestamp}`

## Build & Run

```bash
# 1. Compose the grocery domain component with the React bundle and barcode reader:
just compose-grocery

# 2. Run the composed WebAssembly component on the native host (:3055):
just host-grocery
# Or: node examples/grocery/server.mjs

# 3. Open in browser:
open http://127.0.0.1:3055

# 4. Run the automated Playwright screencasts & generate GIFs:
# Mobile view (414x896):
node tools/screencast/grocery-mobile.mjs
bash tools/screencast/to-gif.sh tools/screencast/videos/grocery-mobile/*.webm docs/media/grocery-mobile.gif 400 12 1

# Tablet view (768x960):
node tools/screencast/grocery-tablet.mjs
bash tools/screencast/to-gif.sh tools/screencast/videos/grocery-tablet/*.webm docs/media/grocery-tablet.gif 640 12 1

# Desktop view (1200x800):
node tools/screencast/grocery-desktop.mjs
bash tools/screencast/to-gif.sh tools/screencast/videos/grocery-desktop/*.webm docs/media/grocery-desktop.gif 960 12 1

# Dual Showcase (Desktop + Mobile side-by-side):
node tools/screencast/preview.mjs
bash tools/screencast/to-gif.sh tools/screencast/videos/grocery-dual/*.webm docs/media/grocery.gif 1020 12 1
```
