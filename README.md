# VoidMic 🌌 (Hybrid Edition)

[![CI](https://github.com/Detair/voidvoice/actions/workflows/ci.yml/badge.svg)](https://github.com/Detair/voidvoice/actions/workflows/ci.yml)
[![Release](https://github.com/Detair/voidvoice/actions/workflows/release.yml/badge.svg)](https://github.com/Detair/voidvoice/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![AI Assisted](https://img.shields.io/badge/AI-Assisted-blue)](https://github.com/google-gemini)

**VoidMic** is a high-fidelity noise reduction tool designed for high-noise environments like **Sim Racing** and **Mechanical Keyboards**.

It uses a **Hybrid Engine**:
1.  **RNN Denoising (RNNoise):** Removes steady background noise (fans, hum, traffic).
2.  **Smart Noise Gate:** Actively silences transient clicks (keyboards, shifters) when you aren't speaking.
3.  **Echo Cancellation (AEC):** Removes speaker feedback for headphone-free gaming.

## 🚀 Features

- **Hybrid Engine**: RNNoise + Smart Gate + AEC.
- **Echo Cancellation**: Play without headphones using WebRTC AEC3.
- **Output Filtering**: Denoise incoming audio (like Discord calls) before it hits your speakers.
- **Daemon Mode**: Run as a background process (`voidmic load`) without the GUI.
- **Auto Virtual Sink**: Automatically creates virtual devices on Linux.
- **Visual Meter**: Real-time feedback on gate status.
- **Cross-Platform**: Linux, Windows, macOS.

## 🗺️ Roadmap
- [x] Cross-platform support
- [x] Hybrid AI + Gate Engine
- [x] Auto-updater
- [x] Tray icon support
- [x] Echo Cancellation
- [x] Headless / Daemon mode
- [x] Flatpak support

## 📥 Build & Install

### 🐧 Arch Linux / Standard Linux
```bash
# Install dependencies (ALSA, PulseAudio/PipeWire)
sudo pacman -S alsa-lib pulseaudio

# Build
cargo build --release

# Run
./target/release/voidmic
```

### 📦 Flatpak
```bash
# 1. Install Flatpak Builder
flatpak install org.freedesktop.Sdk.Extension.rust-stable//23.08

# 2. Build and Install
flatpak-builder --user --install --force-clean --share=network build-dir build-aux/com.voidmic.VoidMic.yml

# 3. Run
flatpak run com.voidmic.VoidMic
```

### 🖥️ Headless / Server
For minimal systems:
```bash
cargo build --release --no-default-features
./target/release/voidmic run -i default
```

### 🪟 Windows
1.  Install **Rust**.
2.  Install **BSVC** (C++ Build Tools).
3.  `cargo build --release`
4.  Run `.\target\release\voidmic.exe`

## 🎮 Usage Guide

### GUI
1.  **Select Devices**: Mic as Input, Virtual Sink as Output.
2.  **Advanced Features**:
    *   **Filter Output**: Check this to denoise what you hear.
    *   **Echo Cancellation**: Check this if using speakers. Select your "Speaker Monitor" as the reference.

### Daemon (NoiseTorch-like)
```bash
# Load: Create virtual sink and start background process
voidmic load -i default

# Unload: Stop and cleanup
voidmic unload
```

## 🧠 AI Transparency
Architected with **Google Gemini** and **Antigravity AI**.

## 📄 License
MIT License.