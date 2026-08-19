export class Walker { run(n: number): void { if (n > 0) { this.run(n - 1); } } }
export class Other  { run(n: number): void { void n; } }
