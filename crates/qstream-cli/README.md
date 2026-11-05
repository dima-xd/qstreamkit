# qstream-cli

Command-line client for the Adreno qprofiler QSTREAM socket.

The `qstream` binary talks to a debuggable Vulkan process that has loaded the
Qualcomm `VK_LAYER_ADRENO_qprofiler` layer. It supports process and device
probes, FPS, frame capture, GPU scope timings, GPU counter selection, monitor
reports and raw protocol inspection.

The layer binary is not distributed here. Pull it from your own device and
host it in a debuggable app as described in the repository README.

## Install

```sh
cargo install qstream-cli
```

From a repository checkout use:

```sh
cargo install --path crates/qstream-cli
```

Run `qstream --help` for the command list.

## License

MIT OR Apache-2.0.
