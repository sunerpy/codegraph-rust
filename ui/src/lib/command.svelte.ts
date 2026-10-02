/**
 * A request for the command palette from somewhere that does not own it.
 *
 * The palette lives in the command bar, which App owns; a Start-screen
 * "Search" button is in a view. A counter rather than a callback, for the same
 * reason the live channel uses one: App's effect reads it and focuses the
 * palette when it moves, and nothing else has to hold a reference to the bar.
 */

let searchTick = $state(0);

export const command = {
  get searchTick(): number {
    return searchTick;
  },
  /** Open the ⌘K palette with the cursor in it. */
  requestSearch(): void {
    searchTick += 1;
  },
};
