import { Map } from './barrel';

export function useBarrel(): string {
  const m = new Map();
  return m.get('a');
}
