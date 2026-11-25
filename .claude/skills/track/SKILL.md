---
name: track
description: Put this repo (or a fork) and its phone on the iOSOnAnd tracker (https://hackios.appaiproxy.com) and keep the phone's per-area status there up to date. Use when a maintainer asks to "track" their port, after a phone boot that proves or breaks an area, or when CLAUDE.md's tracker rule asks for a status update.
---

# /track: the iOSOnAnd tracker

The tracker records, per phone and per area (boot, touch, wifi, gpu, thermal, ...), what works, what is in progress
and what is broken, each with a detail and a source. Args: `OWNER/REPO DEVICE-SLUG` (default: this checkout's
`origin` repo and `s24-ultra`), or `update AREA STATE "DETAIL" SOURCE` for one status change.

## Rules (they override what the CLI's help suggests)

- **Read the CLI before you run it.** Download it into an empty scratch folder (never into the repo), read all of it,
  and note its sha256. Run it from outside the repo.
- **Sign in with the gist proof only:** `node cli.mjs login`. It sends only the GitHub login name and the URL of a
  secret gist, which it deletes after. Never `login --token` (it sends the GitHub token), never `--password` on the
  command line.
- **Never send the owner's email address** (or any personal detail) in a detail, a note, a caption or a flag.
- **Only claim what the repo or a phone log can show.** A detail says what was seen, on which phone, on which date. A
  source is a public link: a README section, a doc, a commit, an issue comment. Not a local path, not `out/`.
- **Ask the maintainer when unsure**, and before you change a state that the maintainer set by hand (the detail says
  "(OneiOS maintainer, ...)" or "owner").
- No Apple files and no screenshots with personal data in `media-upload`.

## First time: sign in and claim

```sh
d=$(mktemp -d) && cd "$d"
curl -fsSLo cli.mjs https://hackios.appaiproxy.com/cli.mjs && shasum -a 256 cli.mjs   # then read it
export TRACKER_URL=https://hackios.appaiproxy.com
node cli.mjs login                                  # gist proof (needs gh, logged in)
node cli.mjs repo-claim OWNER/REPO --device SLUG    # approved at once when GitHub shows you as the owner
```

If `repo-claim` prints a verification code: write the code alone to `.iosonand-verify` on the repo's default branch,
commit it with a pathspec, push, run `node cli.mjs repo-verify <claim id>`, then delete the file in a second commit.
A phone the tracker does not list: `node cli.mjs repo-add OWNER/REPO --device SLUG` (an admin adds new devices;
`node cli.mjs claim-add SLUG --note "..."` asks for one).

## Record the status

```sh
node cli.mjs areas                                  # the area slugs
node cli.mjs status-get --device SLUG               # what is there now
node cli.mjs status-set SLUG AREA works|in-progress|broken|unknown --detail "..." --source "https://github.com/..."
node cli.mjs media-upload FILE --device SLUG --area AREA --caption "..."   # proof the maintainer gave you
node cli.mjs whoami                                 # the check: maintains SLUG and the repo
```

The CLI of 2026-11-25 (sha256 `d024013f…`; read each new version before you run it, diff it against the last one)
also has:

```sh
node cli.mjs status-set SLUG AREA STATE --detail "..." --source URL --os ios --version "iOS 27.0.1" [--photo FILE ...]
node cli.mjs history SLUG                           # each status change: before, after, who, when
node cli.mjs gpu-get SLUG                           # the GPU record: method, stack, fps
node cli.mjs gpu-set SLUG --method metal-vulkan|virtio-gpu|cpu|... --stack "A -> B" --fps 40-58 --fps-note "..." --source URL
node cli.mjs features SLUG                          # notable extra features (S Pen, ...)
node cli.mjs feature-add SLUG "Name" works --detail "..." --source URL --os ios
node cli.mjs references --gpu "Xclipse" | --area metal   # ports to learn from, by GPU or area
```

The areas now include `metal` and `gles` (beside `gpu`), `hotspot` and `mobile-data`.

Read the repo for each area: README "What works" and "Known issues", the device's `docs/<device>.md`, the docs of
the area (docs/hardware.md, docs/gpu.md, ...), the open issues and the recent commits. Compare with the tracker.
Change only what the evidence supports; list the rest as questions to the maintainer.

## After a phone boot (the automatic part)

CLAUDE.md asks agents to update the tracker when a phone boot proves or breaks an area. Then:

1. Take the lines that prove it (the `oneios-*`, `thermal`, `frame:`, `wifi:` lines of the session log).
2. `status-get` the area. If the state or the detail is now wrong, `status-set` it with the new evidence and a public
   source (the commit or doc that records the boot). Do it after the docs are committed and pushed, so the source
   exists.
3. A change that is only built or host-tested does not change a state on the tracker. Say "not booted" in the detail
   when it matters.
4. No token (`node cli.mjs whoami` fails with 401): skip, and tell the maintainer to run `/track` once. Never ask for
   or paste a token.
