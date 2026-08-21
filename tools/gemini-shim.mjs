#!/usr/bin/env node
// A local `/v1/messages` and `/v1/chat/completions` endpoint backed by Google Gemini.
//
// Point `anthropic:base-url` (or `openai:base-url`) at this to route holon's
// `llm:inference` component calls through Google Gemini using your Gemini API key
// or machine tokens on picur.
//
//   node tools/gemini-shim.mjs                     # 127.0.0.1:8788
//   GEMINI_API_KEY=... just gemini-shim
//   PORT=9000 GEMINI_MODEL=gemini-2.5-pro node tools/gemini-shim.mjs
//
// Features:
// - Supports Anthropic `/v1/messages` contract (for `anthropic-provider`).
// - Supports OpenAI `/v1/chat/completions` contract (for `openai-provider`).
// - Converts chat history and system prompts to Gemini's `contents` and `systemInstruction`.
// - Handles concurrency limits and graceful request queueing.

import { createServer } from 'node:http';

const PORT = Number(process.env.PORT || 8788);
const HOST = process.env.HOST || '127.0.0.1';
const API_KEY = process.env.GEMINI_API_KEY || process.env.GOOGLE_API_KEY || '';
const DEFAULT_MODEL = process.env.GEMINI_MODEL || 'gemini-2.5-flash';
const TIMEOUT_MS = Number(process.env.GEMINI_TIMEOUT_MS || 120_000);
const LIMIT = Number(process.env.GEMINI_CONCURRENCY || 8);

let active = 0;
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

/** Anthropic's error envelope */
const anthropicError = (type, message) =>
  JSON.stringify({ type: 'error', error: { type, message } });

/** OpenAI's error envelope */
const openAiError = (message) =>
  JSON.stringify({ error: { message, type: 'invalid_request_error', code: null } });

/** Convert Anthropic / OpenAI body to Gemini generateContent format */
function toGeminiPayload(body) {
  let systemText = '';
  let turns = [];

  // 1. Anthropic format
  if (body.system) {
    if (Array.isArray(body.system)) {
      systemText = body.system
        .filter((b) => b?.type === 'text')
        .map((b) => b.text)
        .join('\n\n');
    } else if (typeof body.system === 'string') {
      systemText = body.system;
    }
  }

  const messages = body.messages || [];
  for (const m of messages) {
    if (m.role === 'system') {
      const txt = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
      systemText = systemText ? `${systemText}\n\n${txt}` : txt;
      continue;
    }

    const role = m.role === 'assistant' ? 'model' : 'user';
    let text = '';
    if (typeof m.content === 'string') {
      text = m.content;
    } else if (Array.isArray(m.content)) {
      text = m.content
        .filter((b) => b?.type === 'text')
        .map((b) => b.text)
        .join('');
    }

    turns.push({
      role,
      parts: [{ text }],
    });
  }

  // Ensure turns alternate correctly for Gemini API
  const contents = [];
  for (const turn of turns) {
    if (contents.length > 0 && contents[contents.length - 1].role === turn.role) {
      contents[contents.length - 1].parts.push(...turn.parts);
    } else {
      contents.push(turn);
    }
  }

  const payload = {
    contents,
    generationConfig: {
      temperature: body.temperature !== undefined ? body.temperature : 0.7,
      maxOutputTokens: body.max_tokens || 4096,
    },
  };

  if (systemText) {
    payload.systemInstruction = {
      parts: [{ text: systemText }],
    };
  }

  if (body.stop_sequences || body.stop) {
    payload.generationConfig.stopSequences = body.stop_sequences || body.stop;
  }

  return payload;
}

async function callGemini(model, payload, apiKeyHeader) {
  const key = apiKeyHeader || API_KEY;
  const targetModel = model || DEFAULT_MODEL;
  const cleanModel = targetModel.replace(/^models\//, '').replace(/^claude-[^/]+/, DEFAULT_MODEL).replace(/^gpt-[^/]+/, DEFAULT_MODEL);

  const url = `https://generativelanguage.googleapis.com/v1beta/models/${cleanModel}:generateContent?key=${key}`;

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
      throw new Error(`Gemini API error (${res.status}): ${JSON.stringify(json.error || json)}`);
    }

    const candidate = json.candidates?.[0];
    const text = candidate?.content?.parts?.map((p) => p.text || '').join('') || '';
    const finishReason = candidate?.finishReason || 'STOP';
    const usage = {
      input_tokens: json.usageMetadata?.promptTokenCount || 0,
      output_tokens: json.usageMetadata?.candidatesTokenCount || 0,
    };

    return { text, finishReason, usage, model: cleanModel };
  } finally {
    clearTimeout(timer);
  }
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
    send(404, anthropicError('not_found_error', `no route for ${req.method} ${req.url}`));
    return;
  }

  let raw = '';
  req.on('data', (c) => (raw += c));
  req.on('end', async () => {
    let body;
    try {
      body = JSON.parse(raw);
    } catch (e) {
      send(400, isAnthropic ? anthropicError('invalid_request_error', `bad json: ${e.message}`) : openAiError(e.message));
      return;
    }

    const apiKeyHeader = req.headers['x-api-key'] || req.headers['authorization']?.replace(/^Bearer\s+/, '') || '';

    const arrived = Date.now();
    await acquire();
    const started = Date.now();

    try {
      const geminiPayload = toGeminiPayload(body);
      const result = await callGemini(body.model, geminiPayload, apiKeyHeader);

      console.error(
        `[gemini-shim] ${result.model} ${Date.now() - started}ms ${result.text.length}B` +
          ` <- ${(body.messages || []).length} turn(s)` +
          ` [queued ${started - arrived}ms, ${active}/${LIMIT} busy]`,
      );

      if (isAnthropic) {
        send(
          200,
          JSON.stringify({
            type: 'message',
            role: 'assistant',
            model: result.model,
            content: [{ type: 'text', text: result.text }],
            stop_reason: result.finishReason === 'STOP' ? 'end_turn' : 'max_tokens',
            usage: result.usage,
          }),
        );
      } else {
        send(
          200,
          JSON.stringify({
            id: `chatcmpl-${Date.now()}`,
            object: 'chat.completion',
            created: Math.floor(Date.now() / 1000),
            model: result.model,
            choices: [
              {
                index: 0,
                message: { role: 'assistant', content: result.text },
                finish_reason: result.finishReason.toLowerCase(),
              },
            ],
            usage: {
              prompt_tokens: result.usage.input_tokens,
              completion_tokens: result.usage.output_tokens,
              total_tokens: result.usage.input_tokens + result.usage.output_tokens,
            },
          }),
        );
      }
    } catch (e) {
      console.error(`[gemini-shim] FAILED after ${Date.now() - started}ms: ${e.message}`);
      send(500, isAnthropic ? anthropicError('api_error', e.message) : openAiError(e.message));
    } finally {
      release();
    }
  });
});

server.listen(PORT, HOST, () => {
  console.error(`[gemini-shim] listening on http://${HOST}:${PORT}`);
  console.error(`  - Anthropic endpoint: http://${HOST}:${PORT}/v1/messages`);
  console.error(`  - OpenAI endpoint:    http://${HOST}:${PORT}/v1/chat/completions`);
  console.error(`  - Default model:      ${DEFAULT_MODEL}`);
});
