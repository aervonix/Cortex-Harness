# After step 26: the Linux sandbox

Step 26 built the Linux sandbox in `cortex-sandbox` (the policy and its wire form, detection, the launcher's two sides, Landlock and seccomp behind the feature `backends`, what the model is told, the look at protected paths, and the Job Object's limits on Windows) and wired it into `cortex-core`, `cortex-cli`, `cortex-policy`, `cortex-tools` and `cortex-agent`. It adds two direct dependencies, `landlock` and `seccompiler`, and four packages to a Linux build: those two, `enumflags2` and its derive.

## The kernels

Measured on 2026-11-13, with a one-job probe workflow run from a scratch branch that was deleted afterwards, and on the two desks.

| Where | Kernel | Landlock | The sandbox |
|---|---|---|---|
| Hosted `ubuntu-24.04`, x86-64 | 7.0.0-1017-azure | 8 | enforces |
| Hosted `ubuntu-24.04`, arm64 | 7.0.0-1017-azure | 8 | enforces |
| Hosted `ubuntu-22.04` | 6.8.0-1070-azure | 4 | enforces, with the gap on signals |
| The desktop (WSL 2, Ubuntu 24.04) | 6.18.58.1-microsoft-standard-WSL2 | 7 | enforces |
| The laptop | 7.0.0-42-generic | 8 | enforces |

Every one has Landlock in its active list of security modules, and seccomp filters. The blocking jobs run on `ubuntu-latest`; the tests of confinement fail there, rather than skip, if the sandbox does not enforce (`CI` set), so the image cannot change under them unseen.

## At a desk

To be filled from the runs before the pull request.

## On the pull request

To be filled from the pull request's runs.
