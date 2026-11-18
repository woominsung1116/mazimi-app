"""The server runner's operator commands (scripts/forms/server/form-auto.sh)."""
import os
import pytest
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
RUNNER = REPO / "scripts/forms/server/form-auto.sh"
RUST = REPO / "crates/api/src/form_auto_import.rs"
UNIT = REPO / "scripts/systemd/mazimi-form-auto.service"


def test_the_kill_switch_survives_a_reboot():
    """/run is a tmpfs: a kill file there is gone after a reboot, and the missed timer
    (Persistent=true) would fire and import. The default must sit on persistent storage, the
    runner and the importer must agree on it, and the unit must not own it as a runtime dir."""
    sh_default = re.search(r'KILL="\$\(flag FORM_AUTO_KILL_FILE ([^)]+)\)"', RUNNER.read_text()).group(1)
    rs_default = re.search(r'DEFAULT_KILL_FILE: &str = "([^"]+)"', RUST.read_text()).group(1)
    assert sh_default == rs_default
    assert sh_default.startswith("/var/lib/") and not sh_default.startswith("/run")
    assert "RuntimeDirectory" not in UNIT.read_text()


def _systemctl(tmp_path, enabled: bool):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    calls = tmp_path / "calls"
    (bin_dir / "systemctl").write_text(f"#!/bin/sh\necho \"$@\" >> {calls}\n"
                                       f"[ \"$1\" = is-enabled ] && exit {0 if enabled else 1}\nexit 0\n")
    (bin_dir / "systemctl").chmod(0o755)
    kill = tmp_path / "form-auto-control" / "stop"
    env = tmp_path / "env"
    env.write_text(f"FORM_AUTO_KILL_FILE={kill}\nFORM_AUTO_RUN_ROOT={tmp_path}/root\n")
    e = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "MAZIMI_ENV_FILE": str(env)}
    return e, kill, calls


def test_stop_disables_an_enabled_timer_and_start_restores_it(tmp_path):
    e, kill, calls = _systemctl(tmp_path, enabled=True)
    subprocess.run(["bash", str(RUNNER), "stop"], env=e, check=True, capture_output=True)
    assert kill.exists() and "disable --now mazimi-form-auto.timer" in calls.read_text()
    subprocess.run(["bash", str(RUNNER), "start"], env=e, check=True, capture_output=True)
    assert not kill.exists() and "enable --now mazimi-form-auto.timer" in calls.read_text()


def test_start_never_enables_a_timer_that_was_not_enabled(tmp_path):
    """Deploy step 5 installs the timer without enabling it; stop/start must not switch it on."""
    e, kill, calls = _systemctl(tmp_path, enabled=False)
    subprocess.run(["bash", str(RUNNER), "stop"], env=e, check=True, capture_output=True)
    subprocess.run(["bash", str(RUNNER), "start"], env=e, check=True, capture_output=True)
    assert not kill.exists()
    assert "enable --now" not in calls.read_text() and "disable --now" not in calls.read_text()

CONVERT = REPO / "scripts/forms/hwp/convert.sh"


