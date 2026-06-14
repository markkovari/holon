// Entry point: build the app and listen. Routes live in app.ts; auth routes
// (/auth/register, /auth/login, /auth/logout) come from the auth plugin.
//
// Run the auth stack first (see README), then:
//   AUTH_BASE_URL=http://localhost:8001 npm run dev

import { buildApp } from "./app.js";

const AUTH_BASE_URL = process.env.AUTH_BASE_URL ?? "http://localhost:8001";
const PORT = Number(process.env.PORT ?? 3000);

const app = await buildApp({ baseUrl: AUTH_BASE_URL, logger: true });

try {
  await app.listen({ port: PORT, host: "0.0.0.0" });
  app.log.info(`auth base url: ${AUTH_BASE_URL}`);
} catch (err) {
  app.log.error(err);
  process.exit(1);
}
