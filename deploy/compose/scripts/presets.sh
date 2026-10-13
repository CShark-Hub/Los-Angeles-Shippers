#!/usr/bin/env bash
# The example presets in config-examples/presets/ (demo, private, strict). Each is a complete set of
# the files the core reads, so `admin config check --dir` takes it as it is; what they share with the
# examples is kept identical rather than written three times more.
#   presets.sh check   every preset has its files, the catalogue and the prompts are the examples' own
#                      and demo's models are the examples'; compiles nothing
#   presets.sh sync    copies the catalogue and the prompts from the examples into every preset, and
#                      the models into demo (run it after changing the examples)
# demo's policies.yaml and projects.yaml are the examples' with the demo's own lines (the manager's
# health question held for sign-off, the poisoned project): presets.rs checks there is nothing else.
# That each preset passes the check the core starts with, and says what its name promises, is
# tested in crates/mercury-core/tests/presets.rs; the real `config check` runs in `just ci`.
set -euo pipefail
cd "$(dirname "$0")/../../.."

examples=config-examples
presets=(demo private strict)
files=(checks.yaml models.yaml policies.yaml probe.yaml projects.yaml)

fail() {
  echo "presets: $*" >&2
  exit 1
}

# `a` and `b` hold the same text, whatever their line ends.
same() {
  diff -q <(tr -d '\r' <"$1") <(tr -d '\r' <"$2") >/dev/null
}

case "${1:-}" in
  check)
    for preset in "${presets[@]}"; do
      dir=$examples/presets/$preset
      for file in "${files[@]}"; do
        [ -f "$dir/$file" ] || fail "$dir/$file is missing"
      done
      same "$dir/checks.yaml" "$examples/checks.yaml" ||
        fail "$dir/checks.yaml differs from $examples/checks.yaml: change the examples, then run 'just presets-sync'"
      for prompt in "$examples"/prompts/*.md; do
        name=$(basename "$prompt")
        [ -f "$dir/prompts/$name" ] || fail "$dir/prompts/$name is missing"
        same "$dir/prompts/$name" "$prompt" ||
          fail "$dir/prompts/$name differs from $prompt: change the examples, then run 'just presets-sync'"
      done
      for prompt in "$dir"/prompts/*.md; do
        [ -f "$examples/prompts/$(basename "$prompt")" ] || fail "$prompt is not one of the examples' prompts"
      done
    done
    same "$examples/presets/demo/models.yaml" "$examples/models.yaml" ||
      fail "$examples/presets/demo/models.yaml is not the examples' models.yaml: demo's models are the examples'"
    echo "presets: ${#presets[@]} presets, complete and in step with the examples"
    ;;
  sync)
    for preset in "${presets[@]}"; do
      dir=$examples/presets/$preset
      mkdir -p "$dir/prompts"
      cp "$examples/checks.yaml" "$dir/checks.yaml"
      cp "$examples"/prompts/*.md "$dir/prompts/"
    done
    cp "$examples/models.yaml" "$examples/presets/demo/models.yaml"
    echo "presets: synced from $examples; demo's policies.yaml and projects.yaml are the examples' with the demo's own lines, so carry a change to the examples' into them by hand"
    ;;
  *)
    fail "usage: presets.sh check|sync"
    ;;
esac
