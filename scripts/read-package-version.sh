#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: scripts/read-package-version.sh <Cargo.toml> <Cargo.lock>" >&2
  exit 1
fi

manifest="$1"
lockfile="$2"

[[ -f "$manifest" && ! -L "$manifest" ]] || {
  echo "error: package manifest is missing or invalid: $manifest" >&2
  exit 1
}
[[ -f "$lockfile" && ! -L "$lockfile" ]] || {
  echo "error: package lockfile is missing or invalid: $lockfile" >&2
  exit 1
}

manifest_version="$(
  perl -0e '
    use strict;
    use warnings;

    local $/;
    my $content = <>;
    my @sections = split(/(?=^\[)/m, $content);
    my @package_sections = grep { /^\[package\]\s*$/m } @sections;
    die "expected exactly one [package] section\n" unless @package_sections == 1;

    my $section = $package_sections[0];
    my @names = ($section =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
    my @versions = ($section =~ /^version\s*=\s*"([^"]+)"\s*$/mg);
    die "expected exactly one package name\n" unless @names == 1;
    die "package name must be ocm\n" unless $names[0] eq "ocm";
    die "expected exactly one package version\n" unless @versions == 1;
    print "$versions[0]\n";
  ' "$manifest"
)" || {
  echo "error: could not read a unique ocm package version from $manifest" >&2
  exit 1
}

lock_version="$(
  perl -0e '
    use strict;
    use warnings;

    local $/;
    my $content = <>;
    my @matches;
    while ($content =~ /(?:\A|\n)\[\[package\]\]\s*\n(.*?)(?=\n\[\[package\]\]\s*\n|\z)/sg) {
      my $block = $1;
      my @names = ($block =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
      next unless grep { $_ eq "ocm" } @names;
      push @matches, $block;
    }
    die "expected exactly one ocm package record\n" unless @matches == 1;

    my $block = $matches[0];
    my @names = ($block =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
    my @versions = ($block =~ /^version\s*=\s*"([^"]+)"\s*$/mg);
    die "expected one ocm package name\n" unless @names == 1 && $names[0] eq "ocm";
    die "expected one ocm package version\n" unless @versions == 1;
    die "ocm package record must be local\n" if $block =~ /^(?:source|checksum)\s*=/m;
    print "$versions[0]\n";
  ' "$lockfile"
)" || {
  echo "error: could not read a unique local ocm package version from $lockfile" >&2
  exit 1
}

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
"${script_dir}/validate-version.sh" "$manifest_version"
"${script_dir}/validate-version.sh" "$lock_version"

if [[ "$manifest_version" != "$lock_version" ]]; then
  echo "error: Cargo.toml and Cargo.lock ocm versions do not match" >&2
  exit 1
fi

printf '%s\n' "$manifest_version"
