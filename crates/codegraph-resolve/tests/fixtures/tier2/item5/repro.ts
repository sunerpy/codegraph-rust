export class LRUCache {
  private store = new Map<string, string>();

  get(key: string): string | undefined {
    return this.store.get(key);
  }

  set(key: string, value: string): void {
    this.store.set(key, value);
  }

  has(key: string): boolean {
    return this.store.has(key);
  }
}

export function useLocalMap(): boolean {
  const values = new Map<string, string>();
  values.set("answer", "42");
  values.get("answer");
  return values.has("answer");
}

export function useNestedMap(
  holder: { values: Map<string, string> },
): string | undefined {
  return holder.values.get("answer");
}