def test_each_converted_file_gets_its_own_output_directory(tmp_path):
    """A converter taken over by file A must not be able to write file B's PDF or IR: each
    container sees only a fresh directory of its own, and only a checked regular file is moved
    into the shared output directory."""
    raw, out, bin_dir = tmp_path / "raw", tmp_path / "out", tmp_path / "bin"
    for d in (raw, out, bin_dir):
        d.mkdir()
    for name in ("a.hwp", "b.hwp"):
        (raw / name).write_bytes(b"x" + name.encode())
    log = tmp_path / "docker.log"
    # Fake docker: records the /out mount and writes the PDF the real converter would, plus a
    # forged copy of the other file's PDF.
    (bin_dir / "docker").write_text(f"""#!/bin/bash
for a in "$@"; do case "$a" in *:/out) dir="${{a%:/out}}"; echo "$dir" >> {log} ;; esac; done
for a in "$@"; do case "$a" in /in/*.hwp) n="$(basename "$a" .hwp)" ;; esac; done
echo "%PDF $n" > "$dir/$n.pdf"
[[ "$n" == a ]] && echo forged > "$dir/b.pdf"
exit 0
""")
    (bin_dir / "docker").chmod(0o755)
    env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
    r = subprocess.run(["bash", str(CONVERT), "rhwp", str(raw), str(out), "a.hwp", "b.hwp"], env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    mounts = log.read_text().split()
    assert len(mounts) == 2 and len(set(mounts)) == 2 and str(out) not in mounts
    assert (out / "a.pdf").read_text().strip() == "%PDF a"
    assert (out / "b.pdf").read_text().strip() == "%PDF b", "file A's converter could not write B's output"
    assert not [p for p in out.iterdir() if p.name.startswith(".conv")], "per-file directories are removed"


def test_log_lines_go_to_stderr_and_the_unit_records_a_failed_result(tmp_path):
    """real() logs inside $(...): on stdout its message became part of a -v argument and never
    reached the journal. A unit that times out or crashes records a failed run (ExecStopPost)."""
    env = tmp_path / "env"
    env.write_text(f"FORM_AUTO_RUN_ROOT={tmp_path}/root\nFORM_AUTO_KILL_FILE={tmp_path}/form-auto-control/stop\n")
    r = subprocess.run(["bash", str(RUNNER), "run"], env={**os.environ, "MAZIMI_ENV_FILE": str(env)}, capture_output=True, text=True)
    assert r.returncode == 0 and "off (FORM_AUTO_ENABLED" in r.stderr and r.stdout == ""
    unit = UNIT.read_text()
    assert "ExecStopPost=" in unit and "form-auto.sh failed" in unit
    for key in ("NoNewPrivileges=yes", "ProtectSystem=full", "PrivateTmp=yes", "ProtectHome=read-only", "UMask=0077"):
        assert key in unit
    assert "on_term" in RUNNER.read_text() and "trap on_term TERM" in RUNNER.read_text()


STATE_SH = REPO / "scripts/forms/server/state.sh"


def _docker_ok():
    try:
        return subprocess.run(["docker", "info"], capture_output=True, timeout=20).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


@pytest.mark.skipif(not _docker_ok(), reason="needs docker")
def test_only_the_forms_uid_can_read_the_trust_files_and_host_uid_1000_can_write_none(tmp_path):
    """Linux permission semantics, in a Docker volume (a Mac bind mount ignores host
    permissions). Root writes through the state helpers with the unit's UMask=0077, and a
    container writes its output dir as the dedicated forms user (1999, mazimi-forms). Then the
    forms user reads every trust file, and a host account with uid/gid 1000 (opc on Oracle
    Linux) can neither write nor read any of it. A control dir written without the helpers is
    unreadable, so the test can see the bug."""
    vol = f"mz-state-test-{os.getpid()}"
    img = "python:3.12-slim@sha256:3d5ed973e45820f5ba5e46bd065bd88b3a504ff0724d85980dcd05eab361fcf4"
    subprocess.run(["docker", "volume", "create", vol], check=True, capture_output=True)
    try:
        write = r'''set -e; umask 0077; . /s/state.sh
            state_dir /v/.state /v/.state/d /v/.state/d/S /v/ctl-kill
            container_dirs /v/.state/d/S/keys
            printf 'a.pdf %s\n' abc | state_put /v/.state/d/S/derived.sha256
            echo '{"forms":1}' | state_put /v/.state/d/S/import.json
            state_log /v/.state/d/S/import.log echo done
            : | state_put /v/ctl-kill/stop
            mkdir -p /v/control; echo x > /v/control/file; chmod 0700 /v/control'''
        subprocess.run(["docker", "run", "--rm", "-v", f"{vol}:/v", "-v", f"{STATE_SH}:/s/state.sh:ro", "--user", "0:0",
                        "--entrypoint", "bash", img, "-c", write], check=True, capture_output=True)
        # The packet stage writes its key as the forms user.
        subprocess.run(["docker", "run", "--rm", "-v", f"{vol}:/v", "--user", "1999:1999", "--entrypoint", "bash", img, "-c",
                        "echo '{}' > /v/.state/d/S/keys/batch.key.json"], check=True, capture_output=True)
        files = "/v/.state/d/S/derived.sha256 /v/.state/d/S/import.json /v/.state/d/S/import.log /v/ctl-kill/stop /v/.state/d/S/keys/batch.key.json"
        read = f'''for f in {files}; do cat "$f" >/dev/null || echo "UNREADABLE $f"; done
                   echo x > /v/.state/d/S/import.json 2>/dev/null && echo "WRITABLE import.json"
                   cat /v/control/file >/dev/null 2>&1 && echo "CONTROL READABLE"; true'''
        r = subprocess.run(["docker", "run", "--rm", "-v", f"{vol}:/v:ro", "--user", "1999:1999", "--entrypoint", "bash", img, "-c", read],
                           capture_output=True, text=True)
        assert "UNREADABLE" not in r.stdout + r.stderr, r.stdout + r.stderr
        assert "WRITABLE" not in r.stdout
        assert "CONTROL READABLE" not in r.stdout, "the control must be unreadable, or this test cannot see the bug"
        host = f'''for f in {files}; do (echo x >> "$f") 2>/dev/null && echo "HOST WROTE $f"; cat "$f" >/dev/null 2>&1 && echo "HOST READ $f"; done
                   (echo x > /v/.state/d/S/keys/planted.json) 2>/dev/null && echo "HOST PLANTED"; true'''
        r = subprocess.run(["docker", "run", "--rm", "-v", f"{vol}:/v", "--user", "1000:1000", "--entrypoint", "bash", img, "-c", host],
                           capture_output=True, text=True)
        assert "HOST" not in r.stdout, r.stdout
    finally:
        subprocess.run(["docker", "volume", "rm", "-f", vol], capture_output=True)


def test_every_root_write_in_the_runner_goes_through_the_state_helpers():
    src = RUNNER.read_text()
    assert re.search(r'^\. "\$\(dirname "\$0"\)/state\.sh"', src, re.M)
    # No plain redirection into the state dir and no ad-hoc 0700 trust dirs.
    assert not re.search(r'>>? "\$S/', src), "write state files with state_put / state_log"
    assert not re.search(r'install -d -m 07\d\d "\$ROOT/\.state', src)


def test_every_documented_operator_command_dispatches_before_the_run_gates(tmp_path):
    """Read the subcommands from the usage text; each operator command must reach its handler
    with the runner switched off and the kill switch on (an emergency rollback is exactly
    that), never fall through to the run gates. Dropping a subcommand fails here."""
    usage = re.search(r'echo "usage: \$0 ([^"]+)" >&2; exit 2 ;;\n\s*esac\s*$', RUNNER.read_text(), re.S).group(1)
    commands = {part.split()[0] for part in usage.split(" | ")}
    gated = {"run", "finish"}  # these are the runs themselves
    assert {"rollback", "pending", "postcheck", "stop", "start"} <= commands
    args = {"rollback": ["0f0e0d0c-0b0a-4908-8706-050403020100", "wrong programme's form"],
            "postcheck": ["7", "rollback", "wrong programme's form"], "pending": [], "stop": [], "start": []}
    handler = {"rollback": "form-auto-import rollback", "postcheck": "form-auto-import postcheck", "pending": "form-auto-import pending"}
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    log = tmp_path / "docker.log"
    (bin_dir / "docker").write_text(f"#!/bin/sh\necho \"$@\" >> {log}\nexit 0\n")
    (bin_dir / "systemctl").write_text("#!/bin/sh\nexit 1\n")
    for f in ("docker", "systemctl"):
        (bin_dir / f).chmod(0o755)
    kill = tmp_path / "form-auto-control" / "stop"
    for cmd in sorted(commands - gated):
        kill.parent.mkdir(exist_ok=True)
        kill.write_text("")
        log.write_text("")
        env_file = tmp_path / "env"
        env_file.write_text(f"FORM_AUTO_KILL_FILE={kill}\nFORM_AUTO_RUN_ROOT={tmp_path}/root\nFORM_AUTO_ENABLED=false\n")
        r = subprocess.run(["bash", str(RUNNER), cmd, *args.get(cmd, [])], capture_output=True, text=True,
                           env={**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "MAZIMI_ENV_FILE": str(env_file)})
        assert r.returncode == 0, (cmd, r.stderr)
        assert "off (FORM_AUTO_ENABLED" not in r.stderr and "skipped" not in r.stderr, (cmd, r.stderr)
        if cmd in handler:
            assert handler[cmd] in log.read_text(), (cmd, log.read_text())


def test_the_kill_file_lives_in_its_own_directory_and_an_existing_one_is_never_re_moded(tmp_path):
    """A non-default FORM_AUTO_KILL_FILE such as /var/lib/mazimi/stop made every run chown and
    chmod /var/lib/mazimi. The parent must be the dedicated form-auto-control directory; it is
    created only when missing, never re-moded."""
    shared = tmp_path / "shared"
    shared.mkdir(mode=0o755)
    env = tmp_path / "env"
    env.write_text(f"FORM_AUTO_KILL_FILE={shared}/stop\nFORM_AUTO_RUN_ROOT={tmp_path}/root\n")
    r = subprocess.run(["bash", str(RUNNER), "stop"], env={**os.environ, "MAZIMI_ENV_FILE": str(env)}, capture_output=True, text=True)
    assert r.returncode == 2 and not (shared / "stop").exists(), r.stderr
    assert (shared.stat().st_mode & 0o777) == 0o755
    own = tmp_path / "form-auto-control"
    own.mkdir(mode=0o700)
    os.chmod(own, 0o700)
    env.write_text(f"FORM_AUTO_KILL_FILE={own}/stop\nFORM_AUTO_RUN_ROOT={tmp_path}/root\n")
    # An existing directory the forms user cannot read would make every import see "killed"
    # forever: refused loudly, and left as it is.
    r = subprocess.run(["bash", str(RUNNER), "stop"], env={**os.environ, "MAZIMI_ENV_FILE": str(env)}, capture_output=True, text=True)
    assert r.returncode == 2 and "form-auto-control" in r.stderr and not (own / "stop").exists(), r.stderr
    assert (own.stat().st_mode & 0o777) == 0o700, "an existing kill dir is never re-moded"
    os.chmod(own, 0o750)
    r = subprocess.run(["bash", str(RUNNER), "stop"], env={**os.environ, "MAZIMI_ENV_FILE": str(env)}, capture_output=True, text=True)
    assert r.returncode == 0 and (own / "stop").exists(), r.stderr


HWP_SAMPLE = Path(os.environ.get("FORMS_HWP_SAMPLE", str(REPO.parent / ".reports/forms/calib/raw/pnu-hardship-support.hwp")))


def _image(tag):
    return subprocess.run(["docker", "image", "inspect", tag], capture_output=True).returncode == 0 if _docker_ok() else False


@pytest.mark.skipif(not HWP_SAMPLE.exists() or not _image("mazimi-hwp2pdf:2"), reason="needs the hwp2pdf image and a blank HWP sample")
def test_the_reference_converter_works_as_the_forms_user(tmp_path):
    """h2o (LibreOffice + H2Orestart) as 1999:1999: the image has no passwd entry for 1999, so
    LibreOffice needs an explicit user installation dir or it exits 77 and no PDF appears."""
    raw, out = tmp_path / "raw", tmp_path / "out"
    raw.mkdir()
    (raw / "a.hwp").write_bytes(HWP_SAMPLE.read_bytes())
    r = subprocess.run(["bash", str(CONVERT), "h2o", str(raw), str(out), "a.hwp"], capture_output=True, text=True,
                       env={**os.environ, "FORMS_USER": "1999:1999"}, timeout=400)
    assert (out / "a.pdf").exists() and (out / "a.pdf").stat().st_size > 1000, r.stderr


def test_converter_failures_are_counted_not_swallowed():
    src = RUNNER.read_text()
    assert not re.search(r'convert\.sh" (h2o|kordoc) [^\n]*>/dev/null \|\| true', src), "reference converter failures must be counted"
    assert "converters.json" in src and "WARN converters failed" in src
