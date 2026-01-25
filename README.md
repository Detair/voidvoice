# VoidMic 🌌 (Hybrid Edition)

[![CI](https://github.com/Detair/voidvoice/actions/workflows/ci.yml/badge.svg)](https://github.com/Detair/voidvoice/actions/workflows/ci.yml)
[![Release](https://github.com/Detair/voidvoice/actions/workflows/release.yml/badge.svg)](https://github.com/Detair/voidvoice/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![AI Assisted](https://img.shields.io/badge/AI-Assisted-blue)](https://github.com/google-gemini)

**VoidMic** is a high-fidelity noise reduction tool designed for high-noise environments like **Sim Racing** and **Mechanical Keyboards**.

It uses a **Hybrid Engine**:
1.  **RNN Denoising (RNNoise):** Removes steady background noise (fans, hum, traffic).
2.  **Smart Noise Gate:** Actively silences transient clicks (keyboards, shifters) when you aren't speaking.

## 🗺️ Roadmap
- [x] Cross-platform support (Linux, Windows, macOS)
- [x] Hybrid AI + Gate Engine
- [ ] Auto-updater
- [ ] Tray icon support
- [ ] Dynamic threshold calibration

## 🚀 Features

- **Hybrid Engine**: Best of both worlds for gamers.
- **Visual Meter**: See exactly when your mic is active vs gated.
- **Cross-Platform**: Linux, Windows, macOS.
- **Zero Config**: No model downloads required (Embeds weights).

## 📥 Build & Install

### 🐧 Arch Linux / Standard Linux
```bash
# Install dependencies (ALSA)
sudo pacman -S alsa-lib

# Build
cargo build --release

# Run
./target/release/voidmic
```

### ⚛️ Fedora Atomic (Silverblue / Kinoite)
You can build VoidMic as a Flatpak to keep your system clean.
```bash
# 1. Install Flatpak Builder
flatpak install org.freedesktop.Sdk.Extension.rust-stable//23.08

# 2. Build and Install (Allowing network for cargo crates)
flatpak-builder --user --install --force-clean --share=network build-dir build-aux/com.voidmic.VoidMic.yml

# 3. Run
flatpak run com.voidmic.VoidMic
```

### 🪟 Windows
1.  **Prerequisites**:
    *   Install **Rust** (rustup-init.exe).
    *   Install **Visual Studio Build Tools** (C++ Workload) for the linker.
2.  **Build**:
    Open PowerShell in the project folder:
    ```powershell
cargo build --release
    ```
3.  **Run**:
    ```powershell
    .\target\release\voidmic.exe
    ```
    *Note: On Windows, use the "Install Virtual Cable" button in the app to download VB-Cable drivers if you haven't already.*

## 🎮 Usage Guide

1.  **Select Devices**:
    *   **Input**: Your Mic.
    *   **Output**: The Virtual Sink / VB-Cable.
2.  **Check the Meter**:
    *   **Green Bar**: You are speaking (Gate Open).
    *   **Gray Bar**: You are silent (Gate Closed - Clicks are muted).

## 🧠 AI Transparency
Architected with **Google Gemini** and **Antigravity AI**.

## 📄 License
MIT License.