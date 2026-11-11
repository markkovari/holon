/**
 * Cloudflare Worker for Serverless Agent Coordination and CAS Storage Gateway
 * Interfaces directly with Cloudflare R2 using atomic Compare-and-Swap (CAS)
 * via `onlyIf: { etagMatches }` for lock-free Git-lineage commits.
 */

export interface Env {
  AGENT_BUCKET: R2Bucket;
  ENVIRONMENT?: string;
  MAX_BODY_SIZE_MB?: string;
}

interface RefManifest {
  repo: string;
  branch: string;
  head_commit_oid: string;
  generation: number;
  updated_at_unix_ms: number;
  writer_agent_id: string;
}

interface AgentCommit {
  oid: string;
  parent_oids: string[];
  tree_oid: string;
  capability_log_oid?: string;
  agent_id: string;
  generation: number;
  message: string;
  eval_score?: string;
  timestamp_ms: number;
}

interface CheckpointRequest {
  branch: string;
  agent_id: string;
  state_root: string;
  capability_log_head: string;
  message: string;
  expected_etag?: string;
  eval_score?: string;
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data);
  const hashArray = Array.from(new Uint8Array(digest));
  return hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");
}

export default {
  async fetch(request: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname.replace(/\/+$/, "") || "/";
    const method = request.method;

    // 1. Health check
    if (method === "GET" && (path === "/health" || path === "/")) {
      return Response.json({
        service: "holon:agent-edge-gateway",
        platform: "cloudflare-workers",
        runtime: "v8-isolate",
        status: "ok",
      });
    }

    // 2. Content-Addressed Objects
    // POST /v1/cas/objects -> store content-addressed chunk
    if (method === "POST" && path === "/v1/cas/objects") {
      const body = new Uint8Array(await request.arrayBuffer());
      const hex = await sha256Hex(body);
      const oid = `sha256:${hex}`;
      const key = `objects/${oid}`;

      await env.AGENT_BUCKET.put(key, body, {
        httpMetadata: { contentType: "application/octet-stream" },
      });

      return Response.json({ oid, size: body.length }, { status: 200 });
    }

    // GET /v1/cas/objects/:oid -> stream object by OID
    const objectMatch = path.match(/^\/v1\/cas\/objects\/(sha256:[a-f0-9]{64})$/);
    if (method === "GET" && objectMatch) {
      const oid = objectMatch[1];
      const key = `objects/${oid}`;
      const object = await env.AGENT_BUCKET.get(key);

      if (!object) {
        return Response.json({ error: "object not found", oid }, { status: 404 });
      }

      const headers = new Headers();
      object.writeHttpMetadata(headers);
      headers.set("ETag", object.httpEtag);
      return new Response(object.body, { headers, status: 200 });
    }

    // 3. Manifests (Git-Lineage Refs)
    const manifestMatch = path.match(/^\/v1\/manifests\/([^/]+)\/(.+)$/);
    if (manifestMatch) {
      const [, repo, branch] = manifestMatch;
      const refKey = `refs/${repo}/${branch}`;

      // GET /v1/manifests/:repo/:branch -> read ref head + ETag
      if (method === "GET") {
        const object = await env.AGENT_BUCKET.get(refKey);
        if (!object) {
          return Response.json({ error: "manifest not found", repo, branch }, { status: 404 });
        }
        const manifestText = await object.text();
        return new Response(manifestText, {
          status: 200,
          headers: {
            "Content-Type": "application/json",
            "ETag": object.httpEtag,
          },
        });
      }

      // PUT /v1/manifests/:repo/:branch -> atomic Compare-and-Swap
      if (method === "PUT") {
        const ifMatch = request.headers.get("if-match");
        const body: { head_commit_oid: string; agent_id: string } = await request.json();

        // Check existing ref for generation counting
        const currentObj = await env.AGENT_BUCKET.get(refKey);
        let generation = 1;
        if (currentObj) {
          const currentManifest: RefManifest = await currentObj.json();
          generation = currentManifest.generation + 1;
        }

        const newManifest: RefManifest = {
          repo,
          branch,
          head_commit_oid: body.head_commit_oid,
          generation,
          updated_at_unix_ms: Date.now(),
          writer_agent_id: body.agent_id,
        };

        const manifestBytes = new TextEncoder().encode(JSON.stringify(newManifest));

        // Execute atomic CAS using Cloudflare R2's `onlyIf: { etagMatches }`
        const putOptions: R2PutOptions = {
          httpMetadata: { contentType: "application/json" },
        };

        if (ifMatch) {
          const cleanEtag = ifMatch.replace(/^W\//, "").replace(/"/g, "");
          if (cleanEtag !== "*") {
            putOptions.onlyIf = { etagMatches: cleanEtag };
          }
        }

        const putResult = await env.AGENT_BUCKET.put(refKey, manifestBytes, putOptions);

        if (!putResult) {
          // Conditional write failed: CAS conflict
          const latest = await env.AGENT_BUCKET.get(refKey);
          const latestManifest = latest ? await latest.json<RefManifest>() : null;
          return Response.json(
            {
              error: "precondition failed (CAS conflict)",
              current_head: latestManifest?.head_commit_oid ?? null,
              current_etag: latest?.httpEtag ?? null,
            },
            { status: 412 }
          );
        }

        return Response.json(newManifest, {
          status: 200,
          headers: { "ETag": putResult.httpEtag },
        });
      }
    }

    // 4. Commits
    const commitMatch = path.match(/^\/v1\/commits\/(sha256:[a-f0-9]{64})$/);
    if (method === "GET" && commitMatch) {
      const commitOid = commitMatch[1];
      const commitKey = `commits/${commitOid}`;
      const object = await env.AGENT_BUCKET.get(commitKey);
      if (!object) {
        return Response.json({ error: "commit not found", oid: commitOid }, { status: 404 });
      }
      return new Response(object.body, {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }

    // 5. Agent Checkpoints (Combined Commit + Manifest CAS)
    const checkpointMatch = path.match(/^\/v1\/checkpoints\/([^/]+)$/);
    if (method === "POST" && checkpointMatch) {
      const repo = checkpointMatch[1];
      const req: CheckpointRequest = await request.json();
      const refKey = `refs/${repo}/${req.branch}`;

      // 1. Read current ref
      const currentRefObj = await env.AGENT_BUCKET.get(refKey);
      let parentOids: string[] = [];
      let generation = 1;
      let currentEtag: string | null = null;

      if (currentRefObj) {
        const currentManifest: RefManifest = await currentRefObj.json();
        parentOids = [currentManifest.head_commit_oid];
        generation = currentManifest.generation + 1;
        currentEtag = currentRefObj.httpEtag;
      }

      // Check if expected_etag was supplied
      if (req.expected_etag && currentEtag) {
        const cleanReqEtag = req.expected_etag.replace(/^W\//, "").replace(/"/g, "");
        const cleanCurEtag = currentEtag.replace(/^W\//, "").replace(/"/g, "");
        if (cleanReqEtag !== "*" && cleanReqEtag !== cleanCurEtag) {
          return Response.json(
            {
              error: "precondition failed (stale checkpoint base)",
              current_head: parentOids[0] ?? null,
              current_etag: currentEtag,
            },
            { status: 412 }
          );
        }
      }

      // 2. Synthesize commit object
      const now = Date.now();
      const commitSeed = `${parentOids.join(":")}:${req.state_root}:${req.capability_log_head}:${req.agent_id}:${generation}:${req.message}:${now}`;
      const commitOid = `sha256:${await sha256Hex(new TextEncoder().encode(commitSeed))}`;

      const commit: AgentCommit = {
        oid: commitOid,
        parent_oids: parentOids,
        tree_oid: req.state_root,
        capability_log_oid: req.capability_log_head,
        agent_id: req.agent_id,
        generation,
        message: req.message,
        eval_score: req.eval_score,
        timestamp_ms: now,
      };

      // 3. Store commit object in R2
      await env.AGENT_BUCKET.put(`commits/${commitOid}`, JSON.stringify(commit), {
        httpMetadata: { contentType: "application/json" },
      });

      // 4. Update manifest atomically using conditional write
      const newManifest: RefManifest = {
        repo,
        branch: req.branch,
        head_commit_oid: commitOid,
        generation,
        updated_at_unix_ms: now,
        writer_agent_id: req.agent_id,
      };

      const putOptions: R2PutOptions = {
        httpMetadata: { contentType: "application/json" },
      };
      if (currentEtag) {
        const cleanEtag = currentEtag.replace(/^W\//, "").replace(/"/g, "");
        putOptions.onlyIf = { etagMatches: cleanEtag };
      }

      const putResult = await env.AGENT_BUCKET.put(
        refKey,
        JSON.stringify(newManifest),
        putOptions
      );

      if (!putResult) {
        // CAS conflict happened while preparing commit
        const fresh = await env.AGENT_BUCKET.get(refKey);
        const freshManifest = fresh ? await fresh.json<RefManifest>() : null;
        return Response.json(
          {
            error: "precondition failed (concurrent branch race)",
            current_head: freshManifest?.head_commit_oid ?? null,
            current_etag: fresh?.httpEtag ?? null,
          },
          { status: 412 }
        );
      }

      return Response.json(
        {
          manifest: newManifest,
          commit,
          etag: putResult.httpEtag,
        },
        { status: 200, headers: { "ETag": putResult.httpEtag } }
      );
    }

    return Response.json({ error: "not found", path }, { status: 404 });
  },
};
