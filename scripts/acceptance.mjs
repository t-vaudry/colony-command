// MVP acceptance measurements (design spec section 12). Results: docs/acceptance.md.
//
//     node scripts/acceptance.mjs latency   # first prompt -> map, permission -> porch, kill -> crashed
//     node scripts/acceptance.mjs failopen  # hooks with no daemon: time, exit code, output
//     node scripts/acceptance.mjs load      # colonyd CPU and memory, idle and under a 50-session load
//     node scripts/acceptance.mjs fps       # 50 synthetic sessions in headless Chrome (needs app/node_modules)
//     node scripts/acceptance.mjs all
//
// Scale: AGENTS=200 node scripts/acceptance.mjs fps   (also SPEED=, default 1;
// FPS_SECS=, default 15, for the measuring window). `load` uses AGENTS too.
// `fps` also reports daemon CPU/RAM, WebSocket messages/s and KB/s, and the
// page's script and task time per frame (CDP Performance.getMetrics).
//
// Needs release binaries: cargo build --release -p colonyd -p colony-hook -p colony-synth
// Everything runs against a throwaway colonyd with its own COLONY_HOME and
// USERPROFILE in a temp folder, on a free port, started with COLONY_INGEST=1.
// It never reads or touches the real ~/.colony or ~/.claude or the real colonyd.

import { spawn, spawnSync, execFileSync } from "node:child_process";
import { copyFileSync, existsSync, openSync, readdirSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir, cpus, homedir } from "node:os";
import { randomUUID } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const exe = process.platform === "win32" ? ".exe" : "";
const bin = (n) => join(root, "target", "release", n + exe);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const now = () => performance.now();

function stats(xs) {
  const s = [...xs].sort((a, b) => a - b);
  const q = (p) => s[Math.min(s.length - 1, Math.floor(p * s.length))];
  return { n: s.length, min: s[0], median: q(0.5), p95: q(0.95), max: s[s.length - 1] };
}
const fmt = (st, unit = "ms") => `n=${st.n} min ${st.min.toFixed(0)} / median ${st.median.toFixed(0)} / p95 ${st.p95.toFixed(0)} / max ${st.max.toFixed(0)} ${unit}`;
const report = (name, ok, detail) => console.log(`${ok ? "PASS" : "FAIL"}  ${name}: ${detail}`);

async function freePort() {
  return new Promise((res, rej) => {
    const s = createServer().listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
    s.on("error", rej);
  });
}

/** The environment a desktop-launched app has: no Claude Code session markers, no Git Bash/MSYS leftovers. */
const desktopEnv = (e) => Object.fromEntries(Object.entries(e).filter(([k]) => !/^(CLAUDECODE|CLAUDE_|MCP_|MSYS|MINGW|SHELL$|TERM$|HOME$|EXEPATH$|ORIGINAL_PATH$|SSH_|PWD$|OLDPWD$|SHLVL$|_$)/i.test(k)));

/** An isolated colonyd. `env` for anything that should talk to it. */
async function startDaemon({ real = false, wslHome = null } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "colony-accept-"));
  const home = join(dir, ".colony");
  mkdirSync(home, { recursive: true });
  mkdirSync(join(dir, ".claude", "sessions"), { recursive: true });
  const port = await freePort();
  // `real`: its own COLONY_HOME and port, but the real user profile, so real Claude Code (signed in
  // under it) registers its sessions where this daemon looks. COLONY_INGEST=1 keeps it away from the
  // user's WSL distros, except with `wslHome`: then ingest is off, so its WSL supervisor attaches a
  // probe to the running distro, with HOME there set to `wslHome` (carried by WSLENV) so the probe
  // and hooks use an isolated ~/.colony and ~/.claude, not the user's.
  const env = {
    ...(real ? desktopEnv(process.env) : process.env),
    COLONY_HOME: home, COLONY_PORT: String(port), COLONY_PTYD_PORT: String(await freePort()),
    ...(real ? {} : { USERPROFILE: dir, HOME: dir }),
    ...(wslHome ? { HOME: wslHome, WSLENV: "HOME" } : { COLONY_INGEST: "1" }),
  };
  // A fake HOME for Git Bash (which runs Claude Code's hooks): the user's `$HOME/.colony/bin/colony-hook.exe`
  // entry then finds this build's hook, and the user's installed copy is not involved.
  const fakeHome = join(dir, "home");
  if (real) {
    mkdirSync(join(fakeHome, ".colony", "bin"), { recursive: true });
    copyFileSync(bin("colony-hook"), join(fakeHome, ".colony", "bin", "colony-hook" + exe));
    if (!wslHome) env.HOME = fakeHome;
  }
  const child = spawn(bin("colonyd"), [], { env, stdio: process.env.KEEP ? ["ignore", openSync(join(dir, "colonyd.out"), "a"), openSync(join(dir, "colonyd.err"), "a")] : "ignore" });
  const deadline = Date.now() + 20000;
  let info;
  while (Date.now() < deadline) {
    try {
      info = JSON.parse(readFileSync(join(home, "daemon.json"), "utf8"));
      if (info.port === port && (await fetch(`http://127.0.0.1:${port}/api/agents`, { headers: { authorization: `Bearer ${info.token}` } })).ok) break;
    } catch {}
    info = undefined;
    await sleep(100);
  }
  if (!info) {
    child.kill();
    throw new Error("colonyd did not start");
  }
  return {
    dir, home, port, env, pid: child.pid, token: info.token,
    /** Only the daemon (a stopped colonyd); the folder stays. */
    kill() { child.kill(); },
    stop() {
      child.kill();
      // A colony-ptyd this daemon started outlives it by design; take it (and its terminals) down.
      try { killTree(JSON.parse(readFileSync(join(home, "ptyd.json"), "utf8")).pid); } catch {}
      if (process.env.KEEP) console.log("kept", dir);
      else try { rmSync(dir, { recursive: true, force: true }); } catch {}
    },
  };
}

