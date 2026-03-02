.PHONY: build build-server build-client build-client-c-lib-ios server client install

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

ifeq ($(RELEASE),1)
CARGO_PROFILE := --release
else
CARGO_PROFILE :=
endif

build: build-server build-client

build-server:
	cargo build $(CARGO_PROFILE) --bin neoshd

build-client:
	cargo build $(CARGO_PROFILE) --bin neosh

# Aliases so `make build server` / `make build client` also work.
server: build-server

client: build-client

install:
	cargo build --release --bin neosh --bin neoshd
	install -d $(BINDIR)
	install -m 0755 target/release/neosh $(BINDIR)/neosh
	install -m 0755 target/release/neoshd $(BINDIR)/neoshd

build-client-c-lib-ios:
	rustup target add --toolchain stable-aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim
	RUSTC="$$(rustup which --toolchain stable-aarch64-apple-darwin rustc)" \
		rustup run stable-aarch64-apple-darwin cargo build --release --manifest-path ffi/neosh-client-c/Cargo.toml --target-dir target/ffi-client-c --target aarch64-apple-ios
	RUSTC="$$(rustup which --toolchain stable-aarch64-apple-darwin rustc)" \
		rustup run stable-aarch64-apple-darwin cargo build --release --manifest-path ffi/neosh-client-c/Cargo.toml --target-dir target/ffi-client-c --target aarch64-apple-ios-sim
	mkdir -p dist
	rm -rf dist/neosh_client.xcframework
	xcodebuild -create-xcframework \
		-library target/ffi-client-c/aarch64-apple-ios/release/libneosh_client_c.a -headers ffi/neosh-client-c/include \
		-library target/ffi-client-c/aarch64-apple-ios-sim/release/libneosh_client_c.a -headers ffi/neosh-client-c/include \
		-output dist/neosh_client.xcframework
