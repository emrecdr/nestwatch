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
asserts its threshold was met; the minutes come from that provider's config here — verified
live, a push claiming 999 minutes granted the configured 25. The original design had the
client send the number, which would have made the phone's integrity load-bearing for a limit
the parent set.

**This paragraph used to end "so a phone that is lost, spoofed, or simply buggy cannot choose
its own reward", and that was wrong.** It is true of the *push*: the client asserts a threshold
and the minutes come from this machine's config. It was not true of the *phone*, because the
phone did not have to push. Pairing minted an ordinary session, so the client holding the
pairing cookie could reconfigure the provider it was governed by, or grant directly as
`source=parent`, skipping the registry and the day latch together. Measured before the fix:
five such requests granted 1200 minutes.

**The sentence is true again today — but only for one of the two QR codes.** An
`--integration` pairing cannot reconfigure its provider or grant as anyone else, so a lost
phone holding one really cannot choose its own reward. A plain dashboard pairing still carries
everything the parent has, and for it the sentence remains as wrong as it was. Which credential
a client holds is therefore the whole of the difference; the mechanism is in
`docs/SECURITY.md`.

**The analysis above is the reason the gap existed, and it is worth naming precisely.** Every
architecture here was weighed on *what a provider runs* — syscalls, egress, in-process
memory safety. That was the right question for choosing between WASM and data, and
architecture 4 answers it completely: a provider runs nothing. But *what a provider is
authenticated as* was never asked, so nothing in this document notices that the declarative
design removed the code-execution risk and left authority untouched. A registry with careful
bounds reads as though it had answered both.

**Fixed, and the answer was a scoped credential rather than a sandbox.** A pairing token now
records what it is for when it is minted; an integration's token reaches two routes and grants
under its own name. Note what that does *not* need: no WASM, no sidecar, no process boundary. The
risk was never that a provider might run something — architecture 4 was right about that — it was
that a provider was authenticated as the parent. Bounding the credential closed it, which is why
the original analysis reached a sound conclusion from an incomplete question.

## The third question, asked 2026-09-06 — `O92`

Twice now this document has answered a question completely and left the neighbouring one unasked,
and the pattern is the useful part rather than either defect.

The architecture comparison asked what a provider *runs*, and architecture 4 answers it entirely: a
provider runs nothing. The section above records the second gap — nothing asked what a provider is
*authenticated as* — and scoped pairings closed it. **Neither asked how long that authentication
outlives the provider.** `delete_provider` dropped the registry entry; `require_auth` read the
session's scope and never consulted the registry; so uninstalling StudyGo from the Integrations
card refused its next grant and left the paired device reading `GET /api/usage/today` — today's
budget, per-app usage and up to `MAX_PAGES` window titles — until the absolute session cap expired
up to thirty days later. Disabling it did the same. Measured against a live router before it was
fixed, not argued.

**The registry now bounds both routes, and uninstalling revokes.** `require_auth` asks the registry
whether an integration's provider is installed and enabled, so the read is governed by the same
switch the grant always was; `delete_provider` ends every session that pairing minted, scoped to
that one source. The parent's own session and any other integration are untouched.

**The split between the two came from prior art rather than from taste**, which is worth recording
because the tempting design — revoke on both — is wrong. GitHub Apps separate *suspend* from
*uninstall*: suspension is offered as the alternative to uninstalling, *"which has the consequence
of deauthorizing every user"*, and a suspended app still *"cannot access the GitHub API or webhook
events."* Both halves matter. Suspension keeps the credential **and** closes the door; only
uninstall destroys it. A toggle that forced a re-pair every time would make *off* expensive enough
that a parent uses *Remove* instead, which is the more destructive control — the same argument
`O77` makes about `change_password` being too expensive a revocation to actually perform.

**The refusal is `400`, and that is a cross-repo contract rather than an HTTP preference.**
Voortgang reads `400` as *"the integration is not switched on over there"* and `401`/`403` as
*"re-pair this app"*. A disabled provider needs the first sentence, because the link is perfectly
good. `403` would have sent a parent to re-pair something that was never broken — a refusal that is
correct HTTP and wrong advice.

