#!/usr/bin/env node
// A `/v1/messages` endpoint backed by `claude -p`; one fresh process per request keeps branches isolated (ADR-0078, ADR-0091).
//   PORT=9000 CLAUDE_MODEL=opus node tools/claude-shim.mjs

import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const PORT = Number(process.env.PORT || 8787)
const HOST = process.env.HOST || '127.0.0.1'
const MODEL = process.env.CLAUDE_MODEL || ''
// Just under the provider's 10-minute wait.
const TIMEOUT_MS = Number(process.env.CLAUDE_TIMEOUT_MS || 540_000)
// Queued, not refused: the loop would record a throttled call as a failed branch.
const LIMIT = Number(process.env.CLAUDE_CONCURRENCY || 4)
let active = 0
const waiting = []

const acquire = () => {
  if (active < LIMIT) {
    active++
    return Promise.resolve()
  }
  return new Promise((resolve) => waiting.push(resolve))
}

// Hand the slot straight to the next waiter so LIMIT stays a ceiling under a burst.
const release = () => {
  const next = waiting.shift()
  if (next) next()
  else active--
}

const ACTING_TOOLS =
  'Bash,Read,Write,Edit,Glob,Grep,Task,WebFetch,WebSearch,NotebookEdit,TodoWrite'

// `codec.rs` keys on `type == "error"`.
const apiError = (type, message) => JSON.stringify({ type: 'error', error: { type, message } })

// Earlier turns are kept with role labels: the repair path sends prior attempts as assistant turns.
function render(body) {
  const system = (Array.isArray(body.system) ? body.system : [])
    .filter((b) => b?.type === 'text')
    .map((b) => b.text)
    .join('\n\n')

  // Order matters: prompts refer to "the first picture".
  const images = []
  const turns = (body.messages || []).map((m) => {
    const blocks = Array.isArray(m.content) ? m.content : []
    const text = blocks
      .filter((b) => b?.type === 'text')
      .map((b) => b.text)
      .join('')
    for (const b of blocks) {
      if (b?.type === 'image' && b.source?.type === 'base64' && b.source.data) {
        images.push({ media_type: b.source.media_type || 'image/jpeg', data: b.source.data })
      }
    }
    return { role: m.role === 'assistant' ? 'assistant' : 'user', text }
  })

  const prompt =
    turns.length === 1
      ? turns[0].text
      : turns.map((t) => `[${t.role}]\n${t.text}`).join('\n\n')

  return { system, prompt, images }
}

const EXT = {
  'image/jpeg': 'jpg',
  'image/jpg': 'jpg',
  'image/png': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
}

let imageSeq = 0

// Unknown media types are rejected, not guessed: the CLI decides what a file is from its extension.
function writeImages(images) {
  const dir = mkdtempSync(join(tmpdir(), `claude-shim-${process.pid}-${imageSeq++}-`))
  return {
    dir,
    paths: images.map((img, i) => {
      const ext = EXT[img.media_type.toLowerCase()]
      if (!ext) throw new Error(`unsupported image media_type: ${img.media_type}`)
      const p = join(dir, `image-${i + 1}.${ext}`)
      writeFileSync(p, Buffer.from(img.data, 'base64'))
      return p
    }),
  }
}

function runClaude({ system, prompt, images = [] }) {
  // Outside the promise so a bad media_type rejects the request instead of crashing the server.
  const staged = images.length ? writeImages(images) : null
  if (staged) {
    // `claude -p` takes a picture only as a path in the prompt.
    prompt =
      `${prompt}\n\n` +
      `The image(s) referred to above are files on this machine. Read them:\n` +
      staged.paths.map((p, i) => `  image ${i + 1}: ${p}`).join('\n')
  }
  return new Promise((resolve, reject) => {
    const args = ['-p']
    if (MODEL) args.push('--model', MODEL)
    // Replaces, not appends: with Claude Code's prompt the branch acts and reports instead of answering.
    if (system) args.push('--system-prompt', system)
    // ponytail: a hand-listed deny-list, so a NEW acting tool would slip through
    // (`--allowedTools ''` denies nothing); revisit if the CLI grows a "no tools" switch.
    // `Read` is allowed only when the request carries images, so the CLI can open them.
    args.push(
      '--disallowedTools',
      staged ? ACTING_TOOLS.split(',').filter((tool) => tool !== 'Read').join(',') : ACTING_TOOLS,
    )

    const child = spawn('claude', args, { stdio: ['pipe', 'pipe', 'pipe'] })
    let out = ''
    let err = ''
    // Called on every exit path: a leaked directory holds somebody's uploaded picture.
    const cleanup = () => {
      if (staged) rmSync(staged.dir, { recursive: true, force: true })
    }
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      cleanup()
      reject(new Error(`claude -p exceeded ${TIMEOUT_MS}ms`))
    }, TIMEOUT_MS)

    child.stdout.on('data', (d) => (out += d))
    child.stderr.on('data', (d) => (err += d))
    child.on('error', (e) => {
      clearTimeout(timer)
      cleanup()
      reject(new Error(`could not spawn \`claude\`: ${e.message}`))
    })
    child.on('close', (code) => {
      clearTimeout(timer)
      cleanup()
      if (code !== 0) {
        // `claude -p` prints its refusals (usage limit, expired auth) to stdout with stderr empty.
        const why = [err.trim(), out.trim()].filter(Boolean).join(' | ') || '(no output)'
        reject(new Error(`claude -p exited ${code}: ${why.slice(0, 300)}`))
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
    // 404 maps to ProviderUnavailable in status_error().
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
      const rendered = render(body)
      const text = await runClaude(rendered)
      const model = MODEL || body.model || 'claude-code'
      console.error(
        `[shim] ${model} ${Date.now() - started}ms ${text.length}B` +
          ` <- ${(body.messages || []).length} turn(s)` +
          (rendered.images.length ? ` +${rendered.images.length} image(s)` : '') +
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
          // Zeroes, not estimates: `claude -p` reports no counts and the wallet must not read a guess as a measurement.
          usage: { input_tokens: 0, output_tokens: 0 },
        }),
      )
    } catch (e) {
      console.error(
        `[shim] FAILED after ${Date.now() - started}ms` +
          ` [queued ${started - arrived}ms]: ${e.message}`,
      )
      // 529 -> ProviderUnavailable, which the driver retries.
      send(529, apiError('overloaded_error', e.message))
    } finally {
      release()
    }
  })
})

server.listen(PORT, HOST, () => {
  console.error(`[shim] /v1/messages on http://${HOST}:${PORT} -> claude -p ${MODEL || '(default model)'}`)
})
