/* The map: every system, what it is made of, and everything it says to another.
 *
 * This is the one file a person edits. `build.mjs` turns it into docs/ARCHITECTURE.md (Mermaid
 * diagrams and tables) and docs/architecture/index.html (the interactive page), and refuses a map
 * that names a path or an ADR that does not exist, so it cannot quietly drift from the tree.
 *
 *   node docs/architecture/build.mjs           write both
 *   node docs/architecture/build.mjs --check   fail if either is stale, or the map names something missing
 *
 * It holds no counts. How many components or interfaces there are is a number a test cannot keep
 * honest; `comp-capgraph` derives it from the built artifacts, which is why the README does not
 * copy it either. Say what a thing is, not how many there are.
 *
 * Two kinds of line on the diagrams: a `DIA[system].edges` entry is inside the system, and a
 * `DIA[system].x` entry is a correspondence with the outside, numbered on the drawing and listed
 * from COR under it.
 */

/** When the map was last read against the repository: the newest ADR it knows about. */
export const META = { revisedAfter: '0114' };

/* ============ DATA ============
   Everything on the page is drawn from this block. Systems, parts and correspondences
   were read from the repository (README, docs/CURRENT.md, the ADR index and the source
   headers of each binary), not invented; each part names the path it stands for. */

export const KINDS = [
  ['wasm','Wasm component, a sandboxed guest'],
  ['native','Native process'],
  ['store','Store or bus'],
  ['actor','Client or person'],
  ['spec','Written spec, contract or check'],
  ['ext','Outside this repository']
];

export const PROTO = {
  http: {name:'HTTP / gRPC', dash:''},
  nats: {name:'NATS', dash:'7 4'},
  art:  {name:'Artifact flow', dash:'9 3 2 3'},
  call: {name:'Call or file', dash:''}
};

/* Substrates: shared state the systems sit on. `uses` chips on each card point here. */
export const SUB = [
  {id:'nats',   t:'NATS JetStream', s:['inventory · commands · wRPC','KV · tickets · oplog · queues'], chip:'NATS'},
  {id:'surreal',t:'SurrealDB',      s:['knowledge graph · run traces','code-store graph · kv backend'], chip:'SURREAL'},
  {id:'s3',     t:'S3 / R2',        s:['object bytes · backups','presigned photo uploads'], chip:'S3'},
  {id:'oci',    t:'OCI registry',   s:['components and composed apps','pulled by digest (GHCR)'], chip:'OCI'},
  {id:'git',    t:'Git (GitHub)',   s:['source · PRs · goals as prose','apps.lock on the deploy branch'], chip:'GIT'},
  {id:'llm',    t:'LLM providers',  s:['Anthropic · OpenAI-compatible','local mlx_lm · Ollama'], chip:'LLM'},
  {id:'disk',   t:'Local disk',     s:['SQLite kv · state dirs','event logs · checkpoints'], chip:'DISK'}
];

export const ACTORS = [
  {id:'devs',    t:'Developers & operators', s:'holon CLI · cargo xtask · git', touch:['library','delivery','control','loop']},
  {id:'users',   t:'End users & callers',    s:'browsers and HTTP clients, via the ingress', touch:['control','runtime']},
  {id:'clients', t:'Agent clients',          s:'bots · scripts · UIs over REST, SSE, gRPC', touch:['agents']},
  {id:'element', t:'Element (Matrix)',       s:'talk to agents, vote on approvals', touch:['agents']}
];

