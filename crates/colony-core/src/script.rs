//! Turn a Colony workflow into a Claude Code dynamic-workflow script.
//!
//! The script is the workflow's data (phases, branches, loops, skip conditions) as JSON,
//! followed by a fixed interpreter. Each phase runs as its own agent; small judge agents
//! decide skips, loop exits and branches. Phases hand work to each other through short notes
//! in the repository (`.colony/notes/`), never through the script, so an agent's context
//! stays small however long the workflow runs.
//!
//! Each phase agent starts with `echo colony-phase:<name>`, the same marker custom phases use,
//! so Colony's graph follows the run with documented hook events only.

use serde_json::{json, Value};

use crate::workflow::Workflow;

/// What the script needs to know about the session that runs it.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The user's request, when it is known at launch; else the script reads `args.task`.
    pub task: String,
    /// Model for the small judge agents (the crew's swarm model), or the session's own.
    pub judge_model: Option<String>,
    /// The session has an advisor: phase agents are told to use it.
    pub advisor: bool,
}

/// Where phases leave notes for each other.
pub const NOTES_DIR: &str = ".colony/notes";

/// The script for `w`. Fails only when the workflow has no phases.
pub fn compile(w: &Workflow, o: &Options) -> Result<String, String> {
    if w.phases.is_empty() {
        return Err("a workflow with no phases has nothing to run".into());
    }
    let index = |name: &str| w.phases.iter().position(|p| p.skill == name);
    let groups: Vec<Vec<usize>> = w.parallel.iter().map(|g| g.phases.iter().filter_map(|n| index(n)).collect()).collect();
    let phases: Vec<Value> = w
        .phases
        .iter()
        .map(|p| {
            let conditional: Vec<Value> = p
                .branches
                .iter()
                .filter(|b| !b.condition.is_empty())
                .filter_map(|b| index(&b.to).map(|to| json!({ "to": to, "cond": b.condition })))
                .collect();
            let otherwise = p.branches.iter().find(|b| b.condition.is_empty()).and_then(|b| index(&b.to));
            let next: Vec<usize> = p.next.iter().filter_map(|n| index(n)).collect();
            json!({
                "name": p.skill,
                "custom": !p.instruction.is_empty(),
                "text": p.instruction,
                "when": p.when,
                "skip": p.skip_when,
                "group": groups.iter().position(|g| index(&p.skill).is_some_and(|i| g.contains(&i))),
                "inline": p.inline,
                "approve": p.approve,
                "approveWhen": p.approve_when,
                "onDeny": if p.approve { index(&p.on_deny) } else { None },
                "next": next,
                "branches": conditional,
                "otherwise": otherwise,
            })
        })
        .collect();
    let loops: Vec<Value> = w
        .loops
        .iter()
        .filter_map(|l| Some(json!({ "from": index(&l.from)?, "to": index(&l.to)?, "until": l.until, "max": l.max })))
        .collect();
    let meta = json!({
        "name": format!("colony-{}", w.id),
        "description": w.label,
        "phases": w.phases.iter().map(|p| {
            let mut m = json!({ "title": p.skill });
            if !p.when.is_empty() {
                m["detail"] = json!(p.when);
            }
            m
        }).collect::<Vec<_>>(),
    });
    let s = |v: &Value| serde_json::to_string(v).expect("json");
    Ok(format!(
        "export const meta = {meta}\n\nconst TASK = {task}\nconst PHASES = {phases}\nconst LOOPS = {loops}\nconst GROUPS = {groups}\nconst JUDGE_MODEL = {judge}\nconst ADVISOR = {advisor}\nconst NOTES = {notes}\n{INTERPRETER}",
        meta = s(&meta),
        task = s(&json!(o.task)),
        phases = s(&Value::Array(phases)),
        loops = s(&Value::Array(loops)),
        groups = s(&json!(groups)),
        judge = s(&json!(o.judge_model)),
        advisor = o.advisor,
        notes = s(&json!(NOTES_DIR)),
    ))
}

