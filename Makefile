.PHONY: all ready distribute

all: ready

ready:
	cargo update
	cargo check
	cargo clippy
	cargo build
	updpkgsums

distribute:
	bash scripts/distribute.sh