const AGENTS = Number(process.env.AGENTS ?? 50);
const SPEED = process.env.SPEED ?? "1";

/** A map: WebSocket client keeping the agent table and the time of each change. */
async function connectMap(d) {
  const ws = new WebSocket(`ws://127.0.0.1:${d.port}/ws?token=${d.token}`);
  const map = { agents: new Map(), waiters: [], ws, stats: { msgs: 0, bytes: 0 }, term: "", last: null, stateAt: new Map() };
  map.send = (o) => ws.send(JSON.stringify(o));
  const seen = (a) => {
    // First time each session is seen in each state (and with a permission pending).
    for (const k of [`${a.session_id}:${a.state}`, a.permission ? `${a.session_id}:permission` : null]) if (k && !map.stateAt.has(k)) map.stateAt.set(k, now());
  };
  /** Applies one message; true when it changed the agent table. A "batch" carries several. */
  const apply = (msg) => {
    if (msg.type === "batch") return msg.msgs.map(apply).some(Boolean);
    if (msg.type === "snapshot") msg.agents.forEach((a) => { map.agents.set(a.id, a); seen(a); });
    else if (msg.type === "upsert") { map.agents.set(msg.agent.id, msg.agent); seen(msg.agent); }
    else if (msg.type === "remove") map.agents.delete(msg.id);
    else if (msg.type === "term_data") {
      map.term = (msg.reset ? "" : map.term) + Buffer.from(msg.data, "base64").toString("utf8");
      return false;
    } else if (msg.type === "spawned" || msg.type === "error") {
      map.last = msg;
      return false;
    } else return false;
    return true;
  };
  ws.onmessage = (m) => {
    map.stats.msgs++;
    map.stats.bytes += Buffer.byteLength(String(m.data));
    if (apply(JSON.parse(m.data))) map.waiters = map.waiters.filter((w) => !w());
  };
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("websocket failed")); });
  await sleep(200);
  /** Resolves with performance.now() at the message that made `pred` true. */
  map.until = (pred, ms = 30000, what = "") =>
    new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error("timed out waiting for the map " + what + JSON.stringify([...map.agents.values()].map((a) => [a.session_id, a.state])))), ms);
      const check = () => {
        if (!pred(map.agents)) return false;
        clearTimeout(t);
        res(now());
        return true;
      };
      if (!check()) map.waiters.push(check);
    });
  return map;
}

const bySession = (sid) => (agents) => [...agents.values()].find((a) => a.session_id === sid && !a.parent_id);

/** Runs colony-hook with a payload on stdin, as Claude Code would. */
function runHook(d, payload, extraEnv = {}) {
  return spawn(bin("colony-hook"), [], { env: { ...d.env, ...extraEnv }, stdio: ["pipe", "pipe", "pipe"] }).on("spawn", function () {
    this.stdin.end(JSON.stringify(payload));
  });
}
const hookDone = (c) => new Promise((res) => c.on("close", (code) => res(code)));

/** A stand-in for a Claude Code process: lives until killed. */
function dummyClaude() {
  return spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], { stdio: "ignore" });
}

function writeRegistry(d, pid, sid, cwd) {
  writeFileSync(join(d.dir, ".claude", "sessions", `${pid}.json`),
    JSON.stringify({ pid, sessionId: sid, cwd, startedAt: Date.now(), version: "2.1.0", kind: "interactive", entrypoint: "cli", status: "idle" }));
}

let seq = 0;
const sid = () => `accept-${process.pid}-${++seq}`;

function killTree(pid) {
  if (process.platform === "win32") spawnSync("taskkill", ["/F", "/T", "/PID", String(pid)], { stdio: "ignore" });
  else try { process.kill(pid, "SIGKILL"); } catch {}
}

async function latency() {
  const d = await startDaemon();
  const map = await connectMap(d);
  const cwd = join(d.dir, "proj");
  mkdirSync(cwd, { recursive: true });
  const first = [], porch = [], crash = [];
  let clean;
  try {
    for (let i = 0; i < Number(process.env.TRIALS ?? 20); i++) {
      // 1. A session's first prompt. Worst case for the daemon: the registry file and the
      //    first hook land together (in reality Claude registers at launch, before the prompt).
      const p = dummyClaude();
      const s = sid();
      const t0 = now();
      writeRegistry(d, p.pid, s, cwd);
      const h = runHook(d, { session_id: s, hook_event_name: "UserPromptSubmit", cwd, prompt: "hello", transcript_path: join(cwd, "t.jsonl") });
      await hookDone(h);
      const t1 = await map.until((a) => bySession(s)(a)?.state === "working");
      first.push(t1 - t0);

      // 2. A permission prompt: the hook is held by colonyd while the map has it on the porch.
      const t2 = now();
      const ph = runHook(d, { session_id: s, hook_event_name: "PermissionRequest", cwd, tool_name: "Bash", tool_input: { command: "npm test" } });
      const t3 = await map.until((a) => { const x = bySession(s)(a); return x?.state === "needs_input" && x.permission; });
      porch.push(t3 - t2);
      ph.kill();
      await map.until((a) => !bySession(s)(a)?.permission);

      // 3. The terminal is killed (nothing gets to say SessionEnd; the registry file stays).
      const t4 = now();
      killTree(p.pid);
      const t5 = await map.until((a) => bySession(s)(a)?.state === "crashed", 60000);
      crash.push(t5 - t4);
      process.stdout.write(".");
    }
    console.log();
    // A normal exit (SessionEnd, then the process goes) must not be reported as a crash.
    const p = dummyClaude(), s = sid();
    writeRegistry(d, p.pid, s, cwd);
    await hookDone(runHook(d, { session_id: s, hook_event_name: "UserPromptSubmit", cwd, prompt: "hi" }));
    await map.until((a) => bySession(s)(a)?.state === "working");
    await hookDone(runHook(d, { session_id: s, hook_event_name: "SessionEnd", cwd }));
    killTree(p.pid);
    rmSync(join(d.dir, ".claude", "sessions", `${p.pid}.json`));
    await sleep(8000);
    clean = bySession(s)(map.agents)?.state;
  } finally {
    map.ws.close();
    d.stop();
  }
  report("normal exit is not a crash", clean === "ended", `state after SessionEnd + exit: ${clean}`);
  report("session appears after first prompt (< 2000 ms)", stats(first).max < 2000, fmt(stats(first)));
  report("permission prompt on the porch (< 1000 ms)", stats(porch).max < 1000, fmt(stats(porch)));
  report("killed terminal shows crashed (< 5000 ms)", stats(crash).max < 5000, fmt(stats(crash)));
}

