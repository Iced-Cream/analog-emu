# SMP slot list + FUNC+YES slot browser for DSP SAMPLE (research 2026-10-09)

Built 2026-10-09 (rytm-mods 08a8b0f; results in mods/0023-teensy-drive/design.md).

OS 1.73, lp3 build. [M] measured in rytm-emu, [R] read in the disassembly
(stock image, bytes identical in lp3), [I] inferred. Not built yet.

Goal (user): on a DSP SAMPLE track the SMPL page's SMP knob opens the stock
list "0:OFF, 1:name, 2:---, ..." of the Teensy's per-project pool; FUNC+YES
(or YES on an empty slot) opens the file browser to set that slot's file. The
encoder folder walk (0022 fld_push, 0B/0C) goes away.

## Stock mechanics

- SMPL knob handler 0x400d29cc: param id via page vmethod +156; id 41 (stock
  SMP) -> list function 0x400d2618(view, knob), page_knob_turn 0x40039f3c,
  0x400d2618 again. "== 41" checks at 0x400d2a1c, 0x400d2a8a, 0x400d2b62 (knob
  push), 0x400d2bc4 (FUNC+SMPL) [R]. First turn opens the list, later turns
  change the value [M].
- DSP SAMPLE's SMP knob reports id 443 (0x1bb) today, so no list [M]: from
  dsps_p2_order at 0x400b1ce2, knob 4's entry at 0x400b1cee = 443 [M]. Records
  41 and 443 hold the same word 19 [M]; record 41 range 0..127 [M].
- SampleListView: constructor 0x400e40a6(this, track, value, knob, 0x46, 0x3a, 1),
  only caller 0x400d279e [R]; 128 items, 6 rows visible [R/M]; key handler
  0x400e4432 (vtable 0x401e70d4 +8) [R]; "CONFIRM? Y/N" title 0x400e3fda [R].
- Row label 0x400e3c2e, every draw [M]: "%d:%.16s" (0x40258301). Index 0 =
  "OFF" (0x4025436e); empty test 0x400b8126(acc, i): 0<i<=127 and
  strlen(nametab[i]) == 0, nametab = RAM 0x41942350[i] (0x4009cf7e), "---" text
  0x4025cf29; name 0x400b80ae(a0 = std::string out, acc, i) [R]. Project slot
  refs: 128 x 16 B at project object +58 + 16 slot (0x400b7228, clamps) [R].
- The footer "__: [A01]" is kit/pattern, on every page [M/I].
- 0x400b8126 is also accessor vtable slot +52, used by the free-slot search
  0x400b81c4 (LOAD TO PROJ): hook its call sites in the list only, never the body.
- YES in the list (0x400e448e): FUNC held (event bit 1, 0x40071f24) or empty
  slot, idx != 0 -> new 0x564 B, 0x400e37fe(obj, idx) (call at 0x400e45e8),
  view_show_popup; else confirm [R]; FUNC+YES opened the browser [M].
- Browser constructor 0x400e37fe(this, slot): slot at +1376; slot ref via
  0x400b7228 (call at 0x400e38c8), checked by 0x4007fff4, opens its folder with
  the cursor on the file; pick callback 0x400e39f6 [R].
- Pick callback 0x400e39f6: ref = 0x4007e62c(model, row), check 0x400801a0,
  0x400b712e(manager, &ref, slot) at 0x400e3ab4, result ignored [R]; [M] BD
  KNOCK into slot 1 loads once, the browser stays, the row shows ">1".
- ">N" mark: row drawer at 0x400e60ac calls 0x400b8348(acc, ref16 by value),
  first equal slot (words 0,1; 0x400121f4; null test 0x40012212), "\x07%d" if
  >= 0; only when the browser's +1352 accessor is set [R]. Teensy rows reach it
  with ref {7e800000, 1, 3e8, 0} [M].
- 128 rows, value 0..127, row 0 OFF: pool slots 1..127 map 1:1 to SMP values.
  The Teensy's pool must then be indexed from 1 (slot N = pool[N-1]) [I].

## Risks

