#!/usr/bin/env bash
set -euo pipefail

# check-package-age.sh: Validate that new dependencies introduced in a PR
# were published at least 7 days ago on crates.io.
#
# This script:
# 1. Diffs Cargo.lock against origin/main to find new [[package]] entries
# 2. Extracts crate name and version pairs
# 3. Queries the crates.io API for each new crate
# 4. Compares the published date to today minus 7 days
# 5. Fails if any crate is too new (unless SKIP_PACKAGE_AGE_CHECK=true)

SKIP_PACKAGE_AGE_CHECK="${SKIP_PACKAGE_AGE_CHECK:-false}"

if [ "$SKIP_PACKAGE_AGE_CHECK" = "true" ]; then
  echo "Package age check skipped (SKIP_PACKAGE_AGE_CHECK=true)"
  exit 0
fi

# Ensure we have Cargo.lock and origin/main is available
if [ ! -f "Cargo.lock" ]; then
  echo "Error: Cargo.lock not found"
  exit 1
fi

# Fetch origin/main to ensure we have the latest base branch
git fetch origin main:refs/remotes/origin/main 2>/dev/null || true

# Get the diff of Cargo.lock against origin/main
# We look for lines that start with '+[[package]]' to find new package entries
DIFF_OUTPUT=$(git diff origin/main -- Cargo.lock || echo "")

if [ -z "$DIFF_OUTPUT" ]; then
  echo "No changes to Cargo.lock detected"
  exit 0
fi

# Extract new package entries: look for added lines with [[package]]
# Then extract the name and version from the following lines
NEW_PACKAGES=$(echo "$DIFF_OUTPUT" | awk '
  /^\+\[\[package\]\]/ {
    in_new_package = 1
  }
  in_new_package && /^\+name = / {
    gsub(/^\+name = "/, "")
    gsub(/"$/, "")
    name = $0
  }
  in_new_package && /^\+version = / {
    gsub(/^\+version = "/, "")
    gsub(/"$/, "")
    version = $0
    print name ":" version
    in_new_package = 0
  }
')

if [ -z "$NEW_PACKAGES" ]; then
  echo "No new packages detected in Cargo.lock"
  exit 0
fi

echo "Checking age of new packages..."

# Use gdate if available (GNU date), otherwise use date
DATE_CMD="date"
if command -v gdate &> /dev/null; then
  DATE_CMD="gdate"
fi

# Calculate the threshold date (7 days ago)
THRESHOLD_SECONDS=$((7 * 24 * 60 * 60))
CURRENT_TIMESTAMP=$($DATE_CMD +%s)
THRESHOLD_TIMESTAMP=$((CURRENT_TIMESTAMP - THRESHOLD_SECONDS))

FAILED=0

while IFS=':' read -r NAME VERSION; do
  if [ -z "$NAME" ] || [ -z "$VERSION" ]; then
    continue
  fi

  echo -n "Checking $NAME@$VERSION... "

  # Query crates.io API for this specific version
  RESPONSE=$(curl -s "https://crates.io/api/v1/crates/$NAME/$VERSION" 2>/dev/null || echo "{}")

  # Extract the created_at timestamp
  CREATED_AT=$(echo "$RESPONSE" | jq -r '.version.created_at // empty' 2>/dev/null || echo "")

  if [ -z "$CREATED_AT" ]; then
    echo "WARNING: Could not fetch metadata from crates.io"
    continue
  fi

  # Convert ISO 8601 timestamp to Unix timestamp
  CREATED_TIMESTAMP=$($DATE_CMD -d "$CREATED_AT" +%s 2>/dev/null || echo "0")

  if [ "$CREATED_TIMESTAMP" -eq 0 ]; then
    echo "WARNING: Could not parse created_at timestamp"
    continue
  fi

  # Check if the package is too new
  if [ "$CREATED_TIMESTAMP" -gt "$THRESHOLD_TIMESTAMP" ]; then
    DAYS_OLD=$(( (CURRENT_TIMESTAMP - CREATED_TIMESTAMP) / (24 * 60 * 60) ))
    echo "FAIL (published $DAYS_OLD days ago, threshold is 7 days)"
    FAILED=1
  else
    DAYS_OLD=$(( (CURRENT_TIMESTAMP - CREATED_TIMESTAMP) / (24 * 60 * 60) ))
    echo "OK (published $DAYS_OLD days ago)"
  fi
done <<< "$NEW_PACKAGES"

if [ "$FAILED" -eq 1 ]; then
  echo ""
  echo "Error: One or more new dependencies are too recent."
  echo "To override this check for urgent security patches, set SKIP_PACKAGE_AGE_CHECK=true"
  exit 1
fi

exit 0
