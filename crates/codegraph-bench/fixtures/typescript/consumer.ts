import { constAlias as viaConstAlias } from './exports';
import { namedAlias as viaNamedAlias } from './exports';
import defaultExportAlias from './exports';
import { jsTarget as viaJsSpecifier } from './js_target.js';
import { collisionTarget as viaCollision } from './collision.js';
import { extensionlessTarget as viaExtensionless } from './extensionless';
import { missingTarget as viaMissing } from './missing.js';

export function runAll() {
  viaConstAlias();
  viaNamedAlias();
  defaultExportAlias();
  viaJsSpecifier();
  viaCollision();
  viaExtensionless();
  viaMissing();
}
