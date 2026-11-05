# QStreamKit

[![CI](https://github.com/dima-xd/qstreamkit/actions/workflows/ci.yml/badge.svg)](https://github.com/dima-xd/qstreamkit/actions/workflows/ci.yml)

Open client for the `QSTREAM` socket protocol exposed by Qualcomm's Adreno
Vulkan layer `VK_LAYER_ADRENO_qprofiler`.

The layer is not a system service. It runs inside an instrumented application
and listens on a TCP or abstract-unix socket. QStreamKit talks to that socket
from the host through `adb forward`, decodes the replies, and provides a small
CLI for profiling work: process and device info, FPS, frame capture, GPU scope
timing, GPU counter monitors and Perfetto experiments.

The protocol is undocumented. Every framing rule and payload layout in this
repository was reconstructed from `libVkLayer_ADRENO_qprofiler.so` and verified
on a device. See [Limitations](#limitations) for what is and is not covered.

Tested on: OnePlus 15 (CPH2747), Android 16, Adreno 840, retail `user` build,
SELinux enforcing. Other Adreno generations may use different payload layouts.

## Supported platforms

| Host | Status |
| --- | --- |
| Linux x86_64 | supported, CI tested |
| Windows x86_64 | supported, CI tested |
| macOS x86_64 | release binary |
| macOS arm64 | release binary |

The host client needs `adb` in `PATH`. TCP transport works on every supported
host. Abstract Unix sockets are Linux only. The probe app requires an arm64
Android device with Vulkan and the Adreno qprofiler layer.

## Features

| Area | Status |
| --- | --- |
| Register / process / device / counters | works |
| FPS stream | works while the app is top resumed |
| Immediate capture (start/stop) | works, foreground and offscreen |
| Frame capture (next presented frame) | works with a `VkDevice` pointer and active GPU scope |
| GPU scope start/stop and submit timing | works |
| GPU scope counter selection | works with A8x GSL block/counter ids |
| GPU scope monitor reports | works in start mode 2 |
| CSV export of submit timings | works |
| CSV export of monitor counters and samples | works |
| JSON output for probe, scope and monitor | works |
| Non-destructive counter add and explicit reset | works |
| Perfetto `gpu.renderstages` | not available in normal layer mode |
| Raw send / decode | works |

Non-goals:

- no root, no system service, no vendor binary redistribution
- no Snapdragon Profiler host replacement
- no guarantee that offsets hold for other driver versions

## Repository layout

```
crates/qstream-core       framing, record and capture parsers, token table
crates/qstream-transport  TCP and unix transports, idle read loop
crates/qstream-cli        the qstream binary
tools/probe-app           Gradle project for the debuggable Vulkan host
```

`tools/probe-app` is a development fixture, not part of the shipped client.
The layer cannot run without a hosting process, so the stand is required on the
device.

## Requirements

Client:

- Rust 1.87 or newer
- `adb`

Device:

- a debuggable application that loads the layer (the stand is provided)
- the vendor layer pulled from your own device

Stand build:

- Android Studio, or JDK 17/21 with the Gradle wrapper
- Android SDK platform 36, build-tools 35.0.0
- Android NDK 27.0.11902837 and CMake 3.22.1

Install the SDK packages with Android Studio's SDK Manager. Gradle finds the SDK
through `ANDROID_HOME`, `ANDROID_SDK_ROOT`, or `local.properties`. If your default
`java` is newer than 21, point `JAVA_HOME` at JDK 17 or 21 before running Gradle.

## Install

Prebuilt host binaries are attached to
[GitHub releases](https://github.com/dima-xd/qstreamkit/releases). Each archive
contains `qstream`, the README, both license texts and the trademark notice,
plus a SHA-256 checksum.

From crates.io, once published:

```
cargo install qstream-cli
```

From a repository checkout:

```
git clone https://github.com/dima-xd/qstreamkit
cd qstreamkit
cargo install --path crates/qstream-cli
```

## Build

```
cargo build --release
cargo test --workspace
```

The binary is `target/release/qstream` (`qstream.exe` on Windows). The workspace
minimum supported Rust version is 1.87.

## Device setup

Pull the layer from the device. It is not distributed with this repository:

```
adb shell find / -name 'libVkLayer_ADRENO_qprofiler.so' 2>/dev/null
adb pull <path> ./libVkLayer_ADRENO_qprofiler.so
```

Build and install the stand. Copy the layer into
`tools/probe-app/app/src/main/jniLibs/arm64-v8a/` first:

```
mkdir -p tools/probe-app/app/src/main/jniLibs/arm64-v8a
cp libVkLayer_ADRENO_qprofiler.so tools/probe-app/app/src/main/jniLibs/arm64-v8a/

cd tools/probe-app
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.example.qprofprobe/.MainActivity
adb forward tcp:20829 tcp:20829
```

On Windows use `gradlew.bat assembleDebug` and create the directory with
Explorer or PowerShell if you are not using Git Bash. You can also open
`tools/probe-app` in Android Studio and run the `app` module.

The layer JSON is stored in `tools/probe-app/app/src/main/resources/lib/arm64-v8a/`
and is packaged next to the `.so` automatically. It is a minimal manifest
written for this project from the Vulkan loader requirements; it is not copied
from Qualcomm. Compare it with the manifest that ships on your device before
using a different layer build. Debug builds use the standard
`~/.android/debug.keystore`; if the signing key changed, uninstall the old
package before installing the new APK.

The stand creates a Vulkan instance with the layer enabled, renders a triangle
through a swapchain, starts a foreground service and keeps the process alive.
When the activity is destroyed the stand switches to a 1280x720 offscreen
image, so the GPU keeps submitting. It also enters picture-in-picture when the
system asks for it.

Useful logs:

```
adb logcat -s QProfiler.VulkanShim   # layer init and frame requests
adb logcat -s qprofprobe             # stand state and vkDevice pointer
```

## Quick start

```
$ qstream probe
records 3
  [..] kind=0x0B010001 (process) payload=136 arg0=0x00000133
      pid=307 text="com.example.qprofprobe"
  [..] kind=0x0B010004 (counters) payload=16 arg0=0x00000133
  [..] kind=0x0B010002 (device) payload=280 arg0=0x00000133
      text="Adreno (TM) 840"

$ qstream fps --seconds 2
fps samples=6 truncated=false
fps min=59.85 avg=60.06 max=60.26 last=60.21

$ qstream capture --seconds 2 --csv submits.csv
capture submits=128 index=31461..31588 dur_us min=50 avg=193 max=836
capture csv submits=128 -> "submits.csv"
```

Frame capture needs the device pointer printed by the stand and an active GPU
scope:

```
$ adb logcat -s qprofprobe | grep vkDevice
I qprofprobe: vkDevice=0xb4000079444d9d40

$ qstream scope --seconds 1
scope bytes=6320 records=53 timing=52 submit_info=0 truncated=false

$ qstream counters list --group 9
block 9 TP counters=143
    9   1 PERF_TP_BUSY_CYCLES
    9   2 PERF_TP_STALL_CYCLES_UCHE
    9   3 PERF_TP_LATENCY_CYCLES

$ qstream monitor --seconds 2 --group 9 --counter 1 --csv monitors.csv
monitor pair group=9 counter=1 reset=false removed=3
monitor [2] frame=306 submit=554 gpu=39 pairs=9:1 value_pair=9:1 value=176328212 delta=- clock=0x6AEC74B9 t0=114690371997991 t1=114690372011087 t2=114690372011543 raw=272
monitor [4] frame=307 submit=555 gpu=39 pairs=9:1 value_pair=9:1 value=192758004 delta=16429792 clock=0x6AEC74B9 t0=114690372313487 t1=114690372320570 t2=114690372344886 raw=208
...
monitor [255] monitors_done
monitor reports=127 done_markers=1
monitor csv reports=127 -> "monitors.csv"

$ qstream capture-frame --device 0xb4000079444d9d40
$ qstream capture --frame --device 0xb4000079444d9d40 --seconds 2 --csv frame.csv
capture frame marker=29706 markers=2 start=19811 end=19813
capture submits=1 index=29954..29954 dur_us min=183 avg=183 max=183

$ qstream scope-off
```

## CLI reference

### Global flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `--tcp host:port` | `127.0.0.1:20829` | TCP target |
| `--unix @name` | none | abstract unix socket, or a filesystem path without `@` (Linux) |
| `--connect-ms N` | `800` | initial connect timeout |
| `--wait-ms N` | `0` | retry the connection for this long |
| `--idle-ms N` | `1000` | stop reading after this much silence |
| `--max-bytes N` | `1048576` | cap on one read burst |
| `--save FILE` | none | append raw replies to a file |
| `--serial SERIAL` | none | adb device serial for `perfetto` |
| `--allow-empty` | off | accept empty replies or zero records as success |
| `--json` | off | JSON output for `probe`, `scope` and `monitor` |
| `--endian le\|be` | `le` | byte order for requests and replies |

### Commands

| Command | Description |
| --- | --- |
| `probe` | send register and print the initial records |
| `version` | start GPU scope, print the version reply, then stop the scope |
| `send --hex HEX [--gap-ms N] [--no-read]` | send one or more raw frames |
| `listen` | connect and only read |
| `start` | start an immediate capture |
| `stop` | stop an immediate capture and read the reply for two seconds |
| `capture-frame --device HEX` | capture the next presented frame |
| `capture [--seconds N] [--frame] [--device HEX] [--csv FILE]` | capture a window of submits; `--frame` does not send stop |
| `fps [--seconds N]` | enable FPS, sample, disable |
| `scope [--seconds N]` | start GPU scope and collect submit timings; leaves the scope running |
| `scope-off` | stop GPU scope |
| `counters list [--group N]` | list A8x blocks and curated counter ids |
| `counters add --group N --counter N [--reset]` | add a counter pair; existing pairs are kept unless `--reset` is set |
| `counters remove --group N --counter N` | remove a counter pair |
| `monitor [--seconds N] [--group N] [--counter N] [--reset] [--csv FILE]` | start scope in monitor mode and decode counter reports |
| `frame-rate --value N` | set the frame rate period |
| `delimiters --value N` | set frame delimiter flags |
| `decode FILE` | parse and print a saved raw stream |
| `tokens` | print the known token table |
| `perfetto [--seconds N] [--out FILE] [--config FILE]` | try the Perfetto data source; honors `--serial` |

Examples:

```
qstream --wait-ms 3000 probe
qstream --unix @QProfilerSocketVK listen
qstream --endian be decode dump.bin
qstream send --hex 0900020b08000000000000000000000001000000
```

### Counter state

Pair state is process-global in the layer. The stock list is `(0,0)` and
`(27,0)`, where `27` is `ALWAYSON`.

- `counters add` is additive. It removes only a duplicate of the requested pair,
  then adds it. Existing stock and custom pairs stay in place.
- `counters add --reset` removes the curated pairs before adding. Use it when you
  want a clean list for a standalone experiment.
- `monitor --group N --counter N` removes the two stock pairs and the requested
  pair, then adds the requested pair. This makes it the first pair on a fresh
  process, so the compact cumulative value belongs to it.
- `monitor --reset` removes every curated pair first. It cannot remove an
  unknown pair that an earlier session added; restart the hosting app for a
  completely clean list.

A monitor report stores the cumulative value for the first pair only. If the
requested pair is not first, `qstream monitor` prints a note in text mode; in
JSON output compare `value_pair` with the pair you requested. Use one pair per
run for a reliable series.

### Output formats

`monitor --csv FILE` writes one row per decoded report:

```
index,frame,submit,gpu,value_group,value_counter,value,delta,clock,t0,t1,t2,raw_size,pairs,sample_count,samples
```

`pairs` is a quoted `group:counter,...` list. `samples` is a quoted
`0xID=value;...` list from the report's per-counter sample block; it is empty
when the report has no samples. The `delta` column is empty for the first
report of a pair.

`--json` is supported by `probe`, `scope` and `monitor`:

- `probe` returns bytes, truncation and a record array.
- `scope` returns counts plus a `timings` array with submit durations.
- `monitor` returns bytes, record and report counts plus an `items` array with
  pairs, value, delta, timestamps and samples.

Examples:

```
qstream --json probe
qstream --json scope --seconds 2
qstream --json monitor --seconds 2 --group 9 --counter 1
```

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | command completed and produced the expected data |
| 1 | connection, protocol, I/O error, or no data where data was expected |
| 2 | invalid command line |

Commands that expect a reply or records fail with code 1 when they receive
nothing. Pass `--allow-empty` when an empty result is meaningful for your
script. This is useful for raw `send` probes and for checking whether a counter
is supported.

### Command side effects

- `version` starts the GPU scope, prints the version reply, then stops it.
- `scope` intentionally leaves the GPU scope running so a capture can follow.
  Stop it with `scope-off` or by restarting the app.
- `capture --frame` arms the next presented frame and does not send capture stop.
- `capture` and `monitor` send their stop command even when the read fails.
- `fps` disables the FPS stream on its error path.

## Protocol

All integers are little endian on the tested device. The `--endian be` flag
exists for experimentation only.

### Transport

- TCP: the layer listens on `0.0.0.0:20829`; `adb forward tcp:20829 tcp:20829`
  maps it to the host.
- Unix: abstract socket names are set by the host integration. QStreamKit
  accepts `@name` for an abstract socket and a path otherwise.
- One client at a time is the normal mode. A second connection may be accepted
  but can receive an empty stream until the layer's command thread wakes up.

### Request framing

Every command starts with a 12-byte header followed by an optional payload:

```
u32 token
u32 payload_len
u32 arg
u8  payload[payload_len]
```

The layer reads exactly `payload_len` bytes and passes that buffer to the
handler. `arg` is only used by a few legacy tokens.

Scalar handlers read their value at `payload + 4`, so a scalar request is 20
bytes:

```
u32 token
u32 8
u32 0
u32 0
u32 value
```

Putting a scalar in the header instead of the payload makes the layer wait for
payload bytes that never arrive and silently drop the command. This is the most
common source of "the layer ignores me" reports.

### Reply framing

Replies are a sequence of records:

```
u32 magic         0xABCD0001
u32 index         per-process sequence number
u32 body_len
u8  body[body_len]:
    u32 kind
    u32 payload_len
    u32 arg0
    u8  payload[payload_len]
```

`body_len == 12 + payload_len`. `arg0` carries the process id on most records,
the device pointer low word on monitor reports, and zero on FPS samples.

### Record kinds

| kind | meaning | payload |
| --- | --- | --- |
| `0x0B010001` | process | size, pid at 4, name cstring at 8 |
| `0x0B010002` | device | size, 0, u64 clock at 8, 0, name cstring at 20 |
| `0x0B010004` | counters | size, pid, two device counters |
| `0x01020004` | frame start marker | size 16, marker at 4, u64 device at 8 |
| `0x01020005` | frame end marker | size 16, marker at 4, u64 device at 8 |
| `0x0B020007` | GPU scope monitor report | 0x38 header plus driver monitor blob |
| `0x0102001C` | monitors done marker | empty, sent after scope stop collects monitors |
| `0x0B020004` | capture started ack | empty |
| `0x0B020005` | capture stopped ack | empty |
| `0x0B020009` | FPS sample | 24 bytes, f32 fps at 8 |
| `0x0B02000E` | capture batch | sequence of state tokens |
| `0x0B020012` | version | size 56, version at 4, 12 values |
| `0x0B050013` | queue submit timing | 96 bytes |
| `0x0B03006B` | serialized `VkSubmitInfo` | 96 bytes |

### Commands

| token | payload | meaning | reply |
| --- | --- | --- | --- |
| `0x01020002` | none | register client | process, counters, device on first registration; may be silent later |
| `0x01020018` | 24 bytes: 0, 0, 0, action, group, counter | add or remove one GSL counter pair | silent |
| `0x01020019` | 16 bytes: size, mode, flag1, flag2 | start GPU scope; mode 0 timing, mode 2 monitors | version record, then timing and monitor stream |
| `0x0102001A` | none | stop GPU scope | silent |
| `0x0B020001` | 16 bytes: pad, pad, u64 device | capture the next presented frame | frame markers and one timing, no ack |
| `0x0B020002` | none | start capture immediately | started ack |
| `0x0B020003` | none | stop capture | stopped ack, then the batch |
| `0x0B020009` | 8 bytes: pad, enable | enable or disable FPS | FPS record stream while enabled |
| `0x0B02000A` | 8 bytes: pad, period | frame rate period | silent |
| `0x0B02000B` | 8 bytes: pad, flags | frame delimiter flags | silent |

### FPS stream

Send `0x0B020009` with `{0, 1}`. The layer emits a 24-byte record roughly every
250 ms:

```
u32 24
u32 0
f32 fps
u32 0
u64 gpu timestamp (0xB4000079...)
```

Send `{0, 0}` to stop. FPS is only produced while the app is the top resumed
activity. A foreground service alone is not enough: in PiP or offscreen the
process stays alive and capture still works, but FPS reports zero samples.

The trailing 64-bit value is a GPU clock, not a frame time. Read as an f32 its
low word looks like 15.917 on a 60 Hz swapchain.

### Immediate capture

`0x0B020002` starts capture. The layer keeps collecting queue submits until
`0x0B020003`; the stop reply is a `0x0B02000E` record whose payload is a flat
sequence of state tokens:

```
repeat:
    u32 kind
    u32 payload_len
    u32 pid
    u8  payload[payload_len]
```

Each token is `12 + payload_len` bytes and the batch is a whole number of
tokens. A capture stream can also contain FPS records between the acks.

The client must keep draining the socket. Closing it in the middle of a capture
leaves the layer in capture mode and later commands may stall; recover with
`adb shell am force-stop <package>`.

`0x0B050013` is the queue submit timing record. Its payload is an 88-byte
struct followed by an 8-byte GPU timestamp fragment:

| offset | field |
| --- | --- |
| 0 | size, 88 |
| 4 | frame index |
| 8 | unknown counter |
| 12 | reserved |
| 16 | start_us, u64 epoch microseconds |
| 24 | end_us, u64 epoch microseconds |
| 32 | gpu_ts, u64 |
| 40 | constant 1 |
| 44 | reserved |
| 48 | frame_clock, u64 |
| 56 | cpu_clock, u64 |
| 64 | reserved |
| 72 | gpu_ts2, u64 |
| 80 | constant 1 |
| 84 | reserved |
| 88 | gpu_ts3, u64 fragment |

`0x0B03006B` is a serialized `VkSubmitInfo` state token. The fixed part is 64
bytes and the rest is pointer and stage-mask fragments:

| offset | field |
| --- | --- |
| 0 | size, 64 |
| 4 | sType, 4 for `VK_STRUCTURE_TYPE_SUBMIT_INFO` |
| 8 | wait semaphore count |
| 16 | pWaitSemaphores |
| 24 | pWaitDstStageMask |
| 32 | command buffer count |
| 40 | pCommandBuffers |
| 48 | signal semaphore count |
| 56 | pSignalSemaphores |
| 64 | fragments |

`capture --csv` writes one row per timing token:

```
frame_index,pid,start_us,end_us,dur_us,gpu_ts,frame_clock,cpu_clock,gpu_ts2,gpu_ts3
```

The `dur_us` column is `end_us - start_us`, the CPU span of the submit.

### Frame capture

`0x0B020001` asks for the next presented frame. The payload must contain the
hosting process's `VkDevice` pointer at `+8`; without a valid pointer the layer
reads stale payload data and never arms the device. The stand logs the pointer:

```
adb logcat -s qprofprobe | grep vkDevice
```

Frame capture also needs GPU scope active, because the layer raises the frame
signal while collecting scope markers. Start scope first, then send the request.

The requested frame is delivered inline, not as a `0x0B02000E` batch. The stream
contains:

1. `0x01020004` frame start marker
2. one `0x0B050013` submit timing
3. `0x01020005` frame end marker

Both markers carry the same counter at `+4` and the device pointer at `+8`.
Other submits may appear between the markers when scope is streaming; the
parser keeps only the timing record inside the marker pair. `capture --frame
--csv` exports that one record.

### GPU scope

`0x01020019` is the scope start command, not a version-only query. Its payload
is `{16, mode, flag1, flag2}`. The layer applies the mode, enables the scope
API, replies with a `0x0B020012` version record and starts a stream of
`0x0B050013` submit timings. Mode `0` collects timings only. Mode `2` also
enables hardware counter monitors and produces `0x0B020007` records.
`qstream version` performs the same call and stops the scope after the version
reply. Use `qstream scope --seconds N` to collect a bounded timing stream; it
leaves the scope running on purpose.

`0x0102001A` stops the scope API and is silent. After a monitor-mode stop the
layer may also send one empty `0x0102001C` marker, meaning "monitors collected";
the CLI treats it as optional.

Counter groups are configured with `0x01020018`, one pair per token:

```
u32 0
u32 0
u32 0
u32 action    0 = remove, nonzero = add
u32 group     GSL block id
u32 counter   counter index inside the block
```

Pairs may be added before scope starts; the layer appends them and copies the
list into the scope API on every start. The ids are A8x driver values, not
QSTREAM objects. QStreamKit ships a curated table for Adreno 8xx and accepts
numeric overrides. The stock pairs are `(0,0)` and `(27,0)`; `27` is
`ALWAYSON`. Pair state is process-global, so restart the hosting app to clear
unknown pairs completely.

A monitor report `0x0B020007` starts with a 0x38 byte header:

```
+0x00 u32 0x38
+0x04 u32 driver blob size
+0x08 u32 gpu id
+0x0c u32 start mode
+0x10 u32 driver field
+0x14 u32 device pointer low
+0x18 u64 object handle, command buffer or zero
+0x20 u32 submit index
+0x24 u32 flags
+0x28 u32 frame index
+0x2c u32 reserved
+0x30 u64 device pointer
+0x38 driver monitor blob
```

The driver blob lists the selected pairs at `+0x30 + 16*i` as
`{group, counter, 0, 0}`. After the pair list, the counter block has
`clock` at `+0x08`, the cumulative value for the first pair at `+0x10`, and
three u64 timestamps inside the counter block at `+0x18`, `+0x20`, `+0x28`.
The first report of a run
may also contain per-counter samples after a 0x30 byte secondary header.
`qstream monitor` decodes these fields and computes deltas between reports.
The compact report carries one cumulative value, for the first selected pair;
the raw blob is kept for additional pairs and is appended to `--save` output.

The scope API is available only when the driver exposes the profiling jump
table. The layer logs `Unable to access the metrics backend` when it is not.
On the tested Adreno 840 it is available.

### Offscreen behavior

When the activity is destroyed but the foreground service keeps the process
alive, the stand switches to offscreen rendering and keeps submitting. Capture
continues to produce batches: a 2 s offscreen run on the tested device produced
128 submit records in one batch. FPS stays at zero because the process is no
longer top visible.

## Testing

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The core crate has unit tests for hex parsing, record framing, FPS decoding,
capture token framing, submit timing fields, monitor report decoding and
request layouts. Fixtures in `crates/qstream-core/tests/fixtures` are real
captures: `register.hex`, `version.hex`, `fps.hex` and `monitor.hex`.

Manual smoke test against the stand:

```
qstream probe
qstream fps --seconds 2
qstream capture --seconds 2 --csv submits.csv
qstream scope --seconds 1
qstream monitor --seconds 2 --group 9 --counter 1 --csv monitors.csv
qstream capture --frame --device <vkDevice> --seconds 2 --csv frame.csv
qstream scope-off
```

## Troubleshooting

**The first command returns no data.** The layer may accept the socket before
its command thread wakes up. Retry, or raise the timeouts:

```
qstream --idle-ms 2000 --wait-ms 3000 probe
```

With the `0.1` release, an empty reply exits with code 1. Use `--allow-empty`
only when an empty result is expected.

**A capture command hangs or later commands stall.** The client closed the
socket while the layer was still in capture mode. Keep draining until the stop
ack and batch arrive, or reset the app:

```
adb shell am force-stop com.example.qprofprobe
adb shell am start -n com.example.qprofprobe/.MainActivity
```

**The layer is not loaded.** The log should contain:

```
QProfiler.VulkanShim: Profiler layer initialized.
```

If it does not, check that the APK embeds the layer under `lib/arm64-v8a`, that
the JSON manifest is next to it, and that the app is debuggable.

**Install fails with a signature mismatch.** The debug keystore changed:

```
adb uninstall com.example.qprofprobe
adb install -r tools/probe-app/app/build/outputs/apk/debug/app-debug.apk
```

**GPU scope is silent.** Check for `Unable to access the metrics backend` in
the layer log. If present, the driver does not expose the scope API.

**Monitor reports are empty.** Start the scope with mode `2`, not mode `0`.
Add at least one counter pair before starting, and confirm it with
`qstream counters list --group N`. A pair that the driver rejects produces
timings without `0x0B020007` reports. Try `monitor --reset` or restart the
hosting app if the pair list contains unknown leftovers from earlier sessions.

**Perfetto cannot read files under `/data/local/tmp`.** SELinux blocks it on
this build. Use `/data/misc/perfetto-configs/` for configs and
`/data/misc/perfetto-traces/` for output.

## Limitations

- **Perfetto direct load.** The layer registers its `gpu.renderstages` data
  source only in the profiler direct-load mode. `profilerSetOptions` copies a
  16-byte options block, but a normally loaded layer never satisfies the
  direct-load check and logs "Perfetto functionality is only supported in
  direct load mode". Direct load is a separate entry point
  (`ReplaceVkCreateInstanceDirect` / `profilerVkCreateInstance`) used by the
  Snapdragon Profiler host. Forcing it from the stand crashes in
  `vkCreateInstance`. `qstream perfetto` reports the empty trace instead of
  pretending success.
- **Counter ids are driver-specific.** QSTREAM has no "list counters" call.
  The built-in table covers common A8x counters from the layer metric database.
  Other Adreno families may use different block/counter numbers; use numeric
  `--group` / `--counter` values from the matching driver.
- **Multiple monitor pairs.** The compact report exposes one cumulative value,
  for the first pair in the selected list. Additional pairs appear in the raw
  blob and in the first report's samples, but their compact cumulative values
  are not fully decoded yet. Use one pair per `qstream monitor` run for a
  reliable series, or read the raw blob from `--save`.
- **Monitor mode.** Counter reports require start mode `2`; mode `0` produces
  submit timings only. Command-buffer monitors also require the layer to track
  the command buffer, so a host that reuses buffers allocated before scope
  start may only produce queue-submit monitors.
- **Frame rate and delimiter semantics.** The value encoding is correct, but
  which flags produce which frame boundaries is not verified.
- **Device pointer.** Frame capture requires a manually obtained `VkDevice`
  pointer. The stand prints it, but there is no protocol token that returns it.
- **Unix sockets.** Abstract unix sockets are Linux only.
- **Driver coverage.** Payload offsets were verified against one retail Adreno
  840 driver. Treat them as version-specific until tested elsewhere.

## Legal

No Qualcomm binaries are distributed. The tool only talks to a layer that the
user obtains from their own device. Reverse engineering was performed for
interoperability with an application the user is entitled to debug. Qualcomm,
Adreno and Snapdragon are trademarks of their respective owners. See `NOTICE`
for the full trademark and provenance statement.

## Release process

Releases use conventional commits and `release-plz`. On a push to `main`,
release-plz opens or updates a release pull request with the version bump and
`CHANGELOG.md` entry. Merging that pull request publishes the crates in
dependency order and creates the `vX.Y.Z` tag and GitHub release.

Do not create version tags manually. `release-plz` creates and pushes the tag,
and the tag is what triggers the binary archive workflow.

Required repository secret: `CARGO_REGISTRY_TOKEN`. The binary workflow runs
when a GitHub release is published and uploads Linux, Windows and macOS
archives with SHA-256 checksums.

The first release publishes `qstream-core` and `qstream-transport` before
`qstream-cli`. Until those crates exist on crates.io, `cargo package -p
qstream-cli` cannot resolve them; `cargo package -p qstream-core -p
qstream-transport` is the CI dry run.

## License

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT License (`LICENSE-MIT`)

at your option. Unless you explicitly state otherwise, any contribution you
intentionally submit for inclusion in this project is dual licensed as above,
without additional terms.

Release notes are in `CHANGELOG.md`. Security reports follow `SECURITY.md`.
