# Every WIT package this repository defines

Generated — `just wit-surfaces`. Do not edit.

Rendered by `wasm-tools component wit` out of the BUILT components, so this
is the shape that actually shipped rather than the shape the source suggests.
Doc comments are not part of it, so editing one does not churn this file.

**The diff is the review.** A change here is a change to a contract. If a
package's shape moved and its version did not, `witsurface.rs` fails — and
it is right to, because an artifact built against the old shape will fail to
plug with a message that names neither the interface nor the reason.

Adding a *function* to an interface is compatible; adding a case to a
*variant* or a field to a *record* is not. Both measured, not assumed.

118 interfaces.

## `ai:inference/inference@0.1.0`

```wit
  interface inference {
    variant assist-error {
      inference-failed(string),
      unexpected-output(string),
      invalid-request(string),
    }

    enum length {
      brief,
      normal,
      detailed,
    }

    record label-score {
      label: string,
      confidence: u32,
    }

    summarize: func(text: string, len: length, focus: string) -> result<string, assist-error>;

    classify: func(text: string, labels: list<string>) -> result<label-score, assist-error>;

    extract: func(text: string, fields: list<string>) -> result<list<tuple<string, string>>, assist-error>;

    generate: func(prompt: string, context: string) -> result<string, assist-error>;

    rewrite: func(text: string, style: string) -> result<string, assist-error>;

    embed: func(text: string) -> result<list<f32>, assist-error>;
  }
```

## `ai:local/local@0.1.0`

```wit
  interface local {
    infer: func(prompt: string) -> string;
  }
```

## `artifact:cache/store@0.1.0`

```wit
  interface store {
    variant cache-error {
      unavailable(string),
      invalid(string),
      not-your-claim(string),
    }

    record artifact-key {
      producer: string,
      version: string,
      inputs: list<string>,
      params: string,
    }

    record artifact {
      id: string,
      bytes: list<u8>,
      content-type: string,
      producer: string,
      stored-at: u64,
    }

    variant outcome {
      hit(artifact),
      claimed(string),
      pending(u64),
    }

    derive-id: func(key: artifact-key) -> string;

    lookup: func(key: artifact-key) -> result<outcome, cache-error>;

    get: func(id: string) -> result<option<artifact>, cache-error>;

    put: func(claim: string, bytes: list<u8>, content-type: string) -> result<string, cache-error>;

    abandon: func(claim: string) -> result<_, cache-error>;
  }
```

## `audit:log/query@0.1.0`

```wit
  interface query {
    use types.{event, audit-error};

    recent: func(limit: u32) -> result<list<event>, audit-error>;

    by-trace: func(trace-id: string) -> result<list<event>, audit-error>;
  }
```

## `audit:log/recorder@0.1.0`

```wit
  interface recorder {
    use types.{event, audit-error};

    record-event: func(e: event) -> result<_, audit-error>;
  }
```

## `auth:identity/accounts@0.1.0`

```wit
  interface accounts {
    use types.{principal, token-pair, auth-error};

    register: func(email: string, password: string, tenant: string) -> result<principal, auth-error>;

    login: func(email: string, password: string, tenant: string) -> result<token-pair, auth-error>;

    verify-password: func(email: string, password: string, tenant: string) -> result<principal, auth-error>;

    change-password: func(email: string, tenant: string, current-password: string, new-password: string) -> result<_, auth-error>;
  }
```

## `auth:identity/authorizer@0.1.0`

```wit
  interface authorizer {
    use types.{principal, permission, auth-error};

    authorize: func(token: string, required: permission) -> result<principal, auth-error>;

    authorize-any: func(token: string, required: list<permission>) -> result<principal, auth-error>;

    introspect: func(token: string) -> result<principal, auth-error>;

    authorize-traced: func(token: string, required: permission, traceparent: string) -> result<principal, auth-error>;
  }
```

## `auth:identity/jwt@0.1.0`

```wit
  interface jwt {
    use types.{claims, auth-error};

    verify: func(token: string) -> result<claims, auth-error>;
  }
```

## `auth:identity/oidc@0.1.0`

```wit
  interface oidc {
    use types.{claims, token-pair, auth-error};

    record oidc-config {
      issuer: string,
      authorization-endpoint: string,
      token-endpoint: string,
      jwks-uri: string,
      userinfo-endpoint: option<string>,
    }

    discover: func(issuer: string) -> result<oidc-config, auth-error>;

    verify-id-token: func(token: string) -> result<claims, auth-error>;

    exchange-code: func(code: string, redirect-uri: string) -> result<token-pair, auth-error>;
  }
```

## `auth:identity/rbac@0.1.0`

```wit
  interface rbac {
    use types.{principal, permission, auth-error};

    check: func(p: principal, required: permission) -> bool;

    assign-role: func(tenant: string, subject: string, role: string) -> result<_, auth-error>;

    revoke-role: func(tenant: string, subject: string, role: string) -> result<_, auth-error>;

    roles-for: func(tenant: string, subject: string) -> result<list<string>, auth-error>;

    permissions-of: func(tenant: string, role: string) -> result<list<permission>, auth-error>;

    set-role-permissions: func(tenant: string, role: string, permissions: list<permission>) -> result<_, auth-error>;
  }
```

## `auth:identity/session@0.1.0`

```wit
  interface session {
    use types.{principal, token-pair, auth-error};

    issue: func(p: principal) -> result<token-pair, auth-error>;

    refresh: func(refresh-token: string) -> result<token-pair, auth-error>;

    revoke: func(session-id: string) -> result<_, auth-error>;

    lookup: func(session-id: string) -> result<principal, auth-error>;
  }
```

## `auth:identity/types@0.1.0`

```wit
  interface types {
    type timestamp = u64;

    record permission {
      target: string,
      action: string,
    }

    record principal {
      subject: string,
      tenant: string,
      roles: list<string>,
      scopes: list<string>,
      expires-at: timestamp,
    }

    record claims {
      iss: string,
      sub: string,
      aud: list<string>,
      exp: timestamp,
      iat: timestamp,
      scopes: list<string>,
      raw: list<tuple<string, string>>,
    }

    record token-pair {
      access-token: string,
      refresh-token: option<string>,
      expires-in: u64,
      session-id: option<string>,
    }

    variant auth-error {
      invalid-token(string),
      expired,
      insufficient-scope(permission),
      unknown-tenant,
      backend-unavailable(string),
      malformed(string),
      invalid-credentials,
      already-exists,
      rate-limited(u32),
      internal(string),
    }
  }
```

## `barcode:read/reader@0.1.0`

```wit
  interface reader {
    record symbol {
      text: string,
      symbology: string,
    }

    variant read-error {
      bad-image(string),
      not-found,
    }

    decode-png: func(image: list<u8>) -> result<symbol, read-error>;
  }
```

## `blob:store/blobstore@0.1.0`

```wit
  interface blobstore {
    variant blob-error {
      not-found,
      backend-unavailable(string),
    }

    record object-info {
      name: string,
      size: u64,
      content-type: string,
    }

    put: func(container: string, name: string, data: list<u8>, content-type: string) -> result<_, blob-error>;

    get: func(container: string, name: string) -> result<list<u8>, blob-error>;

    head: func(container: string, name: string) -> result<object-info, blob-error>;

    exists: func(container: string, name: string) -> result<bool, blob-error>;

    delete: func(container: string, name: string) -> result<_, blob-error>;

    list-objects: func(container: string, prefix: string) -> result<list<object-info>, blob-error>;
  }
```

## `bytes:codec/codec@0.1.0`

```wit
  interface codec {
    enum alphabet {
      standard,
      url-safe,
    }

    variant decode-error {
      not-in-alphabet(tuple<u32, string>),
      truncated-group(u32),
      misplaced-padding(u32),
    }

    encode: func(bytes: list<u8>, alphabet: alphabet) -> string;

    decode: func(text: string, alphabet: alphabet) -> result<list<u8>, decode-error>;

    to-hex: func(bytes: list<u8>) -> string;

    from-hex: func(text: string) -> result<list<u8>, decode-error>;
  }
```

## `cache:store/cache@0.1.0`

```wit
  interface cache {
    variant cache-error {
      backend-unavailable(string),
      source-failed(string),
    }

    get: func(key: string) -> result<option<list<u8>>, cache-error>;

    set: func(key: string, value: list<u8>, ttl-seconds: u64) -> result<_, cache-error>;

    peek: func(key: string) -> result<option<list<u8>>, cache-error>;

    delete: func(key: string) -> result<_, cache-error>;

    invalidate: func(key: string) -> result<_, cache-error>;

    invalidate-prefix: func(prefix: string) -> result<u32, cache-error>;

    ttl: func(key: string) -> result<option<u64>, cache-error>;

    get-through: func(key: string, ttl-seconds: u64) -> result<option<list<u8>>, cache-error>;

    put-through: func(key: string, value: list<u8>, ttl-seconds: u64) -> result<_, cache-error>;

    put-behind: func(key: string, value: list<u8>, ttl-seconds: u64) -> result<_, cache-error>;

    flush: func() -> result<u32, cache-error>;
  }
```

