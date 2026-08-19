import { Map } from './other';

export function useNamed(): string {
  const m = new Map();
  return m.get('a');
}
