# ADR-0116 — One crate writes and reads the model wire formats

*Anthropic's Messages API and the OpenAI-compatible chat API were each written out by hand in eight
places, and the places disagreed about what a token count means.*

**Status: accepted, and built one consumer at a time.** [`crates/holon-llm`](../../crates/holon-llm/)
is the crate; `comp-agentd` is the first thing built on it, and the order of the rest is below.
Changes no wire format and no behaviour a caller can see: each move keeps the tests of the code it
replaces.

## The problem

Anything in this repository that talks to a model builds a JSON request for one of two dialects and reads
a JSON reply, and each of them did it itself: `comp-agentd` (streaming, native tool calls),
`agent-runtime` (text and pictures, a JSON protocol for tools on top), the `anthropic-provider` and
`openai-provider` components, `anthropic-vision`, `photo-critic`, `contrast-audit`, and the console's
`fm.rs`. Eight implementations of about the same two things, with the differences that come from being
written at different times and not from any decision:

- **What a token count is.** `anthropic-provider` adds the cached prefix and the cache writes into
  `prompt_tokens` ("else a cached run looks nearly free and the wallet under-counts"). `comp-agentd` keeps
  the four numbers apart because they are priced apart. `agent-runtime` reads `input_tokens` and nothing
  else. For OpenAI, `comp-agentd` treats `cached_tokens` as a subset of `prompt_tokens` and the others
  ignore it. All four are right for their caller and none says so.
- **What a failure looks like.** Anthropic answers a throttled request with `200` and
  `{"type":"error",…}`; only the provider component checks for it by name, and `agent-runtime` reports it
  as an unexpected response. A reasoning model can spend its whole budget
  thinking and return no text: the provider component and `agent-runtime` each wrote their own sentence for
  it, and the daemon wrote none.
- **Which shape a message takes.** A plain turn is a string in one place and a block list in another;
  roles are merged to alternate in one place and not in the others; the order of a picture and its words
  differs between the dialects and was decided per caller.
- **The part that is not data.** A new field (prompt caching, a new stop reason, a new block type) has to
  be taught eight times, and a component compiled to wasm cannot share Rust with a daemon unless the
  shared part is pure.

## The decision

**One crate, pure, built for both targets.** `holon-llm` is data in and data out. One request is written
once (`Request`: model, system prompt, messages of typed parts — text, picture, tool call, tool result —
tools, `max_tokens`, `temperature`, `stop`, `seed`, `stream`, `cache`) and encoded for either dialect
(`anthropic::body`, `openai::body`). A reply is read into one shape (`Reply`: text, tool calls, the
provider's own finish reason, model, usage) from a whole response (`anthropic::parse`, `openai::parse`) or
assembled from a stream (`anthropic::Stream`, `openai::Stream`, fed by `sse::take_frames`). It has no
I/O and no async runtime, so a native daemon sends the bytes with `reqwest` and a wasm32-wasip2
component with `wasi:http`, and both build and read them the same way.

**Usage is four numbers, and the prompt is their sum.** `Usage { input, output, cache_read, cache_write }`,
with `total_input()`. Anthropic's counts beside `input_tokens` and OpenAI's subset of `prompt_tokens` are
each normalised into it, so a caller that prices apart reads the four and a caller that wants the prompt
reads one number, and neither has to know which dialect it was.

**What the caller keeps.** URLs and headers (a base plus a path, and keys are secrets the crate never
sees), retries, what an HTTP status means, what an empty reply means, and what a model costs. The crate
reports what the model said, including that it ran out of tokens (`Reply::hit_token_limit`), and the caller
decides whether that is an error. Where callers differ on policy they differ in a `Request` field, not in
a fork of the encoder: `max_tokens` (Anthropic's falls back to a default, because without one the API
answers 400), `cache`, `temperature` as the JSON number the server reads, and the order of the parts.

**Two dialect rules are now written down once.** A message that is a single piece of text is the string
form and anything else is a list of blocks. Consecutive user turns are one, because Anthropic wants roles to
alternate and a tool's answer followed by the next prompt is two. Unparseable tool arguments are kept,
under `_raw`, so the tool fails on them visibly.

**The order is the consumer with the most to ask of it first.**

1. `comp-agentd`, which streams and uses native tool calls in both dialects: the crate is its encoders,
   its stream assemblers and its SSE framing, moved with their tests. Its session record, `Msg`, stays
   its own and persisted as it was; the crate's `Call` and `ToolResult` have the same shape on disk.
2. `agent-runtime`: text and pictures, a whole response.
3. The provider components, `anthropic-provider` and `openai-provider`, whose codecs were already
   pure.
4. The vision components and the console.

## What this does not do

It is not an LLM client library: nothing here opens a socket. It does not run tools, price a call or
choose a model. It does not cover embeddings, which are a different endpoint with a different shape and
live with `openai-provider`; or thinking blocks, which are skipped on read as the provider component
already did. And it does not merge `agent-runtime` and `comp-agentd`, which is a separate decision with
a larger blast radius.

## Alternatives

**Share the daemon's `model.rs`.** It is native-only (`reqwest`, `anyhow`, `tokio`) and could not be
linked into a component.

**Generate the types from the providers' OpenAPI documents.** The two dialects are small, stable and
only partly typed in those documents, and the part that matters here — what to do with a cached token or
a thinking block — is judgement, not shape.

**Leave it.** Cheap until the next provider field, which is eight edits and an unknown number of
callers that got it wrong.
