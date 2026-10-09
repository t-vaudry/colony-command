import { COST_HOURS, costByProject, money, type ProjectSpend } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const COLORS = ["var(--p1)", "var(--p2)", "var(--p3)", "var(--p4)", "var(--p5)", "var(--p6)"];

/** Per-project cost over the last day: a total and an hourly bar chart each.
 *  Estimates from list prices, so every figure carries the "~". */
export function costChart(spend: Map<string, ProjectSpend>, now: number): string {
  const rows = costByProject(spend, now);
  if (!rows.length) return "";
  const peak = Math.max(...rows.flatMap((r) => r.hours), 0.0001);
  const w = 240;
  const h = 36;
  const bw = w / COST_HOURS;
  const body = rows
    .map((r, i) => {
      const bars = r.hours
        .map((v, j) => {
          const bh = v > 0 ? Math.max(1, (v / peak) * h) : 0;
          const ago = COST_HOURS - 1 - j;
          return bh ? `<rect x="${(j * bw + 0.5).toFixed(1)}" y="${(h - bh).toFixed(1)}" width="${(bw - 1).toFixed(1)}" height="${bh.toFixed(1)}" fill="${COLORS[i % COLORS.length]}"><title>${esc(r.name)}: ${money(v)}, ${ago ? `${ago}h ago` : "this hour"}</title></rect>` : "";
        })
        .join("");
      return `<div class="cost-row"><div class="row"><span class="k">${esc(r.name)}</span><b>${money(r.usd)}${r.partial ? " partial" : ""}</b></div>` +
        `<svg viewBox="0 0 ${w} ${h}" width="100%" height="${h}" preserveAspectRatio="none" role="img" aria-label="${esc(r.name)} cost per hour over the last ${COST_HOURS} hours"><line x1="0" y1="${h - 0.5}" x2="${w}" y2="${h - 0.5}" stroke="var(--line)"/>${bars}</svg></div>`;
    })
    .join("");
  return `<div class="k">Cost by project, last ${COST_HOURS}h</div><p class="muted small">Estimates from list prices, one bar per hour, all projects on the same scale. The ledger keeps 48 hours.</p>${body}`;
}