const INTERPRETER: &str = r####"
const ANSWERS = (args && typeof args === 'object' && args.answers) || {}
const DONE = (args && typeof args === 'object' && args.done) || {}
const APPROVALS = (args && typeof args === 'object' && args.approvals) || {}
const GOAL = TASK || (typeof args === 'string' ? args : args && args.task) || ''
const slug = (s) => String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
const SKIP_SCHEMA = { type: 'object', properties: { skip: { type: 'boolean' }, reason: { type: 'string' } }, required: ['skip'] }
const NEED_SCHEMA = { type: 'object', properties: { needed: { type: 'boolean' }, reason: { type: 'string' } }, required: ['needed'] }
const MET_SCHEMA = { type: 'object', properties: { met: { type: 'boolean' }, reason: { type: 'string' } }, required: ['met'] }
const CHOICE_SCHEMA = { type: 'object', properties: { choice: { type: 'integer' }, reason: { type: 'string' } }, required: ['choice'] }
const judge = (label, phaseName, schema) => ({ label, phase: phaseName, schema, effort: 'low', ...(JUDGE_MODEL ? { model: JUDGE_MODEL } : {}) })

const noteFile = (step, name) => `${NOTES}/${String(step).padStart(2, '0')}-${slug(name)}.md`
const denies = (said) => /^(no|deny|denied|decline|declined|reject|rejected)\b/i.test(said)
// What the user said when they declined the phase before this one, handed to the phase that took over.
let declined = null
const brief = (p, step, round) => [
  `You are doing one phase, "${p.name}", of a multi-phase workflow. The overall task:`,
  GOAL,
  '',
  `Run this command first, as a Bash call by itself (never chained with another command), before anything else: echo colony-phase:${p.name}`,
  p.custom ? `Then do this: ${p.text}` : `Then use the Skill tool to run the skill "${p.name}" and follow it.`,
  p.when ? `(This phase is for when: ${p.when}.)` : '',
  p.group !== null ? `This phase runs at the same time as ${GROUPS[p.group].filter((m) => PHASES[m].name !== p.name).map((m) => `"${PHASES[m].name}"`).join(', ')}, in the same working folder. Keep to this phase's own job and do not edit files the others are likely to change; if you must, say so in your note.` : '',
  round > 1 ? `This is round ${round} of a repeating run: read the latest notes for what the last round found, and deal with it.` : '',
  `Earlier phases left short notes in ${NOTES}/, one file per phase run, numbered in order. Read the latest ones you need before you start: they are how phases hand work to each other. Use the Glob, Read and Write tools for notes, never shell commands (cd, ls, cat, mkdir, printf): the marker above is the only Bash command this phase needs for them.`,
  `Do only this phase's work; later phases handle the rest. When you finish, write a note of at most 15 lines to ${noteFile(step, p.name)} saying what you did, what you decided and what the next phase must know. Then reply with one sentence.`,
  declined ? `The user declined to start "${declined.phase}"${declined.why ? `, saying: ${declined.why}` : ''}. Take that into account.` : '',
  ANSWERS[p.name] ? `The user answered a question this phase stopped to ask: ${ANSWERS[p.name]}\nAn earlier attempt left a note of its progress at ${noteFile(step, p.name)}: read it first and carry on from there, keeping the work that is already done, instead of starting over.` : '',
  `If you cannot go on without the user's input, do not guess and do not stop quietly. First write down in that note file what you have done so far, what you found and what is still open, so the user can review it. Then reply starting with "BLOCKED:" followed by one clear question (offer the options you see, if there are any).`,
  ADVISOR ? 'You have an advisor tool: consult it before you finish if the work was not trivial.' : '',
].filter(Boolean).join('\n')

const rounds = LOOPS.map(() => 1)
const roundOf = (idx) => LOOPS.reduce((r, l, k) => (l.from <= idx && idx <= l.to ? Math.max(r, rounds[k]) : r), 1)

