#!/usr/bin/env python3
"""Build somebody else's fdroiddata merge request locally, to help clear the review queue.

F-Droid's maintainers have far more merge requests than test capacity, so a contributor who can
run the real build and report the result is genuinely useful. This drives the same container and
the same `fdroid build --on-server` invocation their CI uses (see fdroid/build-locally.sh), so a
result here means something.

    ./fdroid/test-mr.py --list           # merge requests waiting on F-Droid, not on the author
    ./fdroid/test-mr.py 43165            # build whatever that MR changed
    ./fdroid/test-mr.py 43165 --report   # ...and print a comment you could paste into it

Needs no GitLab credentials: the merge request, its diff and the fork's files are all readable
anonymously.

⚠️  A merge request's build recipe is arbitrary shell written by a stranger, and it runs with
network access. It is confined to a rootless podman container (the same one F-Droid uses), which
is meaningful isolation but not a guarantee. Only run this on machines where that is acceptable.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request

API = "https://gitlab.com/api/v4"
PROJECT = urllib.parse.quote_plus("fdroid/fdroiddata")
HERE = os.path.dirname(os.path.abspath(__file__))


def get(url):
    with urllib.request.urlopen(url) as r:
        return json.load(r)


def raw_file(project_id, path, ref):
    """File contents from any public project, or None if it isn't there."""
    url = f"{API}/projects/{project_id}/repository/files/{urllib.parse.quote_plus(path)}/raw?ref={urllib.parse.quote(ref)}"
    try:
        with urllib.request.urlopen(url) as r:
            return r.read().decode()
    except Exception:
        return None


def build_entries(yaml_text):
    """versionCode -> the block's text, without a YAML parser.

    Deliberately regex-based: recipes in the wild use tags and anchors that a strict loader
    rejects, and all we need is which versionCodes exist and whether their text changed.
    """
    if not yaml_text:
        return {}
    out, current, code = {}, [], None
    for line in yaml_text.splitlines():
        m = re.match(r"^  - versionName:", line)
        if m:
            if code is not None:
                out[code] = "\n".join(current)
            current, code = [line], None
        elif current:
            current.append(line)
            if code is None:
                m2 = re.match(r"^\s+versionCode:\s*(\d+)\s*$", line)
                if m2:
                    code = int(m2.group(1))
    if code is not None:
        out[code] = "\n".join(current)
    return out


def changed_builds(mr):
    """[(appid, recipe_text, [versionCodes]), ...] for the metadata this MR touches."""
    changes = get(f"{API}/projects/{PROJECT}/merge_requests/{mr['iid']}/changes")["changes"]
    results = []
    for ch in changes:
        path = ch["new_path"]
        m = re.fullmatch(r"metadata/([A-Za-z0-9_.]+)\.yml", path)
        if not m:
            continue
        appid = m.group(1)
        theirs = raw_file(mr["source_project_id"], path, mr["source_branch"])
        if theirs is None:                       # fork deleted or private
            theirs = raw_file(PROJECT, path, mr["sha"])
        if theirs is None:
            print(f"  ! could not read {path} from the merge request", file=sys.stderr)
            continue
        base = raw_file(PROJECT, path, "master")  # absent for a brand-new app
        new, old = build_entries(theirs), build_entries(base)
        todo = sorted(c for c, text in new.items() if old.get(c) != text)
        # A disabled build is one the author has parked; CI skips them and so do we.
        todo = [c for c in todo if not re.search(r"^\s+disable:", new[c], re.M)]
        if todo:
            results.append((appid, theirs, todo))
    return results


def run_build(appid, recipe_text, vercodes):
    with tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False) as fh:
        fh.write(recipe_text)
        recipe_path = fh.name
    env = dict(os.environ, APPID=appid, RECIPE_FILE=recipe_path,
               VERCODE=" ".join(str(v) for v in vercodes),
               SKIP_BINARY="1",  # the reference APK is the author's problem, not ours to verify
               FDROID_LOCAL_ARTIFACTS=os.path.expanduser(f"~/.cache/fdroid-mrtest/{appid}"))
    os.makedirs(env["FDROID_LOCAL_ARTIFACTS"], exist_ok=True)
    log = os.path.join(env["FDROID_LOCAL_ARTIFACTS"], "build.log")
    print(f"==> building {appid}:{','.join(map(str, vercodes))}   log: {log}")
    with open(log, "w") as out:
        rc = subprocess.call([os.path.join(HERE, "build-locally.sh")],
                             env=env, stdout=out, stderr=subprocess.STDOUT)
    os.unlink(recipe_path)
    return rc, log


