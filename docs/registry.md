# Windows Display Configuration Registry

The Windows CCD (Connecting and Configuring Displays) subsystem persists display configuration in two protected registry keys under:

```
HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\
```

Both keys require **administrator access** to read.

---

## Monitor IDs

Throughout both keys, monitors are identified by a string such as:

```
DEL430F6C19C34_34_07E8_46
SNY07CB16843009_01_07E7_C7
```

The format is: `<MFR><ModelCode><Serial>_<Week>_<Year>_<Index>`, where manufacturer and model code come from the monitor's EDID. The first 6–7 characters (`DEL430F`, `SNY07CB`) match the model code embedded in the display device path returned by `DisplayConfigGetDeviceInfo`:

```
\\?\DISPLAY#DEL430F#5&155a3a47&0&UID4353#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}
              ^^^^^^
```

---

## Connectivity Key

```
HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity\
```

This is the most useful key for understanding automatic topology switching. It stores, for each **physical display set** (unique combination of connected monitors), which topology configuration Windows will apply the next time that combination is detected.

### Key naming

Each subkey is named `<SetId>^<Hash>`, where `Hash` is the uppercase MD5 of `SetId` (UTF-8 encoded), and `SetId` is the `^`-joined list of monitor IDs in the display set:

```
DEL430F6C19C34_34_07E8_46^SNY07CB16843009_01_07E7_C7^<Hash>
```

The `^` separator here denotes set membership — it has no topology meaning.

> **Theory:** Windows registry key names are capped at 255 characters. A SetId with many long monitor IDs joined by `^` could exceed this limit, causing the prefix to be truncated. Appending `^MD5(full_SetId)` lets Windows find the correct entry by hash even when the prefix is truncated. The `SetId` value stored *inside* each subkey is the authoritative full string, confirmed after the hash lookup.

### Values

| Value | Type | Meaning |
|-------|------|---------|
| `SetId` | `REG_SZ` | The monitor IDs in this display set, joined with `^` |
| `Recent` | `REG_SZ` | Configuration key Windows will apply next time this display set connects |
| `Internal` | `REG_SZ` | Stored configuration key for Internal (primary-only) topology |
| `External` | `REG_SZ` | Stored configuration key for External (secondary-only) topology |
| `eXtend` | `REG_SZ` | Stored configuration key for Extend topology |
| `Clone` | `REG_SZ` | Stored configuration key for Clone topology |

Not all topology values are present in every entry — only topologies that have been explicitly saved appear.

### Example (Dell monitor + Sony TV)

```
SetId    = DEL430F6C19C34_34_07E8_46^SNY07CB16843009_01_07E7_C7
Recent   = SNY07CB16843009_01_07E7_C7       ← remembered: External
Internal = DEL430F6C19C34_34_07E8_46
External = SNY07CB16843009_01_07E7_C7
eXtend   = DEL430F6C19C34_34_07E8_46+SNY07CB16843009_01_07E7_C7
Clone    = DEL430F6C19C34_34_07E8_46*SNY07CB16843009_01_07E7_C7
```

`Recent` points to the Sony-only configuration ID, so Windows switches to External (TV only) whenever this display set is detected. Changing `Recent` to the `Internal` value (`DEL430F...`) would make Windows apply primary-only instead.

### Single-monitor entries

A single-monitor display set has no `^` in the SetId:

```
SetId    = DEL430F6C19C34_34_07E8_46
Recent   = DEL430F6C19C34_34_07E8_46
Internal = DEL430F6C19C34_34_07E8_46
```

---

## Configuration Key

```
HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Configuration\
```

Stores the actual display settings (resolution, refresh rate, position, pixel format, etc.) for each saved configuration. Each entry corresponds to one of the configuration IDs referenced by the `Connectivity` key's topology values.

### Key naming

Each subkey is named `<ConfigId>^<Hash>`, where `Hash` is the uppercase MD5 of `ConfigId` (UTF-8 encoded), and `ConfigId` encodes both the monitor set **and** the topology via its separator character:

| Separator | Topology |
|-----------|----------|
| *(none — single monitor)* | Internal or External (single active display) |
| `*` | Clone — one GPU source driving multiple targets |
| `+` | Extend — each monitor has its own GPU source |

Examples:
```
DEL430F6C19C34_34_07E8_46^<Hash>                              — Dell only
SNY07CB16843009_01_07E7_C7^<Hash>                             — Sony only
DEL430F6C19C34_34_07E8_46*SNY07CB16843009_01_07E7_C7^<Hash>  — Clone
DEL430F6C19C34_34_07E8_46+SNY07CB16843009_01_07E7_C7^<Hash>  — Extend
```

> **Theory:** Same as Connectivity — the hash guards against key name truncation at the 255-character registry limit, with the `SetId` value inside the subkey serving as the authoritative full string.

### Top-level values

| Value | Type | Meaning |
|-------|------|---------|
| `SetId` | `REG_SZ` | The configuration ID (same as the key name prefix) |
| `Timestamp` | `REG_QWORD` | Windows FILETIME of when this config was last saved |

### Source subkeys (`\00`, `\01`, …)

Each numbered subkey represents one GPU source (output). Extend configs have one subkey per active monitor; Clone and single-display configs have one.

| Value | Type | Meaning |
|-------|------|---------|
| `PrimSurfSize.cx/cy` | `REG_DWORD` | Surface resolution (width × height) |
| `PixelFormat` | `REG_DWORD` | Pixel format identifier |
| `Position.cx/cy` | `REG_DWORD` | Virtual desktop position (signed; `0xfffff100` = −3840) |
| `CcdDbVersion` | `REG_DWORD` | CCD database schema version |

### Target subkeys (`\00\00`, `\00\01`, …)

Each source subkey contains numbered target subkeys for the physical monitors driven by that source.

| Value | Type | Meaning |
|-------|------|---------|
| `ActiveSize.cx/cy` | `REG_DWORD` | Active display area in pixels |
| `VSyncFreq.Numerator/Denominator` | `REG_DWORD` | Vertical sync rate (e.g. 120/1 = 120 Hz) |
| `HSyncFreq.Numerator/Denominator` | `REG_DWORD` | Horizontal sync rate |
| `PixelRate` | `REG_DWORD` | Pixel clock rate |
| `ScanlineOrdering` | `REG_DWORD` | 1 = progressive |
| `Scaling` | `REG_DWORD` | Scaling mode |
| `Rotation` | `REG_DWORD` | 1 = landscape |
| `VideoStandard` | `REG_DWORD` | Signal standard; `0xFF` = inactive/no signal |
| `DwmClipBox.*` | `REG_DWORD` | DWM visible region (left/top/right/bottom) |

A `VideoStandard` of `0xFF` on a target in a Clone config indicates that target is inactive (present in the configuration structure but not actively driven).

---

## MonitorDataStore Key

```
HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\MonitorDataStore\
```

Stores per-monitor metadata (HDR capability, auto colour management support, etc.). Each subkey is named with the same monitor ID format as above. This key does **not** store topology information.

---

## How automatic topology switching works

1. A monitor connects (HDMI signal detected, display plugged in).
2. Windows builds the current display set from all physically connected monitors.
3. Windows looks up the matching `Connectivity` entry for that display set.
4. Windows reads the `Recent` value from that entry.
5. Windows finds the corresponding `Configuration` entry and applies its stored settings.

To prevent automatic topology switching for a given display set, change the `Recent` value in the matching `Connectivity` entry to point to the desired topology's configuration ID (e.g., change it from the `External` value to the `Internal` value).