**What this says about the analysis above, which is still sound.** Every architecture here was
judged on capability: syscalls, egress, in-process memory safety. That was the right axis for
choosing between WASM and data. But capability is what a credential *may do*, and a lifecycle is
*for how long* — and a document that answers the first thoroughly reads as though it had answered
the second. Three passes over the same design, each complete on its own axis, and the gap sat in
the space between them each time. The general form: **a provider is a registry entry plus a
credential bound to it.** Anything that treats those as two objects sharing a string will leak in
whichever direction was not the subject of the last review.

## The fourth question, asked 2026-09-06 — `F2`, `F3`, `F6`

The section above ends by naming the pattern: each pass answered one axis completely and read as
though it had answered the neighbour. Having named it, the next pass went looking for the
neighbour deliberately, and there were three.

**`F2` — reaching a route and knowing everything it says are separate grants, and only the first
was decided.** `integration_may_reach` admits `GET /api/usage/today` for one stated reason: an
integration pushes a grant and reads it back, because it refuses to tell a parent a number this PC
does not show (`O85`). The route it was handed to do that answers the dashboard's whole day —
seventeen fields, including per-app minutes and up to `MAX_PAGES` window titles. So the allowlist
was route-scoped where the justification was field-scoped, and every provider that ever reaches
this route inherits the wider answer. It now returns `auth::INTEGRATION_USAGE_FIELDS`, which is
`extra_mins`. **`Scope::Dashboard` is untouched**: the same route is the browser's and the Android
client's, and narrowing a shared route for one caller would break a full dashboard to bound an
integration.

That the consumer reads exactly one field was checked, not assumed — its contract test is named
"one field out of fourteen" and derives what it depends on from its own source, and its maintainer
ran the client's parser against both the narrowed and the full body. Worth stating because the
first draft of this analysis asserted a *different* consumer fact that turned out to be false: it
claimed the field would replace error-string parsing, when that client deliberately does not read
those strings — the remedy for both refusals is identical, and matching on prose breaks the moment
prose is reworded or translated. The claim was withdrawn on reading their source.

**`F3` — the fix for `F9` created the state it needed to report.** A *disabled* provider still
answers `/session` with `authenticated: true`, deliberately, because a switched-off integration is
not a bad link. But every grant under that session is then refused, so a pairing screen could say
"linked and working" while nothing a child earned ever landed. Authority and installation are two
facts and only one was being reported. `/session` now carries the entry the credential is bound to.
Its `minutes` half closes a smaller gap in the same seam: the reward lives in this registry and the
bar the child clears lives in the provider's own app, so until now neither side could state the
whole rule.

**`F6` — the registry and its credentials were rendered in two cards that did not know about each
other.** *Integrations* showed the entry, *Signed-in devices* showed the credential, and
"StudyGo: on, 25 min, paired to one phone" was a sentence the page contained and never said. The
join needed no new endpoint and no new data. It did need one honest distinction: the device list is
fetched lazily, so a summary computed from an empty array would report "not paired to any device"
about a perfectly good pairing merely because that card had not been opened. Absent is not empty —
the same distinction `remaining_mins: null` exists to preserve on the server.

## `F7`, and a constraint nobody chose — 2026-09-06

The report that produced `F2`, `F3` and `F6` marked `F7` **decide first**, and researching it
before building found that the thing being decided was not what the report thought.

**The claim:** installing a provider is remote, but pairing one needs a console on the child's PC,
because `pairing::mint` has one caller and `print_pairing` calls `ensure_elevated` first. True, and
it reads as a deliberate security posture — *creating a credential requires physical access to the
machine*. Three documents can be written from that sentence, and one of them was.

**What the code says.** The comment above that call gives the reason: minting writes into the
ACL-locked data dir, so the CLI needs elevation the way `install` does. The service runs as SYSTEM
and can write that file whenever it likes. There was no standing decision that credential creation
requires physical presence — there was a user-mode process that could not reach a file, and a
security posture assembled afterwards from the shape of that limitation.

**This is worth recording as a class, because it is the fourth variant of this document's own
pattern.** The earlier three were questions left unasked next to questions answered. This one is
different and harder to catch: a constraint arrives as an implementation detail, is observed to
have a security-shaped effect, and is then reasoned about as though someone had chosen it. Nobody
lied and no comment was wrong — the inference simply ran the wrong way, from mechanism to intent.
The tell is that the justification appears only in prose *about* the code and never at the
mechanism itself.