/** Time a command; returns wall ms, exit code and what it printed. */
function timed(cmd, args, opts) {
  const t = now();
  const r = spawnSync(cmd, args, { encoding: "utf8", ...opts });
  return { ms: now() - t, code: r.status, out: r.stdout, err: r.stderr };
}

async function failopen() {
  const dir = mkdtempSync(join(tmpdir(), "colony-accept-"));
  const home = join(dir, ".colony");
  mkdirSync(home, { recursive: true });
  const env = { ...process.env, COLONY_HOME: home, USERPROFILE: dir, HOME: dir };
  const cwd = dir;
  const events = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];
  const payload = (e) => JSON.stringify({ session_id: "failopen", hook_event_name: e, cwd, tool_name: "Bash", tool_input: { command: "ls" }, prompt: "hi" });
  const run = (label, mk) => {
    const times = [];
    let bad = [];
    for (let i = 0; i < 5; i++) for (const e of events) {
      const r = mk(e);
      times.push(r.ms);
      if (r.code !== 0 || r.out || r.err) bad.push(`${e}: exit ${r.code} out=${JSON.stringify(r.out)} err=${JSON.stringify(r.err)}`);
    }
    return { label, times, bad };
  };
  const results = [];
  let base;
  try {
    // Baseline: what spawning a trivial process costs here, so hook time can be read net of it.
    base = [];
    for (let i = 0; i < 20; i++) base.push(timed(bin("colony-hook"), ["--version"], { env, input: "" }).ms);
    // a. no daemon.json at all (never installed / app closed and cleaned up)
    results.push(run("colony-hook, no daemon.json", (e) => timed(bin("colony-hook"), [], { env, input: payload(e) })));
    // b. stale daemon.json: the daemon died, its port is dead
    const dead = await freePort();
    writeFileSync(join(home, "daemon.json"), JSON.stringify({ port: dead, token: "abcdef0123456789" }));
    results.push(run("colony-hook, stale daemon.json (dead port)", (e) => timed(bin("colony-hook"), [], { env, input: payload(e) })));
    // c. something else holds the port and never answers
    const hung = createServer((s) => s.on("error", () => {})).listen(0, "127.0.0.1");
    await new Promise((r) => hung.on("listening", r));
    writeFileSync(join(home, "daemon.json"), JSON.stringify({ port: hung.address().port, token: "abcdef0123456789" }));
    results.push(run("colony-hook, port open but silent (non-permission events)", (e) => e === "PermissionRequest"
      ? { ms: 0, code: 0, out: "", err: "" } // a held permission request is meant to wait for the map; see docs
      : timed(bin("colony-hook"), [], { env, input: payload(e) })));
    hung.close();
    rmSync(join(home, "daemon.json"));
    // d. the approval hook script (WSL / manual installs on Windows), needs sh and curl
    const sh = process.platform === "win32" ? "C:\\Program Files\\Git\\usr\\bin\\sh.exe" : "sh";
    if (existsSync(sh) || process.platform !== "win32") {
      const script = join(root, "hooks", "colony-approve.sh");
      results.push(run("colony-approve.sh, no daemon.json", () => timed(sh, [script], { env, input: payload("PermissionRequest") })));
      writeFileSync(join(home, "daemon.json"), JSON.stringify({ port: dead, token: "abcdef0123456789" }));
      results.push(run("colony-approve.sh, stale daemon.json (dead port)", () => timed(sh, [script], { env, input: payload("PermissionRequest") })));
    } else console.log("(sh not found: skipping colony-approve.sh)");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  console.log(`baseline: ${fmt(stats(base))} (colony-hook --version, no work)`);
  for (const r of results) {
    const t = r.times.filter((x) => x > 0);
    report(`${r.label}`, r.bad.length === 0, `${fmt(stats(t))}${r.bad.length ? "\n      " + r.bad.slice(0, 3).join("\n      ") : ""}`);
  }
}

/** CPU seconds and working set of a pid (Windows: PowerShell; elsewhere: /proc). */
function proc(pid) {
  if (process.platform === "win32") {
    const o = execFileSync("powershell", ["-NoProfile", "-Command", `$p=Get-Process -Id ${pid}; "$($p.TotalProcessorTime.TotalSeconds) $($p.WorkingSet64) $($p.PrivateMemorySize64)"`], { encoding: "utf8" });
    const [cpu, ws, priv] = o.trim().split(/\s+/).map(Number);
    return { cpu, ws, priv };
  }
  const st = readFileSync(`/proc/${pid}/stat`, "utf8").split(" ");
  const cpu = (Number(st[13]) + Number(st[14])) / 100;
  const rss = Number(readFileSync(`/proc/${pid}/statm`, "utf8").split(" ")[1]) * 4096;
  return { cpu, ws: rss, priv: rss };
}

