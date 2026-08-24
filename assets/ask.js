// --- Today -----------------------------------------------------------
// Knowing how much time is left is the question that otherwise gets asked out loud, so
// it's the first thing on the page. /status is deliberately narrow: totals only, never
// the rules themselves.
const todayEl = document.getElementById("today");
let limitedToday = null; // null = unknown (server unreachable)

async function loadStatus() {
  try {
    const r = await fetch("/status");
    if (!r.ok) throw new Error("status " + r.status);
    const s = await r.json();
    limitedToday = !!s.limited;
    todayEl.innerHTML = "";

    if (!s.limited) {
      const p = document.createElement("p");
      p.className = "py-2 text-lg font-semibold text-success";
      p.textContent = "No time limit today \u{1F389}";
      todayEl.append(p);
      return;
    }

    const left = document.createElement("p");
    left.className = "text-4xl font-bold tabular-nums";
    left.textContent = s.remaining_mins;
    const unit = document.createElement("p");
    unit.className = "text-sm opacity-70";
    unit.textContent =
      s.remaining_mins === 1 ? "minute left today" : "minutes left today";

    const bar = document.createElement("progress");
    bar.className = "progress progress-primary mt-3 w-full";
    bar.max = s.budget_mins;
    bar.value = Math.min(s.used_mins, s.budget_mins);
    bar.setAttribute(
      "aria-label",
      `${s.used_mins} of ${s.budget_mins} minutes used today`,
    );

    const detail = document.createElement("p");
    detail.className = "mt-1 text-xs opacity-60";
    detail.textContent = `used ${s.used_mins} of ${s.budget_mins} min`;

    todayEl.append(left, unit, bar, detail);
  } catch {
    limitedToday = null;
    todayEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "text-sm opacity-70";
    p.textContent = "Couldn't check your time right now.";
    todayEl.append(p);
  }
}
loadStatus();
// Cheap and only while the page is open; keeps the number honest as time is used.
setInterval(() => {
  if (!document.hidden) loadStatus();
}, 60000);

// One place for the message styling, so success/error can't drift between the two forms.
const setMsg = (el, text, ok = false) => {
  el.textContent = text;
  el.className = `mt-1 text-center text-sm text-${ok ? "success" : "error"}`;
};

const form = document.getElementById("form");
const msg = document.getElementById("msg");
const btn = document.getElementById("btn");
form.addEventListener("submit", async (e) => {
  e.preventDefault();
  btn.disabled = true;
  msg.textContent = "";
  const minutes = parseInt(document.getElementById("minutes").value, 10) || 0;
  const reason = document.getElementById("reason").value;
  try {
    const r = await fetch("/time-request", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ minutes, reason }),
    });
    if (r.ok) {
      setMsg(msg, "Sent — waiting for a parent to reply.", true);
      form.reset();
    } else if (r.status === 429) {
      setMsg(msg, "Too many requests — wait a bit and try again.");
    } else {
      setMsg(msg, "Couldn't send (check the minutes).");
    }
  } catch {
    setMsg(msg, "Couldn't reach the server.");
  } finally {
    btn.disabled = false;
  }
});

const codeForm = document.getElementById("codeForm");
const codeMsg = document.getElementById("codeMsg");
const codeBtn = document.getElementById("codeBtn");
codeForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  codeBtn.disabled = true;
  codeMsg.textContent = "";
  // The field only *looks* uppercase via CSS; normalize before sending.
  const code = document.getElementById("code").value.trim().toUpperCase();
  try {
    const r = await fetch("/redeem-code", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code }),
    });
    const j = await r.json().catch(() => ({}));
    if (r.ok && j.ok) {
      // Don't claim minutes were "added" when no limit applies today — the grant is
      // recorded, but it changes nothing the child would notice, and saying otherwise
      // reads as the tool lying to them.
      setMsg(
        codeMsg,
        limitedToday === false
          ? "Code accepted — but there's no limit today anyway."
          : `Added ${j.minutes} minutes!`,
        true,
      );
      codeForm.reset();
      loadStatus();
    } else if (r.status === 429) {
      setMsg(codeMsg, "Too many tries — wait a bit and try again.");
    } else {
      setMsg(codeMsg, "That code isn't valid.");
    }
  } catch {
    setMsg(codeMsg, "Couldn't reach the server.");
  } finally {
    codeBtn.disabled = false;
  }
});
