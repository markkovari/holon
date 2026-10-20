// Needs the examples' gen/ transpiled first (the runner script does it).

import { writeFileSync } from "node:fs";
import { measure, type Result } from "./measure.js";

import { accounts, authorizer, session } from "../../examples/jco-embed/gen/auth_guard.js";
import { cache } from "../../examples/jco-cache/gen/cache.js";
import { store as idem } from "../../examples/jco-idempotency/gen/idempotency_guard.js";
import { evaluator as flags } from "../../examples/jco-featureflags/gen/feature_flags.js";
import { blobstore as blob } from "../../examples/jco-blob/gen/blob_store.js";
import { recorder as audit } from "../../examples/jco-audit/gen/audit_log.js";
import { verifier as webhook } from "../../examples/jco-webhook/gen/webhook_ingest.js";
import { __seed as seedWebhookKv } from "../../examples/jco-webhook/src/keyvalue-shim.js";
import { createHmac } from "node:crypto";
import { store as sessstore } from "../../examples/jco-session/gen/session_store.js";
import { queue as outbox } from "../../examples/jco-outbox/gen/outbox.js";
import { vault as secrets } from "../../examples/jco-secrets/gen/secrets_vault.js";
import { store as cfg } from "../../examples/jco-config/gen/config_store.js";
import { index as search } from "../../examples/jco-search/gen/search_index.js";
import { arithmetic as money } from "../../examples/jco-money/gen/money.js";
import { generator as slug } from "../../examples/jco-slug/gen/slug.js";
import { validator as validate } from "../../examples/jco-validate/gen/validate.js";
import { cursors as paginate } from "../../examples/jco-pagination/gen/pagination.js";
import { catalog as i18n } from "../../examples/jco-i18n/gen/i18n_catalog.js";
import { renderer as email } from "../../examples/jco-email/gen/email_render.js";
import { gate as upload } from "../../examples/jco-upload/gen/upload_policy.js";
import { authenticator as otp } from "../../examples/jco-otp/gen/otp.js";
import { meter as quota } from "../../examples/jco-quota/gen/quota.js";
import { coords as geo } from "../../examples/jco-geo/gen/geo.js";
import { codec as csv } from "../../examples/jco-csv/gen/csv.js";
import { signer as websign } from "../../examples/jco-websign/gen/webhook_sign.js";
import { redactor as pii } from "../../examples/jco-pii/gen/pii_redact.js";
import { patcher as jsonpatch } from "../../examples/jco-jsonpatch/gen/jsonpatch.js";
import { renderer as markdown } from "../../examples/jco-markdown/gen/markdown.js";
import { generator as id } from "../../examples/jco-id/gen/id_generate.js";
import { store as records } from "../../examples/jco-record/gen/record_store.js";
import { guard as policy } from "../../examples/jco-policy/gen/policy_guard.js";
import { inference as ai } from "../../examples/jco-ai/gen/ai_inference.composed.js";
import { timer } from "../../examples/jco-timer/gen/timer.js";
import { mutex as lock } from "../../examples/jco-lock/gen/lock.js";
import { bus as eventbus } from "../../examples/jco-eventbus/gen/eventbus.js";

const enc = (s: string) => new TextEncoder().encode(s);

