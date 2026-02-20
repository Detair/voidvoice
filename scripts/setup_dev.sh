#!/bin/bash
set -e

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}🌌 VoidMic Development Setup${NC}"

# Detect OS
if [ -f /etc/os-release ]; then
    . /etc/os-release
    OS=$NAME
    ID=$ID
else
    echo -e "${RED}Unknown OS${NC}"
    exit 1
fi

echo -e "Detected OS: ${GREEN}$OS ($ID)${NC}"

# Define Dependencies
DEBIAN_DEPS="build-essential libasound2-dev libgtk-3-dev libappindicator3-dev libxdo-dev libgl1-mesa-dev libx11-dev"
FEDORA_DEPS="alsa-lib-devel gtk3-devel libappindicator-gtk3-devel libX11-devel libXtst-devel mesa-libGL-devel gcc"
ARCH_DEPS="alsa-lib pipewire-pulse gtk3 libappindicator-gtk3 libxdo mesa pkgconf xorg-server-devel"

install_debian() {
    echo -e "${BLUE}Installing dependencies for Debian/Ubuntu...${NC}"
    sudo apt update
    sudo apt install -y $DEBIAN_DEPS
}

install_fedora() {
    echo -e "${BLUE}Installing dependencies for Fedora Workstation...${NC}"
    sudo dnf install -y $FEDORA_DEPS
}

install_fedora_atomic() {
    echo -e "${BLUE}Detected Fedora Atomic (Silverblue/Kinoite)...${NC}"
    
    # Check if we are already inside a toolbox
    if [ -f /run/.containerenv ] && grep -q "toolbox" /run/.containerenv; then
        echo -e "${GREEN}Running inside Toolbox. using DNF...${NC}"
        sudo dnf install -y $FEDORA_DEPS
    else
        echo -e "${BLUE}Host system detected. Attempting to install inside default Toolbox...${NC}"
        if command -v toolbox >/dev/null 2>&1; then
            echo "Running: toolbox run sudo dnf install -y dependencies..."
            toolbox run sudo dnf install -y $FEDORA_DEPS
            echo -e "${GREEN}Dependencies installed in Toolbox!${NC}"
            echo "To build, enter the toolbox with: toolbox enter"
        else
            echo -e "${RED}Toolbox not found! Please install toolbox or use rpm-ostree manually.${NC}"
            exit 1
        fi
    fi
}

install_arch() {
    echo -e "${BLUE}Installing dependencies for Arch Linux...${NC}"
    sudo pacman -S --needed $ARCH_DEPS
}

# Main Logic
case "$ID" in
    ubuntu|debian|pop|mint)
        install_debian
        ;;
    fedora)
        # Check if Atomic
        if rpm-ostree status >/dev/null 2>&1; then
             install_fedora_atomic
        else
             install_fedora
        fi
        ;;
    arch|manjaro|endeavouros)
        install_arch
        ;;
    *)
        echo -e "${RED}Unsupported distribution: $ID${NC}"
        echo "Please install the following manually:"
        echo "Debian-based: $DEBIAN_DEPS"
        echo "Fedora-based: $FEDORA_DEPS"
        echo "Arch-based:   $ARCH_DEPS"
        exit 1
        ;;
esac

echo -e "${GREEN}✅ Setup Complete!${NC}"
echo "You can now build VoidMic with: cargo build --release --workspace"
