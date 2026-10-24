// Pure helpers behind panels.tsx (no opencode imports, so they can be tested with `bun test`).

export interface Question {
  text: string;
  options: string[];
}

export interface Task {
  id: string;
  short: string;
  context: string;
  from: string;
  to: string;
  state: string;
  updated_at: string;
  directory: string | null;
  question: Question | null;
  hop_capped: boolean;
}

export interface Snapshot {
  dispatcher: boolean;
  tasks: Task[];
  progress: { done: number; total: number };
  today: { tokens_in: number; tokens_out: number; cost_usd: number };
}

export const EMPTY: Snapshot = {
  dispatcher: false,
  tasks: [],
  progress: { done: 0, total: 0 },
  today: { tokens_in: 0, tokens_out: 0, cost_usd: 0 },
};

export const OPEN = ["submitted", "working", "input-required", "auth-required"];

export const isOpen = (t: Task): boolean => OPEN.includes(t.state);
export const waiting = (s: Snapshot): Task[] =>
  s.tasks.filter((t) => t.state === "input-required" || t.state === "auth-required");
export const capped = (s: Snapshot): Task[] => s.tasks.filter((t) => t.hop_capped);

/** Filled part and thin rule of the progress bar: `[full, rest]`, e.g. `["██████", "──────"]`. */
export const barParts = (done: number, total: number, width = 12): [string, string] => {
  if (total <= 0) return ["", "─".repeat(width)];
  const full = Math.min(width, Math.round((done / total) * width));
  return ["█".repeat(full), "─".repeat(width - full)];
};

export const bar = (done: number, total: number, width = 12): string => barParts(done, total, width).join("");

/** `6.0 / 12` */
export const progressLabel = (done: number, total: number): string => `${done.toFixed(1)} / ${total}`;

export const glyph = (state: string): string =>
  ({
    submitted: "…",
    working: "▶",
    "input-required": "?",
    "auth-required": "?",
    completed: "✓",
    failed: "✗",
    canceled: "–",
    rejected: "–",
  })[state] ?? "·";

export const tokens = (n: number): string =>
  n >= 1_000_000 ? `${(n / 1e6).toFixed(1)}M` : n >= 1_000 ? `${(n / 1e3).toFixed(1)}k` : String(n);

export const cost = (usd: number): string => `~$${usd.toFixed(usd >= 1 ? 2 : 4)}`;

export const statusLine = (s: Snapshot): string => {
  const open = s.tasks.filter(isOpen).length;
  const t = s.today.tokens_in + s.today.tokens_out;
  return `a2a ${s.dispatcher ? "●" : "○"} ${open} open · ${cost(s.today.cost_usd)} · ${tokens(t)} tok today`;
};

export type Change = { kind: "input" | "done" | "failed" | "cap"; task: Task };

/** What changed since `seen` (task id -> "state|capped"); `seen` is updated. The first call only seeds it. */
export const changes = (seen: Map<string, string>, s: Snapshot): Change[] => {
  const seeded = seen.size > 0 || s.tasks.length === 0;
  const out: Change[] = [];
  for (const t of s.tasks) {
    const key = `${t.state}|${t.hop_capped}`;
    const prev = seen.get(t.id);
    seen.set(t.id, key);
    if (!seeded || prev === key) continue;
    const [prevState, prevCap] = (prev ?? "|false").split("|");
    if (t.state !== prevState) {
      if (t.state === "input-required" || t.state === "auth-required") out.push({ kind: "input", task: t });
      else if (t.state === "completed") out.push({ kind: "done", task: t });
      else if (t.state === "failed") out.push({ kind: "failed", task: t });
    }
    if (t.hop_capped && prevCap !== "true") out.push({ kind: "cap", task: t });
  }
  return out;
};

const NAMED: Record<string, string> = {
  primary: "#7aa2f7",
  secondary: "#bb9af7",
  accent: "#2ac3de",
  success: "#9ece6a",
  warning: "#e0af68",
  error: "#f7768e",
  info: "#7dcfff",
};
const PALETTE = ["#7aa2f7", "#bb9af7", "#2ac3de", "#9ece6a", "#e0af68", "#f7768e", "#7dcfff", "#ff9e64"];

/** The `color:` of an agent file's frontmatter (hex or a theme name), as hex; undefined when absent. */
export const frontmatterColor = (md: string): string | undefined => {
  const fm = /^---\n([\s\S]*?)\n---/.exec(md)?.[1] ?? "";
  const v = /^color:\s*["']?([#\w]+)["']?\s*$/m.exec(fm)?.[1];
  if (!v) return undefined;
  return v.startsWith("#") ? v : NAMED[v];
};

/** A stable colour per agent name, for agents that set none. */
export const hashColor = (name: string): string => {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return PALETTE[h % PALETTE.length];
};

export const STATE_COLOR: Record<string, string> = {
  working: "#7dcfff",
  "input-required": "#e0af68",
  "auth-required": "#e0af68",
  completed: "#9ece6a",
  failed: "#f7768e",
};
