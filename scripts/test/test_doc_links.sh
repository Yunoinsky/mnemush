#!/bin/bash
# TDD: markdown cross-references must point to existing files.
# Catches typos like DECISIONS.md vs decisions.md — on macOS HFS+/APFS
# (case-insensitive) the link resolves; on Linux (case-sensitive) it
# 404s. This test uses Linux-style case-sensitive checks.

set -e
DOCS=$HOME/Project/mnemush
FAIL=0

# Extract (text -> target) pairs from each .md
for md in "$DOCS"/*.md "$DOCS"/docs/*.md; do
    [ -f "$md" ] || continue
    # dir from which relative paths in this file resolve
    md_dir="$(dirname "$md")"
    # Pull out markdown links: [text](path)
    while IFS= read -r target; do
        # strip trailing punctuation that sometimes follows links
        target="${target%.}"
        target="${target%,}"
        # skip external / anchor-only / empty
        [[ "$target" == http* ]] && continue
        [[ "$target" == "" ]] && continue
        [[ "$target" == \#* ]] && continue
        [[ "$target" == /* ]] && continue
        # strip any anchor
        target="${target%%#*}"
        full="${md_dir}/${target}"
        if [[ ! -e "$full" ]]; then
            rel="${md#$DOCS/}"
            echo "BROKEN: ${rel} → ${target}"
            FAIL=1
        fi
    done < <(grep -oE '\[[^]]+\]\([^)]+\)' "$md" \
        | sed -E 's/^[^(]+\(([^)]+)\)$/\1/')
done

if [ $FAIL -eq 0 ]; then
    echo "PASS: all .md cross-references resolve to real files"
fi
exit $FAIL

# Cross-check: link target text should match the actual filename
# (case-sensitive). On macOS's case-insensitive default FS this can
# silently pass even when the doc has a typo like DECISIONS.md vs
# decisions.md. CI on Linux catches it; we catch it here by asserting
# the link text exactly matches an existing file.
EXTRA_FAIL=0
for md in "$DOCS"/*.md "$DOCS"/docs/*.md; do
    [ -f "$md" ] || continue
    md_dir="$(dirname "$md")"
    while IFS= read -r pair; do
        text="$(echo "$pair" | cut -f1)"
        target="$(echo "$pair" | cut -f2)"
        target="${target%.}"
        target="${target%,}"
        [[ "$target" == http* ]] && continue
        [[ "$target" == "" ]] && continue
        [[ "$target" == \#* ]] && continue
        [[ "$target" == /* ]] && continue
        target_no_anchor="${target%%#*}"
        full="${md_dir}/${target_no_anchor}"
        if [[ -e "$full" ]]; then
            # also check that the link TEXT matches the actual filename case
            expected_basename="$(basename "$target_no_anchor")"
            # try matching by either exact case or with the link text as the
            # marker — easier: compare link text against any file in dir
            # with that basename in case-insensitive mode
            actual_match=$(find "$(dirname "$full")" -maxdepth 1 -name "$(basename "$full")" 2>/dev/null | head -1)
            if [[ -n "$actual_match" ]]; then
                actual_basename="$(basename "$actual_match")"
                if [[ "$text" != *"$actual_basename" && "$expected_basename" != "$actual_basename" ]]; then
                    echo "BAD CASE: link text '$text' target '$target_no_anchor' resolves to '$actual_basename' (case-mismatch)"
                    EXTRA_FAIL=1
                fi
            fi
        fi
    done < <(grep -oE '\[[^]]+\]\([^)]+\)' "$md" | sed -E 's/^\[([^(]+)\]\(([^)]+)\)$/\1\t\2/')
done

if [ $EXTRA_FAIL -eq 1 ]; then
    FAIL=1
    echo "FAIL: doc links contain case-mismatched filenames (broken on Linux)"
fi
# Second pass: case-sensitive filename check. On macOS the FS is
# case-insensitive by default (APFS option `case-sensitive` is
# undocumented), so `[ -e DECISIONS.md ]` succeeds even when the
# actual filename is `decisions.md`. Linux CI would catch this but
# we want it caught locally too. Check by case-sensitively globbing
# for the link target.
set +e  # so FAIL doesn't short-circuit the rest
CASE_FAIL=0
for md in "$DOCS"/*.md "$DOCS"/docs/*.md; do
    [ -f "$md" ] || continue
    md_dir="$(dirname "$md")"
    while IFS= read -r target; do
        target="${target%.}"
        target="${target%,}"
        [[ "$target" == http* ]] && continue
        [[ "$target" == "" ]] && continue
        [[ "$target" == \#* ]] && continue
        [[ "$target" == /* ]] && continue
        target_no_anchor="${target%%#*}"
        full="$(cd "$md_dir" && readlink -f "$target_no_anchor" 2>/dev/null)" || full=""
        # Compare link's path basename with actual disk basename (case-sensitive)
        link_basename="$(basename "$target_no_anchor")"
        # Resolve the actual filename as-it-exists on disk (case-sensitive path)
        actual_path=$(cd "$md_dir" && find "$(dirname "$target_no_anchor")" -maxdepth 1 -name "$(basename "$target_no_anchor")" -print -quit 2>/dev/null)
        if [[ -n "$actual_path" ]]; then
            actual_basename="$(basename "$actual_path")"
            if [[ "$link_basename" != "$actual_basename" ]]; then
                rel_md="${md#$DOCS/}"
                echo "CASE-MISMATCH: ${rel_md}: link text writes '${link_basename}' but disk has '${actual_basename}'"
                CASE_FAIL=1
            fi
        fi
    done < <(grep -oE '\]\([^)]+\)' "$md" | sed -E 's/^\]\(([^)]+)\)$/\1/')
done
set -e

if [ $CASE_FAIL -ne 0 ]; then
    echo "FAIL: case-mismatched link targets (broken on Linux)"
    exit 1
fi
