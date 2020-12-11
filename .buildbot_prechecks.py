#!/usr/bin/env/python3

import sys
import toml

LOCK_FILE = "Cargo.lock"
CARGO_TOML = "Cargo.toml"


def check_lock_file(problems):
    tml = toml.load(LOCK_FILE)

    # The circular dependency on ykpack means that we can't always do CI in a
    # manner where we have an up-to-date entry for ykpack in the lock file. We
    # compromise: by ensuring it is absent, we force use of the newest ykpack
    # in git.
    for pkg in tml["package"]:
        if pkg["name"] == "ykpack":
            problems.append(f"ykpack detected in {LOCK_FILE}.")


def check_cargo_toml(problems):
    tml = toml.load(CARGO_TOML)

    # During development it is commonplace to patch the ykpack dependency, but
    # we don't ever want to accidentally commit this.
    for patch in tml["patch"].keys():
        if patch.endswith("/yk"):
            problems.append(f"yk patching detected in {CARGO_TOML}.")


def main():
    problems = []
    check_lock_file(problems)
    check_cargo_toml(problems)

    if problems:
        print(f"{__file__}: the following problems were found:",
              file=sys.stderr)
        for pb in problems:
            print(f" - {pb}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