- 0022 fld_push eats plain YES/NO on the DSP SAMPLE SMPL page [M: YES with the
  list open sent 0B, the list's handler never ran]. Must go.
- Sound saved to the +Drive sound pool and loaded elsewhere: word 19 may be
  re-linked/zeroed by reference (measure). "Unload unused samples" counts SMP=N
  on DSP SAMPLE as using Rytm slot N (harmless). Copy/paste a sound to a stock
  track plays Rytm slot N. The pool is keyed by project name: SAVE AS NEW gives
  an empty pool unless the Teensy copies it. Map ext_param_record(19, 45) to 41.

## Design

A. 0021: dsps_p2_order[3] = 41 (was 443, at 0x400b1cee in lp3) and
   ext_param_record(19, 45) -> 41 [M with a poke: list opens on DSP SAMPLE,
   value moves, FUNC+YES opens the browser]. FLD (word 20) freed.
B. Six call-site retargets (4EB9 xxxxxxxx -> 4EB9 new), verified in lp3:
   - P1 0x400e3c54 (4eb9400b8126) label empty test: pool context -> pool slot
     empty (0 -> 0, >127 -> 1); else stock.
   - P2 0x400e3cae (4eb9400b80ae) label name (asm thunk, string out via a0):
     pool context and 1..127 -> 0x401bcd4c(a0, pool_name[i], &tmp); else stock.
   - P3 0x400e44ba (4eb9400b8126) YES's empty test: as P1.
   - P4 0x400e45e8 (4eb9400e37fe) pool context: slot |= 0x1000, then stock.
   - P5 0x400e38c8 (4eb9400b7228) slot & 0x1000 -> null ref {-1,0,0,0} (as
     0x4027c1e8): the browser opens at the root (TEENSY listed).
   - P6 0x400e60ac (4eb9400b8348) Teensy file ref (id & 0xFF800000 ==
     0x7E800000) -> its pool slot or -1; else stock.
   Pool context = SEL_TRACK (0x41318982) < 12 and MACHINE(t) (0x8000ea00[t])
   == 45 [M].
C. 0023 td_load_entry (detour of 0x400b712e): slot (sp@(12)) & 0x1000 and a
   Teensy file -> f = (id>>12)&0x3ff, e = id&0xfff, name from td_cache into the
   pool table (slot & 0x7f), send 10 <project> then "assign", return 1; a
   +Drive file in pool mode -> 0.
D. 0022: route the pool-table answer to 0023's TDRV mailbox (today every
   command >= 0x0a goes to 0021's); remove fld_push and 0B/0C.
E. Rytm pool table: 128 x {u16 folder, u16 file, char name[14]} = 2304 B from
   0x400829d4 on first use; a valid flag and the project it belongs to.
F. SysEx (F0 7D 52 54 ...), numbers to be chosen (11 is planned for note-off):
   Rytm -> Teensy: assign <slot> <f hi lo> <e hi lo> (after 10 <project>);
   ask <first> (when a pool list draws and the table is stale; rate-limited);
   optionally clear <slot>. Teensy -> Rytm: entries <first> <total> {<slot>
   <f hi lo> <e hi lo> <13 name bytes>} x <= 6, first = 0 replaces; pushed
   after 10, 0F and assign. Labels redraw continuously, so late names appear.
G. Optional: record 41's value text (DESC(41)[8] = 0x400fb454 [M]) showing the
   pool name on machine 45; opening the browser inside TEENSY via
   0x4007eb04(model, 0x7E000000) (unmeasured).

Size [I]: 36 B of patches + 4 B data; ~650-900 B new code; 2.3 KB RAM.

## Measure when built

1. DSP SAMPLE SMP turn: 0x400e40a6 x1, "0:OFF", "---", names after an
   injected entries message, one ask per list open.
2. Stock track: Rytm slot names, no ask, a pick still loads (0x4009df06 x1).
3. YES on an empty pool slot / FUNC+YES: 0x400e37fe with slot & 0x1000, root.
4. Pick a TEENSY file: 10 then assign on USB out, 0x4009df06 x0, no 0x7E ref
   in the slot table, ">N" at once, the name in the list.
5. Pick a +Drive file in pool mode: refused.
6. LOAD TO PROJ with a DSP SAMPLE track selected still finds a free Rytm slot.
7. NO reverts SMP; P11 in the 04 report = the slot; survives project save/load.
8. Sound save/reload elsewhere: word 19 kept?
9. 0023's guards unchanged; no faults.

Scratch (may be gone after a reboot): scratchpad/smp/ (card.img, cold.snap,
smp1, br5, dsps.snap, pa.snap with --poke 400b1cee=29, t1.snap).
