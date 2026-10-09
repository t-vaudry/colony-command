// MVP acceptance measurements (design spec section 12). Results: docs/acceptance.md.
//
//     node scripts/acceptance.mjs latency   # first prompt -> map, permission -> porch, kill -> crashed
//     node scripts/acceptance.mjs failopen  # hooks with no daemon: time, exit code, output
//     node scripts/acceptance.mjs load      # colonyd CPU and memory, idle and under a 50-session load
//     node scripts/acceptance.mjs fps       # 50 synthetic sessions in headless Chrome (needs app/node_modules)
//     node scripts/acceptance.mjs all
//
// Needs release binaries: cargo build --release -p colonyd -p colony-hook -p colony-synth
// Everything runs against a throwaway colonyd with its own COLONY_HOME and
// USERPROFILE in a temp folder, on a free port, started with COLONY_INGEST=1.
// It never reads or touches the real ~/.colony or ~/.claude or the real colonyd.

import { spawn, spawnSync, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir, cpus } from "node:os";
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

/** An isolated colonyd. `env` for anything that should talk to it. */
async function startDaemon() {
  const dir = mkdtempSync(join(tmpdir(), "colony-accept-"));
  const home = join(dir, ".colony");
  mkdirSync(home, { recursive: true });
  mkdirSync(join(dir, ".claude", "sessions"), { recursive: true });
  const port = await freePort();
  const env = { ...process.env, COLONY_HOME: home, COLONY_PORT: String(port), COLONY_INGEST: "1", USERPROFILE: dir, HOME: dir };
  const child = spawn(bin("colonyd"), [], { env, stdio: "ignore" });
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
    stop() {
      child.kill();
      try { rmSync(dir, { recursive: true, force: true }); } catch {}
    },
  };
}

/** A map: WebSocket client keeping the agent table and the time of each change. */
async function connectMap(d) {
  const ws = new WebSocket(`ws://127.0.0.1:${d.port}/ws?token=${d.token}`);
  const map = { agents: new Map(), waiters: [], ws };
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.type === "snapshot") msg.agents.forEach((a) => map.agents.set(a.id, a));
    else if (msg.type === "upsert") map.agents.set(msg.agent.id, msg.agent);
    else if (msg.type === "remove") map.agents.delete(msg.id);
    else return;
    map.waiters = map.waiters.filter((w) => !w());
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
    const synth = spawn(bin("colony-synth"), ["--home", d.home, "--agents", "50", "--speed", "1", "--duration", "120"], { stdio: "ignore" });
    await sleep(2000);
    row("50 sessions at human pace (synth --speed 1), 60 s", await sample(d, 60));
    await new Promise((r) => synth.on("close", r));
    await sleep(3000);
    const idle = await sample(d, 60);
    row("50 sessions on the map, nothing happening, 60 s", idle);
    report("daemon idle: < 1% CPU and < 80 MB RAM", idle.core < 1 && idle.ws < 80, `${idle.core.toFixed(2)}% of a core, ${idle.ws.toFixed(1)} MB`);
    const synth5 = spawn(bin("colony-synth"), ["--home", d.home, "--agents", "50", "--speed", "5", "--duration", "40"], { stdio: "ignore" });
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
  const synth = spawn(bin("colony-synth"), ["--home", d.home, "--agents", "50", "--speed", "1"], { stdio: "ignore" });
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
    ws.onmessage = (m) => { const x = JSON.parse(m.data); pending.get(x.id)?.(x); };
    const cdp = (method, params = {}) => new Promise((res) => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
    await cdp("Page.enable");
    await cdp("Page.navigate", { url: `http://127.0.0.1:${vitePort}/` });
    // Wait for 50 bots on the map (the HUD counts them), then measure.
    const ev = async (expression) => (await cdp("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;
    await sleep(15000);
    const meter = `new Promise(res=>{const t=[];let last=performance.now(),end=last+15000;function f(n){t.push(n-last);last=n;if(n<end)requestAnimationFrame(f);else res(t)}requestAnimationFrame(f)})`;
    const times = await ev(meter);
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
    report("50 sessions render at 60 fps", avg >= 58, `${avg.toFixed(1)} fps average`);
    ws.close();
  } finally {
    synth.kill(); browser.kill(); vite.kill(); d.stop();
    try { rmSync(profile, { recursive: true, force: true }); } catch {}
  }
}

const steps = { latency, failopen, load, fps };
const which = process.argv[2];
if (!which || (which !== "all" && !steps[which])) {
  console.error("usage: node scripts/acceptance.mjs latency|failopen|load|fps|all");
  process.exit(2);
}
for (const n of which === "all" ? Object.keys(steps) : [which]) {
  console.log(`\n== ${n}`);
  await steps[n]();
}
process.exit(0);
