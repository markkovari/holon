# photosocial — social photo sharing with AI critique & RBAC attributes

A social photography platform. **Creators** upload photographic artwork, **AI** automatically generates evocative descriptions and aesthetic critiques, the **community** upvotes and downvotes submissions, and evaluates photos along **admin-defined scoring attributes** (*Perspective*, *Lighting*, *Creativity*, *Composition*, etc.) with real-time aggregate mean computations.

Same shape as the other showcases: one **`photosocial-domain`** HTTP component exporting `wasi:http` and importing only standard WIT capability contracts:
- Composed **`auth-guard`** (`auth:identity`) for accounts, sessions, and role-based access control (RBAC).
- **`records:store`** for photos, attributes, votes, and ratings.
- **`llm:inference`** for automated vision/narrative critique and tag generation.
- **`wasi:random`** and **`wasi:clocks`** for IDs and timing.

No bespoke database or vendor-locked auth. The frontend is a modern, responsive Single-Page Application (SPA) with full gallery feed, AI critique badge, photo detail modal with attribute sliders, and an Admin Studio.

![PhotoSocial social photo sharing application: Admin signs in, configures custom evaluation criteria (Storytelling & Mood) in the Admin Studio, a creator uploads a photo, AI generates an automated critique and narrative, and community voters upvote and score attributes on interactive sliders, updating the live community leaderboard. A live Playwright screencast of the running application.](../media/photosocial.gif)

## Key Capabilities

1. **Automated AI Photo Critique**:
   - On upload, `llm:inference` evaluates the image and metadata, generating a structured critique across **Lighting**, **Perspective**, and **Creativity**, alongside automated aesthetic tags (`golden-hour`, `bokeh`, `street`, `candid`).
2. **RBAC Attribute Governance**:
   - Only authenticated users with the `admin` role can define or delete evaluation attributes (e.g. *Perspective*, *Lighting*, *Creativity*, *Composition*, *Storytelling*).
   - Regular users attempting to mutate attributes are refused with **`403 Forbidden`**.
3. **Multi-Dimensional Community Scoring**:
   - Users rate photos across all active admin attributes using interactive 1..=10 sliders.
   - Aggregate arithmetic mean scores (`avg`) and rating counts (`count`) are computed in real time.
4. **Upvoting & Downvoting**:
   - Net score tracking (`upvotes - downvotes`) with per-user deduplication.

## Data Model

- **`photos`** — `{ id, title, image_url, image_data, author, author_name, description, ai_narrative, ai_critique, ai_tags, upvotes, downvotes, score, created_at }`.
- **`attributes`** — `{ id, name, description, weight, min_score, max_score, created_by, created_at }` (RBAC: Admin-only).
- **`votes`** — `{ id: "{photo_id}_{user_id}", photo_id, user_id, value: 1 | -1, created_at }`.
- **`ratings`** — `{ id: "{photo_id}_{attr_id}_{user_id}", photo_id, attribute_id, user_id, score, created_at }`.

## API Routes

```
GET    /                                      -> Serves the Single-Page Application (SPA)
POST   /api/register                          -> { email, password, role: "admin" | "user" }
POST   /api/login                             -> { email, password } -> { access_token, roles }
POST   /api/logout                            -> Revokes active session
GET    /api/me                                -> Profile & role introspection

GET    /api/attributes                        -> List all active evaluation attributes
POST   /api/admin/attributes                  -> Create new attribute (Admin RBAC required: 403 otherwise)
DELETE /api/admin/attributes/{id}             -> Remove attribute (Admin RBAC required)

GET    /api/photos?sort=latest|top            -> List photos with score, AI critique preview & attribute averages
POST   /api/photos                            -> Upload photo (triggers automated AI critique)
GET    /api/photos/{id}                       -> Detailed photo view with full critique and rating breakdown
POST   /api/photos/{id}/ai-analyze            -> Re-run AI analysis

POST   /api/photos/{id}/vote                  -> { value: 1 | -1 | 0 } (upvote/downvote)
POST   /api/photos/{id}/rate                  -> { ratings: [{ attribute_id, score }] }
GET    /api/photos/{id}/my-ratings            -> View caller's current vote and attribute scores
```

## Run It

```bash
# 1. Compose the wasm component from contracts (auth-guard + record-store + llm-inference)
just compose-photosocial

# 2. Run on the native Rust host + serve on :3055
just host-photosocial

# 3. Run the automated end-to-end integration test suite
just e2e-photosocial

# 4. Record screencast and regenerate the GIF
just screencast-photosocial
```
