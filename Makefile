# macOS build. Windows uses scripts/build-windows.ps1.
#
#   make                       build openfortivpn and the app, install the root
#                              helper, then offer to move the app into
#                              /Applications or ~/Applications
#   make app                   rebuild just the app, then offer to move it
#   make DEST=/Applications    skip the question (or DEST=~/Applications,
#                              DEST=none to leave it in dist/)
#   make uninstall             remove everything setup and the app installed

SHELL := /bin/bash
DEST ?=

.PHONY: all install app uninstall not-running

all: install

install: not-running
	./setup.sh
	scripts/install-app.sh "$(DEST)"

app: not-running
	./setup.sh --app
	scripts/install-app.sh "$(DEST)"

uninstall:
	./uninstall.sh

# setup.sh deletes and rebuilds the app bundle, and the move replaces the
# installed one, so neither should be running.
not-running:
	@if pgrep -qx FortiBar; then \
		echo "FortiBar is running. Disconnect, quit it, then run make again." >&2; \
		exit 1; \
	fi
