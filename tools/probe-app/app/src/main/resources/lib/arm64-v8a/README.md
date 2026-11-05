# Layer manifest provenance

`VK_LAYER_ADRENO_qprofiler.json` is a minimal Vulkan layer manifest written for
this project. It describes the layer name, API version and the
`libVkLayer_ADRENO_qprofiler.so` library that the user supplies from their own
device.

It is not a copy of a Qualcomm file. If you test a different layer build, compare
this manifest with the one on your device and adjust the API or implementation
version if needed. The vendor library remains subject to Qualcomm's terms and is
not distributed here.
