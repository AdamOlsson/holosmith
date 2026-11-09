#!/bin/bash
: > log.txt

PROMPT="Please pick up RESUME.md"

for i in 1 2 3 4 5 6 7 8 9 10 11; do
  echo "=== run $i: $(date) ===" | tee -a log.txt
  timeout 4h pi @RESUME.md -p "$PROMPT" < /dev/null 2>&1 | tee -a log.txt
  echo "exit code: ${PIPESTATUS[0]}" | tee -a log.txt
done
