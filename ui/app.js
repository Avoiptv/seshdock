const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
// mono logos are single-colour and get tinted with the text colour (dark/light mode)
const HARNESS = {
  claude: { name: "Claude Code", logo: "claudecode-color.svg" },
  codex: { name: "Codex", logo: "codex-color.svg" },
  opencode: { name: "OpenCode", logo: "opencode.svg", mono: true },
  pi: { name: "Pi", logo: "pi.svg", mono: true },
  antigravity: { name: "Antigravity", logo: "antigravity-color.svg" },
  dsh: { name: "DeepSeek Harness", logo: "deepseek-color.svg" },
  deepseek: { name: "DeepSeek TUI", logo: "deepseek-color.svg" },
  openclaw: { name: "OpenClaw", logo: "openclaw-color.svg" },
  hermes: { name: "Hermes Agent", logo: "nousresearch.svg", mono: true },
};
const isMac = navigator.userAgent.includes("Mac");

let projects = [];
let apps = [];
let missing = []; // harnesses whose CLI isn't installed
let tab = "all";
let scanText = "";
const expanded = new Set(); // project paths, survives re-render

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const plural = (n, w) => `${n} ${w}${n === 1 ? "" : "s"}`;

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto", style: "short" });
function ago(ms) {
  const s = (ms - Date.now()) / 1000;
  for (const [unit, sec] of [["year", 31536000], ["month", 2592000], ["week", 604800], ["day", 86400], ["hour", 3600], ["minute", 60]]) {
    if (Math.abs(s) >= sec) return rtf.format(Math.round(s / sec), unit);
  }
  return "just now";
}

