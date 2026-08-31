import { legacyTarget } from './legacy_helpers';
import { preferredTarget } from './legacy_priority';

export function runLegacy() {
  legacyTarget();
  preferredTarget();
}