## `cache:store/sink@0.1.0`

```wit
  interface sink {
    store: func(key: string, value: list<u8>) -> result<_, string>;

    remove: func(key: string) -> result<_, string>;
  }
```

## `cache:store/source@0.1.0`

```wit
  interface source {
    load: func(key: string) -> result<option<list<u8>>, string>;
  }
```

## `card:identify/identifier@0.1.0`

```wit
  interface identifier {
    enum variant-kind {
      normal,
      holo,
      reverse-holo,
      first-edition,
      shadowless,
      special,
    }

    enum condition {
      mint,
      near-mint,
      lightly-played,
      moderately-played,
      heavily-played,
      damaged,
    }

    record grade {
      grader: string,
      tenths: u16,
    }

    record guess {
      name: string,
      set-name: string,
      set-code: string,
      number: string,
      rarity: string,
      language: string,
      printing: option<variant-kind>,
      condition: option<condition>,
      graded: option<grade>,
      confidence: u8,
      needs-review: list<string>,
    }

    variant identify-error {
      no-card(string),
      more-than-one-card,
      refused(string),
      unparseable(string),
      no-name,
    }

    parse: func(answer: string) -> result<guess, identify-error>;

    prompt: func() -> string;
  }
```

## `config:store/store@0.1.0`

```wit
  interface store {
    variant config-error {
      not-found,
      type-mismatch(string),
      version-conflict(u32),
      backend-unavailable(string),
    }

    variant value {
      text(string),
      integer(s64),
      boolean(bool),
      decimal(f64),
      json(string),
    }

    record entry {
      value: value,
      version: u32,
      updated: u64,
    }

    get: func(namespace: string, key: string) -> result<entry, config-error>;

    set: func(namespace: string, key: string, value: value) -> result<u32, config-error>;

    set-if: func(namespace: string, key: string, value: value, expected-version: u32) -> result<u32, config-error>;

    delete: func(namespace: string, key: string) -> result<bool, config-error>;

    keys: func(namespace: string, max: u32) -> result<list<string>, config-error>;
  }
```

## `contract:registry/registry@0.1.0`

```wit
  interface registry {
    variant registry-error {
      rejected(string),
      unavailable(string),
      refused(string),
    }

    record contract {
      version: u32,
      body: string,
      canonical: bool,
      owner: string,
      from-request: string,
    }

    enum verdict {
      granted,
      denied,
      counter,
    }

    record request {
      id: string,
      from-part: string,
      to-part: string,
      subject: string,
      body: string,
      at-version: u32,
      answered: bool,
      verdict: verdict,
      answer: string,
    }

    publish: func(body: string) -> result<u32, registry-error>;

    current: func() -> result<contract, registry-error>;

    get: func(version: u32) -> result<option<contract>, registry-error>;

    proposed: func(part: string) -> result<option<contract>, registry-error>;

    ask: func(from-part: string, to-part: string, subject: string, body: string, at-version: u32) -> result<string, registry-error>;

    pending: func(to-part: string) -> result<list<request>, registry-error>;

    answer: func(id: string, v: verdict, body: string) -> result<u32, registry-error>;

    ratify: func(version: u32, part: string, gate-score: s32) -> result<_, registry-error>;

    built-against: func(candidate: string, part: string, version: u32) -> result<_, registry-error>;

    composable: func(candidates: list<string>) -> result<list<string>, registry-error>;
  }
```

## `crdt:merge/merger@0.1.0`

```wit
  interface merger {
    variant crdt-error {
      invalid-json(string),
      invalid-state(string),
      type-mismatch(string),
    }

    merge: func(a: string, b: string) -> result<string, crdt-error>;

    value: func(state: string) -> result<string, crdt-error>;

    lww-new: func(value-json: string, timestamp: u64, replica: string) -> result<string, crdt-error>;

    lww-set: func(state: string, value-json: string, timestamp: u64, replica: string) -> result<string, crdt-error>;

    counter-new: func() -> string;

    counter-add: func(state: string, replica: string, delta: s64) -> result<string, crdt-error>;

    orset-new: func() -> string;

    orset-add: func(state: string, element: string, tag: string) -> result<string, crdt-error>;

    orset-remove: func(state: string, element: string) -> result<string, crdt-error>;

    lwwmap-new: func() -> string;

    lwwmap-set: func(state: string, key: string, value-json: string, timestamp: u64, replica: string) -> result<string, crdt-error>;

    lwwmap-remove: func(state: string, key: string, timestamp: u64, replica: string) -> result<string, crdt-error>;

    rga-new: func() -> string;

    rga-insert: func(state: string, index: u32, text: string, id-base: string) -> result<string, crdt-error>;

    rga-delete: func(state: string, index: u32, count: u32) -> result<string, crdt-error>;

    rga-insert-after: func(state: string, after-id: string, text: string, id-base: string) -> result<string, crdt-error>;

    rga-delete-ids: func(state: string, ids: list<string>) -> result<string, crdt-error>;

    rga-elements: func(state: string) -> result<string, crdt-error>;
  }
```

## `cron:expr/parser@0.1.0`

```wit
  interface parser {
    variant cron-error {
      invalid-expression(string),
      unsatisfiable(string),
    }

    parse: func(expr: string) -> result<string, cron-error>;

    matches: func(expr: string, unix: u64) -> result<bool, cron-error>;

    next: func(expr: string, after: u64, count: u32) -> result<list<u64>, cron-error>;
  }
```

## `csv:codec/codec@0.1.0`

```wit
  interface codec {
    variant csv-error {
      malformed(string),
      ragged-row(u32),
    }

    record dialect {
      delimiter: string,
      has-header: bool,
      trim: bool,
    }

    record row {
      fields: list<string>,
    }

    record record-row {
      pairs: list<tuple<string, string>>,
    }

    parse: func(text: string, opts: dialect) -> result<list<row>, csv-error>;

    parse-records: func(text: string, opts: dialect) -> result<list<record-row>, csv-error>;

    format: func(rows: list<row>, opts: dialect) -> string;
  }
```

## `deck:build/builder@0.1.0`

```wit
  interface builder {
    enum card-kind {
      basic-pokemon,
      evolved-pokemon,
      trainer,
      basic-energy,
      special-energy,
    }

    record slot {
      card-id: string,
      name: string,
      kind: card-kind,
      quantity: u32,
    }

    record owned {
      card-id: string,
      quantity: u32,
    }

    record price {
      card-id: string,
      unit-minor: s64,
      currency: string,
    }

    variant illegal {
      wrong-size(u32),
      too-many-of-a-name(tuple<string, u32>),
      no-basic-pokemon,
      zero-quantity(string),
    }

    record missing {
      card-id: string,
      name: string,
      quantity: u32,
      cost-minor: option<s64>,
    }

    record shortfall-report {
      missing: list<missing>,
      cost-minor: s64,
      currency: string,
      unpriced: u32,
    }

    legality: func(deck: list<slot>) -> list<illegal>;

    shortfall: func(deck: list<slot>, owned: list<owned>, prices: list<price>, currency: string) -> shortfall-report;
  }
```

## `demo:bigadd/bignum@0.1.0`

```wit
  interface bignum {
    add: func(a: string, b: string) -> string;
  }
```

## `demo:calc/arith@0.1.0`

```wit
  interface arith {
    eval: func(expr: string) -> s64;
  }
```

## `demo:expr/language@0.1.0`

```wit
  interface language {
    eval: func(src: string) -> s64;
  }
```

## `demo:glob/matcher@0.1.0`

```wit
  interface matcher {
    matches: func(pattern: string, text: string) -> bool;
  }
```

## `demo:ordinal/suffix@0.1.0`

```wit
  interface suffix {
    ordinal: func(n: u32) -> string;
  }
```

## `demo:roman/numerals@0.1.0`

```wit
  interface numerals {
    to-roman: func(n: u32) -> string;

    from-roman: func(s: string) -> u32;
  }
```

## `demo:rot13/cipher@0.1.0`

```wit
  interface cipher {
    rot13: func(text: string) -> string;
  }
```

## `demo:shape/pager@0.1.0`

```wit
  interface pager {
    record page {
      hits: list<string>,
      has-more: bool,
    }

    paginate: func(ids: list<string>, size: u32, offset: u32) -> page;
  }
```