/* The systems. `parts` become the chips on the overview card. */
export const SYS = [
 {id:'library', name:'Library & Build', short:'components, contracts, composition',
  purpose:'Wasm components and the WIT contracts between them, composed into one artifact per app.',
  blurb:'A library of capability components, each a WIT contract plus a reference implementation, built to wasm32-wasip2. An app is a root component plus whatever comp-plug finds behind its imports: nothing is wired by hand, and the graph is read off the built binaries so it cannot drift.',
  entry:'cargo xtask build · compose · host · test, and the CI workflows',
  parts:[['spec','WIT'],['wasm','components'],['native','comp-plug'],['native','comp-capgraph'],['native','comp-oci']],
  uses:['oci','surreal','git'],
  boundary:['Nothing here serves traffic: this is what is built and checked, then handed on as a digest.',
            'A component’s real imports are in its binary, so the capability graph is derived from the artifacts rather than kept in a list (ADR-0097).',
            'Adding a function to a WIT interface is compatible; adding a variant case or a record field is not, and witsurface.rs fails when a shape moves without its version.'],
  code:['wit/','components/','xtask/','reconciler/fleet/src/plug.rs','reconciler/src/bin/{plug,oci,capgraph,catalog}.rs']},

 {id:'delivery', name:'Delivery', short:'one spec, four lanes',
  purpose:'One app spec rendered for a box, a lattice, wasmCloud 1.x or 2.x. A pull agent keeps boxes current.',
  blurb:'One hand-written apps/<name>.toml is rendered for four lanes: a single box, a lattice, wasmCloud 1.x or wasmCloud 2.x. Moving between them is an edit and a different renderer, never a rewrite. CI publishes composed artifacts by digest; an agent on each box pulls them.',
  entry:'holon node render · fleet render · wadm render, then scp and systemctl; publish-apps.yml on every merge to main',
  parts:[['spec','apps/*.toml'],['native','holon render'],['native','comp-agent'],['spec','publish-apps.yml']],
  uses:['oci','git'],
  boundary:['Pull, not push: comp-agent has no inbound port and the box holds no CI credential, so nothing reaches in from outside.',
            'The one mutable pointer is which commit of apps.lock is current, a branch in git whose history is the deploy history. Nothing references a tag (ADR-0006).',
            'A generated file never makes an app public: access = "tailnet" is the default and is left alone on purpose.',
            'comp-agent only updates apps the box already has a unit for; adding an app stays a deliberate operator step.'],
  code:['apps/*.toml','cli/src/{selfhost,fleet,wadm}.rs','reconciler/src/bin/agent.rs','.github/workflows/publish-apps.yml','docs/SELFHOST.md']},

 {id:'control', name:'Control plane & lattice', short:'decide, reconcile, route',
  purpose:'platform-domain decides, comp-reconciler makes the fleet match, comp-ingress is the door.',
  blurb:'platform-domain, itself a wasm app, keeps orgs, the catalogue, deployments and secrets. comp-reconciler diffs desired against observed and sends commands over NATS; comp-ingress routes HTTP by Host header from inventory. Bare metal joined by NATS, with no Kubernetes on the runtime path.',
  entry:'the holon CLI and the Holon console (both over /api), and comp-ingress for end users',
  parts:[['wasm','platform-domain'],['native','comp-reconciler'],['native','comp-ingress'],['native','comp-relay'],['wasm','console-domain'],['native','comp-backup']],
  uses:['nats','disk'],
  boundary:['comp-reconciler is the only process that can start code on every node. It holds no business logic, no database and no user concept, so it stays small enough to audit in one sitting.',
            'The ingress needs no platform credential: its routing table comes from inventory, so it keeps routing while the control plane is down.',
            'Admission fails closed: 429 above max-placement-lag, 503 when the reconciler’s report goes stale.',
            'Only one reconciler acts at a time, decided by a lease in the store; a standby takes over within the TTL plus one interval.'],
  code:['components/platform-domain','components/console-domain','reconciler/src/{main,plan}.rs','reconciler/src/bin/{ingress,relay}.rs','reconciler/daemons/backup/','lattice/src/']},

 {id:'runtime', name:'Runtime', short:'comp-host, one process per node',
  purpose:'comp-host: one process per node runs every tenant’s components over wasmtime.',
  blurb:'comp-host is a native server that runs any composed component over wasmtime 45, serving WASI p2 and p3. Without --lattice-nats it serves one component; with it, instances arrive as start commands and the process holds every tenant on the node at once.',
  entry:'HTTP from comp-ingress or a unit’s own port; start and stop commands from the reconciler',
  parts:[['native','comp-host'],['native','wasmtime 45'],['wasm','tenant apps']],
  uses:['nats','surreal','s3','disk'],
  boundary:['Isolation is a linker boundary, not a process boundary: between two tenants there is no OS process, only wasmtime’s sandbox and the newtypes in tenant.rs (ADR-0023).',
            'A name is a real boundary only if host-side state the guest cannot write chose it, and the guest has no second path into the namespace.',
            'Guests get config from wasi:config per instance, never from the process environment, which in a shared host would be a cross-tenant read.',
            'A guest names a secret key and never a reference; the host reveals the value on a single-use fetch (ADR-0051, 0071).'],
  code:['host/src/main.rs','host/src/{tenant,kv,secrets,rpc,agent}.rs','host/wit/host.wit']},

 {id:'daemons', name:'Capability daemons', short:'what a guest cannot do',
  purpose:'What a guest cannot do, done by small native daemons, each with its own allow-list.',
  blurb:'Watching a directory, scanning a LAN, running ffmpeg or a gate: a wasm32-wasip2 component has no watch syscall, no raw socket, no process. Each of these is a small native daemon, one per capability, which the component reaches over loopback HTTP; its address comes from wasi:config.',
  entry:'loopback HTTP from a component through wasi:http/outgoing-handler',
  parts:[['native','comp-fswatch'],['native','comp-docker'],['native','comp-ffmpeg'],['native','comp-media'],['native','comp-checks'],['spec','+ more']],
  uses:['s3','nats'],
  boundary:['Each daemon is its own process with its own allow-list and its own blast radius: no shared daemon, no shared allow-list (ADR-0095).',
            'They bind loopback. comp-checks bounds the command with --allow but not the tree it runs over, so off loopback it refuses to start without a token.',
            'A missing dependency (Docker, Ollama, Chrome, ffmpeg, a desktop session) is reported as unavailable, never answered with a plausible constant.',
            'The twelve ADR-0095 daemons are each their own Cargo package, with one heavy crate and the shared comp-daemon-kit, so building or shipping one compiles none of the others (ADR-0115).'],
  code:['reconciler/daemons/{kit,fswatch,browser,docker,clipboard}/','reconciler/daemons/{imageopt,lanscan,llmlocal,mdns}/','reconciler/daemons/{cron,uinotify,ffmpeg,wireguard,media}/','reconciler/src/bin/{mailrelay,checks}.rs']},

 {id:'durable', name:'Durable services', short:'park and vcs on JetStream',
  purpose:'comp-park and comp-vcs: a ticket per outstanding call, and an agent-native code store.',
  blurb:'Two services on NATS JetStream that other components call over HTTP. comp-park records an outstanding call so no thread has to wait for its answer, and comp-resume delivers what came back. comp-vcs stores code as an oplog of commutative, symbol-level patches.',
  entry:'park-gateway and vcs-gateway, tailnet-only by default; comp-agentd calls comp-park directly',
  parts:[['native','comp-park'],['native','comp-resume'],['native','comp-vcs'],['wasm','park-store'],['wasm','vcs-store']],
  uses:['nats','surreal'],
  boundary:['Both gateways are tailnet-only on purpose: whoever reaches the park gateway can park, wake or cancel any session’s ticket, and whoever reaches vcs can edit every workspace.',
            'The gateways keep nothing; every byte of state is in the daemon (kv = "memory" in the app spec).',
            'A bucket that already exists with fewer copies than --replicas is refused, with the command that raises it (ADR-0111).'],
  code:['crates/holon-park','crates/holon-vcs','components/{park,vcs}-{store,gateway}','reconciler/daemons/{park,vcs}/','apps/{park,vcs}.toml']},

 {id:'agents', name:'Agent services', short:'comp-agentd and the agent-runtime stack',
  purpose:'comp-agentd sessions with approvals, and the agent-runtime stack with its Matrix and desktop fronts.',
  blurb:'Two independent ways to run agents; neither calls the other. comp-agentd serves sessions, tasks and one resumable event stream with per-tool approval. agent-runtime runs autonomous agents with memory, schedules and events, fronted by a desktop console and a Matrix bridge.',
  entry:'REST, SSE and gRPC for agentd clients; Element and the GPUI console for agent-runtime',
  parts:[['native','comp-agentd'],['native','agent-runtime'],['native','agent-matrix'],['native','gpui-console'],['wasm','agent gateway']],
  uses:['llm','disk'],
  boundary:['comp-agentd tools are confined to the session’s workspace root; run cannot be confined, which is what approval is for.',
            'agent-runtime refuses a tool an agent was not granted instead of dispatching it, and only the owner’s Matrix messages start a run.',
            'Nothing in this repository is a client of comp-agentd yet: the contract came first, for bots, backends and scripts outside it (ADR-0102).'],
  code:['reconciler/src/bin/agentd/','api/holon/v1/agent.proto','api/openapi.yaml','agent-runtime/','agent-matrix/','gpui-console/']},

 {id:'loop', name:'Engineering loop', short:'goal to pull request',
  purpose:'Parallel AI branches, a real test gate, and a selector that opens the winner as a pull request.',
  blurb:'Each branch explores one goal in its own environment, a real test gate judges every candidate, and a selector opens the winner as a pull request. Nothing on that path is mocked. A run is only as good as the checks a person wrote for it, so every gate is run against the untouched base first.',
  entry:'holon goal run, and comp-goald draining a project’s queue',
  parts:[['native','comp-goalrun'],['native','comp-goald'],['wasm','agent-writer'],['wasm','graph-selector'],['wasm','knowledge-memory']],
  uses:['llm','surreal','git','nats'],
  boundary:['The gate is a native process because a component cannot spawn one: the sandbox working, not a gap (ADR-0095).',
            'graph:select/selector is the only path from a branch to a pull request, and it takes runs rather than files, so a rejected candidate cannot be landed.',
            'API keys arrive as file paths and reach the vault by reference; a key on a command line would be in every ps and shell history.'],
  code:['reconciler/src/{generation,gate,compose,contract,memory,trace}.rs','reconciler/src/bin/{goalrun,goald,selfimprove}.rs','components/{agent-writer,agent-driver,graph-selector,knowledge-memory,contract-registry,github-forge}','.comp/goals/']},

 {id:'proof', name:'Proof & measurement', short:'tests, e2e, benchmarks',
  purpose:'Test suites, end-to-end scripts and benchmarks that drive other machines.',
  blurb:'Every number in the docs is from a run recorded in an ADR. Test suites span the workspaces, shell and browser scripts exercise whole services, and the benchmark tools drive other machines.',
  entry:'cargo xtask test, e2e/run_all.sh, bench/, and CI',
  parts:[['native','xtask test'],['spec','e2e/'],['spec','bench/'],['native','comp-bench']],
  uses:['git'],
  boundary:['Suites that need NATS, SurrealDB, Docker or a model are kept out of CI and run locally; CI compiles and lints them.',
            'CI builds and tests; it does not deploy or publish. Publishing lives in its own workflows with their own triggers.'],
  code:['reconciler/tests/','reconciler/fleet/','e2e/','bench/','xtask/','reconciler/src/bin/{bench,matrix,planscale,hopcost,stub}.rs','.github/workflows/ci.yml']}
];

