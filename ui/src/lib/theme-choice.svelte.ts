/**
 * The reader's theme choice — System, Dark or Light (docs/design/viewer-d.md
 * §3.6, "Switching").
 *
 * The tokens already follow `prefers-color-scheme` on their own; this only
 * records an explicit choice and applies it as `data-theme` on <html>, the
 * attribute `lib/theme.css` lets win over the OS. System removes the attribute
 * again, which hands the decision back to the media query.
 *
 * The choice lives in `localStorage` under one key. A browser that refuses
 * storage (a locked-down profile, a private window that throws) still gets a
 * working switch for the session; it just does not remember it.
 */

export type ThemeChoice = 'system' | 'dark' | 'light';

/** The storage key, named in the design spec. */
export const THEME_STORAGE_KEY = 'codegraph-ui.theme';

export const THEME_CHOICES: readonly ThemeChoice[] = ['system', 'dark', 'light'];

/** The label a control shows for each choice. */
export const THEME_LABEL: Record<ThemeChoice, string> = {
  system: 'System',
  dark: 'Dark',
  light: 'Light',
};

function isChoice(value: unknown): value is ThemeChoice {
  return value === 'system' || value === 'dark' || value === 'light';
}

function readStored(): ThemeChoice {
  try {
    const value = globalThis.localStorage?.getItem(THEME_STORAGE_KEY);
    return isChoice(value) ? value : 'system';
  } catch {
    return 'system';
  }
}

function writeStored(choice: ThemeChoice): void {
  try {
    if (choice === 'system') globalThis.localStorage?.removeItem(THEME_STORAGE_KEY);
    else globalThis.localStorage?.setItem(THEME_STORAGE_KEY, choice);
  } catch {
    // Storage refused: the choice holds for this page only.
  }
}

function applyToDocument(choice: ThemeChoice): void {
  const root = globalThis.document?.documentElement;
  if (!root) return;
  if (choice === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', choice);
}

/** What the page is painted in right now, the OS preference included. */
export function effectiveTheme(choice: ThemeChoice): 'dark' | 'light' {
  if (choice !== 'system') return choice;
  const dark = globalThis.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
  return dark ? 'dark' : 'light';
}

class ThemeState {
  choice = $state<ThemeChoice>(readStored());

  set(choice: ThemeChoice): void {
    this.choice = choice;
    writeStored(choice);
    applyToDocument(choice);
  }

  /** The next choice in System → Dark → Light order, for a one-button control. */
  cycle(): void {
    const at = THEME_CHOICES.indexOf(this.choice);
    this.set(THEME_CHOICES[(at + 1) % THEME_CHOICES.length] ?? 'system');
  }
}

export const theme = new ThemeState();

/** Applies the stored choice before the app mounts (see `main.ts`). */
export function applyStoredTheme(): void {
  applyToDocument(readStored());
}