## `diff:text/differ@0.1.0`

```wit
  interface differ {
    variant op {
      equal(string),
      insert(string),
      delete(string),
    }

    variant diff-error {
      malformed-patch(string),
      context-mismatch(string),
    }

    diff-lines: func(a: string, b: string) -> list<op>;

    unified: func(a: string, b: string, from-label: string, to-label: string, context: u32) -> string;

    apply-unified: func(a: string, patch: string) -> result<string, diff-error>;
  }
```

## `durable:workflow/orchestrator@0.1.0`

```wit
  interface orchestrator {
    record run-request {
      workflow-id: string,
      payload: string,
    }

    variant run-error {
      not-found(string),
      invalid-input(string),
      worker-failed(string),
      unavailable(string),
    }

    record run-status {
      state: string,
      output: option<string>,
    }

    trigger: func(req: run-request) -> result<string, run-error>;

    start: func(req: run-request) -> result<string, run-error>;

    status: func(run-id: string) -> result<run-status, run-error>;
  }
```

## `email:template/renderer@0.1.0`

```wit
  interface renderer {
    variant render-error {
      unknown-template,
      missing-variable(string),
      backend-unavailable(string),
    }

    record var {
      name: string,
      value: string,
    }

    record template {
      subject: string,
      text: string,
      html: string,
    }

    record message {
      subject: string,
      text: string,
      html: string,
    }

    put-template: func(name: string, tmpl: template) -> result<_, render-error>;

    get-template: func(name: string) -> result<template, render-error>;

    render: func(name: string, vars: list<var>) -> result<message, render-error>;
  }
```

## `event:bus/bus@0.1.0`

```wit
  interface bus {
    variant bus-error {
      backend-unavailable(string),
    }

    record event {
      id: string,
      topic: string,
      payload: list<u8>,
      at: u64,
    }

    publish: func(topic: string, payload: list<u8>) -> result<string, bus-error>;

    poll: func(topic: string, group: string, max: u32) -> result<list<event>, bus-error>;

    ack: func(topic: string, group: string, ids: list<string>) -> result<_, bus-error>;

    pending: func(topic: string, group: string) -> result<u64, bus-error>;

    topics: func() -> result<list<string>, bus-error>;
  }
```

## `experiment:assign/assigner@0.1.0`

```wit
  interface assigner {
    variant assign-error {
      not-found,
      invalid-variants(string),
      backend-unavailable(string),
    }

    record arm {
      name: string,
      weight: u32,
    }

    record context {
      tenant: string,
      subject: string,
    }

    record assignment {
      subject: string,
      arm: string,
    }

    set-experiment: func(name: string, tenant: string, variants: list<arm>) -> result<_, assign-error>;

    clear-experiment: func(name: string, tenant: string) -> result<_, assign-error>;

    assign: func(name: string, ctx: context) -> result<string, assign-error>;

    describe: func(name: string, tenant: string) -> result<list<arm>, assign-error>;

    cohort: func(name: string, tenant: string, n: u32) -> result<list<assignment>, assign-error>;
  }
```

## `featureflags:guard/evaluator@0.1.0`

```wit
  interface evaluator {
    variant flag-error {
      backend-unavailable(string),
    }

    record context {
      tenant: string,
      subject: string,
    }

    variant rule {
      enabled,
      disabled,
      percentage(u8),
    }

    enum source {
      config,
      global-override,
      tenant-override,
    }

    record flag-state {
      name: string,
      rule: rule,
      source: source,
    }

    is-enabled: func(flag: string, ctx: context) -> result<bool, flag-error>;

    set-rule: func(flag: string, tenant: string, rule: rule) -> result<_, flag-error>;

    clear-rule: func(flag: string, tenant: string) -> result<_, flag-error>;

    list-flags: func(tenant: string) -> result<list<flag-state>, flag-error>;
  }
```

## `fsm:workflow/engine@0.1.0`

```wit
  interface engine {
    variant fsm-error {
      unknown-machine,
      unknown-instance,
      illegal-transition(string),
      invalid-definition(string),
      backend-unavailable(string),
    }

    record transition {
      event: string,
      source: string,
      target: string,
    }

    record definition {
      states: list<string>,
      initial: string,
      transitions: list<transition>,
      terminal: list<string>,
    }

    record status {
      machine: string,
      instance: string,
      state: string,
      done: bool,
      steps: u32,
    }

    record history-entry {
      event: string,
      source: string,
      target: string,
      at: u64,
    }

    define: func(name: string, def: definition) -> result<_, fsm-error>;

    get-definition: func(name: string) -> result<definition, fsm-error>;

    create-instance: func(machine: string, instance: string) -> result<status, fsm-error>;

    get-status: func(machine: string, instance: string) -> result<status, fsm-error>;

    can-fire: func(machine: string, instance: string, event: string) -> result<bool, fsm-error>;

    allowed-events: func(machine: string, instance: string) -> result<list<string>, fsm-error>;

    fire: func(machine: string, instance: string, event: string) -> result<status, fsm-error>;

    history: func(machine: string, instance: string) -> result<list<history-entry>, fsm-error>;
  }
```

## `geo:resolve/coords@0.1.0`

```wit
  interface coords {
    variant geo-error {
      bad-coordinate,
      bad-ip,
    }

    record point {
      lat: f64,
      lon: f64,
    }

    record bbox {
      min-lat: f64,
      min-lon: f64,
      max-lat: f64,
      max-lon: f64,
    }

    enum ip-class {
      public,
      private,
      loopback,
      special,
    }

    distance-meters: func(a: point, b: point) -> result<f64, geo-error>;

    bounding-box: func(center: point, radius-meters: f64) -> result<bbox, geo-error>;

    contains: func(box: bbox, p: point) -> bool;

    classify-ip: func(ip: string) -> result<ip-class, geo-error>;
  }
```

## `gherkin:validate/validator@0.1.0`

```wit
  interface validator {
    enum severity {
      error,
      warning,
      declined,
    }

    variant problem-kind {
      no-feature,
      multiple-features,
      content-before-feature(string),
      step-outside-scenario,
      continuation-without-a-step(string),
      outline-without-examples(string),
      examples-outside-a-scenario,
      empty-scenario(string),
      row-width(tuple<u32, u32>),
      repeated-docstring,
      dangling-tag,
      malformed-tag(string),
      invalid-language(string),
      unknown-placeholder(string),
      duplicate-column(string),
      background-after-a-scenario,
      multiple-backgrounds,
      unterminated-docstring(string),
      unsupported-language(string),
    }

    record problem {
      line: u32,
      column: u32,
      severity: severity,
      kind: problem-kind,
    }

    record step {
      keyword: string,
      text: string,
      line: u32,
      argument: list<string>,
    }

    record example-table {
      name: string,
      header: list<string>,
      rows: list<list<string>>,
    }

    record scenario {
      name: string,
      line: u32,
      tags: list<string>,
      steps: list<step>,
      examples: list<example-table>,
    }

    record document {
      feature: string,
      tags: list<string>,
      background: list<step>,
      scenarios: list<scenario>,
    }

    parse: func(source: string) -> result<document, list<problem>>;

    validate: func(source: string) -> list<problem>;
  }
```

## `git:forge/repo@0.1.0`

```wit
  interface repo {
    variant forge-error {
      rejected(string),
      unavailable(string),
      not-configured(string),
      conflict(string),
    }

    record file-change {
      path: string,
      content: string,
    }

    record proposal {
      branch: string,
      base: string,
      title: string,
      body: string,
      message: string,
      changes: list<file-change>,
    }

    record opened {
      number: u32,
      url: string,
      commit: string,
      branch: string,
    }

    propose: func(p: proposal) -> result<opened, forge-error>;

    base-commit: func(base: string) -> result<string, forge-error>;
  }
```

## `graph:agent/writer@0.2.0`

```wit
  interface writer {
    variant agent-error {
      inference-failed(string),
      under-specified(string),
      unusable-answer(string),
    }

    record file {
      path: string,
      content: string,
    }

    record failure {
      id: string,
      detail: string,
    }

    record blocked {
      id: string,
      needs: string,
    }

    record goal {
      text: string,
      context: list<file>,
      writable: list<string>,
    }

    record candidate {
      files: list<file>,
      prompt-tokens: u32,
      completion-tokens: u32,
      model: string,
    }

    attempt: func(g: goal, previous: list<failure>, blocked: list<blocked>, seed: u64) -> result<candidate, agent-error>;
  }
```

## `graph:fitness/evaluator@0.2.0`

