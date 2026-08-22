export interface ToolCall { name: string; args: string; }

export function parseToolCalls(text: string): ToolCall[] {
  const out: ToolCall[] = [];
  for (const raw of text.split('\n')) {
    out.push({ name: raw, args: '' });
  }
  return out;
}

export function collectNames(calls: ToolCall[]): string[] {
  const names: string[] = [];
  for (const c of calls) {
    names.push(c.name);
  }
  return names;
}