**What the real decision turned out to be**, once that was cleared away: `pairing`'s module doc
states an exposure model — the token is printed on a screen in the child's house, so the window is
"while the parent is standing at the machine, and it closes the instant they scan". Minting from
the dashboard trades that for an authentication model. Two candidate mitigations did not survive
contact with the source:

* *Add a short TTL.* Already there — `TTL_SECS` is fifteen minutes and tokens are single-use.
* *Allow minting only from the LAN.* No such distinction exists. `security::require_lan_peer` is a
  layer on the outer router, so every route is already LAN-gated, and `docs/REMOTE-ACCESS.md` is
  explicit that remote access works by deciding **where the tunnel terminates** — inside the LAN. A
  remote parent's request is local by construction.

So the bound is step-up authentication, which is what GitHub requires before creating a token, and
it is the bound that matches the actual risk: the minted credential is weaker *per request* than
the session that asked for it, and the thing that is genuinely new is its **durability**. It also
only became safe once `O92` gave the card revocation and `F6` gave it the list of what is paired —
mint, see, and end now live on one surface, which is the argument architecture 4 was chosen for.

**The shape all four share, stated once.** A provider is a registry entry plus a credential bound
to it. Every finding in this document is what happens when those are two objects sharing a string:
the entry outliving the credential (`O89`), the credential outliving the entry (`O92`), the
credential learning more than the entry justified (`F2`), the entry being invisible to the
credential (`F3`), the two never shown together (`F6`), and the entry being creatable only from a
place the credential's owner could not stand (`F7`).

---

The recommendation as originally written follows.

Build architecture **4**: promote the shipped grant endpoint into a real provider
registry (enable/disable + per-provider config + an Integrations dashboard panel), with
StudyGo as the first provider. It is the plugin system the request asks for, it breaks
neither C1 nor C2, and most of it exists. Record architecture **2 (WASM)** as the
sanctioned path *if and when* third-party provider logic is ever wanted, with the
explicit note that it solves code-safety and not network egress. Leave **1** and **3**
rejected.

---

## Review of the shipped registry — 2026-09-02

A second pass over `83f0ce3` and `a258b26`, asked for by the session that built them.
The registry holds. What follows is what changed as a result, and one thing that was
asked for and deliberately *not* done.

### Fixed

- **The registry was unbounded, and had no way to remove anything.** Both, together:
  `MAX_PROVIDERS = 12` plus `POST /api/providers/{name}/delete`. These are one change,
  not two. A cap with no delete is a trap — the twelfth install would be permanent — and
  a delete with no cap leaves the file growable. Reconfiguring a provider that already
  exists is never capped, or a full registry could not be switched off.
- **Removing a provider deliberately leaves `config.earned` alone.** Clearing it would
  make delete-then-reinstall a two-request bypass of the once-per-source-per-day latch,
  available to exactly the caller who would want the second grant. This is now the
  property a test pins rather than an accident of what `remove` happens to touch.
- **`Idempotency-Key` was stored bare.** Two providers picking the same key — and a date
  string is the obvious pick — collided, and the loser was handed the winner's response,
  granted nothing, and reported success. Keys are now namespaced by `source`. A key
  reused across two genuinely different grants is refused rather than replayed.
- **`ExtraTimeBody.minutes` was required and ignored.** It is now `Option<u32>`, required
  only for a parent grant. `POST /api/curfew/extend` got its own body type; while the two
  shared one it accepted a `source` field and silently discarded it.

### Not done, on purpose: a golden file for `/api/providers`

Asked for on the grounds that it is "part of the client contract nestwatch-mobile's CI
checks". It is not, and adding it would **break** that repo.

`nestwatch-mobile/tool/check_golden.sh` loops over `nestwatch/tests/golden/*.json` and
reports `MISSING HERE` — counting it as drift — for every file the phone repo does not
also carry. The Android client never calls `/api/providers`; its paths are `/api/events`,
`/api/time-requests`, `/api/time-codes`, `/api/usage/today` and `/api/screenshot`. So the
golden file would guarantee a drift failure over there, clearable only by vendoring a
fixture that repo has no parser for. `tests/golden.rs` says what belongs in it in its
first line: *every JSON shape the Android client parses.* This is not one.

