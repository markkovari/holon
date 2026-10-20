#!/usr/bin/env node
// A `claude -p`-shaped CLI over the Gemini API: echo "hi" | node tools/gemini-cli.mjs -p [--model m] [--system-prompt s]

import { parseArgs } from 'node:util';

const API_KEY = process.env.GEMINI_API_KEY || process.env.GOOGLE_API_KEY || '';

const { values } = parseArgs({
  options: {
    print: { type: 'boolean', short: 'p', default: false },
    model: { type: 'string', short: 'm', default: process.env.GEMINI_MODEL || 'gemini-2.5-flash' },
    'system-prompt': { type: 'string', default: '' },
    help: { type: 'boolean', short: 'h', default: false },
  },
  strict: false,
});

if (values.help) {
  console.log(`Usage: gemini-cli -p [--model <model>] [--system-prompt <prompt>]`);
  process.exit(0);
}

let prompt = '';
for await (const chunk of process.stdin) {
  prompt += chunk;
}
prompt = prompt.trim();

if (!prompt) {
  process.exit(0);
}

const model = values.model;
const systemPrompt = values['system-prompt'];

const payload = {
  contents: [
    {
      role: 'user',
      parts: [{ text: prompt }],
    },
  ],
  generationConfig: {
    temperature: 0.7,
    maxOutputTokens: 4096,
  },
};

if (systemPrompt) {
  payload.systemInstruction = {
    parts: [{ text: systemPrompt }],
  };
}

const key = API_KEY;
if (!key) {
  console.error("Warning: GEMINI_API_KEY or GOOGLE_API_KEY is not set.");
}

const url = `https://generativelanguage.googleapis.com/v1beta/models/${model}:generateContent?key=${key}`;

try {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(payload),
  });

  const json = await res.json();
  if (!res.ok) {
    console.error(`Gemini API error (${res.status}):`, JSON.stringify(json.error || json));
    process.exit(1);
  }

  const text = json.candidates?.[0]?.content?.parts?.map((p) => p.text || '').join('') || '';
  process.stdout.write(text);
} catch (e) {
  console.error(`CLI execution failed: ${e.message}`);
  process.exit(1);
}
