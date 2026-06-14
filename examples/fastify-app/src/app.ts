// App factory — routes + auth plugin, no `listen`. Importable by tests
// (via app.inject) and by server.ts (which listens).

import Fastify, { type FastifyInstance } from "fastify";
import { authPlugin } from "./auth-plugin.js";

export async function buildApp(opts: {
  baseUrl: string;
  logger?: boolean;
}): Promise<FastifyInstance> {
  const app = Fastify({ logger: opts.logger ?? false });
  await app.register(authPlugin, { baseUrl: opts.baseUrl });

  app.get("/public", async () => ({ ok: true, msg: "no auth needed" }));

  // whoami: any valid token, no permission check (uses introspect, not verify).
  app.get("/auth/me", async (request, reply) => {
    const h = request.headers.authorization;
    const token = h?.startsWith("Bearer ") ? h.slice(7).trim() : undefined;
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    try {
      return await app.auth.me(token);
    } catch (err) {
      const status =
        err && typeof err === "object" && "status" in err ? (err.status as number) : 503;
      return reply.code(status).send({ error: "unauthorized" });
    }
  });

  app.get(
    "/orders",
    { preHandler: app.requireAuth("orders", "read") },
    async (request) => ({
      orders: [{ id: 1, item: "widget" }],
      viewer: request.principal?.subject,
    }),
  );

  app.post(
    "/orders",
    { preHandler: app.requireAuth("orders", "write") },
    async (request, reply) => {
      reply.code(201);
      return { created: true, by: request.principal?.subject };
    },
  );

  return app;
}