**The real gap that question points at** was `/api/extra-time`'s *response*, which
Voortgang parses (`ok`, `reason`, `minutes`) and nothing pinned. **Now pinned**, in
`earned_grant.rs` rather than `tests/golden/`: the exact key set of both 200 bodies, plus
the rule that `ok` and `reason` cannot disagree. A fixture could not go in `tests/golden/`
for the reason given above — that directory is a contract with one specific repo, whose
checker counts an unrecognised file as drift — and where shared fixtures should live is
still open as `O86`, needing a decision in both repositories rather than one.

## A ceiling instead of a latch — 2026-09-08

The registry above grants **once per source per day**, and that rule was right for the only client
it had: Voortgang pushes when a parent opens it, so a second push in a day was a retry rather than
a second earning. Designing a *gate* — practice governs PC time, checked repeatedly through an
afternoon — asked the neighbouring question this document keeps failing to ask on the first pass,
and this time it was asked before anything shipped: **a signal that arrives in pieces cannot be
credited by a rule that fires once.**

`Provider::daily_cap_mins` is the answer, and it is deliberately the smaller change of the two
available. The latch is not removed; it is what a provider still gets when the field is absent,
which is every provider that exists today. Set the field and the same source may push until the
ceiling is reached.

**The bound got stronger, not weaker, which is the part worth stating plainly.** A latch bounds a
compromised or buggy client to *one reward*; a ceiling bounds it to *a number the parent set*.
Neither trusts the push — the reward has come from this machine since `83f0ce3` — so the ceiling
inherits that property and adds an explicit total to it. `used + minutes <= cap` holds by
construction on both arms that grant, so no sequence of pushes can pass the ceiling.

**Nothing changes for an installation that does not opt in, and that is enforced rather than
intended.** `daily_cap_mins` is `skip_serializing_if`, and `EarnedDay` — the type that replaced the
bare `NaiveDate` in `Config::earned` — has hand-written serde impls that write the *old* spelling
whenever no amount is tracked. So a household with no ceiling keeps a byte-identical `config.json`
and a byte-identical `GET /api/providers`, and its file stays readable by an older build.
`config::tests::nothing_written_changes_shape_until_a_ceiling_is_set` is that promise as an
assertion, and it was confirmed to bite by deleting the `skip_serializing_if` and watching it fail.

`#[serde(untagged)]` carries a documented hazard — two variants sharing a shape, where the first
that parses silently wins. These two cannot collide, one being a JSON string and the other an
object, but "cannot" is the kind of claim this project has been wrong about before, so
`serde_reads_both_spellings` measures it.

**One decision was made against the industry default, and the reasoning matters more than the
choice.** When a push would carry a provider past its ceiling, the grant is *clamped to the
remainder* rather than refused. Quota guidance is near-unanimous that an over-quota request should
be rejected — but that guidance is about a request that asked for something, and a provider never
asks: `ExtraTimeBody.minutes` is vestigial on the robot path and is not read. The client asserts a
threshold was met; this machine answers what that is worth today. Refusing would take a child's
completed work and pay nothing for it, which is the failure the feature exists to prevent.

The refusal reason splits accordingly: `already_granted_today` keeps its exact wording for the
latch, and `daily_cap_reached` is new and reachable only where a ceiling is configured. The body's
*key set* is unchanged, which is what `earned_grant.rs` pins and what Voortgang parses — and that
client treats every refusal identically, so a value it has not seen costs it nothing. The split is
not decoration: quota guidance is explicit that a client should not retry a spent daily allowance
until midnight, and a poller cannot implement that against a single undifferentiated refusal.

**A fifth instance of this document's own pattern, caught before it shipped rather than after.**
The four above are each a question left unasked beside a question answered. This one was found by
deliberately asking it: *who else writes a provider entry?* The answer is the dashboard, and
`assets/app.js::loadProviders` rebuilds every integration row as `{name, enabled, minutes}` and
posts that row back on any change. A replace-the-entry upsert would therefore have erased a ceiling
the first time a parent used the on/off toggle — a setting destroyed by the one client that has
never heard of it, silently, with every test green.

