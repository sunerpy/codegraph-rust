import { api } from './namespace';

export function importedCaller() {
  api.run();
  api.stop();
  api.nested.run();
}
