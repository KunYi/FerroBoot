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

.PHONY: all build boot app sign upload clean

# Default target: build, and sign the application
all: build sign

# Aggregate build target (invoke with `make build`)
build: boot app

# Build the bootloader
boot:
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p bootloader-h750

# Build the application
app:
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p app-template

# Convert the application ELF to binary and sign the image
$(APP_SIGNED): app
	arm-none-eabi-objcopy -O binary $(APP_ELF) $(APP_BIN)
	cargo run $(CARGO_FLAGS) -p xtask -- sign --bin $(APP_BIN) --out $(APP_SIGNED)

sign: $(APP_SIGNED)

# Upload the signed image using the Python rescue utility
upload: $(APP_SIGNED)
	python3 $(RESCUE_PY) --port $(PORT) --image $(APP_SIGNED) --slot 0

# Remove build artifacts
clean:
	cargo clean
	rm -f $(APP_BIN) $(APP_SIGNED)
