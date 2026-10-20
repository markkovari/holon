#!/usr/bin/env node
// A `/v1/messages` endpoint backed by an OpenAI-compatible `/v1/chat/completions` server.
//   OPENAI_BASE=http://127.0.0.1:8000/v1 node tools/openai-shim.mjs

import { createServer, request as httpRequest } from 'node:http'
import { request as httpsRequest } from 'node:https'

const PORT = Number(process.env.PORT || 8787)
const HOST = process.env.HOST || '127.0.0.1'
const BASE = (process.env.OPENAI_BASE || 'http://127.0.0.1:8000/v1').replace(/\/$/, '')
const KEY = process.env.OPENAI_KEY || ''
import { writeFileSync } from 'node:fs'

const DUMP_DIR = process.env.OPENAI_DUMP_DIR || ''
let dumped = 0
const MODEL = process.env.OPENAI_MODEL || ''
// Malformed JSON is fatal: an un-tuned request would get the model blamed for a config mistake.
const EXTRA = (() => {
  const raw = process.env.OPENAI_EXTRA_JSON
  if (!raw) return {}
  try {
    const v = JSON.parse(raw)
    if (v === null || typeof v !== 'object' || Array.isArray(v)) throw new Error('not an object')
    return v
  } catch (e) {
    console.error(`OPENAI_EXTRA_JSON is not a JSON object: ${e.message}`)
    process.exit(1)
  }
})()
const TIMEOUT_MS = Number(process.env.OPENAI_TIMEOUT_MS || 540_000)
// An oversized prompt costs ~10 minutes of prefill before failing; refuse it up front (0 = off).
// Estimated pessimistically at 3.6 B/tok (3.95 measured on this repo) so it trips early, not late.
const MAX_PROMPT_TOKENS = Number(process.env.OPENAI_MAX_PROMPT_TOKENS || 0)

const apiError = (type, message) => JSON.stringify({ type: 'error', error: { type, message } })

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
  // Last, so server-specific knobs override the translation, e.g.
  //   OPENAI_EXTRA_JSON='{"chat_template_kwargs":{"enable_thinking":false}}'
  Object.assign(req, EXTRA)
  if (DUMP_DIR) {
    try {
      const f = `${DUMP_DIR}/req-${String(++dumped).padStart(3, '0')}.json`
      writeFileSync(f, JSON.stringify(req, null, 2))
    } catch (e) {
      console.error(`[shim] could not dump: ${e.message}`)
    }
  }
  return req
}

// `node:http`, not `fetch`: undici's fixed 300s `headersTimeout` kills a queued prefill.
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
    req.setTimeout(TIMEOUT_MS, () => req.destroy(new Error(`no response in ${TIMEOUT_MS}ms`)))
    req.on('error', reject)
    req.end(body)
  })
}

// Only transport errors retry, once: an HTTP status is an answer, and a timeout already spent the budget.
async function post(payload) {
  try {
    return await postOnce(payload)
  } catch (e) {
    if (/no response in/.test(e.message)) throw e
    console.error(`[shim] transport failure (${e.message}) — retrying once`)
    return await postOnce(payload)
  }
}

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

    // 400 -> InvalidRequest, which the driver does not retry.
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
        // Status passed through: the driver retries 5xx, not 4xx. Size logged: a failed call reports no usage.
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
  const est = (n) => Math.round(JSON.stringify({ x: 'y'.repeat(n) }).length / 3.6)
  console.assert(est(100_000) > 100_000 / 3.95, 'the estimate must not UNDER-count tokens')
  console.log('ok')
} else {
  server.listen(PORT, HOST, () => {
    console.error(`[shim] /v1/messages on http://${HOST}:${PORT} -> ${BASE} ${MODEL || '(caller model)'}`)
  })
}
