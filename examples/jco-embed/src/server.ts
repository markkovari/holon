// Entry point: build the in-process app and listen. Run `npm run transpile`
// first (the start script does it) so ./gen exists.

import { buildApp } from "./app.js";

const PORT = Number(process.env.PORT ?? 3001);
const app = buildApp({ logger: true });

try {
  await app.listen({ port: PORT, host: "0.0.0.0" });
} catch (err) {
  app.log.error(err);
  process.exit(1);
}
