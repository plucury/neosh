.PHONY: build build-server build-client build-client-c-lib-ios server client

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

build-client-c-lib-ios:
	rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
	cargo build --release --manifest-path ffi/neosh-client-c/Cargo.toml --target aarch64-apple-ios
	cargo build --release --manifest-path ffi/neosh-client-c/Cargo.toml --target aarch64-apple-ios-sim
	cargo build --release --manifest-path ffi/neosh-client-c/Cargo.toml --target x86_64-apple-ios
	mkdir -p dist
	xcodebuild -create-xcframework \
		-library target/aarch64-apple-ios/release/libneosh_client_c.a -headers ffi/neosh-client-c/include \
		-library target/aarch64-apple-ios-sim/release/libneosh_client_c.a -headers ffi/neosh-client-c/include \
		-library target/x86_64-apple-ios/release/libneosh_client_c.a -headers ffi/neosh-client-c/include \
		-output dist/neosh_client.xcframework
