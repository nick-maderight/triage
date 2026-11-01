#!/usr/bin/env bash
set -euo pipefail

triage finding \
  --source cron://nightly-audit \
  --id "nightly-audit/$(date +%F)/003" \
  --title "Nightly audit finding" \
  --situation "The nightly audit found an item requiring a decision." \
  --task "Review the finding." \
  --action "Choose how to proceed." \
  --result "Record the decision in the inbox." \
  --option "Do as recommended" \
  --option "Investigate first" \
  --ref /path/to/finding-003.md \
  --cwd "$HOME/project" \
  --priority 2
