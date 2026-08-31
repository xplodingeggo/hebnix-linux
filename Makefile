# Hebnix Linux Makefile

.PHONY: all build release debug clean install uninstall run test check fmt clippy help

# Default target
all: release

# Build release version
build: release

release:
	@echo "Building release version..."
	cargo build --release

# Build debug version
debug:
	@echo "Building debug version..."
	cargo build

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	cargo clean

# Install to /usr/local/bin (requires sudo)
install: release
	@echo "Installing hebnix-app to /usr/local/bin..."
	@install -Dm755 target/release/hebnix-app /usr/local/bin/hebnix-app
	@echo "Installation complete. Run with: hebnix-app"

# Uninstall from /usr/local/bin
uninstall:
	@echo "Uninstalling hebnix-app..."
	@rm -f /usr/local/bin/hebnix-app
	@echo "Uninstall complete."

# Run the release binary
run: release
	@echo "Running hebnix-app..."
	./target/release/hebnix-app

# Run tests
test:
	@echo "Running tests..."
	cargo test

# Check code without building
check:
	@echo "Checking code..."
	cargo check

# Format code
fmt:
	@echo "Formatting code..."
	cargo fmt

# Run clippy linter
clippy:
	@echo "Running clippy..."
	cargo clippy -- -D warnings

# Development build and run
dev: debug
	@echo "Running debug version..."
	./target/debug/hebnix-app

# Show help
help:
	@echo "Hebnix Linux - Available targets:"
	@echo ""
	@echo "  make              - Build release version (default)"
	@echo "  make release      - Build optimized release version"
	@echo "  make debug        - Build debug version"
	@echo "  make clean        - Remove build artifacts"
	@echo "  make install      - Install to /usr/local/bin (requires sudo)"
	@echo "  make uninstall    - Remove from /usr/local/bin"
	@echo "  make run          - Build and run release version"
	@echo "  make dev          - Build and run debug version"
	@echo "  make test         - Run tests"
	@echo "  make check        - Check code without building"
	@echo "  make fmt          - Format code with rustfmt"
	@echo "  make clippy       - Run clippy linter"
	@echo "  make help         - Show this help message"
