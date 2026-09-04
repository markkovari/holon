#!/usr/bin/env node
// A local `/v1/messages` and `/v1/chat/completions` endpoint backed by Antigravity / Gemini
// for Holon's reconciler and goal-run inference loop.
//
// Point `anthropic:base-url` or `openai:base-url` at this and the loop's inference runs
// through Antigravity / Gemini on your local setup or API credentials.
//
//   node tools/antigravity-shim.mjs                         # 127.0.0.1:8789
//   PORT=8789 ANTIGRAVITY_MODEL=gemini-2.5-flash node tools/antigravity-shim.mjs
//   just antigravity-shim
//
// ## Execution Backends
//
// 1. Antigravity CLI (`agy -p` or custom `ANTIGRAVITY_CLI_BIN`) if available.
// 2. Google Generative AI / Gemini API (via ANTIGRAVITY_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY,
//    or ~/.comp-secrets/gemini) supporting both text and inline image blocks.
// 3. Fallback to `tools/gemini-cli.mjs` if configured.
//
// ## Concurrency & Snapshot Isolation
//
// Concurrency is bounded by `ANTIGRAVITY_CONCURRENCY` (default 4) with FIFO queueing so swarm
// branches remain isolated without dropping or failing requests due to rate limits.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { existsSync, readFileSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 8789);
const HOST = process.env.HOST || '127.0.0.1';
const MODEL = process.env.ANTIGRAVITY_MODEL || process.env.GEMINI_MODEL || 'gemini-2.5-flash';
const TIMEOUT_MS = Number(process.env.ANTIGRAVITY_TIMEOUT_MS || 300_000);
const LIMIT = Number(process.env.ANTIGRAVITY_CONCURRENCY || 4);

// CLI binaries & paths
const AGY_BIN = process.env.ANTIGRAVITY_CLI_BIN || process.env.AGY_BIN || 'agy';
const GEMINI_CLI = join(__dirname, 'gemini-cli.mjs');

// Discover API key from environment or secrets directory
function getApiKey() {
  if (process.env.ANTIGRAVITY_API_KEY) return process.env.ANTIGRAVITY_API_KEY.trim();
  if (process.env.GEMINI_API_KEY) return process.env.GEMINI_API_KEY.trim();
  if (process.env.GOOGLE_API_KEY) return process.env.GOOGLE_API_KEY.trim();

  const secretFiles = [
    join(homedir(), '.comp-secrets', 'gemini'),
    join(homedir(), '.comp-secrets', 'google'),
    join(homedir(), '.comp-secrets', 'antigravity'),
  ];
  for (const f of secretFiles) {
    if (existsSync(f)) {
      try {
        const content = readFileSync(f, 'utf8').trim();
        if (content) return content;
      } catch {}
    }
  }
  return '';
}

let active = 0;
/** Resolvers for calls that arrived while every slot was busy. FIFO. */
const waiting = [];

const acquire = () => {
  if (active < LIMIT) {
    active++;
    return Promise.resolve();
  }
  return new Promise((resolve) => waiting.push(resolve));
};

const release = () => {
  const next = waiting.shift();
  if (next) next();
  else active--;
};

/** Anthropic's error envelope — `codec.rs` keys on `type == "error"`. */
const apiError = (type, message) => JSON.stringify({ type: 'error', error: { type, message } });

/** OpenAI error envelope */
const openAiError = (message) =>
  JSON.stringify({ error: { message, type: 'invalid_request_error', code: null } });

/** Extension map for supported images. */
const EXT = {
  'image/jpeg': 'jpg',
  'image/jpg': 'jpg',
  'image/png': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
};

/**
 * Flatten incoming request body into system text, turn-based prompt, and image list.
 */
