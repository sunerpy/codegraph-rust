/**
 * The viewer's destinations, in the nav rail's order (docs/design/viewer-d.md
 * §2: Start, Map, Symbol, Flow, Screens, Steps, Dead code, Saved trails).
 *
 * One list for the rail, the phone's tab bar and its More sheet, so the three
 * can never disagree about where a destination goes or when it is the current
 * one. Reads the live route, so callers derive from it inside a component.
 */

import type { IconName } from './icons';
import {
  deadHref,
  entryHref,
  flowHref,
  mapHref,
  router,
  screensHref,
  stepsHref,
  symbolHref,
} from './router.svelte';
import { trail } from './trail.svelte';

export type DestinationId =
  | 'start'
  | 'map'
  | 'symbol'
  | 'flow'
  | 'screens'
  | 'steps'
  | 'dead'
  | 'trails';

export interface Destination {
  id: DestinationId;
  label: string;
  /** The phone tab bar's caption — the label, shortened where it must be. */
  short: string;
  icon: IconName;
  href: string;
  active: boolean;
}

/**
 * @param hasScreens the graph holds screen navigation, so `#/` renders the
 *   Screens view (upstream's rule) and Start is not what `#/` shows.
 */
export function destinations(hasScreens: boolean): Destination[] {
  const route = router.route;
  const view = route.view;

  // The Symbol destination returns you to where you were reading, not to a
  // blank view: the current symbol, else the trail's last hop, else the
  // view's own empty screen. Never `#/`, which may render Screens.
  let symbolTarget: string;
  if (route.view === 'symbol' && route.id !== null) symbolTarget = symbolHref(route.id);
  else symbolTarget = symbolHref(trail.current ? trail.current.id : null);

  return [
    {
      id: 'start',
      label: 'Start',
      short: 'Start',
      icon: 'layout-dashboard',
      href: '#/',
      active: view === 'home' && !hasScreens,
    },
    { id: 'map', label: 'Map', short: 'Map', icon: 'network', href: mapHref(), active: view === 'map' },
    {
      id: 'symbol',
      label: 'Symbol',
      short: 'Symbol',
      icon: 'braces',
      href: symbolTarget,
      // The File view is reading code too; the boards light Symbol for it.
      active: view === 'symbol' || view === 'file',
    },
    { id: 'flow', label: 'Flow', short: 'Flow', icon: 'workflow', href: flowHref(), active: view === 'flow' },
    {
      id: 'screens',
      label: 'Screens',
      short: 'Screens',
      icon: 'monitor-smartphone',
      href: screensHref(),
      active: view === 'screens' || (view === 'home' && hasScreens),
    },
    { id: 'steps', label: 'Steps', short: 'Steps', icon: 'footprints', href: stepsHref(), active: view === 'steps' },
    { id: 'dead', label: 'Dead code', short: 'Dead code', icon: 'ghost', href: deadHref(), active: view === 'dead' },
    {
      id: 'trails',
      label: 'Saved trails and entry points',
      short: 'Trails',
      icon: 'bookmark',
      href: entryHref(),
      active: view === 'entry',
    },
  ];
}
