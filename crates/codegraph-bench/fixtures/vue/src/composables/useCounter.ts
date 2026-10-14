import { ref } from 'vue';

export function useCounter(start: number) {
  const count = ref(start);
  function increment() {
    count.value += 1;
  }
  return { count, increment };
}

export function helper(): number {
  return 1;
}