def summarise(log):
    ok, fail, err = [], [], []
    for line in open(log, errors="replace"):
        if "Successfully built" in line:
            ok.append(line.split("INFO:")[-1].strip())
        elif "Could not build" in line:
            fail.append(line.split("ERROR:")[-1].strip())
        elif re.search(r"\bBuildException\b|No apks match|FAILURE: Build failed", line):
            err.append(line.strip())
    # sdkmanager chatters "Failed to fetch URL .../sys-img.xml" for repositories it cannot see;
    # that is noise, not a build failure, and reporting it as one would waste a reviewer's time.
    return ok, fail, err


def cmd_list(args):
    mrs = []
    for page in (1, 2, 3):
        batch = get(f"{API}/projects/{PROJECT}/merge_requests"
                    f"?state=opened&per_page=100&page={page}&order_by=updated_at")
        if not batch:
            break
        mrs += batch
    queue = [m for m in mrs
             if "review-requested" in (m["labels"] or [])
             and "waiting-on-response" not in (m["labels"] or [])
             and "waiting-for-upstream" not in (m["labels"] or [])]
    print(f"{len(queue)} merge request(s) waiting on F-Droid rather than on the author:\n")
    for m in queue[:args.limit]:
        print(f"  !{m['iid']:<6} {m['title'][:60]:62} updated {m['updated_at'][:10]}")
    return 0


def cmd_test(args):
    mr = get(f"{API}/projects/{PROJECT}/merge_requests/{args.mr}")
    print(f"!{mr['iid']}  {mr['title']}")
    print(f"  {mr['source_branch']} @ {mr['sha'][:10]}   labels: {', '.join(mr['labels'] or [])}\n")
    work = changed_builds(mr)
    if not work:
        print("No enabled, changed build entries — nothing for this tool to do.")
        return 0
    results = []
    for appid, recipe, vercodes in work:
        rc, log = run_build(appid, recipe, vercodes)
        ok, fail, err = summarise(log)
        results.append((appid, vercodes, rc, ok, fail, err, log))
        for line in ok:
            print(f"  OK   {line}")
        if rc != 0:
            for line in (fail + err)[:4]:
                print(f"  FAIL {line[:160]}")
        elif fail or err:
            print(f"  note: {len(fail)+len(err)} error-ish line(s) in the log, but the build "
                  f"succeeded — not reported")
    if args.report:
        print("\n" + "=" * 72 + "\nSuggested comment:\n")
        print(render_report(mr, results))
    return 0 if all(r[2] == 0 for r in results) else 1


def render_report(mr, results):
    """A comment a maintainer can act on: what was built, where, and what I did NOT check."""
    out = ["Not a maintainer — just working through the test queue.\n"]
    for appid, vercodes, rc, ok, fail, err, _log in results:
        codes = ", ".join(map(str, vercodes))
        if rc == 0:
            out.append(f"Built `{appid}` {codes} in `buildserver-trixie` with the same command CI "
                       f"runs (`fdroid build --verbose --test --refresh-scanner --on-server "
                       f"--no-tarball`). It builds:\n")
            out.append("```\n" + "\n".join(ok) + "\n```\n")
        else:
            out.append(f"Built `{appid}` {codes} in `buildserver-trixie` with the same command CI "
                       f"runs. It fails:\n")
            out.append("```\n" + "\n".join((fail + err)[:4]) + "\n```\n")
    out.append("x86_64 host. This only says the build completes — I haven't reviewed the recipe, "
               "checked signatures, or verified reproducibility.")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mr", nargs="?", type=int, help="merge request IID, e.g. 43165")
    ap.add_argument("--list", action="store_true", help="show the queue instead of building")
    ap.add_argument("--limit", type=int, default=25, help="how many to list")
    ap.add_argument("--report", action="store_true", help="print a paste-ready comment")
    args = ap.parse_args()
    if args.list:
        return cmd_list(args)
    if not args.mr:
        ap.error("give a merge request number, or --list")
    return cmd_test(args)


if __name__ == "__main__":
    sys.exit(main())
