#!/usr/bin/env python3
"""Generate the LINKED-topology wadm/OAM manifest for the full vet-clinic on
wasmCloud: vet-domain + every capability as SEPARATE components wired by wadm
links (NOT a single wac-fused blob — that overflows wasmtime's per-component
nested-instance limit). This is the idiomatic wasmCloud shape and the one that
actually deploys.

Run: python3 gen-manifest.py > k8s/vet-domain-linked.yaml
"""

import os
# in-cluster registry + NATS the manifest references — override to match the
# cluster's actual namespaces (e.g. VET_REG=registry.nd-brain.svc.cluster.local:5000).
REG = os.environ.get("VET_REG", "registry.wasmcloud.svc.cluster.local:5000")
NATS = os.environ.get("VET_NATS", "nats://nats.wasmcloud.svc.cluster.local:4222")
# http-server listen address — pick a free port when sharing a lattice/host
# with another app (e.g. the DDD petclinic occupies 8080 + 8081).
ADDR = os.environ.get("VET_ADDR", "0.0.0.0:8081")
# vet-domain replica count (the HTTP-facing component). Capabilities stay at 1.
# Override: VET_REPLICAS=5 python3 gen-manifest.py > ...
DOM_REPLICAS = int(os.environ.get("VET_REPLICAS", "1"))
# LATTICE=1: vet-domain is the wac-fused hybrid artifact (`just
# compose-vet-lattice` -> vet_domain.lattice.wasm) with the 6 pure-compute
# capabilities inside — no wrpc hop for money/validate/md/pii/paginate/upload.
# Those components are dropped from the manifest and their wasi:config knobs
# move onto vet-domain itself (the fused code reads the same keys).
LATTICE = os.environ.get("LATTICE") == "1"

# capabilities fused into vet_domain.lattice.wasm in LATTICE mode.
FUSED = {"money", "validate", "markdown", "pii-redact", "pagination", "upload-policy"}

# component id -> (oci image tag, [wasi keyvalue? ], extra-config)
# every stateful capability links wasi:keyvalue -> keyvalue-nats (own bucket).
# config carries the wasi:config knobs each component reads.
CAPS = {
    # id: (image, needs_kv, config-dict)
    "auth-guard":      ("vet-auth-guard:0.1.0", True, {
        "default-tenant": "acme-vet", "session-ttl": "3600",
        "password-min-len": "8", "audit-enabled": "true",
        "max-attempts": "5", "lockout-window": "300"}),
    "records-store":   ("vet-records-store:0.1.0", True, {}),
    "validate":        ("vet-validate:0.1.0", False, {}),
    "search-index":    ("vet-search-index:0.1.0", True, {}),
    "upload-policy":   ("vet-upload-policy:0.1.0", False, {
        "allowed-types": "image/png,image/jpeg,image/webp,image/gif",
        "max-size": "2097152", "ticket-ttl": "300", "ticket-secret": "vet-upload-secret"}),
    "blob-store":      ("vet-blob-store:0.1.0", True, {}),
    "fsm-workflow":    ("vet-fsm-workflow:0.1.0", True, {}),
    "money":           ("vet-money:0.1.0", False, {}),
    "markdown":        ("vet-markdown:0.1.0", False, {}),
    "csv":             ("vet-csv:0.1.0", False, {}),
    "pii-redact":      ("vet-pii-redact:0.1.0", False, {}),
    "otp":             ("vet-otp:0.1.0", True, {}),
    "secrets-vault":   ("vet-secrets-vault:0.1.0", True, {
        "master-key": "dmV0LWNsaW5pYy1kZW1vLW1hc3Rlci1rZXktMzJiISE="}),
    "pagination":      ("vet-pagination:0.1.0", False, {
        "max-page-size": "100", "cursor-secret": "vet-cursor-secret"}),
    "i18n-catalog":    ("vet-i18n-catalog:0.1.0", True, {}),
    "ai-inference":    ("vet-ai:0.1.0", False, {}),       # ai+mock-llm fused (8 modules, fine)
    "cache":           ("vet-cache:0.1.0", True, {}),     # cache+backing fused; backing needs kv
    "scheduler-timer": ("vet-scheduler-timer:0.1.0", True, {}),
    "lock-mutex":      ("vet-lock-mutex:0.1.0", True, {}),
    "event-bus":       ("vet-event-bus:0.1.0", True, {}),
    # the built React SPA as its own component — vet-domain links it for
    # GET / + /assets/* instead of embedding ~620 KB (which inflated its
    # per-request instantiation cost).
    "static-assets":   ("vet-static-assets:0.1.0", False, {}),
}