// Where the run goes after phase `i`: its conditions, else its followers, else the next one listed.
async function after(p, i) {
  const fallback = i + 1 < PHASES.length ? i + 1 : null
  if (p.branches.length) {
    const opts = p.branches.map((b, n) => `${n + 1}. ${b.cond} (goes to ${PHASES[b.to].name})`)
    const r = await agent(
      `Task: ${GOAL}\n\nThe phase "${p.name}" has just finished. Look at the latest notes in ${NOTES}/ and at the repository. Which of these conditions holds?\n${opts.join('\n')}\nAnswer with the number of the first condition that clearly holds, or 0 if none does.`,
      judge(`branch after ${p.name}`, p.name, CHOICE_SCHEMA),
    )
    const n = r && Number.isInteger(r.choice) ? r.choice : 0
    if (n >= 1 && n <= p.branches.length) return p.branches[n - 1].to
    return p.otherwise !== null ? p.otherwise : fallback
  }
  if (p.otherwise !== null) return p.otherwise
  if (p.next.length === 1) return p.next[0]
  if (p.next.length > 1) {
    const opts = p.next.map((t, n) => `${n + 1}. ${PHASES[t].name}${PHASES[t].when ? ` (for when: ${PHASES[t].when})` : ''}`)
    const r = await agent(
      `Task: ${GOAL}\n\nThe phase "${p.name}" has just finished. Look at the latest notes in ${NOTES}/ and at the repository. Which phase should come next?\n${opts.join('\n')}\nAnswer with its number.`,
      judge(`next after ${p.name}`, p.name, CHOICE_SCHEMA),
    )
    const n = r && Number.isInteger(r.choice) ? r.choice : 1
    return p.next[Math.min(Math.max(n, 1), p.next.length) - 1]
  }
  return fallback
}

