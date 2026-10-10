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
SYSTEMD_USER_DIR ?= $(if $(filter $(HOME)/%,$(PREFIX)),$(PREFIX)/share/systemd/user,$(PREFIX)/lib/systemd/user)

BIN   = target/release/keyvibes
DOCS  = README.md docs/install.md docs/environment.md

.PHONY: all build install uninstall help clean

all: build

help:
	@echo "KeyVibes make targets:"
	@echo "  build       cargo build --release"
	@echo "  install     install the binary, packs, docs and user service"
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
	@echo "If doctor cannot read /dev/input, add yourself to the 'input' group:"
	@echo "  sudo usermod -aG input \$$USER   (then log out and back in)"

uninstall:
	rm -f "$(BINDIR)/keyvibes"
	rm -f "$(SYSTEMD_USER_DIR)/keyvibes.service"
	rm -rf "$(DATADIR)/keyvibes"
	rm -rf "$(DOCDIR)"
	# Directories we created, but only while they are still empty - and never
	# $(PREFIX) itself or $(DATADIR): those hold other people's files, and an
	# empty-but-present ~/.local/bin is not ours to delete either. Literal
	# paths, not dir/..: once dir is gone the .. lookup fails too.
	-rmdir "$(SYSTEMD_USER_DIR)" "$(DESTDIR)$(PREFIX)/lib/systemd" \
		"$(DATADIR)/doc" 2>/dev/null || true
	@echo "Removed KeyVibes from PREFIX=$(PREFIX)"
	@echo "Left alone: your configuration (typically ~/.config/keyvibes)."
	@echo "Remove it with: rm -rf \$$XDG_CONFIG_HOME/keyvibes"

clean:
	cargo clean