async function main() {
  const results: Result[] = [];
  const tenant = "bench";
  const password = "hunter2hunter";


  let n = 0;
  results.push(
    await measure("register", () => accounts.register(`u${n++}@b.com`, password, tenant), {
      iters: 300,
    }),
  );

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

  cache.set("k", enc("v"), 60n);
  results.push(await measure("cache.get(hit)", () => cache.get("k"), { iters: 20000 }));
  results.push(await measure("cache.set", () => cache.set("k", enc("v"), 60n), { iters: 20000 }));
  results.push(await measure("cache.get(miss)", () => cache.get("absent"), { iters: 20000 }));

  let m = 0;
  results.push(
    await measure("idem.begin(miss)", () => idem.begin(`miss-${m++}`, 3600n), { iters: 20000 }),
  );
  idem.begin("hit", 3600n);
  idem.complete("hit", 200, enc('{"ok":true}'));
  results.push(await measure("idem.begin(replay)", () => idem.begin("hit", 3600n), { iters: 20000 }));
  results.push(
    await measure("idem.complete", () => idem.complete("done", 200, enc('{"ok":true}')), {
      iters: 20000,
    }),
  );

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

  const obj1k = new Uint8Array(1024).fill(65);
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

  // record-event also echoes to stderr; that cost is in the number.
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

  const evt = enc('{"event":"user.created","id":"u1"}');
  results.push(
    await measure("outbox.enqueue", () => outbox.enqueue("user.created", evt, 0n), {
      iters: 10000,
    }),
  );

  const secret = enc("super-secret-api-key-value");
  secrets.put("bench-key", secret);
  results.push(await measure("secrets.get(decrypt)", () => secrets.get("bench-key"), { iters: 10000 }));
  let sv = 0;
  results.push(
    await measure("secrets.put(encrypt)", () => secrets.put(`k${sv++}`, secret), { iters: 5000 }),
  );

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

  const m1 = money.parse("10.99", "USD");
  const m2 = money.parse("0.01", "USD");
  results.push(await measure("money.add", () => money.add(m1, m2), { iters: 50000 }));
  results.push(await measure("money.allocate(3)", () => money.allocate(m1, 3), { iters: 50000 }));

  results.push(
    await measure("slug.slugify", () => slug.slugify("The Quick Brown Fox: Café déjà vu!"), {
      iters: 50000,
    }),
  );

  const vrules = [
    { field: "name", kind: "text" as const, required: true, minLen: 2, maxLen: 50, minValue: undefined, maxValue: undefined, oneOf: [] },
    { field: "age", kind: "integer" as const, required: true, minLen: 0, maxLen: 0, minValue: 0, maxValue: 130, oneOf: [] },
    { field: "email", kind: "email" as const, required: true, minLen: 0, maxLen: 0, minValue: undefined, maxValue: undefined, oneOf: [] },
  ];
  const vjson = JSON.stringify({ name: "Alice", age: 30, email: "a@b.com" });
  results.push(await measure("validate.validate", () => validate.validate(vjson, vrules), { iters: 20000 }));

  const pos = { sortKey: "2026-06-18T00:00:00Z", lastId: "row-1234", forward: true };
  const cursor = paginate.encode(pos);
  results.push(await measure("paginate.encode", () => paginate.encode(pos), { iters: 20000 }));
  results.push(await measure("paginate.decode(verify)", () => paginate.decode(cursor), { iters: 20000 }));

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

  results.push(
    await measure("upload.authorize(sign)", () => upload.authorize("acme", "image/png", 2048n, 0n), {
      iters: 10000,
    }),
  );
  const ticket = upload.authorize("acme", "image/png", 2048n, 0n);
  results.push(await measure("upload.redeem(verify)", () => upload.redeem(ticket.token), { iters: 10000 }));

  const otpSecret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
  results.push(await measure("otp.totp-now", () => otp.totpNow(otpSecret), { iters: 20000 }));
  const otpCode = otp.totpNow(otpSecret);
  results.push(
    await measure("otp.verify", () => otp.verify(otpSecret, otpCode, 30, 6, 1), { iters: 20000 }),
  );

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

  const london = { lat: 51.5074, lon: -0.1278 };
  const paris = { lat: 48.8566, lon: 2.3522 };
  results.push(
    await measure("geo.distance", () => geo.distanceMeters(london, paris), { iters: 50000 }),
  );
  results.push(await measure("geo.classify-ip", () => geo.classifyIp("192.168.1.1"), { iters: 50000 }));

  const csvOpts = { delimiter: "", hasHeader: false, trim: false };
  const csvText = "a,b,c\n1,2,3\n4,5,6\n7,8,9\n10,11,12";
  results.push(await measure("csv.parse(5x3)", () => csv.parse(csvText, csvOpts), { iters: 20000 }));
  const csvRows = csv.parse(csvText, csvOpts);
  results.push(await measure("csv.format(5x3)", () => csv.format(csvRows, csvOpts), { iters: 20000 }));

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

  const piitext = "Contact john@example.com or 555-123-4567, card 4242 4242 4242 4242, ip 10.0.0.1";
  results.push(await measure("pii.redact", () => pii.redact(piitext, { kinds: [] }), { iters: 20000 }));

  const jpdoc = '{"name":"Alice","age":30,"tags":["a","b"]}';
  const jppatch = '[{"op":"replace","path":"/age","value":31},{"op":"add","path":"/tags/-","value":"c"}]';
  results.push(await measure("jsonpatch.apply(6902)", () => jsonpatch.applyPatch(jpdoc, jppatch), { iters: 20000 }));
  results.push(
    await measure("jsonpatch.merge(7386)", () => jsonpatch.applyMerge(jpdoc, '{"age":32,"name":null}'), {
      iters: 20000,
    }),
  );

  const mdsrc = "# Title\n\nSome **bold** and *italic* text with `code` and a [link](https://x.com).\n\n- one\n- two";
  results.push(await measure("markdown.to-html", () => markdown.toHtml(mdsrc), { iters: 20000 }));

  results.push(await measure("id.ulid", () => id.ulid(), { iters: 50000 }));
  results.push(await measure("id.uuid-v4", () => id.uuidV4(), { iters: 50000 }));
  results.push(await measure("id.nanoid(21)", () => id.nanoid(21), { iters: 50000 }));
  results.push(await measure("id.short-code(8)", () => id.shortCode(8), { iters: 50000 }));

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
  for (let i = 0; i < 20; i++) records.create("bench-fb", JSON.stringify({ owner: "u1", n: i }), ["owner"]);
  results.push(
    await measure("record.find-by(index)", () => records.findBy("bench-fb", "owner", JSON.stringify("u1")), {
      iters: 10000,
    }),
  );

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

  // A MOCK provider: measures the abstraction, not a real LLM call.
  const aiText = "Bella, a 4yo Labrador, limping on the left hind leg with mild stifle swelling.";
  results.push(await measure("ai.summarize(mock)", () => ai.summarize(aiText, "brief", "clinical"), { iters: 5000 }));
  results.push(await measure("ai.classify(mock)", () => ai.classify(aiText, ["urgent", "routine"]), { iters: 5000 }));
  results.push(await measure("ai.embed(mock)", () => ai.embed("golden retriever"), { iters: 5000 }));

  let ts = 0;
  const tpayload = enc("remind");
  results.push(
    await measure("timer.schedule-at", () => timer.scheduleAt(`j${ts++}`, 9_999_999_999n, tpayload), {
      iters: 2000,
    }),
  );
  // `due` is an O(n) scan of the live index: keep its backlog small.
  for (let i = 0; i < ts; i++) timer.cancel(`j${i}`);
  for (let i = 0; i < 16; i++) timer.scheduleAt(`due-${i}`, 1n, tpayload); // run-at in the past = due
  results.push(await measure("timer.due(16)", () => timer.due(1_000_000_000n, 8, 60n), { iters: 5000 }));

  let lk = 0;
  results.push(
    await measure("lock.acquire", () => lock.acquire(`bench-lock-${lk++}`, "worker", 60n), {
      iters: 10000,
    }),
  );
  const lease = lock.acquire("bench-lock-fixed", "worker", 600n) as { token: string };
  results.push(await measure("lock.renew", () => lock.renew(lease.token, 600n), { iters: 10000 }));
  results.push(await measure("lock.holder(peek)", () => lock.holder("bench-lock-fixed"), { iters: 20000 }));

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