/* Overview: where the cards sit (column, row) and the primary correspondences drawn between them. */
export const OV_POS = {library:[0,0],delivery:[1,0],loop:[2,0],control:[0,1],runtime:[1,1],daemons:[2,1],agents:[0,2],durable:[1,2],proof:[2,2]};
export const OV_EDGES = [
  {a:'library',b:'delivery',as:'r',bs:'l',lab:'spec + composed .wasm',p:'art'},
  {a:'library',b:'control', as:'b',bs:'t',lab:'component push',p:'http'},
  {a:'delivery',b:'runtime',as:'b',bs:'t',lab:'units · pull by digest',p:'art'},
  {a:'control',b:'runtime',as:'r',bs:'l',lab:'NATS cmd ⇄ inventory',p:'nats',d:'<->'},
  {a:'control',b:'runtime',as:'r',bs:'l',lab:'ingress · secrets',p:'http',d:'<->'},
  {a:'runtime',b:'daemons',as:'r',bs:'l',lab:'loopback HTTP',p:'http'},
  {a:'runtime',b:'durable',as:'b',bs:'t',lab:'gateways → daemons',p:'http'},
  {a:'agents', b:'durable',as:'r',bs:'l',lab:'park · wake · cancel',p:'http'},
  {a:'durable',b:'agents', as:'l',bs:'r',lab:'sink delivery',p:'http'},
  {a:'loop',b:'daemons',as:'b',bs:'t',lab:'gate: comp-checks',p:'http'},
  {a:'loop',b:'runtime',as:'b',bs:'t',lab:'its own fleet',p:'nats'},
  {a:'loop',b:'library',as:'t',bs:'t',lab:'PRs · plug in-process',p:'art'},
  {a:'proof',b:'runtime',as:'t',bs:'b',lab:'load · e2e · bench',p:'http'}
];

/* ============ CORRESPONDENCES ============
   One row per thing one system says to another. `a` and `b` are [system or substrate, part].
   p is the line style on the diagrams. ADR numbers are the decisions that define the contract. */
