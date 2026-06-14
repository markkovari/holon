// Fastify plugin wiring the auth:identity contract into the request lifecycle.
//
//   - registers /auth/register, /auth/login, /auth/logout, /auth/me passthroughs
//   - decorates `requireAuth(target, action)` -> a preHandler that guards a route
//   - on success, attaches the verified principal to `request.principal`

import type {
  FastifyInstance,
  FastifyPluginAsync,
  FastifyReply,
  FastifyRequest,
  preHandlerHookHandler,
} from "fastify";
import fp from "fastify-plugin";
import { AuthClient, AuthError, type Principal } from "./auth-client.js";

declare module "fastify" {
  interface FastifyRequest {
    principal?: Principal;
  }
  interface FastifyInstance {
    auth: AuthClient;
    requireAuth: (target: string, action: string) => preHandlerHookHandler;
  }
}

function bearer(request: FastifyRequest): string | undefined {
  const h = request.headers.authorization;
  if (h?.startsWith("Bearer ")) return h.slice(7).trim();
  return undefined;
}

export interface AuthPluginOptions {
  baseUrl: string;
}

const authPluginImpl: FastifyPluginAsync<AuthPluginOptions> = async (
  fastify: FastifyInstance,
  opts: AuthPluginOptions,
) => {
  const client = new AuthClient(opts.baseUrl);
  fastify.decorate("auth", client);

  // preHandler factory: guard a route by (target, action).
  fastify.decorate(
    "requireAuth",
    (target: string, action: string): preHandlerHookHandler => {
      return async (request: FastifyRequest, reply: FastifyReply) => {
        const token = bearer(request);
        if (!token) {
          return reply.code(401).send({ error: "missing_bearer_token" });
        }
        try {
          request.principal = await client.verify(token, { target, action });
        } catch (err) {
          if (err instanceof AuthError) {
            return reply.code(err.status).send({ error: err.code });
          }
          request.log.error(err, "auth verify failed");
          return reply.code(503).send({ error: "auth_unavailable" });
        }
      };
    },
  );

  // ---- auth passthrough routes ----
  fastify.post("/auth/register", async (request, reply) => {
    const { email, password, tenant } = request.body as {
      email: string;
      password: string;
      tenant?: string;
    };
    try {
      const principal = await client.register(email, password, tenant ?? "");
      return reply.code(201).send(principal);
    } catch (err) {
      if (err instanceof AuthError) return reply.code(err.status).send({ error: err.code });
      throw err;
    }
  });

  fastify.post("/auth/login", async (request, reply) => {
    const { email, password, tenant } = request.body as {
      email: string;
      password: string;
      tenant?: string;
    };
    try {
      return await client.login(email, password, tenant ?? "");
    } catch (err) {
      if (err instanceof AuthError) return reply.code(err.status).send({ error: err.code });
      throw err;
    }
  });

  fastify.post("/auth/logout", async (request, reply) => {
    const token = bearer(request);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    await client.logout(token);
    return reply.code(204).send();
  });
};

// fastify-plugin breaks encapsulation so the `auth` + `requireAuth` decorators
// (and the routes) are visible on the root instance, not just this scope.
export const authPlugin = fp(authPluginImpl, { name: "auth", fastify: "5.x" });
