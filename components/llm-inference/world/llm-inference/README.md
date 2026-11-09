`llm-inference` is the provider boundary, and the world is intentionally
MINIMAL — just the export. Each concrete provider declares its OWN imports:
a real vendor client (e.g. `openai-provider`) imports wasi:http +
wasi:config (base-url, model, api-key); a deterministic mock provider imports
nothing. Keeping the contract free of those lets any provider shape satisfy
it. ai:assist imports `inference` and never sees which provider answered.