So `ProviderBody::daily_cap_mins` carries three states rather than two: **absent means unchanged**,
`null` clears, a number sets. That is not a new principle here —
`config::tests::a_newer_configs_unknown_settings_survive_a_load_and_save` already keeps settings
this build has no field for, via the `#[serde(flatten)]` capture map. The same rule, moved from the
file to the writer. Any field added to this endpoint after today inherits the protection.

The mechanism has one trap worth recording, because it is invisible and the type looks right without
it: `Option<Option<T>>` alone does **not** distinguish absent from null. Serde maps a JSON `null`
onto the *outer* `None`, which is exactly what an absent field produces, so the two states collapse
and the distinction is lost with no error anywhere. `absent_or_null` forces the inner option to take
the null. It is four lines rather than the `serde_with` dependency that exists to solve this.

## Facts instead of a verdict — 2026-09-08

The ceiling above made repeated grants possible, which is what a ladder needs; this is the ladder,
and the two shipped together because separating them would have meant designing the wrong thing
first. A tier scheme built before facts would have had the *client* assert which rung it reached —
a verdict — and that mechanism becomes dead weight the moment the push carries the work instead.

`Provider::tiers` is a list of `{questions, minutes_practised, reward_mins}`, and
`ExtraTimeBody::progress` is what the child actually did. The push says the work; this machine says
what it is worth.

**This finishes a move `83f0ce3` started and left half-done.** That commit took the *reward* off the
push, on the argument that a compromised client must not choose its own number — and it was right,
but it left the *threshold* in the client, so the bar lived over there and the reward lived here and
neither side could state the whole rule. `F3` recorded the visible symptom (a pairing screen could
say "linked and working" while nothing a child earned ever landed) and fixed the reporting. This
fixes the split itself. A parent can now move the bar from this machine without anyone shipping a
new client, which is what a registry entry being *data* was supposed to mean.

**It buys no trust, and the doc should not imply otherwise.** A client willing to inflate
`progress` was already willing to push when it had earned nothing; reporting numbers rather than a
conclusion changes who holds the rule, not who is believed. What bounds a lying client is
`daily_cap_mins`, exactly as before.

**Three design points that are one comparison away from being wrong.**

*Either condition carries a tier*, not both — *fifteen questions or half an hour*. A child who works
slowly and carefully reaches it on minutes, one who works quickly reaches it on questions, and
requiring both would penalise each for the way they work. That is the same objection that keeps
accuracy out of this calculation, which the reward side had already settled: gating on it "punishes
struggling and rewards picking easy topics".

*A zero threshold states no condition*, rather than one that is trivially satisfied. `questions >= 0`
holds on an empty day, so the naive reading pays out a half-filled tier for doing nothing — the most
expensive possible meaning for a field somebody left blank. A tier asking for neither is refused at
the door rather than stored, because it reads like a rule and behaves like a blank.

*The best matching tier wins, not the first*, so the answer does not depend on the order a parent
happened to type them in.

**A refusal that is not an error.** Reported work meeting no tier answers `200` with
`below_threshold`, not `400`. The whole point is a client that pushes what it sees and lets this
side judge; making "not yet" an error would send it straight back to pre-judging, which is the
arrangement this change exists to end. The body's key set is unchanged, so the cross-repo contract
holds — and a client that wants to behave well can now tell a refusal worth retrying after more
practice from one that stands until midnight.

**The two features compose into the rule a household actually states.** Set the ceiling to the top
rung's reward and a day totals *exactly the best tier the child reached*, however many pushes it
took: clear the lower rung for 16, clear the upper one later, and the second grant pays the 14-minute
difference rather than another full reward. That property is pinned in `earned_grant.rs` rather than
left as an observation, because it is the whole of why these two changes belong in one increment.

**What this still does not do.** No gate exists to use any of it, and no UI sets either field — a
ceiling and a ladder are reachable only through the API today. This is the registry being made
capable of a gate from the side that can move without the other repository agreeing to anything.

## A probe, and the machine's first outbound request — 2026-09-09

The registry could judge work by 2026-09-08 and nothing on the PC could gather it: every grant still
arrived from the phone, and a phone in a pocket cannot ask every fifteen minutes. This is the half of
the gate that asks — Steps 4 and 5 of the plan — and it is the first thing this project has built that
makes the monitored PC contact anyone, so the constraint it bends is stated first.

