#!/usr/bin/env node
// A local `/v1/messages` and `/v1/chat/completions` endpoint backed by `gemini -p` CLI
// instead of direct API HTTP calls — mirroring `tools/claude-shim.mjs`.
//
// Point `anthropic:base-url` or `openai:base-url` at this and the loop's inference
// runs through the Gemini CLI on your subscription/credentials.
//
//   node tools/gemini-shim.mjs                     # 127.0.0.1:8788
//   PORT=9000 GEMINI_MODEL=gemini-2.5-pro node tools/gemini-shim.mjs
//
// ## One CLI process per request (Snapshot & Branch Isolation)
//
// Every incoming request spawns a fresh `gemini -p` (or `node tools/gemini-cli.mjs -p`)
// subprocess. This ensures isolation across swarm branches and generation tasks.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { existsSync } from 'node:fs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 8788);
const HOST = process.env.HOST || '127.0.0.1';
const MODEL = process.env.GEMINI_MODEL || 'gemini-2.5-flash';
const TIMEOUT_MS = Number(process.env.GEMINI_TIMEOUT_MS || 240_000);
const LIMIT = Number(process.env.GEMINI_CONCURRENCY || 4);

// Determine CLI binary: system `gemini`, or `tools/gemini-cli.mjs`
const GEMINI_BIN = process.env.GEMINI_CLI_BIN || 'gemini';
const FALLBACK_CLI = join(__dirname, 'gemini-cli.mjs');

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

/**
 * Flatten a request into one prompt plus a system string.
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

  for (const m of messages) {
    if (m.role === 'system') {
      const txt = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
      system = system ? `${system}\n\n${txt}` : txt;
      continue;
    }
    const text = typeof m.content === 'string'
      ? m.content
      : (Array.isArray(m.content) ? m.content.filter((b) => b?.type === 'text').map((b) => b.text).join('') : '');
    turns.push({ role: m.role === 'assistant' ? 'assistant' : 'user', text });
  }

  const prompt =
    turns.length === 1
      ? turns[0].text
      : turns.map((t) => `[${t.role}]\n${t.text}`).join('\n\n');

  return { system, prompt };
}

/** Run one `gemini -p` CLI subprocess and resolve with its stdout. */
function runGeminiCli({ system, prompt, model }) {
  return new Promise((resolve, reject) => {
    const args = ['-p'];
    const targetModel = model || MODEL;
    if (targetModel) args.push('--model', targetModel);
    if (system) args.push('--system-prompt', system);

    // If global `gemini` executable exists in PATH, spawn it; else spawn node gemini-cli.mjs
    let cmd = GEMINI_BIN;
    let cmdArgs = args;

    // Check if `gemini` is in PATH or use our node CLI runner
    const isNodeScript = cmd.endsWith('.mjs') || cmd.endsWith('.js') || !existsSync(cmd);
    if (isNodeScript && existsSync(FALLBACK_CLI)) {
      cmd = process.execPath;
      cmdArgs = [FALLBACK_CLI, ...args];
    }

    const child = spawn(cmd, cmdArgs, { stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    let err = '';

    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`gemini -p exceeded ${TIMEOUT_MS}ms`));
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
        reject(new Error(`gemini -p exited ${code}: ${err.trim().slice(0, 300)}`));
        return;
      }
      resolve(out);
    });

    child.stdin.end(prompt);
  });
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
      send(400, isAnthropic ? apiError('invalid_request_error', `bad json: ${e.message}`) : openAiError(e.message));
      return;
    }

    const arrived = Date.now();
    await acquire();
    const started = Date.now();

    try {
      const rendered = render(body);
      const text = await runGeminiCli({
        system: rendered.system,
        prompt: rendered.prompt,
        model: body.model,
      });

      const model = MODEL || body.model || 'gemini-cli';
      console.error(
        `[gemini-shim (cli)] ${model} ${Date.now() - started}ms ${text.length}B` +
          ` <- ${(body.messages || []).length} turn(s)` +
          ` [queued ${started - arrived}ms, ${active}/${LIMIT} busy, ${waiting.length} waiting]`,
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
          }),
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
          }),
        );
      }
    } catch (e) {
      console.error(
        `[gemini-shim (cli)] FAILED after ${Date.now() - started}ms` +
          ` [queued ${started - arrived}ms]: ${e.message}`,
      );
      // 529 / 500 retryable error
      send(529, isAnthropic ? apiError('overloaded_error', e.message) : openAiError(e.message));
    } finally {
      release();
    }
  });
});

server.listen(PORT, HOST, () => {
  console.error(`[gemini-shim] /v1/messages & /v1/chat/completions on http://${HOST}:${PORT} -> gemini -p CLI`);
});