function render(body) {
  let system = '';
  if (Array.isArray(body.system)) {
    system = body.system
      .filter((b) => b?.type === 'text')
      .map((b) => b.text)
      .join('\n\n');
  } else if (typeof body.system === 'string') {
    system = body.system;
  }

  const messages = body.messages || [];
  const turns = [];
  const images = [];

  for (const m of messages) {
    if (m.role === 'system') {
      const txt = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
      system = system ? `${system}\n\n${txt}` : txt;
      continue;
    }

    let text = '';
    if (typeof m.content === 'string') {
      text = m.content;
    } else if (Array.isArray(m.content)) {
      for (const block of m.content) {
        if (block?.type === 'text') {
          text += block.text;
        } else if (block?.type === 'image' && block.source?.type === 'base64' && block.source.data) {
          images.push({
            media_type: block.source.media_type || 'image/jpeg',
            data: block.source.data,
          });
        }
      }
    }
    turns.push({ role: m.role === 'assistant' ? 'assistant' : 'user', text });
  }

  const prompt =
    turns.length === 1
      ? turns[0].text
      : turns.map((t) => `[${t.role}]\n${t.text}`).join('\n\n');

  return { system, prompt, images, turns };
}

/**
 * Direct invocation of Google Generative AI / Gemini API.
 */
async function callGenerativeApi({ system, prompt, images = [], model, key }) {
  const targetModel = model || MODEL;
  const url = `https://generativelanguage.googleapis.com/v1beta/models/${targetModel}:generateContent?key=${key}`;

  const parts = [];
  // Add images as inline data parts
  for (const img of images) {
    parts.push({
      inline_data: {
        mime_type: img.media_type,
        data: img.data,
      },
    });
  }

  if (prompt) {
    parts.push({ text: prompt });
  }

  const payload = {
    contents: [{ role: 'user', parts }],
    generationConfig: {
      temperature: 0.7,
      maxOutputTokens: 8192,
    },
  };

  if (system) {
    payload.systemInstruction = {
      parts: [{ text: system }],
    };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);

  try {
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(payload),
      signal: controller.signal,
    });

    const json = await res.json();
    if (!res.ok) {
      const errDetail = json.error ? `${json.error.message || json.error.code}` : JSON.stringify(json);
      throw new Error(`Generative API error (${res.status}): ${errDetail}`);
    }

    const text =
      json.candidates?.[0]?.content?.parts?.map((p) => p.text || '').join('') || '';
    return text;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Fallback CLI invocation (agy or gemini-cli).
 */
function runCli({ system, prompt, model, binary, extraArgs = [] }) {
  return new Promise((resolve, reject) => {
    const args = ['-p', ...extraArgs];
    const targetModel = model || MODEL;
    if (targetModel) args.push('--model', targetModel);
    if (system) args.push('--system-prompt', system);

    let cmd = binary;
    let cmdArgs = args;

    if (cmd.endsWith('.mjs') || cmd.endsWith('.js')) {
      cmdArgs = [cmd, ...args];
      cmd = process.execPath;
    }

    const child = spawn(cmd, cmdArgs, { stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    let err = '';

    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`${binary} exceeded ${TIMEOUT_MS}ms`));
    }, TIMEOUT_MS);

    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (err += d));

    child.on('error', (e) => {
      clearTimeout(timer);
      reject(new Error(`could not spawn \`${cmd}\`: ${e.message}`));
    });

    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) {
        const why = [err.trim(), out.trim()].filter(Boolean).join(' | ') || '(no output)';
        reject(new Error(`${binary} exited ${code}: ${why.slice(0, 300)}`));
        return;
      }
      resolve(out);
    });

    child.stdin.end(prompt);
  });
}

/**
 * Dispatch inference through available backend.
 */
async function dispatchInference({ system, prompt, images, model }) {
  const apiKey = getApiKey();

  // 1. Direct API if key is available (handles multimodal images natively)
  if (apiKey) {
    return await callGenerativeApi({ system, prompt, images, model, key: apiKey });
  }

  // 2. Antigravity CLI binary if present
  try {
    const isNode = AGY_BIN.endsWith('.mjs') || AGY_BIN.endsWith('.js');
    if (isNode ? existsSync(AGY_BIN) : true) {
      return await runCli({ system, prompt, model, binary: AGY_BIN });
    }
  } catch {}

  // 3. Fallback to bundled gemini-cli.mjs if present
  if (existsSync(GEMINI_CLI)) {
    return await runCli({ system, prompt, model, binary: GEMINI_CLI });
  }

  throw new Error(
    'No Antigravity backend available. Set ANTIGRAVITY_API_KEY, GEMINI_API_KEY, or ensure `agy` is in PATH.'
  );
}

