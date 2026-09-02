# Plugin support for Nestwatch — the design space, analyzed

Raised three times, and worth a real answer rather than a reflex. The question is
whether StudyGo (and later a chores app, a reading log) should be a *plugin* you
install and enable in Nestwatch, which then gathers its data and applies rules.

The short finding: **"plugin" is four different architectures, not one.** An earlier
note rejected exactly one of them — native code loaded into the SYSTEM service — and
was right to. This maps all four against the two constraints that actually decide it,
and lands on a plugin *system* that is safe, real, and mostly already built.

---

## The two constraints every option is judged against

Both are load-bearing promises of this project, stated in its own documents.

**C1 — no foreign code runs as SYSTEM from a source the child can influence.**
`docs/SECURITY.md` names the child as the primary adversary and refuses the
auto-updater on exactly this ground: *"a path that writes an executable and runs it
as SYSTEM… local privilege-escalation flaws worded as an authorised local attacker
elevates privileges, which describes the child on this machine."*

**C2 — the monitored PC makes no outbound connection.**
`docs/REMOTE-ACCESS.md`: *"do not give Nestwatch a way out."* A version check *in the
service* was refused because it would reveal *"the household's address and roughly when
that PC is awake."* Fetching StudyGo continuously from the PC is that, amplified, plus
the child's StudyGo credentials on the child's own machine.

---

## The four architectures

### 1. Native dynamic loading (`.dll`/`.so` the service `dlopen`s)

The thing the earlier note rejected. **Fails C1 outright.** Rust has no stable ABI —
the research is blunt that *"the ABI may break between compiler versions, but also
between compiler runs"* — so a native plugin is `unsafe` FFI where a plugin panic
takes the enforcement service down with it. Loaded by a SYSTEM process from a plugin
directory, and a child who can write that directory executes as SYSTEM. No sandbox,
no recovery. **Rejected, and nothing here revisits that.**

### 2. WASM sandbox (embed `wasmtime`; plugins are `.wasm`)

The option the earlier note missed, and the reason this document exists. Modern
practice for running untrusted code in-process: a WebAssembly module *"starts with no
access to the outside world and can only perform operations the host explicitly
grants"* — capability-based, deny-by-default, enforced at the runtime boundary. A
plugin compiled to WASM **cannot make a syscall the host did not hand it**, even inside
a SYSTEM process.

- **C1: solved.** This is the genuine finding. A sandboxed plugin that a child
  overwrites is still a sandboxed computation — no filesystem, no exec, no escalation,
  only the host functions granted. `wasmtime` adds fuel metering and memory caps, so
  even a plugin that loops or allocates is bounded. My earlier flat "no to plugins"
  did not account for this, and that was too broad.

- **C2: *not* solved, and this is the crux.** A StudyGo plugin's whole job is to
  *fetch StudyGo*. That needs a network capability. Grant it and the monitored PC
  dials out — C2 broken, and the sandbox is irrelevant because the capability *is* the
  dangerous one. Withhold it and the plugin cannot fetch, so it is not a fetcher. The
  sandbox governs what code may *do*; it is orthogonal to the promise about network
  egress. WASM answers "can I run untrusted logic safely" — not "may this machine talk
  to the internet."

- **Cost, against this project's ethos.** `wasmtime` embeds Cranelift, a JIT that
  generates and executes machine code at runtime — inside the SYSTEM service. That is a
  large new dependency tree (the crate that gates every dep, `cargo-deny`, would audit
  all of it) and a rich exploit surface (JIT bugs are a classic RCE vector) added to a
  process that today runs no codegen at all. `DECLINED-OPTIONS.md` rejected DuckDB in
  part for a 10 MB source footprint against a 3.79 MiB binary; `wasmtime` is heavier
  still. For a tool whose supply-chain section pins every CI action to a commit hash,
  adding a runtime code generator to the SYSTEM service is a hard sell — to solve a
  problem (C1) that the option below solves with no code at all.

### 3. Out-of-process plugin on the PC (a sidecar the service talks to over a pipe)