/** Samples colonyd for `secs`; CPU as % of one core and of the whole machine. */
async function sample(d, secs) {
  const a = proc(d.pid), t0 = now();
  let peak = a.ws;
  for (let i = 0; i < secs; i += 5) { await sleep(5000); peak = Math.max(peak, proc(d.pid).ws); }
  const b = proc(d.pid), wall = (now() - t0) / 1000;
  const core = ((b.cpu - a.cpu) / wall) * 100;
  return { core, machine: core / cpus().length, ws: b.ws / 1048576, peak: peak / 1048576, priv: b.priv / 1048576 };
}

async function load() {
  const d = await startDaemon();
  const map = await connectMap(d); // a map connected, as in real use
  const row = (label, s) => console.log(`${label}: CPU ${s.core.toFixed(2)}% of one core (${s.machine.toFixed(2)}% of ${cpus().length} cores), working set ${s.ws.toFixed(1)} MB (peak ${s.peak.toFixed(1)}), private ${s.priv.toFixed(1)} MB`);
  try {
    row("empty daemon, map connected, 30 s", await sample(d, 30));
    // Feed 50 synthetic sessions at human pace, then let them sit.
    const synth = spawn(bin("colony-synth"), ["--home", d.home, "--agents", String(AGENTS), "--speed", SPEED, "--duration", "120"], { stdio: "ignore" });
    await sleep(2000);
    row("50 sessions at human pace (synth --speed 1), 60 s", await sample(d, 60));
    await new Promise((r) => synth.on("close", r));
    await sleep(3000);
    const idle = await sample(d, 60);
    row("50 sessions on the map, nothing happening, 60 s", idle);
    report("daemon idle: < 1% CPU and < 80 MB RAM", idle.core < 1 && idle.ws < 80, `${idle.core.toFixed(2)}% of a core, ${idle.ws.toFixed(1)} MB`);
    const synth5 = spawn(bin("colony-synth"), ["--home", d.home, "--agents", String(AGENTS), "--speed", "5", "--duration", "40"], { stdio: "ignore" });
    await sleep(2000);
    row("50 sessions at 5x pace (stress), 30 s", await sample(d, 30));
    synth5.kill();
  } finally {
    map.ws.close();
    d.stop();
  }
}