```wit
  interface evaluator {
    variant eval-error {
      unavailable(string),
      invalid(string),
      need-base(string),
    }

    record check {
      id: string,
      required: bool,
      weight: u32,
      command: list<string>,
      needs: list<string>,
    }

    record file {
      path: string,
      content: string,
    }

    record candidate {
      name: string,
      base-commit: string,
      base-tree: list<file>,
      changes: list<file>,
    }

    enum check-state {
      passed,
      failed,
      not-attempted,
    }

    record outcome {
      id: string,
      required: bool,
      weight: u32,
      state: check-state,
      blocked-by: string,
      took-ms: u64,
      detail: string,
    }

    record verdict {
      accepted: bool,
      score: u32,
      outcomes: list<outcome>,
    }

    evaluate: func(c: candidate, checks: list<check>) -> result<verdict, eval-error>;
  }
```

## `graph:run/driver@0.2.0`

```wit
  interface driver {
    record file {
      path: string,
      content: string,
    }

    record goal {
      text: string,
      context: list<file>,
      writable: list<string>,
    }

    record failure {
      id: string,
      detail: string,
    }

    record check {
      id: string,
      required: bool,
      weight: u32,
      command: list<string>,
      needs: list<string>,
    }

    variant run-error {
      provider-down(string),
      gate-unusable(string),
      invalid(string),
    }

    enum stop-reason {
      accepted,
      exhausted,
      plateau,
      no-progress,
      over-budget,
    }

    record attempt {
      seed: u64,
      digest: string,
      score: u32,
      accepted: bool,
      error: string,
      prompt-tokens: u32,
      completion-tokens: u32,
      model: string,
    }

    record plan {
      goal: goal,
      previous: list<failure>,
      checks: list<check>,
      base-commit: string,
      base-tree: list<file>,
      max-attempts: u32,
      max-tokens: u32,
      patience: u32,
      seed: u64,
    }

    record run-result {
      files: list<file>,
      accepted: bool,
      score: u32,
      failures: list<failure>,
      attempts: list<attempt>,
      spent-tokens: u32,
      stopped: stop-reason,
    }

    run: func(p: plan) -> result<run-result, run-error>;
  }
```

## `graph:select/selector@0.2.0`

```wit
  interface selector {
    use graph:agent/writer@0.2.0.{file};
    use git:forge/repo@0.1.0.{opened};

    record entry {
      branch: string,
      accepted: bool,
      score: u32,
      digest: string,
      spent-tokens: u32,
      attempts: u32,
      files: list<file>,
    }

    record chosen {
      index: u32,
      branch: string,
      because: string,
    }

    variant decision {
      winner(chosen),
      nothing-acceptable(string),
    }

    record outcome {
      decision: decision,
      distinct: u32,
      accepted: u32,
      spent-tokens: u32,
    }

    variant select-error {
      invalid(string),
    }

    record landing {
      branch: string,
      base: string,
      title: string,
      body: string,
      message: string,
    }

    variant land-error {
      nothing-acceptable(string),
      forge(string),
      invalid(string),
    }

    select: func(entries: list<entry>) -> result<outcome, select-error>;

    land: func(entries: list<entry>, p: landing) -> result<opened, land-error>;
  }
```

## `i18n:catalog/catalog@0.1.0`

```wit
  interface catalog {
    variant i18n-error {
      missing-message,
      backend-unavailable(string),
    }

    record arg {
      name: string,
      value: string,
    }

    set-message: func(locale: string, key: string, value: string) -> result<_, i18n-error>;

    set-plural: func(locale: string, key: string, forms: list<tuple<string, string>>) -> result<_, i18n-error>;

    translate: func(locale: string, key: string, args: list<arg>) -> result<string, i18n-error>;

    translate-plural: func(locale: string, key: string, count: u64, args: list<arg>) -> result<string, i18n-error>;

    negotiate: func(preferred: list<string>, available: list<string>) -> string;
  }
```

## `iban:validate/validator@0.1.0`

```wit
  interface validator {
    record iban-info {
      country: string,
      check-digits: string,
      bban: string,
      formatted: string,
      length: u32,
    }

    variant iban-error {
      too-short,
      bad-country(string),
      bad-char(string),
      bad-length(tuple<u32, u32>),
      bad-check,
    }

    validate: func(iban: string) -> result<iban-info, iban-error>;
  }
```

## `ical:codec/codec@0.1.0`

```wit
  interface codec {
    record event {
      uid: string,
      start: u64,
      end: u64,
      summary: string,
      description: string,
      location: string,
      organizer: string,
      rrule: string,
      alarm-minutes: u32,
    }

    format-event: func(ev: event, prod-id: string) -> string;

    format-calendar: func(events: list<event>, prod-id: string, cal-name: string) -> string;
  }
```

## `id:generate/generator@0.1.0`

```wit
  interface generator {
    ulid: func() -> string;

    ulid-at: func(unix-millis: u64) -> string;

    uuid-v4: func() -> string;

    nanoid: func(length: u8) -> string;

    short-code: func(length: u8) -> string;
  }
```

## `idempotency:guard/store@0.1.0`

```wit
  interface store {
    variant idem-error {
      in-progress,
      backend-unavailable(string),
    }

    record cached-response {
      status: u16,
      body: list<u8>,
    }

    begin: func(key: string, ttl-seconds: u64) -> result<option<cached-response>, idem-error>;

    complete: func(key: string, status: u16, body: list<u8>) -> result<_, idem-error>;

    forget: func(key: string) -> result<_, idem-error>;
  }
```

## `iot:scanner/scanner@0.1.0`

```wit
  interface scanner {
    enum protocol {
      bluetooth,
      wifi,
      zigbee,
      thread,
      matter,
    }

    record device {
      id: string,
      name: string,
      protocol: protocol,
      rssi: s32,
      connected: bool,
    }

    scan: func() -> list<device>;
  }
```

## `json:patch/patcher@0.1.0`

```wit
  interface patcher {
    variant patch-error {
      invalid-json(string),
      path-not-found(string),
      test-failed(string),
      invalid-patch(string),
    }

    apply-patch: func(document: string, patch: string) -> result<string, patch-error>;

    apply-merge: func(document: string, merge-patch: string) -> result<string, patch-error>;

    diff: func(source: string, target: string) -> result<string, patch-error>;
  }
```

## `knowledge:graph/store@0.1.0`

```wit
  interface store {
    variant graph-error {
      rejected(string),
      unavailable(string),
      not-configured(string),
    }

    record node {
      kind: string,
      id: string,
      properties: string,
    }

    enum direction {
      outgoing,
      incoming,
      both,
    }

    upsert: func(n: node) -> result<_, graph-error>;

    get: func(kind: string, id: string) -> result<option<node>, graph-error>;

    relate: func(from-node: node, edge: string, to-node: node, properties: string) -> result<_, graph-error>;

    neighbours: func(kind: string, id: string, edge: string, dir: direction, limit: u32) -> result<list<node>, graph-error>;

    query: func(surql: string) -> result<string, graph-error>;
  }
```

## `knowledge:memory/memory@0.2.0`

```wit
  interface memory {
    variant memory-error {
      rejected(string),
      unavailable(string),
      refused(string),
    }

    enum namespace {
      patterns,
      solutions,
      errors,
    }

    record entry {
      ns: namespace,
      key: string,
      text: string,
      goal: string,
      env: string,
      attempt: string,
      score: s32,
      tags: list<string>,
    }

    record hit {
      key: string,
      ns: namespace,
      text: string,
      similarity: f64,
      dense: bool,
    }

    record recall-opts {
      k: u32,
      budget: u32,
      pools: list<namespace>,
      min-similarity: f64,
      tags: list<string>,
    }

    record prior-work {
      goal: string,
      similarity: f64,
      score: s32,
      run: string,
      artifact: string,
      evaluations: u32,
    }

    record sub-goal {
      goal: string,
      ordinal: u32,
      why: string,
      done: bool,
    }

    observe: func(e: entry) -> result<string, memory-error>;

    recall: func(goal: string, opts: recall-opts) -> result<list<hit>, memory-error>;

    attribute: func(keys: list<string>, run: string, succeeded: bool) -> result<_, memory-error>;

    evaluated: func(goal: string, run: string, score: s32, passed: bool, artifact: string) -> result<_, memory-error>;

    decomposed-into: func(parent: string, child: string, ordinal: u32, why: string) -> result<_, memory-error>;

    parts-of: func(goal: string) -> result<list<sub-goal>, memory-error>;

    parents-of: func(goal: string) -> result<list<sub-goal>, memory-error>;

    decay: func(max-age-days: u32, min-uses: u64) -> result<u32, memory-error>;

    already-done: func(goal: string, min-similarity: f64) -> result<option<prior-work>, memory-error>;
  }
```

## `knowledge:memory/promotion@0.2.0`

