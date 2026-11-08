// Runs a compiled workflow script with stubbed agent(), phase() and log(), and prints what happened.
import { readFileSync } from "node:fs";
const [file, scenarioJson] = process.argv.slice(2);
const sc = JSON.parse(scenarioJson);
const src = readFileSync(file, "utf8").replace("export const meta =", "const meta =");
const calls = [];
const phases = [];
const logs = [];
const loopAnswers = [...(sc.loop || [])];
const branchAnswers = [...(sc.branch || [])];
const agent = async (prompt, opts = {}) => {
  const label = opts.label || "";
  calls.push({ label, model: opts.model || null, phase: opts.phase || null, schema: !!opts.schema });
  if (label.startsWith("skip?")) return { skip: !!(sc.skip || {})[label.slice(6)] };
  if (label.startsWith("approval?")) return { needed: (sc.need || {})[label.slice(10)] !== false };
  if (label.startsWith("loop check")) return { met: loopAnswers.length ? loopAnswers.shift() : true };
  if (label.startsWith("branch after") || label.startsWith("next after")) return { choice: branchAnswers.length ? branchAnswers.shift() : 0 };
  const name = label.replace(/ \(round \d+\)$/, "");
  const blocked = (sc.blocked || {})[name];
  if (blocked && !(sc.answers || {})[name]) return "BLOCKED: " + blocked;
  return "done";
};
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const body = new AsyncFunction("agent", "phase", "log", "args", src);
const result = await body(agent, (t) => phases.push(t), (m) => logs.push(m), sc.args);
console.log(JSON.stringify({ result, calls, phases, logs }));
