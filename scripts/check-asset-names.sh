#!/usr/bin/env bash
# check-asset-names.sh — release asset-name drift gate.
#
# Purpose
#   The release archive name is assembled INDEPENDENTLY in three places:
#
#     1. .github/workflows/release-please.yml — the packaging steps
#        (`tar -czf "dist/..."`, `Compress-Archive -DestinationPath "dist/..."`),
#        the artifact `name:`/`path:`, the `download-artifact` `pattern:`, and the
#        `files:` list attached to the GitHub Release,
#     2. scripts/install.sh   — `asset="${BIN}-${version}-${target}.${ext}"`,
#     3. scripts/install.ps1  — `$asset = "$Bin-$version-$target.$ext"`.
#
#   Nothing in CI links those three strings. Change any ONE of them and every
#   test, lint, and build still passes; the failure only appears AFTER a release
#   is cut, when the installers 404 on an asset that was published under a
#   different name — by which point the archives are public and SHA256SUMS is
#   already generated over them. This gate closes that window by re-deriving all
#   three names FROM THE REAL FILES on every CI run and asserting they agree.
#
# Comparison granularity (deliberate trade-off)
#   Comparison is on the NORMALIZED SKELETON, not on raw source bytes. Each side's
#   name expression is parsed and its interpolations are replaced by canonical
#   placeholders (`<bin>`, `<version>`, `<target>`, `<ext>`), so all three must
#   reduce to exactly `<bin>-<version>-<target>.<ext>`.
#     * Immune to harmless edits: YAML re-indentation, renaming a shell variable,
#       moving the version through a different workflow expression, comment churn.
#     * Sensitive to every edit that actually changes the published name: field
#       order, separators, prefixes/suffixes, extension, and the binary name.
#   Concrete names (rendered with a probe version) are additionally matched
#   against the artifact `pattern:` and Release `files:` globs, so the plumbing
#   that MOVES the archives is checked too, not just the archives' names.
#
#   Anything this gate cannot parse is a FAILURE, never a silent pass: an
#   unparsable side means the contract is no longer verifiable, which is exactly
#   when drift hides.
#
# Runs no Cargo, makes no network request, writes nothing.
#
# Usage
#   scripts/check-asset-names.sh [REPO_ROOT]
#   REPO_ROOT defaults to the repository root (the script's parent dir).

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="${1:-"$(cd -- "$SCRIPT_DIR/.." && pwd -P)"}"

command -v python3 > /dev/null 2>&1 || {
	printf 'check-asset-names: ERROR: python3 not found (needed to parse the release workflow YAML)\n' >&2
	exit 2
}

CG_ASSET_ROOT="$REPO_ROOT" python3 <<'PY'
import fnmatch
import os
import re
import sys

ROOT = os.environ["CG_ASSET_ROOT"]
WORKFLOW = os.path.join(ROOT, ".github", "workflows", "release-please.yml")
INSTALL_SH = os.path.join(ROOT, "scripts", "install.sh")
INSTALL_PS1 = os.path.join(ROOT, "scripts", "install.ps1")

# A version that could never be confused with a placeholder, used to render
# concrete asset names for the glob/pattern checks.
PROBE_VERSION = "9.9.9"
CANONICAL_SKELETON = "<bin>-<version>-<target>.<ext>"

failures = []


def fail(side, msg):
    failures.append((side, msg))


def die(msg, code=2):
    print("check-asset-names: ERROR: %s" % msg, file=sys.stderr)
    sys.exit(code)


def read(path, label):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        die("cannot read %s (%s): %s" % (label, path, exc))


try:
    import yaml
except ImportError:
    die(
        "PyYAML is required to parse the release workflow "
        "(pip install PyYAML / apt install python3-yaml)"
    )

# ---------------------------------------------------------------------------
# Normalization: collapse each side's interpolations to canonical placeholders.
# ---------------------------------------------------------------------------
WF_SUBS = [
    (re.compile(r"\$\{\{\s*needs\.[A-Za-z0-9_.-]+\.outputs\.version\s*\}\}"), "<version>"),
    (re.compile(r"\$\{\{\s*matrix\.target\s*\}\}"), "<target>"),
    (re.compile(r"\$\{BINARY_NAME\}"), "<bin>"),
    (re.compile(r"\$\{env:BINARY_NAME\}"), "<bin>"),
]

