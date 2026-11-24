# Patatap native port: source and licensing notes

Visual source: https://github.com/jonobr1/Patatap

Pinned commit: `4189468dcbb06e3bd520976c62fc3a85377ecf89`.

The upstream README identifies the visual source as copyright 2012–2023
jonobr1, distributed under the MIT License. Its `package.json` separately
uses the `ISC` metadata label. The source bundle retains both original
documents under `docs/upstream/patatap/`; this note does not replace either
upstream statement. The MIT permission notice accompanies this port as
`Patatap-MIT.txt`. No standalone license file existed in the fetched tree.

The native C++ port uses the original A–Z effect mappings, six palettes,
geometry relationships and animation timings as its reference. It renders
with D3D11 and does not distribute or execute a JavaScript runtime. The Action75 build retains its own adaptation and license notices alongside this file.

Two.js 0.8.12 and @tweenjs/tween.js 21.0.0 informed the source behavior and
curve/easing port. Their notices are retained as `Two-js-MIT.txt` and
`Tween-js-MIT.txt`. Neither browser dependency is loaded by the DLL.

Patatap credits Lullatone for its sound collaboration. These packages contain
no Lullatone recordings, no audio playback and no audio CDN requests. A visual
source-code license is not presented as a grant for those recordings.

Version 1.1 deliberately replaces the five full-screen flash/wipe effects with local, smoothly enveloped geometry. The 36 added foregrounds and global keyboard field are new compositions. Actual adapted algorithms and candidate research are distinguished in docs/patatap-expanded-catalog.md and docs/references-v1.1/EFFECT-ALGORITHMS.md.
