import { Counter, add } from './math';

export function runDemo(): number {
  const counter = new Counter();
  counter.increment(add(1, 2));
  return counter.increment();
}

runDemo();