**C2 is relaxed, opt-in, and never for the service.** A provider may name a `Probe`: a bare file name
in the program directory and an interval. Once a minute `probe::run_scheduler` asks which probes are
due; for each, the service launches the file **as the child** (`SystemControl::run_probe`, on Windows
through the same `CreateProcessAsUserW` path as the screenshot helper), hands it the provider's
deposited secret on stdin, reads back `{"questions": N, "minutes": M}`, and passes the two numbers to
`Config::earn` — the same judge a push from the phone meets. The request to the third party is made
by that process, under the child's own account, with the child's own session. The service still
contacts nothing, `src/` still contains no HTTP client, and a config that names no probe costs one
config read a minute and no controller call. `SECURITY.md` says the same in its outbound section and
`README.md` in its no-outbound sentence, because a promise relaxed only in the design document is a
promise broken everywhere else it is read.

**C1 holds, and the ACL is what holds it.** The probe is a *file name*, never a path, and
`Probe::validate` refuses anything else — no separator of either platform, no drive letter, no leading
dot. It is resolved inside the program directory, which `install::harden_program_dir` locks to SYSTEM
and Administrators with Users read-and-execute: the child can run it and cannot replace it. A path
would have let a parent point at the desktop, where he can. Nothing the probe prints is believed beyond
two integers, and those go through the ceiling and the ladder exactly as a push does, so the worst a
defeated probe buys is what a lying phone already could — bounded by `daily_cap_mins` — and the
designed outcome of every other fault (no Wi-Fi, an expired session, a killed process, a probe that
hangs) is *the base budget*, which is also what not practising earns.

**The secret has its own file, its own route, and its own scope rule.**
`POST /api/providers/{name}/secret` deposits the session; `probe::store_secret` writes it under
`secrets/` in the ACL-locked data dir, never into `config.json`, because that file leaves the machine
through `/api/policy` and `/api/export`. The integration allowlist gained its third route and it is
scoped to the caller's own name — `integration_may_reach` compares the path segment to the scope's
`source`, so a pairing minted for one provider cannot overwrite another's credential, and nothing an
integration can reach reads a secret back. Uninstalling forgets it, the way uninstalling already
revokes the pairing.

**Bounds, because the writer runs as the child.** `control::PROBE_TIMEOUT` kills a probe after a
minute and `control::MAX_PROBE_OUTPUT` refuses more than 4 KiB; a probe that exits non-zero has said
nothing this side may use. One defect the tests caught before it shipped is worth recording: killing a
hung shell script left its `sleep` holding the pipe, and the runner's join then waited the full thirty
seconds past a timeout of a third of one. Both runners now wait on the *output* for what is left of the
timeout rather than on the reader thread, which is what `a_probe_that_outruns_the_timeout_is_killed`
pins. Mutation testing over the finished
diff and over `probe.rs` then reported misses in four shapes, and only the first was the shape
expected.

*A limit tested from one side.* `secret.len() > MAX_SECRET_BYTES`, `output.len() > max_output` and
the fake's `calls.len() < PROBE_LOG_CAP` were each satisfied equally well by `>=`, because every
test asked what happens past the limit and none asked what happens *at* it. The same lesson
`MAX_TIERS` taught the day before.

*A constant whose value nothing depended on.* `MAX_SECRET_BYTES` is `8 * 1024`, and turning that
`*` into a `+` — 1032 bytes — passed everything, because every test spells the limit by name, which
is correct of them. What that mutant breaks is not a test but the feature: a StudyGo session is a
JWT of a kilobyte or two, so the limit would have refused every real token while every bound test
stayed green. The fix is a test that stores a 2 KiB token, which is an assertion about the thing
being held rather than about the number.

*An assertion that computed both sides from the code under test.* Replacing `probe_dir()`'s whole
body with an empty path was missed, because the only test that used it compared
`probe_dir().join(exe)` against `probe_dir().join(exe)` — which agrees with itself however wrong it
is. This is the one of the four that is a security property: an empty directory resolves the probe
against the service's working directory rather than the ACL-locked program directory, which is the
entire argument for accepting a bare file name in the first place. It is now asserted to be
absolute and to end in the right component.

