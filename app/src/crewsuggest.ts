// Which crew the New session dialog preselects. `suggest` mirrors
// colony-core's `crew::suggest` (crates/colony-core/src/crew.rs); both are
// tested against crates/colony-core/tests/crew-suggest.json, so the word
// lists below must stay in step with the Rust ones.

export interface Suggestion {
  crew: "light" | "default";
  why: string;
}

const BIG_STEMS = ["refactor", "migrat", "redesign", "design", "architect", "rewrit", "overhaul", "flak", "intermittent", "deadlock", "concurren", "scalab"];
const BIG_PHRASES = ["across the", "from scratch", "all the files", "all files", "every file", "entire", "whole codebase", "race condition", "memory leak", "root cause", "end to end", "new feature", "randomly", "sometimes"];
const DOUBT_STEMS = ["break", "broke", "crash", "regress", "secur", "vulnerab", "pipeline", "whole", "fail", "outage"];
const DOUBT_WORDS = ["production", "prod", "auth", "payment", "payments", "billing", "database", "db"];
const DOUBT_PHRASES = ["doesn't work", "doesnt work", "not working", "isn't working", "stopped working"];
const SMALL_STEMS = ["typo", "misspell", "spelling", "rename", "bump", "lint", "eslint", "clippy", "prettier", "rustfmt", "whitespace", "indent", "docstring", "changelog"];
const SMALL_WORDS = ["comment", "comments"];
const SMALL_PHRASES = ["one line", "one-line", "single line", "1 line", "unused import", "unused variable", "log line", "update the version"];
const QUESTION_STARTS = ["what", "whats", "why", "how", "where", "which", "who", "when", "explain", "describe", "summarize", "summarise", "tell", "walk"];
const ACTION_STEMS = ["fix", "implement", "chang", "updat", "writ", "creat", "remov", "delet", "edit", "generat", "convert", "install", "deploy"];
const ACTION_WORDS = ["add", "make", "run", "set", "put", "move"];
const SINGLE_FILE_VERBS = ["change", "set", "replace", "swap", "tweak", "update", "edit", "rename", "remove"];
const FILE_EXTS = ["rs", "ts", "tsx", "js", "jsx", "json", "toml", "yaml", "yml", "md", "py", "go", "java", "css", "html", "cfg", "ini", "txt", "lock", "sh", "cpp", "cs", "rb"];
const LONG_CHARS = 600;
const LONG_LINES = 8;
const SMALL_MAX_CHARS = 300;
const SINGLE_FILE_MAX_CHARS = 200;

const hasStem = (words: string[], stems: string[]) => words.some((w) => stems.some((s) => w.startsWith(s)));
const hasWord = (words: string[], list: string[]) => words.some((w) => list.includes(w));

function fileTokens(text: string): number {
  return text.split(/\s+/).filter((t) => {
    const trimmed = t.replace(/^["'`,;:()?!.]+|["'`,;:()?!.]+$/g, "");
    const dot = trimmed.lastIndexOf(".");
    return dot > 0 && FILE_EXTS.includes(trimmed.slice(dot + 1));
  }).length;
}

/** Which crew fits a first message, judged locally; null when unsure. */
export function suggest(prompt: string): Suggestion | null {
  const norm = prompt.toLowerCase().replace(/’/g, "'");
  const lower = norm.split(/\s+/).filter(Boolean).join(" ");
  if (!lower) return null;
  const words = lower.split(/[^a-z0-9]+/).filter(Boolean);
  const chars = [...lower].length;
  const files = fileTokens(lower);
  const sug = (crew: Suggestion["crew"], why: string): Suggestion => ({ crew, why });

  if (chars > LONG_CHARS || norm.split("\n").filter((l) => l.trim()).length > LONG_LINES) return sug("default", "a long brief usually means a big job");
  if (hasStem(words, BIG_STEMS) || BIG_PHRASES.some((p) => lower.includes(p))) return sug("default", "looks like a large or open-ended job");
  if (files >= 3) return sug("default", "touches several files");
  const doubt = hasStem(words, DOUBT_STEMS) || hasWord(words, DOUBT_WORDS) || DOUBT_PHRASES.some((p) => lower.includes(p));
  const acts = hasStem(words, ACTION_STEMS) || hasWord(words, ACTION_WORDS);
  const asks = QUESTION_STARTS.includes(words[0]) || lower.endsWith("?");
  if (asks && !acts) {
    if (hasWord(words, ["codebase", "overview", "repo", "repository"]) || lower.includes("this project")) return sug("default", "asks about the whole codebase");
    return doubt ? null : sug("light", "looks like a question");
  }
  if (doubt) return null;
  if (chars <= SMALL_MAX_CHARS && (hasStem(words, SMALL_STEMS) || hasWord(words, SMALL_WORDS) || SMALL_PHRASES.some((p) => lower.includes(p)))) {
    return sug("light", "looks like a small, mechanical fix");
  }
  if (chars <= SINGLE_FILE_MAX_CHARS && files === 1 && hasWord(words, SINGLE_FILE_VERBS)) return sug("light", "touches one file");
  return null;
}

export type CrewSource = "picked" | "folder" | "suggested" | "remembered" | "default";

export interface CrewInputs {
  /** What the user chose in this dialog by touching the select; null until then. */
  picked: string | null;
  /** The crew set as this folder's default. */
  folderDefault: string | null;
  /** The heuristic's suggestion for the first message. */
  suggestion: Suggestion | null;
  /** The crew picked last time. */
  remembered: string | null;
  /** Crews that exist; anything else is skipped. */
  available: string[];
}

/**
 * What Start does to the folder's default crew. A box the dialog ticked itself
 * only shows the current default, so only the user's own tick or untick counts.
 */
export function folderDefaultAction(i: { touched: boolean; checked: boolean; hadDefault: boolean }): "set" | "clear" | "none" {
  if (!i.touched) return "none";
  if (i.checked) return "set";
  return i.hadDefault ? "clear" : "none";
}

/** The crew to preselect, and why: the user's pick, then the folder's default, the suggestion, the last pick. */
export function chooseCrew(i: CrewInputs): { crew: string; source: CrewSource } {
  const ok = (id: string | null): id is string => !!id && i.available.includes(id);
  if (ok(i.picked)) return { crew: i.picked, source: "picked" };
  if (ok(i.folderDefault)) return { crew: i.folderDefault, source: "folder" };
  if (i.suggestion && ok(i.suggestion.crew)) return { crew: i.suggestion.crew, source: "suggested" };
  if (ok(i.remembered)) return { crew: i.remembered, source: "remembered" };
  return { crew: "default", source: "default" };
}
