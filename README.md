# OneiOS

iOS 27 next to One UI on a rooted Samsung Galaxy S24 Ultra (SM-S928, SM8650), switched to and back
the way [OneOS](https://github.com/fahadalijaved/OneOS) switches to another Android: nothing flashed,
the image lives in a folder, One UI's kernel keeps running.

OneOS's guests are Android systems that run unchanged on One UI's Linux kernel. iOS's userland needs
Apple's kernel (XNU), so OneiOS runs a source-built XNU **as a process on One UI's kernel**
(`runtime/xnuhost`): iOS's processes are Linux processes whose system calls and faults go to it. The
image, the switch and the return stay OneOS-shaped. Design, milestones and open questions:
[docs/plan.md](docs/plan.md).

Status: **design and first probes**. Nothing boots yet.

## Layout

```
docs/plan.md        the plan: approach, milestones M0-M7, risks, questions
scripts/fetch/      download Apple's IPSW parts for a pinned build (from Apple's servers, never from here)
scripts/image/      turn them into the OneiOS image set (out/image/), MysticGSI-style manifest + SHA-256
runtime/            xnuhost (XNU as a process) and the M0 phone probes (Android NDK, arm64)
kernel/             scripts and patches for the source-built XNU (UniXNU-based, iOS profile)
oneos/              integration with the OneOS fork (akshaynexus/OneOS): the iOS system entry and session scripts
tools/              host tools
```

## Rules

- **No Apple files in git or in releases.** IPSWs, cryptexes, kernelcaches, kexts and every image built
  from them stay in `out/`, `inputs/` or `firmware/` (git-ignored). Scripts fetch them from Apple and
  build them on the owner's machine. Releases carry only this project's own code and builds
  (scripts, runtime binaries, the XNU built from Apple's open source under the APSL, the OneOS patches).
- Long jobs run in tmux: `scripts/bg.sh NAME COMMAND...`.

## Credits

[OneOS](https://github.com/fahadalijaved/OneOS) (fahadalijaved) and
[OneLineage](https://github.com/WeWaffles/OneLineage) (WeWaffles): the container hot-switch.
[MysticGSI](https://github.com/MysticGSI/mysticgsi): the image build model.
[UniXNU](https://github.com/UniXNU/UniXNU) and [Droid-VM](https://github.com/Droid-VM): XNU on non-Apple
ARM64. [iOSPlus10](https://github.com/akshaynexus/iOSPlus10): XNU and iOS userland on Qualcomm phones.
[Darling](https://github.com/darlinghq/darling): Mach and XNU on Linux.