export const COR = [
 {id:'L1',a:['library','apps/<name>.toml'],b:['delivery','holon … render'],p:'art',over:'file',contract:'A pure function from the spec to systemd units, env files and a Caddy route; wadm Applications for lanes 3 and 4.',adr:'0021'},
 {id:'L2',a:['library','comp-oci'],b:['oci','registry'],p:'art',over:'OCI',contract:'Push and pull by digest. A tag is a human convenience and is never what gets deployed.',adr:'0006 · 0024'},
 {id:'L3',a:['library','comp-capgraph'],b:['surreal','knowledge graph'],p:'call',over:'SurrealQL',contract:'The default surql projection of every interface, import edge and composed app into the knowledge graph.',adr:'0097 · 0091'},
 {id:'L4',a:['library','composed .wasm'],b:['runtime','comp-host'],p:'call',over:'process',contract:'cargo xtask host <app> runs a composed artifact on comp-host, for development.',adr:''},
 {id:'L5',a:['library','composed .wasm'],b:['control','platform-domain'],p:'http',over:'HTTP',contract:'POST /api/components?id=…, application/wasm. Reflection is the validation; a save that removes an export needs ?force=true.',adr:'0006'},
 {id:'L6',a:['loop','comp-goalrun'],b:['library','components'],p:'art',over:'git · library call',contract:'New components land as pull requests. reconciler/fleet/src/plug.rs composes a candidate in-process, with the wiring and the gaps as values.',adr:'0087 · 0089'},
 {id:'L7',a:['proof','suites'],b:['library','contract guards'],p:'call',over:'cargo test',contract:'contracts.rs (every import has a provider), witsurface.rs (shape moved means version moved), guestio.rs (no unbounded guest write).',adr:''},

 {id:'D1',a:['library','comp-plug · comp-oci'],b:['delivery','publish-apps.yml'],p:'art',over:'CI',contract:'On every merge to main: build the components, derive each app’s composition with comp-plug, push one artifact per app by digest.',adr:'0087 · 0006'},
 {id:'D2',a:['delivery','publish-apps.yml'],b:['oci','registry'],p:'art',over:'OCI',contract:'One composed artifact per app, named by its digest.',adr:'0006'},
 {id:'D5',a:['oci','registry'],b:['delivery','comp-agent'],p:'art',over:'OCI',contract:'The agent pulls by digest, never by tag, and writes down the installed digest.',adr:'0006'},
 {id:'D7',a:['delivery','holon fleet render'],b:['control','reconciler · ingress'],p:'art',over:'file',contract:'One directory per box holding its units and, on the control box, reconciler.env (with PLATFORM_SECRET to fill in).',adr:'0021'},
 {id:'D8',a:['delivery','lanes 1–2'],b:['runtime','comp-host'],p:'call',over:'systemd',contract:'A rendered unit runs comp-host; comp-agent restarts it when apps.lock names a new digest.',adr:''},

 {id:'C3',a:['control','console-domain'],b:['surreal','knowledge graph'],p:'http',over:'HTTP',contract:'knowledge:graph/store: the run view reads the merged store directly. Run history is not control-plane data.',adr:'0091 · 0092'},
 {id:'C4',a:['control','console-domain'],b:['git','forge'],p:'http',over:'HTTP',contract:'git:forge/repo: a goal’s spec is prose in git, so authoring one from the browser is a handful of HTTP calls that open a pull request.',adr:'0082 · 0092'},
 {id:'C5',a:['control','comp-reconciler'],b:['control','platform-domain'],p:'http',over:'HTTP',contract:'Polls /api/internal/{revisions, status, pending-pushes, artifact, pushed, fetch-token} with x-platform-secret. Internal to the control plane.',adr:'0022',internal:true},
 {id:'C6',a:['control','comp-reconciler'],b:['runtime','node agent'],p:'nats',over:'NATS',contract:'Commands on comp.v1.<lattice>.cmd.<node>.<verb>, request and reply, so “started” means “will serve”. Each node answers with a full inventory snapshot in KV comp-inventory, which expires on its own.',adr:'0021 · 0022 · 0058'},
 {id:'C7',a:['control','comp-ingress'],b:['runtime','HTTP listener'],p:'http',over:'HTTP',contract:'Proxies by Host header straight to a node on the tailnet; least outstanding requests, sheds load past max_inflight.',adr:'0029 · 0041'},
 {id:'C8',a:['control','comp-ingress'],b:['control','comp-reconciler'],p:'nats',over:'NATS',contract:'An activate command when a scaled-to-zero app gets its first request; one activation per host at a time. Internal to the control plane.',adr:'0042',internal:true},
 {id:'C9',a:['users','browsers · callers'],b:['control','comp-ingress'],p:'http',over:'HTTPS',contract:'Caddy terminates TLS for apps declared access = "public"; everything else is reachable on the tailnet only.',adr:'0029'},
 {id:'C10',a:['control','comp-relay'],b:['runtime','tenant apps'],p:'http',over:'HTTP',contract:'Pokes an app’s [triggers] pump (/internal/pump) on an interval so pull-based timers, topics and cron fire.',adr:'0096'},
 {id:'C11',a:['runtime','comp:secrets/reader'],b:['control','platform-domain'],p:'http',over:'HTTP',contract:'reveal fetches the plaintext on first use with a single-use token; a captured fetch is spent. The guest never holds a reference.',adr:'0051 · 0071'},
 {id:'C12',a:['loop','comp-goald'],b:['control','platform-domain'],p:'http',over:'HTTP',contract:'The project’s goal queue and events under /api/projects/{p}/… A person still starts every goal.',adr:'0082'},

 {id:'R1',a:['runtime','wasi:http outgoing'],b:['daemons','comp-*'],p:'http',over:'loopback HTTP',contract:'A component dials its daemon through wasi:http/outgoing-handler; the address is given by wasi:config/store. The daemon alone holds the syscall.',adr:'0095'},
 {id:'R2',a:['runtime','park / vcs guests'],b:['durable','gateways · comp-park · comp-vcs'],p:'http',over:'loopback HTTP',contract:'park-store and vcs-store are guests whose every call is one HTTP request to their daemon.',adr:'0100 · 0099'},
 {id:'R3',a:['runtime','wasi:keyvalue'],b:['nats','state backends'],p:'call',over:'--kv',contract:'--kv memory | sqlite | redis | nats | surreal | turso; the wasm is identical for every backend. A lattice node defaults to nats, the only backend where two replicas share a store.',adr:'0027 · 0057'},
 {id:'R4',a:['runtime','blob-store'],b:['s3','bucket'],p:'http',over:'S3 API',contract:'--blob s3 puts object bytes in S3, R2 or MinIO; keyed state stays on --kv.',adr:'0101'},
 {id:'R5',a:['runtime','wRPC'],b:['runtime','other nodes'],p:'nats',over:'NATS',contract:'Cross-node component links as wRPC on comp.v1.<lattice>.rpc.<instance>; one call costs about 57 µs.',adr:'0028 · 0074'},
 {id:'R7',a:['loop','comp-goalrun'],b:['runtime','comp-host'],p:'nats',over:'NATS · HTTP',contract:'comp-goalrun stands up its own Fleet (NATS, hosts, the gate’s runner) for the run and drives apps on it over HTTP.',adr:''},
 {id:'R8',a:['proof','bench · e2e'],b:['runtime','comp-host'],p:'http',over:'HTTP',contract:'Open-loop load and failure drills against real nodes: losing a machine, overload, cold start, many tenants.',adr:'0035 · 0036'},
 {id:'R9',a:['agents','agent gateway'],b:['runtime','comp-host'],p:'call',over:'lattice',contract:'The gateway is one generic wasm component deployed once per agent; it forwards W3C traceparent both ways.',adr:''},

 {id:'N1',a:['loop','checks-runner'],b:['daemons','comp-checks'],p:'http',over:'HTTP',contract:'Posts the candidate tree over a base tree keyed by commit id (an unknown commit is asked for with 409) and gets the whole check vector back.',adr:'0081'},
 {id:'N2',a:['daemons','comp-media'],b:['s3','bucket'],p:'http',over:'presigned HTTP',contract:'Photos too big for a guest’s 64 MiB go straight to object storage.',adr:'0098'},
 {id:'N3',a:['daemons','comp-media'],b:['nats','work queue'],p:'nats',over:'JetStream',contract:'Evaluation is a work queue on the machine with the GPU.',adr:'0098'},

 {id:'P1',a:['durable','park-store'],b:['durable','comp-park'],p:'http',over:'loopback HTTP',contract:'POST /v1/{park, wake, cancel, pending, …}, one JSON object per call, optional bearer token. Internal to the service.',adr:'0100',internal:true},
 {id:'P2',a:['durable','comp-park · comp-resume'],b:['nats','JetStream'],p:'nats',over:'JetStream',contract:'One KV bucket of tickets and a PARK_WAKE work-queue stream, kept on as many servers as --replicas says.',adr:'0100 · 0111'},
 {id:'P3',a:['durable','comp-resume'],b:['agents','comp-agentd'],p:'http',over:'HTTP',contract:'POST /park/deliver with Idempotency-Key: 2xx means taken, 410 means gone and cancels the ticket, anything else is retried with backoff.',adr:'0106 · 0113'},
 {id:'P4',a:['agents','comp-agentd'],b:['durable','comp-park'],p:'http',over:'HTTP',contract:'Parks an approval (correlation = call id, replay unsafe, deadline = the approval timeout), answers it, cancels it. Opt-in with --park-url.',adr:'0113'},
 {id:'P6',a:['durable','comp-vcs'],b:['surreal','symbol graph'],p:'http',over:'SurrealQL',contract:'The oplog is on JetStream; the commutative patch graph, positions and indexes are in SurrealDB.',adr:'0099'},
 {id:'P8',a:['clients','agents on the tailnet'],b:['durable','gateways'],p:'http',over:'HTTP',contract:'The gateways are the only door: tailnet access, optional token.',adr:'0099 · 0100'},

 {id:'A1',a:['clients','bots · scripts · UIs'],b:['agents','comp-agentd'],p:'http',over:'REST · SSE · gRPC',contract:'api/openapi.yaml and api/holon/v1/agent.proto on one port. Sessions, tasks, one resumable event stream, a state snapshot to catch up, steering, a bounded queue, per-tool approval with a scope.',adr:'0102 · 0107 · 0108 · 0112 · 0114'},
 {id:'A2',a:['agents','comp-agentd'],b:['llm','providers'],p:'http',over:'HTTPS',contract:'Anthropic /v1/messages or any OpenAI-compatible /chat/completions, streamed; a mock provider for tests.',adr:'0102'},
 {id:'A4',a:['agents','agent-runtime'],b:['llm','providers'],p:'http',over:'HTTPS',contract:'On-device, OpenAI-compatible or Anthropic models; each run has a step, token and daily budget.',adr:''},
 {id:'A7',a:['element','Element'],b:['agents','agent-matrix'],p:'http',over:'Matrix',contract:'Every agent is a Matrix user and every project a Space, bridged by an appservice; approvals are polls.',adr:'0105'},

 {id:'E2',a:['loop','agent-driver'],b:['llm','providers'],p:'http',over:'HTTPS',contract:'anthropic-provider (or openai-provider) is the model behind each branch; a real model, not a mock.',adr:''},
 {id:'E4',a:['loop','github-forge'],b:['git','forge'],p:'http',over:'HTTPS',contract:'Opens the pull request for the selected candidate; the only path from a branch to a PR.',adr:''},
 {id:'E5',a:['loop','knowledge-memory'],b:['surreal','knowledge graph'],p:'http',over:'SurrealQL',contract:'KNN fused with TF-IDF; observe and promote split at the linker, so an agent cannot write what the swarm believes.',adr:'0084 · 0091'},
 {id:'E6',a:['loop','run trace'],b:['surreal','knowledge graph'],p:'http',over:'SurrealQL',contract:'run, attempt, event and capability rows, appended as it goes; a run that dies of its own telemetry is worse than one with none.',adr:'0092'},

 {id:'X1',a:['proof','ci.yml'],b:['git','GitHub Actions'],p:'call',over:'CI',contract:'Builds and tests every workspace, runs clippy and semgrep, typechecks the console SPA. Never deploys or publishes.',adr:''},
 {id:'X2',a:['proof','e2e fleets'],b:['control','platform · reconciler · ingress'],p:'nats',over:'NATS · HTTP',contract:'Whole fleets in tests: several manifests on one fleet, two reconcilers with one leader, two ingresses and one dies.',adr:'0035'}
];

