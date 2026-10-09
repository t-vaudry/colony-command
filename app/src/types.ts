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

export type PermissionChoice = "allow" | "allow_always" | "deny" | "pass";

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
