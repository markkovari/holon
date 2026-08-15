# The clinic API — the interface both halves build against

One component serves all of it. Four parts write it: **owners-and-pets**,
**visits**, **access-and-search** and **reports**. Neither may edit this file; if it is wrong or missing something, write
`CONTRACT-REQUEST.md` — first line the subject, the rest why — and the other part
will answer at the next generation.

All bodies are JSON. All ids are strings. Times are RFC3339 UTC (`2026-08-15T09:00:00Z`).

## Owners and pets — owned by the `owners-and-pets` part

```
POST   /api/owners           {name, email}            201 {id, name, email}
GET    /api/owners/{id}                               200 {id, name, email} | 404
GET    /api/owners?q=                                 200 {owners:[…]}   q matches name or email, case-insensitive
POST   /api/owners/{id}/pets {name, species, born}    201 {id, owner_id, name, species, born}
GET    /api/pets/{id}                                 200 {…} | 404
GET    /api/owners/{id}/pets                          200 {pets:[…]}
```

- An owner needs a non-empty `name` and an `email` containing `@` → otherwise **400**
  `{"error":"invalid"}`.
- A pet needs an owner that exists → otherwise **404**.
- `species` is one of `dog`, `cat`, `bird`, `other` → otherwise **400**.

## Visits — owned by the `visits` part

```
POST   /api/visits           {pet_id, vet, start, minutes}   201 {id, pet_id, vet, start, minutes}
GET    /api/visits/{id}                                      200 {…} | 404
GET    /api/visits?vet=&day=YYYY-MM-DD                       200 {visits:[…]} sorted by start, ascending
DELETE /api/visits/{id}                                      204 | 404
```

- `minutes` is 15, 30 or 60 → otherwise **400**.
- `pet_id` must exist → otherwise **404**.
- **A vet cannot be double-booked**: a visit whose `[start, start+minutes)` overlaps
  an existing visit for the same `vet` is **409** `{"error":"clash","with":"<visit id>"}`.
  Touching at the boundary is not an overlap — 09:00+30 and 09:30+30 both fit.
- A deleted visit frees its slot.

## Staff access and pet search — owned by the `access-and-search` part

```
POST   /api/staff            {email, password}     201 {id, email} | 400 | 409
POST   /api/staff/login      {email, password}     200 {token} | 401
GET    /api/pets/search?q=   Bearer <token>        200 {pets:[{id, name, species, owner_id}]}
```

- Registering an email that already has an account is **409** `{"error":"taken"}`.
  A password under 8 characters is **400**.
- Wrong password, or an unknown email, is **401** `{"error":"unauthorized"}` — the
  same answer for both, so the endpoint does not say which emails exist.
- `GET /api/pets/search` without a valid bearer token is **401**. With one, it
  matches `q` against a pet's name and species, ranked, best first; `q` empty is
  a **400**.

**These three capabilities are already in the component's world and are bound at
compose time — use them.** `auth:identity/accounts` for register/login,
`auth:identity/session` for the token, `search:index/index` for the ranking. The
gate checks that the composed component imports all three, so hand-rolled password
hashing or a linear scan over pets fails even if the responses look right.

## Reports — owned by the `reports` part

```
GET /api/reports/visits.csv?day=YYYY-MM-DD      200 text/csv
GET /api/reports/summary?day=YYYY-MM-DD         200 {visits, minutes, by_vet:{}, by_species:{}}
```

- Both need `day`; missing or unparseable is a **400** `{"error":"invalid"}`.
- The CSV has a header row, exactly these columns in this order:
  `id,pet_id,pet_name,vet,start,minutes` — then one row per visit that day,
  sorted by `start` ascending. A day with no visits is the header alone.
- `summary` counts that day: `visits` (how many), `minutes` (their total),
  `by_vet` (vet → count) and `by_species` (the pet's species → count).

**Use `csv:codec` to format the CSV.** A pet's name may contain a comma — the
clinic has one called `Rex, Jr.` — and a field with a comma in it must come back
quoted, so the row still has six columns. The gate counts columns; joining with
commas fails it.

## Shared by all

- Unknown route → **404** `{"error":"not_found"}`.
- Malformed JSON → **400** `{"error":"invalid"}`.
- `GET /health` → **200** `{"ok":true}` (already written; do not change it).

## Storage

`records:store` collections: `owners`, `pets`, `visits`. Ids come from
`id:generate`. Records are JSON documents; the shape above is what a reader
expects to find in them.
