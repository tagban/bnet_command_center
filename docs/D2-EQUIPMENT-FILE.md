# Diablo II equipment in chat — `d2-equipment.json`

A Diablo II realm character in chat carries a 33-byte portrait at the end of its statstring
(`PX2D<realm>,<character>,<portrait>`; see `bnetcc_proto::d2`). Bytes 2..=12 say what the
character is wearing — helm, body armour, weapons, shield — and bytes 14..=24 how each piece is
tinted. The values are not item ids, so a bot needs a map to name them. The server builds that map
from the operator's own Diablo II install at startup and serves it over BNFTP, next to
`icons.bni`.

## For bot authors

1. `SID_GETFILETIME` for `d2-equipment.json` (the name is `diablo2.equipment_file`) tells you
   the file's time. If you have no copy, or yours is older, fetch it over BNFTP like `icons.bni`.
   The server rewrites the file only when its contents change, so the time is stable.
2. Check `format == "bnetcc-d2-equipment"` and `version == 1`. A new `version` means the shape
   changed incompatibly.
3. For a realm user, take the bytes after the second comma of the statstring (the portrait).
   For each entry of `slots`, `portrait[offset]` is the graphics value (255 = nothing) and
   `portrait[tint_offset]` its tint.

The file:

| Key | What it holds |
|---|---|
| `portrait` | Layout reminders: `length` 33, `class_offset` 13 (class + 1), `level_offset` 25, `none` 255. |
| `slots[]` | The eleven equipment bytes in order: `index`, `name` (`head`, `torso`, `legs`, `right_arm`, `left_arm`, `right_hand`, `left_hand`, `shield`, `right_shoulder`, `left_shoulder`, `special`), `holds`, `component` (the game's name, `HD`…`S3`), `offset`, `tint_offset`, and `values[]`. |
| `slots[].values[]` | Every value that slot can hold: `value`, `code` (the graphics code), and either `items[]` (`code`, `name`) — every item drawn with that value in that slot — or `weight` (`light`/`medium`/`heavy`) for the six body armour slots. |
| `body_armor.sets[]` | Body armour writes six slots at once. `parts` is the torso, legs, right arm, left arm, right shoulder and left shoulder values in that order; `items[]` the armours with that pattern (normal, exceptional and elite versions share one). |
| `not_drawn.items[]` | Things worn where the character is drawn that leave the slot at 255 — circlets, arrows, bolts. A 255 head can be a circlet. |
| `tints` | `colors[]`, indexed by colour. A tint byte is `transform * 32 + colour + 1`, kept to a byte, so `colour = (byte - 1) & 0x1F` and `transform = (byte - 1) >> 5`; 255 is untinted. |
| `graphics[]` | The whole graphics table, `value` → `code`, for drawing tools. |

Several items share a value — a Cap, a War Hat and a Shako look the same — so a value names a
look, not an item: show the list, or the first name.

Reading a statstring, in Python:

```python
portrait = statstring.split(b",", 2)[2]           # PX2D<realm>,<character>,<33 bytes>
for slot in equipment["slots"]:
    value = portrait[slot["offset"]]
    if value == 255:
        continue
    look = next((v for v in slot["values"] if v["value"] == value), None)
    if look:
        print(slot["name"], look.get("weight") or ", ".join(i["name"] for i in look["items"]))
```

On this server a character is drawn with nothing until the game server keeps its items: the
portrait's equipment bytes are all 255 for now. Characters from other realms with items carry
real values.

## How the game computes the bytes (1.14d)

From `Game.exe` 1.14.3.71; implemented in `d2_data::appearance`, and checked against BNETDocs'
"Chat Statstrings" tables and the appearance bytes of a real save (libd2's `EpicSorc.d2s`).

- `PLRSAVE2_WriteSaveHeader` (`0x00568F20`) fills a save's sixteen graphics bytes (`0x88`) and
  sixteen tints (`0x98`) with `0x0063E510`; a realm's portrait carries the first eleven of each.
- **The graphics table** (`0x0063D710`) is built once. Slots 1–3 are `lit`, `med`, `hvy`. Then
  every item in class id order (weapons, armour, misc) whose type is a weapon, body armour,
  shield or helm — not a circlet — adds its `alternategfx` (its `code` if blank) unless the code
  is already in the table. It takes the next slot, except that a weapon skips slots that a list in
  `Game.exe` (`0x00744CA8`, `{code, item type}` × 255 — an older layout) gives a weapon, and armour
  skips slots it gives armour. A code placed past the next slot does not count as already there,
  so the next item with that look places it again: bows and crossbows hold several slots.
- **Looking up** (`0x0063D900`) returns the lowest slot equal to the item's graphics code or its
  own code; 0 → the byte becomes 255.
- **Head and hands** (`0x0063DA70`): value into the item's `component`. A one-handed weapon
  (`1hs`, `1ht`, `ht1` — hand classes 2, 3, 12 at `0x007446A0`) goes to the right hand if it is the
  active weapon, the left hand otherwise (`0x0063C050`); with a crossbow the left hand repeats the
  right hand (`0x0063D930`). Circlets are skipped.
- **Body armour** (`0x0063D690`): `Torso`, `Legs`, `rArm`, `lArm`, `rSPad`, `lSPad` (0–2) pick
  `ArmType.txt`'s token, looked up like any code, into components TR, LG, RA, LA, S1, S2.
- **Tints** (`0x0062C100`): a unique's, set's or affix's colour, `transform * 32 + colour`
  (`0x0062A250`), plus one; 255 when the item's `Transform` has no palette (0, 3, 4) or there is
  no colour (`0x00600C20`).

BNETDocs lists some values from earlier patches (for example second codes for claws); the file
is built from the install the server runs, so it matches 1.14d clients.
