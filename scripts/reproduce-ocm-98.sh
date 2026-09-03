#!/usr/bin/env bash
set -euo pipefail

current_ocm=${1:?usage: reproduce-ocm-98.sh <current-ocm> <v0.2.32-ocm> <evidence-root>}
legacy_ocm=${2:?usage: reproduce-ocm-98.sh <current-ocm> <v0.2.32-ocm> <evidence-root>}
evidence_root=${3:?usage: reproduce-ocm-98.sh <current-ocm> <v0.2.32-ocm> <evidence-root>}
fixture_root="$evidence_root/fixture"
seed_home="$fixture_root/seed-home"
seed_state="$seed_home/.openclaw"
seed_checkout="$fixture_root/source-checkout"
seed_acpx="$seed_state/npm/projects/fixture/node_modules/@openclaw/acpx"
npm_cache="$fixture_root/npm-cache"
db_meta_script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/ocm-98-db-meta.mjs"

mkdir -p "$evidence_root" "$seed_state/state" "$seed_state/workspace" "$npm_cache"
mkdir -p "$seed_checkout/dist/extensions/codex"
mkdir -p "$seed_checkout/extensions/whatsapp-target"
mkdir -p "$seed_acpx"

resolved_evidence_root=$(realpath -m "$evidence_root")
resolved_fixture_root=$(realpath -m "$fixture_root")
case "$resolved_fixture_root" in
  "$resolved_evidence_root"/*) ;;
  *)
    echo "fixture root escaped evidence root" >&2
    exit 1
    ;;
esac
if [ "$resolved_fixture_root" = "$(realpath -m "$HOME")" ]; then
  echo "fixture root must not equal the runner home" >&2
  exit 1
fi

printf '{"id":"codex","name":"Credential-free fixture"}\n' \
  > "$seed_checkout/dist/extensions/codex/openclaw.plugin.json"
printf '{"id":"whatsapp","name":"Credential-free fixture"}\n' \
  > "$seed_checkout/extensions/whatsapp-target/openclaw.plugin.json"
printf '{"id":"acpx","name":"Credential-free fixture"}\n' \
  > "$seed_acpx/openclaw.plugin.json"
ln -s whatsapp-target "$seed_checkout/extensions/whatsapp-link"

write_fixture_config() {
  cat > "$seed_state/openclaw.json" <<EOF
{
  "agents": {
    "defaults": { "workspace": "$seed_state/workspace" },
    "list": [{ "id": "main", "workspace": "$seed_state/workspace" }]
  },
  "plugins": {
    "load": { "paths": ["$seed_checkout/extensions/whatsapp-link"] },
    "entries": {
      "codex": { "enabled": true },
      "whatsapp": { "enabled": true }
    },
    "installs": {
      "acpx": {
        "source": "npm",
        "installPath": "$seed_acpx"
      },
      "codex": {
        "source": "path",
        "sourcePath": "$seed_checkout/dist/extensions/codex",
        "installPath": "$seed_checkout/dist/extensions/codex"
      },
      "whatsapp": {
        "source": "path",
        "sourcePath": "$seed_checkout/extensions/whatsapp-link",
        "installPath": "$seed_checkout/extensions/whatsapp-link"
      }
    }
  }
}
EOF
}

write_fixture_config

cat > "$evidence_root/fixture-config.redacted.json" <<'EOF'
{
  "agents": {
    "defaults": { "workspace": "<FIXTURE_ROOT>/seed-home/.openclaw/workspace" },
    "list": [
      {
        "id": "main",
        "workspace": "<FIXTURE_ROOT>/seed-home/.openclaw/workspace"
      }
    ]
  },
  "plugins": {
    "load": {
      "paths": ["<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link"]
    },
    "entries": {
      "codex": { "enabled": true },
      "whatsapp": { "enabled": true }
    },
    "installs": {
      "acpx": {
        "source": "npm",
        "installPath": "<FIXTURE_ROOT>/seed-home/.openclaw/npm/projects/fixture/node_modules/@openclaw/acpx"
      },
      "codex": {
        "source": "path",
        "sourcePath": "<FIXTURE_ROOT>/source-checkout/dist/extensions/codex",
        "installPath": "<FIXTURE_ROOT>/source-checkout/dist/extensions/codex"
      },
      "whatsapp": {
        "source": "path",
        "sourcePath": "<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link",
        "installPath": "<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link"
      }
    }
  }
}
EOF

printf '%s\n' \
  HOME \
  OCM_HOME \
  OPENCLAW_HOME \
  OPENCLAW_STATE_DIR \
  OPENCLAW_CONFIG_PATH \
  OCM_ACTIVE_ENV \
  OPENCLAW_PROFILE \
  > "$evidence_root/environment-variable-names.txt"

{
  printf 'fixtureRoot=<FIXTURE_ROOT>\n'
  printf 'sourceState=<FIXTURE_ROOT>/seed-home/.openclaw\n'
  printf 'agents.defaults.workspace=<FIXTURE_ROOT>/seed-home/.openclaw/workspace\n'
  printf 'agents.list[main].workspace=<FIXTURE_ROOT>/seed-home/.openclaw/workspace\n'
  printf 'plugins.load.paths[0]=<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link\n'
  printf 'plugins.installs.acpx.source=npm\n'
  printf 'plugins.installs.acpx.installPath=<FIXTURE_ROOT>/seed-home/.openclaw/npm/projects/fixture/node_modules/@openclaw/acpx\n'
  printf 'plugins.installs.codex.source=path\n'
  printf 'plugins.installs.codex.sourcePath=<FIXTURE_ROOT>/source-checkout/dist/extensions/codex\n'
  printf 'plugins.installs.codex.installPath=<FIXTURE_ROOT>/source-checkout/dist/extensions/codex\n'
  printf 'plugins.installs.whatsapp.source=path\n'
  printf 'plugins.installs.whatsapp.sourcePath=<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link\n'
  printf 'plugins.installs.whatsapp.installPath=<FIXTURE_ROOT>/source-checkout/extensions/whatsapp-link\n'
  printf 'whatsapp.symlinkTarget=%s\n' "$(readlink "$seed_checkout/extensions/whatsapp-link")"
} > "$evidence_root/path-manifest.txt"

env -i \
  HOME="$seed_home" \
  PATH="$PATH" \
  OPENCLAW_HOME="$seed_state" \
  OPENCLAW_STATE_DIR="$seed_state" \
  OPENCLAW_CONFIG_PATH="$seed_state/openclaw.json" \
  npm_config_cache="$npm_cache" \
  npm view openclaw@2026.8.1-beta.1 version dist.tarball dist.integrity --json \
  > "$evidence_root/openclaw-2026.8.1-beta.1-package.json"

env -i \
  HOME="$seed_home" \
  PATH="$PATH" \
  OPENCLAW_HOME="$seed_state" \
  OPENCLAW_STATE_DIR="$seed_state" \
  OPENCLAW_CONFIG_PATH="$seed_state/openclaw.json" \
  npm_config_cache="$npm_cache" \
  npm view openclaw@2026.8.1-beta.2 version dist.tarball dist.integrity --json \
  > "$evidence_root/openclaw-2026.8.1-beta.2-package.json"

stable_prefix="$fixture_root/stable-prefix"
env -i \
  HOME="$seed_home" \
  PATH="$PATH" \
  OPENCLAW_HOME="$seed_state" \
  OPENCLAW_STATE_DIR="$seed_state" \
  OPENCLAW_CONFIG_PATH="$seed_state/openclaw.json" \
  npm_config_cache="$npm_cache" \
  npm install \
    --prefix "$stable_prefix" \
    --omit=dev \
    --no-save \
    --package-lock=false \
    openclaw@2026.8.1-beta.1 \
  > "$evidence_root/stable-fixture-install.log" 2>&1

seed_database="$seed_state/state/openclaw.sqlite"
if [ ! -f "$seed_database" ]; then
  echo "stable package did not create the credential-free state database" >&2
  exit 1
fi
node "$db_meta_script" "$seed_database" > "$evidence_root/seed-schema6.json"
seed_version=$(node "$db_meta_script" "$seed_database" userVersion)
if [ "$seed_version" != "6" ]; then
  echo "expected the beta.1 fixture to use schema 6; got $seed_version" >&2
  exit 1
fi
node "$db_meta_script" "$seed_database" retireCommitmentsV7 \
  > "$evidence_root/seed-schema7-migration.txt"
write_fixture_config
node "$db_meta_script" "$seed_database" > "$evidence_root/seed-before.json"
seed_version=$(node "$db_meta_script" "$seed_database" userVersion)
if [ "$seed_version" != "7" ]; then
  echo "canonical commitments retirement did not produce schema 7; got $seed_version" >&2
  exit 1
fi

run_lane() {
  lane_name=$1
  ocm_binary=$2
  expected_change=$3
  lane_root="$evidence_root/$lane_name"
  lane_home="$lane_root/home"
  lane_state="$lane_home/.openclaw"
  lane_database="$lane_state/state/openclaw.sqlite"
  lane_config="$lane_state/openclaw.json"
  lane_ocm_home="$lane_root/ocm"
  lane_tmp="$lane_root/tmp"
  mkdir -p "$lane_home" "$lane_ocm_home" "$lane_tmp"
  cp -a "$seed_home/." "$lane_home/"

  node "$db_meta_script" "$lane_database" > "$lane_root/before.json"
  sha256sum "$lane_database" > "$lane_root/before.sha256"
  sha256sum "$lane_config" > "$lane_root/before-config.sha256"
  before_hash=$(cut -d ' ' -f 1 "$lane_root/before.sha256")
  before_config_hash=$(cut -d ' ' -f 1 "$lane_root/before-config.sha256")
  before_version=$(node "$db_meta_script" "$lane_database" userVersion)
  if [ "$before_version" != "7" ]; then
    echo "$lane_name did not start at schema 7" >&2
    exit 1
  fi

  if [ "$expected_change" = "source" ]; then
    printf 'env -i HOME=<FIXTURE_ROOT>/%s/home OCM_HOME=<FIXTURE_ROOT>/%s/ocm PATH=<NODE_AND_OCM_PATH> ocm runtime install --version 2026.8.1-beta.2 --json\n' \
      "$lane_name" "$lane_name" > "$lane_root/command.txt"
    env -i \
      HOME="$lane_home" \
      OCM_HOME="$lane_ocm_home" \
      TMPDIR="$lane_tmp" \
      PATH="$PATH" \
      "$ocm_binary" runtime install --version 2026.8.1-beta.2 --json \
      > "$lane_root/ocm.stdout.json" 2> "$lane_root/ocm.stderr.txt"
  else
    postinstall_state="$lane_root/postinstall-state"
    mkdir -p "$postinstall_state"
    printf 'env -i HOME=<FIXTURE_ROOT>/%s/home OCM_HOME=<FIXTURE_ROOT>/%s/ocm PATH=<NODE_AND_OCM_PATH> OPENCLAW_HOME=<FIXTURE_ROOT>/%s/postinstall-state OPENCLAW_STATE_DIR=<FIXTURE_ROOT>/%s/postinstall-state OPENCLAW_CONFIG_PATH=<FIXTURE_ROOT>/%s/postinstall-state/openclaw.json ocm runtime install --version 2026.8.1-beta.2 --json\n' \
      "$lane_name" "$lane_name" "$lane_name" "$lane_name" "$lane_name" \
      > "$lane_root/command.txt"
    env -i \
      HOME="$lane_home" \
      OCM_HOME="$lane_ocm_home" \
      TMPDIR="$lane_tmp" \
      PATH="$PATH" \
      OPENCLAW_HOME="$postinstall_state" \
      OPENCLAW_STATE_DIR="$postinstall_state" \
      OPENCLAW_CONFIG_PATH="$postinstall_state/openclaw.json" \
      "$ocm_binary" runtime install --version 2026.8.1-beta.2 --json \
      > "$lane_root/ocm.stdout.json" 2> "$lane_root/ocm.stderr.txt"
  fi

  node "$db_meta_script" "$lane_database" > "$lane_root/after.json"
  sha256sum "$lane_database" > "$lane_root/after.sha256"
  sha256sum "$lane_config" > "$lane_root/after-config.sha256"
  after_hash=$(cut -d ' ' -f 1 "$lane_root/after.sha256")
  after_config_hash=$(cut -d ' ' -f 1 "$lane_root/after-config.sha256")
  after_version=$(node "$db_meta_script" "$lane_database" userVersion)

  if [ "$expected_change" = "source" ]; then
    if [ "$after_version" != "8" ] || [ "$before_hash" = "$after_hash" ]; then
      echo "$lane_name did not reproduce the source mutation" >&2
      exit 1
    fi
    result="reproduced-source-mutation"
  else
    if [ "$after_version" != "7" ] || [ "$before_hash" != "$after_hash" ] || \
      [ "$before_config_hash" != "$after_config_hash" ]; then
      echo "$lane_name control source changed unexpectedly" >&2
      exit 1
    fi
    result="source-unchanged"
  fi

  printf '{\n  "lane": "%s",\n  "result": "%s",\n  "beforeVersion": %s,\n  "afterVersion": %s,\n  "sourceHashChanged": %s,\n  "sourceConfigHashChanged": %s\n}\n' \
    "$lane_name" \
    "$result" \
    "$before_version" \
    "$after_version" \
    "$([ "$before_hash" = "$after_hash" ] && printf false || printf true)" \
    "$([ "$before_config_hash" = "$after_config_hash" ] && printf false || printf true)" \
    > "$lane_root/result.json"
}

run_lane v0.2.32 "$legacy_ocm" source
run_lane current-main "$current_ocm" control
run_lane explicit-path-control "$current_ocm" control

printf 'OCM #98 clean-room reproduction completed.\n' > "$evidence_root/complete.txt"
