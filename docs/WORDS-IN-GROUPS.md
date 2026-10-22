# Words in app groups — design

An app group already shares one daily pool between several programs: "Games, 60 minutes" covering
`RobloxPlayerBeta.exe` and `Minecraft.exe`. This document adds **words** to a group. A program whose
name contains one of the words, or a browser page whose address or title contains one, draws from
the same pool. Once the pool is spent, those programs and the browser showing that page are closed,
the same way the listed apps are.

The case that asked for it: Roblox for at most one hour a day, **counted across the app and the
browser together**, without the parent having to know every executable Roblox ships. The app half
works today, if the parent lists the right names. The browser half does not: Roblox in a browser is
usually streamed through a cloud-gaming site, which a group of executables cannot see, and page
titles are recorded but only ever reported.

**Status: designed; built except reading the address bar.** Reading the address bar is gated on a
measurement on the target PC ([Phase 0](#phase-0--measure-before-building-the-address-half)). Until
it passes, pages match on their title only. Matching programs by owner is built, and confirmed on the
target PC by the same measurement.

---

## What the parent sets

Each group row on the dashboard gets one more field, **"Also count programs and web pages
containing"**, holding words separated by **commas**. Unlike the app list beside it, a space does
not separate: `Roblox Studio` stays one entry, which the server refuses ("can't contain spaces"),
rather than silently becoming `roblox` and a far broader `studio` that matches Visual Studio and
OBS Studio.

* `roblox` and `%roblox%` mean the same thing. `%` at either end is removed, since parents who know
  SQL patterns will type it.
* Case is ignored. `Roblox`, `ROBLOX` and `roblox` match the same names and pages, and Turkish
  dotted and dotless `i` count as plain `i`, so `izle` matches `İZLE` and `oyunları` matches
  `OYUNLARI` (`words::fold`).
* A trailing `.exe` is dropped, so `roblox.exe` copied from the apps field becomes `roblox`.
  Programs are matched on their name without `.exe`, and the word would otherwise match nothing.
* **Blank entries are dropped.** A blank word would match everything. This codebase was bitten
  exactly this way by blank app names, which became a kill-on-sight rule nobody wrote (see
  `Targets::from_rules`).
* A word must be **3 to 64 characters** after the `%` is removed, with no spaces, control characters
  or `%` inside it. What is stored is bounded too: at most 66 characters as typed, room for a `%` at
  each end. Anything else is refused with a message saying which rule it broke. Two
  characters would match far too much (`gg` is in `now.gg` and in every page that mentions eggs).
* A group may have words and no apps. `Roblox, 60 minutes` with only the word `roblox` is the
  intended shape. Listed apps still work alongside words, and are still needed for a program whose
  name lacks the word: the Microsoft Store build of Roblox runs as `Windows10Universal.exe` (`O16`).

---

## Programs

A running program belongs to the group when its name, without `.exe`, contains one of the words,
and **it is running under the signed-in user's own account**.

1. **The name is matched without `.exe`.** Otherwise the word `exe` would match every program.
   `roblox` matches `RobloxPlayerBeta.exe`, `RobloxPlayerLauncher.exe` and `RobloxStudioBeta.exe`.
2. **Only the signed-in user's own programs.** A word can only count or close what the child could
   close himself in Task Manager. The service runs as SYSTEM and sees every process on the machine;
   without this rule the word `win` (3 letters, so it passes the length rule) would match
   `wininit.exe` and `winlogon.exe`, and closing either stops Windows with a blue screen. `host`
   would match every `svchost.exe`. The processes Windows needs to keep running run under its own
   accounts, so no word reaches them, however broad.
3. **Never Windows' own programs that run under the child's account.** A few do — the shell, the
   Start menu, text input, per-user services — and `svchost.exe` always runs, so `host` would drain
   the pool all day with nothing being played and then close the child's session piece by piece.
   A built-in list of about thirty (`WINDOWS_PROGRAMS` in `src/words.rs`) is skipped by name. It is
   not complete and does not claim to be; the preview shows anything else a word reaches.
4. **Never Nestwatch's own programs.** The helper runs under the child's account as
   `host-health.exe`, which `host` and `health` would otherwise match. `host-health.exe` and
   `nestwatch.exe` are excluded by name, and so is every check program configured on the
   Integrations card: those run as the child too, and closing one mid-check would leave shut a gate
   that should have opened.
5. **Listed apps are unchanged.** An exact name in the app list still matches machine-wide, as it
   does today. The owner rule applies only to what a word matches.
6. **The dashboard previews what a word matches.** Under the field: "Running now with these words in
   the name: RobloxPlayerBeta.exe". A word that is too broad is visible before it closes anything.
   The server answers it with the enforcer's own rule on the real owner snapshot, so the preview
   lists exactly what the rule would close — no more and no less.

Nestwatch treats whoever is signed in at the console as the child, as screen time already does.
So if a parent signs in on that PC — fast user switching included — words act on the parent's
programs and pages for as long as that session is the active one. The dashboard says "the
signed-in Windows account" rather than "the child's" for that reason.

---

## What pages count

A page counts while **all** of the following hold:

1. A browser window is **in front**, and it is Edge, Chrome, Firefox or Brave: the four in the
   existing `BROWSERS` table.
2. Its **title**, or once Phase 0 passes its **address bar**, contains one of the group's words.
3. The screen is unlocked and in use. A locked screen charges nothing, as it does for apps today.

Examples with the word `roblox`:

| In front | Counts? |
|---|---|
| `roblox.com/games/...` | Yes: title, and address |
| A cloud-gaming page at `now.gg/apps/roblox-corporation/...` | Yes: address (title if it says Roblox) |
| A YouTube video titled "Roblox obby speedrun" | **Yes**: title |
| A Google search for "roblox" | **Yes**: title, and address |
| A Roblox tab open *behind* a homework tab | No: only the tab in front is read |
| A second browser window with Roblox, behind Word | No: only the window in front is read |
| Opera or Vivaldi showing Roblox | No: not in `BROWSERS` (see [Evasions](#evasions)) |

**No double counting.** A program running and a matching page in front can both be true in the same
30-second check. The pool is charged once:

* if a listed app, or a program matching a word, is running, the full check (as today);
* otherwise, the seconds a matching page was in front, never more than the check's length.

---

## When the pool is spent

On each check after the pool reaches its limit:

* listed apps are closed on sight, as today;
* programs matching a word, under the child's account, are closed on sight;
* **every process, under the child's account, of a browser that reported a matching page in this
  check** is closed. The whole browser, not one tab, which is what the parent chose.

A page that comes to front is reported within one helper report (30 s) and acted on at the next
service check (30 s), so a matching page is closed **within about a minute**.

The child sees the existing group notice, which already names the group first:
"Roblox has used its 60 minutes for today, so msedge.exe was closed." No new strings are needed for
the notice. It is announced once per program per day, like every other close.

---

## What is reported, and what is never kept

The Today card's group row gains the web share: **"Roblox: 45 / 60 min · 20 min on the web"**. Web
minutes mean the time the pool was charged *because of* a page, which is the checks where no
matching program was running. So program time and web time add up to the pool's total. Today only:
the daily history is not extended.

**Addresses are never kept.** An address exists in the helper's memory and in the service's memory
for one check, is matched, and is dropped. It is never written to `usage_state.json`, the daily
rollup, the audit log, a tracing log or an export, and no API serves it. Malformed helper lines are
already skipped without logging their contents (`session.rs`), and that stays so. Page titles are
recorded exactly as they are today.

`O20` describes reading the address bar as "a parent-facing decision, not an installer detail".
That decision is made here, by the parent who asked for it, with the full address rather than the
domain only: the domain alone (`now.gg`) misses the cloud-gaming case this feature exists for.

---

## How it works

### The helper

`watcher::observe` already reads the title once the foreground process is known to be a browser.
It now also produces a **visit**: the browser, the title, and the address. A third `Tracker` keys
seconds by visit, next to the existing ones for apps and page titles.

Until Phase 0 passes, the address is empty. When it passes, the helper reads it through **UI
Automation**, the interface screen readers use:

* Only for the foreground window, and only for a browser whose address reading passed Phase 0.
  Each entry in `BROWSERS` gains a flag for this, so a browser that failed stays titles-only.
* The address element is found once per window and cached, keyed by the window handle. It is
  searched for again only when the window changes or a read fails.
* UI Automation calls go out to the browser's process. Short connection and transaction timeouts
  (`IUIAutomation2`) mean a hung browser costs one reading, not the watcher. COM is initialised on
  the worker thread, never inside the `SetWinEventHook` callback.
* An address that cannot be read is sent as empty. The title still counts.

### Data model

`foreground::Sample` gains one map, `#[serde(default)]` like the others:

```rust
/// Seconds per foreground browser visit, keyed "exe\ntitle\naddress". Matched against group
/// words for one check, then dropped. Never persisted.
#[serde(default)]
pub web: BTreeMap<String, u64>,
```

* **The key carries the browser** because the service needs to know which program to close.
  `browser_page` deliberately does not return the browser today. That stays: this is a separate
  map.
* **Matching skips the browser part.** Only the text after the first `\n` is searched. Otherwise
  the word `chrome` would match every Chrome page through `chrome.exe`.
* Words cannot contain whitespace, so no word can match across the title–address boundary.
* The helper cuts the whole key to 512, `MAX_KEY_CHARS`, the longest key the service keeps whole,
  so an honest key is never shortened by the service. With no address — every visit until Phase 0
  passes — the title may use all of it. With an address, the title is cut to 250 characters and the
  address keeps the rest: at least about 250, more for a short title. A word that appears only
  beyond that point in a very long address is missed, while the title still counts.
* `Feed::submit` cuts keys as they arrive, not only at the drain, so forty keys of a megabyte each
  never wait in the service's memory between checks.
* Every existing bound applies to the new map unchanged: `truncate_keys`, `bound` (seconds capped
  at the elapsed check), `retain_top` at `MAX_PAGES`, and `accrue_capped` in `Feed::submit`.
* `MAX_LINE` stays at 1 MiB. The largest honest line grows by 40 more keys of 512 characters at
  6 bytes each, about 123,000 bytes. `the_read_limit_clears_the_largest_honest_line` is extended to
  include the new map and measures the real figure, so a future change that outgrows the limit
  fails there.

The helper and the service are the same executable, so their versions match. A sample without
`web` still parses: it simply counts nothing for web pages.

### Who owns a process

`SystemControl` gains `child_processes()`: every process running under the signed-in console
user's account, ID to name, from one snapshot. On Windows it is one `WTSEnumerateProcessesExW` call
at level 1, which returns every process's ID, name and owner SID without opening each process,
compared with `EqualSid` against the user SID from `WTSQueryUserToken` and
`GetTokenInformation(TokenUser)`. Word matching uses the name from this same snapshot, so "this ID
is the child's" and "this ID is called Roblox" always describe one process. The main process list
is a second read, so a process is closed only when both reads give its ID the same name: an ID
reused between them is left alone rather than closed and announced under the wrong name. The
service is SYSTEM, which the call requires to see other sessions. Both APIs are already enabled in
`Cargo.toml`.

* It runs **only on checks where some group has words**. A household without words pays nothing,
  and the existing process scan is untouched.
* **It fails closed.** If the call fails, or nobody is signed in, there are no child processes:
  no program is matched by a word and no browser is closed for a page. Pages still count toward the
  pool. Listed apps are unaffected.

### The service

The tick already drains the helper's report at the top (`reported`), before `decide`. The order of
work becomes:

1. **Bound.** The report is clamped with `foreground::clamp` before `decide`, not only inside
   `record_foreground`, so `decide` sees seconds already capped at the elapsed check. Its visits are
   then taken out of it, so `record_foreground` never receives an address at all.
2. **Ask who owns what.** `child_processes()`, only if `Rules::has_words` — a group that can enforce has
   a usable word. A failure is logged once when it starts, not every check while it lasts.
3. **Match.** Inside `decide`, a pure function takes each group's words, the report's `web` map
   and the owner snapshot. Per group, **by position rather than by name** (two groups may share a
   name), it returns the matched page seconds, the browsers that showed a matching page, and the
   child's processes whose names match. It drops web entries whose browser is not in `BROWSERS`.
4. **Accrue.** `Usage::accrue` charges each group the full check if a listed app or a matching
   program is running, otherwise the smaller of its matched page seconds and the check. Both are
   zero while the screen is inactive, because `delta` already is.
5. **Close.** In the existing kill loop, a process is closed for a spent group if it is a listed
   app (as today), a child's program matching a word, or a child's process of a browser that matched
   a page in this check. It is announced as `StopReason::GroupPool`. The loop's existing `continue`
   rules apply, so a process that matches several ways is closed once.
6. **Report.** `record_foreground` runs after `decide`, as now, and does not read `web` at all. The
   map is dropped with the sample.

Supporting changes:

* `AppGroup` gains `words: Vec<String>`, `#[serde(default)]`, so existing configs load unchanged.
* `AppGroup::has_pool` becomes "a limit, and a non-blank app **or** a valid word". It is the one
  definition behind `Rules::has_targets`, `Targets::from_rules` and the Today card, so a group with
  only words puts the service in enforce mode and reads the process list it needs.
* `Targets` carries each group's normalised words, dropping blank, short and invalid ones itself.
  A hand-edited `config.json` reaches `decide` without passing `validate`, which is why `Targets`
  filters blank app names today.
* `Rules::validate` refuses invalid words with a message and counts them against the existing
  `MAX_RULE_ENTRIES` and `MAX_RULE_NAME` bounds.
* `Usage` gains `per_group_web_secs`, keyed by group name and reset on rollover with the rest of the
  day. Group names come from the config, not from the child, so the map is bounded by
  `MAX_RULE_ENTRIES`.
* Scheduled routines need nothing extra. `rules_at` returns a routine's own groups, words
  included.

### The dashboard

* The group row gains the words field and the preview, with a hint that a program whose name, or
  a page whose title, contains a word shares the group's time; that only the page in front counts;
  and that only programs under the signed-in Windows account are counted or closed. It says
  *title*, not *address*, until Phase 0 passes. In English, Dutch and Turkish, like every other string.
* The preview is asked of the server, `POST /api/rules/words-preview`, when a words field gains
  focus or changes. The server runs `words::preview` — the enforcer's own matching — over
  `SystemControl::child_processes`, sparing the same check programs, and answers with the names it
  would close. The dashboard keeps no copy of the rules: it only splits the field on commas, and
  does not ask again while the field's words are unchanged.
* The answer is kept with the words it was for, and the line stays empty unless the server
  answered for exactly the words in the field. `programs: null` — the owner lookup failed — shows
  no line rather than "nothing matches", which is the answer a parent would act on. The endpoint is
  bounded like the words a save accepts and writes nothing.
* Saving keeps a group that has words and no apps. It used to drop every group without apps, which
  would have deleted a words-only group on the first save.
* The group apps placeholder changes from `minecraft.exe, roblox.exe` to
  `Minecraft.exe, RobloxPlayerBeta.exe`, and the `AppGroup` doc example likewise. `roblox.exe`
  matches nothing, and it is the example a parent copies.
* The Today card shows the web share described above.

The mobile app reads group rows as `{name, used_mins, limit_mins}` (`nestwatch-mobile`,
`lib/src/api/models.dart`, checked read-only) and ignores keys it does not know, so the added
`web_mins` needs no change there.

---

## Trust: what a forged report can and cannot do

The helper runs as the child, so everything in its report is the child's to choose. This feature
changes the rule `FOREGROUND-TRACKING.md` states as "foreground time does not enforce limits". The
new rule:

> Foreground data may **add** time to a group pool that has words, and may close the child's own
> processes of **one of the four known browsers** that reported a match. It never replaces running
> time, never subtracts, and never reaches per-app limits or the daily budget.

The reason behind the old rule still holds: a program left running still charges its group whether
or not it is in front. `foreground_time_cannot_trigger_a_per_app_limit` stays, and new tests pin
the new boundary.

Program matching does not rely on the helper at all. Process names and owners come from Windows,
read by the service.

What a forged report **can** do:

* add up to one check's length of time per check to a pool. That only costs the child's own time;
* close the child's own processes of one of the four browsers, which the child could close anyway.

What it **cannot** do:

* close anything else. A forged key naming `explorer.exe` matches no entry in `BROWSERS` and is
  dropped before matching, so it neither counts nor closes;
* close a process owned by anyone else, including SYSTEM and a parent signed in alongside;
* claim more than the elapsed check, or reach any limit other than a group pool with words.

### Evasions

Known and accepted, and the dashboard hint and README say so:

* **Renaming a program** so its name lacks the word. The same gap exists for listed names today
  (`SECURITY.md`).
* **Stopping the helper** stops page counting. Programs still count, and the existing
  `focus_missing` warning tells the parent nobody is reporting.
* **A site whose address and title both lack the word**, such as a mirror or a proxy, is not
  matched. The browser policy blocklist (`URLBlocklist`) is the tool for named sites.
* **Another browser** (Opera, Vivaldi, Arc) is not counted as a page, and closed only if its name
  matches a word. Put its executable on the blocklist.

---

## Consequences a parent should expect

* Videos, searches and articles that mention the word count, and are closed once the pool is spent.
  That is what the parent asked for: every address and every tab title containing the word.
* Closing a browser closes every tab in it, homework included.
* A browser reopened on a restored matching tab is closed again at the next check.
* Roblox's launcher and installer match too, so a spent pool also stops a reinstall or relaunch.
* A background program whose name contains the word charges the pool while it runs, even with
  nothing on screen. The preview is how a parent spots one.
* A word in a browser's own name makes the whole browser part of the group: `edge` matches
  `msedge.exe`.
* A Windows program that runs under the child's account and is not on the built-in list is
  reached by a broad word like any other program; closing it disrupts the child's session until
  Windows restarts it. The preview lists it before anything is closed. Use the name of the game.
* Typing the word into the address bar counts for the seconds it is being typed, once addresses are
  read.

---

## Phase 0 — measure before building the address half

Three things are unproven on the target PC.

**Step 1: the helper already works on that PC.** The dashboard's Today card must already list page
titles. If it does not, stop: the foreground watcher this feature builds on has not been confirmed
on a real machine (`FOREGROUND-TRACKING.md`, status line). `O20` sets the same order: do not measure
a second thing on a subsystem that has never run.

**Step 2: a throwaway measurement program**, built outside this repository and cross-compiled for
`x86_64-pc-windows-gnu`. It uses the same `windows` crate calls the product uses or would use, so it
measures the real calls rather than a stand-in. It has two modes:

* **`address`**, run as the child in the child's session. Every 5 seconds it prints the foreground
  browser, the address it read, how it found the address element, and how long the read took.
* **`owners`**, run elevated from an administrator console while the child is signed in. It prints
  each process's name and whether it runs under the signed-in user's account, using exactly the
  calls `child_processes()` makes.

**Go for addresses, per browser** (Edge and Chrome, plus Firefox if it is used):

| Check | How |
|---|---|
| The address is read correctly, in that PC's Windows language | The program's output |
| Chrome does not start building accessibility data for page contents | `chrome://accessibility` / `edge://accessibility` show web-content accessibility still off |
| No noticeable CPU or memory rise in the browser over about ten minutes | Task Manager, with and without the program running |
| A read takes well under a second | The program's output |

Firefox has no per-tab view like `chrome://accessibility`. Its accessibility engine is on or off
for the whole browser (`about:support`, *Accessibility*), so for Firefox the CPU and memory check
decides.

**Go for owners:** the child's programs (Roblox, the browsers, `host-health.exe`) are reported as
the child's, and Windows' own processes (`csrss.exe`, `wininit.exe`, `winlogon.exe`,
`svchost.exe`) are not.

**Outcome.** Each browser that passes gets its address flag. A browser that fails stays
titles-only. If every browser fails, the feature keeps matching titles only, and the README points
parents at the browser policy blocklist for known streaming sites. If the owner check fails, words
match no programs until it is fixed, which is the fail-closed state the code already has. Results,
and the language-independent way of finding the address element, are recorded in `O20`.

---

## Testing

Test-first, on the host. Everything below runs without Windows except the on-device items.

* **Words:** `%` removed at the ends, case ignored, blank dropped, 2 characters refused and 3
  accepted, 64 accepted and 65 refused, inner `%` and whitespace refused. The same rules hold in
  `Targets` for a config that never passed `validate`.
* **Programs:** matched without `.exe` (`exe` matches nothing); a matching program not owned by the
  child neither counts nor closes; Nestwatch's own names, Windows' listed programs and configured
  check programs never match; a failed owner lookup matches nothing; listed apps still match
  machine-wide; two groups sharing a name keep their own matches.
* **Folding:** Turkish `İ`/`ı` match either way; a trailing `.exe` is dropped from a word; a word's
  raw length is bounded, not only the word kept; an error names at most 64 characters of a word and
  marks a cut.
* **Pages:** title match, address match, the browser part never matched (`chrome` against a Chrome
  page), a key naming a non-browser dropped.
* **The pool:** a program and a page in the same check charge once; page only charges the matched
  seconds; inactive charges nothing; a forged claim larger than the check is capped.
* **The limit, both sides:** at 3,599 seconds of a 60-minute pool nothing closes, at 3,600 it does.
  Both cases in the same test, written with the failing test.
* **Closing:** only a browser that reported a match closes, and only the child's processes of it; a
  non-browser key closes nothing; a process that matches several ways closes once; the notice names
  the group.
* **Word-only group:** puts the service in enforce mode and appears on the Today card.
* **Old report:** a sample without `web` parses and counts nothing.
* **Privacy:** after a tick with an address in the report, the address appears in neither the
  stored day, the Today card nor the history row (`a_visits_address_never_reaches_the_stored_day`),
  nor in `usage_state.json` after a real loop tick. `tests/privacy.rs` pins the sample's shape:
  three maps of names to seconds.
* **The tick:** a full check cycle with `FakeControl`, from report to close
  (`tests/enforcer_words.rs`). It was checked by breaking the loop's wiring twice — passing no
  owners, passing no visits — and watching it fail each time.
* **Line size:** the honest-worst-case line test includes the new map.
* **Preview:** `words::preview` lists what the rule matches, each name once, sparing Windows',
  Nestwatch's and check programs; the endpoint (`tests/words_preview.rs`) needs a signed-in parent,
  lists only the child's programs, answers `null` for a failed owner lookup, and is bounded like a
  save.
* **Dashboard:** words separated by commas only; the server's answer shown only for the words it
  was asked about; no line for a failed or unknown answer.
* `cargo mutants` on the new code, with a private `CARGO_TARGET_DIR` and `-j 1`. The rustdoc gate
  stays at its expected 38 warnings.

On-device items go into `WINDOWS-TESTING.md`: the owner check, the address read in each enabled
browser, the `chrome://accessibility` check, a matching program and a matching page closing within
a minute, and a restored tab.

---

## Docs to change when this is built

* `SECURITY.md`: the narrowed trust rule above, the owner rule, and the evasions.
* `FOREGROUND-TRACKING.md`: the visit map, the rule change under *Deliberately not done*, and,
  with the address read, the helper's resource budget with `UIAutomationCore.dll` loaded.
* `README.md`: the feature, its limits, and the evasions.
* `OPEN-FINDINGS.md`: `O20` updated with what Phase 0 measured.
* `WINDOWS-TESTING.md`: the on-device items above.

---

## Deliberately not done

* **Background tabs and windows are not read.** Reading every tab of every window costs a UI
  Automation walk per tab and breaks with each browser redesign. A page that is not in front is not
  being played, and it is caught within a minute of coming to front.
* **No closing of single tabs.** The parent chose the whole browser.
* **No patterns beyond plain words.** No wildcards in the middle, no regular expressions, no
  phrases.
* **No owner rule for listed apps.** An exact name is a deliberate choice by the parent and keeps
  today's machine-wide behaviour.
* **No addresses or domains in the report.** Only the matched minutes per group.
* **No web minutes in the daily history.** The Today card only, until someone asks for more.
