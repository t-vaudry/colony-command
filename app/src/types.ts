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
  subagent_type: string | null;
  state: AgentState;
  state_since: number;
  reason: string | null;
  objective: string | null;
  last_prompt: string | null;
  last_message: string | null;
  current_tool: CurrentTool | null;
  tool_calls: number;
  consecutive_failures: number;
  children: string[];
  last_event_at: number;
  hooks_seen: boolean;
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
