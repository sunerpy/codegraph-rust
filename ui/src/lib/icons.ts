/*!
 * Icon geometry from Lucide 0.544.0 (lucide-static), the set docs/design/viewer-d.md §6
 * names. Copied, not imported: the viewer needs 66 shapes, not a dependency.
 *
 * ISC License
 *
 * Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2023 as part of Feather (MIT). All other copyright (c) for Lucide are held by Lucide Contributors 2025.
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
 * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
 * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 *
 * ---
 *
 * The MIT License (MIT) (for portions derived from Feather)
 *
 * Copyright (c) 2013-2023 Cole Bemis
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

/** Inner SVG of each icon on a 24 x 24 viewBox, stroked by the caller. */
export const ICON_SHAPES = {
  'activity': "<path d='M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2' />",
  'arrow-left': "<path d='m12 19-7-7 7-7' /> <path d='M19 12H5' />",
  'arrow-right': "<path d='M5 12h14' /> <path d='m12 5 7 7-7 7' />",
  'bookmark': "<path d='m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z' />",
  'box': "<path d='M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z' /> <path d='m3.3 7 8.7 5 8.7-5' /> <path d='M12 22V12' />",
  'braces': "<path d='M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1' /> <path d='M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1' />",
  'check': "<path d='M20 6 9 17l-5-5' />",
  'chevron-down': "<path d='m6 9 6 6 6-6' />",
  'chevron-left': "<path d='m15 18-6-6 6-6' />",
  'chevron-right': "<path d='m9 18 6-6-6-6' />",
  'circle-alert': "<circle cx='12' cy='12' r='10' /> <line x1='12' x2='12' y1='8' y2='12' /> <line x1='12' x2='12.01' y1='16' y2='16' />",
  'circle-check': "<circle cx='12' cy='12' r='10' /> <path d='m9 12 2 2 4-4' />",
  'circle-dashed': "<path d='M10.1 2.182a10 10 0 0 1 3.8 0' /> <path d='M13.9 21.818a10 10 0 0 1-3.8 0' /> <path d='M17.609 3.721a10 10 0 0 1 2.69 2.7' /> <path d='M2.182 13.9a10 10 0 0 1 0-3.8' /> <path d='M20.279 17.609a10 10 0 0 1-2.7 2.69' /> <path d='M21.818 10.1a10 10 0 0 1 0 3.8' /> <path d='M3.721 6.391a10 10 0 0 1 2.7-2.69' /> <path d='M6.391 20.279a10 10 0 0 1-2.69-2.7' />",
  'circle-x': "<circle cx='12' cy='12' r='10' /> <path d='m15 9-6 6' /> <path d='m9 9 6 6' />",
  'command': "<path d='M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3' />",
  'copy': "<rect width='14' height='14' x='8' y='8' rx='2' ry='2' /> <path d='M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2' />",
  'corner-down-right': "<path d='m15 10 5 5-5 5' /> <path d='M4 4v7a4 4 0 0 0 4 4h12' />",
  'corner-left-up': "<path d='M14 9 9 4 4 9' /> <path d='M20 20h-7a4 4 0 0 1-4-4V4' />",
  'corner-up-left': "<path d='M20 20v-7a4 4 0 0 0-4-4H4' /> <path d='M9 14 4 9l5-5' />",
  'database': "<ellipse cx='12' cy='5' rx='9' ry='3' /> <path d='M3 5V19A9 3 0 0 0 21 19V5' /> <path d='M3 12A9 3 0 0 0 21 12' />",
  'ellipsis': "<circle cx='12' cy='12' r='1' /> <circle cx='19' cy='12' r='1' /> <circle cx='5' cy='12' r='1' />",
  'external-link': "<path d='M15 3h6v6' /> <path d='M10 14 21 3' /> <path d='M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6' />",
  'eye-off': "<path d='M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49' /> <path d='M14.084 14.158a3 3 0 0 1-4.242-4.242' /> <path d='M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143' /> <path d='m2 2 20 20' />",
  'file': "<path d='M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z' /> <path d='M14 2v4a2 2 0 0 0 2 2h4' />",
  'file-code-2': "<path d='M4 22h14a2 2 0 0 0 2-2V7l-5-5H6a2 2 0 0 0-2 2v4' /> <path d='M14 2v4a2 2 0 0 0 2 2h4' /> <path d='m5 12-3 3 3 3' /> <path d='m9 18 3-3-3-3' />",
  'filter': "<path d='M10 20a1 1 0 0 0 .553.895l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .517-1.341L21.74 4.67A1 1 0 0 0 21 3H3a1 1 0 0 0-.742 1.67l7.225 7.989A2 2 0 0 1 10 14z' />",
  'flask-conical': "<path d='M14 2v6a2 2 0 0 0 .245.96l5.51 10.08A2 2 0 0 1 18 22H6a2 2 0 0 1-1.755-2.96l5.51-10.08A2 2 0 0 0 10 8V2' /> <path d='M6.453 15h11.094' /> <path d='M8.5 2h7' />",
  'folder': "<path d='M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z' />",
  'footprints': "<path d='M4 16v-2.38C4 11.5 2.97 10.5 3 8c.03-2.72 1.49-6 4.5-6C9.37 2 10 3.8 10 5.5c0 3.11-2 5.66-2 8.68V16a2 2 0 1 1-4 0Z' /> <path d='M20 20v-2.38c0-2.12 1.03-3.12 1-5.62-.03-2.72-1.49-6-4.5-6C14.63 6 14 7.8 14 9.5c0 3.11 2 5.66 2 8.68V20a2 2 0 1 0 4 0Z' /> <path d='M16 17h4' /> <path d='M4 13h4' />",
  'ghost': "<path d='M9 10h.01' /> <path d='M15 10h.01' /> <path d='M12 2a8 8 0 0 0-8 8v12l3-3 2.5 2.5L12 19l2.5 2.5L17 19l3 3V10a8 8 0 0 0-8-8z' />",
  'git-branch': "<line x1='6' x2='6' y1='3' y2='15' /> <circle cx='18' cy='6' r='3' /> <circle cx='6' cy='18' r='3' /> <path d='M18 9a9 9 0 0 1-9 9' />",
  'git-commit-horizontal': "<circle cx='12' cy='12' r='3' /> <line x1='3' x2='9' y1='12' y2='12' /> <line x1='15' x2='21' y1='12' y2='12' />",
  'git-fork': "<circle cx='12' cy='18' r='3' /> <circle cx='6' cy='6' r='3' /> <circle cx='18' cy='6' r='3' /> <path d='M18 9v2c0 .6-.4 1-1 1H7c-.6 0-1-.4-1-1V9' /> <path d='M12 12v3' />",
  'info': "<circle cx='12' cy='12' r='10' /> <path d='M12 16v-4' /> <path d='M12 8h.01' />",
  'key-round': "<path d='M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z' /> <circle cx='16.5' cy='7.5' r='.5' fill='currentColor' />",
  'keyboard': "<path d='M10 8h.01' /> <path d='M12 12h.01' /> <path d='M14 8h.01' /> <path d='M16 12h.01' /> <path d='M18 8h.01' /> <path d='M6 8h.01' /> <path d='M7 16h10' /> <path d='M8 12h.01' /> <rect width='20' height='16' x='2' y='4' rx='2' />",
  'layers': "<path d='M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z' /> <path d='M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12' /> <path d='M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17' />",
  'layout-dashboard': "<rect width='7' height='9' x='3' y='3' rx='1' /> <rect width='7' height='5' x='14' y='3' rx='1' /> <rect width='7' height='9' x='14' y='12' rx='1' /> <rect width='7' height='5' x='3' y='16' rx='1' />",
  'link': "<path d='M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71' /> <path d='M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71' />",
  'list-tree': "<path d='M8 5h13' /> <path d='M13 12h8' /> <path d='M13 19h8' /> <path d='M3 10a2 2 0 0 0 2 2h3' /> <path d='M3 5v12a2 2 0 0 0 2 2h3' />",
  'lock': "<rect width='18' height='11' x='3' y='11' rx='2' ry='2' /> <path d='M7 11V7a5 5 0 0 1 10 0v4' />",
  'maximize': "<path d='M8 3H5a2 2 0 0 0-2 2v3' /> <path d='M21 8V5a2 2 0 0 0-2-2h-3' /> <path d='M3 16v3a2 2 0 0 0 2 2h3' /> <path d='M16 21h3a2 2 0 0 0 2-2v-3' />",
  'menu': "<path d='M4 5h16' /> <path d='M4 12h16' /> <path d='M4 19h16' />",
  'minus': "<path d='M5 12h14' />",
  'monitor': "<rect width='20' height='14' x='2' y='3' rx='2' /> <line x1='8' x2='16' y1='21' y2='21' /> <line x1='12' x2='12' y1='17' y2='21' />",
  'monitor-smartphone': "<path d='M18 8V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h8' /> <path d='M10 19v-3.96 3.15' /> <path d='M7 19h5' /> <rect width='6' height='10' x='16' y='12' rx='2' />",
  'moon': "<path d='M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401' />",
  'mouse-pointer-click': "<path d='M14 4.1 12 6' /> <path d='m5.1 8-2.9-.8' /> <path d='m6 12-1.9 2' /> <path d='M7.2 2.2 8 5.1' /> <path d='M9.037 9.69a.498.498 0 0 1 .653-.653l11 4.5a.5.5 0 0 1-.074.949l-4.349 1.041a1 1 0 0 0-.74.739l-1.04 4.35a.5.5 0 0 1-.95.074z' />",
  'network': "<rect x='16' y='16' width='6' height='6' rx='1' /> <rect x='2' y='16' width='6' height='6' rx='1' /> <rect x='9' y='2' width='6' height='6' rx='1' /> <path d='M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3' /> <path d='M12 12V8' />",
  'package': "<path d='M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z' /> <path d='M12 22V12' /> <polyline points='3.29 7 12 12 20.71 7' /> <path d='m7.5 4.27 9 5.15' />",
  'plus': "<path d='M5 12h14' /> <path d='M12 5v14' />",
  'radio': "<path d='M16.247 7.761a6 6 0 0 1 0 8.478' /> <path d='M19.075 4.933a10 10 0 0 1 0 14.134' /> <path d='M4.925 19.067a10 10 0 0 1 0-14.134' /> <path d='M7.753 16.239a6 6 0 0 1 0-8.478' /> <circle cx='12' cy='12' r='2' />",
  'refresh-cw': "<path d='M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8' /> <path d='M21 3v5h-5' /> <path d='M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16' /> <path d='M8 16H3v5' />",
  'route': "<circle cx='6' cy='19' r='3' /> <path d='M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15' /> <circle cx='18' cy='5' r='3' />",
  'search': "<path d='m21 21-4.34-4.34' /> <circle cx='11' cy='11' r='8' />",
  'send': "<path d='M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z' /> <path d='m21.854 2.147-10.94 10.939' />",
  'server': "<rect width='20' height='8' x='2' y='2' rx='2' ry='2' /> <rect width='20' height='8' x='2' y='14' rx='2' ry='2' /> <line x1='6' x2='6.01' y1='6' y2='6' /> <line x1='6' x2='6.01' y1='18' y2='18' />",
  'settings': "<path d='M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915' /> <circle cx='12' cy='12' r='3' />",
  'shield': "<path d='M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z' />",
  'sparkles': "<path d='M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z' /> <path d='M20 2v4' /> <path d='M22 4h-4' /> <circle cx='4' cy='20' r='2' />",
  'sun': "<circle cx='12' cy='12' r='4' /> <path d='M12 2v2' /> <path d='M12 20v2' /> <path d='m4.93 4.93 1.41 1.41' /> <path d='m17.66 17.66 1.41 1.41' /> <path d='M2 12h2' /> <path d='M20 12h2' /> <path d='m6.34 17.66-1.41 1.41' /> <path d='m19.07 4.93-1.41 1.41' />",
  'triangle-alert': "<path d='m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3' /> <path d='M12 9v4' /> <path d='M12 17h.01' />",
  'users': "<path d='M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2' /> <path d='M16 3.128a4 4 0 0 1 0 7.744' /> <path d='M22 21v-2a4 4 0 0 0-3-3.87' /> <circle cx='9' cy='7' r='4' />",
  'workflow': "<rect width='8' height='8' x='3' y='3' rx='2' /> <path d='M7 11v4a2 2 0 0 0 2 2h4' /> <rect width='8' height='8' x='13' y='13' rx='2' />",
  'x': "<path d='M18 6 6 18' /> <path d='m6 6 12 12' />",
  'zap': "<path d='M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z' />",
} as const;

export type IconName = keyof typeof ICON_SHAPES;