/* ============ ISOLATION DIAGRAMS ============
   A grid per system: nodes sit on columns c0..c2 and rows r0..; peers are dashed "ghost"
   boxes at the sides. `x` edges are correspondences with the outside, numbered and listed
   under the drawing from COR; plain edges are inside the system. */
const N = (id,c,r,k,t,s,o) => Object.assign({id,c,r,k,t,s},o||{});
const G = (id,sys,t,s,side,o) => Object.assign({id,sys,t,s,side},o||{});

export const DIA = {
 library: {
  nodes:[
   N('wit',0,0,'spec','WIT contracts',['wit/ · every interface the repo defines','a changed shape needs a new version']),
   N('comps',1,0,'wasm','Components',['components/ · wasm32-wasip2 crates','the *-domain ones are the apps']),
   N('capgraph',2,0,'native','comp-capgraph',['what imports what, read off','the built wasm · + comp-catalog'],{m:1}),
   N('spec',0,1,'spec','App spec',['apps/<name>.toml, one per app','the one hand-written file']),
   N('plug',1,1,'native','comp-plug',['derives a composition from','the binary’s real imports (wac)'],{m:1}),
   N('composed',2,1,'wasm','Composed artifact',['one .wasm per app','named by its digest']),
   N('guards',1,2,'spec','Contract guards',['every import has a provider','bounded guest writes']),
   N('oci',2,2,'native','comp-oci',['push and pull by digest','ghcr.io/<owner>/holon/<name>'],{m:1})
  ],
  edges:[
   ['wit','comps','contract','call'],['comps','capgraph','built wasm','call'],['comps','plug','built wasm','call'],
   ['spec','plug','root','call'],['plug','composed','compose','call'],['composed','oci','push','art'],['guards','plug','checks','call']
  ],
  ghosts:[
   G('g-del','delivery','Delivery',['renders the spec'],'l',{r:1}),
   G('g-loop','loop','Engineering loop',['writes components'],'l',{r:0}),
   G('g-proof','proof','Proof & measurement',['cargo test'],'l',{r:2}),
   G('g-cp','control','Control plane',['platform-domain'],'r',{r:1,dy:-44}),
   G('g-run','runtime','Runtime',['comp-host'],'r',{r:1,dy:44}),
   G('g-oci','oci','OCI registry',['GHCR'],'r',{r:2,store:1}),
   G('g-sur','surreal','SurrealDB',['knowledge graph'],'r',{r:0,store:1})
  ],
  x:[
   {a:'spec',b:'g-del',n:1,lab:'app spec',cor:['L1']},
   {a:'g-loop',b:'comps',n:2,lab:'PRs · plug',cor:['L6']},
   {a:'g-proof',b:'guards',n:3,lab:'cargo test',cor:['L7']},
   {a:'composed',b:'g-cp',n:4,lab:'push .wasm',cor:['L5']},
   {a:'composed',b:'g-run',n:5,lab:'xtask host',cor:['L4']},
   {a:'oci',b:'g-oci',n:6,lab:'OCI',cor:['L2']},
   {a:'capgraph',b:'g-sur',n:7,lab:'surql',cor:['L3']}
  ]},

 delivery: {
  nodes:[
   N('spec',0,0,'spec','App spec',['apps/<name>.toml','fleet.example.toml for a lattice']),
   N('render',1,0,'native','holon … render',['node · fleet · wadm','pure functions of the spec'],{m:1}),
   N('out',2,0,'spec','Rendered files',['unit + env + Caddy route','per-box dirs · wadm Application']),
   N('l1',0,1,'native','Lane 1: one box',['comp-host + systemd + Caddy','no control plane']),
   N('l2',1,1,'native','Lane 2: a lattice',['comp-host per node, plus','reconciler + ingress over NATS']),
   N('l3',2,1,'ext','Lanes 3 and 4',['wasmCloud 1.x (wadm) or 2.x','(runtime-operator, Kubernetes)']),
   N('publish',0,2,'spec','publish-apps.yml',['on merge to main: compose every','app, publish by digest']),
   N('lock',1,2,'store','apps.lock',['digests, on the deploy branch','the one mutable pointer']),
   N('agent',2,2,'native','comp-agent',['on the box, on a timer: pull by','digest, install, restart the unit'],{m:1})
  ],
  groups:[{id:'lanes',t:'Where it runs',ids:['l1','l2','l3']}],
  edges:[
   ['spec','render','render','call'],['render','out','writes','call'],
   ['out','grp:lanes','ship','art'],
   ['publish','lock','git push','art'],['lock','agent','GET','http']
  ],
  ghosts:[
   G('g-lib','library','Library & Build',['comp-plug · comp-oci'],'l',{r:2}),
   G('g-run','runtime','Runtime',['comp-host'],'l',{r:1}),
   G('g-cp','control','Control plane',['reconciler.env + units'],'r',{r:0}),
   G('g-oci','oci','OCI registry',['GHCR'],'r',{r:2,store:1})
  ],
  x:[
   {a:'g-lib',b:'publish',n:1,lab:'build + plug',cor:['D1']},
   {a:'publish',b:'g-oci',n:2,lab:'push',cor:['D2']},
   {a:'g-oci',b:'agent',n:3,lab:'pull',cor:['D5']},
   {a:'out',b:'g-cp',n:4,lab:'fleet render',cor:['D7']},
   {a:'l1',b:'g-run',n:5,lab:'unit',cor:['D8']}
  ]},

 control: {
  nodes:[
   N('cli',0,0,'actor','holon',['login · component push · app','org · project · goal · secret'],{m:1}),
   N('console',1,0,'wasm','Holon console',['console-domain + SPA assets','a second client, not a second plane']),
   N('platform',1,1,'wasm','platform-domain',['orgs · catalogue · deployments','revisions · secrets · market']),
   N('recon',2,1,'native','comp-reconciler',['pure plan() diff, one leader','sole start/stop authority'],{m:1}),
   N('nats',1,2,'store','NATS lattice buckets',['comp-inventory (TTL) · comp-artifacts','comp-load · lease · cmd subjects']),
   N('ingress',2,2,'native','comp-ingress',['Host header → replica','least-outstanding · sheds · activates'],{m:1}),
   N('relay',2,3,'native','comp-relay',['pokes /internal/pump so pull-','based timers and topics fire'],{m:1})
  ],
  edges:[
   ['cli','platform','HTTP /api','http'],['console','platform','HTTP /api','http'],['recon','platform','poll','http'],
   ['recon','nats','cmd · lease','nats'],['ingress','nats','inventory','nats'],['ingress','recon','activate','nats']
  ],
  ghosts:[
   G('g-lib','library','Library & Build',['composed .wasm'],'l',{r:0}),
   G('g-loop','loop','Engineering loop',['comp-goald'],'l',{r:1}),
   G('g-sur','surreal','SurrealDB',['run history'],'r',{r:0,dy:-34,store:1}),
   G('g-git','git','Git (GitHub)',['goals as PRs'],'r',{r:0,dy:40,store:1}),
   G('g-del','delivery','Delivery',['fleet render'],'r',{r:1,dy:-30}),
   G('g-users','users','End users',['browsers · callers'],'r',{r:2,dy:-62,actor:1}),
   G('g-run','runtime','Runtime',['comp-host nodes'],'r',{r:2,dy:96,h:150})
  ],
  x:[
   {a:'g-lib',b:'cli',n:1,lab:'push .wasm',cor:['L5']},
   {a:'g-loop',b:'platform',n:2,lab:'queue · API',cor:['C12']},
   {a:'console',b:'g-sur',n:3,lab:'read runs',cor:['C3']},
   {a:'console',b:'g-git',n:4,lab:'author goal',cor:['C4']},
   {a:'g-del',b:'recon',n:5,lab:'render',cor:['D7']},
   {a:'recon',b:'g-run',n:6,lab:'NATS',cor:['C6'],d:'<->'},
   {a:'ingress',b:'g-run',n:7,lab:'proxy',cor:['C7']},
   {a:'relay',b:'g-run',n:8,lab:'pump',cor:['C10']},
   {a:'g-run',b:'platform',n:9,lab:'reveal',cor:['C11']},
   {a:'g-users',b:'ingress',n:10,lab:'HTTP',cor:['C9']}
  ]},

 runtime: {
  rowGap:[0,64,100,74],
  nodes:[
   N('http',0,0,'native','HTTP listener',['hyper · Host header →','instance routing table']),
   N('engine',1,0,'native','wasmtime 45',['component model · WASI p2 + p3','pooling allocator · compiled cache']),
   N('agent',2,0,'native','Node agent',['inventory snapshot on a timer','start / stop · fetch by digest']),
   N('inst',0,1,'wasm','Component instances',['composed apps, every org on the node. No sockets, no processes, no environment: only what the host links.'],{cs:3}),
   N('kv',0,2,'native','wasi:keyvalue',['memory · sqlite · redis · nats ·','surreal · turso · + --blob s3']),
   N('secrets',1,2,'native','comp:secrets/reader',['a guest names a key; the host','reveals the value, never a ref'],{m:1}),
   N('cfg',2,2,'native','config · egress',['wasi:config per instance,','outgoing-handler allow-list']),
   N('rpc',1,3,'native','wRPC',['cross-node component links','comp.v1.<lattice>.rpc.<id>'])
  ],
  groups:[{id:'tenant',t:'Resolved per tenant (tenant.rs): BucketId · InstanceId · SecretRef',ids:['inst','kv','secrets','cfg']}],
  edges:[['http','engine','dispatch','call'],['agent','engine','start · stop','call'],['engine','inst','instantiate','call']],
  ghosts:[
   G('g-del','delivery','Delivery',['systemd unit'],'t',{x:276}),
   G('g-lib','library','Library & Build',['cargo xtask host'],'t',{x:466}),
   G('g-loop','loop','Engineering loop',['its own Fleet'],'t',{x:656}),
   G('g-proof','proof','Proof & measurement',['load · e2e'],'t',{x:846}),
   G('g-ing','control','Control plane',['comp-ingress'],'l',{r:0}),
   G('g-agents','agents','Agent services',['agent gateway guest'],'l',{r:1}),
   G('g-state','nats','State backends',['NATS · SQLite · Redis','SurrealDB · Turso · S3/R2'],'l',{r:2,store:1,h:62}),
   G('g-cpn','control','Control plane',['comp-reconciler · NATS'],'r',{r:0}),
   G('g-cp2','control','Control plane',['platform-domain'],'r',{r:2,dy:-70}),
   G('g-dae','daemons','Capability daemons',['loopback HTTP'],'r',{r:2,dy:-4}),
   G('g-dur','durable','Durable services',['park · vcs'],'r',{r:2,dy:62}),
   G('g-peer','runtime','Other comp-host nodes',['wRPC over NATS'],'r',{r:3,ext:1})
  ],
  x:[
   {a:'g-ing',b:'http',n:1,lab:'HTTP',cor:['C7']},
   {a:'g-del',b:'http',n:2,lab:'starts it',cor:['D8']},
   {a:'g-lib',b:'http',n:3,lab:'xtask',cor:['L4']},
   {a:'g-loop',b:'http',n:4,lab:'Fleet',cor:['R7']},
   {a:'g-proof',b:'http',n:5,lab:'load',cor:['R8']},
   {a:'g-agents',b:'inst',n:6,lab:'hosted',cor:['R9']},
   {a:'agent',b:'g-cpn',n:7,lab:'NATS',cor:['C6'],d:'<->'},
   {a:'secrets',b:'g-cp2',n:8,lab:'reveal',cor:['C11']},
   {a:'cfg',b:'g-dae',n:9,lab:'loopback',cor:['R1']},
   {a:'cfg',b:'g-dur',n:10,lab:'loopback',cor:['R2']},
   {a:'kv',b:'g-state',n:11,lab:'--kv',cor:['R3','R4']},
   {a:'rpc',b:'g-peer',n:12,lab:'wRPC',cor:['R5'],d:'<->'}
  ]},

 daemons: {
  dir:'TB', W:1430, cols:[256,506,756,1006], cw:176, rowGap:[0,66,66,66],
  nodes:[
   N('dom',1,0,'wasm','*-domain wrapper',['exports wasi:http: the app\u2019s','entry point for one capability']),
   N('cap',2,0,'wasm','Capability crate',['a WIT contract and nothing else','fs-watcher · lan-scanner · \u2026']),
   N('broker',0,1,'ext','Runtime \u00b7 comp-host',['wasi:http/outgoing-handler dials a loopback address; the address comes from wasi:config/store, set per instance. The guest holds no socket.'],{cs:4,ghost:1,sys:'runtime'}),
   N('d-files',0,2,'native','Files and desktop',['comp-fswatch · comp-clipboard','comp-uinotify · comp-cron']),
   N('d-net',1,2,'native','Network',['comp-lanscan · comp-mdns','comp-wireguard']),
   N('d-proc',2,2,'native','Processes and codecs',['comp-docker · comp-ffmpeg','comp-browser · comp-imageopt']),
   N('d-data',3,2,'native','Models, data, gate',['comp-llmlocal · comp-media','comp-mailrelay · comp-checks']),
   N('machine',0,3,'ext','The machine',['docker socket · crontab · desktop session · LAN · WireGuard · Chrome · ffmpeg · Ollama \u2014 what the daemon, and only the daemon, may touch.'],{cs:4})
  ],
  groups:[
   {id:'guest',t:'Guest \u00b7 no sockets, no processes',ids:['dom','cap']},
   {id:'nd',t:'Native daemons',ids:['d-files','d-net','d-proc','d-data']}
  ],
  edges:[['dom','cap','calls','call'],['cap','broker','import','http'],
         ['d-files','machine','syscalls','call'],['d-net','machine','','call'],['d-proc','machine','','call'],['d-data','machine','','call']],
  ghosts:[
   G('g-loop','loop','Engineering loop',['checks-runner'],'r',{r:2,dy:-96}),
   G('g-s3','s3','S3 / R2',['bucket'],'r',{r:2,dy:-30,store:1}),
   G('g-nats','nats','NATS JetStream',['work queue'],'r',{r:2,dy:36,store:1}),
   G('g-smtp','ext','SMTP',['MailHog'],'r',{r:2,dy:102,ext:1})
  ],
  x:[
   {a:'broker',b:'d-files',n:1,lab:'loopback HTTP',cor:['R1']},
   {a:'broker',b:'d-net',p:'http'},{a:'broker',b:'d-proc',p:'http'},{a:'broker',b:'d-data',p:'http'},
   {a:'g-loop',b:'d-data',n:2,lab:'comp-checks',cor:['N1']},
   {a:'d-data',b:'g-s3',n:3,lab:'upload',cor:['N2']},
   {a:'d-data',b:'g-nats',n:4,lab:'queue',cor:['N3']},
   {a:'d-data',b:'g-smtp',lab:'SMTP',p:'http'}
  ]},

 durable: {
  nodes:[
   N('pgw',0,0,'wasm','park-gateway',['HTTP routes · tailnet','apps/park.toml']),
   N('pst',1,0,'wasm','park-store',['holon:park@0.4.0','one request per call']),
   N('ppark',2,0,'native','comp-park',['loopback HTTP /v1/{park,wake,','cancel,pending,…} · holon-park'],{m:1}),
   N('ptk',1,1,'store','NATS JetStream',['KV bucket: tickets','PARK_WAKE work-queue stream']),
   N('pres',2,1,'native','comp-resume',['deliver resolved tickets,','recovery sweep every N s'],{m:1}),
   N('vgw',0,2,'wasm','vcs-gateway',['HTTP routes · tailnet','apps/vcs.toml']),
   N('vst',1,2,'wasm','vcs-store',['holon:vcs/code-store + files','one request per call']),
   N('vvcs',2,2,'native','comp-vcs',['loopback HTTP · holon-vcs','oplog of commutative patches'],{m:1}),
   N('vsur',1,3,'store','SurrealDB',['symbol graph · positions','indexes']),
   N('vlog',2,3,'store','NATS JetStream',['oplog streams'])
  ],
  groups:[{id:'park',t:'park (ADR-0100, 0106, 0109–0111)',ids:['pgw','pst','ppark','ptk','pres']},{id:'vcs',t:'vcs (ADR-0099)',ids:['vgw','vst','vvcs','vsur','vlog']}],
  edges:[
   ['pgw','pst','call','call'],['pst','ppark','HTTP','http'],['ppark','ptk','tickets','nats'],['pres','ptk','sweep','nats'],
   ['vgw','vst','call','call'],['vst','vvcs','HTTP','http'],['vvcs','vlog','oplog','nats'],['vvcs','vsur','graph','http']
  ],
  ghosts:[
   G('g-run','runtime','Runtime',['hosts the gateways'],'l',{r:0,dy:-24}),
   G('g-run2','runtime','Runtime',['hosts the gateways'],'l',{r:2,dy:-24}),
   G('g-cl','clients','Agents on the tailnet',['HTTP clients'],'l',{r:1,dy:54,actor:1}),
   G('g-ag1','agents','Agent services',['comp-agentd'],'r',{r:0}),
   G('g-ag2','agents','Agent services',['comp-agentd sink'],'r',{r:1})
  ],
  x:[
   {a:'g-run',b:'pgw',n:1,lab:'serves',cor:['R2'],p:'call'},
   {a:'g-run2',b:'vgw',n:2,lab:'serves',cor:['R2'],p:'call'},
   {a:'g-cl',b:'pgw',n:3,lab:'HTTP',cor:['P8']},
   {a:'g-ag1',b:'ppark',n:4,lab:'park · wake',cor:['P4']},
   {a:'pres',b:'g-ag2',n:5,lab:'sink',cor:['P3']}
  ]},

 agents: {
  W:1430, cols:[256,506,756,1006], cw:176, rowGap:[0,66,66,110,66],
  nodes:[
   N('tools',3,0,'native','Tools',['list_dir · read_file · write_file','run · workspace-confined*']),
   N('api',0,1,'native','REST · SSE · gRPC',['openapi.yaml + agent.proto,','one port · bearer token']),
   N('sess',1,1,'native','Sessions',['session · task · event log','steer · queue · state snapshot']),
   N('loop',2,1,'native','Agent loop',['model_turn \u2192 tools_turn \u2192 ask','budget · max turns']),
   N('prov',3,1,'native','Providers',['anthropic · openai-compatible','mock']),
   N('disk',1,2,'store','State dir',['session.json · events.jsonl','history.json · checkpoint.json']),
   N('appr',3,2,'native','Approvals',['once · task · session scope,','held in memory or parked']),
   N('gpui',1,3,'native','gpui-console',['desktop UI; embeds the runtime;','where a human approves']),
   N('rt',2,3,'native','agent-runtime',['spec + bounded tool loop, memory,','run log, triggers · --state-dir']),
   N('gw',3,3,'wasm','agent gateway',['one generic component per agent','HTTP front door on the lattice']),
   N('syn',1,4,'ext','Synapse',['Matrix homeserver','infra/matrix, on the tailnet']),
   N('mx',2,4,'native','agent-matrix',['appservice: agent = user,','project = Space · approvals = polls'],{m:1})
  ],
  groups:[
   {id:'ad',t:'comp-agentd (ADR-0102, 0107, 0108, 0112\u20130114)',ids:['api','sess','loop','tools','prov','disk','appr']},
   {id:'ar',t:'agent-runtime stack',ids:['gpui','rt','gw','syn','mx']}
  ],
  edges:[
   ['api','sess','call','call'],['sess','loop','start','call'],['loop','tools','tool call','call'],['loop','prov','model','call'],['loop','appr','ask','call'],['sess','disk','log','call'],
   ['gpui','rt','embeds','call'],['gw','rt','HTTP','http'],['mx','rt','HTTP','http'],['mx','syn','appservice','http']
  ],
  ghosts:[
   G('g-cl','clients','Agent clients',['bots · scripts · UIs'],'l',{r:1,dy:-46,actor:1}),
   G('g-res','durable','Durable services',['comp-resume sink'],'l',{r:1,dy:40}),
   G('g-park','durable','Durable services',['comp-park'],'r',{r:2}),
   G('g-llm','llm','LLM providers',['HTTPS'],'r',{r:1,store:1}),
   G('g-run','runtime','Runtime',['comp-host'],'r',{r:3}),
   G('g-el','element','Element (Matrix)',['chat · polls'],'l',{r:4,actor:1})
  ],
  x:[
   {a:'g-cl',b:'api',n:1,lab:'REST',cor:['A1']},
   {a:'g-res',b:'api',n:2,lab:'sink',cor:['P3']},
   {a:'appr',b:'g-park',n:3,lab:'park',cor:['P4']},
   {a:'prov',b:'g-llm',n:4,lab:'HTTPS',cor:['A2']},
   {a:'gw',b:'g-run',n:5,lab:'hosted',cor:['R9']},
   {a:'g-el',b:'syn',n:6,lab:'Matrix',cor:['A7']}
  ]},

 loop: {
  rowGap:[0,64,64,80],
  nodes:[
   N('goald',0,0,'native','comp-goald',['drains a project\u2019s queue; goals','are prose in git; a person starts one'],{m:1}),
   N('run',1,0,'native','comp-goalrun',['goal \u2192 pull request; stands up','a Fleet: NATS, hosts, the gate'],{m:1}),
   N('trace',2,0,'spec','Run trace',['run \u2192 round \u2192 attempt \u2192','capability rows, as it goes']),
   N('recall',0,1,'wasm','knowledge-memory',['recalls lessons; records every','verdict. An agent cannot promote']),
   N('branches',1,1,'wasm','Branches \u00d7 N',['agent-writer + agent-driver:','attempt \u2192 judge \u2192 repair']),
   N('prov',2,1,'wasm','anthropic-provider',['the model behind each branch']),
   N('forge',0,2,'wasm','github-forge',['opens the pull request']),
   N('sel',1,2,'wasm','graph-selector',['the only path from a branch','to a pull request']),
   N('checks',2,2,'wasm','checks-runner',['posts the candidate tree to','the gate, gets a check vector']),
   N('contract',0,3,'wasm','contract-registry',['parts of a decomposed goal','negotiate an interface']),
   N('selfimp',2,3,'native','comp-selfimprove',['recursive gate: promote only if','healthy and more capable'],{m:1})
  ],
  groups:[{id:'extra',t:'Used by some runs only',ids:['contract','selfimp']}],
  edges:[
   ['goald','run','start','call'],['run','trace','rows','call'],['run','branches','fan out','call'],
   ['branches','prov','model','http'],['branches','recall','lessons','http'],['branches','sel','candidates','call'],['branches','checks','judge','call'],['sel','forge','PR','call']
  ],
  ghosts:[
   G('g-cp','control','Control plane',['platform-domain'],'l',{r:0}),
   G('g-lib','library','Library & Build',['components, plug'],'t',{x:470}),
   G('g-run','runtime','Runtime',['its own fleet of hosts'],'t',{x:660}),
   G('g-sur','surreal','SurrealDB',['knowledge graph'],'r',{r:0,store:1}),
   G('g-sur2','surreal','SurrealDB',['knowledge graph'],'l',{r:1,store:1}),
   G('g-llm','llm','LLM providers',['HTTPS'],'r',{r:1,store:1}),
   G('g-dae','daemons','Capability daemons',['comp-checks'],'r',{r:2}),
   G('g-git','git','Git (GitHub)',['forge'],'l',{r:2,store:1})
  ],
  x:[
   {a:'g-cp',b:'goald',n:1,lab:'queue',cor:['C12']},
   {a:'trace',b:'g-sur',n:2,lab:'rows',cor:['E6']},
   {a:'recall',b:'g-sur2',n:3,lab:'KNN',cor:['E5']},
   {a:'run',b:'g-lib',n:4,lab:'PRs',cor:['L6'],p:'art'},
   {a:'prov',b:'g-llm',n:5,lab:'HTTPS',cor:['E2']},
   {a:'checks',b:'g-dae',n:6,lab:'checks',cor:['N1']},
   {a:'forge',b:'g-git',n:7,lab:'open PR',cor:['E4']},
   {a:'run',b:'g-run',n:8,lab:'Fleet',cor:['R7']}
  ]},

 proof: {
  nodes:[
   N('ci',0,0,'native','CI · GitHub Actions',['ci.yml builds and tests;','never deploys or publishes']),
   N('xt',1,0,'native','cargo xtask test',['compiles every test target','first, then runs them'],{m:1}),
   N('suites',2,0,'spec','reconciler/tests',['fleets · ha · leader · scaling','contracts · witsurface · guestio']),
   N('bench',0,1,'spec','bench/',['HOST-PERF · FLEET-BENCH · needle','adversarial · tenancy · versus']),
   N('tools',1,1,'native','Bench tools',['comp-bench · comp-matrix','comp-planscale · comp-hopcost · comp-stub']),
   N('e2e',2,1,'spec','e2e/',['Playwright and shell:','park.sh · vcs.sh · photoquest.sh'])
  ],
  edges:[['ci','xt','runs','call'],['xt','suites','runs','call'],['tools','bench','numbers','call']],
  ghosts:[
   G('g-git','git','Git (GitHub)',['Actions'],'l',{r:0,store:1}),
   G('g-run','runtime','Runtime',['comp-host nodes'],'l',{r:1}),
   G('g-lib','library','Library & Build',['contract guards'],'r',{r:0}),
   G('g-cp','control','Control plane',['platform · reconciler · ingress'],'r',{r:1})
  ],
  x:[
   {a:'ci',b:'g-git',n:1,lab:'CI',cor:['X1']},
   {a:'bench',b:'g-run',n:2,lab:'load',cor:['R8']},
   {a:'suites',b:'g-lib',n:3,lab:'tests',cor:['L7']},
   {a:'e2e',b:'g-cp',n:4,lab:'fleets',cor:['X2']}
  ]}
};