SH_SUBS = [
    (re.compile(r"\$\{BIN\}"), "<bin>"),
    (re.compile(r"\$\{version\}"), "<version>"),
    (re.compile(r"\$\{target\}"), "<target>"),
    (re.compile(r"\$\{ext\}"), "<ext>"),
]

PS_SUBS = [
    (re.compile(r"\$Bin\b"), "<bin>"),
    (re.compile(r"\$version\b"), "<version>"),
    (re.compile(r"\$target\b"), "<target>"),
    (re.compile(r"\$ext\b"), "<ext>"),
]


def normalize(text, subs):
    for pattern, replacement in subs:
        text = pattern.sub(replacement, text)
    return text


def split_ext(side, template, want_ext):
    """`<bin>-<version>-<target>.tar.gz` -> (`<bin>-...-<target>.<ext>`, 'tar.gz')."""
    suffix = "." + want_ext
    if not template.endswith(suffix):
        fail(side, "asset template %r does not end in the expected %r" % (template, suffix))
        return None, None
    return template[: -len(want_ext)] + "<ext>", want_ext


def strip_dist(side, path):
    if not path.startswith("dist/"):
        fail(side, "packaged archive path %r is not under dist/ (the uploaded directory)" % path)
        return None
    return path[len("dist/") :]


# ---------------------------------------------------------------------------
# Side 1 — the release workflow.
# ---------------------------------------------------------------------------
wf_text = read(WORKFLOW, "release workflow")
try:
    wf = yaml.safe_load(wf_text)
except yaml.YAMLError as exc:
    die("release workflow is not valid YAML: %s" % exc)
if not isinstance(wf, dict):
    die("release workflow did not parse to a mapping")

wf_bin = (wf.get("env") or {}).get("BINARY_NAME")
if not isinstance(wf_bin, str) or not wf_bin:
    die("could not read env.BINARY_NAME from the release workflow")

jobs = wf.get("jobs")
if not isinstance(jobs, dict):
    die("release workflow has no jobs mapping")


def job(name):
    j = jobs.get(name)
    if not isinstance(j, dict):
        die("release workflow has no `%s` job" % name)
    return j


build = job("build-binaries")
upload = job("upload-assets")

matrix = (((build.get("strategy") or {}).get("matrix") or {})).get("include")
if not isinstance(matrix, list) or not matrix:
    die("could not read the build-binaries matrix `include` list")

matrix_targets = []
for entry in matrix:
    if not isinstance(entry, dict) or "target" not in entry or "archive" not in entry:
        die("matrix entry %r lacks `target` and/or `archive`" % (entry,))
    matrix_targets.append((str(entry["target"]), str(entry["archive"])))

build_steps = build.get("steps")
if not isinstance(build_steps, list):
    die("build-binaries job has no steps")


def run_scripts(steps):
    return [s.get("run") for s in steps if isinstance(s, dict) and isinstance(s.get("run"), str)]


TAR_RE = re.compile(r'tar\s+-czf\s+"([^"]+)"', re.S)
TAR_MEMBER_RE = re.compile(r'-C\s+"[^"]*"\s+"([^"]+)"', re.S)
ZIP_DEST_RE = re.compile(r'-DestinationPath\s+"([^"]+)"', re.S)
ZIP_SRC_RE = re.compile(r'-Path\s+"([^"]+)"', re.S)

tar_path = tar_member = zip_path = zip_member = None
for script in run_scripts(build_steps):
    norm = normalize(script, WF_SUBS)
    m = TAR_RE.search(norm)
    if m and tar_path is None:
        tar_path = m.group(1)
        mm = TAR_MEMBER_RE.search(norm)
        if mm:
            tar_member = mm.group(1)
    m = ZIP_DEST_RE.search(norm)
    if m and zip_path is None:
        zip_path = m.group(1)
        ms = ZIP_SRC_RE.search(norm)
        if ms:
            zip_member = os.path.basename(ms.group(1))

if tar_path is None:
    die("could not find the `tar -czf \"dist/...\"` packaging step in build-binaries")
if zip_path is None:
    die("could not find the `Compress-Archive -DestinationPath \"dist/...\"` step in build-binaries")
if tar_member is None or zip_member is None:
    die("could not read the archived binary name from the packaging steps")

