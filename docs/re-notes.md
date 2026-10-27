# Reverse-engineering notes — Canon mac-capt v10.0.9

Source: `mac-capt-v1009-uken.dmg` → `Canon_CAPT_Installer.pkg` → `Canon_CAPT.pkg` (macOS 11+).
All binaries are universal x86_64 + arm64. Analysis done on the arm64 slices with
`llvm-objdump` / `llvm-nm` (symbols are not stripped).

No Canon code or binaries are committed to this repo; only facts about the protocol.

## Canon pipeline

```
PDF → capdftopdl (CUPS filter) → libcaptfilter2.dylib  (raster → CAPT spool stream)
    → cnbma2 (CUPS backend) → ccpd (daemon/broker)
    → captmoncnab3 (bidi plugin, "*opbidiPlugin" in PPD) → caiousb.dylib → USB
```

## Model mapping (PPD `*CNTblModel` → `_tTargetModelTbl` in libcaptfilter2)

| CNTblModel | Model    | Engine id | Codename | filter lib    | bidi plugin   |
|-----------:|----------|-----------|----------|---------------|---------------|
| 1          | LBP3000  | 0x2A30    | P1080    | captfilter2   | captmoncnab3  |
| 4          | LBP5000  | 0x2198    | P860     |               | captmoncnac4  |
| 5          | LBP3300  | 0x2A94    | P1090    |               | captmoncnab5  |
| 9          | LBP3500  | 0x2B5C    | P1110    |               | captmoncnab6  |

The LBP2900 is absent from v10.0.9. The LBP3000 (P1080) path is the closest
relative; the open-source captdriver drives both LBP2900 and LBP3000 with the same code.

## Byte order

`buftool_new(size, 0)` → little-endian for shorts and longs (`buftool_write_short`/`_long`).

## Spool stream (filter → ccpd), libcaptfilter2 `Cmd_*`

Header: `u16 type, u16 total_len` then body.

| type   | builder          | len  | body |
|--------|------------------|------|------|
| 0x0001 | Cmd_JCBeginJob   | 40   | u32 0xFADEDACE, 5×u32 job fields, 3×u32 0 |
| 0x0002 | Cmd_JCPageInfo   | 112  | u32, 64 bytes, 7×u32, 6×u8, 6×u8 0 |
| 0x0003 | Cmd_JCEndJob     | 4    | — |
| 0x0004 | Cmd_JCEndPage    | 6    | u8, u8 |
| 0x8000 | Cmd_LC*Data      | u32! | `u16 0x8000, u32 len(n+8), u16 x, data[n]` |

`Cmd_LCBlackLastData` appends a trailer `8000 0c000000 0000 BD 3C BC BC`.

## CAPT printer commands (little-endian `u16 opcode, u16 total_len, payload`)

Image / page (built in filter, forwarded by plugin):

| opcode | name (Canon fn)       | notes |
|--------|-----------------------|-------|
| D0A0   | IcBeginPage           | 44 bytes total: 2×u16, 18×u8, 7×u16, 3×u8, u8 0 |
| D0A1   | IcBeginData           | 4 bytes |
| D0A2   | IcEndPage             | 4 bytes |
| D0A4   | IcColorPlane          | colour only |
| D0A9   | (CopyPage2)           | |
| C0A0   | IcVideoData           | 4 + n bytes compressed raster |
| C0A4   | IcBlackEnd/ColorEnd   | 4 bytes, end of image data |
| C0A3/5/6/7 | blank page / packets | |

Engine / status (captmoncnab3):

