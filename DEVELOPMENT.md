# 🛠️ VoidMic Developer Guide

Welcome to the VoidMic code forge! This guide will help you set up your environment to build and contribute to VoidMic.

## ⚡ Quick Start

### 1. Install Dependencies
We provide a setup script for **Debian/Ubuntu**, **Fedora**, and **Fedora Atomic**.

```bash
./scripts/setup_dev.sh
```

**Fedora Atomic Note:** This script detects if you are on Silverblue/Kinoite and uses **Toolbox** to install dependencies, keeping your host system clean.

### 2. Build Workspace
VoidMic is a Cargo Workspace containing the App, Core, and Plugins.

```bash
# Build everything (App + VST3 + CLAP + LV2)
cargo build --release --workspace
```

## 📦 Dependencies

If you prefer to install manually:

| OS | Command |
|----|---------|
| **Debian/Ubuntu** | `sudo apt install build-essential libasound2-dev libgtk-3-dev libappindicator3-dev libxdo-dev libgl1-mesa-dev libx11-dev` |
| **Fedora** | `sudo dnf install alsa-lib-devel gtk3-devel libappindicator-gtk3-devel libX11-devel libXtst-devel mesa-libGL-devel gcc` |
| **Arch** | `sudo pacman -S alsa-lib pulseaudio gtk3 libappindicator-gtk3 libxdo mesa` |

## 🏗️ Architecture

The project is split into crates:

- **`crates/core`**: Pure DSP logic (`VoidProcessor`). The "Brain". No Audio I/O.
- **`crates/app`**: The Standalone App (`voidmic_app`). Uses `cpal` for I/O and `egui` for GUI.
- **`crates/plugin`**: VST3/CLAP Plugin. Uses `nih-plug`.
- **`crates/lv2`**: LV2 Plugin. Uses `rust-lv2`.
- **`crates/ui`**: Shared UI components and Theme.

## 🧪 Testing

```bash
# Run all unit tests
cargo test --workspace

# Check for warnings
cargo check --workspace
```

## 🚀 Running

### App
```bash
cargo run --bin voidmic_app
```

### Plugin
To test the plugin, we recommend using a host like **Carla** or **Reaper**.
The built plugins are in `target/release/bundled/`.

## 🤝 Contributing
1.  Fork the repo.
2.  Create a feature branch.
3.  Submit a Pull Request.

Happy Coding! 🌌
