export function target(): number {
  return 1;
}

export const value = target();

export function wrapper(): number {
  const inner = target();
  return inner;
}
