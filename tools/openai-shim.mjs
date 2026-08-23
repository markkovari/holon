#!/usr/bin/env node
// A local `/v1/messages` endpoint backed by an OpenAI-compatible server.
//
// Same swap point as `tools/claude-shim.mjs`: `anthropic-provider` reads its base
// URL from `wasi:config`, so pointing `--anthropic-base-url` here runs the whole
// loop's inference against vLLM / llama.cpp / Ollama / LM Studio — anything that
// speaks `POST /v1/chat/completions`.
//
//   OPENAI_BASE=http://127.0.0.1:8000/v1 node tools/openai-shim.mjs
//   HOST=0.0.0.0 PORT=8787 OPENAI_BASE=... OPENAI_KEY=... node tools/openai-shim.mjs
//
// Translates only the subset `anthropic-provider` sends and parses — system as
// text blocks, text-only messages, max_tokens, temperature, stop_sequences — and
// maps `usage` back so the wallet reads real counts. No streaming, no tools.

import { createServer, request as httpRequest } from 'node:http'
import { request as httpsRequest } from 'node:https'

const PORT = Number(process.env.PORT || 8787)
const HOST = process.env.HOST || '127.0.0.1'
const BASE = (process.env.OPENAI_BASE || 'http://127.0.0.1:8000/v1').replace(/\/$/, '')
const KEY = process.env.OPENAI_KEY || ''
/** Overrides whatever model the caller asked for. Empty keeps the caller's. */
const MODEL = process.env.OPENAI_MODEL || ''
const TIMEOUT_MS = Number(process.env.OPENAI_TIMEOUT_MS || 540_000)
/**
 * Refuse a prompt estimated above this many tokens. 0 disables the check.
 *
 * A context overflow is the failure this setup actually hits, and it is the
 * expensive kind: measured on csatapaci, prefill runs at roughly 60-120 tok/s,
 * so a prompt that cannot fit spends TEN MINUTES being processed before anything
 * says so. Worse, a server that truncates instead of refusing spends the same ten
 * minutes and then answers confidently about a file it only half read.
 *
 * Estimated, not tokenized: the shim has no tokenizer and adding one would mean
 * shipping the model's vocab to guess at something the server measures exactly.
 * The estimate is deliberately PESSIMISTIC (3.6 B/tok against 3.95 measured on
 * this repo's Rust) so the guard trips slightly early rather than slightly late.
 */
const MAX_PROMPT_TOKENS = Number(process.env.OPENAI_MAX_PROMPT_TOKENS || 0)

const apiError = (type, message) => JSON.stringify({ type: 'error', error: { type, message } })

/** Anthropic Messages request -> OpenAI chat/completions request. */
function toOpenAI(body) {
  const text = (c) =>
    Array.isArray(c)
      ? c.filter((b) => b?.type === 'text').map((b) => b.text).join('')
      : String(c ?? '')

  const system = text(body.system)
  const messages = []
  if (system) messages.push({ role: 'system', content: system })
  for (const m of body.messages || []) {
    messages.push({ role: m.role === 'assistant' ? 'assistant' : 'user', content: text(m.content) })
  }

  const req = { model: MODEL || body.model, messages, stream: false }
  if (body.max_tokens) req.max_tokens = body.max_tokens
  if (body.temperature != null) req.temperature = body.temperature
  if (body.stop_sequences?.length) req.stop = body.stop_sequences
  return req
}

/**
 * POST to the model, retrying a TRANSPORT failure once.
 *
 * Uses `node:http` rather than `fetch`, and that is the whole point of this
 * function. Node's `fetch` is undici, whose DEFAULT `headersTimeout` is 300
 * seconds and is not reachable through the `fetch` API — `AbortSignal.timeout`
 * sets a different, longer budget and does nothing about it. A prefill queued
 * behind two others takes longer than that, so every branch of a three-branch
 * generation died at ~300s with `fetch failed`, twice each because of the retry
 * below, and the run reported `provider-down` on a server that was answering
 * other requests with 200 the whole time.
 *
 * `http.request` sets no header timeout unless asked, so the only clock left is
 * the one this shim chose.
 *
 * Measured: two branches calling one mlx server concurrently also produced a
 * genuine dropped connection, so the retry stays. Only transport errors retry,
 * and only once. An HTTP status is an ANSWER — a 400 means the request is wrong
 * and a retry sends the same wrong request, and a 500 from a model server
 * usually means it is out of memory, where a second copy of the same prompt is
 * the worst possible response.
 */
function postOnce(payload) {
  return new Promise((resolve, reject) => {
    const url = new URL(`${BASE}/chat/completions`)
    const body = Buffer.from(JSON.stringify(payload))
    const req = (url.protocol === 'https:' ? httpsRequest : httpRequest)(
      {
        hostname: url.hostname,
        port: url.port || (url.protocol === 'https:' ? 443 : 80),
        path: url.pathname + url.search,
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'content-length': body.length,
          ...(KEY ? { authorization: `Bearer ${KEY}` } : {}),
        },
      },
      (res) => {
        const chunks = []
        res.on('data', (c) => chunks.push(c))
        res.on('end', () => {
          const raw = Buffer.concat(chunks).toString()
          let json
          try {
            json = JSON.parse(raw)
          } catch {
            json = { error: raw.slice(0, 500) }
          }
          resolve({ ok: res.statusCode >= 200 && res.statusCode < 300, status: res.statusCode, json })
        })
      },
    )
    // The ONLY clock. Applies to the whole exchange, not to the headers alone.
    req.setTimeout(TIMEOUT_MS, () => req.destroy(new Error(`no response in ${TIMEOUT_MS}ms`)))
    req.on('error', reject)
    req.end(body)
  })
}