artifact_name = artifact_path = None
for step in build_steps:
    uses = step.get("uses") if isinstance(step, dict) else None
    if isinstance(uses, str) and uses.startswith("actions/upload-artifact"):
        with_ = step.get("with") or {}
        artifact_name = normalize(str(with_.get("name", "")), WF_SUBS)
        artifact_path = str(with_.get("path", ""))
        break
if not artifact_name or not artifact_path:
    die("could not read the upload-artifact `name:`/`path:` from build-binaries")

upload_steps = upload.get("steps")
if not isinstance(upload_steps, list):
    die("upload-assets job has no steps")

download_pattern = None
download_merge = None
release_files = None
for step in upload_steps:
    uses = step.get("uses") if isinstance(step, dict) else None
    if not isinstance(uses, str):
        continue
    with_ = step.get("with") or {}
    if uses.startswith("actions/download-artifact"):
        download_pattern = with_.get("pattern")
        download_merge = with_.get("merge-multiple")
    elif uses.startswith("softprops/action-gh-release"):
        files = with_.get("files")
        if isinstance(files, str):
            release_files = [ln.strip() for ln in files.splitlines() if ln.strip()]
if not isinstance(download_pattern, str) or not download_pattern:
    die("could not read the download-artifact `pattern:` from upload-assets")
if not release_files:
    die("could not read the release `files:` list from upload-assets")

wf_tar_template = strip_dist("workflow(tar.gz)", tar_path)
wf_zip_template = strip_dist("workflow(zip)", zip_path)
wf_tar_skeleton = wf_tar_ext = wf_zip_skeleton = wf_zip_ext = None
if wf_tar_template is not None:
    wf_tar_skeleton, wf_tar_ext = split_ext("workflow(tar.gz)", wf_tar_template, "tar.gz")
if wf_zip_template is not None:
    wf_zip_skeleton, wf_zip_ext = split_ext("workflow(zip)", wf_zip_template, "zip")

# ---------------------------------------------------------------------------
# Side 2 — scripts/install.sh.
# ---------------------------------------------------------------------------
sh_text = read(INSTALL_SH, "install.sh")


def sh_assign(name):
    m = re.search(r'^%s="([^"]*)"' % re.escape(name), sh_text, re.M)
    return m.group(1) if m else None


sh_bin = sh_assign("BIN")
sh_ext = sh_assign("ext")
sh_asset_raw = sh_assign("asset")
sh_target_raw = sh_assign("target")
sh_sums = sh_assign("SUMS")
for label, value in (
    ("BIN", sh_bin),
    ("ext", sh_ext),
    ("asset", sh_asset_raw),
    ("target", sh_target_raw),
    ("SUMS", sh_sums),
):
    if value is None:
        die("could not parse %s= from scripts/install.sh" % label)

sh_os_parts = re.findall(r'os_part="([^"]+)"', sh_text)
sh_arch_parts = re.findall(r'arch_part="([^"]+)"', sh_text)
if not sh_os_parts or not sh_arch_parts:
    die("could not parse the os_part/arch_part detection tables from scripts/install.sh")

if sh_target_raw != "${arch_part}-${os_part}":
    fail(
        "install.sh",
        "target is built as %r; this gate understands only \"${arch_part}-${os_part}\""
        % sh_target_raw,
    )
sh_targets = sorted({"%s-%s" % (a, o) for a in sh_arch_parts for o in sh_os_parts})

sh_asset_template = normalize(sh_asset_raw, SH_SUBS)
sh_skeleton = sh_asset_template if "<ext>" in sh_asset_template else None
if sh_skeleton is None:
    fail("install.sh", "asset template %r does not interpolate ${ext}" % sh_asset_raw)

sh_member = "<bin>" if re.search(r'\[ -f "\$tmp/\$BIN" \]', sh_text) else None
if sh_member is None:
    fail("install.sh", "could not confirm the expected in-archive binary name ($tmp/$BIN)")

# ---------------------------------------------------------------------------
# Side 3 — scripts/install.ps1.
# ---------------------------------------------------------------------------
ps_text = read(INSTALL_PS1, "install.ps1")


def ps_assign(name):
    m = re.search(r"^\$%s\s*=\s*'([^']*)'" % re.escape(name), ps_text, re.M)
    if m:
        return m.group(1)
    m = re.search(r'^\$%s\s*=\s*"([^"]*)"' % re.escape(name), ps_text, re.M)
    return m.group(1) if m else None


