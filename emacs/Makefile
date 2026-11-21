# Makefile for the Triage Emacs client.
#
# Plain make keeps development to Emacs and the Elpaca builds listed in DEPS.
#
#   make test      run the ERT suite
#   make compile   byte-compile, treating every warning as an error
#   make checkdoc  run checkdoc
#   make check     compile, test and check documentation
#   make clean     remove .elc files

EMACS ?= emacs
ELPACA ?= $(HOME)/.emacs.d/elpaca/builds
DEPS = compat cond-let llama magit-section plz transient markdown-mode
BATCH = $(EMACS) -Q --batch -L . $(foreach d,$(DEPS),-L $(ELPACA)/$(d))

# Every --eval form below is written on one line, deliberately.
#
# A backslash continuation inside the single-quoted argument does not work: the
# shell cannot consume a backslash-newline inside single quotes, so the
# backslash reaches Emacs, whose reader takes `\' followed by a newline as an
# escaped character and yields a symbol whose name is a newline.  Evaluating
# that symbol signals `void-variable \' and Emacs exits 255.
#
# It appeared to work here only by accident of make's version.  GNU make 3.81,
# which macOS still ships, collapses the continuation before the shell sees it;
# GNU make 4.x, which every Linux distribution ships, passes it through
# unchanged.  Measured: identical Makefile, `make compile' exits 0 under 3.81
# and 255 under 4.4.1, and CI failed on all six Emacs versions for this reason
# while passing locally.  One line each keeps both versions honest.

# triage.el must compile last because it requires every module above it.
SRC = triage-api.el triage-card.el triage-queue.el triage-render.el triage-launch.el triage-peek.el triage-history.el triage-settings.el triage-inbox.el triage-context.el triage-clock.el triage.el
TEST = test/triage-card-test.el test/triage-render-test.el test/triage-inbox-test.el test/triage-launch-test.el test/triage-context-test.el test/triage-daemon-test.el test/triage-clock-test.el
ELC = $(SRC:.el=.elc) $(TEST:.el=.elc)

.PHONY: all check test compile checkdoc clean

all: check

check: compile test checkdoc

test:
	$(BATCH) -L test $(foreach t,$(TEST),-l $(t)) -f ert-run-tests-batch-and-exit

compile:
	$(BATCH) --eval '(progn (require (quote bytecomp)) (setq byte-compile-error-on-warn t) (dolist (f command-line-args-left) (unless (byte-compile-file f) (kill-emacs 1))))' $(SRC) $(TEST)

checkdoc:
	$(BATCH) --eval '(progn (require (quote checkdoc)) (dolist (f command-line-args-left) (checkdoc-file f)) (let ((buf (get-buffer "*Warnings*"))) (when (and buf (> (buffer-size buf) 0)) (kill-emacs 1))))' $(SRC) $(TEST)

clean:
	rm -f *.elc test/*.elc
