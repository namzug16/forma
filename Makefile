.PHONY: install desktop desktop-app

# Installs the forma binary to ~/.cargo/bin, replacing any previous build.
install:
	cargo install --path . --locked --force

# Runs the desktop viewer from source.
desktop:
	cargo run -p forma-desktop

# Assembles an unsigned target/Forma.app for this machine.
desktop-app:
	cargo build --release -p forma-desktop
	mkdir -p target/Forma.app/Contents/MacOS
	cp desktop/Info.plist target/Forma.app/Contents/
	cp target/release/forma-desktop target/Forma.app/Contents/MacOS/
