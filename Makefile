.PHONY: install

# Installs the forma binary to ~/.cargo/bin, replacing any previous build.
install:
	cargo install --path . --locked --force
