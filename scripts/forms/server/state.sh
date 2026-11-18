# Ownership model of the server run (sourced by form-auto.sh).
#
#   who                                      uid:gid
#   root (this runner)                       0:0
#   every container the run starts: the job
#   stages, converters, the importer and the
#   worker one-shots (mazimi-forms)          1999:1999
#   host accounts (opc is often 1000)        no access
#
#   trust files root writes (state dir, kill dir)   dirs root:1999 0750, files root:1999 0640
#   directories a container writes                  1999:1999 0750 (files it writes: its own)
#
# The forms user reads every trust file and writes only its own output; no host account can read
# or write any of it. The unit's UMask=0077 makes plain redirection 0600 root, so every root
# write goes through state_dir / state_put / state_log, and every container output directory
# through container_dirs / container_files.
FORMS_UID=1999
FORMS_GID=1999
FORMS_USER="$FORMS_UID:$FORMS_GID"

_own() {  # _own <uid> <files...>: owner and group only as root (local runs on a Mac are not root)
  local u="$1"
  shift
  if [[ "$(id -u)" == 0 ]]; then chown "$u:$FORMS_GID" "$@"; fi
}

state_dir() {  # state_dir <dir>...: create (or fix) each as root:1999 0750
  local d
  for d in "$@"; do
    mkdir -p "$d"
    _own 0 "$d"
    chmod 0750 "$d"
  done
}

state_put() {  # state_put <file> < content: replace the file atomically, root:1999 0640
  local tmp
  tmp="$(mktemp "$1.XXXXXX")"
  cat > "$tmp"
  _own 0 "$tmp"
  chmod 0640 "$tmp"
  mv -f "$tmp" "$1"
}

state_log() {  # state_log <file> <command...>: stdout and stderr to the file, root:1999 0640
  local f="$1" rc=0
  shift
  "$@" > "$f" 2>&1 || rc=$?
  _own 0 "$f"
  chmod 0640 "$f"
  return "$rc"
}

container_dirs() {  # container_dirs <dir>...: a directory a container writes, 1999:1999 0750
  local d
  for d in "$@"; do
    mkdir -p "$d"
    _own "$FORMS_UID" "$d"
    chmod 0750 "$d"
  done
}

container_files() {  # container_files <file>...: an empty file a container fills, 1999:1999 0640
  local f
  for f in "$@"; do
    : > "$f"
    _own "$FORMS_UID" "$f"
    chmod 0640 "$f"
  done
}
