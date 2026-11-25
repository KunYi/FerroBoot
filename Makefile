
# Configuration
TARGET       := thumbv7em-none-eabihf
CHIP         := STM32H750VBTx
PROBE_RS     := probe-rs
CARGO_FLAGS  := --release --locked
PORT         := /dev/ttyACM0
SLOT         ?= 0

# Names the build in the boot log. `--dirty` marks uncommitted changes to tracked files.
BUILD_TAG    := $(shell git describe --always --dirty --tags 2>/dev/null || echo unknown)
export FERROBOOT_BUILD := $(BUILD_TAG)

# Artifact paths
RELEASE_DIR  := target/$(TARGET)/release
BOOT_ELF     := $(RELEASE_DIR)/bootloader-h750
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

.PHONY: all build boot flash-boot app sign upload clean FORCE test-imgs test-img-noconfirm test-img-hang test-img-corrupt test-img-big test-img-ver probe
# Default target: build both firmware components and sign the application
all: build sign

# Build both the bootloader and application
build: boot app

# Build the bootloader
boot:
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p bootloader-h750

# Flash the bootloader to internal flash and reset the MCU
flash-boot: boot
	$(PROBE_RS) download --chip $(CHIP) $(BOOT_ELF)
	$(PROBE_RS) reset --chip $(CHIP)

# The tag is compiled into the ELF, so a new tag has to rebuild it even if no source changed.
# The stamp file changes only when the tag does.
TAG_STAMP := $(RELEASE_DIR)/.build_tag
FORCE:
$(TAG_STAMP): FORCE
	@mkdir -p $(dir $@)
	@echo '$(BUILD_TAG)' | cmp -s - $@ || echo '$(BUILD_TAG)' > $@

# Build the application ELF only when its inputs (or the build tag) have changed
$(APP_ELF): $(APP_BUILD_INPUTS) $(TAG_STAMP)
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
	python3 $(RESCUE_PY) --port $(PORT) --image $(APP_SIGNED) --slot $(SLOT)

# Remove Cargo build artifacts and generated application images
clean:
	cargo clean
	rm -rf $(APP_BIN) $(APP_SIGNED) $(TESTIMG) target/variant-*

# ---------------------------------------------------------------------------
# Board-verification images (see docs/board-verification.md). Written to $(TESTIMG).
# Each variant is built into its own target dir so it never overwrites the release app.
# ---------------------------------------------------------------------------
TESTIMG      := target/testimg
XTASK        := cargo run $(CARGO_FLAGS) -q -p xtask --
BIG_SIZE     ?= 327680
VER_BUILDS   ?= 2 3

# Build a feature variant of the app and sign it: $(call variant,<feature>,<name>)
define variant
	@mkdir -p $(TESTIMG)
	cargo build $(CARGO_FLAGS) --target $(TARGET) -p app-template --features $(1) --target-dir target/variant-$(2)
	arm-none-eabi-objcopy -O binary target/variant-$(2)/$(TARGET)/release/app-template $(TESTIMG)/$(2).raw
	$(XTASK) sign --bin $(TESTIMG)/$(2).raw --out $(TESTIMG)/$(2).bin --build 2
endef

# On trial the image never confirms: each boot uses up one trial, then it is rolled back.
test-img-noconfirm:
	$(call variant,trial-no-confirm,noconfirm)

# Like noconfirm, and the watchdog is not kicked, so the board resets itself.
test-img-hang:
	$(call variant,trial-hang,hang)

# A valid image with one body bit flipped: the bootloader must answer ERR_VERIFY.
test-img-corrupt: $(APP_SIGNED)
	@mkdir -p $(TESTIMG)
	$(XTASK) corrupt --input $(APP_SIGNED) --out $(TESTIMG)/corrupt.bin

# Two valid images padded to BIG_SIZE bytes (> 64KB, <= the 512KiB RAM window) with different
# fill bytes. Upload big-a, then big-b to the same slot: if the erase stopped short of the whole
# slot, the old 0xA5 ANDed with the new 0x5A gives 0x00 and 'm' answers ERR_VERIFY.
test-img-big: $(APP_SIGNED)
	@mkdir -p $(TESTIMG)
	$(XTASK) pad --bin $(APP_BIN) --out $(TESTIMG)/big-a.raw --size $(BIG_SIZE) --fill a5
	$(XTASK) sign --bin $(TESTIMG)/big-a.raw --out $(TESTIMG)/big-a.bin --build 2
	$(XTASK) pad --bin $(APP_BIN) --out $(TESTIMG)/big-b.raw --size $(BIG_SIZE) --fill 5a
	$(XTASK) sign --bin $(TESTIMG)/big-b.raw --out $(TESTIMG)/big-b.bin --build 3

# The release app signed with different build numbers (VER_BUILDS) to check version selection.
test-img-ver: $(APP_SIGNED)
	@mkdir -p $(TESTIMG)
	@for b in $(VER_BUILDS); do \
		$(XTASK) sign --bin $(APP_BIN) --out $(TESTIMG)/ver-$$b.bin --build $$b || exit 1; \
	done

test-imgs: test-img-noconfirm test-img-hang test-img-corrupt test-img-big test-img-ver
	@ls -l $(TESTIMG)/*.bin

# Send requests the bootloader must refuse (ERR_SLOT, ERR_RANGE, ERR_LEN). Writes nothing.
probe:
	python3 $(RESCUE_PY) --port $(PORT) --probe
