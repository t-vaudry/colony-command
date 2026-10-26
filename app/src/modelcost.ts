import { money, type ModelCost } from "./types";

const NAMES: Record<string, string> = { opus: "Opus", sonnet: "Sonnet", haiku: "Haiku", fable: "Fable", other: "Other" };

/** "advisor:opus" -> "Opus advisor", "haiku" -> "Haiku". */
export function modelName(key: string): string {
  const advisor = key.startsWith("advisor:");
  const name = NAMES[advisor ? key.slice(8) : key] ?? key;
  return advisor ? `${name} advisor` : name;
}

/** Biggest spender first. */
export function sortedModels(by: Iterable<ModelCost>): ModelCost[] {
  return [...by].sort((a, b) => b.cost_usd - a.cost_usd || b.tokens - a.tokens);
}

/** "Sonnet ~$0.42 · Haiku ~$0.05 · Opus advisor ~$0.18 (3 consults)". Empty with nothing to split. */
export function modelSplit(by: ModelCost[] | undefined): string {
  return sortedModels(by ?? [])
    .map((m) => {
      const c = m.consults ?? 0;
      return `${modelName(m.model)} ${money(m.cost_usd)}${m.partial ? " partial" : ""}${c ? ` (${c} ${c === 1 ? "consult" : "consults"})` : ""}`;
    })
    .join(" · ");
}

/** Share of the cost that went to Opus, as the main model or the advisor; null when nothing is priced. */
export function opusShare(by: ModelCost[] | undefined): number | null {
  const total = (by ?? []).reduce((n, m) => n + m.cost_usd, 0);
  if (total <= 0) return null;
  const opus = (by ?? []).filter((m) => m.model === "opus" || m.model === "advisor:opus").reduce((n, m) => n + m.cost_usd, 0);
  return opus / total;
}

/** The split and the Opus share together, for one line. Empty unless more than one model is involved. */
export function modelLine(by: ModelCost[] | undefined): string {
  if (!by || by.length < 2) return by?.some((m) => m.model.startsWith("advisor:")) ? modelSplit(by) : "";
  const share = opusShare(by);
  return modelSplit(by) + (share === null ? "" : ` · Opus ${Math.round(share * 100)}% of cost`);
}