/* ============ BOUNDARIES ============ */
export const GUEST_NAMES = [
  ['a store name ("default")','a BucketId','0023'],
  ['an import interface','an InstanceId in the link table','0013'],
  ['a config key','a value the uploader declared','0047'],
  ['a secret key','a SecretRef, then a value it never sees','0051 · 0061']
];
export const CREDS = [
  ['The right to start code on every node','comp-reconciler, and nothing else','Holds the lattice credential. No business logic, no database, no user concept.','0022'],
  ['x-platform-secret','comp-reconciler to platform-domain','Guards /api/internal/*. Left empty, the reconciler does not converge.','0022'],
  ['An org’s secrets','platform-domain’s vault','Named by reference (vault://org/key), never by value, in a manifest. A host reveals one on a single-use fetch.','0010 · 0051 · 0071'],
  ['Bearer tokens on native daemons','comp-agentd, comp-park, comp-vcs, comp-checks','Optional on loopback; comp-checks refuses to start off loopback without one.','0113'],
  ['Model and forge keys','comp-goalrun','Arrive as file paths and reach the vault as vault://…=@path, so they are never on argv.','0082'],
  ['The deploy pointer','the deploy branch of the repository','comp-agent holds no CI credential and opens no inbound port; it pulls.','0006']
];