ps_bin = ps_assign("Bin")
ps_ext = ps_assign("ext")
ps_asset_raw = ps_assign("asset")
ps_target_raw = ps_assign("target")
ps_sums = ps_assign("sums")
for label, value in (
    ("Bin", ps_bin),
    ("ext", ps_ext),
    ("asset", ps_asset_raw),
    ("target", ps_target_raw),
    ("sums", ps_sums),
):
    if value is None:
        die("could not parse $%s = from scripts/install.ps1" % label)

ps_arch_parts = re.findall(r"\$archPart\s*=\s*'([^']+)'", ps_text)
if not ps_arch_parts:
    die("could not parse the $archPart detection table from scripts/install.ps1")

ps_target_template = normalize(ps_target_raw, PS_SUBS)
# `$archPart-pc-windows-msvc` -> the fixed windows suffix each arch is joined to.
ps_suffix_match = re.match(r"^\$archPart(-[A-Za-z0-9_.-]+)$", ps_target_raw)
if not ps_suffix_match:
    fail(
        "install.ps1",
        "target is built as %r; this gate understands only \"$archPart-<suffix>\"" % ps_target_raw,
    )
    ps_targets = []
else:
    ps_targets = sorted({a + ps_suffix_match.group(1) for a in ps_arch_parts})

ps_asset_template = normalize(ps_asset_raw, PS_SUBS)
ps_skeleton = ps_asset_template if "<ext>" in ps_asset_template else None
if ps_skeleton is None:
    fail("install.ps1", "asset template %r does not interpolate $ext" % ps_asset_raw)

ps_member = "<bin>.exe" if re.search(r'Join-Path \$tmp "\$Bin\.exe"', ps_text) else None
if ps_member is None:
    fail("install.ps1", 'could not confirm the expected in-archive binary name ($tmp/"$Bin.exe")')

# ---------------------------------------------------------------------------
# A1 — the binary name must be identical on all three sides.
# ---------------------------------------------------------------------------
if not (wf_bin == sh_bin == ps_bin):
    fail(
        "binary-name",
        "workflow env.BINARY_NAME=%r vs install.sh BIN=%r vs install.ps1 $Bin=%r"
        % (wf_bin, sh_bin, ps_bin),
    )

# ---------------------------------------------------------------------------
# A2 — every side must reduce to the SAME canonical skeleton.
# ---------------------------------------------------------------------------
for side, skeleton in (
    ("workflow(tar.gz)", wf_tar_skeleton),
    ("workflow(zip)", wf_zip_skeleton),
    ("install.sh", sh_skeleton),
    ("install.ps1", ps_skeleton),
):
    if skeleton is None:
        continue
    if skeleton != CANONICAL_SKELETON:
        fail(
            side,
            "asset skeleton is %r but the other sides use %r" % (skeleton, CANONICAL_SKELETON),
        )

# ---------------------------------------------------------------------------
# A3 — per-family extension agreement.
# ---------------------------------------------------------------------------
if wf_tar_ext is not None and sh_ext != wf_tar_ext:
    fail(
        "extension(unix)",
        "workflow packages %r but install.sh downloads %r" % (wf_tar_ext, sh_ext),
    )
if wf_zip_ext is not None and ps_ext != wf_zip_ext:
    fail(
        "extension(windows)",
        "workflow packages %r but install.ps1 downloads %r" % (wf_zip_ext, ps_ext),
    )

# ---------------------------------------------------------------------------
# A4 — every matrix target must be producible by exactly one installer, with the
# extension that installer expects.
# ---------------------------------------------------------------------------
for target, archive in matrix_targets:
    in_sh = target in sh_targets
    in_ps = target in ps_targets
    if not in_sh and not in_ps:
        fail(
            "target-coverage",
            "matrix target %r cannot be produced by either installer's platform "
            "detection (install.sh yields %s; install.ps1 yields %s)"
            % (target, sh_targets, ps_targets),
        )
        continue
    if in_sh and in_ps:
        fail(
            "target-coverage",
            "matrix target %r is claimed by BOTH installers (ambiguous)" % target,
        )
        continue
    owner, owner_ext = ("install.sh", sh_ext) if in_sh else ("install.ps1", ps_ext)
    if owner_ext != archive:
        fail(
            "target-coverage",
            "matrix target %r is packaged as %r but %s downloads %r"
            % (target, archive, owner, owner_ext),
        )