```wit
  interface promotion {
    use memory.{entry, memory-error};

    promote: func(e: entry, gate-score: s32) -> result<string, memory-error>;
  }
```

## `ledger:doubleentry/ledger@0.1.0`

```wit
  interface ledger {
    enum side {
      debit,
      credit,
    }

    record line {
      account: string,
      amount: s64,
      side: side,
    }

    record entry {
      id: string,
      memo: string,
      lines: list<line>,
    }

    variant ledger-error {
      unbalanced(tuple<s64, s64>),
      too-few-lines,
      nonpositive(string),
    }

    record account-balance {
      account: string,
      debits: s64,
      credits: s64,
      net: s64,
    }

    record trial {
      accounts: list<account-balance>,
      total-debits: s64,
      total-credits: s64,
      balanced: bool,
    }

    validate: func(e: entry) -> result<_, ledger-error>;

    trial-balance: func(entries: list<entry>) -> result<trial, ledger-error>;
  }
```

## `llm:inference/inference@0.1.0`

```wit
  interface inference {
    variant infer-error {
      invalid-request(string),
      provider-denied(string),
      provider-unavailable(string),
      bad-response(string),
      no-content,
    }

    enum role {
      system,
      user,
      assistant,
    }

    record message {
      role: role,
      content: string,
    }

    record options {
      model: string,
      temperature: u32,
      max-tokens: u32,
      stop: list<string>,
      seed: u64,
    }

    record usage {
      prompt-tokens: u32,
      completion-tokens: u32,
    }

    record completion {
      text: string,
      finish-reason: string,
      model: string,
      usage: usage,
    }

    chat: func(messages: list<message>, opts: options) -> result<completion, infer-error>;

    complete: func(prompt: string, system: string, opts: options) -> result<completion, infer-error>;

    embed: func(text: string, opts: options) -> result<list<f32>, infer-error>;

    describe: func() -> tuple<string, bool>;
  }
```

## `local:reddit/reddit`

```wit
  interface reddit {
    record subreddit {
      id: string,
      name: string,
    }

    record thread {
      id: string,
      subreddit-id: string,
      title: string,
      content: string,
      upvotes: s32,
    }

    record comment {
      id: string,
      thread-id: string,
      content: string,
      upvotes: s32,
    }

    create-subreddit: func(name: string, token: string) -> string;

    get-subreddits: func() -> list<subreddit>;

    create-thread: func(subreddit-id: string, title: string, content: string, token: string) -> string;

    get-threads: func(subreddit-id: string) -> list<thread>;

    upvote-thread: func(thread-id: string, token: string);

    downvote-thread: func(thread-id: string, token: string);

    create-comment: func(thread-id: string, content: string, token: string) -> string;

    get-comments: func(thread-id: string) -> list<comment>;

    upvote-comment: func(comment-id: string, token: string);

    downvote-comment: func(comment-id: string, token: string);
  }
```

## `lock:mutex/mutex@0.1.0`

```wit
  interface mutex {
    record lease {
      key: string,
      owner: string,
      token: string,
      expires: u64,
      fence: u64,
    }

    variant lock-error {
      held(lease),
      not-holder,
      invalid-ttl,
      backend-unavailable(string),
    }

    acquire: func(key: string, owner: string, ttl-seconds: u64) -> result<lease, lock-error>;

    release: func(key: string, token: string) -> result<_, lock-error>;

    renew: func(token: string, ttl-seconds: u64) -> result<lease, lock-error>;

    holder: func(key: string) -> result<option<lease>, lock-error>;
  }
```

## `login:app/auth@0.1.0`

```wit
  interface auth {
    variant auth-error {
      invalid-credentials,
      no-session,
      capability(string),
    }

    record login-result {
      token: string,
      csrf: string,
      expires: u64,
    }

    record identity {
      user: string,
      expires: u64,
    }

    login: func(user: string, password: string) -> result<login-result, auth-error>;

    whoami: func(token: string) -> result<identity, auth-error>;

    logout: func(token: string) -> result<_, auth-error>;
  }
```

## `mail:send/sender@0.1.0`

```wit
  interface sender {
    variant send-error {
      rejected(string),
      unavailable(string),
      not-configured(string),
    }

    record email {
      to: string,
      subject: string,
      body: string,
    }

    send: func(msg: email) -> result<string, send-error>;
  }
```

## `md:render/renderer@0.1.0`

```wit
  interface renderer {
    record options {
      hard-breaks: bool,
      safe-links: bool,
    }

    to-html: func(markdown: string) -> string;

    to-html-with: func(markdown: string, opts: options) -> string;

    to-text: func(markdown: string) -> string;
  }
```

## `media:image/optimizer@0.1.0`

```wit
  interface optimizer {
    optimize: func(img: string) -> string;
  }
```

## `media:video/ffmpeg@0.1.0`

```wit
  interface ffmpeg {
    transcode: func(input: string) -> string;
  }
```

## `metrics:collect/collector@0.1.0`

```wit
  interface collector {
    variant metrics-error {
      backend-unavailable(string),
    }

    record counter {
      key: string,
      value: u64,
      updated: u64,
    }

    incr: func(key: string, by: u64) -> result<u64, metrics-error>;

    get: func(key: string) -> result<u64, metrics-error>;

    scan: func(prefix: string) -> result<list<counter>, metrics-error>;

    rate: func(num-key: string, denom-key: string) -> result<f64, metrics-error>;

    reset: func(key: string) -> result<_, metrics-error>;
  }
```

## `money:amount/arithmetic@0.1.0`

```wit
  interface arithmetic {
    variant money-error {
      unknown-currency(string),
      currency-mismatch,
      overflow,
      divide-by-zero,
    }

    record amount {
      units: s64,
      currency: string,
    }

    parse: func(decimal: string, currency: string) -> result<amount, money-error>;

    format: func(a: amount) -> result<string, money-error>;

    add: func(a: amount, b: amount) -> result<amount, money-error>;

    subtract: func(a: amount, b: amount) -> result<amount, money-error>;

    scale: func(a: amount, factor: s64) -> result<amount, money-error>;

    allocate: func(total: amount, shares: u32) -> result<list<amount>, money-error>;

    compare: func(a: amount, b: amount) -> result<s8, money-error>;
  }
```

## `net:lan/scanner@0.1.0`

```wit
  interface scanner {
    scan: func() -> string;
  }
```

## `net:mdns/discovery@0.1.0`

```wit
  interface discovery {
    discover: func() -> string;
  }
```

## `net:vpn/wireguard@0.1.0`

```wit
  interface wireguard {
    status: func() -> string;
  }
```

## `notify:dispatch/dispatcher@0.1.0`

```wit
  interface dispatcher {
    variant notify-error {
      unsupported-channel(string),
      delivery-failed(string),
      backend-unavailable(string),
    }

    enum channel {
      webhook,
      email,
      sms,
    }

    record message {
      channel: channel,
      target: string,
      subject: string,
      body: string,
    }

    send: func(msg: message) -> result<u16, notify-error>;
  }
```

## `notify:inbox/inbox@0.1.0`

```wit
  interface inbox {
    variant inbox-error {
      backend-unavailable(string),
      invalid(string),
    }

    record note {
      seq: u64,
      kind: string,
      title: string,
      body: string,
      payload: string,
      at: u64,
      read: bool,
    }

    deliver: func(subject: string, kind: string, title: string, body: string, payload: string) -> result<u64, inbox-error>;

    since: func(subject: string, after: u64, limit: u32) -> result<list<note>, inbox-error>;

    unread-count: func(subject: string) -> result<u64, inbox-error>;

    mark-read: func(subject: string, seqs: list<u64>) -> result<u64, inbox-error>;

    mark-all-read: func(subject: string, through: u64) -> result<u64, inbox-error>;
  }
```

## `notify:prefs/preferences@0.1.0`

```wit
  interface preferences {
    variant prefs-error {
      backend-unavailable(string),
      invalid(string),
    }

    enum channel {
      in-app,
      email,
    }

    record outcome {
      channel: channel,
      ok: bool,
      detail: string,
    }

    record preference {
      subject: string,
      default-channels: list<channel>,
      overrides: list<tuple<string, list<channel>>>,
      email-address: string,
    }

    get: func(subject: string) -> result<preference, prefs-error>;

    put: func(pref: preference) -> result<_, prefs-error>;

    notify: func(subject: string, kind: string, title: string, body: string, payload: string) -> result<list<outcome>, prefs-error>;
  }
```

## `os:container/docker@0.1.0`

```wit
  interface docker {
    ps: func() -> string;
  }
```

## `os:desktop/clipboard@0.1.0`

```wit
  interface clipboard {
    read: func() -> string;
  }
```

