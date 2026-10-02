// Fonts are vendored, not fetched: a loopback reader for a local index must
// work with the network off, and must not announce the project to a CDN.
//
// Inter for the interface, JetBrains Mono for code (docs/design/viewer-d.md §5),
// both variable, normal style only: the design sets nothing in italic.
import '@fontsource-variable/inter/wght.css';
import '@fontsource-variable/jetbrains-mono/wght.css';
import './app.css';
import { applyStoredTheme } from './lib/theme-choice.svelte';

import { mount } from 'svelte';
import App from './App.svelte';

// Before the first paint of the app, so a stored Dark or Light choice never
// flashes the OS scheme first. (The CSP forbids an inline script in the head.)
applyStoredTheme();

const target = document.getElementById('app');
if (!target) throw new Error('codegraph ui: #app host element is missing from index.html');

export default mount(App, { target });
