
# Configuration
TARGET       := thumbv7em-none-eabihf
CARGO_FLAGS  := --release --locked
PORT         := /dev/ttyACM0

# Artifact paths
RELEASE_DIR  := target/$(TARGET)/release
APP_ELF      := $(RELEASE_DIR)/app-template
APP_BIN      := $(RELEASE_DIR)/app.bin
APP_SIGNED   := $(RELEASE_DIR)/app_signed.bin
RESCUE_PY    := tools/rescue_upload.py

# Application build inputs
# Include application sources, workspace crates, manifests, linker scripts,
# and Cargo configuration files.
APP_BUILD_INPUTS := $(shell find app-template crates -type f \
	\( -name '*.rs' -o -name '*.c' -o -name '*.h' \
	   -o -name '*.S' -o -name '*.s' -o -name '*.bin' \
	   -o -name 'Cargo.toml' -o -name 'build.rs' \
	   -o -name '*.x' \) 2>/dev/null)

APP_BUILD_INPUTS += Cargo.toml Cargo.lock
APP_BUILD_INPUTS += $(wildcard .cargo/config .cargo/config.toml)
APP_BUILD_INPUTS += $(wildcard rust-toolchain rust-toolchain.toml)

# Signing tool build inputs
XTASK_BUILD_INPUTS := $(shell find tools/xtask -type f \
	\( -name '*.rs' -o -name 'Cargo.toml' -o -name 'build.rs' \) \
	2>/dev/null)

XTASK_BUILD_INPUTS += Cargo.toml Cargo.lock

.PHONY: all build boot app sign upload clean

# Default target: build both firmware components and sign the application
all: build sign

# Build both the bootloader and application
build: boot app

# Build the bootloader
boot:
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p bootloader-h750

# Build the application ELF only when its inputs have changed
$(APP_ELF): $(APP_BUILD_INPUTS)
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p app-template
	@touch $@

# Application build target
app: $(APP_ELF)

# Generate the signed image when the application or signing tool changes
$(APP_SIGNED): $(APP_ELF) $(XTASK_BUILD_INPUTS)
	arm-none-eabi-objcopy -O binary $(APP_ELF) $(APP_BIN)
	cargo run $(CARGO_FLAGS) -p xtask -- sign --bin $(APP_BIN) --out $(APP_SIGNED)

# Build and sign the application image
sign: $(APP_SIGNED)

# Upload the signed image using the Python rescue utility
upload: $(APP_SIGNED)
	python3 $(RESCUE_PY) --port $(PORT) --image $(APP_SIGNED) --slot 0

# Remove Cargo build artifacts and generated application images
clean:
	cargo clean
	rm -f $(APP_BIN) $(APP_SIGNED)
