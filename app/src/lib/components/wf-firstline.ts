export function firstLineOf(s: string): string {
  const l = s.split("\n")[0] ?? "";
  return l.length > 26 ? l.slice(0, 26) + "…" : l;
}
