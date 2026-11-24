# Geometry interaction refactor: references and licensing

Applies to Patatap · 几何互动, version `1.2.0-test.1`, package ID `patatap-native`.

## Newly written code

The `patatap/source/geometric.hpp`, `tempo.hpp` and `color_curtain.hpp` modules are newly written native C++ implementations. The related integration changes implement independent press instances, recent-input tempo control, customizable foreground/background palettes and low-frequency background transitions.

These additions are covered by the MIT notice in `SOURCE-LICENSE.txt`, copyright (c) 2026 Action75 native effects contributors. Retained third-party material and ported algorithms continue to carry their existing notices; this statement does not relicense them.

The 64 geometric recipes are implemented as analytic geometry and time-dependent motion. They do not embed a video clip, sprite capture or audio sample from a reference work.

## Visual and interaction references

- Mikutap by daniwell / AIDN: https://aidn.jp/mikutap/
- Momotap by daniwell / AIDN: https://aidn.jp/momotap/
- User-supplied Bilibili reference, BV1fW411L7qG: https://www.bilibili.com/video/BV1fW411L7qG/
- The first reference identifies the original as *Triangle | Motion Graphics*: https://www.youtube.com/watch?v=jy2aZHUI-6A
- Additional user-supplied references: https://www.bilibili.com/video/BV1Pb4y1f7fx/ and https://www.bilibili.com/video/BV1w54y1z7ga/

Mikutap and Momotap informed the general press-to-response interaction and geometric visual direction. The background module is a newly written, deliberately slower moving-color-plane implementation with bounded transition scheduling. It is not a claim of identical frame-by-frame playback of either webpage.

For BV1fW411L7qG, the research pass inspected 41 approximately one-second keyframes, not every intermediate video frame. Confirmed visual motifs included layered triangles, fine triangle outlines, parallel hatching, triangle tunnels, edge-normal short lines, mirrored fragments, connected corner points and perspective triangular grids. These motifs informed newly composed realtime actions, not a traced copy of the complete film.

The other two Bilibili URLs returned HTTP 412 during the reference pass and were not visually verified. Their URLs are retained as user references; they are not cited as evidence that their detailed animation was reviewed or reproduced.

## Material not included

No Mikutap or Momotap JavaScript implementation, music, voice samples, character models, or video assets are bundled into the new modules or native installation package. No reference-video frame is used as an animation asset. The wallpaper is silent and makes no audio CDN request.

No inference is made that a reference webpage being publicly viewable grants permission to redistribute its code, audio or character assets. The newly authored native implementation is separate from those resources. The original Patatap port and its pinned upstream source references remain documented by `Patatap-source-notes.md` and their retained notices.

## Validation and representation

The included effect cover and any accompanying CPU preview are generated from the production Scene geometry. They are useful for visual and timing review but are not Windows D3D11 screenshots. Neither conceptual inspiration nor CPU geometry checks establish pixel-perfect equivalence to Patatap, Mikutap, Momotap or the reference films.
