# Diablo II characters in chat — `d2-characters.zip`

A realm character's chat statstring says what the character wears
([`d2-equipment.json`](D2-EQUIPMENT-FILE.md) names the items). This pack lets a bot draw the
character the way Diablo II's character-select screen does: animated, in its armour, weapons and
colours. The server builds it from the operator's own install at startup and serves it over BNFTP
beside `d2-equipment.json`.

## Getting it

`SID_GETFILETIME` / BNFTP for `d2-characters.zip` (the name is `diablo2.character_pack`), about
7 MB. The server rewrites it only when its contents change, so one download lasts until the
operator's install changes. A bot can offer it as an opt-in ("D2 layout") and fetch it once.

## What is in it

| Entry | Contents |
|---|---|
| `manifest.json` | `format` = `bnetcc-d2-characters`, `version` = 1, the rules below, and the tables they use. |
| `palette.bin` | 256 RGB triples — the game's palette. Every GIF carries the same table. |
| `tints.bin` | 8 × 21 × 256 bytes: map for transform `t` (1..8) and colour `c` (0..20) at `((t - 1) × 21 + c) × 256`, palette index → palette index. |
| `parts/<class>/<name>.gif` | One body-part graphic, facing the viewer as the character screen draws it: every frame, full size, palette index 0 transparent. |

Manifest tables:

| Key | Meaning |
|---|---|
| `direction`, `tick_ms` | 0 (the character screen's facing index; `order` is indexed by it, and the parts were taken from the file direction it maps to) and 40 (milliseconds per tick). |
| `classes`, `modes`, `components`, `weapon_classes` | Token lists the rules index: `classes[0]` = `AM` … `[6]` = `AI`; `components` = `HD TR LG RA LA RH LH SH S1…S8`; `weapon_classes[1]` = `hth`, then `1ht 2ht 1hs 2hs bow xbw stf 1js 1jt 1ss 1st ht1 ht2`. |
| `hand_pairs` | 15 × 15: weapon class for `[right hand class][left hand class]`; 0 = impossible. |
| `slots` | 256 entries, by graphics value: `code`, `hand`, `two_handed`, `reserved_hand`, `armor`, `helm`. The same values `d2-equipment.json` uses. |
| `animations` | By name (`BATNHTH`): `class`, `mode`, `weapon_class`, `frames`, `speed`, `layers` (`[component, weapon class its part files use]`), `sequence` (one loop as `[frame, ticks]`), `order` (per frame, component indices back to front). |
| `parts` | By name (`BAHDCAPTNHTH`): `file`, `left`, `top` (offset of the frame's top-left from the character's base point), `width`, `height`, `frames`. |

## From a statstring to a picture

The same rules are in the manifest's `rules`.

1. **Read the portrait** — the 33 bytes after the second comma. `class = p[13] - 1`, `status = p[26]`,
   and for components `c` = 0..10: graphics `g[c] = p[2 + c]`, tint `t[c] = p[14 + c]`.
2. **Stance.** Hardcore and dead (`status & 0x0C` = `0x0C`): not in the pack, draw nothing (the game
   draws a ghost). Hardcore: `NU`. Otherwise `TN`.
3. **Hands.** `rh = g[5]`, `lh = g[6]`, `sh = g[7]`; `both = rh != 255 && lh != 255`. For a hand value `v`
   (`s = slots[v]`): its class is `s.two_handed` if `both`, or — right hand only — if `lh == 255 &&
   sh == 255 && s.two_handed != s.hand`; otherwise `s.hand`. If the class is 13 or 14 (claws) and the
   character is not an Assassin, or `s.armor`, use `s.reserved_hand`; if that is still 13 or 14 off an
   Assassin, 0. An empty hand is 0.
4. **Animation.** `w = hand_pairs[right][left]`; if 0, draw nothing (the screen shows its fallback
   figure). The animation is `classes[class] + mode + weapon_classes[w]`, upper case.
5. **Parts.** For each `[c, L]` in the animation's `layers`: `v = g[c]`; the code is `lit` if `v` is 0 or
   255, or `c` is 0 and `slots[v].helm` is false, or the slot has no code; otherwise `slots[v].code`.
   The part is `classes[class] + components[c] + code + mode + L`, upper case — if the pack has no such
   part, that component is not drawn.
6. **Tints.** `x = t[c]`; none if `x` is 0 or 255. `s = x - 1`, `colour = s & 31`, `transform = s >> 5`
   (0 means 8). None if the transform is 3 or 4 or the colour is over 20. Otherwise every non-zero
   pixel `p` becomes `tints[(transform - 1) * 21 + colour][p]`.
7. **Draw and animate.** Walk `sequence` — show frame `f` for `ticks × tick_ms`, then loop. For frame
   `f`, draw `order[f]` back to front: each component's part, GIF frame `min(f, frames - 1)`, with its
   top-left at `(left, top)` from a shared base point; skip index 0; colour through the palette. The
   bounding box of the parts drawn is the image.

Keep the game palette when displaying: tints are palette remaps, so re-quantising the GIFs would lose
them.

## How it was made

From the 1.14d `Game.exe`, reproduced in `d2_data::character` (addresses in its comments): the
character screen builds its figure from the portrait (`0x005066C0`), picks the stance
(`0x00439210`), the weapon class (`0x00504AF0`), the part files (`0x00503740`) and the tints
(`0x005038D0`), and draws the COF's layers each frame (`0x00503BA0`). Its facing index 0 selects
the COF's draw-order row 0, but each sprite file's frames through a direction table
(`0x00600C70`): in the 16-direction character files that is direction 4, straight toward the
viewer. The animation files are DCCs
and COFs from the operator's MPQs, decoded by `d2_formats::dcc` (written from Bilian Belchev's DCC
format description, with one correction — a cell's encoding-type bit is present only when its pixel
mask is non-zero) and `d2_formats::cof`.

A test draws nine characters (classes, both stances, tints, dual wield, claws, bows, crossbows,
Necromancer heads, Druid pelts) twice: once with the renderer, once by following only these rules
over the pack — the pixels, sizes and timings match.

Not covered: other directions or stances than the character screen's, dead hardcore characters, and
very old saves whose armour bytes predate 1.10. `tick_ms` assumes the screen draws at the game's 25
frames a second.
