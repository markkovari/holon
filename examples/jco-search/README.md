# Embed search-index in-process via jco

The `search:index` component running **inside the Node process** — no wasmCloud,
no NATS, no Elasticsearch. `jco transpile` turns `search_index.wasm` into JS;
this example calls its exported `index` interface directly.

`search:index` is a **TF-IDF inverted index built over a KV store**: documents
are tokenized, posting lists and document frequencies live as keyvalue records,
and `query` scores candidates by TF-IDF. It supports `any` / `all` term modes,
tag facets for filtering, and top-k limiting. For the long tail of apps whose
corpus comfortably fits a KV store, that's full-text search without standing up
a search cluster.

```
search_index.wasm   # the built component (copy of components/target/.../search_index.wasm)
src/
  keyvalue-shim.js   # host shim for wasi:keyvalue/store  (in-memory Map)
test/
  search.test.ts     # index / docCount / any+all modes / tags / remove / re-index
gen/                 # Map-backed transpile output     (gitignored)
```

## Run

```bash
npm install
npm run transpile         # search_index.wasm -> gen/
npm test                  # behavioral checks
```

The one non-standard import is mapped to a local shim at transpile time
(`wasi:clocks` is auto-shimmed by jco):

```
jco transpile search_index.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js
```

Swap the in-memory `Map` in `keyvalue-shim.js` for redis/sqlite/NATS to persist
the index; the component neither knows nor cares.

## Interface

```
index.indexDoc(id, text, tags)                 # add/replace a document
index.remove(id)                               # drop a document
index.query(query, mode, tags, limit) -> hits  # mode is 'any' | 'all'
index.docCount() -> bigint                      # documents currently indexed
```

`query` returns `{ id, score }[]` sorted by score descending. Tokens shorter
than 2 characters are dropped during tokenization.
