.PHONY: build build-server build-client server client

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
