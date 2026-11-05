# qstream-core

Protocol framing and parsers for the Adreno qprofiler QSTREAM socket.

This crate contains the record, capture, token and GPU monitor decoders used by
the `qstream` client. It has no transport code and does not talk to a device.

The full project documentation, release notes and limitations are in the
repository README.

The protocol is undocumented and driver-version-specific. The parsers were
reconstructed from a retail Adreno 840 layer and verified on a OnePlus 15 running
Android 16.

## License

MIT OR Apache-2.0.
