# Cloudflare Edge Agent Coordination & CAS Storage Gateway

This package provides a serverless edge deployment of the agent coordination and Git-lineage CAS gateway on **Cloudflare Workers** backed by **Cloudflare R2**.

## Architecture & Primitives
- **Instant Cold Starts (<5ms)**: V8 isolate execution with 0 container spin-up overhead.
- **Scale-to-Zero**: Zero background processes; billing only occurs during active HTTP requests.
- **Atomic Compare-and-Swap (CAS)**: Lock-free branch updates via R2 conditional writes (`onlyIf: { etagMatches }`).
- **Container-Free Agent Checkpoints**: Typed state Merkle DAGs and tamper-evident capability hash-chains (<50 KB).

## API Endpoints

### 1. Ingest Content-Addressed Chunk
```bash
curl -X POST https://<worker-subdomain>.workers.dev/v1/cas/objects \
  -H "Content-Type: application/octet-stream" \
  --data-binary @state_delta.bin
# Returns: {"oid": "sha256:...", "size": 1024}
```

### 2. Fetch Content-Addressed Chunk
```bash
curl https://<worker-subdomain>.workers.dev/v1/cas/objects/sha256:<hash> -o state_delta.bin
```

### 3. Read Branch Manifest & ETag
```bash
curl -i https://<worker-subdomain>.workers.dev/v1/manifests/holon-core/feature-alpha
# Returns HTTP 200 with ETag header and JSON body
```

### 4. Push Agent Checkpoint (Git-Lineage Commit + CAS)
```bash
curl -i -X POST https://<worker-subdomain>.workers.dev/v1/checkpoints/holon-core \
  -H "Content-Type: application/json" \
  -d '{
    "branch": "feature-alpha",
    "agent_id": "agent-worker-01",
    "state_root": "sha256:abcd...",
    "capability_log_head": "sha256:1234...",
    "message": "Completed step 3 of refactoring",
    "expected_etag": "\"previous-etag\""
  }'
# If ETag matches: HTTP 200 with new commit & manifest
# If ETag conflicts: HTTP 412 Precondition Failed with latest head pointer
```

## Local Development & Deployment

### Run locally with Wrangler
```bash
npx wrangler dev
```

### Deploy to Cloudflare
```bash
# 1. Create R2 bucket (one-time setup)
npx wrangler r2 bucket create holon-agent-lineage

# 2. Deploy Worker
npx wrangler deploy
```