function hue(str) {
  let h = 0;
  for (const c of str) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

const chevron = `<svg viewBox="0 0 24 24"><path d="m9 6 6 6-6 6"/></svg>`;
function logo(h, title = HARNESS[h].name) {
  const { logo: f, mono } = HARNESS[h];
  return mono
    ? `<i class="logo mono" style="--src:url(logos/${f})" title="${esc(title)}"></i>`
    : `<img class="logo" src="logos/${f}" alt="" title="${esc(title)}">`;
}
const harnessLabel = (s) => HARNESS[s.harness].name + (s.source ? ` · ${s.source}` : "");

function sessionsOf(p) {
  return tab === "all" ? p.sessions : p.sessions.filter((s) => s.harness === tab);
}

function renderTabs() {
  const count = (h) => projects.filter((p) => p.sessions.some((s) => s.harness === h)).length;
  const items = [["all", "All", projects.length], ...Object.keys(HARNESS).map((h) => [h, HARNESS[h].name, count(h)])]
    .filter(([, , n]) => n > 0);
  $("tabs").innerHTML = items.map(([id, label, n]) => `
    <button role="tab" data-tab="${id}" aria-selected="${tab === id}">
      ${id === "all" ? "" : logo(id)}${label}<span class="count">${n}</span>
    </button>`).join("");
}

function render() {
  const q = $("search").value.trim().toLowerCase();
  const shown = projects.filter((p) => {
    const ss = sessionsOf(p);
    return ss.length && (!q || [p.name, p.path, p.description, ...ss.map((s) => s.title)].some((t) => t.toLowerCase().includes(q)));
  });
  $("empty").hidden = shown.length > 0;
  $("list").innerHTML = shown.map((p) => {
    const i = projects.indexOf(p);
    const ss = sessionsOf(p);
    const open = expanded.has(p.path);
    const harnesses = [...new Set(ss.map((s) => s.harness))];
    const disabled = p.exists ? "" : 'disabled title="Folder no longer exists"';
    return `
    <article class="proj${p.exists ? "" : " gone"}${open ? " open" : ""}">
      <div class="row">
        <div class="avatar" style="--h:${hue(p.name)}">${esc([...p.name][0]?.toUpperCase() ?? "?")}</div>
        <div class="main">
          <div class="line1"><span class="name">${esc(p.name)}</span><span class="logos">${harnesses.map((h) => logo(h)).join("")}</span></div>
          <div class="path" title="${esc(p.path)}">${esc(p.path)}</div>
          <div class="desc">${esc(p.description)}</div>
          <button class="toggle" data-toggle="${i}" aria-expanded="${open}">${chevron}${plural(ss.length, "session")}</button>
        </div>
        <div class="side">
          <time>${ago(ss[0].updated)}</time>
          <button class="btn primary" data-open="${i}" data-id="${ss[0].id}" ${disabled}>Open</button>
        </div>
      </div>
      ${open ? `<ul class="sessions">${ss.map((s) => `
        <li>
          ${logo(s.harness, harnessLabel(s))}
          <span class="title" title="${esc(s.title)}">${esc(s.title)}</span>
          <span class="harness">${esc(harnessLabel(s))}</span>
          <time>${ago(s.updated)}</time>
          <button class="btn ghost" data-open="${i}" data-id="${s.id}" ${disabled}>Resume</button>
        </li>`).join("")}</ul>` : ""}
    </article>`;
  }).join("");
  const total = projects.reduce((n, p) => n + p.sessions.length, 0);
  $("status").innerHTML = `<span>${plural(projects.length, "project")} · ${plural(total, "session")}</span><span>${scanText}</span>`;
}

// ---------- open picker (command-palette style) ----------

let pickSel = 0;
let pickRun = null;

function pick(p, s) {
  $("pick-title").textContent = `Open ${p.name}`;
  $("pick-sub").innerHTML = `${logo(s.harness)}${esc(harnessLabel(s))} · ${esc(s.title)}`;
  $("pick-err").textContent = "";
  const svg = {
    terminal: `<svg viewBox="0 0 24 24"><path d="m5 8 4 4-4 4"/><path d="M12 16h7"/></svg>`,
    ide: `<svg viewBox="0 0 24 24"><path d="m8 7-5 5 5 5"/><path d="m16 7 5 5-5 5"/></svg>`,
  };
  const iconOf = (a) => a.kind === "claude-ext" ? logo("claude") : a.kind === "dsh" ? logo("dsh") : a.kind === "codex-app" ? logo("codex") : a.kind === "obsidian" ? `<img class="logo" src="logos/obsidian.svg" alt="">` : svg[a.kind];
  // which targets make sense for this session
  const only = { "claude-ext": "claude", dsh: "dsh", "codex-app": "codex" };
  const fits = (a) => !only[a.kind] || only[a.kind] === s.harness;
  const noCli = s.harness === "dsh" || missing.includes(s.harness); // terminal can only open the folder
  const groups = [
    ["codex-app", "Resume in app", "resume thread"],
    ["dsh", "Resume in app", "find it in the sidebar"],
    ["terminal", noCli ? "Open in terminal" : "Resume in terminal",
      s.harness !== "dsh" && noCli ? `${HARNESS[s.harness].name} CLI not installed · open folder` : noCli ? "open folder" : "resume session"],
    ["claude-ext", "Resume in editor", "Claude panel"],
    ["ide", "Open folder in editor", "open folder"],
    ["obsidian", "Notes", p.vault ? "open vault" : "not a vault"],
  ];
  $("pick-apps").innerHTML = groups.map(([kind, label, hint]) => {
    const items = apps.map((a, k) => [a, k]).filter(([a]) => a.kind === kind && fits(a));
    return items.length ? `<div class="group">${label}</div>` + items.map(([a, k]) => `
      <button class="option" role="option" data-app="${k}" ${kind === "obsidian" && !p.vault ? "disabled" : ""}>
        <span class="opt-icon">${iconOf(a)}</span>
        <span class="opt-name">${esc(a.name)}</span>
        <span class="opt-hint">${hint}</span>
      </button>`).join("") : "";
  }).join("");
  pickRun = async (k) => {
    const a = apps[k];
    const resume = (a.kind === "terminal" && !noCli) || a.kind in only;
    try {
      await invoke("open_in", { app: a.id, dir: p.path, harness: resume ? s.harness : null, session: resume ? s.id : null });
      $("picker").close();
    } catch (err) {
      $("pick-err").textContent = String(err);
    }
  };
  pickSel = 0;
  markSel();
  $("picker").showModal();
}

function markSel() {
  const opts = [...$("pick-apps").querySelectorAll(".option:not(:disabled)")];
  opts.forEach((o, n) => o.setAttribute("aria-selected", n === pickSel));
  opts[pickSel]?.focus();
}

$("pick-apps").addEventListener("click", (e) => {
  const b = e.target.closest("[data-app]");
  if (b) pickRun(b.dataset.app);
});
$("pick-apps").addEventListener("mousemove", (e) => {
  const opts = [...$("pick-apps").querySelectorAll(".option:not(:disabled)")];
  const n = opts.indexOf(e.target.closest(".option"));
  if (n >= 0 && n !== pickSel) { pickSel = n; markSel(); }
});
$("picker").addEventListener("keydown", (e) => {
  const n = $("pick-apps").querySelectorAll(".option:not(:disabled)").length;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    pickSel = (pickSel + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
    markSel();
  }
});

// click outside a dialog's box closes it
for (const d of document.querySelectorAll("dialog")) {
  d.addEventListener("click", (e) => {
    const r = d.getBoundingClientRect();
    const outside = e.target === d && (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom);
    if (outside || e.target.closest("[data-close]")) d.close();
  });
}

// ---------- load + events ----------

async function load() {
  $("loader").hidden = false;
  $("list").hidden = $("empty").hidden = $("tabs").hidden = true;
  $("status").textContent = "";
  const t0 = performance.now();
  [projects, apps, missing] = await Promise.all([invoke("load_projects"), invoke("detect_apps"), invoke("missing_clis")]);
  scanText = `Scanned in ${Math.round(performance.now() - t0)} ms`;
  $("loader").hidden = true;
  $("list").hidden = $("tabs").hidden = false;
  renderTabs();
  render();
  tabsFade();
}

$("list").addEventListener("click", (e) => {
  const t = e.target.closest("[data-toggle]");
  if (t) {
    const path = projects[t.dataset.toggle].path;
    expanded.has(path) ? expanded.delete(path) : expanded.add(path);
    return render();
  }
  const b = e.target.closest("[data-open]");
  if (b) {
    const p = projects[b.dataset.open];
    pick(p, p.sessions.find((s) => s.id === b.dataset.id));
  }
});
$("tabs").addEventListener("click", (e) => {
  const b = e.target.closest("[data-tab]");
  if (!b) return;
  tab = b.dataset.tab;
  renderTabs();
  render();
});
// vertical wheel scrolls the tab strip sideways; fade the edge while more tabs are hidden
const tabsFade = () => $("tabs").classList.toggle("overflow", $("tabs").scrollLeft + $("tabs").clientWidth < $("tabs").scrollWidth - 2);
$("tabs").addEventListener("wheel", (e) => { if (e.deltaY) { e.preventDefault(); $("tabs").scrollLeft += e.deltaY; } }, { passive: false });
$("tabs").addEventListener("scroll", tabsFade);
window.addEventListener("resize", tabsFade);
$("search").addEventListener("input", render);
$("refresh").addEventListener("click", load);
$("about-btn").addEventListener("click", () => $("about").showModal());
$("about").addEventListener("click", (e) => {
  const a = e.target.closest("[data-url]");
  if (a) window.__TAURI__.opener.openUrl(a.dataset.url);
});
$("kbd").textContent = isMac ? "⌘K" : "Ctrl K";
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    $("search").focus();
    $("search").select();
  }
});
load();
