# jco-markdown

Exercises the `md:render` WebAssembly component **in-process** via
[jco](https://github.com/bytecodealliance/jco) — no server, no host runtime.

`md:render` is a safe Markdown-to-HTML renderer covering a **CommonMark
subset** (headings, emphasis, inline code, links, code fences, lists). It is
**pure-compute**: the component needs no shims or capability imports, so jco
transpiles it directly to JS.

## Safety guarantee

The renderer is designed to produce HTML that is safe to embed in a page from
untrusted Markdown:

- **Raw HTML is escaped** — `<script>` and friends come out as `&lt;script&gt;`,
  never as live markup.
- **Unsafe link schemes are stripped** — `javascript:` and `data:` URLs are
  dropped, so `[click](javascript:alert(1))` cannot produce an executable href.

The test suite asserts both of these as security-critical invariants.

## Exports

Package `md:render`, interface `renderer`:

- `toHtml(markdown: string) -> string` — render with safe defaults.
- `toHtmlWith(markdown: string, opts: { hardBreaks, safeLinks }) -> string` —
  `safeLinks` adds `rel="nofollow"` and `target="_blank"` to links.
- `toText(markdown: string) -> string` — plain text, all formatting stripped.

## Run

```bash
npm install
npm test
```

`npm test` transpiles `markdown.wasm` into `gen/` with jco, then runs the test
suite with `tsx --test`.