# vet-domain import (namespace, package, interfaces) -> target component id.
LINKS = [
    ("auth", "identity", ["accounts", "session", "authorizer", "rbac", "types"], "auth-guard"),
    ("records", "store", ["store"], "records-store"),
    ("validate", "schema", ["validator"], "validate"),
    ("search", "index", ["index"], "search-index"),
    ("upload", "policy", ["gate"], "upload-policy"),
    ("blob", "store", ["blobstore"], "blob-store"),
    ("fsm", "workflow", ["engine"], "fsm-workflow"),
    ("money", "amount", ["arithmetic"], "money"),
    ("md", "render", ["renderer"], "markdown"),
    ("csv", "codec", ["codec"], "csv"),
    ("pii", "redact", ["redactor"], "pii-redact"),
    ("otp", "totp", ["authenticator"], "otp"),
    ("secrets", "vault", ["vault"], "secrets-vault"),
    ("paginate", "cursor", ["cursors"], "pagination"),
    ("i18n", "catalog", ["catalog"], "i18n-catalog"),
    ("ai", "inference", ["inference"], "ai-inference"),
    ("cache", "store", ["cache"], "cache"),
    ("sched", "timer", ["timer"], "scheduler-timer"),
    ("lock", "mutex", ["mutex"], "lock-mutex"),
    ("event", "bus", ["bus"], "event-bus"),
    ("ui", "assets", ["files"], "static-assets"),
]

if LATTICE:
    # the fused caps' config keys move onto vet-domain (same wasi:config reads).
    DOM_CONFIG = {}
    for cid in FUSED:
        DOM_CONFIG.update(CAPS[cid][2])
    CAPS = {cid: v for cid, v in CAPS.items() if cid not in FUSED}
    LINKS = [l for l in LINKS if l[3] not in FUSED]
    DOM_IMAGE = "vet-vet-domain-lattice:0.1.0"
else:
    DOM_CONFIG = {}
    DOM_IMAGE = "vet-vet-domain:0.4.0"

def kv_link(bucket):
    return f"""        - type: link
          properties:
            name: default
            namespace: wasi
            package: keyvalue
            interfaces: [store, atomics]
            target:
              name: keyvalue-nats
              config:
                - name: {bucket}-bucket
                  properties:
                    bucket: {bucket}
                    cluster_uri: {NATS}
                    enable_bucket_auto_create: "true\""""

shape = (
    "HYBRID topology: the 6 pure-compute caps are wac-fused INTO vet-domain\n"
    "# (28 core modules, under the 30 limit); stateful caps stay wadm links"
    if LATTICE
    else "LINKED topology: every capability is a SEPARATE component wired to\n"
    "# vet-domain by wadm links — NOT one wac-fused blob (104 core modules,\n"
    "# over wasmtime's 30 nested-instance per-component limit)"
)
out = []
out.append(f"""# FULL vet-clinic on wasmCloud (generated by gen-manifest.py).
# {shape}. The host runs all components + their links, well within its
# multi-thousand instance density.
apiVersion: core.oam.dev/v1beta1
kind: Application
metadata:
  name: vet-domain
  namespace: vet-clinic
  annotations:
    version: v0.2.0-{'lattice-' if LATTICE else ''}r{DOM_REPLICAS}
    description: "Full vet-clinic, {'hybrid fuse+link' if LATTICE else 'linked-component'} topology, on wasmCloud k8s"
spec:
  components:""")

# vet-domain (the HTTP handler) — links to every capability + gets fronted by httpserver.
dom = ["""    - name: vet-domain
      type: component
      properties:
        image: oci://%s/%s""" % (REG, DOM_IMAGE)]
if DOM_CONFIG:
    dom.append("        config:")
    dom.append("          - name: vet-domain-config")
    dom.append("            properties:")
    for k, v in sorted(DOM_CONFIG.items()):
        dom.append(f'              {k}: "{v}"')
dom.append("""      traits:
        - type: spreadscaler
          properties:
            instances: %d""" % DOM_REPLICAS)
for ns, pkg, ifaces, target in LINKS:
    dom.append(f"""        - type: link
          properties:
            namespace: {ns}
            package: {pkg}
            interfaces: [{', '.join(ifaces)}]
            target:
              name: {target}""")
out.append("\n".join(dom))

# each capability component.
for cid, (img, needs_kv, cfg) in CAPS.items():
    block = [f"""    - name: {cid}
      type: component
      properties:
        image: oci://{REG}/{img}"""]
    if cfg:
        block.append("        config:")
        block.append(f"          - name: {cid}-config")
        block.append("            properties:")
        for k, v in cfg.items():
            block.append(f'              {k}: "{v}"')
    block.append("""      traits:
        - type: spreadscaler
          properties:
            instances: 1""")
    if needs_kv:
        block.append(kv_link(cid.replace("-", "")))
    # auth-guard also needs outbound http (OIDC/JWKS).
    if cid == "auth-guard":
        block.append("""        - type: link
          properties:
            namespace: wasi
            package: http
            interfaces: [outgoing-handler]
            target:
              name: httpclient""")
    out.append("\n".join(block))

# providers: httpserver (fronts vet-domain), httpclient, keyvalue-nats.
out.append(f"""    - name: httpserver
      type: capability
      properties:
        image: ghcr.io/wasmcloud/http-server:0.23.1
      traits:
        - type: link
          properties:
            namespace: wasi
            package: http
            interfaces: [incoming-handler]
            source:
              config:
                - name: listen
                  properties:
                    address: {ADDR}
            target:
              name: vet-domain
    - name: httpclient
      type: capability
      properties:
        image: ghcr.io/wasmcloud/http-client:0.13.0
    - name: keyvalue-nats
      type: capability
      properties:
        image: ghcr.io/wasmcloud/keyvalue-nats:0.3.1""")

print("\n".join(out))