/* ============ BOUNDARIES ============
   Outermost first. Each zone sits inside the one before it. */
export const ZONES = [
  {id:'z1', t:'Public internet', d:'Reachable only where an app says so: a generated file never makes an app public. access = "public" puts it behind Caddy and ACME.',
   items:['End users','GitHub','GHCR','LLM APIs','S3 / R2']},
  {id:'z2', t:'Tailnet', d:'The default reach of every app. Nodes are mutually reachable and say where, so the ingress proxies straight to them and no envelope sits on the bus.',
   items:['comp-ingress','platform-domain','NATS lattice','park / vcs gateways','comp-agentd','Synapse']},
  {id:'z3', t:'One machine', d:'systemd units. Native daemons bind loopback, each with its own allow-list and blast radius. A component reaches one only through an address the operator configured.',
   items:['comp-agent','comp-park','comp-vcs','comp-checks','the capability daemons','local disk: SQLite, state dirs']},
  {id:'z4', t:'comp-host, one process', d:'Every tenant on the node lives here. There is no OS process between two tenants: only wasmtime\u2019s sandbox and the discipline of tenant.rs, which makes it the one module to be paranoid in.',
   items:[]}
];
export const TENANTS = {
  names:['Org A','Org B','Org C'],
  text:'Its own BucketIds, SecretRefs and config. No sockets, no processes, no environment: only the imports the host links for it.'
};
export const RULE = {
  t:'A name is a real boundary only if (1) host-side state the guest cannot write chose it, and (2) the guest has no second path into the namespace.',
  note:'The one rule the isolation story applies at every name a guest can use, each enforced by a private newtype rather than by review. ADR-0023, and docs/CURRENT.md.'
};
