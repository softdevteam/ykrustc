"""
Checks the specified pull request description for a special `ci-yk` line and
patches Config.toml if necessary.

The line should be of the form:
```
ci-yk: <github-user> <branch>'
```
"""

import sys
import github3 as gh3
import subprocess

SOFTDEV_USER = "softdevteam"
YKRUSTC_REPO = "ykrustc"
YK_REPO = "yk"
DEFAULT_BRANCH = "master"
CARGO_TOML = "Cargo.toml"


def get_pr_no():
    # Get the first line of the commit.
    proc = subprocess.run(["git", "log", "-1", "--pretty=format:%s"],
                          capture_output=True, check=True)
    line = proc.stdout.decode('utf-8')

    # If the build is manual or forced from the web interface there may not be
    # a bors merge commit, in which case we don't do any patching.
    if not line.startswith(('Merge #', 'Try #')):
        return None

    pr_no = line.split(" ", maxsplit=1)[1]
    pr_no = pr_no.rstrip(":")  # Colon present on 'Try' only it seems.
    assert pr_no.startswith('#')
    pr_no = int(pr_no[1:])
    return pr_no


def bogus_line():
    print("couldn't parse 'ci-yk' line.", file=sys.stderr)
    sys.exit(1)


def get_yk_branch(pr_no):
    gh = gh3.GitHub()
    issue = gh.issue(SOFTDEV_USER, YKRUSTC_REPO, pr_no)

    # Look for a 'ci-yk' line in the body of the PR.
    user = SOFTDEV_USER
    branch = DEFAULT_BRANCH
    for line in issue.body.splitlines():
        line = line.strip()
        if line.startswith("ci-yk:"):
            elems = line.split(":")
            if len(elems) != 2:
                bogus_line()
            else:
                params = elems[1].strip().split()
                if len(params) != 2:
                    bogus_line()

                user = params[0].strip()
                branch = params[1].strip()
                break
    return f"https://github.com/{user}/{YK_REPO}", branch


def write_cargo_toml(git_url, branch):
    with open(CARGO_TOML, "a") as f:
        f.write("\n[patch.'https://github.com/softdevteam/yk']\n")
        f.write(f"ykpack = {{ git = '{git_url}', branch='{branch}' }}")


if __name__ == "__main__":
    pr_no = get_pr_no()
    if pr_no is not None:
        url, branch = get_yk_branch(pr_no)

        # x.py gets upset if you try to patch the dep to the default path:
        # "patch for `ykpack` in `https://github.com/softdevteam/yk` points to
        # the same source, but patches must point to different sources"
        if (url, branch) != (f"https://github.com/{SOFTDEV_USER}/{YK_REPO}",
                             DEFAULT_BRANCH):
            # For the sake of the CI logs, print the override.
            print(f"Patching yk dependency to: {url} {branch}")
            write_cargo_toml(url, branch)