## `os:fs/watcher@0.1.0`

```wit
  interface watcher {
    enum change {
      created,
      modified,
      removed,
    }

    record event {
      path: string,
      kind: change,
      at: u64,
    }

    record changes {
      events: list<event>,
      cursor: string,
      truncated: bool,
    }

    variant watch-error {
      not-permitted(string),
      no-such-directory(string),
      unavailable(string),
    }

    poll: func(dir: string, cursor: string) -> result<changes, watch-error>;
  }
```

## `os:system/cron@0.1.0`

```wit
  interface cron {
    list-jobs: func() -> string;
  }
```

## `os:ui/notifications@0.1.0`

```wit
  interface notifications {
    notify: func(msg: string) -> string;
  }
```

## `otp:totp/authenticator@0.1.0`

```wit
  interface authenticator {
    variant otp-error {
      bad-secret,
      bad-digits,
    }

    record provisioned {
      secret: string,
      uri: string,
    }

    provision: func(issuer: string, account: string) -> result<provisioned, otp-error>;

    totp-at: func(secret: string, timestamp: u64, period: u32, digits: u8) -> result<string, otp-error>;

    totp-now: func(secret: string) -> result<string, otp-error>;

    verify: func(secret: string, code: string, period: u32, digits: u8, skew: u32) -> result<bool, otp-error>;

    hotp-at: func(secret: string, counter: u64, digits: u8) -> result<string, otp-error>;

    recovery-codes: func(count: u32) -> list<string>;
  }
```

## `outbox:dispatch/queue@0.1.0`

```wit
  interface queue {
    variant outbox-error {
      not-found,
      backend-unavailable(string),
    }

    enum state {
      pending,
      in-flight,
      dead,
    }

    record event {
      id: string,
      topic: string,
      payload: list<u8>,
      state: state,
      attempts: u32,
      created: u64,
      not-before: u64,
    }

    enqueue: func(topic: string, payload: list<u8>, delay-seconds: u64) -> result<string, outbox-error>;

    claim: func(max: u32, lease-seconds: u64) -> result<list<event>, outbox-error>;

    ack: func(id: string) -> result<_, outbox-error>;

    fail: func(id: string) -> result<state, outbox-error>;

    dead-letters: func(max: u32) -> result<list<event>, outbox-error>;

    replay: func(id: string) -> result<_, outbox-error>;
  }
```

## `paginate:cursor/cursors@0.1.0`

```wit
  interface cursors {
    variant cursor-error {
      invalid-cursor,
      bad-limit,
    }

    record position {
      sort-key: string,
      last-id: string,
      forward: bool,
    }

    record page-info {
      next-cursor: option<string>,
      prev-cursor: option<string>,
      has-next: bool,
      has-prev: bool,
    }

    encode: func(pos: position) -> string;

    decode: func(cursor: string) -> result<position, cursor-error>;

    clamp-limit: func(requested: u32) -> result<u32, cursor-error>;

    build-page: func(first: option<position>, last: option<position>, more-before: bool, more-after: bool) -> page-info;
  }
```

## `pdf:codec/codec@0.1.0`

```wit
  interface codec {
    record block {
      text: string,
      size: u32,
      bold: bool,
      gap-before: u32,
    }

    record document {
      title: string,
      blocks: list<block>,
    }

    render: func(doc: document) -> list<u8>;
  }
```

## `pii:redact/redactor@0.1.0`

```wit
  interface redactor {
    enum kind {
      email,
      credit-card,
      ssn,
      phone,
      ip,
    }

    record finding {
      kind: kind,
      start: u32,
      length: u32,
    }

    record options {
      kinds: list<kind>,
    }

    detect: func(text: string, opts: options) -> list<finding>;

    redact: func(text: string, opts: options) -> string;

    mask: func(text: string, opts: options) -> string;
  }
```

## `policy:guard/guard@0.1.0`

```wit
  interface guard {
    variant policy-error {
      invalid-rule(string),
      backend-unavailable(string),
    }

    enum op {
      eq,
      ne,
      in-list,
      lt,
      gt,
      has,
    }

    record condition {
      left: string,
      op: op,
      right: string,
    }

    enum effect {
      allow,
      deny,
    }

    record rule {
      id: string,
      action: string,
      effect: effect,
      conditions: list<condition>,
      priority: u32,
    }

    record attr {
      key: string,
      value: string,
    }

    record decision {
      allowed: bool,
      rule-id: string,
      reason: string,
    }

    set-rules: func(domain: string, rules: list<rule>) -> result<_, policy-error>;

    get-rules: func(domain: string) -> result<list<rule>, policy-error>;

    can: func(domain: string, action: string, principal: list<attr>, target-attrs: list<attr>) -> result<decision, policy-error>;

    enforce: func(domain: string, action: string, principal: list<attr>, target-attrs: list<attr>) -> bool;
  }
```

## `portfolio:value/valuation@0.1.0`

```wit
  interface valuation {
    enum event-kind {
      acquired,
      disposed,
    }

    record event {
      item-id: string,
      kind: event-kind,
      quantity: u32,
      unit-minor: s64,
      currency: string,
      at: u64,
    }

    record quote {
      item-id: string,
      unit-minor: s64,
      currency: string,
      at: u64,
    }

    record valuation {
      cost-basis-minor: s64,
      market-value-minor: s64,
      unrealised-minor: s64,
      realised-minor: s64,
      currency: string,
      unquoted: u32,
    }

    record point {
      at: u64,
      market-value-minor: s64,
      cost-basis-minor: s64,
      realised-minor: s64,
      unquoted: u32,
    }

    variant value-error {
      mixed-currency(tuple<string, string>),
      oversold-at(tuple<string, u64, u32, u32>),
      zero-quantity(tuple<string, u64>),
      zero-step,
      empty,
    }

    value-at: func(events: list<event>, quotes: list<quote>, at: u64) -> result<valuation, value-error>;

    series: func(events: list<event>, quotes: list<quote>, since: u64, until: u64, step: u64) -> result<list<point>, value-error>;
  }
```

## `price:history/history@0.1.0`

```wit
  interface history {
    enum quote-kind {
      market,
      low,
      high,
      last-sold,
    }

    record quote {
      unit-minor: s64,
      currency: string,
      kind: quote-kind,
      source: string,
      at: u64,
    }

    record observed {
      unit-minor: s64,
      currency: string,
      source: string,
      observed-at: u64,
      age-seconds: u64,
      carried: bool,
    }

    record point {
      at: u64,
      unit-minor: s64,
      carried: bool,
    }

    variant price-error {
      not-yet-priced,
      mixed-currency(tuple<string, string>),
      zero-step,
    }

    at: func(quotes: list<quote>, kind: quote-kind, at: u64) -> result<observed, price-error>;

    series: func(quotes: list<quote>, kind: quote-kind, since: u64, until: u64, step: u64) -> result<list<point>, price-error>;
  }
```

## `proxy:route/router@0.1.0`

```wit
  interface router {
    record upstream-response {
      status: u16,
      content-type: string,
      body: list<u8>,
    }

    variant proxy-error {
      no-route,
      upstream-unreachable(string),
    }

    resolve: func(path: string) -> option<string>;

    forward: func(method: string, path-with-query: string, headers: list<tuple<string, string>>, body: list<u8>) -> result<upstream-response, proxy-error>;
  }
```

## `qr:encode/encoder@0.1.0`

```wit
  interface encoder {
    enum ecc {
      low,
      medium,
      quartile,
      high,
    }

    variant qr-error {
      too-long(string),
    }

    svg: func(data: string, level: ecc, quiet-zone: u32) -> result<string, qr-error>;

    unicode: func(data: string, level: ecc) -> result<string, qr-error>;

    matrix: func(data: string, level: ecc) -> result<string, qr-error>;
  }
```

## `quiz:grade/grader@0.1.0`

```wit
  interface grader {
    record grade-result {
      correct: u32,
      total: u32,
      score-pct: u32,
      passed: bool,
    }

    record stats {
      count: u32,
      mean: u32,
      median: u32,
      min: u32,
      max: u32,
      pass-count: u32,
      buckets: list<u32>,
    }

    grade: func(answers: list<u32>, key: list<u32>, pass-mark: u32) -> grade-result;

    distribution: func(scores: list<u32>, pass-mark: u32) -> stats;
  }
```

## `quota:meter/meter@0.1.0`

```wit
  interface meter {
    variant quota-error {
      exceeded(u64),
      backend-unavailable(string),
    }

    record balance {
      used: u64,
      limit: u64,
      remaining: u64,
      resets-at: u64,
    }

    reserve: func(subject: string, amount: u64, limit: u64, period-seconds: u64) -> result<balance, quota-error>;

    record-usage: func(subject: string, amount: u64, limit: u64, period-seconds: u64) -> result<balance, quota-error>;

    peek: func(subject: string, limit: u64, period-seconds: u64) -> result<balance, quota-error>;

    reset: func(subject: string) -> result<_, quota-error>;
  }
```

