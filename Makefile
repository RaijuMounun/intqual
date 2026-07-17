.PHONY: all ready

all: ready

ready:
	cargo update
	cargo check
	cargo clippy
	cargo build
	updpkgsums