The plugin is a separate process at its *own* privilege, so C1 is satisfied without a
sandbox — a crash or compromise is contained to a non-SYSTEM process. But a StudyGo
sidecar still **fetches StudyGo from the child's PC (fails C2)** and still needs the
family's StudyGo credentials on that machine. Same wall as options 1–2's network half,
with none of WASM's containment benefit. Declined.

### 4. Declarative provider (a plugin is a manifest + rules, not code)

A plugin is *data*: a name, the signal it ingests, the rule that turns that signal into
an action, an enable/disable toggle, and its config. No code of the plugin's runs in
the service at all. This is how extensible monitoring systems stay safe — Prometheus
integrations are separate exporters the core never executes; Grafana data sources are
declared, not `dlopen`'d.

- **C1: not applicable** — there is no foreign code to run.
- **C2: satisfied** — the *fetching* happens off the PC, on the device that already
  holds the credentials and the network right (the parent's phone/Mac), which *pushes*
  the signal in over the authenticated LAN API. The PC still never dials out.
- **Cost: near zero** — a config section and a registry, no new runtime, no new
  dependency, no new attack surface.

---

## The synthesis: a provider registry, with StudyGo as the first provider

The user's actual request — *"install StudyGo as a plugin, enable it, it gathers usage
and applies rules"* — decomposes into two separable things:

- **(A) a plugin *system*:** Nestwatch gains a first-class notion of pluggable
  providers you install, enable, and configure. This is desirable and safe.
- **(B) the gathering:** someone fetches StudyGo. The only safe place for that is
  off the monitored PC.

Architecture 4 delivers (A) honestly and puts (B) where C2 requires. Concretely, a
`providers` concept in Nestwatch:

```
providers:
  studygo:
    enabled: true
    grant: { at_least: <signal>, minutes: 30, source: "studygo" }
```

- The parent enables *StudyGo* in an **Integrations** panel on the dashboard — the
  plugin-install experience, minus the code.
- The provider ingests through the grant endpoint that **already exists and is already
  built** (`POST /api/extra-time` with a `source`, the day-latch, and the
  `Idempotency-Key` replay — shipped on `earned-time-grants`). That endpoint is the
  provider registry's ingest point; what is missing is only the registry around it:
  the enable/disable, the per-provider config, and the Integrations surface.
- StudyGo is provider #1. A chores app or a reading log is provider #2 with a new name
  and no server change — which is exactly the modularity a plugin system is *for*.

**Where WASM comes back, honestly:** if Nestwatch ever wants to run *community-authored*
provider logic — a household writing its own "given these raw events, is the bar met?"
rule — architecture 2 is the right way to run that rule safely, as sandboxed pure
computation with **no network capability granted** (the phone already delivered the
data, so the rule only evaluates). That is a real future, and it is the documented
upgrade path. It is not warranted now: it adds a JIT to the SYSTEM service to run a
rule that today is three lines of Rust, and it still does not let the *fetch* run on the
PC. Reach for it when there is a third-party rule to run, not before.

---

## Recommendation — **built, 2026-09-02**

Architecture **4** shipped: `Config::providers`, `GET/POST /api/providers`, and an
Integrations card listing each installed provider with an on/off toggle and its reward.
StudyGo is provider #1, pushing from Voortgang over the authenticated LAN API.

One thing the build added that this analysis did not call for, and it is the security
half worth recording: **the reward moved to this machine.** A push names its provider and
asserts its threshold was met; the minutes come from that provider's config here. So a
phone that is lost, spoofed, or simply buggy cannot choose its own reward — verified live,
a push claiming 999 minutes granted the configured 25. The original design had the client
send the number, which would have made the phone's integrity load-bearing for a limit the
parent set.

The recommendation as originally written follows.

Build architecture **4**: promote the shipped grant endpoint into a real provider
registry (enable/disable + per-provider config + an Integrations dashboard panel), with
StudyGo as the first provider. It is the plugin system the request asks for, it breaks
neither C1 nor C2, and most of it exists. Record architecture **2 (WASM)** as the
sanctioned path *if and when* third-party provider logic is ever wanted, with the
explicit note that it solves code-safety and not network egress. Leave **1** and **3**
rejected.
