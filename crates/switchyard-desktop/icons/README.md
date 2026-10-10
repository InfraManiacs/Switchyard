# Desktop app icon

The app icon uses a connected route junction in ice blue and lavender on a dark
navy tile. The broad strokes remain readable at small Dock sizes.

`icon.png` contains the 1024 × 1024 RGBA source. `icon.icns` contains macOS icon
sizes from 16 to 1024 pixels. The exterior of the rounded tile is transparent.
Windows uses the same artwork at 16, 32, 64, 128, and 256 pixels in `icon.ico`.
The desktop tray uses the separate `tray.png` source; `tray.rgba` contains its
decoded pixels. `tray.svg` is the editable design reference.
Windows uses `tray-windows.rgba`, a small color copy of the app icon, so the tray
icon stays visible on light and dark taskbars. macOS tints the template tray icon.

## Generation

Gemini 3 Pro Image generated the artwork through Inference Hub using
`gcp/google/gemini-3-pro-image`.

To generate a replacement, request three distinct route concepts and compare
them at 16, 32, 64, and 128 pixels. Refine the strongest concept into one
connected junction. Each request should ask for one image with `n: 1` and
`size: "1024x1024"`.

The selected image used this exact prompt:

```text
Design one original professional macOS desktop application icon for Switchyard. Switchyard is a Rust LLM router between coding agents and inference servers, selecting the best model for each request. The desktop app manages routes, tool settings, sessions, and usage. Deliver a single square 1024x1024 icon artwork, not a presentation or mockup. Rounded square dark midnight navy tile, subtle refined surface lighting, centered simple abstract routing/switch junction symbol, large bold silhouette with generous negative space, precise smoothly rounded edges, recognizable at 16 to 64 pixels. Restrained premium developer tool aesthetic. No text, no letters, no logo of an existing company, no NVIDIA eye, no brains, robots, circuit boards, fine lines, stars, sparkles, or busy details. Flat front view, no perspective. The tile should fill the square canvas edge to edge, so the rounded tile exterior can be exported with real transparency. Concept: a SINGLE CONNECTED Y shaped route junction, with three broad rounded arms. One vertical trunk starts at bottom center and gently forks into upper-left and upper-right arms. All three arms meet in one smooth central junction with NO gaps, no crossings, no disconnected fragments, no arrows. The left arm is ice blue and the right arm is lavender, blending smoothly at the junction. Use a large centered calm confident minimal raised symbol, not a detailed railway map. Keep the entire symbol INSIDE the tile with at least 160 pixels of interior padding. Plain solid white background outside the rounded dark tile, NO checkerboard. The square dark tile spans pixels 64 through 960 of the canvas, with generous smooth rounded corners.
```

## Export

1. Preserve the generated image before export.
2. Isolate the navy tile from its generated white exterior. Fill enclosed mask
   regions so the symbol stays opaque, and soften the outer mask by 0.35 pixels.
3. Save the result as a 1024 × 1024 RGBA PNG.
4. Run `cargo desktop-icons` from the repository. The Rust command exports
   `icon.icns`, `icon.ico`, and both tray pixel files on macOS, Linux, or Windows. It uses
   Lanczos filtering for all desktop sizes from the same `icon.png`.
5. Inspect the PNG on light, dark, and vivid backgrounds and check the exported
   icons at small sizes.

Run `cargo desktop-icons --check` to verify the exports without changing files.
Every desktop build runs this check on every platform, so changing either PNG
requires regenerating its exports before the app can build. No Windows runner
is needed to check the Windows icon. Keep both PNG source files as RGBA images
at their existing sizes (1024 × 1024 and 22 × 22).

Keep the generated artwork inside the tile unchanged during export.