## `ratelimit:guard/limiter@0.1.0`

```wit
  interface limiter {
    variant limit-error {
      locked(u32),
      backend-unavailable(string),
    }

    check: func(key: string) -> result<u32, limit-error>;

    record-failure: func(key: string) -> result<_, limit-error>;

    reset: func(key: string) -> result<_, limit-error>;
  }
```

## `records:store/store@0.1.0`

```wit
  interface store {
    variant store-error {
      not-found,
      invalid-json(string),
      revision-conflict(u64),
      backend-unavailable(string),
    }

    record entry {
      id: string,
      data: string,
      revision: u64,
      created: u64,
      updated: u64,
    }

    record filter {
      field: string,
      value: string,
    }

    record page {
      entries: list<entry>,
      next: string,
    }

    record repair-report {
      readded: u64,
      pruned: u64,
      total: u64,
      indexes: u64,
      indexes-dropped: u64,
    }

    create: func(collection: string, data: string, index-fields: list<string>) -> result<entry, store-error>;

    get: func(collection: string, id: string) -> result<entry, store-error>;

    update: func(collection: string, id: string, data: string, expected-revision: u64) -> result<entry, store-error>;

    delete: func(collection: string, id: string) -> result<_, store-error>;

    list-records: func(collection: string, limit: u32, after: string) -> result<page, store-error>;

    find-by: func(collection: string, field: string, value: string) -> result<list<entry>, store-error>;

    query: func(collection: string, filters: list<filter>, limit: u32) -> result<list<entry>, store-error>;

    count: func(collection: string) -> result<u64, store-error>;

    repair: func(collection: string) -> result<repair-report, store-error>;

    verify: func(collection: string) -> result<repair-report, store-error>;
  }
```

## `resilience:breaker/breaker@0.1.0`

```wit
  interface breaker {
    enum circuit-state {
      closed,
      open,
      half-open,
    }

    record policy {
      failure-threshold: u32,
      window-ms: u64,
      open-ms: u64,
      half-open-probes: u32,
      success-threshold: u32,
    }

    record circuit {
      state: circuit-state,
      failures: u32,
      successes: u32,
      window-start-ms: u64,
      changed-ms: u64,
      probes: u32,
    }

    record admission {
      admit: bool,
      state: circuit-state,
      retry-after-ms: u64,
    }

    record retry-policy {
      max-attempts: u32,
      base-ms: u32,
      factor-pct: u32,
      max-ms: u32,
      jitter: bool,
    }

    admit: func(state: circuit, now-ms: u64, pol: policy) -> tuple<admission, circuit>;

    observe: func(state: circuit, now-ms: u64, pol: policy, ok: bool) -> circuit;

    backoff: func(attempt: u32, pol: retry-policy, seed: u64) -> option<u32>;
  }
```

## `rrule:recur/recur@0.1.0`

```wit
  interface recur {
    enum freq {
      daily,
      weekly,
    }

    record rule {
      frequency: freq,
      interval: u32,
      by-weekday: list<u8>,
      count: u32,
      until: string,
    }

    variant recur-error {
      bad-date(string),
      unsupported(string),
    }

    expand: func(dtstart: string, r: rule, window-from: string, window-to: string) -> result<list<string>, recur-error>;
  }
```

## `sched:timer/timer@0.1.0`

```wit
  interface timer {
    variant timer-error {
      not-found,
      invalid-period,
      backend-unavailable(string),
    }

    enum kind {
      once,
      every,
    }

    record job {
      key: string,
      payload: list<u8>,
      kind: kind,
      run-at: u64,
      period-seconds: u64,
      fires: u32,
    }

    schedule-at: func(key: string, run-at: u64, payload: list<u8>) -> result<_, timer-error>;

    schedule-every: func(key: string, period-seconds: u64, first-run-at: u64, payload: list<u8>) -> result<_, timer-error>;

    due: func(now: u64, max: u32, lease-seconds: u64) -> result<list<job>, timer-error>;

    ack: func(key: string) -> result<_, timer-error>;

    cancel: func(key: string) -> result<_, timer-error>;

    peek: func(key: string) -> result<option<job>, timer-error>;

    list-jobs: func(max: u32) -> result<list<job>, timer-error>;
  }
```

## `search:index/index@0.1.0`

```wit
  interface index {
    variant search-error {
      not-found,
      backend-unavailable(string),
    }

    enum mode {
      any,
      all,
    }

    record hit {
      id: string,
      score: f64,
    }

    index-doc: func(id: string, text: string, tags: list<string>) -> result<_, search-error>;

    remove: func(id: string) -> result<_, search-error>;

    query: func(query: string, mode: mode, tags: list<string>, limit: u32) -> result<list<hit>, search-error>;

    doc-count: func() -> result<u64, search-error>;
  }
```

## `secrets:vault/vault@0.1.0`

```wit
  interface vault {
    variant vault-error {
      not-found,
      crypto(string),
      backend-unavailable(string),
    }

    record secret-meta {
      name: string,
      version: u32,
      updated: u64,
    }

    put: func(name: string, value: list<u8>) -> result<secret-meta, vault-error>;

    get: func(name: string) -> result<list<u8>, vault-error>;

    get-version: func(name: string, version: u32) -> result<list<u8>, vault-error>;

    describe: func(name: string) -> result<secret-meta, vault-error>;

    rotate: func(name: string, new-value: list<u8>) -> result<tuple<u32, u32>, vault-error>;

    delete: func(name: string) -> result<_, vault-error>;

    list-names: func(max: u32) -> result<list<string>, vault-error>;
  }
```

## `session:store/store@0.1.0`

```wit
  interface store {
    variant session-error {
      not-found,
      csrf-mismatch,
      backend-unavailable(string),
    }

    record session {
      id: string,
      data: list<u8>,
      created: u64,
      expires: u64,
      csrf-token: string,
    }

    create: func(data: list<u8>, ttl-seconds: u64) -> result<session, session-error>;

    get: func(id: string) -> result<session, session-error>;

    update-data: func(id: string, data: list<u8>) -> result<_, session-error>;

    refresh: func(id: string, ttl-seconds: u64) -> result<session, session-error>;

    verify-csrf: func(id: string, token: string) -> result<_, session-error>;

    revoke: func(id: string) -> result<_, session-error>;
  }
```

## `shaper:limit/limiter@0.1.0`

```wit
  interface limiter {
    record decision {
      allowed: bool,
      retry-after-ms: u64,
      remaining: f64,
    }

    record bucket {
      tokens: f64,
      updated-ms: u64,
    }

    token-bucket: func(state: bucket, now-ms: u64, capacity: f64, refill-per-sec: f64, cost: f64) -> tuple<decision, bucket>;

    gcra: func(tat-ms: u64, now-ms: u64, period-ms: u64, burst: u32, cost: u32) -> tuple<decision, u64>;
  }
```

## `sheet:ingest/reader@0.1.0`

```wit
  interface reader {
    record row {
      cells: list<string>,
    }

    record sheet {
      header: list<string>,
      rows: list<row>,
      sheet-name: string,
    }

    variant import-error {
      unknown-format(string),
      empty,
      no-header,
      duplicate-column(string),
      too-many-cells(tuple<u32, u32, u32>),
      archive(string),
      no-sheet,
      csv(string),
    }

    read: func(name: string, bytes: list<u8>) -> result<sheet, import-error>;
  }
```

## `slug:generate/generator@0.1.0`

```wit
  interface generator {
    record options {
      separator: string,
      max-length: u32,
    }

    slugify: func(text: string) -> string;

    slugify-with: func(text: string, opts: options) -> string;

    uniquify: func(desired: string, taken: list<string>) -> string;
  }
```

## `svg:chart/charts@0.1.0`

```wit
  interface charts {
    enum kind {
      bar,
      line,
      donut,
      sparkline,
    }

    record slice {
      label: string,
      value: f64,
      color: string,
    }

    record chart {
      kind: kind,
      title: string,
      data: list<slice>,
      width: u32,
      height: u32,
    }

    render: func(c: chart) -> string;
  }
```

## `ui:assets/files@0.1.0`

```wit
  interface files {
    record asset {
      content-type: string,
      body: list<u8>,
    }

    get: func(path: string) -> option<asset>;
  }
```

## `upload:policy/gate@0.1.0`