*Three functions that never distinguished absent from unreachable.* `read_secret`,
`secret_deposited_at` and `delete_secret` each match `ErrorKind::NotFound` specifically; widening
that guard to every error, narrowing it to none, and inverting it were all missed. The API layer is
what hid the first two — `list_providers` reads the deposit time through `.ok().flatten()`, so an
error and an absence render identically. The costs differ, and the third is the one worth naming: a
probe running with an empty credential instead of recording a failed run is the silent-failure shape
the status line exists to prevent, while a deletion that could not happen reporting *there was
nothing to delete* would have `delete_provider` audit a surviving credential as one that was never
there — and destroying that credential is exactly what uninstalling promises. Both directions are
now asserted for all three, against a fault induced by putting a file where the `secrets` directory
belongs.

**What the parent sees.** `GET /api/providers` lists `secret_at` — when, never what — and the last
`probe_status`, only where they apply, so a household that opted into neither reads the same bytes it
always did. The dashboard folds the probe under *Check from this PC* beside the reward rules and
renders one translated line under the row: when it last checked, what it found, what that earned or
why nothing was, and how old the phone's session is. A dead link shows as a stale line rather than as
a quiet child.

**Step 1's answers, which shaped Step 3 without building it.** `dart compile exe` refuses the
Voortgang package outright — *'dart compile' does not support build hooks; packages with build hooks:
objective_c, sqlite3* — and the probe imports neither. The pure-Dart closure of fetch→parse→count is
sixteen files and one dependency (`http`); copied out unchanged it compiles in a second and prints
the same `{"questions":3,"minutes":4}` from the recorded fixture that the phone computes. So the probe
is a packaging question on that side (a hook-free core package), not a rewrite. And `--target-os`
reaches only Linux targets, so the Windows executable has to be built on Windows — a `windows-latest`
job, in practice. Whether StudyGo exposes work *in progress* (`topic.exercise_id` beside
`exercise_progress_percentage`) is still unanswered; it needs a live session, and it decides only how
good the signal is, not whether the mechanism works.

**What this still does not do.** The scheduler has no heartbeat (`O102`); a parent reads liveness
off each provider's own `probe_status.at`. And none of the Windows half has executed:
`session::run_probe_in_session` is compile- and lint-checked for the target and listed in
`WINDOWS-TESTING.md` §H8.

## The gate says something — 2026-09-09

`O101`, closed the same day it was filed. The probe granted and refused in silence, so the child's
time extended or did not with no explanation attached — which is the *controlling* frame the
research behind this design warns produces more screen time rather than less. Two sentences now
reach him, through `control::notify_child`, the same wrapper the screen-time enforcer uses.

**A grant is announced every time; a shortfall is mentioned once a day.** That asymmetry is the
whole design. Announcing every grant is what stops the feature being a thing that only ever says
*not yet*; rationing the reminder to one a day is what stops a fifteen-minute timer becoming a
fifteen-minute nag. Neither is a preference — a nagging tool and a purely negative one fail in the
same direction, which is the child ignoring it.

**It names the nearest rung, not the highest.** `Provider::next_rung` picks the cheapest tier not
yet met, chosen by reward rather than by position so the sentence does not depend on the order a
parent typed the ladder in — the same property `reward_for` holds on the paying side. Telling a
child standing at the bottom of a ladder about its top is the discouraging choice, and a message
that changes with entry order is an arbitrary one.

**A reminder the OS refused is not a reminder he got.** `notify_child` reports whether the message
was taken, and the day is recorded only when it was, so an undeliverable notice is retried at the
next check rather than counted as said. That distinction already exists one file over: `rules.rs`
checks the same return before recording a countdown warning, for the same reason — a warning that
silently never arrived looks identical in the log to one that did.

**Only `below_threshold` earns the reminder.** The other two refusals — the day latch and the
ceiling — mean the day is already paid, so there is no rung to aim at and nothing worth saying. A
failed *check* says nothing to him either: a broken link is the parent's to fix and it is already
on their card.

Both strings are built per language, like every other thing the child reads, and
`tests/translated_strings.rs` is what makes that structural rather than a habit.
