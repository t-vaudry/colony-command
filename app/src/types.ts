// Mirrors colony-core's serialized `Agent` (crates/colony-core/src/state.rs).

export type AgentState =
  | "spawning"
  | "working"
  | "needs_input"
  | "awaiting_reply"
  | "blocked"
  | "ready_to_review"
  | "idle"
  | "crashed"
  | "ended";

export interface CurrentTool {
  name: string;
  target: string | null;
  tool_use_id: string | null;
  started_at: number;
}

export interface Agent {
  id: string;
  session_id: string;
  parent_id: string | null;
  kind: "main" | "subagent";
  name: string;
  host: string;
  cwd: string | null;
  project_key: string | null;
  project_name: string | null;
  title: string | null;
  entrypoint: string | null;
  version: string | null;
  pid: number | null;
  /** Every live process registered for this session (two when it's open in two places). */
  pids: number[];
  /** The registered process in Colony's terminal: this session's own copy. */
  terminal_pid: number | null;
  subagent_type: string | null;
  state: AgentState;
  state_since: number;
  reason: string | null;
  objective: string | null;
  last_prompt: string | null;
  last_message: string | null;
  /** The whole last message, for reading a question in full. */
  last_message_full?: string | null;
  current_tool: CurrentTool | null;
  tool_calls: number;
  consecutive_failures: number;
  children: string[];
  last_event_at: number;
  hooks_seen: boolean;
  /** Folder the session started in; decides its district. */
  project_dir: string | null;
  /** Set when Colony started this session in a terminal it owns. */
  terminal: string | null;
  /** A permission request Colony is holding for the map to answer. */
  permission: PermissionAsk | null;
  /** The model the session runs, e.g. "claude-opus-5-5", when known. */
  model: string | null;
  /** A model better suited to what it's been doing lately. */
  model_hint: ModelHint | null;
  /** A tool failed on a missing login or program; the map offers to sign in or install. */
  auth_need?: { kind: "sign_in" | "install"; provider: string; label: string } | null;
  /** Tokens used so far (subagents included on a main agent). Absent from older daemons. */
  tokens?: Tokens;
  /** Colony stopped this session between turns; it shows idle until resumed. */
  paused_at?: number | null;
  /** A pause waits for the current turn to end. */
  pause_pending?: boolean;
  /** Estimated cost in USD at list prices. */
  cost_usd?: number;
  /** Some tokens were from a model without a known price, so the cost is low. */
  cost_partial?: boolean;
  /** Folder it is working in, relative to its project folder, rolled up (`src/auth`; "" is the project folder). */
  work_dir?: string | null;
  /** Another agent is editing the same file or folder. A warning only; fades on its own. */
  collision?: Collision | null;
  /** Files and lines changed, once ready to review and counted. Absent when git couldn't say. */
  diff_stat?: DiffStat | null;
}

export interface Collision {
  scope: "file" | "dir";
  /** The file, or the folder, as the tool call named it. */
  path: string;
  /** The other agents' ids. */
  with: string[];
  at: number;
}

export interface DiffStat {
  files: number;
  added: number;
  removed: number;
}

/** "3 files · +40 −7" */
export function diffText(d: DiffStat): string {
  return `${d.files} file${d.files === 1 ? "" : "s"} · +${d.added} −${d.removed}`;
}

export interface ModelHint {
  /** What to pass to /model, e.g. "haiku". */
  model: string;
  /** Button name, e.g. "Haiku". */
  label: string;
  reason: string;
}

/** Models offered in the map, as [value for --model or /model, label]. */
export const MODELS: [string, string][] = [
  ["opus", "Opus 5.5"],
  ["sonnet", "Sonnet 5.5"],
  ["haiku", "Haiku 5.5"],
  ["claude-fable-5-1", "Fable 5.1"],
];

/** "claude-opus-5-5" -> "Opus 5.5"; aliases and unknown ids pass through readably. */
export function modelLabel(model: string | null): string | null {
  if (!model) return null;
  const m = model.match(/(opus|sonnet|haiku|fable)[-_ ]?(\d+)?[-_.]?(\d+)?/i);
  if (!m) return model;
  const name = m[1][0].toUpperCase() + m[1].slice(1).toLowerCase();
  return m[2] ? `${name} ${m[2]}${m[3] ? "." + m[3] : ""}` : name;
}

