#!/usr/bin/env bash

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}   Hebnix Linux Installation Script${NC}"
echo -e "${BLUE}========================================${NC}\n"

# Track if any dependencies are missing
MISSING_DEPS=0
MISSING_OPTIONAL_DEPS=0

# Function to print status
print_status() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✓${NC} $2"
    else
        echo -e "${RED}✗${NC} $2"
    fi
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_info() {
    echo -e "${BLUE}ℹ${NC} $1"
}

# Check for required commands
check_command() {
    if command -v "$1" &> /dev/null; then
        print_status 0 "$1 found"
        return 0
    else
        print_status 1 "$1 not found"
        return 1
    fi
}

# Detect distribution
detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        echo "$ID"
    else
        echo "unknown"
    fi
}

DISTRO=$(detect_distro)
print_info "Detected distribution: $DISTRO"
echo ""

# Check for Rust
echo -e "${BLUE}[1/5] Checking Rust installation...${NC}"
if check_command rustc && check_command cargo; then
    RUST_VERSION=$(rustc --version | awk '{print $2}')
    print_info "Rust version: $RUST_VERSION"
else
    MISSING_DEPS=1
    print_warning "Rust is required. Install via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
fi
echo ""

# Check for C compiler
echo -e "${BLUE}[2/5] Checking C compiler...${NC}"
if check_command gcc || check_command clang; then
    if command -v gcc &> /dev/null; then
        GCC_VERSION=$(gcc --version | head -n1)
        print_info "$GCC_VERSION"
    elif command -v clang &> /dev/null; then
        CLANG_VERSION=$(clang --version | head -n1)
        print_info "$CLANG_VERSION"
    fi
else
    MISSING_DEPS=1
fi
echo ""

# Check for pkg-config
echo -e "${BLUE}[3/5] Checking build tools...${NC}"
if ! check_command pkg-config; then
    MISSING_DEPS=1
fi

if ! check_command make; then
    MISSING_DEPS=1
fi
echo ""

# Check for system libraries
echo -e "${BLUE}[4/5] Checking system libraries...${NC}"

check_library() {
    if pkg-config --exists "$1" 2>/dev/null; then
        print_status 0 "$1 found"
        return 0
    else
        print_status 1 "$1 not found"
        return 1
    fi
}

# Check GTK3
if ! check_library gtk+-3.0; then
    MISSING_DEPS=1
fi

# Check Wayland
if ! check_library wayland-client; then
    MISSING_DEPS=1
fi

# Check xkbcommon
if ! check_library xkbcommon; then
    MISSING_DEPS=1
fi

# Check for app indicator (varies by distro)
if ! check_library ayatana-appindicator3-0.1 && ! check_library appindicator3-0.1; then
    MISSING_DEPS=1
fi
echo ""

# Check for optional dependencies
echo -e "${BLUE}[5/5] Checking optional dependencies...${NC}"
if check_command curl-impersonate; then
    print_info "curl-impersonate found (for tracker.gg lookups)"
else
    MISSING_OPTIONAL_DEPS=1
    print_warning "curl-impersonate not found (optional, for tracker.gg lookups)"
fi

# Check for KDE tools if on KDE
if [ "$XDG_CURRENT_DESKTOP" = "KDE" ] || [ "$DESKTOP_SESSION" = "plasma" ]; then
    if ! check_command kdotool; then
        MISSING_OPTIONAL_DEPS=1
        print_warning "kdotool not found (recommended for KDE/Plasma)"
    fi
    if ! check_command kscreen-doctor; then
        MISSING_OPTIONAL_DEPS=1
        print_warning "kscreen-doctor not found (part of KDE Plasma)"
    fi
fi
echo ""

# Print installation instructions if dependencies are missing
if [ $MISSING_DEPS -eq 1 ]; then
    echo -e "${RED}========================================${NC}"
    echo -e "${RED}   Missing Required Dependencies${NC}"
    echo -e "${RED}========================================${NC}\n"

    case "$DISTRO" in
        arch|manjaro)
            echo -e "${YELLOW}Run the following command to install dependencies:${NC}"
            echo -e "${GREEN}sudo pacman -S --needed base-devel gtk3 libayatana-appindicator wayland libxkbcommon${NC}"
            echo ""
            echo -e "${YELLOW}For Rust (if not installed):${NC}"
            echo -e "${GREEN}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${NC}"
            ;;
        ubuntu|debian|linuxmint|pop)
            echo -e "${YELLOW}Run the following command to install dependencies:${NC}"
            echo -e "${GREEN}sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev libwayland-dev libxkbcommon-dev${NC}"
            echo ""
            echo -e "${YELLOW}For Rust (if not installed):${NC}"
            echo -e "${GREEN}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${NC}"
            ;;
        fedora|rhel|centos)
            echo -e "${YELLOW}Run the following command to install dependencies:${NC}"
            echo -e "${GREEN}sudo dnf install gcc gtk3-devel libappindicator-gtk3-devel wayland-devel libxkbcommon-devel${NC}"
            echo ""
            echo -e "${YELLOW}For Rust (if not installed):${NC}"
            echo -e "${GREEN}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${NC}"
            ;;
        *)
            echo -e "${YELLOW}Please install the following dependencies for your distribution:${NC}"
            echo "  - Rust (stable) via rustup"
            echo "  - C compiler (gcc or clang)"
            echo "  - GTK3 development libraries"
            echo "  - Wayland client development libraries"
            echo "  - libxkbcommon development libraries"
            echo "  - libayatana-appindicator or libappindicator development libraries"
            ;;
    esac
    echo ""
    exit 1
fi

