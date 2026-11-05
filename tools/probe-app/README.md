# probe-app

Development stand for QStreamKit. It is not part of the shipped client.

The Adreno qprofiler layer is a Vulkan layer, not a system service. It only
listens while a process has it loaded. This app is the smallest debuggable
process that does that:

- embeds the layer and its JSON manifest under `lib/arm64-v8a`
- creates a Vulkan instance with the layer enabled
- renders a triangle through a swapchain
- falls back to a 1280x720 offscreen image when the surface is destroyed
- keeps running through a foreground service
- logs the `VkDevice` pointer needed for frame capture

The layer binary is not in this repository. Pull it from your own device:

```
adb shell find / -name 'libVkLayer_ADRENO_qprofiler.so' 2>/dev/null
adb pull <path> ./libVkLayer_ADRENO_qprofiler.so
```

## Requirements

- Android Studio, or JDK 17/21 and the Gradle wrapper in this directory
- Android SDK platform 36
- Android SDK build-tools 35.0.0
- Android NDK 27.0.11902837
- CMake 3.22.1

Install the SDK packages through Android Studio's SDK Manager. Gradle reads the
SDK location from `ANDROID_HOME`, `ANDROID_SDK_ROOT`, or `local.properties`. If
your default `java` is newer than 21, point `JAVA_HOME` at JDK 17 or 21.

## Build

Copy the layer into the app first:

```
mkdir -p app/src/main/jniLibs/arm64-v8a
cp libVkLayer_ADRENO_qprofiler.so app/src/main/jniLibs/arm64-v8a/
```

On Windows create the directory and copy the file with Explorer or PowerShell if
you are not using Git Bash.

Then build:

```
./gradlew assembleDebug
```

On Windows use `gradlew.bat assembleDebug`. The APK is written to:

```
app/build/outputs/apk/debug/app-debug.apk
```

Alternatively open this directory in Android Studio and run the `app` module.

The layer JSON lives in `app/src/main/resources/lib/arm64-v8a/`. It is shipped
as a Java resource because the Android Gradle plugin packages only `.so` files
from `jniLibs`; the resource path puts the manifest next to the library in the
APK. The manifest is a minimal project-written file, not a copy of a Qualcomm
file. Compare it with the manifest on your device before testing a different
layer build.

Gradle signs debug builds with the standard `~/.android/debug.keystore`. A
different signing key requires uninstalling the old package first:

```
adb uninstall com.example.qprofprobe
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

## Run

```
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.example.qprofprobe/.MainActivity
adb forward tcp:20829 tcp:20829
adb logcat -s qprofprobe | grep vkDevice
```

Then use the `qstream` CLI from the repository root. The output of the last
command is the pointer to pass to `qstream capture-frame --device`. For counter
monitors run `qstream monitor --seconds 3 --group 9 --counter 1`. Add `--reset`
for a clean curated pair list; without it, only the stock `(0,0)` and `(27,0)`
pairs are replaced.

The layer logs to `QProfiler.VulkanShim`:

```
adb logcat -s QProfiler.VulkanShim
```

## Shaders

`app/src/main/cpp/shaders.h` contains the precompiled SPIR-V blobs, so a normal
build does not need Python or `glslc`. After editing the shaders, regenerate it
from the NDK:

```
glslc --target-env=vulkan1.1 app/src/main/cpp/shaders/vertex.vert -o vertex.spv
glslc --target-env=vulkan1.1 app/src/main/cpp/shaders/fragment.frag -o fragment.spv
python app/src/main/cpp/gen_spv.py app/src/main/cpp/shaders.h vertex.spv fragment.spv
```

`glslc` is in `<NDK>/shader-tools/<host>/`.