```wit
  interface gate {
    variant policy-error {
      type-not-allowed(string),
      too-large(u64),
      invalid-ticket,
      backend-unavailable(string),
    }

    record ticket {
      token: string,
      object-key: string,
      expires: u64,
    }

    record grant {
      object-key: string,
      content-type: string,
      max-size: u64,
    }

    check: func(content-type: string, size: u64) -> result<_, policy-error>;

    authorize: func(tenant: string, content-type: string, size: u64, ttl-seconds: u64) -> result<ticket, policy-error>;

    redeem: func(token: string) -> result<grant, policy-error>;
  }
```

## `validate:schema/validator@0.1.0`

```wit
  interface validator {
    enum kind {
      text,
      integer,
      number,
      boolean,
      email,
      alphanumeric,
      uuid,
    }

    record rule {
      field: string,
      kind: kind,
      required: bool,
      min-len: u32,
      max-len: u32,
      min-value: option<f64>,
      max-value: option<f64>,
      one-of: list<string>,
    }

    record field-error {
      field: string,
      code: string,
      message: string,
    }

    validate: func(json: string, rules: list<rule>) -> list<field-error>;
  }
```

## `vgit:store/objects@0.1.0`

```wit
  interface objects {
    variant git-error {
      not-found(string),
      corrupt(string),
      unavailable(string),
      invalid(string),
    }

    record tree-entry {
      mode: string,
      name: string,
      id: string,
    }

    record commit-info {
      tree: string,
      parents: list<string>,
      author: string,
      when: u64,
      message: string,
    }

    write-blob: func(content: list<u8>) -> result<string, git-error>;

    read-blob: func(id: string) -> result<list<u8>, git-error>;

    write-tree: func(entries: list<tree-entry>) -> result<string, git-error>;

    read-tree: func(id: string) -> result<list<tree-entry>, git-error>;

    write-commit: func(info: commit-info) -> result<string, git-error>;

    read-commit: func(id: string) -> result<commit-info, git-error>;

    has: func(id: string) -> result<bool, git-error>;
  }
```

## `vgit:store/refs@0.1.0`

```wit
  interface refs {
    use objects.{git-error};

    read: func(name: string) -> result<option<string>, git-error>;

    update: func(name: string, expect: option<string>, to: string) -> result<bool, git-error>;

    list-refs: func(prefix: string) -> result<list<tuple<string, string>>, git-error>;

    delete: func(name: string) -> result<_, git-error>;
  }
```

## `vgit:store/worktree@0.1.0`

```wit
  interface worktree {
    use objects.{git-error, commit-info};

    record path-change {
      path: string,
      content: list<u8>,
      mode: string,
      remove: bool,
    }

    record changed {
      path: string,
      kind: string,
    }

    read-path: func(commit: string, path: string) -> result<option<list<u8>>, git-error>;

    list-paths: func(commit: string, prefix: string) -> result<list<string>, git-error>;

    commit-changes: func(base: string, changes: list<path-change>, author: string, when: u64, message: string) -> result<string, git-error>;

    diff: func(before: string, after: string) -> result<list<changed>, git-error>;
  }
```

## `vision:describe/describer@0.1.0`

```wit
  interface describer {
    variant describe-error {
      invalid-request(string),
      provider-denied(string),
      provider-unavailable(string),
      bad-response(string),
      no-content,
    }

    describe: func(image: list<u8>, media-type: string, prompt: string) -> result<string, describe-error>;
  }
```

## `wasmcloud:messaging/handler@0.2.0`

```wit
  interface handler {
    use types.{broker-message};

    handle-message: func(msg: broker-message) -> result<_, string>;
  }
```

## `web:browser/automation@0.1.0`

```wit
  interface automation {
    snapshot: func(url: string) -> string;
  }
```

## `webauthn:verify/verifier@0.1.0`

```wit
  interface verifier {
    record expectations {
      rp-id: string,
      origin: string,
      challenge: string,
      require-user-verification: bool,
    }

    record credential {
      id: string,
      public-key: list<u8>,
      alg: s32,
      sign-count: u32,
      aaguid: string,
      user-verified: bool,
      backup-eligible: bool,
      backed-up: bool,
      attestation-format: string,
    }

    record assertion {
      sign-count: u32,
      user-verified: bool,
      backed-up: bool,
    }

    variant verify-error {
      bad-encoding(string),
      bad-type(string),
      challenge-mismatch,
      origin-mismatch(string),
      rp-id-mismatch,
      user-not-present,
      user-not-verified,
      unsupported-algorithm(s32),
      bad-signature,
      counter-regressed(u32),
      malformed(string),
    }

    register: func(exp: expectations, client-data-json: list<u8>, attestation-object: list<u8>) -> result<credential, verify-error>;

    authenticate: func(exp: expectations, cred: credential, client-data-json: list<u8>, authenticator-data: list<u8>, signature: list<u8>) -> result<assertion, verify-error>;
  }
```

## `webhook:ingest/verifier@0.1.0`

```wit
  interface verifier {
    variant ingest-error {
      bad-signature,
      backend-unavailable(string),
    }

    record verdict {
      accepted: bool,
      replay: bool,
    }

    ingest: func(payload: list<u8>, signature-hex: string, secret-ref: string, delivery-id: string) -> result<verdict, ingest-error>;
  }
```

## `webhook:sign/signer@0.1.0`

```wit
  interface signer {
    variant sign-error {
      malformed-signature,
      signature-mismatch,
      timestamp-out-of-tolerance,
    }

    enum scheme {
      stripe,
      github,
    }

    record signature {
      header: string,
      timestamp: u64,
    }

    sign: func(body: list<u8>, secret: string, scheme: scheme) -> result<signature, sign-error>;

    sign-at: func(body: list<u8>, secret: string, scheme: scheme, timestamp: u64) -> result<signature, sign-error>;

    verify: func(body: list<u8>, header: string, secret: string, scheme: scheme, tolerance-seconds: u64) -> result<_, sign-error>;
  }
```

## `wit:reflect/composer@0.1.0`

```wit
  interface composer {
    use inspector.{surface, iface-ref, reflect-error};

    record node {
      id: string,
      surface: surface,
    }

    record edge {
      plug: string,
      socket: string,
      iface: string,
    }

    record plug-step {
      order: u32,
      socket: string,
      plugs: list<string>,
      output: string,
      also-satisfies: list<string>,
    }

    record gap {
      node: string,
      iface: iface-ref,
    }

    record problem {
      kind: string,
      detail: string,
    }

    record composition-plan {
      steps: list<plug-step>,
      unsatisfied: list<gap>,
      host-needs: list<iface-ref>,
      cyclic: bool,
      instance-count: u32,
      over-instance-limit: bool,
      depth: list<tuple<string, u32>>,
      roots: list<string>,
      problems: list<problem>,
    }

    record part {
      id: string,
      bytes: list<u8>,
    }

    variant compose-error {
      missing-part(string),
      unbuildable(string),
      plug-failed(string),
      encode-failed(string),
    }

    record workload-meta {
      name: string,
      namespace: string,
      registry: string,
      tag: string,
      replicas: u32,
      pool-size: u32,
      max-invocations: u32,
      http-host: string,
    }

    satisfies: func(socket: list<u8>, plug: list<u8>) -> result<list<string>, reflect-error>;

    plan: func(nodes: list<node>, edges: list<edge>) -> composition-plan;

    compose: func(parts: list<part>, edges: list<edge>, root: string) -> result<list<u8>, compose-error>;

    emit-plug-script: func(p: composition-plan, out-dir: string) -> string;

    emit-wac: func(nodes: list<node>, edges: list<edge>, p: composition-plan, package-name: string) -> string;

    emit-workload: func(nodes: list<node>, p: composition-plan, meta: workload-meta) -> string;
  }
```

## `wit:reflect/inspector@0.1.0`

```wit
  interface inspector {
    record iface-ref {
      raw: string,
      namespace: string,
      pkg: string,
      name: string,
      version: string,
    }

    record surface {
      name: string,
      exports: list<iface-ref>,
      imports: list<iface-ref>,
      host-imports: list<iface-ref>,
      size-bytes: u64,
      sha256: string,
      nested-instances: u32,
    }

    variant reflect-error {
      not-a-component(string),
      bad-wasm(string),
    }

    inspect: func(bytes: list<u8>) -> result<surface, reflect-error>;
  }
```

## `zip:archive/archiver@0.1.0`

```wit
  interface archiver {
    record file {
      name: string,
      data: list<u8>,
    }

    variant zip-error {
      not-a-zip,
      truncated(u32),
      unsupported-method(u32),
      bad-checksum(string),
      bad-deflate(string),
    }

    archive: func(files: list<file>) -> list<u8>;

    extract: func(bytes: list<u8>) -> result<list<file>, zip-error>;
  }
```

