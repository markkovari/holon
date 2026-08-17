// A stub for `audit:log/types`, which is a TYPES-ONLY interface.
//
// `audit-log` imports it, so a transpiled build emits `import "audit:log/types"`
// — a bare specifier Node reads as a URL scheme and rejects outright:
//
//   ERR_UNSUPPORTED_ESM_URL_SCHEME: Received protocol 'audit:'
//
// Nothing is ever called through it. A WIT interface carrying only type
// definitions has no functions to shim, and `just _derive audit-log` says as much
// on its own: "nothing built exports audit:log/types@0.1.0". Composition cannot
// satisfy it because there is nothing to compose — so the import is resolved
// here, at the transpile boundary, with a module that deliberately has nothing in
// it.
//
// The real fix is upstream: a types-only interface should not appear as a runtime
// import at all. That is a change to the audit WIT and every component built
// against it, which is not something a benchmark run should be doing.
export {};
