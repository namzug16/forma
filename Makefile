.PHONY: install desktop desktop-app release-artifacts

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)

# Installs the forma binary to ~/.cargo/bin, replacing any previous build.
install:
	cargo install --path . --locked --force

# Runs the desktop viewer from source.
desktop:
	cargo run -p forma-desktop

# Assembles an ad-hoc signed target/Forma.app for this machine.
desktop-app:
	cargo build --release -p forma-desktop
	rm -rf target/Forma.app
	mkdir -p target/Forma.app/Contents/MacOS target/Forma.app/Contents/Resources
	cp desktop/assets/Forma.icns target/Forma.app/Contents/Resources/
	cp desktop/Info.plist target/Forma.app/Contents/
	plutil -replace CFBundleShortVersionString -string $(VERSION) target/Forma.app/Contents/Info.plist
	cp target/release/forma-desktop target/Forma.app/Contents/MacOS/
	codesign --force --sign - target/Forma.app

# Builds the files attached to a GitHub release into target/dist.
release-artifacts: desktop-app
	cargo build --release -p forma
	rm -rf target/dist target/dmg
	mkdir -p target/dist target/dmg
	tar -czf target/dist/forma-macos-arm64.tar.gz -C target/release forma
	cp -R target/Forma.app target/dmg/
	ln -s /Applications target/dmg/Applications
	hdiutil create -volname Forma -srcfolder target/dmg -format UDZO target/dist/Forma-macos-arm64.dmg
