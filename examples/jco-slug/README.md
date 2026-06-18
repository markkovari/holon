# Embed slug-generate in-process via jco

The `slug:generate` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `slug.wasm` into JS; this example
calls its exported `generator` interface directly.

It is **pure compute**: the component imports only standard WASI (satisfied by
`@bytecodealliance/preview2-shim`), so there are **no host shims** — nothing to
map at transpile time.

```
slug.wasm                # the built component
test/
  slug.test.ts           # slugify / slugifyWith / uniquify
gen/                     # transpile output (gitignored)
```

## Run

```bash
npm install
npm run transpile        # slug.wasm -> gen/
npm test                 # behavioral checks
```

## API (`generator` interface)

```ts
import { generator as slug } from "./gen/slug.js";

slug.slugify("Hello, World!");                                  // "hello-world"
slug.slugify("Café déjà vu");                                   // "cafe-deja-vu" (transliterated)
slug.slugifyWith("a b c", { separator: "_", maxLength: 0 });    // "a_b_c"  (maxLength 0 = no limit)
slug.slugifyWith("one two three four", { separator: "-", maxLength: 8 }); // "one-two" (word-boundary truncation)
slug.uniquify("post", ["post", "post-2"]);                      // "post-3"
```
