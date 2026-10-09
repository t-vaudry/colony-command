// What a session is waiting on, drawn large enough to read: a question's
// options, a command or diff awaiting permission, or the full text of a
// question Claude asked at the end of a turn.

import type { Agent } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

export interface QuestionOption {
  label: string;
  description?: string;
}

export interface Question {
  question: string;
  header?: string;
  options: QuestionOption[];
  multiSelect?: boolean;
}

/** What's been picked for one question: option labels plus free text. */
export interface Pick {
  labels: string[];
  other: string;
}

/** The questions an AskUserQuestion request carries, if it is one. */
export function questionsOf(a: Agent): Question[] {
  const p = a.permission;
  if (!p || p.tool !== "AskUserQuestion") return [];
  const qs = (p.input as { questions?: unknown } | null | undefined)?.questions;
  if (!Array.isArray(qs)) return [];
  return qs
    .filter((q): q is Question => !!q && typeof q.question === "string")
    .map((q) => ({ ...q, options: Array.isArray(q.options) ? q.options : [] }));
}

/** The answer for one question: the free text if given, else the picked labels. */
function answerFor(p: Pick | undefined): string {
  if (!p) return "";
  return p.other.trim() || p.labels.join(", ");
}

/** Answers keyed by question text, or null until every question has one. */
export function answersFrom(qs: Question[], picks: Pick[]): Record<string, string> | null {
  const out: Record<string, string> = {};
  for (let i = 0; i < qs.length; i++) {
    const a = answerFor(picks[i]);
    if (!a) return null;
    out[qs[i].question] = a;
  }
  return out;
}

const pre = (text: string, cls = "") => `<pre class="ask-block ${cls}">${esc(text)}</pre>`;
const str = (v: unknown) => (typeof v === "string" ? v : v == null ? "" : JSON.stringify(v, null, 2));

/** The tool call itself: what it will run, change, or write. */
function toolDetail(tool: string, input: Record<string, unknown> | null | undefined, target: string | null): string {
  const i = input ?? {};
  const path = str(i.file_path ?? i.notebook_path ?? i.path);
  switch (tool) {
    case "Bash":
    case "PowerShell":
      return pre(str(i.command), "cmd") + (i.description ? `<p class="muted small">${esc(str(i.description))}</p>` : "");
    case "Edit":
      return `<div class="k">${esc(path)}</div><div class="k">Replace</div>${pre(str(i.old_string), "del")}<div class="k">With</div>${pre(str(i.new_string), "add")}`;
    case "MultiEdit": {
      const edits = Array.isArray(i.edits) ? (i.edits as Record<string, unknown>[]) : [];
      return `<div class="k">${esc(path)}</div>` + edits.map((e) => pre(str(e.old_string), "del") + pre(str(e.new_string), "add")).join("");
    }
    case "Write":
      return `<div class="k">${esc(path)}</div>${pre(str(i.content), "add")}`;
    default: {
      const keys = Object.keys(i);
      if (!keys.length) return target ? pre(target) : "";
      return pre(JSON.stringify(i, null, 2));
    }
  }
}

function questionCard(a: Agent, qs: Question[], picks: Pick[]): string {
  const req = esc(a.permission!.request_id);
  const ready = answersFrom(qs, picks) !== null;
  return `<div class="choice ask big" role="group" aria-label="Question from Claude">
    <p class="ask-title"><b>${esc(a.name)} is asking${qs.length > 1 ? ` ${qs.length} questions` : ""}</b></p>
    ${qs
      .map((q, qi) => {
        const pick = picks[qi];
        return `<fieldset class="q">
          <legend>${q.header ? `<span class="chip-h">${esc(q.header)}</span>` : ""}</legend>
          <p class="q-text">${esc(q.question)}</p>
          ${q.multiSelect ? `<p class="muted small">Choose any that apply.</p>` : ""}
          <div class="opts">${q.options
            .map((o) => {
              const on = pick?.labels.includes(o.label);
              return `<button type="button" class="opt${on ? " on" : ""}" aria-pressed="${!!on}" data-pick="${req}" data-q="${qi}" data-label="${esc(o.label)}" data-multi="${q.multiSelect ? 1 : ""}">
                <span class="opt-label">${esc(o.label)}</span>${o.description ? `<span class="opt-desc">${esc(o.description)}</span>` : ""}
              </button>`;
            })
            .join("")}</div>
          <input type="text" class="other" data-other="${qi}" data-req="${req}" placeholder="Or type your own answer…" value="${esc(pick?.other ?? "")}" />
        </fieldset>`;
      })
      .join("")}
    <div class="actions">
      <button type="button" class="primary" data-answer="${req}"${ready ? "" : " disabled"}>Send answer${qs.length > 1 ? "s" : ""}</button>
      <button type="button" data-terminal-answer="${req}" title="Show the question in the session's own terminal and answer it there">Answer in terminal</button>
      <button type="button" class="danger" data-decide="deny" data-req="${req}" title="Skip the question and let Claude carry on">Skip</button>
    </div>
  </div>`;
}

/** The big card for whatever the agent is waiting on, or "" when nothing is. */
export function askCard(a: Agent, picks: Pick[]): string {
  const p = a.permission;
  if (p) {
    const qs = questionsOf(a);
    if (qs.length) return questionCard(a, qs, picks);
    const req = esc(p.request_id);
    return `<div class="choice ask big" role="group" aria-label="Permission request">
      <p class="ask-title"><b>${esc(a.name)} wants to use ${esc(p.tool)}</b></p>
      ${toolDetail(p.tool, p.input, p.target)}
      <div class="actions">
        <button type="button" class="primary" data-decide="allow" data-req="${req}">Allow</button>
        <button type="button" data-decide="allow_always" data-req="${req}" title="Allow, and add Claude Code's suggested rule so it won't ask again for this">Always allow</button>
        <button type="button" class="danger" data-decide="deny" data-req="${req}">Deny</button>
      </div>
    </div>`;
  }
  if (a.state === "awaiting_reply" && (a.last_message_full || a.last_message)) {
    return `<div class="choice ask big" role="group" aria-label="Question from Claude">
      <p class="ask-title"><b>${esc(a.name)} asked</b></p>
      <div class="ask-message">${esc(a.last_message_full ?? a.last_message ?? "")}</div>
      <p class="muted small">Answer in the box below.</p>
    </div>`;
  }
  return "";
}

/** Whether the inspector should open wide for this agent. */
export function needsWide(a: Agent | undefined): boolean {
  return !!a && (!!a.permission || a.state === "awaiting_reply");
}