async function post(payload) {
  try {
    return await postOnce(payload)
  } catch (e) {
    // A timeout is NOT retried: it already waited the full budget, and a second
    // wait doubles a branch's slowest path for a call that was never coming.
    if (/no response in/.test(e.message)) throw e
    console.error(`[shim] transport failure (${e.message}) — retrying once`)
    return await postOnce(payload)
  }
}

/** OpenAI stop reason -> the two values `anthropic-provider` distinguishes. */
const stopReason = (f) => (f === 'length' ? 'max_tokens' : 'end_turn')

const server = createServer((req, res) => {
  const send = (status, payload) => {
    res.writeHead(status, { 'content-type': 'application/json' })
    res.end(payload)
  }

  if (req.method !== 'POST' || !req.url.startsWith('/v1/messages')) {
    send(404, apiError('not_found_error', `no route for ${req.method} ${req.url}`))
    return
  }

  let raw = ''
  req.on('data', (c) => (raw += c))
  req.on('end', async () => {
    let body
    try {
      body = JSON.parse(raw)
    } catch (e) {
      send(400, apiError('invalid_request_error', `bad json: ${e.message}`))
      return
    }

    // Before the request, not after: the whole point is to not spend the prefill.
    // 400 maps to InvalidRequest, which the driver treats as the CALLER's fault
    // and does not retry — correct here, because a retry of an oversized prompt
    // is the same prompt and fails the same way, ten minutes later.
    const estTok = Math.round(JSON.stringify(body).length / 3.6)
    if (MAX_PROMPT_TOKENS && estTok > MAX_PROMPT_TOKENS) {
      console.error(`[shim] REFUSED ~${estTok}tok prompt (ceiling ${MAX_PROMPT_TOKENS})`)
      send(
        400,
        apiError(
          'invalid_request_error',
          `prompt is ~${estTok} tokens, over the ${MAX_PROMPT_TOKENS} ceiling this server is ` +
            `configured for. Scope the goal's base_paths, or serve a longer-context model.`,
        ),
      )
      return
    }

    const started = Date.now()
    try {
      const upstream = await post(toOpenAI(body))
      const out = upstream.json
      if (!upstream.ok) {
        // Pass the upstream status through: 4xx is the caller's fault and 5xx is
        // retried by the driver, and collapsing them would lose that distinction.
        //
        // The prompt SIZE is logged with it, because the failure this shim sees
        // most is a context overflow, and the number that explains it is not in
        // the response — a failed call reports no usage, so the request is the
        // only place left to measure.
        const chars = JSON.stringify(body).length
        console.error(
          `[shim] upstream ${upstream.status} on a ${chars}B prompt (~${Math.round(chars / 3.6)} tok): ` +
            JSON.stringify(out).slice(0, 300),
        )
        send(upstream.status, apiError('api_error', JSON.stringify(out).slice(0, 500)))
        return
      }

      const choice = out.choices?.[0]
      const text = choice?.message?.content ?? ''
      // prompt_tokens is the real measure of what a run spends on context — the
      // whole reason a base tree gets scoped. Estimating it from the request body
      // would be guessing at the tokenizer; the server already counted it.
      console.error(
        `[shim] ${out.model || body.model} ${Date.now() - started}ms ` +
          `in=${out.usage?.prompt_tokens ?? 0}tok out=${out.usage?.completion_tokens ?? 0}tok ` +
          `${text.length}B <- ${(body.messages || []).length} turn(s)` +
          (choice?.finish_reason === 'length' ? ' TRUNCATED(max_tokens)' : ''),
      )
      send(
        200,
        JSON.stringify({
          type: 'message',
          role: 'assistant',
          model: out.model || MODEL || body.model,
          content: [{ type: 'text', text }],
          stop_reason: stopReason(choice?.finish_reason),
          usage: {
            input_tokens: out.usage?.prompt_tokens ?? 0,
            output_tokens: out.usage?.completion_tokens ?? 0,
          },
        }),
      )
    } catch (e) {
      console.error(`[shim] FAILED after ${Date.now() - started}ms: ${e.message}`)
      // 529 -> ProviderUnavailable, which the driver retries.
      send(529, apiError('overloaded_error', e.message))
    }
  })
})

// Self-check: `node tools/openai-shim.mjs --selftest`
if (process.argv.includes('--selftest')) {
  const o = toOpenAI({
    model: 'm',
    max_tokens: 10,
    system: [{ type: 'text', text: 'sys' }],
    messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }],
    stop_sequences: ['x'],
  })
  console.assert(o.messages[0].role === 'system' && o.messages[0].content === 'sys', 'system')
  console.assert(o.messages[1].content === 'hi', 'user turn')
  console.assert(o.max_tokens === 10 && o.stop[0] === 'x', 'options')
  console.assert(stopReason('length') === 'max_tokens' && stopReason('stop') === 'end_turn', 'stop')
  // The guard is arithmetic on the serialized body; assert the estimate tracks
  // what was measured on this repo (3.95 B/tok) from the pessimistic side.
  const est = (n) => Math.round(JSON.stringify({ x: 'y'.repeat(n) }).length / 3.6)
  console.assert(est(100_000) > 100_000 / 3.95, 'the estimate must not UNDER-count tokens')
  console.log('ok')
} else {
  server.listen(PORT, HOST, () => {
    console.error(`[shim] /v1/messages on http://${HOST}:${PORT} -> ${BASE} ${MODEL || '(caller model)'}`)
  })
}
