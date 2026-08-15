# The clinic API — the interface both halves build against

One component serves all of it. Two parts write it: **owners-and-pets** and
**visits**. Neither may edit this file; if it is wrong or missing something, write
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

## Shared by both

- Unknown route → **404** `{"error":"not_found"}`.
- Malformed JSON → **400** `{"error":"invalid"}`.
- `GET /health` → **200** `{"ok":true}` (already written; do not change it).

## Storage

`records:store` collections: `owners`, `pets`, `visits`. Ids come from
`id:generate`. Records are JSON documents; the shape above is what a reader
expects to find in them.