const server = createServer((req, res) => {
  const send = (status, payload) => {
    res.writeHead(status, {
      'content-type': 'application/json',
      'access-control-allow-origin': '*',
      'access-control-allow-headers': '*',
      'access-control-allow-methods': 'POST, GET, OPTIONS',
    });
    res.end(payload);
  };

  if (req.method === 'OPTIONS') {
    send(204, '');
    return;
  }

  // Health check endpoint
  if (req.method === 'GET' && (req.url === '/health' || req.url === '/')) {
    const apiKey = getApiKey();
    send(
      200,
      JSON.stringify({
        status: 'ok',
        shim: 'antigravity-shim',
        model: MODEL,
        port: PORT,
        active,
        limit: LIMIT,
        waiting: waiting.length,
        has_api_key: Boolean(apiKey),
      })
    );
    return;
  }

  const isAnthropic = req.url.startsWith('/v1/messages');
  const isOpenAi = req.url.startsWith('/v1/chat/completions');

  if (req.method !== 'POST' || (!isAnthropic && !isOpenAi)) {
    send(404, apiError('not_found_error', `no route for ${req.method} ${req.url}`));
    return;
  }

  let raw = '';
  req.on('data', (c) => (raw += c));
  req.on('end', async () => {
    let body;
    try {
      body = JSON.parse(raw);
    } catch (e) {
      send(
        400,
        isAnthropic
          ? apiError('invalid_request_error', `bad json: ${e.message}`)
          : openAiError(e.message)
      );
      return;
    }

    const arrived = Date.now();
    await acquire();
    const started = Date.now();

    try {
      const rendered = render(body);
      const text = await dispatchInference({
        system: rendered.system,
        prompt: rendered.prompt,
        images: rendered.images,
        model: body.model || MODEL,
      });

      const model = body.model || MODEL || 'antigravity';
      console.error(
        `[antigravity-shim] ${model} ${Date.now() - started}ms ${text.length}B` +
          ` <- ${(body.messages || []).length} turn(s)` +
          ` [queued ${started - arrived}ms, ${active}/${LIMIT} busy, ${waiting.length} waiting]`
      );

      if (isAnthropic) {
        send(
          200,
          JSON.stringify({
            type: 'message',
            role: 'assistant',
            model,
            content: [{ type: 'text', text }],
            stop_reason: 'end_turn',
            usage: { input_tokens: 0, output_tokens: 0 },
          })
        );
      } else {
        send(
          200,
          JSON.stringify({
            id: `chatcmpl-${Date.now()}`,
            object: 'chat.completion',
            created: Math.floor(Date.now() / 1000),
            model,
            choices: [
              {
                index: 0,
                message: { role: 'assistant', content: text },
                finish_reason: 'stop',
              },
            ],
            usage: {
              prompt_tokens: 0,
              completion_tokens: 0,
              total_tokens: 0,
            },
          })
        );
      }
    } catch (e) {
      console.error(
        `[antigravity-shim] FAILED after ${Date.now() - started}ms` +
          ` [queued ${started - arrived}ms]: ${e.message}`
      );
      // 529 overloaded / retryable error
      send(529, isAnthropic ? apiError('overloaded_error', e.message) : openAiError(e.message));
    } finally {
      release();
    }
  });
});

server.listen(PORT, HOST, () => {
  console.error(
    `[antigravity-shim] /v1/messages & /v1/chat/completions on http://${HOST}:${PORT} (model: ${MODEL}, concurrency: ${LIMIT})`
  );
});
