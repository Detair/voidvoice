# Changelog

## [0.9.6] - 2026-01-29
### 🐛 Fixes
- **AppImage**: Fixed invalid icon resolution (ensured 32x32 icon is correct size).
- **Assets**: Fixed corrupt asset file formats.

## [0.9.5] - 2026-01-29
### 📦 Packaging
- **Fixed DEB/RPM builds**: Corrected crate paths and metadata.
- **Improved LV2 Support**: Added manual manifests for LV2 plugin.

## [0.9.4] - 2026-01-29
### 🔧 Maintenance
- **Formatting**: Fixed trailing whitespace in GUI code that caused CI failures.

## [0.9.3] - 2026-01-29
### ⚡ Performance
- **Zero Allocations**: Refactored `NoiseFloorTracker` to use fixed-size ring buffer.
- **Pre-allocated Buffers**: Spectrum analyzer and calibration now use pre-allocated buffers.

## [0.9.2] - 2026-01-29
### 🐛 Fixes
- **Code Quality**: Removed/silenced unused fields for a cleaner build log.

## [0.9.1] - 2026-01-29
### 🐛 Fixes
- **CI/CD**: Added missing `libx11-xcb-dev` dependency to build workflows to fix Linux build failures.

## [0.9.0] - 2026-01-29

### 🚀 Major Features
- **Stereo Support**: `VoidProcessor` is now fully stereo-aware, with linked VAD/Gate and independent Denoise/EQ per channel.
- **Plugin Ecosystem**: Added **VST3**, **CLAP**, and **LV2** plugin targets.
- **One-Click Setup**: New "Create Virtual Mic" button in the GUI automates audio routing on Linux (PulseAudio/PipeWire).
- **First-Run Wizard**: Integrated setup flow for new users.

### 🎨 Visual & UI
- **Deep Void Theme**: New premium dark theme with rounded corners and consistent styling.
- **Visual Assets**: Updated App Icon and added Hero Banner.
- **Spectrum Visualizer**: Real-time frequency analysis in both App and Plugin.

### 🛠️ Technical
- **Workspace Architecture**: Refactored into `core`, `app`, `ui`, `plugin`, `lv2`.
- **Packaging**: CI pipelines now generate `.deb`, `.rpm`, `AppImage`, and plugin bundles.
- **Developer Experience**: Added `scripts/setup_dev.sh` with support for Debian, Fedora, and Fedora Atomic (Toolbox).

### 🐛 Fixes
- Fixed potential mono-summing issues in previous alpha builds.
- Improved hot-plug resilience for audio devices.
