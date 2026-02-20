#!/bin/bash
set -e

echo "🏓 MQTT Pong Installer"
echo "====================="
echo ""

# Detect OS
OS="$(uname -s)"
case "${OS}" in
    Linux*)     PLATFORM=Linux;;
    Darwin*)    PLATFORM=macOS;;
    CYGWIN*|MINGW*|MSYS*) PLATFORM=Windows;;
    *)          PLATFORM="UNKNOWN:${OS}"
esac

echo "📋 Detected platform: $PLATFORM"
echo ""

# Check if cargo is installed
if command -v cargo &> /dev/null; then
    echo "✅ Rust is already installed ($(cargo --version))"
else
    echo "❌ Rust not found. Installing..."
    echo ""

    if [ "$PLATFORM" = "macOS" ]; then
        # Check if Homebrew is available
        if command -v brew &> /dev/null; then
            echo "🍺 Installing Rust via Homebrew..."
            brew install rust
        else
            echo "📦 Homebrew not found. Installing Rust via rustup..."
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            source "$HOME/.cargo/env"
        fi
    elif [ "$PLATFORM" = "Linux" ]; then
        echo "📦 Installing Rust via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
    elif [ "$PLATFORM" = "Windows" ]; then
        echo "⚠️  Please install Rust manually on Windows:"
        echo "   Visit https://rustup.rs/ or run: choco install rust"
        exit 1
    else
        echo "⚠️  Unsupported platform. Please install Rust manually from https://rustup.rs/"
        exit 1
    fi

    echo ""
    echo "✅ Rust installed successfully!"
fi

echo ""
echo "🔨 Building MQTT Pong..."
echo ""

# Build the game in release mode
cargo build --release

echo ""
echo "✅ Build complete!"
echo ""
echo "🎮 To play, run:"
echo "   cargo run --release"
echo ""
echo "   Or use the binary directly:"
echo "   ./target/release/rust-pong"
echo ""
echo "📖 For more info, see README.md"
echo ""
