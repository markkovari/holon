// In-process benchmark: the components running via jco IN this Node process —
// the raw compute cost of each op, no network, no host. Reuses the transpiled
// `gen/` from the jco-embed (auth) and jco-cache (cache) examples.
//
// Run their transpile first (the runner script does it):
//   (cd ../examples/jco-embed && npm run transpile)
//   (cd ../examples/jco-cache  && npm run transpile)

import { writeFileSync } from "node:fs";
import { measure, type Result } from "./measure.js";

// auth-guard (composed: includes rate-limiter) — exported interfaces.
import { accounts, authorizer, session } from "../../examples/jco-embed/gen/auth_guard.js";
// cache:store
import { cache } from "../../examples/jco-cache/gen/cache.js";
// idempotency:guard/store
import { store as idem } from "../../examples/jco-idempotency/gen/idempotency_guard.js";
// featureflags:guard/evaluator
import { evaluator as flags } from "../../examples/jco-featureflags/gen/feature_flags.js";
// blob:store/blobstore
import { blobstore as blob } from "../../examples/jco-blob/gen/blob_store.js";
// audit:log/recorder
import { recorder as audit } from "../../examples/jco-audit/gen/audit_log.js";
// webhook:ingest/verifier (composed with idempotency-guard)
import { verifier as webhook } from "../../examples/jco-webhook/gen/webhook_ingest.js";
import { __seed as seedWebhookKv } from "../../examples/jco-webhook/src/keyvalue-shim.js";
import { createHmac } from "node:crypto";
// session:store/store (server-side sessions + csrf) — aliased to avoid the
// auth-guard `session` (auth's session-lookup) name clash above.
import { store as sessstore } from "../../examples/jco-session/gen/session_store.js";
// outbox:dispatch/queue
import { queue as outbox } from "../../examples/jco-outbox/gen/outbox.js";
// secrets:vault/vault
import { vault as secrets } from "../../examples/jco-secrets/gen/secrets_vault.js";
// config:store/store
import { store as cfg } from "../../examples/jco-config/gen/config_store.js";
// search:index/index
import { index as search } from "../../examples/jco-search/gen/search_index.js";
// --- tier-2 capabilities ---
// money:amount/arithmetic (pure compute)
import { arithmetic as money } from "../../examples/jco-money/gen/money.js";
// slug:generate/generator (pure compute)
import { generator as slug } from "../../examples/jco-slug/gen/slug.js";
// validate:schema/validator (pure compute)
import { validator as validate } from "../../examples/jco-validate/gen/validate.js";
// paginate:cursor/cursors
import { cursors as paginate } from "../../examples/jco-pagination/gen/pagination.js";
// i18n:catalog/catalog
import { catalog as i18n } from "../../examples/jco-i18n/gen/i18n_catalog.js";
// email:template/renderer
import { renderer as email } from "../../examples/jco-email/gen/email_render.js";
// upload:policy/gate
import { gate as upload } from "../../examples/jco-upload/gen/upload_policy.js";
// --- tier-3 capabilities ---
// otp:totp/authenticator
import { authenticator as otp } from "../../examples/jco-otp/gen/otp.js";
// quota:meter/meter
import { meter as quota } from "../../examples/jco-quota/gen/quota.js";
// geo:resolve/coords (pure compute)
import { coords as geo } from "../../examples/jco-geo/gen/geo.js";
// csv:codec/codec (pure compute)
import { codec as csv } from "../../examples/jco-csv/gen/csv.js";
// --- domain-specific capabilities ---
// webhook:sign/signer
import { signer as websign } from "../../examples/jco-websign/gen/webhook_sign.js";
// pii:redact/redactor (pure compute)
import { redactor as pii } from "../../examples/jco-pii/gen/pii_redact.js";
// json:patch/patcher (pure compute)
import { patcher as jsonpatch } from "../../examples/jco-jsonpatch/gen/jsonpatch.js";
// md:render/renderer (pure compute)
import { renderer as markdown } from "../../examples/jco-markdown/gen/markdown.js";
// --- data-layer + concurrency primitives ---
// id:generate/generator (pure compute: ULID/UUIDv4/nanoid/short-code)
import { generator as id } from "../../examples/jco-id/gen/id_generate.js";
// records:store/store (typed JSON records + secondary indexes over kv)
import { store as records } from "../../examples/jco-record/gen/record_store.js";
// policy:guard/guard (row-level ABAC: setRules + can)
import { guard as policy } from "../../examples/jco-policy/gen/policy_guard.js";
// ai:inference/inference (domain AI verbs over the composed mock provider)
import { inference as ai } from "../../examples/jco-ai/gen/ai_inference.composed.js";
// sched:timer/timer (durable future-job store)
import { timer } from "../../examples/jco-timer/gen/timer.js";
// lock:mutex/mutex (distributed advisory lease)
import { mutex as lock } from "../../examples/jco-lock/gen/lock.js";
// event:bus/bus (durable pub/sub topic log)
import { bus as eventbus } from "../../examples/jco-eventbus/gen/eventbus.js";