async function fps() {
  const appDir = join(root, "app");
  if (!existsSync(join(appDir, "node_modules"))) throw new Error("run `npm install` in app/ first");
  const chrome = [process.env.CHROME, "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "/usr/bin/google-chrome", "/usr/bin/chromium"].find((p) => p && existsSync(p));
  if (!chrome) throw new Error("no Chrome/Edge found (set CHROME)");
  const d = await startDaemon();
  const vitePort = await freePort(), cdpPort = await freePort();
  const vite = spawn(process.execPath, [join(appDir, "node_modules", "vite", "bin", "vite.js"), "--port", String(vitePort), "--strictPort", "--host", "127.0.0.1"], { cwd: appDir, env: d.env, stdio: "ignore" });
  const profile = mkdtempSync(join(tmpdir(), "colony-chrome-"));
  const browser = spawn(chrome, [`--remote-debugging-port=${cdpPort}`, `--user-data-dir=${profile}`, "--headless=new", "--window-size=1600,900", "--no-first-run", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" });
  const map = await connectMap(d); // a second map, to count what the daemon sends
  const synth = spawn(bin("colony-synth"), ["--home", d.home, "--agents", String(AGENTS), "--speed", SPEED], { stdio: "ignore" });
  try {
    let target;
    for (let i = 0; i < 100 && !target; i++) {
      try { target = (await (await fetch(`http://127.0.0.1:${cdpPort}/json`)).json()).find((t) => t.type === "page"); } catch {}
      if (!target) await sleep(200);
    }
    if (!target) throw new Error("browser did not start");
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((r) => (ws.onopen = r));
    let id = 0; const pending = new Map();
    ws.onmessage = (m) => { const x = JSON.parse(m.data); pending.get(x.id)?.(x); if (process.env.DEBUG && x.method && /exception|crash|consoleAPI|loadingFailed/i.test(x.method)) console.log(x.method, JSON.stringify(x.params).slice(0, 400)); };
    const cdp = (method, params = {}) => new Promise((res) => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
    await cdp("Page.enable");
    await cdp("Performance.enable");
    await cdp("Runtime.enable");
    await cdp("Inspector.enable");
    await cdp("Page.navigate", { url: `http://127.0.0.1:${vitePort}/` });
    // Wait for 50 bots on the map (the HUD counts them), then measure.
    const ev = async (expression) => (await cdp("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;
    // Vite's first request builds the dependency cache, which can take a while: wait for the canvas, then let the load settle.
    const hasCanvas = async () => {
      for (let i = 0; i < 120 && !(await ev(`!!document.querySelector('canvas')`)); i++) {
        await sleep(500);
        if (i % 20 === 19) await cdp("Page.reload");
      }
    };
    await hasCanvas();
    await sleep(Math.max(15000, AGENTS * 40));
    await hasCanvas(); // Vite can reload the page once after optimizing dependencies
    const secs = Number(process.env.FPS_SECS ?? 15);
    const metrics = async () => Object.fromEntries(((await cdp("Performance.getMetrics")).result?.metrics ?? []).map((m) => [m.name, m.value]));
    const meter = `new Promise(res=>{const t=[];let last=performance.now(),end=last+${secs * 1000};function f(n){t.push(n-last);last=n;if(n<end)requestAnimationFrame(f);else res(t)}requestAnimationFrame(f)})`;
    if (process.env.PROFILE) await cdp("Profiler.enable"), await cdp("Profiler.start");
    const m0 = await metrics(), p0 = proc(d.pid), w0 = { ...map.stats }, t0 = now();
    const times = await ev(meter);
    if (process.env.PROFILE) {
      const prof = (await cdp("Profiler.stop")).result.profile;
      const dt = prof.timeDeltas, self = new Map(), byId = new Map(prof.nodes.map((n) => [n.id, n]));
      prof.samples.forEach((sid, i) => { const cf = byId.get(sid).callFrame, k = `${cf.functionName || "(anon)"} ${cf.url.split("/").slice(-2).join("/")}:${cf.lineNumber}`; self.set(k, (self.get(k) ?? 0) + (dt[i] ?? 0)); });
      const top = [...self].sort((a, b) => b[1] - a[1]).slice(0, 14).map(([k, v]) => `  ${(v / 1000).toFixed(0).padStart(6)}  ${k}`);
      console.log(["top self time (ms):", ...top].join("\n"));
    }
    const m1 = await metrics(), p1 = proc(d.pid), w1 = { ...map.stats }, wall = (now() - t0) / 1000;
    const canvas = await ev(`(()=>{const c=document.querySelector('canvas');return c?c.width+'x'+c.height:'no canvas'})()`);
    const agents = [...(await (await fetch(`http://127.0.0.1:${d.port}/api/agents`, { headers: { authorization: `Bearer ${d.token}` } })).json()).agents ?? []].length;
    if (process.env.SHOT) writeFileSync(process.env.SHOT, Buffer.from((await cdp("Page.captureScreenshot")).result.data, "base64"));
    const frames = times.slice(5);
    const total = frames.reduce((a, b) => a + b, 0);
    const avg = (frames.length / total) * 1000;
    const sorted = [...frames].sort((a, b) => a - b);
    const p99 = sorted[Math.floor(sorted.length * 0.99)];
    const slow = frames.filter((f) => f > 20).length;
    console.log(`canvas ${canvas}, ${agents} agents on daemon, ${frames.length} frames over ${(total / 1000).toFixed(1)} s`);
    console.log(`frame time ms: median ${sorted[Math.floor(sorted.length / 2)].toFixed(1)}, p99 ${p99.toFixed(1)}, max ${sorted[sorted.length - 1].toFixed(1)}; frames over 20 ms: ${slow} (${((slow / frames.length) * 100).toFixed(1)}%)`);
    const per = (k) => ((m1[k] - m0[k]) * 1000) / frames.length;
    console.log(`page per frame: task ${per("TaskDuration").toFixed(2)} ms, script ${per("ScriptDuration").toFixed(2)} ms, layout+style ${(per("LayoutDuration") + per("RecalcStyleDuration")).toFixed(2)} ms, JS heap ${(m1.JSHeapUsedSize / 1048576).toFixed(1)} MB`);
    console.log(`daemon: CPU ${(((p1.cpu - p0.cpu) / wall) * 100).toFixed(2)}% of one core, working set ${(p1.ws / 1048576).toFixed(1)} MB`);
    console.log(`websocket: ${((w1.msgs - w0.msgs) / wall).toFixed(1)} msgs/s, ${((w1.bytes - w0.bytes) / 1024 / wall).toFixed(1)} KB/s, snapshot-inclusive total ${(w1.bytes / 1024).toFixed(0)} KB in ${w1.msgs} msgs`);
    report(`${AGENTS} sessions render at 60 fps`, avg >= 58, `${avg.toFixed(1)} fps average`);
    ws.close();
  } finally {
    map.ws.close(); synth.kill(); browser.kill(); vite.kill(); d.stop();
    try { rmSync(profile, { recursive: true, force: true }); } catch {}
  }
}


// ---------------------------------------------------------------------------
// Real Claude Code (`real-windows`, `real-colony`, `real-wsl`, or `real` for all three).
//
// These need `claude` signed in on Windows (and in WSL for `real-wsl`), open real console windows,
// and make a few tiny model calls (haiku). They are not part of `all`. Each uses its own colonyd
// (own COLONY_HOME, port and ptyd port) and the real user profile, so the user's running Colony is
// not disturbed: its hooks find the test daemon through the COLONY_HOME the test sets. The test
// sessions carry a session id starting with `a11acc00-`; `reap` ends any left behind.

const SID_PREFIX = "a11acc00-";
const newSid = () => SID_PREFIX + randomUUID().slice(9);
const TOOL_PROMPT = "Use the Bash tool to run exactly this command and nothing else: node -p 6*7*11111 . Then reply with the single word done.";
const PLAIN_PROMPT = "Reply with the single word done and do not use any tools.";
const DISTRO = process.env.WSL_DISTRO ?? "Ubuntu";
const strip = (s) => s.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]|\x1b\][^\x07]*\x07/g, "");
const dbg = (...a) => process.env.VERBOSE && console.log("   ", ...a);
/** Never deleted by this script. The trust question for a fresh folder defaults to "No, exit". */
const trustedCwd = () => process.env.REAL_CWD ?? join(root, "..", "..", "..");
const claudeSessions = () => join(homedir(), ".claude", "sessions");

function windowsClaude() {
  const r = spawnSync("where.exe", ["claude"], { encoding: "utf8" });
  return r.status === 0 ? r.stdout.split(/\r?\n/)[0].trim() : null;
}

async function waitFor(pred, ms, step = 100) {
  const t0 = now();
  while (now() - t0 < ms) { if (pred()) return true; await sleep(step); }
  return false;
}

/** Windows pids of Claude Code processes whose registry entry has a test session id. */
function testSessions() {
  const out = [];
  try {
    for (const f of readdirSync(claudeSessions())) {
      if (!f.endsWith(".json")) continue;
      try {
        const r = JSON.parse(readFileSync(join(claudeSessions(), f), "utf8"));
        if (String(r.sessionId).startsWith(SID_PREFIX)) out.push(r);
      } catch {}
    }
  } catch {}
  return out;
}
const registryPid = (sid) => testSessions().find((r) => r.sessionId === sid)?.pid;

/** Ends test sessions that are still running (Windows). */
function reap() {
  for (const r of testSessions()) {
    killTree(r.pid);
    console.log("ended", r.sessionId, r.pid);
  }
}

/** Every hook payload that reaches the test daemon's folder, with the time it was first seen. */
function watchHooks(d) {
  const files = new Set(), events = [];
  const iv = setInterval(() => {
    for (const sub of ["capture", "spool"]) {
      let names = [];
      try { names = readdirSync(join(d.home, sub)); } catch { continue; }
      for (const f of names) {
        if (!f.endsWith(".json") || files.has(sub + f)) continue;
        files.add(sub + f);
        try {
          const j = JSON.parse(readFileSync(join(d.home, sub, f), "utf8"));
          events.push({ ev: j.hook_event_name, sid: j.session_id, t: now(), sub, transcript: j.transcript_path });
        } catch {}
      }
    }
  }, 25);
  return {
    events,
    find: (sid, ev) => events.find((e) => e.sid === sid && e.ev === ev),
    has: (sid, ev) => events.some((e) => e.sid === sid && e.ev === ev),
    stop: () => clearInterval(iv),
  };
}

/** Claude Code in its own console window (a real Windows terminal session). */
/*  It runs with the user's own settings.json, so with the user's hooks: `$HOME/.colony/bin/colony-hook.exe`
 *  is looked up in the test folder's fake HOME, where a copy of this build's colony-hook sits (what a
 *  Colony install does, minus the user's older copy), and COLONY_HOME points at the test daemon. The
 *  user's raw capture hooks write into the fake HOME too. Nothing reaches the user's real Colony. */
function startConsole(d, sid, prompt) {
  const debug = process.env.KEEP ? ` --debug-file "${join(d.dir, "claude-" + sid.slice(-6) + ".log")}"` : "";
  const cmd = `/c start "colony-accept" /D "${trustedCwd()}" claude --session-id ${sid} --model haiku --permission-mode default${debug} "${prompt}"`;
  return spawn("cmd.exe", [cmd], { env: { ...d.env }, stdio: "ignore", windowsVerbatimArguments: true });
}

const reportTimes = (name, xs, limit, unit = "ms") => report(name, xs.length > 0 && stats(xs).max < limit, xs.length ? fmt(stats(xs), unit) : "no samples");

/** Answers a porch request and waits for Claude Code to act on it. */
async function answerPorch(map, hooks, sid, choice) {
  const ask = () => bySession(sid)(map.agents)?.permission;
  await map.until(() => ask(), 120000, `(porch for ${sid})`);
  const a = ask();
  const tPorch = map.stateAt.get(sid + ":permission");
  const tHook = hooks?.find(sid, "PermissionRequest")?.t;
  map.send({ type: "permission", request_id: a.request_id, choice });
  const tAnswer = now();
  await map.until(() => !ask(), 15000, "(porch cleared)");
  return { tool: a.tool, porchMs: tHook && tPorch ? Math.max(0, tPorch - tHook) : null, clearedMs: now() - tAnswer };
}

async function realWindows() {
  if (!windowsClaude()) return report("real Claude Code in a Windows terminal", false, "claude not found on PATH");
  const d = await startDaemon({ real: true });
  const map = await connectMap(d);
  const hooks = watchHooks(d);
  const sids = [];
  const porch = [], cleared = [], promptToWorking = [], launchToVisible = [], crash = [];
  try {
    for (const choice of ["allow", "deny"]) {
      const sid = newSid();
      sids.push(sid);
      const t0 = now();
      startConsole(d, sid, TOOL_PROMPT);
      const tVisible = await map.until(bySession(sid), 120000, "(session appears)");
      launchToVisible.push(tVisible - t0);
      const r = await answerPorch(map, hooks, sid, choice);
      if (r.porchMs !== null) porch.push(r.porchMs);
      cleared.push(r.clearedMs);
      await waitFor(() => hooks.has(sid, "Stop"), 90000);
      const ran = hooks.has(sid, "PostToolUse");
      report(`${choice === "allow" ? "Allow" : "Deny"} on the porch reaches Claude Code (tool ${r.tool})`, choice === "allow" ? ran : !ran && hooks.has(sid, "Stop"),
        `tool ${ran ? "ran" : "did not run"}, hooks seen: ${hooks.events.filter((e) => e.sid === sid).map((e) => e.ev).join(",")}`);
      const up = hooks.find(sid, "UserPromptSubmit")?.t, wk = map.stateAt.get(sid + ":working");
      if (up && wk) promptToWorking.push(Math.max(0, wk - up));

      // The terminal is killed: nothing says SessionEnd, the registry file stays.
      const pid = registryPid(sid) ?? bySession(sid)(map.agents)?.pids?.[0];
      dbg("killing", sid, pid, JSON.stringify([...map.agents.values()].filter((a) => a.session_id === sid).map((a) => [a.id, a.state, a.pids, a.parent_id])), readdirSync(claudeSessions()).join(" "));
      const tk = now();
      killTree(pid);
      const tc = await map.until((a) => bySession(sid)(a)?.state === "crashed", 60000, "(crashed)");
      crash.push(tc - tk);
    }

    // The daemon goes away while a session is mid-prompt: Claude Code must carry on.
    const sid = newSid();
    sids.push(sid);
    const t0 = now();
    startConsole(d, sid, PLAIN_PROMPT);
    await waitFor(() => hooks.has(sid, "SessionStart"), 120000);
    const transcript = hooks.find(sid, "SessionStart")?.transcript;
    d.kill();
    const answered = await waitFor(() => {
      try { return /"role":"assistant"/.test(readFileSync(transcript, "utf8")); } catch { return false; }
    }, 90000, 250);
    const alive = registryPid(sid) && spawnSync("tasklist", ["/FI", `PID eq ${registryPid(sid)}`, "/NH"], { encoding: "utf8" }).stdout.includes(String(registryPid(sid)));
    const after = hooks.events.filter((e) => e.sid === sid && e.sub === "spool").map((e) => e.ev);
    report("daemon stopped mid-session: Claude Code still answers", answered && !!alive, `reply written to the transcript ${answered ? "yes" : "NO"} (${((now() - t0) / 1000).toFixed(1)} s after launch), process alive ${!!alive}, hooks kept in the spool: ${after.join(",") || "none"}`);
    report("daemon stopped: hooks keep recording (spool)", after.length > 0, `${after.length} payloads`);
  } finally {
    hooks.stop();
    map.ws.close();
    for (const s of sids) { const p = registryPid(s); if (p) killTree(p); }
    d.stop();
  }
  // Not a Colony latency: Claude Code writes its registry file when it has started up.
  console.log(`info  launch of claude.exe to the bot on the map: ${fmt(stats(launchToVisible))}`);
  reportTimes("real: first prompt to working (< 2000 ms)", promptToWorking, 2000);
  reportTimes("real: permission prompt on the porch (< 1000 ms)", porch, 1000);
  reportTimes("real: Allow/Deny answer clears the porch (< 1000 ms)", cleared, 1000);
  reportTimes("real: killed terminal shows crashed (< 5000 ms)", crash, 5000);
}

/** A session in a terminal Colony owns (colony-ptyd). */
async function realColony() {
  if (!windowsClaude()) return report("real Claude Code in a Colony terminal", false, "claude not found on PATH");
  const d = await startDaemon({ real: true });
  const map = await connectMap(d);
  const hooks = watchHooks(d);
  const terms = [];
  const spawnOne = async (prompt) => {
    map.last = null;
    map.send({ type: "spawn", host: "win", dir: trustedCwd(), prompt, model: "haiku", permission_mode: "default", chrome: false });
    await waitFor(() => map.last, 30000);
    if (map.last?.type !== "spawned") throw new Error("spawn failed: " + JSON.stringify(map.last));
    terms.push(map.last.term);
    map.send({ type: "attach", term: map.last.term });
    return map.last;
  };
  const crash = [], ended = [];
  try {
    // Killed from outside (the process dies, the terminal host reports it exited).
    let s = await spawnOne(PLAIN_PROMPT);
    await map.until((a) => ["working", "idle", "ready_to_review"].includes(bySession(s.session_id)(a)?.state), 120000, "(session in a Colony terminal appears)");
    await waitFor(() => registryPid(s.session_id) || bySession(s.session_id)(map.agents)?.pids?.length, 60000);
    const pid = bySession(s.session_id)(map.agents).pids?.[0] ?? registryPid(s.session_id);
    const tk = now();
    killTree(pid);
    crash.push((await map.until((a) => bySession(s.session_id)(a)?.state === "crashed", 60000, "(crashed)")) - tk);
    report("Colony terminal: killed process shows crashed", true, `${(crash[0] / 1000).toFixed(1)} s`);

    // Ended from the map (Kill): not a crash.
    s = await spawnOne(PLAIN_PROMPT);
    await map.until((a) => ["working", "idle", "ready_to_review"].includes(bySession(s.session_id)(a)?.state), 120000, "(second session appears)");
    await sleep(3000);
    const tm = now();
    map.send({ type: "kill", term: s.term });
    const te = await map.until((a) => ["ended"].includes(bySession(s.session_id)(a)?.state), 60000, "(ended)");
    ended.push(te - tm);
    report("Colony terminal: Kill from the map shows ended, not crashed", true, `${(ended[0] / 1000).toFixed(1)} s`);

    // The permission round trip through a Colony terminal.
    s = await spawnOne(TOOL_PROMPT);
    const reached = await waitFor(() => bySession(s.session_id)(map.agents)?.permission, 90000);
    if (!reached) {
      report("Colony terminal: permission prompt reaches the porch", false,
        `not on the porch after 90 s; state ${bySession(s.session_id)(map.agents)?.state}; hooks that reached colonyd: ${hooks.events.filter((e) => e.sid === s.session_id).map((e) => e.ev).join(",") || "none"}\n      terminal: ${strip(map.term).replace(/\s+/g, " ").slice(-300)}`);
    } else {
      const r = await answerPorch(map, hooks, s.session_id, "allow");
      await waitFor(() => hooks.has(s.session_id, "PostToolUse"), 60000);
      report("Colony terminal: permission prompt -> porch -> Allow", hooks.has(s.session_id, "PostToolUse"), `porch ${r.porchMs ?? "?"} ms after the hook, cleared ${r.clearedMs.toFixed(0)} ms after the answer`);
    }
  } finally {
    hooks.stop();
    for (const t of terms) try { map.send({ type: "kill", term: t }); } catch {}
    await sleep(1000);
    map.ws.close();
    d.stop();
  }
  reportTimes("real: Colony terminal killed -> crashed (< 5000 ms)", crash, 5000);
}

// --- WSL ---------------------------------------------------------------------

const toWsl = (p) => p.replace(/^([A-Za-z]):/, (_, l) => `/mnt/${l.toLowerCase()}`).replace(/\\/g, "/");
const wsl = (...args) => spawnSync("wsl.exe", ["-d", DISTRO, "-e", ...args], { encoding: "utf8" });
const WSL_HOME = "/tmp/colony-accept-wsl";

/** An isolated HOME inside the distro: the installed hook, probe and approval script, hooks in settings.json, the user's trust and sign-in (linked, not copied). */
function setupWslHome(dir) {
  const real = wsl("sh", "-c", 'echo "$HOME"').stdout.trim();
  const events = ["SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Notification", "Stop", "StopFailure", "SubagentStart", "SubagentStop"];
  const entry = (command, timeout) => [{ hooks: [{ type: "command", command, timeout }] }];
  const hooksCmd = '[ -x "$HOME/.colony/bin/colony-hook" ] && exec "$HOME/.colony/bin/colony-hook"; exit 0';
  const approveCmd = '[ -f "$HOME/.colony/bin/colony-approve.sh" ] && exec sh "$HOME/.colony/bin/colony-approve.sh"; exit 0';
  const hooks = Object.fromEntries(events.map((e) => [e, entry(hooksCmd, 5)]));
  hooks.PermissionRequest = entry(approveCmd, 600);
  writeFileSync(join(dir, "wsl-settings.json"), JSON.stringify({ hooks }, null, 2));
  const sh = `set -e
rm -rf ${WSL_HOME}; mkdir -p ${WSL_HOME}/.colony ${WSL_HOME}/.claude
cp -r "${real}/.colony/bin" ${WSL_HOME}/.colony/bin
cp "${real}/.claude.json" ${WSL_HOME}/.claude.json
ln -s "${real}/.claude/.credentials.json" ${WSL_HOME}/.claude/.credentials.json
cp ${toWsl(join(dir, "wsl-settings.json"))} ${WSL_HOME}/.claude/settings.json
`;
  writeFileSync(join(dir, "wsl-setup.sh"), sh);
  const r = wsl("sh", toWsl(join(dir, "wsl-setup.sh")));
  if (r.status !== 0) throw new Error("WSL setup failed: " + r.stderr);
  return real;
}

function startWslConsole(dir, real, sid, prompt) {
  const script = join(dir, `wsl-run-${sid.slice(-6)}.sh`);
  writeFileSync(script, `export HOME=${WSL_HOME}\ncd "${real}"\nexec "${real}/.local/bin/claude" --session-id ${sid} --model haiku --permission-mode default "${prompt}"\n`);
  return spawn("cmd.exe", [`/c start "colony-accept-wsl" wsl.exe -d ${DISTRO} -e sh ${toWsl(script)}`], { env: process.env, stdio: "ignore", windowsVerbatimArguments: true });
}

async function realWsl() {
  if (wsl("sh", "-c", "command -v claude || test -x ~/.local/bin/claude && echo ok").status !== 0) return report("real Claude Code in WSL", false, `no claude in ${DISTRO}`);
  // Without COLONY_INGEST, colonyd runs its own WSL supervisor: it starts the probe in the (running)
  // distro and relays approvals. HOME for the probe is the isolated one, carried by WSLENV.
  const stage = mkdtempSync(join(tmpdir(), "colony-accept-wslfiles-"));
  const real = setupWslHome(stage); // before the daemon, so the probe it starts finds its HOME
  const d = await startDaemon({ real: true, wslHome: WSL_HOME });
  const map = await connectMap(d);
  const sid = newSid();
  const first = [], porch = [], crash = [];
  try {
    // The supervisor attaches within its rescan interval; the probe replays what it finds.
    const t0 = now();
    startWslConsole(stage, real, sid, TOOL_PROMPT);
    const tVisible = await map.until(bySession(sid), 120000, "(WSL session appears)");
    first.push(tVisible - t0);
    const a0 = bySession(sid)(map.agents);
    report("WSL session is tagged with its distro", String(a0.host) === `wsl:${DISTRO}`, `host ${a0.host}`);
    const r = await answerPorch(map, null, sid, "allow");
    const tAsk = map.stateAt.get(sid + ":permission");
    porch.push(r.clearedMs);
    const ran = await waitFor(() => bySession(sid)(map.agents)?.state !== "needs_input", 60000);
    report("WSL: permission prompt -> porch -> Allow (probe relay, colony-approve.sh)", ran, `tool ${r.tool}, cleared ${r.clearedMs.toFixed(0)} ms after the answer, state now ${bySession(sid)(map.agents)?.state}`);
    // Wait for the turn to finish, then kill the process inside the distro.
    await waitFor(() => ["idle", "ready_to_review"].includes(bySession(sid)(map.agents)?.state), 90000);
    const pid = bySession(sid)(map.agents).pids?.[0];
    const tk = now();
    wsl("kill", "-9", String(pid));
    crash.push((await map.until((a) => bySession(sid)(a)?.state === "crashed", 60000, "(WSL crashed)")) - tk);
    void tAsk;
  } finally {
    map.ws.close();
    d.stop();
    if (!process.env.KEEP) wsl("rm", "-rf", WSL_HOME);
    if (!process.env.KEEP) try { rmSync(stage, { recursive: true, force: true }); } catch {}
  }
  reportTimes("real WSL: session visible after launch (< 2000 ms after the supervisor's rescan)", first, 20000);
  reportTimes("real WSL: Allow clears the porch (< 1000 ms)", porch, 1000);
  reportTimes("real WSL: killed terminal shows crashed (< 5000 ms)", crash, 5000);
}

const steps = { latency, failopen, load, fps };
const manual = { "real-windows": realWindows, "real-colony": realColony, "real-wsl": realWsl, reap: async () => reap(), real: async () => { await realWindows(); await realColony(); await realWsl(); } };
const which = process.argv[2];
if (manual[which]) {
  try { await manual[which](); } finally { reap(); }
  process.exit(0);
}
if (!which || (which !== "all" && !steps[which])) {
  console.error("usage: node scripts/acceptance.mjs latency|failopen|load|fps|all\n       node scripts/acceptance.mjs real-windows|real-colony|real-wsl|real|reap   (real Claude Code)");
  process.exit(2);
}
for (const n of which === "all" ? Object.keys(steps) : [which]) {
  console.log(`\n== ${n}`);
  await steps[n]();
}
process.exit(0);