const ran = []
const denied = []
const approved = new Set()
let blocked = null
let i = 0
let step = 0
let guard = 0
while (i !== null && i < PHASES.length && guard++ < 300) {
  let p = PHASES[i]
  let forced
  const group = p.group !== null && GROUPS[p.group][0] === i ? GROUPS[p.group] : null
  if (group) {
    // Phases that go side by side: judge them all, ask for the approvals together, run them at once, join.
    phase(p.name)
    const plan = (await parallel(group.map((m) => async () => {
      const q = PHASES[m]
      let skip = false
      let needed = !!q.approve && !approved.has(m)
      if (q.skip) {
        const r = await agent(
          `Task: ${GOAL}

Decide whether the phase "${q.name}" can be skipped. Skip it only if this clearly holds: ${q.skip}.
Look at the repository and the notes in ${NOTES}/ as needed. Answer skip=false if you are unsure.`,
          judge(`skip? ${q.name}`, q.name, SKIP_SCHEMA),
        )
        skip = !!(r && r.skip)
        if (skip) log(`Skipping ${q.name}: ${(r && r.reason) || q.skip}`)
      }
      if (!skip && needed && q.approveWhen) {
        const r = await agent(
          `Task: ${GOAL}

The phase "${q.name}" is about to start and needs the user's approval only if this holds: ${q.approveWhen}.
Look at the repository and the notes in ${NOTES}/ as needed. Answer needed=true if it holds or you are unsure.`,
          judge(`approval? ${q.name}`, q.name, NEED_SCHEMA),
        )
        needed = !r || r.needed !== false
      }
      return { m, skip, needed }
    }))).map((x, k) => x || { m: group[k], skip: false, needed: !!PHASES[group[k]].approve && !approved.has(group[k]) })
    const missing = []
    for (const x of plan) {
      if (x.skip || !x.needed) continue
      const said = String(APPROVALS[PHASES[x.m].name] === undefined ? '' : APPROVALS[PHASES[x.m].name]).trim()
      if (!said) missing.push(PHASES[x.m].name)
      else if (denies(said)) {
        x.skip = true
        if (!denied.includes(PHASES[x.m].name)) denied.push(PHASES[x.m].name)
        log(`The user declined ${PHASES[x.m].name}: it is left out`)
      } else approved.add(x.m)
    }
    if (missing.length) {
      blocked = { phase: missing[0], phases: missing, kind: 'approval', question: missing.length === 1 ? `May the phase "${missing[0]}" start?` : `May these phases start (they run together): ${missing.join(', ')}?` }
      break
    }
    const todo = plan.filter((x) => !x.skip)
    todo.forEach((x) => { x.step = ++step })
    const replies = await parallel(todo.map((x) => () => {
      const q = PHASES[x.m]
      const round = roundOf(x.m)
      return agent(brief(q, x.step, round), { label: round > 1 ? `${q.name} (round ${round})` : q.name, phase: q.name })
    }))
    todo.forEach((x, k) => {
      const reply = replies[k]
      const q = PHASES[x.m]
      if (reply === null || reply === undefined) blocked = blocked || { phase: q.name, note: noteFile(x.step, q.name), question: 'The agent for this phase did not finish.' }
      else if (typeof reply === 'string' && /^\s*BLOCKED:/i.test(reply)) blocked = blocked || { phase: q.name, note: noteFile(x.step, q.name), question: reply.replace(/^\s*BLOCKED:\s*/i, '').slice(0, 600) }
      else ran.push(q.name)
    })
    if (blocked) break
    // The group is done: carry on from its last phase, which says where the run goes next.
    i = group[group.length - 1]
    p = PHASES[i]
  } else {
  phase(p.name)
  let skipped = false
  if (p.skip) {
    const r = await agent(
      `Task: ${GOAL}\n\nDecide whether the phase "${p.name}" can be skipped. Skip it only if this clearly holds: ${p.skip}.\nLook at the repository and the notes in ${NOTES}/ as needed. Answer skip=false if you are unsure.`,
      judge(`skip? ${p.name}`, p.name, SKIP_SCHEMA),
    )
    skipped = !!(r && r.skip)
    if (skipped) log(`Skipping ${p.name}: ${(r && r.reason) || p.skip}`)
  }
  // A phase that needs the user's approval: ask first (when its condition holds), once per run.
  if (!skipped && p.approve && !approved.has(i)) {
    let needed = true
    if (p.approveWhen) {
      const r = await agent(
        `Task: ${GOAL}

The phase "${p.name}" is about to start and needs the user's approval only if this holds: ${p.approveWhen}.
Look at the repository and the notes in ${NOTES}/ as needed. Answer needed=true if it holds or you are unsure.`,
        judge(`approval? ${p.name}`, p.name, NEED_SCHEMA),
      )
      needed = !r || r.needed !== false
    }
    const said = needed ? String(APPROVALS[p.name] === undefined ? '' : APPROVALS[p.name]).trim() : 'approved'
    if (!said) {
      blocked = { phase: p.name, kind: 'approval', question: `May the phase "${p.name}" start?${p.approveWhen ? ` (${p.approveWhen})` : ''}` }
      break
    }
    if (denies(said)) {
      skipped = true
      forced = p.onDeny === null || denied.includes(p.name) ? null : p.onDeny
      if (!denied.includes(p.name)) denied.push(p.name)
      declined = { phase: p.name, why: said.replace(/^\w+\s*[:,-]?\s*/, '').slice(0, 400) }
      log(`The user declined ${p.name}${forced === null ? ': ending the run' : `: going to ${PHASES[forced].name}`}`)
    } else {
      approved.add(i)
    }
  }
  if (!skipped && p.inline) {
    // A phase for the main session (its skill starts agents of its own, which a phase agent can't): stop and hand it over.
    step++
    const round = roundOf(i)
    const key = round > 1 ? `${p.name} (round ${round})` : p.name
    declined = null
    const finished = DONE[key]
    if (finished === undefined || finished === null || String(finished).trim() === '') {
      const file = `${NOTES}/${String(step).padStart(2, '0')}-${slug(p.name)}.md`
      blocked = {
        phase: p.name, kind: 'inline', key, note: file,
        question: [
          `Run the phase "${p.name}" yourself, in this session, not in an agent. The overall task: ${GOAL}`,
          `Start with this command, as a Bash call by itself: echo colony-phase:${p.name}`,
          p.custom ? `Do this: ${p.text}` : `Use the Skill tool to run the skill "${p.name}" and follow it.`,
          p.when ? `(This phase is for when: ${p.when}.)` : '',
          round > 1 ? `This is round ${round} of a repeating run: read the latest notes for what the last round found, and deal with it.` : '',
          `First read the latest notes in ${NOTES}/. When the phase is done, write a note of at most 15 lines to ${file} saying what you did, what you decided and what the next phase must know.`,
        ].filter(Boolean).join(' '),
      }
      break
    }
    ran.push(p.name)
  } else if (!skipped) {
    step++
    const round = roundOf(i)
    const note = brief(p, step, round)
    declined = null
    const reply = await agent(note, { label: round > 1 ? `${p.name} (round ${round})` : p.name, phase: p.name })
    if (reply === null || reply === undefined) {
      blocked = { phase: p.name, note: noteFile(step, p.name), question: 'The agent for this phase did not finish.' }
      break
    }
    if (typeof reply === 'string' && /^\s*BLOCKED:/i.test(reply)) {
      blocked = { phase: p.name, note: noteFile(step, p.name), question: reply.replace(/^\s*BLOCKED:\s*/i, '').slice(0, 600) }
      break
    }
    ran.push(p.name)
  }
  }
  if (forced !== undefined) {
    i = forced
    continue
  }
  // A repeating run that ends here: go round again unless its exit condition holds. Innermost first.
  let next
  const ends = LOOPS.map((l, k) => k).filter((k) => LOOPS[k].to === i).sort((a, b) => (LOOPS[a].to - LOOPS[a].from) - (LOOPS[b].to - LOOPS[b].from))
  for (const k of ends) {
    const l = LOOPS[k]
    if (rounds[k] >= l.max) continue
    const r = await agent(
      `Task: ${GOAL}\n\nThe phase "${p.name}" has just ended round ${rounds[k]} (of at most ${l.max}) of a repeating run from "${PHASES[l.from].name}" to "${PHASES[l.to].name}". ${l.until ? `Has this been achieved: ${l.until}?` : 'Does the work need another round?'} Look at the latest notes in ${NOTES}/ and at the repository. ${l.until ? 'Answer met=true only if it clearly has.' : 'Answer met=true if no further round is needed.'}`,
      judge(`loop check after ${p.name}`, p.name, MET_SCHEMA),
    )
    if (r && r.met) continue
    rounds[k]++
    LOOPS.forEach((m, j) => { if (j !== k && m.from >= l.from && m.to <= l.to) rounds[j] = 1 })
    next = l.from
    break
  }
  if (next === undefined) next = await after(p, i)
  i = next
}
if (guard >= 300) log('Stopped after 300 steps: the workflow kept going round.')
return { ran, blocked, denied, rounds }
"####;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::parse;

    fn w(json: &str) -> Workflow {
        parse("flow", json).unwrap()
    }

    #[test]
    fn data_comes_first_as_literals() {
        let wf = w(r#"{"label":"Flow","phases":[{"skill":"plan","when":"unclear"},{"skill":"build","instruction":"Make it.","skip_when":"nothing to build"}],"loops":[{"from":"plan","to":"build","until":"it works","max":3}]}"#);
        let s = compile(&wf, &Options { task: "do \"it\"\n`now` ${x}".into(), judge_model: Some("haiku".into()), advisor: true }).unwrap();
        assert!(s.starts_with("export const meta = {"));
        assert!(s.contains(r#""name":"colony-flow""#));
        assert!(s.contains(r#""phases":[{"detail":"unclear","title":"plan"},{"title":"build"}]"#), "{s}");
        assert!(s.contains(r#"const TASK = "do \"it\"\n`now` ${x}""#));
        assert!(s.contains(r#""skip":"nothing to build""#));
        assert!(s.contains(r#"{"from":0,"max":3,"to":1,"until":"it works"}"#), "{s}");
        assert!(s.contains("const JUDGE_MODEL = \"haiku\""));
        assert!(s.contains("const ADVISOR = true"));
    }

    #[test]
    fn branches_resolve_to_phase_numbers() {
        let wf = w(r#"{"phases":[{"skill":"triage","branches":[{"to":"bisect","condition":"it is a regression"},{"to":"publish","condition":""}]},{"skill":"bisect"},{"skill":"publish"}]}"#);
        let s = compile(&wf, &Options::default()).unwrap();
        assert!(s.contains(r#""branches":[{"cond":"it is a regression","to":1}]"#), "{s}");
        assert!(s.contains(r#""otherwise":2"#));
    }

    #[test]
    fn nothing_to_run_is_an_error() {
        let wf = Workflow { id: "x".into(), label: "x".into(), description: String::new(), phases: vec![], loops: vec![], parallel: vec![] };
        assert!(compile(&wf, &Options::default()).is_err());
    }
}
