export function add(left: number, right: number): number {
  return left + right;
}

export class Counter {
  private value = 0;

  increment(step: number = 1): number {
    this.value = add(this.value, step);
    return this.value;
  }
}