# Print optional dependency info
if [ $MISSING_OPTIONAL_DEPS -eq 1 ]; then
    echo -e "${YELLOW}========================================${NC}"
    echo -e "${YELLOW}   Optional Dependencies Missing${NC}"
    echo -e "${YELLOW}========================================${NC}\n"
    print_info "curl-impersonate: For tracker.gg player stats/avatar lookups"
    print_info "Download from: https://github.com/lexiforest/curl-impersonate"
    echo ""

    if [ "$XDG_CURRENT_DESKTOP" = "KDE" ] || [ "$DESKTOP_SESSION" = "plasma" ]; then
        print_info "kdotool: For window focus tracking on KDE/Plasma"
        print_info "Install from AUR: yay -S kdotool-bin"
        echo ""
    fi
fi

# All dependencies met, proceed with build
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}   All Required Dependencies Met${NC}"
echo -e "${GREEN}========================================${NC}\n"

echo -e "${BLUE}Building Hebnix...${NC}"
echo ""

cd "$SCRIPT_DIR"

# Build the project
cargo build --release

if [ $? -eq 0 ]; then
    echo ""
    echo -e "${GREEN}========================================${NC}"
    echo -e "${GREEN}   Build Successful!${NC}"
    echo -e "${GREEN}========================================${NC}\n"

    print_info "Binary location: $SCRIPT_DIR/target/release/hebnix-app"
    echo ""

    # Create .desktop file
    echo -e "${BLUE}Creating desktop launcher...${NC}"

    # Copy icon to standard location
    ICON_SOURCE="$SCRIPT_DIR/crates/hebnix-app/assets/hebnix.png"
    ICON_DIR="$HOME/.local/share/icons"
    mkdir -p "$ICON_DIR"
    if [ -f "$ICON_SOURCE" ]; then
        cp "$ICON_SOURCE" "$ICON_DIR/hebnix.png"
        ICON_PATH="$ICON_DIR/hebnix.png"
        print_status 0 "Icon copied to $ICON_PATH"
    else
        ICON_PATH="utilities-terminal"
        print_warning "Icon not found, using default icon"
    fi

    DESKTOP_CONTENT="[Desktop Entry]
Version=1.0
Type=Application
Name=Hebnix
Comment=Rocket League overlay and stats tracker for Linux
Exec=$SCRIPT_DIR/target/release/hebnix-app
Path=$SCRIPT_DIR/target/release
Icon=$ICON_PATH
Terminal=false
Categories=Game;Utility;
Keywords=rocket-league;overlay;stats;"

    # Create in ~/Desktop if it exists
    DESKTOP_DIR="$HOME/Desktop"
    if [ -d "$DESKTOP_DIR" ]; then
        DESKTOP_FILE="$DESKTOP_DIR/hebnix.desktop"
        echo "$DESKTOP_CONTENT" > "$DESKTOP_FILE"
        chmod +x "$DESKTOP_FILE"
        print_status 0 "Desktop launcher created at $DESKTOP_FILE"
    else
        print_warning "~/Desktop directory not found, skipping Desktop icon"
    fi

    # Create in ~/.local/share/applications
    LOCAL_APP_DIR="$HOME/.local/share/applications"
    mkdir -p "$LOCAL_APP_DIR"
    LOCAL_DESKTOP_FILE="$LOCAL_APP_DIR/hebnix.desktop"
    echo "$DESKTOP_CONTENT" > "$LOCAL_DESKTOP_FILE"
    chmod +x "$LOCAL_DESKTOP_FILE"
    print_status 0 "Application menu entry created at $LOCAL_DESKTOP_FILE"

    # Try to create in /usr/share/applications (requires sudo)
    SYSTEM_APP_DIR="/usr/share/applications"
    SYSTEM_DESKTOP_FILE="$SYSTEM_APP_DIR/hebnix.desktop"
    if [ -w "$SYSTEM_APP_DIR" ]; then
        echo "$DESKTOP_CONTENT" > "$SYSTEM_DESKTOP_FILE"
        chmod +x "$SYSTEM_DESKTOP_FILE"
        print_status 0 "System-wide application entry created at $SYSTEM_DESKTOP_FILE"
    else
        echo "$DESKTOP_CONTENT" | sudo tee "$SYSTEM_DESKTOP_FILE" > /dev/null 2>&1
        if [ $? -eq 0 ]; then
            sudo chmod +x "$SYSTEM_DESKTOP_FILE"
            print_status 0 "System-wide application entry created at $SYSTEM_DESKTOP_FILE"
        else
            print_warning "Could not create system-wide entry (no sudo access or declined)"
        fi
    fi

    # Update desktop database
    if command -v update-desktop-database &> /dev/null; then
        update-desktop-database "$LOCAL_APP_DIR" 2>/dev/null
    fi

    echo ""

    print_info "To run Hebnix:"
    echo -e "  ${GREEN}cd $SCRIPT_DIR${NC}"
    echo -e "  ${GREEN}./target/release/hebnix-app${NC}"
    if [ -f "$DESKTOP_FILE" ]; then
        echo -e "  ${GREEN}Or double-click the Hebnix icon on your desktop${NC}"
    fi
    echo ""
    print_info "On first run, an empty plugins/ folder will be created."
    print_info "Install plugins via the app's Plugins tab or clone them manually."
    echo ""

    if [ $MISSING_OPTIONAL_DEPS -eq 1 ]; then
        print_warning "Remember to install optional dependencies for full functionality."
    fi
else
    echo ""
    echo -e "${RED}========================================${NC}"
    echo -e "${RED}   Build Failed${NC}"
    echo -e "${RED}========================================${NC}\n"
    exit 1
fi
