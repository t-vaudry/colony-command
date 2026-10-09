import { COST_HOURS, waitByProject, waitText, type ProjectLatency } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const COLORS = ["var(--p1)", "var(--p2)", "var(--p3)", "var(--p4)", "var(--p5)", "var(--p6)"];

/** How long each project's agents waited on you, last day: the median wait from
 *  asking (a permission or a question) to your response, and an hourly bar chart. */
export function latencyChart(latency: Map<string, ProjectLatency>, now: number): string {
  const rows = waitByProject(latency, now);
  if (!rows.length) return "";
  const peak = Math.max(...rows.flatMap((r) => r.hours.filter((v): v is number => v !== null)), 1);
  const w = 240;
  const h = 36;
  const bw = w / COST_HOURS;
  const body = rows
    .map((r, i) => {
      const bars = r.hours
        .map((v, j) => {
          if (v === null) return "";
          const bh = Math.max(1, (v / peak) * h);
          const ago = COST_HOURS - 1 - j;
          return `<rect x="${(j * bw + 0.5).toFixed(1)}" y="${(h - bh).toFixed(1)}" width="${(bw - 1).toFixed(1)}" height="${bh.toFixed(1)}" fill="${COLORS[i % COLORS.length]}"><title>${esc(r.name)}: median ${waitText(v)}, ${ago ? `${ago}h ago` : "this hour"}</title></rect>`;
        })
        .join("");
      return `<div class="cost-row"><div class="row"><span class="k">${esc(r.name)}</span><b title="${r.count} answered">median ${waitText(r.median)}</b></div>` +
        `<svg viewBox="0 0 ${w} ${h}" width="100%" height="${h}" preserveAspectRatio="none" role="img" aria-label="${esc(r.name)} median wait per hour over the last ${COST_HOURS} hours"><line x1="0" y1="${h - 0.5}" x2="${w}" y2="${h - 0.5}" stroke="var(--line)"/>${bars}</svg></div>`;
    })
    .join("");
  return `<div class="k">Time to respond, last ${COST_HOURS}h</div><p class="muted small">Median time from an agent asking you a question or for a permission to your answer. One bar per hour, all projects on the same scale.</p>${body}`;
}
