# KeyVibes - build and install
#
#   make build                      # release binary in target/release/
#   make install PREFIX=$HOME/.local    # per-user, no root
#   sudo make install PREFIX=/usr/local # system-wide
#   sudo make uninstall PREFIX=/usr/local
#   make help
#
# DESTDIR is honoured throughout for packaging:
#
#   make install DESTDIR=/tmp/pkg PREFIX=/usr
#
# Nothing here needs root except writing to a root-owned PREFIX. The engine
# itself never does - see docs/install.md.

PREFIX  ?= /usr/local
DESTDIR ?=

BINDIR  = $(DESTDIR)$(PREFIX)/bin
DATADIR = $(DESTDIR)$(PREFIX)/share
DOCDIR  = $(DATADIR)/doc/keyvibes
PACKDIR = $(DATADIR)/keyvibes/packs

# Pack names may contain spaces ("Holy Panda.kvpack"), so they are expanded
# by the shell and quoted per file rather than passed through make, which
# would split them.
PACK_GLOB = assets/soundpacks/*.kvpack

# systemd looks in $(PREFIX)/lib/systemd/user for a system prefix and under
# the XDG data dir for a per-user one. Pick on PREFIX so both work unaided;
# override SYSTEMD_USER_DIR if your distribution differs.
# DESTDIR is part of the path: `make install DESTDIR=/stage PREFIX=/usr`
# must stage the unit there, not write into the live /usr.
SYSTEMD_USER_DIR ?= $(if $(filter $(HOME)/%,$(PREFIX)),$(DESTDIR)$(PREFIX)/share/systemd/user,$(DESTDIR)$(PREFIX)/lib/systemd/user)

BIN    = target/release/keyvibes
UIBIN  = target/release/keyvibes-ui
DOCS   = README.md docs/install.md docs/environment.md THIRD_PARTY_NOTICES.md LICENSE-MIT LICENSE-APACHE
DESKTOP = dist/keyvibes-ui.desktop
ICON    = dist/keyvibes.svg
UDEV_RULE = dist/keyvibes-udev.rules

# udev looks in /etc/udev/rules.d (admin) and /usr/lib/udev/rules.d (packages).
# A per-user PREFIX (~/.local) cannot contribute a rule that the system udev
# will load, so we install one only for system prefixes and print the manual
# copy step otherwise. DESTDIR is part of the path for staged packaging.
UDEV_RULES_DIR ?= $(DESTDIR)$(PREFIX)/lib/udev/rules.d

.PHONY: all build install uninstall help clean

all: build

help:
	@echo "KeyVibes make targets:"
	@echo "  build       cargo build --release"
	@echo "  install     install the engine, the desktop app, packs, docs\
	@echo "              PREFIX=$(PREFIX)  DESTDIR=$(DESTDIR)"
	@echo "  uninstall   remove exactly what install put there"
	@echo "  clean       remove build output"
	@echo
	@echo "A per-user install needs no root:"
	@echo "  make install PREFIX=\$$HOME/.local"
	@echo "  systemctl --user daemon-reload && systemctl --user enable --now keyvibes"

build:
	cargo build --release

install: build
	install -d "$(BINDIR)"
	install -m755 "$(BIN)" "$(BINDIR)/keyvibes"
	@echo "  binary   $(BINDIR)/keyvibes"
	@if [ -x "$(UIBIN)" ]; then \
		install -m755 "$(UIBIN)" "$(BINDIR)/keyvibes-ui"; \
		echo "  binary   $(BINDIR)/keyvibes-ui"; \
	else \
		echo "  binary   $(BINDIR)/keyvibes-ui skipped - not built"; \
	fi
	install -d "$(DATADIR)/applications"
	install -m644 "$(DESKTOP)" "$(DATADIR)/applications/keyvibes-ui.desktop"
	@echo "  desktop  $(DATADIR)/applications/keyvibes-ui.desktop"
	install -d "$(DATADIR)/icons/hicolor/scalable/apps"
	install -m644 "$(ICON)" "$(DATADIR)/icons/hicolor/scalable/apps/keyvibes.svg"
	@echo "  icon     $(DATADIR)/icons/hicolor/scalable/apps/keyvibes.svg"
	@if ls $(PACK_GLOB) >/dev/null 2>&1; then \
		install -d "$(PACKDIR)"; \
		count=0; \
		for pack in $(PACK_GLOB); do \
			install -m644 "$$pack" "$(PACKDIR)/"; \
			count=$$((count + 1)); \
		done; \
		echo "  packs    $(PACKDIR) ($$count pack(s))"; \
	else \
		echo "  packs    skipped - build them first with 'keyvibes pack build'"; \
		echo "           (see docs/install.md#sound-packs)"; \
	fi
	install -d "$(DOCDIR)"
	install -m644 $(DOCS) "$(DOCDIR)/"
	@echo "  docs     $(DOCDIR)"
	@# A system prefix gets a udev rule that udev will actually load. A
	@# per-user prefix cannot contribute one, so print the copy step instead
	@# rather than dropping a rule file where udev never looks.
	@if [ "$(PREFIX)" = "/usr" ] || [ "$(PREFIX)" = "/usr/local" ]; then \
		install -d "$(UDEV_RULES_DIR)"; \
		install -m644 "$(UDEV_RULE)" "$(UDEV_RULES_DIR)/70-keyvibes-uaccess.rules"; \
		echo "  udev     $(UDEV_RULES_DIR)/70-keyvibes-uaccess.rules"; \
		echo "           (grants seat users read access to keyboards only;"; \
		echo "            activate with: sudo udevadm control --reload-rules && sudo udevadm trigger)"; \
	fi
	install -d "$(SYSTEMD_USER_DIR)"
	sed -e 's|@BINDIR@|$(PREFIX)/bin|g' -e 's|@PREFIX@|$(PREFIX)|g' \
		dist/keyvibes.service > "$(SYSTEMD_USER_DIR)/keyvibes.service"
	chmod 644 "$(SYSTEMD_USER_DIR)/keyvibes.service"
	@echo "  service  $(SYSTEMD_USER_DIR)/keyvibes.service"
	@echo
	@echo "Installed. Next:"
	@echo "  keyvibes doctor                       # is this machine ready?"
	@echo "  keyvibes pack list                    # which packs are installed?"
	@echo "  systemctl --user daemon-reload"
	@echo "  systemctl --user enable --now keyvibes"
	@echo
	@if id -nG 2>/dev/null | tr ' ' '\n' | grep -qx input; then \
		echo "You are already in the 'input' group - log out and back in if"; \
		echo "doctor still cannot read /dev/input."; \
	else \
		echo "If doctor cannot read /dev/input, either of these works:"; \
		echo "  1. input group (classic):"; \
		echo "       sudo usermod -aG input \$$USER   (then log out and back in)"; \
		echo "  2. udev rule (session-scoped, no group change):"; \
		echo "       see docs/linux-permissions.md"; \
	fi

uninstall:
	rm -f "$(BINDIR)/keyvibes"
	rm -f "$(BINDIR)/keyvibes-ui"
	rm -f "$(DATADIR)/applications/keyvibes-ui.desktop"
	rm -f "$(DATADIR)/icons/hicolor/scalable/apps/keyvibes.svg"
	rm -f "$(SYSTEMD_USER_DIR)/keyvibes.service"
	rm -f "$(UDEV_RULES_DIR)/70-keyvibes-uaccess.rules"
	rm -rf "$(DATADIR)/keyvibes"
	rm -rf "$(DOCDIR)"
	# Directories we created, but only while they are still empty - and never
	# $(PREFIX) itself or $(DATADIR): those hold other people's files, and an
	# empty-but-present ~/.local/bin is not ours to delete either. Literal
	# paths, not dir/..: once dir is gone the .. lookup fails too.
	-rmdir "$(SYSTEMD_USER_DIR)" "$(DESTDIR)$(PREFIX)/lib/systemd" \
		"$(UDEV_RULES_DIR)" "$(DESTDIR)$(PREFIX)/lib/udev" \
		"$(DATADIR)/doc" 2>/dev/null || true
	@echo "Removed KeyVibes from PREFIX=$(PREFIX)"
	@echo "Left alone: your configuration (typically ~/.config/keyvibes)."
	@echo "Remove it with: rm -rf \$$XDG_CONFIG_HOME/keyvibes"

clean:
	cargo clean
