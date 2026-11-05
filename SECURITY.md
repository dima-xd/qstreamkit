# Security Policy

## Supported versions

Only the latest release is supported with security fixes.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | yes       |

## Reporting a vulnerability

Do not open a public issue for a vulnerability. Use GitHub private
vulnerability reporting for this repository. If that is unavailable, contact
the maintainers through the repository profile before publishing details.

Include the affected version, the command used, the device and driver version,
and a minimal reproduction. Do not include vendor layer binaries or proprietary
Qualcomm files.

## Scope

QStreamKit is a client for a local diagnostics socket exposed by the Qualcomm
`VK_LAYER_ADRENO_qprofiler` layer in debuggable applications. The project does
not bypass Android security boundaries, does not require root, and does not
redistribute Qualcomm binaries.

The vendor layer itself is out of scope. Report vulnerabilities in Qualcomm
software to Qualcomm.
