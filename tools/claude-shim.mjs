#!/usr/bin/env node
// A local `/v1/messages` endpoint backed by `claude -p` instead of the API.
//
// Point `anthropic:base-url` at this and the loop's inference runs through the
// Claude Code CLI on your subscription. Nothing in the component graph changes:
// `anthropic-provider` already reads its base URL from `wasi:config`, and the
// swap point is the same one `mock-provider` uses in `reconciler/tests/mockllm.rs`.
//
//   node tools/claude-shim.mjs                     # 127.0.0.1:8787
//   PORT=9000 CLAUDE_MODEL=opus node tools/claude-shim.mjs
//
// ## One session per request, on purpose
//
// Every request spawns a fresh `claude -p`. That is the whole reason this is
// usable as a swarm backend: ADR-0078 runs four branches per generation (the
// stress test runs twenty), and ADR-0091 decided lessons are snapshot-isolated
// per branch so twenty branches don't converge on one early wrong belief.
// Routing them through one shared conversation would hand every branch the same
// context and undo that by construction. Separate processes keep both the
// concurrency and the isolation.
//
// ## What this speaks
//
// Only the subset `anthropic-provider` actually sends and parses — not the
// Messages API. Requests carry `system` (array of text blocks), `messages`
// (text blocks only), `model`, `max_tokens`, and sometimes `temperature` /
// `stop_sequences`. Responses need `type`, `content[].text`, `model`,
// `stop_reason`, and `usage`. No streaming, no tools, no images: the provider
// asks for none of them, and a shim that pretends to support more would fail
// somewhere less obvious than here.
//
// Token counts are **not** measured — `claude -p` does not report them and this
// deliberately does not guess. See `usage` below before trusting a cost number.

import { spawn } from 'node:child_process'
import { createServer } from 'node:http'

const PORT = Number(process.env.PORT || 8787)
const HOST = process.env.HOST || '127.0.0.1'
/** Passed to `claude --model`. Empty means the CLI's own default. */
const MODEL = process.env.CLAUDE_MODEL || ''
/** A generated file on a real task takes a while; the provider waits 10 minutes. */
const TIMEOUT_MS = Number(process.env.CLAUDE_TIMEOUT_MS || 540_000)
/**
 * How many `claude -p` processes may run at once.
 *
 * Six branches per generation, each with a gate that makes its own inference call,
 * is a dozen CLI processes reaching for one subscription. Unqueued, the ones that
 * lose come back as errors — and the loop cannot tell a throttled call from a branch
 * that failed, so a rate limit would be recorded as an agent failure. Queueing makes
 * the same pressure show up as WAITING, which is what it is, and the wait is logged
 * separately from the work so neither hides in the other's number.
 */
const LIMIT = Number(process.env.CLAUDE_CONCURRENCY || 4)
let active = 0
/** Resolvers for calls that arrived while every slot was busy. FIFO. */
const waiting = []

const acquire = () => {
  if (active < LIMIT) {
    active++
    return Promise.resolve()
  }
  return new Promise((resolve) => waiting.push(resolve))
}

// The slot is handed straight to the next waiter rather than freed and re-taken:
// `active` is unchanged in that case, which is what keeps LIMIT a ceiling under a
// burst of arrivals.
const release = () => {
  const next = waiting.shift()
  if (next) next()
  else active--
}

/** Everything that lets the CLI act instead of answer. See `runClaude`. */
const ACTING_TOOLS =
  'Bash,Read,Write,Edit,Glob,Grep,Task,WebFetch,WebSearch,NotebookEdit,TodoWrite'

/** Anthropic's error envelope — `codec.rs` keys on `type == "error"`. */
const apiError = (type, message) => JSON.stringify({ type: 'error', error: { type, message } })

/**
 * Flatten a request into one prompt plus a system string.
 *
 * The provider splits system parts out of `messages` and joins them into a
 * single `system` block, so the turns that arrive here are user/assistant only.
 * Multi-turn requests are rendered with role labels rather than dropped — the
 * loop's repair path sends prior attempts as assistant turns, and losing them
 * would silently make every repair a first attempt.
 */
function render(body) {
  const system = (Array.isArray(body.system) ? body.system : [])
    .filter((b) => b?.type === 'text')
    .map((b) => b.text)
    .join('\n\n')

  const turns = (body.messages || []).map((m) => {
    const text = (Array.isArray(m.content) ? m.content : [])
      .filter((b) => b?.type === 'text')
      .map((b) => b.text)
      .join('')
    return { role: m.role === 'assistant' ? 'assistant' : 'user', text }
  })

  // One turn is the overwhelmingly common case — send it bare so the model sees
  // exactly the prompt the caller wrote, with no framing this shim invented.
  const prompt =
    turns.length === 1
      ? turns[0].text
      : turns.map((t) => `[${t.role}]\n${t.text}`).join('\n\n')

  return { system, prompt }
}

