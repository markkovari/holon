`jev-decision` is the provider boundary, and the world is intentionally
MINIMAL — just the export. Each concrete provider declares its OWN imports:
a real vendor client (e.g. `typesafe-provider`) imports wasi:http +
wasi:config + comp:secrets; a deterministic mock imports only wasi:config.
A router imports `decision` and never sees which provider answered.