| opcode | Canon fn                     |
|--------|------------------------------|
| A0A1   | CheckPaperStatus             |
| A0A2   | CheckOutputStatus            |
| A0A4   | GetCartridgeStatus           |
| A0A6   | GetCalibrationData           |
| A0A7   | GetJobInfo                   |
| A0A8   | GetExtendedStatus / GetStatus|
| A1A0   | GetDeviceId                  |
| A1A1   | GetPrinterInfo               |
| A1A8   | EngineGetEngineVersion       |
| A1A9   | EngineGetPartsList           |
| A1AA   | EngineGetEngineID            |
| A1AB   | EngineGetUSBID               |
| A2A0   | EngineReservePrinter         |
| A2A1   | GetMisprintStatus            |
| A2A2   | GetServiceCallStatus         |
| A2A3   | GetJamStatus                 |
| A2A4   | GetWaitingStatus             |
| A2A7   | GetIllegalConnectionStatus   |
| A3A1   | GetFlashData                 |
| A3A2   | AnnounceJob                  |
| A3A3   | EngineGetCounter             |
| A3AB   | GetPageStatus                |
| E0A0   | GetStatus (ack / status)     |
| E0A1   | ResetEngine                  |
| E0A2   | ClearError                   |
| E0A3   | ClearMisprint                |
| E0A4   | DiscardData                  |
| E0A5   | GoOnline                     |
| E0A6   | GoOffline                    |
| E0A7   | StartPrint                   |
| E0A9   | EngineReleasePrinter         |
| E0AB   | EngineCommand                |
| E0AD   | Cleaning                     |
| E1A1   | SetJobInfo                   |
| E1A2   | SetLEDStatus                 |
| E1A3   | (multi-part response read)   |
| E1A6   | SetPrintedPages              |
| E1A9   | EngineSelectPaperInput       |

## Engine framing (captmoncnab3)

* Command: `u16 opcode, u16 total_len, payload` written in one bulk OUT.
* Response: read 6-byte header `u16 opcode, u16 total_len, u8, u8`, then the rest
  in ≤64-byte reads. opcode 0 → failure; len ≤ 5 → fatal.

| opcode | request payload (after 4-byte header) | response |
|--------|---------------------------------------|----------|
| A1A0   | —                                     | device id |
| A0A8   | — (used when NOT reserved)            | 60-byte extended status |
| E0A0   | — (used while reserved)               | status |
| A2A0   | `u16 arg, u16 0x001E, u32 0` (len 12) | 8 bytes: hdr6 + `u16 session` |
| E1A1   | job info (see below)                  | hdr |
| E1A6   | `u32 printed_pages` (len 8)           | hdr |
| E0A6   | `u8 x, u8 0` (len 6)                  | hdr |
| E0A2 / E0A4 / A3A2 | —                         | hdr |
| E0A7   | `u16 page` (len 6)                    | hdr |
| E0A5   | `u32 0xADEADBEE, u16 a, u8 0, u8 1, 4×u16` (len 20) | hdr |
| E0A9   | `u16 session` (len 6)                 | hdr |

E1A1 SetJobInfo: `u16 0, u16 0, u32 x, u16 len_a, u16 len_b, u16 len_c, u16 0, u8, u8,
u16 session, u16, u16, u16, 6×u8, 10×u32 0, utf16 host, utf16 user, utf16 doc`.

Canon flow: PrintJob1 → ReservePort → CheckDeviceId(A1A0) → ReservePrinter(A2A0)
→ SetJobInfo(E1A1) → SetPrintedPages(E1A6) → PrintContinuous { GoOffline(E0A6),
ClearError(E0A2), DiscardData(E0A4); per page: SendDataQueue (D0A0, D0A4, D0A1) →
StartPrint(E0A7 page) → SendImageQueue (C0A0 bursts) → C0A4, D0A2; DoOnline(E0A5);
status polling E0A0 } → ReleasePrinter(E0A9).

## Page data (P1080)

Per page the filter emits: `D0A0` (44 bytes), `D0A4` (12 bytes: 8-byte REF descriptor),
`D0A1` (4), compressed data, `C0A4`, `D0A2`. The plugin splits the compressed data into
`C0A0` packets of at most 0xFF00 data bytes (`u16 0xC0A0, u16 n+4, data`).

D0A0 body (40 bytes after the 4-byte header):

```
+0  u16 0        +2  u16 0x2A30 (engine)  +4 u8 paper code  +5 0  +6 u8 source  +7 0
+8..11 u8 density (default 0x1F)  +12 u8 media mode (0 for model 1)  +13 u8 0x11
+14 u8 4  +15 u8 0  +16 u8 bpp=1  +17 u8 1 (mono)  +18 u8 0  +19 u8 0  +20 u16 0
+22 u16 left margin px  +24 u16 top margin px  +26 u16 bytes/line  +28 u16 lines
+30 u16 paper width px  +32 u16 paper height px  +34 u16 flags=0  +36 u8 media code (plain=1)
+37..39 0
```