for target in sh_targets + ps_targets:
    if target not in [t for t, _ in matrix_targets]:
        fail(
            "target-coverage",
            "installer platform detection can ask for target %r, which the release "
            "matrix never builds" % target,
        )

# ---------------------------------------------------------------------------
# A5 — artifact plumbing: the download pattern must match every rendered
# artifact name, the merge must be enabled, and the upload path must include the
# packaged archives.
# ---------------------------------------------------------------------------
for target, _archive in matrix_targets:
    rendered_artifact = artifact_name.replace("<target>", target)
    if not fnmatch.fnmatchcase(rendered_artifact, download_pattern):
        fail(
            "artifact-plumbing",
            "upload-artifact name %r does not match upload-assets pattern %r"
            % (rendered_artifact, download_pattern),
        )
if download_merge is not True:
    fail(
        "artifact-plumbing",
        "download-artifact `merge-multiple` is %r; per-target artifacts would land in "
        "separate subdirectories and the release globs would miss them" % (download_merge,),
    )

# ---------------------------------------------------------------------------
# A6 — the Release `files:` globs must cover every rendered archive plus the
# checksum file the installers fetch by name.
# ---------------------------------------------------------------------------
rendered_assets = []
for target, archive in matrix_targets:
    rendered_assets.append(
        CANONICAL_SKELETON.replace("<bin>", wf_bin)
        .replace("<version>", PROBE_VERSION)
        .replace("<target>", target)
        .replace("<ext>", archive)
    )

for asset in rendered_assets:
    in_dist = "dist/" + asset
    if not any(fnmatch.fnmatchcase(in_dist, glob) for glob in release_files):
        fail(
            "release-files",
            "archive %r is not covered by any release `files:` glob %s" % (in_dist, release_files),
        )
    if not fnmatch.fnmatchcase(in_dist, artifact_path):
        fail(
            "artifact-plumbing",
            "archive %r is not covered by the upload-artifact path %r" % (in_dist, artifact_path),
        )

if sh_sums != ps_sums:
    fail("checksums", "install.sh SUMS=%r != install.ps1 $sums=%r" % (sh_sums, ps_sums))
else:
    sums_in_dist = "dist/" + sh_sums
    if not any(fnmatch.fnmatchcase(sums_in_dist, glob) for glob in release_files):
        fail(
            "release-files",
            "the installers fetch %r but no release `files:` glob %s publishes it"
            % (sh_sums, release_files),
        )

# ---------------------------------------------------------------------------
# A7 — the archived binary name must be what the installers look for.
# ---------------------------------------------------------------------------
if sh_member is not None and tar_member != sh_member:
    fail(
        "archive-member",
        "workflow tars member %r but install.sh extracts %r" % (tar_member, sh_member),
    )
if ps_member is not None and zip_member != ps_member:
    fail(
        "archive-member",
        "workflow zips member %r but install.ps1 extracts %r" % (zip_member, ps_member),
    )

# ---------------------------------------------------------------------------
# Report.
# ---------------------------------------------------------------------------
if failures:
    for side, msg in failures:
        print("check-asset-names: MISMATCH [%s]: %s" % (side, msg), file=sys.stderr)
    print(
        "check-asset-names: FAIL: %d asset-name disagreement(s) between the release "
        "workflow and the installers" % len(failures),
        file=sys.stderr,
    )
    sys.exit(1)

print("check-asset-names: OK")
print("  binary name        : %s (workflow == install.sh == install.ps1)" % wf_bin)
print("  asset skeleton     : %s" % CANONICAL_SKELETON)
print("  unix  ext          : %s (workflow tar.gz step == install.sh)" % sh_ext)
print("  windows ext        : %s (workflow zip step == install.ps1)" % ps_ext)
print("  checksum file      : %s (published and fetched by both installers)" % sh_sums)
print("  artifact plumbing  : %s -> pattern %s (merge-multiple: true)" % (artifact_name, download_pattern))
print("  matrix targets     : %d, each produced by exactly one installer" % len(matrix_targets))
for target, archive in matrix_targets:
    owner = "install.sh" if target in sh_targets else "install.ps1"
    print("    %-28s %-7s %s" % (target, archive, owner))
PY
