# GPConnect style lock

Status: **DRAFT — awaiting the user's pick among 5 concepts.**
Surface type: **app shell** (not marketing). Step 2.5 macrostructure rotation does
not apply; layout is governed by component-patterns.md's App shell section.

## Constraints that come from the product, not taste

- Native Rust / egui immediate-mode. No HTML, no web fonts, no GSAP, no shadcn,
  no Openverse photos. Every concept below must be buildable inside egui 0.31
  primitives: `Painter` shapes, `Frame`, `RichText`, custom widget functions.
- No proprietary assets: the official client's icons cannot be shipped.
- Light/Dark/System toggle already exists and must survive any redesign.
- Window is 460x560 today and must stay status-first: connection state is the
  single most important pixel on screen.
- Motion budget is small by nature: egui animates per-frame values, so motion
  means animated numbers/colors, not scroll timelines.

## Mood tie-break

"VPN client for a corporate portal, opened at the start of a work day, then
ignored for 8 hours." That leans **calm and technical** over playful. Concept 4
(playful) is offered as the deliberate outlier, not the default.

## The five concepts

Each has its own generated palette (contrast-verified), shape language, and door
to the secondary content. Only the chosen one gets written into the Color
contract below.

### 1. Instrument — technical / dark (seed 11)
text #e9f3fe · bg #0e1317 · surface #191e23 · primary #3178b9 · accent #df7c8e
border #252a2f · on-primary #ffffff
Shape: 0 radius, hairlines, monospace for every number and state label.
Status is a full-width hero *readout*: big state word, tunnel IP in mono, uptime
counting up, data in/out as thin bars. Log is always one keystroke away (a real
inline panel, not a collapsing header). Settings live in a left rail of 4 icons.
Risk: reads as a monitoring tool, not a consumer app — which may be exactly right
for a corporate VPN.

### 2. Calm — premium / dark (seed 23)
text #eff1fe · bg #0b0c11 · surface #16171d · primary #6c6fb7 · accent #b68ad0
border #222229 · on-primary #ffffff
Shape: 20 radius cards on a near-black canvas, generous 24px padding, one accent
used only for the connected state. Status is a centered 96px ring whose sweep is
the animated connect progress; the whole window dims to 40% while connecting so
there is nothing else to read. Portal list becomes a swipeable card deck. Log and
settings move behind a single "..." menu.
Risk: the ring is decorative if it never carries real progress; must be driven by
actual stage events or dropped.

### 3. Paper — elegant / light (seed 31)
text #221815 · bg #fff4f1 · surface #f5eae7 · primary #8f665a · accent #537961
border #e4dad7 · on-primary #ffffff
Shape: flat paper, no shadows, hairlines, serif for headings, generous margins.
Status is a *document*: a title line "Connected to ETSP", a thin rule, then two
labelled rows (address, since). Secondary content arrives as a page turn (the
window slides the panel) rather than expanding in place. Log is typeset like a
console printed on paper: numbered lines, hanging indent.
Risk: a light theme only — cannot serve the existing System/Dark toggle without
a second palette, so it would need a dark companion generated from the same seed.

### 4. Signal — playful / light (seed 7)
text #21171d · bg #fdf4f9 · surface #f3eaef · primary #bb2994 · accent #7c7011
border #e3dadf · on-primary #ffffff
Shape: 14 radius, bold 2px outlines, chunky buttons, a status "pill" that changes
both color and icon. Connection state is carried by a single large glyph with a
shape change per state (shield / arc / dash) so it works without color. Deliberate
outlier: friendlier than a corporate tool usually looks.
Risk: playful fights the "corporate VPN" mood; chosen only if the user wants the
app to feel less grim than the official client.

### 5. Forge — warm / dark (seed 42)
text #feeded · bg #0f0908 · surface #1b1313 · primary #ab6262 · accent #5ab297
border #261f1e · on-primary #ffffff
Shape: 6 radius, warm dark brown-black rather than blue-black, left-aligned dense
layout, eyebrow labels in small caps. Status is a compact *row* at the top (dot,
name, address, time) with the action button to the right; everything else stacks
below as tight rows. Optimised for a tall narrow window that never grows.
Risk: the warm palette is unusual for infrastructure software; it reads more like
an editor than a network tool.

## Not yet decided
- Which concept the user picks (blocks the Color contract section).
- Whether any light concept gets a generated dark companion from the same seed.