Line width is rounded up to a multiple of 32 pixels. Paper codes (DMPAPER → code):
Letter 13, Legal 12, Executive 10, A4 2, A5 3, B5 7, Monarch 23, Com10 22, DL 24, C5 21,
Index 3x5 64. Paper sizes at 600 dpi: A4 4960×7014, Letter 5100×6600.

## Compression ("Slim", libcanon_slim lCaptCompEx)

REF descriptor for P1080 (CompParamTbl_P1080 row 0, also the hard-coded default):
`01 04 01 01 00 F9 00 00` = left 1 byte, alt-left 4 bytes, up 1 line, alt-up 1 line
shifted +7 bytes (`src - W - (int8)-7`), no far reference. Minimum match length 2.

Bit writer: MSB-first; each completed byte is XORed with 0xBC before output.

Command codes (value, bits): 0 `1`, 1 `01`, 2 `0011`, 3 `0010`, 4 `0001`, 5 `00001`,
6 `000001`, 7 `00000011`, 8 `00000010`, 9 `00000000`, 10 `0000000111`, 11 `0000000110`.

Count n (1..127): class code from count_table[n==0 ? 0 : floor(log2 n)+1] =
`000000`,`11`,`10`,`01`,`001`,`0001`,`00001`,`000001`; then floor(log2 n) low bits of n
(only when n ≥ 2). n ≥ 128: emit `00000011`, count(n>>7), then command, then count(n & 127).

Per position (lines independent, matches stop at line end, max 0x3FFF):
1. copy from current up-ref ≥2 → cmd 0
2. copy from current left-ref ≥2 → cmd 4
3. far-ref (if any, within current line) ≥2 → cmd 5
4. alternate up-ref ≥2 → cmd 2 (switch current up)
5. alternate left-ref ≥2 → cmd 6 (switch current left)
6. else literal byte b: if in 16-entry MRU cache at depth k → `01` + 4-bit k (move to
   front); else if b == 0 → `00000010`; else `0010` + 8 bits (b ^ 0xFF); then push b.
References before the start of the block are invalid. End: `0000000111`, flush partial
byte (zero padded), XOR 0xBC, pad with 0xBC to a multiple of 4 bytes.

## LBP2900 job sequence (from captdriver)

The engine conversation now follows captdriver's LBP2900 path
(github.com/mounaiban/captdriver), which prints on real hardware, including the macOS
fix from github.com/duy12i1i7/canon-LBP2900-for-macOS: always poll the extended
status `A0A8`, because the page counters only live in that record. Only protocol
facts are used here; no captdriver code is copied.

Status record (bytes after the 4-byte reply header): `u16` words at 0, 8, 10, 12,
24, 30, 38; page counters decoding/printing/out/completed at 14/16/18/20 and
received at 34. Word 0 bits: 7 busy, 5/4 uninitialised, 2 buffer full, 1 no paper.
Some replies carry their length in BCD.

```
A1A1, A0A8, A3A2, A2A0 (00 00 1E 00 00 00 00 00) → job = reply u16
E1A2 (13 00 … 16 bytes), wait ready, E1A1 stage 1, wait ready
per page:
  if uninitialised: E0A3, E0A2, E0A4, wait, E0A5 (EE DB EA AD + 12 × 00), wait
  wait while buffer full
  D0A9 { D0A0 page params, D0A4 refs, D0A1, D0A2 }
  C0A0 … (each 70-line band compressed on its own; E0A0 busy check every 16th)
  C0A4
  wait page_received == page_decoding; E1A1 stage 2 (page = decoding), wait
  E0A7 (u16 decoding), wait; E1A1 stage 6; wait page_out == decoding
end: wait page_completed == decoding; E1A1 stage 4; E0A9 (u16 job)
```

`E1A1` payload (72 bytes): `u32 0, u16 page, u16 0, u16 0 ×3 (name lengths), u16 0,
u8 stage, u8 1, u16 job, i16 -60, i16 -120, u16 year-1900, u8 month0, u8 day,
u8 h, u8 m, u8 s, u8 1, 40 × 00`.

Page params: darkness byte `0`, then `1C 1C 1C`; byte +22 (image refinement) `2`;
margins 14 pt; line bytes = page width in points; printable width and line count
in pixels. Hi-SCoA bytes are XORed with 0x43 and its codes are the bit-inverse of
the Slim codes above, so Slim output (XOR 0xBC) is the same bitstream.