/** Run one `claude -p` and resolve with its stdout. */
function runClaude({ system, prompt }) {
  return new Promise((resolve, reject) => {
    const args = ['-p']
    if (MODEL) args.push('--model', MODEL)
    // REPLACES Claude Code's system prompt rather than appending to it, and this
    // used to be the other way around. Appending left a coding AGENT on the far
    // end of an `llm:inference` call, and an agent does the work with tools and
    // then reports on it: measured over two full runs, 5 of 8 branches came back
    // as "Implemented `access.rs` — compiles clean." in 447–1631 characters, with
    // no file block for the loop to extract. The branch's ANSWER is the
    // deliverable here; nothing reads a filesystem it wrote.
    if (system) args.push('--system-prompt', system)
    // And the tools go too, because the prompt alone does not stop it: a branch
    // with tools spends its budget exploring a checkout it cannot submit. With
    // these denied the same prompt answers in seconds.
    //
    // ponytail: a hand-listed deny-list, so a NEW acting tool would slip
    // through. `--allowedTools ''` is the future-proof form and does not work —
    // the empty value does not deny anything. Revisit if the CLI grows a real
    // "no tools" switch.
    args.push('--disallowedTools', ACTING_TOOLS)

    const child = spawn('claude', args, { stdio: ['pipe', 'pipe', 'pipe'] })
    let out = ''
    let err = ''
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      reject(new Error(`claude -p exceeded ${TIMEOUT_MS}ms`))
    }, TIMEOUT_MS)

    child.stdout.on('data', (d) => (out += d))
    child.stderr.on('data', (d) => (err += d))
    child.on('error', (e) => {
      clearTimeout(timer)
      reject(new Error(`could not spawn \`claude\`: ${e.message}`))
    })
    child.on('close', (code) => {
      clearTimeout(timer)
      if (code !== 0) {
        reject(new Error(`claude -p exited ${code}: ${err.trim().slice(0, 300)}`))
        return
      }
      resolve(out)
    })

    child.stdin.end(prompt)
  })
}

const server = createServer((req, res) => {
  const send = (status, payload) => {
    res.writeHead(status, { 'content-type': 'application/json' })
    res.end(payload)
  }

  if (req.method !== 'POST' || !req.url.startsWith('/v1/messages')) {
    // 404 maps to ProviderUnavailable in status_error() — which is the honest
    // reading: this shim does not serve that route.
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
      // 400 -> InvalidRequest, which the driver treats as the caller's fault.
      send(400, apiError('invalid_request_error', `bad json: ${e.message}`))
      return
    }

    const arrived = Date.now()
    await acquire()
    const started = Date.now()
    try {
      const text = await runClaude(render(body))
      const model = MODEL || body.model || 'claude-code'
      console.error(
        `[shim] ${model} ${Date.now() - started}ms ${text.length}B` +
          ` <- ${(body.messages || []).length} turn(s)` +
          ` [queued ${started - arrived}ms, ${active}/${LIMIT} busy, ${waiting.length} waiting]`,
      )
      send(
        200,
        JSON.stringify({
          type: 'message',
          role: 'assistant',
          model,
          content: [{ type: 'text', text }],
          stop_reason: 'end_turn',
          // Zeroes, not estimates. The wallet reads these, and a fabricated
          // count is worse than an absent one: it would look like a measurement.
          // `claude -p` bills against the subscription, so there is no
          // per-request token cost for this path to report.
          usage: { input_tokens: 0, output_tokens: 0 },
        }),
      )
    } catch (e) {
      console.error(
        `[shim] FAILED after ${Date.now() - started}ms` +
          ` [queued ${started - arrived}ms]: ${e.message}`,
      )
      // 529 -> ProviderUnavailable, which the driver retries. A crashed or
      // timed-out CLI is a transient local failure, not a rejected request.
      send(529, apiError('overloaded_error', e.message))
    } finally {
      // In `finally`, because a slot leaked on the failure path would shrink the
      // ceiling call by call until nothing ran at all — and every timeout takes
      // nine minutes to prove it.
      release()
    }
  })
})

server.listen(PORT, HOST, () => {
  console.error(`[shim] /v1/messages on http://${HOST}:${PORT} -> claude -p ${MODEL || '(default model)'}`)
})
