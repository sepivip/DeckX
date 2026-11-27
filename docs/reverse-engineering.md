# Stream Dock AJAZZ Global 3.10.195.0902

## Local evidence

Windows enumerated the connected device as USB `0300:3002`, with a vendor HID interface and a separate keyboard interface. DeckX opened the vendor collection (usage page `0xffa0`) using its read-only startup probe.

REA 6.8.0 inventoried the installation and inspected both PE import tables. `Stream Dock AJAZZ.exe` is an x64 Qt application with QCefView and calls into `SDLibrary1.dll` for device discovery and connection. The DLL imports SetupAPI enumeration functions. REA's Rizin provider identified `HidD_GetAttributes`, `HidD_GetInputReport`, and related names in the DLL's string data, alongside exports for opening HID devices and running their read and write loops.

| Artifact | SHA-256 |
| --- | --- |
| `Stream Dock AJAZZ.exe` | `1c667ce9718a1f0c00770db1997d815a9391680548a756ae53ee0d651c73d834` |
| `SDLibrary1.dll` | `85e572c63743966b2b59bd9e4b68137b9a7e0df498899555621dc56718a9ce30` |

REA disassembly of the DLL shows these command builders writing the `CRT` prefix and command bytes into their packet buffers. Virtual addresses below use the PE's preferred image base.

| Vendor symbol | Virtual address | Observed bytes |
| --- | --- | --- |
| `SDDevice::addAdjustBrightnessPack` | `0x180017c90` | `CRT`, then `LIG` |
| `SDDevice::addFinishCommand` | `0x180018220` | `CRT`, then `STP` |
| `SDDevice::addHeartbeatPack` | `0x180018620` | `CRT`, then `CONNECT` |

Reproduce with REA 6.8.0 and the path to a local Rizin executable in `REA_RIZIN_COMMAND`:

```powershell
$vendor = 'C:\Program Files (x86)\Stream Dock AJAZZ Global'
$dll = Join-Path $vendor 'SDLibrary1.dll'
rea inspect-pe-imports (Join-Path $vendor 'Stream Dock AJAZZ.exe') --json
rea execute-rizin-command $dll 'iE~SDDevice' --json
rea execute-rizin-command $dll 'pd 105 @ 0x180017c90' --json
rea execute-rizin-command $dll 'pd 85 @ 0x180018220' --json
rea execute-rizin-command $dll 'pd 110 @ 0x180018620' --json
```

## AKP03E transport

The model-specific wire format comes from the public [ajazz-sdk](https://github.com/mishamyrt/ajazz-sdk) and [AKP03E protocol implementation](https://github.com/tomekceszke/ajazz-akp03e). DeckX sends 1,024 bytes per output report plus the zero report ID required by HIDAPI. To wake the AKP03E from its welcome screen, DeckX sends `DIS`, `LIG`, `CLE`, and `STP`, closes the HID handle, waits 500 ms, then reopens it and repeats initialization. Each key update sends `BAT` with the JPEG length and one-based key number, streams the JPEG, and commits with `STP`. The input parser uses the action and state bytes at offsets 9 and 10 of an `ACK` report.

The exact-model reference describes a clockwise 90° upload rotation. The editor exposes rotation because reports for related revisions disagree. A physical test after the wake change confirmed that the top-middle LCD key reports as key 2 and can start Stopwatch Timekeeper on the deck.

On the wired `0300:3002`, the large dial uses `0x50` and `0x51` for turns and `0x35` for a press. The lower-left dial uses `0x90`, `0x91`, and `0x33`. The lower-right dial uses `0x60`, `0x61`, and `0x34`. [Hardware-tested device mapping](https://github.com/Erik1028/ajazz-dock/blob/main/dock/gui.py)

DeckX uploads 64×64 JPEG tiles to the wired `0300:3002`. A photo of a 60×60 upload showed four extra LCD pixels on the top and right after the configured rotation. Exposing the partial JPEG blocks in a software decode reproduced that asymmetry. A 64×64 frame fills every block with intended key pixels.
