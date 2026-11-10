# Agents: who owns what (read before every commit)

Everyone works on **`main`** in `/home/akshay/Documents/GitHub/OneiOS`. Commit often, small:
`git add <your paths>` (never `git add -A`), `git commit`, then `git pull --rebase origin main` and `git push origin main`.
If a rebase conflicts in a file you do not own, keep the owner's version and write a note below.

Goal (owner, 2026-11-10): iOS 27 (iPhone18,2, 27.0.1 userland) booting as a OneOS-style guest on the Galaxy S24 Ultra
(SM-S928, One UI, KernelSU 3.3.0). No probe phase and no phase-by-phase delivery: build the whole path at once.
Port what works from UniXNU (DarwinBoot, the XNU patches, prelink, verified root, Metal plugin) and from iOSPlus10's
shim loader into the GSI-style booter (xnuhost). References: `/home/akshay/Documents/GitHub/iOSPlus10` (the full local
repo, with the Mi A3 / laurel_sprout work), UniXNU, Droid-VM, OneOS (`/home/akshay/Documents/GitHub/OneOS`).

| Area | Owner (agent) | Paths |
|---|---|---|
| Image set (IPSW fetch, image build, manifest) | image-build | `profiles/`, `scripts/fetch/`, `scripts/image/`, `docs/image-format.md` |
| xnuhost booter: shim loader, kernel collection load, boot args, device tree, machine layer for XNU as a process | booter | `runtime/xnuhost/`, `runtime/loader/`, `docs/booter.md` |
| XNU kernel: UniXNU + iOSPlus10 patches, iOS profile, the build (Linux toolchain where possible) | release-kernel | `kernel/`, `.github/`, `runtime/package.sh`, `docs/releases.md` |
| iOS 27 userland: root layout, cryptexes, launchd/daemon tweaks, iOS (not iPadOS) identity, display @3x, Metal plugin | userland | `userland/`, `docs/userland.md` |
| OneOS fork integration (`/home/akshay/Documents/GitHub/OneOS`, branch `oneios`) | oneos | that repo; `oneos/` here for shared notes |

## Reaching each other

Write requests in the notes section below; for something urgent, also send a T3 message (`t3_thread_send`,
mode `queue`) to the owner's thread id:

| Agent | Thread id |
|---|---|
| image-build | `thread:delegated-task:command%3Amcp%3A9bead631-dd5f-4441-aca0-cf47f758ae25%3Adelegate-task%3Afd39104b-3ba3-45a0-bbc6-5419196795a3` |
| oneos | `thread:delegated-task:command%3Amcp%3A9bead631-dd5f-4441-aca0-cf47f758ae25%3Adelegate-task%3A9cbb50bc-d9a4-490a-a803-eeb5d4f35b12` |
| release-kernel | `thread:delegated-task:command%3Amcp%3A9bead631-dd5f-4441-aca0-cf47f758ae25%3Adelegate-task%3A1103831e-b6be-44ff-8ba4-cd3b291a7051` |
| booter | `thread:delegated-task:command%3Amcp%3A9bead631-dd5f-4441-aca0-cf47f758ae25%3Adelegate-task%3A81e146dc-96b7-4dff-a49b-8d1b3690a4ec` |
| userland | `thread:delegated-task:command%3Amcp%3A9bead631-dd5f-4441-aca0-cf47f758ae25%3Adelegate-task%3Aab462e7a-0cd0-45a7-806b-f514249a862e` |

## Interfaces (write what others need here; keep it current)

- image set -> booter/userland: (image-build fills in: file names, mount format, manifest schema)
- kernel -> booter: (release-kernel fills in: kernel collection file, entry, boot args, ADT/device tree expectations)
- booter -> oneos: (booter fills in: how xnuhost is started, its arguments, stop/return signal, log path)
- userland -> image-build: (userland fills in: root tweaks applied at build time)

## Notes and requests between agents

(append: date, from -> to, message)
