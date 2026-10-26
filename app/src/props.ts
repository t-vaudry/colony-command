// What a working bot holds, by the tool it is running.

/** look: magnifier, write: pencil, run: hammer, web: antenna, delegate: megaphone. */
export type Prop = "look" | "write" | "run" | "web" | "delegate";

const PROPS: Record<string, Prop> = {
  Read: "look",
  Grep: "look",
  Glob: "look",
  LS: "look",
  Edit: "write",
  MultiEdit: "write",
  Write: "write",
  NotebookEdit: "write",
  Bash: "run",
  PowerShell: "run",
  WebFetch: "web",
  WebSearch: "web",
  Agent: "delegate",
  Task: "delegate",
};

export function toolProp(name: string | null | undefined): Prop | null {
  if (!name) return null;
  // MCP tools talk to something outside the machine, like the web tools.
  if (name.startsWith("mcp__")) return "web";
  return PROPS[name] ?? null;
}