export interface PermissionAsk {
  /** The tool call's input (long strings trimmed): a question's options, a diff, a command. */
  input?: Record<string, unknown> | null;
  request_id: string;
  tool: string;
  target: string | null;
  asked_at: number;
}

export type PermissionChoice = "allow" | "allow_always" | "allow_project" | "deny" | "pass";

/** What "Allow always for project" would save for a held request. */
export interface Offer {
  /** Claude Code's own spelling, e.g. `Bash(npm test)`. */
  rules: string[];
  project: string | null;
}

/** A saved "allow always for project" rule. */
export interface Rule {
  id: string;
  project_key: string;
  project_name: string;
  tool: string;
  content?: string | null;
  created_at: number;
}

/** `Bash(npm test)` for a rule that has a pattern, else just the tool. */
export const ruleLabel = (r: Rule) => (r.content ? `${r.tool}(${r.content})` : r.tool);

/** What the state pill says: pause overrides plain idle. */
export function stateLabel(a: Agent): string {
  if (a.paused_at) return "paused";
  if (a.pause_pending) return `${STATE_LABEL[a.state]}, pausing after this turn`;
  return STATE_LABEL[a.state];
}

/** Where Colony can start sessions. */
export interface HostOption {
  id: string;
  label: string;
  available: boolean;
  note?: string;
}

export interface SpawnRequest {
  host: string;
  dir: string;
  prompt?: string;
  resume?: string;
  name?: string;
  permission_mode?: string;
  chrome?: boolean;
  model?: string;
  /** Start in its own git worktree and branch. */
  isolate?: boolean;
  cols?: number;
  rows?: number;
}

export type Severity = "critical" | "input" | "review";

export function severity(s: AgentState): Severity | null {
  switch (s) {
    case "blocked":
    case "crashed":
      return "critical";
    case "needs_input":
    case "awaiting_reply":
      return "input";
    case "ready_to_review":
      return "review";
    default:
      return null;
  }
}

export const STATE_LABEL: Record<AgentState, string> = {
  spawning: "starting",
  working: "working",
  needs_input: "needs permission",
  awaiting_reply: "has a question",
  blocked: "blocked",
  ready_to_review: "ready to review",
  idle: "idle",
  crashed: "crashed",
  ended: "ended",
};

/** A worktree (or just its branch) Colony couldn't clean up after its bot was done. */
export interface Leftover {
  session_id: string;
  host: string;
  repo: string;
  path: string;
  branch: string;
  /** Why it was kept, e.g. "Has uncommitted changes". */
  kept: string | null;
  /** The folder is gone; only the branch, with unmerged commits, remains. */
  folder_gone: boolean;
}

export interface Tokens {
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
}

/** Spend in one 15-minute ledger bucket. */
export interface Spend {
  tokens: Tokens;
  cost_usd: number;
  /** Includes a model without a known price: the cost is low. */
  partial: boolean;
}

export interface ProjectSpend {
  name: string;
  /** Bucket number (unix ms / BUCKET_MS) to the spend in it. */
  buckets: Record<string, Spend>;
}

export const BUCKET_MS = 15 * 60_000;

/** An estimate, so always shown with "~". */
export function money(usd: number): string {
  if (usd > 0 && usd < 0.01) return "~<$0.01";
  return `~$${usd.toFixed(2)}`;
}

/** 1234 -> "1.2k", 3_400_000 -> "3.4M". */
export function compact(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** What a project (or everything, with no key) spent since local midnight. */
export function spentToday(spend: Map<string, ProjectSpend>, now: number, key?: string): { usd: number; partial: boolean; tokens: number } {
  const midnight = new Date(now);
  midnight.setHours(0, 0, 0, 0);
  const from = midnight.getTime() / BUCKET_MS;
  const out = { usd: 0, partial: false, tokens: 0 };
  for (const [k, p] of spend) {
    if (key !== undefined && k !== key) continue;
    for (const [b, s] of Object.entries(p.buckets)) {
      if (Number(b) < from) continue;
      out.usd += s.cost_usd;
      out.partial ||= s.partial;
      out.tokens += s.tokens.input + s.tokens.output + s.tokens.cache_read + s.tokens.cache_creation;
    }
  }
  return out;
}
