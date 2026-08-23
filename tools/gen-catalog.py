#!/usr/bin/env python3
"""Generate the component catalog: components/CATALOG.md + catalog.json.

The embryo of a platform catalog service: for every component under
components/, extract what a consumer needs to adopt it *without reading the
source* — the WIT package/world (interface contract), its dependency
footprint (which host capabilities it needs), the wasi:config knobs it reads
(detected from source), the built artifact size, and the one-line description
from the crate's module doc.

Sources of truth, laziest that works:
  - wit/*.wit          -> package, exports, imports (regex; no wit parser dep)
  - src/lib.rs //! line -> description
  - src/*.rs           -> config knob detection: string literals inside
                          *get*("...") calls, only for components that import
                          wasi:config (kv get("key") noise filtered that way)
  - target/wasm32-wasip2/release/<name>.wasm -> size + sha256 (if built)

Usage: python3 tools/gen-catalog.py   (from the comp/ directory)
"""

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPONENTS = ROOT / "components"
RELEASE = COMPONENTS / "target" / "wasm32-wasip2" / "release"

# app/demo components: listed, but flagged not-reusable-as-is.
#
# The `-domain` suffix is the repo's own convention for "this is an application",
# so it is DERIVED rather than listed. The hand-written set below had ten names in
# it while the tree had sixty-three domains, and `capsearch` uses this flag to
# keep showcases from outranking the capabilities they are built from — so a
# stale list meant fifty-three applications competing with real capabilities for
# every goal.
#
# The explicit names are the ones the convention does not catch.
APP_SPECIFIC = {
    "vet-domain", "login-app", "accounts-app", "sample-consumer", "bench-suite",
    "link-shortener", "dev-portal", "webhook-relay", "billing-ledger", "status-page",
}


def is_app(name: str) -> bool:
    return name.endswith("-domain") or name in APP_SPECIFIC

GET_CALL = re.compile(r'[a-z_]*(?:get|cfg)[a-z_0-9]*\(\s*"([a-z0-9._-]{2,})"')
# the repo convention: a module-doc block listing knobs with descriptions:
#   //! Config (wasi:config/store):
#   //!   max-attempts     failures allowed per window ... (default 5)
CONFIG_BLOCK = re.compile(r"^//! Config[^\n]*:\n((?://!.*\n)+)", re.M)
CONFIG_LINE = re.compile(r"^//!\s{2,}([a-z0-9._-]{2,})\s{2,}(.+?)\s*$", re.M)
EXPORT = re.compile(r"^\s*export\s+([^;]+);", re.M)
IMPORT = re.compile(r"^\s*import\s+([^;]+);", re.M)
PACKAGE = re.compile(r"^package\s+([^;]+);", re.M)
STD_WASI = re.compile(r"^wasi:(clocks|random|io|cli|filesystem|sockets)/")


def first_doc_line(lib_rs: Path) -> str:
    for line in lib_rs.read_text().splitlines():
        line = line.strip()
        if line.startswith("//!"):
            text = line[3:].strip()
            if text:
                return text.rstrip(".")
        elif line and not line.startswith("//"):
            break
    return ""


def component_wit_dir(d: Path) -> Path | None:
    """Where this crate's WIT actually lives.

    A local `wit/` is the common case and was once assumed to be the only one.
    It is not: a crate may point `[package.metadata.component.target].path` at a
    shared directory instead, and `auth-guard` does — it uses the repo-root
    `wit/` so it can share the wkg-vendored `wasi:*` packages.

    That assumption made the single most-depended-on capability in the tree
    invisible to `capsearch`. 36 applications import `auth:guard`, and a goal
    asking to let users log in could not find it, because a crate with no local
    `wit/` was skipped before its description was ever read.
    """
    manifest = d / "Cargo.toml"
    if manifest.is_file():
        text = manifest.read_text(encoding="utf-8", errors="ignore")
        # Only trust the path when the crate declares itself a component at all.
        if "[package.metadata.component]" in text:
            m = re.search(
                r"\[package\.metadata\.component\.target\][^\[]*?^path\s*=\s*\"([^\"]+)\"",
                text,
                re.M | re.S,
            )
            if m:
                candidate = (d / m.group(1)).resolve()
                if candidate.is_dir():
                    return candidate
    local = d / "wit"
    return local if local.is_dir() else None