const enc = (s: string) => new TextEncoder().encode(s);

async function main() {
  const results: Result[] = [];
  const tenant = "bench";
  const password = "hunter2hunter";

  // --- accounts/session/authorizer ---

  // register: throwaway unique emails (write path: argon2 hash + kv set)
  let n = 0;
  results.push(
    await measure("register", () => accounts.register(`u${n++}@b.com`, password, tenant), {
      iters: 300,
    }),
  );

  // a fixed user for the read/verify paths
  accounts.register("bench@b.com", password, tenant);
  results.push(
    await measure("login", () => accounts.login("bench@b.com", password, tenant), { iters: 300 }),
  );

  const tok = (accounts.login("bench@b.com", password, tenant) as { accessToken: string })
    .accessToken;
  results.push(await measure("introspect", () => authorizer.introspect(tok), { iters: 5000 }));
  results.push(
    await measure(
      "authorize",
      () => {
        try {
          authorizer.authorize(tok, { target: "demo", action: "read" });
        } catch {
          /* 403 throws — still exercises the full path */
        }
      },
      { iters: 5000 },
    ),
  );
  results.push(await measure("session.lookup", () => session.lookup(tok), { iters: 5000 }));

  // --- cache ---
  cache.set("k", enc("v"), 60n);
  results.push(await measure("cache.get(hit)", () => cache.get("k"), { iters: 20000 }));
  results.push(await measure("cache.set", () => cache.set("k", enc("v"), 60n), { iters: 20000 }));
  results.push(await measure("cache.get(miss)", () => cache.get("absent"), { iters: 20000 }));

  // --- idempotency:guard ---
  // begin(miss): unique key each iter -> reserves a fresh pending record (write).
  let m = 0;
  results.push(
    await measure("idem.begin(miss)", () => idem.begin(`miss-${m++}`, 3600n), { iters: 20000 }),
  );
  // a completed key for the replay path.
  idem.begin("hit", 3600n);
  idem.complete("hit", 200, enc('{"ok":true}'));
  results.push(await measure("idem.begin(replay)", () => idem.begin("hit", 3600n), { iters: 20000 }));
  results.push(
    await measure("idem.complete", () => idem.complete("done", 200, enc('{"ok":true}')), {
      iters: 20000,
    }),
  );

  // --- featureflags:guard ---
  const ctx = { tenant: "bench", subject: "user-42" };
  results.push(
    await measure("flags.isEnabled(bool)", () => flags.isEnabled("new-checkout", ctx), {
      iters: 20000,
    }),
  );
  results.push(
    await measure("flags.isEnabled(rollout)", () => flags.isEnabled("beta-search", ctx), {
      iters: 20000,
    }),
  );
  results.push(
    await measure(
      "flags.setRule",
      () => flags.setRule("dark-mode", "bench", { tag: "enabled" }),
      { iters: 20000 },
    ),
  );
  // list-flags walks the keyspace — seed a handful of rules first.
  for (let i = 0; i < 8; i++) flags.setRule(`seeded-${i}`, "bench", { tag: "disabled" });
  results.push(await measure("flags.listFlags", () => flags.listFlags("bench"), { iters: 5000 }));

  // --- blob:store ---
  const obj1k = new Uint8Array(1024).fill(65); // a 1 KiB object
  blob.put("bench", "fixed", obj1k, "application/octet-stream");
  results.push(
    await measure("blob.get(1KiB)", () => blob.get("bench", "fixed"), { iters: 20000 }),
  );
  let bn = 0;
  results.push(
    await measure("blob.put(1KiB)", () => blob.put("bench", `o${bn++}`, obj1k, ""), {
      iters: 20000,
    }),
  );
  results.push(await measure("blob.head", () => blob.head("bench", "fixed"), { iters: 20000 }));

  // --- audit:log ---
  // record-event: kv write + stderr echo (suppress the echo noise via fd later;
  // here it's the compute+write cost).
  results.push(
    await measure(
      "audit.record-event",
      () =>
        audit.recordEvent({
          id: "",
          traceId: "",
          spanId: "",
          timestamp: 0n,
          event: "authorize",
          outcome: "allow",
          tenant: "bench",
          subject: "u1",
          detail: "orders:read",
        }),
      { iters: 5000 },
    ),
  );

  // --- webhook:ingest (composed with idempotency-guard) ---
  const SECRET = "whsec_bench";
  seedWebhookKv("wh-secret", SECRET);
  const wbody = enc('{"event":"ping"}');
  const wsig = createHmac("sha256", SECRET).update(wbody).digest("hex");
  // unique delivery-id each iter -> exercises the full verify + dedup-miss path.
  let w = 0;
  results.push(
    await measure(
      "webhook.ingest(verify+dedup)",
      () => webhook.ingest(wbody, wsig, "wh-secret", `evt-${w++}`),
      { iters: 5000 },
    ),
  );

  // --- session:store ---
  const sdata = enc('{"uid":"u1","roles":["user"]}');
  const sess = sessstore.create(sdata, 3600n);
  results.push(
    await measure("session.create", () => sessstore.create(sdata, 3600n), { iters: 20000 }),
  );
  results.push(await measure("session.get", () => sessstore.get(sess.id), { iters: 20000 }));
  results.push(
    await measure("session.verify-csrf", () => sessstore.verifyCsrf(sess.id, sess.csrfToken), {
      iters: 20000,
    }),
  );

  // --- outbox:dispatch ---
  const evt = enc('{"event":"user.created","id":"u1"}');
  results.push(
    await measure("outbox.enqueue", () => outbox.enqueue("user.created", evt, 0n), {
      iters: 10000,
    }),
  );

  // --- secrets:vault (ChaCha20-Poly1305 envelope) ---
  const secret = enc("super-secret-api-key-value");
  secrets.put("bench-key", secret);
  results.push(await measure("secrets.get(decrypt)", () => secrets.get("bench-key"), { iters: 10000 }));
  let sv = 0;
  results.push(
    await measure("secrets.put(encrypt)", () => secrets.put(`k${sv++}`, secret), { iters: 5000 }),
  );

  // --- config:store ---
  cfg.set("bench", "timeout", { tag: "integer", val: 30n });
  results.push(
    await measure("config.get(typed)", () => cfg.get("bench", "timeout"), { iters: 20000 }),
  );
  results.push(
    await measure(
      "config.set(versioned)",
      () => cfg.set("bench", "hot", { tag: "integer", val: 1n }),
      { iters: 10000 },
    ),
  );

  // --- search:index (TF-IDF inverted index) ---
  for (let i = 0; i < 200; i++) {
    search.indexDoc(`doc-${i}`, `the quick brown fox number ${i} jumps over lazy dogs`, [
      "kind:note",
    ]);
  }
  results.push(
    await measure("search.query(any)", () => search.query("quick fox", "any", [], 10), {
      iters: 5000,
    }),
  );
  let si = 1000;
  results.push(
    await measure(
      "search.index-doc",
      () => search.indexDoc(`new-${si++}`, "fresh document text to index now", []),
      { iters: 5000 },
    ),
  );

  // --- money:amount (pure compute) ---
  const m1 = money.parse("10.99", "USD");
  const m2 = money.parse("0.01", "USD");
  results.push(await measure("money.add", () => money.add(m1, m2), { iters: 50000 }));
  results.push(await measure("money.allocate(3)", () => money.allocate(m1, 3), { iters: 50000 }));

  // --- slug:generate (pure compute) ---
  results.push(
    await measure("slug.slugify", () => slug.slugify("The Quick Brown Fox: Café déjà vu!"), {
      iters: 50000,
    }),
  );

  // --- validate:schema (pure compute) ---
  const vrules = [
    { field: "name", kind: "text" as const, required: true, minLen: 2, maxLen: 50, minValue: undefined, maxValue: undefined, oneOf: [] },
    { field: "age", kind: "integer" as const, required: true, minLen: 0, maxLen: 0, minValue: 0, maxValue: 130, oneOf: [] },
    { field: "email", kind: "email" as const, required: true, minLen: 0, maxLen: 0, minValue: undefined, maxValue: undefined, oneOf: [] },
  ];
  const vjson = JSON.stringify({ name: "Alice", age: 30, email: "a@b.com" });
  results.push(await measure("validate.validate", () => validate.validate(vjson, vrules), { iters: 20000 }));

  // --- paginate:cursor ---
  const pos = { sortKey: "2026-06-18T00:00:00Z", lastId: "row-1234", forward: true };
  const cursor = paginate.encode(pos);
  results.push(await measure("paginate.encode", () => paginate.encode(pos), { iters: 20000 }));
  results.push(await measure("paginate.decode(verify)", () => paginate.decode(cursor), { iters: 20000 }));

  // --- i18n:catalog ---
  i18n.setMessage("en", "greeting", "Hello, {name}! You have {n} messages.");
  results.push(
    await measure(
      "i18n.translate",
      () =>
        i18n.translate("en", "greeting", [
          { name: "name", value: "Al" },
          { name: "n", value: "3" },
        ]),
      { iters: 20000 },
    ),
  );

  // --- email:template (html-escaping render) ---
  email.putTemplate("welcome", {
    subject: "Welcome {name}",
    text: "Hi {name}, your code is {code}",
    html: "<p>Hi {name}, code <b>{code}</b></p>",
  });
  results.push(
    await measure(
      "email.render",
      () =>
        email.render("welcome", [
          { name: "name", value: "Al" },
          { name: "code", value: "123" },
        ]),
      { iters: 20000 },
    ),
  );

  // --- upload:policy (HMAC ticket mint + redeem) ---
  results.push(
    await measure("upload.authorize(sign)", () => upload.authorize("acme", "image/png", 2048n, 0n), {
      iters: 10000,
    }),
  );
  const ticket = upload.authorize("acme", "image/png", 2048n, 0n);
  results.push(await measure("upload.redeem(verify)", () => upload.redeem(ticket.token), { iters: 10000 }));

  // --- otp:totp (HMAC-SHA1) ---
  const otpSecret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
  results.push(await measure("otp.totp-now", () => otp.totpNow(otpSecret), { iters: 20000 }));
  const otpCode = otp.totpNow(otpSecret);
  results.push(
    await measure("otp.verify", () => otp.verify(otpSecret, otpCode, 30, 6, 1), { iters: 20000 }),
  );

  // --- quota:meter (atomic counter) ---
  results.push(
    await measure("quota.reserve", () => quota.reserve("bench", 1n, 1_000_000_000n, 2592000n), {
      iters: 20000,
    }),
  );
  results.push(
    await measure("quota.peek", () => quota.peek("bench", 1_000_000_000n, 2592000n), {
      iters: 20000,
    }),
  );

  // --- geo:resolve (pure compute) ---
  const london = { lat: 51.5074, lon: -0.1278 };
  const paris = { lat: 48.8566, lon: 2.3522 };
  results.push(
    await measure("geo.distance", () => geo.distanceMeters(london, paris), { iters: 50000 }),
  );
  results.push(await measure("geo.classify-ip", () => geo.classifyIp("192.168.1.1"), { iters: 50000 }));

  // --- csv:codec (pure compute) ---
  const csvOpts = { delimiter: "", hasHeader: false, trim: false };
  const csvText = "a,b,c\n1,2,3\n4,5,6\n7,8,9\n10,11,12";
  results.push(await measure("csv.parse(5x3)", () => csv.parse(csvText, csvOpts), { iters: 20000 }));
  const csvRows = csv.parse(csvText, csvOpts);
  results.push(await measure("csv.format(5x3)", () => csv.format(csvRows, csvOpts), { iters: 20000 }));

  // --- webhook:sign (HMAC-SHA256 outbound) ---
  const wsbody = enc('{"id":"evt_1","type":"charge.succeeded"}');
  results.push(
    await measure("websign.sign(stripe)", () => websign.sign(wsbody, "whsec_bench", "stripe"), {
      iters: 20000,
    }),
  );
  const wssig = websign.signAt(wsbody, "whsec_bench", "stripe", 1700000000n);
  results.push(
    await measure(
      "websign.verify(stripe)",
      () => websign.verify(wsbody, wssig.header, "whsec_bench", "stripe", 0n),
      { iters: 20000 },
    ),
  );

  // --- pii:redact (pure compute) ---
  const piitext = "Contact john@example.com or 555-123-4567, card 4242 4242 4242 4242, ip 10.0.0.1";
  results.push(await measure("pii.redact", () => pii.redact(piitext, { kinds: [] }), { iters: 20000 }));

  // --- json:patch (pure compute) ---
  const jpdoc = '{"name":"Alice","age":30,"tags":["a","b"]}';
  const jppatch = '[{"op":"replace","path":"/age","value":31},{"op":"add","path":"/tags/-","value":"c"}]';
  results.push(await measure("jsonpatch.apply(6902)", () => jsonpatch.applyPatch(jpdoc, jppatch), { iters: 20000 }));
  results.push(
    await measure("jsonpatch.merge(7386)", () => jsonpatch.applyMerge(jpdoc, '{"age":32,"name":null}'), {
      iters: 20000,
    }),
  );

  // --- md:render (pure compute, safe) ---
  const mdsrc = "# Title\n\nSome **bold** and *italic* text with `code` and a [link](https://x.com).\n\n- one\n- two";
  results.push(await measure("markdown.to-html", () => markdown.toHtml(mdsrc), { iters: 20000 }));

  // --- id:generate (pure compute) ---
  results.push(await measure("id.ulid", () => id.ulid(), { iters: 50000 }));
  results.push(await measure("id.uuid-v4", () => id.uuidV4(), { iters: 50000 }));
  results.push(await measure("id.nanoid(21)", () => id.nanoid(21), { iters: 50000 }));
  results.push(await measure("id.short-code(8)", () => id.shortCode(8), { iters: 50000 }));

  // --- records:store (typed JSON records + secondary index over kv) ---
  // create: a fresh record each iter (write + index maintenance).
  let rc = 0;
  results.push(
    await measure(
      "record.create(indexed)",
      () => records.create("bench", JSON.stringify({ owner: `u${rc++ % 50}`, n: rc }), ["owner"]),
      { iters: 10000 },
    ),
  );
  const rec = records.create("bench-fixed", JSON.stringify({ owner: "u1", n: 1 }), ["owner"]) as { id: string };
  results.push(await measure("record.get", () => records.get("bench-fixed", rec.id), { iters: 20000 }));
  // findBy: the indexed lookup ("all records owned by u1") — seed a few owners.
  for (let i = 0; i < 20; i++) records.create("bench-fb", JSON.stringify({ owner: "u1", n: i }), ["owner"]);
  results.push(
    await measure("record.find-by(index)", () => records.findBy("bench-fb", "owner", JSON.stringify("u1")), {
      iters: 10000,
    }),
  );

  // --- policy:guard (row-level ABAC) ---
  policy.setRules("bench-res", [
    { id: "owner-rw", action: "*", effect: "allow", priority: 10, conditions: [{ left: "resource.owner", op: "eq", right: "principal.subject" }] },
    { id: "staff-any", action: "*", effect: "allow", priority: 10, conditions: [{ left: "principal.role", op: "in-list", right: "doctor,admin" }] },
  ]);
  results.push(
    await measure(
      "policy.can(allow)",
      () =>
        policy.can(
          "bench-res",
          "read",
          [{ key: "subject", value: "u1" }],
          [{ key: "owner", value: "u1" }],
        ),
      { iters: 20000 },
    ),
  );
  results.push(
    await measure(
      "policy.can(deny)",
      () =>
        policy.can(
          "bench-res",
          "read",
          [{ key: "subject", value: "u2" }],
          [{ key: "owner", value: "u1" }],
        ),
      { iters: 20000 },
    ),
  );

  // --- ai:inference (domain verbs over the composed MOCK provider — measures
  // the abstraction + mock cost, NOT a real LLM call) ---
  const aiText = "Bella, a 4yo Labrador, limping on the left hind leg with mild stifle swelling.";
  results.push(await measure("ai.summarize(mock)", () => ai.summarize(aiText, "brief", "clinical"), { iters: 5000 }));
  results.push(await measure("ai.classify(mock)", () => ai.classify(aiText, ["urgent", "routine"]), { iters: 5000 }));
  results.push(await measure("ai.embed(mock)", () => ai.embed("golden retriever"), { iters: 5000 }));

  // --- sched:timer (durable future-job store) ---
  let ts = 0;
  const tpayload = enc("remind");
  // schedule-at writes one record + index-add; bench the write path with unique
  // keys. NOTE: `due` is an O(n) scan of the live index — measuring it against a
  // large all-far-future backlog is pathological (scans every job, finds none
  // eligible, allocates per job per call). So bench schedule-at on its own, then
  // `due` against a SMALL fixed backlog with a modest iter count.
  results.push(
    await measure("timer.schedule-at", () => timer.scheduleAt(`j${ts++}`, 9_999_999_999n, tpayload), {
      iters: 2000,
    }),
  );
  // due against a small, bounded set: cancel the 2000 scheduled above first so
  // the scan stays cheap, leave ~16 due-now jobs, then measure.
  for (let i = 0; i < ts; i++) timer.cancel(`j${i}`);
  for (let i = 0; i < 16; i++) timer.scheduleAt(`due-${i}`, 1n, tpayload); // run-at in the past = due
  results.push(await measure("timer.due(16)", () => timer.due(1_000_000_000n, 8, 60n), { iters: 5000 }));

  // --- lock:mutex (advisory lease) ---
  let lk = 0;
  results.push(
    await measure("lock.acquire", () => lock.acquire(`bench-lock-${lk++}`, "worker", 60n), {
      iters: 10000,
    }),
  );
  const lease = lock.acquire("bench-lock-fixed", "worker", 600n) as { token: string };
  results.push(await measure("lock.renew", () => lock.renew(lease.token, 600n), { iters: 10000 }));
  results.push(await measure("lock.holder(peek)", () => lock.holder("bench-lock-fixed"), { iters: 20000 }));

  // --- event:bus (durable pub/sub log) ---
  const ebpayload = enc('{"id":"appt-1"}');
  results.push(
    await measure("eventbus.publish", () => eventbus.publish("appt.booked", ebpayload), {
      iters: 10000,
    }),
  );
  // poll: a fresh group each iter starts at offset 0 over the log built above.
  let eg = 0;
  results.push(
    await measure("eventbus.poll", () => eventbus.poll("appt.booked", `g${eg++}`, 10), {
      iters: 10000,
    }),
  );

  const out = { kind: "in-process", node: process.version, when: Date.now(), results };
  writeFileSync(new URL("../results-inproc.json", import.meta.url), JSON.stringify(out, null, 2));

  console.table(
    results.map((r) => ({
      op: r.op,
      "mean µs": (r.meanNs / 1000).toFixed(2),
      "p99 µs": (r.p99Ns / 1000).toFixed(2),
      "ops/sec": r.opsPerSec.toLocaleString(),
    })),
  );
}

main();