def scan_component(d: Path):
    wit_dir = component_wit_dir(d)
    wits = sorted(wit_dir.glob("*.wit")) if wit_dir else []
    lib = d / "src" / "lib.rs"
    if not wits or not lib.is_file():
        return None
    wit_text = "\n".join(w.read_text() for w in wits)

    pkg = PACKAGE.search(wit_text)
    exports = [e.strip() for e in EXPORT.findall(wit_text)]
    imports = [i.strip().split("@")[0] for i in IMPORT.findall(wit_text)]
    deps = sorted({i for i in imports if not STD_WASI.match(i)})

    config_keys = []  # [{name, description}]
    if any(i.startswith("wasi:config") for i in deps):
        seen = set()
        for src in sorted((d / "src").glob("*.rs")):
            if src.name == "bindings.rs":
                continue
            text = src.read_text()
            # preferred: the documented Config block (name + description).
            for block in CONFIG_BLOCK.findall(text):
                for name, desc in CONFIG_LINE.findall(block):
                    if name not in seen:
                        seen.add(name)
                        config_keys.append({"name": name, "description": desc})
            # fallback: string literals at get/cfg call sites, undocumented.
            for key in GET_CALL.findall(text):
                if key not in seen:
                    seen.add(key)
                    config_keys.append({"name": key, "description": ""})

    wasm = RELEASE / (d.name.replace("-", "_") + ".wasm")
    size = sha = None
    if wasm.is_file():
        data = wasm.read_bytes()
        size = len(data)
        sha = hashlib.sha256(data).hexdigest()[:12]

    return {
        "name": d.name,
        "package": pkg.group(1).strip() if pkg else "",
        "description": first_doc_line(lib),
        "exports": exports,
        "capability_deps": deps,
        "config_keys": config_keys,
        "wasm_size_bytes": size,
        "wasm_sha256_12": sha,
        "reusable_as_is": not is_app(d.name),
        # A component whose exports all return an `UNIMPLEMENTED:` marker is a
        # CONTRACT, not a capability, and the catalogue has to say so. Detected
        # from the source rather than kept in a list here, because a list is a
        # second place to forget: implement the thing and the flag clears itself.
        "unimplemented": "UNIMPLEMENTED:" in lib.read_text(encoding="utf-8", errors="ignore")
        if lib.exists()
        else False,
    }


def dep_badge(deps: list[str]) -> str:
    if not deps:
        return "pure compute"
    return ", ".join(d.replace("wasi:keyvalue/", "kv:").replace("wasi:", "") for d in deps)


def main() -> None:
    entries = []
    for d in sorted(COMPONENTS.iterdir()):
        if d.is_dir() and not d.name.startswith("."):
            e = scan_component(d)
            if e:
                entries.append(e)

    (COMPONENTS / "catalog.json").write_text(json.dumps(entries, indent=2) + "\n")

    lines = [
        "# Component catalog",
        "",
        "Generated by `tools/gen-catalog.py` — do not edit by hand.",
        "",
        "Every component exports a WIT interface and depends only on generic WASI",
        "capabilities, so anything marked reusable drops into another app via",
        "`wac plug` or a wasmCloud link, configured through `wasi:config` knobs.",
        "",
        "",
        "`contract only` means the WIT is real and there is NO implementation "
        "behind it: every export returns an `UNIMPLEMENTED:` marker. Those need a "
        "host-side capability a wasm component cannot have (a syscall, a socket, "
        "a subprocess), so they state what a host must satisfy rather than "
        "satisfying it.",
        "",
        "| component | package | deps | config knobs | size | reusable as-is |",
        "|---|---|---|---|--:|:--:|",
    ]
    for e in entries:
        size = f"{e['wasm_size_bytes'] // 1024} KiB" if e["wasm_size_bytes"] else "—"
        knobs = ", ".join(f"`{k['name']}`" for k in e["config_keys"]) or "—"
        lines.append(
            f"| **{e['name']}** | `{e['package']}` | {dep_badge(e['capability_deps'])} "
            f"| {knobs} | {size} | "
            f"{'contract only' if e['unimplemented'] else ('✓' if e['reusable_as_is'] else 'app/demo')} |"
        )
    lines += ["", "## Descriptions", ""]
    for e in entries:
        if e["description"]:
            lines.append(f"- **{e['name']}** — {e['description']}")
    lines.append("")
    (COMPONENTS / "CATALOG.md").write_text("\n".join(lines))
    print(f"{len(entries)} components -> components/CATALOG.md + catalog.json")


if __name__ == "__main__":
    sys.exit(main())
